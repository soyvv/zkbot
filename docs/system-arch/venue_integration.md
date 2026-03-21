# Venue Integration Modules

## Goal

Unify how a new venue is integrated across:

- trading gateway
- RTMD gateway
- refdata service

The extension mechanism should stay consistent even though different capabilities may be implemented
in different languages.

## Design Direction

A venue integration should be packaged as a per-venue module that may provide any subset of:

- trading gateway adaptor
- RTMD gateway adaptor
- refdata loader adaptor
- a manifest describing capabilities, config schema, and implementation locations

Recommended rule:

- one venue = one integration module boundary
- gateway, RTMD, and refdata all discover their venue-specific implementation through the same
  module manifest model

This avoids three different plugin stories for the same venue.

## Integration Package Shape

Canonical rule:

- one venue = one directory under `venue-integrations/`
- one manifest per venue
- one canonical Python package path per venue
- one canonical Rust source tree per venue
- no duplicate flat-module and package-module layouts for the same capability

Required top-level shape:

```text
venue-integrations/
  <venue>/
    manifest.yaml
    schemas/
      gw_config.schema.json
      rtmd_config.schema.json
      refdata_config.schema.json
    rust/
      Cargo.toml
      src/
        lib.rs
        ...
    <venue>/
      __init__.py
      ...
    tests/
      ...
```

Directory contract:

- `manifest.yaml`
  - required for every venue
- `schemas/`
  - stores JSON schemas referenced by the manifest
- `rust/`
  - present only if the venue provides at least one native Rust capability
  - must use normal Rust crate layout under `rust/src/`
- `<venue>/`
  - present only if the venue provides at least one Python capability
  - must be the canonical Python package root
- `tests/`
  - venue-local tests for Python and/or Rust behavior

Disallowed layout:

- top-level `python/` flat modules such as `python/gw.py`, `python/rtmd.py`, `python/refdata.py`
- duplicate implementations of the same capability under both `python/` and `<venue>/`
- manifest entrypoints that do not match the actual on-disk module path

### Rust-Only Venue

Use this when `gw` and/or `rtmd` are native Rust and the venue has no Python capability except
possibly Python refdata.

```text
venue-integrations/
  okx/
    manifest.yaml
    schemas/
      gw_config.schema.json
      rtmd_config.schema.json
      refdata_config.schema.json
    rust/
      Cargo.toml
      src/
        lib.rs
        config.rs
        gw.rs
        rtmd.rs
        rest.rs
        ws.rs
        normalize.rs
    okx/
      __init__.py
      refdata.py
    tests/
      rust/
      python/
```

Contract:

- Rust capability code lives only under `rust/src/`
- if Python refdata exists, it lives only under `<venue>/refdata.py`
- manifest must point to the exact Rust module path and Python package path

### Rust + Python Mixed Venue

Use this when some capabilities are Rust and others are Python.

```text
venue-integrations/
  <venue>/
    manifest.yaml
    schemas/
      gw_config.schema.json
      rtmd_config.schema.json
      refdata_config.schema.json
    rust/
      Cargo.toml
      src/
        lib.rs
        gw.rs
        rtmd.rs
        common.rs
    <venue>/
      __init__.py
      config.py
      refdata.py
      common.py
    tests/
      rust/
      python/
```

Contract:

- the manifest is the only source of truth for which capability is Rust vs Python
- shared logic may exist in `common.*`, but host binaries must not embed venue-specific logic
- Python code must be package-based under `<venue>/`
- Rust code must be crate-based under `rust/src/`

### Python-Only Venue

Use this for OANDA- and IBKR-style integrations.

```text
venue-integrations/
  oanda/
    manifest.yaml
    pyproject.toml
    uv.lock
    schemas/
      gw_config.schema.json
      rtmd_config.schema.json
      refdata_config.schema.json
    oanda/
      __init__.py
      config.py
      gw.py
      rtmd.py
      refdata.py
      client.py
      normalize.py
      stream.py
      proto/
    tests/
      conftest.py
      test_gw.py
      test_rtmd.py
      test_refdata.py
```

Contract:

- Python capability modules live only under the venue package, for example `oanda/gw.py`
- manifest entrypoints must be package-style imports such as `python:oanda.gw:OandaGatewayAdaptor`
- bridge and Python hosts should import the module path exactly as declared in the manifest
- no parallel placeholder modules outside the package root

Language rule:

- `refdata` is Python-only
- `gw` may be Rust or Python
- `rtmd` may be Rust or Python

Not every venue has to provide every capability, but if it provides one, it should expose it
through the same manifest/module structure.

## Manifest

Each venue module should provide a manifest with:

- venue id
- module version
- supported capabilities
  - `gw`
  - `rtmd`
  - `refdata`
- implementation language per capability
- implementation entrypoint per capability
- config schema references
- optional notes or operational caveats

Suggested manifest shape:

```yaml
venue: okx
version: 1
capabilities:
  gw:
    language: rust
    entrypoint: rust::okx::gw::OkxGatewayAdaptor
    config_schema: schemas/gw_config.schema.json
  rtmd:
    language: rust
    entrypoint: rust::okx::rtmd::OkxRtmdAdaptor
    config_schema: schemas/rtmd_config.schema.json
  refdata:
    language: python
    entrypoint: python:okx.refdata:OkxRefdataLoader
    config_schema: schemas/refdata_config.schema.json
metadata:
  supports_tradfi_sessions: false
  notes:
    - orderbook semantics require venue-specific depth handling
```

Manifest validation rules:

- `refdata.language` must be `python`
- `gw.language` may be `rust` or `python`
- `rtmd.language` may be `rust` or `python`
- `python:` entrypoints must resolve to the actual package path under `<venue>/`
- `rust::` entrypoints must resolve to the actual Rust module path under `rust/src/`
- `config_schema` must exist if declared
- implemented capabilities must not be marked as placeholders in the manifest notes

Entrypoint rules:

- Python entrypoints use `python:<package.module>:<ClassName>`
- Rust entrypoints use `rust::<venue>::<capability_module>::<TypeName>`

Examples:

- `python:oanda.gw:OandaGatewayAdaptor`
- `python:ibkr.refdata:IbkrRefdataLoader`
- `rust::okx::gw::OkxGatewayAdaptor`
- `rust::okx::rtmd::OkxRtmdAdaptor`

## Common Facade

The system should expose one shared venue-integration registry/facade responsible for:

- loading module manifests
- validating config against the declared schema
- resolving the correct implementation for a given `(venue, capability)`
- instantiating the adaptor through a language bridge

Suggested high-level interface:

```rust
pub trait VenueIntegrationRegistry {
    fn manifest(&self, venue: &str) -> Result<VenueManifest>;
    fn load_gw(&self, venue: &str, cfg: serde_json::Value) -> Result<Box<dyn VenueAdapter>>;
    fn load_rtmd(&self, venue: &str, cfg: serde_json::Value) -> Result<Box<dyn RtmdVenueAdapter>>;
    fn load_refdata(&self, venue: &str, cfg: serde_json::Value) -> Result<Box<dyn RefdataLoader>>;
}
```

The important design point is not the exact Rust trait, but that all three service families resolve
their venue-specific implementation through the same facade.

For refdata, the logical loading contract is the same even if the host itself is already Python:

- the host must resolve the `refdata` capability from the venue manifest
- the host must validate the refdata config against the manifest-declared schema
- the host must instantiate the manifest-declared Python entrypoint
- the host must then call the adaptor through the stable refdata-loader interface

That means:

- a Rust refdata host would reach the adaptor through the shared Python bridge
- a Python refdata host may import the Python entrypoint directly
- neither host should maintain a separate hardcoded per-venue loader registry outside the manifest
  model

## Language Bridging

The integration mechanism should support both native and bridged implementations.

Recommended execution modes:

- native Rust
  - directly linked trait implementation
- native Python
  - loaded through a Python runtime bridge
- wrapped external helper
  - only if a venue SDK forces an out-of-process wrapper

Design rule:

- the manifest decides the implementation language
- the service wrapper should not care whether the venue adaptor is Rust or Python once loaded
- the language bridge should be shared infrastructure, not reimplemented separately for gateway,
  RTMD, and refdata
- refdata loader implementations are always Python, but the exact loading path depends on the host:
  - Rust host: through the shared Python bridge
  - Python host: direct Python import using the same manifest semantics

## Loading Model

The phrase "generic service host" means the stable service binary for that family:

- `zk-gw-svc`
- `zk-rtmd-gw-svc`
- generic refdata host

These hosts should support runtime venue-adaptor resolution, but that does not necessarily mean
native operating-system-level dynamic plugin loading on day one.

Recommended implementation stages:

- Stage 1: registry-based runtime selection
  - Rust implementations are linked into the host and selected at runtime through the venue
    integration registry
  - Python implementations are loaded through the shared Python bridge at runtime
- Stage 2: optional true dynamic loading
  - native Rust implementations may later be loaded from shared libraries if operational flexibility
    justifies the added complexity

Preferred initial rule:

- use runtime adaptor resolution
- avoid complex native `.so`/`dylib` plugin loading unless there is a clear operational need

This keeps the architecture flexible without forcing the first implementation into a fragile plugin
system.

For the current Python `zk-refdata-svc`, the recommended first implementation is:

- manifest-driven Python import inside the refdata host
- no dependency on the Rust bridge just to load Python refdata adaptors
- same manifest/config/schema semantics as the long-term bridge design
- venue-specific refdata code still lives under `zkbot/venue-integrations/<venue>/`

For Python venue loading specifically:

- the refdata host should import `python:<venue>.<module>:<Class>`
- it should add the venue directory itself to `sys.path`, not a parallel flat-module directory
- the bridge should not rewrite package entrypoints into unrelated flat module names

## Capability Interfaces

The per-capability interfaces should remain service-specific:

- gateway uses `VenueAdapter`
- RTMD gateway uses `RtmdVenueAdapter`
- refdata service uses `RefdataLoader`

But the loading and configuration pattern should be unified.

That means:

- same manifest mechanism
- same config-validation approach
- same venue module directory/package concept
- same operational ownership model

Recommended first-pass Python refdata adaptor contract:

```python
class SomeRefdataLoader:
    def __init__(self, config: dict | None = None): ...
    async def load_instruments(self) -> list[dict]: ...
    async def load_market_sessions(self) -> list[dict]: ...
```

Recommended first-pass Python RTMD adaptor contract:

```python
class SomeRtmdAdaptor:
    def __init__(self, config: dict | None = None): ...
    async def connect(self) -> None: ...
    async def subscribe(self, spec: dict) -> None: ...
    async def unsubscribe(self, spec: dict) -> None: ...
    async def snapshot_active(self) -> list[dict]: ...
    def instrument_exch_for(self, instrument_code: str) -> str | None: ...
    async def query_current_tick(self, instrument_code: str) -> bytes: ...
    async def query_current_orderbook(self, instrument_code: str, depth: int | None = None) -> bytes: ...
    async def query_current_funding(self, instrument_code: str) -> bytes: ...
    async def query_klines(
        self,
        instrument_code: str,
        interval: str,
        limit: int,
        from_ms: int | None = None,
        to_ms: int | None = None,
    ) -> list[bytes]: ...
    async def next_event(self) -> dict: ...
```

Recommended first-pass Python GW adaptor contract:

```python
class SomeGatewayAdaptor:
    def __init__(self, config: dict | None = None): ...
    async def connect(self) -> None: ...
    async def place_order(self, req: dict) -> list[dict]: ...
    async def cancel_order(self, req: dict) -> list[dict]: ...
    async def query_balance(self, req: dict) -> list[dict]: ...
    async def query_order(self, req: dict) -> list[dict]: ...
    async def query_open_orders(self, req: dict) -> list[dict]: ...
    async def query_trades(self, req: dict) -> list[dict]: ...
    async def query_positions(self, req: dict) -> list[dict]: ...
    async def query_funding_fees(self, req: dict) -> list[dict]: ...
    async def next_event(self) -> dict: ...
```

Interface rule:

- `load_market_sessions()` may return an empty list for always-open crypto venues
- session-constrained venues should provide real market-session or calendar data through the adaptor
- the host should not synthesize a blanket `open` state for every venue
- Python adaptor methods should be async by default
- Python package/module names should remain stable once published in the manifest
- a capability should have exactly one canonical implementation path

## Service Wrapper Pattern

Each service binary should stay thin and use the facade.

Recommended ownership model:

- service wrappers are generic, centrally managed binaries
- venue modules provide adaptors/plugins, not full standalone per-venue service binaries by default

That means the wrapping should live in the service hosts, not inside each venue package:

- `zk-gw-svc`
  - generic trading gateway host
- `zk-rtmd-gw-svc`
  - generic RTMD gateway host
- refdata service
  - generic refdata host

Each host resolves the venue adaptor from the venue integration registry at startup.

Per-venue deployment is then achieved by configuration, not by having a separate hard-coded wrapper
binary per venue.

Each service host should stay thin and use the facade:

- `zk-gw-svc`
  - asks the venue integration registry for `gw` capability
- `zk-rtmd-gw-svc`
  - asks the venue integration registry for `rtmd` capability
- refdata service
  - asks the venue integration registry for `refdata` capability

The service wrappers should own:

- bootstrap/config loading
- registration/discovery
- lifecycle and supervision
- generic publication/query contracts

The venue module should not own:

- bootstrap
- service registration/discovery
- service supervision
- generic gRPC/NATS contract glue

The venue module should own:

- venue-specific API/session behavior
- venue-specific metadata normalization
- venue-specific quirks and feature support
- venue-specific market-session source behavior for refdata when applicable

## Adaptor State Ownership

The adaptor is allowed to own venue-local runtime state needed to operate correctly.

Examples of adaptor-owned state:

- REST client/session
- WebSocket connection state
- login/auth session state
- reconnect-local buffers
- subscription handles
- rate-limit state
- venue sequence/checkpoint state
- temporary snapshots used to synthesize venue facts

But the adaptor should not own system-level semantic authority.

Examples of non-adaptor-owned concerns:

- bootstrap and registration/discovery
- generic supervision policy
- generic publication/query contracts
- durable deduplication or idempotency policy
- canonical refdata authority
- cross-service reconciliation policy

Summary rule:

- adaptor owns how to talk to the venue
- service wrapper owns how the system behaves around that venue

## How Services Are Constructed

Recommended construction flow for a deployed venue service:

1. start the generic service host
2. load generic service config from Pilot/bootstrap
3. read the venue id and per-capability config
4. load the venue manifest from the venue integration registry
5. validate the per-capability config against the manifest-declared schema
6. instantiate the adaptor through:
   - direct Rust registry selection, or
   - shared Python bridge
7. run the generic service wrapper around that adaptor

Examples:

- gateway for OKX:
  - start `zk-gw-svc`
  - config says `venue=okx`
  - host loads `okx` manifest and its `gw` adaptor
- RTMD gateway for Binance:
  - start `zk-rtmd-gw-svc`
  - config says `venue=binance`
  - host loads `binance` manifest and its `rtmd` adaptor
- refdata refresh for ICE:
  - start generic refdata host
  - config says `venue=ice`
  - host loads `ice` manifest and its Python `refdata` loader

For a Python refdata host, step 6 becomes:

- instantiate the manifest-declared Python class directly after schema validation

The important invariant is that manifest-driven resolution remains the source of truth.

This keeps deployment and operations consistent:

- one generic host per service family
- one venue module per venue
- no need to create a brand-new wrapper binary every time a venue is added

## Implementation Stack Note

For an OKX-style venue with REST + WebSocket APIs, a practical Rust-first stack is:

- `tokio`
- `reqwest` + `rustls` for REST
- `tokio-tungstenite` + `rustls` for WebSocket
- `serde` / `serde_json`
- `hmac` + `sha2` + `base64` for signing
- `time`
- `governor` or equivalent async rate limiter
- `tracing` + `thiserror`

Recommended package split for that kind of venue:

- `okx-core`
  - auth/signing
  - REST client
  - WS client
  - shared models
  - shared rate-limit policy
- `okx-gw` adaptor
- `okx-rtmd` adaptor
- Python `okx-refdata` loader

Important note for venues like OKX:

- shared rate limits across REST and WS trading paths should be handled above the individual
  transport client, not separately inside each transport implementation

## Config Model

Config should be split into:

- generic service config owned by the service wrapper
- per-venue capability config owned by the venue module

The manifest-declared schema should validate the per-venue config before the adaptor is started.

This keeps control-plane config queryable while still allowing venue-specific extensibility.

## Operational Benefits

This pattern should make it easier to:

- add a new venue once and expose it consistently to gw/rtmd/refdata
- know which capabilities a venue supports
- mix Rust and Python implementations without changing service-level design
- reason about rollout/versioning per venue module
- keep service wrappers generic and stable

## Venue Tester Tool

A dedicated venue tester tool should exist to validate venue integration modules outside the normal
production service hosts.

Purpose:

- verify that a venue module can be loaded through the shared facade
- validate manifest and config-schema correctness
- exercise capability-specific smoke tests without bootstrapping the full service stack
- help developers and operators check venue integrations before rollout

Recommended shape:

- one generic tester tool, for example `zk-venue-tester`
- it uses the same venue integration registry/facade as the real services
- it loads a venue module by `(venue, capability)` and runs targeted checks

Suggested commands:

- `zk-venue-tester manifest --venue okx`
  - validate manifest and declared capabilities
- `zk-venue-tester gw --venue okx --config gw_config.json`
  - load the gateway adaptor and run connectivity/query/order smoke checks
- `zk-venue-tester rtmd --venue okx --config rtmd_config.json`
  - load the RTMD adaptor and run subscribe/query smoke checks
- `zk-venue-tester refdata --venue ice --config refdata_config.json`
  - load the Python refdata loader and run fetch/normalize/diff smoke checks

Capability-specific expectations:

- `gw`
  - adaptor loads successfully
  - config validates
  - venue connectivity/auth can be checked
  - query-after-action / periodic-query mode can be smoke-tested where relevant
- `rtmd`
  - adaptor loads successfully
  - config validates
  - basic subscribe/unsubscribe/query behavior can be checked
- `refdata`
  - Python loader imports successfully
  - config validates
  - fetch and normalization output can be inspected
  - market-session/calendar fetch can be checked when supported

Design rules:

- the tester should reuse the same manifest, config validation, and language bridge used by the real
  service hosts
- the tester should not introduce a second integration-loading mechanism
- test helpers may be capability-specific, but module resolution should remain unified

Operational use cases:

- developer smoke testing during venue integration work
- CI checks for manifest/schema/loading regressions
- operator validation before enabling a venue module in Pilot-managed config

This keeps venue integration verification explicit and repeatable without turning the production
service hosts into ad hoc test harnesses.

## TODO

- define the exact manifest schema
- define the shared language-bridge mechanism
- decide module discovery path and packaging model
- define version compatibility rules between service wrappers and venue modules
- define test harness expectations per venue capability
- define the exact CLI and output contract for `zk-venue-tester`
- decide whether shared utility code should live inside the venue module or in common crates/packages

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [Refdata Loader Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/refdata_loader_service.md)
