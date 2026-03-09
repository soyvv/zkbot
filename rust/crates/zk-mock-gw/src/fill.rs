use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::proto::exch_gw::{
    order_report_entry::Report, ExchangeOrderStatus, OrderIdLinkageReport, OrderReport,
    OrderReportEntry, OrderReportType, OrderSourceType, OrderStateReport, TradeReport,
};
use crate::state::MockGwState;
use prost::Message;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn base_report(exch_order_ref: &str, order_id: i64, account_id: i64) -> OrderReport {
    OrderReport {
        exchange: "MOCK".to_string(),
        account_id,
        exch_order_ref: exch_order_ref.to_string(),
        order_id,
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        update_timestamp: now_ms(),
        ..Default::default()
    }
}

async fn publish(nats: &async_nats::Client, gw_id: &str, report: OrderReport) {
    let subject = format!("zk.gw.{gw_id}.report");
    let bytes = report.encode_to_vec();
    if let Err(e) = nats.publish(subject.clone(), bytes.into()).await {
        warn!(subject, error = %e, "failed to publish report");
    }
}

/// Publish Linkage + BOOKED immediately after order acceptance.
/// Called from the gRPC handler before spawning the fill task.
pub async fn publish_booked_report(
    nats: &async_nats::Client,
    gw_id: &str,
    exch_order_ref: &str,
    order_id: i64,
    account_id: i64,
    qty: f64,
) {
    let mut report = base_report(exch_order_ref, order_id, account_id);
    report.order_report_entries = vec![
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeLinkage as i32,
            report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                exch_order_ref: exch_order_ref.to_string(),
                order_id,
            })),
        },
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(Report::OrderStateReport(OrderStateReport {
                exch_order_status: ExchangeOrderStatus::ExchOrderStatusBooked as i32,
                filled_qty: 0.0,
                unfilled_qty: qty,
                avg_price: 0.0,
                order_info: None,
            })),
        },
    ];
    publish(nats, gw_id, report).await;
}

/// Spawned as a tokio task per placed order. Sleeps for fill_delay_ms then
/// publishes a FILLED OrderReport to NATS `zk.gw.{gw_id}.report`.
pub async fn simulate_fill(
    state: Arc<Mutex<MockGwState>>,
    exch_order_ref: String,
    order_id: i64,
) {
    let delay_ms = {
        let s = state.lock().await;
        s.fill_delay_ms
    };

    if delay_ms > 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
    }

    let (gw_id, account_id, qty, price, nats) = {
        let mut s = state.lock().await;
        match s.orders.remove(&exch_order_ref) {
            Some(order) => {
                let gw_id = s.gw_id.clone();
                let nats = s.nats_client.clone();
                s.fill_tasks.remove(&exch_order_ref);
                (gw_id, order.account_id, order.qty, order.price, nats)
            }
            None => {
                // Order was already cancelled.
                return;
            }
        }
    };

    info!(exch_order_ref, order_id, qty, price, "fill simulated");

    let Some(nats) = nats else {
        warn!("no NATS client — fill not published");
        return;
    };

    let ts = now_ms();
    let mut report = base_report(&exch_order_ref, order_id, account_id);
    report.update_timestamp = ts;
    report.order_report_entries = vec![
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(Report::OrderStateReport(OrderStateReport {
                exch_order_status: ExchangeOrderStatus::ExchOrderStatusFilled as i32,
                filled_qty: qty,
                unfilled_qty: 0.0,
                avg_price: price,
                order_info: None,
            })),
        },
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeTrade as i32,
            report: Some(Report::TradeReport(TradeReport {
                exch_trade_id: format!("mock_trade_{order_id}"),
                filled_qty: qty,
                filled_price: price,
                fill_type: 0,
                filled_ts: ts,
                exch_pnl: 0.0,
                order_info: None,
            })),
        },
    ];
    publish(&nats, &gw_id, report).await;
}

/// Publish a CANCELLED OrderReport immediately.
pub async fn publish_cancel_report(
    nats: &async_nats::Client,
    gw_id: &str,
    exch_order_ref: &str,
    order_id: i64,
    account_id: i64,
) {
    let mut report = base_report(exch_order_ref, order_id, account_id);
    report.order_report_entries = vec![OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeState as i32,
        report: Some(Report::OrderStateReport(OrderStateReport {
            exch_order_status: ExchangeOrderStatus::ExchOrderStatusCancelled as i32,
            filled_qty: 0.0,
            unfilled_qty: 0.0,
            avg_price: 0.0,
            order_info: None,
        })),
    }];
    publish(nats, gw_id, report).await;
}
