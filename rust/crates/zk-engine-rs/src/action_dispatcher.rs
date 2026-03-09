/// `ActionDispatcher` — side-effect handler for strategy `SAction`s.
///
/// The live engine calls these methods after each strategy callback.
/// In production, implementations publish orders to OMS via NATS, emit logs,
/// and send signals. In tests, `RecordingDispatcher` captures all dispatched
/// actions for assertion.
///
/// Timer subscriptions are handled directly by the engine (they update
/// `StrategyRunner::timer`), not via this trait.
use zk_strategy_sdk_rs::models::{StrategyCancel, StrategyLog, StrategyOrder};

pub trait ActionDispatcher: Send + 'static {
    /// Send a new limit order to OMS.
    fn place_order(&mut self, order: &StrategyOrder);

    /// Send a cancel request to OMS.
    fn cancel_order(&mut self, cancel: &StrategyCancel);

    /// Publish a strategy log line.
    fn log(&mut self, log: &StrategyLog);
}

// ---------------------------------------------------------------------------
// NoopDispatcher — discards all actions; useful in tests that only care about
// strategy callback invocations, not about side-effects.
// ---------------------------------------------------------------------------

pub struct NoopDispatcher;

impl ActionDispatcher for NoopDispatcher {
    fn place_order(&mut self, _order: &StrategyOrder) {}
    fn cancel_order(&mut self, _cancel: &StrategyCancel) {}
    fn log(&mut self, _log: &StrategyLog) {}
}

// ---------------------------------------------------------------------------
// RecordingDispatcher — captures all dispatched actions for test assertions.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct RecordingDispatcher {
    pub orders: Vec<StrategyOrder>,
    pub cancels: Vec<StrategyCancel>,
    pub logs: Vec<StrategyLog>,
}

impl ActionDispatcher for RecordingDispatcher {
    fn place_order(&mut self, order: &StrategyOrder) {
        self.orders.push(order.clone());
    }
    fn cancel_order(&mut self, cancel: &StrategyCancel) {
        self.cancels.push(cancel.clone());
    }
    fn log(&mut self, log: &StrategyLog) {
        self.logs.push(log.clone());
    }
}
