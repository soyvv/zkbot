/// `zk-engine-rs` — live event loop, event batching/coalescing, timer manager,
/// and strategy dispatch.
///
/// Owns event orchestration; strategy execution is pluggable
/// (Rust-native via the `Strategy` trait, Python-embedded via `zk-pyo3-rs`).
///
/// # Architecture
///
/// ```text
/// ┌──────────────────────────────────────────────────────────┐
/// │  Event Sources                                           │
/// │  NATS ticks / bars / order updates / position updates    │
/// │  TimerClock (1 Hz pulse)                                 │
/// └──────────────────┬───────────────────────────────────────┘
/// │  mpsc::Sender<EngineEvent> (cap 128, mirrors asyncio.Queue)
/// ▼
/// ┌──────────────────────────────────────────────────────────┐
/// │  LiveEngine                                              │
/// │  • coalesce ticks (deduplicate per symbol)               │
/// │  • dispatch to StrategyRunner callbacks                  │
/// │  • process SActions → ActionDispatcher / TimerManager    │
/// └──────────────────────────────────────────────────────────┘
/// ```
pub mod action_dispatcher;
pub mod engine_event;
pub mod latency;
pub mod live_engine;
pub mod snapshot;

pub use action_dispatcher::{ActionDispatcher, NoopDispatcher, RecordingDispatcher};
pub use engine_event::{
    coalesce_ticks, system_time_ns, ControlCommand, EngineEvent, EventEnvelope, TriggerContext,
};
pub use latency::{EngineLatencyTracker, LatencySample};
pub use live_engine::{run_timer_clock, EngineConfig, LiveEngine};
pub use snapshot::{new_read_replica, EngineReadReplica, EngineSnapshot, LifecycleState};
