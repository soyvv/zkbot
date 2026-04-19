/// `zk_backtest` — PyO3 extension module exposing the Rust backtester to Python.
///
/// Import in Python:
/// ```python
/// import zk_backtest
/// from zk_backtest import RustBacktester, ZkQuantAdapter
/// import zk_backtest.sim_core as sim_core
/// ```
///
/// Build via `make pyo3-wheel` (wraps `maturin build --release`). The wheel
/// is published to `dist/` and consumed through `[tool.uv.sources]` in the
/// workspace `pyproject.toml`. Editable/in-place builds are intentionally not
/// supported — see `docs/system-arch/dependency-contract.md`.
use pyo3::prelude::*;

mod adapter;
mod py_strategy;
mod runner;
mod sim_core;

pub use adapter::{ZkBalance, ZkQuantAdapter};
pub use py_strategy::PyStrategyAdapter;
pub use runner::RustBacktester;

#[pymodule]
fn zk_backtest(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Top-level classes
    m.add_class::<ZkQuantAdapter>()?;
    m.add_class::<ZkBalance>()?;
    m.add_class::<RustBacktester>()?;

    // sim_core submodule — mirrors `zk_simulator.sim_core` import path used by ETS
    let sim_core_mod = PyModule::new_bound(m.py(), "sim_core")?;
    sim_core::register(&sim_core_mod)?;
    m.add_submodule(&sim_core_mod)?;

    Ok(())
}
