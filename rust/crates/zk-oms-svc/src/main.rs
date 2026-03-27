//! `zk-oms-svc` — production OMS gRPC service.
//!
//! # Startup sequence
//! 1. Load `OmsBootstrapConfig` from `ZK_*` env vars.
//! 2. Initialise JSON/pretty tracing.
//! 3. Connect to NATS.
//! 4. Determine bootstrap mode and assemble `OmsRuntimeConfig`:
//!    - Direct mode: load provided config from env, assemble runtime config.
//!    - Pilot mode: register with Pilot, decode payload, assemble runtime config.
//! 5. Connect to Redis, PostgreSQL (using runtime config).
//! 6. Load `ConfdataManager` from PG (`cfg.*` tables).
//! 7. Warm-start: load open order snapshots from Redis → `OmsCore::init_state`.
//! 8. Watch `svc.gw.*` in NATS KV; connect to each live gateway via gRPC.
//! 9. Reconcile: `QueryAccountBalance` for each bound account (populate balances).
//! 10. Start OMS writer task (single-writer actor).
//! 11. Start tonic gRPC server.
//! 12. Register `svc.oms.<oms_id>` in NATS KV (with heartbeat).
//! 13. Start NATS subscribers for gateway reports.
//! 14. Start periodic tasks (cleanup, resync timers).
//! 15. Await shutdown signal (SIGTERM / SIGINT).

use zk_oms_svc::{
    config::{self, OmsService},
    config_introspection::OmsConfigIntrospection,
    db,
    grpc_handler,
    gw_client,
    gw_executor::GwExecutorPool,
    latency::LatencyEvent,
    nats_handler,
    oms_actor,
    persist_executor::PersistExecutorPool,
    reconcile,
    proto,
    publish_executor::PublishExecutorPool,
    recorder_executor,
    redis_writer,
};

use std::{net::SocketAddr, sync::Arc, time::Duration};

use arc_swap::ArcSwap;
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
    bootstrap::{self, BootstrapMode},
    nats_kv::KvRegistryClient,
    service_registry::ServiceRegistration,
    tracing as zk_tracing,
};
use zk_oms_rs::{config::ConfdataManager, oms_core_v2::OmsCoreV2};
use zk_proto_rs::zk::{
    exch_gw::v1::{BalanceUpdate, PositionReport},
    gateway::v1::PositionResponse,
};

fn gateway_addr_from_registration(value: &[u8]) -> anyhow::Result<String> {
    let reg = zk_infra_rs::discovery_registration::decode_registration(value)?;
    let endpoint = reg
        .endpoint
        .ok_or_else(|| anyhow::anyhow!("gateway registration missing endpoint"))?;
    let addr = endpoint.address.trim();
    if addr.is_empty() {
        return Err(anyhow::anyhow!("gateway registration has empty endpoint address"));
    }
    if addr.starts_with("http://") || addr.starts_with("https://") {
        Ok(addr.to_string())
    } else {
        Ok(format!("http://{addr}"))
    }
}

fn exch_account_code_for_gw(confdata: &ConfdataManager, gw_key: &str) -> Option<String> {
    confdata
        .gw_key_to_account_ids
        .get(gw_key)
        .and_then(|account_ids| {
            account_ids
                .iter()
                .find_map(|account_id| confdata.account_routes.get(account_id))
        })
        .map(|route| route.exch_account_id.clone())
}

fn normalize_gw_balance_update_for_oms(
    confdata: &ConfdataManager,
    gw_key: &str,
    mut update: BalanceUpdate,
) -> BalanceUpdate {
    let fallback_exch_account = exch_account_code_for_gw(confdata, gw_key).unwrap_or_default();
    for balance in &mut update.balances {
        if balance.exch_account_code.trim().is_empty() {
            balance.exch_account_code = fallback_exch_account.clone();
        }
    }
    update
}

fn position_response_to_balance_update_for_oms(
    confdata: &ConfdataManager,
    gw_key: &str,
    response: &PositionResponse,
) -> BalanceUpdate {
    use zk_proto_rs::zk::common::v1::InstrumentType;

    let fallback_exch_account = exch_account_code_for_gw(confdata, gw_key).unwrap_or_default();
    let balances = response
        .positions
        .iter()
        .map(|p| {
            let exch_account_code = confdata
                .account_routes
                .get(&p.account_id)
                .map(|route| route.exch_account_id.clone())
                .unwrap_or_else(|| fallback_exch_account.clone());

            PositionReport {
                instrument_code: p.instrument_code.clone(),
                instrument_type: if p.instrument_type != 0 {
                    p.instrument_type
                } else if p.instrument_code.contains("SWAP") {
                    InstrumentType::InstTypePerp as i32
                } else {
                    InstrumentType::InstTypeSpot as i32
                },
                long_short_type: p.long_short_type,
                qty: p.total_qty,
                avail_qty: p.avail_qty,
                exch_account_code,
                ..Default::default()
            }
        })
        .collect();
    BalanceUpdate { balances }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. Bootstrap config ──────────────────────────────────────────────────
    let boot_cfg = config::load_bootstrap().expect("Failed to load OmsBootstrapConfig from env");

    // ── 2. Tracing ───────────────────────────────────────────────────────────
    zk_tracing::init_tracing(&format!("zk-oms-svc[{}]", boot_cfg.oms_id));
    info!(oms_id = %boot_cfg.oms_id, "starting zk-oms-svc");

    // ── 3. Connect NATS (needs nats_url from bootstrap) ──────────────────────
    let nats_client = zk_infra_rs::nats::connect(&boot_cfg.nats_url)
        .await
        .expect("NATS connect failed");
    info!("NATS connected");
    let js = async_nats::jetstream::new(nats_client.clone());

    // ── 4. Determine mode and assemble runtime config ────────────────────────
    let (pilot_grant, outcome) = if boot_cfg.bootstrap_token.is_empty() {
        // Direct mode: load provided config from env vars, assemble runtime config.
        let outcome = bootstrap::bootstrap_runtime_config::<OmsService>(
            &boot_cfg,
            BootstrapMode::Direct,
        )
        .expect("Failed to assemble runtime config (direct mode)");
        info!(source = "direct", "runtime config assembled");
        (None, outcome)
    } else {
        // Pilot mode: request Pilot grant, then decode payload into runtime config.
        info!("requesting Pilot bootstrap grant");
        let grpc_address = format!("{}:{}", boot_cfg.grpc_host, boot_cfg.grpc_port);
        let mut runtime_info = std::collections::HashMap::new();
        runtime_info.insert("grpc_address".into(), grpc_address);

        let grant = ServiceRegistration::pilot_request(
            &nats_client,
            &boot_cfg.bootstrap_token,
            &boot_cfg.oms_id,
            &boot_cfg.instance_type,
            &boot_cfg.env,
            runtime_info,
        )
        .await
        .expect("Pilot bootstrap request failed — is Pilot running?");

        let outcome = bootstrap::bootstrap_runtime_config::<OmsService>(
            &boot_cfg,
            BootstrapMode::Pilot {
                payload: grant.payload.clone(),
                validate_hash: false, // until Pilot/Rust normalization contract confirmed
            },
        )
        .expect("Failed to assemble runtime config (Pilot mode)");
        info!(source = "pilot", "runtime config assembled");
        (Some(grant), outcome)
    };

    let cfg = outcome.runtime_config;
    let config_envelope = Arc::new(bootstrap::wrap_in_envelope(cfg.clone(), &outcome.source));
    info!(
        oms_id = %cfg.oms_id,
        grpc_port = cfg.grpc_port,
        source = outcome.source.as_metadata_source(),
        "effective runtime config loaded"
    );

    // ── 5. Connect Redis, PostgreSQL (using runtime config) ──────────────────
    let redis_conn = zk_infra_rs::redis::connect(&cfg.redis_url)
        .await
        .expect("Redis connect failed");
    info!("Redis connected");

    let pg_pool = zk_infra_rs::pg::connect(&cfg.pg_url)
        .await
        .expect("PostgreSQL connect failed");
    info!("PostgreSQL connected");

    // ── 6. Load ConfdataManager ──────────────────────────────────────────────
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

    // ── 7. Warm-start from Redis ─────────────────────────────────────────────
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

    // ── 8. Build OmsCoreV2 ───────────────────────────────────────────────────
    let mut core = OmsCoreV2::new(
        &confdata,
        false, // use_time_emulation: false for live trading
        cfg.risk_check_enabled,
        cfg.handle_external_orders,
        cfg.max_cached_orders,
    );
    core.init_state(warm_orders, persisted_positions, persisted_balances);
    info!("OmsCoreV2 initialised");

    // ── 9. Connect to gateways via NATS KV ──────────────────────────────────
    let kv_registry = KvRegistryClient::create(
        &js,
        zk_infra_rs::nats_kv::REGISTRY_BUCKET,
        Duration::from_secs(cfg.kv_heartbeat_secs * 3),
    )
    .await
    .expect("Failed to open/create KV registry bucket");

    let discovery = zk_infra_rs::nats_kv_discovery::KvDiscoveryClient::start(&js)
        .await
        .expect("Failed to start KV discovery client");
    let discovery_handle = discovery.spawn_watch_loop();

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
                            if confdata.gw_configs.contains_key(&gw_id) {
                                match gateway_addr_from_registration(e.value.as_ref()) {
                                    Ok(addr) => match gw_pool.connect(gw_id.clone(), &addr).await {
                                        Ok(_) => info!(gw_id, addr, "gateway gRPC connected"),
                                        Err(e) => warn!(gw_id, error = %e, "gateway gRPC connect failed"),
                                    },
                                    Err(e) => warn!(gw_id, error = %e, "gateway registration missing usable KV endpoint"),
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
    for gw_id in confdata.gw_configs.keys() {
        if gw_pool.contains(gw_id) {
            continue;
        }
        let kv_key = format!("{}.{}", cfg.gateway_kv_prefix, gw_id);
        match kv_registry.get(&kv_key).await {
            Ok(Some(value)) => {
                match gateway_addr_from_registration(value.as_ref()) {
                    Ok(addr) => match gw_pool.connect(gw_id.clone(), &addr).await {
                        Ok(_) => info!(gw_id, addr, "gateway gRPC connected (direct KV probe)"),
                        Err(e) => warn!(gw_id, error = %e, "gateway gRPC connect failed"),
                    },
                    Err(e) => warn!(gw_id, kv_key, error = %e, "gateway KV entry missing usable endpoint"),
                }
            }
            Ok(None) => warn!(
                gw_id,
                kv_key, "gateway not registered in KV — will connect when it registers"
            ),
            Err(e) => warn!(gw_id, error = %e, "KV probe failed"),
        }
    }

    // ── 10. Balance/position reconciliation ──────────────────────────────────
    {
        use zk_oms_rs::models_v2::OmsActionV2;
        use zk_proto_rs::zk::gateway::v1::{QueryAccountRequest, QueryPositionRequest};

        // Query each connected gateway once for account balances/positions.
        let gw_keys: Vec<String> = gw_pool.gw_keys().map(String::from).collect();
        for gw_key in &gw_keys {
            let req = QueryAccountRequest::default();
            match gw_pool.query_account_balance(gw_key, req).await {
                Ok(resp) => {
                    if let Some(balance_update) = resp.balance_update {
                        let balance_update =
                            normalize_gw_balance_update_for_oms(&confdata, gw_key, balance_update);
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
                    match gw_pool.query_position(gw_key, QueryPositionRequest {}).await {
                        Ok(pos_resp) => {
                            if !pos_resp.positions.is_empty() {
                                let balance_update = position_response_to_balance_update_for_oms(
                                    &confdata, gw_key, &pos_resp,
                                );
                                let actions = core.process_message(
                                    zk_oms_rs::models::OmsMessage::BalanceUpdate(balance_update),
                                );
                                for action in &actions {
                                    match action {
                                        OmsActionV2::PersistBalance { account_id, asset_id } => {
                                            oms_actor::persist_balance_to_redis(
                                                &core,
                                                &mut redis_writer,
                                                *account_id,
                                                *asset_id,
                                            )
                                            .await;
                                        }
                                        OmsActionV2::PersistPosition { account_id, instrument_id } => {
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
                        }
                        Err(e) => warn!(gw_key, error = %e, "startup position reconcile failed"),
                    }
                    info!(gw_key, "startup reconcile succeeded");
                }
                Err(e) => warn!(gw_key, error = %e, "startup reconcile failed"),
            }
        }
        info!("startup balance reconciliation complete");
    }

    // ── 10b. Order reconciliation (two-phase) ──────────────────────────────
    {
        use zk_oms_rs::models_v2::OmsActionV2;
        use zk_proto_rs::zk::gateway::v1::{
            QueryOpenOrderRequest, QueryOrderDetailRequest, SingleOrderQuery,
        };

        /// Apply a synthetic reconcile report: feed through core, persist results.
        async fn apply_reconcile_report(
            report: zk_proto_rs::zk::exch_gw::v1::OrderReport,
            core: &mut zk_oms_rs::oms_core_v2::OmsCoreV2,
            redis_writer: &mut redis_writer::RedisWriter,
        ) {
            let actions = core.process_message(
                zk_oms_rs::models::OmsMessage::GatewayOrderReport(report),
            );
            for action in &actions {
                match action {
                    OmsActionV2::PersistOrder { order_id, .. } => {
                        oms_actor::persist_order_to_redis(core, redis_writer, *order_id).await;
                    }
                    OmsActionV2::PersistBalance {
                        account_id,
                        asset_id,
                    } => {
                        oms_actor::persist_balance_to_redis(
                            core,
                            redis_writer,
                            *account_id,
                            *asset_id,
                        )
                        .await;
                    }
                    _ => {}
                }
            }
        }

        let snapshots = reconcile::extract_open_order_snapshots(&core);
        if snapshots.is_empty() {
            info!("startup order reconcile: no open orders to reconcile");
        } else {
            let now_ms = zk_oms_rs::utils::gen_timestamp_ms();
            let gw_keys: Vec<String> = gw_pool.gw_keys().map(String::from).collect();

            for gw_key in &gw_keys {
                // Phase 1: QueryOpenOrders
                let req = QueryOpenOrderRequest {
                    exch_account_code: String::new(),
                };
                let gw_result = tokio::time::timeout(
                    Duration::from_secs(10),
                    gw_pool.query_open_orders(gw_key, req),
                )
                .await;
                let gw_open_orders = match gw_result {
                    Ok(Ok(resp)) => resp.orders,
                    Ok(Err(e)) => {
                        warn!(gw_key, error = %e, "startup order reconcile: open orders query failed, skipping");
                        continue;
                    }
                    Err(_) => {
                        warn!(gw_key, "startup order reconcile: open orders query timed out, skipping");
                        continue;
                    }
                };
                let (reports, needs_detail, stats) = reconcile::reconcile_orders_against_gateway(
                    &snapshots,
                    &gw_open_orders,
                    gw_key,
                    now_ms,
                );
                info!(
                    gw_key,
                    matched = stats.orders_matched,
                    updated = stats.orders_updated,
                    need_detail = stats.orders_need_detail,
                    rejected = stats.orders_rejected,
                    "startup order reconcile phase 1",
                );

                // Apply immediate reports (state divergence + no-exch-ref rejects).
                for report in reports {
                    apply_reconcile_report(report, &mut core, &mut redis_writer).await;
                }

                // Phase 2: QueryOrderDetails for orders absent from open set.
                if !needs_detail.is_empty() {
                    let queries: Vec<SingleOrderQuery> = needs_detail
                        .iter()
                        .map(|snap| SingleOrderQuery {
                            exch_order_ref: snap.exch_order_ref.clone().unwrap_or_default(),
                            order_id: snap.order_id,
                            symbol: snap.instrument.clone(),
                            ..Default::default()
                        })
                        .collect();
                    let detail_req = QueryOrderDetailRequest {
                        order_queries: queries,
                    };
                    let detail_result = tokio::time::timeout(
                        Duration::from_secs(10),
                        gw_pool.query_order_details(gw_key, detail_req),
                    )
                    .await;
                    let detail_orders = match detail_result {
                        Ok(Ok(resp)) => resp.orders,
                        Ok(Err(e)) => {
                            warn!(gw_key, error = %e, "startup order reconcile: detail query failed, using fallback");
                            vec![]
                        }
                        Err(_) => {
                            warn!(gw_key, "startup order reconcile: detail query timed out, using fallback");
                            vec![]
                        }
                    };

                    for snap in &needs_detail {
                        let report = reconcile::resolve_missing_order(snap, &detail_orders, now_ms);
                        apply_reconcile_report(report, &mut core, &mut redis_writer).await;
                    }
                    info!(
                        gw_key,
                        terminated = needs_detail.len(),
                        detail_found = detail_orders.len(),
                        "startup order reconcile phase 2",
                    );
                }
            }
            info!("startup order reconciliation complete");
        }
    }

    // ── 11. Start OMS writer task ────────────────────────────────────────────
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

    // ── Executor pools + latency feedback channel ────────────────────────────
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

    {
        let mut discovery_rx = discovery.subscribe();
        let configured_gws: std::collections::HashSet<String> =
            confdata.gw_configs.keys().cloned().collect();
        let gw_id_lookup: std::collections::HashMap<String, u32> =
            gw_id_to_key.iter().map(|(id, key)| (key.clone(), *id)).collect();
        let gw_prefix = format!("{}.", cfg.gateway_kv_prefix);
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            loop {
                match discovery_rx.recv().await {
                    Ok(zk_infra_rs::nats_kv_discovery::DiscoveryEvent::Upsert { key, registration }) => {
                        if registration.service_type != "gw" {
                            continue;
                        }
                        let Some(gw_key) = key.strip_prefix(&gw_prefix).map(str::to_string) else {
                            continue;
                        };
                        if !configured_gws.contains(&gw_key) {
                            continue;
                        }
                        let Some(&gw_id) = gw_id_lookup.get(&gw_key) else {
                            continue;
                        };
                        let Some(endpoint) = registration.endpoint else {
                            continue;
                        };
                        let addr = endpoint.address.trim();
                        if addr.is_empty() {
                            continue;
                        }
                        let addr = if addr.starts_with("http://") || addr.starts_with("https://") {
                            addr.to_string()
                        } else {
                            format!("http://{addr}")
                        };
                        let _ = cmd_tx
                            .send(OmsCommand::GatewayDiscovered {
                                gw_key,
                                gw_id,
                                addr,
                            })
                            .await;
                    }
                    Ok(zk_infra_rs::nats_kv_discovery::DiscoveryEvent::Remove { key }) => {
                        let Some(gw_key) = key.strip_prefix(&gw_prefix).map(str::to_string) else {
                            continue;
                        };
                        let Some(&gw_id) = gw_id_lookup.get(&gw_key) else {
                            continue;
                        };
                        let _ = cmd_tx
                            .send(OmsCommand::GatewayRemoved { gw_key, gw_id })
                            .await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "gateway discovery receiver lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Recorder JetStream streams are owned by the recorder service / infra setup.
    // OMS only publishes to `zk.recorder.oms.<oms_id>.*.<account_id>` subjects.

    let persist_executor = PersistExecutorPool::new(1024, redis_writer.clone_writer());

    let nats_publisher = NatsPublisher::new(nats_client.clone(), js.clone(), cfg.oms_id.clone());
    let publish_executor = PublishExecutorPool::new(256, nats_publisher.clone(), latency_tx);
    let recorder_executor =
        recorder_executor::RecorderExecutorPool::new(256, nats_publisher.clone());

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
        recorder_executor,
        latency_rx,
        shutdown_clone,
        Duration::from_secs(cfg.metrics_interval_secs),
        cfg.metrics_max_pending,
        cfg.metrics_max_complete,
    );

    // ── 12. Start gRPC server ────────────────────────────────────────────────
    let handler = OmsGrpcHandler {
        cmd_tx: cmd_tx.clone(),
        replica: replica.clone(),
        oms_id: Arc::new(cfg.oms_id.clone()),
    };

    let config_handler = OmsConfigIntrospection {
        envelope: config_envelope,
        oms_id: Arc::new(cfg.oms_id.clone()),
    };

    use crate::proto::config_svc::config_introspection_service_server::ConfigIntrospectionServiceServer;

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port)
        .parse()
        .expect("invalid grpc_port");

    let grpc_shutdown = shutdown.clone();
    let grpc_handle = tokio::spawn(async move {
        info!(%listen_addr, "gRPC server listening");
        Server::builder()
            .add_service(OmsServiceServer::new(handler))
            .add_service(ConfigIntrospectionServiceServer::new(config_handler))
            .serve_with_shutdown(listen_addr, async move {
                grpc_shutdown.cancelled().await;
            })
            .await
            .expect("gRPC server error");
        info!("gRPC server stopped");
    });

    // ── 13. Register in NATS KV with heartbeat ──────────────────────────────
    let kv_key = format!("svc.oms.{}", cfg.oms_id);
    let account_ids: Vec<i64> = confdata.account_routes.keys().copied().collect();
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let reg_proto = zk_infra_rs::discovery_registration::oms_registration(
        &cfg.oms_id,
        &grpc_address,
        &account_ids,
    );
    let kv_value = zk_infra_rs::discovery_registration::encode_registration(&reg_proto);

    let mut registration = if let Some(grant) = pilot_grant {
        // Pilot mode: register now that the final discovery payload is known.
        let reg = ServiceRegistration::register_kv_with_grant(
            &nats_client,
            &js,
            &grant,
            kv_value,
            Duration::from_secs(cfg.kv_heartbeat_secs),
        )
        .await
        .expect("failed to register in NATS KV via Pilot grant");
        info!(
            kv_key = reg.grant().kv_key,
            session_id = reg.grant().owner_session_id,
            "registered in NATS KV via Pilot grant"
        );
        reg
    } else {
        // Direct mode: register now.
        ServiceRegistration::register_direct(
            &js,
            kv_key,
            kv_value,
            Duration::from_secs(cfg.kv_heartbeat_secs),
        )
        .await
        .expect("failed to register in NATS KV")
    };
    info!(
        kv_key = registration.grant().kv_key,
        session_id = registration.grant().owner_session_id,
        "registered in NATS KV"
    );

    // ── 14. Start NATS subscribers ──────────────────────────────────────────
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

    // ── 15. Periodic tasks ──────────────────────────────────────────────────
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

    let order_recheck_tx = cmd_tx.clone();
    let order_recheck_secs = cfg.order_resync_interval_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(order_recheck_secs));
        interval.tick().await; // skip first tick — startup reconcile already ran
        loop {
            interval.tick().await;
            let _ = order_recheck_tx
                .send(OmsCommand::OrderReconcile)
                .await;
        }
    });

    // ── 16. Await shutdown signal or KV fencing ─────────────────────────────
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
    discovery_handle.abort();
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

#[cfg(test)]
mod tests {
    use super::{
        normalize_gw_balance_update_for_oms, position_response_to_balance_update_for_oms,
    };
    use zk_oms_rs::config::ConfdataManager;
    use zk_proto_rs::ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry};
    use zk_proto_rs::zk::{
        common::v1::{InstrumentRefData, InstrumentType, LongShortType},
        exch_gw::v1::{BalanceUpdate, PositionReport},
        gateway::v1::PositionResponse,
        oms::v1::Position,
    };

    fn test_confdata() -> ConfdataManager {
        ConfdataManager::new(
            OmsConfigEntry {
                oms_id: "oms1".into(),
                managed_account_ids: vec![8002],
                ..Default::default()
            },
            vec![OmsRouteEntry {
                account_id: 8002,
                exch_account_id: "OKX-DEMO-1".into(),
                gw_key: "gw_okx_demo1".into(),
                ..Default::default()
            }],
            vec![GwConfigEntry {
                gw_key: "gw_okx_demo1".into(),
                exch_name: "OKX".into(),
                ..Default::default()
            }],
            Vec::<InstrumentRefData>::new(),
            Vec::new(),
        )
    }

    #[test]
    fn startup_balance_reconcile_backfills_exch_account_code() {
        let confdata = test_confdata();
        let update = BalanceUpdate {
            balances: vec![PositionReport {
                instrument_code: "USDT".into(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                qty: 1.0,
                avail_qty: 1.0,
                ..Default::default()
            }],
        };

        let normalized = normalize_gw_balance_update_for_oms(&confdata, "gw_okx_demo1", update);
        assert_eq!(normalized.balances[0].exch_account_code, "OKX-DEMO-1");
    }

    #[test]
    fn startup_position_reconcile_uses_exch_account_code_not_internal_account_id() {
        let confdata = test_confdata();
        let response = PositionResponse {
            positions: vec![Position {
                account_id: 8002,
                instrument_code: "BTC-USDT-SWAP".into(),
                instrument_type: InstrumentType::InstTypePerp as i32,
                long_short_type: LongShortType::LsLong as i32,
                total_qty: 0.5,
                avail_qty: 0.4,
                ..Default::default()
            }],
        };

        let update = position_response_to_balance_update_for_oms(
            &confdata,
            "gw_okx_demo1",
            &response,
        );
        assert_eq!(update.balances[0].exch_account_code, "OKX-DEMO-1");
        assert_eq!(
            update.balances[0].instrument_type,
            InstrumentType::InstTypePerp as i32
        );
    }
}
