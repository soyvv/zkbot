use crate::config::InstrumentTradingConfig;
use zk_proto_rs::zk::{
    common::v1::InstrumentRefData,
    exch_gw::v1::{BalanceUpdate, OrderReport},
    gateway::v1::{
        BatchCancelOrdersRequest as ExchBatchCancelOrdersRequest,
        BatchSendOrdersRequest as ExchBatchSendOrdersRequest,
        CancelOrderRequest as ExchCancelOrderRequest, SendOrderRequest as ExchSendOrderRequest,
    },
    ods::v1::{GwConfigEntry, OmsRouteEntry},
    oms::v1::{
        Balance, BalanceUpdateEvent, ExecMessage, Fee, Order, OrderCancelRequest, OrderRequest,
        OrderUpdateEvent, Position, PositionUpdateEvent, Trade,
    },
};

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
            Ok(OrderStatus::Filled) | Ok(OrderStatus::Cancelled) | Ok(OrderStatus::Rejected)
        )
    }
}

// ---------------------------------------------------------------------------
// Domain types — Position / Balance / Reservation separation
// ---------------------------------------------------------------------------

/// OMS-owned executable position state for an instrument.
/// Position qty is owned by the OMS and reconciled against exchange.
#[derive(Debug, Clone)]
pub struct OmsManagedPosition {
    pub account_id: i64,
    pub instrument_code: String,
    pub symbol_exch: Option<String>,
    pub instrument_type: i32,
    pub is_short: bool,
    pub qty_total: f64,
    pub qty_frozen: f64,
    pub qty_available: f64,
    pub last_local_update_ts: i64,
    pub last_exch_sync_ts: i64,
    // Reconcile tracking
    pub reconcile_status: ReconcileStatus,
    pub last_exch_qty: f64,
    pub first_diverged_ts: i64,
    pub divergence_count: u32,
}

impl OmsManagedPosition {
    pub fn new(
        account_id: i64,
        instrument_code: impl Into<String>,
        instrument_type: i32,
        is_short: bool,
    ) -> Self {
        Self {
            account_id,
            instrument_code: instrument_code.into(),
            symbol_exch: None,
            instrument_type,
            is_short,
            qty_total: 0.0,
            qty_frozen: 0.0,
            qty_available: 0.0,
            last_local_update_ts: 0,
            last_exch_sync_ts: 0,
            reconcile_status: ReconcileStatus::Unknown,
            last_exch_qty: 0.0,
            first_diverged_ts: 0,
            divergence_count: 0,
        }
    }

    /// Build a proto `Position` from managed state.
    pub fn to_proto(&self) -> Position {
        use zk_proto_rs::zk::common::v1::LongShortType;
        Position {
            account_id: self.account_id,
            instrument_code: self.instrument_code.clone(),
            instrument_type: self.instrument_type,
            long_short_type: if self.is_short {
                LongShortType::LsShort as i32
            } else {
                LongShortType::LsLong as i32
            },
            total_qty: self.qty_total,
            frozen_qty: self.qty_frozen,
            avail_qty: self.qty_available,
            update_timestamp: self.last_local_update_ts,
            sync_timestamp: self.last_exch_sync_ts,
            ..Default::default()
        }
    }
}

/// Exchange-reported position snapshot (read-only cache).
#[derive(Debug, Clone)]
pub struct ExchPositionSnapshot {
    pub account_id: i64,
    pub instrument_code: String,
    pub symbol_exch: Option<String>,
    pub position_state: Position,
    pub exch_data_raw: String,
    pub sync_ts: i64,
}

/// Exchange-reported balance snapshot (exchange-owned, canonical).
#[derive(Debug, Clone)]
pub struct ExchBalanceSnapshot {
    pub account_id: i64,
    pub asset: String,
    pub symbol_exch: Option<String>,
    pub balance_state: Balance,
    pub exch_data_raw: String,
    pub sync_ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReconcileStatus {
    #[default]
    Unknown,
    InSync,
    DivergedTransient,
    DivergedPersistent,
    ExchangeOnly,
    OmsOnly,
}

/// Per-order hold against cash or inventory.
#[derive(Debug, Clone)]
pub struct Reservation {
    pub order_id: i64,
    pub account_id: i64,
    pub symbol: String,
    pub reserved_qty: f64,
    /// false = cash reservation (buy), true = inventory reservation (sell).
    pub is_position: bool,
}

/// Position quantity delta applied by PositionManager.
#[derive(Debug, Clone)]
pub struct PositionDelta {
    pub account_id: i64,
    pub instrument_code: String,
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
    Panic {
        account_id: i64,
    },
    DontPanic {
        account_id: i64,
    },
    /// Periodic cleanup tick; `ts_ms` is the current wall-clock time in milliseconds.
    Cleanup {
        ts_ms: i64,
    },
    ReloadConfig,
    /// Periodic position recheck — triggers reconciliation of managed positions
    /// against exchange-reported state.
    PositionRecheck,
    /// Gateway worker failed to send order to exchange (gRPC error).
    /// Writer handles this as a synthetic rejection.
    GatewaySendFailed {
        order_id: i64,
        gw_id: u32,
        error_msg: String,
    },
    /// Gateway worker failed to send cancel to exchange (gRPC error).
    /// Writer handles this as a synthetic cancel-reject.
    GatewayCancelSendFailed {
        order_id: i64,
        gw_id: u32,
        error_msg: String,
    },
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
        order_id: i64,         // for latency tracking
        order_created_at: i64, // Order.created_at in ms (= OrderRequest.timestamp)
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
    /// Publish a balance update (exchange-owned asset inventory).
    PublishBalanceUpdate(Box<BalanceUpdateEvent>),
    /// Publish a position update (OMS-managed instrument exposure).
    PublishPositionUpdate(Box<PositionUpdateEvent>),
    /// Persist order state (to Redis etc.).
    PersistOrder {
        order: Box<OmsOrder>,
        set_expire: bool,
        set_closed: bool,
    },
    /// Persist balance snapshot (to Redis etc.).
    PersistBalance {
        account_id: i64,
        asset: String,
        snapshot: ExchBalanceSnapshot,
    },
    /// Persist position snapshot (to Redis etc.).
    PersistPosition {
        account_id: i64,
        instrument_code: String,
        side: String,
        position: OmsManagedPosition,
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
