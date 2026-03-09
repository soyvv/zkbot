# Target Architecture

## Core decisions

- Rust owns performance-critical domain logic.
- Python remains the orchestration/reporting environment for research and backtesting.
- Strategy execution is pluggable.
- OMS uses a single-writer ownership model with immutable snapshots for readers.
- Rust service transport split:
  - `gRPC` for point-to-point RPC between services
  - `NATS` pub/sub for market/event fanout
- Service discovery uses `NATS KV` with TTL-backed registrations.

## gRPC + NATS service topology (migration target)

### Scope for first migration wave
- migrate first:
  - simulator service
  - OMS service
  - strategy engine service
- keep all gateways on existing runtime transport in this wave

### Discovery registry
- `NATS KV` bucket stores live endpoint records.
- records include:
  - service type (`gateway`, `oms`, `engine`, `sim`)
  - instance id
  - gRPC address (if applicable)
  - capability/account metadata
  - heartbeat timestamp / lease TTL

### Startup/registration flow

1. Gateway startup
- gateway registers in `NATS KV`:
  - transport info (current gateway transport endpoint and metadata)
  - account info it serves
- this allows OMS to discover active gateways by account mapping.

2. OMS startup
- OMS loads account-related config from DB.
- OMS discovers registered gateways from `NATS KV` and builds account->gateway routing.
- OMS establishes gateway connectivity using the discovered transport metadata.
- OMS registers its own gRPC transport info in `NATS KV`, keyed by OMS id.

3. Engine startup
- engine loads config to determine target OMS id.
- engine resolves OMS endpoint from `NATS KV`.
- engine establishes gRPC channel to OMS and starts normal event flow.

## Strategy runtimes

### RustStrategyRuntime
- native Rust trait-based strategies
- target for highest criticality strategies

### PythonEmbeddedRuntime
- Rust engine hosts CPython via `PyO3`
- used for backtesting, parity tests, and less performance-critical strategies

### PythonWorkerRuntime
- separate Python process receives strategy events and returns actions
- used for higher criticality Python strategies needing isolation

## OMS concurrency model

### Required design
- one authoritative mutable OMS core actor
- readers consume immutable snapshots or a materialized read model
- no direct unsynchronized shared mutable memory access

### Why
- current Python code benefits from GIL-imposed serialization
- Rust must make ordering and visibility guarantees explicit

## Backtesting model

### Rust owns
- replay engine
- event ordering
- simulator core
- performance-critical bookkeeping

### Python owns
- experiment orchestration
- parameter sweeps
- report generation
- analysis and charts

## Intended Rust crate layout

- `zk-proto-rs`
- `zk-domain-rs`
- `zk-oms-rs`
- `zk-engine-rs`
- `zk-backtest-rs`
- `zk-strategy-sdk-rs`
- `zk-strategy-py`
- `zk-strategy-worker-proto-rs`
- `zk-infra-rs`
