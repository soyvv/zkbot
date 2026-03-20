use std::collections::HashMap;

use async_nats::jetstream::kv::Store;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, warn};

use zk_rtmd_rs::{
    sub_manager::{RtmdLease, SubInterestChange, SubInterestSource},
    types::{ChannelType, RtmdError, StreamKey},
};

/// Wire format written by the trading SDK (`RtmdInterestManager`).
/// Field names match the SDK's `RtmdInterestSpec` serialisation exactly.
#[derive(Deserialize)]
struct WireInterestSpec {
    subscriber_id: String,
    #[allow(dead_code)]
    scope: String,
    instrument_id: String,
    channel_type: String,
    channel_param: Option<String>,
    venue: String,
    instrument_exch: String,
    lease_expiry_ms: i64,
}

impl WireInterestSpec {
    /// Convert to `RtmdLease`. `kv_key` is used as the unique subscription identity.
    fn into_lease(self, kv_key: String) -> Option<RtmdLease> {
        let channel = match self.channel_type.as_str() {
            "tick" => ChannelType::Tick,
            "kline" => ChannelType::Kline {
                interval: self.channel_param.unwrap_or_else(|| "1m".to_string()),
            },
            "orderbook" => ChannelType::OrderBook {
                depth: self.channel_param.as_deref().and_then(|s| s.parse().ok()),
            },
            "funding" => ChannelType::Funding,
            other => {
                warn!(channel_type = other, "unknown channel_type in lease, skipping");
                return None;
            }
        };
        Some(RtmdLease {
            subscriber_id: self.subscriber_id,
            subscription_id: kv_key,
            instrument_code: self.instrument_id,
            channel,
            instrument_exch: self.instrument_exch,
            venue: self.venue,
            lease_expiry_ms: self.lease_expiry_ms,
        })
    }
}

pub struct NatsKvSubSource {
    store: Store,
    venue: String,
    /// Persistent watcher; created lazily on first call to next_change().
    /// Reused across all subsequent calls to avoid replaying the full KV snapshot.
    watcher: tokio::sync::Mutex<Option<async_nats::jetstream::kv::Watch>>,
    /// Tracks every KV key that has already produced an `Added` event.
    /// Key: full NATS KV key string (e.g. "sub.logical.strat_a.tick.BTC-USDT").
    /// Value: (subscriber_id, StreamKey) needed to reconstruct `Removed` events.
    ///
    /// Used for two purposes:
    ///   1. Dedup: subsequent Puts for an existing key (lease refreshes) are silently
    ///      skipped so refcounts don't grow on every heartbeat.
    ///   2. Delete reconstruction: NATS Delete/Purge entries carry no value; we look
    ///      up the StreamKey by the full KV key here.
    key_cache: tokio::sync::Mutex<HashMap<String, (String, StreamKey)>>,
}

impl NatsKvSubSource {
    pub fn new(store: Store, venue: String) -> Self {
        Self {
            store,
            venue,
            watcher: tokio::sync::Mutex::new(None),
            key_cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SubInterestSource for NatsKvSubSource {
    async fn snapshot(&self) -> Result<Vec<RtmdLease>, RtmdError> {
        let mut leases = Vec::new();
        let keys = self
            .store
            .keys()
            .await
            .map_err(|e| RtmdError::Internal(format!("KV keys: {e}")))?;

        tokio::pin!(keys);
        while let Some(key_result) = keys.next().await {
            let kv_key = match key_result {
                Ok(k) => k,
                Err(e) => {
                    warn!(error = %e, "KV keys stream error");
                    continue;
                }
            };
            if let Some(lease) = self.fetch_and_parse(&kv_key).await {
                // Populate key_cache so the watch loop replay skips these as refreshes.
                let stream_key = lease.stream_key();
                self.key_cache
                    .lock()
                    .await
                    .insert(kv_key, (lease.subscriber_id.clone(), stream_key));
                leases.push(lease);
            }
        }
        Ok(leases)
    }

    async fn next_change(&self) -> Result<SubInterestChange, RtmdError> {
        // Acquire watcher mutex and lazily initialise if needed.
        let mut watcher_guard = self.watcher.lock().await;
        if watcher_guard.is_none() {
            let w = self
                .store
                .watch_all()
                .await
                .map_err(|e| RtmdError::Internal(format!("KV watch: {e}")))?;
            *watcher_guard = Some(w);
        }
        let watcher = watcher_guard.as_mut().expect("watcher initialised above");

        loop {
            match watcher.next().await {
                None => return Err(RtmdError::ChannelClosed),
                Some(Ok(entry)) => {
                    use async_nats::jetstream::kv::Operation;
                    match entry.operation {
                        Operation::Put => {
                            // Skip if key is already tracked — this is a lease refresh, not
                            // a new subscription. Emitting Added again would inflate refcounts.
                            if self.key_cache.lock().await.contains_key(&entry.key) {
                                debug!(key = %entry.key, "KV put — lease refresh, skipping");
                                continue;
                            }
                            if let Some(lease) = self.parse_wire(&entry.key, &entry.value) {
                                let stream_key = lease.stream_key();
                                self.key_cache.lock().await.insert(
                                    entry.key,
                                    (lease.subscriber_id.clone(), stream_key),
                                );
                                return Ok(SubInterestChange::Added(lease));
                            }
                        }
                        Operation::Delete | Operation::Purge => {
                            // Look up by the full KV key — no parsing required.
                            if let Some((subscriber_id, stream_key)) =
                                self.key_cache.lock().await.remove(&entry.key)
                            {
                                return Ok(SubInterestChange::Removed {
                                    subscriber_id,
                                    subscription_id: entry.key,
                                    stream_key,
                                });
                            }
                            debug!(key = %entry.key, "KV delete — no cached entry, skipping");
                        }
                    }
                }
                Some(Err(e)) => {
                    warn!(error = %e, "KV watcher error");
                }
            }
        }
    }
}

impl NatsKvSubSource {
    /// Fetch a single key from the KV store and parse it as `WireInterestSpec`.
    async fn fetch_and_parse(&self, kv_key: &str) -> Option<RtmdLease> {
        let bytes = match self.store.get(kv_key).await {
            Ok(Some(b)) => b,
            _ => return None,
        };
        self.parse_wire(kv_key, &bytes)
    }

    /// Parse raw bytes as `WireInterestSpec` and convert to `RtmdLease`.
    /// Filters out leases for a different venue.
    fn parse_wire(&self, kv_key: &str, bytes: &[u8]) -> Option<RtmdLease> {
        let wire = match serde_json::from_slice::<WireInterestSpec>(bytes) {
            Ok(w) => w,
            Err(e) => {
                warn!(key = kv_key, error = %e, "failed to deserialise lease");
                return None;
            }
        };
        if wire.venue != self.venue {
            return None;
        }
        wire.into_lease(kv_key.to_string())
    }
}
