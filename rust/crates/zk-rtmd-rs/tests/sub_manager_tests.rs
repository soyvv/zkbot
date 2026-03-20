use std::sync::Arc;
use tokio::sync::mpsc;
use async_trait::async_trait;
use zk_rtmd_rs::{
    sub_manager::{RtmdLease, SubInterestChange, SubInterestSource, SubscriptionManager},
    types::{ChannelType, RtmdError, RtmdEvent, RtmdSubscriptionSpec, StreamKey},
    venue_adapter::RtmdVenueAdapter,
};
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};

// ── Mock SubInterestSource ─────────────────────────────────────────────────

struct MockSubSource {
    snapshot_leases: Vec<RtmdLease>,
    change_rx: tokio::sync::Mutex<mpsc::Receiver<SubInterestChange>>,
}

impl MockSubSource {
    fn new(snapshot_leases: Vec<RtmdLease>) -> (Self, mpsc::Sender<SubInterestChange>) {
        let (tx, rx) = mpsc::channel(16);
        (Self { snapshot_leases, change_rx: tokio::sync::Mutex::new(rx) }, tx)
    }
}

#[async_trait]
impl SubInterestSource for MockSubSource {
    async fn snapshot(&self) -> Result<Vec<RtmdLease>, RtmdError> {
        Ok(self.snapshot_leases.clone())
    }

    async fn next_change(&self) -> Result<SubInterestChange, RtmdError> {
        self.change_rx.lock().await.recv().await.ok_or(RtmdError::ChannelClosed)
    }
}

// ── Mock RtmdVenueAdapter ──────────────────────────────────────────────────

struct MockAdapter {
    subscribed: tokio::sync::Mutex<Vec<StreamKey>>,
    unsubscribed: tokio::sync::Mutex<Vec<StreamKey>>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            subscribed: Default::default(),
            unsubscribed: Default::default(),
        }
    }

    async fn subscribed_keys(&self) -> Vec<StreamKey> {
        self.subscribed.lock().await.clone()
    }

    async fn unsubscribed_keys(&self) -> Vec<StreamKey> {
        self.unsubscribed.lock().await.clone()
    }
}

#[async_trait]
impl RtmdVenueAdapter for MockAdapter {
    async fn connect(&self) -> Result<(), RtmdError> { Ok(()) }
    async fn subscribe(&self, spec: RtmdSubscriptionSpec) -> Result<(), RtmdError> {
        self.subscribed.lock().await.push(spec.stream_key);
        Ok(())
    }
    async fn unsubscribe(&self, spec: RtmdSubscriptionSpec) -> Result<(), RtmdError> {
        self.unsubscribed.lock().await.push(spec.stream_key);
        Ok(())
    }
    async fn snapshot_active(&self) -> Result<Vec<RtmdSubscriptionSpec>, RtmdError> { Ok(vec![]) }
    async fn next_event(&self) -> Result<RtmdEvent, RtmdError> { Err(RtmdError::ChannelClosed) }
    async fn query_current_tick(&self, code: &str) -> Result<TickData, RtmdError> { Err(RtmdError::NotFound(code.to_string())) }
    async fn query_current_orderbook(&self, code: &str, _: Option<u32>) -> Result<OrderBook, RtmdError> { Err(RtmdError::NotFound(code.to_string())) }
    async fn query_current_funding(&self, code: &str) -> Result<FundingRate, RtmdError> { Err(RtmdError::NotFound(code.to_string())) }
    async fn query_klines(&self, code: &str, _: &str, _: u32, _: Option<i64>, _: Option<i64>) -> Result<Vec<Kline>, RtmdError> { Err(RtmdError::NotFound(code.to_string())) }
    fn instrument_exch_for(&self, _instrument_code: &str) -> Option<String> { None }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_lease(subscriber_id: &str, subscription_id: &str, instrument_code: &str) -> RtmdLease {
    RtmdLease {
        subscriber_id: subscriber_id.to_string(),
        subscription_id: subscription_id.to_string(),
        instrument_code: instrument_code.to_string(),
        channel: ChannelType::Tick,
        instrument_exch: instrument_code.to_string(),
        venue: "simulator".to_string(),
        lease_expiry_ms: 9_999_999_999_999,
    }
}

fn make_stream_key(instrument_code: &str) -> StreamKey {
    StreamKey { instrument_code: instrument_code.to_string(), channel: ChannelType::Tick }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_refcount_zero_to_one_subscribes() {
    let adapter = Arc::new(MockAdapter::new());
    let (source, _tx) = MockSubSource::new(vec![]);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    let lease = make_lease("strat_a", "sub_1", "BTC-USDT");
    mgr.apply_change(SubInterestChange::Added(lease)).await.unwrap();

    let keys = adapter.subscribed_keys().await;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], make_stream_key("BTC-USDT"));
    assert_eq!(mgr.refcount(&make_stream_key("BTC-USDT")).await, 1);
}

#[tokio::test]
async fn test_second_subscriber_does_not_duplicate_upstream_subscribe() {
    let adapter = Arc::new(MockAdapter::new());
    let (source, _tx) = MockSubSource::new(vec![]);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    let lease_a = make_lease("strat_a", "sub_1", "BTC-USDT");
    let lease_b = make_lease("strat_b", "sub_2", "BTC-USDT");
    mgr.apply_change(SubInterestChange::Added(lease_a)).await.unwrap();
    mgr.apply_change(SubInterestChange::Added(lease_b)).await.unwrap();

    // Should only subscribe once upstream
    assert_eq!(adapter.subscribed_keys().await.len(), 1);
    assert_eq!(mgr.refcount(&make_stream_key("BTC-USDT")).await, 2);
}

#[tokio::test]
async fn test_refcount_one_to_zero_unsubscribes() {
    let adapter = Arc::new(MockAdapter::new());
    let (source, _tx) = MockSubSource::new(vec![]);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    let lease = make_lease("strat_a", "sub_1", "BTC-USDT");
    mgr.apply_change(SubInterestChange::Added(lease)).await.unwrap();
    mgr.apply_change(SubInterestChange::Removed {
        subscriber_id: "strat_a".to_string(),
        subscription_id: "sub_1".to_string(),
        stream_key: make_stream_key("BTC-USDT"),
    }).await.unwrap();

    assert_eq!(adapter.unsubscribed_keys().await.len(), 1);
    assert_eq!(mgr.refcount(&make_stream_key("BTC-USDT")).await, 0);
}

#[tokio::test]
async fn test_two_subscribers_one_removal_keeps_upstream() {
    let adapter = Arc::new(MockAdapter::new());
    let (source, _tx) = MockSubSource::new(vec![]);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    let lease_a = make_lease("strat_a", "sub_1", "ETH-USDT");
    let lease_b = make_lease("strat_b", "sub_2", "ETH-USDT");
    mgr.apply_change(SubInterestChange::Added(lease_a)).await.unwrap();
    mgr.apply_change(SubInterestChange::Added(lease_b)).await.unwrap();

    // Remove one subscriber
    mgr.apply_change(SubInterestChange::Removed {
        subscriber_id: "strat_a".to_string(),
        subscription_id: "sub_1".to_string(),
        stream_key: make_stream_key("ETH-USDT"),
    }).await.unwrap();

    // Should NOT unsubscribe upstream — still one subscriber
    assert_eq!(adapter.unsubscribed_keys().await.len(), 0);
    assert_eq!(mgr.refcount(&make_stream_key("ETH-USDT")).await, 1);
}

#[tokio::test]
async fn test_reconcile_on_start_subscribes_existing_leases() {
    let adapter = Arc::new(MockAdapter::new());
    let leases = vec![
        make_lease("strat_a", "sub_1", "BTC-USDT"),
        make_lease("strat_b", "sub_2", "ETH-USDT"),
    ];
    let (source, _tx) = MockSubSource::new(leases);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    mgr.reconcile_on_start().await.unwrap();

    let keys = adapter.subscribed_keys().await;
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn test_different_channel_types_are_separate_stream_keys() {
    let adapter = Arc::new(MockAdapter::new());
    let (source, _tx) = MockSubSource::new(vec![]);
    let mgr = SubscriptionManager::new(Arc::clone(&adapter) as _, Arc::new(source) as _);

    let lease_tick = make_lease("strat_a", "sub_tick", "BTC-USDT");
    let mut lease_kline = make_lease("strat_a", "sub_kline", "BTC-USDT");
    lease_kline.channel = ChannelType::Kline { interval: "1m".to_string() };

    mgr.apply_change(SubInterestChange::Added(lease_tick)).await.unwrap();
    mgr.apply_change(SubInterestChange::Added(lease_kline)).await.unwrap();

    // Two separate upstream subscriptions (different channel types)
    assert_eq!(adapter.subscribed_keys().await.len(), 2);
}

#[tokio::test]
async fn test_simulator_adapter_event_injection_and_query() {
    use zk_rtmd_rs::venue::simulator::SimRtmdAdapter;
    use zk_rtmd_rs::types::RtmdEvent;

    let adapter = Arc::new(SimRtmdAdapter::new());
    let sender = adapter.event_sender();

    let tick = TickData {
        instrument_code: "BTC-USDT".to_string(),
        exchange: "simulator".to_string(),
        ..Default::default()
    };

    sender.send(RtmdEvent::Tick(tick.clone())).await.unwrap();
    let event = adapter.next_event().await.unwrap();
    assert!(matches!(event, RtmdEvent::Tick(_)));

    // After consuming the event, cache should be populated
    let queried = adapter.query_current_tick("BTC-USDT").await.unwrap();
    assert_eq!(queried.instrument_code, "BTC-USDT");
}

/// Verify that the exch_map is refcounted per instrument_code, so that unsubscribing
/// one channel does not remove the instrument_exch mapping for other active channels.
#[tokio::test]
async fn test_simulator_exch_map_refcounted_across_channels() {
    use zk_rtmd_rs::venue::simulator::SimRtmdAdapter;

    let adapter = SimRtmdAdapter::new();

    let tick_spec = RtmdSubscriptionSpec {
        stream_key: StreamKey { instrument_code: "BTC-USDT".to_string(), channel: ChannelType::Tick },
        instrument_exch: "BTC-USDT-SWAP".to_string(),
        venue: "OKX".to_string(),
    };
    let funding_spec = RtmdSubscriptionSpec {
        stream_key: StreamKey { instrument_code: "BTC-USDT".to_string(), channel: ChannelType::Funding },
        instrument_exch: "BTC-USDT-SWAP".to_string(),
        venue: "OKX".to_string(),
    };

    adapter.subscribe(tick_spec.clone()).await.unwrap();
    adapter.subscribe(funding_spec.clone()).await.unwrap();

    // Both channels active — exch resolved
    assert_eq!(
        adapter.instrument_exch_for("BTC-USDT"),
        Some("BTC-USDT-SWAP".to_string())
    );

    // Unsubscribe tick — funding still active
    adapter.unsubscribe(tick_spec).await.unwrap();
    assert_eq!(
        adapter.instrument_exch_for("BTC-USDT"),
        Some("BTC-USDT-SWAP".to_string()),
        "exch_map should remain while funding channel is still subscribed"
    );

    // Unsubscribe funding — no channels left
    adapter.unsubscribe(funding_spec).await.unwrap();
    assert_eq!(
        adapter.instrument_exch_for("BTC-USDT"),
        None,
        "exch_map should be empty once all channels unsubscribed"
    );
}
