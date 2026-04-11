use anyhow::{bail, Result};
use serde_json::from_str;
use strategy_smoke_test::SmokeMMConfig;
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

pub fn parse_strategy_spec(strategy_type_key: &str) -> Result<StrategySpec> {
    match strategy_type_key {
        "smoke-test" => Ok(StrategySpec::SmokeTest),
        "mm" => Ok(StrategySpec::Mm),
        "arbi" => Ok(StrategySpec::Arbi),
        "noop" | "" => Ok(StrategySpec::Noop),
        other => bail!("unknown strategy type key: {other}"),
    }
}

pub fn build_strategy(spec: &StrategySpec, config_json: Option<&str>) -> Result<Box<dyn Strategy>> {
    match spec {
        StrategySpec::Noop => Ok(Box::new(NoopStrategy::new())),
        StrategySpec::SmokeTest => {
            let strategy = match config_json.filter(|json| !json.trim().is_empty()) {
                Some(json) => SmokeTestStrategy::with_config(from_str::<SmokeMMConfig>(json)?),
                None => SmokeTestStrategy::new(),
            };
            Ok(Box::new(strategy))
        }
        StrategySpec::Mm => Ok(Box::new(MarketMakingStrategy::new())),
        StrategySpec::Arbi => Ok(Box::new(ArbitrageStrategy::new())),
        StrategySpec::Python(spec) => build_python_strategy(spec, config_json),
    }
}

#[cfg(feature = "python")]
fn build_python_strategy(spec: &PythonStrategySpec, config_json: Option<&str>) -> Result<Box<dyn Strategy>> {
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
        let effective_config_json = config_json.or(spec.config_json.as_deref());
        if let Some(config_json) = effective_config_json {
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
fn build_python_strategy(_spec: &PythonStrategySpec, _config_json: Option<&str>) -> Result<Box<dyn Strategy>> {
    bail!("python strategy support is disabled; enable zk-strategy-host-rs/python")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_test_uses_config_json() {
        let spec = StrategySpec::SmokeTest;
        let result = build_strategy(
            &spec,
            Some(r#"{"quote_interval_ms":1234,"quote_qty":0.25,"buy_tick_distance":2,"sell_tick_distance":4}"#),
        );
        assert!(result.is_ok());
    }
}
