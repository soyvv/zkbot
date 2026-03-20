//! Runtime snapshot for the engine event loop.
//!
//! `EngineSnapshot` is a point-in-time view of engine state published via
//! `ArcSwap` (same pattern as `zk-oms-svc::oms_actor::ReadReplica`).
//! The hot loop stores a new snapshot once per batch iteration; gRPC query
//! handlers read it lock-free via `snapshot.load()`.

use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;

use crate::latency::LatencySample;

/// Lifecycle state of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Starting,
    Running,
    Pausing,
    Paused,
    Resuming,
    Degraded,
    Stopping,
    Stopped,
    Fenced,
    Failed,
}

impl LifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Pausing => "PAUSING",
            Self::Paused => "PAUSED",
            Self::Resuming => "RESUMING",
            Self::Degraded => "DEGRADED",
            Self::Stopping => "STOPPING",
            Self::Stopped => "STOPPED",
            Self::Fenced => "FENCED",
            Self::Failed => "FAILED",
        }
    }
}

/// Point-in-time snapshot of engine state, published once per batch iteration.
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    // ── Identity ──
    pub execution_id: String,
    pub strategy_key: String,

    // ── Lifecycle ──
    pub lifecycle_state: LifecycleState,
    pub paused: bool,
    pub degraded_reason: Option<String>,

    // ── Counters ──
    pub events_processed: u64,
    pub ticks_coalesced: u64,
    pub orders_dispatched: u64,
    pub decision_seq: u64,

    // ── Timing ──
    /// Uptime in milliseconds since engine start.
    pub uptime_ms: i64,
    /// Wall-clock nanosecond timestamp of the last event processed.
    pub last_event_ts_ns: i64,

    // ── Latency ──
    pub avg_queue_wait_ns: i64,
    pub avg_decision_ns: i64,
    pub max_queue_wait_ns: i64,
    pub max_decision_ns: i64,
    pub action_event_count: u64,
    pub recent_latency_samples: Vec<LatencySample>,
}

impl EngineSnapshot {
    /// Initial snapshot before the engine starts processing events.
    pub fn initial(execution_id: &str, strategy_key: &str) -> Self {
        Self {
            execution_id: execution_id.to_string(),
            strategy_key: strategy_key.to_string(),
            lifecycle_state: LifecycleState::Starting,
            paused: false,
            degraded_reason: None,
            events_processed: 0,
            ticks_coalesced: 0,
            orders_dispatched: 0,
            decision_seq: 0,
            uptime_ms: 0,
            last_event_ts_ns: 0,
            avg_queue_wait_ns: 0,
            avg_decision_ns: 0,
            max_queue_wait_ns: 0,
            max_decision_ns: 0,
            action_event_count: 0,
            recent_latency_samples: Vec::new(),
        }
    }
}

/// Atomic read replica — shared between the engine hot loop and gRPC query handlers.
pub type EngineReadReplica = Arc<ArcSwap<EngineSnapshot>>;

/// Create a new read replica initialized with a starting snapshot.
pub fn new_read_replica(execution_id: &str, strategy_key: &str) -> EngineReadReplica {
    Arc::new(ArcSwap::from_pointee(EngineSnapshot::initial(
        execution_id,
        strategy_key,
    )))
}

/// Helper to compute uptime from an `Instant` recorded at engine start.
#[inline]
pub fn uptime_ms_since(start: Instant) -> i64 {
    start.elapsed().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_snapshot() {
        let snap = EngineSnapshot::initial("exec-1", "strat-a");
        assert_eq!(snap.lifecycle_state, LifecycleState::Starting);
        assert_eq!(snap.execution_id, "exec-1");
        assert_eq!(snap.strategy_key, "strat-a");
        assert_eq!(snap.events_processed, 0);
        assert!(!snap.paused);
    }

    #[test]
    fn test_read_replica_lock_free() {
        let replica = new_read_replica("exec-1", "strat-a");

        // Writer publishes a new snapshot.
        let mut snap = EngineSnapshot::initial("exec-1", "strat-a");
        snap.lifecycle_state = LifecycleState::Running;
        snap.events_processed = 42;
        replica.store(Arc::new(snap));

        // Reader loads the latest.
        let loaded = replica.load();
        assert_eq!(loaded.lifecycle_state, LifecycleState::Running);
        assert_eq!(loaded.events_processed, 42);
    }

    #[test]
    fn test_lifecycle_state_as_str() {
        assert_eq!(LifecycleState::Running.as_str(), "RUNNING");
        assert_eq!(LifecycleState::Paused.as_str(), "PAUSED");
        assert_eq!(LifecycleState::Fenced.as_str(), "FENCED");
    }
}
