use anyhow::{anyhow, bail, Result};
use serde_json::from_str;
use serde_json::Value;
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
        "python-wrapper" => Ok(StrategySpec::Python(PythonStrategySpec {
            module: String::new(),
            class_name: String::new(),
            search_path: None,
            config_json: None,
        })),
        "noop" | "" => Ok(StrategySpec::Noop),
        other if other.starts_with("python:") => parse_python_entrypoint(other),
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
    use zk_backtest::PyStrategyAdapter;

    pyo3::prepare_freethreaded_python();

    Python::with_gil(|py| -> Result<Box<dyn Strategy>> {
        let resolved = resolve_python_strategy_spec(spec, config_json)?;

        if let Some(path) = &resolved.search_path {
            let sys = py.import_bound("sys")?;
            let sys_path = sys.getattr("path")?;
            sys_path.call_method1("insert", (0, path.as_str()))?;
        }

        let module = py.import_bound(resolved.module.as_str())?;
        let cls = module.getattr(resolved.class_name.as_str())?;
        let instance = cls.call0()?;

        let config = py_config_dict(py, &resolved.strategy_config)?;

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

fn parse_python_entrypoint(strategy_type_key: &str) -> Result<StrategySpec> {
    let raw = strategy_type_key
        .strip_prefix("python:")
        .ok_or_else(|| anyhow!("invalid python entrypoint: {strategy_type_key}"))?;
    let mut parts = raw.split(':');
    let module = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("python strategy entrypoint is missing module: {strategy_type_key}"))?;
    let class_name = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("python strategy entrypoint is missing class: {strategy_type_key}"))?;
    if parts.next().is_some() {
        bail!("invalid python strategy entrypoint: {strategy_type_key}");
    }

    Ok(StrategySpec::Python(PythonStrategySpec {
        module: module.to_string(),
        class_name: class_name.to_string(),
        search_path: None,
        config_json: None,
    }))
}

#[derive(Debug)]
struct ResolvedPythonStrategySpec {
    module: String,
    class_name: String,
    search_path: Option<String>,
    strategy_config: Value,
}

fn resolve_python_strategy_spec(
    spec: &PythonStrategySpec,
    config_json: Option<&str>,
) -> Result<ResolvedPythonStrategySpec> {
    let effective_config_json = config_json.or(spec.config_json.as_deref());
    let parsed = match effective_config_json {
        Some(json) if !json.trim().is_empty() => serde_json::from_str::<Value>(json)?,
        _ => Value::Null,
    };

    // `python-wrapper` keeps module/class in the outer wrapper config and passes the
    // nested `python_strategy_config` through to the wrapped strategy instance.
    if spec.module.is_empty() || spec.class_name.is_empty() {
        let obj = parsed
            .as_object()
            .ok_or_else(|| anyhow!("python-wrapper config must be a JSON object"))?;
        let module = obj
            .get("python_module")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("python-wrapper config is missing python_module"))?;
        let class_name = obj
            .get("python_class")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("python-wrapper config is missing python_class"))?;
        let search_path = obj
            .get("python_search_path")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let strategy_config = obj
            .get("python_strategy_config")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));

        return Ok(ResolvedPythonStrategySpec {
            module: module.to_string(),
            class_name: class_name.to_string(),
            search_path,
            strategy_config,
        });
    }

    Ok(ResolvedPythonStrategySpec {
        module: spec.module.clone(),
        class_name: spec.class_name.clone(),
        search_path: spec.search_path.clone(),
        strategy_config: parsed,
    })
}

#[cfg(feature = "python")]
fn value_to_py(py: pyo3::Python<'_>, value: &Value) -> Result<Option<pyo3::PyObject>> {
    use pyo3::types::{PyDict, PyDictMethods, PyList, PyListMethods};
    use pyo3::IntoPy;

    let obj = match value {
        Value::Null => return Ok(None),
        Value::Bool(v) => v.into_py(py),
        Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                i.into_py(py)
            } else if let Some(u) = v.as_u64() {
                u.into_py(py)
            } else if let Some(f) = v.as_f64() {
                f.into_py(py)
            } else {
                return Err(anyhow!("unsupported JSON number: {v}"));
            }
        }
        Value::String(v) => v.into_py(py),
        Value::Array(values) => {
            let list = PyList::empty_bound(py);
            for value in values {
                match value_to_py(py, value)? {
                    Some(item) => list.append(item)?,
                    None => list.append(py.None())?,
                }
            }
            list.into_py(py)
        }
        Value::Object(values) => {
            let dict = PyDict::new_bound(py);
            for (key, value) in values {
                match value_to_py(py, value)? {
                    Some(item) => dict.set_item(key, item)?,
                    None => dict.set_item(key, py.None())?,
                }
            }
            dict.into_py(py)
        }
    };
    Ok(Some(obj))
}

#[cfg(feature = "python")]
fn py_config_dict<'py>(
    py: pyo3::Python<'py>,
    value: &Value,
) -> Result<pyo3::Bound<'py, pyo3::types::PyDict>> {
    use pyo3::types::{PyDict, PyDictMethods};

    let dict = PyDict::new_bound(py);
    match value {
        Value::Null => {}
        Value::Object(values) => {
            for (key, value) in values {
                match value_to_py(py, value)? {
                    Some(item) => dict.set_item(key, item)?,
                    None => dict.set_item(key, py.None())?,
                }
            }
        }
        _ => bail!("python strategy config must be a JSON object"),
    }
    Ok(dict)
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
