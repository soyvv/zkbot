use std::collections::HashMap;

/// In-flight order tracked by the mock gateway.
#[derive(Clone, Debug)]
pub struct MockOrder {
    pub exch_order_ref: String,
    /// Matches ExchSendOrderRequest::correlation_id (= OMS order_id).
    pub order_id: i64,
    pub account_id: i64,
    pub instrument: String,
    pub side: i32,
    pub qty: f64,
    pub price: f64,
    pub filled_qty: f64,
}

pub struct MockGwState {
    pub gw_id: String,
    pub account_id: i64,
    /// Delay before simulating a fill. 0 = immediate (next scheduler tick).
    pub fill_delay_ms: u64,
    /// Active orders keyed by exch_order_ref.
    pub orders: HashMap<String, MockOrder>,
    /// Spot-style balances: symbol → available quantity.
    /// Seeded from ZK_MOCK_BALANCES env var ("BTC:10,USDT:100000").
    pub balances: HashMap<String, f64>,
    /// NATS client for publishing OrderReport events. None when NATS_URL is unset.
    pub nats_client: Option<async_nats::Client>,
    /// Per-order fill task handles, used for cancellation.
    pub fill_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
}

impl MockGwState {
    pub fn new(
        gw_id: String,
        account_id: i64,
        fill_delay_ms: u64,
        balances: HashMap<String, f64>,
        nats_client: Option<async_nats::Client>,
    ) -> Self {
        Self {
            gw_id,
            account_id,
            fill_delay_ms,
            orders: HashMap::new(),
            balances,
            nats_client,
            fill_tasks: HashMap::new(),
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
