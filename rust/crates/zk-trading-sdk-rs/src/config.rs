//! TradingClient configuration.

use crate::error::SdkError;

const DEFAULT_DISCOVERY_BUCKET: &str = "zk-svc-registry-v1";
const DEFAULT_DISCOVERY_TIMEOUT_MS: u64 = 15_000;

/// Configuration for `TradingClient`.
#[derive(Debug, Clone)]
pub struct TradingClientConfig {
    pub nats_url: String,
    pub env: String,
    pub account_ids: Vec<i64>,
    /// Optional OMS logical id to constrain discovery resolution.
    pub oms_id: Option<String>,
    /// Snowflake instance ID (0–1023).
    /// Production: from Pilot `enriched_config.instance_id`.
    /// Dev/test: from `ZK_CLIENT_INSTANCE_ID` env var.
    pub client_instance_id: u16,
    /// NATS KV bucket name. Default: `zk-svc-registry-v1`.
    pub discovery_bucket: String,
    /// How long to wait for required discovery entries before failing startup.
    pub discovery_timeout_ms: u64,
    /// Optional refdata gRPC address override (skips KV discovery).
    pub refdata_grpc: Option<String>,
}

impl TradingClientConfig {
    /// Load configuration from environment variables.
    ///
    /// Required: `ZK_NATS_URL`, `ZK_ENV`, `ZK_ACCOUNT_IDS`, `ZK_CLIENT_INSTANCE_ID`
    /// Optional: `ZK_DISCOVERY_BUCKET`, `ZK_REFDATA_GRPC`
    pub fn from_env() -> Result<Self, SdkError> {
        let nats_url = std::env::var("ZK_NATS_URL")
            .map_err(|_| SdkError::Config("ZK_NATS_URL is required".into()))?;

        let env =
            std::env::var("ZK_ENV").map_err(|_| SdkError::Config("ZK_ENV is required".into()))?;

        let account_ids_str = std::env::var("ZK_ACCOUNT_IDS")
            .map_err(|_| SdkError::Config("ZK_ACCOUNT_IDS is required".into()))?;
        let account_ids = account_ids_str
            .split(',')
            .map(|s| {
                s.trim().parse::<i64>().map_err(|_| {
                    SdkError::Config(format!("ZK_ACCOUNT_IDS: invalid account_id '{s}'"))
                })
            })
            .collect::<Result<Vec<i64>, SdkError>>()?;

        let client_instance_id = std::env::var("ZK_CLIENT_INSTANCE_ID")
            .map_err(|_| SdkError::InstanceIdMissing)
            .and_then(|s| {
                s.parse::<u16>().map_err(|_| {
                    SdkError::Config("ZK_CLIENT_INSTANCE_ID must be a number 0–1023".into())
                })
            })?;
        if client_instance_id > 1023 {
            return Err(SdkError::InstanceIdOutOfRange(client_instance_id));
        }

        let discovery_bucket = std::env::var("ZK_DISCOVERY_BUCKET")
            .unwrap_or_else(|_| DEFAULT_DISCOVERY_BUCKET.to_string());

        let discovery_timeout_ms = std::env::var("ZK_DISCOVERY_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_DISCOVERY_TIMEOUT_MS);

        let oms_id = std::env::var("ZK_OMS_ID")
            .ok()
            .filter(|value| !value.is_empty());

        let refdata_grpc = std::env::var("ZK_REFDATA_GRPC").ok();

        Ok(Self {
            nats_url,
            env,
            account_ids,
            oms_id,
            client_instance_id,
            discovery_bucket,
            discovery_timeout_ms,
            refdata_grpc,
        })
    }
}
