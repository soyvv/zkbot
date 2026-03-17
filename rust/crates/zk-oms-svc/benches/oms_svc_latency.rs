//! In-process latency benchmarks for the OMS writer actor.
//!
//! Measures the round-trip time of `OmsCommand` → writer task → `OmsResponse`
//! using a real mpsc channel but with no NATS/Redis/GW I/O.
//!
//! Run:
//!     cargo bench --package zk-oms-svc --bench oms_svc_latency
//!
//! HTML report in `target/criterion/`.

use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use arc_swap::ArcSwap;
use criterion::{criterion_group, criterion_main, Criterion};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use zk_oms_rs::{
    config::ConfdataManager,
    models::OmsMessage,
    oms_core::OmsCore,
    utils::gen_timestamp_ms,
};
use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::{
        common::v1::{BasicOrderType, BuySellType, InstrumentRefData, OpenCloseType},
        oms::v1::{OmsErrorType, OmsResponse, OrderCancelRequest, OrderRequest},
    },
};
use zk_oms_svc::oms_actor::{self, OmsCommand, ReadReplica};

// ── Shared order counter ──────────────────────────────────────────────────────

static ORDER_CTR: AtomicI64 = AtomicI64::new(1_000_000);

fn next_id() -> i64 {
    ORDER_CTR.fetch_add(1, Ordering::Relaxed)
}

// ── Setup helpers ─────────────────────────────────────────────────────────────

const ACCOUNT_ID: i64 = 9001;
const GW_KEY: &str = "gw_mock_1";
const INSTRUMENT: &str = "BTC-USDT";

fn bench_confdata() -> ConfdataManager {
    let oms_cfg = OmsConfigEntry {
        oms_id:              "bench".into(),
        managed_account_ids: vec![ACCOUNT_ID],
        ..Default::default()
    };
    let route = OmsRouteEntry {
        account_id:      ACCOUNT_ID,
        gw_key:          GW_KEY.into(),
        exch_account_id: "BENCH_ACCT".into(),
        ..Default::default()
    };
    let gw = GwConfigEntry {
        gw_key:    GW_KEY.into(),
        exch_name: "BENCH_EXCH".into(),
        rpc_endpoint: "localhost:9999".into(),
        ..Default::default()
    };
    let refdata = InstrumentRefData {
        instrument_id:          INSTRUMENT.into(),
        instrument_id_exchange: INSTRUMENT.into(),
        exchange_name:          "BENCH_EXCH".into(),
        instrument_type:        1, // SPOT
        ..Default::default()
    };
    ConfdataManager::new(oms_cfg, vec![route], vec![gw], vec![refdata], vec![])
}

fn make_order_req(order_id: i64) -> OrderRequest {
    OrderRequest {
        order_id,
        account_id:      ACCOUNT_ID,
        instrument_code: INSTRUMENT.into(),
        buy_sell_type:   BuySellType::BsBuy as i32,
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type:      BasicOrderType::OrdertypeLimit as i32,
        price:           50_000.0,
        qty:             0.01,
        source_id:       "bench".into(),
        timestamp:       1_000_000,
        ..Default::default()
    }
}

fn make_cancel_req(order_id: i64, exch_order_ref: &str) -> OrderCancelRequest {
    OrderCancelRequest {
        order_id,
        exch_order_ref: exch_order_ref.into(),
        timestamp:      1_000_001,
        ..Default::default()
    }
}

/// Spawn a lightweight writer loop with no I/O — suitable for benchmarking.
fn spawn_bench_actor(
    rt: &tokio::runtime::Runtime,
) -> (mpsc::Sender<OmsCommand>, ReadReplica, CancellationToken) {
    let confdata = bench_confdata();
    let core = OmsCore::new(confdata, true, false, false, 100_000);
    let (cmd_tx, cmd_rx) = mpsc::channel::<OmsCommand>(1024);
    let (snap, _) = core.take_snapshot();
    let replica: ReadReplica = Arc::new(ArcSwap::new(Arc::new(snap)));
    let shutdown = CancellationToken::new();

    let replica2 = replica.clone();
    let shutdown2 = shutdown.clone();
    rt.spawn(bench_writer_loop(core, cmd_rx, replica2, shutdown2));

    (cmd_tx, replica, shutdown)
}

async fn bench_writer_loop(
    mut core:    OmsCore,
    mut rx:      mpsc::Receiver<OmsCommand>,
    replica:     ReadReplica,
    shutdown:    CancellationToken,
) {
    use zk_oms_rs::models::OmsAction;
    let (snap, mut writer) = core.take_snapshot();
    replica.store(Arc::new(snap));

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                let (msg, reply_opt) = match cmd {
                    OmsCommand::PlaceOrder  { req, reply, .. } =>
                        (OmsMessage::PlaceOrder(req),  Some(reply)),
                    OmsCommand::CancelOrder { req, reply } =>
                        (OmsMessage::CancelOrder(req), Some(reply)),
                    OmsCommand::BatchPlaceOrders { reqs, reply } =>
                        (OmsMessage::BatchPlaceOrders(reqs), Some(reply)),
                    _ => continue,
                };

                let actions = core.process_message(msg);
                let mut balances_dirty = false;
                let mut positions_dirty = false;
                for a in &actions {
                    match a {
                        OmsAction::PersistOrder { order, set_closed, .. } =>
                            writer.apply_persist_order(order, *set_closed),
                        OmsAction::PublishBalanceUpdate(_) => balances_dirty = true,
                        OmsAction::PublishPositionUpdate(_) => positions_dirty = true,
                        _ => {}
                    }
                }
                let (managed_pos, exch_pos) = if positions_dirty {
                    (core.position_mgr.snapshot_managed(), core.position_mgr.snapshot_exch())
                } else {
                    let prev = replica.load();
                    (prev.managed_positions.clone(), prev.exch_positions.clone())
                };
                let exch_bal = if balances_dirty {
                    core.balance_mgr.snapshot_exch_balances()
                } else {
                    let prev = replica.load();
                    prev.exch_balances.clone()
                };
                let snap = writer.publish(managed_pos, exch_pos, exch_bal, vec![], vec![], gen_timestamp_ms());
                replica.store(Arc::new(snap));

                if let Some(tx) = reply_opt {
                    let ok = actions.iter().any(|a| matches!(
                        a, OmsAction::PublishOrderUpdate(_) | OmsAction::SendOrderToGw { .. }
                    ));
                    let resp = if ok {
                        oms_actor::ok_response("")
                    } else {
                        oms_actor::err_response(OmsErrorType::OmsErrTypeInvalidReq, "rejected")
                    };
                    let _ = tx.send(resp);
                }
            }
        }
    }
}

async fn send(
    cmd_tx: &mpsc::Sender<OmsCommand>,
    make: impl FnOnce(oneshot::Sender<OmsResponse>) -> OmsCommand,
) -> OmsResponse {
    let (tx, rx) = oneshot::channel();
    cmd_tx.send(make(tx)).await.unwrap();
    rx.await.unwrap()
}

// ── Benchmarks ────────────────────────────────────────────────────────────────

/// PlaceOrder command round-trip via mpsc → writer → oneshot reply.
fn bench_place_order(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cmd_tx, _replica, shutdown) = spawn_bench_actor(&rt);

    c.bench_function("actor/place_order", |b| {
        b.to_async(&rt).iter(|| {
            let cmd_tx = cmd_tx.clone();
            async move {
                let id = next_id();
                let resp = send(&cmd_tx, |reply| OmsCommand::PlaceOrder {
                    req: make_order_req(id),
                    oms_received_ns: 0,
                    reply,
                })
                .await;
                criterion::black_box(resp);
            }
        });
    });

    shutdown.cancel();
}

/// BatchPlaceOrders with 10 orders per call.
fn bench_batch_place_10(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cmd_tx, _replica, shutdown) = spawn_bench_actor(&rt);

    c.bench_function("actor/batch_place_10", |b| {
        b.to_async(&rt).iter(|| {
            let cmd_tx = cmd_tx.clone();
            async move {
                let reqs: Vec<_> = (0..10).map(|_| make_order_req(next_id())).collect();
                let resp = send(&cmd_tx, |reply| OmsCommand::BatchPlaceOrders {
                    reqs,
                    reply,
                })
                .await;
                criterion::black_box(resp);
            }
        });
    });

    shutdown.cancel();
}

/// PlaceOrder then CancelOrder in sequence (two round-trips).
fn bench_place_then_cancel(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (cmd_tx, _replica, shutdown) = spawn_bench_actor(&rt);

    c.bench_function("actor/place_then_cancel", |b| {
        b.to_async(&rt).iter(|| {
            let cmd_tx = cmd_tx.clone();
            async move {
                let id = next_id();
                // Place
                let _ = send(&cmd_tx, |reply| OmsCommand::PlaceOrder {
                    req: make_order_req(id),
                    oms_received_ns: 0,
                    reply,
                })
                .await;
                // Cancel (uses exch_order_ref that doesn't exist yet — OmsCore
                // will reject or buffer; we're measuring command latency here)
                let resp = send(&cmd_tx, |reply| OmsCommand::CancelOrder {
                    req: make_cancel_req(id, &format!("E{id}")),
                    reply,
                })
                .await;
                criterion::black_box(resp);
            }
        });
    });

    shutdown.cancel();
}

criterion_group!(benches, bench_place_order, bench_batch_place_10, bench_place_then_cancel);
criterion_main!(benches);
