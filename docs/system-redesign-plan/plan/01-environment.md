# Phase 0: Local Development Environment

## Goal

Provide a reproducible local stack for all integration tests and manual validation.
All phases from Phase 1 onwards depend on this being up.

## docker-compose stack

File: `zkbot/docker/dev/docker-compose.yml`

### Services

| Service | Image | Port | Purpose |
|---|---|---|---|
| `nats` | `nats:2.10-alpine` | 4222 (client), 8222 (monitor) | messaging + JetStream + KV |
| `postgres` | `postgres:16-alpine` | 5432 | config, trades, refdata, monitoring |
| `mongo` | `mongo:7` | 27017 | event store |
| `redis` | `redis:7-alpine` | 6379 | OMS warm cache |
| `vault` | `hashicorp/vault:1.17` | 8200 | exchange credential store (dev mode) |
| `mock-gw` | local image | 51051 (gRPC) | trading simulator implementing `GatewayService` |
| `prometheus` | `prom/prometheus` | 9090 | metrics scraping |
| `grafana` | `grafana/grafana` | 3000 | dashboards (optional) |

```yaml
# zkbot/docker/dev/docker-compose.yml
services:
  nats:
    image: nats:2.10-alpine
    command: ["-js", "-m", "8222", "--name", "zk-dev"]
    ports:
      - "4222:4222"
      - "8222:8222"
    healthcheck:
      test: ["CMD", "wget", "-qO-", "http://localhost:8222/healthz"]
      interval: 5s
      timeout: 3s
      retries: 5

  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: zk
      POSTGRES_PASSWORD: zk
      POSTGRES_DB: zkbot
    ports:
      - "5432:5432"
    volumes:
      - ./init/postgres:/docker-entrypoint-initdb.d
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U zk -d zkbot"]
      interval: 5s
      timeout: 3s
      retries: 10

  mongo:
    image: mongo:7
    ports:
      - "27017:27017"
    environment:
      MONGO_INITDB_DATABASE: zk_events
    healthcheck:
      test: ["CMD", "mongosh", "--eval", "db.adminCommand('ping')"]
      interval: 5s
      timeout: 3s
      retries: 10

  redis:
    image: redis:7-alpine
    ports:
      - "6379:6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 5s
      timeout: 3s
      retries: 5

  vault:
    image: hashicorp/vault:1.17
    environment:
      VAULT_DEV_ROOT_TOKEN_ID: "dev-root-token"
      VAULT_DEV_LISTEN_ADDRESS: "0.0.0.0:8200"
    ports:
      - "8200:8200"
    cap_add:
      - IPC_LOCK
    command: ["vault", "server", "-dev"]
    healthcheck:
      test: ["CMD", "vault", "status", "-address=http://localhost:8200"]
      interval: 5s
      timeout: 3s
      retries: 5

  mock-gw:
    build:
      context: ../..
      dockerfile: docker/dev/Dockerfile.mock-gw
    ports:
      - "51051:51051"
    environment:
      ZK_NATS_URL: "nats://nats:4222"
      ZK_GW_ID: "gw_mock_1"
      ZK_ACCOUNT_ID: "9001"
    depends_on:
      nats:
        condition: service_healthy
```

### Init scripts

`docker/dev/init/postgres/00_schema.sql` — runs all `create schema` and `create table` DDL from [data_layer.md](../../system-arch/data_layer.md) in order:
1. `cfg` schema tables
2. `trd` schema tables
3. `mon` schema tables

`docker/dev/init/postgres/01_seed.sql` — minimal seed data for local testing:
- one OMS instance: `oms_id = 'oms_dev_1'`
- one gateway: `gw_id = 'gw_mock_1'`, venue `MOCK`
- one account: `account_id = 9001`
- account binding: `9001 → oms_dev_1 → gw_mock_1`
- one strategy definition: `strategy_id = 'strat_test'`, `runtime_type = RUST`
- one instrument: `instrument_id = 'BTCUSDT_MOCK'`, `venue = MOCK`

`docker/dev/init/vault.sh` — seeds Vault dev instance:
```bash
#!/usr/bin/env bash
export VAULT_ADDR=http://localhost:8200
export VAULT_TOKEN=dev-root-token
vault kv put kv/trading/gw/9001/api_key value="mock-api-key"
vault kv put kv/trading/gw/9001/api_secret value="mock-api-secret"
```

## Makefile targets

Add to `zkbot/Makefile`:

```makefile
dev-up:
	docker compose -f docker/dev/docker-compose.yml up -d
	./docker/dev/init/vault.sh

dev-down:
	docker compose -f docker/dev/docker-compose.yml down

dev-reset:
	docker compose -f docker/dev/docker-compose.yml down -v
	$(MAKE) dev-up

dev-logs:
	docker compose -f docker/dev/docker-compose.yml logs -f

test-unit:
	cargo test --workspace --exclude zk-pyo3-rs
	pytest zkbot/tests/unit/

test-integration:
	cargo test --workspace --exclude zk-pyo3-rs -- --ignored
	pytest zkbot/tests/integration/ --docker

test-parity:
	cargo test --workspace -- parity
```

## Environment variables for local dev

File: `zkbot/docker/dev/.env` (gitignored)

```
ZK_NATS_URL=nats://localhost:4222
ZK_PG_URL=postgresql://zk:zk@localhost:5432/zkbot
ZK_MONGO_URL=mongodb://localhost:27017
ZK_REDIS_URL=redis://localhost:6379
ZK_VAULT_ADDR=http://localhost:8200
ZK_VAULT_TOKEN=dev-root-token
ZK_ENV=dev
ZK_ACCOUNT_IDS=9001
ZK_CLIENT_INSTANCE_ID=1
ZK_PILOT_API_URL=http://localhost:8080
```

## mock-gw

`mock-gw` is a minimal Rust binary (`zk-mock-gw`) implementing `zk.gateway.v1.GatewayService`:
- accepts order and cancel requests
- simulates fills after a configurable delay (default 100ms)
- publishes `OrderReport` to `zk.gw.gw_mock_1.report`
- publishes `BalanceUpdate` to `zk.gw.gw_mock_1.balance`
- registers `svc.gw.gw_mock_1` in NATS KV

The mock-gw is the primary integration test target for OMS, trading_sdk, and engine tests.

## Exit criteria

- [ ] `make dev-up` completes without error on a clean checkout
- [ ] all health checks pass: NATS, PG, Mongo, Redis, Vault
- [ ] `psql -U zk -d zkbot -c '\dt cfg.*'` lists all expected tables
- [ ] `nats server info` returns JetStream-enabled server
- [ ] `vault kv get kv/trading/gw/9001/api_key` returns `mock-api-key`
- [ ] `make dev-reset` tears down volumes and restarts cleanly
