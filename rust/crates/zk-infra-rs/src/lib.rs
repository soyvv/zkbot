// zk-infra-rs: infrastructure adapters for all zkbot service binaries.
// Domain crates (zk-oms-rs, zk-engine-rs, etc.) have no infra dependencies.

pub mod bootstrap;
pub mod config_mgmt;
pub mod discovery_registration;
pub mod config;
pub mod grpc;
pub mod vault;
pub mod mongo;
pub mod nats;
pub mod nats_js;
pub mod nats_kv;
pub mod nats_kv_discovery;
pub mod pg;
pub mod redis;
pub mod service_registry;
pub mod tracing;
