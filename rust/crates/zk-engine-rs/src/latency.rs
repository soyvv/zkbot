//! Hot-path latency tracking for the engine event loop.
//!
//! `EngineLatencyTracker` lives inside the `LiveEngine` hot loop — no locking,
//! no allocation per event.  It accumulates lightweight counters and timestamp
//! deltas that the service layer can periodically read (via the `EngineSnapshot`)
//! and publish as metrics or latency batches.
//!
//! Design mirrors `zk-oms-svc/src/latency.rs`: cheap inline capture in the hot
//! path, aggregation and publishing happen off the hot path.

use std::collections::VecDeque;

use crate::engine_event::TriggerContext;

// ---------------------------------------------------------------------------
// Per-order latency sample (kept in a small ring buffer for query API)
// ---------------------------------------------------------------------------

/// Compact latency sample for a single order decision.
/// Stored in the ring buffer; exposed through `EngineSnapshot` for debugging.
#[derive(Debug, Clone)]
pub struct LatencySample {
    /// Queue wait: dispatch_ts - enqueue_ts (nanoseconds).
    pub queue_wait_ns: i64,
    /// Strategy decision time: decision_ts - dispatch_ts (nanoseconds).
    pub decision_ns: i64,
    /// Full engine path: decision_ts - recv_ts (nanoseconds).
    pub recv_to_decision_ns: i64,
    /// Event type that triggered this sample.
    pub trigger_event_type: &'static str,
    /// Instrument code (empty for non-instrument events).
    pub instrument_code: String,
    /// Wall-clock timestamp of the sample (nanoseconds).
    pub sample_ts_ns: i64,
}

impl LatencySample {
    /// Build a sample from a completed `TriggerContext`.
    ///
    /// `decision_ns` uses monotonic `Instant` when available (clock-jump safe),
    /// falling back to wall-clock delta for synthetic/lifecycle contexts.
    pub fn from_trigger_context(ctx: &TriggerContext) -> Self {
        let decision_ns = monotonic_decision_ns(ctx);
        Self {
            queue_wait_ns: ctx.dispatch_ts_ns.saturating_sub(ctx.enqueue_ts_ns),
            decision_ns,
            recv_to_decision_ns: ctx.decision_ts_ns.saturating_sub(ctx.recv_ts_ns),
            trigger_event_type: ctx.trigger_event_type,
            instrument_code: ctx.instrument_code.clone(),
            sample_ts_ns: ctx.decision_ts_ns,
        }
    }
}

/// Compute strategy decision duration preferring monotonic `Instant` over wall-clock.
#[inline]
fn monotonic_decision_ns(ctx: &TriggerContext) -> i64 {
    match (ctx.dispatch_instant, ctx.decision_instant) {
        (Some(d), Some(dec)) => dec.saturating_duration_since(d).as_nanos() as i64,
        _ => ctx.decision_ts_ns.saturating_sub(ctx.dispatch_ts_ns),
    }
}

// ---------------------------------------------------------------------------
// Aggregate counters
// ---------------------------------------------------------------------------

/// Running latency statistics maintained by the engine hot loop.
///
/// All updates are `#[inline]` and branch-free where possible.
/// No heap allocation per event — only the ring buffer grows (bounded).
#[derive(Debug)]
pub struct EngineLatencyTracker {
    // ── Counters ──
    /// Total events processed (all types).
    pub events_processed: u64,
    /// Total tick events coalesced (dropped duplicates).
    pub ticks_coalesced: u64,
    /// Total order actions dispatched.
    pub orders_dispatched: u64,

    // ── Running sums for averages (nanoseconds) ──
    /// Sum of queue_wait_ns across all events with actions.
    sum_queue_wait_ns: i64,
    /// Sum of decision_ns across all events with actions.
    sum_decision_ns: i64,
    /// Count of events that produced actions (denominator for averages).
    action_event_count: u64,

    // ── Extremes ──
    /// Max queue wait seen (nanoseconds).
    pub max_queue_wait_ns: i64,
    /// Max strategy decision time seen (nanoseconds).
    pub max_decision_ns: i64,

    // ── Recent samples ring buffer ──
    samples: VecDeque<LatencySample>,
    max_samples: usize,
}

impl EngineLatencyTracker {
    pub fn new(max_samples: usize) -> Self {
        Self {
            events_processed: 0,
            ticks_coalesced: 0,
            orders_dispatched: 0,
            sum_queue_wait_ns: 0,
            sum_decision_ns: 0,
            action_event_count: 0,
            max_queue_wait_ns: 0,
            max_decision_ns: 0,
            samples: VecDeque::with_capacity(max_samples.min(256)),
            max_samples,
        }
    }

    /// Record that a batch was processed. Called once per batch in the hot loop.
    #[inline]
    pub fn record_batch(&mut self, batch_size: usize, coalesced_count: usize) {
        self.events_processed += batch_size as u64;
        self.ticks_coalesced += coalesced_count as u64;
    }

    /// Record an event that produced actions (orders/cancels).
    /// Called after `dispatch_actions` if the event had non-empty actions.
    #[inline]
    pub fn record_action_event(&mut self, ctx: &TriggerContext, order_count: usize) {
        self.orders_dispatched += order_count as u64;

        let queue_wait = ctx.dispatch_ts_ns.saturating_sub(ctx.enqueue_ts_ns);
        let decision = monotonic_decision_ns(ctx);

        self.sum_queue_wait_ns += queue_wait;
        self.sum_decision_ns += decision;
        self.action_event_count += 1;

        if queue_wait > self.max_queue_wait_ns {
            self.max_queue_wait_ns = queue_wait;
        }
        if decision > self.max_decision_ns {
            self.max_decision_ns = decision;
        }

        // Push sample into ring buffer.
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples
            .push_back(LatencySample::from_trigger_context(ctx));
    }

    /// Average queue wait across action-producing events (nanoseconds), or 0 if none.
    pub fn avg_queue_wait_ns(&self) -> i64 {
        if self.action_event_count == 0 {
            0
        } else {
            self.sum_queue_wait_ns / self.action_event_count as i64
        }
    }

    /// Average strategy decision time across action-producing events (nanoseconds), or 0.
    pub fn avg_decision_ns(&self) -> i64 {
        if self.action_event_count == 0 {
            0
        } else {
            self.sum_decision_ns / self.action_event_count as i64
        }
    }

    /// Recent latency samples (for query API / debugging).
    pub fn recent_samples(&self) -> Vec<LatencySample> {
        self.samples.iter().cloned().collect()
    }

    /// Number of events that produced actions.
    pub fn action_event_count(&self) -> u64 {
        self.action_event_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_event::TriggerContext;

    fn make_ctx(enqueue: i64, dispatch: i64, decision: i64) -> TriggerContext {
        TriggerContext {
            trigger_event_type: "tick",
            instrument_code: "BTCUSDT".to_string(),
            source_ts_ns: 0,
            recv_ts_ns: enqueue - 1000,
            enqueue_ts_ns: enqueue,
            dispatch_ts_ns: dispatch,
            decision_ts_ns: decision,
            execution_id: String::new(),
            strategy_key: String::new(),
            decision_seq: 1,
            dispatch_instant: None, // wall-clock fallback path
            decision_instant: None,
        }
    }

    #[test]
    fn test_tracker_basic_counters() {
        let mut tracker = EngineLatencyTracker::new(64);

        tracker.record_batch(10, 3);
        assert_eq!(tracker.events_processed, 10);
        assert_eq!(tracker.ticks_coalesced, 3);

        let ctx = make_ctx(1_000_000, 1_100_000, 1_200_000);
        tracker.record_action_event(&ctx, 2);

        assert_eq!(tracker.orders_dispatched, 2);
        assert_eq!(tracker.action_event_count(), 1);
        assert_eq!(tracker.max_queue_wait_ns, 100_000); // dispatch - enqueue
        assert_eq!(tracker.max_decision_ns, 100_000); // decision - dispatch
    }

    #[test]
    fn test_tracker_averages() {
        let mut tracker = EngineLatencyTracker::new(64);

        // Event 1: queue_wait=100k, decision=200k
        let ctx1 = make_ctx(1_000_000, 1_100_000, 1_300_000);
        tracker.record_action_event(&ctx1, 1);

        // Event 2: queue_wait=300k, decision=400k
        let ctx2 = make_ctx(2_000_000, 2_300_000, 2_700_000);
        tracker.record_action_event(&ctx2, 1);

        assert_eq!(tracker.avg_queue_wait_ns(), 200_000); // (100k + 300k) / 2
        assert_eq!(tracker.avg_decision_ns(), 300_000); // (200k + 400k) / 2
        assert_eq!(tracker.max_queue_wait_ns, 300_000);
        assert_eq!(tracker.max_decision_ns, 400_000);
    }

    #[test]
    fn test_ring_buffer_eviction() {
        let mut tracker = EngineLatencyTracker::new(2);

        for i in 0..5 {
            let ctx = make_ctx(i * 1000, i * 1000 + 100, i * 1000 + 200);
            tracker.record_action_event(&ctx, 1);
        }

        let samples = tracker.recent_samples();
        assert_eq!(
            samples.len(),
            2,
            "ring buffer should keep at most 2 samples"
        );
        // Should be the last 2 (i=3 and i=4)
        assert_eq!(samples[0].sample_ts_ns, 3200);
        assert_eq!(samples[1].sample_ts_ns, 4200);
    }
}
