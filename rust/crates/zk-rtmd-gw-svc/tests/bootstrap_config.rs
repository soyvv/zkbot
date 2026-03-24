//! Tests for MDGW `ServiceBootstrap` implementation.
//!
//! All trait methods are sync — no async runtime needed.
//! Tests that call `load_direct_config()` manipulate env vars and must not run
//! in parallel with each other (but `decode_pilot_config` / `assemble_runtime_config`
//! tests are safe to run concurrently).

use zk_infra_rs::bootstrap::{
    bootstrap_runtime_config, BootstrapConfigError, BootstrapMode, ConfigSource, PilotPayload,
    ServiceBootstrap,
};
use zk_proto_rs::zk::config::v1::ConfigMetadata;
use zk_rtmd_gw_svc::config::{MdgwBootstrapConfig, MdgwProvidedConfig, MdgwService};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn test_boot_cfg() -> MdgwBootstrapConfig {
    MdgwBootstrapConfig {
        mdgw_id: "mdgw_test_1".into(),
        env: "test".into(),
        nats_url: "nats://localhost:4222".into(),
        grpc_port: 52051,
        grpc_host: "127.0.0.1".into(),
        bootstrap_token: String::new(),
        instance_type: "MDGW".into(),
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

fn compute_hash(json: &str) -> String {
    use sha2::{Digest, Sha256};
    // Reproduce the normalization contract: sorted keys, compact JSON.
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let normalized = serde_json::to_string(&normalize_json_value(&value)).unwrap();
    let hash = Sha256::digest(normalized.as_bytes());
    hex::encode(hash)
}

/// Deterministic JSON normalization (sorted keys, compact).
fn normalize_json_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                sorted.insert(k.clone(), normalize_json_value(&map[k]));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(normalize_json_value).collect())
        }
        other => other.clone(),
    }
}

// ── Pilot mode: decode + assemble ────────────────────────────────────────────

#[test]
fn pilot_mode_valid_json() {
    let boot = test_boot_cfg();
    let json = r#"{"venue":"okx","venue_config":{"ws_url":"wss://ws.okx.com"}}"#;
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload(json),
        validate_hash: false,
    };
    let outcome = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap();
    assert_eq!(outcome.runtime_config.venue, "okx");
    assert_eq!(outcome.runtime_config.mdgw_id, "mdgw_test_1");
    assert_eq!(outcome.runtime_config.env, "test");
    assert_eq!(outcome.runtime_config.nats_url, "nats://localhost:4222");
    assert_eq!(outcome.runtime_config.grpc_port, 52051);
    assert_eq!(outcome.runtime_config.gateway_kv_prefix, "svc.mdgw");
    assert_eq!(outcome.runtime_config.rtmd_sub_bucket, "zk-rtmd-subs-v1");
    assert_eq!(outcome.runtime_config.sub_lease_ttl_s, 60);
    assert!(matches!(outcome.source, ConfigSource::Pilot { .. }));
}

#[test]
fn pilot_mode_malformed_json_fails() {
    let boot = test_boot_cfg();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload("{not valid json}"),
        validate_hash: false,
    };
    let err = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

#[test]
fn pilot_mode_empty_json_fails() {
    let boot = test_boot_cfg();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload(""),
        validate_hash: false,
    };
    let err = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

#[test]
fn pilot_mode_missing_venue_fails() {
    let boot = test_boot_cfg();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload(r#"{"venue":""}"#),
        validate_hash: false,
    };
    let err = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::MissingField { .. }));
}

#[test]
fn pilot_mode_hash_match() {
    let json = r#"{"venue":"okx"}"#;
    let hash = compute_hash(json);
    let boot = test_boot_cfg();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload_with_hash(json, &hash),
        validate_hash: true,
    };
    let outcome = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap();
    assert_eq!(outcome.runtime_config.venue, "okx");
}

#[test]
fn pilot_mode_hash_mismatch_fails() {
    let json = r#"{"venue":"okx"}"#;
    let boot = test_boot_cfg();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload_with_hash(json, "badhash"),
        validate_hash: true,
    };
    let err = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::HashMismatch { .. }));
}

// ── Assembly ─────────────────────────────────────────────────────────────────

#[test]
fn assemble_merges_bootstrap_and_provided() {
    let boot = MdgwBootstrapConfig {
        mdgw_id: "mdgw_prod_1".into(),
        env: "prod".into(),
        nats_url: "nats://prod:4222".into(),
        grpc_port: 9999,
        grpc_host: "10.0.0.5".into(),
        bootstrap_token: String::new(),
        instance_type: "MDGW".into(),
    };
    let provided = MdgwProvidedConfig {
        venue: "binance".into(),
        venue_config: serde_json::json!({"api_type": "spot"}),
        venue_root: Some("/opt/venues".into()),
        gateway_kv_prefix: "svc.mdgw.prod".into(),
        rtmd_sub_bucket: "zk-rtmd-subs-prod-v1".into(),
        sub_lease_ttl_s: 120,
    };
    let cfg = MdgwService::assemble_runtime_config(&boot, provided).unwrap();

    // Bootstrap fields
    assert_eq!(cfg.mdgw_id, "mdgw_prod_1");
    assert_eq!(cfg.env, "prod");
    assert_eq!(cfg.nats_url, "nats://prod:4222");
    assert_eq!(cfg.grpc_port, 9999);
    assert_eq!(cfg.grpc_host, "10.0.0.5");
    // Provided fields
    assert_eq!(cfg.venue, "binance");
    assert_eq!(cfg.venue_config, serde_json::json!({"api_type": "spot"}));
    assert_eq!(cfg.venue_root, Some("/opt/venues".into()));
    assert_eq!(cfg.gateway_kv_prefix, "svc.mdgw.prod");
    assert_eq!(cfg.rtmd_sub_bucket, "zk-rtmd-subs-prod-v1");
    assert_eq!(cfg.sub_lease_ttl_s, 120);
}

#[test]
fn different_venues_produce_different_configs() {
    let boot = test_boot_cfg();

    let okx = bootstrap_runtime_config::<MdgwService>(
        &boot,
        BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"venue":"okx"}"#),
            validate_hash: false,
        },
    )
    .unwrap();

    let binance = bootstrap_runtime_config::<MdgwService>(
        &boot,
        BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"venue":"binance","sub_lease_ttl_s":30}"#),
            validate_hash: false,
        },
    )
    .unwrap();

    assert_ne!(okx.runtime_config.venue, binance.runtime_config.venue);
    assert_ne!(
        okx.runtime_config.sub_lease_ttl_s,
        binance.runtime_config.sub_lease_ttl_s
    );
}

#[test]
fn pilot_mode_with_all_provided_fields() {
    let boot = test_boot_cfg();
    let json = r#"{
        "venue": "oanda",
        "venue_config": {"account_id": "101-001"},
        "venue_root": "/opt/venues/oanda",
        "gateway_kv_prefix": "svc.mdgw.oanda",
        "rtmd_sub_bucket": "zk-rtmd-subs-oanda",
        "sub_lease_ttl_s": 90
    }"#;
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload(json),
        validate_hash: false,
    };
    let outcome = bootstrap_runtime_config::<MdgwService>(&boot, mode).unwrap();
    let cfg = outcome.runtime_config;

    assert_eq!(cfg.venue, "oanda");
    assert_eq!(cfg.venue_config, serde_json::json!({"account_id": "101-001"}));
    assert_eq!(cfg.venue_root, Some("/opt/venues/oanda".into()));
    assert_eq!(cfg.gateway_kv_prefix, "svc.mdgw.oanda");
    assert_eq!(cfg.rtmd_sub_bucket, "zk-rtmd-subs-oanda");
    assert_eq!(cfg.sub_lease_ttl_s, 90);
}

// ── Direct mode (env-dependent, run serially) ────────────────────────────────

// NOTE: Tests calling load_direct_config() read env vars and are sensitive to
// test parallelism.  The decode_pilot_config / assemble tests above are safe
// to run in parallel since they don't touch env vars.
//
// For load_direct_config coverage, we test it indirectly through
// decode_pilot_config (same validation logic) and assemble_runtime_config.
// Full env-based integration is validated by the dev stack smoke test.

#[test]
fn direct_mode_decode_equivalent() {
    // Simulate what load_direct_config would produce by calling decode_pilot_config
    // with the same JSON shape.  This validates the same struct without env var issues.
    let json = r#"{"venue":"simulator","venue_config":{}}"#;
    let provided = MdgwService::decode_pilot_config(&pilot_payload(json)).unwrap();
    assert_eq!(provided.venue, "simulator");
    assert_eq!(provided.gateway_kv_prefix, "svc.mdgw");
    assert_eq!(provided.rtmd_sub_bucket, "zk-rtmd-subs-v1");
    assert_eq!(provided.sub_lease_ttl_s, 60);
}
