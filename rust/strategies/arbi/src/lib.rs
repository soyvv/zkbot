use zk_proto_rs::zk::rtmd::v1::RealtimeSignal;
use zk_strategy_sdk_rs::context::StrategyContext;
use zk_strategy_sdk_rs::models::{SAction, StrategyLog};
use zk_strategy_sdk_rs::strategy::Strategy;

/// Placeholder arbitrage strategy crate.
#[derive(Debug, Default, Clone)]
pub struct ArbitrageStrategy {
    signal_count: u64,
}

impl ArbitrageStrategy {
    pub fn new() -> Self {
        Self::default()
    }

    fn log(message: impl Into<String>, ts_ms: i64) -> SAction {
        SAction::Log(StrategyLog {
            ts_ms,
            message: message.into(),
        })
    }
}

impl Strategy for ArbitrageStrategy {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log("arbi_placeholder.on_create", ctx.current_ts_ms)]
    }

    fn on_signal(&mut self, signal: &RealtimeSignal, ctx: &StrategyContext) -> Vec<SAction> {
        self.signal_count += 1;
        vec![Self::log(
            format!(
                "arbi_placeholder.on_signal instrument={} count={}",
                signal.instrument, self.signal_count
            ),
            ctx.current_ts_ms,
        )]
    }

    fn on_resume(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log(
            format!("arbi_placeholder.on_resume reason={reason}"),
            ctx.current_ts_ms,
        )]
    }
}
