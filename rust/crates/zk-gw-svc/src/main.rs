use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use zk_gw_svc::config::{GwBootstrap, GwBootstrapConfig, GwRuntimeConfig};
use zk_gw_svc::grpc_handler::GrpcHandler;
use zk_gw_svc::gw_executor::GwExecPool;
use zk_gw_svc::nats_publisher::NatsPublisher;
use zk_gw_svc::proto::zk_gw_v1::gateway_service_server::GatewayServiceServer;
use zk_gw_svc::proto::zk_gw_v1::gateway_simulator_admin_service_server::GatewaySimulatorAdminServiceServer;
use zk_gw_svc::reconnect::GatewayState;
use zk_gw_svc::semantic_pipeline::SemanticPipeline;
use zk_gw_svc::venue::simulator::admin::SimAdminHandler;
use zk_infra_rs::bootstrap::{bootstrap_runtime_config, BootstrapMode, BootstrapOutcome};
use zk_infra_rs::service_registry::{PilotBootstrapGrant, ServiceRegistration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── 1. Bootstrap config from env ─────────────────────────────────────
    let bootstrap = GwBootstrapConfig::from_env()
        .expect("failed to load bootstrap config from env");

    // ── 2. Tracing ───────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zk_gw_svc=info,warn".into()),
        )
        .init();

    info!(
        gw_id = bootstrap.gw_id,
        grpc_port = bootstrap.grpc_port,
        "zk-gw-svc starting"
    );

    // ── 3. NATS connection ───────────────────────────────────────────────
    let nats_client = if let Some(ref url) = bootstrap.nats_url {
        info!(url, "connecting to NATS");
        Some(async_nats::connect(url).await?)
    } else {
        info!("ZK_NATS_URL not set — NATS publishing disabled");
        None
    };

    // ── 4. Pilot request (split-phase) or direct-mode flag ───────────────
    let pilot_grant: Option<PilotBootstrapGrant> =
        if !bootstrap.bootstrap_token.is_empty() {
            let nats = nats_client
                .as_ref()
                .expect("ZK_NATS_URL is required for Pilot bootstrap");
            info!("sending Pilot bootstrap request");
            let mut runtime_info = std::collections::HashMap::new();
            runtime_info.insert(
                "grpc_address".into(),
                format!("{}:{}", bootstrap.grpc_host, bootstrap.grpc_port),
            );
            let grant = ServiceRegistration::pilot_request(
                nats,
                &bootstrap.bootstrap_token,
                &bootstrap.gw_id,
                &bootstrap.instance_type,
                &bootstrap.env,
                runtime_info,
            )
            .await
            .expect("Pilot registration failed — is Pilot running?");
            Some(grant)
        } else {
            None
        };

    // ── 5. Assemble runtime config ───────────────────────────────────────
    let mode = if let Some(ref grant) = pilot_grant {
        BootstrapMode::Pilot {
            payload: grant.payload.clone(),
            validate_hash: false,
        }
    } else {
        BootstrapMode::Direct
    };

    let outcome: BootstrapOutcome<GwRuntimeConfig> =
        bootstrap_runtime_config::<GwBootstrap>(&bootstrap, mode)
            .expect("config assembly failed");

    let runtime_cfg = &outcome.runtime_config;
    info!(
        gw_id = runtime_cfg.gw_id,
        venue = runtime_cfg.venue,
        account_id = runtime_cfg.account_id,
        grpc_port = runtime_cfg.grpc_port,
        source = ?outcome.source,
        "runtime config assembled"
    );

    // ── Gateway state ────────────────────────────────────────────────────
    let gw_state = Arc::new(Mutex::new(GatewayState::Starting));

    // ── 6. Build full KV registration proto ──────────────────────────────
    let admin_port = if let Some(ref sim) = runtime_cfg.simulator {
        if sim.enable_admin_controls {
            Some(sim.admin_grpc_port)
        } else {
            None
        }
    } else {
        None
    };
    let grpc_address = format!("{}:{}", runtime_cfg.grpc_host, runtime_cfg.grpc_port);
    let reg_proto = zk_infra_rs::discovery_registration::gw_registration(
        &runtime_cfg.gw_id,
        &grpc_address,
        &runtime_cfg.venue,
        runtime_cfg.account_id,
        admin_port,
    );
    let kv_value = zk_infra_rs::discovery_registration::encode_registration(&reg_proto);

    // ── 7. KV registration is deferred until gRPC listeners are bound. ─────
    // Registering earlier allows discovery consumers to dial a gateway that
    // has announced itself but is not yet actually serving.
    let mut registration: Option<ServiceRegistration> = None;

    // ── 8. Build venue adapter via factory ───────────────────────────────
    let built = zk_gw_svc::venue::build_adapter(runtime_cfg).await?;
    let adapter = built.adapter;

    // Connect adapter.
    adapter.connect().await?;

    // ── Semantic pipeline + publisher ────────────────────────────────────
    let pipeline = Arc::new(Mutex::new(SemanticPipeline::new(
        runtime_cfg.exch_account_id.clone(),
    )));

    let publisher = if let Some(ref nats) = nats_client {
        Some(Arc::new(NatsPublisher::new(
            nats.clone(),
            runtime_cfg.gw_id.clone(),
        )))
    } else {
        None
    };

    // ── Event loop: adapter events → pipeline → publisher ────────────────
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

    // ── Transition: Starting → Connecting → Live ─────────────────────────
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
            gw_name: runtime_cfg.gw_id.clone(),
            event_type: zk_proto_rs::zk::exch_gw::v1::GatewayEventType::GwEventStarted as i32,
            service_endpoint: format!("0.0.0.0:{}", runtime_cfg.grpc_port),
            event_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        };
        pub_arc.publish_system_event(&event).await;
    }

    info!(gw_id = runtime_cfg.gw_id, "gateway LIVE");

    // ── Internal execution pool ──────────────────────────────────────────
    let exec_pool = if let Some(ref pub_arc) = publisher {
        Arc::new(GwExecPool::new(
            runtime_cfg.exec_shard_count,
            runtime_cfg.exec_queue_capacity,
            Arc::clone(&adapter),
            Arc::clone(pub_arc),
            runtime_cfg.gw_id.clone(),
            runtime_cfg.account_id,
        ))
    } else {
        panic!("ZK_NATS_URL is required for gateway operation");
    };

    // ── gRPC servers ─────────────────────────────────────────────────────
    let gw_handler = GrpcHandler {
        exec_pool: Arc::clone(&exec_pool),
        adapter: Arc::clone(&adapter),
        gw_state: Arc::clone(&gw_state),
        account_id: runtime_cfg.account_id,
    };

    let gw_addr: SocketAddr = format!("0.0.0.0:{}", runtime_cfg.grpc_port).parse()?;
    let gw_listener = tokio::net::TcpListener::bind(gw_addr).await?;

    // Bind admin listener early (before serving) so the port is guaranteed live.
    let admin_listener = if let Some(ref sim) = runtime_cfg.simulator {
        if sim.enable_admin_controls {
            let admin_addr: SocketAddr =
                format!("0.0.0.0:{}", sim.admin_grpc_port).parse()?;
            Some(tokio::net::TcpListener::bind(admin_addr).await?)
        } else {
            None
        }
    } else {
        None
    };

    // ── Serve ────────────────────────────────────────────────────────────
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

        if let Some(ref nats) = nats_client {
            let js = async_nats::jetstream::new(nats.clone());
            let reg = if let Some(ref grant) = pilot_grant {
                let r = ServiceRegistration::register_kv_with_grant(
                    nats,
                    &js,
                    grant,
                    kv_value.clone(),
                    std::time::Duration::from_secs(15),
                )
                .await
                .expect("Pilot KV registration failed");
                info!(
                    kv_key = r.grant().kv_key,
                    session_id = r.grant().owner_session_id,
                    "registered via Pilot (split-phase)"
                );
                r
            } else {
                let kv_prefix =
                    std::env::var("ZK_GATEWAY_KV_PREFIX").unwrap_or_else(|_| "svc.gw".into());
                let kv_key = format!("{kv_prefix}.{}", runtime_cfg.gw_id);
                let r = ServiceRegistration::register_direct(
                    &js,
                    kv_key.clone(),
                    kv_value.clone(),
                    std::time::Duration::from_secs(15),
                )
                .await
                .expect("failed to register in NATS KV");
                info!(kv_key, "registered in NATS KV (direct)");
                r
            };
            registration = Some(reg);
        }

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

        if let Some(ref nats) = nats_client {
            let js = async_nats::jetstream::new(nats.clone());
            let reg = if let Some(ref grant) = pilot_grant {
                let r = ServiceRegistration::register_kv_with_grant(
                    nats,
                    &js,
                    grant,
                    kv_value.clone(),
                    std::time::Duration::from_secs(15),
                )
                .await
                .expect("Pilot KV registration failed");
                info!(
                    kv_key = r.grant().kv_key,
                    session_id = r.grant().owner_session_id,
                    "registered via Pilot (split-phase)"
                );
                r
            } else {
                let kv_prefix =
                    std::env::var("ZK_GATEWAY_KV_PREFIX").unwrap_or_else(|_| "svc.gw".into());
                let kv_key = format!("{kv_prefix}.{}", runtime_cfg.gw_id);
                let r = ServiceRegistration::register_direct(
                    &js,
                    kv_key.clone(),
                    kv_value.clone(),
                    std::time::Duration::from_secs(15),
                )
                .await
                .expect("failed to register in NATS KV");
                info!(kv_key, "registered in NATS KV (direct)");
                r
            };
            registration = Some(reg);
        }

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
