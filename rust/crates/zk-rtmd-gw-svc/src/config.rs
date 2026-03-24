use serde::{Deserialize, Serialize};
use zk_infra_rs::bootstrap::{BootstrapConfigError, PilotPayload, ServiceBootstrap};

// ── Bootstrap Config (deployment-owned, always from env) ─────────────────────

/// Minimal startup inputs supplied by deployment/orchestration.
///
/// Contains only what the process needs to reach NATS, Pilot, and identify
/// itself.  Loaded unconditionally from `ZK_*` env vars at startup.
#[derive(Debug, Clone)]
pub struct MdgwBootstrapConfig {
    /// Unique MDGW instance identifier (e.g. "mdgw_dev_1").
    pub mdgw_id: String,
    /// Environment tag (e.g. "dev", "staging", "prod").
    pub env: String,
    /// NATS server URL for connectivity.
    pub nats_url: String,
    /// gRPC listen port.
    pub grpc_port: u16,
    /// Advertised gRPC host for service registration.
    pub grpc_host: String,
    /// Bootstrap token for Pilot registration.  Empty = direct mode.
    pub bootstrap_token: String,
    /// Instance type for Pilot registration (default "MDGW").
    pub instance_type: String,
}

impl MdgwBootstrapConfig {
    /// Load bootstrap config from `ZK_*` environment variables.
    pub fn from_env() -> Result<Self, BootstrapConfigError> {
        Ok(Self {
            mdgw_id: std::env::var("ZK_MDGW_ID")
                .unwrap_or_else(|_| "mdgw_dev_1".into()),
            env: std::env::var("ZK_ENV").unwrap_or_else(|_| "dev".into()),
            nats_url: std::env::var("ZK_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".into()),
            grpc_port: std::env::var("ZK_GRPC_PORT")
                .unwrap_or_else(|_| "52051".into())
                .parse()
                .map_err(|_| {
                    BootstrapConfigError::Validation(
                        "ZK_GRPC_PORT must be a valid u16".into(),
                    )
                })?,
            grpc_host: std::env::var("ZK_GRPC_HOST")
                .unwrap_or_else(|_| "127.0.0.1".into()),
            bootstrap_token: std::env::var("ZK_BOOTSTRAP_TOKEN").unwrap_or_default(),
            instance_type: std::env::var("ZK_INSTANCE_TYPE")
                .unwrap_or_else(|_| "MDGW".into()),
        })
    }
}

// ── Provided Config (Pilot-owned, or loaded from env in direct mode) ─────────

/// Control-plane config returned by Pilot, or loaded from env vars in direct
/// mode.
///
/// Contains venue identity, venue-specific market-data config, and tuning
/// parameters.  In Pilot mode this is decoded from the bootstrap response JSON.
/// In direct mode it is assembled from `ZK_*` env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdgwProvidedConfig {
    /// Venue identifier (e.g. "okx", "binance", "simulator").  Required.
    pub venue: String,
    /// Opaque venue-specific market-data config blob.
    #[serde(default = "default_venue_config")]
    pub venue_config: serde_json::Value,
    /// Root directory for venue integration modules.
    #[serde(default)]
    pub venue_root: Option<String>,
    /// KV prefix for service registration (default "svc.mdgw").
    #[serde(default = "default_gateway_kv_prefix")]
    pub gateway_kv_prefix: String,
    /// KV bucket for RTMD subscription leases (default "zk-rtmd-subs-v1").
    #[serde(default = "default_rtmd_sub_bucket")]
    pub rtmd_sub_bucket: String,
    /// Lease TTL in seconds (default 60).
    #[serde(default = "default_sub_lease_ttl_s")]
    pub sub_lease_ttl_s: u64,
}

fn default_venue_config() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}
fn default_gateway_kv_prefix() -> String {
    "svc.mdgw".into()
}
fn default_rtmd_sub_bucket() -> String {
    "zk-rtmd-subs-v1".into()
}
fn default_sub_lease_ttl_s() -> u64 {
    60
}

// ── Runtime Config (effective, used by service startup) ──────────────────────

/// Effective assembled config used at runtime.
///
/// Merges bootstrap fields (connectivity, identity) with provided fields
/// (venue, tuning).  This is the single config struct that `main.rs` and all
/// downstream components consume.
#[derive(Debug, Clone, Serialize)]
pub struct MdgwRuntimeConfig {
    // From bootstrap
    pub mdgw_id: String,
    pub env: String,
    pub nats_url: String,
    pub grpc_port: u16,
    pub grpc_host: String,
    // From provided
    pub venue: String,
    pub venue_config: serde_json::Value,
    pub venue_root: Option<String>,
    pub gateway_kv_prefix: String,
    pub rtmd_sub_bucket: String,
    pub sub_lease_ttl_s: u64,
}

// ── ServiceBootstrap implementation ──────────────────────────────────────────

/// Marker type for the MDGW `ServiceBootstrap` implementation.
pub struct MdgwService;

impl ServiceBootstrap for MdgwService {
    type BootstrapConfig = MdgwBootstrapConfig;
    type ProvidedConfig = MdgwProvidedConfig;
    type RuntimeConfig = MdgwRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: MdgwProvidedConfig =
            serde_json::from_str(&payload.runtime_config_json)?;
        if cfg.venue.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "venue".into(),
            });
        }
        Ok(cfg)
    }

    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let venue = std::env::var("ZK_VENUE").map_err(|_| {
            BootstrapConfigError::MissingField {
                field: "venue (ZK_VENUE)".into(),
            }
        })?;
        if venue.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "venue (ZK_VENUE)".into(),
            });
        }

        let venue_config_str =
            std::env::var("ZK_VENUE_CONFIG").unwrap_or_else(|_| "{}".into());
        let venue_config: serde_json::Value =
            serde_json::from_str(&venue_config_str).map_err(|e| {
                BootstrapConfigError::Validation(format!(
                    "ZK_VENUE_CONFIG invalid JSON: {e}"
                ))
            })?;

        Ok(MdgwProvidedConfig {
            venue,
            venue_config,
            venue_root: std::env::var("ZK_VENUE_ROOT").ok(),
            gateway_kv_prefix: std::env::var("ZK_GATEWAY_KV_PREFIX")
                .unwrap_or_else(|_| "svc.mdgw".into()),
            rtmd_sub_bucket: std::env::var("ZK_RTMD_SUB_BUCKET")
                .unwrap_or_else(|_| "zk-rtmd-subs-v1".into()),
            sub_lease_ttl_s: std::env::var("ZK_SUB_LEASE_TTL_S")
                .unwrap_or_else(|_| "60".into())
                .parse()
                .map_err(|_| {
                    BootstrapConfigError::Validation(
                        "ZK_SUB_LEASE_TTL_S must be a valid u64".into(),
                    )
                })?,
        })
    }

    fn assemble_runtime_config(
        bootstrap: &Self::BootstrapConfig,
        provided: Self::ProvidedConfig,
    ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
        Ok(MdgwRuntimeConfig {
            mdgw_id: bootstrap.mdgw_id.clone(),
            env: bootstrap.env.clone(),
            nats_url: bootstrap.nats_url.clone(),
            grpc_port: bootstrap.grpc_port,
            grpc_host: bootstrap.grpc_host.clone(),
            venue: provided.venue,
            venue_config: provided.venue_config,
            venue_root: provided.venue_root,
            gateway_kv_prefix: provided.gateway_kv_prefix,
            rtmd_sub_bucket: provided.rtmd_sub_bucket,
            sub_lease_ttl_s: provided.sub_lease_ttl_s,
        })
    }
}
