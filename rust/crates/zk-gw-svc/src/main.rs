use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use zk_gw_svc::config::GwSvcConfig;
use zk_gw_svc::grpc_handler::GrpcHandler;
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

    // ── NATS KV self-registration ──────────────────────────────────────────
    if let Some(ref nats) = nats_client {
        let kv_prefix = std::env::var("ZK_GATEWAY_KV_PREFIX")
            .unwrap_or_else(|_| "svc.gw".to_string());
        let kv_key = format!("{kv_prefix}.{}", cfg.gw_id);
        let kv_val_str = format!(
            r#"{{"service_type":"GW","gw_id":"{}","grpc_port":{},"venue":"{}"}}"#,
            cfg.gw_id, cfg.grpc_port, cfg.venue
        );
        let kv_val = bytes::Bytes::from(kv_val_str.clone());

        let js = async_nats::jetstream::new(nats.clone());
        let bucket = "zk-svc-registry-v1";
        let store = match js.get_key_value(bucket).await {
            Ok(s) => s,
            Err(_) => js
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: bucket.to_string(),
                    max_age: std::time::Duration::from_secs(90),
                    ..Default::default()
                })
                .await
                .expect("KV bucket create failed"),
        };
        store
            .put(&kv_key, kv_val.clone())
            .await
            .expect("KV put failed");
        info!(kv_key, "registered in NATS KV");

        // Heartbeat loop.
        let kv_key2 = kv_key.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                ticker.tick().await;
                if let Err(e) = store.put(&kv_key2, kv_val.clone()).await {
                    tracing::warn!(kv_key = kv_key2, error = %e, "KV heartbeat failed");
                }
            }
        });
    }

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

    // ── gRPC servers ───────────────────────────────────────────────────────
    let gw_handler = GrpcHandler {
        adapter: Arc::clone(&adapter),
        gw_state: Arc::clone(&gw_state),
        account_id: cfg.account_id,
    };

    let gw_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port).parse()?;

    if cfg.venue == "simulator" && cfg.enable_admin_controls {
        let handles = built.simulator_handles.expect(
            "simulator_handles must be present when venue=simulator",
        );

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

        let admin_addr: SocketAddr = format!("0.0.0.0:{}", cfg.admin_grpc_port).parse()?;

        info!(%gw_addr, %admin_addr, "starting dual gRPC servers");

        let gw_server = tonic::transport::Server::builder()
            .add_service(GatewayServiceServer::new(gw_handler))
            .serve(gw_addr);

        let admin_server = tonic::transport::Server::builder()
            .add_service(GatewaySimulatorAdminServiceServer::new(admin_handler))
            .serve(admin_addr);

        tokio::select! {
            r = gw_server => r?,
            r = admin_server => r?,
        }
    } else {
        info!(%gw_addr, "starting gRPC server");
        tonic::transport::Server::builder()
            .add_service(GatewayServiceServer::new(gw_handler))
            .serve(gw_addr)
            .await?;
    }

    Ok(())
}
