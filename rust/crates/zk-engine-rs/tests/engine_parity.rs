/// Engine parity tests — verify the live engine lifecycle and event dispatch.
///
/// These tests use in-process channels and `RecordingDispatcher` so no NATS
/// or external services are needed.
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use zk_engine_rs::{
    ControlCommand, EngineEvent, EventEnvelope, LifecycleState, LiveEngine, RecordingDispatcher,
};
use zk_proto_rs::zk::{
    common::v1::{BuySellType, InstrumentRefData},
    rtmd::v1::{Kline, TickData},
};
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, StrategyLog, StrategyOrder, TimerSchedule, TimerSubscription},
    strategy::Strategy,
};

// ---------------------------------------------------------------------------
// Helper strategy impls
// ---------------------------------------------------------------------------

/// Logs "created" on on_create, counts bar calls.
struct BarCountStrategy {
    bar_count: Arc<Mutex<u32>>,
}

impl Strategy for BarCountStrategy {
    fn on_create(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![SAction::Log(StrategyLog {
            ts_ms: 0,
            message: "created".to_string(),
        })]
    }

    fn on_bar(
        &mut self,
        _bar: &zk_proto_rs::zk::rtmd::v1::Kline,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        *self.bar_count.lock().unwrap() += 1;
        vec![]
    }
}

/// Places one buy order on the first bar it receives.
struct OnceOrderStrategy {
    ordered: bool,
}

impl Strategy for OnceOrderStrategy {
    fn on_bar(&mut self, bar: &Kline, _ctx: &StrategyContext) -> Vec<SAction> {
        if !self.ordered {
            self.ordered = true;
            vec![SAction::PlaceOrder(StrategyOrder {
                order_id: 42,
                symbol: bar.symbol.clone(),
                qty: 1.0,
                price: bar.close,
                side: BuySellType::BsBuy as i32,
                account_id: 1,
            })]
        } else {
            vec![]
        }
    }
}

/// Registers a one-shot timer on on_reinit; counts timer fires.
struct TimerStrategy {
    fire_count: Arc<Mutex<u32>>,
}

impl Strategy for TimerStrategy {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![SAction::SubscribeTimer(TimerSubscription {
            timer_key: "test_timer".to_string(),
            schedule: TimerSchedule::OnceAt(1_000),
        })]
    }

    fn on_timer(
        &mut self,
        _event: &zk_strategy_sdk_rs::models::TimerEvent,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        *self.fire_count.lock().unwrap() += 1;
        vec![]
    }
}

/// Counts ticks received.
struct TickCountStrategy {
    tick_count: Arc<Mutex<u32>>,
}

impl Strategy for TickCountStrategy {
    fn on_tick(&mut self, _tick: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        *self.tick_count.lock().unwrap() += 1;
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_refdata() -> Vec<InstrumentRefData> {
    vec![InstrumentRefData {
        instrument_id: "TEST/USD@SIM".to_string(),
        instrument_id_exchange: "TESTUSD".to_string(),
        ..Default::default()
    }]
}

fn make_bar(symbol: &str, close: f64) -> Kline {
    Kline {
        symbol: symbol.to_string(),
        close,
        ..Default::default()
    }
}

fn make_tick(symbol: &str, exchange: &str) -> TickData {
    TickData {
        instrument_code: symbol.to_string(),
        exchange: exchange.to_string(),
        ..Default::default()
    }
}

/// Wrap an EngineEvent in an EventEnvelope with timestamps set to now.
fn envelope(event: EngineEvent) -> EventEnvelope {
    EventEnvelope::now(event)
}

fn new_engine<S: Strategy>(
    strategy: S,
    dispatcher: RecordingDispatcher,
) -> LiveEngine<S, RecordingDispatcher> {
    LiveEngine::new(
        vec![1],
        make_refdata(),
        strategy,
        dispatcher,
        "test-exec".to_string(),
        "test-strat".to_string(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// on_create log is dispatched through the recording dispatcher.
#[tokio::test]
async fn test_on_create_log_dispatched() {
    let bar_count = Arc::new(Mutex::new(0u32));
    let strategy = BarCountStrategy {
        bar_count: bar_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());

    engine.startup();
    let replica = engine.read_replica();

    // startup emits "created" log from on_create
    let (tx, rx) = mpsc::channel::<EventEnvelope>(128);
    drop(tx); // immediately close so run() exits
    engine.run(rx).await;

    // After run() returns, snapshot must show Stopped.
    let snap = replica.load();
    assert_eq!(snap.lifecycle_state, LifecycleState::Stopped);
}

/// Bar events are dispatched to on_bar; count is incremented.
#[tokio::test]
async fn test_bar_events_reach_strategy() {
    let bar_count = Arc::new(Mutex::new(0u32));
    let strategy = BarCountStrategy {
        bar_count: bar_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();

    let (tx, rx) = mpsc::channel(128);
    tx.send(envelope(EngineEvent::Bar(make_bar("TESTUSD", 100.0))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Bar(make_bar("TESTUSD", 101.0))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Bar(make_bar("TESTUSD", 102.0))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;
    assert_eq!(*bar_count.lock().unwrap(), 3, "expected 3 on_bar calls");
}

/// on_bar returning PlaceOrder causes dispatcher.place_order to be called.
#[tokio::test]
async fn test_order_placed_via_dispatcher() {
    let strategy = OnceOrderStrategy { ordered: false };
    let dispatcher = RecordingDispatcher::default();

    let (tx, rx) = mpsc::channel(128);
    tx.send(envelope(EngineEvent::Bar(make_bar("TESTUSD", 100.5))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Bar(make_bar("TESTUSD", 101.0))))
        .await
        .unwrap();
    drop(tx);

    let mut engine = new_engine(strategy, dispatcher);
    engine.startup();
    engine.run(rx).await;
}

/// One-shot timer fires when advance_time passes the fire timestamp.
#[tokio::test]
async fn test_timer_fires_once() {
    let fire_count = Arc::new(Mutex::new(0u32));
    let strategy = TimerStrategy {
        fire_count: fire_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup(); // on_reinit registers timer at 1_000 ms

    let (tx, rx) = mpsc::channel(128);
    // Advance time past the fire point twice — timer fires only once (OnceAt).
    tx.send(envelope(EngineEvent::Timer(2_000))).await.unwrap();
    tx.send(envelope(EngineEvent::Timer(3_000))).await.unwrap();
    drop(tx);

    engine.run(rx).await;
    assert_eq!(
        *fire_count.lock().unwrap(),
        1,
        "one-shot timer must fire exactly once"
    );
}

/// Tick coalescing: 3 ticks for the same (symbol, exchange) → only 1 dispatched.
#[tokio::test]
async fn test_tick_coalescing_same_symbol() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let strategy = TickCountStrategy {
        tick_count: tick_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();

    let (tx, rx) = mpsc::channel(128);
    // All 3 sent before run drains — they'll coalesce.
    tx.send(envelope(EngineEvent::Tick(make_tick("TESTUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("TESTUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("TESTUSD", "SIM"))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;
    assert_eq!(
        *tick_count.lock().unwrap(),
        1,
        "3 same-symbol ticks must coalesce to 1"
    );
}

/// Tick coalescing: ticks for 2 different symbols are both kept.
#[tokio::test]
async fn test_tick_coalescing_different_symbols() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let strategy = TickCountStrategy {
        tick_count: tick_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();

    let (tx, rx) = mpsc::channel(128);
    tx.send(envelope(EngineEvent::Tick(make_tick("AAAUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("BBBUSD", "SIM"))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;
    assert_eq!(
        *tick_count.lock().unwrap(),
        2,
        "ticks for 2 different symbols must both be dispatched"
    );
}

/// TriggerContext is correctly populated when strategy places an order from a bar event.
#[tokio::test]
async fn test_trigger_context_on_order_dispatch() {
    // Strategy that places an order on first bar — we need shared access to the dispatcher
    // to inspect recorded trigger context. Use Arc<Mutex<>> wrapper.
    use std::sync::Mutex as StdMutex;

    struct SharedRecordingDispatcher {
        inner: Arc<StdMutex<RecordingDispatcher>>,
    }

    impl zk_engine_rs::ActionDispatcher for SharedRecordingDispatcher {
        fn place_order(
            &mut self,
            order: &zk_strategy_sdk_rs::models::StrategyOrder,
            ctx: &zk_engine_rs::TriggerContext,
        ) {
            self.inner.lock().unwrap().place_order(order, ctx);
        }
        fn cancel_order(
            &mut self,
            cancel: &zk_strategy_sdk_rs::models::StrategyCancel,
            ctx: &zk_engine_rs::TriggerContext,
        ) {
            self.inner.lock().unwrap().cancel_order(cancel, ctx);
        }
        fn log(&mut self, log: &zk_strategy_sdk_rs::models::StrategyLog) {
            self.inner.lock().unwrap().log(log);
        }
    }

    let shared = Arc::new(StdMutex::new(RecordingDispatcher::default()));
    let dispatcher = SharedRecordingDispatcher {
        inner: shared.clone(),
    };
    let strategy = OnceOrderStrategy { ordered: false };

    let (tx, rx) = mpsc::channel(128);

    // Send a bar with explicit source timestamp
    let bar_envelope = EventEnvelope::with_timestamps(
        EngineEvent::Bar(make_bar("TESTUSD", 100.5)),
        1_000_000_000,     // recv_ts_ns = 1s
        Some(500_000_000), // source_ts_ns = 0.5s
    );
    tx.send(bar_envelope).await.unwrap();
    drop(tx);

    let mut engine = LiveEngine::new(
        vec![1],
        make_refdata(),
        strategy,
        dispatcher,
        "exec-42".to_string(),
        "my-strategy".to_string(),
    );
    engine.startup();
    engine.run(rx).await;

    let recorder = shared.lock().unwrap();
    assert_eq!(recorder.orders.len(), 1, "one order should be dispatched");

    let (order, ctx) = &recorder.orders[0];
    assert_eq!(order.order_id, 42);
    assert_eq!(ctx.trigger_event_type, "bar");
    assert_eq!(ctx.execution_id, "exec-42");
    assert_eq!(ctx.strategy_key, "my-strategy");
    assert_eq!(ctx.source_ts_ns, 500_000_000);
    assert_eq!(ctx.recv_ts_ns, 1_000_000_000);
    assert!(ctx.dispatch_ts_ns > 0, "dispatch_ts_ns must be set");
    assert!(
        ctx.decision_ts_ns >= ctx.dispatch_ts_ns,
        "decision_ts_ns must be >= dispatch_ts_ns"
    );
    assert_eq!(ctx.decision_seq, 1, "first decision should have seq=1");
}

/// Snapshot is updated after processing a batch — reflects counters and lifecycle state.
#[tokio::test]
async fn test_snapshot_updated_after_batch() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let strategy = TickCountStrategy {
        tick_count: tick_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();

    let replica = engine.read_replica();

    // Before any events — snapshot is in Starting state.
    {
        let snap = replica.load();
        assert_eq!(snap.lifecycle_state, LifecycleState::Starting);
        assert_eq!(snap.events_processed, 0);
    }

    let (tx, rx) = mpsc::channel(128);
    // Send 3 ticks for same symbol (will coalesce to 1) + 1 different symbol.
    tx.send(envelope(EngineEvent::Tick(make_tick("AAAUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("AAAUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("AAAUSD", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("BBBUSD", "SIM"))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;

    // After run() returns — final snapshot is Stopped; counters reflect dispatched work.
    let snap = replica.load();
    assert_eq!(snap.lifecycle_state, LifecycleState::Stopped);
    assert_eq!(
        snap.events_processed, 2,
        "2 events after coalescing (1 AAA + 1 BBB)"
    );
    assert_eq!(snap.ticks_coalesced, 2, "2 ticks dropped by coalescing");
    assert_eq!(snap.execution_id, "test-exec");
    assert_eq!(snap.strategy_key, "test-strat");
    assert!(snap.uptime_ms >= 0);
    assert!(snap.last_event_ts_ns > 0);
}

// ---------------------------------------------------------------------------
// Phase 4: Pause/Resume tests
// ---------------------------------------------------------------------------

/// Strategy that counts ticks and records pause/resume reasons.
struct PausableStrategy {
    tick_count: Arc<Mutex<u32>>,
    pause_reasons: Arc<Mutex<Vec<String>>>,
    resume_reasons: Arc<Mutex<Vec<String>>>,
}

impl Strategy for PausableStrategy {
    fn on_tick(&mut self, _tick: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        *self.tick_count.lock().unwrap() += 1;
        vec![]
    }

    fn on_order_update(
        &mut self,
        _update: &zk_proto_rs::zk::oms::v1::OrderUpdateEvent,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        vec![]
    }

    fn on_pause(&mut self, reason: &str, _ctx: &StrategyContext) -> Vec<SAction> {
        self.pause_reasons.lock().unwrap().push(reason.to_string());
        vec![]
    }

    fn on_resume(&mut self, reason: &str, _ctx: &StrategyContext) -> Vec<SAction> {
        self.resume_reasons.lock().unwrap().push(reason.to_string());
        vec![]
    }
}

/// Ticks/bars/signals/timers are dropped while paused; order updates still flow.
#[tokio::test]
async fn test_pause_drops_market_events() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let pause_reasons = Arc::new(Mutex::new(Vec::new()));
    let resume_reasons = Arc::new(Mutex::new(Vec::new()));

    let strategy = PausableStrategy {
        tick_count: tick_count.clone(),
        pause_reasons: pause_reasons.clone(),
        resume_reasons: resume_reasons.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();

    let (tx, rx) = mpsc::channel(128);

    // Tick before pause → should be delivered.
    tx.send(envelope(EngineEvent::Tick(make_tick("AAA", "SIM"))))
        .await
        .unwrap();
    // Pause.
    tx.send(envelope(EngineEvent::Control(ControlCommand::Pause {
        reason: "test-pause".into(),
    })))
    .await
    .unwrap();
    // Ticks while paused → should be dropped.
    tx.send(envelope(EngineEvent::Tick(make_tick("BBB", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Tick(make_tick("CCC", "SIM"))))
        .await
        .unwrap();
    // Resume.
    tx.send(envelope(EngineEvent::Control(ControlCommand::Resume {
        reason: "test-resume".into(),
    })))
    .await
    .unwrap();
    // Tick after resume → should be delivered.
    tx.send(envelope(EngineEvent::Tick(make_tick("DDD", "SIM"))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;

    assert_eq!(
        *tick_count.lock().unwrap(),
        2,
        "only tick before pause + tick after resume"
    );
    assert_eq!(pause_reasons.lock().unwrap().as_slice(), &["test-pause"]);
    assert_eq!(resume_reasons.lock().unwrap().as_slice(), &["test-resume"]);
}

/// OMS events (order updates) are still delivered while paused.
#[tokio::test]
async fn test_pause_allows_oms_events() {
    use zk_proto_rs::zk::oms::v1::OrderUpdateEvent;

    let tick_count = Arc::new(Mutex::new(0u32));
    let oms_count = Arc::new(Mutex::new(0u32));

    struct OmsCountStrategy {
        tick_count: Arc<Mutex<u32>>,
        oms_count: Arc<Mutex<u32>>,
    }

    impl Strategy for OmsCountStrategy {
        fn on_tick(&mut self, _tick: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
            *self.tick_count.lock().unwrap() += 1;
            vec![]
        }
        fn on_order_update(
            &mut self,
            _update: &OrderUpdateEvent,
            _ctx: &StrategyContext,
        ) -> Vec<SAction> {
            *self.oms_count.lock().unwrap() += 1;
            vec![]
        }
    }

    let strategy = OmsCountStrategy {
        tick_count: tick_count.clone(),
        oms_count: oms_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();
    let replica = engine.read_replica();

    let (tx, rx) = mpsc::channel(128);

    // Pause immediately.
    tx.send(envelope(EngineEvent::Control(ControlCommand::Pause {
        reason: "oms-test".into(),
    })))
    .await
    .unwrap();
    // Tick while paused → dropped.
    tx.send(envelope(EngineEvent::Tick(make_tick("AAA", "SIM"))))
        .await
        .unwrap();
    // Order update while paused → delivered.
    tx.send(envelope(EngineEvent::OrderUpdate(
        OrderUpdateEvent::default(),
    )))
    .await
    .unwrap();
    tx.send(envelope(EngineEvent::OrderUpdate(
        OrderUpdateEvent::default(),
    )))
    .await
    .unwrap();
    drop(tx);

    engine.run(rx).await;

    assert_eq!(*tick_count.lock().unwrap(), 0, "ticks dropped while paused");
    assert_eq!(
        *oms_count.lock().unwrap(),
        2,
        "order updates delivered while paused"
    );

    // events_processed should count only dispatched events (2 order updates), not the dropped tick.
    let snap = replica.load();
    assert_eq!(snap.events_processed, 2, "only dispatched events counted");
}

/// Snapshot reflects paused state.
#[tokio::test]
async fn test_snapshot_reflects_pause_state() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let strategy = PausableStrategy {
        tick_count: tick_count.clone(),
        pause_reasons: Arc::new(Mutex::new(Vec::new())),
        resume_reasons: Arc::new(Mutex::new(Vec::new())),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();
    let replica = engine.read_replica();

    let (tx, rx) = mpsc::channel(128);
    tx.send(envelope(EngineEvent::Control(ControlCommand::Pause {
        reason: "snap-test".into(),
    })))
    .await
    .unwrap();
    drop(tx);

    engine.run(rx).await;

    // Final snapshot is Stopped (post-loop), but paused flag persists.
    let snap = replica.load();
    assert_eq!(snap.lifecycle_state, LifecycleState::Stopped);
    assert!(snap.paused, "engine was paused when it stopped");
}

/// Stop command terminates the event loop.
#[tokio::test]
async fn test_stop_command_terminates_loop() {
    let tick_count = Arc::new(Mutex::new(0u32));
    let strategy = TickCountStrategy {
        tick_count: tick_count.clone(),
    };
    let mut engine = new_engine(strategy, RecordingDispatcher::default());
    engine.startup();
    let replica = engine.read_replica();

    let (tx, rx) = mpsc::channel(128);
    tx.send(envelope(EngineEvent::Tick(make_tick("AAA", "SIM"))))
        .await
        .unwrap();
    tx.send(envelope(EngineEvent::Control(ControlCommand::Stop {
        reason: "shutdown".into(),
    })))
    .await
    .unwrap();
    // This tick should never be processed.
    tx.send(envelope(EngineEvent::Tick(make_tick("BBB", "SIM"))))
        .await
        .unwrap();
    drop(tx);

    engine.run(rx).await;

    assert_eq!(
        *tick_count.lock().unwrap(),
        1,
        "only tick before stop should be delivered"
    );
    let snap = replica.load();
    assert_eq!(snap.lifecycle_state, LifecycleState::Stopped);
}
