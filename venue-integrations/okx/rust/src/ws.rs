use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

use crate::auth;
use crate::config::OkxConfig;
use crate::normalize::{self, OkxIdMap, OrderReport};
use crate::rest::OkxRestClient;

/// Events produced by the WS supervisor for the adapter's event channel.
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// A normalized order report from a WS push.
    OrderReport(OrderReport),
    /// A balance snapshot from the WS "account" channel.
    Balance(Vec<normalize::BalanceFact>),
    /// A position snapshot from the WS "positions" channel.
    Position(Vec<normalize::PositionFact>),
    /// System connectivity event.
    Connected,
    Disconnected,
}

/// Outcome of a single WS session attempt.
enum SessionOutcome {
    /// Clean shutdown requested — exit the supervisor loop.
    Shutdown,
    /// Session failed. `established` indicates whether login+subscribe succeeded
    /// before the failure (used to decide whether to reset backoff).
    Failed {
        established: bool,
        error: anyhow::Error,
    },
}

/// WebSocket supervisor — owns the WS lifecycle, runs as a spawned task.
pub struct OkxWsSupervisor {
    pub config: OkxConfig,
    pub event_tx: mpsc::Sender<WsEvent>,
    pub rest_client: Arc<OkxRestClient>,
    pub id_map: Arc<OkxIdMap>,
    pub shutdown_rx: watch::Receiver<bool>,
    pub gw_id: String,
    pub account_id: i64,
    /// Fires once after the first successful login+subscribe (or on first failure).
    /// `None` after it's been consumed.
    pub ready_tx: Option<oneshot::Sender<anyhow::Result<()>>>,
}

impl OkxWsSupervisor {
    /// Main run loop. Call via `tokio::spawn(supervisor.run())`.
    pub async fn run(mut self) {
        let mut backoff_ms: u64 = 1000;
        let max_backoff_ms: u64 = 30_000;

        loop {
            if *self.shutdown_rx.borrow() {
                info!("OKX WS supervisor: shutdown requested");
                self.signal_ready(Err(anyhow::anyhow!("shutdown before connect")));
                break;
            }

            match self.run_session().await {
                SessionOutcome::Shutdown => {
                    info!("OKX WS session ended cleanly");
                    break;
                }
                SessionOutcome::Failed { established, error } => {
                    error!(error = %error, "OKX WS session error");

                    // If the first session never established, signal connect failure
                    // so that connect() can return an error.
                    if !established {
                        if self.ready_tx.is_some() {
                            self.signal_ready(Err(anyhow::anyhow!(
                                "WS initial login failed: {error}"
                            )));
                            // Don't retry on initial connect failure — let caller decide.
                            break;
                        }
                    }

                    let _ = self.event_tx.send(WsEvent::Disconnected).await;

                    // Compensating queries after disconnect.
                    if let Err(e) = self.run_compensating_queries().await {
                        warn!(error = %e, "compensating queries failed");
                    }

                    // Reset backoff if the session was established (login succeeded),
                    // meaning this was a mid-session disconnect, not a connection failure.
                    if established {
                        backoff_ms = 1000;
                    }

                    info!(backoff_ms, "OKX WS reconnecting after backoff");
                    sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                }
            }
        }
    }

    /// Signal the ready channel (idempotent — only fires once).
    fn signal_ready(&mut self, result: anyhow::Result<()>) {
        if let Some(tx) = self.ready_tx.take() {
            let _ = tx.send(result);
        }
    }

    async fn run_session(&mut self) -> SessionOutcome {
        let ws_url = self.config.ws_url();
        info!(url = %ws_url, "OKX WS connecting");

        let (ws_stream, _resp) = match tokio_tungstenite::connect_async(ws_url).await {
            Ok(r) => r,
            Err(e) => {
                return SessionOutcome::Failed {
                    established: false,
                    error: e.into(),
                }
            }
        };
        let (mut write, mut read) = ws_stream.split();

        info!("OKX WS connected, sending login");

        // ── Login ────────────────────────────────────────────────────────
        let ts = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        let sign = auth::sign_ws(&self.config.secret_key, &ts);

        let login_msg = serde_json::json!({
            "op": "login",
            "args": [{
                "apiKey": self.config.api_key,
                "passphrase": self.config.passphrase,
                "timestamp": ts,
                "sign": sign
            }]
        });
        if let Err(e) = write
            .send(tokio_tungstenite::tungstenite::Message::Text(
                login_msg.to_string(),
            ))
            .await
        {
            return SessionOutcome::Failed {
                established: false,
                error: e.into(),
            };
        }

        // Wait for login response.
        let login_ok = match self.wait_for_login(&mut read).await {
            Ok(ok) => ok,
            Err(e) => {
                return SessionOutcome::Failed {
                    established: false,
                    error: e,
                }
            }
        };
        if !login_ok {
            return SessionOutcome::Failed {
                established: false,
                error: anyhow::anyhow!("OKX WS login failed"),
            };
        }
        info!("OKX WS login successful");

        // ── Subscribe ────────────────────────────────────────────────────
        let sub_msg = serde_json::json!({
            "op": "subscribe",
            "args": [
                {"channel": "orders", "instType": "ANY"},
                {"channel": "account"},
                {"channel": "positions", "instType": "ANY"}
            ]
        });
        if let Err(e) = write
            .send(tokio_tungstenite::tungstenite::Message::Text(
                sub_msg.to_string(),
            ))
            .await
        {
            return SessionOutcome::Failed {
                established: false,
                error: e.into(),
            };
        }
        info!("OKX WS subscribed to orders + account + positions");

        let _ = self.event_tx.send(WsEvent::Connected).await;

        // Signal readiness (first session only).
        self.signal_ready(Ok(()));

        // Session is now established — any failure from here should reset backoff.

        // ── Read loop ────────────────────────────────────────────────────
        let mut ping_interval = interval(Duration::from_secs(25));
        ping_interval.tick().await; // consume immediate first tick

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            self.handle_text_message(&text).await;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                            let _ = write.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                            warn!("OKX WS received close frame");
                            return SessionOutcome::Failed {
                                established: true,
                                error: anyhow::anyhow!("WS closed by server"),
                            };
                        }
                        Some(Err(e)) => {
                            return SessionOutcome::Failed {
                                established: true,
                                error: anyhow::anyhow!("WS read error: {e}"),
                            };
                        }
                        None => {
                            return SessionOutcome::Failed {
                                established: true,
                                error: anyhow::anyhow!("WS stream ended"),
                            };
                        }
                        _ => {} // Binary, Frame — ignore
                    }
                }
                _ = ping_interval.tick() => {
                    debug!("OKX WS sending ping");
                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Text("ping".to_string())).await;
                }
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        info!("OKX WS shutdown signal received");
                        let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                        return SessionOutcome::Shutdown;
                    }
                }
            }
        }
    }

    async fn wait_for_login(
        &self,
        read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> anyhow::Result<bool> {
        let timeout = sleep(Duration::from_secs(10));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            if text == "pong" {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                                if v["event"].as_str() == Some("login") {
                                    return Ok(v["code"].as_str() == Some("0"));
                                }
                                if v["event"].as_str() == Some("error") {
                                    warn!(msg = %text, "OKX WS login error");
                                    return Ok(false);
                                }
                            }
                        }
                        Some(Err(e)) => return Err(anyhow::anyhow!("WS error during login: {e}")),
                        None => return Err(anyhow::anyhow!("WS closed during login")),
                        _ => {}
                    }
                }
                _ = &mut timeout => {
                    return Err(anyhow::anyhow!("OKX WS login timeout (10s)"));
                }
            }
        }
    }

    async fn handle_text_message(&self, text: &str) {
        if text == "pong" {
            return;
        }

        let v: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "OKX WS: failed to parse message");
                return;
            }
        };

        if v.get("event").is_some() {
            debug!(event = %v["event"], "OKX WS event");
            return;
        }

        let channel = match v["arg"]["channel"].as_str() {
            Some(c) => c,
            None => return,
        };

        let data_arr = match v["data"].as_array() {
            Some(arr) => arr,
            None => return,
        };

        match channel {
            "orders" => {
                for item in data_arr {
                    if let Some(report) = normalize::normalize_ws_order(
                        item,
                        &self.id_map,
                        &self.gw_id,
                        self.account_id,
                    ) {
                        let _ = self.event_tx.send(WsEvent::OrderReport(report)).await;
                    }
                }
            }
            "account" => {
                for item in data_arr {
                    let facts = normalize::normalize_ws_account(item);
                    if !facts.is_empty() {
                        let _ = self.event_tx.send(WsEvent::Balance(facts)).await;
                    }
                }
            }
            "positions" => {
                for item in data_arr {
                    let facts = normalize::normalize_ws_positions(item);
                    if !facts.is_empty() {
                        let _ = self.event_tx.send(WsEvent::Position(facts)).await;
                    }
                }
            }
            _ => {
                debug!(channel, "OKX WS: unhandled channel");
            }
        }
    }

    /// Run compensating queries after reconnect to fill any gaps.
    async fn run_compensating_queries(&self) -> anyhow::Result<()> {
        info!("OKX: running compensating queries");

        // 1. Open orders (still live on exchange).
        match self.rest_client.query_open_orders(None).await {
            Ok(orders) => {
                for o in &orders {
                    let fact = normalize::normalize_rest_order(o, &self.id_map);
                    let report = fact_to_order_report(&fact, &self.gw_id, self.account_id);
                    let _ = self.event_tx.send(WsEvent::OrderReport(report)).await;
                }
                info!(count = orders.len(), "compensating: open orders");
            }
            Err(e) => warn!(error = %e, "compensating: open orders failed"),
        }

        // 2. Recent trades — catches fills that happened while WS was down.
        match self.rest_client.query_trades(None, None).await {
            Ok(trades) => {
                for t in &trades {
                    let fact = normalize::normalize_rest_trade(t, &self.id_map);
                    let report =
                        trade_fact_to_order_report(&fact, &self.gw_id, self.account_id);
                    let _ = self.event_tx.send(WsEvent::OrderReport(report)).await;
                }
                info!(count = trades.len(), "compensating: recent trades");
            }
            Err(e) => warn!(error = %e, "compensating: recent trades failed"),
        }

        // 3. Balances.
        match self.rest_client.query_balance(None).await {
            Ok(details) => {
                let facts: Vec<normalize::BalanceFact> =
                    details.iter().map(normalize::normalize_rest_balance).collect();
                if !facts.is_empty() {
                    let _ = self.event_tx.send(WsEvent::Balance(facts)).await;
                }
                info!(count = details.len(), "compensating: balances");
            }
            Err(e) => warn!(error = %e, "compensating: balances failed"),
        }

        // 4. Positions (for derivatives).
        match self.rest_client.query_positions(None).await {
            Ok(positions) => {
                let facts: Vec<normalize::PositionFact> = positions
                    .iter()
                    .map(normalize::normalize_rest_position)
                    .collect();
                if !facts.is_empty() {
                    let _ = self.event_tx.send(WsEvent::Position(facts)).await;
                }
                info!(count = positions.len(), "compensating: positions");
            }
            Err(e) => warn!(error = %e, "compensating: positions failed"),
        }

        Ok(())
    }
}

/// Convert an OrderFact into a minimal OrderReport for the semantic pipeline.
fn fact_to_order_report(
    fact: &normalize::OrderFact,
    gw_id: &str,
    account_id: i64,
) -> OrderReport {
    use normalize::*;

    let entries = vec![
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeLinkage as i32,
            report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                exch_order_ref: fact.exch_order_ref.clone(),
                order_id: fact.order_id,
            })),
        },
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(Report::OrderStateReport(OrderStateReport {
                exch_order_status: fact.status as i32,
                filled_qty: fact.filled_qty,
                unfilled_qty: fact.unfilled_qty,
                avg_price: fact.avg_price,
                order_info: Some(OrderInfo {
                    exch_order_ref: fact.exch_order_ref.clone(),
                    client_order_id: fact.order_id.to_string(),
                    instrument: fact.instrument.clone(),
                    exch_account_id: String::new(),
                    buy_sell_type: 0,
                    place_qty: fact.filled_qty + fact.unfilled_qty,
                    place_price: 0.0,
                }),
            })),
        },
    ];

    OrderReport {
        exch_order_ref: fact.exch_order_ref.clone(),
        order_id: fact.order_id,
        exchange: gw_id.to_string(),
        order_report_entries: entries,
        account_id,
        update_timestamp: fact.timestamp,
        extra_order_id: String::new(),
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        gw_received_at_ns: 0,
    }
}

/// Convert a TradeFact into an OrderReport with a trade entry for the semantic pipeline.
fn trade_fact_to_order_report(
    fact: &normalize::TradeFact,
    gw_id: &str,
    account_id: i64,
) -> OrderReport {
    use normalize::*;

    let entries = vec![
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeLinkage as i32,
            report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                exch_order_ref: fact.exch_order_ref.clone(),
                order_id: fact.order_id,
            })),
        },
        OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeTrade as i32,
            report: Some(Report::TradeReport(TradeReport {
                exch_trade_id: fact.exch_trade_id.clone(),
                filled_qty: fact.filled_qty,
                filled_price: fact.filled_price,
                fill_type: 0,
                filled_ts: fact.timestamp,
                exch_pnl: 0.0,
                order_info: Some(OrderInfo {
                    exch_order_ref: fact.exch_order_ref.clone(),
                    client_order_id: fact.order_id.to_string(),
                    instrument: fact.instrument.clone(),
                    exch_account_id: String::new(),
                    buy_sell_type: fact.buysell_type,
                    place_qty: 0.0,
                    place_price: 0.0,
                }),
            })),
        },
    ];

    OrderReport {
        exch_order_ref: fact.exch_order_ref.clone(),
        order_id: fact.order_id,
        exchange: gw_id.to_string(),
        order_report_entries: entries,
        account_id,
        update_timestamp: fact.timestamp,
        extra_order_id: String::new(),
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        gw_received_at_ns: 0,
    }
}
