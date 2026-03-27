use serde::{Deserialize, Serialize};

use zk_infra_rs::bootstrap::{BootstrapConfigError, PilotPayload, ServiceBootstrap};

// ── Bootstrap config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RecorderBootstrapConfig {
    pub recorder_id: String,

    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    #[serde(default)]
    pub bootstrap_token: String,

    #[serde(default = "default_instance_type")]
    pub instance_type: String,

    #[serde(default = "default_env")]
    pub env: String,
}

// ── Provided config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RecorderProvidedConfig {
    #[serde(default)]
    pub pg_url: String,

    #[serde(default = "default_pg_max_connections")]
    pub pg_max_connections: u32,

    /// How often to run partition maintenance (seconds). Default 3600 (hourly).
    #[serde(default = "default_partition_interval")]
    pub partition_maintenance_interval_secs: u64,

    /// How many months of partitions to keep. Default 3.
    #[serde(default = "default_retention_months")]
    pub partition_retention_months: u32,

    /// JetStream consumer ack wait (seconds). Default 30.
    #[serde(default = "default_ack_wait")]
    pub consumer_ack_wait_secs: u64,
}

// ── Runtime config ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RecorderRuntimeConfig {
    pub recorder_id: String,
    pub nats_url: String,
    pub env: String,
    pub pg_url: String,
    pub pg_max_connections: u32,
    pub partition_maintenance_interval_secs: u64,
    pub partition_retention_months: u32,
    pub consumer_ack_wait_secs: u64,
}

// ── ServiceBootstrap ─────────────────────────────────────────────────────────

pub struct RecorderService;

impl ServiceBootstrap for RecorderService {
    type BootstrapConfig = RecorderBootstrapConfig;
    type ProvidedConfig = RecorderProvidedConfig;
    type RuntimeConfig = RecorderRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: RecorderProvidedConfig = serde_json::from_str(&payload.runtime_config_json)?;
        if cfg.pg_url.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "pg_url".into(),
            });
        }
        Ok(cfg)
    }

    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: RecorderProvidedConfig = envy::prefixed("ZK_")
            .from_env()
            .map_err(|e| BootstrapConfigError::Custom(e.to_string()))?;
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
        Ok(RecorderRuntimeConfig {
            recorder_id: bootstrap.recorder_id.clone(),
            nats_url: bootstrap.nats_url.clone(),
            env: bootstrap.env.clone(),
            pg_url: provided.pg_url,
            pg_max_connections: provided.pg_max_connections,
            partition_maintenance_interval_secs: provided.partition_maintenance_interval_secs,
            partition_retention_months: provided.partition_retention_months,
            consumer_ack_wait_secs: provided.consumer_ack_wait_secs,
        })
    }
}

// ── Loading ──────────────────────────────────────────────────────────────────

pub fn load_bootstrap() -> Result<RecorderBootstrapConfig, BootstrapConfigError> {
    envy::prefixed("ZK_")
        .from_env::<RecorderBootstrapConfig>()
        .map_err(|e| BootstrapConfigError::Custom(e.to_string()))
}

// ── Defaults ─────────────────────────────────────────────────────────────────

fn default_nats_url() -> String {
    "nats://localhost:4222".into()
}
fn default_instance_type() -> String {
    "RECORDER".into()
}
fn default_env() -> String {
    "dev".into()
}
fn default_pg_max_connections() -> u32 {
    10
}
fn default_partition_interval() -> u64 {
    3600
}
fn default_retention_months() -> u32 {
    3
}
fn default_ack_wait() -> u64 {
    30
}
