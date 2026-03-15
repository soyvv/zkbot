//! TradingClient — top-level entry point.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_nats::jetstream;
use zk_proto_rs::zk::common::v1::{
    AuditMeta, BasicOrderType, BuySellType, OpenCloseType, TimeInForceType,
};
use zk_proto_rs::zk::oms::v1::{
    Balance as ProtoBalance, BalanceUpdateEvent, QueryBalancesRequest,
    oms_response, BatchCancelOrdersRequest, BatchPlaceOrdersRequest, CancelOrderRequest,
    Order as ProtoOrder, OrderCancelRequest, OrderRequest, OrderUpdateEvent as ProtoOrderUpdateEvent,
    PlaceOrderRequest, Position as ProtoPosition, PositionUpdateEvent as ProtoPositionUpdateEvent,
    QueryOpenOrderRequest, QueryPositionRequest,
};
use zk_proto_rs::zk::rtmd::v1::{
    FundingRate as ProtoFundingRate, Kline as ProtoKline, OrderBook as ProtoOrderBook,
    TickData as ProtoTickData,
};

use crate::config::TradingClientConfig;
use crate::discovery::{OmsDiscovery, OmsEndpoint};
use crate::error::SdkError;
use crate::id_gen::SnowflakeIdGen;
use crate::model::{CommandAck, OrderType, Side, TradingCancel, TradingOrder};
use crate::oms::OmsChannelPool;
use crate::refdata::RefdataSdk;
use crate::rtmd_sub::{funding_topic, kline_topic, orderbook_topic, tick_topic};
use crate::stream::{
    balance_update_topic, order_update_topic, position_update_topic, spawn_protobuf_subscription,
};

/// The main trading client.
pub struct TradingClient {
    config: TradingClientConfig,
    nats: async_nats::Client,
    discovery: OmsDiscovery,
    oms_pool: OmsChannelPool,
    refdata: RefdataSdk,
    id_gen: Arc<SnowflakeIdGen>,
}

impl TradingClient {
    pub async fn from_env() -> Result<Self, SdkError> {
        Self::from_config(TradingClientConfig::from_env()?).await
    }

    pub async fn from_config(config: TradingClientConfig) -> Result<Self, SdkError> {
        if config.client_instance_id > 1023 {
            return Err(SdkError::InstanceIdOutOfRange(config.client_instance_id));
        }

        let nats = async_nats::connect(config.nats_url.clone())
            .await
            .map_err(|e| SdkError::Config(format!("NATS connect error: {e}")))?;
        let js = jetstream::new(nats.clone());
        let (discovery, snapshot_ready) = OmsDiscovery::start(&js, &config).await?;

        if !snapshot_ready && !config.account_ids.is_empty() {
            return Err(SdkError::Config(
                "discovery timed out: no service registry entries found within 2s; \
                 cannot resolve configured account_ids"
                    .into(),
            ));
        }

        let refdata_endpoint = match config.refdata_grpc.clone() {
            Some(endpoint) => endpoint,
            None => discovery
                .resolve_refdata()
                .await
                .ok_or(SdkError::RefdataServiceNotFound)?,
        };
        let refdata = RefdataSdk::start(nats.clone(), refdata_endpoint).await?;
        let oms_pool = OmsChannelPool::new();
        let id_gen = Arc::new(SnowflakeIdGen::new(config.client_instance_id)?);

        let client = Self { config, nats, discovery, oms_pool, refdata, id_gen };
        client.preconnect_oms().await?;
        Ok(client)
    }

    pub fn refdata(&self) -> &RefdataSdk {
        &self.refdata
    }

    pub fn next_order_id(&self) -> i64 {
        self.id_gen.next_id()
    }

    pub async fn place_order(&self, account_id: i64, mut order: TradingOrder) -> Result<CommandAck, SdkError> {
        if order.order_id == 0 {
            order.order_id = self.next_order_id();
        }
        let mut client = self.oms_client(account_id).await?;
        let response = client
            .place_order(PlaceOrderRequest {
                order_request: Some(map_order_request(account_id, &order)),
                audit_meta: Some(self.audit_meta(&order.source_id)),
                idempotency_key: format!("{}:{}:{}", account_id, order.source_id, order.order_id),
            })
            .await?
            .into_inner();
        Ok(map_command_ack(response))
    }

    pub async fn cancel_order(&self, account_id: i64, cancel: TradingCancel) -> Result<CommandAck, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let response = client
            .cancel_order(CancelOrderRequest {
                order_cancel_request: Some(map_cancel_request(&cancel)),
                audit_meta: Some(self.audit_meta(&cancel.source_id)),
                idempotency_key: format!("{}:{}:{}", account_id, cancel.source_id, cancel.order_id),
            })
            .await?
            .into_inner();
        Ok(map_command_ack(response))
    }

    pub async fn batch_place_orders(&self, account_id: i64, orders: Vec<TradingOrder>) -> Result<CommandAck, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let source_id = orders.first().map(|o| o.source_id.clone()).unwrap_or_else(|| "sdk".to_string());
        let requests = orders
            .into_iter()
            .map(|mut order| {
                if order.order_id == 0 {
                    order.order_id = self.next_order_id();
                }
                map_order_request(account_id, &order)
            })
            .collect();
        let response = client
            .batch_place_orders(BatchPlaceOrdersRequest {
                order_requests: requests,
                audit_meta: Some(self.audit_meta(&source_id)),
            })
            .await?
            .into_inner();
        Ok(map_command_ack(response))
    }

    pub async fn batch_cancel_orders(&self, account_id: i64, cancels: Vec<TradingCancel>) -> Result<CommandAck, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let source_id = cancels.first().map(|o| o.source_id.clone()).unwrap_or_else(|| "sdk".to_string());
        let response = client
            .batch_cancel_orders(BatchCancelOrdersRequest {
                order_cancel_requests: cancels.iter().map(map_cancel_request).collect(),
                audit_meta: Some(self.audit_meta(&source_id)),
            })
            .await?
            .into_inner();
        Ok(map_command_ack(response))
    }

    pub async fn query_open_orders(&self, account_id: i64) -> Result<Vec<ProtoOrder>, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let response = client
            .query_open_orders(QueryOpenOrderRequest {
                account_id,
                query_gw: false,
                pagination: None,
            })
            .await?
            .into_inner();
        Ok(response.orders)
    }

    pub async fn query_positions(&self, account_id: i64) -> Result<Vec<ProtoPosition>, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let response = client
            .query_position(QueryPositionRequest {
                account_id,
                query_gw: false,
            })
            .await?
            .into_inner();
        Ok(response.positions)
    }

    pub async fn query_balances(&self, account_id: i64) -> Result<Vec<ProtoBalance>, SdkError> {
        let mut client = self.oms_client(account_id).await?;
        let response = client
            .query_balances(QueryBalancesRequest {
                account_id,
                query_gw: false,
            })
            .await?
            .into_inner();
        Ok(response.balances)
    }

    pub async fn subscribe_order_updates<F>(&self, handler: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(ProtoOrderUpdateEvent) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        let subjects = self.order_update_subjects().await;
        let nc = self.nats.clone();

        tokio::spawn(async move {
            let mut handles = Vec::new();
            for subject in subjects {
                handles.push(spawn_protobuf_subscription(
                    nc.clone(),
                    subject,
                    std::convert::identity,
                    Arc::clone(&handler),
                ));
            }
            for handle in handles {
                let _ = handle.await;
            }
        })
    }

    /// Subscribe to balance updates from OMS instances.
    pub async fn subscribe_balance_updates<F>(&self, asset: &str, handler: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(BalanceUpdateEvent) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        let nc = self.nats.clone();
        let subjects = self.balance_update_subjects(asset).await;
        tokio::spawn(async move {
            let mut handles = Vec::new();
            for subject in subjects {
                handles.push(spawn_protobuf_subscription(
                    nc.clone(),
                    subject,
                    |bue: BalanceUpdateEvent| bue,
                    handler.clone(),
                ));
            }
            for handle in handles {
                let _ = handle.await;
            }
        })
    }

    pub async fn subscribe_position_updates<F>(&self, instrument: &str, handler: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(ProtoPositionUpdateEvent) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        let nc = self.nats.clone();
        let subjects = self.position_update_subjects(instrument).await;

        tokio::spawn(async move {
            let mut handles = Vec::new();
            for subject in subjects {
                handles.push(spawn_protobuf_subscription(
                    nc.clone(),
                    subject,
                    std::convert::identity,
                    Arc::clone(&handler),
                ));
            }
            for handle in handles {
                let _ = handle.await;
            }
        })
    }

    pub async fn subscribe_ticks<F>(&self, instrument_id: &str, handler: F) -> Result<tokio::task::JoinHandle<()>, SdkError>
    where
        F: Fn(ProtoTickData) + Send + Sync + 'static,
    {
        let subject = self.rtmd_subject(instrument_id, RtmdChannel::Tick).await?;
        Ok(spawn_protobuf_subscription(
            self.nats.clone(),
            subject,
            std::convert::identity,
            Arc::new(handler),
        ))
    }

    pub async fn subscribe_klines<F>(
        &self,
        instrument_id: &str,
        interval: &str,
        handler: F,
    ) -> Result<tokio::task::JoinHandle<()>, SdkError>
    where
        F: Fn(ProtoKline) + Send + Sync + 'static,
    {
        let subject = self
            .rtmd_subject(instrument_id, RtmdChannel::Kline(Some(interval.to_string())))
            .await?;
        Ok(spawn_protobuf_subscription(
            self.nats.clone(),
            subject,
            std::convert::identity,
            Arc::new(handler),
        ))
    }

    pub async fn subscribe_funding<F>(&self, instrument_id: &str, handler: F) -> Result<tokio::task::JoinHandle<()>, SdkError>
    where
        F: Fn(ProtoFundingRate) + Send + Sync + 'static,
    {
        let subject = self.rtmd_subject(instrument_id, RtmdChannel::Funding).await?;
        Ok(spawn_protobuf_subscription(
            self.nats.clone(),
            subject,
            std::convert::identity,
            Arc::new(handler),
        ))
    }

    pub async fn subscribe_orderbook<F>(&self, instrument_id: &str, handler: F) -> Result<tokio::task::JoinHandle<()>, SdkError>
    where
        F: Fn(ProtoOrderBook) + Send + Sync + 'static,
    {
        let subject = self.rtmd_subject(instrument_id, RtmdChannel::OrderBook).await?;
        Ok(spawn_protobuf_subscription(
            self.nats.clone(),
            subject,
            std::convert::identity,
            Arc::new(handler),
        ))
    }

    async fn preconnect_oms(&self) -> Result<(), SdkError> {
        let mut seen = HashSet::new();
        for account_id in &self.config.account_ids {
            let endpoint = self.resolve_account_endpoint(*account_id).await?;
            if seen.insert(endpoint.oms_id.clone()) {
                let _ = self.oms_pool.get_or_connect(&endpoint).await?;
            }
        }
        Ok(())
    }

    async fn oms_client(
        &self,
        account_id: i64,
    ) -> Result<crate::proto::oms_svc::oms_service_client::OmsServiceClient<tonic::transport::Channel>, SdkError> {
        let endpoint = self.resolve_account_endpoint(account_id).await?;
        self.oms_pool.get_or_connect(&endpoint).await
    }

    async fn resolve_account_endpoint(&self, account_id: i64) -> Result<OmsEndpoint, SdkError> {
        self.discovery.refresh_account_cache().await?;
        self.discovery.resolve_oms(account_id).await.ok_or(SdkError::OmsNotFound(account_id))
    }

    fn audit_meta(&self, source_id: &str) -> AuditMeta {
        let ts = now_ms();
        AuditMeta {
            source_id: source_id.to_string(),
            request_id: format!("{}-{}", source_id, ts),
            trace_id: format!("trace-{ts}"),
            timestamp_ms: ts,
        }
    }

    async fn order_update_subjects(&self) -> Vec<String> {
        let _ = self.discovery.refresh_account_cache().await;
        let snapshot = self.discovery.snapshot().await;
        self.config
            .account_ids
            .iter()
            .filter_map(|account_id| snapshot.get(account_id).map(|ep| order_update_topic(&ep.oms_id, *account_id)))
            .collect()
    }

    async fn balance_update_subjects(&self, asset: &str) -> Vec<String> {
        let _ = self.discovery.refresh_account_cache().await;
        let snapshot = self.discovery.snapshot().await;
        unique_oms(snapshot)
            .into_values()
            .map(|ep| balance_update_topic(&ep.oms_id, asset))
            .collect()
    }

    async fn position_update_subjects(&self, instrument: &str) -> Vec<String> {
        let _ = self.discovery.refresh_account_cache().await;
        let snapshot = self.discovery.snapshot().await;
        unique_oms(snapshot)
            .into_values()
            .map(|ep| position_update_topic(&ep.oms_id, instrument))
            .collect()
    }

    async fn rtmd_subject(&self, instrument_id: &str, channel: RtmdChannel) -> Result<String, SdkError> {
        let instrument = self.refdata.query_instrument(instrument_id).await?;
        Ok(match channel {
            RtmdChannel::Tick => tick_topic(&instrument.venue, &instrument.instrument_exch),
            RtmdChannel::Kline(Some(interval)) => {
                kline_topic(&instrument.venue, &instrument.instrument_exch, &interval)
            }
            RtmdChannel::Funding => funding_topic(&instrument.venue, &instrument.instrument_exch),
            RtmdChannel::OrderBook => orderbook_topic(&instrument.venue, &instrument.instrument_exch),
            RtmdChannel::Kline(None) => {
                return Err(SdkError::Config("kline interval is required".into()));
            }
        })
    }
}

enum RtmdChannel {
    Tick,
    Kline(Option<String>),
    Funding,
    OrderBook,
}

fn unique_oms(snapshot: HashMap<i64, OmsEndpoint>) -> HashMap<String, OmsEndpoint> {
    let mut unique = HashMap::new();
    for endpoint in snapshot.into_values() {
        unique.entry(endpoint.oms_id.clone()).or_insert(endpoint);
    }
    unique
}

fn map_order_request(account_id: i64, order: &TradingOrder) -> OrderRequest {
    OrderRequest {
        order_id: order.order_id,
        account_id,
        instrument_code: order.instrument_id.clone(),
        buy_sell_type: match order.side {
            Side::Buy => BuySellType::BsBuy as i32,
            Side::Sell => BuySellType::BsSell as i32,
        },
        open_close_type: OpenCloseType::OcUnspecified as i32,
        order_type: match order.order_type {
            OrderType::Limit => BasicOrderType::OrdertypeLimit as i32,
            OrderType::Market => BasicOrderType::OrdertypeMarket as i32,
        },
        price: order.price,
        qty: order.qty,
        source_id: order.source_id.clone(),
        timestamp: now_ms(),
        leverage: 0.0,
        extra_properties: None,
        time_inforce_type: TimeInForceType::TimeinforceGtc as i32,
        extra_order_tags: Vec::new(),
    }
}

fn map_cancel_request(cancel: &TradingCancel) -> OrderCancelRequest {
    OrderCancelRequest {
        order_id: cancel.order_id,
        exch_order_ref: cancel.exch_order_ref.clone(),
        extra_info_required: None,
        source_id: cancel.source_id.clone(),
        timestamp: now_ms(),
    }
}

fn map_command_ack(response: zk_proto_rs::zk::oms::v1::OmsResponse) -> CommandAck {
    let success = response.status == oms_response::Status::OmsRespStatusSuccess as i32;
    CommandAck {
        success,
        message: response.message,
        timestamp_ms: response.timestamp,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
