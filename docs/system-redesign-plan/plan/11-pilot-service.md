# Phase 10: Pilot Service

## Goal

Implement `zk-pilot` — the control-plane service that:
- handles bootstrap registration and deregistration over NATS request/reply
- manages control-plane metadata in PostgreSQL
- owns RTMD policy/default state and RTMD subscription observability
- watches NATS KV for live topology reconciliation
- serves a comprehensive REST API for operator, admin, and UI workflows
- acts as the UI backend for manual trading, strategy lifecycle, risk, topology, and refdata ops
- optionally coordinates runtime start/stop through a runtime-orchestrator adaptor (k8s/Docker)

The Phase 6 scaffolding services bootstrap-only stub is promoted to the full Pilot service in this phase.

## Implementation Language

Architecture recommendation: **Java** (primary), or Go.

Rationale (from [pilot_service.md](../../system-arch/services/pilot_service.md)):

- strong support for larger REST backends and layered service design
- mature ecosystem for OIDC, RBAC, DB migrations, typed APIs, schedulers, and admin workflows
- a good fit for DTO-heavy control-plane logic and long-lived backend maintenance
- better team velocity if Java is already the stronger language

Go is a valid alternative: simpler runtime footprint, strong for infra/control-plane services.

Python (FastAPI) may be used for an early prototype if it accelerates delivery, but the
architecture recommendation is Java or Go given the target service scope.

Key constraint: **keep bootstrap and service-discovery contracts language-agnostic**. The
PostgreSQL schema and KV/NATS contracts remain the shared boundary regardless of implementation
language.

## Prerequisites

- Phase 6 complete (bootstrap subjects, `instance_id` lease table)
- Phase 8 complete (Engine service calls Pilot REST on startup/shutdown)
- Phase 9 complete (Recorder/Monitor operational)
- docker-compose stack running (NATS, PG, Redis)

## Deliverables

### 9.1 REST API (7 domains)

Reference: [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

---

#### Domain 1 — Trading (manual orders and account views)

```
POST   /v1/manual/orders                       -- submit one manual order through OMS
POST   /v1/manual/orders:batch                 -- submit multiple manual orders
POST   /v1/manual/cancels                      -- submit one or more manual cancel requests
POST   /v1/manual/panic                        -- operator panic/kill-switch for account or OMS
POST   /v1/manual/panic/clear                  -- clear panic state where policy allows
GET    /v1/manual/orders/{order_id}            -- inspect manual-order status
GET    /v1/manual/trades                       -- inspect recent manual-trading fills
GET    /v1/accounts                            -- list accounts visible to operator
GET    /v1/accounts/{account_id}               -- fetch summary view for one account
GET    /v1/accounts/{account_id}/balances      -- fetch current balance view
GET    /v1/accounts/{account_id}/positions     -- fetch current positions
GET    /v1/accounts/{account_id}/orders/open   -- fetch current open orders
GET    /v1/accounts/{account_id}/trades        -- fetch recent trades/fills
```

Design note:

- Pilot initiates or routes the operation through OMS/gateway control surfaces
- Pilot must not become a second independent order-state authority
- account views aggregate from OMS (via gRPC or Redis read) and are query-only

---

#### Domain 2 — System Topology / System Ops

```
GET    /v1/topology                            -- return current system topology view
PUT    /v1/topology/bindings                   -- create or update logical topology bindings
GET    /v1/topology/services                   -- list service instances and metadata
GET    /v1/topology/sessions                   -- list active bootstrap/runtime sessions
POST   /v1/services/{logical_id}/issue-bootstrap-token  -- issue or rotate bootstrap token
POST   /v1/services/{logical_id}/reload        -- request config reload for one service
POST   /v1/ops/reload                          -- broader reload workflow for selected scope
POST   /v1/ops/restart                         -- bounded restart workflow for selected scope
```

Design note:

- service topology comes from Pilot's KV watcher cache
- start/stop/restart requests are forwarded to the runtime-orchestrator adaptor (§9.5)

---

#### Domain 3 — Bot (strategy definition and execution)

```
POST   /v1/strategies                          -- create strategy definition + baseline config
PUT    /v1/strategies/{strategy_key}           -- update strategy config or metadata
GET    /v1/strategies                          -- list strategy definitions
GET    /v1/strategies/{strategy_key}           -- fetch one strategy definition + status summary
POST   /v1/strategies/{strategy_key}/validate  -- validate config, bindings, and symbol refs

POST   /v1/strategy-executions/start           -- claim a new live execution for a strategy
POST   /v1/strategy-executions/stop            -- stop/finalize a live execution
POST   /v1/strategy-executions/{execution_id}/restart  -- bounded restart of one execution
GET    /v1/strategy-executions/{execution_id}  -- fetch runtime status for one execution
GET    /v1/strategies/{strategy_key}/executions -- list execution history for one strategy
GET    /v1/strategies/{strategy_key}/logs      -- fetch strategy logs
```

`POST /v1/strategy-executions/start` request:
```json
{
  "strategy_key": "strat_test",
  "runtime_info": { "host": "...", "pid": "..." }
}
```

Processing:
1. look up `cfg.strategy` row by `strategy_key`
2. validate strategy is enabled; enforce singleton policy
3. atomically allocate authoritative `execution_id` (UUID)
4. insert `cfg.strategy_execution` row
5. optionally forward start request to runtime orchestrator if managed mode
6. return execution claim:
   ```json
   {
     "execution_id": "<uuid>",
     "strategy_key": "strat_test",
     "symbols": ["BTC-USDT-PERP"],
     "account_ids": [9001],
     "strategy_script_file_name": "my_strategy.py",
     "tq_config": { ... }
   }
   ```

`POST /v1/strategy-executions/stop` request:
```json
{
  "strategy_key": "strat_test",
  "execution_id": "<uuid>",
  "reason": "graceful_shutdown"
}
```

Processing:
1. mark `cfg.strategy_execution.status = 'stopped'`, set `stopped_at`
2. release `cfg.instance_id_lease` row for the execution
3. optionally notify runtime orchestrator to stop the process

---

#### Domain 4 — Risk / Monitor

```
GET    /v1/risk/accounts/{account_id}          -- fetch risk config and risk-state summary
PUT    /v1/risk/accounts/{account_id}          -- update account-level risk config
GET    /v1/risk/alerts                         -- list active or recent risk/monitor alerts
GET    /v1/risk/alerts/{alert_id}              -- fetch one alert with detailed context
GET    /v1/risk/summary                        -- aggregated cross-account risk summary
POST   /v1/risk/accounts/{account_id}/disable  -- disable account trading
POST   /v1/risk/accounts/{account_id}/enable   -- re-enable account trading after review
```

---

#### Domain 5 — Trading Ops (gateway, OMS, MDGW, refdata, audit)

```
POST   /v1/gateways                            -- onboard a trading gateway runtime definition
GET    /v1/gateways                            -- list configured gateway runtimes
GET    /v1/gateways/{gw_id}                    -- fetch one gateway definition and status
POST   /v1/gateways/{gw_id}/reload             -- request config reload for one gateway
POST   /v1/gateways/{gw_id}/issue-bootstrap-token

POST   /v1/oms                                 -- onboard an OMS runtime definition
GET    /v1/oms                                 -- list OMS runtimes
GET    /v1/oms/{oms_id}                        -- fetch one OMS config and status
POST   /v1/oms/{oms_id}/reload                 -- trigger ReloadConfig on live OMS
POST   /v1/oms/{oms_id}/issue-bootstrap-token

POST   /v1/mdgw                                -- onboard a shared RTMD gateway runtime
GET    /v1/mdgw                                -- list configured RTMD gateway runtimes
GET    /v1/mdgw/{logical_id}                   -- fetch one RTMD gateway definition and status
POST   /v1/mdgw/{logical_id}/reload            -- request config reload for one RTMD gateway
POST   /v1/mdgw/{logical_id}/issue-bootstrap-token

GET    /v1/mdgw/subscriptions                  -- list control-plane policy/default subscriptions
POST   /v1/mdgw/subscriptions                  -- add or update control-plane policy/default
DELETE /v1/mdgw/subscriptions/{id}             -- disable control-plane policy/default
POST   /v1/mdgw/subscriptions/reload           -- trigger reload for venue or logical RTMD gateway
GET    /v1/mdgw/subscriptions/effective        -- inspect effective desired set for a target

GET    /v1/refdata/instruments                 -- browse instrument refdata
GET    /v1/refdata/instruments/{instrument_id} -- fetch one instrument canonical refdata
PATCH  /v1/refdata/instruments/{instrument_id} -- update refdata lifecycle state
POST   /v1/refdata/instruments/{instrument_id}/refresh -- request targeted refresh
GET    /v1/refdata/markets/{venue}/{market}/status     -- fetch current market session state
GET    /v1/refdata/markets/{venue}/{market}/calendar   -- fetch market calendar
POST   /v1/refdata/refresh                     -- request broader refdata refresh

GET    /v1/audit/registrations                 -- inspect bootstrap/registration audit history
GET    /v1/audit/reconciliation                -- inspect reconciliation/audit outcomes
POST   /v1/ops/reconcile                       -- request bounded reconciliation workflow
```

`POST /v1/oms/{oms_id}/reload`:
- resolve OMS gRPC endpoint from NATS KV watcher cache
- call `OMSService::ReloadConfig(ReloadConfigRequest{})`

RTMD subscription separation rule:

- clients publish live subscription interest directly into `zk.rtmd.subs.v1` KV space
- RTMD gateways watch live interest and react without going through Pilot
- Pilot owns control-plane policy/default state in `cfg.mdgw_subscription` only
- `POST /v1/mdgw/subscriptions/reload` publishes:
  - `zk.control.mdgw.<target_logical_id>.reload` (if `target_logical_id` given)
  - `zk.control.mdgw.<venue>.reload` (otherwise)

---

### 9.2 PostgreSQL Schema

```sql
-- Strategy definitions (stable logical identity, keyed by strategy_key)
create table cfg.strategy (
  strategy_key       text primary key,
  display_name       text not null,
  runtime_type       text not null,        -- 'RUST' | 'PYTHON'
  script_file_name   text,                 -- for Python strategies
  account_ids        bigint[] not null default '{}',
  symbols            text[] not null default '{}',
  tq_config          jsonb,
  is_enabled         boolean not null default true,
  created_at         timestamptz not null default now(),
  updated_at         timestamptz not null default now()
);

-- Execution records (one per live or historical run of a strategy)
-- strategy_key is the stable identity; execution_id identifies one concrete run
create table cfg.strategy_execution (
  execution_id       text primary key,     -- UUID allocated by Pilot (never by the engine)
  strategy_key       text not null references cfg.strategy(strategy_key),
  instance_id        int,                  -- Snowflake instance_id (if applicable)
  status             text not null default 'running',  -- 'running' | 'stopped' | 'fenced' | 'crashed'
  started_at         timestamptz not null default now(),
  stopped_at         timestamptz,
  stop_reason        text,
  runtime_info       jsonb                 -- host, pid, runtime_version
);

-- Logical instance metadata (generic, all service types)
create table cfg.logical_instance (
  logical_id         text primary key,
  instance_type      text not null,        -- 'gw' | 'oms' | 'engine' | 'mdgw' | 'rec' | etc.
  metadata           jsonb,               -- registration_kind, composite roles, venue/account scope
  created_at         timestamptz not null default now()
);

-- Gateway instance definitions
create table cfg.gateway_instance (
  gw_id              text primary key,
  venue              text not null,
  account_id         bigint not null,
  capabilities       text[] not null default '{}',
  created_at         timestamptz not null default now()
);

-- OMS instance definitions
create table cfg.oms_instance (
  oms_id             text primary key,
  account_ids        bigint[] not null default '{}',
  risk_config        jsonb,
  created_at         timestamptz not null default now()
);

-- Bootstrap sessions (audit trail)
create table cfg.bootstrap_session (
  session_id         text primary key,
  logical_id         text not null,
  instance_type      text not null,
  instance_id_val    int,
  registered_at      timestamptz not null default now(),
  deregistered_at    timestamptz,
  status             text not null default 'active'   -- 'active' | 'deregistered' | 'fenced'
);

-- Service tokens (pre-provisioned per logical service)
create table cfg.service_token (
  token_hash         text primary key,
  logical_id         text not null,
  instance_type      text not null,
  env                text not null,
  expires_at         timestamptz,
  created_at         timestamptz not null default now()
);

-- Snowflake instance_id leases (engines only)
create table cfg.instance_id_lease (
  env                text not null,
  instance_id        int not null,
  logical_id         text not null,
  leased_until       timestamptz not null,
  primary key (env, instance_id)
);

-- RTMD subscription policy/defaults (slow control-plane layer only)
create table cfg.mdgw_subscription (
  id                 bigserial primary key,
  venue              text not null,
  instrument_exch    text not null,
  channel_types      text[] not null,
  kline_intervals    text[],
  scope_kind         text not null default 'global',  -- 'global' | 'logical_instance'
  target_logical_id  text,
  requested_by       text,
  is_enabled         boolean not null default true,
  created_at         timestamptz not null default now()
);
```

### 9.3 Bootstrap NATS subjects (promoted from Phase 5 stub)

Promote the Phase 5 minimal bootstrap service into the full Pilot process:

- `zk.bootstrap.register` → `handle_register` (full token validation + execution-claim + audit)
- `zk.bootstrap.deregister` → `handle_deregister`
- `zk.bootstrap.reissue` → `handle_reissue` (credential rotation)
- `zk.bootstrap.sessions.query` → `handle_sessions_query` (admin query)

### 9.4 NATS KV topology watcher

Pilot watches `zk-svc-registry-v1` bucket and maintains an in-memory cache of all live
`ServiceRegistration` entries. Used by topology REST endpoints and OMS endpoint resolution.

```python
class TopologyWatcher:
    async def start(self): ...
    def get_all(self) -> list[ServiceRegistration]: ...
    def get_by_type(self, service_type: str) -> list[ServiceRegistration]: ...
    def get_oms_endpoint(self, oms_id: str) -> str | None: ...
```

### 9.5 Runtime orchestrator adaptor (stub + interface for Phase 9)

Pilot coordinates bounded runtime operations through a pluggable adaptor:

```
interface RuntimeOrchestratorAdaptor {
    start(logical_id, profile) -> StartResult
    stop(logical_id, reason) -> StopResult
    restart(logical_id) -> StartResult
}
```

Implementations:

- `DockerEngineAdaptor` — for local docker-compose (`docker compose up/down`)
- `KubernetesAdaptor` — for k3s/k8s deployments
- `NoOpAdaptor` — stub (log + no-op), acceptable for initial delivery

Important constraint: this adaptor is for **runtime lifecycle only**, not image builds or rollouts.

For Phase 9: implement `NoOpAdaptor` with the interface defined. Tag k8s/Docker implementations
as `TODO(phase-9-orchestrator)`.

### 9.6 High Availability (active-passive leader election)

Use **Option A** (active-passive) for initial production:

- multiple Pilot replicas; one holds NATS KV CAS lock `pilot.leader`
- only leader handles `zk.bootstrap.*` subjects and REST writes
- followers watch KV and serve read-only REST/gRPC
- failover time ≈ 5s (TTL-based lease expiry)

Option B (stateless behind load balancer with idempotent writes) documented as future fallback.

## Tests

### Unit tests

- `test_strategy_execution_start_allocates_execution_id`: `POST /v1/strategy-executions/start` → assert Pilot allocates `execution_id`, not caller
- `test_strategy_execution_singleton_conflict`: start second execution for same `strategy_key` while first is running → assert 409
- `test_strategy_execution_stop_releases_lease`: stop → assert `cfg.instance_id_lease` row released and `cfg.strategy_execution.status = 'stopped'`
- `test_oms_reload_resolves_from_kv`: `POST /v1/oms/oms_dev_1/reload` → assert Pilot looks up OMS gRPC from KV cache and calls `ReloadConfig`
- `test_mdgw_reload_publishes_targeted_nats_event`: `POST /v1/mdgw/subscriptions/reload` with `target_logical_id` → assert `zk.control.mdgw.<logical_id>.reload` published
- `test_topology_watcher_resolves_oms_endpoint`: populate KV with OMS entry → `get_oms_endpoint(oms_id)` returns correct address
- `test_leader_election_single_instance_acquires`: single Pilot instance → `is_leader()` returns `True` within 5s
- `test_leader_election_failover`: simulate primary crash → secondary acquires within 15s
- `test_manual_panic_routes_through_oms_grpc`: `POST /v1/manual/panic` → assert OMS `Panic` gRPC called
- `test_bootstrap_token_validation_expired`: send register with expired token → returns `TOKEN_EXPIRED`

### Integration tests (docker-compose)

- `test_pilot_bootstrap_full_flow`:
  1. start Pilot
  2. publish `zk.bootstrap.register` with valid dev token
  3. assert response OK; assert `cfg.bootstrap_session` row created
- `test_pilot_strategy_execution_lifecycle`:
  1. engine calls `POST /v1/strategy-executions/start` → assert `execution_id` returned, row active
  2. engine calls `POST /v1/strategy-executions/stop` → assert row stopped, lease released
- `test_pilot_oms_reload`:
  1. start OMS + Pilot
  2. `POST /v1/oms/oms_dev_1/reload`
  3. assert OMS `ReloadConfig` gRPC called
- `test_pilot_topology_rest`:
  1. start OMS + mock-gw + Pilot
  2. `GET /v1/topology` → assert `svc.oms.*` and `svc.gw.*` both present
- `test_pilot_rtmd_reload_targets_embedded_runtime`:
  1. start engine registered as `engine+mdgw`
  2. `POST /v1/mdgw/subscriptions/reload` with engine's RTMD logical target
  3. assert `zk.control.mdgw.<logical_id>.reload` published and observed
- `test_pilot_ha_leader_failover` (requires two Pilot replicas):
  1. start two Pilot instances; confirm one is leader
  2. kill leader
  3. confirm second acquires leadership within 15s
  4. confirm bootstrap requests still handled

## Exit criteria

- [ ] `POST /v1/strategy-executions/start` works: `execution_id` allocated by Pilot, config returned, execution row created
- [ ] `POST /v1/strategy-executions/stop` works: row stopped, lease released
- [ ] No `POST /v1/strat/strategy-inst` endpoint — old URL must not exist
- [ ] `GET /v1/topology` returns live NATS KV entries
- [ ] `POST /v1/oms/{oms_id}/reload` calls OMS gRPC `ReloadConfig`
- [ ] `POST /v1/manual/panic` routes panic command to OMS gRPC
- [ ] All 5 REST domains have at least stub routes returning correct HTTP status codes (domains 1–5 from §9.1)
- [ ] `POST /v1/mdgw/subscriptions/reload` can target venue-wide or logical-instance-scoped RTMD
- [ ] Pilot returns the effective desired RTMD subscription set for debugging
- [ ] Bootstrap NATS subjects functional (promoted from Phase 5 stub)
- [ ] Leader election: single Pilot acquires leadership within 5s
- [ ] HA failover: second Pilot acquires leadership within 15s after primary fails
- [ ] `test_pilot_strategy_execution_lifecycle` integration test passes end-to-end
- [ ] `cfg.strategy`, `cfg.strategy_execution`, `cfg.logical_instance`, `cfg.gateway_instance`, `cfg.oms_instance` tables exist in PG migration
- [ ] All unit tests pass; all integration tests pass against docker-compose stack

## TODO

- async job model for long-running workflows (refresh, reconcile, bounded restart)
- extend audit tables and audit flows for newer control-plane scenarios
- partial-failure semantics for multi-step control-plane workflows
- Pilot health/SLO design and degraded-mode behavior
- export/reporting workflow design
- manual-trading safety rails and confirmation policy
- full runtime orchestrator adaptor implementation (docker + k8s)
