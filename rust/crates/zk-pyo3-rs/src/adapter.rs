/// ZkQuantAdapter — the Python-facing runtime context object.
///
/// Analogous to Python's `TokkaQuant`. Created fresh at each strategy callback,
/// wrapping a snapshot of the current `StrategyContext`. The Python strategy calls
/// methods like `tq.buy(...)`, `tq.get_account_balance(...)`, etc., and all
/// resulting actions are accumulated here. After the callback, the caller drains
/// `actions` and processes them via the Rust backtester.
use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use zk_proto_rs::zk::common::v1::BuySellType;
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, StrategyLog, StrategyOrder, TimerSchedule, TimerSubscription},
};

// ---------------------------------------------------------------------------
// ZkBalance — minimal position snapshot exposed to Python strategies
// ---------------------------------------------------------------------------

/// Per-asset balance snapshot returned by `ZkQuantAdapter.get_account_balance()`.
/// Exposes the fields from the `Balance` proto (cash/spot inventory).
#[pyclass(name = "ZkBalance")]
#[derive(Clone)]
pub struct ZkBalance {
    /// Total quantity held.
    #[pyo3(get)]
    pub total_qty: f64,
    /// Frozen (locked) quantity.
    #[pyo3(get)]
    pub frozen_qty: f64,
    /// Available quantity.
    #[pyo3(get)]
    pub avail_qty: f64,
}

// ---------------------------------------------------------------------------
// ZkQuantAdapter
// ---------------------------------------------------------------------------

/// The Python-facing runtime API provided to each strategy callback.
///
/// Built from a snapshot of `StrategyContext` at callback entry. The Python
/// strategy calls `tq.buy(...)`, `tq.log(...)`, etc. Resulting `SAction`s
/// are accumulated in `actions` and drained by the Rust caller after the
/// callback returns.
#[pyclass(name = "ZkQuantAdapter")]
pub struct ZkQuantAdapter {
    pub current_ts_ms: i64,
    /// account_id → symbol → balance snapshot
    balances: HashMap<i64, HashMap<String, ZkBalance>>,
    /// Python object returned by `init_data_fetcher` (or `None`).
    /// Retrieved via `ctx.get_init_data::<PyObject>()`.
    init_data: Option<PyObject>,
    /// Accumulated strategy actions, drained by the Rust caller.
    pub actions: Vec<SAction>,
    /// Monotonic order-ID counter; updated by `buy`/`sell` and read back by
    /// `PyStrategyAdapter` to keep IDs unique across callbacks.
    pub next_order_id: i64,
}

impl ZkQuantAdapter {
    /// Snapshot the parts of `StrategyContext` that Python queries need.
    pub fn from_ctx(py: Python<'_>, ctx: &StrategyContext, next_order_id: i64) -> Self {
        let mut balances: HashMap<i64, HashMap<String, ZkBalance>> = HashMap::new();
        for acc_id in ctx.account_ids() {
            if let Some(map) = ctx.get_balances_map(acc_id) {
                let acc_map = map
                    .iter()
                    .map(|(asset, bal)| {
                        (
                            asset.clone(),
                            ZkBalance {
                                total_qty: bal.total_qty,
                                frozen_qty: bal.frozen_qty,
                                avail_qty: bal.avail_qty,
                            },
                        )
                    })
                    .collect();
                balances.insert(acc_id, acc_map);
            }
        }

        // Init data is stored as `Py<PyAny>` in ctx (typed as `PyObject`).
        let init_data = ctx.get_init_data::<PyObject>().map(|obj| obj.clone_ref(py));

        Self {
            current_ts_ms: ctx.current_ts_ms,
            balances,
            init_data,
            actions: Vec::new(),
            next_order_id,
        }
    }

    /// Refresh mutable fields in place — reuse the existing Python object.
    ///
    /// Always updates `current_ts_ms` and `next_order_id`.
    /// Rebuilds `balances` only when `ctx.balance_generation != cached_gen`.
    /// Returns the new generation so the caller can store it.
    /// `init_data` is set once before `on_init` and never changes — no refresh needed.
    pub fn refresh(
        &mut self,
        _py: Python<'_>,
        ctx: &StrategyContext,
        next_order_id: i64,
        cached_gen: u64,
    ) -> u64 {
        self.current_ts_ms = ctx.current_ts_ms;
        self.next_order_id = next_order_id;
        // actions must be empty at entry (drained by caller after previous callback)

        if ctx.balance_generation != cached_gen {
            self.balances.clear();
            for acc_id in ctx.account_ids() {
                if let Some(map) = ctx.get_balances_map(acc_id) {
                    let acc_map = map
                        .iter()
                        .map(|(asset, bal)| {
                            (
                                asset.clone(),
                                ZkBalance {
                                    total_qty: bal.total_qty,
                                    frozen_qty: bal.frozen_qty,
                                    avail_qty: bal.avail_qty,
                                },
                            )
                        })
                        .collect();
                    self.balances.insert(acc_id, acc_map);
                }
            }
        }

        ctx.balance_generation
    }

    /// Take all accumulated actions out of the adapter.
    pub fn drain_actions(&mut self) -> Vec<SAction> {
        std::mem::take(&mut self.actions)
    }
}

#[pymethods]
impl ZkQuantAdapter {
    // -----------------------------------------------------------------------
    // Order placement
    // -----------------------------------------------------------------------

    /// Place a buy limit order. Returns the assigned `order_id`.
    ///
    /// Expected kwargs: account_id (int), symbol (str), qty (float), price (float).
    /// Any extra kwargs (e.g. IOC flags) are accepted and ignored for backtest.
    #[pyo3(signature = (**kwargs))]
    fn buy(&mut self, kwargs: Option<Bound<'_, PyDict>>) -> PyResult<i64> {
        let order = build_order(self, kwargs, BuySellType::BsBuy)?;
        let id = order.order_id;
        self.actions.push(SAction::PlaceOrder(order));
        self.next_order_id += 1;
        Ok(id)
    }

    /// Place a sell limit order. Returns the assigned `order_id`.
    #[pyo3(signature = (**kwargs))]
    fn sell(&mut self, kwargs: Option<Bound<'_, PyDict>>) -> PyResult<i64> {
        let order = build_order(self, kwargs, BuySellType::BsSell)?;
        let id = order.order_id;
        self.actions.push(SAction::PlaceOrder(order));
        self.next_order_id += 1;
        Ok(id)
    }

    // -----------------------------------------------------------------------
    // Context queries
    // -----------------------------------------------------------------------

    /// Return a `dict[symbol, ZkBalance]` for the given account.
    fn get_account_balance<'py>(
        &self,
        py: Python<'py>,
        account_id: i64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new_bound(py);
        if let Some(acc_map) = self.balances.get(&account_id) {
            for (sym, bal) in acc_map {
                dict.set_item(sym.as_str(), Py::new(py, bal.clone())?)?;
            }
        }
        Ok(dict)
    }

    /// Return the init data injected by `init_data_fetcher` (any Python object),
    /// or `None` if none was set. Mirrors `TokkaQuant.get_custom_init_data()`.
    fn get_custom_init_data<'py>(&self, py: Python<'py>) -> PyObject {
        match &self.init_data {
            Some(obj) => obj.clone_ref(py),
            None => py.None(),
        }
    }

    /// Return the current simulation time as a timezone-aware UTC `datetime`.
    /// Mirrors `TokkaQuant.get_current_ts()`.
    fn get_current_ts<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let seconds = self.current_ts_ms as f64 / 1_000.0;
        let datetime_mod = py.import_bound("datetime")?;
        let datetime_cls = datetime_mod.getattr("datetime")?;
        let utc = datetime_mod.getattr("timezone")?.getattr("utc")?;
        let dt = datetime_cls.call_method1("fromtimestamp", (seconds, utc))?;
        Ok(dt.into())
    }

    // -----------------------------------------------------------------------
    // Timer
    // -----------------------------------------------------------------------

    /// Schedule a one-shot timer to fire at `date_time` (Python `datetime`).
    /// Mirrors `TokkaQuant.set_timer_clock(timer_name, date_time)`.
    fn set_timer_clock(
        &mut self,
        timer_name: &str,
        date_time: Bound<'_, PyAny>,
    ) -> PyResult<()> {
        // `.timestamp()` returns float unix seconds (handles timezone-aware or naive)
        let secs: f64 = date_time.call_method0("timestamp")?.extract()?;
        let fire_ts_ms = (secs * 1_000.0) as i64;
        self.actions.push(SAction::SubscribeTimer(TimerSubscription {
            timer_key: timer_name.to_string(),
            schedule: TimerSchedule::OnceAt(fire_ts_ms),
        }));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Logging / notification
    // -----------------------------------------------------------------------

    /// Emit a log line. Mirrors `TokkaQuant.log(msg)`.
    fn log(&mut self, msg: &str) {
        self.actions.push(SAction::Log(StrategyLog {
            ts_ms: self.current_ts_ms,
            message: msg.to_string(),
        }));
    }

    /// No-op in backtest. Mirrors `TokkaQuant.send_notification(...)`.
    #[pyo3(signature = (*_args, **_kwargs))]
    fn send_notification(
        &self,
        _args: Bound<'_, PyTuple>,
        _kwargs: Option<Bound<'_, PyDict>>,
    ) {
        // intentionally ignored in backtest
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_order(
    adapter: &ZkQuantAdapter,
    kwargs: Option<Bound<'_, PyDict>>,
    side: BuySellType,
) -> PyResult<StrategyOrder> {
    let kw = kwargs.ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err("buy/sell requires keyword arguments")
    })?;

    let account_id: i64 = kw
        .get_item("account_id")?
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing account_id"))?
        .extract()?;
    let symbol: String = kw
        .get_item("symbol")?
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing symbol"))?
        .extract()?;
    let qty: f64 = kw
        .get_item("qty")?
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing qty"))?
        .extract()?;
    let price: f64 = kw
        .get_item("price")?
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("missing price"))?
        .extract()?;

    Ok(StrategyOrder {
        order_id: adapter.next_order_id,
        account_id,
        symbol,
        qty,
        price,
        side: side as i32,
    })
}

/// Serialize any prost `Message` to bytes, then call `cls.FromString(data)` in Python.
/// Returns the resulting Python betterproto object.
pub fn rust_proto_to_py<M: prost::Message>(
    py: Python<'_>,
    msg: &M,
    module: &str,
    cls_name: &str,
) -> PyResult<PyObject> {
    let bytes = prost::Message::encode_to_vec(msg);
    let m = py.import_bound(module)?;
    let cls = m.getattr(cls_name)?;
    let obj = cls.call_method1("FromString", (PyBytes::new_bound(py, &bytes),))?;
    Ok(obj.into())
}
