use zk_trading_sdk_rs::client::TradingClient;
use zk_trading_sdk_rs::config::TradingClientConfig;
use zk_trading_sdk_rs::error::SdkError;

/// `from_config` must fail fast with `InstanceIdOutOfRange` before attempting any NATS I/O.
#[tokio::test]
async fn test_from_config_fails_with_instance_id_out_of_range() {
    let cfg = TradingClientConfig {
        nats_url: "nats://localhost:4222".to_string(),
        env: "test".to_string(),
        account_ids: vec![9001],
        client_instance_id: 1024, // > 1023: invalid
        discovery_bucket: "zk-svc-registry-v1".to_string(),
        refdata_grpc: None,
    };

    let err = match TradingClient::from_config(cfg).await {
        Err(e) => e,
        Ok(_) => panic!("expected Err but got Ok"),
    };
    assert!(
        matches!(err, SdkError::InstanceIdOutOfRange(1024)),
        "must return InstanceIdOutOfRange(1024), got: {err:?}"
    );
}
