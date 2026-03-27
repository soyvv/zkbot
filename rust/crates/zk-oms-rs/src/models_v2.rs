use std::collections::HashMap;

use zk_proto_rs::zk::{
    gateway::v1::{
        CancelOrderRequest as ExchCancelOrderRequest, SendOrderRequest as ExchSendOrderRequest,
    },
    oms::v1::{ExecMessage, Fee, OrderRequest, Trade},
};

use crate::models::ReconcileStatus;

// ---------------------------------------------------------------------------
// DynStrId — handle into a per-store dynamic string side-table
// ---------------------------------------------------------------------------

/// Handle into a per-store dynamic string side-table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynStrId(pub u32);

// ---------------------------------------------------------------------------
// DynStringTable — owned by OrderStore
// ---------------------------------------------------------------------------

pub struct DynStringTable {
    ids_by_str: HashMap<Box<str>, DynStrId>,
    strs_by_id: Vec<Box<str>>,
}

impl DynStringTable {
    pub fn new() -> Self {
        Self {
            ids_by_str: HashMap::new(),
            strs_by_id: Vec::new(),
        }
    }

    pub fn intern(&mut self, s: &str) -> DynStrId {
        if let Some(&id) = self.ids_by_str.get(s) {
            return id;
        }
        let id = DynStrId(self.strs_by_id.len() as u32);
        let boxed: Box<str> = s.into();
        self.strs_by_id.push(boxed.clone());
        self.ids_by_str.insert(boxed, id);
        id
    }

    pub fn resolve(&self, id: DynStrId) -> &str {
        &self.strs_by_id[id.0 as usize]
    }

    pub fn try_resolve(&self, id: DynStrId) -> Option<&str> {
        self.strs_by_id.get(id.0 as usize).map(|s| &**s)
    }

    /// Look up a string to get its ID (if already interned).
    pub fn lookup(&self, s: &str) -> Option<DynStrId> {
        self.ids_by_str.get(s).copied()
    }
}

impl Default for DynStringTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// OrderAccountingFlags
// ---------------------------------------------------------------------------

/// Pre-computed bookkeeping flags. Set once during order creation from TradingMeta + InstrumentMeta.
/// Eliminates the need for context_cache.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrderAccountingFlags {
    pub bookkeeping_balance: bool,
    pub balance_check: bool,
    pub use_margin: bool,
    pub is_buy: bool,
    /// Asset ID for cash reservation (quote/settlement asset). None if no balance bookkeeping.
    pub fund_asset_id: Option<u32>,
    /// Instrument ID for position reservation (base instrument). None if no position bookkeeping.
    pub pos_instrument_id: Option<u32>,
}

// ---------------------------------------------------------------------------
// LiveOrder — compact hot-path order state
// ---------------------------------------------------------------------------

/// Compact mutable order state. No embedded requests, trades, fees, or cloned config.
#[derive(Debug, Clone)]
pub struct LiveOrder {
    pub order_id: i64,
    pub account_id: i64,
    pub instrument_id: u32,
    pub gw_id: u32,
    pub route_exch_account_sym: u32,
    pub source_sym: u32,

    pub order_status: i32,
    pub buy_sell_type: i32,
    pub open_close_type: i32,
    pub order_type: i32,
    pub tif_type: i32,

    pub price: f64,
    pub qty: f64,
    pub filled_qty: f64,
    pub filled_avg_price: f64,

    /// Accumulated filled qty from explicit trade reports (for inferred trade synthesis).
    pub acc_trades_filled_qty: f64,
    /// Accumulated filled value from explicit trade reports.
    pub acc_trades_value: f64,

    pub exch_order_ref_id: Option<DynStrId>,
    pub snapshot_version: u32,
    pub created_at: i64,
    pub updated_at: i64,

    pub is_external: bool,
    pub cancel_attempts: u32,

    pub accounting_flags: OrderAccountingFlags,

    /// Error message (set on rejection).
    pub error_msg: String,

    /// Frozen timestamp (ms) when the order first transitioned to terminal state.
    /// Set exactly once — never updated after first terminal transition.
    pub terminal_at: Option<i64>,
}

impl LiveOrder {
    pub fn is_in_terminal_state(&self) -> bool {
        use zk_proto_rs::zk::oms::v1::OrderStatus;
        matches!(
            OrderStatus::try_from(self.order_status),
            Ok(OrderStatus::Filled) | Ok(OrderStatus::Cancelled) | Ok(OrderStatus::Rejected)
        )
    }
}

// ---------------------------------------------------------------------------
// OrderDetailLog — cold side-structure for audit/history
// ---------------------------------------------------------------------------

/// Cold audit/history data. Stored alongside LiveOrder but not on the hot mutation path.
#[derive(Debug, Clone, Default)]
pub struct OrderDetailLog {
    pub order_id: i64,
    pub original_req: Option<Box<OrderRequest>>,
    pub last_gw_req: Option<Box<ExchSendOrderRequest>>,
    pub cancel_req: Option<Box<ExchCancelOrderRequest>>,
    pub trades: Vec<Trade>,
    pub inferred_trades: Vec<Trade>,
    pub exec_msgs: Vec<ExecMessage>,
    pub fees: Vec<Fee>,
}

// ---------------------------------------------------------------------------
// ResolvedOrderMeta — compact resolved context (replaces fat OrderContext)
// ---------------------------------------------------------------------------

/// Compact resolved context for an order. Replaces the cloned-config OrderContext.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedOrderMeta {
    pub account_id: i64,
    pub gw_id: u32,
    pub instrument_id: u32,
    pub instrument_exch_sym: u32,
    pub exch_account_sym: u32,
    pub source_sym: u32,
    pub fund_asset_id: Option<u32>,
    pub pos_instrument_id: Option<u32>,
    pub bookkeeping_balance: bool,
    pub balance_check: bool,
    pub use_margin: bool,
    pub max_order_size: Option<f64>,
    pub qty_precision: i32,
    pub price_precision: i32,
}

// ---------------------------------------------------------------------------
// ManagedPositionV2 — compact position (integer IDs)
// ---------------------------------------------------------------------------

/// OMS-owned position state using integer IDs.
#[derive(Debug, Clone)]
pub struct ManagedPositionV2 {
    pub account_id: i64,
    pub instrument_id: u32,
    pub instrument_type: i32,
    pub is_short: bool,
    pub qty_total: f64,
    pub qty_frozen: f64,
    pub qty_available: f64,
    pub last_local_update_ts: i64,
    pub last_exch_sync_ts: i64,
    pub reconcile_status: ReconcileStatus,
    pub last_exch_qty: f64,
    pub first_diverged_ts: i64,
    pub divergence_count: u32,
}

impl ManagedPositionV2 {
    pub fn new(account_id: i64, instrument_id: u32, instrument_type: i32, is_short: bool) -> Self {
        Self {
            account_id,
            instrument_id,
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
}

// ---------------------------------------------------------------------------
// ReservationRecordV2
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveKind {
    CashAsset,
    InventoryInstrument,
}

/// Per-order reservation using integer IDs (no string keys).
#[derive(Debug, Clone)]
pub struct ReservationRecordV2 {
    pub order_id: i64,
    pub account_id: i64,
    pub kind: ReserveKind,
    pub target_id: u32,
    pub reserved_qty: f64,
}

// ---------------------------------------------------------------------------
// PositionDeltaV2
// ---------------------------------------------------------------------------

/// Position delta using integer instrument IDs.
#[derive(Debug, Clone)]
pub struct PositionDeltaV2 {
    pub account_id: i64,
    pub instrument_id: u32,
    pub is_short: bool,
    pub avail_change: f64,
    pub frozen_change: f64,
    pub total_change: f64,
}

// ---------------------------------------------------------------------------
// OmsActionV2 — compact output actions
// ---------------------------------------------------------------------------

/// Compact output actions carrying IDs and scalars, not full proto payloads.
/// Service layer calls materialize_action() to reconstruct full protos when needed.
#[derive(Debug, Clone)]
pub enum OmsActionV2 {
    SendOrderToGw {
        gw_id: u32,
        order_id: i64,
        order_created_at: i64,
    },
    BatchSendOrdersToGw {
        gw_id: u32,
        order_ids: Vec<i64>,
    },
    SendCancelToGw {
        gw_id: u32,
        order_id: i64,
    },
    BatchCancelToGw {
        gw_id: u32,
        order_ids: Vec<i64>,
    },
    PublishOrderUpdate {
        order_id: i64,
        include_last_trade: bool,
        include_last_fee: bool,
        include_exec_message: bool,
        include_inferred_trade: bool,
    },
    PublishBalanceUpdate {
        account_id: i64,
    },
    PublishPositionUpdate {
        account_id: i64,
    },
    PersistOrder {
        order_id: i64,
        set_expire: bool,
        set_closed: bool,
    },
    PersistBalance {
        account_id: i64,
        asset_id: u32,
    },
    PersistPosition {
        account_id: i64,
        instrument_id: u32,
    },
}

// ---------------------------------------------------------------------------
// RetryState
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct RetryState {
    pub total_retries_orders: u32,
    pub total_retries_cancels: u32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_order_size_under_256_bytes() {
        let size = std::mem::size_of::<LiveOrder>();
        assert!(
            size <= 256,
            "LiveOrder is {size} bytes, expected <= 256 bytes"
        );
    }
}
