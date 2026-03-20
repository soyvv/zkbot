use async_trait::async_trait;
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};

use crate::types::{RtmdError, RtmdEvent, RtmdSubscriptionSpec};

pub type Result<T> = std::result::Result<T, RtmdError>;

#[async_trait]
pub trait RtmdVenueAdapter: Send + Sync {
    /// Connect to the venue (WebSocket handshake, auth, etc.).
    async fn connect(&self) -> Result<()>;

    /// Subscribe to a single upstream stream.
    async fn subscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()>;

    /// Unsubscribe from a single upstream stream.
    async fn unsubscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()>;

    /// Return the set of currently-active upstream subscriptions.
    async fn snapshot_active(&self) -> Result<Vec<RtmdSubscriptionSpec>>;

    /// Block until the next market data event is available.
    async fn next_event(&self) -> Result<RtmdEvent>;

    /// Map an internal `instrument_code` to the venue-native `instrument_exch` used in
    /// NATS subject names. Returns `None` if the mapping is unknown (caller should fall
    /// back to using `instrument_code` directly).
    fn instrument_exch_for(&self, instrument_code: &str) -> Option<String>;

    // ── Query API (venue-backed, not gateway-local cache) ──────────────────

    async fn query_current_tick(&self, instrument_code: &str) -> Result<TickData>;
    async fn query_current_orderbook(
        &self,
        instrument_code: &str,
        depth: Option<u32>,
    ) -> Result<OrderBook>;
    async fn query_current_funding(&self, instrument_code: &str) -> Result<FundingRate>;
    async fn query_klines(
        &self,
        instrument_code: &str,
        interval: &str,
        limit: u32,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<Vec<Kline>>;
}
