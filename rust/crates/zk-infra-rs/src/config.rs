/// Load typed configuration from environment variables using `envy`.
///
/// Each service binary defines its own `Config` struct that embeds
/// `InfraConfig` (or a subset thereof) plus service-specific fields,
/// then calls `from_env::<Config>()` at startup.
///
/// # Example
///
/// ```no_run
/// #[derive(serde::Deserialize)]
/// struct MyServiceConfig {
///     #[serde(flatten)]
///     infra: zk_infra_rs::config::InfraConfig,
///     my_field: String,
/// }
/// fn main() -> Result<(), zk_infra_rs::config::Error> {
///     let _cfg = zk_infra_rs::config::from_env::<MyServiceConfig>()?;
///     Ok(())
/// }
/// ```
pub use envy::Error;

/// Common infrastructure connection URLs shared by all service binaries.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct InfraConfig {
    #[serde(default = "default_nats_url")]
    pub zk_nats_url: String,

    #[serde(default = "default_pg_url")]
    pub zk_pg_url: String,

    #[serde(default = "default_redis_url")]
    pub zk_redis_url: String,

    #[serde(default = "default_mongo_url")]
    pub zk_mongo_url: String,

    #[serde(default = "default_env")]
    pub zk_env: String,
}

fn default_nats_url()  -> String { "nats://localhost:4222".to_string() }
fn default_pg_url()    -> String { "postgresql://zk:zk@localhost:5432/zkbot".to_string() }
fn default_redis_url() -> String { "redis://localhost:6379".to_string() }
fn default_mongo_url() -> String { "mongodb://localhost:27017".to_string() }
fn default_env()       -> String { "dev".to_string() }

/// Deserialise environment variables into `T` via `envy`.
///
/// `envy` maps `SCREAMING_SNAKE_CASE` env vars to `snake_case` struct fields,
/// so `ZK_NATS_URL` → `zk_nats_url`.
pub fn from_env<T: serde::de::DeserializeOwned>() -> Result<T, envy::Error> {
    envy::from_env::<T>()
}
