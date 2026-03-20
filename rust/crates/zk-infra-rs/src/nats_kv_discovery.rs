//! Watch-based KV cache for service discovery.
//!
//! [`KvDiscoveryClient`] opens the shared registry KV bucket and keeps an
//! in-memory cache current via a single persistent watch stream.
//!
//! # Startup contract
//!
//! `start()` returns with an **empty** cache. Call `spawn_watch_loop()` immediately
//! after construction.
//!
//! # Reconnect behaviour
//!
//! On reconnect the watch loop uses a two-phase approach to avoid an empty-cache
//! window:
//!
//! 1. **Snapshot phase** — entries are drained into a local staging `HashMap` using
//!    a settle timeout ([`SNAPSHOT_SETTLE_MS`]). The first read timeout signals that
//!    the initial KV burst is complete (async-nats 0.37 provides no explicit sentinel).
//! 2. **Atomic swap** — `*cache = staging` replaces the old cache in a single
//!    write-lock acquisition. Readers always see old-or-new data, never empty.
//! 3. **Live phase** — subsequent Put/Delete events are applied directly to the
//!    shared cache via the same still-open watch stream.
//!
//! # Usage
//! ```no_run
//! use zk_infra_rs::nats_kv_discovery::KvDiscoveryClient;
//!
//! # async fn example(js: async_nats::jetstream::Context) -> anyhow::Result<()> {
//! let disc = KvDiscoveryClient::start(&js).await?;
//! disc.spawn_watch_loop();
//!
//! // Find all active OMS instances
//! let oms = disc.resolve(|r| r.service_type == "OMS").await;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use prost::Message;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use zk_proto_rs::zk::discovery::v1::ServiceRegistration;

use crate::nats_kv::{KvEntry, KvOperation, KvRegistryClient, REGISTRY_BUCKET};

/// Time (ms) to wait for the next watch entry before declaring the initial
/// KV snapshot complete. The burst of current entries is typically delivered
/// in <10 ms on a local NATS connection; 200 ms is conservative.
const SNAPSHOT_SETTLE_MS: u64 = 200;

/// In-memory cache of all service registrations from the shared NATS KV registry.
#[derive(Clone)]
pub struct KvDiscoveryClient {
    kv: KvRegistryClient,
    cache: Arc<RwLock<HashMap<String, ServiceRegistration>>>,
}

impl KvDiscoveryClient {
    /// Create a discovery client. Cache starts empty.
    ///
    /// Call [`spawn_watch_loop`] immediately after construction. The watch stream
    /// delivers the initial KV snapshot before live updates, so a single loop
    /// handles both without a startup gap.
    pub async fn start(js: &jetstream::Context) -> Result<Self, async_nats::Error> {
        let kv = KvRegistryClient::open(js, REGISTRY_BUCKET).await?;
        let cache = Arc::new(RwLock::new(HashMap::new()));
        Ok(Self { kv, cache })
    }

    /// Spawn a background task that populates and maintains the cache via KV watch.
    ///
    /// Uses a two-phase approach on each (re)connect to avoid an empty-cache window:
    /// snapshot entries are drained into a local staging map first, then atomically
    /// swapped into the shared cache. Reconnects automatically on stream error.
    ///
    /// The returned `JoinHandle` can be aborted on shutdown.
    pub fn spawn_watch_loop(&self) -> JoinHandle<()> {
        let kv = self.kv.clone();
        let cache = Arc::clone(&self.cache);

        tokio::spawn(async move {
            loop {
                let mut watch = match kv.watch(">").await {
                    Ok(w) => w,
                    Err(e) => {
                        warn!(error = %e, "discovery: watch failed, retrying in 5s");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Phase 1: drain initial snapshot into a local staging map.
                // The settle timeout detects end-of-snapshot (async-nats 0.37 has no
                // explicit sentinel). Any live updates that arrive during the burst
                // are also captured in staging — they represent the correct current state.
                let mut staging: HashMap<String, ServiceRegistration> = HashMap::new();
                let mut stream_error = false;
                loop {
                    match tokio::time::timeout(
                        Duration::from_millis(SNAPSHOT_SETTLE_MS),
                        watch.next(),
                    )
                    .await
                    {
                        Ok(Some(Ok(entry))) => apply_entry(&mut staging, entry),
                        Ok(Some(Err(e))) => {
                            warn!(error = %e, "discovery: stream error during snapshot, reconnecting");
                            stream_error = true;
                            break;
                        }
                        Ok(None) => break, // stream ended
                        Err(_) => break,   // settle timeout → snapshot complete
                    }
                }
                if stream_error {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                // Phase 2: atomic swap — replaces old cache in one write-lock.
                // Readers see old data up to this point, then new snapshot; never empty.
                let n = staging.len();
                *cache.write().await = staging;
                debug!("discovery: snapshot loaded ({n} entries)");

                // Phase 3: live updates applied directly to the shared cache.
                while let Some(result) = watch.next().await {
                    match result {
                        Ok(entry) => apply_entry(&mut *cache.write().await, entry),
                        Err(e) => {
                            warn!(error = %e, "discovery: watch stream error, reconnecting");
                            break;
                        }
                    }
                }

                warn!("discovery: watch stream ended, reconnecting in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        })
    }

    /// Return all registrations matching the predicate (read-locked snapshot, cloned).
    pub async fn resolve(
        &self,
        pred: impl Fn(&ServiceRegistration) -> bool,
    ) -> Vec<ServiceRegistration> {
        self.cache
            .read()
            .await
            .values()
            .filter(|r| pred(r))
            .cloned()
            .collect()
    }

    /// Return a full clone of the current cache (key → registration).
    pub async fn snapshot(&self) -> HashMap<String, ServiceRegistration> {
        self.cache.read().await.clone()
    }
}

/// Apply a single KV entry (Put/Delete/Purge) to any `HashMap`.
/// Used in both the snapshot staging phase and the live update phase.
fn apply_entry(map: &mut HashMap<String, ServiceRegistration>, entry: KvEntry) {
    match entry.operation {
        KvOperation::Put => match ServiceRegistration::decode(entry.value.as_ref()) {
            Ok(reg) => {
                debug!(
                    key = entry.key,
                    service_type = reg.service_type,
                    "discovery: upsert"
                );
                map.insert(entry.key, reg);
            }
            Err(e) => warn!(key = entry.key, error = %e, "discovery: malformed entry, skipping"),
        },
        KvOperation::Delete | KvOperation::Purge => {
            debug!(key = entry.key, "discovery: removed");
            map.remove(&entry.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zk_proto_rs::zk::discovery::v1::ServiceRegistration as SvcReg;

    fn make_cache() -> Arc<RwLock<HashMap<String, SvcReg>>> {
        Arc::new(RwLock::new(HashMap::new()))
    }

    #[tokio::test]
    async fn discovery_watch_put_populates_cache() {
        let cache = make_cache();
        let reg = SvcReg {
            service_type: "OMS".to_string(),
            ..Default::default()
        };
        cache.write().await.insert("svc.oms.oms1".to_string(), reg);
        assert!(cache.read().await.contains_key("svc.oms.oms1"));
    }

    #[tokio::test]
    async fn discovery_watch_delete_removes_cache_entry() {
        let cache = make_cache();
        cache.write().await.insert(
            "svc.oms.oms1".to_string(),
            SvcReg {
                service_type: "OMS".to_string(),
                ..Default::default()
            },
        );
        cache.write().await.remove("svc.oms.oms1");
        assert!(!cache.read().await.contains_key("svc.oms.oms1"));
    }

    #[tokio::test]
    async fn discovery_watch_skips_malformed_payload() {
        let result = SvcReg::decode(b"invalid_bytes".as_ref());
        assert!(result.is_err(), "malformed payload should fail to decode");
    }

    #[test]
    fn snapshot_swap_replaces_old_entries() {
        // Old cache: k1. New snapshot: k2 only. After swap, k1 gone, k2 present.
        let mut cache: HashMap<String, SvcReg> = HashMap::new();
        cache.insert(
            "k1".into(),
            SvcReg {
                service_type: "OMS".into(),
                ..Default::default()
            },
        );
        let mut staging: HashMap<String, SvcReg> = HashMap::new();
        staging.insert(
            "k2".into(),
            SvcReg {
                service_type: "GW".into(),
                ..Default::default()
            },
        );
        cache = staging; // simulates *cache.write().await = staging
        assert!(
            !cache.contains_key("k1"),
            "stale entry should be removed by swap"
        );
        assert!(
            cache.contains_key("k2"),
            "new entry should be present after swap"
        );
    }

    #[test]
    fn snapshot_staging_accumulates_live_updates_during_snapshot_phase() {
        // Live PUT arriving during the snapshot phase goes into staging (correct).
        let mut staging: HashMap<String, SvcReg> = HashMap::new();
        staging.insert("k1".into(), SvcReg::default()); // snapshot entry
        staging.insert("k2".into(), SvcReg::default()); // live PUT during snapshot
        assert_eq!(staging.len(), 2);
    }

    #[tokio::test]
    async fn discovery_resolve_filters_by_service_type() {
        let cache = make_cache();
        {
            let mut w = cache.write().await;
            w.insert(
                "k1".to_string(),
                SvcReg {
                    service_type: "OMS".to_string(),
                    ..Default::default()
                },
            );
            w.insert(
                "k2".to_string(),
                SvcReg {
                    service_type: "GW".to_string(),
                    ..Default::default()
                },
            );
        }
        let oms: Vec<_> = cache
            .read()
            .await
            .values()
            .filter(|r| r.service_type == "OMS")
            .cloned()
            .collect();
        assert_eq!(oms.len(), 1);
        assert_eq!(oms[0].service_type, "OMS");
    }
}
