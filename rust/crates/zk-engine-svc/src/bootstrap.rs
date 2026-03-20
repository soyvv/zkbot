//! Pilot bootstrap — claim execution identity, receive runtime config.
//!
//! In "direct mode" (no Pilot), the engine uses env-var config directly.
//! In "pilot mode", the engine sends a `BootstrapRegisterRequest` and
//! receives a grant with `execution_id`, `instance_id`, and enriched config.

use std::time::Duration;

use tracing::{info, warn};

use zk_infra_rs::service_registry::ServiceRegistration;

use crate::config::EngineSvcConfig;

/// Result of the bootstrap phase — everything the runtime needs to proceed.
pub struct BootstrapGrant {
    /// Unique execution ID for this run (from Pilot or generated locally).
    pub execution_id: String,
    /// Strategy key.
    pub strategy_key: String,
    /// Account IDs bound to this engine.
    pub account_ids: Vec<i64>,
    /// Instrument codes to subscribe.
    pub instruments: Vec<String>,
    /// Snowflake instance ID for order ID generation (0 if not assigned).
    pub instance_id: u16,
}

/// Run the bootstrap phase.
///
/// - If `bootstrap_token` is non-empty, attempts Pilot registration (currently stubbed).
/// - Otherwise, builds the grant from env-var config ("direct mode").
pub async fn bootstrap(
    cfg: &EngineSvcConfig,
    _nats: &async_nats::Client,
    _js: &async_nats::jetstream::Context,
) -> anyhow::Result<BootstrapGrant> {
    if cfg.bootstrap_token.is_empty() {
        info!("bootstrap: direct mode (no Pilot token)");
        return Ok(direct_grant(cfg));
    }

    // TODO(phase5): Pilot registration via ServiceRegistration::register_with_pilot().
    // For now, fall back to direct mode with a warning.
    warn!("bootstrap: Pilot token provided but Pilot registration not yet implemented — falling back to direct mode");
    Ok(direct_grant(cfg))
}

/// Build a grant from env-var config (no Pilot).
fn direct_grant(cfg: &EngineSvcConfig) -> BootstrapGrant {
    let execution_id = format!("{}-{}", cfg.engine_id, uuid_v4_short());
    BootstrapGrant {
        execution_id,
        strategy_key: cfg.strategy_key.clone(),
        account_ids: cfg.account_ids(),
        instruments: cfg.instruments(),
        instance_id: 0,
    }
}

/// Register in NATS KV with CAS heartbeat (direct mode).
pub async fn register_kv(
    js: &async_nats::jetstream::Context,
    cfg: &EngineSvcConfig,
    grant: &BootstrapGrant,
) -> anyhow::Result<ServiceRegistration> {
    let kv_key = format!("svc.engine.{}", cfg.engine_id);
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let reg = zk_infra_rs::discovery_registration::engine_registration(
        &cfg.engine_id,
        &grpc_address,
        &grant.execution_id,
        &grant.strategy_key,
        &grant.account_ids,
    );
    let kv_value = zk_infra_rs::discovery_registration::encode_registration(&reg);

    let registration = ServiceRegistration::register_direct(
        js,
        kv_key,
        kv_value,
        Duration::from_secs(cfg.kv_heartbeat_secs),
    )
    .await
    .map_err(|e| anyhow::anyhow!("KV registration failed: {e}"))?;

    info!(
        kv_key = registration.grant().kv_key,
        session_id = registration.grant().owner_session_id,
        "registered in NATS KV"
    );
    Ok(registration)
}

/// Generate a short pseudo-unique suffix (first 8 chars of a timestamp + random).
fn uuid_v4_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{:x}", ts)
}
