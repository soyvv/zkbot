use std::sync::Arc;

use zk_proto_rs::zk::{
    common::v1::InstrumentRefData,
    oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{Kline, RealtimeSignal, TickData},
};

use crate::{
    context::{StrategyContext, StrategyIdAllocator},
    models::{SAction, StrategyCancel, StrategyOrder},
    strategy::Strategy,
    timer_manager::TimerManager,
};

/// Shared dispatch kernel — mirrors Python `StrategyTemplate`.
///
/// Owned by both `Backtester` (replay) and the future `LiveEngine` (real-time).
/// The caller is responsible for translating `SAction`s into runtime effects
/// (order placement, cancel, timer subscription, logging).
pub struct StrategyRunner {
    pub ctx: StrategyContext,
    pub timer: TimerManager,
}

impl StrategyRunner {
    pub fn new(account_ids: &[i64], refdata: &[InstrumentRefData]) -> Self {
        Self::new_with_id_allocator(account_ids, refdata, None)
    }

    pub fn new_with_id_allocator(
        account_ids: &[i64],
        refdata: &[InstrumentRefData],
        id_allocator: Option<Arc<dyn StrategyIdAllocator>>,
    ) -> Self {
        Self {
            ctx: match id_allocator {
                Some(alloc) => StrategyContext::new_with_id_allocator(account_ids, refdata, alloc),
                None => StrategyContext::new(account_ids, refdata),
            },
            timer: TimerManager::new(),
        }
    }

    pub fn on_create<S: Strategy>(&mut self, strategy: &mut S) -> Vec<SAction> {
        strategy.on_create(&self.ctx)
    }

    /// Call after the runtime has injected init data via `ctx.set_init_data(...)`.
    /// Mirrors the position of Python `__tq_init__` in the lifecycle.
    pub fn on_init<S: Strategy>(&mut self, strategy: &mut S) -> Vec<SAction> {
        strategy.on_init(&self.ctx)
    }

    pub fn on_reinit<S: Strategy>(&mut self, strategy: &mut S) -> Vec<SAction> {
        strategy.on_reinit(&self.ctx)
    }

    pub fn on_tick<S: Strategy>(&mut self, strategy: &mut S, tick: &TickData) -> Vec<SAction> {
        self.ctx.current_ts_ms = tick.original_timestamp;
        strategy.on_tick(tick, &self.ctx)
    }

    pub fn on_bar<S: Strategy>(&mut self, strategy: &mut S, bar: &Kline) -> Vec<SAction> {
        strategy.on_bar(bar, &self.ctx)
    }

    pub fn on_signal<S: Strategy>(
        &mut self,
        strategy: &mut S,
        sig: &RealtimeSignal,
    ) -> Vec<SAction> {
        strategy.on_signal(sig, &self.ctx)
    }

    /// Updates `ctx` with the order snapshot first, then calls the strategy callback.
    pub fn on_order_update<S: Strategy>(
        &mut self,
        strategy: &mut S,
        oue: &OrderUpdateEvent,
    ) -> Vec<SAction> {
        self.ctx.on_order_update(oue);
        strategy.on_order_update(oue, &self.ctx)
    }

    /// Like `on_order_update` but restores `ctx.current_ts_ms` after the context
    /// update so that the strategy callback sees the pre-OUE logical time.
    ///
    /// Used by the backtester for synthetic OUE events (timestamped `bar_ts + N`)
    /// to ensure `tq.get_current_ts()` returns the originating bar time, matching
    /// the Python backtester's `TokkaQuant.get_current_ts()` behaviour.
    pub fn on_order_update_preserve_ts<S: Strategy>(
        &mut self,
        strategy: &mut S,
        oue: &OrderUpdateEvent,
    ) -> Vec<SAction> {
        let saved_ts = self.ctx.current_ts_ms;
        self.ctx.on_order_update(oue);
        self.ctx.current_ts_ms = saved_ts;
        strategy.on_order_update(oue, &self.ctx)
    }

    /// Updates `ctx` with balance snapshots first, then calls the strategy callback.
    pub fn on_balance_update<S: Strategy>(
        &mut self,
        strategy: &mut S,
        bue: &BalanceUpdateEvent,
    ) -> Vec<SAction> {
        self.ctx.on_balance_update(bue);
        strategy.on_balance_update(bue, &self.ctx)
    }

    /// Updates `ctx` with position snapshots first, then calls the strategy callback.
    pub fn on_position_update<S: Strategy>(
        &mut self,
        strategy: &mut S,
        pue: &PositionUpdateEvent,
    ) -> Vec<SAction> {
        self.ctx.on_position_update(pue);
        strategy.on_position_update(pue, &self.ctx)
    }

    /// Drains all timers due at `now_ms`, calling `on_timer` for each.
    /// Also updates `ctx.current_ts_ms` so that `tq.get_current_ts()` reflects the
    /// event time during callbacks.
    /// Returns the combined actions from all timer fires.
    pub fn advance_time<S: Strategy>(&mut self, strategy: &mut S, now_ms: i64) -> Vec<SAction> {
        self.ctx.current_ts_ms = now_ms;
        let events = self.timer.drain_due(now_ms);
        let mut actions = Vec::new();
        for te in events {
            actions.extend(strategy.on_timer(&te, &self.ctx));
        }
        actions
    }

    /// Like `advance_time` but does NOT update `ctx.current_ts_ms`.
    ///
    /// Used for synthetic events (OUE / PUE) whose timestamps are artificial offsets
    /// from a bar time rather than real clock advances. Keeping `ctx.current_ts_ms`
    /// at the originating bar timestamp ensures that strategy calls to
    /// `tq.get_current_ts()` (e.g. when setting a timer) see the bar time, matching
    /// the Python backtester's behaviour.
    pub fn drain_timers_at<S: Strategy>(&mut self, strategy: &mut S, now_ms: i64) -> Vec<SAction> {
        let events = self.timer.drain_due(now_ms);
        let mut actions = Vec::new();
        for te in events {
            actions.extend(strategy.on_timer(&te, &self.ctx));
        }
        actions
    }

    /// Record a newly submitted order in the context (pending state).
    pub fn book_order(&mut self, order: &StrategyOrder) {
        self.ctx.book_order(order);
    }

    /// Record a cancel submission in the context.
    pub fn book_cancel(&mut self, cancel: &StrategyCancel) {
        self.ctx.book_cancel(cancel);
    }
}
