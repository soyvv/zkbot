//! `zk-oms-svc` — production OMS gRPC service.
//!
//! # Startup sequence
//! 1. Load `OmsSvcConfig` from `ZK_*` env vars.
//! 2. Initialise JSON/pretty tracing.
//! 3. Connect to NATS, Redis, PostgreSQL.
//! 4. Load `ConfdataManager` from PG (`cfg.*` tables).
//! 5. Warm-start: load open order snapshots from Redis → `OmsCore::init_state`.
//! 6. Watch `svc.gw.*` in NATS KV; connect to each live gateway via gRPC.
//! 7. Reconcile: `QueryAccountBalance` for each bound account (populate balances).
//! 8. Start OMS writer task (single-writer actor).
//! 9. Start tonic gRPC server.
//! 10. Register `svc.oms.<oms_id>` in NATS KV (with heartbeat).
//! 11. Start NATS subscribers for gateway reports.
//! 12. Start periodic tasks (cleanup, resync timers).
//! 13. Await shutdown signal (SIGTERM / SIGINT).

use zk_oms_svc::{
    config, db, grpc_handler, gw_client, gw_executor::GwExecutorPool, latency::LatencyEvent,
    nats_handler, oms_actor, persist_executor::PersistExecutorPool, proto,
    publish_executor::PublishExecutorPool, redis_writer,
};

use std::{net::SocketAddr, sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tracing::{info, warn};

use crate::{
    grpc_handler::OmsGrpcHandler,
    gw_client::GwClientPool,
    nats_handler::NatsPublisher,
    oms_actor::{OmsCommand, ReadReplica},
    proto::oms_svc::oms_service_server::OmsServiceServer,
    redis_writer::RedisWriter,
};
use zk_infra_rs::{
    nats_kv::KvRegistryClient, service_registry::ServiceRegistration, tracing as zk_tracing,
};
use zk_oms_rs::{config::ConfdataManager, oms_core_v2::OmsCoreV2};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. Config ─────────────────────────────────────────────────────────────
    let cfg = config::load().expect("Failed to load OmsSvcConfig from env");

    // ── 2. Tracing ────────────────────────────────────────────────────────────
    zk_tracing::init_tracing(&format!("zk-oms-svc[{}]", cfg.oms_id));
    info!(oms_id = %cfg.oms_id, grpc_port = cfg.grpc_port, "starting zk-oms-svc");

    // ── 3. Infrastructure connections ─────────────────────────────────────────
    let nats_client = zk_infra_rs::nats::connect(&cfg.nats_url)
        .await
        .expect("NATS connect failed");
    info!("NATS connected");

    let redis_conn = zk_infra_rs::redis::connect(&cfg.redis_url)
        .await
        .expect("Redis connect failed");
    info!("Redis connected");

    let pg_pool = zk_infra_rs::pg::connect(&cfg.pg_url)
        .await
        .expect("PostgreSQL connect failed");
    info!("PostgreSQL connected");

    // ── 4. Load ConfdataManager ───────────────────────────────────────────────
    let raw = db::load_config(&pg_pool, &cfg.oms_id)
        .await
        .expect("Failed to load config from PG");

    let confdata = ConfdataManager::new(
        raw.oms_config,
        raw.account_routes,
        raw.gw_configs.clone(),
        raw.refdata,
        raw.trading_configs,
    );
    info!(oms_id = %confdata.oms_id, accounts = confdata.account_routes.len(), "ConfdataManager loaded");

    // ── 5. Warm-start from Redis ──────────────────────────────────────────────
    let mut redis_writer = RedisWriter::new(redis_conn, cfg.oms_id.clone());

    let persisted_orders = redis_writer.load_orders().await.unwrap_or_else(|e| {
        warn!(error = %e, "failed to load orders from Redis, starting cold");
        vec![]
    });
    info!(
        loaded_orders = persisted_orders.len(),
        "warm-start orders loaded from Redis"
    );

    let warm_orders: Vec<_> = persisted_orders
        .into_iter()
        .map(|p| p.into_oms_order())
        .collect();

    let persisted_balances = redis_writer.load_balances().await.unwrap_or_else(|e| {
        warn!(error = %e, "failed to load balances from Redis, starting cold");
        vec![]
    });
    info!(
        loaded_balances = persisted_balances.len(),
        "warm-start balances loaded from Redis"
    );

    let persisted_positions = redis_writer.load_positions().await.unwrap_or_else(|e| {
        warn!(error = %e, "failed to load positions from Redis, starting cold");
        vec![]
    });
    info!(
        loaded_positions = persisted_positions.len(),
        "warm-start positions loaded from Redis"
    );

    // ── 6. Build OmsCoreV2 ────────────────────────────────────────────────────
    let mut core = OmsCoreV2::new(
        &confdata,
        false, // use_time_emulation: false for live trading
        cfg.risk_check_enabled,
        cfg.handle_external_orders,
        cfg.max_cached_orders,
    );
    core.init_state(warm_orders, persisted_positions, persisted_balances);
    info!("OmsCoreV2 initialised");

    // ── 7. Connect to gateways via NATS KV ───────────────────────────────────
    let js = async_nats::jetstream::new(nats_client.clone());
    let kv_registry = KvRegistryClient::create(
        &js,
        zk_infra_rs::nats_kv::REGISTRY_BUCKET,
        Duration::from_secs(cfg.kv_heartbeat_secs * 3),
    )
    .await
    .expect("Failed to open/create KV registry bucket");

    let mut gw_pool = GwClientPool::new();
    let gw_prefix = format!("{}.>", cfg.gateway_kv_prefix);
    let mut gw_watch = kv_registry
        .watch(&gw_prefix)
        .await
        .expect("Failed to watch gateway KV");

    // Drain current entries (don't wait for new ones).
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            entry = gw_watch.next() => {
                match entry {
                    Some(Ok(e)) => {
                        use zk_infra_rs::nats_kv::KvOperation;
                        if e.operation == KvOperation::Put {
                            let gw_id = e.key.trim_start_matches(&format!("{}.", cfg.gateway_kv_prefix)).to_string();
                            if let Some(gw_cfg) = confdata.gw_configs.get(&gw_id) {
                                let addr = db::gw_grpc_addr(gw_cfg);
                                match gw_pool.connect(gw_id.clone(), &addr).await {
                                    Ok(_) => info!(gw_id, addr, "gateway gRPC connected"),
                                    Err(e) => warn!(gw_id, error = %e, "gateway gRPC connect failed"),
                                }
                            }
                        }
                    }
                    _ => break,
                }
            }
            _ = &mut deadline => break,
        }
    }

    // Fallback: directly probe KV for any configured gateways not yet connected.
    // The watch may miss already-registered entries due to JetStream consumer timing.
    for (gw_id, gw_cfg) in &confdata.gw_configs {
        if gw_pool.contains(gw_id) {
            continue;
        }
        let kv_key = format!("{}.{}", cfg.gateway_kv_prefix, gw_id);
        match kv_registry.get(&kv_key).await {
            Ok(Some(_)) => {
                let addr = db::gw_grpc_addr(gw_cfg);
                match gw_pool.connect(gw_id.clone(), &addr).await {
                    Ok(_) => info!(gw_id, addr, "gateway gRPC connected (direct KV probe)"),
                    Err(e) => warn!(gw_id, error = %e, "gateway gRPC connect failed"),
                }
            }
            Ok(None) => warn!(
                gw_id,
                kv_key, "gateway not registered in KV — will connect when it registers"
            ),
            Err(e) => warn!(gw_id, error = %e, "KV probe failed"),
        }
    }

    // ── 8. Balance/position reconciliation ─────────────────────────────────────
    {
        use zk_oms_rs::models_v2::OmsActionV2;
        use zk_proto_rs::zk::gateway::v1::QueryAccountRequest;

        // Query each connected gateway once for account balances/positions.
        let gw_keys: Vec<String> = gw_pool.gw_keys().map(String::from).collect();
        for gw_key in &gw_keys {
            let req = QueryAccountRequest::default();
            match gw_pool.query_account_balance(gw_key, req).await {
                Ok(resp) => {
                    if let Some(balance_update) = resp.balance_update {
                        let actions = core.process_message(
                            zk_oms_rs::models::OmsMessage::BalanceUpdate(balance_update),
                        );
                        for action in &actions {
                            match action {
                                OmsActionV2::PersistBalance {
                                    account_id,
                                    asset_id,
                                } => {
                                    oms_actor::persist_balance_to_redis(
                                        &core,
                                        &mut redis_writer,
                                        *account_id,
                                        *asset_id,
                                    )
                                    .await;
                                }
                                OmsActionV2::PersistPosition {
                                    account_id,
                                    instrument_id,
                                } => {
                                    oms_actor::persist_position_to_redis(
                                        &core,
                                        &mut redis_writer,
                                        *account_id,
                                        *instrument_id,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    info!(gw_key, "startup reconcile succeeded");
                }
                Err(e) => warn!(gw_key, error = %e, "startup reconcile failed"),
            }
        }
        info!("startup reconciliation complete");
    }

    // ── 9. Start OMS writer task ──────────────────────────────────────────────
    let (cmd_tx, cmd_rx) = mpsc::channel::<OmsCommand>(cfg.cmd_channel_buf);

    // Build initial snapshot from warm-started state.
    use crate::oms_actor::{
        build_exch_balances, build_exch_positions, build_managed_positions,
        build_snapshot_detail_from_core, build_snapshot_order_from_live,
    };
    use zk_oms_rs::snapshot_v2::OmsSnapshotWriterV2;

    let snap_meta = Arc::new(core.build_snapshot_metadata());
    let mut snap_writer = OmsSnapshotWriterV2::new(snap_meta);

    // Walk all live orders + details to hydrate the snapshot writer.
    let order_ids: Vec<i64> = core.orders.live.keys().copied().collect();
    for oid in &order_ids {
        if let Some(live) = core.orders.live.get(oid) {
            let snap_order = build_snapshot_order_from_live(live, &core.orders.dyn_strings);
            let is_terminal = live.is_in_terminal_state();
            snap_writer.apply_order_update(snap_order, is_terminal);

            if let Some(detail_log) = core.orders.get_detail(*oid) {
                let snap_detail = build_snapshot_detail_from_core(
                    *oid,
                    live,
                    detail_log,
                    &core.metadata,
                    &core.orders.dyn_strings,
                );
                snap_writer.apply_order_detail(snap_detail);
            }
        }
    }

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
    info!(
        seq = initial_snap.seq,
        orders = initial_snap.orders.len(),
        "initial snapshot built"
    );

    let replica: ReadReplica = Arc::new(ArcSwap::new(Arc::new(initial_snap)));

    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    // ── Executor pools + latency feedback channel ───────────────────────────
    let (latency_tx, latency_rx) = mpsc::channel::<LatencyEvent>(2048);

    // Build gw_id (u32) → gw_key (String) mapping from core metadata.
    let gw_id_to_key: Vec<(u32, String)> = core
        .metadata
        .gws
        .iter()
        .map(|gw| {
            let key = core.metadata.strings.resolve(gw.gw_key_sym).to_string();
            (gw.gw_id, key)
        })
        .collect();

    let gw_executor = GwExecutorPool::new(
        cfg.gw_exec_shard_count,
        cfg.gw_exec_queue_capacity,
        &gw_id_to_key,
        &gw_pool,
        latency_tx.clone(),
        cmd_tx.clone(),
    );

    let persist_executor = PersistExecutorPool::new(1024, redis_writer.clone_writer());

    let nats_publisher = NatsPublisher::new(nats_client.clone(), cfg.oms_id.clone());
    let publish_executor = PublishExecutorPool::new(256, nats_publisher.clone(), latency_tx);

    let writer_handle = oms_actor::spawn_writer(
        core,
        snap_writer,
        cmd_rx,
        replica.clone(),
        nats_publisher,
        redis_writer,
        gw_pool,
        gw_executor,
        persist_executor,
        publish_executor,
        latency_rx,
        shutdown_clone,
        Duration::from_secs(cfg.metrics_interval_secs),
        cfg.metrics_max_pending,
        cfg.metrics_max_complete,
    );

    // ── 10. Start gRPC server ─────────────────────────────────────────────────
    let handler = OmsGrpcHandler {
        cmd_tx: cmd_tx.clone(),
        replica: replica.clone(),
        oms_id: Arc::new(cfg.oms_id.clone()),
    };

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port)
        .parse()
        .expect("invalid grpc_port");

    let grpc_shutdown = shutdown.clone();
    let grpc_handle = tokio::spawn(async move {
        info!(%listen_addr, "gRPC server listening");
        Server::builder()
            .add_service(OmsServiceServer::new(handler))
            .serve_with_shutdown(listen_addr, async move {
                grpc_shutdown.cancelled().await;
            })
            .await
            .expect("gRPC server error");
        info!("gRPC server stopped");
    });

    // ── 11. Register in NATS KV with heartbeat ────────────────────────────────
    let kv_key = format!("svc.oms.{}", cfg.oms_id);
    let kv_value = Bytes::from(
        serde_json::json!({
            "service_type": "OMS",
            "instance_id":  cfg.oms_id,
            "grpc_port":    cfg.grpc_port,
        })
        .to_string()
        .into_bytes(),
    );

    let mut registration = ServiceRegistration::register_direct(
        &js,
        kv_key,
        kv_value,
        Duration::from_secs(cfg.kv_heartbeat_secs),
    )
    .await
    .expect("failed to register in NATS KV");
    info!(
        kv_key = registration.grant().kv_key,
        session_id = registration.grant().owner_session_id,
        "registered in NATS KV"
    );

    // ── 12. Start NATS subscribers ────────────────────────────────────────────
    let gw_ids: Vec<String> = confdata.gw_configs.keys().cloned().collect();
    let mut sub_handles = Vec::new();

    for gw_id in &gw_ids {
        match nats_handler::spawn_gw_subscriber(&nats_client, gw_id, cmd_tx.clone()).await {
            Ok(handles) => {
                info!(gw_id, "NATS subscriptions started");
                sub_handles.extend(handles);
            }
            Err(e) => warn!(gw_id, error = %e, "failed to subscribe to gateway NATS topics"),
        }
    }

    // ── 13. Periodic tasks ────────────────────────────────────────────────────
    let cleanup_tx = cmd_tx.clone();
    let cleanup_secs = cfg.cleanup_interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(cleanup_secs));
        loop {
            interval.tick().await;
            let ts_ms = zk_oms_rs::utils::gen_timestamp_ms();
            let _ = cleanup_tx.send(OmsCommand::Cleanup { ts_ms }).await;
        }
    });

    let recheck_tx = cmd_tx.clone();
    let recheck_secs = cfg.position_recheck_interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(recheck_secs));
        interval.tick().await; // skip immediate first tick
        loop {
            interval.tick().await;
            let _ = recheck_tx.send(OmsCommand::PositionRecheck).await;
        }
    });

    // ── 14. Await shutdown signal or KV fencing ───────────────────────────────
    info!("zk-oms-svc running — press Ctrl-C to stop");
    let fenced = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received");
            false
        }
        _ = registration.wait_fenced() => {
            warn!("KV fencing detected — another instance owns this identity, shutting down");
            true
        }
    };

    // Signal all tasks to stop.
    shutdown.cancel();

    // Wait for writer and gRPC server to drain.
    let _ = tokio::join!(writer_handle, grpc_handle);
    for h in sub_handles {
        h.abort();
    }

    // Deregister from NATS KV only on clean shutdown (not when fenced — the
    // new owner already holds the key).
    if !fenced {
        registration.deregister().await.ok();
    }

    info!("zk-oms-svc stopped");
    Ok(())
}
