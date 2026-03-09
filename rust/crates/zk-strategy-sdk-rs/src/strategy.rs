use zk_proto_rs::{
    oms::{OrderUpdateEvent, PositionUpdateEvent},
    rtmd::{Kline, RealtimeSignal, TickData},
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
    fn on_order_update(&mut self, _update: &OrderUpdateEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Position / balance update.
    fn on_position_update(&mut self, _update: &PositionUpdateEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }

    /// Timer fire.
    fn on_timer(&mut self, _event: &TimerEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![]
    }
}
