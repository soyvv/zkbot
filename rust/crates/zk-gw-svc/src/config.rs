use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zk_infra_rs::bootstrap::{BootstrapConfigError, PilotPayload, ServiceBootstrap};

// ── Defaults ────────────────────────────────────────────────────────────────

fn default_venue_config() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_exec_shard_count() -> usize {
    4
}

fn default_exec_queue_capacity() -> usize {
    256
}

fn default_mock_balances() -> String {
    "BTC:10,USDT:100000,ETH:50".into()
}

fn default_match_policy() -> String {
    "immediate".into()
}

fn default_admin_grpc_port() -> u16 {
    51052
}

// ── GwBootstrapConfig ───────────────────────────────────────────────────────

/// Minimal env/bootstrap inputs. Only what's needed to reach NATS and Pilot.
///
/// Loaded from `ZK_*` env vars at process start. Never from Pilot.
#[derive(Debug, Clone)]
pub struct GwBootstrapConfig {
    pub gw_id: String,
    pub grpc_host: String,
    pub grpc_port: u16,
    pub nats_url: Option<String>,
    pub bootstrap_token: String,
    pub instance_type: String,
    pub env: String,
}

impl GwBootstrapConfig {
    /// Load from `ZK_*` env vars with defaults.
    pub fn from_env() -> Result<Self, BootstrapConfigError> {
        Ok(Self {
            gw_id: std::env::var("ZK_GW_ID").unwrap_or_else(|_| "gw_sim_1".into()),
            grpc_host: std::env::var("ZK_GRPC_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            grpc_port: std::env::var("ZK_GRPC_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(51051),
            nats_url: std::env::var("ZK_NATS_URL").ok(),
            bootstrap_token: std::env::var("ZK_BOOTSTRAP_TOKEN").unwrap_or_default(),
            instance_type: std::env::var("ZK_INSTANCE_TYPE").unwrap_or_else(|_| "GW".into()),
            env: std::env::var("ZK_ENV").unwrap_or_else(|_| "dev".into()),
        })
    }
}

// ── GwProvidedConfig ────────────────────────────────────────────────────────

/// Pilot-owned config (or direct-mode equivalent from env vars).
///
/// In Pilot mode: deserialized from `PilotPayload.runtime_config_json`.
/// In direct mode: loaded from remaining `ZK_*` env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GwProvidedConfig {
    /// Venue name (required, no default).
    pub venue: String,
    /// Trading account ID (required, no default).
    pub account_id: i64,
    /// Exchange-side account identifier (e.g. OKX sub-account ID).
    /// Used by the semantic pipeline to set `exch_account_code` on outbound
    /// balance/position publications so OMS can link exchange-owned state.
    #[serde(default)]
    pub exch_account_id: String,
    /// Opaque venue-specific config blob.
    #[serde(default = "default_venue_config")]
    pub venue_config: serde_json::Value,
    /// Root directory for manifest-driven venue loading.
    #[serde(default)]
    pub venue_root: Option<String>,
    /// Number of internal execution shards.
    #[serde(default = "default_exec_shard_count")]
    pub exec_shard_count: usize,
    /// Per-shard queue capacity.
    #[serde(default = "default_exec_queue_capacity")]
    pub exec_queue_capacity: usize,
    /// Simulator-only config. Must be `Some` when venue == "simulator", `None` otherwise.
    #[serde(default)]
    pub simulator: Option<SimulatorProvidedConfig>,
}

/// Simulator-specific provided config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatorProvidedConfig {
    /// Initial balances ("BTC:10,USDT:100000,ETH:50").
    #[serde(default = "default_mock_balances")]
    pub mock_balances: String,
    /// Fill delay in ms (0 = match policy decides immediately).
    #[serde(default)]
    pub fill_delay_ms: u64,
    /// Match policy name ("immediate" or "fcfs").
    #[serde(default = "default_match_policy")]
    pub match_policy: String,
    /// Admin gRPC port for simulator controls.
    #[serde(default = "default_admin_grpc_port")]
    pub admin_grpc_port: u16,
    /// Whether admin controls are enabled.
    #[serde(default)]
    pub enable_admin_controls: bool,
}

// ── GwRuntimeConfig ─────────────────────────────────────────────────────────

/// Effective assembled runtime config. Pure business config — no bootstrap metadata.
///
/// Assembled from `GwBootstrapConfig` + `GwProvidedConfig`.
/// This is what downstream code (adapter factory, exec pool, gRPC handler) consumes.
#[derive(Debug, Clone)]
pub struct GwRuntimeConfig {
    // From bootstrap
    pub gw_id: String,
    pub grpc_host: String,
    pub grpc_port: u16,
    pub nats_url: Option<String>,
    pub instance_type: String,
    pub env: String,
    // From provided
    pub venue: String,
    pub account_id: i64,
    pub exch_account_id: String,
    pub venue_config: serde_json::Value,
    pub venue_root: Option<String>,
    pub exec_shard_count: usize,
    pub exec_queue_capacity: usize,
    pub simulator: Option<SimulatorRuntimeConfig>,
}

/// Simulator runtime config. Same shape as `SimulatorProvidedConfig`
/// (may diverge later if assembly adds derived fields).
#[derive(Debug, Clone)]
pub struct SimulatorRuntimeConfig {
    pub mock_balances: String,
    pub fill_delay_ms: u64,
    pub match_policy: String,
    pub admin_grpc_port: u16,
    pub enable_admin_controls: bool,
}

// ── ServiceBootstrap impl ───────────────────────────────────────────────────

/// Unit struct implementing `ServiceBootstrap` for the gateway service.
pub struct GwBootstrap;

impl ServiceBootstrap for GwBootstrap {
    type BootstrapConfig = GwBootstrapConfig;
    type ProvidedConfig = GwProvidedConfig;
    type RuntimeConfig = GwRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let mut raw: Value = serde_json::from_str(&payload.runtime_config_json)?;
        normalize_simulator_pilot_config(&mut raw);
        let cfg: GwProvidedConfig = serde_json::from_value(raw)?;
        Ok(cfg)
    }

    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let venue = std::env::var("ZK_VENUE").map_err(|_| BootstrapConfigError::MissingField {
            field: "venue (ZK_VENUE)".into(),
        })?;

        let account_id: i64 = std::env::var("ZK_ACCOUNT_ID")
            .map_err(|_| BootstrapConfigError::MissingField {
                field: "account_id (ZK_ACCOUNT_ID)".into(),
            })?
            .parse()
            .map_err(|e| BootstrapConfigError::Validation(format!("invalid ZK_ACCOUNT_ID: {e}")))?;

        let exch_account_id = std::env::var("ZK_EXCH_ACCOUNT_ID").unwrap_or_default();

        let venue_config_str = std::env::var("ZK_VENUE_CONFIG").unwrap_or_else(|_| "{}".into());
        let venue_config: serde_json::Value =
            serde_json::from_str(&venue_config_str).unwrap_or_else(|_| default_venue_config());

        let venue_root = std::env::var("ZK_VENUE_MANIFEST_ROOT").ok();

        let exec_shard_count: usize = std::env::var("ZK_EXEC_SHARD_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_exec_shard_count);

        let exec_queue_capacity: usize = std::env::var("ZK_EXEC_QUEUE_CAPACITY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_exec_queue_capacity);

        let simulator = if venue == "simulator" {
            Some(SimulatorProvidedConfig {
                mock_balances: std::env::var("ZK_MOCK_BALANCES")
                    .unwrap_or_else(|_| default_mock_balances()),
                fill_delay_ms: std::env::var("ZK_FILL_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                match_policy: std::env::var("ZK_MATCH_POLICY")
                    .unwrap_or_else(|_| default_match_policy()),
                admin_grpc_port: std::env::var("ZK_ADMIN_GRPC_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(default_admin_grpc_port),
                enable_admin_controls: std::env::var("ZK_ENABLE_ADMIN_CONTROLS")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false),
            })
        } else {
            None
        };

        Ok(GwProvidedConfig {
            venue,
            account_id,
            exch_account_id,
            venue_config,
            venue_root,
            exec_shard_count,
            exec_queue_capacity,
            simulator,
        })
    }

    fn assemble_runtime_config(
        bootstrap: &Self::BootstrapConfig,
        provided: Self::ProvidedConfig,
    ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
        // Validate simulator consistency.
        if provided.venue == "simulator" && provided.simulator.is_none() {
            return Err(BootstrapConfigError::MissingField {
                field: "simulator".into(),
            });
        }
        if provided.venue != "simulator" && provided.simulator.is_some() {
            return Err(BootstrapConfigError::Validation(
                "simulator config present on non-simulator venue".into(),
            ));
        }

        // Validate exec pool config.
        if provided.exec_shard_count < 1 {
            return Err(BootstrapConfigError::Validation(
                "exec_shard_count must be >= 1".into(),
            ));
        }
        if provided.exec_queue_capacity < 1 {
            return Err(BootstrapConfigError::Validation(
                "exec_queue_capacity must be >= 1".into(),
            ));
        }

        let simulator = provided.simulator.map(|s| SimulatorRuntimeConfig {
            mock_balances: s.mock_balances,
            fill_delay_ms: s.fill_delay_ms,
            match_policy: s.match_policy,
            admin_grpc_port: s.admin_grpc_port,
            enable_admin_controls: s.enable_admin_controls,
        });

        Ok(GwRuntimeConfig {
            gw_id: bootstrap.gw_id.clone(),
            grpc_host: bootstrap.grpc_host.clone(),
            grpc_port: bootstrap.grpc_port,
            nats_url: bootstrap.nats_url.clone(),
            instance_type: bootstrap.instance_type.clone(),
            env: bootstrap.env.clone(),
            venue: provided.venue,
            account_id: provided.account_id,
            exch_account_id: provided.exch_account_id,
            venue_config: provided.venue_config,
            venue_root: provided
                .venue_root
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("ZK_VENUE_MANIFEST_ROOT").ok()),
            exec_shard_count: provided.exec_shard_count,
            exec_queue_capacity: provided.exec_queue_capacity,
            simulator,
        })
    }
}

fn normalize_simulator_pilot_config(raw: &mut Value) {
    let Some(obj) = raw.as_object_mut() else {
        return;
    };

    let venue = obj
        .get("venue")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if venue != "simulator" || obj.contains_key("simulator") {
        return;
    }

    let mut simulator = Map::new();
    move_if_present(obj, &mut simulator, "mock_balances");
    move_if_present(obj, &mut simulator, "fill_delay_ms");
    move_if_present(obj, &mut simulator, "match_policy");
    move_if_present(obj, &mut simulator, "admin_grpc_port");
    move_if_present(obj, &mut simulator, "enable_admin_controls");

    if !simulator.is_empty() {
        obj.insert("simulator".to_string(), Value::Object(simulator));
    }
}

fn move_if_present(root: &mut Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    if let Some(value) = root.remove(key) {
        target.insert(key.to_string(), value);
    }
}

// ── Utilities ───────────────────────────────────────────────────────────────

/// Parse "BTC:10.5,USDT:100000" into a balance map.
pub fn parse_balances(s: &str) -> HashMap<String, f64> {
    let mut map = HashMap::new();
    for entry in s.split(',') {
        let mut parts = entry.splitn(2, ':');
        let symbol = parts.next().unwrap_or("").trim().to_uppercase();
        let qty: f64 = parts
            .next()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0);
        if !symbol.is_empty() {
            map.insert(symbol, qty);
        }
    }
    map
}
