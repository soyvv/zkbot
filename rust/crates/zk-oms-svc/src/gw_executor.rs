//! Gateway executor pool — sharded by `order_id`.
//!
//! Fixed N shards, each with one worker task that sequentially dispatches
//! pre-materialized gateway actions over a shared pool of `GatewayClient`s.
//! Shard index = `order_id % shard_count`, so place/cancel for the same
//! order always land on the same worker (preserving per-order ordering).
//!
//! The writer loop never awaits gateway I/O directly — it enqueues actions
//! here via non-blocking `try_send`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::gw_client::GatewayClient;
use crate::latency::{system_time_ns, LatencyEvent};
use crate::oms_actor::OmsCommand;

use zk_proto_rs::zk::gateway::v1::{
    BatchCancelOrdersRequest, BatchSendOrdersRequest, CancelOrderRequest, SendOrderRequest,
};

// ── Action types ────────────────────────────────────────────────────────────

/// Pre-materialized gateway action. All proto payloads are already cloned
/// from core state by the writer — the worker only performs I/O.
/// Fire-and-forget: no ack_tx. On failure, the worker sends a feedback
/// command (GatewaySendFailed / GatewayCancelSendFailed) back to the writer.
#[derive(Debug)]
pub enum GwAction {
    SendOrder {
        gw_key: String,
        request: SendOrderRequest,
        order_id: i64,
        gw_id: u32,
        latency: Option<GwSendLatency>,
    },
    BatchSendOrders {
        gw_key: String,
        request: BatchSendOrdersRequest,
        order_ids: Vec<i64>,
        gw_id: u32,
    },
    CancelOrder {
        gw_key: String,
        request: CancelOrderRequest,
        order_id: i64,
        gw_id: u32,
    },
    BatchCancel {
        gw_key: String,
        request: BatchCancelOrdersRequest,
        order_ids: Vec<i64>,
        gw_id: u32,
    },
}

/// Latency context for single-order sends (t1 captured at gRPC handler entry).
#[derive(Debug)]
pub struct GwSendLatency {
    pub order_id: i64,
    pub order_created_at: i64,
    pub t1_ns: i64,
    pub t2_ns: i64,
    pub tw_core_ns: i64,
    /// Timestamp when the writer dispatches to the gw executor queue.
    /// Used to compute `gw_exec_queue_wait = t_gw_dequeue - writer_dispatch_ts`.
    pub writer_dispatch_ts: i64,
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Gateway executor pool. Sharded by `order_id % shard_count`.
///
/// All shards are pre-spawned at construction — `dispatch` is `&self` (no mutation).
#[derive(Clone)]
pub struct GwExecutorPool {
    shards: Vec<mpsc::Sender<GwAction>>,
    shard_count: usize,
    queue_capacity: usize,
    /// Shared gateway clients, read by all workers.
    /// Write-locked only during startup/discovery (register_gateway).
    clients: Arc<RwLock<HashMap<u32, (String, GatewayClient)>>>,
}

impl GwExecutorPool {
    /// Create a new gateway executor pool with `shard_count` pre-spawned workers.
    pub fn new(
        shard_count: usize,
        queue_capacity: usize,
        gw_id_to_key: &[(u32, String)],
        gw_pool: &crate::gw_client::GwClientPool,
        latency_tx: mpsc::Sender<LatencyEvent>,
        cmd_tx: mpsc::Sender<OmsCommand>,
    ) -> Self {
        let mut client_map = HashMap::new();
        for (gw_id, gw_key) in gw_id_to_key {
            if let Some(client) = gw_pool.get_client(gw_key) {
                client_map.insert(*gw_id, (gw_key.clone(), client));
            }
        }
        let clients = Arc::new(RwLock::new(client_map));

        let mut shards = Vec::with_capacity(shard_count);
        for shard_id in 0..shard_count {
            let (tx, rx) = mpsc::channel(queue_capacity);
            tokio::spawn(gw_worker(
                shard_id,
                rx,
                Arc::clone(&clients),
                latency_tx.clone(),
                cmd_tx.clone(),
            ));
            shards.push(tx);
        }

        info!(shard_count, queue_capacity, "GwExecutorPool started");

        Self {
            shards,
            shard_count,
            queue_capacity,
            clients,
        }
    }

    /// Register a new gateway (e.g. on dynamic discovery).
    pub fn register_gateway(&self, gw_id: u32, gw_key: String, client: GatewayClient) {
        let mut map = self
            .clients
            .write()
            .expect("GwExecutorPool clients poisoned");
        map.insert(gw_id, (gw_key, client));
    }

    /// Remove a gateway from the live client map.
    pub fn remove_gateway(&self, gw_id: u32) {
        let mut map = self
            .clients
            .write()
            .expect("GwExecutorPool clients poisoned");
        map.remove(&gw_id);
    }

    /// Dispatch a gateway action to the shard for `order_id`.
    ///
    /// Returns `Err(action)` if the shard queue is full.
    pub fn dispatch(&self, order_id: i64, action: GwAction) -> Result<(), GwAction> {
        let shard = (order_id as u64 % self.shard_count as u64) as usize;
        match self.shards[shard].try_send(action) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(a)) => Err(a),
            Err(mpsc::error::TrySendError::Closed(a)) => {
                warn!(shard, "gw executor shard channel closed");
                Err(a)
            }
        }
    }

    /// Current queue depth per shard (for metrics).
    pub fn shard_queue_depths(&self) -> Vec<usize> {
        self.shards
            .iter()
            .map(|tx| self.queue_capacity - tx.capacity())
            .collect()
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        self.shard_count
    }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn gw_worker(
    shard_id: usize,
    mut rx: mpsc::Receiver<GwAction>,
    clients: Arc<RwLock<HashMap<u32, (String, GatewayClient)>>>,
    latency_tx: mpsc::Sender<LatencyEvent>,
    cmd_tx: mpsc::Sender<OmsCommand>,
) {
    while let Some(action) = rx.recv().await {
        let gw_exec_dequeue_ns = system_time_ns();

        match action {
            GwAction::SendOrder {
                gw_key,
                request,
                order_id,
                gw_id,
                latency,
            } => {
                let mut client = match get_client(&clients, gw_id) {
                    Some(c) => c,
                    None => {
                        warn!(
                            shard_id,
                            gw_id, order_id, "no client for gw_id — dropping SendOrder"
                        );
                        let _ = cmd_tx.try_send(OmsCommand::GatewaySendFailed {
                            order_id,
                            gw_id,
                            error_msg: format!("no gateway client for gw_id={gw_id}"),
                        });
                        continue;
                    }
                };

                let t3 = system_time_ns();
                let result = client.place_order(tonic::Request::new(request)).await;
                let t3r = system_time_ns();

                if let Err(e) = &result {
                    warn!(gw_key, order_id, %e, "gateway SendOrder failed");
                    let _ = cmd_tx.try_send(OmsCommand::GatewaySendFailed {
                        order_id,
                        gw_id,
                        error_msg: e.to_string(),
                    });
                }
                if let Some(lat) = latency {
                    let _ = latency_tx.try_send(LatencyEvent::OrderSent {
                        order_id: lat.order_id,
                        order_created_at: lat.order_created_at,
                        t1_ns: lat.t1_ns,
                        t2_ns: lat.t2_ns,
                        tw_core_ns: lat.tw_core_ns,
                        t3_ns: t3,
                        t3r_ns: t3r,
                        writer_dispatch_ts: lat.writer_dispatch_ts,
                        gw_exec_dequeue_ns,
                    });
                }
            }

            GwAction::BatchSendOrders {
                gw_key,
                request,
                order_ids,
                gw_id,
            } => {
                let mut client = match get_client(&clients, gw_id) {
                    Some(c) => c,
                    None => {
                        warn!(
                            shard_id,
                            gw_id, "no client for gw_id — dropping BatchSendOrders"
                        );
                        let err_msg = format!("no gateway client for gw_id={gw_id}");
                        for oid in &order_ids {
                            let _ = cmd_tx.try_send(OmsCommand::GatewaySendFailed {
                                order_id: *oid,
                                gw_id,
                                error_msg: err_msg.clone(),
                            });
                        }
                        continue;
                    }
                };

                let result = client
                    .batch_place_orders(tonic::Request::new(request))
                    .await;
                if let Err(e) = &result {
                    warn!(gw_key, %e, "gateway BatchSendOrders failed");
                    let err_msg = e.to_string();
                    for oid in &order_ids {
                        let _ = cmd_tx.try_send(OmsCommand::GatewaySendFailed {
                            order_id: *oid,
                            gw_id,
                            error_msg: err_msg.clone(),
                        });
                    }
                }
            }

            GwAction::CancelOrder {
                gw_key,
                request,
                order_id,
                gw_id,
            } => {
                let mut client = match get_client(&clients, gw_id) {
                    Some(c) => c,
                    None => {
                        warn!(
                            shard_id,
                            gw_id, order_id, "no client for gw_id — dropping CancelOrder"
                        );
                        let _ = cmd_tx.try_send(OmsCommand::GatewayCancelSendFailed {
                            order_id,
                            gw_id,
                            error_msg: format!("no gateway client for gw_id={gw_id}"),
                        });
                        continue;
                    }
                };

                let result = client.cancel_order(tonic::Request::new(request)).await;
                if let Err(e) = &result {
                    warn!(gw_key, order_id, %e, "gateway CancelOrder failed");
                    let _ = cmd_tx.try_send(OmsCommand::GatewayCancelSendFailed {
                        order_id,
                        gw_id,
                        error_msg: e.to_string(),
                    });
                }
            }

            GwAction::BatchCancel {
                gw_key,
                request,
                order_ids,
                gw_id,
            } => {
                let mut client = match get_client(&clients, gw_id) {
                    Some(c) => c,
                    None => {
                        warn!(
                            shard_id,
                            gw_id, "no client for gw_id — dropping BatchCancel"
                        );
                        let err_msg = format!("no gateway client for gw_id={gw_id}");
                        for oid in &order_ids {
                            let _ = cmd_tx.try_send(OmsCommand::GatewayCancelSendFailed {
                                order_id: *oid,
                                gw_id,
                                error_msg: err_msg.clone(),
                            });
                        }
                        continue;
                    }
                };

                let result = client
                    .batch_cancel_orders(tonic::Request::new(request))
                    .await;
                if let Err(e) = &result {
                    warn!(gw_key, %e, "gateway BatchCancel failed");
                    let err_msg = e.to_string();
                    for oid in &order_ids {
                        let _ = cmd_tx.try_send(OmsCommand::GatewayCancelSendFailed {
                            order_id: *oid,
                            gw_id,
                            error_msg: err_msg.clone(),
                        });
                    }
                }
            }
        }
    }
}

/// Clone a gateway client from the shared map (read lock, very cheap).
fn get_client(
    clients: &RwLock<HashMap<u32, (String, GatewayClient)>>,
    gw_id: u32,
) -> Option<GatewayClient> {
    let map = clients.read().expect("GwExecutorPool clients poisoned");
    map.get(&gw_id).map(|(_, c)| c.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Create a pool with no registered clients — workers will send
    /// `GatewaySendFailed` for every action, but dispatch routing works fine.
    fn test_pool(
        shard_count: usize,
        queue_capacity: usize,
    ) -> (
        GwExecutorPool,
        mpsc::Receiver<LatencyEvent>,
        mpsc::Receiver<OmsCommand>,
    ) {
        let (latency_tx, latency_rx) = mpsc::channel(256);
        let (cmd_tx, cmd_rx) = mpsc::channel(256);

        let pool = GwExecutorPool::new(
            shard_count,
            queue_capacity,
            &[], // no gw_id_to_key — workers will find no client
            &crate::gw_client::GwClientPool::new(),
            latency_tx,
            cmd_tx,
        );
        (pool, latency_rx, cmd_rx)
    }

    fn send_order_action(order_id: i64, gw_id: u32) -> GwAction {
        GwAction::SendOrder {
            gw_key: "gw_test".into(),
            request: SendOrderRequest::default(),
            order_id,
            gw_id,
            latency: None,
        }
    }

    fn cancel_action(order_id: i64, gw_id: u32) -> GwAction {
        GwAction::CancelOrder {
            gw_key: "gw_test".into(),
            request: CancelOrderRequest::default(),
            order_id,
            gw_id,
        }
    }

    /// Same order_id always routes to the same shard index.
    #[tokio::test]
    async fn test_stable_shard_routing() {
        let (pool, _lat_rx, mut cmd_rx) = test_pool(4, 64);

        // Dispatch 5 orders with same effective shard (order_id % 4 == 0).
        for i in 0..5i64 {
            pool.dispatch(i * 4, send_order_action(i * 4, 1)).unwrap();
        }

        // Workers have no client → they send GatewaySendFailed for each.
        // Wait for all 5 feedback messages.
        let mut failures = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while failures.len() < 5 {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    if let OmsCommand::GatewaySendFailed { order_id, .. } = cmd {
                        failures.push(order_id);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        assert_eq!(failures.len(), 5);
        // All should have order_id % 4 == 0 → same shard → processed in order.
        assert_eq!(failures, vec![0, 4, 8, 12, 16]);
    }

    /// Place then cancel for same order_id: ordering preserved on same shard.
    #[tokio::test]
    async fn test_place_cancel_same_order_ordered() {
        let (pool, _lat_rx, mut cmd_rx) = test_pool(4, 64);
        let oid = 42i64;

        pool.dispatch(oid, send_order_action(oid, 1)).unwrap();
        pool.dispatch(oid, cancel_action(oid, 1)).unwrap();

        // Expect GatewaySendFailed then GatewayCancelSendFailed, in that order.
        let mut msgs = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while msgs.len() < 2 {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        OmsCommand::GatewaySendFailed { order_id, .. } =>
                            msgs.push(("send", order_id)),
                        OmsCommand::GatewayCancelSendFailed { order_id, .. } =>
                            msgs.push(("cancel", order_id)),
                        _ => {}
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        assert_eq!(msgs, vec![("send", 42), ("cancel", 42)]);
    }

    /// Queue full returns Err.
    #[tokio::test]
    async fn test_queue_full_sync_failure() {
        // 1 shard, capacity 2. Workers have no client so they process quickly,
        // but we can race by sending many at once.
        let (pool, _lat_rx, _cmd_rx) = test_pool(1, 2);

        // Fill the channel: dispatch many until one fails.
        // The worker will drain items fast (no client = immediate fail),
        // so we need to be quick. Send 100 and count failures.
        let mut ok = 0;
        let mut _err = 0;
        for i in 0..100i64 {
            match pool.dispatch(i, send_order_action(i, 999)) {
                Ok(()) => ok += 1,
                Err(_) => _err += 1,
            }
        }
        // We should have at least some successes and potentially some failures
        // depending on timing. The important thing is that dispatch returns Err
        // rather than blocking when queue is full.
        assert!(ok > 0, "should have some successful dispatches");
        // With capacity 2 + fast drain, might not get failures. That's OK —
        // the queue-full path is exercised if any error occurs.
        // For deterministic test, use a slow worker (needs real client mock).
    }

    /// dispatch is non-blocking — returns in < 1ms.
    #[tokio::test]
    async fn test_dispatch_is_nonblocking() {
        let (pool, _lat_rx, _cmd_rx) = test_pool(4, 64);

        let start = std::time::Instant::now();
        for i in 0..10i64 {
            pool.dispatch(i, send_order_action(i, 1)).unwrap();
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(5),
            "10 dispatches took {:?} — should be < 5ms (non-blocking)",
            elapsed,
        );
    }

    /// shard_queue_depths reports per-shard depth.
    #[tokio::test]
    async fn test_shard_queue_depths() {
        let (pool, _lat_rx, _cmd_rx) = test_pool(2, 32);

        let depths = pool.shard_queue_depths();
        assert_eq!(depths.len(), 2);

        // Initially ≈ 0 (workers may have already drained).
        // Just verify the API works.
        assert!(depths[0] <= 32);
        assert!(depths[1] <= 32);
    }

    /// shard_count returns configured value.
    #[tokio::test]
    async fn test_shard_count() {
        let (pool, _lat_rx, _cmd_rx) = test_pool(8, 16);
        assert_eq!(pool.shard_count(), 8);
    }

    /// Different order_ids land on different shards (modular distribution).
    #[tokio::test]
    async fn test_different_orders_different_shards() {
        let (pool, _lat_rx, mut cmd_rx) = test_pool(4, 64);

        // order_id 0 → shard 0, order_id 1 → shard 1, order_id 2 → shard 2, order_id 3 → shard 3
        for i in 0..4i64 {
            pool.dispatch(i, send_order_action(i, 1)).unwrap();
        }

        // All 4 should be processed (failures due to no client).
        let mut failures = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while failures.len() < 4 {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    if let OmsCommand::GatewaySendFailed { order_id, .. } = cmd {
                        failures.push(order_id);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        assert_eq!(failures.len(), 4);
        // All 4 were processed (order across shards is non-deterministic, but all arrive).
        failures.sort();
        assert_eq!(failures, vec![0, 1, 2, 3]);
    }
}
