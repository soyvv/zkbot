//! Shared helpers for building proto-encoded service registration payloads.
//!
//! All services should use these helpers to build their `ServiceRegistration`
//! proto and encode it to `Bytes` before passing to
//! [`ServiceRegistration::register_direct`](crate::service_registry::ServiceRegistration::register_direct).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use prost::Message;
use zk_proto_rs::zk::discovery::v1::{
    ServiceRegistration as SvcRegProto, TransportEndpoint,
};

/// Build a gRPC transport endpoint.
pub fn grpc_endpoint(address: impl Into<String>, authority: Option<String>) -> TransportEndpoint {
    TransportEndpoint {
        protocol: "grpc".to_string(),
        address: address.into(),
        authority: authority.unwrap_or_default(),
    }
}

/// Encode a `ServiceRegistration` proto to `Bytes` for KV storage.
pub fn encode_registration(reg: &SvcRegProto) -> Bytes {
    Bytes::from(reg.encode_to_vec())
}

/// Build an engine service registration.
pub fn engine_registration(
    engine_id: &str,
    grpc_address: &str,
    execution_id: &str,
    strategy_key: &str,
    account_ids: &[i64],
) -> SvcRegProto {
    let mut attrs = HashMap::new();
    attrs.insert("execution_id".to_string(), execution_id.to_string());
    attrs.insert("strategy_key".to_string(), strategy_key.to_string());
    SvcRegProto {
        service_type: "engine".to_string(),
        service_id: engine_id.to_string(),
        instance_id: execution_id.to_string(),
        endpoint: Some(grpc_endpoint(grpc_address, None)),
        account_ids: account_ids.to_vec(),
        attrs,
        updated_at_ms: now_ms(),
        ..Default::default()
    }
}

/// Build an OMS service registration.
pub fn oms_registration(
    oms_id: &str,
    grpc_address: &str,
    account_ids: &[i64],
) -> SvcRegProto {
    SvcRegProto {
        service_type: "oms".to_string(),
        service_id: oms_id.to_string(),
        endpoint: Some(grpc_endpoint(grpc_address, None)),
        account_ids: account_ids.to_vec(),
        updated_at_ms: now_ms(),
        ..Default::default()
    }
}

/// Build an MDGW (market data gateway) service registration.
pub fn mdgw_registration(
    mdgw_id: &str,
    grpc_address: &str,
    venue: &str,
    capabilities: Vec<String>,
    metadata: HashMap<String, String>,
) -> SvcRegProto {
    SvcRegProto {
        service_type: "mdgw".to_string(),
        service_id: mdgw_id.to_string(),
        endpoint: Some(grpc_endpoint(grpc_address, None)),
        venue: venue.to_string(),
        capabilities,
        attrs: metadata,
        updated_at_ms: now_ms(),
        ..Default::default()
    }
}

/// Build a GW (exchange gateway) service registration.
///
/// When `admin_grpc_port` is provided, it is stored in the `attrs` map so that
/// Pilot can discover the simulator admin gRPC surface without port conventions.
pub fn gw_registration(
    gw_id: &str,
    grpc_address: &str,
    venue: &str,
    account_id: i64,
    admin_grpc_port: Option<u16>,
) -> SvcRegProto {
    let mut attrs = HashMap::new();
    attrs.insert("account_id".to_string(), account_id.to_string());
    if let Some(port) = admin_grpc_port {
        attrs.insert("admin_grpc_port".to_string(), port.to_string());
    }
    SvcRegProto {
        service_type: "gw".to_string(),
        service_id: gw_id.to_string(),
        endpoint: Some(grpc_endpoint(grpc_address, None)),
        venue: venue.to_string(),
        account_ids: vec![account_id],
        attrs,
        updated_at_ms: now_ms(),
        ..Default::default()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use zk_proto_rs::zk::discovery::v1::ServiceRegistration as SvcRegProto;

    #[test]
    fn engine_registration_roundtrip() {
        let reg = engine_registration("eng1", "127.0.0.1:50060", "eng1-abc", "strat_a", &[1, 2]);
        let bytes = encode_registration(&reg);
        let decoded = SvcRegProto::decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded.service_type, "engine");
        assert_eq!(decoded.service_id, "eng1");
        assert_eq!(decoded.instance_id, "eng1-abc");
        assert_eq!(decoded.account_ids, vec![1, 2]);
        assert_eq!(decoded.endpoint.unwrap().address, "127.0.0.1:50060");
        assert_eq!(decoded.attrs.get("strategy_key").unwrap(), "strat_a");
    }

    #[test]
    fn oms_registration_roundtrip() {
        let reg = oms_registration("oms1", "127.0.0.1:50051", &[100, 200]);
        let bytes = encode_registration(&reg);
        let decoded = SvcRegProto::decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded.service_type, "oms");
        assert_eq!(decoded.service_id, "oms1");
        assert_eq!(decoded.account_ids, vec![100, 200]);
        assert_eq!(decoded.endpoint.unwrap().address, "127.0.0.1:50051");
    }

    #[test]
    fn mdgw_registration_roundtrip() {
        let mut meta = HashMap::new();
        meta.insert("publisher_mode".to_string(), "standalone".to_string());
        let caps = vec!["tick".to_string(), "kline".to_string()];
        let reg = mdgw_registration("mdgw1", "127.0.0.1:52051", "binance", caps.clone(), meta);
        let bytes = encode_registration(&reg);
        let decoded = SvcRegProto::decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded.service_type, "mdgw");
        assert_eq!(decoded.venue, "binance");
        assert_eq!(decoded.capabilities, caps);
        assert_eq!(decoded.attrs.get("publisher_mode").unwrap(), "standalone");
    }

    #[test]
    fn gw_registration_roundtrip() {
        let reg = gw_registration("gw1", "127.0.0.1:51051", "simulator", 9001, Some(51052));
        let bytes = encode_registration(&reg);
        let decoded = SvcRegProto::decode(bytes.as_ref()).unwrap();
        assert_eq!(decoded.service_type, "gw");
        assert_eq!(decoded.service_id, "gw1");
        assert_eq!(decoded.venue, "simulator");
        assert_eq!(decoded.account_ids, vec![9001]);
    }
}
