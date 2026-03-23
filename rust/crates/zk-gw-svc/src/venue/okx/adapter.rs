use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::info;

use zk_venue_okx::config::OkxConfig;
use zk_venue_okx::normalize::{self, OkxIdMap};
use zk_venue_okx::rest::OkxRestClient;
use zk_venue_okx::ws::{OkxWsSupervisor, WsEvent};

use crate::venue_adapter::*;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub struct OkxVenueAdapter {
    config: OkxConfig,
    rest_client: Arc<OkxRestClient>,
    id_map: Arc<OkxIdMap>,
    event_tx: mpsc::Sender<VenueEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<VenueEvent>>>,
    ws_shutdown_tx: watch::Sender<bool>,
    gw_id: String,
    account_id: i64,
}

impl OkxVenueAdapter {
    pub fn new(config: OkxConfig, gw_id: String, account_id: i64) -> Self {
        let (event_tx, event_rx) = mpsc::channel(2048);
        let (ws_shutdown_tx, _ws_shutdown_rx) = watch::channel(false);

        Self {
            rest_client: Arc::new(OkxRestClient::new(config.clone())),
            id_map: Arc::new(OkxIdMap::new()),
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            ws_shutdown_tx,
            config,
            gw_id,
            account_id,
        }
    }

    /// Spawn the WS-to-VenueEvent bridge task.
    fn spawn_ws_bridge(&self, mut ws_rx: mpsc::Receiver<WsEvent>) {
        let event_tx = self.event_tx.clone();
        let account_id = self.account_id;

        tokio::spawn(async move {
            while let Some(ws_event) = ws_rx.recv().await {
                let venue_event = match ws_event {
                    WsEvent::OrderReport(report) => VenueEvent::OrderReport(report),
                    WsEvent::Balance(facts) => VenueEvent::Balance(
                        facts
                            .into_iter()
                            .map(|f| VenueBalanceFact {
                                asset: f.asset,
                                total_qty: f.total_qty,
                                avail_qty: f.avail_qty,
                                frozen_qty: f.frozen_qty,
                            })
                            .collect(),
                    ),
                    WsEvent::Position(facts) => VenueEvent::Position(
                        facts
                            .into_iter()
                            .map(|f| VenuePositionFact {
                                instrument: f.instrument,
                                long_short_type: f.long_short_type,
                                qty: f.qty,
                                avail_qty: f.avail_qty,
                                frozen_qty: 0.0,
                                account_id,
                            })
                            .collect(),
                    ),
                    WsEvent::Connected => VenueEvent::System(VenueSystemEvent {
                        event_type: VenueSystemEventType::Connected,
                        message: "OKX WS connected".to_string(),
                        timestamp: now_ms(),
                    }),
                    WsEvent::Disconnected => VenueEvent::System(VenueSystemEvent {
                        event_type: VenueSystemEventType::Disconnected,
                        message: "OKX WS disconnected".to_string(),
                        timestamp: now_ms(),
                    }),
                };
                if event_tx.send(venue_event).await.is_err() {
                    break;
                }
            }
        });
    }
}

#[async_trait]
impl VenueAdapter for OkxVenueAdapter {
    async fn connect(&self) -> anyhow::Result<()> {
        info!("OKX venue adapter connecting");

        // Create readiness channel — WS supervisor signals after login+subscribe.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Spawn WS supervisor.
        let (ws_tx, ws_rx) = mpsc::channel(2048);
        let supervisor = OkxWsSupervisor {
            config: self.config.clone(),
            event_tx: ws_tx,
            rest_client: Arc::clone(&self.rest_client),
            id_map: Arc::clone(&self.id_map),
            shutdown_rx: self.ws_shutdown_tx.subscribe(),
            gw_id: self.gw_id.clone(),
            account_id: self.account_id,
            ready_tx: Some(ready_tx),
        };
        tokio::spawn(supervisor.run());
        self.spawn_ws_bridge(ws_rx);

        // Wait for WS login+subscribe to succeed before declaring connected.
        match ready_rx.await {
            Ok(Ok(())) => {
                info!("OKX WS login+subscribe confirmed");
            }
            Ok(Err(e)) => {
                return Err(anyhow::anyhow!("OKX WS connect failed: {e}"));
            }
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "OKX WS supervisor exited before signaling readiness"
                ));
            }
        }

        // Initial balance query.
        match self.rest_client.query_balance(None).await {
            Ok(details) => {
                let facts: Vec<VenueBalanceFact> = details
                    .iter()
                    .map(|d| {
                        let f = normalize::normalize_rest_balance(d);
                        VenueBalanceFact {
                            asset: f.asset,
                            total_qty: f.total_qty,
                            avail_qty: f.avail_qty,
                            frozen_qty: f.frozen_qty,
                        }
                    })
                    .collect();
                if !facts.is_empty() {
                    let _ = self.event_tx.send(VenueEvent::Balance(facts)).await;
                }
                info!(count = details.len(), "OKX initial balance loaded");
            }
            Err(e) => {
                tracing::warn!(error = %e, "OKX initial balance query failed (non-fatal)");
            }
        }

        info!("OKX venue adapter connected");
        Ok(())
    }

    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck> {
        let cl_ord_id = req.correlation_id.to_string();
        let inst_id = &req.instrument;

        let side = if req.buysell_type == zk_proto_rs::zk::common::v1::BuySellType::BsBuy as i32 {
            "buy"
        } else {
            "sell"
        };

        let ord_type = if req.order_type
            == zk_proto_rs::zk::common::v1::BasicOrderType::OrdertypeLimit as i32
        {
            "limit"
        } else {
            "market"
        };

        let sz = format!("{}", req.qty);
        let px = if ord_type == "limit" {
            Some(format!("{}", req.price))
        } else {
            None
        };

        let td_mode = "cash";

        match self
            .rest_client
            .place_order(
                inst_id,
                side,
                ord_type,
                &sz,
                px.as_deref(),
                &cl_ord_id,
                td_mode,
            )
            .await
        {
            Ok(data) => {
                self.id_map.record(&cl_ord_id, &data.ord_id, inst_id);
                Ok(VenueCommandAck {
                    success: true,
                    exch_order_ref: Some(data.ord_id),
                    error_message: None,
                })
            }
            Err(e) => Ok(VenueCommandAck {
                success: false,
                exch_order_ref: None,
                error_message: Some(e.to_string()),
            }),
        }
    }

    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck> {
        let ord_id = &req.exch_order_ref;

        let inst_id = self
            .id_map
            .ord_to_inst
            .get(ord_id)
            .map(|r| r.value().clone())
            .ok_or_else(|| anyhow::anyhow!("cannot cancel: instId not found for ordId={ord_id}"))?;

        match self.rest_client.cancel_order(&inst_id, ord_id).await {
            Ok(_data) => Ok(VenueCommandAck {
                success: true,
                exch_order_ref: Some(ord_id.to_string()),
                error_message: None,
            }),
            Err(e) => Ok(VenueCommandAck {
                success: false,
                exch_order_ref: Some(ord_id.to_string()),
                error_message: Some(e.to_string()),
            }),
        }
    }

    async fn query_balance(
        &self,
        _req: VenueBalanceQuery,
    ) -> anyhow::Result<Vec<VenueBalanceFact>> {
        let details = self.rest_client.query_balance(None).await?;
        Ok(details
            .iter()
            .map(|d| {
                let f = normalize::normalize_rest_balance(d);
                VenueBalanceFact {
                    asset: f.asset,
                    total_qty: f.total_qty,
                    avail_qty: f.avail_qty,
                    frozen_qty: f.frozen_qty,
                }
            })
            .collect())
    }

    async fn query_order(&self, req: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>> {
        // OKX requires instId for single-order queries.
        // Try: (1) id_map cache, (2) request.instrument field, (3) give up.
        let inst_id = req
            .exch_order_ref
            .as_ref()
            .and_then(|oid| self.id_map.ord_to_inst.get(oid).map(|r| r.value().clone()))
            .or_else(|| req.instrument.clone());

        let inst_id_str = match inst_id {
            Some(id) if !id.is_empty() => id,
            _ => {
                return Err(anyhow::anyhow!(
                    "query_order requires instrument or a cached ordId→instId mapping"
                ));
            }
        };

        let data = self
            .rest_client
            .query_order(
                &inst_id_str,
                req.exch_order_ref.as_deref(),
                req.order_id.map(|id| id.to_string()).as_deref(),
            )
            .await?;

        Ok(data
            .iter()
            .map(|d| {
                let f = normalize::normalize_rest_order(d, &self.id_map);
                VenueOrderFact {
                    order_id: f.order_id,
                    exch_order_ref: f.exch_order_ref,
                    instrument: f.instrument,
                    status: match f.status {
                        normalize::ExchangeOrderStatus::ExchOrderStatusBooked => {
                            VenueOrderStatus::Booked
                        }
                        normalize::ExchangeOrderStatus::ExchOrderStatusPartialFilled => {
                            VenueOrderStatus::PartiallyFilled
                        }
                        normalize::ExchangeOrderStatus::ExchOrderStatusFilled => {
                            VenueOrderStatus::Filled
                        }
                        normalize::ExchangeOrderStatus::ExchOrderStatusCancelled => {
                            VenueOrderStatus::Cancelled
                        }
                        normalize::ExchangeOrderStatus::ExchOrderStatusExchRejected => {
                            VenueOrderStatus::Rejected
                        }
                        _ => VenueOrderStatus::Booked,
                    },
                    filled_qty: f.filled_qty,
                    unfilled_qty: f.unfilled_qty,
                    avg_price: f.avg_price,
                    timestamp: f.timestamp,
                }
            })
            .collect())
    }

    async fn query_open_orders(
        &self,
        _req: VenueOpenOrdersQuery,
    ) -> anyhow::Result<Vec<VenueOrderFact>> {
        let data = self.rest_client.query_open_orders(None).await?;
        Ok(data
            .iter()
            .map(|d| {
                let f = normalize::normalize_rest_order(d, &self.id_map);
                VenueOrderFact {
                    order_id: f.order_id,
                    exch_order_ref: f.exch_order_ref,
                    instrument: f.instrument,
                    status: VenueOrderStatus::Booked,
                    filled_qty: f.filled_qty,
                    unfilled_qty: f.unfilled_qty,
                    avg_price: f.avg_price,
                    timestamp: f.timestamp,
                }
            })
            .collect())
    }

    async fn query_trades(&self, _req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>> {
        let data = self.rest_client.query_trades(None, None).await?;
        Ok(data
            .iter()
            .map(|d| {
                let f = normalize::normalize_rest_trade(d, &self.id_map);
                VenueTradeFact {
                    exch_trade_id: f.exch_trade_id,
                    order_id: f.order_id,
                    exch_order_ref: f.exch_order_ref,
                    instrument: f.instrument,
                    buysell_type: f.buysell_type,
                    filled_qty: f.filled_qty,
                    filled_price: f.filled_price,
                    timestamp: f.timestamp,
                }
            })
            .collect())
    }

    async fn query_positions(
        &self,
        _req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>> {
        let data = self.rest_client.query_positions(None).await?;
        Ok(data
            .iter()
            .map(|d| {
                let f = normalize::normalize_rest_position(d);
                VenuePositionFact {
                    instrument: f.instrument,
                    long_short_type: f.long_short_type,
                    qty: f.qty,
                    avail_qty: f.avail_qty,
                    frozen_qty: 0.0,
                    account_id: self.account_id,
                }
            })
            .collect())
    }

    async fn next_event(&self) -> anyhow::Result<VenueEvent> {
        let mut rx = self.event_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("OKX event channel closed"))
    }
}
