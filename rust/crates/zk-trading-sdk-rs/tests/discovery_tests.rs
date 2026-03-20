use std::collections::HashMap;

use zk_proto_rs::zk::discovery::v1::{ServiceRegistration, TransportEndpoint};
use zk_trading_sdk_rs::discovery::{build_account_map, resolve_refdata_endpoint};

fn make_reg(
    key: &str,
    service_type: &str,
    service_id: &str,
    account_ids: Vec<i64>,
    grpc_addr: &str,
) -> (String, ServiceRegistration) {
    let reg = ServiceRegistration {
        service_type: service_type.to_string(),
        service_id: service_id.to_string(),
        instance_id: key.to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: grpc_addr.to_string(),
            authority: grpc_addr.to_string(),
        }),
        account_ids,
        ..Default::default()
    };
    (key.to_string(), reg)
}

// ── build_account_map tests ───────────────────────────────────────────────────

#[test]
fn test_discovery_resolve_by_account() {
    let mut snapshot = HashMap::new();
    snapshot.extend([
        make_reg(
            "svc.oms.oms_dev_1",
            "oms",
            "oms_dev_1",
            vec![9001, 9002],
            "localhost:50051",
        ),
        make_reg(
            "svc.oms.oms_dev_2",
            "oms",
            "oms_dev_2",
            vec![9003],
            "localhost:50052",
        ),
    ]);

    let map = build_account_map(&snapshot).expect("must succeed");

    assert_eq!(
        map.get(&9001).expect("9001 must resolve").grpc_address,
        "localhost:50051"
    );
    assert_eq!(
        map.get(&9002).expect("9002 must resolve").grpc_address,
        "localhost:50051"
    );
    assert_eq!(
        map.get(&9003).expect("9003 must resolve").grpc_address,
        "localhost:50052"
    );
}

#[test]
fn test_discovery_ignores_non_oms_entries() {
    let mut snapshot = HashMap::new();
    snapshot.extend([
        make_reg(
            "svc.oms.oms_dev_1",
            "oms",
            "oms_dev_1",
            vec![9001],
            "localhost:50051",
        ),
        make_reg(
            "svc.gw.gw_dev_1",
            "gw",
            "gw_dev_1",
            vec![9001],
            "localhost:50060",
        ),
        make_reg(
            "svc.refdata.refdata_dev_1",
            "refdata",
            "refdata_dev_1",
            vec![],
            "localhost:50070",
        ),
        make_reg(
            "svc.engine.eng_dev_1",
            "engine",
            "eng_dev_1",
            vec![],
            "localhost:50080",
        ),
    ]);

    let map = build_account_map(&snapshot).expect("must succeed");

    // Only OMS entry should be in the account map
    assert_eq!(map.len(), 1);
    assert_eq!(
        map.get(&9001).expect("9001 must resolve").grpc_address,
        "localhost:50051"
    );
}

#[test]
fn test_discovery_conflict_on_duplicate_account_owner() {
    let mut snapshot = HashMap::new();
    snapshot.extend([
        make_reg(
            "svc.oms.oms_a",
            "oms",
            "oms_a",
            vec![9001],
            "localhost:50051",
        ),
        make_reg(
            "svc.oms.oms_b",
            "oms",
            "oms_b",
            vec![9001],
            "localhost:50052",
        ), // same account!
    ]);

    let result = build_account_map(&snapshot);
    assert!(
        result.is_err(),
        "duplicate account ownership must return error"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("9001") || err.to_string().to_lowercase().contains("conflict"),
        "error should mention account or conflict: {err}"
    );
}

#[test]
fn test_discovery_oms_without_endpoint_is_skipped() {
    let mut snapshot = HashMap::new();
    // OMS with no endpoint set
    let bad_reg = ServiceRegistration {
        service_type: "oms".to_string(),
        service_id: "oms_no_ep".to_string(),
        instance_id: "svc.oms.no_ep".to_string(),
        endpoint: None,
        account_ids: vec![9001],
        ..Default::default()
    };
    snapshot.insert("svc.oms.no_ep".to_string(), bad_reg);

    // No conflict, but account 9001 has no usable endpoint — should either skip or error
    // Expect: no panic; either Ok(empty) or Err is acceptable but not Ok(map_with_no_ep_entry)
    let result = build_account_map(&snapshot);
    if let Ok(map) = result {
        assert!(
            map.get(&9001).is_none(),
            "entry without endpoint must not be in the account map"
        );
    }
    // If Err, that's also acceptable
}

#[test]
fn test_discovery_oms_id_from_service_id() {
    let mut snapshot = HashMap::new();
    // Key is full KV path, service_id is the stable logical ID
    snapshot.extend([make_reg(
        "svc.oms.oms_a",
        "oms",
        "oms_a",
        vec![9001],
        "localhost:50051",
    )]);

    let map = build_account_map(&snapshot).expect("must succeed");
    let ep = map.get(&9001).expect("9001 must resolve");

    // oms_id must come from service_id ("oms_a"), NOT the full key ("svc.oms.oms_a")
    assert_eq!(
        ep.oms_id, "oms_a",
        "oms_id must be service_id, not full KV key"
    );
}

#[test]
fn test_discovery_oms_id_falls_back_to_key_suffix() {
    let mut snapshot = HashMap::new();
    // service_id is empty — fallback to last dot-delimited segment of key
    let reg = ServiceRegistration {
        service_type: "oms".to_string(),
        service_id: "".to_string(), // intentionally empty
        instance_id: "svc.oms.oms_a".to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: "localhost:50051".to_string(),
            authority: "localhost:50051".to_string(),
        }),
        account_ids: vec![9001],
        ..Default::default()
    };
    snapshot.insert("svc.oms.oms_a".to_string(), reg);

    let map = build_account_map(&snapshot).expect("must succeed");
    let ep = map.get(&9001).expect("9001 must resolve");

    // Falls back to last segment of key: "svc.oms.oms_a" → "oms_a"
    assert_eq!(
        ep.oms_id, "oms_a",
        "oms_id must fall back to last key segment"
    );
}

/// Entries with a past `lease_expiry_ms` must be treated as expired and skipped.
/// This matches `api_contracts.md` line 52: consumers must evict entries past TTL.
#[test]
fn test_discovery_skips_expired_oms_lease() {
    let mut snapshot = HashMap::new();
    // Lease expired at epoch+1ms — always in the past
    let expired_reg = ServiceRegistration {
        service_type: "oms".to_string(),
        service_id: "oms_a".to_string(),
        instance_id: "svc.oms.oms_a".to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: "localhost:50051".to_string(),
            authority: "localhost:50051".to_string(),
        }),
        account_ids: vec![9001],
        lease_expiry_ms: 1, // epoch+1ms — definitely expired
        ..Default::default()
    };
    snapshot.insert("svc.oms.oms_a".to_string(), expired_reg);

    let map = build_account_map(&snapshot).expect("must succeed");
    assert!(
        map.get(&9001).is_none(),
        "expired OMS lease must not appear in account map"
    );
}

/// Entries with `lease_expiry_ms == 0` are treated as having no expiry (permanent / no TTL).
#[test]
fn test_discovery_zero_lease_expiry_is_not_expired() {
    let mut snapshot = HashMap::new();
    snapshot.extend([
        make_reg(
            "svc.oms.oms_a",
            "oms",
            "oms_a",
            vec![9001],
            "localhost:50051",
        ),
        // make_reg sets lease_expiry_ms = 0 via Default
    ]);

    let map = build_account_map(&snapshot).expect("must succeed");
    assert!(
        map.get(&9001).is_some(),
        "zero lease_expiry_ms means no expiry — must be included"
    );
}

/// `is_service_type` uses case-insensitive comparison so that the SDK tolerates
/// current services which register uppercase `"OMS"` while the architecture contract
/// specifies lowercase `"oms"`.
#[test]
fn test_discovery_accepts_uppercase_service_type_for_legacy_oms() {
    let mut snapshot = HashMap::new();
    // Simulate current zk-oms-svc which registers "OMS" (uppercase)
    let legacy_reg = ServiceRegistration {
        service_type: "OMS".to_string(), // uppercase — current OMS service behaviour
        service_id: "oms_dev_1".to_string(),
        instance_id: "svc.oms.oms_dev_1".to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: "localhost:50051".to_string(),
            authority: "localhost:50051".to_string(),
        }),
        account_ids: vec![9001],
        ..Default::default()
    };
    snapshot.insert("svc.oms.oms_dev_1".to_string(), legacy_reg);

    let map = build_account_map(&snapshot).expect("must succeed");
    assert!(
        map.get(&9001).is_some(),
        "case-insensitive match must accept uppercase 'OMS' from legacy services"
    );
}

/// An entry whose KV key does NOT start with `svc.oms.` is excluded even if
/// service_type == "oms" — the key prefix is an additional guard.
#[test]
fn test_discovery_ignores_oms_entry_with_wrong_key_prefix() {
    let mut snapshot = HashMap::new();
    // key doesn't start with "svc.oms."
    let miskeyed = ServiceRegistration {
        service_type: "oms".to_string(),
        service_id: "oms_a".to_string(),
        instance_id: "oms_a".to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: "localhost:50051".to_string(),
            authority: "localhost:50051".to_string(),
        }),
        account_ids: vec![9001],
        ..Default::default()
    };
    snapshot.insert("oms.dev.1".to_string(), miskeyed); // wrong prefix

    let map = build_account_map(&snapshot).expect("must succeed");
    assert!(
        map.get(&9001).is_none(),
        "entry with wrong key prefix must be ignored"
    );
}

/// Expired refdata lease must not be returned by resolve_refdata_endpoint.
#[test]
fn test_refdata_discovery_skips_expired_lease() {
    let mut snapshot = HashMap::new();
    let expired = ServiceRegistration {
        service_type: "refdata".to_string(),
        service_id: "refdata_dev_1".to_string(),
        instance_id: "svc.refdata.refdata_dev_1".to_string(),
        endpoint: Some(TransportEndpoint {
            protocol: "grpc".to_string(),
            address: "localhost:50070".to_string(),
            authority: "localhost:50070".to_string(),
        }),
        account_ids: vec![],
        lease_expiry_ms: 1, // expired
        ..Default::default()
    };
    snapshot.insert("svc.refdata.refdata_dev_1".to_string(), expired);

    let endpoint = resolve_refdata_endpoint(&snapshot);
    assert!(
        endpoint.is_none(),
        "expired refdata lease must not be returned"
    );
}

// ── resolve_refdata_endpoint tests ───────────────────────────────────────────

#[test]
fn test_refdata_discovery_scans_svc_refdata_prefix() {
    let mut snapshot = HashMap::new();
    snapshot.extend([
        make_reg(
            "svc.oms.oms_dev_1",
            "oms",
            "oms_dev_1",
            vec![9001],
            "localhost:50051",
        ),
        make_reg(
            "svc.refdata.refdata_dev_1",
            "refdata",
            "refdata_dev_1",
            vec![],
            "localhost:50070",
        ),
    ]);

    let endpoint = resolve_refdata_endpoint(&snapshot);
    assert_eq!(endpoint.as_deref(), Some("localhost:50070"));
}

#[test]
fn test_refdata_discovery_returns_none_when_not_present() {
    let mut snapshot = HashMap::new();
    snapshot.extend([make_reg(
        "svc.oms.oms_dev_1",
        "oms",
        "oms_dev_1",
        vec![9001],
        "localhost:50051",
    )]);

    let endpoint = resolve_refdata_endpoint(&snapshot);
    assert!(endpoint.is_none());
}
