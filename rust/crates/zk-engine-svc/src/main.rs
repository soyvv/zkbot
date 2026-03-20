//! `zk-engine-svc` — production engine service binary.
//!
//! # Startup sequence
//! 1. Load `EngineSvcConfig` from `ZK_*` env vars.
//! 2. Initialise tracing.
//! 3. Connect NATS, bootstrap with Pilot (or direct mode).
//! 4. Connect TradingClient.
//! 5. Rehydrate initial state from OMS (stubbed).
//! 6. Build LiveEngine with TradingDispatcher.
//! 7. Start gRPC server (control + query API).
//! 8. Register in NATS KV with CAS heartbeat.
//! 9. Start RTMD/OMS subscriptions + timer clock.
//! 10. Run engine event loop.
//! 11. Supervise: await Ctrl-C or KV fencing.
//! 12. Graceful shutdown.

use zk_engine_svc::config;
use zk_infra_rs::tracing as zk_tracing;
use zk_strategy_host_rs::{build_strategy, StrategySpec};

use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. Config ─────────────────────────────────────────────────────
    let cfg = config::load().expect("Failed to load EngineSvcConfig from env");

    // ── 2. Tracing ────────────────────────────────────────────────────
    zk_tracing::init_tracing(&format!("zk-engine-svc[{}]", cfg.engine_id));
    info!(
        engine_id = %cfg.engine_id,
        grpc_port = cfg.grpc_port,
        strategy_key = %cfg.strategy_key,
        "starting zk-engine-svc"
    );

    // ── 3-12. Run full lifecycle ──────────────────────────────────────
    // Default to the shared noop strategy placeholder until runtime strategy
    // selection is wired through Pilot/runtime config.
    let strategy = build_strategy(&StrategySpec::Noop)?;
    let refdata = vec![];

    zk_engine_svc::runtime::run(cfg, strategy, refdata).await
}
