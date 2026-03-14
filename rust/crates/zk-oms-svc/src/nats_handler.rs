//! NATS message publisher and gateway report subscriber.
//!
//! ## Publisher
//!
//! `NatsPublisher` is a clone-friendly handle used by the writer loop to push
//! `OrderUpdateEvent` and `PositionUpdateEvent` messages to downstream consumers
//! (strategies, monitoring).  Each message is proto-encoded and published as raw
//! bytes — consumers decode with prost.
//!
//! ## Subscriber
//!
//! `spawn_gw_subscriber` launches a tokio task that listens on
//! `zk.gw.{gw_id}.report` and `zk.gw.{gw_id}.balance` subjects, decodes the
//! incoming proto payloads, and forwards them to the OMS writer task via the
//! command channel.
//!
//! # Late-event handling
//! NATS provides at-most-once delivery.  Late or out-of-order reports are handled
//! by `OmsCore::process_order_report` which buffers them in `pending_order_reports`
//! and replays on linkage.  No extra buffering is done here.

use std::sync::Arc;

use async_nats::{Client as NatsClient, Message};
use bytes::Bytes;
use futures::StreamExt;
use prost::Message as ProstMessage;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use zk_infra_rs::nats::subject;
use zk_proto_rs::zk::{
    exch_gw::v1::{BalanceUpdate, OrderReport},
    oms::v1::{OrderUpdateEvent, PositionUpdateEvent},
};

use crate::oms_actor::OmsCommand;

// ── Publisher ────────────────────────────────────────────────────────────────

/// Clone-friendly handle for publishing OMS events to NATS.
#[derive(Clone)]
pub struct NatsPublisher {
    pub nats:   NatsClient,
    pub oms_id: Arc<String>,
}

impl NatsPublisher {
    pub fn new(nats: NatsClient, oms_id: impl Into<String>) -> Self {
        Self { nats, oms_id: Arc::new(oms_id.into()) }
    }

    /// Publish a proto-encoded message to `subject`.
    pub async fn publish_proto<M: ProstMessage>(&self, subject: String, msg: &M) {
        let bytes = Bytes::from(msg.encode_to_vec());
        if let Err(e) = self.nats.publish(subject.clone(), bytes).await {
            warn!(subject, error = %e, "NATS publish failed");
        }
    }

    /// Publish `OrderUpdateEvent` to per-account topic.
    pub async fn publish_order_update(&self, event: &OrderUpdateEvent) {
        let subj = subject::oms_order_update(&self.oms_id, event.account_id);
        self.publish_proto(subj, event).await;
    }

    /// Publish `PositionUpdateEvent` to the OMS balance topic.
    pub async fn publish_position_update(&self, event: &PositionUpdateEvent) {
        let subj = subject::oms_balance_update(&self.oms_id);
        self.publish_proto(subj, event).await;
    }
}

// ── Subscriber ───────────────────────────────────────────────────────────────

/// Spawn tasks that subscribe to gateway report subjects for the given `gw_id`
/// and forward decoded messages to `cmd_tx`.
///
/// Returns handles to both spawned tasks (report + balance).  Abort them to
/// stop the subscriptions on gateway disconnect.
pub async fn spawn_gw_subscriber(
    nats:   &NatsClient,
    gw_id:  &str,
    cmd_tx: mpsc::Sender<OmsCommand>,
) -> Result<Vec<tokio::task::JoinHandle<()>>, async_nats::SubscribeError> {
    let report_subj  = subject::gw_report(gw_id);
    let balance_subj = subject::gw_balance(gw_id);

    let mut report_sub  = nats.subscribe(report_subj.clone()).await?;
    let mut balance_sub = nats.subscribe(balance_subj.clone()).await?;

    // ── Order report task ───────────────────────────────────────────────────
    let tx1 = cmd_tx.clone();
    let gw_id_owned = gw_id.to_string();
    let report_handle = tokio::spawn(async move {
        while let Some(msg) = report_sub.next().await {
            handle_order_report_msg(msg, &tx1, &gw_id_owned).await;
        }
        warn!(gw_id = gw_id_owned, "order report subscription ended");
    });

    // ── Balance update task ─────────────────────────────────────────────────
    let tx2 = cmd_tx;
    let gw_id_owned2 = gw_id.to_string();
    let balance_handle = tokio::spawn(async move {
        while let Some(msg) = balance_sub.next().await {
            handle_balance_msg(msg, &tx2, &gw_id_owned2).await;
        }
        warn!(gw_id = gw_id_owned2, "balance subscription ended");
    });

    Ok(vec![report_handle, balance_handle])
}

async fn handle_order_report_msg(
    msg:    Message,
    cmd_tx: &mpsc::Sender<OmsCommand>,
    gw_id:  &str,
) {
    match OrderReport::decode(msg.payload.as_ref()) {
        Ok(report) => {
            debug!(gw_id, order_ref = report.exch_order_ref, "received OrderReport");
            if cmd_tx.send(OmsCommand::GatewayOrderReport(report)).await.is_err() {
                error!(gw_id, "OMS command channel closed — dropping order report");
            }
        }
        Err(e) => {
            warn!(gw_id, error = %e, "failed to decode OrderReport from NATS");
        }
    }
}

async fn handle_balance_msg(
    msg:    Message,
    cmd_tx: &mpsc::Sender<OmsCommand>,
    gw_id:  &str,
) {
    match BalanceUpdate::decode(msg.payload.as_ref()) {
        Ok(update) => {
            debug!(gw_id, "received BalanceUpdate");
            if cmd_tx.send(OmsCommand::GatewayBalanceUpdate(update)).await.is_err() {
                error!(gw_id, "OMS command channel closed — dropping balance update");
            }
        }
        Err(e) => {
            warn!(gw_id, error = %e, "failed to decode BalanceUpdate from NATS");
        }
    }
}
