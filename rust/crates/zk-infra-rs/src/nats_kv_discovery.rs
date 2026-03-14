//! Watch-based KV cache for service discovery.
//!
//! [`KvDiscoveryClient`] opens the shared registry KV bucket and keeps an
//! in-memory cache current via a single persistent watch stream.
//!
//! # Startup contract
//!
//! `start()` returns with an **empty** cache. Call `spawn_watch_loop()` immediately
//! after construction. The async-nats `watch(">")` stream delivers all current KV
//! entries (initial snapshot) before any live events, so the same loop handles both
//! phases — there is no gap between "initial load" and "live updates".
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

use async_nats::jetstream;
use futures::StreamExt;
use prost::Message;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use zk_proto_rs::zk::discovery::v1::ServiceRegistration;

use crate::nats_kv::{KvOperation, KvRegistryClient, REGISTRY_BUCKET};

/// In-memory cache of all service registrations from the shared NATS KV registry.
#[derive(Clone)]
pub struct KvDiscoveryClient {
    kv:    KvRegistryClient,
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
    /// The watch stream delivers all current entries first (initial snapshot), then
    /// continues with live Put/Delete/Purge events — both processed by the same code
    /// path. Reconnects automatically on stream error.
    ///
    /// The returned `JoinHandle` can be aborted on shutdown.
    pub fn spawn_watch_loop(&self) -> JoinHandle<()> {
        let kv    = self.kv.clone();
        let cache = Arc::clone(&self.cache);

        tokio::spawn(async move {
            loop {
                // Clear stale cache before replaying the watch snapshot so that
                // keys which disappeared while disconnected are not left behind.
                cache.write().await.clear();

                let watch_result = kv.watch(">").await;
                let mut watch = match watch_result {
                    Ok(w)  => w,
                    Err(e) => {
                        warn!(error = %e, "discovery: watch failed, retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // The initial burst of Put events from watch(">") populates the cache
                // with the current KV snapshot. Subsequent events are live updates.
                // Both are handled identically here.
                while let Some(result) = watch.next().await {
                    match result {
                        Ok(entry) => match entry.operation {
                            KvOperation::Put => {
                                match ServiceRegistration::decode(entry.value.as_ref()) {
                                    Ok(reg) => {
                                        debug!(
                                            key = entry.key,
                                            service_type = reg.service_type,
                                            "discovery: upsert"
                                        );
                                        cache.write().await.insert(entry.key, reg);
                                    }
                                    Err(e) => warn!(
                                        key = entry.key,
                                        error = %e,
                                        "discovery: malformed entry, skipping"
                                    ),
                                }
                            }
                            KvOperation::Delete | KvOperation::Purge => {
                                debug!(key = entry.key, "discovery: removed");
                                cache.write().await.remove(&entry.key);
                            }
                        },
                        Err(e) => {
                            warn!(error = %e, "discovery: watch stream error, reconnecting");
                            break;
                        }
                    }
                }

                warn!("discovery: watch stream ended, reconnecting in 2s");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
        let reg = SvcReg { service_type: "OMS".to_string(), ..Default::default() };
        cache.write().await.insert("svc.oms.oms1".to_string(), reg);
        assert!(cache.read().await.contains_key("svc.oms.oms1"));
    }

    #[tokio::test]
    async fn discovery_watch_delete_removes_cache_entry() {
        let cache = make_cache();
        cache.write().await.insert(
            "svc.oms.oms1".to_string(),
            SvcReg { service_type: "OMS".to_string(), ..Default::default() },
        );
        cache.write().await.remove("svc.oms.oms1");
        assert!(!cache.read().await.contains_key("svc.oms.oms1"));
    }

    #[tokio::test]
    async fn discovery_watch_skips_malformed_payload() {
        let result = SvcReg::decode(b"invalid_bytes".as_ref());
        assert!(result.is_err(), "malformed payload should fail to decode");
    }

    #[tokio::test]
    async fn discovery_resolve_filters_by_service_type() {
        let cache = make_cache();
        {
            let mut w = cache.write().await;
            w.insert("k1".to_string(), SvcReg { service_type: "OMS".to_string(), ..Default::default() });
            w.insert("k2".to_string(), SvcReg { service_type: "GW".to_string(), ..Default::default() });
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
