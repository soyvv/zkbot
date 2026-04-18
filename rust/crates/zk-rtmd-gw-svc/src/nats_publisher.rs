use std::sync::Arc;

use prost::Message as _;
use tracing::{debug, warn};
use zk_infra_rs::nats::subject;
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};
use zk_rtmd_rs::{types::RtmdEvent, venue_adapter::RtmdVenueAdapter};

pub struct RtmdNatsPublisher {
    client: async_nats::Client,
    venue: String,
    /// Used to resolve internal instrument_code → venue-native instrument_exch for subjects.
    adapter: Arc<dyn RtmdVenueAdapter>,
}

impl RtmdNatsPublisher {
    pub fn new(
        client: async_nats::Client,
        venue: String,
        adapter: Arc<dyn RtmdVenueAdapter>,
    ) -> Self {
        Self { client, venue, adapter }
    }

    pub async fn publish(&self, event: RtmdEvent) {
        match event {
            RtmdEvent::Tick(t) => self.publish_tick(t).await,
            RtmdEvent::Kline(k) => self.publish_kline(k).await,
            RtmdEvent::OrderBook(ob) => self.publish_orderbook(ob).await,
            RtmdEvent::Funding(f) => self.publish_funding(f).await,
        }
    }

    async fn publish_tick(&self, tick: TickData) {
        let instrument_exch = self
            .adapter
            .instrument_exch_for(&tick.instrument_code)
            .unwrap_or_else(|| tick.instrument_code.clone());
        let subj = subject::rtmd_tick(&self.venue, &instrument_exch);
        self.publish_bytes(&subj, tick.encode_to_vec()).await;
    }

    async fn publish_kline(&self, kline: Kline) {
        let interval = kline_interval_str(kline.kline_type);
        let instrument_exch = self
            .adapter
            .instrument_exch_for(&kline.symbol)
            .unwrap_or_else(|| kline.symbol.clone());
        let subj = subject::rtmd_kline(&self.venue, &instrument_exch, interval);
        debug!(
            subject = %subj,
            symbol = %kline.symbol,
            timestamp = kline.timestamp,
            "publishing kline to NATS"
        );
        self.publish_bytes(&subj, kline.encode_to_vec()).await;
    }

    async fn publish_orderbook(&self, ob: OrderBook) {
        let instrument_exch = self
            .adapter
            .instrument_exch_for(&ob.instrument_code)
            .unwrap_or_else(|| ob.instrument_code.clone());
        let subj = subject::rtmd_orderbook(&self.venue, &instrument_exch);
        self.publish_bytes(&subj, ob.encode_to_vec()).await;
    }

    async fn publish_funding(&self, fr: FundingRate) {
        let instrument_exch = self
            .adapter
            .instrument_exch_for(&fr.instrument_code)
            .unwrap_or_else(|| fr.instrument_code.clone());
        let subj = subject::rtmd_funding(&self.venue, &instrument_exch);
        self.publish_bytes(&subj, fr.encode_to_vec()).await;
    }

    async fn publish_bytes(&self, subject: &str, payload: Vec<u8>) {
        if let Err(e) = self.client.publish(subject.to_string(), payload.into()).await {
            warn!(subject, error = %e, "NATS publish failed");
        }
    }
}

/// Map kline_type i32 to an interval string using the proto enum.
fn kline_interval_str(kline_type: i32) -> &'static str {
    use zk_proto_rs::zk::rtmd::v1::kline::KlineType;
    match KlineType::try_from(kline_type).unwrap_or(KlineType::Kline1min) {
        KlineType::Kline1min  => "1m",
        KlineType::Kline5min  => "5m",
        KlineType::Kline15min => "15m",
        KlineType::Kline30min => "30m",
        KlineType::Kline1hour => "1h",
        KlineType::Kline4hour => "4h",
        KlineType::Kline1day  => "1d",
    }
}
