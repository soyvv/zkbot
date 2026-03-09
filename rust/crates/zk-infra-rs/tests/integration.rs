/// Integration tests — require `make dev-up` (docker-compose stack).
/// Run with: `cargo test -p zk-infra-rs -- --ignored`

use zk_infra_rs::{nats, pg, redis};

const NATS_URL: &str = "nats://localhost:4222";
const PG_URL: &str = "postgresql://zk:zk@localhost:5432/zkbot";
const REDIS_URL: &str = "redis://localhost:6379";

#[tokio::test]
#[ignore]
async fn test_nats_connect() {
    let client = nats::connect(NATS_URL).await.expect("NATS connect failed");
    // Verify JetStream is enabled by creating a context — panics if JS is disabled.
    let _js = async_nats::jetstream::new(client);
}

#[tokio::test]
#[ignore]
async fn test_pg_connect() {
    let pool = pg::connect(PG_URL).await.expect("PG connect failed");

    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("SELECT 1 failed");
    assert_eq!(row.0, 1);

    // Verify our schema was applied.
    let count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_schema = 'cfg' AND table_name = 'oms_instance'",
    )
    .fetch_one(&pool)
    .await
    .expect("schema check failed");
    assert_eq!(count.0, 1, "cfg.oms_instance table not found");
}

#[tokio::test]
#[ignore]
async fn test_redis_connect() {
    use ::redis::AsyncCommands;

    let mut conn = redis::connect(REDIS_URL)
        .await
        .expect("Redis connect failed");

    let key = redis::key::order("oms_dev_1", 999999);
    let value = r#"{"order_id":999999,"status":"OPEN"}"#;

    let _: () = conn.set(&key, value).await.expect("SET failed");
    let got: String = conn.get(&key).await.expect("GET failed");
    assert_eq!(got, value);
    let _: () = conn.del(&key).await.expect("DEL failed");
}
