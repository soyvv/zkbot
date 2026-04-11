/// `LiveEngine` — async event loop wrapping `StrategyRunner`.
///
/// Mirrors Python `StrategyRealtimeEngine`:
///   - Serialises all events through a `tokio::sync::mpsc` channel (asyncio.Queue).
///   - Coalesces tick events before dispatch (deduplicates by symbol).
///   - Runs the startup lifecycle: `on_create → on_init → on_reinit`.
///   - Dispatches market events and timer fires to the strategy.
///   - Processes returned `SAction`s via `ActionDispatcher`.
///   - Provides `run_timer_clock()` to periodically push `Timer` events.
///   - Builds `TriggerContext` per event and threads it through dispatch for
///     end-to-end latency observability.
///
/// # Usage
///
/// ```rust,ignore
/// use tokio::sync::mpsc;
/// use zk_engine_rs::live_engine::LiveEngine;
/// use zk_engine_rs::action_dispatcher::NoopDispatcher;
/// use zk_engine_rs::engine_event::{EngineEvent, EventEnvelope};
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
///         "exec-1".into(),
///         "my-strat".into(),
///     );
///
///     tokio::spawn(async move {
///         engine.run(rx).await;
///     });
///
///     // Push a bar:
///     tx.send(EventEnvelope::now(EngineEvent::Bar(bar))).await.ok();
/// }
/// ```
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;

use zk_proto_rs::zk::common::v1::InstrumentRefData;
use zk_strategy_sdk_rs::{
    context::StrategyIdAllocator,
    models::{SAction, TimerSubscription},
    runner::StrategyRunner,
    strategy::Strategy,
};

use crate::{
    action_dispatcher::ActionDispatcher,
    engine_event::{
        coalesce_ticks, count_coalesced, system_time_ns, ControlCommand, EngineEvent,
        EventEnvelope, TriggerContext,
    },
    latency::EngineLatencyTracker,
    snapshot::{uptime_ms_since, EngineReadReplica, EngineSnapshot, LifecycleState},
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
    /// Identifies the execution run — propagated into `TriggerContext`.
    execution_id: String,
    /// Strategy key — propagated into `TriggerContext`.
    strategy_key: String,
    /// Monotonic decision counter, incremented each time an event produces actions.
    decision_seq: u64,
    /// Hot-path latency tracker — updated inline in `run()`.
    pub latency: EngineLatencyTracker,
    /// Lock-free read replica for gRPC query handlers.
    replica: EngineReadReplica,
    /// Engine start time for uptime calculation.
    start_time: Instant,
    /// Current lifecycle state — driven by control commands and engine logic.
    lifecycle_state: LifecycleState,
    /// Whether the engine is paused. While paused, market events (tick/bar/signal/timer)
    /// are dropped; OMS events (order/balance/position updates) still flow through.
    paused: bool,
}

impl<S: Strategy, D: ActionDispatcher> LiveEngine<S, D> {
    pub fn new(
        account_ids: Vec<i64>,
        refdata: Vec<InstrumentRefData>,
        strategy: S,
        dispatcher: D,
        execution_id: String,
        strategy_key: String,
    ) -> Self {
        Self::new_with_id_allocator(
            account_ids,
            refdata,
            strategy,
            dispatcher,
            execution_id,
            strategy_key,
            None,
        )
    }

    pub fn new_with_id_allocator(
        account_ids: Vec<i64>,
        refdata: Vec<InstrumentRefData>,
        strategy: S,
        dispatcher: D,
        execution_id: String,
        strategy_key: String,
        id_allocator: Option<Arc<dyn StrategyIdAllocator>>,
    ) -> Self {
        let runner = StrategyRunner::new_with_id_allocator(&account_ids, &refdata, id_allocator);
        let replica = crate::snapshot::new_read_replica(&execution_id, &strategy_key);
        Self {
            runner,
            strategy,
            dispatcher,
            execution_id,
            strategy_key,
            decision_seq: 0,
            latency: EngineLatencyTracker::new(64),
            replica,
            start_time: Instant::now(),
            lifecycle_state: LifecycleState::Starting,
            paused: false,
        }
    }

    /// Returns a clone of the read replica handle.
    /// Pass this to gRPC query handlers before calling `run()`.
    pub fn read_replica(&self) -> EngineReadReplica {
        Arc::clone(&self.replica)
    }

    /// Run the startup lifecycle (sync): `on_create → on_init → on_reinit`.
    ///
    /// Call before `run()`. Separated so callers can inject `init_data` into
    /// `self.runner.ctx` between `on_create` and `on_init` (the `__tq_init__` slot).
    ///
    /// During startup, a synthetic `TriggerContext` is used for any dispatched
    /// actions (trigger_event_type = "lifecycle").
    pub fn startup(&mut self) {
        let actions = self.runner.on_create(&mut self.strategy);
        self.dispatch_actions_lifecycle(actions);

        // on_init reads ctx.init_data if set by the caller.
        let actions = self.runner.on_init(&mut self.strategy);
        self.dispatch_actions_lifecycle(actions);

        let actions = self.runner.on_reinit(&mut self.strategy);
        self.dispatch_actions_lifecycle(actions);
    }

    /// Inject init data between `on_create` and `on_init`.
    ///
    /// Call after constructing the engine but before `startup()` if you need
    /// to provide warmup data (e.g. historical klines).
    pub fn set_init_data(&mut self, data: Box<dyn std::any::Any + Send + 'static>) {
        self.runner.ctx.set_init_data(data);
    }

    /// Main async event loop. Runs until the sender side of `rx` is dropped
    /// or a `ControlCommand::Stop` is received.
    ///
    /// Drains the channel in batches (coalescing ticks), then dispatches each
    /// event to the strategy with a `TriggerContext` for latency observability.
    ///
    /// While paused:
    /// - OMS events (order/balance/position updates) still flow through to keep
    ///   strategy state consistent.
    /// - Market events (tick/bar/signal) and timer events are dropped.
    /// - Control commands are always processed.
    pub async fn run(&mut self, mut rx: mpsc::Receiver<EventEnvelope>) {
        self.lifecycle_state = LifecycleState::Running;
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
            let pre_coalesce_len = batch.len();
            let batch = coalesce_ticks(batch);
            let coalesced = count_coalesced(pre_coalesce_len, batch.len());

            let mut should_stop = false;
            let mut dispatched_count = 0usize;

            for envelope in batch {
                // Handle control commands before normal dispatch.
                if let EngineEvent::Control(ref cmd) = envelope.event {
                    match cmd {
                        ControlCommand::Pause { reason } => {
                            self.handle_pause(reason);
                        }
                        ControlCommand::Resume { reason } => {
                            self.handle_resume(reason);
                        }
                        ControlCommand::Stop { .. } => {
                            should_stop = true;
                            break;
                        }
                    }
                    continue;
                }

                // While paused: only OMS state events flow through.
                if self.paused {
                    match &envelope.event {
                        EngineEvent::OrderUpdate(_)
                        | EngineEvent::BalanceUpdate(_)
                        | EngineEvent::PositionUpdate(_) => {
                            // Allow — keep strategy state consistent.
                        }
                        _ => continue, // Drop ticks/bars/signals/timers while paused.
                    }
                }

                // Build trigger context from envelope at dispatch time.
                let dispatch_instant = Instant::now();
                let mut ctx = TriggerContext::from_envelope(
                    &envelope,
                    &self.execution_id,
                    &self.strategy_key,
                );
                ctx.dispatch_instant = Some(dispatch_instant);

                let actions = self.process_event(envelope.event);

                // Stamp decision time and assign sequence number.
                ctx.decision_ts_ns = system_time_ns();
                ctx.decision_instant = Some(Instant::now());
                if !actions.is_empty() {
                    self.decision_seq += 1;
                    ctx.decision_seq = self.decision_seq;
                    // Count order actions for latency tracking.
                    let order_count = actions
                        .iter()
                        .filter(|a| matches!(a, SAction::PlaceOrder(_)))
                        .count();
                    self.latency.record_action_event(&ctx, order_count);
                }

                self.dispatch_actions(actions, &ctx);
                dispatched_count += 1;
            }

            // Record batch-level metrics (only events that reached strategy dispatch).
            self.latency.record_batch(dispatched_count, coalesced);

            // Publish snapshot once per batch (not per event).
            self.publish_snapshot();

            if should_stop {
                self.lifecycle_state = LifecycleState::Stopping;
                self.publish_snapshot();
                break;
            }
        }

        // Terminal state — covers both channel-close and Stop command exits.
        self.lifecycle_state = LifecycleState::Stopped;
        self.publish_snapshot();
    }

    /// Handle a Pause control command: call strategy's on_pause, update state.
    fn handle_pause(&mut self, reason: &str) {
        if self.paused {
            return; // Already paused — idempotent.
        }
        tracing::info!(reason, "engine pausing");
        self.lifecycle_state = LifecycleState::Pausing;
        let actions = self.strategy.on_pause(reason, &self.runner.ctx);
        self.dispatch_actions_lifecycle(actions);
        self.paused = true;
        self.lifecycle_state = LifecycleState::Paused;
    }

    /// Handle a Resume control command: call strategy's on_resume, update state.
    fn handle_resume(&mut self, reason: &str) {
        if !self.paused {
            return; // Not paused — idempotent.
        }
        tracing::info!(reason, "engine resuming");
        self.lifecycle_state = LifecycleState::Resuming;
        let actions = self.strategy.on_resume(reason, &self.runner.ctx);
        self.dispatch_actions_lifecycle(actions);
        self.paused = false;
        self.lifecycle_state = LifecycleState::Running;
    }

    /// Build and publish an `EngineSnapshot` to the read replica.
    fn publish_snapshot(&self) {
        let snap = EngineSnapshot {
            execution_id: self.execution_id.clone(),
            strategy_key: self.strategy_key.clone(),
            lifecycle_state: self.lifecycle_state,
            paused: self.paused,
            degraded_reason: None,
            events_processed: self.latency.events_processed,
            ticks_coalesced: self.latency.ticks_coalesced,
            orders_dispatched: self.latency.orders_dispatched,
            decision_seq: self.decision_seq,
            uptime_ms: uptime_ms_since(self.start_time),
            last_event_ts_ns: system_time_ns(),
            avg_queue_wait_ns: self.latency.avg_queue_wait_ns(),
            avg_decision_ns: self.latency.avg_decision_ns(),
            max_queue_wait_ns: self.latency.max_queue_wait_ns,
            max_decision_ns: self.latency.max_decision_ns,
            action_event_count: self.latency.action_event_count(),
            recent_latency_samples: self.latency.recent_samples(),
        };
        self.replica.store(Arc::new(snap));
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn process_event(&mut self, event: EngineEvent) -> Vec<SAction> {
        match event {
            EngineEvent::Tick(t) => self.runner.on_tick(&mut self.strategy, &t),
            EngineEvent::Bar(b) => self.runner.on_bar(&mut self.strategy, &b),
            EngineEvent::OrderUpdate(oue) => self.runner.on_order_update(&mut self.strategy, &oue),
            EngineEvent::BalanceUpdate(bue) => {
                self.runner.on_balance_update(&mut self.strategy, &bue)
            }
            EngineEvent::PositionUpdate(pue) => {
                self.runner.on_position_update(&mut self.strategy, &pue)
            }
            EngineEvent::Signal(s) => self.runner.on_signal(&mut self.strategy, &s),
            EngineEvent::Timer(now_ms) => self.runner.advance_time(&mut self.strategy, now_ms),
            EngineEvent::Control(_) => vec![], // Handled before process_event; should not reach here.
        }
    }

    fn dispatch_actions(&mut self, actions: Vec<SAction>, ctx: &TriggerContext) {
        for action in actions {
            match action {
                SAction::PlaceOrder(order) => {
                    self.runner.book_order(&order);
                    self.dispatcher.place_order(&order, ctx);
                }
                SAction::Cancel(cancel) => {
                    self.runner.book_cancel(&cancel);
                    self.dispatcher.cancel_order(&cancel, ctx);
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

    /// Dispatch actions during startup lifecycle (on_create/on_init/on_reinit).
    /// Uses a synthetic TriggerContext with trigger_event_type = "lifecycle".
    fn dispatch_actions_lifecycle(&mut self, actions: Vec<SAction>) {
        if actions.is_empty() {
            return;
        }
        let now = system_time_ns();
        let ctx = TriggerContext {
            trigger_event_type: "lifecycle",
            instrument_code: String::new(),
            source_ts_ns: 0,
            recv_ts_ns: now,
            enqueue_ts_ns: now,
            dispatch_ts_ns: now,
            decision_ts_ns: now,
            execution_id: self.execution_id.clone(),
            strategy_key: self.strategy_key.clone(),
            decision_seq: 0,
            dispatch_instant: None,
            decision_instant: None,
        };
        self.dispatch_actions(actions, &ctx);
    }
}

/// Subscribe a timer from an `SAction::SubscribeTimer` into the runner's timer manager.
fn subscribe_timer(
    timer: &mut zk_strategy_sdk_rs::timer_manager::TimerManager,
    sub: TimerSubscription,
) {
    use zk_strategy_sdk_rs::models::TimerSchedule;
    match sub.schedule {
        TimerSchedule::Cron {
            expr,
            start_ms,
            end_ms,
        } => {
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
pub async fn run_timer_clock(tx: mpsc::Sender<EventEnvelope>, interval_ms: u64) {
    let dur = tokio::time::Duration::from_millis(interval_ms);
    let mut ticker = tokio::time::interval(dur);
    loop {
        ticker.tick().await;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let envelope = EventEnvelope::now(EngineEvent::Timer(now_ms));
        if tx.send(envelope).await.is_err() {
            break; // receiver dropped
        }
    }
}
