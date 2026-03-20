use zk_trading_sdk_rs::config::TradingClientConfig;
use zk_trading_sdk_rs::error::SdkError;

static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap()
}

#[test]
fn test_from_env_parses_required_vars() {
    let _guard = env_lock();
    std::env::set_var("ZK_NATS_URL", "nats://localhost:4222");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001,9002");
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "7");
    std::env::remove_var("ZK_DISCOVERY_BUCKET");
    std::env::remove_var("ZK_REFDATA_GRPC");

    let cfg = TradingClientConfig::from_env().expect("must succeed");

    assert_eq!(cfg.nats_url, "nats://localhost:4222");
    assert_eq!(cfg.env, "dev");
    assert_eq!(cfg.account_ids, vec![9001i64, 9002i64]);
    assert_eq!(cfg.client_instance_id, 7);
    assert_eq!(cfg.discovery_bucket, "zk-svc-registry-v1");
    assert!(cfg.refdata_grpc.is_none());
}

#[test]
fn test_from_env_uses_discovery_bucket_override() {
    let _guard = env_lock();
    std::env::set_var("ZK_NATS_URL", "nats://localhost:4222");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001");
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1");
    std::env::set_var("ZK_DISCOVERY_BUCKET", "my-custom-bucket");
    std::env::remove_var("ZK_REFDATA_GRPC");

    let cfg = TradingClientConfig::from_env().expect("must succeed");
    assert_eq!(cfg.discovery_bucket, "my-custom-bucket");

    std::env::remove_var("ZK_DISCOVERY_BUCKET");
}

#[test]
fn test_from_env_sets_refdata_grpc_override() {
    let _guard = env_lock();
    std::env::set_var("ZK_NATS_URL", "nats://localhost:4222");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001");
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1");
    std::env::remove_var("ZK_DISCOVERY_BUCKET");
    std::env::set_var("ZK_REFDATA_GRPC", "http://refdata:50051");

    let cfg = TradingClientConfig::from_env().expect("must succeed");
    assert_eq!(cfg.refdata_grpc.as_deref(), Some("http://refdata:50051"));

    std::env::remove_var("ZK_REFDATA_GRPC");
}

#[test]
fn test_from_env_fails_when_nats_url_missing() {
    let _guard = env_lock();
    std::env::remove_var("ZK_NATS_URL");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001");
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1");

    let result = TradingClientConfig::from_env();
    assert!(result.is_err(), "must fail when ZK_NATS_URL is missing");
}

#[test]
fn test_from_env_rejects_instance_id_out_of_range() {
    let _guard = env_lock();
    std::env::set_var("ZK_NATS_URL", "nats://localhost:4222");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001");
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1500"); // > 1023

    let result = TradingClientConfig::from_env();
    assert!(
        result.is_err(),
        "must fail when ZK_CLIENT_INSTANCE_ID > 1023"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, SdkError::InstanceIdOutOfRange(1500)),
        "must return InstanceIdOutOfRange(1500), got: {err:?}"
    );

    // restore
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1");
}

#[test]
fn test_from_env_fails_when_instance_id_missing() {
    let _guard = env_lock();
    std::env::set_var("ZK_NATS_URL", "nats://localhost:4222");
    std::env::set_var("ZK_ENV", "dev");
    std::env::set_var("ZK_ACCOUNT_IDS", "9001");
    std::env::remove_var("ZK_CLIENT_INSTANCE_ID");

    let result = TradingClientConfig::from_env();
    assert!(
        result.is_err(),
        "must fail when ZK_CLIENT_INSTANCE_ID is missing"
    );

    // restore
    std::env::set_var("ZK_CLIENT_INSTANCE_ID", "1");
}
