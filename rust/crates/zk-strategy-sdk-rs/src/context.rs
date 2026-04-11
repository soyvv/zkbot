use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use zk_proto_rs::zk::{
    common::v1::InstrumentRefData,
    oms::v1::{
        Balance, BalanceUpdateEvent, Order, OrderStatus, OrderUpdateEvent, Position,
        PositionUpdateEvent,
    },
};

use crate::models::{StrategyCancel, StrategyOrder};

pub trait StrategyIdAllocator: Send + Sync {
    fn next_id(&self) -> i64;
}

#[derive(Debug)]
pub struct SequentialIdAllocator {
    next: AtomicI64,
}

impl SequentialIdAllocator {
    pub fn new(start: i64) -> Self {
        Self {
            next: AtomicI64::new(start),
        }
    }
}

impl Default for SequentialIdAllocator {
    fn default() -> Self {
        Self::new(1)
    }
}

impl StrategyIdAllocator for SequentialIdAllocator {
    fn next_id(&self) -> i64 {
        self.next.fetch_add(1, Ordering::AcqRel)
    }
}

// ---------------------------------------------------------------------------
// AccountState — per-account order and balance tracking
// ---------------------------------------------------------------------------

pub struct AccountState {
    pub account_id: i64,
    /// Submitted by strategy, not yet confirmed by OMS (pending → open transition).
    pub pending_orders: HashMap<i64, StrategyOrder>,
    /// Booked or partially filled.
    pub open_orders: HashMap<i64, Order>,
    /// Terminal (filled / cancelled / rejected).
    pub terminal_orders: HashMap<i64, Order>,
    /// Order IDs for which a cancel has been submitted.
    pub pending_cancels: HashSet<i64>,
    /// Asset-level balances (cash / spot inventory), keyed by asset name.
    pub balances: HashMap<String, Balance>,
    /// Instrument-level positions (derivatives exposure), keyed by instrument code.
    pub positions: HashMap<String, Position>,
}

impl AccountState {
    pub fn new(account_id: i64) -> Self {
        Self {
            account_id,
            pending_orders: HashMap::new(),
            open_orders: HashMap::new(),
            terminal_orders: HashMap::new(),
            pending_cancels: HashSet::new(),
            balances: HashMap::new(),
            positions: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// OrderView — unified lookup result across order lifecycle stages
// ---------------------------------------------------------------------------

pub enum OrderView<'a> {
    Pending(&'a StrategyOrder),
    Open(&'a Order),
    Terminal(&'a Order),
}

// ---------------------------------------------------------------------------
// StrategyContext — full runtime state visible to a strategy during callbacks
// Mirrors Python StrategyStateProxy + read API of TokkaQuant.
// ---------------------------------------------------------------------------

pub struct StrategyContext {
    account_states: HashMap<i64, AccountState>,
    /// order_id → account_id reverse index.
    order_id_to_account: HashMap<i64, i64>,
    pub symbol_refs: HashMap<String, InstrumentRefData>,
    pub current_ts_ms: i64,
    /// Monotonic counter incremented every time balance state changes.
    /// Callers can cache this value and skip balance re-snapshotting when unchanged.
    pub balance_generation: u64,
    /// Monotonic counter incremented every time position state changes.
    pub position_generation: u64,
    /// Arbitrary init data injected by the runtime before `on_init` fires.
    /// Mirrors Python `tq.__tq_init_output__` / `tq.get_custom_init_data()`.
    init_data: Option<Box<dyn Any + Send + 'static>>,
    id_allocator: Arc<dyn StrategyIdAllocator>,
}

impl StrategyContext {
    pub fn new(account_ids: &[i64], refdata: &[InstrumentRefData]) -> Self {
        Self::new_with_id_allocator(account_ids, refdata, Arc::new(SequentialIdAllocator::default()))
    }

    pub fn new_with_id_allocator(
        account_ids: &[i64],
        refdata: &[InstrumentRefData],
        id_allocator: Arc<dyn StrategyIdAllocator>,
    ) -> Self {
        let account_states = account_ids
            .iter()
            .map(|&id| (id, AccountState::new(id)))
            .collect();
        let symbol_refs = refdata
            .iter()
            .map(|r| (r.instrument_id.clone(), r.clone()))
            .collect();
        Self {
            account_states,
            order_id_to_account: HashMap::new(),
            symbol_refs,
            current_ts_ms: 0,
            balance_generation: 0,
            position_generation: 0,
            init_data: None,
            id_allocator,
        }
    }

    // -----------------------------------------------------------------------
    // Runtime-driven mutations (called by StrategyRunner / Backtester)
    // -----------------------------------------------------------------------

    /// Record a newly submitted strategy order as pending.
    pub fn book_order(&mut self, order: &StrategyOrder) {
        let acc = self
            .account_states
            .entry(order.account_id)
            .or_insert_with(|| AccountState::new(order.account_id));
        acc.pending_orders.insert(order.order_id, order.clone());
        self.order_id_to_account
            .insert(order.order_id, order.account_id);
    }

    /// Record a cancel submission for an order.
    pub fn book_cancel(&mut self, cancel: &StrategyCancel) {
        if let Some(&acc_id) = self.order_id_to_account.get(&cancel.order_id) {
            if let Some(acc) = self.account_states.get_mut(&acc_id) {
                acc.pending_cancels.insert(cancel.order_id);
            }
        }
    }

    /// Update order tracking from an OMS order update event.
    pub fn on_order_update(&mut self, oue: &OrderUpdateEvent) {
        let order_id = oue.order_id;
        let snapshot = match &oue.order_snapshot {
            Some(s) => s.clone(),
            None => return,
        };

        if oue.timestamp > self.current_ts_ms {
            self.current_ts_ms = oue.timestamp;
        }

        let acc_id = match self.order_id_to_account.get(&order_id).copied() {
            Some(id) => id,
            None => {
                // Could be an external order — insert into the event's account
                let id = oue.account_id;
                self.order_id_to_account.insert(order_id, id);
                id
            }
        };

        let acc = match self.account_states.get_mut(&acc_id) {
            Some(a) => a,
            None => return,
        };

        if is_terminal(snapshot.order_status) {
            acc.terminal_orders.insert(order_id, snapshot);
            acc.open_orders.remove(&order_id);
            acc.pending_orders.remove(&order_id);
            acc.pending_cancels.remove(&order_id);
        } else {
            acc.open_orders.insert(order_id, snapshot);
            acc.pending_orders.remove(&order_id);
        }
    }

    /// Update balance state (cash/spot inventory). Only touches `acc.balances`.
    pub fn on_balance_update(&mut self, bue: &BalanceUpdateEvent) {
        if let Some(acc) = self.account_states.get_mut(&bue.account_id) {
            for bal in &bue.balance_snapshots {
                acc.balances.insert(bal.asset.clone(), bal.clone());
            }
        }
        self.balance_generation += 1;
    }

    /// Update position state (derivatives exposure). Only touches `acc.positions`.
    pub fn on_position_update(&mut self, pue: &PositionUpdateEvent) {
        for pos in &pue.position_snapshots {
            if let Some(acc) = self.account_states.get_mut(&pos.account_id) {
                acc.positions
                    .insert(pos.instrument_code.clone(), pos.clone());
            }
        }
        self.position_generation += 1;
    }

    // -----------------------------------------------------------------------
    // Strategy-callable queries (all take &self)
    // -----------------------------------------------------------------------

    pub fn get_order(&self, order_id: i64) -> Option<OrderView<'_>> {
        let &acc_id = self.order_id_to_account.get(&order_id)?;
        let acc = self.account_states.get(&acc_id)?;
        if let Some(o) = acc.open_orders.get(&order_id) {
            return Some(OrderView::Open(o));
        }
        if let Some(o) = acc.terminal_orders.get(&order_id) {
            return Some(OrderView::Terminal(o));
        }
        if let Some(o) = acc.pending_orders.get(&order_id) {
            return Some(OrderView::Pending(o));
        }
        None
    }

    pub fn get_open_orders(&self, account_id: i64) -> Vec<&Order> {
        self.account_states
            .get(&account_id)
            .map(|acc| acc.open_orders.values().collect())
            .unwrap_or_default()
    }

    pub fn get_pending_orders(&self, account_id: i64) -> Vec<&StrategyOrder> {
        self.account_states
            .get(&account_id)
            .map(|acc| acc.pending_orders.values().collect())
            .unwrap_or_default()
    }

    pub fn get_pending_cancels(&self, account_id: i64) -> Vec<i64> {
        self.account_states
            .get(&account_id)
            .map(|acc| acc.pending_cancels.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn get_position(&self, account_id: i64, symbol: &str) -> Option<&Position> {
        self.account_states
            .get(&account_id)
            .and_then(|acc| acc.positions.get(symbol))
    }

    /// Single balance lookup by asset name.
    pub fn get_balance(&self, account_id: i64, asset: &str) -> Option<&Balance> {
        self.account_states
            .get(&account_id)
            .and_then(|acc| acc.balances.get(asset))
    }

    /// Convenience: return `total_qty` from a balance entry for the given asset.
    pub fn get_spot_inventory(&self, account_id: i64, asset: &str) -> Option<f64> {
        self.get_balance(account_id, asset).map(|b| b.total_qty)
    }

    pub fn get_symbol_info(&self, symbol: &str) -> Option<&InstrumentRefData> {
        self.symbol_refs.get(symbol)
    }

    /// All balances held by `account_id` (asset → Balance snapshot).
    pub fn get_balances_map(&self, account_id: i64) -> Option<&HashMap<String, Balance>> {
        self.account_states
            .get(&account_id)
            .map(|acc| &acc.balances)
    }

    /// All positions held by `account_id` (instrument → Position snapshot).
    pub fn get_positions_map(&self, account_id: i64) -> Option<&HashMap<String, Position>> {
        self.account_states
            .get(&account_id)
            .map(|acc| &acc.positions)
    }

    /// All registered account IDs.
    pub fn account_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.account_states.keys().copied()
    }

    /// Allocate a runtime-scoped unique order/correlation id.
    ///
    /// The allocator is injected by the host runtime:
    /// - live engine: Snowflake / Pilot-granted worker id
    /// - backtester: deterministic local sequence
    pub fn next_id(&self) -> i64 {
        self.id_allocator.next_id()
    }

    // -----------------------------------------------------------------------
    // Init data — set by runtime before on_init, readable in any callback
    // -----------------------------------------------------------------------

    /// Store arbitrary init data fetched by the runtime (backtester / live engine).
    /// Mirrors Python `tq.__tq_init_output__ = data`.
    pub fn set_init_data(&mut self, data: Box<dyn Any + Send + 'static>) {
        self.init_data = Some(data);
    }

    /// Retrieve the init data cast to type `T`.
    /// Returns `None` if no data was set or the type does not match.
    /// Mirrors Python `tq.get_custom_init_data()`.
    pub fn get_init_data<T: Any + Send + 'static>(&self) -> Option<&T> {
        self.init_data.as_ref().and_then(|d| d.downcast_ref::<T>())
    }
}

fn is_terminal(status: i32) -> bool {
    matches!(
        OrderStatus::try_from(status),
        Ok(OrderStatus::Filled) | Ok(OrderStatus::Cancelled) | Ok(OrderStatus::Rejected)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> StrategyContext {
        StrategyContext::new(&[100], &[])
    }

    #[test]
    fn next_id_is_monotonic() {
        let ctx = make_ctx();
        let a = ctx.next_id();
        let b = ctx.next_id();
        assert!(b > a);
    }

    fn make_balance_update(account_id: i64, asset: &str, total_qty: f64) -> BalanceUpdateEvent {
        BalanceUpdateEvent {
            account_id,
            balance_snapshots: vec![Balance {
                account_id,
                asset: asset.to_string(),
                total_qty,
                frozen_qty: 0.0,
                avail_qty: total_qty,
                ..Default::default()
            }],
            timestamp: 1000,
        }
    }

    fn make_position_update(
        account_id: i64,
        instrument: &str,
        total_qty: f64,
    ) -> PositionUpdateEvent {
        PositionUpdateEvent {
            account_id,
            position_snapshots: vec![Position {
                account_id,
                instrument_code: instrument.to_string(),
                total_qty,
                ..Default::default()
            }],
            timestamp: 1000,
        }
    }

    #[test]
    fn balance_update_populates_balances_not_positions() {
        let mut ctx = make_ctx();
        ctx.on_balance_update(&make_balance_update(100, "USDT", 5000.0));

        assert!(ctx.get_balance(100, "USDT").is_some());
        assert_eq!(ctx.get_balance(100, "USDT").unwrap().total_qty, 5000.0);
        // Must NOT touch positions
        assert!(ctx.get_positions_map(100).unwrap().is_empty());
    }

    #[test]
    fn position_update_populates_positions_not_balances() {
        let mut ctx = make_ctx();
        ctx.on_position_update(&make_position_update(100, "BTC-USDT-PERP", 1.5));

        assert!(ctx.get_position(100, "BTC-USDT-PERP").is_some());
        assert_eq!(
            ctx.get_position(100, "BTC-USDT-PERP").unwrap().total_qty,
            1.5
        );
        // Must NOT touch balances
        assert!(ctx.get_balances_map(100).unwrap().is_empty());
    }

    #[test]
    fn get_balance_returns_correct_entry() {
        let mut ctx = make_ctx();
        ctx.on_balance_update(&make_balance_update(100, "BTC", 2.0));
        ctx.on_balance_update(&make_balance_update(100, "ETH", 10.0));

        assert_eq!(ctx.get_balance(100, "BTC").unwrap().total_qty, 2.0);
        assert_eq!(ctx.get_balance(100, "ETH").unwrap().total_qty, 10.0);
        assert!(ctx.get_balance(100, "SOL").is_none());
        assert!(ctx.get_balance(999, "BTC").is_none());
    }

    #[test]
    fn get_position_returns_correct_entry() {
        let mut ctx = make_ctx();
        ctx.on_position_update(&make_position_update(100, "BTC-USDT-PERP", 3.0));

        assert_eq!(
            ctx.get_position(100, "BTC-USDT-PERP").unwrap().total_qty,
            3.0
        );
        assert!(ctx.get_position(100, "ETH-USDT-PERP").is_none());
    }

    #[test]
    fn get_spot_inventory_returns_total_qty() {
        let mut ctx = make_ctx();
        ctx.on_balance_update(&make_balance_update(100, "USDT", 1500.0));

        assert_eq!(ctx.get_spot_inventory(100, "USDT"), Some(1500.0));
        assert_eq!(ctx.get_spot_inventory(100, "BTC"), None);
    }

    #[test]
    fn balance_generation_incremented_only_by_balance_updates() {
        let mut ctx = make_ctx();
        assert_eq!(ctx.balance_generation, 0);
        assert_eq!(ctx.position_generation, 0);

        ctx.on_balance_update(&make_balance_update(100, "USDT", 100.0));
        assert_eq!(ctx.balance_generation, 1);
        assert_eq!(ctx.position_generation, 0);

        ctx.on_balance_update(&make_balance_update(100, "BTC", 1.0));
        assert_eq!(ctx.balance_generation, 2);
        assert_eq!(ctx.position_generation, 0);
    }

    #[test]
    fn position_generation_incremented_only_by_position_updates() {
        let mut ctx = make_ctx();
        assert_eq!(ctx.balance_generation, 0);
        assert_eq!(ctx.position_generation, 0);

        ctx.on_position_update(&make_position_update(100, "BTC-USDT-PERP", 1.0));
        assert_eq!(ctx.position_generation, 1);
        assert_eq!(ctx.balance_generation, 0);

        ctx.on_position_update(&make_position_update(100, "ETH-USDT-PERP", 5.0));
        assert_eq!(ctx.position_generation, 2);
        assert_eq!(ctx.balance_generation, 0);
    }
}
