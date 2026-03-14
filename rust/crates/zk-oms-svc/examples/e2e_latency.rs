//! End-to-end OMS latency benchmark.
//!
//! Measures realistic round-trip latencies against a running dev stack by
//! executing three complete order lifecycle flows:
//!
//! - **Flow 1: Place → Booked** — gRPC PlaceOrder + wait for Booked NATS event
//! - **Flow 2: Place → Booked → Cancel → Cancelled** — cancel flow latency
//! - **Flow 3: Place → Booked → Filled** — mock-gw auto-fill latency
//!
//! Each flow is run `E2E_ITERATIONS` times (default 100).
//! Results: min / p50 / p95 / p99 / max per flow, plus throughput.
//!
//! After all flows, a full segment breakdown is printed using:
//! - OMS-published `LatencyMetricBatch` (t0–t7 timestamps per order)
//! - Bench-measured timestamps (t8 client receive, gw_ms from event fields)
//!
//! ## Timestamp definitions (all nanoseconds)
//!
//! | Tag  | Captured by | Meaning |
//! |------|-------------|---------|
//! | `t0` | bench       | `OrderRequest.timestamp × 1_000_000` — client clock at order creation |
//! | `t1` | OMS handler | `system_time_ns()` at OMS gRPC handler entry (single PlaceOrder only) |
//! | `t3` | OMS actor   | `system_time_ns()` immediately **before** `gw_pool.send_order()` |
//! | `t3r`| OMS actor   | `system_time_ns()` immediately **after** `send_order()` returns (gRPC ACK) |
//! | `t4` | mock-gw     | `system_time_ns()` at entry of `GatewayService::place_order` handler |
//! | `t5` | mock-gw     | `OrderReport.update_timestamp × 1_000_000` — GW clock when BOOKED NATS msg is published |
//! | `t6` | OMS actor   | `system_time_ns()` at entry of `GatewayOrderReport` command processing |
//! | `t7` | OMS actor   | `OrderUpdateEvent.timestamp × 1_000_000` — OMS clock when update event is published |
//! | `t8` | bench       | `system_time_ns()` immediately after receiving `OrderUpdateEvent` from NATS |
//!
//! ## Segments
//!
//! | Segment | Formula | Measured by | Meaning |
//! |---------|---------|-------------|---------|
//! | `oms_order`   | `t3 - t0`   | OMS metrics | OMS order processing + client→OMS network |
//! | `oms_through` | `t3 - t1`   | OMS metrics | Internal OMS time: cmd queue wait + core processing |
//! | `rpc`         | `t3r - t3`  | OMS metrics | gRPC round-trip OMS→GW→OMS |
//! | `gw_proc`     | `t5 - t4`   | OMS metrics | GW: receive order → publish BOOKED NATS |
//! | `nats_gw→oms` | `t6 - t5`   | OMS metrics | NATS delivery GW→OMS |
//! | `oms_report`  | `t7 - t6`   | OMS metrics | OMS: process GW report → publish update event |
//! | `gw_ms`       | `t5 - t4`   | bench (event)| Same as `gw_proc`, derived from event fields |
//! | `nats→client` | `t8 - t7`   | bench       | NATS delivery OMS→client |
//! | `total`       | `t8 - t0`   | bench       | Full end-to-end latency |
//!
//! # Prerequisites
//! ```bash
//! make dev-up       # start NATS, Postgres, Redis, mock-gw
//! make oms-run      # start OMS service in another terminal
//! ```
//!
//! # Run
//! ```bash
//! make oms-e2e-bench
//! # or:
//! E2E_OMS_ADDR=http://localhost:50051 \
//! E2E_NATS_URL=nats://localhost:4222 \
//! E2E_ITERATIONS=200 \
//! cargo run --example e2e_latency -p zk-oms-svc --release
//! ```

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use prost::Message as ProstMessage;
use tokio::time::timeout;
use tonic::transport::Channel;

use zk_proto_rs::zk::{
    common::v1::{BasicOrderType, BuySellType, LatencyMetricBatch, OpenCloseType},
    oms::v1::{
        CancelOrderRequest, OrderCancelRequest, OrderRequest, OrderStatus, OrderUpdateEvent,
        PlaceOrderRequest,
    },
};

use zk_oms_svc::proto::oms_svc::oms_service_client::OmsServiceClient;

// ── Config ────────────────────────────────────────────────────────────────────

fn oms_addr() -> String {
    std::env::var("E2E_OMS_ADDR").unwrap_or_else(|_| "http://localhost:50051".into())
}

fn nats_url() -> String {
    std::env::var("E2E_NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into())
}

fn iterations() -> usize {
    std::env::var("E2E_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
}

/// How long to wait between last flow and latency metrics flush (seconds).
fn metrics_wait_secs() -> u64 {
    std::env::var("E2E_METRICS_WAIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15)
}

const OMS_ID:     &str = "oms_dev_1";
const ACCOUNT_ID: i64  = 9001;
const INSTRUMENT: &str = "BTCUSDT_MOCK";

// ── Order ID counter ──────────────────────────────────────────────────────────

static ORDER_CTR: AtomicI64 = AtomicI64::new(5_000_000);

fn next_id() -> i64 {
    ORDER_CTR.fetch_add(1, Ordering::Relaxed)
}

// ── Wall-clock nanoseconds ────────────────────────────────────────────────────

fn system_time_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── Stats ─────────────────────────────────────────────────────────────────────

fn percentile(mut samples: Vec<f64>, p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((p / 100.0) * (samples.len() - 1) as f64).round() as usize;
    samples[idx]
}

fn print_stats(label: &str, samples_us: Vec<f64>, total_secs: f64) {
    if samples_us.is_empty() {
        println!("{label}: no samples");
        return;
    }
    let n = samples_us.len();
    println!(
        "{label} (n={n}, throughput={:.0} ops/s)",
        n as f64 / total_secs
    );
    println!(
        "  min={:.0}µs  p50={:.0}µs  p95={:.0}µs  p99={:.0}µs  max={:.0}µs",
        percentile(samples_us.clone(), 0.0),
        percentile(samples_us.clone(), 50.0),
        percentile(samples_us.clone(), 95.0),
        percentile(samples_us.clone(), 99.0),
        percentile(samples_us.clone(), 100.0),
    );
}

/// Print a latency breakdown row (values in ms, formatted to 3 decimal places).
fn print_row(label: &str, tag: &str, samples_ms: &[f64]) {
    if samples_ms.is_empty() {
        println!("  {label:<28} {tag:<8}  (no data)");
        return;
    }
    let v = samples_ms.to_vec();
    println!(
        "  {label:<28} {tag:<8}  p50={:.3}ms  p95={:.3}ms  p99={:.3}ms  max={:.3}ms  n={}",
        percentile(v.clone(), 50.0),
        percentile(v.clone(), 95.0),
        percentile(v.clone(), 99.0),
        percentile(v.clone(), 100.0),
        v.len(),
    );
}

// ── gRPC helpers ──────────────────────────────────────────────────────────────

fn make_place_req(order_id: i64) -> PlaceOrderRequest {
    PlaceOrderRequest {
        order_request: Some(OrderRequest {
            order_id,
            account_id:      ACCOUNT_ID,
            instrument_code: INSTRUMENT.into(),
            buy_sell_type:   BuySellType::BsBuy as i32,
            open_close_type: OpenCloseType::OcOpen as i32,
            order_type:      BasicOrderType::OrdertypeLimit as i32,
            price:           50_000.0,
            qty:             0.001,
            source_id:       "e2e_bench".into(),
            timestamp:       zk_oms_rs::utils::gen_timestamp_ms(),
            ..Default::default()
        }),
        audit_meta:       None,
        idempotency_key:  String::new(),
    }
}

fn make_cancel_req(order_id: i64) -> CancelOrderRequest {
    CancelOrderRequest {
        order_cancel_request: Some(OrderCancelRequest {
            order_id,
            exch_order_ref: String::new(), // OMS looks up exch_order_ref from snapshot
            timestamp:      zk_oms_rs::utils::gen_timestamp_ms(),
            ..Default::default()
        }),
        audit_meta:      None,
        idempotency_key: String::new(),
    }
}

// ── NATS subscriber helpers ───────────────────────────────────────────────────

async fn subscribe_order_updates(nats: &async_nats::Client) -> async_nats::Subscriber {
    let subj = format!("zk.oms.{OMS_ID}.order_update.{ACCOUNT_ID}");
    nats.subscribe(subj).await.expect("subscribe failed")
}

/// Timestamps extracted from a matched `OrderUpdateEvent`.
struct EventTimestamps {
    /// event.gw_report_timestamp_ns  (t5: GW publishes BOOKED)
    gw_report_ns:   i64,
    /// event.gw_received_at_ns       (t4: GW handler entry)
    gw_received_ns: i64,
    /// event.timestamp × 1e6         (t7: OMS publishes event)
    oms_publish_ns: i64,
    /// system_time_ns() at bench receive  (t8)
    client_recv_ns: i64,
}

/// Wait for an `OrderUpdateEvent` with the given `order_id` and `target_status`.
/// Returns `EventTimestamps` on success.
async fn wait_for_status(
    sub:           &mut async_nats::Subscriber,
    order_id:      i64,
    target_status: OrderStatus,
) -> Result<EventTimestamps, String> {
    let deadline = Duration::from_secs(5);
    let start = Instant::now();
    loop {
        if start.elapsed() > deadline {
            return Err(format!(
                "timeout waiting for order_id={order_id} status={target_status:?}"
            ));
        }
        match timeout(Duration::from_millis(200), sub.next()).await {
            Ok(Some(msg)) => {
                if let Ok(event) = OrderUpdateEvent::decode(msg.payload.as_ref()) {
                    if event.order_id == order_id {
                        if let Some(snap) = &event.order_snapshot {
                            let status = OrderStatus::try_from(snap.order_status)
                                .unwrap_or(OrderStatus::Unspecified);
                            if status == target_status {
                                let t8 = system_time_ns();
                                return Ok(EventTimestamps {
                                    gw_report_ns:   event.gw_report_timestamp_ns,
                                    gw_received_ns: event.gw_received_at_ns,
                                    oms_publish_ns: event.timestamp * 1_000_000,
                                    client_recv_ns: t8,
                                });
                            }
                        }
                    }
                }
            }
            Ok(None) => return Err("subscription closed".into()),
            Err(_)   => {} // timeout — loop again
        }
    }
}

// ── Per-iteration latency samples ─────────────────────────────────────────────

/// Latency samples collected per-iteration by the bench itself.
#[derive(Default)]
struct BenchSamples {
    /// (t5 - t4) / 1e6: GW receive → BOOKED publish (bench-computed from event fields)
    gw_ms:              Vec<f64>,
    /// (t8 - t7) / 1e6: OMS publish → client receive
    nats_oms_client_ms: Vec<f64>,
    /// (t8 - t0) / 1e6: full end-to-end
    total_ms:           Vec<f64>,
}

impl BenchSamples {
    fn record(&mut self, ts: &EventTimestamps, order_created_ns: i64) {
        let ns_to_ms = |ns: i64| ns as f64 / 1_000_000.0;

        if ts.gw_report_ns > 0 && ts.gw_received_ns > 0 {
            let gw = ts.gw_report_ns - ts.gw_received_ns;
            if gw >= 0 { self.gw_ms.push(ns_to_ms(gw)); }
        }
        if ts.oms_publish_ns > 0 {
            let nats_c = ts.client_recv_ns - ts.oms_publish_ns;
            if nats_c >= 0 { self.nats_oms_client_ms.push(ns_to_ms(nats_c)); }
        }
        if order_created_ns > 0 {
            let total = ts.client_recv_ns - order_created_ns;
            if total >= 0 { self.total_ms.push(ns_to_ms(total)); }
        }
    }
}

// ── OMS metrics subscriber ────────────────────────────────────────────────────

type MetricsBuf = Arc<Mutex<Vec<LatencyMetricBatch>>>;

async fn start_metrics_subscriber(nats: &async_nats::Client) -> MetricsBuf {
    let subj = format!("zk.oms.{OMS_ID}.metrics.latency");
    let mut sub = nats.subscribe(subj.clone()).await.expect("metrics subscribe failed");
    let buf: MetricsBuf = Arc::new(Mutex::new(Vec::new()));
    let buf2 = buf.clone();

    tokio::spawn(async move {
        while let Some(msg) = sub.next().await {
            if let Ok(batch) = LatencyMetricBatch::decode(msg.payload.as_ref()) {
                if let Ok(mut guard) = buf2.lock() {
                    guard.push(batch);
                }
            }
        }
    });

    buf
}

// ── Flows ─────────────────────────────────────────────────────────────────────

/// Flow 1: Place → wait for Booked.
/// Returns `(order_id, elapsed_us, event_timestamps, order_created_ns)`.
async fn flow_place_booked(
    client: &mut OmsServiceClient<Channel>,
    nats:   &async_nats::Client,
) -> Result<(i64, f64, EventTimestamps, i64), String> {
    let order_id = next_id();
    let req = make_place_req(order_id);
    let order_created_ns = req.order_request.as_ref().map(|r| r.timestamp).unwrap_or(0) * 1_000_000;

    let mut sub = subscribe_order_updates(nats).await;

    let t0 = Instant::now();
    client.place_order(req).await.map_err(|e| e.to_string())?;

    let ts = wait_for_status(&mut sub, order_id, OrderStatus::Booked).await?;
    let elapsed_us = t0.elapsed().as_secs_f64() * 1_000_000.0;

    Ok((order_id, elapsed_us, ts, order_created_ns))
}

/// Flow 2: Place → Booked → Cancel → Cancelled.
async fn flow_place_cancel(
    client: &mut OmsServiceClient<Channel>,
    nats:   &async_nats::Client,
) -> Result<(f64, EventTimestamps, i64), String> {
    let (order_id, _, _, _) = flow_place_booked(client, nats).await?;

    let mut sub = subscribe_order_updates(nats).await;
    let t_cancel = Instant::now();

    client.cancel_order(make_cancel_req(order_id)).await.map_err(|e| e.to_string())?;

    let ts = wait_for_status(&mut sub, order_id, OrderStatus::Cancelled).await?;
    let cancel_us = t_cancel.elapsed().as_secs_f64() * 1_000_000.0;
    let order_created_ns = 0i64; // cancel flow: don't measure e2e from place

    Ok((cancel_us, ts, order_created_ns))
}

/// Flow 3: Place → Booked → wait for Filled (mock-gw auto-fills).
async fn flow_place_filled(
    client: &mut OmsServiceClient<Channel>,
    nats:   &async_nats::Client,
) -> Result<(f64, EventTimestamps, i64), String> {
    let order_id = next_id();
    let req = make_place_req(order_id);
    let order_created_ns = req.order_request.as_ref().map(|r| r.timestamp).unwrap_or(0) * 1_000_000;

    let mut sub = subscribe_order_updates(nats).await;

    let t0 = Instant::now();
    client.place_order(req).await.map_err(|e| e.to_string())?;

    let ts = wait_for_status(&mut sub, order_id, OrderStatus::Filled).await?;
    let elapsed_us = t0.elapsed().as_secs_f64() * 1_000_000.0;

    Ok((elapsed_us, ts, order_created_ns))
}

// ── Latency breakdown from OMS metrics ────────────────────────────────────────

fn compute_oms_breakdown(batches: &[LatencyMetricBatch]) -> HashMap<String, Vec<f64>> {
    let ns_to_ms = |ns: i64| ns as f64 / 1_000_000.0;
    let mut segments: HashMap<String, Vec<f64>> = HashMap::new();

    for batch in batches {
        for metric in &batch.metrics {
            let get = |tag: &str| metric.tagged_timestamps.get(tag).copied().unwrap_or(0);
            let t0  = get("t0");
            let t1  = get("t1");
            let t3  = get("t3");
            let t3r = get("t3r");
            let t4  = get("t4");
            let t5  = get("t5");
            let t6  = get("t6");
            let t7  = get("t7");

            if t3 > 0 && t0 > 0 { segments.entry("oms_order".into()).or_default().push(ns_to_ms(t3 - t0)); }
            if t1 > 0 && t3 > 0 && t3 >= t1 { segments.entry("oms_through".into()).or_default().push(ns_to_ms(t3 - t1)); }
            if t3r > 0 && t3 > 0 { segments.entry("rpc".into()).or_default().push(ns_to_ms(t3r - t3)); }
            if t5 > 0 && t4 > 0 { segments.entry("gw_proc".into()).or_default().push(ns_to_ms(t5 - t4)); }
            if t6 > 0 && t5 > 0 { segments.entry("nats_gw_oms".into()).or_default().push(ns_to_ms(t6 - t5)); }
            if t7 > 0 && t6 > 0 { segments.entry("oms_report".into()).or_default().push(ns_to_ms(t7 - t6)); }
        }
    }
    segments
}

fn print_breakdown(
    bench: &BenchSamples,
    oms_segs: &HashMap<String, Vec<f64>>,
) {
    println!("\n=== Latency Breakdown ===");
    println!("  {:<28} {:<8}  {:>14}  {:>14}  {:>14}  {:>14}  {:>6}", "Segment", "Tags", "p50", "p95", "p99", "max", "n");
    println!("  {}", "-".repeat(100));

    // OMS-published segments
    let empty = vec![];
    print_row("oms_order  client→gw_send",    "t3-t0",   oms_segs.get("oms_order").unwrap_or(&empty));
    print_row("oms_through queue+core proc",  "t3-t1",   oms_segs.get("oms_through").unwrap_or(&empty));
    print_row("rpc        gRPC round-trip",   "t3r-t3",  oms_segs.get("rpc").unwrap_or(&empty));
    print_row("gw_proc    recv→BOOKED",       "t5-t4",   oms_segs.get("gw_proc").unwrap_or(&empty));
    print_row("nats_gw→oms GW→OMS delivery", "t6-t5",   oms_segs.get("nats_gw_oms").unwrap_or(&empty));
    print_row("oms_report OMS proc→publish",  "t7-t6",   oms_segs.get("oms_report").unwrap_or(&empty));

    println!("  {}", "·".repeat(100));

    // Bench-measured segments
    print_row("gw_ms      GW through (event)", "t5-t4",  &bench.gw_ms);
    print_row("nats→client OMS→client",        "t8-t7",  &bench.nats_oms_client_ms);
    print_row("total      end-to-end",          "t8-t0",  &bench.total_ms);
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let n = iterations();

    println!("=== OMS E2E Latency Benchmark ===");
    println!("OMS:        {}", oms_addr());
    println!("NATS:       {}", nats_url());
    println!("Iterations: {n}");
    println!("Instrument: {INSTRUMENT} @ account {ACCOUNT_ID}");
    println!();

    // Connect to OMS gRPC.
    let channel = tonic::transport::Channel::from_shared(oms_addr())
        .expect("invalid OMS address")
        .connect()
        .await
        .expect("failed to connect to OMS gRPC — is `make oms-run` running?");
    let mut client = OmsServiceClient::new(channel);

    // Connect to NATS.
    let nats = async_nats::connect(&nats_url())
        .await
        .expect("failed to connect to NATS — is `make dev-up` running?");

    // Start background OMS metrics subscriber.
    let metrics_buf = start_metrics_subscriber(&nats).await;

    // Bench samples (bench-measured segments).
    let mut bench_samples = BenchSamples::default();

    // ── Flow 1: Place → Booked ────────────────────────────────────────────────
    println!("--- Flow 1: Place → Booked ---");
    let mut f1_samples = Vec::with_capacity(n);
    let f1_start = Instant::now();
    let mut f1_errors = 0usize;
    for i in 0..n {
        match flow_place_booked(&mut client, &nats).await {
            Ok((_, us, ts, order_created_ns)) => {
                f1_samples.push(us);
                bench_samples.record(&ts, order_created_ns);
            }
            Err(e) => {
                f1_errors += 1;
                if f1_errors <= 3 { eprintln!("  iter {i} error: {e}"); }
            }
        }
    }
    let f1_secs = f1_start.elapsed().as_secs_f64();
    print_stats("Place→Booked", f1_samples, f1_secs);
    if f1_errors > 0 { println!("  ERRORS: {f1_errors}/{n}"); }
    println!();

    // ── Flow 2: Place → Booked → Cancel → Cancelled ───────────────────────────
    println!("--- Flow 2: Place → Booked → Cancel → Cancelled ---");
    let mut f2_samples = Vec::with_capacity(n);
    let f2_start = Instant::now();
    let mut f2_errors = 0usize;
    for i in 0..n {
        match flow_place_cancel(&mut client, &nats).await {
            Ok((us, ts, order_created_ns)) => {
                f2_samples.push(us);
                bench_samples.record(&ts, order_created_ns);
            }
            Err(e) => {
                f2_errors += 1;
                if f2_errors <= 3 { eprintln!("  iter {i} error: {e}"); }
            }
        }
    }
    let f2_secs = f2_start.elapsed().as_secs_f64();
    print_stats("Cancel→Cancelled (from Booked)", f2_samples, f2_secs);
    if f2_errors > 0 { println!("  ERRORS: {f2_errors}/{n}"); }
    println!();

    // ── Flow 3: Place → Booked → Filled ──────────────────────────────────────
    println!("--- Flow 3: Place → Booked → Filled (mock-gw auto-fill) ---");
    let mut f3_samples = Vec::with_capacity(n);
    let f3_start = Instant::now();
    let mut f3_errors = 0usize;
    // Use fewer iterations for Flow 3 — fill delay adds up.
    let n3 = n.min(50);
    for i in 0..n3 {
        match flow_place_filled(&mut client, &nats).await {
            Ok((us, ts, order_created_ns)) => {
                f3_samples.push(us);
                bench_samples.record(&ts, order_created_ns);
            }
            Err(e) => {
                f3_errors += 1;
                if f3_errors <= 3 { eprintln!("  iter {i} error: {e}"); }
            }
        }
    }
    let f3_secs = f3_start.elapsed().as_secs_f64();
    print_stats("Place→Filled (end-to-end)", f3_samples, f3_secs);
    if f3_errors > 0 { println!("  ERRORS: {f3_errors}/{n3}"); }

    // ── Latency breakdown ─────────────────────────────────────────────────────
    // Wait for OMS to flush its latency metrics batch.
    let wait = metrics_wait_secs();
    println!("\nWaiting {wait}s for OMS latency metrics flush...");
    tokio::time::sleep(Duration::from_secs(wait)).await;

    let batches = metrics_buf.lock().unwrap().drain(..).collect::<Vec<_>>();
    let oms_segs = compute_oms_breakdown(&batches);
    let total_oms_samples: usize = oms_segs.values().map(|v| v.len()).max().unwrap_or(0);
    println!("OMS metrics batches received: {}  (≈{} order samples)", batches.len(), total_oms_samples);

    print_breakdown(&bench_samples, &oms_segs);
}
