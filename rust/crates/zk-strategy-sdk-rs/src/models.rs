/// Order placement request emitted by a strategy callback.
#[derive(Debug, Clone)]
pub struct StrategyOrder {
    pub order_id: i64,
    pub symbol: String,
    pub price: f64,
    pub qty: f64,
    /// BuySellType i32 (zk_proto_rs::common::BuySellType).
    pub side: i32,
    /// OpenCloseType i32 (zk_proto_rs::common::OpenCloseType).
    pub open_close_type: i32,
    pub account_id: i64,
}

/// Cancel request emitted by a strategy callback.
#[derive(Debug, Clone)]
pub struct StrategyCancel {
    pub order_id: i64,
    pub account_id: i64,
}

/// Timer fire event delivered to a strategy.
#[derive(Debug, Clone)]
pub struct TimerEvent {
    pub timer_key: String,
    /// Millisecond unix timestamp when the timer fired.
    pub ts_ms: i64,
}

/// Structured log line emitted by a strategy.
#[derive(Debug, Clone)]
pub struct StrategyLog {
    pub ts_ms: i64,
    pub message: String,
}

/// Timer schedule: either a cron expression or a fixed one-shot timestamp.
#[derive(Debug, Clone)]
pub enum TimerSchedule {
    Cron {
        expr: String,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    },
    OnceAt(i64),
}

/// Timer subscription emitted by a strategy to register a recurring or one-shot timer.
#[derive(Debug, Clone)]
pub struct TimerSubscription {
    pub timer_key: String,
    pub schedule: TimerSchedule,
}

/// All actions a strategy can request from the runtime.
#[derive(Debug, Clone)]
pub enum SAction {
    PlaceOrder(StrategyOrder),
    Cancel(StrategyCancel),
    Log(StrategyLog),
    SubscribeTimer(TimerSubscription),
}
