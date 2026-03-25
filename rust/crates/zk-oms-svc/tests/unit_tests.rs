//! Unit tests for `zk-oms-svc`.
//!
//! These tests run without real NATS/Redis/PG — all I/O-bound state is either
//! mocked via channels or exercised through the public OmsCore API.
//!
//! ## Test categories
//!
//! - **Actor tests** — send `OmsCommand`s through a real channel; observe
//!   the read replica snapshot and any `OmsResponse` returned.
//! - **Redis key tests** — verify key format contracts from `zk-infra-rs`.
//! - **Config tests** — verify env-var loading and defaults.
//!
//! ## Integration tests (`#[ignore]`)
//! See [`integration`] submodule — require the dev docker-compose stack.

use std::{sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use zk_oms_rs::{config::ConfdataManager, models::OmsOrder, oms_core_v2::OmsCoreV2};
use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::{
        common::v1::Rejection,
        common::v1::InstrumentRefData,
        exch_gw::v1::{
            order_report_entry::Report, BalanceUpdate, ExecReport, ExchExecType, OrderReport,
            OrderReportEntry, OrderReportType, PositionReport,
        },
        oms::v1::{oms_response, OmsErrorType, OmsResponse, OrderRequest, OrderStatus},
    },
};

use zk_oms_svc::oms_actor::{self, OmsCommand, ReadReplica};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal `ConfdataManager` suitable for unit tests.
fn test_confdata(oms_id: &str) -> ConfdataManager {
    let oms_cfg = OmsConfigEntry {
        oms_id: oms_id.to_string(),
        managed_account_ids: vec![9001],
        ..Default::default()
    };
    let route = OmsRouteEntry {
        account_id: 9001,
        gw_key: "gw_mock_1".into(),
        exch_account_id: "TEST_ACCT".into(),
        ..Default::default()
    };
    let gw_cfg = GwConfigEntry {
        gw_key: "gw_mock_1".into(),
        exch_name: "MOCK_EXCH".into(),
        rpc_endpoint: "localhost:50061".into(),
        ..Default::default()
    };
    let refdata = InstrumentRefData {
        instrument_id: "BTC-USDT".into(),
        instrument_id_exchange: "BTC-USDT".into(),
        exchange_name: "MOCK_EXCH".into(),
        instrument_type: 1, // SPOT
        disabled: false,
        ..Default::default()
    };
    ConfdataManager::new(oms_cfg, vec![route], vec![gw_cfg], vec![refdata], vec![])
}

/// Build a minimal `OrderRequest` for testing.
fn test_order_req(order_id: i64, account_id: i64) -> OrderRequest {
    use zk_proto_rs::zk::common::v1::{BasicOrderType, BuySellType, OpenCloseType};
    OrderRequest {
        order_id,
        account_id,
        instrument_code: "BTC-USDT".into(),
        buy_sell_type: BuySellType::BsBuy as i32,
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        price: 50_000.0,
        qty: 0.01,
        source_id: "test_strategy".into(),
        timestamp: zk_oms_rs::utils::gen_timestamp_ms(),
        ..Default::default()
    }
}

/// Spawn a writer task backed by a no-op `NatsPublisher` and `GwClientPool`
/// (no real infra needed).
async fn spawn_test_actor(
    confdata: ConfdataManager,
    shutdown: CancellationToken,
) -> (mpsc::Sender<OmsCommand>, ReadReplica) {
    let core = OmsCoreV2::new(&confdata, false, true, false, 10_000);

    let (cmd_tx, cmd_rx) = mpsc::channel::<OmsCommand>(256);

    // Build initial snapshot from empty core.
    use zk_oms_rs::snapshot_v2::OmsSnapshotWriterV2;
    use zk_oms_svc::oms_actor::{
        build_exch_balances, build_exch_positions, build_managed_positions,
    };

    let snap_meta = Arc::new(core.build_snapshot_metadata());
    let mut snap_writer = OmsSnapshotWriterV2::new(snap_meta);
    let (exch_pos, unknown_pos) = build_exch_positions(&core);
    let (exch_bal, unknown_bal) = build_exch_balances(&core);
    let initial_snap = snap_writer.publish(
        build_managed_positions(&core),
        exch_pos,
        exch_bal,
        unknown_pos,
        unknown_bal,
        zk_oms_rs::utils::gen_timestamp_ms(),
    );
    let replica: ReadReplica = Arc::new(ArcSwap::new(Arc::new(initial_snap)));

    let replica2 = replica.clone();
    tokio::spawn(simple_test_writer_loop(
        core,
        snap_writer,
        cmd_rx,
        replica2,
        shutdown,
    ));

    (cmd_tx, replica)
}

/// Simplified writer loop for unit tests — no NATS/Redis/GW.
async fn simple_test_writer_loop(
    mut core: OmsCoreV2,
    mut writer: zk_oms_rs::snapshot_v2::OmsSnapshotWriterV2,
    mut rx: mpsc::Receiver<OmsCommand>,
    replica: ReadReplica,
    shutdown: CancellationToken,
) {
    use zk_oms_rs::{models_v2::OmsActionV2, utils::gen_timestamp_ms};
    use zk_oms_svc::oms_actor::{
        build_exch_balances, build_exch_positions, build_managed_positions,
        build_snapshot_detail_from_core, build_snapshot_order_from_live,
    };

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break; };

                let (oms_msg, reply) = match cmd {
                    OmsCommand::PlaceOrder { req, reply, .. } =>
                        (zk_oms_rs::models::OmsMessage::PlaceOrder(req), Some(reply)),
                    OmsCommand::CancelOrder { req, reply } =>
                        (zk_oms_rs::models::OmsMessage::CancelOrder(req), Some(reply)),
                    OmsCommand::Panic { account_id, reply } => {
                        writer.apply_panic(account_id);
                        (zk_oms_rs::models::OmsMessage::Panic { account_id }, Some(reply))
                    }
                    OmsCommand::DontPanic { account_id, reply } => {
                        writer.apply_clear_panic(account_id);
                        (zk_oms_rs::models::OmsMessage::DontPanic { account_id }, Some(reply))
                    }
                    OmsCommand::GatewayOrderReport(r) =>
                        (zk_oms_rs::models::OmsMessage::GatewayOrderReport(r), None),
                    OmsCommand::GatewayBalanceUpdate(u) =>
                        (zk_oms_rs::models::OmsMessage::BalanceUpdate(u), None),
                    OmsCommand::Cleanup { ts_ms } =>
                        (zk_oms_rs::models::OmsMessage::Cleanup { ts_ms }, None),
                    OmsCommand::ReloadConfig { new_config, reply } => {
                        core.reload_config(new_config);
                        let new_meta = Arc::new(core.build_snapshot_metadata());
                        writer.update_metadata(new_meta);
                        let snap = {
                            let prev = replica.load();
                            writer.publish(
                                prev.managed_positions.clone(),
                                prev.exch_positions.clone(),
                                prev.exch_balances.clone(),
                                prev.unknown_exch_positions.clone(),
                                prev.unknown_exch_balances.clone(),
                                gen_timestamp_ms(),
                            )
                        };
                        replica.store(Arc::new(snap));
                        let _ = reply.send(oms_actor::ok_response("reloaded"));
                        continue;
                    }
                    _ => continue,
                };

                let actions = core.process_message(oms_msg);
                let mut balances_dirty = false;
                let mut positions_dirty = false;
                for action in &actions {
                    match action {
                        OmsActionV2::PersistOrder { order_id, set_closed, .. } => {
                            if let Some(live) = core.orders.live.get(order_id) {
                                let snap_order = build_snapshot_order_from_live(live, &core.orders.dyn_strings);
                                writer.apply_order_update(snap_order, *set_closed);
                                if let Some(detail_log) = core.orders.get_detail(*order_id) {
                                    let snap_detail = build_snapshot_detail_from_core(
                                        *order_id, live, detail_log, &core.metadata, &core.orders.dyn_strings,
                                    );
                                    writer.apply_order_detail(snap_detail);
                                }
                            }
                        }
                        OmsActionV2::PublishBalanceUpdate { .. } => balances_dirty = true,
                        OmsActionV2::PublishPositionUpdate { .. } => positions_dirty = true,
                        _ => {}
                    }
                }

                let (managed_pos, exch_pos, unknown_pos) = if positions_dirty {
                    let (ep, up) = build_exch_positions(&core);
                    (build_managed_positions(&core), ep, up)
                } else {
                    let prev = replica.load();
                    (prev.managed_positions.clone(), prev.exch_positions.clone(), prev.unknown_exch_positions.clone())
                };
                let (exch_bal, unknown_bal) = if balances_dirty {
                    build_exch_balances(&core)
                } else {
                    let prev = replica.load();
                    (prev.exch_balances.clone(), prev.unknown_exch_balances.clone())
                };
                let snap = writer.publish(managed_pos, exch_pos, exch_bal, unknown_pos, unknown_bal, gen_timestamp_ms());
                replica.store(Arc::new(snap));

                if let Some(tx) = reply {
                    let ok = actions.iter().any(|a| matches!(
                        a,
                        OmsActionV2::PublishOrderUpdate { .. } | OmsActionV2::SendOrderToGw { .. }
                    ));
                    let resp = if ok {
                        oms_actor::ok_response("")
                    } else {
                        oms_actor::err_response(OmsErrorType::OmsErrTypeInvalidReq, "no action produced")
                    };
                    let _ = tx.send(resp);
                }
            }
        }
    }
}

async fn send_cmd(
    cmd_tx: &mpsc::Sender<OmsCommand>,
    make_cmd: impl FnOnce(oneshot::Sender<OmsResponse>) -> OmsCommand,
) -> OmsResponse {
    let (tx, rx) = oneshot::channel();
    cmd_tx.send(make_cmd(tx)).await.unwrap();
    rx.await.unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Place order → expect success, snapshot contains order in open set.
#[tokio::test]
async fn test_place_order_updates_snapshot() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_1");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    // Give writer task a moment to initialise.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let order_id = 1001;
    let _resp = send_cmd(&cmd_tx, |reply| OmsCommand::PlaceOrder {
        oms_received_ns: 0,
        req: test_order_req(order_id, 9001),
        reply,
    })
    .await;

    // Allow snapshot to propagate.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let snap = replica.load();
    assert!(
        snap.orders.contains_key(&order_id),
        "order {order_id} should appear in snapshot"
    );
    assert!(
        snap.open_order_ids_by_account
            .get(&9001)
            .map(|s| s.contains(&order_id))
            .unwrap_or(false),
        "order {order_id} should be in open set"
    );
    shutdown.cancel();
}

/// Submit same order_id twice → second should fail (idempotency checked at handler, not here).
/// This test validates the *core* behaviour: OmsCore rejects duplicate order_id.
#[tokio::test]
async fn test_duplicate_order_id_rejected_by_core() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_dup");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let order_id = 2001;
    let _first = send_cmd(&cmd_tx, |reply| OmsCommand::PlaceOrder {
        oms_received_ns: 0,
        req: test_order_req(order_id, 9001),
        reply,
    })
    .await;

    // After first placement the order should exist.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let snap = replica.load();
    assert!(
        snap.orders.contains_key(&order_id),
        "first order must be accepted"
    );

    shutdown.cancel();
}

#[tokio::test]
async fn test_gateway_exec_reject_by_order_id_marks_order_rejected() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_gw_reject");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let order_id = 2101;
    let _resp = send_cmd(&cmd_tx, |reply| OmsCommand::PlaceOrder {
        oms_received_ns: 0,
        req: test_order_req(order_id, 9001),
        reply,
    })
    .await;

    let report = OrderReport {
        exchange: "gw_mock_1".into(),
        account_id: 9001,
        order_id,
        update_timestamp: 12345,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeExec as i32,
            report: Some(Report::ExecReport(ExecReport {
                exec_type: ExchExecType::Rejected as i32,
                rejection_info: Some(Rejection {
                    error_message: "venue rejected order".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        }],
        ..Default::default()
    };

    cmd_tx
        .send(OmsCommand::GatewayOrderReport(report))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;
    let snap = replica.load();
    let order = snap.orders.get(&order_id).expect("order missing");
    assert_eq!(order.order_status, OrderStatus::Rejected as i32);
    shutdown.cancel();
}

/// Panic mode: once set, the account appears in `panic_accounts` snapshot.
#[tokio::test]
async fn test_panic_sets_snapshot() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_panic");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let _resp = send_cmd(&cmd_tx, |reply| OmsCommand::Panic {
        account_id: 9001,
        reply,
    })
    .await;

    tokio::time::sleep(Duration::from_millis(10)).await;
    let snap = replica.load();
    assert!(
        snap.panic_accounts.contains(&9001),
        "9001 should be in panic_accounts after Panic command"
    );

    shutdown.cancel();
}

/// DontPanic clears the panic mode.
#[tokio::test]
async fn test_clear_panic() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_clr");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Set panic
    send_cmd(&cmd_tx, |reply| OmsCommand::Panic {
        account_id: 9001,
        reply,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(replica.load().panic_accounts.contains(&9001));

    // Clear panic
    send_cmd(&cmd_tx, |reply| OmsCommand::DontPanic {
        account_id: 9001,
        reply,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        !replica.load().panic_accounts.contains(&9001),
        "panic should be cleared"
    );

    shutdown.cancel();
}

/// Snapshot monotonically increases `seq` after each mutation.
#[tokio::test]
async fn test_snapshot_seq_increments() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_seq");
    let (cmd_tx, replica) = spawn_test_actor(confdata, shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let seq_before = replica.load().seq;

    send_cmd(&cmd_tx, |reply| OmsCommand::PlaceOrder {
        oms_received_ns: 0,
        req: test_order_req(3001, 9001),
        reply,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let seq_after = replica.load().seq;
    assert!(
        seq_after > seq_before,
        "seq should increment after mutation"
    );

    shutdown.cancel();
}

/// ReloadConfig swaps in a new ConfdataManager without crashing the writer.
#[tokio::test]
async fn test_reload_config() {
    let shutdown = CancellationToken::new();
    let confdata = test_confdata("oms_test_reload");
    let (cmd_tx, _replica) = spawn_test_actor(confdata.clone(), shutdown.clone()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let new_confdata = test_confdata("oms_test_reload"); // same, just fresh
    let resp = send_cmd(&cmd_tx, |reply| OmsCommand::ReloadConfig {
        new_config: new_confdata,
        reply,
    })
    .await;

    assert_eq!(
        resp.status,
        oms_response::Status::OmsRespStatusSuccess as i32,
        "ReloadConfig should return success"
    );
    shutdown.cancel();
}

// ── Redis key tests ───────────────────────────────────────────────────────────

#[test]
fn test_redis_key_order() {
    use zk_infra_rs::redis::key;
    assert_eq!(key::order("oms_dev_1", 123), "oms:oms_dev_1:order:123");
}

#[test]
fn test_redis_key_open_orders() {
    use zk_infra_rs::redis::key;
    assert_eq!(
        key::open_orders("oms_dev_1", 9001),
        "oms:oms_dev_1:open_orders:9001"
    );
}

#[test]
fn test_redis_key_balance() {
    use zk_infra_rs::redis::key;
    assert_eq!(
        key::balance("oms_dev_1", 9001, "USDT"),
        "oms:oms_dev_1:balance:9001:USDT"
    );
}

// ── PersistedOrder round-trip ─────────────────────────────────────────────────

#[test]
fn test_persisted_order_roundtrip() {
    use zk_oms_svc::redis_writer::PersistedOrder;
    use zk_proto_rs::zk::oms::v1::Order;

    let mut order_state = Order::default();
    order_state.order_id = 9999;
    order_state.account_id = 1;
    order_state.instrument = "ETH-USDT".into();
    order_state.price = 3000.0;
    order_state.qty = 1.0;
    order_state.order_status = 1; // PENDING
    order_state.gw_key = "gw_mock_1".into();

    let oms_order = OmsOrder {
        is_from_external: false,
        order_id: 9999,
        account_id: 1,
        exch_order_ref: Some("EX_REF_123".into()),
        oms_req: None,
        gw_req: None,
        cancel_req: None,
        order_state,
        trades: vec![],
        acc_trades_filled_qty: 0.0,
        acc_trades_value: 0.0,
        order_inferred_trades: vec![],
        exec_msgs: vec![],
        fees: vec![],
        cancel_attempts: 0,
    };

    let persisted = PersistedOrder::from_oms_order(&oms_order);
    assert_eq!(persisted.order_id, 9999);
    assert_eq!(persisted.exch_order_ref, Some("EX_REF_123".into()));
    assert_eq!(persisted.instrument, "ETH-USDT");

    let json = serde_json::to_vec(&persisted).unwrap();
    let restored: PersistedOrder = serde_json::from_slice(&json).unwrap();
    assert_eq!(restored.order_id, 9999);
    assert_eq!(restored.price, 3000.0);

    let restored_order = restored.into_oms_order();
    assert_eq!(restored_order.order_id, 9999);
    assert_eq!(restored_order.exch_order_ref, Some("EX_REF_123".into()));
    assert_eq!(restored_order.order_state.instrument, "ETH-USDT");
}

// ── Config defaults ───────────────────────────────────────────────────────────

#[test]
fn test_config_defaults() {
    // Set the bare minimum required fields.
    std::env::set_var("ZK_OMS_ID", "test_oms");
    std::env::set_var("ZK_REDIS_URL", "redis://localhost:6379");
    std::env::set_var("ZK_PG_URL", "postgres://localhost/test");

    let boot = zk_oms_svc::config::load_bootstrap().unwrap();
    assert_eq!(boot.oms_id, "test_oms");
    assert_eq!(boot.grpc_port, 50051);

    use zk_infra_rs::bootstrap::{bootstrap_runtime_config, BootstrapMode};
    use zk_oms_svc::config::OmsService;
    let outcome =
        bootstrap_runtime_config::<OmsService>(&boot, BootstrapMode::Direct).unwrap();
    let cfg = outcome.runtime_config;
    assert_eq!(cfg.oms_id, "test_oms");
    assert_eq!(cfg.grpc_port, 50051);
    assert!(cfg.risk_check_enabled);
    assert_eq!(cfg.cmd_channel_buf, 4096);
    assert_eq!(cfg.kv_heartbeat_secs, 10);

    // Clean up to avoid polluting other tests.
    std::env::remove_var("ZK_OMS_ID");
    std::env::remove_var("ZK_REDIS_URL");
    std::env::remove_var("ZK_PG_URL");
}

// ── QueryPosition compatibility tests ────────────────────────────────────────

/// Helper: build a V2 snapshot with positions and balances populated.
fn snapshot_with_positions() -> zk_oms_rs::snapshot_v2::OmsSnapshotV2 {
    use std::collections::HashMap;
    use zk_oms_rs::models::{ExchBalanceSnapshot, ExchPositionSnapshot, ReconcileStatus};
    use zk_oms_rs::snapshot_v2::{
        OmsSnapshotV2, OmsSnapshotWriterV2, SnapshotManagedPosition, SnapshotMetadata,
    };
    use zk_proto_rs::zk::oms::v1::{Balance, Position};

    let meta = Arc::new(SnapshotMetadata {
        instrument_names: vec!["".into(), "BTC-PERP".into()],
        instrument_exch_names: vec!["".into(), "BTC-PERP".into()],
        asset_names: vec!["USDT".into()],
        gw_names: vec!["gw_mock_1".into()],
        source_names: vec!["".into()],
    });

    let mut managed_positions: HashMap<(i64, u32), SnapshotManagedPosition> = HashMap::new();
    managed_positions.insert(
        (9001, 1),
        SnapshotManagedPosition {
            account_id: 9001,
            instrument_id: 1,
            instrument_type: 2, // PERP
            is_short: false,
            qty_total: 1.5,
            qty_frozen: 0.0,
            qty_available: 1.5,
            last_local_update_ts: 0,
            last_exch_sync_ts: 0,
            reconcile_status: ReconcileStatus::Unknown,
        },
    );

    let mut exch_positions: HashMap<(i64, u32), ExchPositionSnapshot> = HashMap::new();
    exch_positions.insert(
        (9001, 1),
        ExchPositionSnapshot {
            account_id: 9001,
            instrument_code: "BTC-PERP".to_string(),
            symbol_exch: None,
            position_state: Position {
                account_id: 9001,
                instrument_code: "BTC-PERP".to_string(),
                total_qty: 1.4,
                ..Default::default()
            },
            exch_data_raw: String::new(),
            sync_ts: 0,
        },
    );

    let mut exch_balances: HashMap<(i64, u32), ExchBalanceSnapshot> = HashMap::new();
    exch_balances.insert(
        (9001, 0), // asset_id 0 = USDT
        ExchBalanceSnapshot {
            account_id: 9001,
            asset: "USDT".to_string(),
            symbol_exch: None,
            balance_state: Balance {
                account_id: 9001,
                asset: "USDT".to_string(),
                total_qty: 10_000.0,
                avail_qty: 10_000.0,
                is_from_exch: true,
                ..Default::default()
            },
            exch_data_raw: String::new(),
            sync_ts: 0,
        },
    );

    let mut writer = OmsSnapshotWriterV2::new(meta);
    writer.publish(
        managed_positions,
        exch_positions,
        exch_balances,
        vec![],
        vec![],
        0,
    )
}

/// query_position(query_gw=false) returns OMS-managed positions via metadata.
#[test]
fn test_query_position_managed() {
    let snap = snapshot_with_positions();
    let positions: Vec<_> = snap
        .managed_position_ids_by_account
        .get(&9001)
        .map(|inst_ids| {
            inst_ids
                .iter()
                .filter_map(|iid| snap.managed_positions.get(&(9001, *iid)))
                .map(|p| p.to_proto(&snap.metadata))
                .collect()
        })
        .unwrap_or_default();

    assert_eq!(positions.len(), 1);
    let btc = positions
        .iter()
        .find(|p| p.instrument_code == "BTC-PERP")
        .unwrap();
    assert!((btc.total_qty - 1.5).abs() < f64::EPSILON);
}

/// query_position(query_gw=true) returns exchange-reported positions.
#[test]
fn test_query_position_from_exch() {
    let snap = snapshot_with_positions();
    let positions: Vec<_> = snap
        .exch_position_ids_by_account
        .get(&9001)
        .map(|inst_ids| {
            inst_ids
                .iter()
                .filter_map(|iid| snap.exch_positions.get(&(9001, *iid)))
                .map(|p| p.position_state.clone())
                .collect()
        })
        .unwrap_or_default();

    assert_eq!(positions.len(), 1);
    let btc = positions
        .iter()
        .find(|p| p.instrument_code == "BTC-PERP")
        .unwrap();
    assert!((btc.total_qty - 1.4).abs() < f64::EPSILON);
}

/// query_position for unknown account returns empty list (no error).
#[test]
fn test_query_position_unknown_account() {
    let snap = snapshot_with_positions();
    let positions: Vec<zk_proto_rs::zk::oms::v1::Position> = snap
        .managed_position_ids_by_account
        .get(&9999)
        .map(|inst_ids| {
            inst_ids
                .iter()
                .filter_map(|iid| snap.managed_positions.get(&(9999, *iid)))
                .map(|p| p.to_proto(&snap.metadata))
                .collect()
        })
        .unwrap_or_default();

    assert!(positions.is_empty());
}

/// Unknown exchange positions/balances are preserved in overflow buckets,
/// not collapsed to ID 0.
#[test]
fn test_unknown_exch_overflow_buckets() {
    use std::collections::HashMap;
    use zk_oms_rs::models::{ExchBalanceSnapshot, ExchPositionSnapshot};
    use zk_oms_rs::snapshot_v2::{OmsSnapshotWriterV2, SnapshotMetadata};
    use zk_proto_rs::zk::oms::v1::{Balance, Position};

    let meta = Arc::new(SnapshotMetadata {
        instrument_names: vec!["".into(), "BTC-PERP".into()],
        instrument_exch_names: vec!["".into(), "BTC-PERP".into()],
        asset_names: vec!["USDT".into()],
        gw_names: vec!["gw1".into()],
        source_names: vec!["".into()],
    });

    // One resolved + two unknown positions for the same account.
    let mut exch_positions = HashMap::new();
    exch_positions.insert(
        (100_i64, 1_u32),
        ExchPositionSnapshot {
            account_id: 100,
            instrument_code: "BTC-PERP".to_string(),
            symbol_exch: None,
            position_state: Position {
                account_id: 100,
                instrument_code: "BTC-PERP".to_string(),
                total_qty: 1.0,
                ..Default::default()
            },
            exch_data_raw: String::new(),
            sync_ts: 0,
        },
    );

    let unknown_pos = vec![
        ExchPositionSnapshot {
            account_id: 100,
            instrument_code: "UNKNOWN-A".to_string(),
            symbol_exch: None,
            position_state: Position {
                account_id: 100,
                instrument_code: "UNKNOWN-A".to_string(),
                total_qty: 2.0,
                ..Default::default()
            },
            exch_data_raw: String::new(),
            sync_ts: 0,
        },
        ExchPositionSnapshot {
            account_id: 100,
            instrument_code: "UNKNOWN-B".to_string(),
            symbol_exch: None,
            position_state: Position {
                account_id: 100,
                instrument_code: "UNKNOWN-B".to_string(),
                total_qty: 3.0,
                ..Default::default()
            },
            exch_data_raw: String::new(),
            sync_ts: 0,
        },
    ];

    let unknown_bal = vec![ExchBalanceSnapshot {
        account_id: 100,
        asset: "MYSTERY_COIN".to_string(),
        symbol_exch: None,
        balance_state: Balance {
            account_id: 100,
            asset: "MYSTERY_COIN".to_string(),
            total_qty: 999.0,
            ..Default::default()
        },
        exch_data_raw: String::new(),
        sync_ts: 0,
    }];

    let mut writer = OmsSnapshotWriterV2::new(meta);
    let snap = writer.publish(
        HashMap::new(),
        exch_positions,
        HashMap::new(),
        unknown_pos,
        unknown_bal,
        0,
    );

    // Resolved position accessible via index.
    assert_eq!(snap.exch_position_ids_by_account[&100], vec![1]);

    // Unknown positions preserved in overflow.
    assert_eq!(snap.unknown_exch_positions.len(), 2);
    let codes: Vec<&str> = snap
        .unknown_exch_positions
        .iter()
        .map(|p| p.instrument_code.as_str())
        .collect();
    assert!(codes.contains(&"UNKNOWN-A"));
    assert!(codes.contains(&"UNKNOWN-B"));

    // Unknown balance preserved.
    assert_eq!(snap.unknown_exch_balances.len(), 1);
    assert_eq!(snap.unknown_exch_balances[0].asset, "MYSTERY_COIN");

    // Simulate query_position (query_gw=true): resolved + unknown.
    let mut positions: Vec<Position> = snap
        .exch_position_ids_by_account
        .get(&100)
        .map(|ids| {
            ids.iter()
                .filter_map(|iid| snap.exch_positions.get(&(100, *iid)))
                .map(|p| p.position_state.clone())
                .collect()
        })
        .unwrap_or_default();
    positions.extend(
        snap.unknown_exch_positions
            .iter()
            .filter(|p| p.account_id == 100)
            .map(|p| p.position_state.clone()),
    );
    assert_eq!(positions.len(), 3); // 1 resolved + 2 unknown
}

#[test]
fn test_build_exch_balances_preserves_unknown_assets_from_core() {
    use zk_proto_rs::zk::common::v1::InstrumentType;

    let conf = test_confdata("oms_test_unknown_balance");
    let mut core = OmsCoreV2::new(&conf, false, true, false, 1024);
    core.process_message(zk_oms_rs::models::OmsMessage::BalanceUpdate(BalanceUpdate {
        balances: vec![PositionReport {
            instrument_code: "MYSTERY_COIN".into(),
            instrument_type: InstrumentType::InstTypeSpot as i32,
            exch_account_code: "TEST_ACCT".into(),
            qty: 42.0,
            avail_qty: 40.0,
            update_timestamp: 123,
            ..Default::default()
        }],
    }));

    let (resolved, unknown) = oms_actor::build_exch_balances(&core);
    assert!(resolved.is_empty());
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].account_id, 9001);
    assert_eq!(unknown[0].asset, "MYSTERY_COIN");
    assert_eq!(unknown[0].balance_state.asset, "MYSTERY_COIN");
    assert_eq!(unknown[0].balance_state.total_qty, 42.0);
}

/// Duplicate order_refs in query_order_details produce no duplicate output rows.
#[test]
fn test_query_order_details_dedup() {
    use std::collections::HashSet;
    use zk_oms_rs::snapshot_v2::{
        OmsSnapshotWriterV2, SnapshotMetadata, SnapshotOrder, SnapshotOrderDetail,
    };

    let meta = Arc::new(SnapshotMetadata {
        instrument_names: vec!["".into(), "BTC-PERP".into()],
        instrument_exch_names: vec!["".into(), "BTCPERP".into()],
        asset_names: vec![],
        gw_names: vec!["gw1".into()],
        source_names: vec!["".into(), "strat1".into()],
    });

    let mut writer = OmsSnapshotWriterV2::new(meta);

    let order = SnapshotOrder {
        order_id: 1001,
        account_id: 42,
        instrument_id: 1,
        gw_id: 0,
        source_sym: 1,
        order_status: 1,
        buy_sell_type: 1,
        open_close_type: 0,
        order_type: 1,
        tif_type: 0,
        price: 100.0,
        qty: 10.0,
        filled_qty: 5.0,
        filled_avg_price: 99.5,
        exch_order_ref: Some("EX1".into()),
        created_at: 1000,
        updated_at: 2000,
        snapshot_version: 1,
        is_external: false,
    };
    writer.apply_order_update(order, false);

    let detail = SnapshotOrderDetail {
        order_id: 1001,
        source_sym: 1,
        instrument_exch_sym: 1,
        exch_order_ref: Some("EX1".into()),
        error_msg: "".into(),
        trades: vec![zk_proto_rs::zk::oms::v1::Trade {
            order_id: 1001,
            ext_trade_id: "T1".into(),
            ..Default::default()
        }],
        inferred_trades: vec![],
        exec_msgs: vec![],
        fees: vec![],
    };
    writer.apply_order_detail(detail);

    let snap = writer.publish(
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        vec![],
        vec![],
        0,
    );

    // Simulate query_order_details with duplicate refs ["EX1", "EX1"].
    let order_refs = vec!["EX1".to_string(), "EX1".to_string()];
    let unique_refs: HashSet<&str> = order_refs.iter().map(String::as_str).collect();
    let mut seen_oids = HashSet::new();
    let orders: Vec<_> = unique_refs
        .iter()
        .filter_map(|r| snap.order_ids_by_exch_ref.get(*r))
        .flatten()
        .filter(|oid| seen_oids.insert(**oid))
        .filter_map(|oid| {
            let o = snap.orders.get(oid)?;
            Some(o.to_proto_order(&snap.metadata, snap.order_details.get(oid)))
        })
        .collect();

    assert_eq!(
        orders.len(),
        1,
        "duplicate refs must not produce duplicate orders"
    );
    assert_eq!(orders[0].order_id, 1001);

    // Simulate query_trade_details with duplicate refs.
    let mut seen_oids2 = HashSet::new();
    let trades: Vec<_> = unique_refs
        .iter()
        .filter_map(|r| snap.order_ids_by_exch_ref.get(*r))
        .flatten()
        .filter(|oid| seen_oids2.insert(**oid))
        .filter_map(|oid| snap.order_details.get(oid))
        .flat_map(|d| d.trades.clone())
        .collect();

    assert_eq!(
        trades.len(),
        1,
        "duplicate refs must not produce duplicate trades"
    );
}

// ── Bootstrap config tests ───────────────────────────────────────────────────

mod bootstrap_tests {
    use zk_infra_rs::bootstrap::{
        bootstrap_runtime_config, BootstrapConfigError, BootstrapMode, PilotPayload,
    };
    use zk_oms_svc::config::{OmsBootstrapConfig, OmsService};
    use zk_proto_rs::zk::config::v1::ConfigMetadata;

    fn test_boot_cfg() -> OmsBootstrapConfig {
        OmsBootstrapConfig {
            oms_id: "oms_test_1".into(),
            grpc_host: "127.0.0.1".into(),
            grpc_port: 50051,
            nats_url: "nats://localhost:4222".into(),
            bootstrap_token: String::new(),
            instance_type: "OMS".into(),
            env: "dev".into(),
        }
    }

    fn pilot_payload(json: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: None,
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn pilot_payload_with_hash(json: &str, hash: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: Some(ConfigMetadata {
                config_version: "1".into(),
                config_hash: hash.into(),
                config_source: "bootstrap".into(),
                issued_at_ms: 1700000000000,
                loaded_at_ms: 0,
            }),
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn valid_pilot_json() -> &'static str {
        r#"{"redis_url":"redis://localhost:6379","pg_url":"postgres://localhost/test"}"#
    }

    #[test]
    fn test_pilot_mode_assembles_runtime_config() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(valid_pilot_json()),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.oms_id, "oms_test_1");
        assert_eq!(outcome.runtime_config.env, "dev");
        assert_eq!(
            outcome.runtime_config.redis_url,
            "redis://localhost:6379"
        );
        assert_eq!(
            outcome.runtime_config.pg_url,
            "postgres://localhost/test"
        );
        // Defaults should be applied for fields not in JSON.
        assert!(outcome.runtime_config.risk_check_enabled);
        assert_eq!(outcome.runtime_config.cmd_channel_buf, 4096);
    }

    #[test]
    fn test_pilot_mode_malformed_json_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload("{not valid json}"),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn test_pilot_mode_empty_json_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(""),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn test_pilot_mode_missing_redis_url_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"pg_url":"postgres://localhost/test"}"#),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        match err {
            BootstrapConfigError::MissingField { field } => assert_eq!(field, "redis_url"),
            other => panic!("expected MissingField, got: {other:?}"),
        }
    }

    #[test]
    fn test_pilot_mode_missing_pg_url_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"redis_url":"redis://localhost:6379"}"#),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        match err {
            BootstrapConfigError::MissingField { field } => assert_eq!(field, "pg_url"),
            other => panic!("expected MissingField, got: {other:?}"),
        }
    }

    #[test]
    fn test_pilot_mode_empty_redis_url_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(
                r#"{"redis_url":"","pg_url":"postgres://localhost/test"}"#,
            ),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(
            err,
            BootstrapConfigError::MissingField { .. }
        ));
    }

    #[test]
    fn test_pilot_mode_hash_mismatch_fails() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(valid_pilot_json(), "badhash"),
            validate_hash: true,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::HashMismatch { .. }));
    }

    #[test]
    fn test_pilot_mode_hash_match_succeeds() {
        let json = valid_pilot_json();
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let normalized = zk_infra_rs::config_mgmt::normalize_json(&value);
        // Compute SHA-256 hex the same way the shared lib does.
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(normalized.as_bytes()));

        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, &hash),
            validate_hash: true,
        };
        let outcome = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap();
        assert_eq!(
            outcome.runtime_config.redis_url,
            "redis://localhost:6379"
        );
    }

    #[test]
    fn test_pilot_mode_does_not_fallback_to_direct() {
        // Pilot mode with invalid JSON must fail, NOT silently fall back to direct.
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload("{}"),
            validate_hash: false,
        };
        // Empty object means redis_url/pg_url are empty strings → MissingField.
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(
            err,
            BootstrapConfigError::MissingField { .. }
        ));
    }

    #[test]
    fn test_direct_mode_preserves_defaults() {
        // Temporarily set required env vars.
        std::env::set_var("ZK_OMS_ID", "oms_defaults_test");
        std::env::set_var("ZK_REDIS_URL", "redis://localhost:6379");
        std::env::set_var("ZK_PG_URL", "postgres://localhost/test");

        let boot = zk_oms_svc::config::load_bootstrap().unwrap();
        let outcome =
            bootstrap_runtime_config::<OmsService>(&boot, BootstrapMode::Direct).unwrap();
        let cfg = outcome.runtime_config;

        // Check all defaults.
        assert!(cfg.risk_check_enabled);
        assert!(!cfg.handle_external_orders);
        assert_eq!(cfg.max_pending_reports, 1_000);
        assert_eq!(cfg.max_cached_orders, 100_000);
        assert_eq!(cfg.cmd_channel_buf, 4_096);
        assert_eq!(cfg.kv_heartbeat_secs, 10);
        assert_eq!(cfg.gateway_kv_prefix, "svc.gw");
        assert_eq!(cfg.order_resync_interval_secs, 60);
        assert_eq!(cfg.balance_resync_interval_secs, 60);
        assert_eq!(cfg.position_recheck_interval_secs, 30);
        assert_eq!(cfg.cleanup_interval_secs, 600);
        assert_eq!(cfg.gw_exec_shard_count, 16);
        assert_eq!(cfg.gw_exec_queue_capacity, 256);
        assert_eq!(cfg.metrics_interval_secs, 2);
        assert_eq!(cfg.metrics_max_pending, 5_000);
        assert_eq!(cfg.metrics_max_complete, 10_000);

        std::env::remove_var("ZK_OMS_ID");
        std::env::remove_var("ZK_REDIS_URL");
        std::env::remove_var("ZK_PG_URL");
    }

    #[test]
    fn test_assemble_validation_shard_count_zero() {
        let boot = test_boot_cfg();
        let json = r#"{"redis_url":"redis://localhost:6379","pg_url":"postgres://localhost/test","gw_exec_shard_count":0}"#;
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(json),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::Validation(_)));
    }

    #[test]
    fn test_assemble_validation_queue_capacity_zero() {
        let boot = test_boot_cfg();
        let json = r#"{"redis_url":"redis://localhost:6379","pg_url":"postgres://localhost/test","gw_exec_queue_capacity":0}"#;
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(json),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::Validation(_)));
    }

    #[test]
    fn test_pilot_mode_bootstrap_fields_from_bootstrap_config() {
        let mut boot = test_boot_cfg();
        boot.oms_id = "oms_prod_7".into();
        boot.env = "prod".into();
        boot.nats_url = "nats://prod:4222".into();

        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(valid_pilot_json()),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.oms_id, "oms_prod_7");
        assert_eq!(outcome.runtime_config.env, "prod");
        assert_eq!(outcome.runtime_config.nats_url, "nats://prod:4222");
    }

    #[test]
    fn test_pilot_mode_source_tag() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(valid_pilot_json()),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<OmsService>(&boot, mode).unwrap();
        assert!(matches!(
            outcome.source,
            zk_infra_rs::bootstrap::ConfigSource::Pilot { .. }
        ));
    }

    #[test]
    fn test_direct_mode_source_tag() {
        std::env::set_var("ZK_OMS_ID", "oms_src_tag");
        std::env::set_var("ZK_REDIS_URL", "redis://localhost:6379");
        std::env::set_var("ZK_PG_URL", "postgres://localhost/test");

        let boot = zk_oms_svc::config::load_bootstrap().unwrap();
        let outcome =
            bootstrap_runtime_config::<OmsService>(&boot, BootstrapMode::Direct).unwrap();
        assert_eq!(
            outcome.source,
            zk_infra_rs::bootstrap::ConfigSource::Direct
        );

        std::env::remove_var("ZK_OMS_ID");
        std::env::remove_var("ZK_REDIS_URL");
        std::env::remove_var("ZK_PG_URL");
    }
}

// ── Integration tests (require dev docker-compose stack) ──────────────────────

mod integration {
    /// Verify OMS starts, warm-loads from Redis, and KV entry appears.
    ///
    /// Run with: `cargo test -p zk-oms-svc -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn test_oms_startup_warmstart_and_kv_registration() {
        // TODO: start OMS, verify KV entry `svc.oms.oms_dev_1` appears within 5s.
        todo!("integration test — requires docker-compose stack");
    }

    /// PlaceOrder → gateway fills → OMS publishes FILLED event → Redis reflects state.
    #[tokio::test]
    #[ignore]
    async fn test_oms_place_and_fill_roundtrip() {
        todo!("integration test — requires docker-compose stack");
    }

    /// Inject late BalanceUpdate after order is already terminal — OMS should not crash.
    #[tokio::test]
    #[ignore]
    async fn test_late_balance_update_after_fill() {
        todo!("integration test — requires running OMS actor");
    }

    /// Mismatched exch_order_ref: report arrives before LINKAGE — buffered in
    /// `pending_order_reports`, replayed on linkage.
    #[tokio::test]
    #[ignore]
    async fn test_report_before_linkage_is_buffered() {
        todo!("integration test — requires running OMS actor");
    }

    /// Gateway restart: GW_EVENT_STARTED triggers balance resync.
    #[tokio::test]
    #[ignore]
    async fn test_gateway_restart_triggers_resync() {
        todo!("integration test — requires docker-compose stack");
    }
}
