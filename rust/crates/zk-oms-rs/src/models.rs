use zk_proto_rs::{
    zk::{
        common::v1::InstrumentRefData,
        exch_gw::v1::{BalanceUpdate, OrderReport},
        gateway::v1::{CancelOrderRequest as ExchCancelOrderRequest, SendOrderRequest as ExchSendOrderRequest, BatchSendOrdersRequest as ExchBatchSendOrdersRequest, BatchCancelOrdersRequest as ExchBatchCancelOrdersRequest},
        oms::v1::{ExecMessage, Fee, Order, OrderCancelRequest, OrderRequest, OrderUpdateEvent, PositionUpdateEvent, Position, Trade},
    },
    ods::{GwConfigEntry, OmsRouteEntry},
};
use crate::config::InstrumentTradingConfig;

/// Internal OMS order record. Wraps the proto `Order` snapshot plus bookkeeping.
/// Mirrors Python `OMSOrder`.
#[derive(Debug, Clone)]
pub struct OmsOrder {
    /// True if this order was not submitted by this OMS (observed externally on the exchange).
    pub is_from_external: bool,
    pub order_id: i64,
    pub account_id: i64,
    /// Exchange-assigned order reference (set after LINKAGE report arrives).
    pub exch_order_ref: Option<String>,
    /// Original order request (None for external orders).
    pub oms_req: Option<OrderRequest>,
    /// Generated gateway request (None for external orders).
    pub gw_req: Option<ExchSendOrderRequest>,
    /// Generated cancel request (if a cancel has been submitted).
    pub cancel_req: Option<ExchCancelOrderRequest>,
    /// Authoritative order state, published to downstream on each update.
    pub order_state: Order,
    pub trades: Vec<Trade>,
    /// Accumulated filled qty from explicit trade reports.
    pub acc_trades_filled_qty: f64,
    /// Accumulated filled value from explicit trade reports.
    pub acc_trades_value: f64,
    /// Inferred trades synthesised from state-report deltas.
    pub order_inferred_trades: Vec<Trade>,
    pub exec_msgs: Vec<ExecMessage>,
    pub fees: Vec<Fee>,
    pub cancel_attempts: u32,
}

impl OmsOrder {
    pub fn is_in_terminal_state(&self) -> bool {
        use zk_proto_rs::zk::oms::v1::OrderStatus;
        matches!(
            OrderStatus::try_from(self.order_state.order_status),
            Ok(OrderStatus::Filled)
                | Ok(OrderStatus::Cancelled)
                | Ok(OrderStatus::Rejected)
        )
    }
}

/// Internal position / balance record.
/// Mirrors Python `OMSPosition`.
#[derive(Debug, Clone)]
pub struct OmsPosition {
    pub account_id: i64,
    pub symbol: String,
    /// Exchange-side symbol name (for matching inbound balance updates).
    pub symbol_exch: Option<String>,
    pub is_short: bool,
    /// Proto snapshot — used for publishing.
    pub position_state: Position,
}

impl OmsPosition {
    pub fn new(account_id: i64, symbol: impl Into<String>, instrument_type: i32, is_short: bool, is_from_exch: bool) -> Self {
        use zk_proto_rs::zk::common::v1::LongShortType;
        let symbol = symbol.into();
        let mut pos_state = Position::default();
        pos_state.account_id = account_id;
        pos_state.instrument_code = symbol.clone();
        pos_state.instrument_type = instrument_type;
        pos_state.long_short_type = if is_short {
            LongShortType::LsShort as i32
        } else {
            LongShortType::LsLong as i32
        };
        pos_state.avail_qty = 0.0;
        pos_state.frozen_qty = 0.0;
        pos_state.total_qty = 0.0;
        pos_state.is_from_exch = is_from_exch;

        Self {
            account_id,
            symbol,
            symbol_exch: None,
            is_short,
            position_state: pos_state,
        }
    }
}

/// Balance change request applied atomically by BalanceManager.
/// Mirrors Python `PositionChange`.
#[derive(Debug, Clone)]
pub struct PositionChange {
    pub account_id: i64,
    pub symbol: String,
    pub is_short: bool,
    pub avail_change: f64,
    pub frozen_change: f64,
    pub total_change: f64,
}

/// Pending order recheck (e.g. order still PENDING after gateway timeout).
#[derive(Debug, Clone)]
pub struct OrderRecheckRequest {
    pub order_id: i64,
    pub check_delay_secs: i64,
    pub timestamp: i64,
}

/// Pending cancel recheck.
#[derive(Debug, Clone)]
pub struct CancelRecheckRequest {
    pub orig_cancel_request: ExchCancelOrderRequest,
    pub check_delay_secs: i64,
    pub retry: bool,
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// OmsMessage — the single input type accepted by OmsCore::process_message
// ---------------------------------------------------------------------------

/// All message types the OMS can receive.
pub enum OmsMessage {
    PlaceOrder(OrderRequest),
    BatchPlaceOrders(Vec<OrderRequest>),
    CancelOrder(OrderCancelRequest),
    BatchCancelOrders(Vec<OrderCancelRequest>),
    GatewayOrderReport(OrderReport),
    BalanceUpdate(BalanceUpdate),
    RecheckOrder(OrderRecheckRequest),
    RecheckCancel(CancelRecheckRequest),
    Panic { account_id: i64 },
    DontPanic { account_id: i64 },
    /// Periodic cleanup tick; `ts_ms` is the current wall-clock time in milliseconds.
    Cleanup { ts_ms: i64 },
    ReloadConfig,
}

// ---------------------------------------------------------------------------
// OmsAction — the output produced by OmsCore::process_message
// ---------------------------------------------------------------------------

/// Actions the OMS service layer must execute after processing a message.
/// Cleaner than Python's `OMSAction(action_type, action_meta: dict, action_data: Any)`.
#[derive(Debug, Clone)]
pub enum OmsAction {
    /// Send a single order to the gateway.
    SendOrderToGw {
        gw_key: String,
        request: ExchSendOrderRequest,
        order_id: i64,          // for latency tracking
        order_created_at: i64,  // Order.created_at in ms (= OrderRequest.timestamp)
    },
    /// Send a batch of orders to the gateway (if the exchange supports it).
    BatchSendOrdersToGw {
        gw_key: String,
        request: ExchBatchSendOrdersRequest,
    },
    /// Send a single cancel to the gateway.
    SendCancelToGw {
        gw_key: String,
        request: ExchCancelOrderRequest,
    },
    /// Send a batch cancel to the gateway.
    BatchCancelToGw {
        gw_key: String,
        request: ExchBatchCancelOrdersRequest,
    },
    /// Publish an order state update (to NATS etc.).
    PublishOrderUpdate(Box<OrderUpdateEvent>),
    /// Publish a balance / position update.
    PublishBalanceUpdate(Box<PositionUpdateEvent>),
    /// Persist order state (to Redis etc.).
    PersistOrder {
        order: Box<OmsOrder>,
        set_expire: bool,
        set_closed: bool,
    },
}

// ---------------------------------------------------------------------------
// OrderContext — resolved once per order, cached for subsequent events
// ---------------------------------------------------------------------------

/// Per-order resolved context.
/// Mirrors Python `OrderContext`.
#[derive(Debug, Clone)]
pub struct OrderContext {
    pub account_id: i64,
    /// The fund (quote/settlement) symbol used for balance bookkeeping.
    pub fund_symbol: Option<String>,
    /// The position (base) symbol used for balance bookkeeping.
    pub pos_symbol: Option<String>,
    pub route: Option<OmsRouteEntry>,
    pub trading_config: Option<InstrumentTradingConfig>,
    pub symbol_ref: Option<InstrumentRefData>,
    pub gw_config: Option<GwConfigEntry>,
    /// Set after `OrderManager::create_order`.
    pub order: Option<OmsOrder>,
    pub errors: Vec<String>,
}

impl OrderContext {
    pub fn has_error(&self) -> bool {
        !self.errors.is_empty()
    }
}
