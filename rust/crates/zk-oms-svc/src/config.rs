use serde::Deserialize;

/// Environment-variable-driven config for the OMS service.
///
/// All fields map from `ZK_*` env vars (case-insensitive via `envy`).
///
/// # Extensibility (risk checking — TODO Phase 3.5)
///
/// `risk_check_enabled` currently gates the existing `OmsCore` risk checks
/// (max order size, panic mode, balance check). A future `RiskPolicy` trait
/// will allow pluggable per-instrument / per-account risk rules loaded from
/// PG at startup and refreshed via `ReloadConfig`.
///
/// # Config readability (Pilot integration — TODO Phase 3.5)
///
/// The current config is exposed read-only via `GetConfig` RPC (see
/// `grpc_handler::OmsGrpcHandler::get_config`). Once the pilot service is
/// live, `PilotService` will query this to display per-OMS rule summaries.
#[derive(Debug, Clone, Deserialize)]
pub struct OmsSvcConfig {
    // ── Connectivity ───────────────────────────────────────────────────────
    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    pub redis_url: String,

    pub pg_url: String,

    // ── Identity ───────────────────────────────────────────────────────────
    /// Unique OMS instance identifier (e.g. "oms_dev_1").
    pub oms_id: String,

    // ── gRPC server ────────────────────────────────────────────────────────
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    /// Advertised gRPC host for service registration endpoint.
    #[serde(default = "default_grpc_host")]
    pub grpc_host: String,

    // ── OMS core tunables ──────────────────────────────────────────────────
    /// Whether to handle exchange orders not submitted through this OMS.
    #[serde(default)]
    pub handle_external_orders: bool,

    /// Enable built-in risk checks (max order size, balance pre-check).
    #[serde(default = "default_true")]
    pub risk_check_enabled: bool,

    /// Maximum number of entries in the `pending_order_reports` cache (per gw_key).
    /// Oldest entry is dropped on overflow to prevent unbounded memory growth.
    #[serde(default = "default_max_pending_reports")]
    pub max_pending_reports: usize,

    /// Maximum number of orders held in the `OrderManager` LRU cache.
    #[serde(default = "default_max_cached_orders")]
    pub max_cached_orders: usize,

    // ── Actor tuning ───────────────────────────────────────────────────────
    /// Capacity of the `OmsCommand` channel between gRPC/NATS handlers and
    /// the writer task. Default 4096 gives ~4 ms of buffering at 1M ops/s.
    #[serde(default = "default_cmd_channel_buf")]
    pub cmd_channel_buf: usize,

    // ── Service registry ───────────────────────────────────────────────────
    /// KV heartbeat interval in seconds (default 10). Entry TTL in the
    /// registry bucket is 3× this value so one missed beat is tolerated.
    #[serde(default = "default_kv_heartbeat_secs")]
    pub kv_heartbeat_secs: u64,

    /// NATS KV prefix for gateway registrations (default "svc.gw").
    /// OMS watches `{gateway_kv_prefix}.>` to discover live gateways.
    #[serde(default = "default_gw_kv_prefix")]
    pub gateway_kv_prefix: String,

    // ── Pilot bootstrap ─────────────────────────────────────────────────
    /// Bootstrap token for Pilot registration. Empty = direct mode (default).
    #[serde(default)]
    pub bootstrap_token: String,

    /// Instance type for Pilot registration (default "OMS").
    #[serde(default = "default_instance_type_oms")]
    pub instance_type: String,

    /// Environment tag for Pilot registration (default "dev").
    #[serde(default = "default_env")]
    pub env: String,

    // ── Periodic task intervals ────────────────────────────────────────────
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

    // ── Gateway executor tuning ────────────────────────────────────────
    /// Number of gateway executor shards (default 16). Orders are routed
    /// by `order_id % gw_exec_shard_count`.
    #[serde(default = "default_gw_exec_shard_count")]
    pub gw_exec_shard_count: usize,

    /// Per-shard queue capacity for gateway executor (default 256).
    #[serde(default = "default_gw_exec_queue_capacity")]
    pub gw_exec_queue_capacity: usize,

    // ── Latency metrics ──────────────────────────────────────────────
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

/// Load config from environment variables.
pub fn load() -> Result<OmsSvcConfig, envy::Error> {
    let cfg = envy::prefixed("ZK_").from_env::<OmsSvcConfig>()?;
    assert!(
        cfg.gw_exec_shard_count >= 1,
        "ZK_GW_EXEC_SHARD_COUNT must be >= 1"
    );
    assert!(
        cfg.gw_exec_queue_capacity >= 1,
        "ZK_GW_EXEC_QUEUE_CAPACITY must be >= 1"
    );
    Ok(cfg)
}
