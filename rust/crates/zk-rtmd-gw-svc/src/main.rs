use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use zk_infra_rs::discovery_registration;
use zk_infra_rs::service_registry::ServiceRegistration;
use zk_rtmd_gw_svc::{
    config::RtmdGwConfig,
    grpc_handler::RtmdQueryHandler,
    nats_publisher::RtmdNatsPublisher,
    nats_sub_source::NatsKvSubSource,
    proto::zk_rtmd_v1::rtmd_query_service_server::RtmdQueryServiceServer,
};
use zk_rtmd_rs::{sub_manager::SubscriptionManager, venue::build_adapter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zk_rtmd_gw_svc=info,warn".into()),
        )
        .init();

    let cfg = RtmdGwConfig::from_env();
    info!(
        mdgw_id = %cfg.mdgw_id,
        venue = %cfg.venue,
        grpc_port = cfg.grpc_port,
        "zk-rtmd-gw-svc starting"
    );

    // ── NATS connection ────────────────────────────────────────────────────
    info!(url = %cfg.nats_url, "connecting to NATS");
    let nats_client = async_nats::connect(&cfg.nats_url).await?;
    let js = async_nats::jetstream::new(nats_client.clone());

    // ── Venue adapter ──────────────────────────────────────────────────────
    let adapter = {
        #[cfg(feature = "python-venue")]
        {
            if let Some(ref venue_root) = cfg.venue_root {
                let root = std::path::PathBuf::from(venue_root);
                // Fail-fast: if venue_root is set, manifest must load successfully.
                let manifest =
                    zk_pyo3_bridge::manifest::load_manifest(&root, &cfg.venue)?;
                // Capability miss is OK — venue may only declare gw/refdata, not rtmd.
                let rtmd_cap = zk_pyo3_bridge::manifest::resolve_capability(
                    &manifest,
                    zk_pyo3_bridge::manifest::CAP_RTMD,
                )
                .ok()
                .filter(|cap| cap.language == "python");

                if let Some(cap) = rtmd_cap {
                    zk_pyo3_bridge::manifest::validate_config(
                        &root, &cfg.venue, cap, &cfg.venue_config,
                    )?;
                    let ep =
                        zk_pyo3_bridge::manifest::parse_python_entrypoint(&cap.entrypoint)?;
                    let rt = zk_pyo3_bridge::py_runtime::PyRuntime::initialize(&root)?;
                    let handle =
                        rt.load_class(&ep, cfg.venue_config.clone(), Some(&cfg.venue))?;
                    Arc::new(zk_pyo3_bridge::rtmd_adapter::PyRtmdVenueAdapter::new(handle))
                        as Arc<dyn zk_rtmd_rs::venue_adapter::RtmdVenueAdapter>
                } else {
                    build_adapter(&cfg.venue, &cfg.venue_config)?
                }
            } else {
                build_adapter(&cfg.venue, &cfg.venue_config)?
            }
        }
        #[cfg(not(feature = "python-venue"))]
        {
            build_adapter(&cfg.venue, &cfg.venue_config)?
        }
    };
    adapter.connect().await?;
    info!(venue = %cfg.venue, "venue adapter connected");

    // ── Subscription KV bucket ─────────────────────────────────────────────
    let sub_bucket = &cfg.rtmd_sub_bucket;
    let sub_ttl = Duration::from_secs(cfg.sub_lease_ttl_s * 3);
    let sub_store = match js.get_key_value(sub_bucket).await {
        Ok(s) => s,
        Err(_) => js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: sub_bucket.to_string(),
                max_age: sub_ttl,
                ..Default::default()
            })
            .await?,
    };

    // ── Subscription manager ───────────────────────────────────────────────
    let sub_source = Arc::new(NatsKvSubSource::new(sub_store, cfg.venue.clone()));
    let sub_mgr = Arc::new(SubscriptionManager::new(
        Arc::clone(&adapter),
        Arc::clone(&sub_source) as _,
    ));
    sub_mgr.reconcile_on_start().await?;
    info!("subscription reconciliation complete");

    // Spawn subscription watch loop.
    let sub_mgr_watch = Arc::clone(&sub_mgr);
    tokio::spawn(async move {
        sub_mgr_watch.run_watch_loop().await;
    });

    // ── Event loop: adapter events → NATS publisher ────────────────────────
    let publisher = Arc::new(RtmdNatsPublisher::new(
        nats_client.clone(),
        cfg.venue.clone(),
        Arc::clone(&adapter),
    ));
    let adapter_events = Arc::clone(&adapter);
    let publisher_events = Arc::clone(&publisher);
    tokio::spawn(async move {
        loop {
            match adapter_events.next_event().await {
                Ok(event) => {
                    publisher_events.publish(event).await;
                }
                Err(zk_rtmd_rs::types::RtmdError::ChannelClosed) => {
                    tracing::info!("adapter event channel closed, event loop exiting");
                    break;
                }
                Err(e) => {
                    tracing::error!(error = %e, "event loop error");
                    break;
                }
            }
        }
    });

    // ── Bind gRPC listener before registering ──────────────────────────────
    let grpc_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port).parse()?;
    let listener = tokio::net::TcpListener::bind(grpc_addr).await?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "gRPC listener bound");

    // ── Service registry KV registration (after listener is ready) ─────────
    let kv_prefix = &cfg.gateway_kv_prefix;
    let kv_key = format!("{kv_prefix}.{}", cfg.mdgw_id);
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let capabilities = vec![
        "tick".to_string(), "kline".to_string(), "funding".to_string(),
        "orderbook".to_string(), "query_current".to_string(), "query_history".to_string(),
    ];
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("publisher_mode".to_string(), "standalone".to_string());
    metadata.insert("subscription_scope".to_string(), "global".to_string());
    metadata.insert(
        "query_types".to_string(),
        "current_tick,current_orderbook,current_funding,kline_history".to_string(),
    );
    let reg_proto = discovery_registration::mdgw_registration(
        &cfg.mdgw_id,
        &grpc_address,
        &cfg.venue,
        capabilities,
        metadata,
    );
    let kv_value = discovery_registration::encode_registration(&reg_proto);

    let mut registration = ServiceRegistration::register_direct(
        &js,
        kv_key.clone(),
        kv_value,
        Duration::from_secs(15),
    )
    .await
    .expect("failed to register in NATS KV");
    info!(kv_key = %kv_key, "registered in NATS KV");

    // ── gRPC server (serve on already-bound listener) ──────────────────────
    let handler = RtmdQueryHandler { adapter: Arc::clone(&adapter) };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    info!(mdgw_id = %cfg.mdgw_id, "zk-rtmd-gw-svc LIVE");

    let grpc_server = tonic::transport::Server::builder()
        .add_service(RtmdQueryServiceServer::new(handler))
        .serve_with_incoming(incoming);

    let fenced = tokio::select! {
        r = grpc_server => {
            r?;
            false
        }
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received");
            false
        }
        _ = registration.wait_fenced() => {
            tracing::warn!("KV fencing detected — shutting down");
            true
        }
    };

    if !fenced {
        registration.deregister().await.ok();
    }

    info!("zk-rtmd-gw-svc stopped");
    Ok(())
}
