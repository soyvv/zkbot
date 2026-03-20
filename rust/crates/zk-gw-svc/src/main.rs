use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use zk_gw_svc::config::GwSvcConfig;
use zk_gw_svc::grpc_handler::GrpcHandler;
use zk_gw_svc::gw_executor::GwExecPool;
use zk_gw_svc::nats_publisher::NatsPublisher;
use zk_gw_svc::proto::zk_gw_v1::gateway_service_server::GatewayServiceServer;
use zk_gw_svc::proto::zk_gw_v1::gateway_simulator_admin_service_server::GatewaySimulatorAdminServiceServer;
use zk_gw_svc::reconnect::GatewayState;
use zk_gw_svc::semantic_pipeline::SemanticPipeline;
use zk_gw_svc::venue::simulator::admin::SimAdminHandler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zk_gw_svc=info,warn".into()),
        )
        .init();

    let cfg = GwSvcConfig::from_env();
    info!(
        gw_id = cfg.gw_id,
        venue = cfg.venue,
        grpc_port = cfg.grpc_port,
        account_id = cfg.account_id,
        match_policy = cfg.match_policy,
        "zk-gw-svc starting"
    );

    // ── NATS connection ────────────────────────────────────────────────────
    let nats_client = if let Some(ref url) = cfg.nats_url {
        info!(url, "connecting to NATS");
        Some(async_nats::connect(url).await?)
    } else {
        info!("ZK_NATS_URL not set — NATS publishing disabled");
        None
    };

    // ── Gateway state ──────────────────────────────────────────────────────
    let gw_state = Arc::new(Mutex::new(GatewayState::Starting));

    // ── Build venue adapter via factory ──────────────────────────────────
    let built = zk_gw_svc::venue::build_adapter(&cfg).await?;
    let adapter = built.adapter;

    // Connect adapter.
    adapter.connect().await?;

    // ── Semantic pipeline + publisher ──────────────────────────────────────
    let pipeline = Arc::new(Mutex::new(SemanticPipeline::new(cfg.account_id)));

    let publisher = if let Some(ref nats) = nats_client {
        Some(Arc::new(NatsPublisher::new(
            nats.clone(),
            cfg.gw_id.clone(),
        )))
    } else {
        None
    };

    // ── Event loop: adapter events → pipeline → publisher ──────────────────
    if let Some(ref pub_arc) = publisher {
        let adapter_clone = Arc::clone(&adapter);
        let pipeline_clone = Arc::clone(&pipeline);
        let pub_clone = Arc::clone(pub_arc);

        tokio::spawn(async move {
            loop {
                match adapter_clone.next_event().await {
                    Ok(event) => {
                        let mut pl = pipeline_clone.lock().await;
                        pl.process(event, &pub_clone).await;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "event loop error — channel closed");
                        break;
                    }
                }
            }
        });
    }

    // ── Transition: Starting → Connecting → Live ──────────────────────────
    {
        let mut state = gw_state.lock().await;
        *state = GatewayState::Connecting;
    }
    {
        let mut state = gw_state.lock().await;
        *state = GatewayState::Live;
    }

    // Publish GW_EVENT_STARTED.
    if let Some(ref pub_arc) = publisher {
        let event = zk_proto_rs::zk::exch_gw::v1::GatewaySystemEvent {
            gw_name: cfg.gw_id.clone(),
            event_type: zk_proto_rs::zk::exch_gw::v1::GatewayEventType::GwEventStarted as i32,
            service_endpoint: format!("0.0.0.0:{}", cfg.grpc_port),
            event_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        };
        pub_arc.publish_system_event(&event).await;
    }

    info!(gw_id = cfg.gw_id, "gateway LIVE");

    // ── Internal execution pool ─────────────────────────────────────────
    let exec_pool = if let Some(ref pub_arc) = publisher {
        Arc::new(GwExecPool::new(
            cfg.exec_shard_count,
            cfg.exec_queue_capacity,
            Arc::clone(&adapter),
            Arc::clone(pub_arc),
            cfg.gw_id.clone(),
            cfg.account_id,
        ))
    } else {
        // Even without NATS, we need the exec pool for gRPC to work.
        // Create a dummy NatsPublisher — rejection reports will fail to publish
        // but the pool itself will function. In practice NATS is always present.
        panic!("ZK_NATS_URL is required for gateway operation");
    };

    // ── gRPC servers ───────────────────────────────────────────────────────
    let gw_handler = GrpcHandler {
        exec_pool: Arc::clone(&exec_pool),
        adapter: Arc::clone(&adapter),
        gw_state: Arc::clone(&gw_state),
        account_id: cfg.account_id,
    };

    let gw_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port).parse()?;
    let gw_listener = tokio::net::TcpListener::bind(gw_addr).await?;

    // Bind admin listener early (before registration) so the port is guaranteed live.
    let admin_listener = if cfg.venue == "simulator" && cfg.enable_admin_controls {
        let admin_addr: SocketAddr = format!("0.0.0.0:{}", cfg.admin_grpc_port).parse()?;
        Some(tokio::net::TcpListener::bind(admin_addr).await?)
    } else {
        None
    };


    // ── NATS KV self-registration (after adapter connected + listener bound) ──
    let mut registration = if let Some(ref nats) = nats_client {
        let js = async_nats::jetstream::new(nats.clone());
        let kv_prefix =
            std::env::var("ZK_GATEWAY_KV_PREFIX").unwrap_or_else(|_| "svc.gw".to_string());
        let kv_key = format!("{kv_prefix}.{}", cfg.gw_id);
        let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
        let reg_proto = zk_infra_rs::discovery_registration::gw_registration(
            &cfg.gw_id,
            &grpc_address,
            &cfg.venue,
            cfg.account_id,
        );
        let kv_value = zk_infra_rs::discovery_registration::encode_registration(&reg_proto);

        let reg = zk_infra_rs::service_registry::ServiceRegistration::register_direct(
            &js,
            kv_key.clone(),
            kv_value,
            std::time::Duration::from_secs(15),
        )
        .await
        .expect("failed to register in NATS KV");
        info!(kv_key, "registered in NATS KV");
        Some(reg)
    } else {
        None
    };

    // ── Serve ──────────────────────────────────────────────────────────────
    let fenced;

    if let Some(admin_listener) = admin_listener {
        let handles = built
            .simulator_handles
            .expect("simulator_handles must be present when venue=simulator");

        let admin_handler = SimAdminHandler {
            sim_state: Arc::clone(&handles.sim_state),
            adapter: Arc::clone(&handles.sim_adapter),
            publisher: publisher
                .clone()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "ZK_NATS_URL must be set when admin controls are enabled (ZK_ENABLE_ADMIN_CONTROLS=true)"
                    )
                })?,
            pipeline: Arc::clone(&pipeline),
        };

        info!(gw_addr = %gw_listener.local_addr()?, admin_addr = %admin_listener.local_addr()?, "starting dual gRPC servers");

        let gw_incoming = tokio_stream::wrappers::TcpListenerStream::new(gw_listener);
        let admin_incoming = tokio_stream::wrappers::TcpListenerStream::new(admin_listener);

        let gw_server = tonic::transport::Server::builder()
            .add_service(GatewayServiceServer::new(gw_handler))
            .serve_with_incoming(gw_incoming);

        let admin_server = tonic::transport::Server::builder()
            .add_service(GatewaySimulatorAdminServiceServer::new(admin_handler))
            .serve_with_incoming(admin_incoming);

        if let Some(ref mut reg) = registration {
            fenced = tokio::select! {
                r = gw_server => { r?; false }
                r = admin_server => { r?; false }
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown signal received");
                    false
                }
                _ = reg.wait_fenced() => {
                    tracing::warn!("KV fencing detected — shutting down");
                    true
                }
            };
        } else {
            tokio::select! {
                r = gw_server => r?,
                r = admin_server => r?,
            }
            fenced = false;
        }
    } else {
        info!(addr = %gw_listener.local_addr()?, "starting gRPC server");

        let gw_incoming = tokio_stream::wrappers::TcpListenerStream::new(gw_listener);
        let gw_server = tonic::transport::Server::builder()
            .add_service(GatewayServiceServer::new(gw_handler))
            .serve_with_incoming(gw_incoming);

        if let Some(ref mut reg) = registration {
            fenced = tokio::select! {
                r = gw_server => { r?; false }
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown signal received");
                    false
                }
                _ = reg.wait_fenced() => {
                    tracing::warn!("KV fencing detected — shutting down");
                    true
                }
            };
        } else {
            gw_server.await?;
            fenced = false;
        }
    }

    // Deregister on clean shutdown (not when fenced — new owner holds the key).
    if !fenced {
        if let Some(ref reg) = registration {
            reg.deregister().await.ok();
        }
    }

    info!("zk-gw-svc stopped");
    Ok(())
}
