/// Proto module re-exports for gateway service stubs.
///
/// GatewayService server + GatewaySimulatorAdminService server are generated
/// by build.rs from the gateway protos. Message types come from zk-proto-rs.

pub mod zk_gw_v1 {
    tonic::include_proto!("zk.gateway.v1");
}

// Re-export commonly used proto types from zk-proto-rs for convenience.
pub use zk_proto_rs::zk::common::v1 as common;
pub use zk_proto_rs::zk::exch_gw::v1 as exch_gw;
