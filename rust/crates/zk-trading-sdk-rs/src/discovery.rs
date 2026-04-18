//! Registry-backed endpoint resolution.
//!
//! `OmsDiscovery` resolves `account_id -> OmsEndpoint` from the shared NATS KV
//! registry and keeps a derived account map in memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use zk_infra_rs::nats_kv_discovery::KvDiscoveryClient;
use zk_proto_rs::zk::discovery::v1::ServiceRegistration;

use crate::config::TradingClientConfig;
use crate::error::SdkError;

/// Resolved OMS endpoint for a specific account.
#[derive(Debug, Clone)]
pub struct OmsEndpoint {
    pub oms_id: String,
    pub grpc_address: String,
    pub grpc_authority: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MdgwEndpoint {
    pub service_id: String,
    pub venue: String,
    pub grpc_address: String,
    pub grpc_authority: Option<String>,
}

/// Snapshot-backed discovery helper for OMS and refdata resolution.
pub struct OmsDiscovery {
    inner: KvDiscoveryClient,
    watch: JoinHandle<()>,
    cache: Arc<RwLock<HashMap<i64, OmsEndpoint>>>,
}

impl OmsDiscovery {
    /// Start the registry watcher and wait for the initial snapshot settle period.
    ///
    /// Returns `(Self, bool)` where the bool indicates whether the initial snapshot
    /// was populated (`true`) or timed out empty (`false`). Callers that require
    /// specific accounts should treat `false` as a warning and verify resolution.
    pub async fn start(
        js: &jetstream::Context,
        config: &TradingClientConfig,
    ) -> Result<(Self, bool), SdkError> {
        let inner = KvDiscoveryClient::start(js).await?;
        let watch = inner.spawn_watch_loop();
        let cache = Arc::new(RwLock::new(HashMap::new()));
        let discovery = Self {
            inner,
            watch,
            cache,
        };

        let snapshot_ready = discovery.wait_for_required_snapshot(config).await?;
        discovery.refresh_account_cache(config.oms_id.as_deref()).await?;
        Ok((discovery, snapshot_ready))
    }

    pub async fn resolve_oms(&self, account_id: i64) -> Option<OmsEndpoint> {
        self.cache.read().await.get(&account_id).cloned()
    }

    pub async fn snapshot(&self) -> HashMap<i64, OmsEndpoint> {
        self.cache.read().await.clone()
    }

    pub async fn snapshot_service_type_counts(&self) -> HashMap<String, usize> {
        let snapshot = self.inner.snapshot().await;
        snapshot_service_type_counts(&snapshot)
    }

    pub async fn resolve_refdata(&self) -> Option<String> {
        let snapshot = self.inner.snapshot().await;
        resolve_refdata_endpoint(&snapshot)
    }

    pub async fn resolve_mdgw(&self, venue: &str) -> Option<MdgwEndpoint> {
        let snapshot = self.inner.snapshot().await;
        resolve_mdgw_endpoint(&snapshot, venue)
    }

    pub async fn refresh_account_cache(&self, oms_id: Option<&str>) -> Result<(), SdkError> {
        let snapshot = self.inner.snapshot().await;
        let account_map = build_account_map_with_target(&snapshot, oms_id)?;
        *self.cache.write().await = account_map;
        Ok(())
    }

    /// Poll until discovery has the dependencies required by the client config:
    /// all configured OMS accounts and, unless overridden, one live refdata service.
    /// Returns `true` when those requirements are satisfied; `false` on timeout.
    async fn wait_for_required_snapshot(
        &self,
        config: &TradingClientConfig,
    ) -> Result<bool, SdkError> {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(config.discovery_timeout_ms);
        while tokio::time::Instant::now() < deadline {
            let snapshot = self.inner.snapshot().await;
            if snapshot_satisfies_requirements(&snapshot, config)? {
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(false)
    }
}

pub fn snapshot_service_type_counts(
    snapshot: &HashMap<String, ServiceRegistration>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for reg in snapshot.values() {
        let service_type = if reg.service_type.is_empty() {
            "unknown".to_string()
        } else {
            reg.service_type.to_ascii_lowercase()
        };
        *counts.entry(service_type).or_insert(0) += 1;
    }
    counts
}

impl Drop for OmsDiscovery {
    fn drop(&mut self) {
        self.watch.abort();
    }
}

/// Build an `account_id -> OmsEndpoint` map from the registry snapshot.
///
/// Only live `svc.oms.*` entries are considered. Returns
/// `SdkError::DiscoveryConflict` if two OMS registrations claim the same account.
pub fn build_account_map(
    snapshot: &HashMap<String, ServiceRegistration>,
) -> Result<HashMap<i64, OmsEndpoint>, SdkError> {
    build_account_map_with_target(snapshot, None)
}

pub fn build_account_map_with_target(
    snapshot: &HashMap<String, ServiceRegistration>,
    target_oms_id: Option<&str>,
) -> Result<HashMap<i64, OmsEndpoint>, SdkError> {
    let mut map: HashMap<i64, OmsEndpoint> = HashMap::new();
    let now_ms = now_ms();

    for (key, reg) in snapshot {
        if !key.starts_with("svc.oms.") || !is_service_type(reg, "oms") {
            continue;
        }
        if reg.lease_expiry_ms > 0 && reg.lease_expiry_ms < now_ms {
            continue;
        }

        let endpoint = match reg.endpoint.as_ref() {
            Some(endpoint) if !endpoint.address.is_empty() => endpoint,
            _ => continue,
        };
        let grpc_authority = if endpoint.authority.is_empty() {
            None
        } else {
            Some(endpoint.authority.clone())
        };
        let oms_id = extract_service_id(key, reg);
        if let Some(target_oms_id) = target_oms_id {
            if oms_id != target_oms_id {
                continue;
            }
        }

        for &account_id in &reg.account_ids {
            if let Some(existing) = map.get(&account_id) {
                if existing.oms_id != oms_id {
                    return Err(SdkError::DiscoveryConflict(format!(
                        "account {account_id} claimed by both '{}' and '{oms_id}'",
                        existing.oms_id
                    )));
                }
                continue;
            }
            map.insert(
                account_id,
                OmsEndpoint {
                    oms_id: oms_id.clone(),
                    grpc_address: endpoint.address.clone(),
                    grpc_authority: grpc_authority.clone(),
                },
            );
        }
    }

    Ok(map)
}

/// Resolve the refdata gRPC endpoint from the registry snapshot.
pub fn resolve_refdata_endpoint(snapshot: &HashMap<String, ServiceRegistration>) -> Option<String> {
    let now_ms = now_ms();

    for (key, reg) in snapshot {
        if !key.starts_with("svc.refdata.") || !is_service_type(reg, "refdata") {
            continue;
        }
        if reg.lease_expiry_ms > 0 && reg.lease_expiry_ms < now_ms {
            continue;
        }

        if let Some(ep) = &reg.endpoint {
            if !ep.address.is_empty() {
                return Some(ep.address.clone());
            }
        }
    }

    None
}

pub fn resolve_mdgw_endpoint(
    snapshot: &HashMap<String, ServiceRegistration>,
    venue: &str,
) -> Option<MdgwEndpoint> {
    let now_ms = now_ms();

    for (key, reg) in snapshot {
        if !key.starts_with("svc.mdgw.") || !is_service_type(reg, "mdgw") {
            continue;
        }
        if reg.lease_expiry_ms > 0 && reg.lease_expiry_ms < now_ms {
            continue;
        }
        if !reg.venue.eq_ignore_ascii_case(venue) {
            continue;
        }

        if let Some(endpoint) = &reg.endpoint {
            if endpoint.address.is_empty() {
                continue;
            }
            let grpc_authority = if endpoint.authority.is_empty() {
                None
            } else {
                Some(endpoint.authority.clone())
            };
            return Some(MdgwEndpoint {
                service_id: extract_service_id(key, reg),
                venue: reg.venue.clone(),
                grpc_address: endpoint.address.clone(),
                grpc_authority,
            });
        }
    }

    None
}

fn extract_service_id(key: &str, reg: &ServiceRegistration) -> String {
    if !reg.service_id.is_empty() {
        return reg.service_id.clone();
    }
    key.rsplit('.').next().unwrap_or(key).to_string()
}

fn is_service_type(reg: &ServiceRegistration, expected: &str) -> bool {
    reg.service_type.eq_ignore_ascii_case(expected)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn snapshot_satisfies_requirements(
    snapshot: &HashMap<String, ServiceRegistration>,
    config: &TradingClientConfig,
) -> Result<bool, SdkError> {
    if snapshot.is_empty() {
        return Ok(false);
    }

    let account_map = build_account_map_with_target(snapshot, config.oms_id.as_deref())?;
    let accounts_ready = config
        .account_ids
        .iter()
        .all(|account_id| account_map.contains_key(account_id));

    let refdata_ready = config.refdata_grpc.is_some() || resolve_refdata_endpoint(snapshot).is_some();

    Ok(accounts_ready && refdata_ready)
}
