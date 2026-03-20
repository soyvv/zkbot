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
    /// Optional trigger context for engine→OMS latency correlation.
    /// Populated by engine's `TradingDispatcher`; `None` for non-engine callers.
    #[serde(skip)]
    pub trigger_context: Option<zk_proto_rs::zk::common::v1::TriggerContext>,
}

/// A cancel request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingCancel {
    pub order_id: i64,
    pub exch_order_ref: String,
    pub source_id: String,
    /// Optional trigger context for engine→OMS latency correlation.
    #[serde(skip)]
    pub trigger_context: Option<zk_proto_rs::zk::common::v1::TriggerContext>,
}

/// Acknowledgement returned from an OMS command (place, cancel, batch).
///
/// **Important:** `success == true` means the OMS has validated and accepted the request
/// for asynchronous processing. It does **not** mean the order has been sent to the
/// exchange gateway, booked, or executed. The final execution outcome arrives
/// asynchronously via order-update subscriptions (`subscribe_order_updates`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAck {
    /// `true` = request accepted and queued for processing; `false` = rejected at OMS validation.
    pub success: bool,
    /// Human-readable status message from OMS.
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
