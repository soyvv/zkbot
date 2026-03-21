# Python Venue Adaptor Bridge — Design Spec

## Context

The zkbot architecture requires generic Rust service hosts (`zk-gw-svc`, `zk-rtmd-gw-svc`, refdata)
to load venue-specific adaptors that may be implemented in Python (OANDA, IBKR) or Rust (OKX).
Today the manifest system and venue stubs exist but the actual bridge layer is missing — all
`build_adapter()` factories only wire `"simulator"`. This spec defines the shared PyO3 bridge
that makes manifest-declared Python venue modules real.

Authoritative upstream design: `zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md`

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Bridge crate | New `zk-pyo3-bridge` (rlib) | `zk-pyo3-rs` is cdylib (maturin); mixing targets causes feature-flag conflicts (`extension-module` vs `auto-initialize`) |
| Trait extraction | New `zk-gw-types` (rlib) | `VenueAdapter` lives in binary crate `zk-gw-svc`; bridge can't depend on it |
| Async model | `pyo3-async-runtimes` (tokio) | Native tokio↔asyncio bridging; supports both async and sync Python adaptors |
| Type conversion | `pythonize` (serde ↔ Python dict) | Zero-boilerplate roundtrip; requires adding `Serialize`/`Deserialize` to venue types |
| Proto types in events | Protobuf bytes over the bridge | `OrderReport`, `TickData`, etc. are prost messages; Python serializes to bytes, Rust decodes |
| Host integration | Feature-gated `python-venue` | Pure-Rust build stays fast; Python embedding is opt-in |
| Scope | Bridge + stub refdata binary | Full refdata-svc deferred; stub binary proves end-to-end path |
| RTMD trait dep | Depend on `zk-rtmd-rs` directly | `zk-rtmd-rs` is already a lib crate (rlib); no extraction needed unlike `zk-gw-svc` (binary) |
| Proto over bridge | Bytes, not dicts | Justified deviation from plan's "start with dicts" — proto messages don't round-trip cleanly through JSON (`oneof`, nested messages, `bytes` fields) |

## Crate Architecture

```
zkbot/rust/crates/
  zk-gw-types/           # NEW — VenueAdapter trait + request/response types
  zk-pyo3-bridge/        # NEW — shared Python venue bridge
  zk-pyo3-rs/            # UNCHANGED — cdylib backtest bindings
  zk-gw-svc/             # MODIFIED — re-exports from zk-gw-types, manifest-driven dispatch
  zk-rtmd-rs/            # MODIFIED — build_adapter gets manifest-driven dispatch
  zk-rtmd-gw-svc/        # MODIFIED — PyRuntime::initialize() in main
```

### Dependency graph

```
zk-gw-types ← zk-gw-svc
            ← zk-pyo3-bridge ← zk-gw-svc [python-venue]
                              ← zk-rtmd-gw-svc [python-venue]
zk-rtmd-rs  ← zk-pyo3-bridge
zk-proto-rs ← zk-gw-types
            ← zk-pyo3-bridge
```

## 1. `zk-gw-types` — Trait Extraction

Move the entire contents of `zk-gw-svc/src/venue_adapter.rs` into this crate.

### Types to extract

**Trait:**
- `VenueAdapter` (async_trait, Send + Sync)

**Request types:**
- `VenuePlaceOrder`, `VenueCancelOrder`
- `VenueBalanceQuery`, `VenueOrderQuery`, `VenueOpenOrdersQuery`
- `VenueTradeQuery`, `VenueFundingFeeQuery`, `VenuePositionQuery`

**Response types:**
- `VenueCommandAck`
- `VenueBalanceFact`, `VenueOrderFact`, `VenueTradeFact`
- `VenueFundingFeeFact`, `VenuePositionFact`
- `VenueOrderStatus` (enum)
- `VenueSystemEvent`, `VenueSystemEventType` (enum)
- `VenueEvent` (enum: OrderReport, Balance, Position, System)
- `VenuePageDirection` (enum)

**Serde addition:** All types gain `#[derive(Serialize, Deserialize)]`. Enums use string
representation (`#[serde(rename_all = "snake_case")]` where appropriate).

**`VenueEvent` note:** The `OrderReport` variant contains `zk_proto_rs::zk::exch_gw::v1::OrderReport`
(prost message). This variant does NOT go through serde — it uses proto bytes over the bridge
(see section 4).

### Dependencies

```toml
[dependencies]
async-trait = "0.1"
anyhow = "1"
serde = { version = "1", features = ["derive"] }
zk-proto-rs = { path = "../zk-proto-rs" }
```

### Migration in `zk-gw-svc`

Replace `venue_adapter.rs` with:
```rust
pub use zk_gw_types::*;
```

Add `zk-gw-types = { path = "../zk-gw-types" }` to deps.

## 2. `zk-pyo3-bridge` — Module Layout

```
zk-pyo3-bridge/
  Cargo.toml
  src/
    lib.rs
    manifest.rs          # manifest parsing
    py_runtime.rs        # Python interpreter lifecycle
    py_types.rs          # serde ↔ Python dict conversion
    py_errors.rs         # error model
    gw_adapter.rs        # PyVenueAdapter → VenueAdapter
    rtmd_adapter.rs      # PyRtmdVenueAdapter → RtmdVenueAdapter
    refdata_adapter.rs   # RefdataLoader trait + PyRefdataLoader
    bin/
      refdata_stub.rs    # stub binary for end-to-end validation
  tests/
    fixtures/
      fake_refdata.py    # test Python refdata module
      fake_gw.py         # test Python gateway module
      fake_rtmd.py       # test Python RTMD module
    manifest_tests.rs
    type_roundtrip_tests.rs
    bridge_integration_tests.rs
```

### Dependencies

```toml
[dependencies]
pyo3 = { version = "0.22", features = ["auto-initialize"] }
pyo3-async-runtimes = { version = "0.22", features = ["tokio-runtime"] }
pythonize = "0.22"
tokio = { version = "1", features = ["sync", "rt-multi-thread"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
async-trait = "0.1"
thiserror = "2"
tracing = "0.1"
anyhow = "1"
prost = "0.13"
zk-gw-types = { path = "../zk-gw-types" }
zk-rtmd-rs = { path = "../zk-rtmd-rs" }
zk-proto-rs = { path = "../zk-proto-rs" }

[[bin]]
name = "refdata-stub"
path = "src/bin/refdata_stub.rs"
required-features = []  # always buildable; Python must be available at runtime
```

## 3. `manifest.rs` — Venue Manifest Parsing

### Types

```rust
#[derive(Debug, Deserialize)]
pub struct VenueManifest {
    pub venue: String,
    pub version: u32,
    pub capabilities: HashMap<String, CapabilityEntry>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CapabilityEntry {
    pub language: String,
    pub entrypoint: String,
    pub config_schema: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PythonEntrypoint {
    pub module_path: String,   // "oanda.gw"
    pub class_name: String,    // "OandaGatewayAdaptor"
}
```

### Functions

- `load_manifest(venue_root: &Path, venue: &str) -> Result<VenueManifest>`
  Reads `{venue_root}/{venue}/manifest.yaml`, deserializes.

- `resolve_capability(manifest: &VenueManifest, cap: &str) -> Result<&CapabilityEntry>`
  Looks up capability key ("gw", "rtmd", "refdata"), returns error if missing.

- `parse_python_entrypoint(raw: &str) -> Result<PythonEntrypoint>`
  Splits `"python:module.path:ClassName"` into parts. Rejects non-`python:` prefix.

Config schema validation deferred — just validate manifest structure for now.

Canonical capability keys: `"gw"`, `"rtmd"`, `"refdata"`. Define as constants to prevent
string-matching bugs:
```rust
pub const CAP_GW: &str = "gw";
pub const CAP_RTMD: &str = "rtmd";
pub const CAP_REFDATA: &str = "refdata";
```

## 4. `py_types.rs` — Type Conversion

### Helpers

```rust
/// Convert a serde-serializable Rust value to a Python object (dict/list).
pub fn to_py_object<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<PyObject> {
    Ok(pythonize::pythonize(py, value)?)
}

/// Convert a Python object (dict/list) to a serde-deserializable Rust value.
pub fn from_py_object<T: DeserializeOwned>(obj: &Bound<'_, PyAny>) -> PyResult<T> {
    Ok(pythonize::depythonize(obj)?)
}
```

### VenueEvent conversion (special)

Python returns a tagged dict. Proto-carrying variants use bytes:

```python
# order_report — proto bytes
{"event_type": "order_report", "payload_bytes": b"<protobuf>"}

# balance — plain dicts
{"event_type": "balance", "payload": [{"asset": "USDT", "total_qty": 1000.0, ...}]}

# position — plain dicts
{"event_type": "position", "payload": [{"instrument": "BTC-USDT-PERP", ...}]}

# system — plain dict
{"event_type": "system", "payload": {"event_type": "connected", "message": "...", "timestamp": 123}}
```

Rust-side intermediate:

```rust
#[derive(Deserialize)]
struct RawPyEvent {
    event_type: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    payload_bytes: Option<Vec<u8>>,
}
```

Dispatch on `event_type`:
- `"order_report"` → `prost::Message::decode(payload_bytes)` → `VenueEvent::OrderReport`
- `"balance"` → `serde_json::from_value(payload)` → `VenueEvent::Balance`
- `"position"` → `serde_json::from_value(payload)` → `VenueEvent::Position`
- `"system"` → `serde_json::from_value(payload)` → `VenueEvent::System`

### RTMD event conversion

Same proto-bytes approach for `RtmdEvent`:

```python
{"event_type": "tick", "payload_bytes": b"<TickData proto>"}
{"event_type": "kline", "payload_bytes": b"<Kline proto>"}
{"event_type": "orderbook", "payload_bytes": b"<OrderBook proto>"}
{"event_type": "funding", "payload_bytes": b"<FundingRate proto>"}
```

RTMD query responses (`query_current_tick`, `query_klines`, etc.) also return proto bytes that
Rust decodes.

## 5. `py_runtime.rs` — Python Interpreter Lifecycle

### Types

```rust
pub struct PyRuntime {
    _marker: (),
}

pub struct PyObjectHandle {
    pub inner: Py<PyAny>,
}
```

### API

```rust
impl PyRuntime {
    /// Initialize Python interpreter. Call once, early in main().
    /// Adds venue_root to sys.path for module imports.
    pub fn initialize(venue_root: &Path) -> Result<Self, PyBridgeError>;

    /// Import a Python class and instantiate it with config.
    pub fn load_class(
        &self,
        entrypoint: &PythonEntrypoint,
        config: serde_json::Value,
    ) -> Result<PyObjectHandle, PyBridgeError>;
}
```

### Implementation notes

- `pyo3::prepare_freethreaded_python()` called in `initialize()` — safe to call multiple times
  (no-op after first)
- `sys.path` configured to include the venue root (e.g., `venue-integrations/`) so that
  Python imports like `okx.refdata` resolve to `venue-integrations/okx/python/refdata.py`
  (each venue's `python/` dir is added as a package root)
- `load_class` acquires GIL, calls `importlib.import_module(module_path)`,
  gets `getattr(module, class_name)`, calls `cls(config_dict)`
- The `config` JSON value is converted to a Python dict via `pythonize`

## 6. `py_errors.rs` — Error Model

```rust
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
```

Python exception mapping:
- `ModuleNotFoundError`, `ImportError` → `PythonImport`
- `TypeError`, `ValueError` during construction → `SchemaValidation`
- Runtime `Exception` during method call → `PythonCall`
- Python raises `VenueRejectionError` (custom) → `VenueRejection` (permanent venue-side rejection)
- Python raises `TransientError` (custom, e.g. timeout/connection) → `TransientError` (retriable)
- Conversion failure from Python result → `ResponseDecode`

Python-side convention: venue adaptors should raise `VenueRejectionError` for permanent rejections
(e.g., invalid symbol, insufficient margin) and `TransientError` for retriable conditions (e.g.,
rate limit, timeout). The bridge detects these by exception class name. All other exceptions map
to `PythonCall`.

`PyBridgeError` → `RtmdError` conversion for the RTMD adapter:
- `VenueRejection`, `TransientError`, `PythonCall` → `RtmdError::Venue`
- `Unsupported` → `RtmdError::Subscription`
- `ResponseDecode` → `RtmdError::Internal`
- `PythonImport`, `ManifestLoad`, `SchemaValidation` → `RtmdError::Internal`

Helper: `format_py_error(py, err) -> String` extracts Python traceback for tracing logs.

## 7. `gw_adapter.rs` — PyVenueAdapter

```rust
pub struct PyVenueAdapter {
    obj: Py<PyAny>,
}

impl PyVenueAdapter {
    pub fn new(handle: PyObjectHandle) -> Self {
        Self { obj: handle.inner }
    }
}
```

### Async bridging pattern (all methods follow this)

```rust
async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck> {
    let obj = self.obj.clone();

    // 1. GIL: convert request, get coroutine
    let future = Python::with_gil(|py| -> PyResult<_> {
        let py_req = py_types::to_py_object(py, &req)?;
        let coro = obj.call_method1(py, "place_order", (py_req,))?;
        pyo3_async_runtimes::tokio::into_future(coro.into_bound(py))
    }).map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;

    // 2. Await OUTSIDE GIL
    let py_result = future.await
        .map_err(|e| PyBridgeError::PythonCall(e.to_string()))?;

    // 3. GIL: convert response
    Python::with_gil(|py| {
        py_types::from_py_object(py_result.bind(py))
    }).map_err(|e| PyBridgeError::ResponseDecode(e.to_string()).into())
}
```

### Sync/async detection

Helper that tries `into_future` first, falls back to direct value:

```rust
// Note: IntoPy is deprecated in pyo3 0.22 (migrate to IntoPyObject when upgrading pyo3).
async fn call_method<T: DeserializeOwned>(
    obj: &Py<PyAny>,
    method: &str,
    args: impl IntoPy<Py<PyTuple>> + Send,
) -> Result<T, PyBridgeError> {
    let (future_opt, value_opt) = Python::with_gil(|py| -> PyResult<_> {
        let result = obj.call_method1(py, method, args)?;
        match pyo3_async_runtimes::tokio::into_future(result.into_bound(py)) {
            Ok(fut) => Ok((Some(fut), None)),
            Err(_) => Ok((None, Some(result))),
        }
    })?;

    let py_obj = match future_opt {
        Some(fut) => fut.await?,
        None => value_opt.unwrap(),
    };

    Python::with_gil(|py| py_types::from_py_object(py_obj.bind(py)))
        .map_err(|e| PyBridgeError::ResponseDecode(e.to_string()))
}
```

**Timeout protection:** All Python coroutine awaits should be wrapped with
`tokio::time::timeout(duration, future)` using a configurable default (e.g., 30s for queries,
5min for `next_event`). This prevents hung Python coroutines from blocking the host indefinitely.

### `next_event()` — special

Uses the same pattern but decodes via `RawPyEvent` dispatch (section 4) instead of direct
`from_py_object<VenueEvent>`.

## 8. `rtmd_adapter.rs` — PyRtmdVenueAdapter

Same pattern as `gw_adapter.rs`. Key differences:

- `instrument_exch_for(&self, instrument_code: &str) -> Option<String>` is sync —
  uses `Python::with_gil` directly, no coroutine
- `next_event()` decodes via RTMD proto-bytes dispatch
- Query methods return proto bytes that are decoded with `prost::Message::decode`
- Error type is `RtmdError`, not `anyhow::Error` — bridge maps `PyBridgeError` → `RtmdError`
  (see section 6 error mapping table)

**RTMD serde prerequisite:** `RtmdSubscriptionSpec`, `StreamKey`, and `ChannelType` need
`#[derive(Serialize, Deserialize)]` added in `zk-rtmd-rs`. `ChannelType` serialization:
```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelType {
    Tick,
    Kline { interval: String },
    OrderBook { depth: Option<u32> },
    Funding,
}
```

Python `subscribe`/`unsubscribe` receive dicts like:
```python
{"stream_key": {"instrument_code": "BTC-USDT", "channel": {"type": "tick"}},
 "instrument_exch": "BTC-USDT", "venue": "okx"}
```

## 9. `refdata_adapter.rs` — RefdataLoader

### New trait

```rust
#[async_trait]
pub trait RefdataLoader: Send + Sync {
    async fn load_instruments(&self) -> anyhow::Result<Vec<RefdataInstrumentRow>>;
    async fn load_market_sessions(&self) -> anyhow::Result<Vec<serde_json::Value>>;
}
```

`RefdataInstrumentRow` is a flat serde struct matching the fields Python loaders return.
It maps to `InstrumentRefData` proto but doesn't depend on prost — the service host does
the proto conversion.

### PyRefdataLoader

Same async bridging pattern. Python returns `list[dict]`, Rust deserializes to
`Vec<RefdataInstrumentRow>`.

### Stub binary

```rust
// src/bin/refdata_stub.rs
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let venue = std::env::args().nth(1).expect("usage: refdata-stub <venue>");
    let venue_root = PathBuf::from("venue-integrations");
    let rt = PyRuntime::initialize(&venue_root)?;
    let manifest = manifest::load_manifest(&venue_root, &venue)?;
    let cap = manifest::resolve_capability(&manifest, "refdata")?;
    let ep = manifest::parse_python_entrypoint(&cap.entrypoint)?;
    let handle = rt.load_class(&ep, serde_json::json!({}))?;
    let loader = PyRefdataLoader::new(handle);
    let instruments = loader.load_instruments().await?;
    println!("{}", serde_json::to_string_pretty(&instruments)?);
    Ok(())
}
```

## 10. Host Integration

### `zk-gw-svc`

**Cargo.toml:**
```toml
[features]
python-venue = ["zk-pyo3-bridge"]

[dependencies]
zk-gw-types = { path = "../zk-gw-types" }
zk-pyo3-bridge = { path = "../zk-pyo3-bridge", optional = true }
```

**`src/venue/mod.rs` update:**
```rust
pub async fn build_adapter(cfg: &GwSvcConfig) -> anyhow::Result<BuiltVenue> {
    // Try manifest-driven loading first
    if let Some(venue_root) = &cfg.venue_root {
        let manifest = zk_pyo3_bridge::manifest::load_manifest(venue_root, &cfg.venue)?;
        let cap = zk_pyo3_bridge::manifest::resolve_capability(&manifest, "gw")?;
        match cap.language.as_str() {
            "python" => {
                #[cfg(feature = "python-venue")]
                {
                    let ep = zk_pyo3_bridge::manifest::parse_python_entrypoint(&cap.entrypoint)?;
                    let handle = runtime.load_class(&ep, cfg.venue_config.clone())?;
                    let adapter = Arc::new(PyVenueAdapter::new(handle));
                    return Ok(BuiltVenue { adapter, simulator_handles: None });
                }
                #[cfg(not(feature = "python-venue"))]
                return Err(anyhow::anyhow!("python venue support not compiled"));
            }
            "rust" => { /* fall through to match below */ }
            other => return Err(anyhow::anyhow!("unsupported language: {other}")),
        }
    }
    // Fallback: hard-coded match (simulator, etc.)
    match cfg.venue.as_str() {
        "simulator" => { /* existing code */ }
        other => Err(anyhow::anyhow!("unsupported venue: {other}")),
    }
}
```

**`src/main.rs`:** Add `PyRuntime::initialize()` before `build_adapter()` when
`python-venue` feature is enabled.

### `zk-rtmd-gw-svc`

Same pattern — update `zk-rtmd-rs/src/venue/mod.rs` `build_adapter()` with manifest check.

### New config

Env var `ZK_VENUE_ROOT` → path to `venue-integrations/` directory. Added to service configs.

## 11. Python-Side Contract

Python venue modules follow a stable object contract. They exchange plain dicts and proto bytes.
They do not need to know about PyO3.

### Gateway adaptor

```python
class SomeGatewayAdaptor:
    def __init__(self, config: dict): ...
    async def connect(self) -> None: ...
    async def disconnect(self) -> None: ...  # clean shutdown: close connections, cancel tasks
    async def place_order(self, req: dict) -> dict: ...
    async def cancel_order(self, req: dict) -> dict: ...
    async def query_balance(self, req: dict) -> list[dict]: ...
    async def query_order(self, req: dict) -> list[dict]: ...
    async def query_open_orders(self, req: dict) -> list[dict]: ...
    async def query_trades(self, req: dict) -> list[dict]: ...
    async def query_funding_fees(self, req: dict) -> list[dict]: ...
    async def query_positions(self, req: dict) -> list[dict]: ...
    async def next_event(self) -> dict: ...
```

### RTMD adaptor

```python
class SomeRtmdAdaptor:
    def __init__(self, config: dict): ...
    async def connect(self) -> None: ...
    async def disconnect(self) -> None: ...  # clean shutdown
    async def subscribe(self, req: dict) -> None: ...
    async def unsubscribe(self, req: dict) -> None: ...
    async def snapshot_active(self) -> list[dict]: ...
    def instrument_exch_for(self, instrument_code: str) -> str | None: ...
    async def query_current_tick(self, instrument_code: str) -> bytes: ...
    async def query_current_orderbook(self, instrument_code: str, depth: int | None = None) -> bytes: ...
    async def query_current_funding(self, instrument_code: str) -> bytes: ...
    async def query_klines(self, instrument_code: str, interval: str, limit: int,
                           from_ms: int | None = None, to_ms: int | None = None) -> list[bytes]: ...
    async def next_event(self) -> dict: ...
```

### Refdata loader

```python
class SomeRefdataLoader:
    def __init__(self, config: dict): ...
    async def load_instruments(self) -> list[dict]: ...
    async def load_market_sessions(self) -> list[dict]: ...
```

## 12. Testing Strategy

### Unit tests

| Test | What it verifies |
|------|-----------------|
| `test_manifest_parse_okx` | Parses real OKX manifest, validates capabilities |
| `test_manifest_parse_ibkr` | Parses real IBKR manifest (all-python) |
| `test_entrypoint_parse_valid` | `"python:oanda.gw:OandaGatewayAdaptor"` → correct parts |
| `test_entrypoint_parse_rejects_malformed` | Missing prefix, wrong delimiter |
| `test_type_roundtrip_place_order` | `VenuePlaceOrder` → Python dict → `VenuePlaceOrder` |
| `test_type_roundtrip_command_ack` | `VenueCommandAck` round-trip |
| `test_type_roundtrip_balance_fact` | `VenueBalanceFact` round-trip |
| `test_event_decode_order_report` | Proto bytes → `VenueEvent::OrderReport` |
| `test_event_decode_balance` | Dict payload → `VenueEvent::Balance` |
| `test_event_decode_rejects_unknown_type` | Unknown `event_type` → error |
| `test_error_mapping_import` | Python `ImportError` → `PyBridgeError::PythonImport` |
| `test_error_mapping_runtime` | Python `RuntimeError` → `PyBridgeError::PythonCall` |

### Integration tests

| Test | What it verifies |
|------|-----------------|
| `test_py_runtime_imports_fixture_class` | Loads `FakeRefdataLoader` from test fixtures |
| `test_refdata_loader_end_to_end` | Manifest → PyRefdataLoader → `load_instruments` returns data |
| `test_gw_adapter_place_order_smoke` | FakeGw → `place_order` → returns ack dict |
| `test_rtmd_adapter_subscribe_smoke` | FakeRtmd → `subscribe` → no error |
| `test_async_python_method` | Calls an async Python method, awaits result |
| `test_sync_python_fallback` | Calls a sync Python method through async bridge |
| `test_gw_adapter_python_exception_maps_to_error` | Python raises during `place_order` → `PyBridgeError::PythonCall` |
| `test_py_runtime_bad_module_returns_import_error` | Non-existent module → `PyBridgeError::PythonImport` |
| `test_manifest_missing_capability_returns_error` | Request `"gw"` from refdata-only manifest → error |

### Test fixtures

Small Python files in `tests/fixtures/` implementing the contract with hardcoded returns.

## 13. Implementation Order

| Step | Crate | Work | Depends on |
|------|-------|------|-----------|
| 1 | `zk-gw-types` | Extract trait + types, add serde derives | — |
| 2 | `zk-pyo3-bridge` | Skeleton: Cargo.toml, empty modules, workspace member | Step 1 |
| 3 | `zk-pyo3-bridge` | `py_errors.rs` + `manifest.rs` + tests | Step 2 |
| 4 | `zk-pyo3-bridge` | `py_types.rs` + roundtrip tests | Step 2 |
| 5 | `zk-pyo3-bridge` | `py_runtime.rs` + import test | Step 3 |
| 6 | `zk-pyo3-bridge` | `refdata_adapter.rs` + stub binary + fixture | Steps 4, 5 |
| 7 | `zk-pyo3-bridge` | `gw_adapter.rs` + fixture + tests | Steps 4, 5 |
| 8 | `zk-pyo3-bridge` | `rtmd_adapter.rs` + fixture + tests | Steps 4, 5 |
| 9 | `zk-gw-svc` | Feature-gated host integration | Step 7 |
| 10 | `zk-rtmd-gw-svc` | Feature-gated host integration | Step 8 |

Steps 3+4 are parallelizable. Steps 6+7+8 are parallelizable after 4+5 complete.

## 14. Verification

- [ ] `cargo build -p zk-gw-types` compiles
- [ ] `cargo build -p zk-gw-svc` compiles (re-exports from zk-gw-types)
- [ ] `cargo test -p zk-gw-svc` passes (no regressions)
- [ ] `cargo build -p zk-pyo3-bridge` compiles
- [ ] `cargo test -p zk-pyo3-bridge` — all unit + integration tests pass
- [ ] `cargo run --bin refdata-stub -- okx` loads OKX Python refdata, prints instruments
- [ ] `cargo build -p zk-gw-svc --features python-venue` compiles
- [ ] `cargo build -p zk-rtmd-gw-svc --features python-venue` compiles
- [ ] Existing simulator tests in gw-svc and rtmd-gw-svc still pass (no regressions)
