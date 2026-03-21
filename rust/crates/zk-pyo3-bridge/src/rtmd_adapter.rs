use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pyo3::prelude::*;
use zk_proto_rs::zk::rtmd::v1::{FundingRate, Kline, OrderBook, TickData};
use zk_rtmd_rs::types::{RtmdError, RtmdEvent, RtmdSubscriptionSpec};
use zk_rtmd_rs::venue_adapter::{Result, RtmdVenueAdapter};

use crate::py_errors::PyBridgeError;
use crate::py_event_loop::PyEventLoop;
use crate::py_runtime::PyObjectHandle;
use crate::py_types;

/// Default timeout for RTMD query/command operations.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for next_event — long-lived blocking coroutine.
const EVENT_TIMEOUT: Duration = Duration::from_secs(300);

/// Python-backed RTMD venue adapter.
///
/// All async Python methods execute on a persistent asyncio event loop.
pub struct PyRtmdVenueAdapter {
    obj: Py<PyAny>,
    event_loop: Arc<PyEventLoop>,
}

impl PyRtmdVenueAdapter {
    pub fn new(handle: PyObjectHandle) -> Self {
        Self {
            obj: handle.inner,
            event_loop: handle.event_loop,
        }
    }
}

/// Call a Python async void method on the persistent event loop.
async fn call_rtmd_void(
    obj: &Py<PyAny>,
    event_loop: &Arc<PyEventLoop>,
    method: &str,
    timeout: Duration,
) -> Result<()> {
    let obj = Python::with_gil(|py| obj.clone_ref(py));
    let loop_ref = event_loop.loop_ref();
    let method = method.to_string();
    let method2 = method.clone();
    let timeout_secs = timeout.as_secs_f64();

    tokio::time::timeout(
        timeout + Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let coro = obj
                    .call_method0(py, method.as_str())
                    .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                let asyncio = py.import_bound("asyncio")
                    .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                let future = asyncio
                    .call_method1(
                        "run_coroutine_threadsafe",
                        (coro.into_bound(py), loop_ref.bind(py)),
                    )
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method}: {e}")))?;
                let _result = future
                    .call_method1("result", (timeout_secs,))
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method}: {e}")))?;
                Ok::<(), PyBridgeError>(())
            })
        }),
    )
    .await
    .map_err(|_| RtmdError::from(PyBridgeError::TransientError(format!("{method2} timed out"))))?
    .map_err(|e| {
        RtmdError::from(PyBridgeError::PythonCall(format!(
            "spawn_blocking join: {e}"
        )))
    })?
    .map_err(RtmdError::from)
}

/// Call a Python async method with args, returning proto bytes decoded into T.
async fn call_rtmd_proto<T: prost::Message + Default + Send + 'static>(
    obj: &Py<PyAny>,
    event_loop: &Arc<PyEventLoop>,
    method: &str,
    args: impl IntoPy<Py<pyo3::types::PyTuple>> + Send + 'static,
    type_name: &str,
    timeout: Duration,
) -> Result<T> {
    let obj = Python::with_gil(|py| obj.clone_ref(py));
    let loop_ref = event_loop.loop_ref();
    let method_str = method.to_string();
    let method_str2 = method_str.clone();
    let type_name = type_name.to_string();
    let timeout_secs = timeout.as_secs_f64();

    tokio::time::timeout(
        timeout + Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let coro = obj
                    .call_method1(py, method_str.as_str(), args)
                    .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                let asyncio = py.import_bound("asyncio")
                    .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                let future = asyncio
                    .call_method1(
                        "run_coroutine_threadsafe",
                        (coro.into_bound(py), loop_ref.bind(py)),
                    )
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method_str}: {e}")))?;
                let result = future
                    .call_method1("result", (timeout_secs,))
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method_str}: {e}")))?;
                py_types::decode_proto_bytes(py, &result, &type_name)
            })
        }),
    )
    .await
    .map_err(|_| {
        RtmdError::from(PyBridgeError::TransientError(format!(
            "{method_str2} timed out"
        )))
    })?
    .map_err(|e| {
        RtmdError::from(PyBridgeError::PythonCall(format!(
            "spawn_blocking join: {e}"
        )))
    })?
    .map_err(RtmdError::from)
}

/// Call a Python async void method with args on the persistent event loop.
async fn call_rtmd_void_with_args(
    obj: &Py<PyAny>,
    event_loop: &Arc<PyEventLoop>,
    method: &str,
    args: impl IntoPy<Py<pyo3::types::PyTuple>> + Send + 'static,
    timeout: Duration,
) -> Result<()> {
    let obj = Python::with_gil(|py| obj.clone_ref(py));
    let loop_ref = event_loop.loop_ref();
    let method = method.to_string();
    let method2 = method.clone();
    let timeout_secs = timeout.as_secs_f64();

    tokio::time::timeout(
        timeout + Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let coro = obj
                    .call_method1(py, method.as_str(), args)
                    .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                let asyncio = py.import_bound("asyncio")
                    .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                let future = asyncio
                    .call_method1(
                        "run_coroutine_threadsafe",
                        (coro.into_bound(py), loop_ref.bind(py)),
                    )
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method}: {e}")))?;
                let _result = future
                    .call_method1("result", (timeout_secs,))
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method}: {e}")))?;
                Ok::<(), PyBridgeError>(())
            })
        }),
    )
    .await
    .map_err(|_| RtmdError::from(PyBridgeError::TransientError(format!("{method2} timed out"))))?
    .map_err(|e| {
        RtmdError::from(PyBridgeError::PythonCall(format!(
            "spawn_blocking join: {e}"
        )))
    })?
    .map_err(RtmdError::from)
}

#[async_trait]
impl RtmdVenueAdapter for PyRtmdVenueAdapter {
    async fn connect(&self) -> Result<()> {
        call_rtmd_void(&self.obj, &self.event_loop, "connect", QUERY_TIMEOUT).await
    }

    async fn subscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()> {
        let py_spec = Python::with_gil(|py| py_types::to_py_object(py, &spec)).map_err(|e| {
            RtmdError::from(PyBridgeError::ResponseDecode(format!(
                "subscribe encode: {e}"
            )))
        })?;
        call_rtmd_void_with_args(&self.obj, &self.event_loop, "subscribe", (py_spec,), QUERY_TIMEOUT)
            .await
    }

    async fn unsubscribe(&self, spec: RtmdSubscriptionSpec) -> Result<()> {
        let py_spec = Python::with_gil(|py| py_types::to_py_object(py, &spec)).map_err(|e| {
            RtmdError::from(PyBridgeError::ResponseDecode(format!(
                "unsubscribe encode: {e}"
            )))
        })?;
        call_rtmd_void_with_args(
            &self.obj,
            &self.event_loop,
            "unsubscribe",
            (py_spec,),
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn snapshot_active(&self) -> Result<Vec<RtmdSubscriptionSpec>> {
        let obj = Python::with_gil(|py| self.obj.clone_ref(py));
        let loop_ref = self.event_loop.loop_ref();
        let timeout_secs = QUERY_TIMEOUT.as_secs_f64();

        tokio::time::timeout(
            QUERY_TIMEOUT + Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let coro = obj
                        .call_method0(py, "snapshot_active")
                        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                    let asyncio = py.import_bound("asyncio")
                        .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                    let future = asyncio
                        .call_method1(
                            "run_coroutine_threadsafe",
                            (coro.into_bound(py), loop_ref.bind(py)),
                        )
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("snapshot_active: {e}"))
                        })?;
                    let result = future
                        .call_method1("result", (timeout_secs,))
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("snapshot_active: {e}"))
                        })?;
                    py_types::from_py_object::<Vec<RtmdSubscriptionSpec>>(&result).map_err(|e| {
                        PyBridgeError::ResponseDecode(format!("snapshot_active: {e}"))
                    })
                })
            }),
        )
        .await
        .map_err(|_| {
            RtmdError::from(PyBridgeError::TransientError(
                "snapshot_active timed out".into(),
            ))
        })?
        .map_err(|e| {
            RtmdError::from(PyBridgeError::PythonCall(format!(
                "spawn_blocking join: {e}"
            )))
        })?
        .map_err(RtmdError::from)
    }

    async fn next_event(&self) -> Result<RtmdEvent> {
        let obj = Python::with_gil(|py| self.obj.clone_ref(py));
        let loop_ref = self.event_loop.loop_ref();
        let timeout_secs = EVENT_TIMEOUT.as_secs_f64();

        tokio::time::timeout(
            EVENT_TIMEOUT + Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let coro = obj
                        .call_method0(py, "next_event")
                        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                    let asyncio = py.import_bound("asyncio")
                        .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                    let future = asyncio
                        .call_method1(
                            "run_coroutine_threadsafe",
                            (coro.into_bound(py), loop_ref.bind(py)),
                        )
                        .map_err(|e| PyBridgeError::PythonCall(format!("next_event: {e}")))?;
                    let result = future
                        .call_method1("result", (timeout_secs,))
                        .map_err(|e| PyBridgeError::PythonCall(format!("next_event: {e}")))?;
                    py_types::decode_rtmd_event(py, &result)
                })
            }),
        )
        .await
        .map_err(|_| {
            RtmdError::from(PyBridgeError::TransientError(
                "next_event timed out".into(),
            ))
        })?
        .map_err(|e| {
            RtmdError::from(PyBridgeError::PythonCall(format!(
                "spawn_blocking join: {e}"
            )))
        })?
        .map_err(RtmdError::from)
    }

    fn instrument_exch_for(&self, instrument_code: &str) -> Option<String> {
        Python::with_gil(|py| {
            let result = self
                .obj
                .call_method1(py, "instrument_exch_for", (instrument_code,))
                .ok()?;
            result.extract::<Option<String>>(py).ok()?
        })
    }

    async fn query_current_tick(&self, instrument_code: &str) -> Result<TickData> {
        call_rtmd_proto(
            &self.obj,
            &self.event_loop,
            "query_current_tick",
            (instrument_code.to_string(),),
            "TickData",
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn query_current_orderbook(
        &self,
        instrument_code: &str,
        depth: Option<u32>,
    ) -> Result<OrderBook> {
        call_rtmd_proto(
            &self.obj,
            &self.event_loop,
            "query_current_orderbook",
            (instrument_code.to_string(), depth),
            "OrderBook",
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn query_current_funding(&self, instrument_code: &str) -> Result<FundingRate> {
        call_rtmd_proto(
            &self.obj,
            &self.event_loop,
            "query_current_funding",
            (instrument_code.to_string(),),
            "FundingRate",
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn query_klines(
        &self,
        instrument_code: &str,
        interval: &str,
        limit: u32,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<Vec<Kline>> {
        let obj = Python::with_gil(|py| self.obj.clone_ref(py));
        let loop_ref = self.event_loop.loop_ref();
        let instrument_code = instrument_code.to_string();
        let interval = interval.to_string();
        let timeout_secs = QUERY_TIMEOUT.as_secs_f64();

        tokio::time::timeout(
            QUERY_TIMEOUT + Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                Python::with_gil(|py| {
                    let coro = obj
                        .call_method1(
                            py,
                            "query_klines",
                            (instrument_code, interval, limit, from_ms, to_ms),
                        )
                        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                    let asyncio = py.import_bound("asyncio")
                        .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
                    let future = asyncio
                        .call_method1(
                            "run_coroutine_threadsafe",
                            (coro.into_bound(py), loop_ref.bind(py)),
                        )
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("query_klines: {e}"))
                        })?;
                    let result = future
                        .call_method1("result", (timeout_secs,))
                        .map_err(|e| {
                            PyBridgeError::PythonCall(format!("query_klines: {e}"))
                        })?;
                    // query_klines returns a list of proto bytes
                    let py_list: &Bound<'_, pyo3::types::PyList> =
                        result.downcast().map_err(|e| {
                            PyBridgeError::ResponseDecode(format!(
                                "query_klines: expected list: {e}"
                            ))
                        })?;
                    let mut klines = Vec::with_capacity(py_list.len());
                    for item in py_list.iter() {
                        let kline: Kline =
                            py_types::decode_proto_bytes(py, &item, "Kline")?;
                        klines.push(kline);
                    }
                    Ok::<Vec<Kline>, PyBridgeError>(klines)
                })
            }),
        )
        .await
        .map_err(|_| {
            RtmdError::from(PyBridgeError::TransientError(
                "query_klines timed out".into(),
            ))
        })?
        .map_err(|e| {
            RtmdError::from(PyBridgeError::PythonCall(format!(
                "spawn_blocking join: {e}"
            )))
        })?
        .map_err(RtmdError::from)
    }
}
