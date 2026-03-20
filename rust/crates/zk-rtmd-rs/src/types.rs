use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};

/// Which market data channel a subscription targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChannelType {
    Tick,
    Kline { interval: String },
    OrderBook { depth: Option<u32> },
    Funding,
}

/// Deduplication key for upstream venue subscriptions.
/// Multiple downstream leases with the same StreamKey collapse to one upstream subscription.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamKey {
    pub instrument_code: String,
    pub channel: ChannelType,
}

/// A resolved subscription spec ready to send to the venue adapter.
#[derive(Debug, Clone)]
pub struct RtmdSubscriptionSpec {
    pub stream_key: StreamKey,
    /// Venue-native instrument identifier (resolved from refdata).
    pub instrument_exch: String,
    /// Venue name (e.g. "OKX", "simulator").
    pub venue: String,
}

/// A normalised market data event emitted by a venue adapter.
#[derive(Debug, Clone)]
pub enum RtmdEvent {
    Tick(TickData),
    Kline(Kline),
    OrderBook(OrderBook),
    Funding(FundingRate),
}

#[derive(Debug, thiserror::Error)]
pub enum RtmdError {
    #[error("venue error: {0}")]
    Venue(String),
    #[error("subscription error: {0}")]
    Subscription(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("channel closed")]
    ChannelClosed,
    #[error("internal: {0}")]
    Internal(String),
}
