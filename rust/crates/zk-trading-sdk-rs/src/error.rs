//! SDK error types.

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("NATS error: {0}")]
    Nats(#[from] async_nats::Error),

    #[error("gRPC error: {0}")]
    Grpc(#[from] tonic::Status),

    #[error("gRPC transport error: {0}")]
    GrpcTransport(#[from] tonic::transport::Error),

    #[error("discovery conflict: {0}")]
    DiscoveryConflict(String),

    #[error("discovery: no OMS found for account {0}")]
    OmsNotFound(i64),

    #[error("discovery: no refdata service found in registry")]
    RefdataServiceNotFound,

    #[error("refdata: entry deprecated for {0}")]
    RefdataDeprecated(String),

    #[error("refdata: stale entry served for {0} (gRPC unavailable)")]
    RefdataStale(String),

    #[error("refdata: not available for {0} (no cached entry and gRPC unreachable)")]
    RefdataNotAvailable(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("instance_id not set: provide ZK_CLIENT_INSTANCE_ID or use Pilot-granted enriched_config")]
    InstanceIdMissing,

    #[error("instance_id {0} out of range (must be 0–1023)")]
    InstanceIdOutOfRange(u16),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("decode error: {0}")]
    Decode(String),
}
