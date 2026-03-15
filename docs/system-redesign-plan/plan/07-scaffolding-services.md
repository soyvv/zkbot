# Phase 6: Scaffolding Services (Refdata Service + Pilot Execution Scaffold)

## Goal

Provide two thin services that unblock SDK and Engine development against canonical interfaces,
without waiting for the full Phase 10 Pilot or a production refdata loader.

**Dependency map:**

| Consumer | What it needs from this phase |
|---|---|
| Phase 7 (SDK) | `zk-refdata-svc` gRPC endpoint registered in KV |
| Phase 8 (Engine) | `zk-pilot-scaffold` REST endpoint; `zk-refdata-svc` already running |

The **Refdata gRPC Service** alone unblocks Phase 7. The SDK does not call Pilot at startup — it
reads `instance_id` from `ZK_CLIENT_INSTANCE_ID` env var and discovers OMS/refdata entirely from
NATS KV. The Pilot scaffold is **only required by Phase 8** (Engine startup calls
`POST /v1/strategy-executions/start` to claim execution and receive enriched config).

Both services match their respective Phase 10 real implementations exactly on the API surface.

## Prerequisites

- Phase 0 complete (docker-compose stack: NATS, PG, Vault, mock-gw)
- Phase 3 complete (OMS gRPC service; `cfg` schema tables present in PG)
- Phase 5 complete (NATS KV registry; `KvRegistryClient`; bootstrap flow)

---

## Deliverable 1 — Refdata gRPC Service (`zk-refdata-svc`)

**Priority: high — unblocks SDK (Phase 7)**

### Scope

Implements `zk.refdata.v1.RefdataService` and serves instrument reference data from
`cfg.instrument_refdata` in PostgreSQL. Registers in NATS KV bucket `zk-svc-registry-v1` so
`RefdataSdk` can discover it at startup. No venue loader automation in this phase; instrument rows
are seeded via `01_seed.sql` and inserted manually for dev/test.

The service shape follows `docs/system-arch/services/refdata_loader_service.md` exactly. A future
phase adds the venue loader and full lifecycle management on top of this gRPC layer.

### Proto service (new, added to `zk-proto`)

```protobuf
// zk/refdata/v1/refdata.proto
syntax = "proto3";
package zk.refdata.v1;

service RefdataService {
  rpc QueryInstrumentById          (QueryInstrumentByIdRequest)      returns (InstrumentRefdataResponse);
  rpc QueryInstrumentByVenueSymbol (QueryByVenueSymbolRequest)       returns (InstrumentRefdataResponse);
  rpc ListInstruments              (ListInstrumentsRequest)          returns (ListInstrumentsResponse);
  rpc QueryRefdataWatermark        (QueryWatermarkRequest)           returns (WatermarkResponse);
  rpc QueryMarketStatus            (QueryMarketStatusRequest)        returns (MarketStatusResponse);
}

enum DisclosureLevel {
  DISCLOSURE_LEVEL_UNSPECIFIED = 0;
  BASIC    = 1;
  EXTENDED = 2;
  FULL     = 3;
}

message QueryInstrumentByIdRequest {
  string instrument_id  = 1;
  DisclosureLevel level = 2;
}

message QueryByVenueSymbolRequest {
  string venue           = 1;
  string instrument_exch = 2;
  DisclosureLevel level  = 3;
}

message ListInstrumentsRequest {
  string venue          = 1;  // optional filter
  bool   enabled_only   = 2;
  DisclosureLevel level = 3;
}

message InstrumentRefdataResponse {
  string instrument_id   = 1;
  string venue           = 2;
  string instrument_exch = 3;
  string instrument_type = 4;
  string lifecycle_state = 5;  // active | disabled | deprecated
  double tick_size       = 6;
  double lot_size        = 7;
  int64  updated_at_ms   = 8;
}

message ListInstrumentsResponse {
  repeated InstrumentRefdataResponse instruments = 1;
}

message QueryWatermarkRequest {}
message WatermarkResponse {
  int64 watermark_ms = 1;
}

message QueryMarketStatusRequest {
  string venue  = 1;
  string market = 2;
}
message MarketStatusResponse {
  string venue           = 1;
  string market          = 2;
  string session_state   = 3;  // open | closed | halted | pre_open | special
  int64  effective_at_ms = 4;
}
```

### DB table (new, applied by service on startup)

```sql
create table if not exists cfg.instrument_refdata (
  instrument_id    text primary key,
  venue            text not null,
  instrument_exch  text not null,
  instrument_type  text not null,
  lifecycle_state  text not null default 'active',
  tick_size        double precision,
  lot_size         double precision,
  extra_props      jsonb,
  updated_at       timestamptz not null default now()
);
```

Seed row added to `docker/dev/init/postgres/01_seed.sql`:
```sql
insert into cfg.instrument_refdata (instrument_id, venue, instrument_exch, instrument_type, tick_size, lot_size)
values ('BTCUSDT_MOCK', 'MOCK', 'BTC-USDT', 'spot', 0.01, 0.001)
on conflict do nothing;
```

### KV registration

Registers in bucket `zk-svc-registry-v1` (dashes — NATS KV cannot contain dots) under key:

```
svc.refdata.refdata_dev_1
```

Value (same generic contract as other services):
```json
{
  "service_type": "refdata",
  "logical_id":   "refdata_dev_1",
  "grpc_endpoint": "grpc://refdata-svc:50052",
  "capabilities": [
    "query_instrument_refdata",
    "query_refdata_by_venue_symbol",
    "list_instruments",
    "query_refdata_watermark",
    "query_market_status"
  ],
  "registered_at_ms": 1234567890000
}
```

### SDK discovery note

`RefdataSdk` in `zk-trading-sdk-rs` watches prefix `svc.refdata.>` in bucket `zk-svc-registry-v1`
(dashes). The SDK config doc shows `ZK_DISCOVERY_BUCKET` defaulting to `zk.svc.registry.v1` (dots)
— this is a known naming inconsistency in `sdk.md`. The Rust implementation must use
`zk-svc-registry-v1` (dashes).

Once `svc.refdata.refdata_dev_1` is registered, `RefdataSdk` resolves the `grpc_endpoint` and
opens a channel. Hot-path SDK calls against this service:

- `refdata.query_instrument_refdata("BTCUSDT_MOCK")` → lookup by instrument_id
- `refdata.query_instrument_by_venue_symbol("MOCK", "BTC-USDT")` → used by RTMD subject mapping
- `refdata.query_market_status("MOCK", "MOCK")` → returns stub `closed` in this phase

### Implementation notes

- Python 3.12, grpcio-tools, asyncpg
- `uv` for dependency management
- `zkbot/services/zk-refdata-svc/`
- Port: `50052`
- `QueryMarketStatus` returns stub `session_state: "closed"` in this phase; real session logic deferred

### docker-compose addition

```yaml
  refdata-svc:
    build:
      context: ../..
      dockerfile: docker/dev/Dockerfile.refdata-svc
    ports:
      - "50052:50052"
    environment:
      ZK_NATS_URL:            "nats://nats:4222"
      ZK_PG_URL:              "postgresql://zk:zk@postgres:5432/zkbot"
      ZK_REFDATA_LOGICAL_ID:  "refdata_dev_1"
    depends_on:
      nats:
        condition: service_healthy
      postgres:
        condition: service_healthy
```

---

## Deliverable 2 — Pilot Execution Scaffold (`zk-pilot-scaffold`)

**Priority: required for Phase 8 (Engine) — not needed for Phase 7 (SDK)**

### Scope

Exposes the minimum REST surface that the Strategy Engine calls at startup and shutdown per the
unified bootstrap contract in `bootstrap_and_runtime_config.md`:

> 1. authenticate to Pilot using bootstrap identity/token
> 2. receive bootstrap grant plus effective enriched runtime config
> 3. fetch required secrets from Vault using `secret_ref`
> ...

In this phase the scaffold skips token authentication (dev mode) and returns enriched config sourced
from PG seed data. It allocates `execution_id`, records the `instance_id` lease claim to prevent
Snowflake ID collisions, and returns the config payload the engine needs to initialize
`TradingClient`.

Endpoints match Phase 10 Pilot exactly — the real Pilot replaces this scaffold with no change to
engine callers.

### REST API (subset)

```
POST   /v1/strategy-executions/start
POST   /v1/strategy-executions/stop
GET    /v1/strategy-executions/{execution_id}
GET    /v1/strategies/{strategy_key}
GET    /v1/health
```

#### `POST /v1/strategy-executions/start`

Request body:
```json
{
  "strategy_key": "strat_test",
  "instance_id": 1,
  "runtime_params": {}
}
```

Response body (`201 Created`):
```json
{
  "execution_id": "<uuid-allocated-by-pilot>",
  "strategy_key": "strat_test",
  "instance_id": 1,
  "status": "running",
  "enriched_config": {
    "account_ids": [9001],
    "instruments": ["BTCUSDT_MOCK"],
    "nats_url": "nats://nats:4222",
    "discovery_bucket": "zk-svc-registry-v1",
    "secret_refs": {
      "vault_path": "kv/trading/gw/9001"
    }
  }
}
```

On this call the scaffold:
1. Checks `cfg.instance_id_lease` — rejects if another active entry exists for `(env, instance_id)`
2. Upserts an active lease row with TTL of 5 minutes
3. Inserts a `cfg.strategy_execution` row with the allocated UUID `execution_id`
4. Derives `enriched_config` from PG seed data (`cfg.strategy`, `cfg.account`, account→oms bindings)

`execution_id` is a UUID v4 allocated by the scaffold. The engine must treat it as opaque and
pass it on KV registration.

#### `POST /v1/strategy-executions/stop`

Request body:
```json
{
  "execution_id": "<uuid>",
  "stop_reason": "graceful"
}
```

Response (`200 OK`):
```json
{
  "execution_id": "<uuid>",
  "status": "stopped",
  "stopped_at": "<iso8601>"
}
```

On this call the scaffold releases the `cfg.instance_id_lease` row for `(env, instance_id)`.

#### `GET /v1/strategy-executions/{execution_id}`

Returns current row from `cfg.strategy_execution` or `404`.

#### `GET /v1/strategies/{strategy_key}`

Returns strategy definition row from `cfg.strategy` or `404`.

### DB schema

```sql
-- cfg.strategy (seeded by 01_seed.sql)
create table if not exists cfg.strategy (
  strategy_key   text primary key,
  display_name   text,
  runtime_type   text not null,   -- 'RUST' | 'PYTHON'
  config_schema  jsonb,
  created_at     timestamptz not null default now()
);

-- cfg.strategy_execution (new, applied by scaffold on startup)
create table if not exists cfg.strategy_execution (
  execution_id   text primary key,
  strategy_key   text not null references cfg.strategy(strategy_key),
  instance_id    int,
  status         text not null default 'running',
  started_at     timestamptz not null default now(),
  stopped_at     timestamptz,
  stop_reason    text,
  runtime_info   jsonb
);

-- cfg.instance_id_lease (defined in Phase 5; used here for validation)
-- create table cfg.instance_id_lease (
--   env          text not null,
--   instance_id  int  not null,
--   logical_id   text not null,
--   leased_until timestamptz not null,
--   primary key (env, instance_id)
-- );
```

### Implementation notes

- Python 3.12, FastAPI, asyncpg
- `uv` for dependency management
- `zkbot/services/zk-pilot-scaffold/src/main.py`
- Port: `8080` (matches `ZK_PILOT_API_URL=http://localhost:8080` in `.env`)
- Guard: `ZK_SCAFFOLD_MODE=true` required; service refuses to start without it, preventing
  accidental scaffold startup in production

### docker-compose addition

```yaml
  pilot-scaffold:
    build:
      context: ../..
      dockerfile: docker/dev/Dockerfile.pilot-scaffold
    ports:
      - "8080:8080"
    environment:
      ZK_PG_URL:          "postgresql://zk:zk@postgres:5432/zkbot"
      ZK_ENV:             "dev"
      ZK_SCAFFOLD_MODE:   "true"
    depends_on:
      postgres:
        condition: service_healthy
```

---

## Unit tests

### Refdata gRPC
- `test_query_by_venue_symbol_found` — query `(MOCK, BTC-USDT)` returns `BTCUSDT_MOCK`
- `test_query_by_id_found` — query `BTCUSDT_MOCK` returns canonical row
- `test_list_instruments_mock_venue` — list MOCK, `enabled_only=true` returns `[BTCUSDT_MOCK]`
- `test_query_unknown_id_returns_not_found` — unknown id returns gRPC `NOT_FOUND`
- `test_market_status_stub` — any query returns `session_state: "closed"`

### Pilot scaffold
- `test_start_execution_allocates_uuid` — POST start returns UUID `execution_id`
- `test_start_execution_inserts_pg_row` — `cfg.strategy_execution` row created with correct fields
- `test_start_execution_records_instance_id_lease` — `cfg.instance_id_lease` row upserted
- `test_start_execution_rejects_duplicate_instance_id` — second start with same `instance_id` returns `409`
- `test_stop_execution_releases_lease` — stop updates execution status and deletes lease row
- `test_get_execution_404_unknown` — GET unknown `execution_id` returns `404`

---

## Integration tests

Run with `pytest tests/integration/ --docker` against docker-compose stack.

- `test_refdata_registers_in_kv` — after `make dev-up`, KV key `svc.refdata.refdata_dev_1` exists with `grpc_endpoint`
- `test_refdata_grpc_query_via_sdk` — `RefdataSdk` discovers endpoint from KV, calls `QueryInstrumentById("BTCUSDT_MOCK")`, returns correct row
- `test_pilot_scaffold_start_stop_roundtrip` — POST start → assert `execution_id` returned and PG row exists; POST stop → assert `status: stopped` and lease released

---

## Exit criteria

### Refdata service (required for Phase 7 SDK start)
- [x] `make dev-up` starts `refdata-svc` successfully
- [x] `nats kv get zk-svc-registry-v1 svc.refdata.refdata_dev_1` returns JSON with `grpc_endpoint`
- [x] `grpcurl -plaintext localhost:50052 zk.refdata.v1.RefdataService/ListInstruments` returns `BTCUSDT_MOCK`
- [x] `cfg.instrument_refdata` contains the `BTCUSDT_MOCK` seed row
- [x] Refdata unit tests pass (9/9)

### Pilot scaffold (required for Phase 8 Engine start)
- [x] Pilot scaffold router implemented and wired into existing `zk-pilot` service at port 8090
  - Note: scaffold endpoints added to `zk-pilot` (port 8090) rather than a separate service at 8080;
    uses existing `cfg.strategy_definition` and `cfg.strategy_instance` tables (actual DB schema)
- [x] `POST /v1/strategy-executions/start` returns `201` with a UUID `execution_id` and `enriched_config`
- [x] `POST /v1/strategy-executions/stop` returns `200` with `status: stopped`
- [x] Pilot scaffold integration tests pass (11/11)

### Schema notes (actual vs. planned)
The plan doc used proposed names that diverged from the real DB schema. Actual implementation uses:
- `cfg.strategy_definition` (not `cfg.strategy`) with column `strategy_id` (not `strategy_key`)
- `cfg.strategy_instance` (not `cfg.strategy_execution`) with `execution_id` PK
- No `cfg.instance_id_lease` table; duplicate-instance protection deferred to Phase 10 Pilot

---

## Deferred (not in this phase)

- Full Pilot 5-domain REST API (Phase 10)
- Venue loader adaptors and automated instrument refresh
- Market session calendar and real `QueryMarketStatus` responses
- Refdata change events (`zk.control.refdata.updated`) and SDK cache invalidation
- Progressive disclosure `EXTENDED` / `FULL` response levels
- Pilot orchestrator adaptors (DockerEngineAdaptor, KubernetesAdaptor)
- Bootstrap token authentication on the scaffold REST endpoints
