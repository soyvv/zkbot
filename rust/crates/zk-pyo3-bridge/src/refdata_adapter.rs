use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};

use crate::py_errors::PyBridgeError;
use crate::py_event_loop::PyEventLoop;
use crate::py_runtime::PyObjectHandle;
use crate::py_types;

/// Default timeout for refdata operations.
const REFDATA_TIMEOUT: Duration = Duration::from_secs(60);

/// A row of instrument reference data returned by a Python refdata loader.
///
/// This is a flat serde struct matching the dict fields Python loaders return.
/// The service host is responsible for converting to the `InstrumentRefData` proto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefdataInstrumentRow {
    pub instrument_id: String,
    pub instrument_id_exchange: String,
    pub exchange_name: String,
    pub instrument_type: String,
    #[serde(default)]
    pub base_asset: Option<String>,
    #[serde(default)]
    pub quote_asset: Option<String>,
    #[serde(default)]
    pub settlement_asset: Option<String>,
    #[serde(default)]
    pub contract_size: Option<f64>,
    #[serde(default)]
    pub price_precision: Option<i64>,
    #[serde(default)]
    pub qty_precision: Option<i64>,
    #[serde(default)]
    pub price_tick_size: Option<f64>,
    #[serde(default)]
    pub qty_lot_size: Option<f64>,
    #[serde(default)]
    pub min_notional: Option<f64>,
    #[serde(default)]
    pub min_order_qty: Option<f64>,
    #[serde(default)]
    pub max_order_qty: Option<f64>,
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub expiry_date: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub asset_class: Option<String>,
    #[serde(default)]
    pub margin_requirement: Option<f64>,
    /// Catch-all for venue-specific fields.
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Trait for venue-specific reference data loaders.
#[async_trait]
pub trait RefdataLoader: Send + Sync {
    /// Load instrument definitions from the venue.
    async fn load_instruments(&self) -> anyhow::Result<Vec<RefdataInstrumentRow>>;

    /// Load market session schedules (if supported).
    async fn load_market_sessions(&self) -> anyhow::Result<Vec<serde_json::Value>>;
}

/// Python-backed refdata loader wrapping a Python object via PyO3.
pub struct PyRefdataLoader {
    obj: Py<PyAny>,
    event_loop: Arc<PyEventLoop>,
}

impl PyRefdataLoader {
    pub fn new(handle: PyObjectHandle) -> Self {
        Self {
            obj: handle.inner,
            event_loop: handle.event_loop,
        }
    }
}

#[async_trait]
impl RefdataLoader for PyRefdataLoader {
    async fn load_instruments(&self) -> anyhow::Result<Vec<RefdataInstrumentRow>> {
        let obj = Python::with_gil(|py| self.obj.clone_ref(py));
        let loop_ref = self.event_loop.loop_ref();
        let timeout_secs = REFDATA_TIMEOUT.as_secs_f64();

        let result = tokio::time::timeout(
            REFDATA_TIMEOUT + Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let coro = obj
                        .call_method0(py, "load_instruments")
                        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                    let asyncio = py.import_bound("asyncio").map_err(|e| {
                        PyBridgeError::PythonCall(format!("import asyncio: {e}"))
                    })?;
                    let future = asyncio
                        .call_method1(
                            "run_coroutine_threadsafe",
                            (coro.into_bound(py), loop_ref.bind(py)),
                        )
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("load_instruments: {e}"))
                        })?;
                    let result = future
                        .call_method1("result", (timeout_secs,))
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("load_instruments: {e}"))
                        })?;
                    py_types::from_py_object::<Vec<RefdataInstrumentRow>>(&result).map_err(|e| {
                        PyBridgeError::ResponseDecode(format!("load_instruments: {e}"))
                    })
                })
            }),
        )
        .await
        .map_err(|_| PyBridgeError::TransientError("load_instruments timed out".into()))?
        .map_err(|e| PyBridgeError::PythonCall(format!("spawn_blocking join: {e}")))?;

        result.map_err(|e| e.into())
    }

    async fn load_market_sessions(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let obj = Python::with_gil(|py| self.obj.clone_ref(py));
        let loop_ref = self.event_loop.loop_ref();
        let timeout_secs = REFDATA_TIMEOUT.as_secs_f64();

        let result = tokio::time::timeout(
            REFDATA_TIMEOUT + Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let coro = obj
                        .call_method0(py, "load_market_sessions")
                        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                    let asyncio = py.import_bound("asyncio").map_err(|e| {
                        PyBridgeError::PythonCall(format!("import asyncio: {e}"))
                    })?;
                    let future = asyncio
                        .call_method1(
                            "run_coroutine_threadsafe",
                            (coro.into_bound(py), loop_ref.bind(py)),
                        )
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("load_market_sessions: {e}"))
                        })?;
                    let result = future
                        .call_method1("result", (timeout_secs,))
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("load_market_sessions: {e}"))
                        })?;
                    py_types::from_py_object::<Vec<serde_json::Value>>(&result).map_err(|e| {
                        PyBridgeError::ResponseDecode(format!("load_market_sessions: {e}"))
                    })
                })
            }),
        )
        .await
        .map_err(|_| {
            PyBridgeError::TransientError("load_market_sessions timed out".into())
        })?
        .map_err(|e| PyBridgeError::PythonCall(format!("spawn_blocking join: {e}")))?;

        result.map_err(|e| e.into())
    }
}
