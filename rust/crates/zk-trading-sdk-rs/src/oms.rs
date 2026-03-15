//! OMS gRPC channel pool.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tonic::transport::{Channel, Endpoint};

use crate::discovery::OmsEndpoint;
use crate::error::SdkError;
use crate::proto::oms_svc::oms_service_client::OmsServiceClient;

/// Pool of OMS gRPC channels keyed by logical OMS ID.
#[derive(Clone, Default)]
pub struct OmsChannelPool {
    channels: Arc<RwLock<HashMap<String, Channel>>>,
}

impl OmsChannelPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_connect(
        &self,
        endpoint: &OmsEndpoint,
    ) -> Result<OmsServiceClient<Channel>, SdkError> {
        if let Some(channel) = self.channels.read().await.get(&endpoint.oms_id).cloned() {
            return Ok(OmsServiceClient::new(channel));
        }

        let channel = connect_channel(endpoint).await?;
        self.channels
            .write()
            .await
            .insert(endpoint.oms_id.clone(), channel.clone());
        Ok(OmsServiceClient::new(channel))
    }

    pub async fn drain_and_reconnect(
        &self,
        oms_id: &str,
        new_endpoint: &OmsEndpoint,
    ) -> Result<(), SdkError> {
        let channel = connect_channel(new_endpoint).await?;
        self.channels.write().await.insert(oms_id.to_string(), channel);
        Ok(())
    }
}

async fn connect_channel(endpoint: &OmsEndpoint) -> Result<Channel, SdkError> {
    let uri = grpc_uri(&endpoint.grpc_address);
    let mut ep = Endpoint::from_shared(uri)?;
    if let Some(authority) = &endpoint.grpc_authority {
        ep = ep.origin(format!("http://{authority}").parse().map_err(|e| {
            SdkError::Config(format!("invalid gRPC authority '{authority}': {e}"))
        })?);
    }
    Ok(ep.connect().await?)
}

fn grpc_uri(address: &str) -> String {
    if address.starts_with("http://") || address.starts_with("https://") {
        address.to_string()
    } else {
        format!("http://{address}")
    }
}
