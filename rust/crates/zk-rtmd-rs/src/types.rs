use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};

/// Which market data channel a subscription targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChannelType {
    /// Best-bid/offer and last-trade tick stream.
    Tick,
    /// Candlestick (OHLCV) stream for the given interval string (e.g. `"1m"`).
    Kline { interval: String },
    /// Order book snapshot/diff stream, optionally limited to `depth` levels per side.
    OrderBook { depth: Option<u32> },
    /// Perpetual funding rate stream.
    Funding,
}

/// Deduplication key for upstream venue subscriptions.
/// Multiple downstream leases with the same StreamKey collapse to one upstream subscription.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamKey {
    /// Canonical internal instrument identifier (e.g. `"BTC-USDT-PERP"`).
    pub instrument_code: String,
    /// The market data channel being subscribed.
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
    /// Tick update (best bid/ask and last trade).
    Tick(TickData),
    /// Candlestick bar.
    Kline(Kline),
    /// Order book snapshot or incremental update.
    OrderBook(OrderBook),
    /// Perpetual funding rate.
    Funding(FundingRate),
}

#[derive(Debug, thiserror::Error)]
pub enum RtmdError {
    /// Error reported by the venue connection or feed.
    #[error("venue error: {0}")]
    Venue(String),
    /// Error managing a subscription (e.g. rejected by venue).
    #[error("subscription error: {0}")]
    Subscription(String),
    /// Requested instrument or data was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// The underlying channel (mpsc/watcher) was closed; the caller should stop.
    #[error("channel closed")]
    ChannelClosed,
    /// Unexpected internal error.
    #[error("internal: {0}")]
    Internal(String),
}
