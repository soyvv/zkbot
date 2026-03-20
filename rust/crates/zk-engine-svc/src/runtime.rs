//! Engine runtime — composes and orchestrates all service components.
//!
//! This module ties together:
//! - `LiveEngine` (from zk-engine-rs)
//! - `TradingDispatcher` (from dispatcher.rs)
//! - Event subscriptions (from subscriptions.rs)
//! - gRPC server (control + query APIs)
//! - Timer clock
//! - KV registration + supervision

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use zk_engine_rs::{run_timer_clock, EngineReadReplica, EventEnvelope, LiveEngine};
use zk_proto_rs::zk::common::v1::InstrumentRefData;
use zk_strategy_sdk_rs::strategy::Strategy;
use zk_trading_sdk_rs::client::TradingClient;

use crate::bootstrap;
use crate::config::EngineSvcConfig;
use crate::control_api::ControlApiState;
use crate::dispatcher::TradingDispatcher;
use crate::proto::engine_svc::engine_service_server::{EngineService, EngineServiceServer};
use crate::query_api::QueryApiState;
use crate::supervision::{self, ShutdownReason};

use zk_proto_rs::zk::common::v1::{CommandAck, DummyRequest, ServiceHealthResponse};
use zk_proto_rs::zk::engine::v1::{
    EngineStatusRequest, EngineStatusResponse, PauseEngineRequest, ResumeEngineRequest,
    StartEngineRequest, StopEngineRequest,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;
const GRACE_QUEUE_CAPACITY: usize = 512;
const CONTROL_QUEUE_CAPACITY: usize = 32;

/// Combined gRPC handler implementing `EngineService`.
///
/// Delegates to `ControlApiState` and `QueryApiState` internally.
pub struct EngineGrpcHandler {
    control: Arc<ControlApiState>,
    query: Arc<QueryApiState>,
}

#[tonic::async_trait]
impl EngineService for EngineGrpcHandler {
    async fn start(
        &self,
        request: Request<StartEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        self.control.start(request).await
    }

    async fn stop(
        &self,
        request: Request<StopEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        self.control.stop(request).await
    }

    async fn pause(
        &self,
        request: Request<PauseEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        self.control.pause(request).await
    }

    async fn resume(
        &self,
        request: Request<ResumeEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        self.control.resume(request).await
    }

    async fn get_status(
        &self,
        request: Request<EngineStatusRequest>,
    ) -> Result<Response<EngineStatusResponse>, Status> {
        self.query.get_status(request).await
    }

    async fn health_check(
        &self,
        request: Request<DummyRequest>,
    ) -> Result<Response<ServiceHealthResponse>, Status> {
        self.query.health_check(request).await
    }
}

/// Run the full engine service lifecycle.
///
/// Generic over `S: Strategy` so the binary can plug in any concrete strategy.
///
/// 1. Bootstrap (Pilot claim or direct mode)
/// 2. Connect TradingClient
/// 3. Rehydrate initial state (stubbed)
/// 4. Build LiveEngine with TradingDispatcher
/// 5. Start gRPC server
/// 6. Register in NATS KV
/// 7. Start event subscriptions + timer clock
/// 8. Run engine event loop
/// 9. Supervise (fencing / signal)
/// 10. Graceful shutdown
pub async fn run<S: Strategy>(
    cfg: EngineSvcConfig,
    strategy: S,
    refdata: Vec<InstrumentRefData>,
) -> anyhow::Result<()> {
    let start_time = Instant::now();

    // ── 1. Infrastructure connections ───────────────────────────────────
    let nats_client = zk_infra_rs::nats::connect(&cfg.nats_url)
        .await
        .expect("NATS connect failed");
    info!("NATS connected");

    let js = async_nats::jetstream::new(nats_client.clone());

    // ── 2. Bootstrap ────────────────────────────────────────────────────
    let grant = bootstrap::bootstrap(&cfg, &nats_client, &js).await?;
    info!(
        execution_id = %grant.execution_id,
        strategy_key = %grant.strategy_key,
        accounts = ?grant.account_ids,
        instruments = ?grant.instruments,
        "bootstrap complete"
    );

    // ── 3. Connect TradingClient ────────────────────────────────────────
    let trading_config = zk_trading_sdk_rs::config::TradingClientConfig {
        nats_url: cfg.nats_url.clone(),
        env: cfg.env.clone(),
        account_ids: grant.account_ids.clone(),
        client_instance_id: grant.instance_id,
        discovery_bucket: cfg.discovery_bucket.clone(),
        refdata_grpc: None,
    };
    let trading_client = Arc::new(
        TradingClient::from_config(trading_config)
            .await
            .expect("TradingClient init failed"),
    );
    info!("TradingClient connected");

    // ── 4. Rehydrate initial state (stubbed) ────────────────────────────
    let _rehydrated = crate::rehydration::rehydrate(&trading_client, &grant.account_ids).await;

    // ── 5. Build LiveEngine ─────────────────────────────────────────────
    let dispatcher =
        TradingDispatcher::new(Arc::clone(&trading_client), grant.execution_id.clone());

    let mut engine = LiveEngine::new(
        grant.account_ids.clone(),
        refdata,
        strategy,
        dispatcher,
        grant.execution_id.clone(),
        grant.strategy_key.clone(),
    );

    // Grab the read replica before moving into the event loop task.
    let replica: EngineReadReplica = engine.read_replica();

    // ── 6. Event channel + control/grace queues ────────────────────────
    let (event_tx, event_rx) = mpsc::channel::<EventEnvelope>(EVENT_CHANNEL_CAPACITY);
    let (control_tx, mut control_rx) = mpsc::channel::<EventEnvelope>(CONTROL_QUEUE_CAPACITY);
    let (grace_tx, mut grace_rx) = mpsc::channel::<EventEnvelope>(GRACE_QUEUE_CAPACITY);

    // Priority forwarder: drains control (high-priority) and grace queues
    // into the engine channel. Control commands are always preferred.
    let engine_tx_fwd = event_tx.clone();
    let priority_forwarder = tokio::spawn(async move {
        loop {
            let envelope = tokio::select! {
                biased;
                Some(env) = control_rx.recv() => env,
                Some(env) = grace_rx.recv() => env,
                else => break,
            };
            if engine_tx_fwd.send(envelope).await.is_err() {
                warn!("engine channel closed — priority forwarder exiting");
                break;
            }
        }
    });

    // ── 7. Start gRPC server ────────────────────────────────────────────
    let control_state = Arc::new(ControlApiState {
        control_tx: control_tx.clone(),
        engine_id: cfg.engine_id.clone(),
    });
    let query_state = Arc::new(QueryApiState {
        replica: replica.clone(),
        engine_id: cfg.engine_id.clone(),
        start_time,
    });

    let grpc_handler = EngineGrpcHandler {
        control: control_state,
        query: query_state,
    };

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port)
        .parse()
        .expect("invalid grpc_port");

    let grpc_handle = tokio::spawn(async move {
        info!(%listen_addr, "gRPC server listening");
        Server::builder()
            .add_service(EngineServiceServer::new(grpc_handler))
            .serve(listen_addr)
            .await
            .expect("gRPC server error");
    });

    // ── 8. Register in NATS KV ──────────────────────────────────────────
    let mut registration = bootstrap::register_kv(&js, &cfg, &grant).await?;

    // ── 9. Start subscriptions + timer ──────────────────────────────────
    let mut sub_handles = Vec::new();

    // OMS update subscriptions (order, balance, position).
    let oms_handles =
        crate::subscriptions::subscribe_oms_updates(&trading_client, grace_tx.clone()).await;
    sub_handles.extend(oms_handles);

    // RTMD tick subscriptions.
    let tick_handles = crate::subscriptions::subscribe_ticks(
        &trading_client,
        &grant.instruments,
        event_tx.clone(),
    )
    .await;
    sub_handles.extend(tick_handles);

    // RTMD kline (bar) subscriptions.
    if !cfg.kline_interval.is_empty() {
        let kline_handles = crate::subscriptions::subscribe_klines(
            &trading_client,
            &grant.instruments,
            &cfg.kline_interval,
            grace_tx.clone(),
        )
        .await;
        sub_handles.extend(kline_handles);
    }

    // Timer clock (1 Hz by default).
    let timer_tx = event_tx.clone();
    let timer_interval = cfg.timer_interval_ms;
    let timer_handle = tokio::spawn(async move {
        run_timer_clock(timer_tx, timer_interval).await;
    });

    // ── 10. Run engine startup lifecycle ─────────────────────────────────
    engine.startup();
    info!("engine startup lifecycle complete (on_create → on_init → on_reinit)");

    // ── 11. Run engine event loop in a task ──────────────────────────────
    let engine_handle = tokio::spawn(async move {
        engine.run(event_rx).await;
        info!("engine event loop exited");
    });

    // ── 12. Supervise — wait for shutdown trigger ────────────────────────
    info!("zk-engine-svc running — press Ctrl-C to stop");
    let reason = supervision::wait_for_shutdown(&mut registration).await;

    // ── 13. Graceful shutdown ────────────────────────────────────────────
    info!("initiating graceful shutdown");

    // Send Stop command directly to engine channel (bypasses grace queue).
    supervision::stop_engine(&event_tx, "service shutdown").await;

    // Abort tasks that hold sender clones so channels can close.
    grpc_handle.abort(); // drops ControlApiState's control_tx clone
    for h in sub_handles {
        h.abort(); // drops subscription grace_tx / engine_tx clones
    }
    timer_handle.abort(); // drops timer_tx (engine_tx clone)

    // Drop local senders — all clones now gone, channels will close.
    drop(control_tx);
    drop(grace_tx);
    drop(event_tx);

    // Await priority forwarder drain (both control_rx and grace_rx closed).
    let _ = priority_forwarder.await;

    // Await engine loop exit (event_rx closed after forwarder drops engine_tx_fwd).
    let _ = engine_handle.await;

    // Deregister from KV only on clean shutdown (not when fenced).
    match reason {
        ShutdownReason::Signal => {
            registration.deregister().await.ok();
            info!("deregistered from NATS KV");
        }
        ShutdownReason::Fenced => {
            warn!("skipping KV deregister — fenced by another instance");
        }
    }

    info!("zk-engine-svc stopped");
    Ok(())
}
