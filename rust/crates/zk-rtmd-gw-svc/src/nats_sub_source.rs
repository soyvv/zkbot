use async_nats::jetstream::kv::Store;
use async_trait::async_trait;
use futures::StreamExt;
use tracing::{debug, warn};

use zk_rtmd_rs::{
    sub_manager::{RtmdLease, SubInterestChange, SubInterestSource},
    types::RtmdError,
};

pub struct NatsKvSubSource {
    store: Store,
    venue: String,
}

impl NatsKvSubSource {
    pub fn new(store: Store, venue: String) -> Self {
        Self { store, venue }
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
        let mut watcher = self
            .store
            .watch_all()
            .await
            .map_err(|e| RtmdError::Internal(format!("KV watch: {e}")))?;

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
                                    return Ok(SubInterestChange::Added(lease));
                                }
                            }
                        }
                        Operation::Delete | Operation::Purge => {
                            // Phase 1 limitation: delete events don't carry the value,
                            // so we can't reconstruct the StreamKey without a local cache.
                            // Deletions are handled via lease TTL expiry — when no client
                            // refreshes the lease, the KV entry expires and the upstream
                            // subscription is eventually cleaned up on next reconcile.
                            debug!(key = %entry.key, "KV delete received — handled via TTL expiry");
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
