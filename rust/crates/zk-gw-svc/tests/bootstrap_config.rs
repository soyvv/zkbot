//! Tests for GW bootstrap config types and ServiceBootstrap impl.

use zk_gw_svc::config::{GwBootstrap, GwBootstrapConfig};
use zk_infra_rs::bootstrap::{
    bootstrap_runtime_config, BootstrapConfigError, BootstrapMode, PilotPayload, ServiceBootstrap,
};
use zk_proto_rs::zk::config::v1::ConfigMetadata;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn test_bootstrap() -> GwBootstrapConfig {
    GwBootstrapConfig {
        gw_id: "gw_test_1".into(),
        grpc_host: "127.0.0.1".into(),
        grpc_port: 51051,
        nats_url: Some("nats://localhost:4222".into()),
        bootstrap_token: String::new(),
        instance_type: "GW".into(),
        env: "dev".into(),
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
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let normalized = zk_infra_rs::config_mgmt::normalize_json(&value);
    zk_infra_rs::config_mgmt::sha256_hex(normalized.as_bytes())
}

const SIMULATOR_JSON: &str = r#"{
    "venue": "simulator",
    "account_id": 9001,
    "simulator": {
        "mock_balances": "BTC:5,USDT:50000",
        "fill_delay_ms": 10,
        "match_policy": "fcfs",
        "admin_grpc_port": 51052,
        "enable_admin_controls": true
    }
}"#;

const OKX_JSON: &str = r#"{
    "venue": "okx",
    "account_id": 1234,
    "venue_config": {"api_key": "test_key", "mode": "demo"}
}"#;

const SIMULATOR_INLINE_JSON: &str = r#"{
    "venue": "simulator",
    "account_id": 9001,
    "mock_balances": "BTC:5,USDT:50000",
    "fill_delay_ms": 10,
    "match_policy": "fcfs",
    "admin_grpc_port": 51052,
    "enable_admin_controls": true
}"#;

// ── 1. direct_mode_builds_runtime_from_env ──────────────────────────────────

#[test]
fn direct_mode_builds_runtime_from_env() {
    // Set required env vars for direct mode.
    std::env::set_var("ZK_VENUE", "simulator");
    std::env::set_var("ZK_ACCOUNT_ID", "9001");
    std::env::set_var("ZK_MOCK_BALANCES", "BTC:5,ETH:20");
    std::env::set_var("ZK_MATCH_POLICY", "fcfs");
    std::env::set_var("ZK_EXEC_SHARD_COUNT", "2");
    std::env::set_var("ZK_EXEC_QUEUE_CAPACITY", "128");

    let boot = test_bootstrap();
    let provided = GwBootstrap::load_direct_config().unwrap();
    let runtime = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap();

    assert_eq!(runtime.gw_id, "gw_test_1");
    assert_eq!(runtime.venue, "simulator");
    assert_eq!(runtime.account_id, 9001);
    assert_eq!(runtime.exec_shard_count, 2);
    assert_eq!(runtime.exec_queue_capacity, 128);
    assert!(runtime.simulator.is_some());
    let sim = runtime.simulator.as_ref().unwrap();
    assert_eq!(sim.mock_balances, "BTC:5,ETH:20");
    assert_eq!(sim.match_policy, "fcfs");

    // Cleanup env.
    std::env::remove_var("ZK_VENUE");
    std::env::remove_var("ZK_ACCOUNT_ID");
    std::env::remove_var("ZK_MOCK_BALANCES");
    std::env::remove_var("ZK_MATCH_POLICY");
    std::env::remove_var("ZK_EXEC_SHARD_COUNT");
    std::env::remove_var("ZK_EXEC_QUEUE_CAPACITY");
}

// ── 2. pilot_mode_builds_runtime_from_payload ───────────────────────────────

#[test]
fn pilot_mode_builds_runtime_from_payload() {
    let boot = test_bootstrap();
    let payload = pilot_payload(SIMULATOR_JSON);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let runtime = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap();

    assert_eq!(runtime.gw_id, "gw_test_1");
    assert_eq!(runtime.venue, "simulator");
    assert_eq!(runtime.account_id, 9001);
    assert!(runtime.simulator.is_some());
    let sim = runtime.simulator.as_ref().unwrap();
    assert_eq!(sim.mock_balances, "BTC:5,USDT:50000");
    assert_eq!(sim.fill_delay_ms, 10);
    assert_eq!(sim.match_policy, "fcfs");
    assert_eq!(sim.admin_grpc_port, 51052);
    assert!(sim.enable_admin_controls);
}

// ── 3. malformed_pilot_json_fails ───────────────────────────────────────────

#[test]
fn malformed_pilot_json_fails() {
    let payload = pilot_payload("{not valid json}");
    let err = GwBootstrap::decode_pilot_config(&payload).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

#[test]
fn empty_pilot_json_fails() {
    let payload = pilot_payload("");
    let err = GwBootstrap::decode_pilot_config(&payload).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

// ── 4. missing_required_fields_fails ────────────────────────────────────────

#[test]
fn missing_venue_in_pilot_json_fails() {
    // account_id present but venue missing → serde error.
    let payload = pilot_payload(r#"{"account_id": 9001}"#);
    let err = GwBootstrap::decode_pilot_config(&payload).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

#[test]
fn missing_account_id_in_pilot_json_fails() {
    // venue present but account_id missing → serde error.
    let payload = pilot_payload(r#"{"venue": "simulator"}"#);
    let err = GwBootstrap::decode_pilot_config(&payload).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
}

// ── 5. hash_mismatch_fails ──────────────────────────────────────────────────

#[test]
fn hash_mismatch_fails() {
    let boot = test_bootstrap();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload_with_hash(SIMULATOR_JSON, "badhash"),
        validate_hash: true,
    };
    let err = bootstrap_runtime_config::<GwBootstrap>(&boot, mode).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::HashMismatch { .. }));
}

#[test]
fn hash_match_succeeds() {
    let boot = test_bootstrap();
    let hash = compute_hash(SIMULATOR_JSON);
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload_with_hash(SIMULATOR_JSON, &hash),
        validate_hash: true,
    };
    let outcome = bootstrap_runtime_config::<GwBootstrap>(&boot, mode).unwrap();
    assert_eq!(outcome.runtime_config.venue, "simulator");
}

// ── 6. real_venue_uses_venue_config ─────────────────────────────────────────

#[test]
fn real_venue_uses_venue_config() {
    let boot = test_bootstrap();
    let payload = pilot_payload(OKX_JSON);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let runtime = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap();

    assert_eq!(runtime.venue, "okx");
    assert_eq!(runtime.account_id, 1234);
    assert!(runtime.simulator.is_none());
    assert_eq!(runtime.venue_config["api_key"], "test_key");
    assert_eq!(runtime.venue_config["mode"], "demo");
}

// ── 7. simulator_payload_accepts_simulator_fields ───────────────────────────

#[test]
fn simulator_payload_accepts_simulator_fields() {
    let boot = test_bootstrap();
    let mode = BootstrapMode::Pilot {
        payload: pilot_payload(SIMULATOR_JSON),
        validate_hash: false,
    };
    let outcome = bootstrap_runtime_config::<GwBootstrap>(&boot, mode).unwrap();
    assert_eq!(outcome.runtime_config.venue, "simulator");
    assert!(outcome.runtime_config.simulator.is_some());
}

#[test]
fn simulator_payload_accepts_inline_simulator_fields() {
    let boot = test_bootstrap();
    let payload = pilot_payload(SIMULATOR_INLINE_JSON);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let runtime = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap();

    let sim = runtime.simulator.as_ref().unwrap();
    assert_eq!(runtime.account_id, 9001);
    assert_eq!(sim.mock_balances, "BTC:5,USDT:50000");
    assert_eq!(sim.fill_delay_ms, 10);
    assert_eq!(sim.match_policy, "fcfs");
    assert_eq!(sim.admin_grpc_port, 51052);
    assert!(sim.enable_admin_controls);
}

// ── 8. non_simulator_rejects_simulator_config ───────────────────────────────

#[test]
fn non_simulator_rejects_simulator_config() {
    let json = r#"{
        "venue": "okx",
        "account_id": 1234,
        "simulator": {
            "mock_balances": "BTC:5",
            "match_policy": "immediate"
        }
    }"#;
    let boot = test_bootstrap();
    let payload = pilot_payload(json);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let err = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::Validation(_)));
}

// ── 9. simulator_venue_missing_simulator_config_fails ────────────────────────

#[test]
fn simulator_venue_missing_simulator_config_fails() {
    let json = r#"{"venue": "simulator", "account_id": 9001}"#;
    let boot = test_bootstrap();
    let payload = pilot_payload(json);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let err = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::MissingField { .. }));
}

// ── exec pool validation ────────────────────────────────────────────────────

#[test]
fn exec_shard_count_zero_fails() {
    let json = r#"{"venue": "okx", "account_id": 1234, "exec_shard_count": 0}"#;
    let boot = test_bootstrap();
    let payload = pilot_payload(json);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let err = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::Validation(_)));
}

#[test]
fn exec_queue_capacity_zero_fails() {
    let json = r#"{"venue": "okx", "account_id": 1234, "exec_queue_capacity": 0}"#;
    let boot = test_bootstrap();
    let payload = pilot_payload(json);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let err = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap_err();
    assert!(matches!(err, BootstrapConfigError::Validation(_)));
}

// ── serde defaults ──────────────────────────────────────────────────────────

#[test]
fn pilot_json_uses_serde_defaults() {
    // Minimal valid JSON — only required fields + simulator object.
    let json = r#"{"venue": "simulator", "account_id": 9001, "simulator": {}}"#;
    let boot = test_bootstrap();
    let payload = pilot_payload(json);
    let provided = GwBootstrap::decode_pilot_config(&payload).unwrap();
    let runtime = GwBootstrap::assemble_runtime_config(&boot, provided).unwrap();

    // exec pool defaults
    assert_eq!(runtime.exec_shard_count, 4);
    assert_eq!(runtime.exec_queue_capacity, 256);
    // simulator defaults
    let sim = runtime.simulator.as_ref().unwrap();
    assert_eq!(sim.mock_balances, "BTC:10,USDT:100000,ETH:50");
    assert_eq!(sim.fill_delay_ms, 0);
    assert_eq!(sim.match_policy, "immediate");
    assert_eq!(sim.admin_grpc_port, 51052);
    assert!(!sim.enable_admin_controls);
}

// ── parse_balances utility ──────────────────────────────────────────────────

#[test]
fn parse_balances_normal() {
    let map = zk_gw_svc::config::parse_balances("BTC:10.5,USDT:100000,ETH:50");
    assert_eq!(map["BTC"], 10.5);
    assert_eq!(map["USDT"], 100000.0);
    assert_eq!(map["ETH"], 50.0);
}

#[test]
fn parse_balances_empty() {
    let map = zk_gw_svc::config::parse_balances("");
    assert!(map.is_empty());
}
