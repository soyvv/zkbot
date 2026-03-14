//! OMS single-writer actor + read replica.
//!
//! # Architecture
//!
//! ```text
//! PlaceOrder gRPC ──────────────────────────────────► OMS writer task (single)
//! CancelOrder gRPC ─────────────────────────────────►   process_message()
//! NATS report sub ──────────────────────────────────►   dispatch actions
//! Periodic timers ──────────────────────────────────►   take_snapshot()
//!                                                        replica.store(Arc::new(snap))
//!                                                                 │
//!                                                      ArcSwap<OmsSnapshot>
//!                                                                 │
//! QueryOpenOrders gRPC ◄──── replica.load() ◄──────────────────┘
//! QueryBalances gRPC   ◄──── replica.load()
//! ```
//!
//! Mutation commands are sent via `mpsc::Sender<OmsCommand>`.  Commands that
//! require a response (PlaceOrder, CancelOrder, Panic, etc.) include a
//! `oneshot::Sender<OMSResponse>` reply channel.  The writer task sends the
//! response after processing.
//!
//! # Performance
//!
//! - `replica.load()` is a single atomic pointer load — never blocks the writer.
//! - `OmsSnapshot` uses `im::HashMap` (HAMT), so `replica.store()` is O(1).
//! - Gateway gRPC calls are fire-and-forget from the writer's perspective:
//!   the response comes back via the NATS report subscription.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::latency::{publish_latency_batch, system_time_ns, LatencyTracker};

use zk_oms_rs::{
    config::ConfdataManager,
    models::{CancelRecheckRequest, OmsAction, OmsMessage, OrderRecheckRequest},
    oms_core::OmsCore,
    snapshot::{OmsSnapshot, OmsSnapshotWriter},
    utils::gen_timestamp_ms,
};
use zk_proto_rs::zk::{
    exch_gw::v1::{BalanceUpdate, OrderReport},
    oms::v1::{
        oms_response, OmsResponse, OrderCancelRequest, OrderRequest, OmsErrorType,
    },
};

use crate::{
    gw_client::GwClientPool,
    nats_handler::NatsPublisher,
    redis_writer::RedisWriter,
};

/// Atomic read replica — shared between the writer task and all gRPC query handlers.
pub type ReadReplica = Arc<ArcSwap<OmsSnapshot>>;

/// All commands the OMS writer task can receive.
pub enum OmsCommand {
    PlaceOrder {
        req:             OrderRequest,
        oms_received_ns: i64,   // t1: system_time_ns() at gRPC handler entry
        reply:           oneshot::Sender<OmsResponse>,
    },
    BatchPlaceOrders {
        reqs:  Vec<OrderRequest>,
        reply: oneshot::Sender<OmsResponse>,
    },
    CancelOrder {
        req:   OrderCancelRequest,
        reply: oneshot::Sender<OmsResponse>,
    },
    BatchCancelOrders {
        reqs:  Vec<OrderCancelRequest>,
        reply: oneshot::Sender<OmsResponse>,
    },
    /// Gateway order report arriving via NATS subscription.
    GatewayOrderReport(OrderReport),
    /// Gateway balance update arriving via NATS subscription.
    GatewayBalanceUpdate(BalanceUpdate),
    Panic {
        account_id: i64,
        reply:      oneshot::Sender<OmsResponse>,
    },
    DontPanic {
        account_id: i64,
        reply:      oneshot::Sender<OmsResponse>,
    },
    /// Reload config from PG. The caller builds the new `ConfdataManager`
    /// before sending so that the writer task stays synchronous.
    ReloadConfig {
        new_config: ConfdataManager,
        reply:      oneshot::Sender<OmsResponse>,
    },
    /// Periodic cleanup tick.
    Cleanup { ts_ms: i64 },
    /// Retry check for a pending order.
    RecheckOrder(OrderRecheckRequest),
    /// Retry check for a pending cancel.
    RecheckCancel(CancelRecheckRequest),
}

// ── Writer loop ──────────────────────────────────────────────────────────────

/// Spawn the OMS writer task.
///
/// This function takes ownership of `core`, `redis`, and `gw_pool` — they live
/// exclusively inside the writer task to avoid any locking.
///
/// Returns a `JoinHandle` for the task; abort it or send a shutdown signal via
/// the `CancellationToken` to gracefully stop.
pub fn spawn_writer(
    core:                OmsCore,
    rx:                  mpsc::Receiver<OmsCommand>,
    replica:             ReadReplica,
    nats:                NatsPublisher,
    redis:               RedisWriter,
    gw_pool:             GwClientPool,
    shutdown:            CancellationToken,
    metrics_interval:    Duration,
    metrics_max_pending: usize,
    metrics_max_complete: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(oms_writer_loop(
        core, rx, replica, nats, redis, gw_pool, shutdown,
        metrics_interval, metrics_max_pending, metrics_max_complete,
    ))
}

async fn oms_writer_loop(
    mut core:             OmsCore,
    mut rx:               mpsc::Receiver<OmsCommand>,
    replica:              ReadReplica,
    nats:                 NatsPublisher,
    mut redis:            RedisWriter,
    mut gw_pool:          GwClientPool,
    shutdown:             CancellationToken,
    metrics_interval:     Duration,
    metrics_max_pending:  usize,
    metrics_max_complete: usize,
) {
    // Build initial snapshot from warm-started OmsCore state.
    let (initial_snap, mut writer) = core.take_snapshot();
    replica.store(Arc::new(initial_snap));
    info!("OMS writer loop started");

    let mut tracker = LatencyTracker::new(metrics_max_pending, metrics_max_complete);
    let mut metrics_tick = tokio::time::interval(metrics_interval);
    metrics_tick.tick().await; // skip the immediate first tick

    loop {
        tokio::select! {
            biased;  // always check shutdown first

            _ = shutdown.cancelled() => {
                info!("OMS writer loop: shutdown signal received");
                break;
            }

            cmd = rx.recv() => {
                let Some(cmd) = cmd else {
                    info!("OMS command channel closed");
                    break;
                };
                process_command(
                    cmd,
                    &mut core,
                    &mut writer,
                    &replica,
                    &nats,
                    &mut redis,
                    &mut gw_pool,
                    &mut tracker,
                ).await;
            }

            _ = metrics_tick.tick() => {
                let records = tracker.drain_complete();
                if !records.is_empty() {
                    let ts_ns = system_time_ns();
                    publish_latency_batch(&nats.nats, &nats.oms_id, records, ts_ns).await;
                }
            }
        }
    }
    info!("OMS writer loop exited");
}

// ── Command dispatch ─────────────────────────────────────────────────────────

async fn process_command(
    cmd:      OmsCommand,
    core:     &mut OmsCore,
    writer:   &mut OmsSnapshotWriter,
    replica:  &ReadReplica,
    nats:     &NatsPublisher,
    redis:    &mut RedisWriter,
    gw_pool:  &mut GwClientPool,
    tracker:  &mut LatencyTracker,
) {
    // For GW report commands, capture t6 and the report fields before consuming cmd.
    // (order_id, t6_ns, t4_ns, t5_ns)
    let report_latency_ctx: Option<(i64, i64, i64, i64)> = match &cmd {
        OmsCommand::GatewayOrderReport(r) => {
            let t6 = system_time_ns(); // hot path: one syscall
            Some((r.order_id, t6, r.gw_received_at_ns, r.update_timestamp * 1_000_000))
        }
        _ => None,
    };

    // t1: OMS gRPC handler entry (only available for single PlaceOrder; 0 for batch/other).
    let place_order_t1: i64 = match &cmd {
        OmsCommand::PlaceOrder { oms_received_ns, .. } => *oms_received_ns,
        _ => 0,
    };

    // Convert to OmsMessage (and capture reply channel if present).
    let (oms_msg, reply): (OmsMessage, Option<oneshot::Sender<OmsResponse>>) = match cmd {
        OmsCommand::PlaceOrder { req, reply, .. } =>
            (OmsMessage::PlaceOrder(req), Some(reply)),
        OmsCommand::BatchPlaceOrders { reqs, reply } =>
            (OmsMessage::BatchPlaceOrders(reqs), Some(reply)),
        OmsCommand::CancelOrder { req, reply } =>
            (OmsMessage::CancelOrder(req), Some(reply)),
        OmsCommand::BatchCancelOrders { reqs, reply } =>
            (OmsMessage::BatchCancelOrders(reqs), Some(reply)),
        OmsCommand::GatewayOrderReport(r) =>
            (OmsMessage::GatewayOrderReport(r), None),
        OmsCommand::GatewayBalanceUpdate(u) =>
            (OmsMessage::BalanceUpdate(u), None),
        OmsCommand::Panic { account_id, reply } => {
            // Pre-apply to snapshot writer so readers see panic immediately.
            writer.apply_panic(account_id);
            (OmsMessage::Panic { account_id }, Some(reply))
        }
        OmsCommand::DontPanic { account_id, reply } => {
            writer.apply_clear_panic(account_id);
            (OmsMessage::DontPanic { account_id }, Some(reply))
        }
        OmsCommand::ReloadConfig { new_config, reply } => {
            core.reload_config(new_config);
            // Publish refreshed snapshot (config changes can affect risk limits).
            publish_snapshot(core, writer, replica, false);
            let resp = ok_response("Config reloaded");
            let _ = reply.send(resp);
            return; // ReloadConfig is handled inline; no process_message call needed.
        }
        OmsCommand::Cleanup { ts_ms } =>
            (OmsMessage::Cleanup { ts_ms }, None),
        OmsCommand::RecheckOrder(r) =>
            (OmsMessage::RecheckOrder(r), None),
        OmsCommand::RecheckCancel(r) =>
            (OmsMessage::RecheckCancel(r), None),
    };

    // ── Drive OmsCore ────────────────────────────────────────────────────────
    let actions = core.process_message(oms_msg);

    // ── Dispatch actions ─────────────────────────────────────────────────────
    let mut balances_dirty = false;
    let mut success = true;
    let mut err_msg = String::new();

    for action in actions {
        match action {
            OmsAction::PersistOrder { order, set_expire, set_closed } => {
                writer.apply_persist_order(&order, set_closed);
                if let Err(e) = redis.write_order(&order, set_expire, set_closed).await {
                    warn!(order_id = order.order_id, error = %e, "Redis write_order failed");
                }
            }
            OmsAction::PublishOrderUpdate(ref event) => {
                // Hot path: record t7 from event.timestamp before publishing.
                if let Some((order_id, t6, t4, t5)) = report_latency_ctx {
                    let t7 = event.timestamp * 1_000_000;
                    tracker.record_report_published(order_id, t4, t5, t6, t7);
                }
                nats.publish_order_update(event).await;
            }
            OmsAction::PublishBalanceUpdate(event) => {
                balances_dirty = true;
                nats.publish_position_update(&event).await;
            }
            OmsAction::SendOrderToGw { gw_key, request, order_id, order_created_at } => {
                let t3 = system_time_ns(); // hot path: one syscall before send
                if let Err(e) = gw_pool.send_order(&gw_key, request).await {
                    warn!(gw_key, error = %e, "SendOrder to gateway failed");
                    success = false;
                    err_msg = e.to_string();
                }
                let t3r = system_time_ns(); // hot path: one syscall after send
                tracker.record_order_sent(order_id, order_created_at, place_order_t1, t3, t3r);
            }
            OmsAction::BatchSendOrdersToGw { gw_key, request } => {
                if let Err(e) = gw_pool.batch_send_orders(&gw_key, request).await {
                    warn!(gw_key, error = %e, "BatchSendOrders to gateway failed");
                }
            }
            OmsAction::SendCancelToGw { gw_key, request } => {
                if let Err(e) = gw_pool.cancel_order(&gw_key, request).await {
                    warn!(gw_key, error = %e, "CancelOrder to gateway failed");
                }
            }
            OmsAction::BatchCancelToGw { gw_key, request } => {
                if let Err(e) = gw_pool.batch_cancel_orders(&gw_key, request).await {
                    warn!(gw_key, error = %e, "BatchCancelOrders to gateway failed");
                }
            }
        }
    }

    // ── Publish updated snapshot (O(1) for im fields) ────────────────────────
    publish_snapshot(core, writer, replica, balances_dirty);

    // ── Reply to caller ──────────────────────────────────────────────────────
    if let Some(tx) = reply {
        let resp = if success {
            ok_response("")
        } else {
            err_response(OmsErrorType::OmsErrTypeInternalError, &err_msg)
        };
        if tx.send(resp).is_err() {
            warn!("gRPC caller dropped — OmsResponse not delivered");
        }
    }
}

// ── Snapshot helper ──────────────────────────────────────────────────────────

fn publish_snapshot(
    core:          &OmsCore,
    writer:        &mut OmsSnapshotWriter,
    replica:       &ReadReplica,
    balances_dirty: bool,
) {
    let (bal, exch_bal) = if balances_dirty {
        (
            core.balance_mgr.snapshot_balances(),
            core.balance_mgr.snapshot_exch_balances(),
        )
    } else {
        let prev = replica.load();
        (prev.balances.clone(), prev.exch_balances.clone())
    };
    let snap = writer.publish(bal, exch_bal, gen_timestamp_ms());
    replica.store(Arc::new(snap));
}

// ── OMSResponse helpers ──────────────────────────────────────────────────────

pub fn ok_response(msg: &str) -> OmsResponse {
    OmsResponse {
        status:     oms_response::Status::OmsRespStatusSuccess as i32,
        error_type: OmsErrorType::OmsErrTypeUnspecified as i32,
        message:    msg.to_string(),
        timestamp:  gen_timestamp_ms(),
    }
}

pub fn err_response(error_type: OmsErrorType, msg: &str) -> OmsResponse {
    OmsResponse {
        status:     oms_response::Status::OmsRespStatusFail as i32,
        error_type: error_type as i32,
        message:    msg.to_string(),
        timestamp:  gen_timestamp_ms(),
    }
}

// ── Utility: send command and await response ─────────────────────────────────

/// Send a command that expects an `OmsResponse` and await the reply.
///
/// Returns `Err` if the writer task has stopped (channel closed).
pub async fn send_cmd_await(
    cmd_tx: &mpsc::Sender<OmsCommand>,
    make_cmd: impl FnOnce(oneshot::Sender<OmsResponse>) -> OmsCommand,
) -> Result<OmsResponse, tonic::Status> {
    let (tx, rx) = oneshot::channel::<OmsResponse>();
    cmd_tx
        .send(make_cmd(tx))
        .await
        .map_err(|_| tonic::Status::unavailable("OMS writer task stopped"))?;
    rx.await
        .map_err(|_| tonic::Status::internal("OMS writer task dropped reply"))
}
