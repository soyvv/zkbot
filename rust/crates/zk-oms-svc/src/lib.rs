/// Public re-exports for integration/unit tests.
///
/// The binary modules are exposed here so that `tests/` can import them
/// without duplicating code.  This is the only purpose of this lib target.
pub mod config;
pub mod config_introspection;
pub mod db;
pub mod executor;
pub mod grpc_handler;
pub mod gw_client;
pub mod gw_executor;
pub mod latency;
pub mod nats_handler;
pub mod oms_actor;
pub mod persist_executor;
pub mod proto;
pub mod publish_executor;
pub mod recorder_executor;
pub mod reconcile;
pub mod redis_writer;
