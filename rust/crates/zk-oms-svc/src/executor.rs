//! Generic sharded executor pool.
//!
//! Each shard key gets one bounded mpsc channel and one worker task.
//! Workers are spawned lazily on first dispatch to a shard.
//! Within a shard, actions are processed FIFO — ordering is guaranteed.

use std::collections::HashMap;
use std::hash::Hash;

use tokio::sync::mpsc;

/// Sharded executor pool.
///
/// `K` = shard key type (e.g. `u32` for gw_id, `i64` for account_id).
/// `A` = action payload type.
pub struct ShardedPool<K, A> {
    shards: HashMap<K, mpsc::Sender<A>>,
    queue_capacity: usize,
}

/// Result of a dispatch attempt.
pub enum DispatchResult<A> {
    /// Action was enqueued successfully.
    Ok,
    /// Shard queue is full — action returned to caller.
    QueueFull(A),
}

impl<K: Eq + Hash + Clone, A: Send + 'static> ShardedPool<K, A> {
    pub fn new(queue_capacity: usize) -> Self {
        Self {
            shards: HashMap::new(),
            queue_capacity,
        }
    }

    /// Dispatch action to the shard for `key` using non-blocking `try_send`.
    ///
    /// If this is the first action for the shard, `worker_factory` is called to
    /// spawn a worker task that drains the receiver.
    ///
    /// Returns `DispatchResult::QueueFull` if the shard queue is at capacity.
    pub fn try_dispatch(
        &mut self,
        key: K,
        action: A,
        worker_factory: impl FnOnce(mpsc::Receiver<A>) -> tokio::task::JoinHandle<()>,
    ) -> DispatchResult<A> {
        let tx = self.shards.entry(key).or_insert_with(|| {
            let (tx, rx) = mpsc::channel(self.queue_capacity);
            worker_factory(rx);
            tx
        });
        match tx.try_send(action) {
            Ok(()) => DispatchResult::Ok,
            Err(mpsc::error::TrySendError::Full(a)) => DispatchResult::QueueFull(a),
            Err(mpsc::error::TrySendError::Closed(a)) => {
                // Worker died — log would happen at caller level.
                DispatchResult::QueueFull(a)
            }
        }
    }

    /// Dispatch action to the shard for `key` using blocking `send().await`.
    ///
    /// Blocks the caller if the shard queue is full (backpressure).
    /// Returns `Err` only if the worker has been dropped (channel closed).
    pub async fn send_dispatch(
        &mut self,
        key: K,
        action: A,
        worker_factory: impl FnOnce(mpsc::Receiver<A>) -> tokio::task::JoinHandle<()>,
    ) -> Result<(), A> {
        let tx = self.shards.entry(key).or_insert_with(|| {
            let (tx, rx) = mpsc::channel(self.queue_capacity);
            worker_factory(rx);
            tx
        });
        tx.send(action).await.map_err(|e| e.0)
    }

    /// Number of active shards.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
}
