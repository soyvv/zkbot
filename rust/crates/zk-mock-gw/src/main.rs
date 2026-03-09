mod fill;
mod gateway;
mod proto;
mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::gateway::MockGatewayHandler;
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

    tonic::transport::Server::builder()
        .add_service(ExchangeGatewayServiceServer::new(MockGatewayHandler { state }))
        .serve(addr)
        .await?;

    Ok(())
}
