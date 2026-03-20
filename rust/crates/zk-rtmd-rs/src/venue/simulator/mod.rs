use std::collections::HashMap;
use tokio::sync::{mpsc, Mutex};
use async_trait::async_trait;
use tracing::debug;
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};

use crate::types::{RtmdError, RtmdEvent, RtmdSubscriptionSpec};
use crate::venue_adapter::RtmdVenueAdapter;

type Result<T> = std::result::Result<T, RtmdError>;

/// In-memory cache of the last seen value for each instrument per channel type.
#[derive(Default)]
struct SimCache {
    ticks: HashMap<String, TickData>,
    orderbooks: HashMap<String, OrderBook>,
    funding: HashMap<String, FundingRate>,
    klines: HashMap<String, Vec<Kline>>,
}

pub struct SimRtmdAdapter {
    /// Test code sends events here.
    sender: mpsc::Sender<RtmdEvent>,
    /// Adapter reads events here.
    receiver: Mutex<mpsc::Receiver<RtmdEvent>>,
    /// Cache of last seen values per instrument.
    cache: Mutex<SimCache>,
    /// Active subscriptions (instrument_code → spec).
    active: Mutex<Vec<RtmdSubscriptionSpec>>,
}

impl SimRtmdAdapter {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel(256);
        Self {
            sender,
            receiver: Mutex::new(receiver),
            cache: Mutex::new(SimCache::default()),
            active: Mutex::new(Vec::new()),
        }
    }

    /// Returns a sender that test code can use to inject events.
    pub fn event_sender(&self) -> mpsc::Sender<RtmdEvent> {
        self.sender.clone()
    }
}

impl Default for SimRtmdAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RtmdVenueAdapter for SimRtmdAdapter {
    async fn connect(&self) -> Result<()> {
        debug!("SimRtmdAdapter: connected");
        Ok(())
    }

    async fn subscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()> {
        debug!(instrument_code = %spec.stream_key.instrument_code, "SimRtmdAdapter: subscribe");
        self.active.lock().await.push(spec);
        Ok(())
    }

    async fn unsubscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()> {
        debug!(instrument_code = %spec.stream_key.instrument_code, "SimRtmdAdapter: unsubscribe");
        self.active.lock().await.retain(|s| s.stream_key != spec.stream_key);
        Ok(())
    }

    async fn snapshot_active(&self) -> Result<Vec<RtmdSubscriptionSpec>> {
        Ok(self.active.lock().await.clone())
    }

    async fn next_event(&self) -> Result<RtmdEvent> {
        let event = self.receiver.lock().await.recv().await
            .ok_or(RtmdError::ChannelClosed)?;
        // Update cache.
        let mut cache = self.cache.lock().await;
        match &event {
            RtmdEvent::Tick(t) => { cache.ticks.insert(t.instrument_code.clone(), t.clone()); }
            RtmdEvent::OrderBook(ob) => { cache.orderbooks.insert(ob.instrument_code.clone(), ob.clone()); }
            RtmdEvent::Funding(f) => { cache.funding.insert(f.instrument_code.clone(), f.clone()); }
            RtmdEvent::Kline(k) => {
                cache.klines.entry(k.symbol.clone()).or_default().push(k.clone());
            }
        }
        Ok(event)
    }

    async fn query_current_tick(&self, instrument_code: &str) -> Result<TickData> {
        self.cache.lock().await.ticks.get(instrument_code).cloned()
            .ok_or_else(|| RtmdError::NotFound(instrument_code.to_string()))
    }

    async fn query_current_orderbook(&self, instrument_code: &str, _depth: Option<u32>) -> Result<OrderBook> {
        self.cache.lock().await.orderbooks.get(instrument_code).cloned()
            .ok_or_else(|| RtmdError::NotFound(instrument_code.to_string()))
    }

    async fn query_current_funding(&self, instrument_code: &str) -> Result<FundingRate> {
        self.cache.lock().await.funding.get(instrument_code).cloned()
            .ok_or_else(|| RtmdError::NotFound(instrument_code.to_string()))
    }

    async fn query_klines(&self, instrument_code: &str, _interval: &str, limit: u32, _from_ms: Option<i64>, _to_ms: Option<i64>) -> Result<Vec<Kline>> {
        let cache = self.cache.lock().await;
        let all = cache.klines.get(instrument_code).cloned().unwrap_or_default();
        let limit = limit as usize;
        Ok(if all.len() > limit { all[all.len() - limit..].to_vec() } else { all })
    }
}
