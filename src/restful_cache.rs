use std::hash::Hash;
use std::time::{Instant, Duration};
use std::collections::HashMap;
use std::sync::Arc;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::{mpsc,RwLock};
use reqwest::Client;
use anyhow::Error;

pub struct RESTfulCache<K, V, F> 
where 
    K: 'static + Eq + Hash + Debug, 
    V: 'static + Send + Sync,
    F: 'static + Send + Sync + Fn(K, &Client) -> Pin<Box<dyn Future<Output = Result<V, Error>> + Send>>,
{
    responses: HashMap<K, V>,
    fetched_at: HashMap<K, Instant>,
    time_to_live: Duration,
    fetch: F,
    sender: mpsc::Sender<(K, V)>,
    client: Client, 
}

impl<K, V, F> RESTfulCache<K, V, F> 
where 
    K: 'static + Eq + Hash + Clone + Send + Sync + Debug, 
    V: 'static + Send + Sync + Clone,
    F: 'static + Send + Sync + Fn(K, &Client) -> Pin<Box<dyn Future<Output = Result<V, Error>> + Send>>,
{   
    pub fn new(time_to_live: Duration, fetch: F) -> Arc<RwLock<Self>> {
        let (tx, mut rx) = mpsc::channel(100);
        let client = reqwest::Client::new();  // Add this line
        let cache = Arc::new(RwLock::new(Self { 
            responses: HashMap::new(), 
            fetched_at: HashMap::new(), 
            time_to_live,
            fetch,
            sender: tx,
            client
        }));

        let cache_clone = Arc::clone(&cache);
        tokio::spawn(async move {
            while let Some((key, value)) = rx.recv().await {
                let mut cache = cache_clone.write().await;
                cache.set(key, value).await;
            }
        });

        cache
    }

    pub async fn get(&self, key: K) -> Result<V, Error> {
        let now = Instant::now();
        let should_fetch = self.should_fetch(&key, now).await;

        if !should_fetch {
            log::info!("Cache hit for key: {:?}", key);
            return Ok(self.responses.get(&key).unwrap().clone());
        } 

        match (self.fetch)(key.clone(), &self.client).await {
            Ok(value) => {
                self.sender.send((key.clone(), value.clone())).await.unwrap();
                Ok(value)
            },
            Err(e) => Err(e),
        }
    }

    pub async fn set(&mut self, key: K, value: V) {
        let now = Instant::now();
        self.responses.insert(key.clone(), value);
        self.fetched_at.insert(key, now);
    }

    pub async fn should_fetch(&self, key: &K, now: Instant) -> bool {
        let two_ttl_ago = now - 2 * self.time_to_live;
        let fetched_at = self.fetched_at.get(key).unwrap_or(&two_ttl_ago);
        now.duration_since(*fetched_at) > self.time_to_live
    }
}
