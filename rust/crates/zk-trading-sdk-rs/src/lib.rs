//! ZKBot Trading SDK — Rust client library replacing TQClient.
//!
//! Provides ODS-free service discovery, OMS gRPC command/query, RTMD subscriptions,
//! Snowflake order-ID generation, and RefdataSdk.

pub mod client;
pub use client::TradingClient;
pub mod config;
pub mod discovery;
pub mod error;
pub mod id_gen;
pub mod model;
pub mod oms;
pub mod refdata;
pub mod rtmd_sub;
pub mod stream;

pub mod types {
    pub use crate::model::{CommandAck, OrderType, Side, TradingCancel, TradingOrder};
    pub use crate::proto::refdata_svc::{InstrumentRefdataResponse, MarketStatusResponse};
    pub use zk_proto_rs::zk::oms::v1::{
        Balance, BalanceUpdateEvent, Order, OrderUpdateEvent, Position, PositionUpdateEvent,
    };
    pub use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};
}

/// Tonic-generated gRPC client stubs (OMS + Refdata services).
pub mod proto {
    pub mod oms_svc {
        include!(concat!(env!("OUT_DIR"), "/zk.oms.v1.rs"));
    }
    pub mod refdata_svc {
        include!(concat!(env!("OUT_DIR"), "/zk.refdata.v1.rs"));
    }
}
