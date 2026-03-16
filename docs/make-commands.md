# Make Commands Reference

All commands are run from the `zkbot/` repo root.

## Prerequisites

| Tool | Required for | Install |
|------|-------------|---------|
| Docker + Compose v2 | Dev stack, `dev-*` targets | [docs.docker.com](https://docs.docker.com) |
| Rust + cargo | Rust targets | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| protobuf-compiler | Proto gen, Rust builds | `brew install protobuf` / `apt install protobuf-compiler` |
| buf | `gen` target | [buf.build/docs/installation](https://buf.build/docs/installation) |
| grpcurl | `oms-e2e-bench` (optional) | `brew install grpcurl` |
| Python 3.11+ + uv | Python targets | `pip install uv` |

---

## Infrastructure

```bash
make dev-up        # Start infra + gw-sim (NATS, Postgres, Redis, Mongo, Vault, gw-sim)
                   # Also runs devops/init/vault.sh to seed Vault
make dev-down      # Stop all containers (preserves volumes)
make dev-reset     # Destroy volumes + restart clean (data loss!)
make dev-logs      # Follow logs from all containers
make dev-ps        # Show container status
```

Dev stack services and ports:

| Service | Port | Notes |
|---------|------|-------|
| NATS | 4222 (client), 8222 (monitoring) | JetStream enabled |
| PostgreSQL | 5432 | user `zk`, db `zkbot` |
| Redis | 6379 | |
| MongoDB | 27017 | db `zk_events` |
| Vault | 8200 | dev mode, token `dev-root-token` |
| gw-sim | 51051 (gRPC), 51052 (admin) | Gateway simulator (`zk-gw-svc --venue=simulator`) |
| oms-svc | 50051 | OMS gRPC service |

---

## Proto Generation

```bash
make gen           # Run buf generate to rebuild all proto bindings
                   # Output: rust/crates/zk-proto-rs/src/gen/
```

---

## Rust Tests

These targets run against the entire workspace (all Rust crates):

```bash
make test-unit         # cargo test --workspace (unit + integration tests, no --ignored)
make test-integration  # cargo test --workspace -- --ignored  (requires dev stack up)
make test-parity       # cargo test --workspace -- parity     (OMS parity tests)
```

---

## OMS Service

Per-crate targets for faster iteration on `zk-oms-svc`:

```bash
make oms-check             # Syntax + type check only (fast)
make oms-build             # Release build
make oms-test              # Unit tests (no dev stack needed)
make oms-test-integration  # Integration tests (requires dev stack: make dev-up)
make oms-bench             # In-process Criterion micro-benchmarks (actor round-trip latency)
make oms-e2e-bench         # E2E latency benchmark: 3 realistic flows against live stack
                           # Requires: make dev-up && make oms-run (in another terminal)
make oms-run               # Run OMS locally against dev stack
                           # Requires: make dev-up
```

### E2E bench flows

`oms-e2e-bench` runs 100 iterations of each:

1. **Place → Booked** — gRPC PlaceOrder, wait for Booked NATS event
2. **Place → Booked → Cancel → Cancelled** — cancel flow latency
3. **Place → Booked → ForceMatch → Filled** — admin-triggered fill latency (via gw-sim admin gRPC)

Reports min/p50/p95/p99/max per flow. Configure via env:
```bash
E2E_OMS_ADDR=http://localhost:50051 \
E2E_NATS_URL=nats://localhost:4222 \
E2E_ITERATIONS=200 \
make oms-e2e-bench
```

---

## Gateway Service

Per-crate targets for `zk-gw-svc` (gateway simulator):

```bash
make gw-check   # Syntax + type check only (fast)
make gw-build   # Release build
make gw-test    # Unit tests
make gw-run     # Run gateway simulator locally (requires NATS: make dev-up)
                # Binds to :51051 (gRPC) and :51052 (admin)
```

---

## Python

```bash
make lint    # ruff check + mypy on libs/ and services/
make test    # pytest -q
make build   # uv build for all libs with pyproject.toml
make publish # (TODO) publish wheels to Nexus
```
