mod fill;
mod zk_gateway;
mod gateway;
mod proto;
mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::gateway::MockGatewayHandler;
use crate::zk_gateway::ZkGatewayHandler;
use crate::proto::zk_gw_v1::gateway_service_server::GatewayServiceServer;
use crate::proto::tqrpc_exch_gw::exchange_gateway_service_server::ExchangeGatewayServiceServer;
use crate::state::MockGwState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zk_mock_gw=info,warn".into()),
        )
        .init();

    // ── Config from env ──────────────────────────────────────────────────────
    let gw_id = std::env::var("ZK_GW_ID").unwrap_or_else(|_| "gw_mock_1".to_string());
    let account_id: i64 = std::env::var("ZK_ACCOUNT_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);
    let fill_delay_ms: u64 = std::env::var("ZK_FILL_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let nats_url = std::env::var("ZK_NATS_URL").ok();
    let grpc_port: u16 = std::env::var("ZK_GRPC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(51051);
    let balances_str = std::env::var("ZK_MOCK_BALANCES")
        .unwrap_or_else(|_| "BTC:10,USDT:100000,ETH:50".to_string());

    info!(gw_id, account_id, fill_delay_ms, grpc_port, "zk-mock-gw starting");

    // ── NATS connection (optional) ────────────────────────────────────────────
    let nats_client = if let Some(url) = nats_url {
        info!(url, "connecting to NATS");
        Some(async_nats::connect(&url).await?)
    } else {
        info!("ZK_NATS_URL not set — OrderReport publishing disabled");
        None
    };

    // ── NATS KV self-registration ─────────────────────────────────────────────
    // Register under `svc.gw.{gw_id}` so OMS can discover this gateway.
    // OMS watches this prefix and connects via gRPC using the address in PG.
    if let Some(ref nats) = nats_client {
        let kv_prefix = std::env::var("ZK_GATEWAY_KV_PREFIX")
            .unwrap_or_else(|_| "svc.gw".to_string());
        let kv_key = format!("{kv_prefix}.{gw_id}");
        let kv_val_str = format!(
            r#"{{"service_type":"GW","gw_id":"{gw_id}","grpc_port":{grpc_port}}}"#
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

        // Heartbeat — re-put every 15 s so the TTL-based entry stays alive.
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

    // ── State ─────────────────────────────────────────────────────────────────
    let balances: HashMap<String, f64> = MockGwState::parse_balances(&balances_str);
    let state = Arc::new(Mutex::new(MockGwState::new(
        gw_id.clone(),
        account_id,
        fill_delay_ms,
        balances,
        nats_client,
    )));

    // ── gRPC server ───────────────────────────────────────────────────────────
    let addr: SocketAddr = format!("0.0.0.0:{grpc_port}").parse()?;
    info!(%addr, "gRPC server listening");

    let zk_handler = ZkGatewayHandler { state: Arc::clone(&state) };
    tonic::transport::Server::builder()
        .add_service(ExchangeGatewayServiceServer::new(MockGatewayHandler { state }))
        .add_service(GatewayServiceServer::new(zk_handler))
        .serve(addr)
        .await?;

    Ok(())
}
