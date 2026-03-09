# Phase 8: Pilot Service

## Goal

Implement `zk-pilot` — the control plane service that:
- handles operator REST API (account management, config CRUD, strategy lifecycle)
- serves strategy registration and `instance_id` lease endpoints
- serves `zk.pilot.v1.PilotService` gRPC for internal queries (topology, risk state)
- handles Pilot bootstrap NATS subjects (`zk.bootstrap.*`)
- manages MDGW subscription config (`cfg.mdgw_subscription`)
- watches NATS KV for live topology

The Phase 4 bootstrap-only stub is promoted to a full Pilot service in this phase.

## Prerequisites

- Phase 4 complete (bootstrap subjects, `instance_id` lease table)
- Phase 6 complete (Engine service calls Pilot REST on startup/shutdown)
- Phase 7 complete (Recorder/Monitor operational)
- docker-compose stack running (NATS, PG, Redis)

## Deliverables

### 8.1 REST API

Language: Python (FastAPI)

Location: `zkbot/services/zk-pilot/`

Reference: `services/zk-service/src/tq_service_app/tq_app.py`

#### Strategy lifecycle endpoints

```
POST   /v1/strat/strategy-inst          -- register execution (called by engine on startup)
DELETE /v1/strat/strategy-inst          -- deregister execution (called by engine on shutdown)
GET    /v1/strat/strategy-inst          -- list active executions
```

`POST /v1/strat/strategy-inst` request:
```json
{
  "strategy_key": "strat_test",
  "execution_id": "<uuid>",
  "instance_id": 5
}
```

Processing:
1. look up `cfg.strategy` row by `strategy_key`
2. validate `instance_id` uniqueness via `cfg.instance_id_lease` (upsert or conflict)
3. insert `cfg.strategy_execution` row:
   ```sql
   create table cfg.strategy_execution (
     execution_id  text primary key,
     strategy_key  text not null,
     instance_id   int not null,
     status        text not null default 'running',
     registered_at timestamptz not null default now(),
     deregistered_at timestamptz
   );
   ```
4. return strategy config:
   ```json
   {
     "symbols": ["BTC-USDT-PERP"],
     "account_ids": [9001],
     "tq_config": {...},
     "strategy_script_file_name": "my_strategy.py"
   }
   ```

`DELETE /v1/strat/strategy-inst?strategy_key=<key>&execution_id=<uuid>`:
1. mark `cfg.strategy_execution.status = 'deregistered'`
2. release `cfg.instance_id_lease` row

#### Account management endpoints

```
GET    /v1/accounts                     -- list accounts
POST   /v1/accounts                     -- create account
GET    /v1/accounts/{account_id}        -- get account detail
PATCH  /v1/accounts/{account_id}        -- update account
```

#### OMS instance config endpoints

```
GET    /v1/oms                          -- list OMS instances
POST   /v1/oms                          -- create OMS instance
GET    /v1/oms/{oms_id}                 -- get OMS config
PATCH  /v1/oms/{oms_id}                 -- update OMS config
POST   /v1/oms/{oms_id}/reload          -- trigger ReloadConfig on live OMS
```

`POST /v1/oms/{oms_id}/reload`:
- resolve OMS gRPC endpoint from NATS KV
- call `OMSService::ReloadConfig(ReloadConfigRequest{})`
- return result

#### MDGW subscription endpoints

```
GET    /v1/mdgw/subscriptions           -- list subscriptions
POST   /v1/mdgw/subscriptions           -- add subscription (upsert by venue+instrument)
DELETE /v1/mdgw/subscriptions/{id}      -- disable subscription
POST   /v1/mdgw/subscriptions/reload    -- trigger reload on MDGW for venue
```

`POST /v1/mdgw/subscriptions/reload`:
- publish `zk.control.mdgw.<venue>.reload` on NATS

#### Refdata endpoints

```
GET    /v1/refdata/instruments          -- list instruments (from cfg.instrument_refdata)
GET    /v1/refdata/instruments/{venue}/{instrument_exch}  -- single instrument
```

### 8.2 gRPC service — `zk.pilot.v1.PilotService`

Language: Python (grpcio)

Implement all RPCs from [05-api-contracts.md](../05-api-contracts.md) §2.4:

**Control:**
- `StartEngine`: calls internal process manager or Kubernetes API to start engine pod
- `StopEngine`: sends SIGTERM to engine (or calls Kubernetes delete)
- `ReloadOmsConfig`: calls OMS gRPC `ReloadConfig`

**Queries (backed by NATS KV watch + PG):**
- `QueryServiceTopology`: return all `svc.*` entries from NATS KV watch cache
- `QueryAccountBindings`: read `cfg.account_binding` from PG
- `QueryRiskState`: aggregate risk state from OMS (Redis read) + Monitor (in-memory cache)
- `QueryExecutionState`: read `cfg.strategy_execution` table
- `QueryInstrumentRefdata`: read `cfg.instrument_refdata` table

### 8.3 Bootstrap NATS subjects (promoted from Phase 4 stub)

Promote the Phase 4 minimal bootstrap service into the full Pilot process:
- `zk.bootstrap.register` → `handle_register` (full token validation + lease + audit)
- `zk.bootstrap.deregister` → `handle_deregister`
- `zk.bootstrap.reissue` → `handle_reissue` (for credential rotation)
- `zk.bootstrap.sessions.query` → `handle_sessions_query` (admin query)

### 8.4 NATS KV topology watcher

Pilot watches `zk.svc.registry.v1` bucket and caches all `ServiceRegistration` entries in memory. Used by:
- `QueryServiceTopology` gRPC
- `ReloadOmsConfig` (to resolve OMS gRPC endpoint)
- Operator dashboard (via REST `/v1/topology`)

```python
class TopologyWatcher:
    def __init__(self, nats_client): ...
    async def start(self): ...
    def get_all(self) -> list[ServiceRegistration]: ...
    def get_by_type(self, service_type: str) -> list[ServiceRegistration]: ...
    def get_oms_endpoint(self, oms_id: str) -> str | None: ...
```

### 8.5 High Availability considerations

Pilot is required for bootstrap of new instances. If Pilot is down:
- existing instances continue (direct KV heartbeat, no Pilot in steady state)
- new instances (or fenced/restarting instances) cannot bootstrap until Pilot recovers

HA options (pick one before production):

**Option A (recommended for initial deployment): Active-passive with leader election**
- Multiple Pilot replicas; one holds NATS KV lock `pilot.leader`
- Only leader handles `zk.bootstrap.*` subjects and REST POSTs
- Followers watch KV and serve read-only REST/gRPC
- Leader election via NATS KV CAS (same pattern as service registry lock)
- Failover time: ~5s (TTL-based lease expiry)

**Option B: Stateless behind load balancer**
- All replicas handle all requests
- All write operations (lease, session) must be idempotent (PostgreSQL `ON CONFLICT DO UPDATE`)
- Bootstrap token validation is stateless (PG lookup)
- Suitable if REST idempotency is guaranteed

For Phase 8: implement Option A. Document Option B as fallback.

Leader election implementation:
```python
class PilotLeaderElection:
    LOCK_KEY = "pilot.leader"
    TTL_MS = 10_000  # 10s
    HEARTBEAT_S = 3

    async def try_acquire(self) -> bool: ...
    async def heartbeat_loop(self): ...
    async def release(self): ...
    def is_leader(self) -> bool: ...
```

## Tests

### Unit tests

- `test_strategy_inst_post_registers_execution`: POST with valid strategy_key → assert `cfg.strategy_execution` row created, config returned
- `test_strategy_inst_post_instance_id_conflict`: POST with already-leased `instance_id` → assert 409 response
- `test_strategy_inst_delete_releases_lease`: DELETE → assert `cfg.instance_id_lease` row removed
- `test_mdgw_reload_publishes_nats_event`: POST `/v1/mdgw/subscriptions/reload` → assert NATS `zk.control.mdgw.<venue>.reload` published
- `test_topology_watcher_resolves_oms_endpoint`: populate KV with OMS entry → assert `get_oms_endpoint(oms_id)` returns correct address
- `test_leader_election_single_instance_acquires`: single Pilot instance → `is_leader()` returns `True` within 5s
- `test_leader_election_failover`: simulate primary Pilot crash → secondary acquires within 10s + 5s TTL

### Integration tests (docker-compose)

- `test_pilot_bootstrap_full_flow`:
  1. start Pilot
  2. publish `zk.bootstrap.register` with valid dev token
  3. assert response OK; assert `cfg.bootstrap_session` row created
  4. assert KV entry writable with returned session_id
- `test_pilot_engine_lifecycle`:
  1. start engine → POST to Pilot → assert `cfg.strategy_execution` row active
  2. stop engine (SIGTERM) → DELETE to Pilot → assert row deregistered
  3. assert `cfg.instance_id_lease` released
- `test_pilot_oms_reload`:
  1. start OMS + Pilot
  2. POST `/v1/oms/oms_dev_1/reload`
  3. assert OMS `ReloadConfig` gRPC called (check OMS logs or side effect)
- `test_pilot_topology_rest`:
  1. start OMS + mock-gw + Pilot
  2. GET `/v1/topology` → assert both `svc.oms.*` and `svc.gw.*` present in response
- `test_pilot_ha_leader_failover` (requires two Pilot replicas):
  1. start two Pilot instances
  2. confirm one is leader
  3. kill leader process
  4. confirm second acquires leadership within 15s
  5. confirm bootstrap requests still handled

## Exit criteria

- [ ] `POST /v1/strat/strategy-inst` works: config returned, execution row created, lease acquired
- [ ] `DELETE /v1/strat/strategy-inst` works: row deregistered, lease released
- [ ] `GET /v1/topology` returns live NATS KV entries
- [ ] `POST /v1/oms/{oms_id}/reload` calls OMS gRPC `ReloadConfig`
- [ ] `POST /v1/mdgw/subscriptions/reload` publishes NATS control event
- [ ] Bootstrap NATS subjects functional (from Phase 4 stub promoted to full Pilot)
- [ ] Leader election: single Pilot acquires leadership within 5s
- [ ] HA failover: second Pilot acquires leadership within 15s after primary fails
- [ ] `test_pilot_engine_lifecycle` integration test passes end-to-end
- [ ] All Python unit tests pass: `pytest tests/unit/`
- [ ] All integration tests pass: `pytest tests/integration/ --docker`
