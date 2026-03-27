//! `zk-recorder-svc` — persists terminal OMS orders and trades from JetStream
//! into Postgres.
//!
//! # Startup sequence
//! 1. Load `RecorderBootstrapConfig` from `ZK_*` env vars.
//! 2. Initialise tracing.
//! 3. Connect to NATS, create JetStream context.
//! 4. Determine bootstrap mode and assemble `RecorderRuntimeConfig`.
//! 5. Connect to PostgreSQL.
//! 6. Ensure JetStream stream exists.
//! 7. Run partition maintenance (startup).
//! 8. Spawn JetStream consumers (terminal-order + trade).
//! 9. Spawn periodic partition maintenance task.
//! 10. Await shutdown signal (SIGTERM / SIGINT).

use tokio_util::sync::CancellationToken;
use tracing::info;

use zk_infra_rs::{
    bootstrap::{self, BootstrapMode},
    nats_js,
    tracing as zk_tracing,
};
use zk_recorder_svc::{
    config::{self, RecorderService},
    consumer, partition,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. Bootstrap config ────────────────────────────────────────────────
    let boot_cfg =
        config::load_bootstrap().expect("Failed to load RecorderBootstrapConfig from env");

    // ── 2. Tracing ─────────────────────────────────────────────────────────
    zk_tracing::init_tracing(&format!("zk-recorder-svc[{}]", boot_cfg.recorder_id));
    info!(recorder_id = %boot_cfg.recorder_id, "starting zk-recorder-svc");

    // ── 3. Connect NATS ────────────────────────────────────────────────────
    let nats_client = zk_infra_rs::nats::connect(&boot_cfg.nats_url)
        .await
        .expect("NATS connect failed");
    info!("NATS connected");
    let js = async_nats::jetstream::new(nats_client.clone());

    // ── 4. Assemble runtime config ─────────────────────────────────────────
    let outcome = if boot_cfg.bootstrap_token.is_empty() {
        let outcome = bootstrap::bootstrap_runtime_config::<RecorderService>(
            &boot_cfg,
            BootstrapMode::Direct,
        )
        .expect("Failed to assemble runtime config (direct mode)");
        info!(source = "direct", "runtime config assembled");
        outcome
    } else {
        let grant = zk_infra_rs::service_registry::ServiceRegistration::pilot_request(
            &nats_client,
            &boot_cfg.bootstrap_token,
            &boot_cfg.recorder_id,
            &boot_cfg.instance_type,
            &boot_cfg.env,
            std::collections::HashMap::new(),
        )
        .await
        .expect("Pilot bootstrap request failed");

        let outcome = bootstrap::bootstrap_runtime_config::<RecorderService>(
            &boot_cfg,
            BootstrapMode::Pilot {
                payload: grant.payload.clone(),
                validate_hash: false,
            },
        )
        .expect("Failed to assemble runtime config (Pilot mode)");
        info!(source = "pilot", "runtime config assembled");
        outcome
    };

    let cfg = outcome.runtime_config;
    info!(
        recorder_id = %cfg.recorder_id,
        env = %cfg.env,
        "effective runtime config loaded"
    );

    // ── 5. Connect PostgreSQL ──────────────────────────────────────────────
    let pg_pool = zk_infra_rs::pg::connect_with_max(&cfg.pg_url, cfg.pg_max_connections)
        .await
        .expect("PostgreSQL connect failed");
    info!("PostgreSQL connected");

    // ── 6. Ensure JetStream stream ─────────────────────────────────────────
    nats_js::ensure_recorder_stream(&js)
        .await
        .expect("Failed to ensure recorder JetStream stream");
    info!("JetStream stream ready");

    // ── 7. Partition maintenance (startup) ─────────────────────────────────
    partition::run_once(&pg_pool, cfg.partition_retention_months)
        .await
        .expect("Startup partition maintenance failed");

    // ── 8. Spawn consumers ─────────────────────────────────────────────────
    let shutdown = CancellationToken::new();

    let order_handle = consumer::spawn_terminal_order_consumer(
        &js,
        pg_pool.clone(),
        cfg.consumer_ack_wait_secs,
        shutdown.clone(),
    )
    .await?;

    let trade_handle = consumer::spawn_trade_consumer(
        &js,
        pg_pool.clone(),
        cfg.consumer_ack_wait_secs,
        shutdown.clone(),
    )
    .await?;

    info!("consumers started");

    // ── 9. Spawn periodic partition maintenance ────────────────────────────
    let partition_handle = partition::spawn_periodic(
        pg_pool.clone(),
        cfg.partition_maintenance_interval_secs,
        cfg.partition_retention_months,
        shutdown.clone(),
    );

    // ── 10. Await shutdown ─────────────────────────────────────────────────
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for ctrl-c");
    info!("shutdown signal received");

    shutdown.cancel();

    let _ = tokio::join!(order_handle, trade_handle, partition_handle);
    info!("zk-recorder-svc stopped");

    Ok(())
}
