use serde::{Deserialize, Serialize};

use zk_infra_rs::bootstrap::{BootstrapConfigError, PilotPayload, ServiceBootstrap};

// ── Bootstrap config ─────────────────────────────────────────────────────────

/// Minimal deployment-owned startup inputs.
///
/// Loaded from `ZK_*` env vars. Contains only what the process needs to
/// reach Pilot/NATS and identify itself.
#[derive(Debug, Clone, Deserialize)]
pub struct OmsBootstrapConfig {
    /// Unique OMS instance identifier (e.g. "oms_dev_1").
    pub oms_id: String,

    /// Advertised gRPC host for service registration endpoint.
    #[serde(default = "default_grpc_host")]
    pub grpc_host: String,

    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    /// Bootstrap token for Pilot registration. Empty = direct mode (default).
    #[serde(default)]
    pub bootstrap_token: String,

    /// Instance type for Pilot registration (default "OMS").
    #[serde(default = "default_instance_type_oms")]
    pub instance_type: String,

    /// Environment tag for Pilot registration (default "dev").
    #[serde(default = "default_env")]
    pub env: String,
}

// ── Provided config ──────────────────────────────────────────────────────────

/// Pilot-managed config (or direct-mode equivalent loaded from env vars).
///
/// In Pilot mode, deserialized from the JSON payload returned by
/// `BootstrapRegisterResponse`. In direct mode, loaded from `ZK_*` env vars.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OmsProvidedConfig {
    // ── Connectivity ─────────────────────────────────────────────────────
    #[serde(default)]
    pub redis_url: String,
    #[serde(default)]
    pub pg_url: String,

    // ── OMS core tunables ────────────────────────────────────────────────
    /// Whether to handle exchange orders not submitted through this OMS.
    #[serde(default)]
    pub handle_external_orders: bool,

    /// Enable built-in risk checks (max order size, balance pre-check).
    #[serde(default = "default_true")]
    pub risk_check_enabled: bool,

    /// Maximum number of entries in the `pending_order_reports` cache (per gw_key).
    #[serde(default = "default_max_pending_reports")]
    pub max_pending_reports: usize,

    /// Maximum number of orders held in the `OrderManager` LRU cache.
    #[serde(default = "default_max_cached_orders")]
    pub max_cached_orders: usize,

    // ── Actor tuning ─────────────────────────────────────────────────────
    /// Capacity of the `OmsCommand` channel between gRPC/NATS handlers and
    /// the writer task. Default 4096 gives ~4 ms of buffering at 1M ops/s.
    #[serde(default = "default_cmd_channel_buf")]
    pub cmd_channel_buf: usize,

    // ── Service registry ─────────────────────────────────────────────────
    /// KV heartbeat interval in seconds (default 10). Entry TTL in the
    /// registry bucket is 3× this value so one missed beat is tolerated.
    #[serde(default = "default_kv_heartbeat_secs")]
    pub kv_heartbeat_secs: u64,

    /// NATS KV prefix for gateway registrations (default "svc.gw").
    /// OMS watches `{gateway_kv_prefix}.>` to discover live gateways.
    #[serde(default = "default_gw_kv_prefix")]
    pub gateway_kv_prefix: String,

    // ── Periodic task intervals ──────────────────────────────────────────
    /// Order resync interval in seconds (default 60).
    #[serde(default = "default_resync_interval_secs")]
    pub order_resync_interval_secs: u64,

    /// Balance resync interval in seconds (default 60).
    #[serde(default = "default_resync_interval_secs")]
    pub balance_resync_interval_secs: u64,

    /// Position recheck interval in seconds (default 30).
    #[serde(default = "default_position_recheck_interval_secs")]
    pub position_recheck_interval_secs: u64,

    /// Cleanup interval in seconds (default 600).
    #[serde(default = "default_cleanup_interval_secs")]
    pub cleanup_interval_secs: u64,

    // ── Gateway executor tuning ──────────────────────────────────────────
    /// Number of gateway executor shards (default 16). Orders are routed
    /// by `order_id % gw_exec_shard_count`.
    #[serde(default = "default_gw_exec_shard_count")]
    pub gw_exec_shard_count: usize,

    /// Per-shard queue capacity for gateway executor (default 256).
    #[serde(default = "default_gw_exec_queue_capacity")]
    pub gw_exec_queue_capacity: usize,

    // ── Latency metrics ──────────────────────────────────────────────────
    /// How often (seconds) OMS flushes accumulated latency samples to NATS (default 2).
    #[serde(default = "default_metrics_interval_secs")]
    pub metrics_interval_secs: u64,

    /// Max in-flight orders tracked for latency (default 5000).
    #[serde(default = "default_metrics_max_pending")]
    pub metrics_max_pending: usize,

    /// Max completed latency records buffered between flushes (default 10000).
    #[serde(default = "default_metrics_max_complete")]
    pub metrics_max_complete: usize,
}

// ── Runtime config ───────────────────────────────────────────────────────────

/// Effective assembled config used at runtime.
///
/// Assembled from `OmsBootstrapConfig` + `OmsProvidedConfig` by
/// [`OmsService::assemble_runtime_config`]. Reported via `GetCurrentConfig`.
#[derive(Debug, Clone, Serialize)]
pub struct OmsRuntimeConfig {
    // ── Identity (from bootstrap) ────────────────────────────────────────
    pub oms_id: String,
    pub grpc_host: String,
    pub grpc_port: u16,
    pub nats_url: String,
    pub env: String,

    // ── Provided (from Pilot or direct) ──────────────────────────────────
    pub redis_url: String,
    pub pg_url: String,
    pub handle_external_orders: bool,
    pub risk_check_enabled: bool,
    pub max_pending_reports: usize,
    pub max_cached_orders: usize,
    pub cmd_channel_buf: usize,
    pub kv_heartbeat_secs: u64,
    pub gateway_kv_prefix: String,
    pub order_resync_interval_secs: u64,
    pub balance_resync_interval_secs: u64,
    pub position_recheck_interval_secs: u64,
    pub cleanup_interval_secs: u64,
    pub gw_exec_shard_count: usize,
    pub gw_exec_queue_capacity: usize,
    pub metrics_interval_secs: u64,
    pub metrics_max_pending: usize,
    pub metrics_max_complete: usize,
}

// ── ServiceBootstrap implementation ──────────────────────────────────────────

/// Marker type for OMS service bootstrap.
pub struct OmsService;

impl ServiceBootstrap for OmsService {
    type BootstrapConfig = OmsBootstrapConfig;
    type ProvidedConfig = OmsProvidedConfig;
    type RuntimeConfig = OmsRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: OmsProvidedConfig = serde_json::from_str(&payload.runtime_config_json)?;
        if cfg.redis_url.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "redis_url".into(),
            });
        }
        if cfg.pg_url.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "pg_url".into(),
            });
        }
        Ok(cfg)
    }

    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: OmsProvidedConfig = envy::prefixed("ZK_")
            .from_env()
            .map_err(|e| BootstrapConfigError::Custom(e.to_string()))?;
        if cfg.redis_url.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "redis_url".into(),
            });
        }
        if cfg.pg_url.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "pg_url".into(),
            });
        }
        Ok(cfg)
    }

    fn assemble_runtime_config(
        bootstrap: &Self::BootstrapConfig,
        provided: Self::ProvidedConfig,
    ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
        if provided.gw_exec_shard_count < 1 {
            return Err(BootstrapConfigError::Validation(
                "gw_exec_shard_count must be >= 1".into(),
            ));
        }
        if provided.gw_exec_queue_capacity < 1 {
            return Err(BootstrapConfigError::Validation(
                "gw_exec_queue_capacity must be >= 1".into(),
            ));
        }
        Ok(OmsRuntimeConfig {
            oms_id: bootstrap.oms_id.clone(),
            grpc_host: bootstrap.grpc_host.clone(),
            grpc_port: bootstrap.grpc_port,
            nats_url: bootstrap.nats_url.clone(),
            env: bootstrap.env.clone(),
            redis_url: provided.redis_url,
            pg_url: provided.pg_url,
            handle_external_orders: provided.handle_external_orders,
            risk_check_enabled: provided.risk_check_enabled,
            max_pending_reports: provided.max_pending_reports,
            max_cached_orders: provided.max_cached_orders,
            cmd_channel_buf: provided.cmd_channel_buf,
            kv_heartbeat_secs: provided.kv_heartbeat_secs,
            gateway_kv_prefix: provided.gateway_kv_prefix,
            order_resync_interval_secs: provided.order_resync_interval_secs,
            balance_resync_interval_secs: provided.balance_resync_interval_secs,
            position_recheck_interval_secs: provided.position_recheck_interval_secs,
            cleanup_interval_secs: provided.cleanup_interval_secs,
            gw_exec_shard_count: provided.gw_exec_shard_count,
            gw_exec_queue_capacity: provided.gw_exec_queue_capacity,
            metrics_interval_secs: provided.metrics_interval_secs,
            metrics_max_pending: provided.metrics_max_pending,
            metrics_max_complete: provided.metrics_max_complete,
        })
    }
}

// ── Loading ──────────────────────────────────────────────────────────────────

/// Paths to fields containing resolved secret material — redacted in GetCurrentConfig.
/// OmsRuntimeConfig contains no secrets (bootstrap_token stays in OmsBootstrapConfig).
pub const RESOLVED_SECRET_PATHS: &[&str] = &[];

/// Load bootstrap config from `ZK_*` environment variables.
pub fn load_bootstrap() -> Result<OmsBootstrapConfig, BootstrapConfigError> {
    envy::prefixed("ZK_")
        .from_env::<OmsBootstrapConfig>()
        .map_err(|e| BootstrapConfigError::Custom(e.to_string()))
}

// ── Defaults ─────────────────────────────────────────────────────────────────

fn default_nats_url() -> String {
    "nats://localhost:4222".into()
}
fn default_grpc_port() -> u16 {
    50051
}
fn default_grpc_host() -> String {
    "127.0.0.1".into()
}
fn default_true() -> bool {
    true
}
fn default_max_pending_reports() -> usize {
    1_000
}
fn default_max_cached_orders() -> usize {
    100_000
}
fn default_cmd_channel_buf() -> usize {
    4_096
}
fn default_kv_heartbeat_secs() -> u64 {
    10
}
fn default_gw_kv_prefix() -> String {
    "svc.gw".into()
}
fn default_instance_type_oms() -> String {
    "OMS".into()
}
fn default_env() -> String {
    "dev".into()
}
fn default_resync_interval_secs() -> u64 {
    60
}
fn default_position_recheck_interval_secs() -> u64 {
    30
}
fn default_cleanup_interval_secs() -> u64 {
    600
}
fn default_gw_exec_shard_count() -> usize {
    16
}
fn default_gw_exec_queue_capacity() -> usize {
    256
}
fn default_metrics_interval_secs() -> u64 {
    2
}
fn default_metrics_max_pending() -> usize {
    5_000
}
fn default_metrics_max_complete() -> usize {
    10_000
}
