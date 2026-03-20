use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::types::{ChannelType, RtmdError, RtmdSubscriptionSpec, StreamKey};
use crate::venue_adapter::RtmdVenueAdapter;

pub type Result<T> = std::result::Result<T, RtmdError>;

/// A single active subscription lease held by a downstream consumer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtmdLease {
    pub subscriber_id: String,
    pub subscription_id: String,
    pub instrument_code: String,
    pub channel: ChannelType,
    /// Venue-native instrument code (resolved from refdata).
    pub instrument_exch: String,
    pub venue: String,
    /// Unix epoch ms when this lease expires.
    pub lease_expiry_ms: i64,
}

impl RtmdLease {
    pub fn stream_key(&self) -> StreamKey {
        StreamKey {
            instrument_code: self.instrument_code.clone(),
            channel: self.channel.clone(),
        }
    }

    pub fn to_spec(&self) -> RtmdSubscriptionSpec {
        RtmdSubscriptionSpec {
            stream_key: self.stream_key(),
            instrument_exch: self.instrument_exch.clone(),
            venue: self.venue.clone(),
        }
    }
}

/// A change event emitted by a SubInterestSource.
#[derive(Debug)]
pub enum SubInterestChange {
    /// A new or refreshed lease was added/updated.
    Added(RtmdLease),
    /// A lease was deleted or expired.
    Removed { subscriber_id: String, subscription_id: String, stream_key: StreamKey },
}

/// Abstract source of subscription interest changes.
///
/// In the standalone gateway this is backed by the NATS KV bucket `zk-rtmd-subs-v1`.
/// In AIO builds it can be backed by an in-process channel.
#[async_trait]
pub trait SubInterestSource: Send + Sync {
    /// Return the current snapshot of all active leases for this scope.
    async fn snapshot(&self) -> Result<Vec<RtmdLease>>;
    /// Block until the next interest change arrives.
    async fn next_change(&self) -> Result<SubInterestChange>;
}

/// Tracks active upstream subscriptions and drives subscribe/unsubscribe on the adapter.
///
/// Refcount rule:
///   - refcount 0 → 1: call adapter.subscribe()
///   - refcount 1 → 0: call adapter.unsubscribe()
pub struct SubscriptionManager {
    adapter: Arc<dyn RtmdVenueAdapter>,
    source: Arc<dyn SubInterestSource>,
    /// StreamKey → (refcount, one representative spec for the adapter call)
    state: Mutex<HashMap<StreamKey, (usize, RtmdSubscriptionSpec)>>,
}

impl SubscriptionManager {
    /// Create a new manager wrapping the given adapter and interest source.
    pub fn new(adapter: Arc<dyn RtmdVenueAdapter>, source: Arc<dyn SubInterestSource>) -> Self {
        Self { adapter, source, state: Mutex::new(HashMap::new()) }
    }

    /// On startup: snapshot KV, reconcile with adapter.snapshot_active().
    /// Subscribes any stream that has interest but isn't yet active upstream.
    pub async fn reconcile_on_start(&self) -> Result<()> {
        let leases = self.source.snapshot().await?;
        let active = self.adapter.snapshot_active().await?;
        let active_keys: std::collections::HashSet<StreamKey> =
            active.iter().map(|s| s.stream_key.clone()).collect();

        // Build list of specs that need subscribing, update refcounts under the lock,
        // then drop the lock before calling adapter.subscribe() across the async boundary.
        let to_subscribe: Vec<(StreamKey, RtmdSubscriptionSpec)> = {
            let mut state = self.state.lock().await;
            let mut to_subscribe = Vec::new();
            for lease in leases {
                let key = lease.stream_key();
                let spec = lease.to_spec();
                let entry = state.entry(key.clone()).or_insert((0, spec.clone()));
                entry.0 += 1;
                if entry.0 == 1 && !active_keys.contains(&key) {
                    to_subscribe.push((key, spec));
                }
            }
            to_subscribe
        }; // lock dropped here

        for (key, spec) in to_subscribe {
            info!(instrument_code = %key.instrument_code, "reconcile: subscribing upstream");
            if let Err(e) = self.adapter.subscribe(spec).await {
                warn!(error = %e, "reconcile: subscribe failed");
            }
        }
        Ok(())
    }

    /// Process a single interest change event.
    pub async fn apply_change(&self, change: SubInterestChange) -> Result<()> {
        match change {
            SubInterestChange::Added(lease) => {
                let key = lease.stream_key();
                let spec = lease.to_spec();
                let refcount = {
                    let mut state = self.state.lock().await;
                    let entry = state.entry(key.clone()).or_insert((0, spec.clone()));
                    entry.0 += 1;
                    let refcount = entry.0;
                    debug!(instrument_code = %key.instrument_code, refcount, "lease added");
                    refcount
                }; // lock dropped here
                if refcount == 1 {
                    info!(instrument_code = %key.instrument_code, "refcount 0→1: subscribing upstream");
                    if let Err(e) = self.adapter.subscribe(spec.clone()).await {
                        // Roll back refcount — upstream subscription was never established.
                        let mut state = self.state.lock().await;
                        if let Some(entry) = state.get_mut(&key) {
                            if entry.0 > 0 {
                                entry.0 -= 1;
                            }
                            if entry.0 == 0 {
                                state.remove(&key);
                            }
                        }
                        return Err(e);
                    }
                }
            }
            SubInterestChange::Removed { stream_key, .. } => {
                let unsubscribe = {
                    let mut state = self.state.lock().await;
                    if let Some(entry) = state.get_mut(&stream_key) {
                        if entry.0 > 0 {
                            entry.0 -= 1;
                            let remaining = entry.0;
                            debug!(instrument_code = %stream_key.instrument_code, refcount = remaining, "lease removed");
                            if remaining == 0 {
                                let spec = entry.1.clone();
                                state.remove(&stream_key);
                                Some((stream_key.clone(), spec))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }; // lock dropped here

                if let Some((key, spec)) = unsubscribe {
                    info!(instrument_code = %key.instrument_code, "refcount 1→0: unsubscribing upstream");
                    if let Err(e) = self.adapter.unsubscribe(spec.clone()).await {
                        // On failure: re-insert to keep state consistent —
                        // upstream subscription is still active.
                        let mut state = self.state.lock().await;
                        state.entry(key).or_insert((1, spec));
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }

    /// Run the watch loop. Calls next_change() in a loop and applies each change.
    /// Returns when the source is closed or an unrecoverable error occurs.
    pub async fn run_watch_loop(&self) {
        loop {
            match self.source.next_change().await {
                Ok(change) => {
                    if let Err(e) = self.apply_change(change).await {
                        warn!(error = %e, "failed to apply interest change");
                    }
                }
                Err(RtmdError::ChannelClosed) => {
                    info!("SubInterestSource closed, watch loop exiting");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "watch loop error, continuing");
                }
            }
        }
    }

    /// Current refcount for a stream key (for testing).
    pub async fn refcount(&self, key: &StreamKey) -> usize {
        self.state.lock().await.get(key).map(|(c, _)| *c).unwrap_or(0)
    }
}
