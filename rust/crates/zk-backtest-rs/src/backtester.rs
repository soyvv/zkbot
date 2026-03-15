use std::any::Any;
use std::collections::HashMap;

use zk_proto_rs::zk::common::v1::InstrumentRefData;
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, TimerSchedule},
    runner::StrategyRunner,
    strategy::Strategy,
};

use crate::{
    backtest_oms::BacktestOms,
    event_queue::{BtEventKind, EventQueue},
    match_policy::MatchPolicy,
    models::BtOmsOutput,
};

/// Type alias for the init-data fetcher closure used in `BacktestConfig`.
/// The `+ Send` bound is required so that `Backtester` is itself `Send` (needed for PyO3).
pub type InitDataFetcher =
    Box<dyn FnOnce(&StrategyContext) -> Box<dyn Any + Send + 'static> + Send>;

/// Configuration for a single backtest run.
pub struct BacktestConfig {
    pub account_ids: Vec<i64>,
    pub refdata: Vec<InstrumentRefData>,
    /// Initial holdings: account_id → symbol → quantity.
    pub init_balances: Option<HashMap<i64, HashMap<String, f64>>>,
    pub match_policy: Box<dyn MatchPolicy>,
    /// Optional init-data provider called once after `on_create` and before `on_init`.
    /// Mirrors Python `BacktestConfig.init_data_fetcher`.
    pub init_data_fetcher: Option<InitDataFetcher>,
    /// Optional progress callback: receives percentage (0–100) as events are processed.
    /// Called at each 1 % boundary of the backtest time range.
    /// Mirrors Python `StrategyBacktestor(progress_callback=...)`.
    pub progress_callback: Option<Box<dyn Fn(u8) + Send + 'static>>,
}

// ---------------------------------------------------------------------------
// ProgressTracker
// ---------------------------------------------------------------------------

/// Fires a `Fn(u8)` callback at each 1 % milestone of the backtest time range.
///
/// 101 checkpoints are pre-computed (0 % … 100 %). `update(now_ms)` is called
/// after every event and fires the callback for each checkpoint that `now_ms` has
/// reached or passed. Mirrors Python `ProgressHandler`.
struct ProgressTracker {
    /// Pre-computed timestamps for 0 %, 1 %, …, 100 %.
    checkpoints: Vec<i64>,
    next: usize,
    callback: Box<dyn Fn(u8) + Send + 'static>,
}

impl ProgressTracker {
    fn new(start_ms: i64, end_ms: i64, callback: Box<dyn Fn(u8) + Send + 'static>) -> Self {
        let total = end_ms.saturating_sub(start_ms);
        let checkpoints = (0i64..=100)
            .map(|i| start_ms + total * i / 100)
            .collect();
        Self { checkpoints, next: 0, callback }
    }

    /// Advance progress to cover all checkpoints ≤ `now_ms`.
    fn update(&mut self, now_ms: i64) {
        while self.next < self.checkpoints.len() && now_ms >= self.checkpoints[self.next] {
            (self.callback)(self.next as u8);
            self.next += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// BacktestResult
// ---------------------------------------------------------------------------

/// Summary results collected during a backtest run.
#[derive(Default)]
pub struct BacktestResult {
    /// (ts_ms, strategy_order) for each placement emitted by the strategy.
    pub order_placements: Vec<(i64, zk_strategy_sdk_rs::models::StrategyOrder)>,
    pub order_updates: Vec<zk_proto_rs::zk::oms::v1::OrderUpdateEvent>,
    pub balance_updates: Vec<zk_proto_rs::zk::oms::v1::BalanceUpdateEvent>,
    pub position_updates: Vec<zk_proto_rs::zk::oms::v1::PositionUpdateEvent>,
    pub logs: Vec<zk_strategy_sdk_rs::models::StrategyLog>,
    /// All fills extracted from `OrderUpdateEvent.last_trade`.
    pub trades: Vec<zk_proto_rs::zk::oms::v1::Trade>,
}

// ---------------------------------------------------------------------------
// Backtester
// ---------------------------------------------------------------------------

/// Drives a deterministic replay backtest.
///
/// Usage:
/// 1. Construct with `BacktestConfig`.
/// 2. Add market data with `add_sorted_stream` (one call per data type / symbol).
/// 3. Call `run(strategy)` to execute the event loop.
/// 4. Call `result()` to retrieve collected output.
pub struct Backtester {
    runner: StrategyRunner,
    oms: BacktestOms,
    queue: EventQueue,
    result: BacktestResult,
    /// Consumed on first `run()` call.
    init_data_fetcher: Option<InitDataFetcher>,
    /// Consumed at the start of `run()` to build a `ProgressTracker`.
    progress_callback: Option<Box<dyn Fn(u8) + Send + 'static>>,
    /// Inferred from added streams; used to set up progress checkpoints.
    data_start_ms: Option<i64>,
    data_end_ms: Option<i64>,
}

impl Backtester {
    pub fn new(config: BacktestConfig) -> Self {
        let runner = StrategyRunner::new(&config.account_ids, &config.refdata);
        let oms = BacktestOms::new(
            config.refdata,
            config.account_ids,
            config.match_policy,
            config.init_balances.as_ref(),
        );
        Self {
            runner,
            oms,
            queue: EventQueue::new(),
            result: BacktestResult::default(),
            init_data_fetcher: config.init_data_fetcher,
            progress_callback: config.progress_callback,
            data_start_ms: None,
            data_end_ms: None,
        }
    }

    /// Replace the init-data fetcher after construction (used by the PyO3 binding).
    pub fn set_init_data_fetcher(&mut self, fetcher: InitDataFetcher) {
        self.init_data_fetcher = Some(fetcher);
    }

    /// Set or replace the progress callback after construction (used by the PyO3 binding).
    pub fn set_progress_callback(&mut self, cb: Box<dyn Fn(u8) + Send + 'static>) {
        self.progress_callback = Some(cb);
    }

    /// Add a pre-sorted event stream (e.g. all klines for one symbol).
    ///
    /// Events need not be globally sorted — each stream is sorted internally at
    /// insertion time. Multiple streams are merged lazily at `pop()` time via a
    /// k-way merge, keeping the in-memory priority heap O(k + dynamic) rather
    /// than O(n_events). This mirrors Python's per-type DataFrame + lazy pull.
    ///
    /// Also records the overall time range for progress callback scheduling.
    pub fn add_sorted_stream(&mut self, events: Vec<(i64, BtEventKind)>) {
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            let first_ts = first.0;
            let last_ts = last.0;
            self.data_start_ms =
                Some(self.data_start_ms.map_or(first_ts, |s| s.min(first_ts)));
            self.data_end_ms =
                Some(self.data_end_ms.map_or(last_ts, |e| e.max(last_ts)));
        }
        self.queue.add_stream(events);
    }

    /// Run the backtest event loop with `strategy`.
    /// Returns a reference to the collected result.
    pub fn run<S: Strategy>(&mut self, strategy: &mut S) -> &BacktestResult {
        // Build progress tracker if callback + time range are available.
        let mut progress: Option<ProgressTracker> =
            if let Some(cb) = self.progress_callback.take() {
                let start = self.data_start_ms.unwrap_or(0);
                let end = self.data_end_ms.unwrap_or(start);
                Some(ProgressTracker::new(start, end, cb))
            } else {
                None
            };

        // 1. on_create
        let create_actions = self.runner.on_create(strategy);
        self.dispatch_actions(create_actions, 0);

        // 2. Inject init data (if a fetcher was provided), then call on_init.
        if let Some(fetcher) = self.init_data_fetcher.take() {
            let data = fetcher(&self.runner.ctx);
            self.runner.ctx.set_init_data(data);
        }
        let on_init_actions = self.runner.on_init(strategy);
        self.dispatch_actions(on_init_actions, 0);

        // 3. on_reinit
        let init_actions = self.runner.on_reinit(strategy);
        self.dispatch_actions(init_actions, 0);

        // 4. Event loop
        while let Some(event) = self.queue.pop() {
            let ts = event.ts_ms;

            // Report progress for all checkpoints reached by this event's timestamp.
            if let Some(p) = &mut progress {
                p.update(ts);
            }

            // Drain timers due at this event's timestamp.
            // For synthetic OUE/PUE events (bar_ts + small offset) we fire timers
            // but do NOT update ctx.current_ts_ms, preserving the originating bar
            // time so that strategy calls to tq.get_current_ts() match Python behaviour.
            let actions = match &event.kind {
                BtEventKind::OrderUpdate(_) | BtEventKind::BalanceUpdate(_) | BtEventKind::PositionUpdate(_) => {
                    let timer_actions = self.runner.drain_timers_at(strategy, ts);
                    self.dispatch_actions(timer_actions, ts);
                    // Use _preserve_ts variants: ctx.on_order_update / ctx.on_position_update
                    // would advance current_ts_ms to the synthetic OUE/PUE timestamp
                    // (bar_ts + small offset). Restoring the saved bar time before the
                    // strategy callback ensures tq.get_current_ts() matches Python behaviour.
                    match event.kind {
                        BtEventKind::OrderUpdate(oue) => {
                            if let Some(trade) = &oue.last_trade {
                                self.result.trades.push(trade.clone());
                            }
                            self.result.order_updates.push(oue.clone());
                            self.runner.on_order_update_preserve_ts(strategy, &oue)
                        }
                        BtEventKind::BalanceUpdate(bue) => {
                            self.result.balance_updates.push(bue.clone());
                            self.runner.on_balance_update(strategy, &bue)
                        }
                        BtEventKind::PositionUpdate(pue) => {
                            self.result.position_updates.push(pue.clone());
                            self.runner.on_position_update(strategy, &pue)
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    // Real market data events: advance ctx.current_ts_ms and fire timers.
                    let timer_actions = self.runner.advance_time(strategy, ts);
                    self.dispatch_actions(timer_actions, ts);
                    match event.kind {
                        BtEventKind::Tick(tick) => {
                            let sim_out = self.oms.on_tick(&tick);
                            self.requeue_bt_oms_output(sim_out);
                            self.runner.on_tick(strategy, &tick)
                        }
                        BtEventKind::Bar(bar) => self.runner.on_bar(strategy, &bar),
                        BtEventKind::Signal(sig) => self.runner.on_signal(strategy, &sig),
                        BtEventKind::Timer(te) => strategy.on_timer(&te, &self.runner.ctx),
                        _ => unreachable!(),
                    }
                }
            };
            self.dispatch_actions(actions, ts);
        }

        // Ensure 100 % is always fired.
        if let Some(p) = &mut progress {
            p.update(i64::MAX);
        }

        &self.result
    }

    pub fn result(&self) -> &BacktestResult {
        &self.result
    }

    /// Returns the (start_ms, end_ms) time range inferred from added event streams.
    pub fn data_range_ms(&self) -> (Option<i64>, Option<i64>) {
        (self.data_start_ms, self.data_end_ms)
    }

    // --- internal ---

    fn dispatch_actions(&mut self, actions: Vec<SAction>, ts: i64) {
        for action in actions {
            match action {
                SAction::PlaceOrder(order) => {
                    self.result.order_placements.push((ts, order.clone()));
                    self.runner.book_order(&order);
                    let bt_out = self.oms.on_new_order(&order, ts);
                    self.requeue_bt_oms_output(bt_out);
                }
                SAction::Cancel(cancel) => {
                    self.runner.book_cancel(&cancel);
                    let bt_out = self.oms.on_cancel(&cancel, ts);
                    self.requeue_bt_oms_output(bt_out);
                }
                SAction::Log(log) => {
                    self.result.logs.push(log);
                }
                SAction::SubscribeTimer(sub) => match sub.schedule {
                    TimerSchedule::OnceAt(fire_ts) => {
                        self.runner.timer.subscribe_once_at(&sub.timer_key, fire_ts);
                    }
                    TimerSchedule::Cron { expr, start_ms, end_ms } => {
                        let start = start_ms.unwrap_or(0);
                        self.runner.timer.subscribe_cron(&sub.timer_key, &expr, start, end_ms);
                    }
                },
            }
        }
    }

    fn requeue_bt_oms_output(&mut self, outputs: Vec<BtOmsOutput>) {
        for out in outputs {
            if let Some(oue) = out.order_update {
                self.queue.push(out.ts_ms, BtEventKind::OrderUpdate(oue));
            }
            if let Some(bue) = out.balance_update {
                self.queue.push(out.ts_ms, BtEventKind::BalanceUpdate(bue));
            }
            if let Some(pue) = out.position_update {
                self.queue.push(out.ts_ms, BtEventKind::PositionUpdate(pue));
            }
        }
    }
}
