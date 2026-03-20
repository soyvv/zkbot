# Phase 12A: Python Venue Adaptor Bridge

## Goal

Implement a shared Python integration bridge so the generic Rust service hosts can load
manifest-declared Python venue modules for:

- trading gateway adaptors
- RTMD gateway adaptors
- refdata loader adaptors

This phase exists to make the venue-integration model in
[venue_integration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md)
real rather than aspirational.

The design target is:

- `zk-gw-svc`, `zk-rtmd-gw-svc`, and the refdata host remain generic Rust wrappers
- venue modules may be implemented in Rust or Python
- Python venue implementations are loaded through one shared PyO3 bridge
- Rust traits remain the internal host-facing contract

This phase is primarily motivated by:

- `oanda` integration
- `ibkr` integration
- future slower-path or SDK-constrained venues

It is not required for the first native Rust venue (`okx`), but it should be built as shared
infrastructure rather than as OANDA- or IBKR-specific glue.

## Why this phase exists

The current architecture already says:

- Python implementations should be loaded through a shared Python bridge
- the service wrapper should not care whether a venue adaptor is Rust or Python
- the manifest decides the implementation language

But the current codebase only has:

- native Rust service hosts
- placeholder venue manifests under `zkbot/venue-integrations/`
- an existing PyO3 crate (`zk-pyo3-rs`) used for strategy/backtest bindings

What is missing is the adaptor layer that allows a Python object to satisfy Rust-side service
traits such as:

- `VenueAdapter`
- `RtmdVenueAdapter`
- `RefdataLoader`

Without this phase, Python venue integration would drift toward:

- bespoke subprocess wrappers per service
- per-venue hard-coded glue
- inconsistent contracts between OANDA, IBKR, and future venues

That would contradict the architecture.

## Scope

This phase covers:

- manifest-driven loading of Python venue entrypoints
- a shared Rust-side PyO3 wrapper layer
- Python-side adaptor base contract and packaging conventions
- type conversion between Rust request/fact structs and Python payloads
- async bridging for command/query/event methods
- consistent error mapping from Python exceptions into Rust host errors
- host integration in:
  - `zk-gw-svc`
  - `zk-rtmd-gw-svc`
  - refdata service host

This phase does not cover:

- full OANDA business logic
- full IBKR business logic
- dynamic native `.so` plugin loading
- an out-of-process helper model except as a later fallback

## Prerequisites

- Phase 1 complete enough for shared infra (`zk-infra-rs`)
- Phase 4 gateway host structure available
- venue module placeholder layout available under `zkbot/venue-integrations/`
- existing `zk-pyo3-rs` crate available as the Python embedding foundation

## Deliverables

### 12A.1 Shared PyO3 bridge support

Primary location:

- `zkbot/rust/crates/zk-pyo3-rs/`

Recommended additions:

```text
zk-pyo3-rs/src/
  venue_bridge/
    mod.rs
    manifest.rs
    py_loader.rs
    py_runtime.rs
    py_types.rs
    py_errors.rs
    gw_adapter.rs
    rtmd_adapter.rs
    refdata_adapter.rs
```

Design rule:

- do not expose Rust traits directly to Python
- instead, load a Python object and wrap it in a Rust struct that implements the Rust trait

That means:

- `PyVenueAdapter` implements `VenueAdapter`
- `PyRtmdVenueAdapter` implements `RtmdVenueAdapter`
- `PyRefdataLoader` implements `RefdataLoader`

This keeps the Rust hosts fully trait-oriented while allowing Python implementations behind the
boundary.

### 12A.2 Manifest-driven Python loading

The bridge should consume the same venue manifest model defined in
[venue_integration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md).

Recommended Rust-side manifest model:

```rust
pub struct VenueManifest {
    pub venue: String,
    pub version: u32,
    pub capabilities: VenueCapabilities,
    pub metadata: serde_json::Value,
}

pub struct CapabilityManifest {
    pub language: String,
    pub entrypoint: String,
    pub config_schema: String,
}
```

Loading rules:

1. resolve `venue-integrations/<venue>/manifest.yaml`
2. parse manifest and validate capability exists
3. validate `language`
4. validate the provided config against the declared schema
5. if `language == rust`, use the native registry path
6. if `language == python`, call the shared PyO3 loader

The loader should parse entrypoints in the form:

- `python:<module_path>:<class_name>`

Examples:

- `python:oanda.gw:OandaGatewayAdaptor`
- `python:ibkr.rtmd:IbkrRtmdAdaptor`
- `python:okx.refdata:OkxRefdataLoader`

### 12A.3 Shared Python runtime and module loading

The bridge needs one shared Python embedding/runtime layer. It should not be reimplemented inside
each host binary.

Recommended responsibilities:

- initialize Python once
- configure `sys.path` to include the venue integration root
- import the target module
- instantiate the configured class with typed config payload
- hold a `Py<PyAny>` reference to the live adaptor object

Recommended shape:

```rust
pub struct PyRuntimeHandle {
    // shared Python runtime state
}

pub struct PyObjectHandle {
    inner: Py<PyAny>,
}

impl PyRuntimeHandle {
    pub fn new(venue_root: &Path) -> Result<Self>;
    pub fn load_class(&self, module: &str, class_name: &str) -> Result<PyObjectHandle>;
}
```

Important rule:

- Python interpreter lifecycle must be owned centrally
- service binaries should receive ready-to-use wrapper objects, not manage interpreter details

### 12A.4 Rust-side wrapper adaptors

#### Trading gateway wrapper

`PyVenueAdapter` should implement the Rust `VenueAdapter` trait by forwarding calls into Python.

Suggested internal shape:

```rust
pub struct PyVenueAdapter {
    obj: Py<PyAny>,
}

#[async_trait]
impl VenueAdapter for PyVenueAdapter {
    async fn connect(&self) -> anyhow::Result<()>;
    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck>;
    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck>;
    async fn query_balance(&self, req: VenueBalanceQuery) -> anyhow::Result<Vec<VenueBalanceFact>>;
    async fn query_order(&self, req: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>>;
    async fn query_trades(&self, req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>>;
    async fn query_positions(
        &self,
        req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>>;
    async fn next_event(&self) -> anyhow::Result<VenueEvent>;
}
```

#### RTMD wrapper

`PyRtmdVenueAdapter` should wrap the Rust RTMD trait with the same pattern:

- `connect`
- `subscribe`
- `unsubscribe`
- `snapshot_active`
- `query_current_tick`
- `query_current_orderbook`
- `query_current_funding`
- `query_klines`

#### Refdata wrapper

`PyRefdataLoader` should wrap the refdata loader trait used by the refdata host. Refdata is
Python-only by architecture rule, so this wrapper is required from day one.

### 12A.5 Python-side adaptor contract

Python venue modules should follow a stable object contract. They should not need to know about
PyO3 internals.

Recommended contract for gateway adaptors:

```python
class SomeGatewayAdaptor:
    def __init__(self, config: dict): ...

    async def connect(self) -> None: ...
    async def place_order(self, req: dict) -> dict: ...
    async def cancel_order(self, req: dict) -> dict: ...
    async def query_balance(self, req: dict) -> list[dict]: ...
    async def query_order(self, req: dict) -> list[dict]: ...
    async def query_trades(self, req: dict) -> list[dict]: ...
    async def query_positions(self, req: dict) -> list[dict]: ...
    async def next_event(self) -> dict: ...
```

Recommended contract for RTMD adaptors:

```python
class SomeRtmdAdaptor:
    def __init__(self, config: dict): ...

    async def connect(self) -> None: ...
    async def subscribe(self, req: dict) -> None: ...
    async def unsubscribe(self, req: dict) -> None: ...
    async def snapshot_active(self) -> list[dict]: ...
    async def query_current_tick(self, instrument_id: str) -> dict: ...
    async def query_current_orderbook(self, instrument_id: str, depth: int | None = None) -> dict: ...
    async def query_current_funding(self, instrument_id: str) -> dict: ...
    async def query_klines(
        self,
        instrument_id: str,
        interval: str,
        limit: int,
        from_ms: int | None = None,
        to_ms: int | None = None,
    ) -> list[dict]: ...
```

Recommended contract for refdata loaders:

```python
class SomeRefdataLoader:
    def __init__(self, config: dict): ...

    async def load_instruments(self) -> list[dict]: ...
    async def load_market_sessions(self) -> list[dict]: ...
```

Important rule:

- the Python layer should exchange plain structured values
- avoid passing arbitrary Python classes back into Rust

### 12A.6 Type conversion strategy

The bridge must define one clear conversion layer between:

- Rust request/fact types
- Python dict/list payloads

Recommended rule:

- use explicit serde-backed structs on the Rust side
- convert to/from Python `dict` / `list` using `serde_json::Value` or equivalent helper structs
- do not use handwritten field-by-field conversion everywhere in host code

Examples:

- `VenuePlaceOrder` -> Python dict
- Python dict -> `VenueCommandAck`
- Python dict -> `VenueEvent`

Suggested helper direction:

```rust
fn to_py_dict<T: Serialize>(py: Python<'_>, value: &T) -> PyResult<PyObject>;
fn from_py_value<T: DeserializeOwned>(obj: &Bound<'_, PyAny>) -> PyResult<T>;
```

This is especially important for `next_event()`, because that method needs one stable tagged-event
shape.

Suggested Python event shape:

```json
{
  "event_type": "order_report",
  "payload": { ... }
}
```

Allowed `event_type` values:

- `order_report`
- `balance`
- `position`
- `system`

### 12A.7 Async bridging rules

The Python bridge must support async methods cleanly.

Recommended approach:

- use PyO3 async interop from the shared bridge layer
- Rust async trait methods may await Python coroutines
- synchronous Python implementations may still be allowed, but the bridge should normalize them into
  the async Rust interface

Design rules:

- the Rust host remains `tokio`-driven
- the Python bridge owns Python coroutine execution details
- host code should not contain ad hoc `Python::with_gil` blocks for every venue call

Important constraint:

- never hold the GIL across unrelated Rust awaits
- perform Python object access/conversion in narrow sections
- convert back to Rust-native data as early as possible

### 12A.8 Error model

Python exceptions need predictable mapping into Rust host errors.

Recommended error classes:

- configuration error
- import/load error
- schema validation error
- venue request rejection
- temporary transport/session error
- unsupported operation
- malformed Python response

Recommended behavior:

- Python import/class construction failures fail startup
- schema validation failures fail startup
- malformed response payloads are treated as bridge bugs and surfaced loudly
- recoverable venue/session errors become ordinary runtime adaptor errors

Suggested Rust-side error enum:

```rust
pub enum PyBridgeError {
    ManifestLoad(String),
    SchemaValidation(String),
    PythonImport(String),
    PythonCall(String),
    ResponseDecode(String),
    Unsupported(String),
}
```

### 12A.9 Host integration points

#### `zk-gw-svc`

`build_adapter()` should evolve from hard-coded venue matching to manifest-driven loading.

Recommended future path:

1. read `cfg.venue`
2. load venue manifest
3. resolve `gw` capability
4. if capability language is:
   - `rust`: build native adaptor
   - `python`: build `PyVenueAdapter`

The gateway host should still own:

- gRPC API
- queueing / execution shards
- semantic pipeline
- NATS publication
- registration

The Python adaptor should own only venue-specific behavior.

#### `zk-rtmd-gw-svc`

Use the same pattern for `rtmd` capability:

- native Rust for venues like OKX
- Python bridge for venues like OANDA and IBKR

#### Refdata host

The refdata host should always resolve the venue loader through the shared bridge for Python
implementations. Since refdata is Python-only by current architecture rule, the refdata host becomes
the first mandatory consumer of this bridge.

## Recommended implementation order

1. build manifest parsing + schema validation support
2. build shared Python runtime loader in `zk-pyo3-rs`
3. implement `PyRefdataLoader` first
4. integrate the refdata host with the bridge
5. implement `PyVenueAdapter`
6. integrate `zk-gw-svc`
7. implement `PyRtmdVenueAdapter`
8. integrate `zk-rtmd-gw-svc`
9. wire OANDA and IBKR placeholder modules into real Python implementations

Why this order:

- refdata is architecturally simplest
- gateway is more sensitive because of event semantics
- RTMD adds streaming and subscription complexity

## OANDA and IBKR target usage

The bridge directly supports the intended venue split:

- `okx`
  - `gw`: Rust
  - `rtmd`: Rust
  - `refdata`: Python
- `oanda`
  - `gw`: Python
  - `rtmd`: Python
  - `refdata`: Python
- `ibkr`
  - `gw`: Python
  - `rtmd`: Python
  - `refdata`: Python

## Tests

### Unit tests

- `test_manifest_loader_reads_python_entrypoint`
- `test_manifest_loader_rejects_missing_capability`
- `test_schema_validation_rejects_invalid_python_config`
- `test_py_runtime_imports_module_and_class`
- `test_py_bridge_converts_place_order_request`
- `test_py_bridge_decodes_command_ack`
- `test_py_bridge_decodes_order_report_event`
- `test_py_bridge_decodes_balance_event`
- `test_py_bridge_maps_python_exception_to_bridge_error`
- `test_py_refdata_loader_decodes_instrument_rows`
- `test_py_rtmd_adapter_decodes_kline_query_result`
- `test_py_bridge_rejects_malformed_event_type`

### Integration tests

- `test_refdata_host_loads_python_loader_from_manifest`
- `test_gateway_host_loads_python_venue_adapter_from_manifest`
- `test_rtmd_host_loads_python_adapter_from_manifest`
- `test_oanda_placeholder_manifest_resolves_python_entrypoints`
- `test_ibkr_placeholder_manifest_resolves_python_entrypoints`
- `test_rust_okx_manifest_uses_native_registry_path`

### Exit criteria

- the refdata host can load a Python loader from a manifest and execute one end-to-end load cycle
- `zk-gw-svc` can instantiate a Python gateway adaptor through the shared bridge
- `zk-rtmd-gw-svc` can instantiate a Python RTMD adaptor through the shared bridge
- host code does not contain per-venue embedded Python glue
- OANDA and IBKR use the shared bridge path rather than ad hoc wrappers
- OKX still works through the native Rust path

## Open questions

1. Should the shared bridge live inside `zk-pyo3-rs` or in a new dedicated crate such as
   `zk-python-bridge-rs`?

Recommended answer:

- start inside `zk-pyo3-rs` to reduce setup cost
- split later only if the crate becomes too mixed

2. Should Python venue adaptors run in-process or in a helper subprocess?

Recommended answer:

- default to in-process PyO3
- only introduce subprocess wrappers for venues whose SDKs force that operationally

3. Should the Python contract use dicts or protobuf bytes?

Recommended answer:

- start with dict/list structured payloads
- only optimize the boundary later if profiling says it matters

4. Should the bridge support both async and sync Python methods?

Recommended answer:

- yes, but normalize them into the same async Rust-facing wrapper API

## Related docs

- [Venue Integration Modules](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [Refdata Loader Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/refdata_loader_service.md)
- [Rust Crates](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rust_crates.md)
