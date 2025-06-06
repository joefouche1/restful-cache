// Importing necessary libraries and modules
use std::hash::Hash;
use std::time::{Instant, Duration};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc,RwLock};
use tokio::time::sleep;
use reqwest::Client;
use anyhow::Error;

// Defining the RESTfulCache struct
pub struct RESTfulCache<K, V, F> 
where 
    K: 'static + Eq + Hash + Debug, // Key must implement these traits
    V: 'static + Send + Sync, // Value must implement these traits
    F: 'static + Send + Sync + Fn(K, &Client) -> Pin<Box<dyn Future<Output = Result<V, Error>> + Send>>, // Fetch function type
{
    responses: HashMap<K, V>, // Cache for responses
    fetched_at: HashMap<K, Instant>, // Timestamps for when each key was fetched
    time_to_live: Duration, // Duration for which a response is considered fresh
    fetch: F, // Function to fetch a value for a key
    sender: mpsc::Sender<(K, V)>, // Sender for the channel to update cache
    client: Client, // HTTP client
    hits: AtomicUsize, // Number of cache hits
    requests: AtomicUsize, // Number of total requests
    activity: Arc<RwLock<VecDeque<Instant>>>, // Timestamps of the last hour's requests
}


// Implementing methods for RESTfulCache
impl<K, V, F> RESTfulCache<K, V, F> 
where 
    K: 'static + Eq + Hash + Clone + Send + Sync + Debug, // Key must implement these traits
    V: 'static + Send + Sync + Clone, // Value must implement these traits
    F: 'static + Send + Sync + Fn(K, &Client) -> Pin<Box<dyn Future<Output = Result<V, Error>> + Send>>, // Fetch function type
{   
    // Constructor for RESTfulCache
    pub fn new(time_to_live: Duration, fetch: F) -> Arc<RwLock<Self>> {
        let (tx, mut rx) = mpsc::channel(100); // Creating a channel for cache updates
        let client = reqwest::Client::new();  // Creating a new HTTP client
        let cache = Arc::new(RwLock::new(Self { 
            responses: HashMap::new(), 
            fetched_at: HashMap::new(), 
            time_to_live,
            fetch,
            sender: tx,
            client,
            hits: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            activity: Arc::new(RwLock::new(VecDeque::new())),
        }));

        let cache_clone = Arc::clone(&cache);
        // Spawning a new task to listen for cache updates
        tokio::spawn(async move {
            while let Some((key, value)) = rx.recv().await {
                let mut cache = cache_clone.write().await;
                cache.set(key, value).await;
            }
        });

        // Spawning a new task to evict stale entries
        let cache_clone_evict = Arc::clone(&cache);

        tokio::spawn(async move {
            let mut sleep_duration = Duration::from_secs(2 * time_to_live.as_secs());
            loop {
                sleep(sleep_duration).await;
                if let Ok(mut cache) = cache_clone_evict.try_write() {
                    let evicted = cache.evict();
                    if evicted {
                        sleep_duration = Duration::from_secs(2 * time_to_live.as_secs());
                    } else {
                        sleep_duration *= 2;
                    }
                }
            }
        });

        cache
    }

    // Method to get a value for a key
    pub async fn get(&self, key: K) -> Result<V, Error> {
        let now = Instant::now();
        let should_fetch = self.should_fetch(&key, now).await;

        self.requests.fetch_add(1, Ordering::Relaxed);

        {
            let mut activity = self.activity.write().await;
            activity.push_back(now);
            while activity.front().map_or(false, |front| front.elapsed() > Duration::from_secs(3600)) {
                activity.pop_front();
            }
        }

        if !should_fetch {
            log::info!("Cache hit for key: {:?}", key);
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(self.responses.get(&key).unwrap().clone());
        } 

        // Fetching a new value if necessary
        match (self.fetch)(key.clone(), &self.client).await {
            Ok(value) => {
                if let Err(e) = self.sender.send((key.clone(), value.clone())).await {
                    log::error!("Failed to send update to cache: {}", e);
                }
                Ok(value)
            },
            Err(e) => Err(e),
        }
    }

    // Method to set a value for a key
    pub async fn set(&mut self, key: K, value: V) {
        let now = Instant::now();
        self.responses.insert(key.clone(), value);
        self.fetched_at.insert(key, now);
    }

    // Method to check whether or not a value should be fetched
    pub async fn should_fetch(&self, key: &K, now: Instant) -> bool {
        let two_ttl_ago = now - 2 * self.time_to_live;
        let fetched_at = self.fetched_at.get(key).unwrap_or(&two_ttl_ago);
        now.duration_since(*fetched_at) > self.time_to_live
    }

    // Method to evict stale entries
    pub fn evict(&mut self) -> bool {
        let now = Instant::now();
        let keys_to_keep: HashSet<_> = self.fetched_at.iter()
            .filter(|&(_, &v)| now.duration_since(v) <= self.time_to_live)
            .map(|(k, _)| k.clone())
            .collect();

        let keys_to_remove: Vec<_> = self.fetched_at.keys()
            .filter(|k| !keys_to_keep.contains(k))
            .cloned()
            .collect();

        let evicted = !keys_to_remove.is_empty();

        for k in keys_to_remove {
            self.fetched_at.remove(&k);
            self.responses.remove(&k);
        }

        evicted
    }

    // Returns (hits, requests, recent_activity_count)
    pub async fn stats(&self) -> (usize, usize, usize) {
        let hits = self.hits.load(Ordering::Relaxed);
        let requests = self.requests.load(Ordering::Relaxed);
        let activity_len = self.activity.read().await.len();
        (hits, requests, activity_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};
    use std::sync::Arc;

    // Helper function to create a simple fetch function for testing
    fn create_test_fetch_fn() -> impl Fn(String, &Client) -> Pin<Box<dyn Future<Output = Result<String, Error>> + Send>> {
        |key: String, _client: &Client| {
            Box::pin(async move {
                // Simulate some async work
                sleep(Duration::from_millis(10)).await;
                Ok(format!("value_for_{}", key))
            })
        }
    }

    // Helper function to create a fetch function that fails
    fn create_failing_fetch_fn() -> impl Fn(String, &Client) -> Pin<Box<dyn Future<Output = Result<String, Error>> + Send>> {
        |_key: String, _client: &Client| {
            Box::pin(async move {
                Err(anyhow::anyhow!("Fetch failed"))
            })
        }
    }

    #[tokio::test]
    async fn test_cache_basic_operations() {
        let ttl = Duration::from_secs(1);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // Test getting a value (should fetch from function)
        let result = cache.read().await.get("test_key".to_string()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "value_for_test_key");

        // Check stats - should have 1 request, 0 hits (first fetch)
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 1);
        assert_eq!(hits, 0);
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let ttl = Duration::from_secs(2);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // First get - should fetch
        let result1 = cache.read().await.get("test_key".to_string()).await;
        assert!(result1.is_ok());
        
        // Give a little time for the async update to complete
        sleep(Duration::from_millis(50)).await;

        // Second get immediately - should hit cache
        let result2 = cache.read().await.get("test_key".to_string()).await;
        assert!(result2.is_ok());
        assert_eq!(result1.unwrap(), result2.unwrap());

        // Check stats - should have 2 requests, 1 hit
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 2);
        assert_eq!(hits, 1);
    }

    #[tokio::test]
    async fn test_cache_ttl_expiration() {
        let ttl = Duration::from_millis(100);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // First get - should fetch
        let result1 = cache.read().await.get("test_key".to_string()).await;
        assert!(result1.is_ok());

        // Wait for TTL to expire
        sleep(Duration::from_millis(150)).await;

        // Second get after TTL - should fetch again (not hit cache)
        let result2 = cache.read().await.get("test_key".to_string()).await;
        assert!(result2.is_ok());

        // Check stats - should have 2 requests, 0 hits (both were fetches)
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 2);
        assert_eq!(hits, 0);
    }

    #[tokio::test]
    async fn test_manual_set_and_get() {
        let ttl = Duration::from_secs(1);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // Set a value manually
        {
            let mut cache_guard = cache.write().await;
            cache_guard.set("manual_key".to_string(), "manual_value".to_string()).await;
        }

        // Get the manually set value (should be a cache hit)
        let result = cache.read().await.get("manual_key".to_string()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "manual_value");

        // Check stats - should have 1 request, 1 hit
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 1);
        assert_eq!(hits, 1);
    }

    #[tokio::test]
    async fn test_should_fetch_logic() {
        let ttl = Duration::from_millis(100);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());
        let cache_guard = cache.read().await;

        let now = Instant::now();
        
        // Key doesn't exist - should fetch
        assert!(cache_guard.should_fetch(&"nonexistent".to_string(), now).await);
        
        drop(cache_guard);

        // Set a value manually
        {
            let mut cache_guard = cache.write().await;
            cache_guard.set("existing_key".to_string(), "value".to_string()).await;
        }

        let cache_guard = cache.read().await;
        let now = Instant::now();
        
        // Key exists and is fresh - should not fetch
        assert!(!cache_guard.should_fetch(&"existing_key".to_string(), now).await);

        // Wait for TTL to expire
        drop(cache_guard);
        sleep(Duration::from_millis(150)).await;
        let cache_guard = cache.read().await;
        let now = Instant::now();

        // Key exists but is stale - should fetch
        assert!(cache_guard.should_fetch(&"existing_key".to_string(), now).await);
    }

    #[tokio::test]
    async fn test_eviction() {
        // Use 2 seconds TTL to avoid background eviction interference
        let ttl = Duration::from_secs(2);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // Set some values and record the time we set them
        {
            let mut cache_guard = cache.write().await;
            cache_guard.set("key1".to_string(), "value1".to_string()).await;
            cache_guard.set("key2".to_string(), "value2".to_string()).await;
            
            // Verify values were actually set
            assert!(cache_guard.responses.contains_key("key1"));
            assert!(cache_guard.fetched_at.contains_key("key1"));
            assert!(cache_guard.responses.contains_key("key2"));
            assert!(cache_guard.fetched_at.contains_key("key2"));
        }

        // Check state before eviction
        let (keys_count_before, fetched_count_before) = {
            let cache_guard = cache.read().await;
            (cache_guard.responses.len(), cache_guard.fetched_at.len())
        };
        assert_eq!(keys_count_before, 2);
        assert_eq!(fetched_count_before, 2);

        // Manually modify the timestamps to simulate old entries
        {
            let mut cache_guard = cache.write().await;
            let old_timestamp = Instant::now() - Duration::from_secs(3); // 3 seconds ago
            cache_guard.fetched_at.insert("key1".to_string(), old_timestamp);
            cache_guard.fetched_at.insert("key2".to_string(), old_timestamp);
        }

        // Manually trigger eviction
        let evicted = {
            let mut cache_guard = cache.write().await;
            cache_guard.evict()
        };

        assert!(evicted); // Should have evicted stale entries

        // Check state after eviction
        let (keys_count_after, fetched_count_after) = {
            let cache_guard = cache.read().await;
            (cache_guard.responses.len(), cache_guard.fetched_at.len())
        };
        assert_eq!(keys_count_after, 0);
        assert_eq!(fetched_count_after, 0);

        // Try to evict again - should not evict anything
        let evicted_again = {
            let mut cache_guard = cache.write().await;
            cache_guard.evict()
        };

        assert!(!evicted_again); // Should not have evicted anything
    }

    #[tokio::test]
    async fn test_fetch_error_handling() {
        let ttl = Duration::from_secs(1);
        let cache = RESTfulCache::new(ttl, create_failing_fetch_fn());

        // Get a value with a failing fetch function
        let result = cache.read().await.get("test_key".to_string()).await;
        assert!(result.is_err());

        // Check stats - should have 1 request, 0 hits
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 1);
        assert_eq!(hits, 0);
    }

    #[tokio::test]
    async fn test_concurrent_access() {
        let ttl = Duration::from_secs(1);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // Spawn multiple concurrent tasks
        let mut handles = Vec::new();
        for i in 0..10 {
            let cache_clone = Arc::clone(&cache);
            handles.push(tokio::spawn(async move {
                cache_clone.read().await.get(format!("key_{}", i)).await
            }));
        }

        // Wait for all tasks to complete
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }

        // Check stats
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 10);
        // All should be fetches since keys are different
        assert_eq!(hits, 0);
    }

    #[tokio::test]
    async fn test_activity_tracking() {
        let ttl = Duration::from_secs(1);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // Make some requests
        for i in 0..5 {
            let _ = cache.read().await.get(format!("key_{}", i)).await;
        }

        // Check activity count
        let (_, _, activity_count) = cache.read().await.stats().await;
        assert_eq!(activity_count, 5);
    }

    #[tokio::test]
    async fn test_same_key_multiple_requests() {
        let ttl = Duration::from_secs(2);
        let cache = RESTfulCache::new(ttl, create_test_fetch_fn());

        // First request
        let result1 = cache.read().await.get("same_key".to_string()).await;
        assert!(result1.is_ok());

        // Give time for async update
        sleep(Duration::from_millis(50)).await;

        // Multiple subsequent requests for the same key
        let result2 = cache.read().await.get("same_key".to_string()).await;
        let result3 = cache.read().await.get("same_key".to_string()).await;
        
        assert!(result2.is_ok());
        assert!(result3.is_ok());
        assert_eq!(result1.unwrap(), result2.as_ref().unwrap().clone());
        assert_eq!(result2.unwrap(), result3.unwrap());

        // Check stats - should have 3 requests, 2 hits
        let (hits, requests, _) = cache.read().await.stats().await;
        assert_eq!(requests, 3);
        assert_eq!(hits, 2);
    }
}
