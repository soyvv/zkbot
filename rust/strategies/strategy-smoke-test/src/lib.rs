use zk_proto_rs::zk::common::v1::BuySellType;
use zk_proto_rs::zk::rtmd::v1::TickData;
use zk_strategy_sdk_rs::context::StrategyContext;
use zk_strategy_sdk_rs::models::{
    SAction, StrategyCancel, StrategyLog, StrategyOrder, TimerEvent, TimerSchedule,
    TimerSubscription,
};
use zk_strategy_sdk_rs::strategy::Strategy;

/// Basic strategy used to smoke-test engine lifecycle and action paths.
#[derive(Debug, Clone)]
pub struct SmokeTestStrategy {
    next_order_id: i64,
    active_order_id: Option<i64>,
    timer_armed: bool,
}

impl Default for SmokeTestStrategy {
    fn default() -> Self {
        Self {
            next_order_id: 1,
            active_order_id: None,
            timer_armed: false,
        }
    }
}

impl SmokeTestStrategy {
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

impl Strategy for SmokeTestStrategy {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log("smoke_test.on_create", ctx.current_ts_ms)]
    }

    fn on_init(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        let account_count = ctx.account_ids().count();
        vec![Self::log(
            format!("smoke_test.on_init accounts={account_count}"),
            ctx.current_ts_ms,
        )]
    }

    fn on_reinit(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log("smoke_test.on_reinit", ctx.current_ts_ms)];
        if !self.timer_armed {
            self.timer_armed = true;
            let fire_ms = if ctx.current_ts_ms > 0 {
                ctx.current_ts_ms + 1_000
            } else {
                1_000
            };
            actions.push(SAction::SubscribeTimer(TimerSubscription {
                timer_key: "smoke_test/one_shot".to_string(),
                schedule: TimerSchedule::OnceAt(fire_ms),
            }));
        }
        actions
    }

    fn on_tick(&mut self, tick: &TickData, ctx: &StrategyContext) -> Vec<SAction> {
        if self.active_order_id.is_some() {
            return vec![Self::log(
                format!("smoke_test.on_tick symbol={} ignored", tick.instrument_code),
                ctx.current_ts_ms,
            )];
        }

        let Some(account_id) = ctx.account_ids().next() else {
            return vec![Self::log(
                "smoke_test.on_tick no account_ids",
                ctx.current_ts_ms,
            )];
        };

        let order_id = self.next_order_id;
        self.next_order_id += 1;
        self.active_order_id = Some(order_id);

        vec![
            Self::log(
                format!("smoke_test.place_order symbol={}", tick.instrument_code),
                ctx.current_ts_ms,
            ),
            SAction::PlaceOrder(StrategyOrder {
                order_id,
                symbol: tick.instrument_code.clone(),
                price: 1.0,
                qty: 1.0,
                side: BuySellType::BsBuy as i32,
                account_id,
            }),
        ]
    }

    fn on_timer(&mut self, event: &TimerEvent, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log(
            format!("smoke_test.on_timer key={}", event.timer_key),
            event.ts_ms,
        )];
        if let Some(order_id) = self.active_order_id.take() {
            let account_id = ctx.account_ids().next().unwrap_or_default();
            actions.push(SAction::Cancel(StrategyCancel {
                order_id,
                account_id,
            }));
        }
        actions
    }

    fn on_pause(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log(
            format!("smoke_test.on_pause reason={reason}"),
            ctx.current_ts_ms,
        )];
        if let Some(order_id) = self.active_order_id {
            let account_id = ctx.account_ids().next().unwrap_or_default();
            actions.push(SAction::Cancel(StrategyCancel {
                order_id,
                account_id,
            }));
        }
        actions
    }

    fn on_resume(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log(
            format!("smoke_test.on_resume reason={reason}"),
            ctx.current_ts_ms,
        )]
    }
}
