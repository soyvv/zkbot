use std::sync::Arc;

use pyo3::prelude::*;

use crate::manifest::PythonEntrypoint;
use crate::py_errors::PyBridgeError;
use crate::py_event_loop::PyEventLoop;
use crate::py_types;

/// Handle to the shared Python runtime.
///
/// Wraps the process-global Python interpreter. Call `initialize()` once early
/// in `main()` before constructing any Python-backed adaptor.
///
/// Module resolution contract: the caller is responsible for launching this
/// process with `VIRTUAL_ENV` / `PYO3_PYTHON` pointing at a venv whose
/// `site-packages` already contains every venue adaptor module referenced by
/// manifests. This runtime does not mutate `sys.path`, probe venvs, or do any
/// path-based discovery — see `docs/system-arch/dependency-contract.md`.
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
    /// Initialize the Python interpreter.
    ///
    /// Honors `PYO3_PYTHON` at runtime by promoting it to `PYTHONEXECUTABLE`
    /// before `Py_Initialize` is called. With `PYTHONHOME` pinned to the base
    /// prefix (for stdlib), the embedded interpreter would otherwise miss the
    /// venv's `site-packages` because Python derives `sys.executable` from the
    /// host binary. Setting `PYTHONEXECUTABLE=<venv-python>` makes Python read
    /// the venv's `pyvenv.cfg` and load its `.pth` files at init time.
    pub fn initialize() -> Result<Self, PyBridgeError> {
        if std::env::var_os("PYTHONEXECUTABLE").is_none() {
            if let Some(pyo3_python) = std::env::var_os("PYO3_PYTHON") {
                std::env::set_var("PYTHONEXECUTABLE", pyo3_python);
            }
        }
        pyo3::prepare_freethreaded_python();
        Ok(PyRuntime { _marker: () })
    }

    /// Import a Python module by its fully-qualified dotted path and
    /// instantiate a class with the given config.
    ///
    /// The module must be importable from the active interpreter's
    /// `site-packages` — this method does not manipulate `sys.path`.
    pub fn load_class(
        &self,
        entrypoint: &PythonEntrypoint,
        config: serde_json::Value,
    ) -> Result<PyObjectHandle, PyBridgeError> {
        let event_loop = Arc::new(PyEventLoop::start()?);

        Python::with_gil(|py| {
            let importlib = py.import_bound("importlib").map_err(|e| {
                PyBridgeError::PythonImport(format!("failed to import importlib: {e}"))
            })?;
            let module = importlib
                .call_method1("import_module", (entrypoint.module_path.as_str(),))
                .map_err(|e| {
                    PyBridgeError::PythonImport(format!(
                        "failed to import module '{}': {e}. \
                         Ensure the venue package is installed in the active \
                         interpreter (VIRTUAL_ENV / PYO3_PYTHON) — run 'uv sync'.",
                        entrypoint.module_path
                    ))
                })?;

            let cls = module.getattr(entrypoint.class_name.as_str()).map_err(|e| {
                PyBridgeError::PythonImport(format!(
                    "class '{}' not found in module '{}': {e}",
                    entrypoint.class_name, entrypoint.module_path
                ))
            })?;

            let py_config = py_types::to_py_object(py, &config).map_err(|e| {
                PyBridgeError::SchemaValidation(format!("config conversion: {e}"))
            })?;

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
