use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, kv};
use bytes::Bytes;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

pub use async_nats::jetstream::kv::Entry as KvEntry;
pub use async_nats::jetstream::kv::Operation as KvOperation;
pub use async_nats::jetstream::kv::Watch as KvWatch;

/// Default NATS KV bucket used for the service registry.
/// NATS KV bucket names must not contain dots — use dashes.
pub const REGISTRY_BUCKET: &str = "zk-svc-registry-v1";

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

    /// Open an existing KV bucket or create it if it does not exist.
    ///
    /// If the bucket already exists (created by another service), the existing
    /// bucket is returned regardless of TTL. This avoids "different configuration"
    /// errors when multiple services race to create the same shared bucket.
    pub async fn create(
        js: &jetstream::Context,
        bucket: &str,
        ttl: Duration,
    ) -> Result<Self, async_nats::Error> {
        if let Ok(store) = js.get_key_value(bucket).await {
            return Ok(Self { store });
        }
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
    /// Returns the new KV revision (used as the seed for CAS heartbeats).
    pub async fn put(&self, key: &str, value: Bytes) -> Result<u64, async_nats::Error> {
        Ok(self.store.put(key, value).await?)
    }

    /// CAS update — succeeds only if the current KV revision matches `last_revision`.
    /// Returns the new revision on success, or an error if another writer won.
    pub async fn update(
        &self,
        key: &str,
        value: Bytes,
        last_revision: u64,
    ) -> Result<u64, async_nats::Error> {
        Ok(self.store.update(key, value, last_revision).await?)
    }

    /// Get the current value for `key`.
    pub async fn get(&self, key: &str) -> Result<Option<Bytes>, async_nats::Error> {
        Ok(self.store.get(key).await?)
    }

    /// Watch all updates under `prefix` (use `">"` for all keys).
    /// Returns the raw `kv::Watch` stream — callers may use it with `StreamExt`.
    pub async fn watch(&self, prefix: &str) -> Result<KvWatch, async_nats::Error> {
        Ok(self.store.watch(prefix).await?)
    }

    /// Delete `key` from the KV store (creates a tombstone; entry expires on TTL).
    pub async fn delete(&self, key: &str) -> Result<(), async_nats::Error> {
        self.store.delete(key).await?;
        Ok(())
    }

    /// Spawn a CAS heartbeat task that re-writes `key → value` every `interval`.
    ///
    /// Uses `update(revision)` instead of a plain `put` to detect concurrent writers.
    /// If another writer modifies `key` (CAS conflict), the task fires `fenced_tx`
    /// with `true` and terminates — the owning service must observe this signal and
    /// shut down to avoid split-brain.
    ///
    /// The task runs until the returned `JoinHandle` is aborted or a CAS conflict
    /// is detected.
    pub fn heartbeat_loop(
        self: Arc<Self>,
        key: String,
        value: Bytes,
        interval: Duration,
        initial_revision: u64,
        fenced_tx: watch::Sender<bool>,
    ) -> JoinHandle<()> {
        let k = key.clone();
        heartbeat_loop_inner(key, value, interval, initial_revision, fenced_tx, move |val, rev| {
            let kv = Arc::clone(&self);
            let k = k.clone();
            async move { kv.update(&k, val, rev).await }
        })
    }
}

/// Testable heartbeat loop — decoupled from `KvRegistryClient`.
///
/// Accepts an async closure for the update step so tests can inject fake
/// updaters without a real NATS connection.
pub(crate) fn heartbeat_loop_inner<F, Fut>(
    key: String,
    value: Bytes,
    interval: Duration,
    initial_revision: u64,
    fenced_tx: watch::Sender<bool>,
    update_fn: F,
) -> JoinHandle<()>
where
    F: Fn(Bytes, u64) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<u64, async_nats::Error>> + Send,
{
    tokio::spawn(async move {
        let mut revision = initial_revision;
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match update_fn(value.clone(), revision).await {
                Ok(new_rev) => {
                    revision = new_rev;
                    tracing::debug!(key, revision, "KV heartbeat");
                }
                Err(e) => {
                    warn!(key, error = %e, "KV heartbeat CAS failed — fenced, stopping");
                    let _ = fenced_tx.send(true);
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[tokio::test]
    async fn heartbeat_loop_advances_revision_on_success() {
        let (fenced_tx, fenced_rx) = watch::channel(false);
        let revision = Arc::new(AtomicU64::new(1));
        let rev_clone = Arc::clone(&revision);

        let handle = heartbeat_loop_inner(
            "key".into(),
            Bytes::new(),
            Duration::from_millis(10),
            1,
            fenced_tx,
            move |_val, rev| {
                let r = Arc::clone(&rev_clone);
                async move {
                    let new = rev + 1;
                    r.store(new, Ordering::SeqCst);
                    Ok(new)
                }
            },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        assert!(!*fenced_rx.borrow(), "should not be fenced on success");
        assert!(revision.load(Ordering::SeqCst) > 1, "revision should have advanced");
    }

    #[tokio::test]
    async fn heartbeat_loop_sends_fenced_signal_on_cas_conflict() {
        let (fenced_tx, mut fenced_rx) = watch::channel(false);

        let handle = heartbeat_loop_inner(
            "key".into(),
            Bytes::new(),
            Duration::from_millis(10),
            1,
            fenced_tx,
            |_val, _rev| async {
                Err(async_nats::Error::from("CAS conflict"))
            },
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), fenced_rx.changed())
            .await
            .expect("timed out waiting for fenced signal")
            .expect("channel closed");
        assert!(*fenced_rx.borrow(), "should be fenced after CAS conflict");
        handle.abort();
    }
}
