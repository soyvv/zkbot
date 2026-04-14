use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::prelude::*;
use tracing::{info, warn};

use crate::py_errors::PyBridgeError;

/// A persistent Python asyncio event loop running on a dedicated background thread.
///
/// All coroutines dispatched via `run_coroutine` execute on the **same** loop,
/// preserving adapter state: sockets, queues, background tasks, HTTP clients, etc.
///
/// The event loop thread runs `loop.run_forever()` and releases the GIL when idle
/// (waiting on selector I/O). Callers dispatch work via
/// `asyncio.run_coroutine_threadsafe`, whose `.result()` also releases the GIL
/// during its internal condition-wait, so there is no GIL deadlock.
pub struct PyEventLoop {
    loop_obj: Py<PyAny>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

// Safety: Py<PyAny> is Send because PyO3 guarantees GIL-gated access.
// The Mutex<Option<JoinHandle>> is Send+Sync. We only touch loop_obj inside
// with_gil blocks.
unsafe impl Send for PyEventLoop {}
unsafe impl Sync for PyEventLoop {}

impl PyEventLoop {
    /// Spawn a background thread with a fresh asyncio event loop.
    pub fn start() -> Result<Self, PyBridgeError> {
        let (tx, rx) = mpsc::channel();

        let thread = std::thread::Builder::new()
            .name("py-event-loop".into())
            .spawn(move || {
                Python::with_gil(|py| {
                    let asyncio = py
                        .import_bound("asyncio")
                        .expect("failed to import asyncio");
                    let loop_obj = asyncio
                        .call_method0("new_event_loop")
                        .expect("new_event_loop failed");
                    asyncio
                        .call_method1("set_event_loop", (&loop_obj,))
                        .expect("set_event_loop failed");

                    let loop_py: Py<PyAny> = loop_obj.unbind();
                    let loop_run = loop_py.clone_ref(py);
                    // Send the loop reference to the creating thread.
                    let _ = tx.send(loop_py);

                    // Blocks until loop.stop() is called (or process exits).
                    let _ = loop_run.call_method0(py, "run_forever");
                });
            })
            .map_err(|e| {
                PyBridgeError::PythonCall(format!("failed to spawn event loop thread: {e}"))
            })?;

        let loop_obj = rx.recv().map_err(|_| {
            PyBridgeError::PythonCall(
                "event loop thread died before sending loop reference".into(),
            )
        })?;

        Ok(Self {
            loop_obj,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Dispatch a coroutine onto the persistent loop and block until it completes.
    ///
    /// Must be called while holding the GIL. The GIL is temporarily released
    /// during the wait (inside `Future.result()`), allowing the event loop thread
    /// to run the coroutine.
    ///
    /// `timeout_secs` is the Python-side timeout passed to `Future.result()`.
    /// On timeout, Python raises `concurrent.futures.TimeoutError`.
    pub fn run_coroutine<'py>(
        &self,
        py: Python<'py>,
        coro: Bound<'py, PyAny>,
        timeout_secs: f64,
    ) -> Result<Bound<'py, PyAny>, PyBridgeError> {
        let asyncio = py
            .import_bound("asyncio")
            .map_err(|e| PyBridgeError::PythonCall(format!("import asyncio: {e}")))?;
        let future = asyncio
            .call_method1(
                "run_coroutine_threadsafe",
                (&coro, self.loop_obj.bind(py)),
            )
            .map_err(|e| {
                PyBridgeError::PythonCall(format!("run_coroutine_threadsafe: {e}"))
            })?;
        let result = future
            .call_method1("result", (timeout_secs,))
            .map_err(|e| PyBridgeError::PythonCall(format!("coroutine execution: {e}")))?;
        Ok(result)
    }

    /// Get a cloned reference to the underlying Python loop object.
    pub fn loop_ref(&self) -> Py<PyAny> {
        Python::with_gil(|py| self.loop_obj.clone_ref(py))
    }

    /// Cancel all pending asyncio tasks on the event loop.
    ///
    /// This causes `concurrent.futures.Future.result()` to raise
    /// `CancelledError` on any `spawn_blocking` threads waiting for the
    /// cancelled coroutines, unblocking them promptly. The event loop
    /// itself keeps running so the cancellations can propagate.
    pub fn cancel_all_tasks(&self) {
        let result = Python::with_gil(|py| -> PyResult<()> {
            let locals = pyo3::types::PyDict::new_bound(py);
            locals.set_item("loop_obj", self.loop_obj.bind(py))?;
            py.run_bound(
                concat!(
                    "import asyncio, functools\n",
                    "def _cancel_all(loop):\n",
                    "    import asyncio as _aio\n",
                    "    for t in _aio.all_tasks(loop):\n",
                    "        t.cancel()\n",
                    "loop_obj.call_soon_threadsafe(",
                    "    functools.partial(_cancel_all, loop_obj))\n",
                ),
                None,
                Some(&locals),
            )?;
            Ok(())
        });
        if let Err(e) = result {
            warn!(error = %e, "failed to cancel asyncio tasks");
        }
    }

    /// Stop the event loop and join the background thread.
    ///
    /// Call this only after pending tasks have been cancelled and the
    /// cancellations have had time to propagate (see [`cancel_all_tasks`]).
    pub fn stop_and_join(&self, timeout: Duration) {
        // Signal the loop to stop.
        let stop_ok = Python::with_gil(|py| -> PyResult<()> {
            let stop = self.loop_obj.getattr(py, "stop")?;
            self.loop_obj
                .call_method1(py, "call_soon_threadsafe", (stop,))?;
            Ok(())
        });
        if let Err(e) = stop_ok {
            warn!(error = %e, "failed to send loop.stop to Python event loop");
        }

        // Join the thread.
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                if handle.is_finished() {
                    let _ = handle.join();
                    info!("Python event loop thread joined");
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    warn!("Python event loop thread did not exit within timeout — abandoning");
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

impl Drop for PyEventLoop {
    fn drop(&mut self) {
        // If stop_and_join() was already called, thread is None and this is a no-op.
        if self.thread.get_mut().unwrap().is_some() {
            self.cancel_all_tasks();
            // Brief pause to let cancellations propagate before stopping the loop.
            std::thread::sleep(Duration::from_millis(100));
            self.stop_and_join(Duration::from_secs(3));
        }
    }
}
