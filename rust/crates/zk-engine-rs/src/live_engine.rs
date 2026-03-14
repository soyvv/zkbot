/// `LiveEngine` — async event loop wrapping `StrategyRunner`.
///
/// Mirrors Python `StrategyRealtimeEngine`:
///   - Serialises all events through a `tokio::sync::mpsc` channel (asyncio.Queue).
///   - Coalesces tick events before dispatch (deduplicates by symbol).
///   - Runs the startup lifecycle: `on_create → on_init → on_reinit`.
///   - Dispatches market events and timer fires to the strategy.
///   - Processes returned `SAction`s via `ActionDispatcher`.
///   - Provides `run_timer_clock()` to periodically push `Timer` events.
///
/// # Usage
///
/// ```rust,ignore
/// use tokio::sync::mpsc;
/// use zk_engine_rs::live_engine::LiveEngine;
/// use zk_engine_rs::action_dispatcher::NoopDispatcher;
/// use zk_engine_rs::engine_event::EngineEvent;
/// use zk_proto_rs::zk::common::v1::InstrumentRefData;
///
/// #[tokio::main]
/// async fn main() {
///     let (tx, rx) = mpsc::channel(128);
///     let mut engine = LiveEngine::new(
///         vec![123],
///         vec![InstrumentRefData::default()],
///         my_strategy,
///         NoopDispatcher,
///     );
///
///     tokio::spawn(async move {
///         engine.run(rx).await;
///     });
///
///     // Push a bar:
///     tx.send(EngineEvent::Bar(bar)).await.ok();
/// }
/// ```
use tokio::sync::mpsc;

use zk_proto_rs::zk::common::v1::InstrumentRefData;
use zk_strategy_sdk_rs::{
    models::{SAction, TimerSubscription},
    runner::StrategyRunner,
    strategy::Strategy,
};

use crate::{
    action_dispatcher::ActionDispatcher,
    engine_event::{coalesce_ticks, EngineEvent},
};

/// Configuration for the live engine startup lifecycle.
pub struct EngineConfig {
    pub account_ids: Vec<i64>,
    pub refdata: Vec<InstrumentRefData>,
}

/// The live strategy engine.
///
/// Generic over `S: Strategy` (the concrete strategy type) and
/// `D: ActionDispatcher` (the side-effect handler).
pub struct LiveEngine<S: Strategy, D: ActionDispatcher> {
    runner: StrategyRunner,
    strategy: S,
    dispatcher: D,
}

impl<S: Strategy, D: ActionDispatcher> LiveEngine<S, D> {
    pub fn new(
        account_ids: Vec<i64>,
        refdata: Vec<InstrumentRefData>,
        strategy: S,
        dispatcher: D,
    ) -> Self {
        let runner = StrategyRunner::new(&account_ids, &refdata);
        Self {
            runner,
            strategy,
            dispatcher,
        }
    }

    /// Run the startup lifecycle (sync): `on_create → on_init → on_reinit`.
    ///
    /// Call before `run()`. Separated so callers can inject `init_data` into
    /// `self.runner.ctx` between `on_create` and `on_init` (the `__tq_init__` slot).
    pub fn startup(&mut self) {
        let actions = self.runner.on_create(&mut self.strategy);
        self.dispatch_actions(actions);

        // on_init reads ctx.init_data if set by the caller.
        let actions = self.runner.on_init(&mut self.strategy);
        self.dispatch_actions(actions);

        let actions = self.runner.on_reinit(&mut self.strategy);
        self.dispatch_actions(actions);
    }

    /// Inject init data between `on_create` and `on_init`.
    ///
    /// Call after constructing the engine but before `startup()` if you need
    /// to provide warmup data (e.g. historical klines).
    pub fn set_init_data(&mut self, data: Box<dyn std::any::Any + Send + 'static>) {
        self.runner.ctx.set_init_data(data);
    }

    /// Main async event loop. Runs until the sender side of `rx` is dropped.
    ///
    /// Drains the channel in batches (coalescing ticks), then dispatches each
    /// event to the strategy. Timer events advance the timer clock.
    pub async fn run(&mut self, mut rx: mpsc::Receiver<EngineEvent>) {
        loop {
            // Block until at least one event is available.
            let first = match rx.recv().await {
                Some(e) => e,
                None => break, // channel closed — shutdown
            };

            // Drain any additional events already in the buffer (non-blocking).
            let mut batch = vec![first];
            while let Ok(ev) = rx.try_recv() {
                batch.push(ev);
            }

            // Deduplicate ticks (keep latest per symbol).
            let batch = coalesce_ticks(batch);

            for event in batch {
                let actions = self.process_event(event);
                self.dispatch_actions(actions);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn process_event(&mut self, event: EngineEvent) -> Vec<SAction> {
        match event {
            EngineEvent::Tick(t) => self.runner.on_tick(&mut self.strategy, &t),
            EngineEvent::Bar(b) => self.runner.on_bar(&mut self.strategy, &b),
            EngineEvent::OrderUpdate(oue) => self.runner.on_order_update(&mut self.strategy, &oue),
            EngineEvent::PositionUpdate(pue) => {
                self.runner.on_position_update(&mut self.strategy, &pue)
            }
            EngineEvent::Signal(s) => self.runner.on_signal(&mut self.strategy, &s),
            EngineEvent::Timer(now_ms) => self.runner.advance_time(&mut self.strategy, now_ms),
        }
    }

    fn dispatch_actions(&mut self, actions: Vec<SAction>) {
        for action in actions {
            match action {
                SAction::PlaceOrder(order) => {
                    self.runner.book_order(&order);
                    self.dispatcher.place_order(&order);
                }
                SAction::Cancel(cancel) => {
                    self.runner.book_cancel(&cancel);
                    self.dispatcher.cancel_order(&cancel);
                }
                SAction::Log(log) => {
                    self.dispatcher.log(&log);
                }
                SAction::SubscribeTimer(sub) => {
                    subscribe_timer(&mut self.runner.timer, sub);
                }
            }
        }
    }
}

/// Subscribe a timer from an `SAction::SubscribeTimer` into the runner's timer manager.
fn subscribe_timer(timer: &mut zk_strategy_sdk_rs::timer_manager::TimerManager, sub: TimerSubscription) {
    use zk_strategy_sdk_rs::models::TimerSchedule;
    match sub.schedule {
        TimerSchedule::Cron { expr, start_ms, end_ms } => {
            timer.subscribe_cron(&sub.timer_key, &expr, start_ms.unwrap_or(0), end_ms);
        }
        TimerSchedule::OnceAt(fire_ms) => {
            timer.subscribe_once_at(&sub.timer_key, fire_ms);
        }
    }
}

// ---------------------------------------------------------------------------
// Timer clock — runs in a separate task, generating Timer events every second.
// ---------------------------------------------------------------------------

/// Spawn a background task that sends a `Timer(now_ms)` event every `interval_ms`
/// milliseconds. Drop the returned `JoinHandle` to stop it (or let it end when
/// `tx` is dropped).
///
/// Mirrors Python `run_clock()` (1 Hz pulse → `generate_next_batch_events`).
pub async fn run_timer_clock(tx: mpsc::Sender<EngineEvent>, interval_ms: u64) {
    let dur = tokio::time::Duration::from_millis(interval_ms);
    let mut ticker = tokio::time::interval(dur);
    loop {
        ticker.tick().await;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if tx.send(EngineEvent::Timer(now_ms)).await.is_err() {
            break; // receiver dropped
        }
    }
}
