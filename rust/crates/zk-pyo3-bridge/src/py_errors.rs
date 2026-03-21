use pyo3::types::{PyTracebackMethods, PyTypeMethods};
use pyo3::PyErr;
use zk_rtmd_rs::types::RtmdError;

/// Errors originating from the Python venue bridge.
#[derive(Debug, thiserror::Error)]
pub enum PyBridgeError {
    #[error("manifest load: {0}")]
    ManifestLoad(String),

    #[error("schema validation: {0}")]
    SchemaValidation(String),

    #[error("python import: {0}")]
    PythonImport(String),

    #[error("python call: {0}")]
    PythonCall(String),

    #[error("venue rejection: {0}")]
    VenueRejection(String),

    #[error("transient error: {0}")]
    TransientError(String),

    #[error("response decode: {0}")]
    ResponseDecode(String),

    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl From<PyErr> for PyBridgeError {
    fn from(err: PyErr) -> Self {
        pyo3::Python::with_gil(|py| {
            let type_name = err.get_type_bound(py).qualname().map(|n| n.to_string()).unwrap_or_default();
            let msg = format_py_error(py, &err);
            match type_name.as_str() {
                "ModuleNotFoundError" | "ImportError" => PyBridgeError::PythonImport(msg),
                "TypeError" | "ValueError" => PyBridgeError::SchemaValidation(msg),
                "VenueRejectionError" => PyBridgeError::VenueRejection(msg),
                "TransientError" => PyBridgeError::TransientError(msg),
                _ => PyBridgeError::PythonCall(msg),
            }
        })
    }
}

impl From<PyBridgeError> for RtmdError {
    fn from(err: PyBridgeError) -> Self {
        match err {
            PyBridgeError::VenueRejection(m)
            | PyBridgeError::TransientError(m)
            | PyBridgeError::PythonCall(m) => RtmdError::Venue(m),
            PyBridgeError::Unsupported(m) => RtmdError::Subscription(m),
            PyBridgeError::ResponseDecode(m)
            | PyBridgeError::PythonImport(m)
            | PyBridgeError::ManifestLoad(m)
            | PyBridgeError::SchemaValidation(m) => RtmdError::Internal(m),
        }
    }
}

/// Extract a readable error message including Python traceback.
pub fn format_py_error(py: pyo3::Python<'_>, err: &PyErr) -> String {
    let tb = err
        .traceback_bound(py)
        .and_then(|tb| tb.format().ok())
        .unwrap_or_default();
    if tb.is_empty() {
        err.to_string()
    } else {
        format!("{err}\n{tb}")
    }
}
