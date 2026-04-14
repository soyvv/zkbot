use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Venue-facing request/response types ────────────────────────────────────

/// Command to place an order on the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenuePlaceOrder {
    pub correlation_id: i64,
    pub exch_account_id: String,
    pub instrument: String,
    pub buysell_type: i32,
    pub openclose_type: i32,
    pub order_type: i32,
    pub price: f64,
    pub qty: f64,
    pub leverage: f64,
    pub timestamp: i64,
}

/// Command to cancel an order on the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueCancelOrder {
    pub exch_order_ref: String,
    pub order_id: i64,
    pub timestamp: i64,
}

/// Acknowledgment from venue after a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueCommandAck {
    pub success: bool,
    pub exch_order_ref: Option<String>,
    pub error_message: Option<String>,
}

/// Query for account balances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueBalanceQuery {
    pub explicit_symbols: Vec<String>,
}

/// Query for orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOrderQuery {
    pub exch_order_ref: Option<String>,
    pub order_id: Option<i64>,
    pub instrument: Option<String>,
}

/// Query for open orders, primarily used for startup/reconnect resync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VenueOpenOrdersQuery {
    pub instrument: Option<String>,
    pub updated_since_ts: Option<i64>,
    pub limit: Option<u32>,
}

/// Direction for cursor-based venue pagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenuePageDirection {
    Forward,
    Backward,
}

/// Query for trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueTradeQuery {
    pub exch_account_id: Option<String>,
    pub instrument: Option<String>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub limit: Option<i64>,
    /// Optional venue paging cursor or page token.
    pub page_cursor: Option<String>,
    /// Direction for cursor-based paging when the venue supports it.
    pub page_direction: Option<VenuePageDirection>,
    /// Explicit page size when the venue separates page size from result limit.
    pub page_size: Option<u32>,
}

/// Query for positions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenuePositionQuery {}

/// Query for funding fees or analogous venue financing charges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VenueFundingFeeQuery {
    pub instrument: Option<String>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub limit: Option<u32>,
    pub page_cursor: Option<String>,
    pub page_direction: Option<VenuePageDirection>,
    pub page_size: Option<u32>,
}

// ─── Venue fact types (pre-semantic-unification) ────────────────────────────

/// A balance fact from the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueBalanceFact {
    pub asset: String,
    pub total_qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
}

/// An order fact from the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueOrderFact {
    pub order_id: i64,
    pub exch_order_ref: String,
    pub instrument: String,
    pub status: VenueOrderStatus,
    pub filled_qty: f64,
    pub unfilled_qty: f64,
    pub avg_price: f64,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueOrderStatus {
    Booked,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// A trade fact from the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueTradeFact {
    pub exch_trade_id: String,
    pub order_id: i64,
    pub exch_order_ref: String,
    pub instrument: String,
    pub buysell_type: i32,
    pub filled_qty: f64,
    pub filled_price: f64,
    pub timestamp: i64,
}

/// A funding fee or similar financing-charge fact from the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueFundingFeeFact {
    pub instrument: String,
    pub fee_asset: String,
    pub fee_qty: f64,
    pub timestamp: i64,
    pub venue_ref: Option<String>,
}

/// A position fact from the venue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenuePositionFact {
    pub instrument: String,
    pub long_short_type: i32,
    pub qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
    pub account_id: i64,
    /// Venue-authoritative instrument type (proto `InstrumentType` i32).
    /// When non-zero, the gateway host uses this instead of string heuristics.
    #[serde(default)]
    pub instrument_type: i32,
}

/// A system event from the venue (connectivity changes, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueSystemEvent {
    pub event_type: VenueSystemEventType,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueSystemEventType {
    Connected,
    Disconnected,
    Reconnecting,
    Error,
}

// ─── Unified venue event ────────────────────────────────────────────────────

/// A single event surfaced by the venue adapter.
///
/// Note: `VenueEvent` itself does NOT derive Serialize/Deserialize because the
/// `OrderReport` variant contains a prost proto message. The Python bridge uses
/// proto bytes for this variant instead of serde (see py_types.rs).
#[derive(Debug, Clone)]
pub enum VenueEvent {
    /// An order report (linkage, state change, fill).
    OrderReport(zk_proto_rs::zk::exch_gw::v1::OrderReport),
    /// A balance update snapshot.
    Balance(Vec<VenueBalanceFact>),
    /// A position update snapshot.
    Position(Vec<VenuePositionFact>),
    /// A system/lifecycle event.
    System(VenueSystemEvent),
}

// ─── VenueAdapter trait ─────────────────────────────────────────────────────

/// Trait for venue-specific adapters.
///
/// Implementations translate gateway commands into venue requests, manage
/// venue connectivity, and surface normalized venue facts.
///
/// Semantic unification (trade exactly-once, order at-least-once,
/// balance/position causality) belongs in the gateway service layer,
/// NOT in the adapter.
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    /// Establish connectivity to the venue.
    async fn connect(&self) -> anyhow::Result<()>;

    /// Place an order on the venue.
    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck>;

    /// Cancel an order on the venue.
    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck>;

    /// Query account balances.
    async fn query_balance(&self, req: VenueBalanceQuery) -> anyhow::Result<Vec<VenueBalanceFact>>;

    /// Query order details.
    async fn query_order(&self, req: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>>;

    /// Query open orders for startup/reconnect resync.
    async fn query_open_orders(
        &self,
        req: VenueOpenOrdersQuery,
    ) -> anyhow::Result<Vec<VenueOrderFact>>;

    /// Query trade history.
    async fn query_trades(&self, req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>>;

    /// Query funding fees or similar financing charges.
    ///
    /// Optional because many venues do not expose these semantics or expose them
    /// only through cold-path account statements.
    async fn query_funding_fees(
        &self,
        _req: VenueFundingFeeQuery,
    ) -> anyhow::Result<Vec<VenueFundingFeeFact>> {
        Err(anyhow::anyhow!("funding fee query not supported by this venue adaptor"))
    }

    /// Query positions.
    async fn query_positions(
        &self,
        req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>>;

    /// Receive the next venue event (order report, balance, position, system).
    /// Blocks until an event is available.
    async fn next_event(&self) -> anyhow::Result<VenueEvent>;

    /// Gracefully shut down the adapter: cancel background tasks, close
    /// connections, and release resources. Called by the gateway host before
    /// process exit. The default implementation is a no-op.
    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
