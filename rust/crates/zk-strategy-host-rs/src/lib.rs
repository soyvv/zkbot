use anyhow::{bail, Result};
use strategy_smoke_test::SmokeTestStrategy;
use zk_strategy_arbi_rs::ArbitrageStrategy;
use zk_strategy_mm_rs::MarketMakingStrategy;
use zk_strategy_noop_rs::NoopStrategy;
use zk_strategy_sdk_rs::strategy::Strategy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategySpec {
    Noop,
    SmokeTest,
    Mm,
    Arbi,
    Python(PythonStrategySpec),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonStrategySpec {
    pub module: String,
    pub class_name: String,
    pub search_path: Option<String>,
    pub config_json: Option<String>,
}

pub fn available_rust_strategies() -> &'static [&'static str] {
    &["noop", "smoke-test", "mm", "arbi"]
}

pub fn build_strategy(spec: &StrategySpec) -> Result<Box<dyn Strategy>> {
    match spec {
        StrategySpec::Noop => Ok(Box::new(NoopStrategy::new())),
        StrategySpec::SmokeTest => Ok(Box::new(SmokeTestStrategy::new())),
        StrategySpec::Mm => Ok(Box::new(MarketMakingStrategy::new())),
        StrategySpec::Arbi => Ok(Box::new(ArbitrageStrategy::new())),
        StrategySpec::Python(spec) => build_python_strategy(spec),
    }
}

#[cfg(feature = "python")]
fn build_python_strategy(spec: &PythonStrategySpec) -> Result<Box<dyn Strategy>> {
    use pyo3::prelude::*;
    use pyo3::types::PyDict;
    use serde_json::Value;
    use zk_backtest::PyStrategyAdapter;

    Python::with_gil(|py| -> Result<Box<dyn Strategy>> {
        if let Some(path) = &spec.search_path {
            let sys = py.import_bound("sys")?;
            let sys_path = sys.getattr("path")?;
            sys_path.call_method1("insert", (0, path.as_str()))?;
        }

        let module = py.import_bound(spec.module.as_str())?;
        let cls = module.getattr(spec.class_name.as_str())?;
        let instance = cls.call0()?;

        let config = PyDict::new_bound(py);
        if let Some(config_json) = &spec.config_json {
            let parsed: Value = serde_json::from_str(config_json)?;
            if let Some(obj) = parsed.as_object() {
                for (key, value) in obj {
                    config.set_item(key, value.to_string())?;
                }
            }
        }

        Ok(Box::new(PyStrategyAdapter::new(
            instance.into(),
            config.into(),
        )))
    })
}

#[cfg(not(feature = "python"))]
fn build_python_strategy(_spec: &PythonStrategySpec) -> Result<Box<dyn Strategy>> {
    bail!("python strategy support is disabled; enable zk-strategy-host-rs/python")
}
