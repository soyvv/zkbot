use std::collections::{HashMap, HashSet, VecDeque};

use tracing::warn;
use zk_proto_rs::zk::{
    common::v1::Rejection,
    exch_gw::v1::{
        order_report_entry::Report, ExchangeOrderStatus, OrderReportEntry, OrderReportType,
        OrderStateReport,
    },
    gateway::v1::SendOrderRequest,
    oms::v1::{ExecMessage, ExecType, Fee, OrderRequest, OrderStatus, Trade},
};

use crate::{
    models_v2::{
        DynStrId, DynStringTable, LiveOrder, OrderAccountingFlags, OrderDetailLog,
        ResolvedOrderMeta,
    },
    utils::{gen_order_id, gen_timestamp_ms},
};

// ---------------------------------------------------------------------------
// OrderStoreV2
// ---------------------------------------------------------------------------

pub struct OrderStoreV2 {
    /// order_id -> hot mutable order state
    pub live: HashMap<i64, LiveOrder>,
    /// order_id -> cold audit/history data
    pub(crate) detail: HashMap<i64, OrderDetailLog>,
    /// IDs of orders not yet in terminal state
    pub(crate) open_order_ids: HashSet<i64>,
    /// Open order IDs indexed by account
    pub(crate) open_by_account: HashMap<i64, HashSet<i64>>,
    /// (gw_id, DynStrId) -> order_id for exchange order ref lookup
    pub(crate) order_id_by_exch_ref: HashMap<(u32, DynStrId), i64>,
    /// (gw_id, DynStrId) -> order_id for external (non-OMS) orders
    pub(crate) external_order_ids: HashMap<(u32, DynStrId), i64>,
    /// LRU eviction queue for closed orders
    closed_lru: VecDeque<i64>,
    max_cached_orders: usize,
    /// Dynamic string table for exchange order refs
    pub dyn_strings: DynStringTable,
    use_time_emulation: bool,
}

impl OrderStoreV2 {
    pub fn new(max_cached_orders: usize, use_time_emulation: bool) -> Self {
        crate::utils::init_id_gen();
        Self {
            live: HashMap::new(),
            detail: HashMap::new(),
            open_order_ids: HashSet::new(),
            open_by_account: HashMap::new(),
            order_id_by_exch_ref: HashMap::new(),
            external_order_ids: HashMap::new(),
            closed_lru: VecDeque::new(),
            max_cached_orders,
            dyn_strings: DynStringTable::new(),
            use_time_emulation,
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    /// Initialize with warm-start orders (from Redis persistence).
    /// Takes pre-built LiveOrder + OrderDetailLog pairs.
    pub fn init_with_orders(&mut self, orders: Vec<(LiveOrder, OrderDetailLog)>) {
        for (live, detail) in orders {
            let oid = live.order_id;
            if oid == 0 {
                warn!("invalid order missing order_id; skipping");
                continue;
            }
            if !live.is_in_terminal_state() {
                self.open_order_ids.insert(oid);
                self.open_by_account
                    .entry(live.account_id)
                    .or_default()
                    .insert(oid);
            }
            if let Some(ref_id) = live.exch_order_ref_id {
                self.order_id_by_exch_ref.insert((live.gw_id, ref_id), oid);
            }
            self.live.insert(oid, live);
            self.detail.insert(oid, detail);
        }
    }

    // ------------------------------------------------------------------
    // Order creation
    // ------------------------------------------------------------------

    /// Create a new order from a resolved meta. Returns the order_id.
    pub fn create_order(
        &mut self,
        order_id: i64,
        req: &OrderRequest,
        meta: &ResolvedOrderMeta,
        flags: OrderAccountingFlags,
        gw_req: Option<Box<SendOrderRequest>>,
    ) -> i64 {
        let ts = if self.use_time_emulation {
            req.timestamp
        } else {
            gen_timestamp_ms()
        };

        let live = LiveOrder {
            order_id,
            account_id: req.account_id,
            instrument_id: meta.instrument_id,
            gw_id: meta.gw_id,
            route_exch_account_sym: meta.exch_account_sym,
            source_sym: meta.source_sym,
            order_status: OrderStatus::Pending as i32,
            buy_sell_type: req.buy_sell_type,
            open_close_type: req.open_close_type,
            order_type: req.order_type,
            tif_type: req.time_inforce_type,
            price: req.price,
            qty: req.qty,
            filled_qty: 0.0,
            filled_avg_price: 0.0,
            acc_trades_filled_qty: 0.0,
            acc_trades_value: 0.0,
            exch_order_ref_id: None,
            snapshot_version: 1,
            created_at: ts,
            updated_at: ts,
            is_external: false,
            cancel_attempts: 0,
            accounting_flags: flags,
            error_msg: String::new(),
            terminal_at: None,
        };

        let detail = OrderDetailLog {
            order_id,
            original_req: Some(Box::new(req.clone())),
            last_gw_req: gw_req,
            cancel_req: None,
            trades: Vec::new(),
            inferred_trades: Vec::new(),
            exec_msgs: Vec::new(),
            fees: Vec::new(),
        };

        self.live.insert(order_id, live);
        self.detail.insert(order_id, detail);
        self.open_order_ids.insert(order_id);
        self.open_by_account
            .entry(req.account_id)
            .or_default()
            .insert(order_id);

        order_id
    }

    // ------------------------------------------------------------------
    // Lookups
    // ------------------------------------------------------------------

    pub fn get_live(&self, order_id: i64) -> Option<&LiveOrder> {
        self.live.get(&order_id)
    }

    pub fn get_live_mut(&mut self, order_id: i64) -> Option<&mut LiveOrder> {
        self.live.get_mut(&order_id)
    }

    pub fn get_detail(&self, order_id: i64) -> Option<&OrderDetailLog> {
        self.detail.get(&order_id)
    }

    pub fn get_detail_mut(&mut self, order_id: i64) -> Option<&mut OrderDetailLog> {
        self.detail.get_mut(&order_id)
    }

    /// Get mutable references to both live and detail for the same order.
    /// Avoids double-mutable-borrow of `self` when both are needed.
    pub fn get_live_and_detail_mut(
        &mut self,
        order_id: i64,
    ) -> Option<(&mut LiveOrder, &mut OrderDetailLog)> {
        // Both HashMaps are separate fields, so we can borrow them independently.
        let live = self.live.get_mut(&order_id)?;
        let detail = self.detail.get_mut(&order_id)?;
        Some((live, detail))
    }

    /// Look up order by exchange ref. Falls back to exch_ref if order_id is 0.
    pub fn resolve_order_id(&self, order_id: i64, gw_id: u32, exch_ref: &str) -> Option<i64> {
        if order_id != 0 && self.live.contains_key(&order_id) {
            return Some(order_id);
        }
        let dyn_id = self.dyn_strings.lookup(exch_ref)?;
        let resolved = self.order_id_by_exch_ref.get(&(gw_id, dyn_id)).copied()?;
        Some(resolved)
    }

    pub fn is_external(&self, gw_id: u32, exch_ref_id: DynStrId) -> bool {
        self.external_order_ids.contains_key(&(gw_id, exch_ref_id))
    }

    pub fn get_open_orders_for_account(&self, account_id: i64) -> Vec<i64> {
        self.open_by_account
            .get(&account_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Linkage
    // ------------------------------------------------------------------

    /// Register exchange order ref linkage. Interns the string in dyn_strings.
    pub fn register_exch_ref(&mut self, order_id: i64, gw_id: u32, exch_ref: &str) {
        let dyn_id = self.dyn_strings.intern(exch_ref);
        self.order_id_by_exch_ref.insert((gw_id, dyn_id), order_id);
        if let Some(live) = self.live.get_mut(&order_id) {
            live.exch_order_ref_id = Some(dyn_id);
        }
    }

    // ------------------------------------------------------------------
    // State management
    // ------------------------------------------------------------------

    /// Mark order as closed (terminal). Move to LRU. Evict oldest if over capacity.
    pub fn mark_closed(&mut self, order_id: i64) {
        self.open_order_ids.remove(&order_id);
        if let Some(live) = self.live.get(&order_id) {
            if let Some(set) = self.open_by_account.get_mut(&live.account_id) {
                set.remove(&order_id);
            }
        }
        self.closed_lru.push_back(order_id);
        if self.closed_lru.len() > self.max_cached_orders {
            if let Some(oldest) = self.closed_lru.pop_front() {
                self.cleanup_order(oldest);
            }
        }
    }

    fn cleanup_order(&mut self, order_id: i64) {
        if let Some(live) = self.live.remove(&order_id) {
            if let Some(ref_id) = live.exch_order_ref_id {
                self.order_id_by_exch_ref.remove(&(live.gw_id, ref_id));
            }
        }
        self.detail.remove(&order_id);
    }

    // ------------------------------------------------------------------
    // External order handling
    // ------------------------------------------------------------------

    /// Create a dummy external order from a gateway report.
    pub fn create_external_order(
        &mut self,
        gw_id: u32,
        exch_ref: &str,
        account_id: i64,
        ts: i64,
        source_sym: u32,
    ) -> i64 {
        let order_id = gen_order_id();
        let dyn_id = self.dyn_strings.intern(exch_ref);

        let live = LiveOrder {
            order_id,
            account_id,
            instrument_id: 0,
            gw_id,
            route_exch_account_sym: 0,
            source_sym,
            order_status: OrderStatus::Pending as i32,
            buy_sell_type: 0,
            open_close_type: 0,
            order_type: 0,
            tif_type: 0,
            price: 0.0,
            qty: 0.0,
            filled_qty: 0.0,
            filled_avg_price: 0.0,
            acc_trades_filled_qty: 0.0,
            acc_trades_value: 0.0,
            exch_order_ref_id: Some(dyn_id),
            snapshot_version: 1,
            created_at: ts,
            updated_at: ts,
            is_external: true,
            cancel_attempts: 0,
            accounting_flags: OrderAccountingFlags::default(),
            error_msg: String::new(),
            terminal_at: None,
        };

        let detail = OrderDetailLog {
            order_id,
            ..Default::default()
        };

        self.external_order_ids.insert((gw_id, dyn_id), order_id);
        self.order_id_by_exch_ref.insert((gw_id, dyn_id), order_id);
        self.live.insert(order_id, live);
        self.detail.insert(order_id, detail);

        order_id
    }

    // ------------------------------------------------------------------
    // Report processing helpers
    // ------------------------------------------------------------------

    /// Process a linkage-only report: promote PENDING -> BOOKED.
    pub fn apply_linkage_promotion(&mut self, order_id: i64) {
        if let Some(live) = self.live.get_mut(&order_id) {
            let status =
                OrderStatus::try_from(live.order_status).unwrap_or(OrderStatus::Unspecified);
            if matches!(status, OrderStatus::Pending | OrderStatus::Rejected) {
                live.order_status = OrderStatus::Booked as i32;
            }
        }
    }

    /// Apply a state report to a live order. Returns whether state was updated.
    pub fn apply_state_report(
        live: &mut LiveOrder,
        detail: &mut OrderDetailLog,
        filled_qty_from_report: Option<f64>,
        avg_price: f64,
        exch_status: ExchangeOrderStatus,
        ts: i64,
    ) -> bool {
        let orig_filled = live.filled_qty;
        let orig_avg = live.filled_avg_price;

        let new_filled = if let Some(fq) = filled_qty_from_report {
            if fq != 0.0 {
                Some(fq)
            } else {
                None
            }
        } else {
            None
        };

        if let Some(nf) = new_filled {
            live.filled_qty = f64::max(nf, orig_filled);
        }
        if avg_price != 0.0 {
            live.filled_avg_price = avg_price;
        }

        // Synthesise inferred trade for fill increment
        let fill_increment = live.filled_qty - orig_filled;
        if fill_increment > 1e-8 {
            let pseudo_id = format!("{}_{}", live.order_id, detail.inferred_trades.len());
            let inferred_price = infer_trade_price(
                orig_filled,
                orig_avg,
                live.filled_qty,
                live.filled_avg_price,
            );
            let inferred_trade = Trade {
                order_id: live.order_id,
                ext_trade_id: pseudo_id,
                filled_ts: ts,
                filled_qty: fill_increment,
                filled_price: inferred_price.unwrap_or(0.0),
                ..Default::default()
            };
            detail.inferred_trades.push(inferred_trade);
        }

        let new_status = map_order_status(exch_status);
        if can_update_status(
            OrderStatus::try_from(live.order_status).unwrap_or(OrderStatus::Unspecified),
            new_status,
        ) {
            live.order_status = new_status as i32;
        }

        true
    }

    /// Apply a trade report. Returns whether a new trade was added.
    pub fn apply_trade_report(
        live: &mut LiveOrder,
        detail: &mut OrderDetailLog,
        trade: Trade,
    ) -> bool {
        live.acc_trades_filled_qty += trade.filled_qty;
        live.acc_trades_value += trade.filled_qty * trade.filled_price;
        detail.trades.push(trade);
        true
    }

    /// Apply an exec error report.
    pub fn apply_exec_report(
        live: &mut LiveOrder,
        detail: &mut OrderDetailLog,
        exec_msg: ExecMessage,
    ) -> bool {
        let is_placing = exec_msg.exec_type == ExecType::PlacingOrder as i32;
        detail.exec_msgs.push(exec_msg);

        if is_placing {
            live.order_status = OrderStatus::Rejected as i32;
        }

        true
    }

    /// Apply a fee report.
    pub fn apply_fee_report(detail: &mut OrderDetailLog, fee: Fee) -> bool {
        detail.fees.push(fee);
        true
    }

    /// Infers a synthetic state entry from trade entries when no explicit state is present.
    pub fn infer_state_from_trades(
        entries: &[OrderReportEntry],
        live: &LiveOrder,
    ) -> Option<OrderReportEntry> {
        if live.is_in_terminal_state() {
            return None;
        }

        let has_state = entries
            .iter()
            .any(|e| matches!(&e.report, Some(Report::OrderStateReport(_))));
        if has_state {
            return None;
        }

        let mut new_qty = 0f64;
        let mut new_value = 0f64;
        let mut has_trade = false;
        for e in entries {
            if let Some(Report::TradeReport(t)) = &e.report {
                new_qty += t.filled_qty;
                new_value += t.filled_qty * t.filled_price;
                has_trade = true;
            }
        }
        if !has_trade {
            return None;
        }

        let total_qty = new_qty + live.acc_trades_filled_qty;
        if total_qty > live.qty {
            warn!(total_qty, order_qty = live.qty, "trades exceed order qty");
            return None;
        }
        let avg_price = if total_qty != 0.0 {
            (live.acc_trades_value + new_value) / total_qty
        } else {
            0.0
        };

        let exch_status = if (total_qty - live.qty).abs() < 1e-8 {
            ExchangeOrderStatus::ExchOrderStatusFilled
        } else {
            ExchangeOrderStatus::ExchOrderStatusPartialFilled
        };

        Some(OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(Report::OrderStateReport(OrderStateReport {
                exch_order_status: exch_status as i32,
                filled_qty: total_qty,
                unfilled_qty: live.qty - total_qty,
                avg_price,
                ..Default::default()
            })),
        })
    }

    // ------------------------------------------------------------------
    // Error helpers
    // ------------------------------------------------------------------

    /// Apply an OMS-level error to an order (rejection).
    pub fn apply_oms_error(
        live: &mut LiveOrder,
        detail: &mut OrderDetailLog,
        error_msg: &str,
        ts: i64,
        exec_type: i32,
        rejection: Rejection,
    ) {
        let mut exec_msg = ExecMessage::default();
        exec_msg.order_id = live.order_id;
        exec_msg.exec_type = exec_type;
        exec_msg.exec_success = false;
        exec_msg.timestamp = ts;
        exec_msg.error_msg = error_msg.to_string();
        exec_msg.rejection_info = Some(rejection);
        detail.exec_msgs.push(exec_msg);

        if exec_type == ExecType::PlacingOrder as i32 {
            live.snapshot_version += 1;
            live.order_status = OrderStatus::Rejected as i32;
            live.updated_at = ts;
            live.error_msg = error_msg.to_string();
            // Freeze terminal timestamp on rejection
            if live.terminal_at.is_none() {
                live.terminal_at = Some(ts);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DynStringTable lookup helper
// ---------------------------------------------------------------------------

/// Non-interning lookup on DynStringTable using the public API.
// ---------------------------------------------------------------------------
// Static helper functions (ported from v1)
// ---------------------------------------------------------------------------

fn map_order_status(gw_status: ExchangeOrderStatus) -> OrderStatus {
    match gw_status {
        ExchangeOrderStatus::ExchOrderStatusBooked => OrderStatus::Booked,
        ExchangeOrderStatus::ExchOrderStatusPartialFilled => OrderStatus::PartiallyFilled,
        ExchangeOrderStatus::ExchOrderStatusFilled => OrderStatus::Filled,
        ExchangeOrderStatus::ExchOrderStatusCancelled => OrderStatus::Cancelled,
        ExchangeOrderStatus::ExchOrderStatusExchRejected => OrderStatus::Rejected,
        ExchangeOrderStatus::ExchOrderStatusExpired => OrderStatus::Cancelled,
        _ => OrderStatus::Unspecified,
    }
}

fn can_update_status(old: OrderStatus, new: OrderStatus) -> bool {
    !matches!(old, OrderStatus::Cancelled | OrderStatus::Filled) && new != OrderStatus::Unspecified
}

fn infer_trade_price(orig_qty: f64, orig_avg: f64, new_qty: f64, new_avg: f64) -> Option<f64> {
    if new_qty == 0.0 {
        return None;
    }
    if orig_avg == 0.0 {
        return Some(new_avg);
    }
    if new_avg == 0.0 {
        return None;
    }
    let increment = new_qty - orig_qty;
    if increment == 0.0 {
        return None;
    }
    let orig_value = orig_avg * orig_qty;
    let new_value = new_avg * new_qty;
    Some((new_value - orig_value) / increment)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_v2::{OrderAccountingFlags, ResolvedOrderMeta};
    use zk_proto_rs::zk::{
        exch_gw::v1::ExchangeOrderStatus,
        oms::v1::{OrderRequest, OrderStatus},
    };

    fn test_meta() -> ResolvedOrderMeta {
        ResolvedOrderMeta {
            account_id: 100,
            gw_id: 1,
            instrument_id: 10,
            instrument_exch_sym: 20,
            exch_account_sym: 30,
            source_sym: 0,
            fund_asset_id: None,
            pos_instrument_id: None,
            bookkeeping_balance: false,
            balance_check: false,
            use_margin: false,
            max_order_size: None,
            qty_precision: 2,
            price_precision: 2,
        }
    }

    fn test_req(order_id: i64) -> OrderRequest {
        let mut req = OrderRequest::default();
        req.order_id = order_id;
        req.account_id = 100;
        req.price = 50_000.0;
        req.qty = 1.0;
        req.timestamp = 1000;
        req
    }

    #[test]
    fn create_order_and_get_live_roundtrip() {
        let mut store = OrderStoreV2::new(100, true);
        let req = test_req(42);
        let meta = test_meta();
        let flags = OrderAccountingFlags::default();

        let oid = store.create_order(42, &req, &meta, flags, None);
        assert_eq!(oid, 42);

        let live = store.get_live(oid).unwrap();
        assert_eq!(live.order_id, 42);
        assert_eq!(live.account_id, 100);
        assert_eq!(live.price, 50_000.0);
        assert_eq!(live.qty, 1.0);
        assert_eq!(live.order_status, OrderStatus::Pending as i32);
        assert_eq!(live.snapshot_version, 1);
        assert!(!live.is_external);

        let detail = store.get_detail(oid).unwrap();
        assert!(detail.original_req.is_some());
        assert!(detail.trades.is_empty());

        // Should be in open set
        assert!(store.open_order_ids.contains(&42));
        let open = store.get_open_orders_for_account(100);
        assert_eq!(open, vec![42]);
    }

    #[test]
    fn register_exch_ref_and_resolve() {
        let mut store = OrderStoreV2::new(100, true);
        let req = test_req(42);
        let meta = test_meta();
        store.create_order(42, &req, &meta, OrderAccountingFlags::default(), None);

        store.register_exch_ref(42, 1, "EXCH-REF-123");

        // Resolve by order_id
        let resolved = store.resolve_order_id(42, 1, "");
        assert_eq!(resolved, Some(42));

        // Resolve by exch_ref (order_id = 0)
        let resolved = store.resolve_order_id(0, 1, "EXCH-REF-123");
        assert_eq!(resolved, Some(42));

        // Unknown exch_ref
        let resolved = store.resolve_order_id(0, 1, "UNKNOWN");
        assert!(resolved.is_none());

        // Verify live order has ref set
        let live = store.get_live(42).unwrap();
        assert!(live.exch_order_ref_id.is_some());
    }

    #[test]
    fn mark_closed_and_lru_eviction() {
        let mut store = OrderStoreV2::new(2, true);
        let meta = test_meta();
        let flags = OrderAccountingFlags::default();

        // Create 4 orders
        for i in 1..=4 {
            let req = test_req(i);
            store.create_order(i, &req, &meta, flags, None);
        }

        // Close orders 1, 2, 3
        store.mark_closed(1);
        assert!(!store.open_order_ids.contains(&1));
        assert!(store.live.contains_key(&1)); // still cached

        store.mark_closed(2);
        assert!(store.live.contains_key(&1));
        assert!(store.live.contains_key(&2));

        // Closing 3 should evict 1 (max_cached = 2)
        store.mark_closed(3);
        assert!(!store.live.contains_key(&1), "order 1 should be evicted");
        assert!(!store.detail.contains_key(&1));
        assert!(store.live.contains_key(&2));
        assert!(store.live.contains_key(&3));

        // Order 4 still open
        assert!(store.open_order_ids.contains(&4));
        assert!(store.live.contains_key(&4));
    }

    #[test]
    fn map_order_status_correctness() {
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusBooked),
            OrderStatus::Booked
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusPartialFilled),
            OrderStatus::PartiallyFilled
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusFilled),
            OrderStatus::Filled
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusCancelled),
            OrderStatus::Cancelled
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusExchRejected),
            OrderStatus::Rejected
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusExpired),
            OrderStatus::Cancelled
        );
        assert_eq!(
            map_order_status(ExchangeOrderStatus::ExchOrderStatusUnspecified),
            OrderStatus::Unspecified
        );
    }

    #[test]
    fn can_update_status_rules() {
        // Terminal states block updates
        assert!(!can_update_status(
            OrderStatus::Filled,
            OrderStatus::Cancelled
        ));
        assert!(!can_update_status(
            OrderStatus::Cancelled,
            OrderStatus::Filled
        ));

        // Unspecified target blocked
        assert!(!can_update_status(
            OrderStatus::Pending,
            OrderStatus::Unspecified
        ));

        // Normal transitions
        assert!(can_update_status(OrderStatus::Pending, OrderStatus::Booked));
        assert!(can_update_status(
            OrderStatus::Booked,
            OrderStatus::PartiallyFilled
        ));
        assert!(can_update_status(
            OrderStatus::PartiallyFilled,
            OrderStatus::Filled
        ));
        assert!(can_update_status(
            OrderStatus::Booked,
            OrderStatus::Cancelled
        ));
    }

    #[test]
    fn infer_trade_price_basic() {
        // First fill: orig_avg = 0 -> returns new_avg
        assert_eq!(infer_trade_price(0.0, 0.0, 1.0, 100.0), Some(100.0));

        // Incremental fill
        let price = infer_trade_price(1.0, 100.0, 2.0, 110.0).unwrap();
        // new_value = 110*2 = 220, orig_value = 100*1 = 100, increment = 1
        // inferred = (220 - 100) / 1 = 120
        assert!((price - 120.0).abs() < 1e-10);

        // No increment
        assert_eq!(infer_trade_price(1.0, 100.0, 1.0, 100.0), None);

        // Zero new_qty
        assert_eq!(infer_trade_price(1.0, 100.0, 0.0, 0.0), None);

        // Zero new_avg
        assert_eq!(infer_trade_price(1.0, 100.0, 2.0, 0.0), None);
    }

    #[test]
    fn external_order_creation() {
        let mut store = OrderStoreV2::new(100, true);
        let oid = store.create_external_order(1, "EXT-123", 100, 5000, 99);

        assert!(store.live.contains_key(&oid));
        let live = store.get_live(oid).unwrap();
        assert!(live.is_external);
        assert_eq!(live.account_id, 100);
        assert_eq!(live.gw_id, 1);
        assert_eq!(live.created_at, 5000);

        // Resolvable by exch_ref
        let resolved = store.resolve_order_id(0, 1, "EXT-123");
        assert_eq!(resolved, Some(oid));

        // Marked as external
        let dyn_id = store.dyn_strings.lookup("EXT-123").unwrap();
        assert!(store.is_external(1, dyn_id));
    }

    #[test]
    fn linkage_promotion() {
        let mut store = OrderStoreV2::new(100, true);
        let req = test_req(42);
        let meta = test_meta();
        store.create_order(42, &req, &meta, OrderAccountingFlags::default(), None);

        assert_eq!(
            store.get_live(42).unwrap().order_status,
            OrderStatus::Pending as i32
        );

        store.apply_linkage_promotion(42);

        assert_eq!(
            store.get_live(42).unwrap().order_status,
            OrderStatus::Booked as i32
        );

        // Calling again on Booked should be a no-op (not Pending/Rejected)
        store.apply_linkage_promotion(42);
        assert_eq!(
            store.get_live(42).unwrap().order_status,
            OrderStatus::Booked as i32
        );
    }

    #[test]
    fn apply_state_report_fill() {
        let mut store = OrderStoreV2::new(100, true);
        let req = test_req(42);
        let meta = test_meta();
        store.create_order(42, &req, &meta, OrderAccountingFlags::default(), None);

        let (live, detail) = store.get_live_and_detail_mut(42).unwrap();

        let updated = OrderStoreV2::apply_state_report(
            live,
            detail,
            Some(0.5),
            50_000.0,
            ExchangeOrderStatus::ExchOrderStatusPartialFilled,
            2000,
        );

        assert!(updated);
        assert_eq!(live.filled_qty, 0.5);
        assert_eq!(live.filled_avg_price, 50_000.0);
        assert_eq!(live.order_status, OrderStatus::PartiallyFilled as i32);
        assert_eq!(detail.inferred_trades.len(), 1);
        assert_eq!(detail.inferred_trades[0].filled_qty, 0.5);
    }

    #[test]
    fn init_with_orders_restores_state() {
        let mut store = OrderStoreV2::new(100, true);

        let live = LiveOrder {
            order_id: 99,
            account_id: 100,
            instrument_id: 10,
            gw_id: 1,
            route_exch_account_sym: 0,
            source_sym: 0,
            order_status: OrderStatus::Booked as i32,
            buy_sell_type: 0,
            open_close_type: 0,
            order_type: 0,
            tif_type: 0,
            price: 100.0,
            qty: 1.0,
            filled_qty: 0.0,
            filled_avg_price: 0.0,
            acc_trades_filled_qty: 0.0,
            acc_trades_value: 0.0,
            exch_order_ref_id: None,
            snapshot_version: 2,
            created_at: 1000,
            updated_at: 2000,
            is_external: false,
            cancel_attempts: 0,
            accounting_flags: OrderAccountingFlags::default(),
            error_msg: String::new(),
            terminal_at: None,
        };
        let detail = OrderDetailLog {
            order_id: 99,
            ..Default::default()
        };

        store.init_with_orders(vec![(live, detail)]);

        assert!(store.open_order_ids.contains(&99));
        assert!(store.get_open_orders_for_account(100).contains(&99));
        assert!(store.get_live(99).is_some());
    }

    #[test]
    fn dyn_string_table_lookup() {
        let mut table = DynStringTable::new();

        assert!(table.lookup("foo").is_none());

        let id = table.intern("foo");
        assert_eq!(table.lookup("foo"), Some(id));
        assert!(table.lookup("bar").is_none());
    }
}
