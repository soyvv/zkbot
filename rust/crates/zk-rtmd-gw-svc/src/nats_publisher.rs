use prost::Message as _;
use tracing::warn;
use zk_infra_rs::nats::subject;
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};
use zk_rtmd_rs::types::RtmdEvent;

pub struct RtmdNatsPublisher {
    client: async_nats::Client,
    venue: String,
}

impl RtmdNatsPublisher {
    pub fn new(client: async_nats::Client, venue: String) -> Self {
        Self { client, venue }
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
        let subj = subject::rtmd_tick(&self.venue, &tick.instrument_code);
        self.publish_bytes(&subj, tick.encode_to_vec()).await;
    }

    async fn publish_kline(&self, kline: Kline) {
        let interval = kline_interval_str(kline.kline_type);
        let subj = subject::rtmd_kline(&self.venue, &kline.symbol, interval);
        self.publish_bytes(&subj, kline.encode_to_vec()).await;
    }

    async fn publish_orderbook(&self, ob: OrderBook) {
        let subj = subject::rtmd_orderbook(&self.venue, &ob.instrument_code);
        self.publish_bytes(&subj, ob.encode_to_vec()).await;
    }

    async fn publish_funding(&self, fr: FundingRate) {
        let subj = subject::rtmd_funding(&self.venue, &fr.instrument_code);
        self.publish_bytes(&subj, fr.encode_to_vec()).await;
    }

    async fn publish_bytes(&self, subject: &str, payload: Vec<u8>) {
        if let Err(e) = self.client.publish(subject.to_string(), payload.into()).await {
            warn!(subject, error = %e, "NATS publish failed");
        }
    }
}

/// Map kline_type i32 to an interval string.
/// Values: 0=1m, 1=5m, 2=15m, 3=30m, 4=1h, 5=4h, 6=1d
fn kline_interval_str(kline_type: i32) -> &'static str {
    match kline_type {
        0 => "1m",
        1 => "5m",
        2 => "15m",
        3 => "30m",
        4 => "1h",
        5 => "4h",
        6 => "1d",
        _ => "1m",
    }
}
