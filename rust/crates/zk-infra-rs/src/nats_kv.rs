use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, kv};
use bytes::Bytes;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use async_nats::jetstream::kv::Entry as KvEntry;
pub use async_nats::jetstream::kv::Operation as KvOperation;
pub use async_nats::jetstream::kv::Watch as KvWatch;

/// Default NATS KV bucket used for the service registry.
pub const REGISTRY_BUCKET: &str = "zk.svc.registry.v1";

/// Wraps a NATS JetStream KV store with registry-oriented helpers.
///
/// Construct via [`KvRegistryClient::open`] or [`KvRegistryClient::create`].
#[derive(Clone)]
pub struct KvRegistryClient {
    store: kv::Store,
}

impl KvRegistryClient {
    /// Open an existing KV bucket by name.
    pub async fn open(
        js: &jetstream::Context,
        bucket: &str,
    ) -> Result<Self, async_nats::Error> {
        let store = js.get_key_value(bucket).await?;
        Ok(Self { store })
    }

    /// Create a KV bucket (or open if it already exists).
    pub async fn create(
        js: &jetstream::Context,
        bucket: &str,
        ttl: Duration,
    ) -> Result<Self, async_nats::Error> {
        let store = js
            .create_key_value(kv::Config {
                bucket: bucket.to_string(),
                max_age: ttl,
                ..Default::default()
            })
            .await?;
        Ok(Self { store })
    }

    /// Write `value` at `key`. Overwrites any existing value.
    pub async fn put(&self, key: &str, value: Bytes) -> Result<u64, async_nats::Error> {
        Ok(self.store.put(key, value).await?)
    }

    /// Get the current value for `key`.
    /// In async-nats 0.37 `kv::Store::get` returns `Result<Option<Bytes>>` directly.
    pub async fn get(&self, key: &str) -> Result<Option<Bytes>, async_nats::Error> {
        Ok(self.store.get(key).await?)
    }

    /// Watch all updates under `prefix` (use `">"` for all keys).
    /// Returns the raw `kv::Watch` stream — callers may use it with `StreamExt`.
    pub async fn watch(&self, prefix: &str) -> Result<KvWatch, async_nats::Error> {
        Ok(self.store.watch(prefix).await?)
    }

    /// Spawn a task that re-puts `key → value` every `interval`.
    ///
    /// The task runs until the returned `JoinHandle` is aborted or the NATS
    /// connection drops.
    pub fn heartbeat_loop(
        self: Arc<Self>,
        key: String,
        value: Bytes,
        interval: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                match self.put(&key, value.clone()).await {
                    Ok(_) => info!(key, "KV heartbeat"),
                    Err(e) => warn!(key, error = %e, "KV heartbeat failed"),
                }
            }
        })
    }
}
