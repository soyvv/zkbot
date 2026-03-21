use std::path::Path;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::manifest::PythonEntrypoint;
use crate::py_errors::PyBridgeError;
use crate::py_event_loop::PyEventLoop;
use crate::py_types;

/// Handle to the shared Python runtime.
///
/// Wraps the process-global Python interpreter. Call `initialize()` once early
/// in `main()` before constructing any Python-backed adaptor.
pub struct PyRuntime {
    _marker: (),
}

/// An instantiated Python adaptor object handle.
///
/// Carries both the Python object and a persistent event loop so that all
/// async method calls execute on the same loop, preserving adapter state.
pub struct PyObjectHandle {
    pub inner: Py<PyAny>,
    pub event_loop: Arc<PyEventLoop>,
}

impl std::fmt::Debug for PyObjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyObjectHandle")
            .field("inner", &"<Py<PyAny>>")
            .finish()
    }
}

impl PyRuntime {
    /// Initialize the Python interpreter and configure `sys.path`.
    ///
    /// `venue_root` should point to the `venue-integrations/` directory (or the
    /// test fixtures dir). Only `venue_root` itself is added to `sys.path` here.
    /// The venue-specific `<venue>/python/` directory is added later in
    /// `load_class()` so that flat module names (e.g. `gw`) resolve to the
    /// correct venue without cross-venue collisions.
    pub fn initialize(venue_root: &Path) -> Result<Self, PyBridgeError> {
        pyo3::prepare_freethreaded_python();

        Python::with_gil(|py| {
            let sys = py.import_bound("sys").map_err(|e| {
                PyBridgeError::PythonImport(format!("failed to import sys: {e}"))
            })?;
            let sys_path = sys
                .getattr("path")
                .map_err(|e| PyBridgeError::PythonImport(format!("failed to get sys.path: {e}")))?;
            let path: &Bound<'_, PyList> = sys_path
                .downcast::<PyList>()
                .map_err(|e| {
                    PyBridgeError::PythonImport(format!("sys.path is not a list: {e}"))
                })?;

            // Add venue_root itself so tests with flat fixtures still work.
            let root_str = venue_root.to_string_lossy().to_string();
            path.insert(0, root_str)
                .map_err(|e| PyBridgeError::PythonImport(format!("sys.path insert: {e}")))?;

            Ok(PyRuntime { _marker: () })
        })
    }

    /// Import a Python module and instantiate a class with the given config.
    ///
    /// A persistent asyncio event loop is created and attached to the returned
    /// handle so that all async calls share the same loop.
    ///
    /// `venue` is used to resolve the module path: manifest entrypoints use
    /// `<venue>.<module>` as a namespace (e.g. `oanda.gw`), but the actual file
    /// is at `<venue>/python/gw.py`. When `venue` is `Some`, the venue-specific
    /// `<venue>/python/` directory is prepended to `sys.path` and the venue
    /// prefix is stripped from the module name. This ensures each service loads
    /// from the correct venue directory without cross-venue collisions.
    ///
    /// Pass `None` for test fixtures or when the module path is already flat.
    pub fn load_class(
        &self,
        entrypoint: &PythonEntrypoint,
        config: serde_json::Value,
        venue: Option<&str>,
    ) -> Result<PyObjectHandle, PyBridgeError> {
        // Start a persistent event loop for this adapter instance.
        let event_loop = Arc::new(PyEventLoop::start()?);

        // Resolve the actual importable module path by stripping the venue prefix.
        let import_path = match venue {
            Some(v) => crate::manifest::resolve_module_path(entrypoint, v),
            None => entrypoint.module_path.clone(),
        };

        Python::with_gil(|py| {
            // When loading a real venue, prepend its python/ dir to sys.path so
            // that flat module names like `gw` resolve to this venue's files only.
            if let Some(v) = venue {
                let sys = py.import_bound("sys").map_err(|e| {
                    PyBridgeError::PythonImport(format!("failed to import sys: {e}"))
                })?;
                let sys_path = sys.getattr("path").map_err(|e| {
                    PyBridgeError::PythonImport(format!("failed to get sys.path: {e}"))
                })?;
                let path: &Bound<'_, PyList> = sys_path.downcast::<PyList>().map_err(|e| {
                    PyBridgeError::PythonImport(format!("sys.path is not a list: {e}"))
                })?;
                // venue_root is already at path[0]; derive venue python dir from it.
                let venue_root_str: String = path.get_item(0)
                    .map_err(|e| PyBridgeError::PythonImport(format!("sys.path empty: {e}")))?
                    .extract()
                    .map_err(|e| PyBridgeError::PythonImport(format!("sys.path[0] not str: {e}")))?;
                let py_dir = std::path::PathBuf::from(&venue_root_str)
                    .join(v)
                    .join("python");
                if py_dir.is_dir() {
                    let dir_str = py_dir.to_string_lossy().to_string();
                    path.insert(0, dir_str).map_err(|e| {
                        PyBridgeError::PythonImport(format!("sys.path insert: {e}"))
                    })?;
                }
            }

            // Import the module
            let importlib = py.import_bound("importlib").map_err(|e| {
                PyBridgeError::PythonImport(format!("failed to import importlib: {e}"))
            })?;
            let module = importlib
                .call_method1("import_module", (&import_path,))
                .map_err(|e| {
                    PyBridgeError::PythonImport(format!(
                        "failed to import module '{}' (resolved from '{}'): {e}",
                        import_path, entrypoint.module_path
                    ))
                })?;

            // Get the class
            let cls = module.getattr(entrypoint.class_name.as_str()).map_err(|e| {
                PyBridgeError::PythonImport(format!(
                    "class '{}' not found in module '{}': {e}",
                    entrypoint.class_name, entrypoint.module_path
                ))
            })?;

            // Convert config to Python dict
            let py_config = py_types::to_py_object(py, &config).map_err(|e| {
                PyBridgeError::SchemaValidation(format!("config conversion: {e}"))
            })?;

            // Instantiate
            let instance = cls.call1((py_config,)).map_err(|e| {
                PyBridgeError::SchemaValidation(format!(
                    "failed to instantiate {}::{}: {e}",
                    entrypoint.module_path, entrypoint.class_name
                ))
            })?;

            Ok(PyObjectHandle {
                inner: instance.unbind(),
                event_loop,
            })
        })
    }
}
