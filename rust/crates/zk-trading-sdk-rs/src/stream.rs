//! OMS event stream topic construction.

use std::sync::Arc;

use futures::StreamExt;
use prost::Message;
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;

use crate::error::SdkError;

/// NATS subject for order updates for a specific account from an OMS instance.
pub fn order_update_topic(oms_id: &str, account_id: i64) -> String {
    format!("zk.oms.{oms_id}.order_update.{account_id}")
}

/// NATS subject for balance updates from an OMS instance.
///
/// The current OMS runtime publishes to `zk.oms.<oms_id>.balance_update` without an
/// asset suffix. The architecture contract (api_contracts.md line 267) designates the
/// per-asset suffix as deferred. We subscribe to the bare topic to match the current
/// runtime; the `asset` parameter is reserved for future per-asset filtering when the
/// OMS migrates to the suffixed topic shape.
pub fn balance_update_topic(oms_id: &str, _asset: &str) -> String {
    format!("zk.oms.{oms_id}.balance_update")
}

/// NATS subject for position updates from an OMS instance.
///
/// The current OMS runtime publishes to `zk.oms.<oms_id>.position_update` without an
/// instrument suffix. The architecture contract (api_contracts.md) designates the
/// per-instrument suffix as deferred. We subscribe to the bare topic to match the
/// current runtime; the `instrument` parameter is reserved for future per-instrument
/// filtering when the OMS migrates to the suffixed topic shape.
pub fn position_update_topic(oms_id: &str, _instrument: &str) -> String {
    format!("zk.oms.{oms_id}.position_update")
}

pub fn spawn_protobuf_subscription<T, U, M, F>(
    nc: async_nats::Client,
    subject: String,
    map: M,
    handler: Arc<F>,
) -> JoinHandle<()>
where
    T: Message + Default + Send + 'static,
    U: Send + 'static,
    M: Fn(T) -> U + Send + Sync + 'static,
    F: Fn(U) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let Ok(mut sub) = nc.subscribe(subject).await else {
            return;
        };
        while let Some(msg) = sub.next().await {
            let Ok(value) = T::decode(msg.payload.as_ref()) else {
                continue;
            };
            handler(map(value));
        }
    })
}

pub fn spawn_json_subscription<T, F>(
    nc: async_nats::Client,
    subject: String,
    handler: Arc<F>,
) -> JoinHandle<()>
where
    T: DeserializeOwned + Send + 'static,
    F: Fn(T) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let Ok(mut sub) = nc.subscribe(subject).await else {
            return;
        };
        while let Some(msg) = sub.next().await {
            let Ok(value) = serde_json::from_slice::<T>(msg.payload.as_ref()) else {
                continue;
            };
            handler(value);
        }
    })
}

pub async fn try_refresh_and_collect<T>(tasks: Vec<JoinHandle<T>>) -> Result<Vec<T>, SdkError>
where
    T: Send + 'static,
{
    let mut values = Vec::with_capacity(tasks.len());
    for task in tasks {
        values.push(task.await.map_err(|e| SdkError::Decode(e.to_string()))?);
    }
    Ok(values)
}
