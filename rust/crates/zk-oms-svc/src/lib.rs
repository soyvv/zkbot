/// Public re-exports for integration/unit tests.
///
/// The binary modules are exposed here so that `tests/` can import them
/// without duplicating code.  This is the only purpose of this lib target.
pub mod config;
pub mod db;
pub mod gw_client;
pub mod grpc_handler;
pub mod latency;
pub mod nats_handler;
pub mod oms_actor;
pub mod proto;
pub mod redis_writer;
