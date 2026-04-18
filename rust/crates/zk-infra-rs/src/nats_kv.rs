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

/// Maximum consecutive retryable heartbeat failures before the loop
/// gives up and signals registry-unavailable.
const MAX_HEARTBEAT_RETRIES: u32 = 5;

/// Classification of a heartbeat update failure.
#[derive(Debug, Clone)]
pub(crate) enum HeartbeatError {
    /// CAS revision conflict — another writer modified the key.
    Fenced(String),
    /// Retryable transport error (timeout, disconnect, etc.)
    Retryable(String),
}

/// Reason the heartbeat loop terminated abnormally.
///
/// Sent through the watch channel; `None` means healthy / running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeartbeatTermination {
    /// CAS conflict — another writer owns the key.
    Fenced,
    /// Too many consecutive retryable failures (timeout, disconnect).
    RegistryUnavailable,
}

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
    pub async fn open(js: &jetstream::Context, bucket: &str) -> Result<Self, async_nats::Error> {
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
    ///
    /// Termination paths:
    /// - **CAS conflict** (`WrongLastRevision`): signals `Fenced` immediately.
    /// - **Transport error** (timeout, disconnect): retries up to
    ///   [`MAX_HEARTBEAT_RETRIES`] consecutive failures, then signals
    ///   `RegistryUnavailable`.
    ///
    /// The task runs until the `JoinHandle` is aborted or one of the above
    /// termination conditions is met.
    pub(crate) fn heartbeat_loop(
        self: Arc<Self>,
        key: String,
        value: Bytes,
        interval: Duration,
        initial_revision: u64,
        fenced_tx: watch::Sender<Option<HeartbeatTermination>>,
    ) -> JoinHandle<()> {
        let k = key.clone();
        heartbeat_loop_inner(
            key,
            value,
            interval,
            initial_revision,
            fenced_tx,
            move |val, rev| {
                let kv = Arc::clone(&self);
                let k = k.clone();
                async move {
                    kv.store.update(&k, val, rev).await.map_err(|e| {
                        match e.kind() {
                            kv::UpdateErrorKind::WrongLastRevision => {
                                HeartbeatError::Fenced(e.to_string())
                            }
                            _ => HeartbeatError::Retryable(e.to_string()),
                        }
                    })
                }
            },
        )
    }
}

/// Testable heartbeat loop — decoupled from `KvRegistryClient`.
///
/// Accepts an async closure for the update step so tests can inject fake
/// updaters without a real NATS connection.
///
/// Error classification:
/// - [`HeartbeatError::Fenced`] (CAS conflict) → immediately signal fencing and stop.
/// - [`HeartbeatError::Retryable`] (timeout, disconnect) → log and retry on the next
///   tick. After [`MAX_HEARTBEAT_RETRIES`] consecutive failures, signal
///   [`HeartbeatTermination::RegistryUnavailable`] and stop. A single success resets
///   the counter.
pub(crate) fn heartbeat_loop_inner<F, Fut>(
    key: String,
    value: Bytes,
    interval: Duration,
    initial_revision: u64,
    fenced_tx: watch::Sender<Option<HeartbeatTermination>>,
    update_fn: F,
) -> JoinHandle<()>
where
    F: Fn(Bytes, u64) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<u64, HeartbeatError>> + Send,
{
    tokio::spawn(async move {
        let mut revision = initial_revision;
        let mut ticker = tokio::time::interval(interval);
        let mut consecutive_failures: u32 = 0;
        loop {
            ticker.tick().await;
            match update_fn(value.clone(), revision).await {
                Ok(new_rev) => {
                    revision = new_rev;
                    if consecutive_failures > 0 {
                        tracing::info!(
                            key,
                            revision,
                            recovered_after = consecutive_failures,
                            "KV heartbeat recovered"
                        );
                    }
                    consecutive_failures = 0;
                    tracing::debug!(key, revision, "KV heartbeat");
                }
                Err(HeartbeatError::Fenced(msg)) => {
                    warn!(key, error = %msg, "KV heartbeat CAS conflict — fenced, stopping");
                    let _ = fenced_tx.send(Some(HeartbeatTermination::Fenced));
                    return;
                }
                Err(HeartbeatError::Retryable(msg)) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_HEARTBEAT_RETRIES {
                        warn!(
                            key,
                            error = %msg,
                            consecutive_failures,
                            max_retries = MAX_HEARTBEAT_RETRIES,
                            "KV heartbeat registry unavailable after max retries — stopping",
                        );
                        let _ =
                            fenced_tx.send(Some(HeartbeatTermination::RegistryUnavailable));
                        return;
                    }
                    warn!(
                        key,
                        error = %msg,
                        consecutive_failures,
                        max_retries = MAX_HEARTBEAT_RETRIES,
                        "KV heartbeat retryable failure — will retry"
                    );
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
        let (fenced_tx, fenced_rx) = watch::channel(None);
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
        assert!(
            fenced_rx.borrow().is_none(),
            "should not be terminated on success"
        );
        assert!(
            revision.load(Ordering::SeqCst) > 1,
            "revision should have advanced"
        );
    }

    #[tokio::test]
    async fn heartbeat_loop_sends_fenced_signal_on_cas_conflict() {
        let (fenced_tx, mut fenced_rx) = watch::channel(None);

        let handle = heartbeat_loop_inner(
            "key".into(),
            Bytes::new(),
            Duration::from_millis(10),
            1,
            fenced_tx,
            |_val, _rev| async { Err(HeartbeatError::Fenced("CAS conflict".into())) },
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), fenced_rx.changed())
            .await
            .expect("timed out waiting for fenced signal")
            .expect("channel closed");
        assert_eq!(
            *fenced_rx.borrow(),
            Some(HeartbeatTermination::Fenced),
            "should be fenced after CAS conflict"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_loop_retries_then_signals_registry_unavailable() {
        let (fenced_tx, mut fenced_rx) = watch::channel(None);
        let call_count = Arc::new(AtomicU64::new(0));
        let cc = Arc::clone(&call_count);

        let handle = heartbeat_loop_inner(
            "key".into(),
            Bytes::new(),
            Duration::from_millis(5),
            1,
            fenced_tx,
            move |_val, _rev| {
                let cc = Arc::clone(&cc);
                async move {
                    cc.fetch_add(1, Ordering::SeqCst);
                    Err(HeartbeatError::Retryable("timed out".into()))
                }
            },
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), fenced_rx.changed())
            .await
            .expect("timed out waiting for termination signal")
            .expect("channel closed");
        assert_eq!(
            *fenced_rx.borrow(),
            Some(HeartbeatTermination::RegistryUnavailable),
            "should signal registry unavailable after max retries"
        );
        assert!(
            call_count.load(Ordering::SeqCst) >= MAX_HEARTBEAT_RETRIES as u64,
            "should have retried at least MAX_HEARTBEAT_RETRIES times"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_loop_resets_retry_count_on_success() {
        let (fenced_tx, fenced_rx) = watch::channel(None);
        let call_count = Arc::new(AtomicU64::new(0));
        let cc = Arc::clone(&call_count);

        // Fail for (MAX_RETRIES - 1) calls, then succeed, repeat.
        // Should never terminate because success resets the counter.
        let handle = heartbeat_loop_inner(
            "key".into(),
            Bytes::new(),
            Duration::from_millis(5),
            1,
            fenced_tx,
            move |_val, rev| {
                let cc = Arc::clone(&cc);
                async move {
                    let n = cc.fetch_add(1, Ordering::SeqCst);
                    // Succeed every MAX_HEARTBEAT_RETRIES - 1 calls
                    if (n + 1) % (MAX_HEARTBEAT_RETRIES as u64) == 0 {
                        Ok(rev + 1)
                    } else {
                        Err(HeartbeatError::Retryable("timed out".into()))
                    }
                }
            },
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.abort();
        assert!(
            fenced_rx.borrow().is_none(),
            "should not terminate when retries reset on success"
        );
        assert!(
            call_count.load(Ordering::SeqCst) > MAX_HEARTBEAT_RETRIES as u64,
            "should have made multiple rounds of calls"
        );
    }
}
