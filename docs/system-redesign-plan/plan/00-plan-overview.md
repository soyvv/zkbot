# ZKBot Redesign — Implementation Plan Overview

## Phases

| Phase | Status | Name | Key deliverable | Depends on |
|---|---|---|---|---|
| 0 | ✓ Done | Local environment | docker-compose stack: NATS, PG, Mongo, Redis, Vault dev | — |
| 1 | In progress | Proto + infra foundation | `zk-infra-rs` core modules; proto migration deferred | 0 |
| 2 | ✓ Done | Mock Gateway (simulator) | `zk-mock-gw`; fills, NATS reports (built ahead of proto migration) | 0 |
| 3 | — | OMS gRPC service | `zk-oms-svc` binary with full gRPC API + NATS pub | 1, 2 |
| 4 | — | Real Trading Gateway | `zk-gw-svc`; OKX venue adapter, NATS report pub, KV registration | 1 |
| 5 | — | Registry + Pilot bootstrap | NATS KV registry lib; Pilot bootstrap subjects; `instance_id` lease | 3, 4 |
| 6 | — | Scaffolding services | `zk-pilot-scaffold` REST stub; `zk-refdata-svc` gRPC; KV registration | 3, 5 |
| 7 | — | Trading SDK | `zk-trading-sdk-rs` with KV discovery, OMS gRPC pool, `RefdataSdk`, id_gen | 3, 5, 6 |
| 8 | — | Strategy Engine service | `zk-engine-svc`; Pilot REST startup/shutdown; Python worker | 6, 7 |
| 9 | — | Recorder + Monitor | trade write, snapshot, recon jobs, risk alert | 3, 8 |
| 10 | — | Pilot service | Java/Go control plane; 7-domain REST API; strategy-executions lifecycle; runtime orchestrator adaptor | 5, 8, 9 |
| 11 | — | Collocated mode | `zk-collocated-svc`; `GatewayPlugin` trait; inter-thread channels | 3, 4, 8 |
| 12A | — | Python venue bridge | shared PyO3 bridge for manifest-loaded Python venue adaptors | 1, 4 |

**Why simulator first (Phase 2):** Every downstream phase needs a controllable order-fill loop
for integration tests. Building `zk-mock-gw` before the OMS service means Phase 3 OMS tests
drive a real process (`PlaceOrder → fill delay → NATS OrderReport`) rather than a hand-rolled
mock. The admin `TriggerFill` RPC gives deterministic control over fill timing for reliable CI.
Phase 4 (real gateway) is independent and can run in parallel with Phase 3.

Each phase has its own detailed plan file. This file defines the shared test framework, docker-compose setup, and exit-criteria format.

## Exit Criteria Format (per phase)

Each phase plan specifies:

1. **Unit tests**: pure-logic tests, no external deps, run with `cargo test` / `pytest`
2. **Integration tests**: run against local docker-compose stack, tagged `#[ignore]` by default
3. **Exit criteria**: specific measurable conditions that must all be true before the phase is closed:
   - tests pass (CI green)
   - no TODO/stub markers left in delivered code
   - component registers in NATS KV and is discoverable by a consumer (from Phase 3 onwards)
   - basic smoke scenario works end-to-end against docker-compose stack

## Shared Test Conventions

### Rust

- Unit tests: `cargo test -p <crate>` — no external deps allowed in unit test
- Integration tests: `cargo test -p <crate> -- --ignored` — requires docker-compose stack
- Test helper: `tests/common/mod.rs` per crate for NATS/PG/Redis fixture setup
- Parity tests: `cargo test -p <crate> parity` — compare Rust output against Python reference

### Python

- Unit: `pytest tests/unit/`
- Integration: `pytest tests/integration/ --docker` — requires docker-compose stack
- All tests CI-runnable via `make test-unit` and `make test-integration`

## Files in this directory

| File | Phase | Contents |
|---|---|---|
| [00-plan-overview.md](00-plan-overview.md) | — | This file |
| [01-environment.md](01-environment.md) | 0 | docker-compose stack, local dev setup |
| [02-proto-and-infra.md](02-proto-and-infra.md) | 1 | proto migration + `zk-infra-rs` modules |
| [03-mock-gateway.md](03-mock-gateway.md) | 2 | `zk-mock-gw` trading simulator |
| [04-oms-service.md](04-oms-service.md) | 3 | OMS gRPC service binary |
| [05-real-gateway-service.md](05-real-gateway-service.md) | 4 | Real trading gateway — OKX venue adapter; RTMD gateway |
| [06-registry-and-pilot-bootstrap.md](06-registry-and-pilot-bootstrap.md) | 5 | NATS KV registry + Pilot bootstrap |
| [07-scaffolding-services.md](07-scaffolding-services.md) | 6 | Pilot scaffold REST stub + Refdata gRPC service |
| [08-trading-sdk.md](08-trading-sdk.md) | 7 | `zk-trading-sdk-rs` and Python bindings |
| [09-engine-service.md](09-engine-service.md) | 8 | Strategy Engine service |
| [10-recorder-and-monitor.md](10-recorder-and-monitor.md) | 9 | Recorder, Reconciliation, Monitor |
| [11-pilot-service.md](11-pilot-service.md) | 10 | Pilot control plane — Java/Go; 7-domain REST API; runtime orchestrator |
| [12-collocated-mode.md](12-collocated-mode.md) | 11 | 2-layer / all-in-one binary |
| [13-python-venue-bridge.md](13-python-venue-bridge.md) | 12A | Shared PyO3 bridge for Python venue adaptors |
| [13-refdata-service.md](13-refdata-service.md) | 12 | Full Python refdata service + scheduled jobs + KV registration |

## Supplemental Follow-on Phases

| Phase | Status | Name | Key deliverable | Depends on |
|---|---|---|---|---|
| 12 | — | Full refdata service | Python `zk-refdata-svc` with scheduled refresh jobs, lifecycle management, market session ownership, and shared KV registration | 5, 6 |
