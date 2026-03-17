//! Persist executor pool — sharded by `account_id`.
//!
//! Each account shard has one worker task that sequentially writes
//! pre-materialized order/balance/position state to Redis.
//!
//! **Backpressure policy:** PersistAction uses blocking `send().await` (not
//! `try_send`) because persistence is correctness-critical for warm-start
//! recovery. If the queue fills, the writer blocks — this is intentional.

use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::executor::ShardedPool;
use crate::redis_writer::RedisWriter;

use zk_oms_rs::models::{ExchBalanceSnapshot, OmsManagedPosition, OmsOrder};

// ── Action types ────────────────────────────────────────────────────────────

/// Pre-materialized persistence action. All data is already extracted from
/// core state by the writer — the worker only performs Redis I/O.
pub enum PersistAction {
    Order {
        order: OmsOrder,
        set_expire: bool,
        set_closed: bool,
    },
    Balance {
        account_id: i64,
        asset_name: String,
        snap: ExchBalanceSnapshot,
    },
    Position {
        account_id: i64,
        inst_code: String,
        side: String,
        pos: OmsManagedPosition,
    },
}

impl PersistAction {
    /// Extract account_id for shard routing.
    pub fn account_id(&self) -> i64 {
        match self {
            Self::Order { order, .. } => order.account_id,
            Self::Balance { account_id, .. } => *account_id,
            Self::Position { account_id, .. } => *account_id,
        }
    }
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Persist executor pool. Sharded by `account_id: i64`.
pub struct PersistExecutorPool {
    pool: ShardedPool<i64, PersistAction>,
    /// Template writer cloned for each new shard worker.
    writer_template: RedisWriter,
}

impl PersistExecutorPool {
    pub fn new(queue_capacity: usize, writer_template: RedisWriter) -> Self {
        Self {
            pool: ShardedPool::new(queue_capacity),
            writer_template,
        }
    }

    /// Dispatch a persist action using blocking send (backpressure).
    ///
    /// Blocks the caller if the shard queue is full. Returns `Err` only if
    /// the worker has been dropped.
    pub async fn dispatch(&mut self, action: PersistAction) -> Result<(), PersistAction> {
        let account_id = action.account_id();
        let writer = self.writer_template.clone_writer();
        self.pool
            .send_dispatch(account_id, action, |rx| {
                tokio::spawn(persist_worker(rx, writer))
            })
            .await
    }

    /// Dispatch without blocking. Logs error if queue is full.
    /// Used in post-ack stage where we can't fail the command.
    pub fn try_dispatch(&mut self, action: PersistAction) {
        let account_id = action.account_id();
        let writer = self.writer_template.clone_writer();
        match self.pool.try_dispatch(account_id, action, |rx| {
            tokio::spawn(persist_worker(rx, writer))
        }) {
            crate::executor::DispatchResult::Ok => {}
            crate::executor::DispatchResult::QueueFull(_) => {
                error!(account_id, "persist queue full — action dropped (operational alert)");
            }
        }
    }

    pub fn queue_depth(&self) -> usize { self.pool.total_queue_depth() }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn persist_worker(mut rx: mpsc::Receiver<PersistAction>, mut redis: RedisWriter) {
    while let Some(action) = rx.recv().await {
        match action {
            PersistAction::Order { order, set_expire, set_closed } => {
                if let Err(e) = redis.write_order(&order, set_expire, set_closed).await {
                    warn!(order_id = order.order_id, error = %e, "persist: write_order failed");
                }
            }
            PersistAction::Balance { account_id, asset_name, snap } => {
                if let Err(e) = redis.write_balance(account_id, &asset_name, &snap).await {
                    warn!(account_id, asset = %asset_name, error = %e, "persist: write_balance failed");
                }
            }
            PersistAction::Position { account_id, inst_code, side, pos } => {
                if let Err(e) = redis.write_position(account_id, &inst_code, &side, &pos).await {
                    warn!(account_id, instrument = %inst_code, error = %e, "persist: write_position failed");
                }
            }
        }
    }
}
