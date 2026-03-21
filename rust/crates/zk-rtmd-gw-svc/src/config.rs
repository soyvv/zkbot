/// Configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct RtmdGwConfig {
    pub mdgw_id: String,
    pub venue: String,
    pub nats_url: String,
    pub grpc_port: u16,
    /// Advertised host used in service registration gRPC endpoint.
    /// Set to the container/pod hostname or IP reachable by query clients.
    /// Default "127.0.0.1" is suitable for local dev / docker-compose.
    pub grpc_host: String,
    /// Opaque venue-specific config blob (parsed from ZK_VENUE_CONFIG JSON).
    pub venue_config: serde_json::Value,
    /// Root directory for venue integration modules (ZK_VENUE_ROOT).
    pub venue_root: Option<String>,
    /// KV prefix for service registration (default: "svc.mdgw")
    pub gateway_kv_prefix: String,
    /// KV bucket for RTMD subscription leases (default: "zk-rtmd-subs-v1")
    pub rtmd_sub_bucket: String,
    /// Lease TTL in seconds (default: 60). KV bucket TTL = lease_ttl_s * 3.
    pub sub_lease_ttl_s: u64,
}

impl RtmdGwConfig {
    pub fn from_env() -> Self {
        let venue_config_str =
            std::env::var("ZK_VENUE_CONFIG").unwrap_or_else(|_| "{}".to_string());
        let venue_config: serde_json::Value = serde_json::from_str(&venue_config_str)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        Self {
            mdgw_id: std::env::var("ZK_MDGW_ID").unwrap_or_else(|_| "mdgw_dev_1".to_string()),
            venue: std::env::var("ZK_VENUE").unwrap_or_else(|_| "simulator".to_string()),
            venue_config,
            venue_root: std::env::var("ZK_VENUE_ROOT").ok(),
            nats_url: std::env::var("ZK_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            grpc_port: std::env::var("ZK_GRPC_PORT")
                .unwrap_or_else(|_| "52051".to_string())
                .parse()
                .unwrap_or(52051),
            grpc_host: std::env::var("ZK_GRPC_HOST")
                .unwrap_or_else(|_| "127.0.0.1".to_string()),
            gateway_kv_prefix: std::env::var("ZK_GATEWAY_KV_PREFIX")
                .unwrap_or_else(|_| "svc.mdgw".to_string()),
            rtmd_sub_bucket: std::env::var("ZK_RTMD_SUB_BUCKET")
                .unwrap_or_else(|_| "zk-rtmd-subs-v1".to_string()),
            sub_lease_ttl_s: std::env::var("ZK_SUB_LEASE_TTL_S")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .unwrap_or(60),
        }
    }
}
