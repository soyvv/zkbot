use zk_proto_rs::zk::{
    oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{Kline, RealtimeSignal, TickData},
};

use crate::{
    context::StrategyContext,
    models::{SAction, TimerEvent},
};

/// Rust-native strategy trait.
///
/// Mirrors Python's `StrategyBase` callbacks. Each handler receives the event,
/// a read-only view of the current strategy state (`ctx`), and returns any
/// actions the strategy wants the runtime to execute.
/// All callbacks have no-op defaults.
pub trait Strategy: Send + 'static {
    /// Called once when the strategy is created (before init).
    fn on_create(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Called once after OMS state is loaded and init data is available in `ctx`.
    /// Runs before `on_reinit` and before any market events.
    /// Use `ctx.get_init_data::<T>()` to access data injected by the runtime.
    /// Mirrors Python `__tq_init__` (sync; async live-engine path is Phase 4/WS8).
    fn on_init(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Called on each (re-)initialization. Subscribe timers and set up state here.
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Tick / orderbook update.
    fn on_tick(&mut self, _tick: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Bar / kline event.
    fn on_bar(&mut self, _bar: &Kline, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Signal event.
    fn on_signal(&mut self, _signal: &RealtimeSignal, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Order state update.
    fn on_order_update(
        &mut self,
        _update: &OrderUpdateEvent,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        vec![]
    }

    /// Balance update — asset inventory (cash/spot).
    fn on_balance_update(
        &mut self,
        _update: &BalanceUpdateEvent,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        vec![]
    }

    /// Position update — instrument exposure (derivatives only).
    fn on_position_update(
        &mut self,
        _update: &PositionUpdateEvent,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        vec![]
    }

    /// Timer fire.
    fn on_timer(&mut self, _event: &TimerEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Called when the engine is paused (e.g. by gRPC command or supervision).
    /// Return actions to execute before the engine enters paused state (e.g. cancel open orders).
    fn on_pause(&mut self, _reason: &str, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Called when the engine resumes from a paused state.
    /// Return actions to execute after resuming (e.g. re-enter positions).
    fn on_resume(&mut self, _reason: &str, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }
}

impl<T: Strategy + ?Sized> Strategy for Box<T> {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_create(ctx)
    }

    fn on_init(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_init(ctx)
    }

    fn on_reinit(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_reinit(ctx)
    }

    fn on_tick(&mut self, tick: &TickData, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_tick(tick, ctx)
    }

    fn on_bar(&mut self, bar: &Kline, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_bar(bar, ctx)
    }

    fn on_signal(&mut self, signal: &RealtimeSignal, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_signal(signal, ctx)
    }

    fn on_order_update(
        &mut self,
        update: &OrderUpdateEvent,
        ctx: &StrategyContext,
    ) -> Vec<SAction> {
        (**self).on_order_update(update, ctx)
    }

    fn on_balance_update(
        &mut self,
        update: &BalanceUpdateEvent,
        ctx: &StrategyContext,
    ) -> Vec<SAction> {
        (**self).on_balance_update(update, ctx)
    }

    fn on_position_update(
        &mut self,
        update: &PositionUpdateEvent,
        ctx: &StrategyContext,
    ) -> Vec<SAction> {
        (**self).on_position_update(update, ctx)
    }

    fn on_timer(&mut self, event: &TimerEvent, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_timer(event, ctx)
    }

    fn on_pause(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_pause(reason, ctx)
    }

    fn on_resume(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        (**self).on_resume(reason, ctx)
    }
}
