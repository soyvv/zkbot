use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

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

    // ── Service registry KV registration ──────────────────────────────────
    let kv_prefix = &cfg.gateway_kv_prefix;
    let kv_key = format!("{kv_prefix}.{}", cfg.mdgw_id);
    let kv_val = serde_json::json!({
        "service_type": "mdgw",
        "mdgw_id": cfg.mdgw_id,
        "venue": cfg.venue,
        "grpc_port": cfg.grpc_port,
        "capabilities": ["tick", "kline", "funding", "orderbook", "query_current"],
        "publisher_mode": "standalone",
        "subscription_scope": "global"
    })
    .to_string();
    let kv_val_bytes = bytes::Bytes::from(kv_val);

    let registry_bucket = "zk-svc-registry-v1";
    let registry_store = match js.get_key_value(registry_bucket).await {
        Ok(s) => s,
        Err(_) => js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: registry_bucket.to_string(),
                max_age: Duration::from_secs(90),
                ..Default::default()
            })
            .await?,
    };
    registry_store.put(&kv_key, kv_val_bytes.clone()).await?;
    info!(kv_key = %kv_key, "registered in NATS KV");

    // Heartbeat loop.
    let store_hb = registry_store.clone();
    let kv_key_hb = kv_key.clone();
    let kv_val_hb = kv_val_bytes.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            ticker.tick().await;
            if let Err(e) = store_hb.put(&kv_key_hb, kv_val_hb.clone()).await {
                tracing::warn!(error = %e, "KV heartbeat failed");
            }
        }
    });

    // ── Venue adapter ──────────────────────────────────────────────────────
    let adapter = build_adapter(&cfg.venue)?;
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
    let publisher = Arc::new(RtmdNatsPublisher::new(nats_client.clone(), cfg.venue.clone()));
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

    // ── gRPC server ────────────────────────────────────────────────────────
    let grpc_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port).parse()?;
    let handler = RtmdQueryHandler { adapter: Arc::clone(&adapter) };

    info!(%grpc_addr, mdgw_id = %cfg.mdgw_id, "zk-rtmd-gw-svc LIVE");

    tonic::transport::Server::builder()
        .add_service(RtmdQueryServiceServer::new(handler))
        .serve(grpc_addr)
        .await?;

    Ok(())
}
