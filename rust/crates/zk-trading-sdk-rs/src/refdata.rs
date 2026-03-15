//! RefdataSdk: gRPC client, in-memory cache, and NATS invalidation.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use crate::error::SdkError;
use crate::proto::refdata_svc::refdata_service_client::RefdataServiceClient;
use crate::proto::refdata_svc::{
    DisclosureLevel, InstrumentRefdataResponse, MarketStatusResponse, QueryByVenueSymbolRequest,
    QueryInstrumentByIdRequest, QueryMarketStatusRequest,
};

const REFDATA_UPDATED_TOPIC: &str = "zk.control.refdata.updated";
const MARKET_STATUS_UPDATED_TOPIC: &str = "zk.control.market_status.updated";

/// Dependency-injectable gRPC trait for testing without a real server.
#[async_trait::async_trait]
pub trait RefdataGrpc: Send + Sync + 'static {
    async fn query_instrument_by_id(&self, instrument_id: &str) -> Result<InstrumentRefdataResponse, SdkError>;
    async fn query_instrument_by_venue_symbol(&self, venue: &str, instrument_exch: &str) -> Result<InstrumentRefdataResponse, SdkError>;
    async fn query_market_status(&self, venue: &str, market: &str) -> Result<MarketStatusResponse, SdkError>;
}

/// A cached entry that can be marked invalid after an invalidation event.
#[derive(Debug, Clone)]
pub struct CachedEntry<T> {
    pub data: T,
    /// False after invalidation, until reload completes.
    pub valid: bool,
    /// False after deprecation means no auto-reload.
    pub deprecated: bool,
    pub loaded_at_ms: i64,
}

/// In-memory refdata cache.
#[derive(Default)]
pub struct RefdataCache {
    pub instruments: HashMap<String, CachedEntry<InstrumentRefdataResponse>>,
    pub by_venue_symbol: HashMap<(String, String), String>,
    pub market_status: HashMap<(String, String), CachedEntry<MarketStatusResponse>>,
}

/// Core cache-and-gRPC logic, injectable with any `RefdataGrpc` implementation.
pub struct RefdataSdkInner<G: RefdataGrpc> {
    grpc: G,
    cache: Arc<RwLock<RefdataCache>>,
}

impl<G: RefdataGrpc> RefdataSdkInner<G> {
    pub fn new_with_mock(grpc: G) -> Self {
        Self { grpc, cache: Arc::new(RwLock::new(RefdataCache::default())) }
    }

    pub async fn query_instrument(&self, instrument_id: &str) -> Result<InstrumentRefdataResponse, SdkError> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.instruments.get(instrument_id) {
                if entry.deprecated {
                    return Err(SdkError::RefdataDeprecated(instrument_id.to_string()));
                }
                if entry.valid {
                    return Ok(entry.data.clone());
                }
                let stale_data = entry.data.clone();
                drop(cache);
                match self.grpc.query_instrument_by_id(instrument_id).await {
                    Ok(fresh) => {
                        self.upsert_instrument(fresh.clone()).await;
                        return Ok(fresh);
                    }
                    Err(_) => return Ok(stale_data),
                }
            }
        }

        let data = self
            .grpc
            .query_instrument_by_id(instrument_id)
            .await
            .map_err(|_| SdkError::RefdataNotAvailable(instrument_id.to_string()))?;
        self.upsert_instrument(data.clone()).await;
        Ok(data)
    }

    pub async fn query_by_venue_symbol(
        &self,
        venue: &str,
        instrument_exch: &str,
    ) -> Result<InstrumentRefdataResponse, SdkError> {
        {
            let cache = self.cache.read().await;
            if let Some(instrument_id) = cache
                .by_venue_symbol
                .get(&(venue.to_string(), instrument_exch.to_string()))
            {
                let instrument_id = instrument_id.clone();
                drop(cache);
                return self.query_instrument(&instrument_id).await;
            }
        }

        let data = self
            .grpc
            .query_instrument_by_venue_symbol(venue, instrument_exch)
            .await
            .map_err(|_| SdkError::RefdataNotAvailable(format!("{venue}/{instrument_exch}")))?;
        self.upsert_instrument(data.clone()).await;
        Ok(data)
    }

    pub async fn query_market_status(&self, venue: &str, market: &str) -> Result<MarketStatusResponse, SdkError> {
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.market_status.get(&(venue.to_string(), market.to_string())) {
                if entry.valid {
                    return Ok(entry.data.clone());
                }
            }
        }

        let data = self.grpc.query_market_status(venue, market).await?;
        self.upsert_market_status(data.clone()).await;
        Ok(data)
    }

    pub async fn invalidate(&self, instrument_id: &str, deprecated: bool) {
        let mut cache = self.cache.write().await;
        if let Some(entry) = cache.instruments.get_mut(instrument_id) {
            entry.valid = false;
            entry.deprecated = deprecated;
        }
    }

    pub async fn update_market_status_inline(&self, data: MarketStatusResponse) {
        self.upsert_market_status(data).await;
    }

    pub async fn upsert_instrument(&self, data: InstrumentRefdataResponse) {
        let key = (data.venue.clone(), data.instrument_exch.clone());
        let instrument_id = data.instrument_id.clone();
        let mut cache = self.cache.write().await;
        cache.instruments.insert(
            instrument_id.clone(),
            CachedEntry {
                data,
                valid: true,
                deprecated: false,
                loaded_at_ms: now_ms(),
            },
        );
        cache.by_venue_symbol.insert(key, instrument_id);
    }

    async fn upsert_market_status(&self, data: MarketStatusResponse) {
        let key = (data.venue.clone(), data.market.clone());
        let mut cache = self.cache.write().await;
        cache.market_status.insert(
            key,
            CachedEntry {
                data,
                valid: true,
                deprecated: false,
                loaded_at_ms: now_ms(),
            },
        );
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// gRPC implementation using the generated tonic client.
pub struct LiveRefdataGrpc {
    client: Mutex<RefdataServiceClient<Channel>>,
}

impl LiveRefdataGrpc {
    pub async fn connect(grpc_endpoint: String) -> Result<Self, SdkError> {
        let endpoint = if grpc_endpoint.starts_with("http://") || grpc_endpoint.starts_with("https://") {
            grpc_endpoint
        } else {
            format!("http://{grpc_endpoint}")
        };
        let client = RefdataServiceClient::connect(endpoint).await?;
        Ok(Self { client: Mutex::new(client) })
    }
}

#[async_trait::async_trait]
impl RefdataGrpc for LiveRefdataGrpc {
    async fn query_instrument_by_id(&self, instrument_id: &str) -> Result<InstrumentRefdataResponse, SdkError> {
        let response = self
            .client
            .lock()
            .await
            .query_instrument_by_id(QueryInstrumentByIdRequest {
                instrument_id: instrument_id.to_string(),
                level: DisclosureLevel::Basic as i32,
            })
            .await?;
        Ok(response.into_inner())
    }

    async fn query_instrument_by_venue_symbol(
        &self,
        venue: &str,
        instrument_exch: &str,
    ) -> Result<InstrumentRefdataResponse, SdkError> {
        let response = self
            .client
            .lock()
            .await
            .query_instrument_by_venue_symbol(QueryByVenueSymbolRequest {
                venue: venue.to_string(),
                instrument_exch: instrument_exch.to_string(),
                level: DisclosureLevel::Basic as i32,
            })
            .await?;
        Ok(response.into_inner())
    }

    async fn query_market_status(&self, venue: &str, market: &str) -> Result<MarketStatusResponse, SdkError> {
        let response = self
            .client
            .lock()
            .await
            .query_market_status(QueryMarketStatusRequest {
                venue: venue.to_string(),
                market: market.to_string(),
            })
            .await?;
        Ok(response.into_inner())
    }
}

/// Full RefdataSdk with NATS invalidation tasks and production gRPC client.
pub struct RefdataSdk {
    inner: Arc<RefdataSdkInner<LiveRefdataGrpc>>,
    _invalidation_task: JoinHandle<()>,
    _market_status_task: JoinHandle<()>,
}

impl RefdataSdk {
    pub async fn start(nc: async_nats::Client, grpc_endpoint: String) -> Result<Self, SdkError> {
        let live = LiveRefdataGrpc::connect(grpc_endpoint).await?;
        let inner = Arc::new(RefdataSdkInner::new_with_mock(live));

        let invalidation_task = spawn_invalidation_task(nc.clone(), Arc::clone(&inner));
        let market_status_task = spawn_market_status_task(nc, Arc::clone(&inner));

        Ok(Self {
            inner,
            _invalidation_task: invalidation_task,
            _market_status_task: market_status_task,
        })
    }

    pub async fn query_instrument(&self, instrument_id: &str) -> Result<InstrumentRefdataResponse, SdkError> {
        self.inner.query_instrument(instrument_id).await
    }

    pub async fn query_by_venue_symbol(
        &self,
        venue: &str,
        instrument_exch: &str,
    ) -> Result<InstrumentRefdataResponse, SdkError> {
        self.inner.query_by_venue_symbol(venue, instrument_exch).await
    }

    pub async fn query_market_status(&self, venue: &str, market: &str) -> Result<MarketStatusResponse, SdkError> {
        self.inner.query_market_status(venue, market).await
    }
}

fn spawn_invalidation_task(
    nc: async_nats::Client,
    inner: Arc<RefdataSdkInner<LiveRefdataGrpc>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(mut sub) = nc.subscribe(REFDATA_UPDATED_TOPIC.to_string()).await else {
            return;
        };

        while let Some(msg) = sub.next().await {
            let Ok(event) = serde_json::from_slice::<RefdataInvalidationEvent>(&msg.payload) else {
                continue;
            };
            if event.event_type != "refdata_updated" || event.instrument_id.is_empty() {
                continue;
            }

            let deprecated = event.change_class.eq_ignore_ascii_case("deprecate");
            inner.invalidate(&event.instrument_id, deprecated).await;
            if !deprecated {
                let _ = inner.query_instrument(&event.instrument_id).await;
            }
        }
    })
}

fn spawn_market_status_task(
    nc: async_nats::Client,
    inner: Arc<RefdataSdkInner<LiveRefdataGrpc>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(mut sub) = nc.subscribe(MARKET_STATUS_UPDATED_TOPIC.to_string()).await else {
            return;
        };

        while let Some(msg) = sub.next().await {
            let Ok(event) = serde_json::from_slice::<MarketStatusEvent>(&msg.payload) else {
                continue;
            };
            if event.event_type != "market_status_updated" {
                continue;
            }
            inner
                .update_market_status_inline(MarketStatusResponse {
                    venue: event.venue,
                    market: event.market,
                    session_state: event.session_state,
                    effective_at_ms: event.effective_at_ms,
                })
                .await;
        }
    })
}

#[derive(serde::Deserialize)]
struct RefdataInvalidationEvent {
    event_type: String,
    instrument_id: String,
    #[serde(default)]
    change_class: String,
}

#[derive(serde::Deserialize)]
struct MarketStatusEvent {
    event_type: String,
    venue: String,
    market: String,
    session_state: String,
    effective_at_ms: i64,
}
