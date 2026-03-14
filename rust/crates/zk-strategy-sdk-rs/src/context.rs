use std::any::Any;
use std::collections::{HashMap, HashSet};

use zk_proto_rs::zk::{
    common::v1::InstrumentRefData,
    oms::v1::{Order, OrderStatus, OrderUpdateEvent, Position, PositionUpdateEvent},
};

use crate::models::{StrategyCancel, StrategyOrder};

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
    /// Latest known position per instrument code.
    pub balances: HashMap<String, Position>,
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
    /// Monotonic counter incremented every time position/balance state changes.
    /// Callers can cache this value and skip balance re-snapshotting when unchanged.
    pub balance_generation: u64,
    /// Arbitrary init data injected by the runtime before `on_init` fires.
    /// Mirrors Python `tq.__tq_init_output__` / `tq.get_custom_init_data()`.
    init_data: Option<Box<dyn Any + Send + 'static>>,
}

impl StrategyContext {
    pub fn new(account_ids: &[i64], refdata: &[InstrumentRefData]) -> Self {
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
            init_data: None,
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
        self.order_id_to_account.insert(order.order_id, order.account_id);
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

    /// Update balance tracking from an OMS position update event.
    pub fn on_position_update(&mut self, pue: &PositionUpdateEvent) {
        for pos in &pue.position_snapshots {
            if let Some(acc) = self.account_states.get_mut(&pos.account_id) {
                acc.balances.insert(pos.instrument_code.clone(), pos.clone());
            }
        }
        self.balance_generation += 1;
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
            .and_then(|acc| acc.balances.get(symbol))
    }

    pub fn get_symbol_info(&self, symbol: &str) -> Option<&InstrumentRefData> {
        self.symbol_refs.get(symbol)
    }

    /// All balances held by `account_id` (symbol → Position snapshot).
    pub fn get_balances_map(
        &self,
        account_id: i64,
    ) -> Option<&HashMap<String, Position>> {
        self.account_states.get(&account_id).map(|acc| &acc.balances)
    }

    /// All registered account IDs.
    pub fn account_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.account_states.keys().copied()
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
