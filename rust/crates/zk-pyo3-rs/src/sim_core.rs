/// sim_core submodule — exposes Rust match-policy marker types to Python.
///
/// Python code that does:
///   import zk_backtest.sim_core as sim_core
///   backtest_config.match_policy_cls = sim_core.ImmediateMatchPolicy
///
/// will work because `ImmediateMatchPolicy` and `FirstComeFirstServedMatchPolicy`
/// are valid Python classes here, importable from `zk_backtest.sim_core`.
/// Their sole role is to serve as type tokens that `RustBacktester` recognises.
use pyo3::prelude::*;

#[pyclass(name = "ImmediateMatchPolicy")]
pub struct PyImmediateMatchPolicy;

#[pymethods]
impl PyImmediateMatchPolicy {
    #[new]
    fn new() -> Self {
        Self
    }
}

#[pyclass(name = "FirstComeFirstServedMatchPolicy")]
pub struct PyFcfsMatchPolicy;

#[pymethods]
impl PyFcfsMatchPolicy {
    #[new]
    fn new() -> Self {
        Self
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyImmediateMatchPolicy>()?;
    m.add_class::<PyFcfsMatchPolicy>()?;
    Ok(())
}
