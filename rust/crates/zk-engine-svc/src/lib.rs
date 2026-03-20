//! `zk-engine-svc` — production engine service wrapping `zk-engine-rs`.
//!
//! Thin runtime: composes Pilot bootstrap, TradingClient subscriptions,
//! LiveEngine event loop, gRPC query/control API, and KV-based liveness.

pub mod bootstrap;
pub mod config;
pub mod control_api;
pub mod dispatcher;
pub mod query_api;
pub mod rehydration;
pub mod runtime;
pub mod subscriptions;
pub mod supervision;

pub mod proto {
    pub mod engine_svc {
        tonic::include_proto!("zk.engine.v1");
    }
}
