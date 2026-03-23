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
//!                                                      ArcSwap<OmsSnapshotV2>
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
//! - `OmsSnapshotV2` uses `im::HashMap` (HAMT), so `replica.store()` is O(1).
//! - Gateway gRPC calls are fire-and-forget from the writer's perspective:
//!   the response comes back via the NATS report subscription.
//!   On gateway RPC failure, the gw_worker sends `GatewaySendFailed` /
//!   `GatewayCancelSendFailed` commands back to the writer for state convergence.
//! - gRPC ACK = validated + accepted for async processing. Queue-full dispatch
//!   drops are handled inline by calling core.process_message(GatewaySendFailed)
//!   directly, so the order is synthetically rejected and the client is notified
//!   asynchronously. The gRPC reply is always success after core validation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::latency::{publish_latency_batch, system_time_ns, LatencyEvent, LatencyTracker};

use zk_oms_rs::{
    config::ConfdataManager,
    models::{
        CancelRecheckRequest, ExchBalanceSnapshot, ExchPositionSnapshot, OmsMessage,
        OrderRecheckRequest,
    },
    models_v2::OmsActionV2,
    oms_core_v2::OmsCoreV2,
    snapshot_v2::{
        OmsSnapshotV2, OmsSnapshotWriterV2, SnapshotManagedPosition, SnapshotOrder,
        SnapshotOrderDetail,
    },
    utils::gen_timestamp_ms,
};
use zk_proto_rs::zk::{
    exch_gw::v1::{BalanceUpdate, OrderReport},
    oms::v1::{oms_response, OmsErrorType, OmsResponse, OrderCancelRequest, OrderRequest},
};

use crate::{
    gw_client::GwClientPool,
    gw_executor::{GwAction, GwExecutorPool, GwSendLatency},
    nats_handler::NatsPublisher,
    persist_executor::{PersistAction, PersistExecutorPool},
    publish_executor::{PublishAction, PublishExecutorPool, ReportLatencyCtx},
    redis_writer::RedisWriter,
};

/// Atomic read replica — shared between the writer task and all gRPC query handlers.
pub type ReadReplica = Arc<ArcSwap<OmsSnapshotV2>>;

/// All commands the OMS writer task can receive.
pub enum OmsCommand {
    PlaceOrder {
        req: OrderRequest,
        oms_received_ns: i64,
        reply: oneshot::Sender<OmsResponse>,
    },
    BatchPlaceOrders {
        reqs: Vec<OrderRequest>,
        reply: oneshot::Sender<OmsResponse>,
    },
    CancelOrder {
        req: OrderCancelRequest,
        reply: oneshot::Sender<OmsResponse>,
    },
    BatchCancelOrders {
        reqs: Vec<OrderCancelRequest>,
        reply: oneshot::Sender<OmsResponse>,
    },
    GatewayOrderReport(OrderReport),
    GatewayBalanceUpdate(BalanceUpdate),
    Panic {
        account_id: i64,
        reply: oneshot::Sender<OmsResponse>,
    },
    DontPanic {
        account_id: i64,
        reply: oneshot::Sender<OmsResponse>,
    },
    ReloadConfig {
        new_config: ConfdataManager,
        reply: oneshot::Sender<OmsResponse>,
    },
    Cleanup {
        ts_ms: i64,
    },
    RecheckOrder(OrderRecheckRequest),
    RecheckCancel(CancelRecheckRequest),
    PositionRecheck,
    /// Gateway worker failed to send order (gRPC error). Feedback from gw_worker.
    GatewaySendFailed {
        order_id: i64,
        gw_id: u32,
        error_msg: String,
    },
    /// Gateway worker failed to send cancel (gRPC error). Feedback from gw_worker.
    GatewayCancelSendFailed {
        order_id: i64,
        gw_id: u32,
        error_msg: String,
    },
}

// ── Writer loop ──────────────────────────────────────────────────────────────

pub fn spawn_writer(
    core: OmsCoreV2,
    writer: OmsSnapshotWriterV2,
    rx: mpsc::Receiver<OmsCommand>,
    replica: ReadReplica,
    nats: NatsPublisher,
    redis: RedisWriter,
    gw_pool: GwClientPool,
    gw_executor: GwExecutorPool,
    persist_executor: PersistExecutorPool,
    publish_executor: PublishExecutorPool,
    latency_rx: mpsc::Receiver<LatencyEvent>,
    shutdown: CancellationToken,
    metrics_interval: Duration,
    metrics_max_pending: usize,
    metrics_max_complete: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(oms_writer_loop(
        core,
        writer,
        rx,
        replica,
        nats,
        redis,
        gw_pool,
        gw_executor,
        persist_executor,
        publish_executor,
        latency_rx,
        shutdown,
        metrics_interval,
        metrics_max_pending,
        metrics_max_complete,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn oms_writer_loop(
    mut core: OmsCoreV2,
    mut writer: OmsSnapshotWriterV2,
    mut rx: mpsc::Receiver<OmsCommand>,
    replica: ReadReplica,
    nats: NatsPublisher,
    mut redis: RedisWriter,
    mut gw_pool: GwClientPool,
    gw_executor: GwExecutorPool,
    mut persist_executor: PersistExecutorPool,
    mut publish_executor: PublishExecutorPool,
    mut latency_rx: mpsc::Receiver<LatencyEvent>,
    shutdown: CancellationToken,
    metrics_interval: Duration,
    metrics_max_pending: usize,
    metrics_max_complete: usize,
) {
    info!("OMS writer loop started");

    let mut tracker = LatencyTracker::new(metrics_max_pending, metrics_max_complete);
    let mut last_metrics_publish = tokio::time::Instant::now();

    loop {
        tokio::select! {
            biased;

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
                    &gw_executor,
                    &mut persist_executor,
                    &mut publish_executor,
                    &mut tracker,
                );

                // Drain latency events inline — the biased select starves
                // lower-priority branches while the cmd channel has work.
                drain_latency_events(&mut latency_rx, &mut tracker);

                // Publish latency metrics inline on the same interval.
                if last_metrics_publish.elapsed() >= metrics_interval {
                    last_metrics_publish = tokio::time::Instant::now();
                    let records = tracker.drain_complete();
                    if !records.is_empty() {
                        tracing::debug!(
                            count = records.len(),
                            pending = tracker.pending_count(),
                            "publishing latency batch",
                        );
                        let ts_ns = system_time_ns();
                        publish_latency_batch(&nats.nats, &nats.oms_id, records, ts_ns).await;
                    }
                }
            }

            // Idle-path: fires only when no commands are pending.
            _ = tokio::time::sleep(metrics_interval) => {
                drain_latency_events(&mut latency_rx, &mut tracker);
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

// ── Latency drain ───────────────────────────────────────────────────────────

/// Non-blocking drain of all pending latency events from executor workers.
fn drain_latency_events(
    latency_rx: &mut mpsc::Receiver<LatencyEvent>,
    tracker: &mut LatencyTracker,
) {
    while let Ok(event) = latency_rx.try_recv() {
        match event {
            LatencyEvent::OrderSent {
                order_id,
                order_created_at,
                t1_ns,
                t2_ns,
                tw_core_ns,
                t3_ns,
                t3r_ns,
                writer_dispatch_ts,
                gw_exec_dequeue_ns,
            } => {
                tracker.record_order_sent(
                    order_id,
                    order_created_at,
                    t1_ns,
                    t2_ns,
                    tw_core_ns,
                    t3_ns,
                    t3r_ns,
                    writer_dispatch_ts,
                    gw_exec_dequeue_ns,
                );
            }
            LatencyEvent::ReportPublished {
                order_id,
                t4_ns,
                t5_ns,
                t6_ns,
                t7_ns,
            } => {
                tracker.record_report_published(order_id, t4_ns, t5_ns, t6_ns, t7_ns);
            }
        }
    }
}

// ── Command dispatch ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn process_command(
    cmd: OmsCommand,
    core: &mut OmsCoreV2,
    writer: &mut OmsSnapshotWriterV2,
    replica: &ReadReplica,
    _nats: &NatsPublisher,
    _redis: &mut RedisWriter,
    _gw_pool: &mut GwClientPool,
    gw_executor: &GwExecutorPool,
    persist_executor: &mut PersistExecutorPool,
    publish_executor: &mut PublishExecutorPool,
    _tracker: &mut LatencyTracker,
) {
    let writer_start_ts = system_time_ns();

    // For GW report commands, capture t6 and the report fields before consuming cmd.
    let report_latency_ctx: Option<(i64, i64, i64, i64)> = match &cmd {
        OmsCommand::GatewayOrderReport(r) => {
            let t6 = system_time_ns();
            Some((
                r.order_id,
                t6,
                r.gw_received_at_ns,
                r.update_timestamp * 1_000_000,
            ))
        }
        _ => None,
    };

    let place_order_t1: i64 = match &cmd {
        OmsCommand::PlaceOrder {
            oms_received_ns, ..
        } => *oms_received_ns,
        _ => 0,
    };

    // Whether this is a place/cancel command (hot path — skip Redis persistence).
    let is_hot_path = matches!(
        &cmd,
        OmsCommand::PlaceOrder { .. }
            | OmsCommand::BatchPlaceOrders { .. }
            | OmsCommand::CancelOrder { .. }
            | OmsCommand::BatchCancelOrders { .. }
    );

    // Convert to OmsMessage (and capture reply channel if present).
    let (oms_msg, reply): (OmsMessage, Option<oneshot::Sender<OmsResponse>>) = match cmd {
        OmsCommand::PlaceOrder { req, reply, .. } => (OmsMessage::PlaceOrder(req), Some(reply)),
        OmsCommand::BatchPlaceOrders { reqs, reply } => {
            (OmsMessage::BatchPlaceOrders(reqs), Some(reply))
        }
        OmsCommand::CancelOrder { req, reply } => (OmsMessage::CancelOrder(req), Some(reply)),
        OmsCommand::BatchCancelOrders { reqs, reply } => {
            (OmsMessage::BatchCancelOrders(reqs), Some(reply))
        }
        OmsCommand::GatewayOrderReport(r) => (OmsMessage::GatewayOrderReport(r), None),
        OmsCommand::GatewayBalanceUpdate(u) => (OmsMessage::BalanceUpdate(u), None),
        OmsCommand::Panic { account_id, reply } => {
            writer.apply_panic(account_id);
            (OmsMessage::Panic { account_id }, Some(reply))
        }
        OmsCommand::DontPanic { account_id, reply } => {
            writer.apply_clear_panic(account_id);
            (OmsMessage::DontPanic { account_id }, Some(reply))
        }
        OmsCommand::ReloadConfig { new_config, reply } => {
            core.reload_config(new_config);
            let new_snap_meta = Arc::new(core.build_snapshot_metadata());
            writer.update_metadata(new_snap_meta);
            publish_snapshot(core, writer, replica, true, true);
            let resp = ok_response("Config reloaded");
            let _ = reply.send(resp);
            return;
        }
        OmsCommand::Cleanup { ts_ms } => (OmsMessage::Cleanup { ts_ms }, None),
        OmsCommand::RecheckOrder(r) => (OmsMessage::RecheckOrder(r), None),
        OmsCommand::RecheckCancel(r) => (OmsMessage::RecheckCancel(r), None),
        OmsCommand::PositionRecheck => (OmsMessage::PositionRecheck, None),
        OmsCommand::GatewaySendFailed {
            order_id,
            gw_id,
            error_msg,
        } => (
            OmsMessage::GatewaySendFailed {
                order_id,
                gw_id,
                error_msg,
            },
            None,
        ),
        OmsCommand::GatewayCancelSendFailed {
            order_id,
            gw_id,
            error_msg,
        } => (
            OmsMessage::GatewayCancelSendFailed {
                order_id,
                gw_id,
                error_msg,
            },
            None,
        ),
    };

    // ── Drive OmsCoreV2 ──────────────────────────────────────────────────────
    let actions = core.process_message(oms_msg);
    let writer_core_done_ts = system_time_ns();

    // ── Fire-and-forget action dispatch ──────────────────────────────────────
    //
    // All side effects dispatch immediately. Gateway sends are fire-and-forget;
    // failures come back as OmsCommand::GatewaySendFailed / GatewayCancelSendFailed.
    // If dispatch itself fails (queue full), we handle the failure inline by
    // calling core.process_message(GatewaySendFailed/GatewayCancelSendFailed)
    // and processing the resulting persist/publish actions. The gRPC reply is
    // always success after core validation — queue-full drops are surfaced
    // asynchronously through the normal failure path.
    //
    // On the place/cancel hot path, Redis persistence is skipped (deferred to
    // the report/failure path). Snapshot update is always immediate.
    let mut failure_actions: Vec<OmsActionV2> = Vec::new();
    let mut balances_dirty = false;
    let mut positions_dirty = false;

    for action in &actions {
        match action {
            // ── LOCAL: snapshot bookkeeping ──────────────────────────────
            OmsActionV2::PersistOrder {
                order_id,
                set_expire,
                set_closed,
            } => {
                // Snapshot update (always immediate for reader visibility)
                if let Some(live) = core.orders.get_live(*order_id) {
                    let snap_order = build_snapshot_order_from_live(live, &core.orders.dyn_strings);
                    writer.apply_order_update(snap_order, *set_closed);
                }
                if let Some(live) = core.orders.get_live(*order_id) {
                    if let Some(detail) = core.orders.get_detail(*order_id) {
                        let snap_detail = build_snapshot_detail_from_core(
                            *order_id,
                            live,
                            detail,
                            &core.metadata,
                            &core.orders.dyn_strings,
                        );
                        writer.apply_order_detail(snap_detail);
                    }
                }
                // Redis persist: skip on hot path (place/cancel), dispatch on report/failure path
                if !is_hot_path {
                    if let Some(persisted) = build_persisted_order(core, *order_id) {
                        let pa = PersistAction::Order {
                            order: persisted,
                            set_expire: *set_expire,
                            set_closed: *set_closed,
                        };
                        persist_executor.try_dispatch(pa);
                    }
                }
            }

            // ── GATEWAY: fire-and-forget dispatch ───────────────────────
            OmsActionV2::SendOrderToGw {
                gw_id,
                order_id,
                order_created_at,
            } => {
                let gw_key = core.resolve_gw_key(*gw_id).unwrap_or("").to_string();
                let request = core
                    .orders
                    .get_detail(*order_id)
                    .and_then(|d| d.last_gw_req.as_ref())
                    .map(|r| (**r).clone())
                    .unwrap_or_default();
                let writer_dispatch_ts = system_time_ns();
                let latency = Some(GwSendLatency {
                    order_id: *order_id,
                    order_created_at: *order_created_at,
                    t1_ns: place_order_t1,
                    t2_ns: writer_start_ts,
                    tw_core_ns: writer_core_done_ts,
                    writer_dispatch_ts,
                });
                debug!(
                    gw_id,
                    order_id,
                    gw_key = %gw_key,
                    instrument = %request.instrument,
                    exch_account_id = %request.exch_account_id,
                    qty = request.scaled_qty,
                    price = request.scaled_price,
                    "dispatching SendOrderToGw from OMS writer"
                );
                if let Err(_action) = gw_executor.dispatch(
                    *order_id,
                    GwAction::SendOrder {
                        gw_key,
                        request,
                        order_id: *order_id,
                        gw_id: *gw_id,
                        latency,
                    },
                ) {
                    warn!(
                        gw_id,
                        order_id,
                        "gateway executor queue full — SendOrder dropped, injecting async failure"
                    );
                    failure_actions.extend(core.process_message(OmsMessage::GatewaySendFailed {
                        order_id: *order_id,
                        gw_id: *gw_id,
                        error_msg: "dispatch queue full".into(),
                    }));
                }
            }
            OmsActionV2::BatchSendOrdersToGw { gw_id, order_ids } => {
                let gw_key = core.resolve_gw_key(*gw_id).unwrap_or("").to_string();
                // Partition order_ids by shard key so each sub-batch lands on the
                // correct shard, preserving per-order ordering guarantees.
                let sc = gw_executor.shard_count();
                let mut by_shard: Vec<Vec<i64>> = vec![vec![]; sc];
                for oid in order_ids {
                    by_shard[(*oid as u64 % sc as u64) as usize].push(*oid);
                }
                for (shard_idx, shard_oids) in by_shard.into_iter().enumerate() {
                    if shard_oids.is_empty() {
                        continue;
                    }
                    let requests: Vec<_> = shard_oids
                        .iter()
                        .filter_map(|oid| {
                            core.orders
                                .get_detail(*oid)
                                .and_then(|d| d.last_gw_req.as_ref())
                                .map(|r| (**r).clone())
                        })
                        .collect();
                    let batch_req = zk_proto_rs::zk::gateway::v1::BatchSendOrdersRequest {
                        order_requests: requests,
                    };
                    // All oids in this group hash to the same shard, so any works as route_id.
                    let route_id = shard_oids[0];
                    debug!(
                        gw_id,
                        shard_idx,
                        gw_key = %gw_key,
                        batch_size = shard_oids.len(),
                        order_ids = ?shard_oids,
                        "dispatching BatchSendOrdersToGw from OMS writer"
                    );
                    if let Err(_action) = gw_executor.dispatch(
                        route_id,
                        GwAction::BatchSendOrders {
                            gw_key: gw_key.clone(),
                            request: batch_req,
                            order_ids: shard_oids.clone(),
                            gw_id: *gw_id,
                        },
                    ) {
                        warn!(gw_id, shard_idx, "gateway executor queue full — BatchSendOrders dropped, injecting async failures");
                        for oid in &shard_oids {
                            failure_actions.extend(core.process_message(
                                OmsMessage::GatewaySendFailed {
                                    order_id: *oid,
                                    gw_id: *gw_id,
                                    error_msg: "dispatch queue full".into(),
                                },
                            ));
                        }
                    }
                }
            }
            OmsActionV2::SendCancelToGw { gw_id, order_id } => {
                let gw_key = core.resolve_gw_key(*gw_id).unwrap_or("").to_string();
                let request = core
                    .orders
                    .get_detail(*order_id)
                    .and_then(|d| d.cancel_req.as_ref())
                    .map(|r| (**r).clone())
                    .unwrap_or_default();
                debug!(
                    gw_id,
                    order_id,
                    gw_key = %gw_key,
                    exch_order_ref = %request.exch_order_ref,
                    "dispatching SendCancelToGw from OMS writer"
                );
                if let Err(_action) = gw_executor.dispatch(
                    *order_id,
                    GwAction::CancelOrder {
                        gw_key,
                        request,
                        order_id: *order_id,
                        gw_id: *gw_id,
                    },
                ) {
                    warn!(gw_id, order_id, "gateway executor queue full — CancelOrder dropped, injecting async failure");
                    failure_actions.extend(core.process_message(
                        OmsMessage::GatewayCancelSendFailed {
                            order_id: *order_id,
                            gw_id: *gw_id,
                            error_msg: "dispatch queue full".into(),
                        },
                    ));
                }
            }
            OmsActionV2::BatchCancelToGw { gw_id, order_ids } => {
                let gw_key = core.resolve_gw_key(*gw_id).unwrap_or("").to_string();
                // Partition by shard key — same pattern as BatchSendOrdersToGw.
                let sc = gw_executor.shard_count();
                let mut by_shard: Vec<Vec<i64>> = vec![vec![]; sc];
                for oid in order_ids {
                    by_shard[(*oid as u64 % sc as u64) as usize].push(*oid);
                }
                for (shard_idx, shard_oids) in by_shard.into_iter().enumerate() {
                    if shard_oids.is_empty() {
                        continue;
                    }
                    let requests: Vec<_> = shard_oids
                        .iter()
                        .filter_map(|oid| {
                            core.orders
                                .get_detail(*oid)
                                .and_then(|d| d.cancel_req.as_ref())
                                .map(|r| (**r).clone())
                        })
                        .collect();
                    let batch_req = zk_proto_rs::zk::gateway::v1::BatchCancelOrdersRequest {
                        cancel_requests: requests,
                    };
                    let route_id = shard_oids[0];
                    debug!(
                        gw_id,
                        shard_idx,
                        gw_key = %gw_key,
                        batch_size = shard_oids.len(),
                        order_ids = ?shard_oids,
                        "dispatching BatchCancelToGw from OMS writer"
                    );
                    if let Err(_action) = gw_executor.dispatch(
                        route_id,
                        GwAction::BatchCancel {
                            gw_key: gw_key.clone(),
                            request: batch_req,
                            order_ids: shard_oids.clone(),
                            gw_id: *gw_id,
                        },
                    ) {
                        warn!(gw_id, shard_idx, "gateway executor queue full — BatchCancel dropped, injecting async failures");
                        for oid in &shard_oids {
                            failure_actions.extend(core.process_message(
                                OmsMessage::GatewayCancelSendFailed {
                                    order_id: *oid,
                                    gw_id: *gw_id,
                                    error_msg: "dispatch queue full".into(),
                                },
                            ));
                        }
                    }
                }
            }

            // ── PERSIST (balance/position) — always immediate ───────────
            OmsActionV2::PersistBalance {
                account_id,
                asset_id,
            } => {
                if let Some(pa) = build_persist_balance(core, *account_id, *asset_id) {
                    persist_executor.try_dispatch(pa);
                }
            }
            OmsActionV2::PersistPosition {
                account_id,
                instrument_id,
            } => {
                if let Some(pa) = build_persist_position(core, *account_id, *instrument_id) {
                    persist_executor.try_dispatch(pa);
                }
            }

            // ── PUBLISH — always immediate ──────────────────────────────
            OmsActionV2::PublishOrderUpdate {
                order_id,
                include_last_trade,
                include_last_fee,
                include_exec_message,
                include_inferred_trade,
            } => {
                if let Some(event) = core.build_order_update_event(
                    *order_id,
                    *include_last_trade,
                    *include_last_fee,
                    *include_exec_message,
                    *include_inferred_trade,
                ) {
                    let latency_ctx =
                        report_latency_ctx.map(|(oid, t6, t4, t5)| ReportLatencyCtx {
                            order_id: oid,
                            t4_ns: t4,
                            t5_ns: t5,
                            t6_ns: t6,
                        });
                    let pa = PublishAction::OrderUpdate { event, latency_ctx };
                    publish_executor.dispatch(*order_id, pa);
                }
            }
            OmsActionV2::PublishBalanceUpdate { account_id } => {
                balances_dirty = true;
                if let Some(event) = core.build_balance_update_event(*account_id) {
                    let pa = PublishAction::BalanceUpdate { event };
                    publish_executor.dispatch(*account_id, pa);
                }
            }
            OmsActionV2::PublishPositionUpdate { account_id } => {
                positions_dirty = true;
                if let Some(event) = core.build_position_update_event(*account_id) {
                    let pa = PublishAction::PositionUpdate { event };
                    publish_executor.dispatch(*account_id, pa);
                }
            }
        }
    }

    // ── Process inline failure actions (from queue-full dispatch drops) ────────
    //
    // These are persist/publish actions from core.process_message(GatewaySendFailed/
    // GatewayCancelSendFailed) called inline above. They never contain gateway
    // dispatch actions, so no recursion risk.
    for action in &failure_actions {
        match action {
            OmsActionV2::PersistOrder {
                order_id,
                set_expire,
                set_closed,
            } => {
                if let Some(live) = core.orders.get_live(*order_id) {
                    let snap_order = build_snapshot_order_from_live(live, &core.orders.dyn_strings);
                    writer.apply_order_update(snap_order, *set_closed);
                }
                if let Some(live) = core.orders.get_live(*order_id) {
                    if let Some(detail) = core.orders.get_detail(*order_id) {
                        let snap_detail = build_snapshot_detail_from_core(
                            *order_id,
                            live,
                            detail,
                            &core.metadata,
                            &core.orders.dyn_strings,
                        );
                        writer.apply_order_detail(snap_detail);
                    }
                }
                if let Some(persisted) = build_persisted_order(core, *order_id) {
                    let pa = PersistAction::Order {
                        order: persisted,
                        set_expire: *set_expire,
                        set_closed: *set_closed,
                    };
                    persist_executor.try_dispatch(pa);
                }
            }
            OmsActionV2::PublishOrderUpdate {
                order_id,
                include_last_trade,
                include_last_fee,
                include_exec_message,
                include_inferred_trade,
            } => {
                if let Some(event) = core.build_order_update_event(
                    *order_id,
                    *include_last_trade,
                    *include_last_fee,
                    *include_exec_message,
                    *include_inferred_trade,
                ) {
                    let pa = PublishAction::OrderUpdate {
                        event,
                        latency_ctx: None,
                    };
                    publish_executor.dispatch(*order_id, pa);
                }
            }
            OmsActionV2::PersistBalance {
                account_id,
                asset_id,
            } => {
                if let Some(pa) = build_persist_balance(core, *account_id, *asset_id) {
                    persist_executor.try_dispatch(pa);
                }
            }
            OmsActionV2::PublishBalanceUpdate { account_id } => {
                balances_dirty = true;
                if let Some(event) = core.build_balance_update_event(*account_id) {
                    let pa = PublishAction::BalanceUpdate { event };
                    publish_executor.dispatch(*account_id, pa);
                }
            }
            _ => {} // No other action types expected from failure path
        }
    }

    // ── Publish updated snapshot (synchronous, writer-owned) ─────────────────
    publish_snapshot(core, writer, replica, balances_dirty, positions_dirty);

    // ── Reply immediately ────────────────────────────────────────────────────
    // ACK = validated + accepted for async processing. Queue-full dispatch
    // drops are handled inline above (synthetic rejection via core).
    send_reply(reply, true, "");
}

/// Send the gRPC reply if a reply channel exists.
fn send_reply(reply: Option<oneshot::Sender<OmsResponse>>, success: bool, err_msg: &str) {
    if let Some(tx) = reply {
        let resp = if success {
            ok_response("")
        } else {
            err_response(OmsErrorType::OmsErrTypeInternalError, err_msg)
        };
        if tx.send(resp).is_err() {
            warn!("gRPC caller dropped — OmsResponse not delivered");
        }
    }
}

/// Build a PersistAction::Balance from core state.
fn build_persist_balance(
    core: &OmsCoreV2,
    account_id: i64,
    asset_id: u32,
) -> Option<PersistAction> {
    let asset_name = core.resolve_asset_name(asset_id).to_string();
    let snap = core.balances.get_balance(account_id, asset_id)?;
    Some(PersistAction::Balance {
        account_id,
        asset_name,
        snap: snap.clone(),
    })
}

/// Build a PersistAction::Position from core state.
fn build_persist_position(
    core: &OmsCoreV2,
    account_id: i64,
    instrument_id: u32,
) -> Option<PersistAction> {
    let inst_code = core.resolve_instrument_code(instrument_id).to_string();
    let pos = core.positions.get_position(account_id, instrument_id)?;
    let side = if pos.is_short { "SHORT" } else { "LONG" };
    let managed_pos = zk_oms_rs::models::OmsManagedPosition {
        account_id: pos.account_id,
        instrument_code: inst_code.clone(),
        symbol_exch: None,
        instrument_type: pos.instrument_type,
        is_short: pos.is_short,
        qty_total: pos.qty_total,
        qty_frozen: pos.qty_frozen,
        qty_available: pos.qty_available,
        last_local_update_ts: pos.last_local_update_ts,
        last_exch_sync_ts: pos.last_exch_sync_ts,
        reconcile_status: pos.reconcile_status,
        last_exch_qty: pos.last_exch_qty,
        first_diverged_ts: pos.first_diverged_ts,
        divergence_count: pos.divergence_count,
    };
    Some(PersistAction::Position {
        account_id,
        inst_code,
        side: side.to_string(),
        pos: managed_pos,
    })
}

// ── Shared Redis persistence helpers ─────────────────────────────────────────
// Used by both the writer loop and startup reconciliation in main.rs.

/// Persist a balance snapshot to Redis.
pub async fn persist_balance_to_redis(
    core: &OmsCoreV2,
    redis: &mut RedisWriter,
    account_id: i64,
    asset_id: u32,
) {
    let asset_name = core.resolve_asset_name(asset_id).to_string();
    if let Some(snap) = core.balances.get_balance(account_id, asset_id) {
        if let Err(e) = redis.write_balance(account_id, &asset_name, snap).await {
            warn!(account_id, asset = %asset_name, error = %e, "Redis write_balance failed");
        }
    }
}

/// Persist a managed position to Redis.
pub async fn persist_position_to_redis(
    core: &OmsCoreV2,
    redis: &mut RedisWriter,
    account_id: i64,
    instrument_id: u32,
) {
    let inst_code = core.resolve_instrument_code(instrument_id).to_string();
    if let Some(pos) = core.positions.get_position(account_id, instrument_id) {
        let side = if pos.is_short { "SHORT" } else { "LONG" };
        let managed_pos = zk_oms_rs::models::OmsManagedPosition {
            account_id: pos.account_id,
            instrument_code: inst_code.clone(),
            symbol_exch: None,
            instrument_type: pos.instrument_type,
            is_short: pos.is_short,
            qty_total: pos.qty_total,
            qty_frozen: pos.qty_frozen,
            qty_available: pos.qty_available,
            last_local_update_ts: pos.last_local_update_ts,
            last_exch_sync_ts: pos.last_exch_sync_ts,
            reconcile_status: pos.reconcile_status,
            last_exch_qty: pos.last_exch_qty,
            first_diverged_ts: pos.first_diverged_ts,
            divergence_count: pos.divergence_count,
        };
        if let Err(e) = redis
            .write_position(account_id, &inst_code, side, &managed_pos)
            .await
        {
            warn!(account_id, instrument_code = %inst_code, error = %e, "Redis write_position failed");
        }
    }
}

// ── Snapshot helper ──────────────────────────────────────────────────────────

fn publish_snapshot(
    core: &OmsCoreV2,
    writer: &mut OmsSnapshotWriterV2,
    replica: &ReadReplica,
    balances_dirty: bool,
    positions_dirty: bool,
) {
    let (managed, exch_pos, unknown_pos) = if positions_dirty {
        let (resolved, unknown) = build_exch_positions(core);
        (build_managed_positions(core), resolved, unknown)
    } else {
        let prev = replica.load();
        (
            prev.managed_positions.clone(),
            prev.exch_positions.clone(),
            prev.unknown_exch_positions.clone(),
        )
    };
    let (exch_bal, unknown_bal) = if balances_dirty {
        build_exch_balances(core)
    } else {
        let prev = replica.load();
        (
            prev.exch_balances.clone(),
            prev.unknown_exch_balances.clone(),
        )
    };
    let snap = writer.publish(
        managed,
        exch_pos,
        exch_bal,
        unknown_pos,
        unknown_bal,
        gen_timestamp_ms(),
    );
    replica.store(Arc::new(snap));
}

/// Build managed position map from V2 position store.
pub fn build_managed_positions(core: &OmsCoreV2) -> HashMap<(i64, u32), SnapshotManagedPosition> {
    let mut map = HashMap::new();
    for pos in core.positions.all_positions() {
        map.insert(
            (pos.account_id, pos.instrument_id),
            SnapshotManagedPosition {
                account_id: pos.account_id,
                instrument_id: pos.instrument_id,
                instrument_type: pos.instrument_type,
                is_short: pos.is_short,
                qty_total: pos.qty_total,
                qty_frozen: pos.qty_frozen,
                qty_available: pos.qty_available,
                last_local_update_ts: pos.last_local_update_ts,
                last_exch_sync_ts: pos.last_exch_sync_ts,
                reconcile_status: pos.reconcile_status,
            },
        );
    }
    map
}

/// Build exchange position map from V2 position store.
/// Returns (resolved map, unknown overflow vec).
pub fn build_exch_positions(
    core: &OmsCoreV2,
) -> (
    HashMap<(i64, u32), ExchPositionSnapshot>,
    Vec<ExchPositionSnapshot>,
) {
    let mut resolved = HashMap::new();
    let mut unknown = Vec::new();
    for snap in core.positions.all_exch_positions() {
        if let Some(inst) = core.metadata.resolve_instrument(&snap.instrument_code) {
            resolved.insert((snap.account_id, inst.instrument_id), snap.clone());
        } else {
            tracing::warn!(
                account_id = snap.account_id,
                instrument = %snap.instrument_code,
                "exchange position not in metadata — stored in overflow bucket"
            );
            unknown.push(snap.clone());
        }
    }
    (resolved, unknown)
}

/// Build exchange balance map from V2 balance store.
/// Returns (resolved map, unknown overflow vec).
pub fn build_exch_balances(
    core: &OmsCoreV2,
) -> (
    HashMap<(i64, u32), ExchBalanceSnapshot>,
    Vec<ExchBalanceSnapshot>,
) {
    let mut resolved = HashMap::new();
    let mut unknown = Vec::new();
    for snap in core.balances.all_balances() {
        let asset_sym = core.metadata.strings.lookup(&snap.asset);
        let asset_id = asset_sym.and_then(|s| core.metadata.asset_by_symbol.get(&s).copied());
        if let Some(id) = asset_id {
            resolved.insert((snap.account_id, id), snap.clone());
        } else {
            tracing::warn!(
                account_id = snap.account_id,
                asset = %snap.asset,
                "exchange balance not in metadata — stored in overflow bucket"
            );
            unknown.push(snap.clone());
        }
    }
    (resolved, unknown)
}

// ── SnapshotOrder builders ──────────────────────────────────────────────────

pub fn build_snapshot_order_from_live(
    live: &zk_oms_rs::models_v2::LiveOrder,
    dyn_strings: &zk_oms_rs::models_v2::DynStringTable,
) -> SnapshotOrder {
    SnapshotOrder {
        order_id: live.order_id,
        account_id: live.account_id,
        instrument_id: live.instrument_id,
        gw_id: live.gw_id,
        source_sym: live.source_sym,
        order_status: live.order_status,
        buy_sell_type: live.buy_sell_type,
        open_close_type: live.open_close_type,
        order_type: live.order_type,
        tif_type: live.tif_type,
        price: live.price,
        qty: live.qty,
        filled_qty: live.filled_qty,
        filled_avg_price: live.filled_avg_price,
        exch_order_ref: live
            .exch_order_ref_id
            .map(|id| dyn_strings.resolve(id).into()),
        created_at: live.created_at,
        updated_at: live.updated_at,
        snapshot_version: live.snapshot_version,
        is_external: live.is_external,
    }
}

pub fn build_snapshot_detail_from_core(
    order_id: i64,
    live: &zk_oms_rs::models_v2::LiveOrder,
    detail: &zk_oms_rs::models_v2::OrderDetailLog,
    metadata: &zk_oms_rs::metadata_v2::MetadataTables,
    dyn_strings: &zk_oms_rs::models_v2::DynStringTable,
) -> SnapshotOrderDetail {
    SnapshotOrderDetail {
        order_id,
        source_sym: live.source_sym,
        instrument_exch_sym: metadata
            .instrument(live.instrument_id)
            .map(|i| i.instrument_exch_sym)
            .unwrap_or(0),
        exch_order_ref: live
            .exch_order_ref_id
            .map(|id| dyn_strings.resolve(id).into()),
        error_msg: live.error_msg.clone().into_boxed_str(),
        trades: detail.trades.clone(),
        inferred_trades: detail.inferred_trades.clone(),
        exec_msgs: detail.exec_msgs.clone(),
        fees: detail.fees.clone(),
    }
}

/// Build a V1 PersistedOrder from V2 core state, for Redis persistence.
fn build_persisted_order(core: &OmsCoreV2, order_id: i64) -> Option<zk_oms_rs::models::OmsOrder> {
    let live = core.orders.get_live(order_id)?;
    let detail = core.orders.get_detail(order_id);
    let order_state = core.build_proto_order(live);

    Some(zk_oms_rs::models::OmsOrder {
        is_from_external: live.is_external,
        order_id: live.order_id,
        account_id: live.account_id,
        exch_order_ref: live
            .exch_order_ref_id
            .map(|id| core.orders.dyn_strings.resolve(id).to_string()),
        oms_req: detail.and_then(|d| d.original_req.as_ref().map(|r| (**r).clone())),
        gw_req: detail.and_then(|d| d.last_gw_req.as_ref().map(|r| (**r).clone())),
        cancel_req: detail.and_then(|d| d.cancel_req.as_ref().map(|r| (**r).clone())),
        order_state,
        trades: detail.map(|d| d.trades.clone()).unwrap_or_default(),
        acc_trades_filled_qty: live.acc_trades_filled_qty,
        acc_trades_value: live.acc_trades_value,
        order_inferred_trades: detail
            .map(|d| d.inferred_trades.clone())
            .unwrap_or_default(),
        exec_msgs: detail.map(|d| d.exec_msgs.clone()).unwrap_or_default(),
        fees: detail.map(|d| d.fees.clone()).unwrap_or_default(),
        cancel_attempts: live.cancel_attempts,
    })
}

// ── OMSResponse helpers ──────────────────────────────────────────────────────

pub fn ok_response(msg: &str) -> OmsResponse {
    OmsResponse {
        status: oms_response::Status::OmsRespStatusSuccess as i32,
        error_type: OmsErrorType::OmsErrTypeUnspecified as i32,
        message: msg.to_string(),
        timestamp: gen_timestamp_ms(),
    }
}

pub fn err_response(error_type: OmsErrorType, msg: &str) -> OmsResponse {
    OmsResponse {
        status: oms_response::Status::OmsRespStatusFail as i32,
        error_type: error_type as i32,
        message: msg.to_string(),
        timestamp: gen_timestamp_ms(),
    }
}

// ── Utility: send command and await response ─────────────────────────────────

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
