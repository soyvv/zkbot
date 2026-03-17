//! Gateway executor pool — sharded by `gw_id`.
//!
//! Each gateway shard has one worker task that sequentially dispatches
//! pre-materialized gateway actions (send order, cancel, batch variants)
//! over its own clone of the `GatewayServiceClient`.
//!
//! The writer loop never awaits gateway I/O directly — it enqueues actions
//! here and optionally awaits a oneshot ACK for reply-critical commands.

use std::collections::HashMap;

use tokio::sync::mpsc;
use tracing::warn;

use crate::executor::{DispatchResult, ShardedPool};
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
pub struct GwSendLatency {
    pub order_id: i64,
    pub order_created_at: i64,
    pub t1_ns: i64,
    pub t2_ns: i64,
    pub tw_core_ns: i64,
    /// Timestamp when the writer dispatches to the gw executor queue.
    /// Used to compute `gw_exec_queue_wait = t3 - writer_dispatch_ts`.
    pub writer_dispatch_ts: i64,
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Gateway executor pool. Sharded by `gw_id: u32`.
pub struct GwExecutorPool {
    pool: ShardedPool<u32, GwAction>,
    /// Cloned clients for spawning new shard workers.
    /// Keyed by (gw_id → gw_key → client). We store (gw_key, client) pairs.
    clients: HashMap<u32, (String, GatewayClient)>,
    latency_tx: mpsc::Sender<LatencyEvent>,
    /// Feedback channel for gateway failure commands back to the writer.
    cmd_tx: mpsc::Sender<OmsCommand>,
}

impl GwExecutorPool {
    /// Create a new gateway executor pool.
    ///
    /// `gw_id_to_key` maps integer gw_id → gw_key string.
    /// Clients are cloned from `gw_pool` for each known gateway.
    pub fn new(
        queue_capacity: usize,
        gw_id_to_key: &[(u32, String)],
        gw_pool: &crate::gw_client::GwClientPool,
        latency_tx: mpsc::Sender<LatencyEvent>,
        cmd_tx: mpsc::Sender<OmsCommand>,
    ) -> Self {
        let mut clients = HashMap::new();
        for (gw_id, gw_key) in gw_id_to_key {
            if let Some(client) = gw_pool.get_client(gw_key) {
                clients.insert(*gw_id, (gw_key.clone(), client));
            }
        }
        Self {
            pool: ShardedPool::new(queue_capacity),
            clients,
            latency_tx,
            cmd_tx,
        }
    }

    /// Register a new gateway (e.g. on dynamic discovery).
    pub fn register_gateway(&mut self, gw_id: u32, gw_key: String, client: GatewayClient) {
        self.clients.insert(gw_id, (gw_key, client));
    }

    /// Dispatch a gateway action to the shard for `gw_id`.
    ///
    /// Returns `Err(action)` if the shard queue is full.
    pub fn dispatch(&mut self, gw_id: u32, action: GwAction) -> Result<(), GwAction> {
        let latency_tx = self.latency_tx.clone();
        let cmd_tx = self.cmd_tx.clone();
        let client_entry = self.clients.get(&gw_id).cloned();

        self.pool.try_dispatch(gw_id, action, |rx| {
            let (_, client) = client_entry.unwrap_or_else(|| {
                panic!("GwExecutorPool: no client registered for gw_id={gw_id}");
            });
            tokio::spawn(gw_worker(rx, client, latency_tx, cmd_tx))
        }).into()
    }
}

impl From<DispatchResult<GwAction>> for Result<(), GwAction> {
    fn from(r: DispatchResult<GwAction>) -> Self {
        match r {
            DispatchResult::Ok => Ok(()),
            DispatchResult::QueueFull(a) => Err(a),
        }
    }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn gw_worker(
    mut rx: mpsc::Receiver<GwAction>,
    mut client: GatewayClient,
    latency_tx: mpsc::Sender<LatencyEvent>,
    cmd_tx: mpsc::Sender<OmsCommand>,
) {
    while let Some(action) = rx.recv().await {
        match action {
            GwAction::SendOrder { gw_key, request, order_id, gw_id, latency } => {
                let t3 = system_time_ns();
                let result = client
                    .place_order(tonic::Request::new(request))
                    .await;
                let t3r = system_time_ns();

                if let Err(e) = &result {
                    warn!(gw_key, order_id, %e, "gateway SendOrder failed");
                    // Best-effort feedback to writer — drop if channel full
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
                    });
                }
            }

            GwAction::BatchSendOrders { gw_key, request, order_ids, gw_id } => {
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

            GwAction::CancelOrder { gw_key, request, order_id, gw_id } => {
                let result = client
                    .cancel_order(tonic::Request::new(request))
                    .await;
                if let Err(e) = &result {
                    warn!(gw_key, order_id, %e, "gateway CancelOrder failed");
                    let _ = cmd_tx.try_send(OmsCommand::GatewayCancelSendFailed {
                        order_id,
                        gw_id,
                        error_msg: e.to_string(),
                    });
                }
            }

            GwAction::BatchCancel { gw_key, request, order_ids, gw_id } => {
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
