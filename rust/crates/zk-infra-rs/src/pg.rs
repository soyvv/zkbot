use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Connect to PostgreSQL and return a connection pool.
///
/// `max_connections` defaults to 5, suitable for service binaries.
/// Adjust for high-throughput services like Recorder.
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    connect_with_max(url, 5).await
}

/// Connect with an explicit maximum pool size.
pub async fn connect_with_max(url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
}

/// Re-export `PgPool` so callers only need to import `zk_infra_rs`.
pub use sqlx::PgPool as Pool;
