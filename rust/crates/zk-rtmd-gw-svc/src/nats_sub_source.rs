use std::collections::{HashMap, VecDeque};

use async_nats::jetstream::kv::Store;
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, info, warn};

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
        let channel = channel_type_from_wire(&self.channel_type, self.channel_param.as_deref())?;
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

fn channel_type_from_wire(channel_type: &str, channel_param: Option<&str>) -> Option<ChannelType> {
    match channel_type {
        "tick" => Some(ChannelType::Tick),
        "kline" => Some(ChannelType::Kline {
            interval: channel_param.unwrap_or("1m").to_string(),
        }),
        "orderbook" => Some(ChannelType::OrderBook {
            depth: channel_param.and_then(|s| s.parse().ok()),
        }),
        "funding" => Some(ChannelType::Funding),
        other => {
            warn!(channel_type = other, "unknown channel_type in lease, skipping");
            None
        }
    }
}

/// Cached state for a KV key. Holds the fields that define upstream subscription identity.
/// Used to detect whether a Put is a simple heartbeat/refresh (no-op) or a transport change
/// (old stream removed + new stream added).
struct CachedLease {
    subscriber_id: String,
    stream_key: StreamKey,
    instrument_exch: String,
    venue: String,
}

/// Result of evaluating a Put entry against the current cache.
enum PutOutcome {
    /// Key not previously seen — emit Added.
    New,
    /// Key seen; effective subscription fields unchanged — pure refresh, emit nothing.
    Refresh,
    /// Key seen; effective fields changed — emit Removed for old, then Added for new.
    TransportChange { old_subscriber_id: String, old_stream_key: StreamKey },
}

fn classify_put(cached: Option<&CachedLease>, new_lease: &RtmdLease) -> PutOutcome {
    match cached {
        None => PutOutcome::New,
        Some(c) => {
            if c.stream_key == new_lease.stream_key()
                && c.instrument_exch == new_lease.instrument_exch
                && c.venue == new_lease.venue
            {
                PutOutcome::Refresh
            } else {
                PutOutcome::TransportChange {
                    old_subscriber_id: c.subscriber_id.clone(),
                    old_stream_key: c.stream_key.clone(),
                }
            }
        }
    }
}

pub struct NatsKvSubSource {
    store: Store,
    venue: String,
    /// Persistent watcher; created lazily on first call to next_change().
    watcher: tokio::sync::Mutex<Option<async_nats::jetstream::kv::Watch>>,
    /// Per KV-key cache of effective subscription fields.
    ///
    /// Serves two purposes:
    ///   1. Dedup: Puts that don't change stream/transport fields are silently skipped
    ///      (lease heartbeats), so refcounts never inflate.
    ///   2. Transport change: if instrument_exch or stream_key changes under the same
    ///      KV key, we emit a synthetic Removed for the old lease before Added for the new.
    ///   3. Delete reconstruction: Delete/Purge entries carry no value; we look up the
    ///      StreamKey by the full KV key.
    key_cache: tokio::sync::Mutex<HashMap<String, CachedLease>>,
    /// One-deep buffer for events that must be returned before the next watcher entry.
    /// Used to emit a synthetic Removed before the Added on transport-field changes.
    pending: tokio::sync::Mutex<VecDeque<SubInterestChange>>,
}

impl NatsKvSubSource {
    pub fn new(store: Store, venue: String) -> Self {
        Self {
            store,
            venue,
            watcher: tokio::sync::Mutex::new(None),
            key_cache: tokio::sync::Mutex::new(HashMap::new()),
            pending: tokio::sync::Mutex::new(VecDeque::new()),
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
                // Populate key_cache so the watch-loop replay treats these as refreshes.
                self.key_cache.lock().await.insert(
                    kv_key,
                    CachedLease {
                        subscriber_id: lease.subscriber_id.clone(),
                        stream_key: lease.stream_key(),
                        instrument_exch: lease.instrument_exch.clone(),
                        venue: lease.venue.clone(),
                    },
                );
                leases.push(lease);
            }
        }
        Ok(leases)
    }

    async fn next_change(&self) -> Result<SubInterestChange, RtmdError> {
        // Drain pending events before pulling from the watcher.
        if let Some(change) = self.pending.lock().await.pop_front() {
            return Ok(change);
        }

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
            // Re-check pending after each watcher iteration (another task could have added one).
            if let Some(change) = self.pending.lock().await.pop_front() {
                return Ok(change);
            }

            match watcher.next().await {
                None => return Err(RtmdError::ChannelClosed),
                Some(Ok(entry)) => {
                    use async_nats::jetstream::kv::Operation;
                    match entry.operation {
                        Operation::Put => {
                            let new_lease = match self.parse_wire(&entry.key, &entry.value) {
                                Some(l) => l,
                                None => continue,
                            };

                            let mut cache = self.key_cache.lock().await;
                            let outcome = classify_put(cache.get(&entry.key), &new_lease);

                            match outcome {
                                PutOutcome::Refresh => {
                                    debug!(key = %entry.key, "KV put — lease refresh, skipping");
                                    // Update cached expiry metadata only (no stream fields changed).
                                }
                                PutOutcome::New => {
                                    cache.insert(
                                        entry.key,
                                        CachedLease {
                                            subscriber_id: new_lease.subscriber_id.clone(),
                                            stream_key: new_lease.stream_key(),
                                            instrument_exch: new_lease.instrument_exch.clone(),
                                            venue: new_lease.venue.clone(),
                                        },
                                    );
                                    drop(cache);
                                    return Ok(SubInterestChange::Added(new_lease));
                                }
                                PutOutcome::TransportChange { old_subscriber_id, old_stream_key } => {
                                    // Update cache to reflect new transport fields.
                                    cache.insert(
                                        entry.key.clone(),
                                        CachedLease {
                                            subscriber_id: new_lease.subscriber_id.clone(),
                                            stream_key: new_lease.stream_key(),
                                            instrument_exch: new_lease.instrument_exch.clone(),
                                            venue: new_lease.venue.clone(),
                                        },
                                    );
                                    drop(cache);
                                    // Queue the Added; return Removed first so the sub_manager
                                    // tears down the old upstream subscription before creating new.
                                    info!(
                                        key = %entry.key,
                                        old_instrument_code = %old_stream_key.instrument_code,
                                        new_instrument_exch = %new_lease.instrument_exch,
                                        "KV put — transport change, emitting Removed+Added"
                                    );
                                    self.pending.lock().await.push_back(SubInterestChange::Added(new_lease));
                                    return Ok(SubInterestChange::Removed {
                                        subscriber_id: old_subscriber_id,
                                        subscription_id: entry.key,
                                        stream_key: old_stream_key,
                                    });
                                }
                            }
                        }
                        Operation::Delete | Operation::Purge => {
                            // Look up by the full KV key — no parsing required.
                            if let Some(cached) = self.key_cache.lock().await.remove(&entry.key) {
                                return Ok(SubInterestChange::Removed {
                                    subscriber_id: cached.subscriber_id,
                                    subscription_id: entry.key,
                                    stream_key: cached.stream_key,
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

// ── Unit tests for pure logic ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zk_rtmd_rs::sub_manager::RtmdLease;

    fn make_lease(instrument_code: &str, channel: ChannelType, instrument_exch: &str) -> RtmdLease {
        RtmdLease {
            subscriber_id: "strat_a".to_string(),
            subscription_id: "sub.logical.strat_a.tick.BTC".to_string(),
            instrument_code: instrument_code.to_string(),
            channel,
            instrument_exch: instrument_exch.to_string(),
            venue: "simulator".to_string(),
            lease_expiry_ms: 9_999_999,
        }
    }

    #[test]
    fn test_classify_put_new_key() {
        let lease = make_lease("BTC-USDT", ChannelType::Tick, "BTC-USDT");
        assert!(matches!(classify_put(None, &lease), PutOutcome::New));
    }

    #[test]
    fn test_classify_put_refresh_no_change() {
        let lease = make_lease("BTC-USDT", ChannelType::Tick, "BTC-USDT");
        let cached = CachedLease {
            subscriber_id: "strat_a".to_string(),
            stream_key: lease.stream_key(),
            instrument_exch: "BTC-USDT".to_string(),
            venue: "simulator".to_string(),
        };
        assert!(matches!(classify_put(Some(&cached), &lease), PutOutcome::Refresh));
    }

    #[test]
    fn test_classify_put_transport_change_instrument_exch() {
        let old_lease = make_lease("BTC-USDT", ChannelType::Tick, "OLD-EXCH");
        let new_lease = make_lease("BTC-USDT", ChannelType::Tick, "NEW-EXCH");
        let cached = CachedLease {
            subscriber_id: "strat_a".to_string(),
            stream_key: old_lease.stream_key(),
            instrument_exch: "OLD-EXCH".to_string(),
            venue: "simulator".to_string(),
        };
        assert!(matches!(
            classify_put(Some(&cached), &new_lease),
            PutOutcome::TransportChange { .. }
        ));
    }

    #[test]
    fn test_channel_type_parsing() {
        assert_eq!(channel_type_from_wire("tick", None), Some(ChannelType::Tick));
        assert_eq!(
            channel_type_from_wire("kline", Some("5m")),
            Some(ChannelType::Kline { interval: "5m".to_string() })
        );
        assert_eq!(
            channel_type_from_wire("kline", None),
            Some(ChannelType::Kline { interval: "1m".to_string() })
        );
        assert_eq!(
            channel_type_from_wire("orderbook", Some("10")),
            Some(ChannelType::OrderBook { depth: Some(10) })
        );
        assert_eq!(channel_type_from_wire("funding", None), Some(ChannelType::Funding));
        assert_eq!(channel_type_from_wire("unknown", None), None);
    }
}
