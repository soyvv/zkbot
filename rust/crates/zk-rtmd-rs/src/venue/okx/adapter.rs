use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tracing::{debug, error, info, warn};

use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};
use zk_venue_okx::config::OkxConfig;
use zk_venue_okx::rest::OkxRestClient;
use zk_venue_okx::rtmd_normalize;

use crate::types::{ChannelType, RtmdError, RtmdEvent, RtmdSubscriptionSpec, StreamKey};
use crate::venue_adapter::RtmdVenueAdapter;

// ── Internal types ──────────────────────────────────────────────────────────

/// Commands sent from the adapter to the WS supervisor task.
enum WsCommand {
    Subscribe {
        channel: String,
        inst_id: String,
    },
    Unsubscribe {
        channel: String,
        inst_id: String,
    },
}

/// Metadata tracked per active subscription.
#[derive(Clone, Debug)]
struct SubMeta {
    spec: RtmdSubscriptionSpec,
    okx_channel: String,
    inst_id: String,
}

enum SessionOutcome {
    Shutdown,
    Failed { established: bool, error: String },
}

/// Maximum supported orderbook depth for the `books5` channel.
const MAX_BOOKS5_DEPTH: u32 = 5;

// ── Adapter ─────────────────────────────────────────────────────────────────

pub struct OkxRtmdAdapter {
    config: OkxConfig,
    rest_client: OkxRestClient,
    event_tx: mpsc::Sender<RtmdEvent>,
    event_rx: Mutex<mpsc::Receiver<RtmdEvent>>,
    cmd_tx: mpsc::Sender<WsCommand>,
    cmd_rx: Mutex<Option<mpsc::Receiver<WsCommand>>>,
    shutdown_tx: watch::Sender<bool>,
    /// Active subscriptions — shared with WS task for re-subscribe on reconnect.
    active_subs: Arc<Mutex<HashMap<StreamKey, SubMeta>>>,
    /// Reverse lookup: (okx_channel, inst_id) → instrument_code.
    reverse_map: Arc<DashMap<(String, String), String>>,
    /// instrument_code → instrument_exch.
    exch_map: Arc<DashMap<String, String>>,
}

impl OkxRtmdAdapter {
    pub fn new(config: OkxConfig) -> Self {
        let (event_tx, event_rx) = mpsc::channel(4096);
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let rest_client = OkxRestClient::new(config.clone());

        Self {
            config,
            rest_client,
            event_tx,
            event_rx: Mutex::new(event_rx),
            cmd_tx,
            cmd_rx: Mutex::new(Some(cmd_rx)),
            shutdown_tx,
            active_subs: Arc::new(Mutex::new(HashMap::new())),
            reverse_map: Arc::new(DashMap::new()),
            exch_map: Arc::new(DashMap::new()),
        }
    }

    /// Resolve a subscription spec to (okx_channel, inst_id).
    /// Returns an error if the requested depth is not supported.
    fn resolve_okx_sub(spec: &RtmdSubscriptionSpec) -> Result<(String, String), RtmdError> {
        let inst_id = spec.instrument_exch.clone();
        let channel = match &spec.stream_key.channel {
            ChannelType::Tick => "tickers".to_string(),
            ChannelType::OrderBook { depth } => {
                if let Some(d) = depth {
                    if *d > MAX_BOOKS5_DEPTH {
                        return Err(RtmdError::Subscription(format!(
                            "OKX adapter only supports orderbook depth <= {MAX_BOOKS5_DEPTH}, \
                             requested {d}. Deeper books require checksum verification \
                             (not yet implemented)."
                        )));
                    }
                }
                "books5".to_string()
            }
            ChannelType::Kline { interval } => rtmd_normalize::okx_candle_channel(interval),
            ChannelType::Funding => "funding-rate".to_string(),
        };
        Ok((channel, inst_id))
    }
}

#[async_trait]
impl RtmdVenueAdapter for OkxRtmdAdapter {
    async fn connect(&self) -> crate::venue_adapter::Result<()> {
        let cmd_rx = self
            .cmd_rx
            .lock()
            .await
            .take()
            .ok_or_else(|| RtmdError::Internal("already connected".into()))?;

        let ws_url = self.config.ws_public_url().to_string();
        let event_tx = self.event_tx.clone();
        let shutdown_rx = self.shutdown_tx.subscribe();
        let active_subs = Arc::clone(&self.active_subs);
        let reverse_map = Arc::clone(&self.reverse_map);

        // Readiness gate: supervisor signals once the first WS session connects.
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();

        tokio::spawn(async move {
            ws_supervisor_loop(
                ws_url,
                event_tx,
                cmd_rx,
                shutdown_rx,
                active_subs,
                reverse_map,
                Some(ready_tx),
            )
            .await;
        });

        // Block until the first WS session establishes (with timeout).
        match tokio::time::timeout(Duration::from_secs(15), ready_rx).await {
            Ok(Ok(Ok(()))) => {
                info!("OKX RTMD adapter connected (public WS session established)");
                Ok(())
            }
            Ok(Ok(Err(e))) => Err(RtmdError::Venue(format!(
                "OKX public WS initial connect failed: {e}"
            ))),
            Ok(Err(_)) => Err(RtmdError::Venue(
                "OKX public WS supervisor exited before signaling readiness".into(),
            )),
            Err(_) => Err(RtmdError::Venue(
                "OKX public WS connect timed out after 15s".into(),
            )),
        }
    }

    async fn subscribe(&self, spec: RtmdSubscriptionSpec) -> crate::venue_adapter::Result<()> {
        let (okx_channel, inst_id) = Self::resolve_okx_sub(&spec)?;

        // Update maps
        self.exch_map
            .insert(spec.stream_key.instrument_code.clone(), spec.instrument_exch.clone());
        self.reverse_map.insert(
            (okx_channel.clone(), inst_id.clone()),
            spec.stream_key.instrument_code.clone(),
        );

        let meta = SubMeta {
            spec: spec.clone(),
            okx_channel: okx_channel.clone(),
            inst_id: inst_id.clone(),
        };
        self.active_subs
            .lock()
            .await
            .insert(spec.stream_key.clone(), meta);

        self.cmd_tx
            .send(WsCommand::Subscribe {
                channel: okx_channel,
                inst_id,
            })
            .await
            .map_err(|e| RtmdError::Internal(format!("cmd send failed: {e}")))?;

        Ok(())
    }

    async fn unsubscribe(&self, spec: RtmdSubscriptionSpec) -> crate::venue_adapter::Result<()> {
        let (okx_channel, inst_id) = Self::resolve_okx_sub(&spec)?;

        self.active_subs.lock().await.remove(&spec.stream_key);
        self.reverse_map
            .remove(&(okx_channel.clone(), inst_id.clone()));

        self.cmd_tx
            .send(WsCommand::Unsubscribe {
                channel: okx_channel,
                inst_id,
            })
            .await
            .map_err(|e| RtmdError::Internal(format!("cmd send failed: {e}")))?;

        Ok(())
    }

    async fn snapshot_active(&self) -> crate::venue_adapter::Result<Vec<RtmdSubscriptionSpec>> {
        let subs = self.active_subs.lock().await;
        Ok(subs.values().map(|m| m.spec.clone()).collect())
    }

    async fn next_event(&self) -> crate::venue_adapter::Result<RtmdEvent> {
        self.event_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or(RtmdError::ChannelClosed)
    }

    fn instrument_exch_for(&self, instrument_code: &str) -> Option<String> {
        self.exch_map.get(instrument_code).map(|e| e.clone())
    }

    async fn query_current_tick(
        &self,
        instrument_code: &str,
    ) -> crate::venue_adapter::Result<TickData> {
        let inst_id = self
            .instrument_exch_for(instrument_code)
            .unwrap_or_else(|| instrument_code.to_string());
        let raw = self
            .rest_client
            .query_ticker(&inst_id)
            .await
            .map_err(|e| RtmdError::Venue(e.to_string()))?;
        Ok(rtmd_normalize::normalize_rest_ticker(&raw, instrument_code))
    }

    async fn query_current_orderbook(
        &self,
        instrument_code: &str,
        depth: Option<u32>,
    ) -> crate::venue_adapter::Result<OrderBook> {
        let inst_id = self
            .instrument_exch_for(instrument_code)
            .unwrap_or_else(|| instrument_code.to_string());
        let raw = self
            .rest_client
            .query_orderbook(&inst_id, depth)
            .await
            .map_err(|e| RtmdError::Venue(e.to_string()))?;
        rtmd_normalize::normalize_ws_orderbook(&raw, instrument_code, "okx")
            .ok_or_else(|| RtmdError::Venue("failed to parse orderbook".into()))
    }

    async fn query_current_funding(
        &self,
        instrument_code: &str,
    ) -> crate::venue_adapter::Result<FundingRate> {
        let inst_id = self
            .instrument_exch_for(instrument_code)
            .unwrap_or_else(|| instrument_code.to_string());
        let raw = self
            .rest_client
            .query_funding_rate(&inst_id)
            .await
            .map_err(|e| RtmdError::Venue(e.to_string()))?;
        Ok(rtmd_normalize::normalize_rest_funding(&raw, instrument_code))
    }

    async fn query_klines(
        &self,
        instrument_code: &str,
        interval: &str,
        limit: u32,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> crate::venue_adapter::Result<Vec<Kline>> {
        let inst_id = self
            .instrument_exch_for(instrument_code)
            .unwrap_or_else(|| instrument_code.to_string());
        let raw = self
            .rest_client
            .query_candles(&inst_id, interval, Some(limit), from_ms, to_ms)
            .await
            .map_err(|e| RtmdError::Venue(e.to_string()))?;

        let klines: Vec<Kline> = raw
            .iter()
            .filter_map(|item| {
                rtmd_normalize::normalize_ws_kline(item, instrument_code, interval, "okx")
            })
            .collect();
        Ok(klines)
    }
}

impl Drop for OkxRtmdAdapter {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ── WebSocket supervisor ────────────────────────────────────────────────────

async fn ws_supervisor_loop(
    ws_url: String,
    event_tx: mpsc::Sender<RtmdEvent>,
    mut cmd_rx: mpsc::Receiver<WsCommand>,
    mut shutdown_rx: watch::Receiver<bool>,
    active_subs: Arc<Mutex<HashMap<StreamKey, SubMeta>>>,
    reverse_map: Arc<DashMap<(String, String), String>>,
    mut ready_tx: Option<oneshot::Sender<Result<(), String>>>,
) {
    let mut backoff_ms: u64 = 1000;
    let max_backoff_ms: u64 = 30_000;

    loop {
        if *shutdown_rx.borrow() {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(Err("shutdown before connect".into()));
            }
            break;
        }

        match run_ws_session(
            &ws_url,
            &event_tx,
            &mut cmd_rx,
            &mut shutdown_rx,
            &active_subs,
            &reverse_map,
        )
        .await
        {
            SessionOutcome::Shutdown => {
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(Err("shutdown before connect".into()));
                }
                break;
            }
            SessionOutcome::Failed { established, error } => {
                error!(error = %error, "OKX public WS session failed");

                if established {
                    // Session connected then dropped — signal readiness (first time)
                    // and reset backoff.
                    if let Some(tx) = ready_tx.take() {
                        let _ = tx.send(Ok(()));
                    }
                    backoff_ms = 1000;
                } else if let Some(tx) = ready_tx.take() {
                    // First connect attempt failed — tell connect() caller.
                    let _ = tx.send(Err(error.clone()));
                }

                info!(backoff_ms, "reconnecting after backoff");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
            }
        }
    }
    info!("OKX public WS supervisor exiting");
}

async fn run_ws_session(
    ws_url: &str,
    event_tx: &mpsc::Sender<RtmdEvent>,
    cmd_rx: &mut mpsc::Receiver<WsCommand>,
    shutdown_rx: &mut watch::Receiver<bool>,
    active_subs: &Arc<Mutex<HashMap<StreamKey, SubMeta>>>,
    reverse_map: &Arc<DashMap<(String, String), String>>,
) -> SessionOutcome {
    // 1. Connect
    let (ws_stream, _) = match tokio_tungstenite::connect_async(ws_url).await {
        Ok(pair) => pair,
        Err(e) => {
            return SessionOutcome::Failed {
                established: false,
                error: format!("WS connect failed: {e}"),
            };
        }
    };

    info!(url = ws_url, "OKX public WS connected");
    let (mut write, mut read) = ws_stream.split();
    let established = true;

    // 2. Re-subscribe all active subscriptions (handles reconnect)
    {
        let subs = active_subs.lock().await;
        for meta in subs.values() {
            let msg = build_subscribe_msg(&meta.okx_channel, &meta.inst_id);
            if let Err(e) = write
                .send(tokio_tungstenite::tungstenite::Message::Text(msg.into()))
                .await
            {
                return SessionOutcome::Failed {
                    established: false,
                    error: format!("re-subscribe send failed: {e}"),
                };
            }
        }
        if !subs.is_empty() {
            debug!(count = subs.len(), "re-subscribed active channels");
        }
    }

    // 3. Read loop
    let mut ping_interval = tokio::time::interval(Duration::from_secs(25));
    ping_interval.reset(); // don't fire immediately

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        let text_str: &str = &text;
                        if text_str == "pong" {
                            continue;
                        }
                        handle_ws_message(text_str, event_tx, reverse_map).await;
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        let _ = write.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {} // Binary, Pong, Close frames
                    Some(Err(e)) => {
                        return SessionOutcome::Failed {
                            established,
                            error: format!("WS read error: {e}"),
                        };
                    }
                    None => {
                        return SessionOutcome::Failed {
                            established,
                            error: "WS stream ended".into(),
                        };
                    }
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(WsCommand::Subscribe { channel, inst_id }) => {
                        let msg = build_subscribe_msg(&channel, &inst_id);
                        debug!(channel = %channel, inst_id = %inst_id, "subscribing");
                        if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await {
                            return SessionOutcome::Failed {
                                established,
                                error: format!("subscribe send failed: {e}"),
                            };
                        }
                    }
                    Some(WsCommand::Unsubscribe { channel, inst_id }) => {
                        let msg = build_unsubscribe_msg(&channel, &inst_id);
                        debug!(channel = %channel, inst_id = %inst_id, "unsubscribing");
                        if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await {
                            warn!(error = %e, "unsubscribe send failed");
                        }
                    }
                    None => {
                        return SessionOutcome::Shutdown;
                    }
                }
            }

            _ = ping_interval.tick() => {
                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text("ping".into())).await {
                    return SessionOutcome::Failed {
                        established,
                        error: format!("ping send failed: {e}"),
                    };
                }
            }

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return SessionOutcome::Shutdown;
                }
            }
        }
    }
}

async fn handle_ws_message(
    text: &str,
    event_tx: &mpsc::Sender<RtmdEvent>,
    reverse_map: &Arc<DashMap<(String, String), String>>,
) {
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to parse WS message");
            return;
        }
    };

    // Subscription confirmations: {"event": "subscribe", "arg": {...}}
    if v.get("event").is_some() {
        let event = v["event"].as_str().unwrap_or("");
        match event {
            "subscribe" => {
                debug!(arg = %v["arg"], "subscription confirmed");
            }
            "unsubscribe" => {
                debug!(arg = %v["arg"], "unsubscription confirmed");
            }
            "error" => {
                warn!(code = %v["code"], msg = %v["msg"], "WS error event");
            }
            _ => {}
        }
        return;
    }

    // Data push: {"arg": {"channel": "...", "instId": "..."}, "data": [...]}
    let channel = match v["arg"]["channel"].as_str() {
        Some(ch) => ch,
        None => return,
    };
    let inst_id = match v["arg"]["instId"].as_str() {
        Some(id) => id,
        None => return,
    };

    // Look up instrument_code
    let instrument_code = match reverse_map.get(&(channel.to_string(), inst_id.to_string())) {
        Some(entry) => entry.value().clone(),
        None => {
            // Fallback: use inst_id as instrument_code.
            inst_id.to_string()
        }
    };

    let data_arr = match v["data"].as_array() {
        Some(arr) => arr,
        None => return,
    };

    match channel {
        "tickers" => {
            for item in data_arr {
                if let Some(tick) =
                    rtmd_normalize::normalize_ws_ticker(item, &instrument_code, "okx")
                {
                    let _ = event_tx.send(RtmdEvent::Tick(tick)).await;
                }
            }
        }
        "books5" | "books" | "books-l2-tbt" => {
            for item in data_arr {
                if let Some(ob) =
                    rtmd_normalize::normalize_ws_orderbook(item, &instrument_code, "okx")
                {
                    let _ = event_tx.send(RtmdEvent::OrderBook(ob)).await;
                }
            }
        }
        "funding-rate" => {
            for item in data_arr {
                if let Some(fr) =
                    rtmd_normalize::normalize_ws_funding(item, &instrument_code, "okx")
                {
                    let _ = event_tx.send(RtmdEvent::Funding(fr)).await;
                }
            }
        }
        ch if ch.starts_with("candle") => {
            let interval = &ch["candle".len()..];
            for item in data_arr {
                if let Some(kline) =
                    rtmd_normalize::normalize_ws_kline(item, &instrument_code, interval, "okx")
                {
                    let _ = event_tx.send(RtmdEvent::Kline(kline)).await;
                }
            }
        }
        _ => {
            debug!(channel, "unhandled WS channel");
        }
    }
}

fn build_subscribe_msg(channel: &str, inst_id: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [{"channel": channel, "instId": inst_id}]
    })
    .to_string()
}

fn build_unsubscribe_msg(channel: &str, inst_id: &str) -> String {
    serde_json::json!({
        "op": "unsubscribe",
        "args": [{"channel": channel, "instId": inst_id}]
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_okx_sub_tick() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "BTC-USDT".into(),
                channel: ChannelType::Tick,
            },
            instrument_exch: "BTC-USDT".into(),
            venue: "okx".into(),
        };
        let (ch, id) = OkxRtmdAdapter::resolve_okx_sub(&spec).unwrap();
        assert_eq!(ch, "tickers");
        assert_eq!(id, "BTC-USDT");
    }

    #[test]
    fn test_resolve_okx_sub_orderbook_5() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "ETH-USDT".into(),
                channel: ChannelType::OrderBook { depth: Some(5) },
            },
            instrument_exch: "ETH-USDT".into(),
            venue: "okx".into(),
        };
        let (ch, _) = OkxRtmdAdapter::resolve_okx_sub(&spec).unwrap();
        assert_eq!(ch, "books5");
    }

    #[test]
    fn test_resolve_okx_sub_orderbook_deep_rejected() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "ETH-USDT".into(),
                channel: ChannelType::OrderBook { depth: Some(20) },
            },
            instrument_exch: "ETH-USDT".into(),
            venue: "okx".into(),
        };
        let result = OkxRtmdAdapter::resolve_okx_sub(&spec);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("depth"), "error should mention depth: {err}");
    }

    #[test]
    fn test_resolve_okx_sub_orderbook_none_depth() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "ETH-USDT".into(),
                channel: ChannelType::OrderBook { depth: None },
            },
            instrument_exch: "ETH-USDT".into(),
            venue: "okx".into(),
        };
        let (ch, _) = OkxRtmdAdapter::resolve_okx_sub(&spec).unwrap();
        assert_eq!(ch, "books5");
    }

    #[test]
    fn test_resolve_okx_sub_kline() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "BTC-USDT".into(),
                channel: ChannelType::Kline {
                    interval: "1H".into(),
                },
            },
            instrument_exch: "BTC-USDT".into(),
            venue: "okx".into(),
        };
        let (ch, _) = OkxRtmdAdapter::resolve_okx_sub(&spec).unwrap();
        assert_eq!(ch, "candle1H");
    }

    #[test]
    fn test_resolve_okx_sub_funding() {
        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "BTC-USDT-SWAP".into(),
                channel: ChannelType::Funding,
            },
            instrument_exch: "BTC-USDT-SWAP".into(),
            venue: "okx".into(),
        };
        let (ch, _) = OkxRtmdAdapter::resolve_okx_sub(&spec).unwrap();
        assert_eq!(ch, "funding-rate");
    }

    #[test]
    fn test_build_subscribe_msg() {
        let msg = build_subscribe_msg("tickers", "BTC-USDT");
        let v: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["op"], "subscribe");
        assert_eq!(v["args"][0]["channel"], "tickers");
        assert_eq!(v["args"][0]["instId"], "BTC-USDT");
    }
}
