use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde::{de::DeserializeOwned, Serialize};

use crate::py_errors::PyBridgeError;

/// Convert a serde-serializable Rust value to a Python object (dict/list).
pub fn to_py_object<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<PyObject> {
    let obj = pythonize::pythonize(py, value)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(obj.into())
}

/// Convert a Python object (dict/list) to a serde-deserializable Rust value.
pub fn from_py_object<T: DeserializeOwned>(obj: &Bound<'_, PyAny>) -> PyResult<T> {
    Ok(pythonize::depythonize(obj)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?)
}

/// Intermediate struct for decoding tagged events from Python.
///
/// Python returns events like:
/// ```json
/// {"event_type": "order_report", "payload_bytes": "<protobuf bytes>"}
/// {"event_type": "balance", "payload": [{"asset": "USDT", ...}]}
/// ```
#[derive(Debug, serde::Deserialize)]
pub struct RawPyEvent {
    pub event_type: String,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    pub payload_bytes: Option<Vec<u8>>,
}

/// Decode a Python event dict into a `VenueEvent`.
pub fn decode_venue_event(
    _py: Python<'_>,
    obj: &Bound<'_, PyAny>,
) -> Result<zk_gw_types::VenueEvent, PyBridgeError> {
    use prost::Message;
    use zk_gw_types::*;

    let raw: RawPyEvent = from_py_object(obj)
        .map_err(|e| PyBridgeError::ResponseDecode(format!("event decode: {e}")))?;

    match raw.event_type.as_str() {
        "order_report" => {
            let bytes = raw.payload_bytes.ok_or_else(|| {
                PyBridgeError::ResponseDecode(
                    "order_report event missing payload_bytes".to_string(),
                )
            })?;
            let report =
                zk_proto_rs::zk::exch_gw::v1::OrderReport::decode(bytes.as_slice()).map_err(
                    |e| PyBridgeError::ResponseDecode(format!("order_report proto decode: {e}")),
                )?;
            Ok(VenueEvent::OrderReport(report))
        }
        "balance" => {
            let payload = raw.payload.ok_or_else(|| {
                PyBridgeError::ResponseDecode("balance event missing payload".to_string())
            })?;
            let facts: Vec<VenueBalanceFact> = serde_json::from_value(payload)
                .map_err(|e| PyBridgeError::ResponseDecode(format!("balance decode: {e}")))?;
            Ok(VenueEvent::Balance(facts))
        }
        "position" => {
            let payload = raw.payload.ok_or_else(|| {
                PyBridgeError::ResponseDecode("position event missing payload".to_string())
            })?;
            let facts: Vec<VenuePositionFact> = serde_json::from_value(payload)
                .map_err(|e| PyBridgeError::ResponseDecode(format!("position decode: {e}")))?;
            Ok(VenueEvent::Position(facts))
        }
        "system" => {
            let payload = raw.payload.ok_or_else(|| {
                PyBridgeError::ResponseDecode("system event missing payload".to_string())
            })?;
            let event: VenueSystemEvent = serde_json::from_value(payload)
                .map_err(|e| PyBridgeError::ResponseDecode(format!("system decode: {e}")))?;
            Ok(VenueEvent::System(event))
        }
        other => Err(PyBridgeError::ResponseDecode(format!(
            "unknown venue event_type: '{other}'"
        ))),
    }
}

/// Decode a Python RTMD event dict into an `RtmdEvent`.
pub fn decode_rtmd_event(
    _py: Python<'_>,
    obj: &Bound<'_, PyAny>,
) -> Result<zk_rtmd_rs::types::RtmdEvent, PyBridgeError> {
    use prost::Message;
    use zk_proto_rs::zk::rtmd::v1;
    use zk_rtmd_rs::types::RtmdEvent;

    let raw: RawPyEvent = from_py_object(obj)
        .map_err(|e| PyBridgeError::ResponseDecode(format!("rtmd event decode: {e}")))?;

    let bytes = raw.payload_bytes.ok_or_else(|| {
        PyBridgeError::ResponseDecode(format!(
            "rtmd event '{}' missing payload_bytes",
            raw.event_type
        ))
    })?;

    match raw.event_type.as_str() {
        "tick" => {
            let data = v1::TickData::decode(bytes.as_slice())
                .map_err(|e| PyBridgeError::ResponseDecode(format!("tick proto decode: {e}")))?;
            Ok(RtmdEvent::Tick(data))
        }
        "kline" => {
            let data = v1::Kline::decode(bytes.as_slice())
                .map_err(|e| PyBridgeError::ResponseDecode(format!("kline proto decode: {e}")))?;
            Ok(RtmdEvent::Kline(data))
        }
        "orderbook" => {
            let data = v1::OrderBook::decode(bytes.as_slice()).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("orderbook proto decode: {e}"))
            })?;
            Ok(RtmdEvent::OrderBook(data))
        }
        "funding" => {
            let data = v1::FundingRate::decode(bytes.as_slice()).map_err(|e| {
                PyBridgeError::ResponseDecode(format!("funding proto decode: {e}"))
            })?;
            Ok(RtmdEvent::Funding(data))
        }
        other => Err(PyBridgeError::ResponseDecode(format!(
            "unknown rtmd event_type: '{other}'"
        ))),
    }
}

/// Decode proto bytes from a Python `bytes` object.
pub fn decode_proto_bytes<T: prost::Message + Default>(
    _py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    type_name: &str,
) -> Result<T, PyBridgeError> {
    let py_bytes: &Bound<'_, PyBytes> = obj.downcast().map_err(|e| {
        PyBridgeError::ResponseDecode(format!("{type_name}: expected bytes, got: {e}"))
    })?;
    let bytes = py_bytes.as_bytes();
    T::decode(bytes)
        .map_err(|e| PyBridgeError::ResponseDecode(format!("{type_name} proto decode: {e}")))
}
