use std::collections::HashMap;

use async_nats::jetstream::kv::Store;
use async_trait::async_trait;
use futures::StreamExt;
use tracing::{debug, warn};

use zk_rtmd_rs::{
    sub_manager::{RtmdLease, SubInterestChange, SubInterestSource},
    types::{RtmdError, StreamKey},
};

pub struct NatsKvSubSource {
    store: Store,
    venue: String,
    /// Persistent watcher; created lazily on first call to next_change().
    /// Reused across all subsequent calls to avoid replaying the full KV snapshot.
    watcher: tokio::sync::Mutex<Option<async_nats::jetstream::kv::Watch>>,
    /// Cache of last-seen StreamKey per subscription_id, needed to reconstruct
    /// Removed events when a Delete/Purge arrives (deleted entries carry no value).
    /// HashMap<subscription_id, (subscriber_id, StreamKey)>
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
            let key = match key_result {
                Ok(k) => k,
                Err(e) => {
                    warn!(error = %e, "KV keys stream error");
                    continue;
                }
            };
            if let Some(lease) = self.get_lease(&key).await {
                if self.matches_venue(&lease) {
                    leases.push(lease);
                }
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
                            if let Ok(lease) =
                                serde_json::from_slice::<RtmdLease>(&entry.value)
                            {
                                if self.matches_venue(&lease) {
                                    // Update key_cache so we can reconstruct Removed events later.
                                    let stream_key = lease.stream_key();
                                    self.key_cache.lock().await.insert(
                                        lease.subscription_id.clone(),
                                        (lease.subscriber_id.clone(), stream_key),
                                    );
                                    return Ok(SubInterestChange::Added(lease));
                                }
                            }
                        }
                        Operation::Delete | Operation::Purge => {
                            // Key format: sub.<scope>.<subscriber_id>.<subscription_id>
                            let parts: Vec<&str> = entry.key.splitn(4, '.').collect();
                            if parts.len() == 4 {
                                let subscription_id = parts[3];
                                if let Some((subscriber_id, stream_key)) =
                                    self.key_cache.lock().await.remove(subscription_id)
                                {
                                    return Ok(SubInterestChange::Removed {
                                        subscriber_id,
                                        subscription_id: subscription_id.to_string(),
                                        stream_key,
                                    });
                                }
                            }
                            debug!(key = %entry.key, "KV delete received — no cached entry, skipping");
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
    async fn get_lease(&self, key: &str) -> Option<RtmdLease> {
        match self.store.get(key).await {
            Ok(Some(bytes)) => serde_json::from_slice::<RtmdLease>(&bytes).ok(),
            _ => None,
        }
    }

    fn matches_venue(&self, lease: &RtmdLease) -> bool {
        lease.venue == self.venue
    }
}
