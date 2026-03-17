//! Publish executor pool — sharded by order_id or account_id.
//!
//! Each shard has one worker task that sequentially publishes pre-built
//! proto events to NATS. Publishing is best-effort: full queues drop
//! actions with a warning log.

use tokio::sync::mpsc;
use tracing::warn;

use crate::executor::{DispatchResult, ShardedPool};
use crate::latency::{system_time_ns, LatencyEvent};
use crate::nats_handler::NatsPublisher;

use zk_proto_rs::zk::oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent};

// ── Action types ────────────────────────────────────────────────────────────

/// Pre-materialized publish action. Proto events are already built by the
/// writer from core state — the worker only encodes and publishes.
pub enum PublishAction {
    OrderUpdate {
        event: OrderUpdateEvent,
        latency_ctx: Option<ReportLatencyCtx>,
    },
    BalanceUpdate {
        event: BalanceUpdateEvent,
    },
    PositionUpdate {
        event: PositionUpdateEvent,
    },
}

/// Latency context for GW-report-triggered order updates.
/// t4/t5/t6 are captured before the writer enqueues the publish action;
/// t7 is captured by the worker after NATS publish completes.
pub struct ReportLatencyCtx {
    pub order_id: i64,
    pub t4_ns: i64,
    pub t5_ns: i64,
    pub t6_ns: i64,
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Publish executor pool. Sharded by `i64` (order_id for order updates,
/// account_id for balance/position updates).
pub struct PublishExecutorPool {
    pool: ShardedPool<i64, PublishAction>,
    nats: NatsPublisher,
    latency_tx: mpsc::Sender<LatencyEvent>,
}

impl PublishExecutorPool {
    pub fn new(
        queue_capacity: usize,
        nats: NatsPublisher,
        latency_tx: mpsc::Sender<LatencyEvent>,
    ) -> Self {
        Self {
            pool: ShardedPool::new(queue_capacity),
            nats,
            latency_tx,
        }
    }

    /// Dispatch a publish action. Best-effort: drops on full queue.
    pub fn dispatch(&mut self, shard_key: i64, action: PublishAction) {
        let nats = self.nats.clone();
        let latency_tx = self.latency_tx.clone();
        match self.pool.try_dispatch(shard_key, action, |rx| {
            tokio::spawn(publish_worker(rx, nats, latency_tx))
        }) {
            DispatchResult::Ok => {}
            DispatchResult::QueueFull(_) => {
                warn!(shard_key, "publish queue full — action dropped (best-effort)");
            }
        }
    }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn publish_worker(
    mut rx: mpsc::Receiver<PublishAction>,
    nats: NatsPublisher,
    latency_tx: mpsc::Sender<LatencyEvent>,
) {
    while let Some(action) = rx.recv().await {
        match action {
            PublishAction::OrderUpdate { event, latency_ctx } => {
                nats.publish_order_update(&event).await;
                // Capture t7 as wall-clock time AFTER publish completes.
                if let Some(ctx) = latency_ctx {
                    let t7 = system_time_ns();
                    let _ = latency_tx.try_send(LatencyEvent::ReportPublished {
                        order_id: ctx.order_id,
                        t4_ns: ctx.t4_ns,
                        t5_ns: ctx.t5_ns,
                        t6_ns: ctx.t6_ns,
                        t7_ns: t7,
                    });
                }
            }
            PublishAction::BalanceUpdate { event } => {
                nats.publish_balance_update(&event).await;
            }
            PublishAction::PositionUpdate { event } => {
                nats.publish_position_update(&event).await;
            }
        }
    }
}
