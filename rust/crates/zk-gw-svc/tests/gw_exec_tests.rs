//! Tests for the gateway internal execution pool (`GwExecPool`).
//!
//! Uses a mock `VenueAdapter` and a mock NATS publisher (via a real
//! `NatsPublisher` isn't feasible without NATS, so we test at the pool
//! dispatch level and use inline adapter mocks for worker behaviour).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};

use zk_gw_svc::gw_executor::{GwExecAction, GwExecPool};
use zk_gw_svc::venue_adapter::*;

// ── Mock adapter ─────────────────────────────────────────────────────────────

/// Records calls and optionally returns errors.
struct MockAdapter {
    place_calls: Mutex<Vec<i64>>,  // correlation_ids
    cancel_calls: Mutex<Vec<i64>>, // order_ids
    place_result: Mutex<Option<anyhow::Result<VenueCommandAck>>>,
    cancel_result: Mutex<Option<anyhow::Result<VenueCommandAck>>>,
    /// Notified after each place/cancel call completes.
    call_notify: Notify,
    /// Optional delay to simulate slow venue I/O.
    delay: Option<Duration>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            place_calls: Mutex::new(Vec::new()),
            cancel_calls: Mutex::new(Vec::new()),
            place_result: Mutex::new(None),
            cancel_result: Mutex::new(None),
            call_notify: Notify::new(),
            delay: None,
        }
    }

    fn with_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    fn with_place_error(self, msg: &str) -> Self {
        *self.place_result.blocking_lock() = Some(Err(anyhow::anyhow!(msg.to_string())));
        self
    }

    fn with_cancel_rejection(self, msg: &str) -> Self {
        *self.cancel_result.blocking_lock() = Some(Ok(VenueCommandAck {
            success: false,
            exch_order_ref: None,
            error_message: Some(msg.to_string()),
        }));
        self
    }
}

#[async_trait]
impl VenueAdapter for MockAdapter {
    async fn connect(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck> {
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        self.place_calls.lock().await.push(req.correlation_id);
        self.call_notify.notify_waiters();

        let override_result = self.place_result.lock().await.take();
        match override_result {
            Some(r) => r,
            None => Ok(VenueCommandAck {
                success: true,
                exch_order_ref: Some("EX123".into()),
                error_message: None,
            }),
        }
    }

    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck> {
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        self.cancel_calls.lock().await.push(req.order_id);
        self.call_notify.notify_waiters();

        let override_result = self.cancel_result.lock().await.take();
        match override_result {
            Some(r) => r,
            None => Ok(VenueCommandAck {
                success: true,
                exch_order_ref: None,
                error_message: None,
            }),
        }
    }

    async fn query_balance(&self, _: VenueBalanceQuery) -> anyhow::Result<Vec<VenueBalanceFact>> {
        Ok(vec![])
    }
    async fn query_order(&self, _: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>> {
        Ok(vec![])
    }
    async fn query_trades(&self, _: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>> {
        Ok(vec![])
    }
    async fn query_positions(
        &self,
        _: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>> {
        Ok(vec![])
    }
    async fn next_event(&self) -> anyhow::Result<VenueEvent> {
        // Block forever — tests don't use the event stream.
        std::future::pending().await
    }
}

// ── Helper: build a minimal VenuePlaceOrder ─────────────────────────────────

fn place_req(correlation_id: i64) -> VenuePlaceOrder {
    VenuePlaceOrder {
        correlation_id,
        exch_account_id: "ACCT".into(),
        instrument: "BTC-USDT".into(),
        buysell_type: 1,
        openclose_type: 0,
        order_type: 1,
        price: 50_000.0,
        qty: 0.01,
        leverage: 1.0,
        timestamp: 0,
    }
}

fn cancel_req(order_id: i64) -> VenueCancelOrder {
    VenueCancelOrder {
        exch_order_ref: format!("EX{order_id}"),
        order_id,
        timestamp: 0,
    }
}

// ── Helper: create pool without real NATS (workers will warn on publish) ────
//
// We can't easily create a NatsPublisher without a real NATS connection,
// so we test dispatch-level behaviour (queue full, shard routing) using
// the pool directly, and test worker behaviour through the mock adapter.

/// Create a pool backed by the given mock adapter.
/// Uses a real NatsPublisher connected to nothing — publish_order_report
/// will fail silently (warn log). Good enough for unit tests.
async fn test_pool(
    shard_count: usize,
    queue_capacity: usize,
    adapter: Arc<dyn VenueAdapter>,
) -> GwExecPool {
    // We need a NatsPublisher, but can't connect to real NATS in unit tests.
    // Build one via a dummy NATS connection. If that's not possible, we skip
    // the NATS-dependent tests.
    //
    // Alternative: test only dispatch routing + queue-full, not worker NATS publish.
    // For now, connect to localhost and let publish fail silently.
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    let publisher = match nats {
        Ok(client) => Arc::new(zk_gw_svc::nats_publisher::NatsPublisher::new(
            client,
            "test_gw".into(),
        )),
        Err(_) => {
            // Fallback: connect to a bogus address — the pool will still
            // function; publish will just fail. We don't assert on NATS publish
            // in these unit tests.
            panic!("NATS not available — run `docker compose up nats` for GW exec tests");
        }
    };

    GwExecPool::new(
        shard_count,
        queue_capacity,
        adapter,
        publisher,
        "test_gw".into(),
        42,
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Same order_id always routes to the same shard.
#[tokio::test]
async fn test_stable_shard_routing() {
    // We test shard routing without NATS by checking that dispatch succeeds
    // and the adapter receives the calls in order.
    // Skip if no NATS.
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    let adapter = Arc::new(MockAdapter::new());
    let pool = test_pool(4, 64, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    // Dispatch several orders with same order_id — they should all go to same shard.
    let order_id = 100;
    for i in 0..5 {
        let action = GwExecAction::PlaceOrder {
            venue_req: place_req(order_id + i * 4), // +4 keeps same shard (100%4==0, 104%4==0, ...)
            correlation_id: order_id + i * 4,
        };
        // All have order_id % 4 == 0, so same shard.
        pool.dispatch(order_id, action).unwrap();
    }

    // Wait for adapter to process all 5.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = adapter.place_calls.lock().await;
    assert_eq!(calls.len(), 5);
    // Verify ordering: since same shard, calls should be in dispatch order.
    assert_eq!(calls[0], 100);
    assert_eq!(calls[1], 104);
    assert_eq!(calls[2], 108);
    assert_eq!(calls[3], 112);
    assert_eq!(calls[4], 116);
}

/// Place then cancel for same order_id are processed in FIFO order on same shard.
#[tokio::test]
async fn test_place_cancel_same_order_ordered() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    let adapter = Arc::new(MockAdapter::new());
    let pool = test_pool(4, 64, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    let order_id = 42;
    pool.dispatch(
        order_id,
        GwExecAction::PlaceOrder {
            venue_req: place_req(order_id),
            correlation_id: order_id,
        },
    )
    .unwrap();
    pool.dispatch(
        order_id,
        GwExecAction::CancelOrder {
            venue_req: cancel_req(order_id),
            order_id,
        },
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let places = adapter.place_calls.lock().await;
    let cancels = adapter.cancel_calls.lock().await;
    assert_eq!(places.len(), 1);
    assert_eq!(cancels.len(), 1);
    assert_eq!(places[0], 42);
    assert_eq!(cancels[0], 42);
}

/// Fill queue to capacity, next dispatch returns Err.
#[tokio::test]
async fn test_queue_full_returns_failure() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    // Use a slow adapter so the queue fills up.
    let adapter = Arc::new(MockAdapter::new().with_delay(Duration::from_secs(10)));
    let pool = test_pool(1, 2, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    // First dispatch: worker picks it up and blocks on the slow adapter.
    // Give the worker a moment to dequeue the first item.
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(1),
            correlation_id: 1,
        },
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fill the remaining capacity (2 slots).
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(2),
            correlation_id: 2,
        },
    )
    .unwrap();
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(3),
            correlation_id: 3,
        },
    )
    .unwrap();

    // Next one should fail — queue full.
    let result = pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(4),
            correlation_id: 4,
        },
    );
    assert!(result.is_err(), "dispatch should fail when queue is full");
}

/// `dispatch` returns immediately even when adapter is slow (queue-then-reply).
#[tokio::test]
async fn test_dispatch_returns_immediately() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    let adapter = Arc::new(MockAdapter::new().with_delay(Duration::from_secs(5)));
    let pool = test_pool(2, 64, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    let start = std::time::Instant::now();
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(1),
            correlation_id: 1,
        },
    )
    .unwrap();
    let elapsed = start.elapsed();

    // dispatch should return in < 10ms (it's just a channel send).
    assert!(
        elapsed < Duration::from_millis(10),
        "dispatch took {:?} — should be instant (queue-then-reply)",
        elapsed
    );
}

/// shard_queue_depths reports correct values.
#[tokio::test]
async fn test_shard_queue_depths() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    let adapter = Arc::new(MockAdapter::new().with_delay(Duration::from_secs(10)));
    let pool = test_pool(2, 8, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    // Initially empty.
    let depths = pool.shard_queue_depths();
    assert_eq!(depths.len(), 2);

    // Enqueue 3 items to shard 0 (order_id % 2 == 0).
    for i in 0..3 {
        pool.dispatch(
            i * 2, // even → shard 0
            GwExecAction::PlaceOrder {
                venue_req: place_req(i * 2),
                correlation_id: i * 2,
            },
        )
        .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    let depths = pool.shard_queue_depths();
    // One item dequeued by worker (blocking on delay), 2 remain in queue.
    assert!(
        depths[0] >= 2,
        "shard 0 depth should be >= 2, got {}",
        depths[0]
    );
    assert_eq!(depths[1], 0, "shard 1 should be empty");
}

/// shard_count returns the correct value.
#[tokio::test]
async fn test_shard_count() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    let adapter = Arc::new(MockAdapter::new());
    let pool = test_pool(8, 16, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;
    assert_eq!(pool.shard_count(), 8);
}

/// `dispatch_or_reject` never returns an error — queue-full drops are
/// reported asynchronously via synthetic rejection reports to NATS.
#[tokio::test]
async fn test_dispatch_or_reject_does_not_block() {
    let nats = async_nats::connect("nats://127.0.0.1:4222").await;
    if nats.is_err() {
        eprintln!("SKIP: NATS not available");
        return;
    }

    // Slow adapter + tiny queue = easy to fill.
    let adapter = Arc::new(MockAdapter::new().with_delay(Duration::from_secs(10)));
    let pool = test_pool(1, 2, Arc::clone(&adapter) as Arc<dyn VenueAdapter>).await;

    // First dispatch: worker picks it up and blocks on slow adapter.
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(1),
            correlation_id: 1,
        },
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fill remaining capacity.
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(2),
            correlation_id: 2,
        },
    )
    .unwrap();
    pool.dispatch(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(3),
            correlation_id: 3,
        },
    )
    .unwrap();

    // Queue is now full. dispatch_or_reject should NOT panic or block — it
    // publishes a synthetic rejection asynchronously and returns.
    pool.dispatch_or_reject(
        1,
        GwExecAction::PlaceOrder {
            venue_req: place_req(4),
            correlation_id: 4,
        },
    );
    // If we reach here, dispatch_or_reject returned successfully despite full queue.

    // Give the spawned rejection publish task a moment to execute.
    tokio::time::sleep(Duration::from_millis(100)).await;
}
