// Generated protobuf types for zkbot.
//
// Legacy modules (below) are compiled by `buf generate` and committed to the repo.
// New versioned modules under `zk::` are compiled by `build.rs` at build time.

// ===== Legacy committed modules =====

pub mod common;
pub mod exch_gw;
pub mod ods;
pub mod oms;
pub mod rtmd;
pub mod strategy;
pub mod tqrpc_exch_gw;
pub mod tqrpc_ods;
pub mod tqrpc_oms;
pub mod tqrpc_ref;
pub mod tqrpc_rtmd;
pub mod tqrpc_strategy;

// ===== New versioned zk.* modules (compiled via build.rs / prost-build) =====

pub mod zk {
    pub mod common {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.common.v1.rs"));
        }
    }
    pub mod config {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.config.v1.rs"));
        }
    }
    pub mod discovery {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.discovery.v1.rs"));
        }
    }
    pub mod oms {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.oms.v1.rs"));
        }
    }
    pub mod exch_gw {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.exch_gw.v1.rs"));
        }
    }
    pub mod gateway {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.gateway.v1.rs"));
        }
    }
    pub mod strategy {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.strategy.v1.rs"));
        }
    }
    pub mod rtmd {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.rtmd.v1.rs"));
        }
    }
    pub mod engine {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.engine.v1.rs"));
        }
    }
    pub mod monitor {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.monitor.v1.rs"));
        }
    }
    pub mod pilot {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.pilot.v1.rs"));
        }
    }
    pub mod recorder {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/zk.recorder.v1.rs"));
        }
    }
}
