//! Gateway internal execution pool — sharded by `order_id`.
//!
//! The gRPC handler enqueues actions here and replies immediately ("accepted").
//! gRPC success = validated + accepted for async processing. It does NOT
//! guarantee the request was enqueued to a worker or sent to the venue.
//!
//! Worker tasks drain shards and call the venue adapter asynchronously.
//! On adapter failure or queue-full drops, a synthetic rejection OrderReport
//! is published directly to NATS so the OMS receives it through the normal
//! report path.
//!
//! Shard index = `order_id % shard_count`, so place/cancel for the same
//! order always land on the same worker (preserving per-order ordering).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tracing::{info, warn};

use zk_proto_rs::zk::exch_gw::v1::{
    order_report_entry, OrderReport, OrderReportEntry, OrderReportType,
};

use crate::nats_publisher::NatsPublisher;
use crate::venue_adapter::{VenueAdapter, VenueCancelOrder, VenuePlaceOrder};

// ── Action types ────────────────────────────────────────────────────────────

/// Action to be executed by a gateway shard worker.
#[derive(Debug)]
pub enum GwExecAction {
    PlaceOrder {
        venue_req: VenuePlaceOrder,
        correlation_id: i64,
    },
    CancelOrder {
        venue_req: VenueCancelOrder,
        order_id: i64,
    },
}

// ── Executor pool ───────────────────────────────────────────────────────────

/// Gateway-internal execution pool. Sharded by `order_id % shard_count`.
///
/// All shards are pre-spawned at construction — `dispatch` is `&self`.
pub struct GwExecPool {
    shards: Vec<mpsc::Sender<GwExecAction>>,
    shard_count: usize,
    queue_capacity: usize,
    /// Kept for publishing synthetic rejection reports on queue-full drops.
    nats_publisher: Arc<NatsPublisher>,
    gw_id: String,
    account_id: i64,
}

impl GwExecPool {
    /// Create a new execution pool with `shard_count` pre-spawned workers.
    pub fn new(
        shard_count: usize,
        queue_capacity: usize,
        adapter: Arc<dyn VenueAdapter>,
        nats_publisher: Arc<NatsPublisher>,
        gw_id: String,
        account_id: i64,
    ) -> Self {
        let mut shards = Vec::with_capacity(shard_count);
        for shard_id in 0..shard_count {
            let (tx, rx) = mpsc::channel(queue_capacity);
            tokio::spawn(gw_exec_worker(
                shard_id,
                rx,
                Arc::clone(&adapter),
                Arc::clone(&nats_publisher),
                gw_id.clone(),
                account_id,
            ));
            shards.push(tx);
        }

        info!(shard_count, queue_capacity, "GwExecPool started");

        Self {
            shards,
            shard_count,
            queue_capacity,
            nats_publisher,
            gw_id,
            account_id,
        }
    }

    /// Dispatch an action to the shard for `order_id`.
    ///
    /// Returns `Err(action)` if the shard queue is full.
    pub fn dispatch(&self, order_id: i64, action: GwExecAction) -> Result<(), GwExecAction> {
        let shard = (order_id as u64 % self.shard_count as u64) as usize;
        match self.shards[shard].try_send(action) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(a)) => Err(a),
            Err(mpsc::error::TrySendError::Closed(a)) => {
                warn!(shard, "gw exec shard channel closed");
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

    /// Dispatch an action, publishing a synthetic rejection if the queue is full.
    ///
    /// Always "succeeds" from the caller's perspective — queue-full drops are
    /// reported asynchronously via a synthetic rejection OrderReport to NATS.
    pub fn dispatch_or_reject(&self, order_id: i64, action: GwExecAction) {
        if let Err(action) = self.dispatch(order_id, action) {
            let pub_ = Arc::clone(&self.nats_publisher);
            let gw_id = self.gw_id.clone();
            let account_id = self.account_id;
            warn!(
                order_id,
                "execution queue full — publishing synthetic rejection"
            );
            tokio::spawn(async move {
                let report = match &action {
                    GwExecAction::PlaceOrder { correlation_id, .. } => build_rejection_report(
                        *correlation_id,
                        &gw_id,
                        account_id,
                        "execution queue full",
                    ),
                    GwExecAction::CancelOrder { order_id, .. } => build_cancel_rejection_report(
                        *order_id,
                        &gw_id,
                        account_id,
                        "execution queue full",
                    ),
                };
                pub_.publish_order_report(&report).await;
            });
        }
    }
}

// ── Worker loop ─────────────────────────────────────────────────────────────

async fn gw_exec_worker(
    shard_id: usize,
    mut rx: mpsc::Receiver<GwExecAction>,
    adapter: Arc<dyn VenueAdapter>,
    nats_publisher: Arc<NatsPublisher>,
    gw_id: String,
    account_id: i64,
) {
    while let Some(action) = rx.recv().await {
        match action {
            GwExecAction::PlaceOrder {
                venue_req,
                correlation_id,
            } => {
                match adapter.place_order(venue_req).await {
                    Ok(ack) if !ack.success => {
                        // Adapter returned a logical rejection (not a transport error).
                        let msg = ack.error_message.unwrap_or_default();
                        warn!(
                            shard_id,
                            correlation_id, msg, "adapter rejected place_order"
                        );
                        let report =
                            build_rejection_report(correlation_id, &gw_id, account_id, &msg);
                        nats_publisher.publish_order_report(&report).await;
                    }
                    Err(e) => {
                        // Transport / adapter error after queue acceptance.
                        warn!(shard_id, correlation_id, error = %e, "adapter place_order failed");
                        let report = build_rejection_report(
                            correlation_id,
                            &gw_id,
                            account_id,
                            &e.to_string(),
                        );
                        nats_publisher.publish_order_report(&report).await;
                    }
                    Ok(_) => {
                        // Success — normal reports flow through the event_tx path.
                    }
                }
            }

            GwExecAction::CancelOrder {
                venue_req,
                order_id,
            } => match adapter.cancel_order(venue_req).await {
                Ok(ack) if !ack.success => {
                    let msg = ack.error_message.unwrap_or_default();
                    warn!(shard_id, order_id, msg, "adapter rejected cancel_order");
                    let report = build_cancel_rejection_report(order_id, &gw_id, account_id, &msg);
                    nats_publisher.publish_order_report(&report).await;
                }
                Err(e) => {
                    warn!(shard_id, order_id, error = %e, "adapter cancel_order failed");
                    let report =
                        build_cancel_rejection_report(order_id, &gw_id, account_id, &e.to_string());
                    nats_publisher.publish_order_report(&report).await;
                }
                Ok(_) => {}
            },
        }
    }
}

// ── Rejection report builders ───────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Build a synthetic rejection OrderReport for a failed place order.
fn build_rejection_report(
    order_id: i64,
    gw_id: &str,
    account_id: i64,
    error_msg: &str,
) -> OrderReport {
    let ts = now_ms();
    OrderReport {
        exchange: gw_id.to_string(),
        account_id,
        order_id,
        update_timestamp: ts,
        gw_received_at_ns: 0,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeExec as i32,
            report: Some(order_report_entry::Report::ExecReport(
                zk_proto_rs::zk::exch_gw::v1::ExecReport {
                    exec_type: zk_proto_rs::zk::exch_gw::v1::ExchExecType::Rejected as i32,
                    rejection_info: Some(zk_proto_rs::zk::common::v1::Rejection {
                        reason:
                            zk_proto_rs::zk::common::v1::RejectionReason::RejReasonGwInternalError
                                as i32,
                        error_message: error_msg.to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
        }],
        ..Default::default()
    }
}

/// Build a synthetic rejection OrderReport for a failed cancel.
fn build_cancel_rejection_report(
    order_id: i64,
    gw_id: &str,
    account_id: i64,
    error_msg: &str,
) -> OrderReport {
    let ts = now_ms();
    OrderReport {
        exchange: gw_id.to_string(),
        account_id,
        order_id,
        update_timestamp: ts,
        gw_received_at_ns: 0,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeExec as i32,
            report: Some(order_report_entry::Report::ExecReport(
                zk_proto_rs::zk::exch_gw::v1::ExecReport {
                    exec_type: zk_proto_rs::zk::exch_gw::v1::ExchExecType::CancelReject as i32,
                    rejection_info: Some(zk_proto_rs::zk::common::v1::Rejection {
                        reason:
                            zk_proto_rs::zk::common::v1::RejectionReason::RejReasonGwInternalError
                                as i32,
                        error_message: error_msg.to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
        }],
        ..Default::default()
    }
}
