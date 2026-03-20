/// RustBacktester — Python-callable wrapper around the Rust `Backtester`.
///
/// Usage from Python:
/// ```python
/// import zk_backtest
///
/// bt = zk_backtest.RustBacktester(
///     account_ids=[123],
///     refdata=[...],          # list of InstrumentRefData dicts
///     init_balances={123: {"USD": 1500.0}},
///     match_policy="immediate",    # "immediate" | "fcfs"
///     include_klines_in_result=True,  # optional, default False
/// )
///
/// # Pre-load kline events (one row per bar from Parquet fixture)
/// for _, row in df.iterrows():
///     bt.push_bar(row["timestamp"], row["symbol"],
///                 row["open"], row["high"], row["low"], row["close"], row["volume"])
///
/// # Instantiate the Python strategy class
/// from ETS_on_kline import ETS
/// strategy = ETS()
///
/// result = bt.run(strategy, config_dict, init_data_fetcher)
/// print(result["logs"])
/// ```
use pyo3::prelude::*;
use pyo3::types::PyDict;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use zk_backtest_rs::{
    backtester::{BacktestConfig, Backtester, InitDataFetcher},
    event_queue::BtEventKind,
    match_policy::{FirstComeFirstServedMatchPolicy, ImmediateMatchPolicy},
};
use zk_proto_rs::zk::{
    common::v1::{BuySellType, InstrumentRefData},
    rtmd::v1::{kline::KlineType, Kline},
};

use crate::py_strategy::PyStrategyAdapter;

// ---------------------------------------------------------------------------

#[pyclass(name = "RustBacktester")]
pub struct RustBacktester {
    backtester: Backtester,
    /// Bars accumulated via `push_bar`; flushed as a single sorted stream in `run()`.
    pending_bars: Vec<(i64, BtEventKind)>,
    /// When true, `push_bar` also records bars in `pending_klines` for result output.
    include_klines: bool,
    /// Kline rows captured when `include_klines` is true.
    /// Tuple: (ts_ms, symbol, open, high, low, close, volume)
    pending_klines: Vec<(i64, String, f64, f64, f64, f64, f64)>,
}

#[pymethods]
impl RustBacktester {
    /// Construct a new backtester.
    ///
    /// Args:
    ///   account_ids             : list[int]
    ///   refdata                 : list[dict]  — each dict has keys matching InstrumentRefData fields
    ///   init_balances           : dict[int, dict[str, float]]  account_id → {symbol: qty}
    ///   init_positions          : dict[int, dict[str, float]]  account_id → {instrument_code: qty}  (negative = short)
    ///   match_policy            : str  "immediate" (default) | "fcfs"
    ///   include_klines_in_result: bool  when True, pushed bars are included in the result dict
    #[new]
    #[pyo3(signature = (account_ids, refdata, init_balances=None, init_positions=None, match_policy="immediate", include_klines_in_result=false))]
    fn new(
        account_ids: Vec<i64>,
        refdata: Vec<Bound<'_, PyDict>>,
        init_balances: Option<Bound<'_, PyDict>>,
        init_positions: Option<Bound<'_, PyDict>>,
        match_policy: &str,
        include_klines_in_result: bool,
    ) -> PyResult<Self> {
        let ref_vec: Vec<InstrumentRefData> = refdata
            .iter()
            .map(|d| parse_refdata(d))
            .collect::<PyResult<_>>()?;

        let init_bals = init_balances.map(|d| parse_init_balances(&d)).transpose()?;

        let init_pos = init_positions
            .map(|d| parse_init_balances(&d))
            .transpose()?;

        let mp: Box<dyn zk_backtest_rs::match_policy::MatchPolicy> = match match_policy {
            "fcfs" => Box::new(FirstComeFirstServedMatchPolicy),
            _ => Box::new(ImmediateMatchPolicy),
        };

        let config = BacktestConfig {
            account_ids,
            refdata: ref_vec,
            init_balances: init_bals,
            init_positions: init_pos,
            match_policy: mp,
            init_data_fetcher: None,
            progress_callback: None,
        };

        Ok(Self {
            backtester: Backtester::new(config),
            pending_bars: Vec::new(),
            include_klines: include_klines_in_result,
            pending_klines: Vec::new(),
        })
    }

    /// Push one 1-minute bar event into the replay queue.
    ///
    /// `ts_ms` is the bar's open timestamp in unix milliseconds.
    /// `symbol` should be the *exchange* symbol (e.g. "USDJPY") matching
    /// the kline_symbols mapping used by ETS.
    fn push_bar(
        &mut self,
        ts_ms: i64,
        symbol: &str,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) {
        let kline = Kline {
            kline_type: KlineType::Kline1min as i32,
            symbol: symbol.to_string(),
            open,
            high,
            low,
            close,
            volume,
            amount: 0.0,
            timestamp: ts_ms,
            kline_end_timestamp: ts_ms,
            source: String::new(),
        };
        self.pending_bars.push((ts_ms, BtEventKind::Bar(kline)));

        if self.include_klines {
            self.pending_klines
                .push((ts_ms, symbol.to_string(), open, high, low, close, volume));
        }
    }

    /// Run the backtest.
    ///
    /// Args:
    ///   py_strategy        : the Python strategy instance (e.g. ETS())
    ///   config_dict        : dict passed as first arg to on_reinit(config, tq)
    ///   init_data_fetcher  : optional Python callable() → any; return value is
    ///                        accessible via tq.get_custom_init_data()
    ///   progress_callback  : optional Python callable(int) called at each 1% milestone
    ///
    /// Returns a dict with keys:
    ///   "order_placements" : list[dict]  (ts_ms, side, symbol, qty, price, order_id)
    ///   "logs"             : list[dict]  (ts_ms, message)
    ///   "trades"           : list[dict]  (ts_ms, symbol, side, price, qty)
    ///   "klines"           : list[dict] | None  (ts_ms, symbol, open, high, low, close, volume)
    ///   "meta"             : dict  (wall_clock timing, num_trades, num_orders, data range)
    #[pyo3(signature = (py_strategy, config_dict, init_data_fetcher=None, progress_callback=None))]
    fn run(
        &mut self,
        py: Python<'_>,
        py_strategy: PyObject,
        config_dict: PyObject,
        init_data_fetcher: Option<PyObject>,
        progress_callback: Option<PyObject>,
    ) -> PyResult<PyObject> {
        // Flush accumulated bars as a single sorted stream (O(n log n) sort once).
        let bars = std::mem::take(&mut self.pending_bars);
        if !bars.is_empty() {
            self.backtester.add_sorted_stream(bars);
        }

        // If a Python init_data_fetcher was provided, wrap it so the Rust
        // Backtester can call it between on_create and on_init.
        if let Some(py_fetcher) = init_data_fetcher {
            let fetcher: InitDataFetcher = Box::new(move |_ctx| {
                Python::with_gil(|py| {
                    let result = py_fetcher
                        .call0(py)
                        .expect("init_data_fetcher raised an exception");
                    Box::new(result) as Box<dyn std::any::Any + Send + 'static>
                })
            });
            self.backtester.set_init_data_fetcher(fetcher);
        }

        // Wrap Python progress callback (0–100 %).
        if let Some(py_cb) = progress_callback {
            let cb: Box<dyn Fn(u8) + Send + 'static> = Box::new(move |pct| {
                Python::with_gil(|py| {
                    py_cb.call1(py, (pct as u32,)).ok();
                });
            });
            self.backtester.set_progress_callback(cb);
        }

        let mut adapter = PyStrategyAdapter::new(py_strategy, config_dict);

        // Capture data range before run() borrows the backtester.
        let (data_start, data_end) = self.backtester.data_range_ms();

        let wall_start_ms = epoch_ms();
        let result = self.backtester.run(&mut adapter);
        let wall_end_ms = epoch_ms();

        // Serialize results to Python-friendly dicts
        let out = PyDict::new_bound(py);

        let placements = result
            .order_placements
            .iter()
            .map(|(ts, o)| {
                let d = PyDict::new_bound(py);
                d.set_item("ts_ms", ts)?;
                d.set_item("order_id", o.order_id)?;
                d.set_item("symbol", o.symbol.as_str())?;
                d.set_item("qty", o.qty)?;
                d.set_item("price", o.price)?;
                d.set_item(
                    "side",
                    if o.side == BuySellType::BsBuy as i32 {
                        "BUY"
                    } else {
                        "SELL"
                    },
                )?;
                Ok::<_, PyErr>(d.into_py(py))
            })
            .collect::<PyResult<Vec<_>>>()?;
        out.set_item("order_placements", placements)?;

        let logs = result
            .logs
            .iter()
            .map(|l| {
                let d = PyDict::new_bound(py);
                d.set_item("ts_ms", l.ts_ms)?;
                d.set_item("message", l.message.as_str())?;
                Ok::<_, PyErr>(d.into_py(py))
            })
            .collect::<PyResult<Vec<_>>>()?;
        out.set_item("logs", logs)?;

        // trades — extracted from OrderUpdateEvent.last_trade
        let num_trades = result.trades.len();
        let trades = result
            .trades
            .iter()
            .map(|t| {
                let d = PyDict::new_bound(py);
                d.set_item("ts_ms", t.filled_ts)?;
                d.set_item("symbol", t.instrument.as_str())?;
                d.set_item(
                    "side",
                    if t.buy_sell_type == BuySellType::BsBuy as i32 {
                        "BUY"
                    } else {
                        "SELL"
                    },
                )?;
                d.set_item("price", t.filled_price)?;
                d.set_item("qty", t.filled_qty)?;
                Ok::<_, PyErr>(d.into_py(py))
            })
            .collect::<PyResult<Vec<_>>>()?;
        out.set_item("trades", trades)?;

        // klines — only when include_klines_in_result=True
        let klines = self.pending_klines.take_as_py(py)?;
        out.set_item("klines", klines)?;

        // meta
        let num_orders = result.order_placements.len();
        let wall_runtime_s = (wall_end_ms - wall_start_ms) as f64 / 1000.0;

        let meta = PyDict::new_bound(py);
        meta.set_item("wall_clock_start_ms", wall_start_ms)?;
        meta.set_item("wall_clock_end_ms", wall_end_ms)?;
        meta.set_item("wall_clock_runtime_s", wall_runtime_s)?;
        meta.set_item("num_trades", num_trades)?;
        meta.set_item("num_orders", num_orders)?;
        meta.set_item("data_start_ms", data_start)?;
        meta.set_item("data_end_ms", data_end)?;
        out.set_item("meta", meta)?;

        Ok(out.into())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Extension trait to convert `Vec<(i64, String, ...)>` to `Option<Vec<PyObject>>` in one call.
trait KlineVecExt {
    fn take_as_py(&mut self, py: Python<'_>) -> PyResult<PyObject>;
}

impl KlineVecExt for Vec<(i64, String, f64, f64, f64, f64, f64)> {
    fn take_as_py(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        if self.is_empty() {
            return Ok(py.None());
        }
        let rows = std::mem::take(self)
            .into_iter()
            .map(|(ts_ms, symbol, open, high, low, close, volume)| {
                let d = PyDict::new_bound(py);
                d.set_item("ts_ms", ts_ms)?;
                d.set_item("symbol", symbol.as_str())?;
                d.set_item("open", open)?;
                d.set_item("high", high)?;
                d.set_item("low", low)?;
                d.set_item("close", close)?;
                d.set_item("volume", volume)?;
                Ok::<_, PyErr>(d.into_py(py))
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(rows.into_py(py))
    }
}

fn parse_refdata(d: &Bound<'_, PyDict>) -> PyResult<InstrumentRefData> {
    let get_str = |key: &str| -> PyResult<String> {
        d.get_item(key)?
            .map(|v| v.extract::<String>())
            .unwrap_or_else(|| Ok(String::new()))
    };
    let get_i32 = |key: &str| -> PyResult<i32> {
        Ok(d.get_item(key)?
            .map(|v| v.extract::<i32>())
            .transpose()?
            .unwrap_or(0))
    };
    Ok(InstrumentRefData {
        instrument_id: get_str("instrument_id")?,
        instrument_id_exchange: get_str("instrument_id_exchange")?,
        base_asset: get_str("base_asset")?,
        quote_asset: get_str("quote_asset")?,
        instrument_type: get_i32("instrument_type")?,
        exchange_name: get_str("exchange_name")?,
        price_precision: get_i32("price_precision")? as i64,
        qty_precision: get_i32("qty_precision")? as i64,
        ..Default::default()
    })
}

fn parse_init_balances(d: &Bound<'_, PyDict>) -> PyResult<HashMap<i64, HashMap<String, f64>>> {
    let mut out: HashMap<i64, HashMap<String, f64>> = HashMap::new();
    for (k, v) in d.iter() {
        let acc_id: i64 = k.extract()?;
        let inner: Bound<'_, PyDict> = v.extract()?;
        let mut inner_map: HashMap<String, f64> = HashMap::new();
        for (sym, qty) in inner.iter() {
            inner_map.insert(sym.extract()?, qty.extract()?);
        }
        out.insert(acc_id, inner_map);
    }
    Ok(out)
}
