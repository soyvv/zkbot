# Rust Port Plan

This folder contains the agent-friendly execution plan for porting the performance-critical parts of `zkbot` from Python to Rust.

## How to use this plan

Read in this order:

1. `01-scope-and-principles.md`
2. `02-target-architecture.md`
3. `03-workstreams.md`
4. `04-phase-plan.md`
5. `05-testing-and-data-plan.md`
6. `06-protobuf-evolution-plan.md`
7. `07-open-questions-and-non-port-changes.md`

## Execution rules

- Preserve existing external stacks: `NATS`, `protobuf`, `Redis`, `ArcticDB`.
- Runtime transport target for Rust services: `gRPC` for request/response, `NATS` for pub/sub fanout.
- Keep Python as the orchestration/reporting layer for backtesting.
- Support both Python interop modes:
  - embedded `PyO3`
  - Python worker
- Do not reproduce GIL-era implicit shared-memory assumptions in Rust.
- Prefer additive and backward-compatible protobuf changes.
- Build test/replay infrastructure before large-scale porting.

## Runtime policy

- Less performance-critical Python strategy: embedded runtime is acceptable.
- Higher criticality Python strategy: use Python worker runtime.
- Highest criticality strategy: port to Rust.

## Priority order

1. Contracts and tests
2. GIL/shared-state redesign
3. Simulator service, OMS service, and live strategy engine transport migration to `gRPC + NATS pub/sub`
4. OMS core to Rust ✓
5. Backtester core to Rust with Python orchestration retained
6. Live engine to Rust
7. OMS/engine shadow mode + Python worker runtime
8. Broader protocol expansion

## Service discovery policy (new)

- Use `NATS KV` as the single dynamic service registry for runtime discovery.
- Gateways remain on existing transport for now, but must publish endpoint metadata to `NATS KV` at startup.
- OMS loads account-to-gateway config from DB, resolves live gateway endpoints from `NATS KV`, connects, then registers OMS gRPC endpoint in `NATS KV` keyed by OMS id.
- Strategy engine loads config to pick the target OMS id, resolves OMS endpoint from `NATS KV`, and connects via gRPC.

## Deliverable checklist

- [x] Rust workspace foundation in place (`rust/` workspace, 7 crates, `zk-proto-rs` generated types, `buf.gen.yaml` updated)
- [x] Rust OMS core ported (`zk-oms-rs`: ConfdataManager, OrderManager, BalanceManager, OmsCore, 12 parity tests all green)
- [x] Rust backtest core ported (`zk-backtest-rs`: SimulatorCore, MatchPolicy, EventQueue, BacktestOms, Backtester, 5 parity tests green)
- [ ] Rust backtest core callable from Python (PyO3 binding — Phase 3 deferred)
- [ ] Rust live engine supports Rust and embedded Python strategies (WS8 — Phase 4)
- [ ] Rust OMS shadow mode in place (NATS service + shadow-compare flow — Phase 5)
- [ ] Python worker runtime available (WS9 — Phase 5)
- [ ] Proto parity tests in place (WS4 design)
- [ ] ArcticDB-based local data prep pipeline in place (WS5)
- [ ] Protocol layer expanded for broader strategy/instrument support (WS10 — Phase 6)
- [ ] Rust services validated in shadow/canary mode and production cutover complete (WS11 — Phase 7)
