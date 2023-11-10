// Importing necessary libraries and modules
use std::hash::Hash;
use std::time::{Instant, Duration};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
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
    hits: Arc<RwLock<usize>>, // Number of cache hits
    requests: Arc<RwLock<usize>>, // Number of total requests
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
            hits: Arc::new(RwLock::new(0)),
            requests: Arc::new(RwLock::new(0)),
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

        *self.requests.write().await += 1;
        self.activity.write().await.push_back(Instant::now());
        while self.activity.read().await.front().unwrap().elapsed() > Duration::from_secs(3600) {
            self.activity.write().await.pop_front();
        }

        if !should_fetch {
            log::info!("Cache hit for key: {:?}", key);
            *self.hits.write().await += 1;
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
}
