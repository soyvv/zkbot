//! Recorder publish executor — JetStream publication of terminal-order and
//! trade events for the recorder service, off the OMS writer critical path.
//!
//! Follows the same sharded-pool pattern as `publish_executor`, but uses
//! JetStream (at-least-once) instead of plain NATS (at-most-once).
//! Sharded by `account_id` to match the per-account JetStream subjects.
//! Drop-on-full semantics: the writer never blocks on recorder publish.

use tokio::sync::mpsc;
use tracing::warn;

use crate::executor::{DispatchResult, ShardedPool};
use crate::nats_handler::NatsPublisher;

use zk_proto_rs::zk::recorder::v1::{RecorderTerminalOrder, RecorderTradeEvent};

// ── Action types ────────────────────────────────────────────────────────────

/// Pre-materialized recorder action. Events are built by the writer from
/// core state — the worker only encodes and publishes to JetStream.
pub enum RecorderAction {
    TerminalOrder(RecorderTerminalOrder),
    Trade(RecorderTradeEvent),
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Recorder executor pool. Sharded by `account_id`.
pub struct RecorderExecutorPool {
    pool: ShardedPool<i64, RecorderAction>,
    nats: NatsPublisher,
}

impl RecorderExecutorPool {
    pub fn new(queue_capacity: usize, nats: NatsPublisher) -> Self {
        Self {
            pool: ShardedPool::new(queue_capacity),
            nats,
        }
    }

    /// Dispatch a recorder action. Best-effort: drops on full queue.
    pub fn dispatch(&mut self, account_id: i64, action: RecorderAction) {
        let nats = self.nats.clone();
        match self.pool.try_dispatch(account_id, action, |rx| {
            tokio::spawn(recorder_worker(rx, nats))
        }) {
            DispatchResult::Ok => {}
            DispatchResult::QueueFull(_) => {
                warn!(
                    account_id,
                    "recorder publish queue full — action dropped (best-effort)"
                );
            }
        }
    }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn recorder_worker(mut rx: mpsc::Receiver<RecorderAction>, nats: NatsPublisher) {
    while let Some(action) = rx.recv().await {
        match action {
            RecorderAction::TerminalOrder(event) => {
                nats.publish_recorder_terminal_order(&event).await;
            }
            RecorderAction::Trade(event) => {
                nats.publish_recorder_trade(&event).await;
            }
        }
    }
}
