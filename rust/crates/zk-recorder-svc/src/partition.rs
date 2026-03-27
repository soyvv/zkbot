//! Periodic partition maintenance task.
//!
//! Runs `trd.ensure_order_oms_partitions()` and
//! `trd.drop_expired_order_oms_partitions($1)` on a configurable interval.
//! Also runs once at startup before the consumer loops begin.

use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::db;

/// Run partition maintenance once (startup call).
pub async fn run_once(pool: &PgPool, retention_months: u32) -> anyhow::Result<()> {
    db::ensure_partitions(pool).await?;
    db::drop_expired_partitions(pool, retention_months).await?;
    info!("partition maintenance completed (startup)");
    Ok(())
}

/// Spawn a periodic partition maintenance task.
pub fn spawn_periodic(
    pool: PgPool,
    interval_secs: u64,
    retention_months: u32,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // First tick fires immediately — skip it since we already ran at startup.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("partition maintenance task shutting down");
                    break;
                }
                _ = interval.tick() => {
                    if let Err(e) = db::ensure_partitions(&pool).await {
                        error!(error = %e, "failed to ensure partitions");
                    }
                    if let Err(e) = db::drop_expired_partitions(&pool, retention_months).await {
                        error!(error = %e, "failed to drop expired partitions");
                    }
                    info!("partition maintenance completed (periodic)");
                }
            }
        }
    })
}
