use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pyo3::prelude::*;
use serde::de::DeserializeOwned;

use crate::py_errors::PyBridgeError;
use crate::py_event_loop::PyEventLoop;
use crate::py_runtime::PyObjectHandle;
use crate::py_types;
use zk_gw_types::*;

/// Default timeout for venue query/command operations.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for next_event — long-lived blocking coroutine.
const EVENT_TIMEOUT: Duration = Duration::from_secs(300);
/// Timeout for adapter shutdown (Python side + event loop thread join).
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Python-backed venue gateway adapter.
///
/// All async Python methods execute on a persistent asyncio event loop, so
/// adapter state (sockets, queues, background tasks) is preserved across calls.
pub struct PyVenueAdapter {
    obj: Py<PyAny>,
    event_loop: Arc<PyEventLoop>,
    event_timeout: Duration,
    shutting_down: Arc<AtomicBool>,
}

impl PyVenueAdapter {
    pub fn new(handle: PyObjectHandle) -> Self {
        Self {
            obj: handle.inner,
            event_loop: handle.event_loop,
            event_timeout: EVENT_TIMEOUT,
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Override the event timeout (for testing).
    pub fn set_event_timeout(&mut self, timeout: Duration) {
        self.event_timeout = timeout;
    }
}

/// Call a Python async method on the persistent event loop and deserialize the result.
async fn call_method<T: DeserializeOwned + Send + 'static>(
    obj: &Py<PyAny>,
    event_loop: &Arc<PyEventLoop>,
    method: &str,
    args: impl IntoPy<Py<pyo3::types::PyTuple>> + Send + 'static,
    timeout: Duration,
) -> Result<T, PyBridgeError> {
    let obj = Python::with_gil(|py| obj.clone_ref(py));
    let loop_ref = event_loop.loop_ref();
    let method = method.to_string();
    let method2 = method.clone();
    let timeout_secs = timeout.as_secs_f64();

    let result = tokio::time::timeout(
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
                let result = future
                    .call_method1("result", (timeout_secs,))
                    .map_err(|e| PyBridgeError::PythonCall(format!("{method}: {e}")))?;
                py_types::from_py_object::<T>(&result)
                    .map_err(|e| PyBridgeError::ResponseDecode(format!("{method}: {e}")))
            })
        }),
    )
    .await
    .map_err(|_| PyBridgeError::TransientError(format!("{method2} timed out")))?
    .map_err(|e| PyBridgeError::PythonCall(format!("spawn_blocking join: {e}")))?;

    result
}

/// Call a Python async void method on the persistent event loop.
async fn call_void(
    obj: &Py<PyAny>,
    event_loop: &Arc<PyEventLoop>,
    method: &str,
    timeout: Duration,
) -> Result<(), PyBridgeError> {
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
    .map_err(|_| PyBridgeError::TransientError(format!("{method2} timed out")))?
    .map_err(|e| PyBridgeError::PythonCall(format!("spawn_blocking join: {e}")))?
}

#[async_trait]
impl VenueAdapter for PyVenueAdapter {
    async fn connect(&self) -> anyhow::Result<()> {
        call_void(&self.obj, &self.event_loop, "connect", COMMAND_TIMEOUT).await?;
        Ok(())
    }

    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("place_order request encode: {e}"))
            })?;
        Ok(call_method(&self.obj, &self.event_loop, "place_order", (py_req,), COMMAND_TIMEOUT)
            .await?)
    }

    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("cancel_order request encode: {e}"))
            })?;
        Ok(call_method(&self.obj, &self.event_loop, "cancel_order", (py_req,), COMMAND_TIMEOUT)
            .await?)
    }

    async fn query_balance(
        &self,
        req: VenueBalanceQuery,
    ) -> anyhow::Result<Vec<VenueBalanceFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_balance request encode: {e}"))
            })?;
        Ok(call_method(&self.obj, &self.event_loop, "query_balance", (py_req,), COMMAND_TIMEOUT)
            .await?)
    }

    async fn query_order(&self, req: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_order request encode: {e}"))
            })?;
        Ok(call_method(&self.obj, &self.event_loop, "query_order", (py_req,), COMMAND_TIMEOUT)
            .await?)
    }

    async fn query_open_orders(
        &self,
        req: VenueOpenOrdersQuery,
    ) -> anyhow::Result<Vec<VenueOrderFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_open_orders request encode: {e}"))
            })?;
        Ok(
            call_method(
                &self.obj,
                &self.event_loop,
                "query_open_orders",
                (py_req,),
                COMMAND_TIMEOUT,
            )
            .await?,
        )
    }

    async fn query_trades(&self, req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_trades request encode: {e}"))
            })?;
        Ok(call_method(&self.obj, &self.event_loop, "query_trades", (py_req,), COMMAND_TIMEOUT)
            .await?)
    }

    async fn query_funding_fees(
        &self,
        req: VenueFundingFeeQuery,
    ) -> anyhow::Result<Vec<VenueFundingFeeFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_funding_fees request encode: {e}"))
            })?;
        Ok(call_method(
            &self.obj,
            &self.event_loop,
            "query_funding_fees",
            (py_req,),
            COMMAND_TIMEOUT,
        )
        .await?)
    }

    async fn query_positions(
        &self,
        req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>> {
        let py_req =
            Python::with_gil(|py| py_types::to_py_object(py, &req)).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("query_positions request encode: {e}"))
            })?;
        Ok(
            call_method(
                &self.obj,
                &self.event_loop,
                "query_positions",
                (py_req,),
                COMMAND_TIMEOUT,
            )
            .await?,
        )
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        // 1. Set shutdown flag so next_event() stops retrying.
        self.shutting_down.store(true, Ordering::Release);

        // 2. Cancel all pending asyncio tasks — this unblocks any
        //    spawn_blocking threads waiting on Future.result().
        self.event_loop.cancel_all_tasks();

        // 3. Brief yield to let cancellations propagate through the event loop
        //    and unblock spawn_blocking threads.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 4. Call Python adapter's shutdown() if it exists (clean up streams, clients).
        let has_shutdown = Python::with_gil(|py| self.obj.getattr(py, "shutdown").is_ok());
        if has_shutdown {
            tracing::info!("calling Python adapter shutdown()");
            if let Err(e) =
                call_void(&self.obj, &self.event_loop, "shutdown", SHUTDOWN_TIMEOUT).await
            {
                tracing::warn!(error = %e, "Python adapter shutdown() failed");
            }
        }

        // 5. Stop the asyncio event loop and join its thread.
        self.event_loop.stop_and_join(SHUTDOWN_TIMEOUT);
        Ok(())
    }

    async fn next_event(&self) -> anyhow::Result<VenueEvent> {
        // Retry loop: next_event() has indefinite-wait semantics.
        // The bridge timeout is an internal watchdog, not a caller-visible error.
        loop {
            if self.shutting_down.load(Ordering::Acquire) {
                anyhow::bail!("adapter shutting down");
            }
            let obj = Python::with_gil(|py| self.obj.clone_ref(py));
            let loop_ref = self.event_loop.loop_ref();
            let timeout_secs = self.event_timeout.as_secs_f64();

            let result = tokio::time::timeout(
                self.event_timeout + Duration::from_secs(5),
                tokio::task::spawn_blocking(move || {
                    Python::with_gil(|py| {
                        let coro = obj
                            .call_method0(py, "next_event")
                            .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;
                        let asyncio = py.import_bound("asyncio")
                            .map_err(|e| {
                                PyBridgeError::PythonCall(format!("import asyncio: {e}"))
                            })?;
                        let future = asyncio
                            .call_method1(
                                "run_coroutine_threadsafe",
                                (coro.into_bound(py), loop_ref.bind(py)),
                            )
                            .map_err(|e| {
                                PyBridgeError::PythonCall(format!("next_event: {e}"))
                            })?;
                        let result = future
                            .call_method1("result", (timeout_secs,));
                        match result {
                            Ok(val) => py_types::decode_venue_event(py, &val),
                            Err(e) => {
                                // Cancel the Python coroutine before propagating.
                                // run_coroutine_threadsafe returns a concurrent.futures.Future
                                // whose .cancel() also cancels the underlying asyncio task,
                                // preventing orphaned coroutines on retry.
                                let _ = future.call_method0("cancel");
                                Err(PyBridgeError::PythonCall(format!("next_event: {e}")))
                            }
                        }
                    })
                }),
            )
            .await;

            match result {
                // Happy path: got an event.
                Ok(Ok(Ok(event))) => return Ok(event),
                // Python error — check if it's the specific idle timeout from
                // Future.result(timeout) raising concurrent.futures.TimeoutError.
                Ok(Ok(Err(e))) => {
                    if let PyBridgeError::PythonCall(ref msg) = e {
                        if msg.starts_with("next_event: TimeoutError") {
                            tracing::debug!("next_event: idle timeout, re-polling");
                            continue;
                        }
                    }
                    return Err(e.into());
                }
                // spawn_blocking JoinError — fatal.
                Ok(Err(join_err)) => {
                    return Err(PyBridgeError::PythonCall(format!(
                        "spawn_blocking join: {join_err}"
                    ))
                    .into());
                }
                // Tokio outer timeout — Python-side should have fired first,
                // but if not, also treat as idle retry.
                Err(_elapsed) => {
                    tracing::debug!("next_event: tokio timeout elapsed, re-polling");
                    continue;
                }
            }
        }
    }
}
