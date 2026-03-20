//! Hot-path timestamp collection + periodic latency metric publishing.
//!
//! # Design
//!
//! The writer loop only does cheap timestamp captures in the hot path:
//!   - `system_time_ns()` at writer dequeue / after core / before GW enqueue
//!     → `t2_ns`, `tw_core_ns`, `writer_dispatch_ts`
//!   - `system_time_ns()` before/after GW gRPC send  → `t3_ns`, `t3r_ns`
//!   - `system_time_ns()` on GW report arrival        → `t6_ns`
//!   - field read from `OrderUpdateEvent.timestamp`   → `t7_ns`
//!
//! Every `metrics_interval` seconds the flush task drains `complete` records,
//! builds `LatencyMetricBatch` (proto), and publishes to NATS.
//!
//! # Timeline tags
//!
//! | Tag  | Source             | What it is                        |
//! |------|--------------------|-----------------------------------|
//! | `t0` | OrderRequest.timestamp × 1e6 | Client order-creation clock  |
//! | `t1` | system_time_ns() at gRPC handler entry | OMS receives request |
//! | `t2` | system_time_ns() at writer dequeue      | Writer starts command |
//! | `tw_core` | system_time_ns() after core.process_message() | Core done |
//! | `tw_dispatch` | system_time_ns() before gw-exec enqueue | Writer hands off |
//! | `t3` | system_time_ns() before send_order() | OMS dispatches to GW   |
//! | `t3r`| system_time_ns() after send_order()  | gRPC ACK returned      |
//! | `t4` | report.gw_received_at_ns             | GW handler entry       |
//! | `t5` | report.update_timestamp × 1e6        | GW publishes BOOKED    |
//! | `t6` | system_time_ns() on NATS arrival     | OMS receives report    |
//! | `t7` | event.timestamp × 1e6               | OMS publishes event    |
//!
//! `t3 - t1` = oms_through: OMS receive-to-actual-send time.
//! Bench adds `t8` (client receive) locally.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message as ProstMessage;
use tracing::warn;
use zk_proto_rs::zk::common::v1::{LatencyMetric, LatencyMetricBatch};

/// Cheap per-order record stored in the hot path — raw nanosecond timestamps only.
#[derive(Debug, Default)]
pub struct TimestampRecord {
    pub order_id: i64,
    pub t0_ns: i64,      // OrderRequest.timestamp × 1_000_000
    pub t1_ns: i64,      // system_time_ns() at OMS gRPC handler entry (0 for batch)
    pub t2_ns: i64,      // system_time_ns() at writer dequeue/start
    pub tw_core_ns: i64, // system_time_ns() right after core.process_message()
    pub t3_ns: i64,      // before gw_pool.send_order()
    pub t3r_ns: i64,     // after  gw_pool.send_order() returns
    pub t4_ns: i64,      // report.gw_received_at_ns   (filled on GW report)
    pub t5_ns: i64,      // report.update_timestamp × 1e6 (filled on GW report)
    pub t6_ns: i64,      // system_time_ns() on NATS arrival  (filled on GW report)
    pub t7_ns: i64,      // event.timestamp × 1e6             (filled on PublishOrderUpdate)
    /// writer dispatch to gw executor queue (for gw_exec_queue_wait = t_gw_dequeue - writer_dispatch_ts)
    pub writer_dispatch_ts: i64,
    /// system_time_ns() when the gw worker dequeues the action (before client lookup)
    pub gw_exec_dequeue_ns: i64,
}

/// Ring-buffer tracker. Lives entirely inside the OMS writer task — no locking needed.
pub struct LatencyTracker {
    /// In-flight: have t0–t3r, waiting for GW report.
    pending: HashMap<i64, TimestampRecord>,
    /// Fully populated records (have all t0–t7).
    complete: VecDeque<TimestampRecord>,
    max_pending: usize,
    max_complete: usize,
}

impl LatencyTracker {
    pub fn new(max_pending: usize, max_complete: usize) -> Self {
        Self {
            pending: HashMap::with_capacity(max_pending.min(4096)),
            complete: VecDeque::with_capacity(max_complete.min(4096)),
            max_pending,
            max_complete,
        }
    }

    /// Called just after `gw_pool.send_order()` returns.
    /// `order_created_at_ms` is `Order.created_at` (ms) from `OmsAction::SendOrderToGw`.
    /// `t1_ns` is the OMS gRPC handler entry timestamp (0 for batch orders).
    #[inline]
    pub fn record_order_sent(
        &mut self,
        order_id: i64,
        order_created_at_ms: i64,
        t1_ns: i64,
        t2_ns: i64,
        tw_core_ns: i64,
        t3_ns: i64,
        t3r_ns: i64,
        writer_dispatch_ts: i64,
        gw_exec_dequeue_ns: i64,
    ) {
        if self.pending.len() >= self.max_pending {
            return;
        }
        self.pending.insert(
            order_id,
            TimestampRecord {
                order_id,
                t0_ns: order_created_at_ms * 1_000_000,
                t1_ns,
                t2_ns,
                tw_core_ns,
                t3_ns,
                t3r_ns,
                writer_dispatch_ts,
                gw_exec_dequeue_ns,
                ..Default::default()
            },
        );
    }

    /// Called after `PublishOrderUpdate` action is dispatched for a GW report.
    /// Completes the record and moves it to `complete`.
    #[inline]
    pub fn record_report_published(
        &mut self,
        order_id: i64,
        t4_ns: i64,
        t5_ns: i64,
        t6_ns: i64,
        t7_ns: i64,
    ) {
        let Some(mut rec) = self.pending.remove(&order_id) else {
            // Order was placed before this OMS session or evicted — skip.
            return;
        };
        rec.t4_ns = t4_ns;
        rec.t5_ns = t5_ns;
        rec.t6_ns = t6_ns;
        rec.t7_ns = t7_ns;

        if self.complete.len() >= self.max_complete {
            self.complete.pop_front(); // evict oldest
        }
        self.complete.push_back(rec);
    }

    /// Drain all completed records.  Called by the flush task — off hot path.
    pub fn drain_complete(&mut self) -> Vec<TimestampRecord> {
        self.complete.drain(..).collect()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn complete_count(&self) -> usize {
        self.complete.len()
    }
}

/// Monotonic-ish wall-clock nanoseconds.  One `SystemTime::now()` call.
#[inline]
pub fn system_time_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── Async latency feedback from executor workers ────────────────────────────

/// Events sent from executor workers back to the writer loop's `LatencyTracker`.
///
/// Timestamps are captured at the correct wall-clock moment in the worker;
/// only the tracker bookkeeping is deferred via the feedback channel.
pub enum LatencyEvent {
    /// Gateway worker completed a `SendOrder` — carries t3/t3r captured around
    /// the actual gRPC call.
    OrderSent {
        order_id: i64,
        order_created_at: i64,
        t1_ns: i64,
        t2_ns: i64,
        tw_core_ns: i64,
        t3_ns: i64,
        t3r_ns: i64,
        writer_dispatch_ts: i64,
        gw_exec_dequeue_ns: i64,
    },
    /// Publish worker completed an `OrderUpdateEvent` publish for a GW report —
    /// carries t7 captured as `system_time_ns()` immediately after NATS publish.
    ReportPublished {
        order_id: i64,
        t4_ns: i64,
        t5_ns: i64,
        t6_ns: i64,
        t7_ns: i64,
    },
}

/// Build `LatencyMetricBatch` from a batch of completed records and publish to NATS.
/// Called entirely off the hot path (in the periodic flush tick).
pub async fn publish_latency_batch(
    nats: &async_nats::Client,
    oms_id: &str,
    records: Vec<TimestampRecord>,
    publish_ts_ns: i64,
) {
    let metrics: Vec<LatencyMetric> = records
        .iter()
        .map(|r| {
            let mut ts = HashMap::new();
            ts.insert("t0".into(), r.t0_ns);
            ts.insert("t1".into(), r.t1_ns);
            ts.insert("t2".into(), r.t2_ns);
            ts.insert("tw_core".into(), r.tw_core_ns);
            ts.insert("t3".into(), r.t3_ns);
            ts.insert("t3r".into(), r.t3r_ns);
            ts.insert("t4".into(), r.t4_ns);
            ts.insert("t5".into(), r.t5_ns);
            ts.insert("t6".into(), r.t6_ns);
            ts.insert("t7".into(), r.t7_ns);
            ts.insert("tw_dispatch".into(), r.writer_dispatch_ts);
            ts.insert("t_gw_dequeue".into(), r.gw_exec_dequeue_ns);
            LatencyMetric {
                tagged_timestamps: ts,
                order_id: r.order_id,
                source_id: oms_id.to_string(),
            }
        })
        .collect();

    let batch = LatencyMetricBatch {
        metrics,
        publisher_id: oms_id.to_string(),
        publish_ts_ns,
    };

    let subject = format!("zk.oms.{oms_id}.metrics.latency");
    let payload = batch.encode_to_vec();
    if let Err(e) = nats.publish(subject.clone(), payload.into()).await {
        warn!(subject, error = %e, "latency batch publish failed");
    }
}
