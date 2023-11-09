## RESTful Cache

restful-cache is a Rust library that provides a simple, efficient, and customizable caching solution specifically designed for RESTful APIs. It allows you to cache responses from RESTful API endpoints and automatically fetches fresh data when the cache expires, improving the performance and responsiveness of your applications.

Key features of restful-cache include:

- Asynchronous: restful-cache is built for async programming and fully supports async/await syntax, making it easy to integrate into modern Rust applications.
- Customizable Fetch Function: You can provide your own fetch function to restful-cache, giving you full control over how data is fetched when a cache miss occurs.
- Time-to-Live (TTL) Expiration: Each cache entry has a TTL, after which it is considered stale and a fresh value is fetched.
- Thread-Safe: restful-cache uses RwLock for concurrency control, ensuring that it's safe to use in multi-threaded applications.
- Multi-Producer, Single-Consumer (mpsc) Channels: restful-cache uses mpsc::Sender for cache updates. This allows multiple threads to send updates to the cache concurrently, while a single consumer thread applies the updates. This design ensures that cache updates are thread-safe and do not block the threads that are sending updates.

Whether you're building a web server, a microservice, or a CLI tool, restful-cache can help you reduce load on your RESTful APIs and make your applications faster and more efficient.

Please see the Usage section for examples on how to use restful-cache in your Rust applications.


# Quickstart

```rust

use restful_cache::Cache;
use std::time::Duration;
use reqwest::Client;
use std::pin::Pin;
use std::future::Future;
use anyhow::Error;

// Define a fetch function
fn fetch_function(key: String, client: &Client) -> Pin<Box<dyn Future<Output = Result<String, Error>> + Send>> {
    Box::pin(async move {
        let url = format!("https://api.example.com/data/{}", key);
        let response = client.get(&url).send().await?;
        let text = response.text().await?;
        Ok(text)
    })
}

// Create a new cache with a TTL of 60 seconds
let cache = Cache::new(Duration::from_secs(60), fetch_function);

// Fetch data for a key
let key = "some-key".to_string();
let data = cache.get(key).await.unwrap();

println!("Data for key: {}", data);

```
