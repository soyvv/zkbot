use zk_proto_rs::zk::rtmd::v1::{Kline, TickData};
use zk_strategy_sdk_rs::context::StrategyContext;
use zk_strategy_sdk_rs::models::{SAction, StrategyLog};
use zk_strategy_sdk_rs::strategy::Strategy;

/// Placeholder market-making strategy crate.
#[derive(Debug, Default, Clone)]
pub struct MarketMakingStrategy {
    last_symbol: Option<String>,
}

impl MarketMakingStrategy {
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

impl Strategy for MarketMakingStrategy {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log("mm_placeholder.on_create", ctx.current_ts_ms)]
    }

    fn on_tick(&mut self, tick: &TickData, ctx: &StrategyContext) -> Vec<SAction> {
        self.last_symbol = Some(tick.instrument_code.clone());
        vec![Self::log(
            format!("mm_placeholder.on_tick symbol={}", tick.instrument_code),
            ctx.current_ts_ms,
        )]
    }

    fn on_bar(&mut self, bar: &Kline, ctx: &StrategyContext) -> Vec<SAction> {
        self.last_symbol = Some(bar.symbol.clone());
        vec![Self::log(
            format!("mm_placeholder.on_bar symbol={}", bar.symbol),
            ctx.current_ts_ms,
        )]
    }

    fn on_pause(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log(
            format!(
                "mm_placeholder.on_pause reason={reason} last_symbol={:?}",
                self.last_symbol
            ),
            ctx.current_ts_ms,
        )]
    }
}
