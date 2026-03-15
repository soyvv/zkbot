//! SDK domain model types.

use serde::{Deserialize, Serialize};

/// A trading order submitted by a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingOrder {
    /// Pre-allocated order ID (from `TradingClient::next_order_id()`).
    /// If 0, the SDK will assign one before sending.
    pub order_id: i64,
    pub instrument_id: String,
    pub side: Side,
    pub order_type: OrderType,
    pub price: f64,
    pub qty: f64,
    pub source_id: String,
}

/// A cancel request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingCancel {
    pub order_id: i64,
    pub exch_order_ref: String,
    pub source_id: String,
}

/// Ack returned from an OMS command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAck {
    pub success: bool,
    pub message: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    Market,
}
