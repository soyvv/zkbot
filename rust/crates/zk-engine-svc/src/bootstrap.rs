//! Engine bootstrap — implements `ServiceBootstrap` for the shared config path.
//!
//! Two modes:
//! - **Direct mode** (no Pilot): loads provided config from env vars.
//! - **Pilot mode**: registers with Pilot, receives config payload, decodes it.
//!
//! No silent fallback from Pilot to direct mode.

use std::time::Duration;

use tracing::info;

use zk_infra_rs::bootstrap::{
    bootstrap_runtime_config, BootstrapConfigError, BootstrapMode, BootstrapOutcome,
    PilotPayload, ServiceBootstrap,
};
use zk_infra_rs::service_registry::{PilotBootstrapGrant, ServiceRegistration};

use crate::config::{
    self, EngineBootstrapConfig, EngineProvidedConfig, EngineRuntimeConfig,
};

// ── ServiceBootstrap impl ───────────────────────────────────────────────────

/// Marker type for `ServiceBootstrap` trait implementation.
pub struct EngineService;

impl ServiceBootstrap for EngineService {
    type BootstrapConfig = EngineBootstrapConfig;
    type ProvidedConfig = EngineProvidedConfig;
    type RuntimeConfig = EngineRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        let cfg: EngineProvidedConfig =
            serde_json::from_str(&payload.runtime_config_json)?;

        // Validate required bindings.
        if cfg.strategy_key.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "strategy_key".into(),
            });
        }
        if cfg.account_ids.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "account_ids".into(),
            });
        }
        if cfg.instruments.is_empty() {
            return Err(BootstrapConfigError::MissingField {
                field: "instruments".into(),
            });
        }
        Ok(cfg)
    }

    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
        config::load_provided_from_env()
    }

    fn assemble_runtime_config(
        bootstrap: &Self::BootstrapConfig,
        provided: Self::ProvidedConfig,
    ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
        let execution_id = format!("{}-{}", bootstrap.engine_id, uuid_v4_short());

        Ok(EngineRuntimeConfig {
            // bootstrap
            nats_url: bootstrap.nats_url.clone(),
            env: bootstrap.env.clone(),
            engine_id: bootstrap.engine_id.clone(),
            grpc_port: bootstrap.grpc_port,
            grpc_host: bootstrap.grpc_host.clone(),
            kv_heartbeat_secs: bootstrap.kv_heartbeat_secs,
            discovery_bucket: bootstrap.discovery_bucket.clone(),
            // provided
            strategy_key: provided.strategy_key,
            account_ids: provided.account_ids,
            instruments: provided.instruments,
            timer_interval_ms: provided.timer_interval_ms,
            kline_interval: provided.kline_interval,
            // runtime-derived
            execution_id,
            instance_id: 0,
        })
    }
}

// ── Bootstrap orchestrator ──────────────────────────────────────────────────

/// Run the full bootstrap sequence.
///
/// Returns the assembled runtime config outcome and, in Pilot mode, the
/// split-phase Pilot grant. In direct mode the grant is `None` and the caller
/// must call [`register_kv`] directly. In Pilot mode, the caller must finish
/// registration with [`register_kv_with_grant`].
pub async fn run_bootstrap(
    boot_cfg: &EngineBootstrapConfig,
    nats: &async_nats::Client,
) -> anyhow::Result<(
    BootstrapOutcome<EngineRuntimeConfig>,
    Option<PilotBootstrapGrant>,
)> {
    if boot_cfg.bootstrap_token.is_empty() {
        info!("bootstrap: direct mode (no Pilot token)");
        let outcome = bootstrap_runtime_config::<EngineService>(boot_cfg, BootstrapMode::Direct)
            .map_err(|e| anyhow::anyhow!("bootstrap config assembly failed: {e}"))?;
        return Ok((outcome, None));
    }

    // Pilot mode: request Pilot grant first, then decode config payload.
    info!("bootstrap: Pilot mode — requesting Pilot grant");
    let grant = ServiceRegistration::pilot_request(
        nats,
        &boot_cfg.bootstrap_token,
        &boot_cfg.engine_id,
        "engine",
        &boot_cfg.env,
        std::collections::HashMap::new(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Pilot bootstrap request failed: {e}"))?;

    // Decode and assemble runtime config from Pilot payload.
    let mut outcome = bootstrap_runtime_config::<EngineService>(
        boot_cfg,
        BootstrapMode::Pilot {
            payload: grant.payload.clone(),
            validate_hash: false,
        },
    )
    .map_err(|e| anyhow::anyhow!("Pilot config assembly failed: {e}"))?;

    // Override instance_id from Pilot grant if assigned.
    if grant.instance_id != 0 {
        outcome.runtime_config.instance_id = grant.instance_id as u16;
    }

    info!(
        execution_id = %outcome.runtime_config.execution_id,
        instance_id = outcome.runtime_config.instance_id,
        "Pilot bootstrap complete"
    );

    Ok((outcome, Some(grant)))
}

// ── KV registration (direct mode) ──────────────────────────────────────────

/// Register in NATS KV with CAS heartbeat (direct mode only).
///
/// In Pilot mode, registration is already established by [`run_bootstrap`].
pub async fn register_kv(
    js: &async_nats::jetstream::Context,
    cfg: &EngineRuntimeConfig,
) -> anyhow::Result<ServiceRegistration> {
    let kv_key = format!("svc.engine.{}", cfg.engine_id);
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let reg = zk_infra_rs::discovery_registration::engine_registration(
        &cfg.engine_id,
        &grpc_address,
        &cfg.execution_id,
        &cfg.strategy_key,
        &cfg.account_ids,
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

/// Register in NATS KV with CAS heartbeat using a split-phase Pilot grant.
pub async fn register_kv_with_grant(
    nats: &async_nats::Client,
    js: &async_nats::jetstream::Context,
    grant: &PilotBootstrapGrant,
    cfg: &EngineRuntimeConfig,
) -> anyhow::Result<ServiceRegistration> {
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let reg = zk_infra_rs::discovery_registration::engine_registration(
        &cfg.engine_id,
        &grpc_address,
        &cfg.execution_id,
        &cfg.strategy_key,
        &cfg.account_ids,
    );
    let kv_value = zk_infra_rs::discovery_registration::encode_registration(&reg);

    let registration = ServiceRegistration::register_kv_with_grant(
        nats,
        js,
        grant,
        kv_value,
        Duration::from_secs(cfg.kv_heartbeat_secs),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Pilot KV registration failed: {e}"))?;

    info!(
        kv_key = registration.grant().kv_key,
        session_id = registration.grant().owner_session_id,
        "registered in NATS KV via Pilot grant"
    );
    Ok(registration)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Generate a short pseudo-unique suffix (first 8 chars of a timestamp + random).
fn uuid_v4_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{:x}", ts)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use zk_infra_rs::bootstrap::{BootstrapMode, PilotPayload};
    use zk_proto_rs::zk::config::v1::ConfigMetadata;

    fn test_boot_cfg() -> EngineBootstrapConfig {
        EngineBootstrapConfig {
            nats_url: "nats://localhost:4222".into(),
            env: "dev".into(),
            engine_id: "engine-test-1".into(),
            grpc_port: 50060,
            grpc_host: "127.0.0.1".into(),
            kv_heartbeat_secs: 5,
            bootstrap_token: String::new(),
            discovery_bucket: "zk-svc-registry-v1".into(),
        }
    }

    fn pilot_payload(json: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: None,
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn pilot_payload_with_hash(json: &str, hash: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: Some(ConfigMetadata {
                config_version: "1".into(),
                config_hash: hash.into(),
                config_source: "bootstrap".into(),
                issued_at_ms: 1700000000000,
                loaded_at_ms: 0,
            }),
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn valid_provided_json() -> &'static str {
        r#"{"strategy_key":"alpha","account_ids":[1,2],"instruments":["BTC-USDT","ETH-USDT"],"timer_interval_ms":500,"kline_interval":"1m"}"#
    }

    /// Compute normalized JSON hash (same algorithm as zk-infra-rs config_mgmt).
    fn compute_hash(json: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let normalized = normalize_json(&value);
        let hash = Sha256::digest(normalized.as_bytes());
        hex::encode(hash)
    }

    /// Deterministic JSON normalization (sorted keys, compact).
    fn normalize_json(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted: Vec<_> = map.iter().collect();
                sorted.sort_by_key(|(k, _)| *k);
                let entries: Vec<String> = sorted
                    .into_iter()
                    .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), normalize_json(v)))
                    .collect();
                format!("{{{}}}", entries.join(","))
            }
            _ => serde_json::to_string(value).unwrap(),
        }
    }

    // -- Direct mode ----------------------------------------------------------

    #[test]
    fn direct_mode_assembles_runtime_config() {
        // Set env vars for direct-mode provided config loading.
        std::env::set_var("ZK_STRATEGY_KEY", "test-strat");
        std::env::set_var("ZK_ACCOUNT_IDS", "100,200");
        std::env::set_var("ZK_INSTRUMENTS", "BTC-USDT,ETH-USDT");
        std::env::set_var("ZK_TIMER_INTERVAL_MS", "500");
        std::env::set_var("ZK_KLINE_INTERVAL", "5m");

        let boot = test_boot_cfg();
        let outcome =
            bootstrap_runtime_config::<EngineService>(&boot, BootstrapMode::Direct).unwrap();

        let cfg = &outcome.runtime_config;
        assert_eq!(cfg.env, "dev");
        assert_eq!(cfg.engine_id, "engine-test-1");
        assert_eq!(cfg.nats_url, "nats://localhost:4222");
        assert_eq!(cfg.strategy_key, "test-strat");
        assert_eq!(cfg.account_ids, vec![100, 200]);
        assert_eq!(cfg.instruments, vec!["BTC-USDT", "ETH-USDT"]);
        assert_eq!(cfg.timer_interval_ms, 500);
        assert_eq!(cfg.kline_interval, "5m");
        assert!(cfg.execution_id.starts_with("engine-test-1-"));
        assert_eq!(cfg.instance_id, 0);

        // Clean up.
        std::env::remove_var("ZK_STRATEGY_KEY");
        std::env::remove_var("ZK_ACCOUNT_IDS");
        std::env::remove_var("ZK_INSTRUMENTS");
        std::env::remove_var("ZK_TIMER_INTERVAL_MS");
        std::env::remove_var("ZK_KLINE_INTERVAL");
    }

    // -- Pilot mode: decode ---------------------------------------------------

    #[test]
    fn pilot_mode_decodes_valid_json() {
        let payload = pilot_payload(valid_provided_json());
        let provided = EngineService::decode_pilot_config(&payload).unwrap();
        assert_eq!(provided.strategy_key, "alpha");
        assert_eq!(provided.account_ids, vec![1, 2]);
        assert_eq!(provided.instruments, vec!["BTC-USDT", "ETH-USDT"]);
        assert_eq!(provided.timer_interval_ms, 500);
        assert_eq!(provided.kline_interval, "1m");
    }

    #[test]
    fn pilot_mode_assembles_runtime_config() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(valid_provided_json()),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap();
        let cfg = &outcome.runtime_config;

        // Bootstrap fields forwarded.
        assert_eq!(cfg.env, "dev");
        assert_eq!(cfg.engine_id, "engine-test-1");
        assert_eq!(cfg.nats_url, "nats://localhost:4222");

        // Provided fields decoded.
        assert_eq!(cfg.strategy_key, "alpha");
        assert_eq!(cfg.account_ids, vec![1, 2]);
        assert_eq!(cfg.instruments, vec!["BTC-USDT", "ETH-USDT"]);
        assert_eq!(cfg.timer_interval_ms, 500);
        assert_eq!(cfg.kline_interval, "1m");

        // Runtime-derived.
        assert!(cfg.execution_id.starts_with("engine-test-1-"));
        assert_eq!(cfg.instance_id, 0);
    }

    // -- Pilot mode: validation errors ----------------------------------------

    #[test]
    fn pilot_mode_malformed_json_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload("{not valid json}"),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn pilot_mode_empty_json_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(""),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn pilot_mode_missing_strategy_key_fails() {
        let boot = test_boot_cfg();
        let json = r#"{"strategy_key":"","account_ids":[1],"instruments":["BTC-USDT"]}"#;
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(json),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(
            err,
            BootstrapConfigError::MissingField { ref field } if field == "strategy_key"
        ));
    }

    #[test]
    fn pilot_mode_missing_account_ids_fails() {
        let boot = test_boot_cfg();
        let json = r#"{"strategy_key":"alpha","account_ids":[],"instruments":["BTC-USDT"]}"#;
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(json),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(
            err,
            BootstrapConfigError::MissingField { ref field } if field == "account_ids"
        ));
    }

    #[test]
    fn pilot_mode_missing_instruments_fails() {
        let boot = test_boot_cfg();
        let json = r#"{"strategy_key":"alpha","account_ids":[1],"instruments":[]}"#;
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(json),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(
            err,
            BootstrapConfigError::MissingField { ref field } if field == "instruments"
        ));
    }

    // -- Hash validation ------------------------------------------------------

    #[test]
    fn pilot_mode_hash_mismatch_fails() {
        let boot = test_boot_cfg();
        let json = valid_provided_json();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, "badhash"),
            validate_hash: true,
        };
        let err = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::HashMismatch { .. }));
    }

    #[test]
    fn pilot_mode_hash_match_succeeds() {
        let boot = test_boot_cfg();
        let json = valid_provided_json();
        let hash = compute_hash(json);
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, &hash),
            validate_hash: true,
        };
        let outcome = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.strategy_key, "alpha");
    }

    // -- No silent fallback (structural) --------------------------------------

    #[test]
    fn pilot_mode_does_not_call_load_direct_config() {
        // The shared orchestrator calls decode_pilot_config in Pilot mode
        // and load_direct_config in Direct mode — never both.  This test
        // verifies Pilot mode works even if ZK_* env vars are unset/wrong.
        std::env::remove_var("ZK_STRATEGY_KEY");
        std::env::remove_var("ZK_ACCOUNT_IDS");
        std::env::remove_var("ZK_INSTRUMENTS");

        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(valid_provided_json()),
            validate_hash: false,
        };
        // Should succeed — Pilot config is self-contained, env vars irrelevant.
        let outcome = bootstrap_runtime_config::<EngineService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.strategy_key, "alpha");
    }
}
