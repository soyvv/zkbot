//! Engine service configuration loaded from `ZK_*` environment variables.

use serde::Deserialize;

/// Deployment-level config loaded from env vars.
///
/// Pilot-enriched runtime config (strategy bindings, account lists, etc.)
/// is obtained at bootstrap time, not from env.
#[derive(Debug, Deserialize)]
pub struct EngineSvcConfig {
    /// NATS server URL (e.g. "nats://localhost:4222").
    #[serde(rename = "zk_nats_url")]
    pub nats_url: String,

    /// Environment tag (e.g. "dev", "staging", "prod").
    #[serde(rename = "zk_env")]
    pub env: String,

    /// Logical identity for this engine (used in Pilot registration).
    #[serde(rename = "zk_engine_id")]
    pub engine_id: String,

    /// gRPC listen port for the EngineService.
    #[serde(rename = "zk_grpc_port", default = "default_grpc_port")]
    pub grpc_port: u16,

    /// Advertised gRPC host for service registration endpoint.
    #[serde(rename = "zk_grpc_host", default = "default_grpc_host")]
    pub grpc_host: String,

    /// KV heartbeat interval in seconds.
    #[serde(rename = "zk_kv_heartbeat_secs", default = "default_heartbeat")]
    pub kv_heartbeat_secs: u64,

    /// Bootstrap token for Pilot registration.
    /// Empty string means "direct mode" (no Pilot).
    #[serde(rename = "zk_bootstrap_token", default)]
    pub bootstrap_token: String,

    /// Comma-separated account IDs bound to this engine.
    /// Overridden by Pilot grant when available.
    #[serde(rename = "zk_account_ids", default)]
    pub account_ids_csv: String,

    /// Strategy key (e.g. "my-alpha-strat").
    /// Overridden by Pilot grant when available.
    #[serde(rename = "zk_strategy_key", default)]
    pub strategy_key: String,

    /// Comma-separated instrument codes to subscribe.
    /// Overridden by Pilot grant when available.
    #[serde(rename = "zk_instruments", default)]
    pub instruments_csv: String,

    /// NATS KV discovery bucket name.
    #[serde(rename = "zk_discovery_bucket", default = "default_bucket")]
    pub discovery_bucket: String,

    /// Timer clock interval in milliseconds (default 1000 = 1 Hz).
    #[serde(rename = "zk_timer_interval_ms", default = "default_timer_interval")]
    pub timer_interval_ms: u64,

    /// Kline interval for bar subscriptions (e.g. "1m", "5m", "1h").
    /// Empty string disables kline subscriptions.
    #[serde(rename = "zk_kline_interval", default)]
    pub kline_interval: String,
}

fn default_grpc_port() -> u16 {
    50060
}
fn default_grpc_host() -> String {
    "127.0.0.1".into()
}
fn default_heartbeat() -> u64 {
    5
}
fn default_bucket() -> String {
    "zk-svc-registry-v1".into()
}
fn default_timer_interval() -> u64 {
    1000
}

impl EngineSvcConfig {
    /// Parse account IDs from the comma-separated env var.
    pub fn account_ids(&self) -> Vec<i64> {
        self.account_ids_csv
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Parse instrument codes from the comma-separated env var.
    pub fn instruments(&self) -> Vec<String> {
        self.instruments_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Load config from environment variables.
pub fn load() -> Result<EngineSvcConfig, envy::Error> {
    envy::from_env::<EngineSvcConfig>()
}
