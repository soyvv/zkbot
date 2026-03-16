use std::collections::HashMap;

/// Gateway service configuration loaded from environment variables.
///
/// Shared fields apply to all venues. Simulator-specific fields are kept at
/// top-level for ergonomics since the simulator is the default venue.
/// Real venue adapters should parse their config from `venue_config`.
pub struct GwSvcConfig {
    // ── Shared ────────────────────────────────────────────────────────────
    pub gw_id: String,
    pub account_id: i64,
    pub venue: String,
    pub grpc_port: u16,
    pub nats_url: Option<String>,
    /// Opaque venue-specific config blob (parsed from ZK_VENUE_CONFIG JSON).
    /// Real venue adapters (okx, ibkr, oanda) parse their typed config from this.
    pub venue_config: serde_json::Value,

    // ── Simulator-specific ────────────────────────────────────────────────
    /// Initial balances ("BTC:10,USDT:100000").
    pub mock_balances: String,
    /// Fill delay in ms (0 = immediate match policy decides).
    pub fill_delay_ms: u64,
    /// Match policy name ("immediate" or "fcfs").
    pub match_policy: String,
    /// Admin gRPC port for simulator controls (separate from trading port).
    pub admin_grpc_port: u16,
    /// Whether admin controls are enabled.
    pub enable_admin_controls: bool,
}

impl GwSvcConfig {
    pub fn from_env() -> Self {
        let venue_config_str =
            std::env::var("ZK_VENUE_CONFIG").unwrap_or_else(|_| "{}".to_string());
        let venue_config: serde_json::Value =
            serde_json::from_str(&venue_config_str).unwrap_or(serde_json::Value::Object(
                serde_json::Map::new(),
            ));

        Self {
            gw_id: std::env::var("ZK_GW_ID").unwrap_or_else(|_| "gw_sim_1".to_string()),
            account_id: std::env::var("ZK_ACCOUNT_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(9001),
            venue: std::env::var("ZK_VENUE").unwrap_or_else(|_| "simulator".to_string()),
            grpc_port: std::env::var("ZK_GRPC_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(51051),
            nats_url: std::env::var("ZK_NATS_URL").ok(),
            venue_config,
            mock_balances: std::env::var("ZK_MOCK_BALANCES")
                .unwrap_or_else(|_| "BTC:10,USDT:100000,ETH:50".to_string()),
            fill_delay_ms: std::env::var("ZK_FILL_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            match_policy: std::env::var("ZK_MATCH_POLICY")
                .unwrap_or_else(|_| "immediate".to_string()),
            admin_grpc_port: std::env::var("ZK_ADMIN_GRPC_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(51052),
            enable_admin_controls: std::env::var("ZK_ENABLE_ADMIN_CONTROLS")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        }
    }

    /// Parse "BTC:10.5,USDT:100000" into a balance map.
    pub fn parse_balances(s: &str) -> HashMap<String, f64> {
        let mut map = HashMap::new();
        for entry in s.split(',') {
            let mut parts = entry.splitn(2, ':');
            let symbol = parts.next().unwrap_or("").trim().to_uppercase();
            let qty: f64 = parts.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0.0);
            if !symbol.is_empty() {
                map.insert(symbol, qty);
            }
        }
        map
    }
}
