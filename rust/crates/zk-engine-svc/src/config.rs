//! Engine service configuration — three-layer model.
//!
//! Follows the shared bootstrap/config architecture from
//! `docs/system-arch/bootstrap_and_runtime_config.md`:
//!
//! - [`EngineBootstrapConfig`] — deployment-owned minimal startup inputs
//! - [`EngineProvidedConfig`] — Pilot-managed or direct-mode equivalent
//! - [`EngineRuntimeConfig`] — effective assembled config used at runtime

use serde::{Deserialize, Serialize};

use zk_infra_rs::bootstrap::BootstrapConfigError;

// ── Bootstrap config (deployment-owned) ─────────────────────────────────────

/// Minimal startup inputs supplied by deployment/orchestration.
///
/// Loaded from `ZK_*` env vars via `envy`.  Contains only what the process
/// needs to reach NATS, identify itself, and begin the bootstrap handshake.
#[derive(Debug, Clone, Deserialize)]
pub struct EngineBootstrapConfig {
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

    /// NATS KV discovery bucket name.
    #[serde(rename = "zk_discovery_bucket", default = "default_bucket")]
    pub discovery_bucket: String,
}

// ── Provided config (Pilot-managed or direct-mode equivalent) ───────────────

/// Runtime behavior config returned by Pilot or loaded from env in direct mode.
///
/// Derives both `Serialize` (for introspection) and `Deserialize` (for Pilot
/// JSON decode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineProvidedConfig {
    /// Logical strategy identity for this engine run.
    #[serde(default)]
    pub strategy_key: String,

    /// Strategy implementation selector (e.g. "smoke-test", "mm").
    pub strategy_type_key: String,

    /// Account IDs bound to this engine.
    pub account_ids: Vec<i64>,

    /// Instrument codes to subscribe.
    pub instruments: Vec<String>,

    /// Optional OMS workspace target for discovery filtering.
    #[serde(default)]
    pub oms_id: String,

    /// Serialized strategy config payload from strategy_definition.config_json.
    #[serde(default)]
    pub strategy_config_json: String,

    /// Timer clock interval in milliseconds (default 1000 = 1 Hz).
    #[serde(default = "default_timer_interval")]
    pub timer_interval_ms: u64,

    /// Kline interval for bar subscriptions (e.g. "1m", "5m", "1h").
    /// Empty string disables kline subscriptions.
    #[serde(default)]
    pub kline_interval: String,
}

// ── Runtime config (effective assembled config) ─────────────────────────────

/// Effective engine startup config assembled from bootstrap + provided inputs.
///
/// All fields needed by the runtime are present here — no need to carry
/// separate bootstrap/provided structs after assembly.
#[derive(Debug, Clone, Serialize)]
pub struct EngineRuntimeConfig {
    // -- from bootstrap --
    pub nats_url: String,
    pub env: String,
    pub engine_id: String,
    pub grpc_port: u16,
    pub grpc_host: String,
    pub kv_heartbeat_secs: u64,
    pub discovery_bucket: String,

    // -- from provided --
    pub strategy_key: String,
    pub strategy_type_key: String,
    pub account_ids: Vec<i64>,
    pub instruments: Vec<String>,
    pub oms_id: String,
    pub strategy_config_json: String,
    pub timer_interval_ms: u64,
    pub kline_interval: String,

    // -- runtime-derived --
    /// Unique execution ID for this run (from Pilot or generated locally).
    pub execution_id: String,
    /// Snowflake instance ID for order ID generation (0 if not assigned).
    pub instance_id: u16,
}

// ── Loaders ─────────────────────────────────────────────────────────────────

/// Load bootstrap config from `ZK_*` environment variables.
pub fn load_bootstrap() -> Result<EngineBootstrapConfig, envy::Error> {
    envy::from_env::<EngineBootstrapConfig>()
}

/// Load provided config from `ZK_*` environment variables (direct mode).
///
/// Uses manual `std::env::var` because `envy` cannot deserialize CSV strings
/// into `Vec<i64>` or `Vec<String>`.
pub fn load_provided_from_env() -> Result<EngineProvidedConfig, BootstrapConfigError> {
    use std::env;

    let strategy_key = env::var("ZK_STRATEGY_KEY").unwrap_or_default();
    let strategy_type_key = env::var("ZK_STRATEGY_TYPE_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| strategy_key.clone());

    let account_ids: Vec<i64> = env::var("ZK_ACCOUNT_IDS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            s.trim()
                .parse::<i64>()
                .map_err(|e| BootstrapConfigError::Validation(format!("invalid account_id: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let instruments: Vec<String> = env::var("ZK_INSTRUMENTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let oms_id = env::var("ZK_OMS_ID").unwrap_or_default();
    let strategy_config_json = env::var("ZK_STRATEGY_CONFIG_JSON").unwrap_or_default();

    let timer_interval_ms: u64 = env::var("ZK_TIMER_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(default_timer_interval);

    let kline_interval = env::var("ZK_KLINE_INTERVAL").unwrap_or_default();

    Ok(EngineProvidedConfig {
        strategy_key,
        strategy_type_key,
        account_ids,
        instruments,
        oms_id,
        strategy_config_json,
        timer_interval_ms,
        kline_interval,
    })
}

// ── Defaults ────────────────────────────────────────────────────────────────

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
