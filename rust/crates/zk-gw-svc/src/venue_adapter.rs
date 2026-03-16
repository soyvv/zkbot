use async_trait::async_trait;

// ─── Venue-facing request/response types ────────────────────────────────────

/// Command to place an order on the venue.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub struct VenueCancelOrder {
    pub exch_order_ref: String,
    pub order_id: i64,
    pub timestamp: i64,
}

/// Acknowledgment from venue after a command.
#[derive(Debug, Clone)]
pub struct VenueCommandAck {
    pub success: bool,
    pub exch_order_ref: Option<String>,
    pub error_message: Option<String>,
}

/// Query for account balances.
#[derive(Debug, Clone)]
pub struct VenueBalanceQuery {
    pub explicit_symbols: Vec<String>,
}

/// Query for orders.
#[derive(Debug, Clone)]
pub struct VenueOrderQuery {
    pub exch_order_ref: Option<String>,
    pub order_id: Option<i64>,
    pub instrument: Option<String>,
}

/// Query for trades.
#[derive(Debug, Clone)]
pub struct VenueTradeQuery {
    pub exch_account_id: Option<String>,
    pub instrument: Option<String>,
    pub start_ts: Option<i64>,
    pub end_ts: Option<i64>,
    pub limit: Option<i64>,
}

/// Query for positions.
#[derive(Debug, Clone)]
pub struct VenuePositionQuery {}

// ─── Venue fact types (pre-semantic-unification) ────────────────────────────

/// A balance fact from the venue.
#[derive(Debug, Clone)]
pub struct VenueBalanceFact {
    pub asset: String,
    pub total_qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
}

/// An order fact from the venue.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, PartialEq)]
pub enum VenueOrderStatus {
    Booked,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// A trade fact from the venue.
#[derive(Debug, Clone)]
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

/// A position fact from the venue.
#[derive(Debug, Clone)]
pub struct VenuePositionFact {
    pub instrument: String,
    pub long_short_type: i32,
    pub qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
    pub account_id: i64,
}

/// A system event from the venue (connectivity changes, etc.).
#[derive(Debug, Clone)]
pub struct VenueSystemEvent {
    pub event_type: VenueSystemEventType,
    pub message: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VenueSystemEventType {
    Connected,
    Disconnected,
    Reconnecting,
    Error,
}

// ─── Unified venue event ────────────────────────────────────────────────────

/// A single event surfaced by the venue adapter.
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

    /// Query trade history.
    async fn query_trades(&self, req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>>;

    /// Query positions.
    async fn query_positions(
        &self,
        req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>>;

    /// Receive the next venue event (order report, balance, position, system).
    /// Blocks until an event is available.
    async fn next_event(&self) -> anyhow::Result<VenueEvent>;
}
