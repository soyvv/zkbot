//! Gateway gRPC client pool.
//!
//! Each live gateway discovered in the NATS KV registry gets one
//! `GatewayServiceClient` stored here.  The writer task calls
//! `GwClientPool::send_order` / `cancel_order` when `OmsCore` emits the
//! corresponding `OmsAction` variants.
//!
//! # Performance note
//! tonic channels are internally connection-pooled (HTTP/2 multiplexing).
//! No per-request connection overhead.

use std::collections::HashMap;
use std::time::Duration;

use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};

use crate::proto::gw_svc::gateway_service_client::GatewayServiceClient;
use zk_proto_rs::zk::gateway::v1::{
    AccountResponse, BatchCancelOrdersRequest, BatchSendOrdersRequest, CancelOrderRequest,
    GatewayResponse, QueryAccountRequest, SendOrderRequest,
};

pub type GatewayClient = GatewayServiceClient<Channel>;

/// Pool of `GatewayServiceClient`s, keyed by `gw_key`.
///
/// Owned exclusively by the OMS writer task — no locking needed.
pub struct GwClientPool {
    clients: HashMap<String, GatewayClient>,
}

impl GwClientPool {
    pub fn new() -> Self {
        Self { clients: HashMap::new() }
    }

    /// Connect to a gateway at `addr` and register it under `gw_key`.
    ///
    /// Uses a 5s connect timeout and keeps the channel alive for reconnects.
    pub async fn connect(&mut self, gw_key: String, addr: &str) -> Result<(), tonic::transport::Error> {
        let endpoint = Endpoint::from_shared(addr.to_string())
            .expect("invalid gateway address")
            .connect_timeout(Duration::from_secs(5))
            .tcp_keepalive(Some(Duration::from_secs(30)));

        let channel = endpoint.connect().await?;
        info!(gw_key, addr, "gateway client connected");
        self.clients.insert(gw_key, GatewayServiceClient::new(channel));
        Ok(())
    }

    /// Remove a disconnected gateway from the pool.
    pub fn remove(&mut self, gw_key: &str) {
        if self.clients.remove(gw_key).is_some() {
            warn!(gw_key, "gateway client removed from pool");
        }
    }

    /// Check whether a client exists for `gw_key`.
    pub fn contains(&self, gw_key: &str) -> bool {
        self.clients.contains_key(gw_key)
    }

    /// Iterate all registered `gw_key`s.
    pub fn gw_keys(&self) -> impl Iterator<Item = &str> {
        self.clients.keys().map(String::as_str)
    }

    /// Clone the client for `gw_key`.
    ///
    /// Tonic `GatewayServiceClient<Channel>` is cheaply cloneable —
    /// clones share the underlying HTTP/2 connection.
    pub fn get_client(&self, gw_key: &str) -> Option<GatewayClient> {
        self.clients.get(gw_key).cloned()
    }

    // ── Order dispatch ───────────────────────────────────────────────────────

    pub async fn send_order(
        &mut self,
        gw_key: &str,
        req: SendOrderRequest,
    ) -> Result<GatewayResponse, GwError> {
        let client = self.clients.get_mut(gw_key).ok_or_else(|| GwError::NotFound(gw_key.into()))?;
        let resp = client
            .place_order(tonic::Request::new(req))
            .await
            .map_err(GwError::Rpc)?;
        Ok(resp.into_inner())
    }

    pub async fn batch_send_orders(
        &mut self,
        gw_key: &str,
        req: BatchSendOrdersRequest,
    ) -> Result<GatewayResponse, GwError> {
        let client = self.clients.get_mut(gw_key).ok_or_else(|| GwError::NotFound(gw_key.into()))?;
        let resp = client
            .batch_place_orders(tonic::Request::new(req))
            .await
            .map_err(GwError::Rpc)?;
        Ok(resp.into_inner())
    }

    pub async fn cancel_order(
        &mut self,
        gw_key: &str,
        req: CancelOrderRequest,
    ) -> Result<GatewayResponse, GwError> {
        let client = self.clients.get_mut(gw_key).ok_or_else(|| GwError::NotFound(gw_key.into()))?;
        let resp = client
            .cancel_order(tonic::Request::new(req))
            .await
            .map_err(GwError::Rpc)?;
        Ok(resp.into_inner())
    }

    pub async fn batch_cancel_orders(
        &mut self,
        gw_key: &str,
        req: BatchCancelOrdersRequest,
    ) -> Result<GatewayResponse, GwError> {
        let client = self.clients.get_mut(gw_key).ok_or_else(|| GwError::NotFound(gw_key.into()))?;
        let resp = client
            .batch_cancel_orders(tonic::Request::new(req))
            .await
            .map_err(GwError::Rpc)?;
        Ok(resp.into_inner())
    }

    // ── Query ─────────────────────────────────────────────────────────────────

    pub async fn query_account_balance(
        &mut self,
        gw_key: &str,
        req: QueryAccountRequest,
    ) -> Result<AccountResponse, GwError> {
        let client = self.clients.get_mut(gw_key).ok_or_else(|| GwError::NotFound(gw_key.into()))?;
        let resp = client
            .query_account_balance(tonic::Request::new(req))
            .await
            .map_err(GwError::Rpc)?;
        Ok(resp.into_inner())
    }
}

impl Default for GwClientPool {
    fn default() -> Self { Self::new() }
}

#[derive(Debug, thiserror::Error)]
pub enum GwError {
    #[error("no gateway client for gw_key={0}")]
    NotFound(String),
    #[error("gateway gRPC error: {0}")]
    Rpc(#[from] tonic::Status),
}
