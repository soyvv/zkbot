/// PyStrategyAdapter — Rust `Strategy` implementation that drives a Python strategy.
///
/// Each callback:
/// 1. Acquires the GIL.
/// 2. Builds a `ZkQuantAdapter` from the current `StrategyContext`.
/// 3. Converts the event to the matching Python betterproto type.
/// 4. Calls the corresponding Python method on the wrapped strategy object.
/// 5. Drains accumulated `SAction`s from the adapter.
///
/// Python callback name conventions (ETS uses the legacy names):
///   on_create, on_reinit, on_bar, on_tick, on_orderupdate, on_scheduled_time
///   on_order_update, on_timer are also tried as fallbacks.
use pyo3::prelude::*;
use pyo3::types::PyDict;
use zk_proto_rs::zk::{oms::v1::OrderUpdateEvent, rtmd::v1::Kline};
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, TimerEvent},
    strategy::Strategy,
};

use crate::adapter::{rust_proto_to_py, ZkQuantAdapter};

// ---------------------------------------------------------------------------

pub struct PyStrategyAdapter {
    /// The Python strategy instance (e.g. an `ETS` object).
    py_strategy: PyObject,
    /// `custom_params` dict passed as first arg to `on_reinit(config, tq)`.
    config_dict: PyObject,
    /// Monotonic order-ID counter shared across all callbacks.
    next_order_id: i64,
    /// Persistent adapter object — allocated once, reused each callback.
    cached_adapter: Option<Py<ZkQuantAdapter>>,
    /// The `balance_generation` value last synced into `cached_adapter`.
    cached_balance_gen: u64,
    /// Cached `zk_datamodel.rtmd.Kline` class — resolved once in `on_bar`.
    kline_cls: Option<PyObject>,
}

impl PyStrategyAdapter {
    pub fn new(py_strategy: PyObject, config_dict: PyObject) -> Self {
        Self {
            py_strategy,
            config_dict,
            next_order_id: 1_000,
            cached_adapter: None,
            cached_balance_gen: 0,
            kline_cls: None,
        }
    }

    /// Get-or-create the persistent adapter, refresh its mutable fields, then run `f`.
    ///
    /// The adapter is allocated once (via `Py::new`) and reused every subsequent call.
    /// Balances are only re-snapshotted when `ctx.balance_generation` has changed,
    /// which happens exclusively in `on_balance_update` — not on `on_bar`.
    fn with_adapter<F>(&mut self, py: Python<'_>, ctx: &StrategyContext, f: F) -> Vec<SAction>
    where
        F: FnOnce(&mut Self, Python<'_>, &Bound<'_, ZkQuantAdapter>) -> PyResult<()>,
    {
        // Allocate once on first call.
        if self.cached_adapter.is_none() {
            let adapter = ZkQuantAdapter::from_ctx(py, ctx, self.next_order_id);
            self.cached_balance_gen = ctx.balance_generation;
            self.cached_adapter = match Py::new(py, adapter) {
                Ok(a) => Some(a),
                Err(e) => {
                    e.print(py);
                    return vec![];
                }
            };
        }

        // clone_ref is a single atomic INCREF — no heap allocation.
        // It decouples the Py<> handle lifetime from &self so that f() can take &mut self.
        let adapter_py = self.cached_adapter.as_ref().unwrap().clone_ref(py);

        // Refresh ts + balances (if gen changed). Block ends before f() borrows &mut self.
        {
            let mut borrow = adapter_py.borrow_mut(py);
            self.cached_balance_gen =
                borrow.refresh(py, ctx, self.next_order_id, self.cached_balance_gen);
        }

        // Pass a Bound reference into the closure — the Py<> refcount is already held above.
        let bound = adapter_py.bind(py);
        if let Err(e) = f(self, py, &bound) {
            e.print(py);
        }

        let mut borrow = adapter_py.borrow_mut(py);
        self.next_order_id = borrow.next_order_id;
        borrow.drain_actions()
    }

    /// Try calling a Python method; silently skip if attribute not found.
    fn call_method(
        py: Python<'_>,
        strategy: &PyObject,
        name: &str,
        args: impl IntoPy<Py<pyo3::types::PyTuple>>,
    ) -> PyResult<()> {
        match strategy.call_method1(py, name, args) {
            Ok(_) => Ok(()),
            Err(e) if e.is_instance_of::<pyo3::exceptions::PyAttributeError>(py) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy trait implementation
// ---------------------------------------------------------------------------

impl Strategy for PyStrategyAdapter {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        Python::with_gil(|py| {
            self.with_adapter(py, ctx, |s, py, tq| {
                Self::call_method(py, &s.py_strategy, "on_create", (tq,))
            })
        })
    }

    fn on_reinit(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        Python::with_gil(|py| {
            let config = self.config_dict.clone_ref(py);
            self.with_adapter(py, ctx, |s, py, tq| {
                // Legacy ETS signature: on_reinit(config: dict, tq)
                Self::call_method(py, &s.py_strategy, "on_reinit", (config, tq))
            })
        })
    }

    fn on_bar(&mut self, bar: &Kline, ctx: &StrategyContext) -> Vec<SAction> {
        Python::with_gil(|py| {
            // Resolve and cache the Kline class on first call — avoids a module
            // import + attribute lookup on every bar (was ~2 Python ops per call).
            if self.kline_cls.is_none() {
                match py.import_bound("zk_datamodel.rtmd").and_then(|m| m.getattr("Kline")) {
                    Ok(cls) => { self.kline_cls = Some(cls.into()); }
                    Err(e) => { e.print(py); return vec![]; }
                }
            }
            let cls = self.kline_cls.as_ref().unwrap().bind(py);
            let py_bar = match make_py_kline_with_cls(py, &cls, bar) {
                Ok(b) => b,
                Err(e) => { e.print(py); return vec![]; }
            };
            self.with_adapter(py, ctx, |s, py, tq| {
                Self::call_method(py, &s.py_strategy, "on_bar", (py_bar, tq))
            })
        })
    }

    fn on_order_update(
        &mut self,
        oue: &OrderUpdateEvent,
        ctx: &StrategyContext,
    ) -> Vec<SAction> {
        Python::with_gil(|py| {
            // Serialize Rust proto → bytes → betterproto parse
            let py_oue =
                match rust_proto_to_py(py, oue, "zk_datamodel.oms", "OrderUpdateEvent") {
                    Ok(o) => o,
                    Err(e) => {
                        e.print(py);
                        return vec![];
                    }
                };
            self.with_adapter(py, ctx, |s, py, tq| {
                // Try ETS legacy name first, then canonical name
                if s.py_strategy
                    .getattr(py, "on_orderupdate")
                    .map(|a| !a.is_none(py))
                    .unwrap_or(false)
                {
                    Self::call_method(py, &s.py_strategy, "on_orderupdate", (py_oue, tq))
                } else {
                    Self::call_method(py, &s.py_strategy, "on_order_update", (py_oue, tq))
                }
            })
        })
    }

    fn on_timer(&mut self, event: &TimerEvent, ctx: &StrategyContext) -> Vec<SAction> {
        Python::with_gil(|py| {
            let py_te = match make_py_timer_event(py, event) {
                Ok(t) => t,
                Err(e) => {
                    e.print(py);
                    return vec![];
                }
            };
            self.with_adapter(py, ctx, |s, py, tq| {
                // ETS uses on_scheduled_time; fallback to on_timer
                if s.py_strategy
                    .getattr(py, "on_scheduled_time")
                    .map(|a| !a.is_none(py))
                    .unwrap_or(false)
                {
                    Self::call_method(py, &s.py_strategy, "on_scheduled_time", (py_te, tq))
                } else {
                    Self::call_method(py, &s.py_strategy, "on_timer", (py_te, tq))
                }
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Event conversion helpers
// ---------------------------------------------------------------------------

/// Build a Python `rtmd.Kline` from a pre-resolved class object.
///
/// Avoids `py.import_bound` + `getattr` on every call. The class is cached in
/// `PyStrategyAdapter::kline_cls` and passed in as a `Bound` reference.
fn make_py_kline_with_cls(_py: Python<'_>, cls: &Bound<'_, PyAny>, bar: &Kline) -> PyResult<PyObject> {
    let obj = cls.call0()?;
    obj.setattr("symbol", bar.symbol.as_str())?;
    obj.setattr("open", bar.open)?;
    obj.setattr("high", bar.high)?;
    obj.setattr("low", bar.low)?;
    obj.setattr("close", bar.close)?;
    obj.setattr("volume", bar.volume)?;
    obj.setattr("amount", bar.amount)?;
    obj.setattr("timestamp", bar.timestamp)?;
    obj.setattr("kline_end_timestamp", bar.kline_end_timestamp)?;
    Ok(obj.into())
}

/// Build a `zk_strategy.models.TimerEvent` instance.
///
/// ETS checks `isinstance(event, TimerEvent)` before reading `event.timer_key`,
/// so the object must be an actual `TimerEvent` — not a plain namespace.
fn make_py_timer_event(py: Python<'_>, event: &TimerEvent) -> PyResult<PyObject> {
    let m = py.import_bound("zk_strategy.models")?;
    let cls = m.getattr("TimerEvent")?;
    let kwargs = PyDict::new_bound(py);
    kwargs.set_item("timer_key", event.timer_key.as_str())?;
    let obj = cls.call((), Some(&kwargs))?;
    Ok(obj.into())
}
