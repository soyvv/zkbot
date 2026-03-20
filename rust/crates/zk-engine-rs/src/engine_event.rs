/// `EngineEvent` — unified event type flowing through the live engine's channel.
///
/// Mirrors the asyncio.Queue payload types from Python `StrategyRealtimeEngine`.
/// All NATS subscription callbacks and the timer clock funnel events into a
/// `tokio::sync::mpsc::Sender<EventEnvelope>` so the engine loop processes
/// them serially, one at a time (no concurrent strategy calls).
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use zk_proto_rs::zk::{
    oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{Kline, RealtimeSignal, TickData},
};

#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Market tick (bid/ask/last). Multiple ticks for the same symbol within
    /// a batch are coalesced: only the last one is dispatched to the strategy.
    Tick(TickData),
    /// OHLCV bar (1-minute or any kline type).
    Bar(Kline),
    /// Order fill, rejection, or status change from OMS.
    OrderUpdate(OrderUpdateEvent),
    /// Asset inventory (cash/spot) update from OMS.
    BalanceUpdate(BalanceUpdateEvent),
    /// Instrument exposure (derivatives) update from OMS.
    PositionUpdate(PositionUpdateEvent),
    /// External real-time signal.
    Signal(RealtimeSignal),
    /// Timer clock pulse. The engine passes `now_ms` to `StrategyRunner::advance_time`,
    /// which drains all due timers and calls `on_timer` for each.
    Timer(i64),
    /// Control command from gRPC or supervision layer.
    Control(ControlCommand),
}

/// Control commands injected via the engine's event channel.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    Pause { reason: String },
    Resume { reason: String },
    Stop { reason: String },
}

/// Envelope wrapping an `EngineEvent` with ingestion timestamps for latency
/// observability. Producers stamp `recv_ts_ns` at NATS callback entry and
/// `enqueue_ts_ns` just before `mpsc::send`.
#[derive(Debug, Clone)]
pub struct EventEnvelope {
    pub event: EngineEvent,
    /// Wall-clock nanoseconds when the engine process first received this event
    /// (e.g., NATS subscription callback entry).
    pub recv_ts_ns: i64,
    /// Source/exchange timestamp from the RTMD payload, if available.
    pub source_ts_ns: Option<i64>,
    /// Wall-clock nanoseconds when the event was pushed into the mpsc channel.
    pub enqueue_ts_ns: i64,
}

impl EventEnvelope {
    /// Create an envelope with `recv_ts_ns` and `enqueue_ts_ns` both set to now.
    /// Useful for internally generated events (timers, control commands) where
    /// ingestion and enqueue are effectively simultaneous.
    pub fn now(event: EngineEvent) -> Self {
        let ts = system_time_ns();
        Self {
            event,
            recv_ts_ns: ts,
            source_ts_ns: None,
            enqueue_ts_ns: ts,
        }
    }

    /// Create an envelope with explicit timestamps.
    pub fn with_timestamps(event: EngineEvent, recv_ts_ns: i64, source_ts_ns: Option<i64>) -> Self {
        Self {
            event,
            recv_ts_ns,
            source_ts_ns,
            enqueue_ts_ns: system_time_ns(),
        }
    }
}

/// Engine-local trigger context built from an `EventEnvelope` during dispatch.
/// Carried through to `ActionDispatcher` so orders include full correlation metadata.
///
/// Maps 1:1 to `zk.common.v1.TriggerContext` proto for OMS propagation.
#[derive(Debug, Clone)]
pub struct TriggerContext {
    /// "tick", "bar", "signal", "timer", "order_update", "balance_update", "position_update"
    pub trigger_event_type: &'static str,
    /// Instrument code from the triggering event (empty for non-instrument events).
    pub instrument_code: String,
    /// RTMD/source timestamp if available (nanoseconds).
    pub source_ts_ns: i64,
    /// Engine receive timestamp (nanoseconds).
    pub recv_ts_ns: i64,
    /// Queue admission timestamp (nanoseconds).
    pub enqueue_ts_ns: i64,
    /// Strategy dispatch start timestamp (nanoseconds).
    pub dispatch_ts_ns: i64,
    /// Strategy callback return timestamp (nanoseconds), set after process_event.
    pub decision_ts_ns: i64,
    /// From engine config — identifies the execution run.
    pub execution_id: String,
    /// From engine config.
    pub strategy_key: String,
    /// Monotonic per-engine decision counter, incremented per event that produces actions.
    pub decision_seq: u64,
    /// Monotonic dispatch instant — engine-internal, not serialized to proto.
    /// Used for clock-jump-safe latency computation.
    pub dispatch_instant: Option<Instant>,
    /// Monotonic decision instant — engine-internal, not serialized to proto.
    pub decision_instant: Option<Instant>,
}

impl TriggerContext {
    /// Build a `TriggerContext` from an envelope at dispatch time.
    pub fn from_envelope(envelope: &EventEnvelope, execution_id: &str, strategy_key: &str) -> Self {
        let (event_type, instrument_code) = match &envelope.event {
            EngineEvent::Tick(t) => ("tick", t.instrument_code.clone()),
            EngineEvent::Bar(b) => ("bar", b.symbol.clone()),
            EngineEvent::Signal(s) => ("signal", s.instrument.clone()),
            EngineEvent::Timer(_) => ("timer", String::new()),
            EngineEvent::OrderUpdate(_) => ("order_update", String::new()),
            EngineEvent::BalanceUpdate(_) => ("balance_update", String::new()),
            EngineEvent::PositionUpdate(_) => ("position_update", String::new()),
            EngineEvent::Control(_) => ("control", String::new()),
        };
        Self {
            trigger_event_type: event_type,
            instrument_code,
            source_ts_ns: envelope.source_ts_ns.unwrap_or(0),
            recv_ts_ns: envelope.recv_ts_ns,
            enqueue_ts_ns: envelope.enqueue_ts_ns,
            dispatch_ts_ns: system_time_ns(),
            decision_ts_ns: 0,
            execution_id: execution_id.to_string(),
            strategy_key: strategy_key.to_string(),
            decision_seq: 0,
            dispatch_instant: None, // Set by LiveEngine::run() after construction.
            decision_instant: None,
        }
    }

    /// Convert to the proto `TriggerContext` for OMS propagation.
    pub fn to_proto(
        &self,
        oms_submit_ts_ns: i64,
        client_order_id: &str,
    ) -> zk_proto_rs::zk::common::v1::TriggerContext {
        zk_proto_rs::zk::common::v1::TriggerContext {
            execution_id: self.execution_id.clone(),
            strategy_key: self.strategy_key.clone(),
            trigger_event_type: self.trigger_event_type.to_string(),
            instrument_code: self.instrument_code.clone(),
            source_ts_ns: self.source_ts_ns,
            recv_ts_ns: self.recv_ts_ns,
            enqueue_ts_ns: self.enqueue_ts_ns,
            dispatch_ts_ns: self.dispatch_ts_ns,
            decision_ts_ns: self.decision_ts_ns,
            decision_seq: self.decision_seq,
            oms_submit_ts_ns,
            client_order_id: client_order_id.to_string(),
        }
    }
}

/// Deduplicate ticks: for each `(symbol, exchange)` pair, keep only the last.
/// All non-tick events are passed through in original order.
///
/// Mirrors Python engine's coalescing in `run_strategy()`:
/// `deduplicated_ticks: dict[(symbol, exchange) -> TickData]`
pub fn coalesce_ticks(events: Vec<EventEnvelope>) -> Vec<EventEnvelope> {
    // Pass 1: record the index of the *last* occurrence of each (symbol, exchange) pair.
    let mut last_seen: HashMap<(String, String), usize> = HashMap::new();
    for (i, env) in events.iter().enumerate() {
        if let EngineEvent::Tick(t) = &env.event {
            last_seen.insert((t.instrument_code.clone(), t.exchange.clone()), i);
        }
    }

    // Convert to a HashSet of keep-indices so the filter pass needs no string allocation.
    let keep: std::collections::HashSet<usize> = last_seen.into_values().collect();

    // Pass 2: emit non-tick events unconditionally; emit ticks only at their last index.
    events
        .into_iter()
        .enumerate()
        .filter(|(i, env)| !matches!(env.event, EngineEvent::Tick(_)) || keep.contains(i))
        .map(|(_, env)| env)
        .collect()
}

/// Returns the number of events dropped during coalescing (batch_size_before - batch_size_after).
pub fn count_coalesced(before_len: usize, after_len: usize) -> usize {
    before_len.saturating_sub(after_len)
}

/// Monotonic-ish wall-clock nanoseconds. Matches `zk-oms-svc/src/latency.rs::system_time_ns()`.
#[inline]
pub fn system_time_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}
