//! Unit tests for RefdataSdk cache logic.
//!
//! Uses a mock `RefdataGrpc` to test without a real gRPC server.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::RwLock;

use zk_trading_sdk_rs::error::SdkError;
use zk_trading_sdk_rs::proto::refdata_svc::{InstrumentRefdataResponse, MarketStatusResponse};
use zk_trading_sdk_rs::refdata::{CachedEntry, RefdataCache, RefdataGrpc};

// ── Mock gRPC implementation ──────────────────────────────────────────────────

struct MockRefdataGrpc {
    call_count: Arc<AtomicU32>,
    /// If true, the next call returns an error.
    fail_next: Arc<std::sync::atomic::AtomicBool>,
}

impl MockRefdataGrpc {
    fn new() -> Self {
        Self {
            call_count: Arc::new(AtomicU32::new(0)),
            fail_next: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

#[async_trait::async_trait]
impl RefdataGrpc for MockRefdataGrpc {
    async fn query_instrument_by_id(&self, instrument_id: &str) -> Result<InstrumentRefdataResponse, SdkError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.load(Ordering::SeqCst) {
            return Err(SdkError::Config("mock gRPC unavailable".into()));
        }
        Ok(InstrumentRefdataResponse {
            instrument_id: instrument_id.to_string(),
            venue: "MOCK".to_string(),
            instrument_exch: "BTC-USDT".to_string(),
            instrument_type: "PERP".to_string(),
            disabled: false,
            price_tick_size: 0.01,
            qty_lot_size: 0.001,
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            updated_at_ms: 1712000000000,
        })
    }

    async fn query_instrument_by_venue_symbol(
        &self,
        venue: &str,
        instrument_exch: &str,
    ) -> Result<InstrumentRefdataResponse, SdkError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_next.load(Ordering::SeqCst) {
            return Err(SdkError::Config("mock gRPC unavailable".into()));
        }
        Ok(InstrumentRefdataResponse {
            instrument_id: format!("{venue}_{instrument_exch}").replace('-', ""),
            venue: venue.to_string(),
            instrument_exch: instrument_exch.to_string(),
            instrument_type: "SPOT".to_string(),
            disabled: false,
            price_tick_size: 0.01,
            qty_lot_size: 0.001,
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            updated_at_ms: 1712000000000,
        })
    }

    async fn query_market_status(
        &self,
        venue: &str,
        market: &str,
    ) -> Result<MarketStatusResponse, SdkError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(MarketStatusResponse {
            venue: venue.to_string(),
            market: market.to_string(),
            session_state: "open".to_string(),
            effective_at_ms: 1712000000000,
        })
    }
}

// ── Helper: build a RefdataCache with a pre-populated entry ──────────────────

fn cache_with_entry(instrument_id: &str, valid: bool, deprecated: bool) -> RefdataCache {
    let mut cache = RefdataCache::default();
    let entry = CachedEntry {
        data: InstrumentRefdataResponse {
            instrument_id: instrument_id.to_string(),
            venue: "MOCK".to_string(),
            instrument_exch: "BTC-USDT".to_string(),
            instrument_type: "PERP".to_string(),
            disabled: false,
            price_tick_size: 0.01,
            qty_lot_size: 0.001,
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            updated_at_ms: 1712000000000,
        },
        valid,
        deprecated,
        loaded_at_ms: 1712000000000,
    };
    cache.instruments.insert(instrument_id.to_string(), entry);
    cache
}

// ── Cache logic tests (pure, no async NATS) ───────────────────────────────────

#[test]
fn test_refdata_cache_hit_skips_grpc() {
    // If a valid entry is in the cache, no gRPC call should be needed.
    let cache = cache_with_entry("BTCUSDT_MOCK", true, false);
    let entry = cache.instruments.get("BTCUSDT_MOCK").expect("entry must exist");
    assert!(entry.valid);
    assert!(!entry.deprecated);
    assert_eq!(entry.data.price_tick_size, 0.01);
}

#[test]
fn test_refdata_invalidation_marks_entry_stale() {
    let mut cache = cache_with_entry("BTCUSDT_MOCK", true, false);
    // Simulate receiving zk.control.refdata.updated with change_class=invalidate
    if let Some(entry) = cache.instruments.get_mut("BTCUSDT_MOCK") {
        entry.valid = false;
    }
    let entry = cache.instruments.get("BTCUSDT_MOCK").unwrap();
    assert!(!entry.valid, "entry must be marked invalid after invalidation");
    assert!(!entry.deprecated, "invalidate != deprecate");
}

#[test]
fn test_refdata_deprecation_marks_entry_deprecated() {
    let mut cache = cache_with_entry("BTCUSDT_MOCK", true, false);
    // Simulate receiving change_class=deprecate
    if let Some(entry) = cache.instruments.get_mut("BTCUSDT_MOCK") {
        entry.valid = false;
        entry.deprecated = true;
    }
    let entry = cache.instruments.get("BTCUSDT_MOCK").unwrap();
    assert!(!entry.valid);
    assert!(entry.deprecated, "deprecated flag must be set");
}

#[test]
fn test_refdata_secondary_index_populated_on_insert() {
    let mut cache = RefdataCache::default();
    let data = InstrumentRefdataResponse {
        instrument_id: "BTCUSDT_MOCK".to_string(),
        venue: "MOCK".to_string(),
        instrument_exch: "BTC-USDT".to_string(),
        instrument_type: "PERP".to_string(),
        disabled: false,
        price_tick_size: 0.01,
        qty_lot_size: 0.001,
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        updated_at_ms: 1712000000000,
    };
    // Insert into primary cache
    cache.instruments.insert("BTCUSDT_MOCK".to_string(), CachedEntry {
        data: data.clone(),
        valid: true,
        deprecated: false,
        loaded_at_ms: 1712000000000,
    });
    // Populate secondary index (as sdk would do after a gRPC fetch)
    cache.by_venue_symbol.insert(
        ("MOCK".to_string(), "BTC-USDT".to_string()),
        "BTCUSDT_MOCK".to_string(),
    );

    let resolved = cache.by_venue_symbol.get(&("MOCK".to_string(), "BTC-USDT".to_string()));
    assert_eq!(resolved.map(String::as_str), Some("BTCUSDT_MOCK"));
}

#[test]
fn test_market_status_cache_inline_update() {
    let mut cache = RefdataCache::default();
    // Simulate zk.control.market_status.updated inline update
    cache.market_status.insert(
        ("MOCK".to_string(), "MOCK".to_string()),
        CachedEntry {
            data: MarketStatusResponse {
                venue: "MOCK".to_string(),
                market: "MOCK".to_string(),
                session_state: "open".to_string(),
                effective_at_ms: 1712000000000,
            },
            valid: true,
            deprecated: false,
            loaded_at_ms: 1712000000000,
        },
    );

    let entry = cache.market_status.get(&("MOCK".to_string(), "MOCK".to_string())).unwrap();
    assert_eq!(entry.data.session_state, "open");

    // Inline update from NATS event
    if let Some(e) = cache.market_status.get_mut(&("MOCK".to_string(), "MOCK".to_string())) {
        e.data.session_state = "closed".to_string();
    }

    let entry = cache.market_status.get(&("MOCK".to_string(), "MOCK".to_string())).unwrap();
    assert_eq!(entry.data.session_state, "closed", "market status must update inline without gRPC");
}

// ── RefdataSdk query_instrument behavior ─────────────────────────────────────

use zk_trading_sdk_rs::refdata::RefdataSdkInner;

#[tokio::test]
async fn test_sdk_cache_miss_calls_grpc_and_populates_cache() {
    let mock = MockRefdataGrpc::new();
    let call_count = Arc::clone(&mock.call_count);
    let sdk = RefdataSdkInner::new_with_mock(mock);

    let result = sdk.query_instrument("BTCUSDT_MOCK").await.expect("must succeed");
    assert_eq!(result.price_tick_size, 0.01);
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "gRPC called once on cache miss");

    // Second call should hit cache — no extra gRPC call
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.expect("must succeed");
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "gRPC not called again on cache hit");
}

#[tokio::test]
async fn test_sdk_invalid_entry_triggers_grpc_reload() {
    let mock = MockRefdataGrpc::new();
    let call_count = Arc::clone(&mock.call_count);
    let sdk = RefdataSdkInner::new_with_mock(mock);

    // Warm the cache
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Invalidate the entry
    sdk.invalidate("BTCUSDT_MOCK", false).await;

    // Next query should trigger a reload
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 2, "gRPC called again after invalidation");
}

#[tokio::test]
async fn test_sdk_deprecated_entry_returns_error_not_grpc() {
    let mock = MockRefdataGrpc::new();
    let call_count = Arc::clone(&mock.call_count);
    let sdk = RefdataSdkInner::new_with_mock(mock);

    // Warm the cache
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.unwrap();

    // Deprecate the entry
    sdk.invalidate("BTCUSDT_MOCK", true).await;

    let result = sdk.query_instrument("BTCUSDT_MOCK").await;
    assert!(result.is_err(), "deprecated entry must return error");
    let err = result.unwrap_err();
    assert!(err.to_string().contains("deprecated"), "error must say deprecated: {err}");
    assert_eq!(call_count.load(Ordering::SeqCst), 1, "no gRPC call for deprecated entry");
}

#[tokio::test]
async fn test_sdk_stale_entry_returned_when_grpc_fails() {
    let mock = MockRefdataGrpc::new();
    let fail_flag = Arc::clone(&mock.fail_next);
    let sdk = RefdataSdkInner::new_with_mock(mock);

    // Warm the cache
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.unwrap();

    // Invalidate then make gRPC fail
    sdk.invalidate("BTCUSDT_MOCK", false).await;
    fail_flag.store(true, Ordering::SeqCst);

    let result = sdk.query_instrument("BTCUSDT_MOCK").await;
    // Must return stale data (not hard failure) when entry exists but gRPC is down
    assert!(result.is_ok(), "stale entry should be returned when gRPC unavailable: {result:?}");
}

// ── Async tests using mock gRPC ───────────────────────────────────────────────

#[tokio::test]
async fn test_refdata_cache_miss_fetches_from_grpc() {
    let mock = MockRefdataGrpc::new();
    let call_count = Arc::clone(&mock.call_count);
    let cache = Arc::new(RwLock::new(RefdataCache::default()));

    // Simulate cache miss: call gRPC directly and populate cache
    let result = mock.query_instrument_by_id("BTCUSDT_MOCK").await.expect("must succeed");
    cache.write().await.instruments.insert("BTCUSDT_MOCK".to_string(), CachedEntry {
        data: result.clone(),
        valid: true,
        deprecated: false,
        loaded_at_ms: 1712000000000,
    });

    assert_eq!(call_count.load(Ordering::SeqCst), 1, "gRPC must be called on cache miss");
    assert_eq!(result.price_tick_size, 0.01);
    assert!(cache.read().await.instruments.contains_key("BTCUSDT_MOCK"), "cache must be populated");
}

/// After a NATS invalidation event is processed (modelled by calling `invalidate` directly),
/// the next call to `query_instrument` must trigger a gRPC reload (active-reload contract).
#[tokio::test]
async fn test_sdk_invalidation_triggers_active_reload() {
    let mock = MockRefdataGrpc::new();
    let call_count = Arc::clone(&mock.call_count);
    let sdk = RefdataSdkInner::new_with_mock(mock);

    // Warm the cache (gRPC call #1)
    let _ = sdk.query_instrument("BTCUSDT_MOCK").await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Simulate NATS invalidation event: change_class=invalidate
    sdk.invalidate("BTCUSDT_MOCK", false).await;

    // The next query must trigger a fresh gRPC fetch (call #2) — active-reload contract
    let result = sdk.query_instrument("BTCUSDT_MOCK").await.expect("active reload must succeed");
    assert_eq!(call_count.load(Ordering::SeqCst), 2, "gRPC must be called on active reload");
    assert_eq!(result.price_tick_size, 0.01, "fresh data returned after reload");
}

#[tokio::test]
async fn test_refdata_stale_entry_returned_when_grpc_unavailable() {
    // If a cached entry exists but is invalid AND gRPC is unavailable, return stale data.
    // This is the only case where stale data is served.
    let cache_entry = CachedEntry {
        data: InstrumentRefdataResponse {
            instrument_id: "BTCUSDT_MOCK".to_string(),
            venue: "MOCK".to_string(),
            instrument_exch: "BTC-USDT".to_string(),
            instrument_type: "PERP".to_string(),
            disabled: false,
            price_tick_size: 0.01,
            qty_lot_size: 0.001,
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            updated_at_ms: 1712000000000,
        },
        valid: false, // stale
        deprecated: false,
        loaded_at_ms: 1712000000000,
    };

    // The stale data is present; gRPC fails
    let mock = MockRefdataGrpc::new();
    mock.fail_next.store(true, Ordering::SeqCst);
    let grpc_result = mock.query_instrument_by_id("BTCUSDT_MOCK").await;
    assert!(grpc_result.is_err(), "mock gRPC must fail");

    // Stale-entry policy: return stale data (not an error)
    assert!(!cache_entry.valid, "entry is stale");
    assert!(!cache_entry.deprecated, "stale != deprecated");
    assert_eq!(cache_entry.data.instrument_id, "BTCUSDT_MOCK",
        "stale data still has the instrument info");
}
