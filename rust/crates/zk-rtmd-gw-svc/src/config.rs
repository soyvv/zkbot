/// Configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct RtmdGwConfig {
    pub mdgw_id: String,
    pub venue: String,
    pub nats_url: String,
    pub grpc_port: u16,
    /// KV prefix for service registration (default: "svc.mdgw")
    pub gateway_kv_prefix: String,
    /// KV bucket for RTMD subscription leases (default: "zk-rtmd-subs-v1")
    pub rtmd_sub_bucket: String,
    /// Lease TTL in seconds (default: 60). KV bucket TTL = lease_ttl_s * 3.
    pub sub_lease_ttl_s: u64,
}

impl RtmdGwConfig {
    pub fn from_env() -> Self {
        Self {
            mdgw_id: std::env::var("ZK_MDGW_ID").unwrap_or_else(|_| "mdgw_dev_1".to_string()),
            venue: std::env::var("ZK_VENUE").unwrap_or_else(|_| "simulator".to_string()),
            nats_url: std::env::var("ZK_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            grpc_port: std::env::var("ZK_GRPC_PORT")
                .unwrap_or_else(|_| "52051".to_string())
                .parse()
                .unwrap_or(52051),
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
