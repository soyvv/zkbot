use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use prost::Message as ProstMessage;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tonic::transport::{Channel, Endpoint};

use zk_infra_rs::nats_kv::{KvRegistryClient, REGISTRY_BUCKET};
use zk_proto_rs::zk::common::v1::{BasicOrderType, BuySellType, LatencyMetricBatch, OpenCloseType};
use zk_proto_rs::zk::exch_gw::v1::{BalanceUpdate as GwBalanceUpdate, OrderReport};
use zk_proto_rs::zk::gateway::v1::{gateway_response, GatewayResponse, SendOrderRequest};
use zk_proto_rs::zk::oms::v1::{
    oms_response, BalanceUpdateEvent as OmsBalanceUpdateEvent, CancelOrderRequest,
    OrderCancelRequest, OrderRequest, OrderStatus, OrderUpdateEvent, PlaceOrderRequest,
    PositionUpdateEvent,
};

use crate::cli::Cli;
use crate::report::{PercentileSummary, ScenarioReport, StepReport};

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(crate) fn next_order_id() -> i64 {
    now_ms() * 1_000
}

pub(crate) async fn timed_step<T, F>(report: &mut ScenarioReport, name: &str, fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<(T, String)>>,
{
    let started = Instant::now();
    match fut.await {
        Ok((value, detail)) => {
            report.steps.push(StepReport {
                name: name.to_string(),
                ok: true,
                duration_ms: started.elapsed().as_millis(),
                detail,
            });
            Ok(value)
        }
        Err(err) => {
            report.steps.push(StepReport {
                name: name.to_string(),
                ok: false,
                duration_ms: started.elapsed().as_millis(),
                detail: err.to_string(),
            });
            Err(err)
        }
    }
}

pub(crate) async fn connect_nats(url: &str) -> Result<async_nats::Client> {
    zk_infra_rs::nats::connect(url)
        .await
        .map_err(|e| anyhow!("NATS connect failed: {e}"))
}

pub(crate) async fn connect_channel(addr: &str) -> Result<Channel> {
    Endpoint::from_shared(normalize_endpoint(addr))
        .context("invalid gRPC endpoint")?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|e| anyhow!("gRPC connect failed to {addr}: {e}"))
}

fn normalize_endpoint(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    }
}

async fn resolve_addr_from_kv(
    nats: &async_nats::Client,
    key: &str,
) -> Result<String> {
    use prost::Message as _;
    use zk_proto_rs::zk::discovery::v1::ServiceRegistration as SvcRegProto;

    let js = async_nats::jetstream::new(nats.clone());
    let kv = KvRegistryClient::open(&js, REGISTRY_BUCKET)
        .await
        .map_err(|e| anyhow!("failed to open KV registry bucket: {e}"))?;
    let bytes = kv
        .get(key)
        .await
        .map_err(|e| anyhow!("failed to read registry key {key}: {e}"))?
        .ok_or_else(|| anyhow!("service registry entry not found: {key}"))?;
    let reg = SvcRegProto::decode(bytes.as_ref())
        .context("failed to decode registry proto")?;
    let address = reg
        .endpoint
        .as_ref()
        .map(|ep| &ep.address)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| anyhow!("registry entry missing endpoint address: {key}"))?;
    Ok(normalize_endpoint(address))
}

pub(crate) async fn resolve_gw_addr(
    nats: &async_nats::Client,
    gw_id: &str,
    override_addr: &Option<String>,
) -> Result<String> {
    if let Some(addr) = override_addr {
        return Ok(normalize_endpoint(addr));
    }
    resolve_addr_from_kv(nats, &format!("svc.gw.{gw_id}")).await
}

pub(crate) async fn resolve_oms_addr(
    nats: &async_nats::Client,
    oms_id: &str,
    override_addr: &Option<String>,
) -> Result<String> {
    if let Some(addr) = override_addr {
        return Ok(normalize_endpoint(addr));
    }
    resolve_addr_from_kv(nats, &format!("svc.oms.{oms_id}")).await
}

pub(crate) fn default_exch_account_id(cli: &Cli) -> String {
    cli.exch_account_id
        .clone()
        .unwrap_or_else(|| cli.account_id.to_string())
}

pub(crate) fn make_gw_place(cli: &Cli, order_id: i64) -> SendOrderRequest {
    SendOrderRequest {
        correlation_id: order_id,
        exch_account_id: default_exch_account_id(cli),
        instrument: cli.instrument.clone(),
        buysell_type: BuySellType::BsBuy as i32,
        openclose_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        scaled_price: cli.price,
        scaled_qty: cli.qty,
        leverage: 1.0,
        timestamp: now_ms(),
        ..Default::default()
    }
}

pub(crate) fn make_oms_place(cli: &Cli, order_id: i64) -> PlaceOrderRequest {
    PlaceOrderRequest {
        order_request: Some(OrderRequest {
            order_id,
            account_id: cli.account_id,
            instrument_code: cli.instrument.clone(),
            buy_sell_type: BuySellType::BsBuy as i32,
            open_close_type: OpenCloseType::OcOpen as i32,
            order_type: BasicOrderType::OrdertypeLimit as i32,
            price: cli.price,
            qty: cli.qty,
            source_id: "trade_tools".to_string(),
            timestamp: now_ms(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) fn make_oms_cancel(order_id: i64) -> CancelOrderRequest {
    CancelOrderRequest {
        order_cancel_request: Some(OrderCancelRequest {
            order_id,
            timestamp: now_ms(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(crate) async fn wait_for_gw_order_report(
    sub: &mut async_nats::Subscriber,
    order_id: i64,
    timeout_ms: u64,
) -> Result<OrderReport> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for gateway OrderReport for order_id={order_id}");
        }
        let msg = tokio::time::timeout(remaining, sub.next())
            .await
            .context("timeout waiting for NATS message")?
            .ok_or_else(|| anyhow!("gateway report subscription ended"))?;
        let report = OrderReport::decode(msg.payload.as_ref())
            .context("failed to decode gateway OrderReport")?;
        if report.order_id == order_id {
            return Ok(report);
        }
    }
}

pub(crate) async fn wait_for_any_gw_balance(
    sub: &mut async_nats::Subscriber,
    timeout_ms: u64,
) -> Result<GwBalanceUpdate> {
    let msg = tokio::time::timeout(Duration::from_millis(timeout_ms), sub.next())
        .await
        .context("timeout waiting for gateway balance update")?
        .ok_or_else(|| anyhow!("gateway balance subscription ended"))?;
    GwBalanceUpdate::decode(msg.payload.as_ref()).context("failed to decode gateway BalanceUpdate")
}

pub(crate) async fn wait_for_oms_order_update(
    sub: &mut async_nats::Subscriber,
    order_id: i64,
    expected_status: Option<OrderStatus>,
    timeout_ms: u64,
) -> Result<OrderUpdateEvent> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out waiting for OMS OrderUpdateEvent for order_id={order_id}");
        }
        let msg = tokio::time::timeout(remaining, sub.next())
            .await
            .context("timeout waiting for OMS order update")?
            .ok_or_else(|| anyhow!("OMS order update subscription ended"))?;
        let event = OrderUpdateEvent::decode(msg.payload.as_ref())
            .context("failed to decode OMS OrderUpdateEvent")?;
        if event.order_id != order_id {
            continue;
        }
        if let Some(status) = expected_status {
            let seen = event
                .order_snapshot
                .as_ref()
                .and_then(|o| OrderStatus::try_from(o.order_status).ok());
            if seen != Some(status) {
                continue;
            }
        }
        return Ok(event);
    }
}

pub(crate) async fn wait_for_any_oms_balance(
    sub: &mut async_nats::Subscriber,
    timeout_ms: u64,
) -> Result<OmsBalanceUpdateEvent> {
    let msg = tokio::time::timeout(Duration::from_millis(timeout_ms), sub.next())
        .await
        .context("timeout waiting for OMS balance update")?
        .ok_or_else(|| anyhow!("OMS balance subscription ended"))?;
    OmsBalanceUpdateEvent::decode(msg.payload.as_ref())
        .context("failed to decode OMS BalanceUpdateEvent")
}

pub(crate) async fn wait_for_any_oms_position(
    sub: &mut async_nats::Subscriber,
    timeout_ms: u64,
) -> Result<PositionUpdateEvent> {
    let msg = tokio::time::timeout(Duration::from_millis(timeout_ms), sub.next())
        .await
        .context("timeout waiting for OMS position update")?
        .ok_or_else(|| anyhow!("OMS position subscription ended"))?;
    PositionUpdateEvent::decode(msg.payload.as_ref())
        .context("failed to decode OMS PositionUpdateEvent")
}

pub(crate) async fn wait_for_latency_metric(
    sub: &mut async_nats::Subscriber,
    order_id: i64,
    timeout_ms: u64,
) -> Result<Option<HashMap<String, i64>>> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let Some(msg) = tokio::time::timeout(remaining, sub.next())
            .await
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let batch = LatencyMetricBatch::decode(msg.payload.as_ref())
            .context("failed to decode LatencyMetricBatch")?;
        for metric in batch.metrics {
            if metric.order_id == order_id {
                return Ok(Some(metric.tagged_timestamps));
            }
        }
    }
}

pub(crate) fn gateway_response_ok(resp: &GatewayResponse) -> bool {
    gateway_response::Status::try_from(resp.status).ok()
        == Some(gateway_response::Status::GwRespStatusSuccess)
}

pub(crate) fn oms_response_ok(resp: &zk_proto_rs::zk::oms::v1::OmsResponse) -> bool {
    oms_response::Status::try_from(resp.status).ok()
        == Some(oms_response::Status::OmsRespStatusSuccess)
}

pub(crate) struct GwWorkloadGenerator {
    instruments: Vec<String>,
    exch_accounts: Vec<String>,
    base_price: f64,
    base_qty: f64,
    seq: u64,
}

impl GwWorkloadGenerator {
    pub(crate) fn new(
        cli: &Cli,
        instrument_pool: Vec<String>,
        exch_account_pool: Vec<String>,
    ) -> Self {
        let instruments = if instrument_pool.is_empty() {
            vec![cli.instrument.clone()]
        } else {
            instrument_pool
        };
        let exch_accounts = if exch_account_pool.is_empty() {
            vec![default_exch_account_id(cli)]
        } else {
            exch_account_pool
        };
        Self {
            instruments,
            exch_accounts,
            base_price: cli.price,
            base_qty: cli.qty,
            seq: 0,
        }
    }

    pub(crate) fn next_request(&mut self, order_id: i64) -> SendOrderRequest {
        let seq = self.seq;
        self.seq += 1;
        let side = if seq % 2 == 0 {
            BuySellType::BsBuy
        } else {
            BuySellType::BsSell
        } as i32;
        let instrument = self.instruments[(seq as usize) % self.instruments.len()].clone();
        let exch_account_id = self.exch_accounts[(seq as usize) % self.exch_accounts.len()].clone();
        let price_jitter = ((seq % 11) as f64 - 5.0) * 0.0002;
        let qty_jitter = 1.0 + ((seq % 5) as f64) * 0.05;
        SendOrderRequest {
            correlation_id: order_id,
            exch_account_id,
            instrument,
            buysell_type: side,
            openclose_type: OpenCloseType::OcOpen as i32,
            order_type: BasicOrderType::OrdertypeLimit as i32,
            scaled_price: self.base_price * (1.0 + price_jitter),
            scaled_qty: self.base_qty * qty_jitter,
            leverage: 1.0,
            timestamp: now_ms(),
            ..Default::default()
        }
    }
}

pub(crate) struct OmsWorkloadGenerator {
    instruments: Vec<String>,
    base_price: f64,
    base_qty: f64,
    account_id: i64,
    seq: u64,
}

impl OmsWorkloadGenerator {
    pub(crate) fn new(cli: &Cli, instrument_pool: Vec<String>) -> Self {
        let instruments = if instrument_pool.is_empty() {
            vec![cli.instrument.clone()]
        } else {
            instrument_pool
        };
        Self {
            instruments,
            base_price: cli.price,
            base_qty: cli.qty,
            account_id: cli.account_id,
            seq: 0,
        }
    }

    pub(crate) fn next_request(&mut self, order_id: i64) -> PlaceOrderRequest {
        let seq = self.seq;
        self.seq += 1;
        let side = if seq % 2 == 0 {
            BuySellType::BsBuy
        } else {
            BuySellType::BsSell
        } as i32;
        let instrument = self.instruments[(seq as usize) % self.instruments.len()].clone();
        let price_jitter = ((seq % 11) as f64 - 5.0) * 0.0002;
        let qty_jitter = 1.0 + ((seq % 5) as f64) * 0.05;
        PlaceOrderRequest {
            order_request: Some(OrderRequest {
                order_id,
                account_id: self.account_id,
                instrument_code: instrument,
                buy_sell_type: side,
                open_close_type: OpenCloseType::OcOpen as i32,
                order_type: BasicOrderType::OrdertypeLimit as i32,
                price: self.base_price * (1.0 + price_jitter),
                qty: self.base_qty * qty_jitter,
                source_id: "trade_tools".to_string(),
                timestamp: now_ms(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

pub(crate) struct GatewayReportTracker {
    pending: std::sync::Arc<Mutex<HashMap<i64, oneshot::Sender<Instant>>>>,
    handle: JoinHandle<()>,
}

impl GatewayReportTracker {
    pub(crate) fn new(mut sub: async_nats::Subscriber) -> Self {
        let pending =
            std::sync::Arc::new(Mutex::new(HashMap::<i64, oneshot::Sender<Instant>>::new()));
        let pending_task = pending.clone();
        let handle = tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                let Ok(report) = OrderReport::decode(msg.payload.as_ref()) else {
                    continue;
                };
                let sender = {
                    let mut guard = pending_task.lock().await;
                    guard.remove(&report.order_id)
                };
                if let Some(tx) = sender {
                    let _ = tx.send(Instant::now());
                }
            }
        });
        Self { pending, handle }
    }

    pub(crate) async fn register(&self, order_id: i64) -> oneshot::Receiver<Instant> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(order_id, tx);
        rx
    }

    pub(crate) async fn clear(&self, order_id: i64) {
        self.pending.lock().await.remove(&order_id);
    }

    pub(crate) fn shutdown(self) {
        self.handle.abort();
    }
}

pub(crate) struct OmsOrderUpdateTracker {
    pending: std::sync::Arc<Mutex<HashMap<i64, oneshot::Sender<Instant>>>>,
    handle: JoinHandle<()>,
}

impl OmsOrderUpdateTracker {
    pub(crate) fn new(mut sub: async_nats::Subscriber) -> Self {
        let pending =
            std::sync::Arc::new(Mutex::new(HashMap::<i64, oneshot::Sender<Instant>>::new()));
        let pending_task = pending.clone();
        let handle = tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                let Ok(event) = OrderUpdateEvent::decode(msg.payload.as_ref()) else {
                    continue;
                };
                let sender = {
                    let mut guard = pending_task.lock().await;
                    guard.remove(&event.order_id)
                };
                if let Some(tx) = sender {
                    let _ = tx.send(Instant::now());
                }
            }
        });
        Self { pending, handle }
    }

    pub(crate) async fn register(&self, order_id: i64) -> oneshot::Receiver<Instant> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(order_id, tx);
        rx
    }

    pub(crate) async fn clear(&self, order_id: i64) {
        self.pending.lock().await.remove(&order_id);
    }

    pub(crate) fn shutdown(self) {
        self.handle.abort();
    }
}

pub(crate) struct OmsMetricObservation {
    pub(crate) received_at: Instant,
    pub(crate) tagged_timestamps: HashMap<String, i64>,
}

pub(crate) struct OmsMetricTracker {
    pending: std::sync::Arc<Mutex<HashMap<i64, oneshot::Sender<OmsMetricObservation>>>>,
    handle: JoinHandle<()>,
}

impl OmsMetricTracker {
    pub(crate) fn new(mut sub: async_nats::Subscriber) -> Self {
        let pending = std::sync::Arc::new(Mutex::new(HashMap::<
            i64,
            oneshot::Sender<OmsMetricObservation>,
        >::new()));
        let pending_task = pending.clone();
        let handle = tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                let Ok(batch) = LatencyMetricBatch::decode(msg.payload.as_ref()) else {
                    continue;
                };
                for metric in batch.metrics {
                    let sender = {
                        let mut guard = pending_task.lock().await;
                        guard.remove(&metric.order_id)
                    };
                    if let Some(tx) = sender {
                        let _ = tx.send(OmsMetricObservation {
                            received_at: Instant::now(),
                            tagged_timestamps: metric.tagged_timestamps,
                        });
                    }
                }
            }
        });
        Self { pending, handle }
    }

    pub(crate) async fn register(&self, order_id: i64) -> oneshot::Receiver<OmsMetricObservation> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(order_id, tx);
        rx
    }

    pub(crate) async fn clear(&self, order_id: i64) {
        self.pending.lock().await.remove(&order_id);
    }

    pub(crate) fn shutdown(self) {
        self.handle.abort();
    }
}

pub(crate) fn compute_percentiles(mut values: Vec<f64>) -> PercentileSummary {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = |p: f64| -> usize {
        if values.len() <= 1 {
            return 0;
        }
        let rank = ((values.len() - 1) as f64 * p).round() as usize;
        rank.min(values.len() - 1)
    };
    let sum: f64 = values.iter().sum();
    PercentileSummary {
        avg_ms: sum / values.len() as f64,
        p50_ms: values[idx(0.50)],
        p90_ms: values[idx(0.90)],
        p99_ms: values[idx(0.99)],
        p99_9_ms: values[idx(0.999)],
        max_ms: *values.last().unwrap_or(&0.0),
    }
}
