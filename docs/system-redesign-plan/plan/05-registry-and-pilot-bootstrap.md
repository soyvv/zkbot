# Phase 4: Registry + Pilot Bootstrap

## Goal

Implement:
1. **`KvRegistryClient`** in `zk-infra-rs` — reusable library for KV registration and heartbeat used by all services
2. **Pilot bootstrap subjects** — NATS request/reply handlers (`zk.bootstrap.register` etc.) for first-registration authorization
3. **`instance_id` lease management** — `cfg.instance_id_lease` table + Pilot validation to prevent Snowflake ID collisions

## Design Principle

Keep the service discovery mechanism stable and business-agnostic.

That means:

- the NATS KV registry should expose a generic `ServiceRegistration` contract
- Pilot should interpret business meaning from control-plane metadata
- service-specific policies should live in Pilot configuration/validation, not in the registry protocol itself

Examples of registration kinds Pilot may support:

- `gw`
- `oms`
- `strategy`
- `oms+gw`
- `oms+gw+strategy`

The registry/discovery path should not require a separate protocol for each of these combinations.

## Prerequisites

- Phase 1 complete (`zk-infra-rs`, proto)
- Phase 2 complete (OMS service registers via `KvRegistryClient`)
- Phase 3 complete (gateway service registers via `KvRegistryClient`)
- docker-compose stack running (NATS JetStream, PostgreSQL)

## Deliverables

### 4.1 `KvRegistryClient` — `zk-infra-rs`

Location: `zkbot/rust/crates/zk-infra-rs/src/nats_kv_registry.rs`

```rust
pub struct KvRegistryClient {
    kv: JetStreamKvStore,
    config: RegistryConfig,
    session_id: String,
}

impl KvRegistryClient {
    /// Register entry and start background heartbeat task.
    pub async fn register_and_heartbeat(
        &self,
        key: &str,
        payload: ServiceRegistration,
    ) -> Result<RegistrationHandle>;
}

pub struct RegistrationHandle {
    /// Cancel to stop heartbeat and let TTL expire naturally.
    pub cancel: CancellationToken,
}
```

Heartbeat loop (internal):
1. every 5s: update `lease_expiry_ms = now + 20_000` in payload
2. CAS-write `kv_key` with updated payload
3. CAS-write `lock_key` with `{owner_session_id, expiry_ms}`
4. on CAS failure: emit log warning "CAS conflict on <key>, re-bootstrapping" and return error to caller (caller decides whether to re-bootstrap)

`ServiceRegistration` proto:
- `service_type`, `service_id`, `instance_id` (pod/process identifier)
- `transport: TransportInfo` (`protocol`, `address`, `authority`)
- `account_ids: Vec<i64>`
- `venue: String`
- `capabilities: Vec<String>`
- `lease_expiry_ms: i64`
- `updated_at_ms: i64`

Architecture reference:

- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Topology Registration](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)

### 4.2 Pilot bootstrap NATS handlers

Location: `zkbot/services/zk-pilot/` (Python, as part of the Pilot service skeleton)

For Phase 4 this is a **minimal standalone bootstrap service** that handles registration requests only. Full Pilot is Phase 8.

#### Bootstrap subjects

`zk.bootstrap.register` handler:

```python
async def handle_register(msg: NatsMsg):
    req = BootstrapRegisterRequest.parse(msg.data)
    # 1. validate token (check cfg.service_token table in PG)
    # 2. load logical_instance metadata / registration profile from Pilot DB
    # 3. check duplicate policy for the logical identity
    # 4. apply type-specific rules from metadata/profile
    #    (engine lease, strategy singleton, composite role constraints, etc.)
    # 5. generate owner_session_id (UUID)
    # 6. issue scoped NATS credential (or return existing shared cred for dev)
    # 7. record session / audit state
    # 8. reply with BootstrapRegisterResponse
    await msg.respond(response.serialize())
```

`zk.bootstrap.deregister` handler:

```python
async def handle_deregister(msg: NatsMsg):
    req = BootstrapDeregisterRequest.parse(msg.data)
    # 1. look up session in cfg.bootstrap_session
    # 2. mark as deregistered
    # 3. release instance_id lease if instance_type == 'engine'
    await msg.respond(CommandAck(success=True).serialize())
```

#### PostgreSQL tables

```sql
-- Bootstrap sessions (audit trail)
create table cfg.bootstrap_session (
  session_id      text primary key,
  logical_id      text not null,
  instance_type   text not null,        -- 'gw', 'oms', 'engine', 'mdgw', 'rec'
  instance_id_val int,                  -- Snowflake instance_id (engines only)
  registered_at   timestamptz not null default now(),
  deregistered_at timestamptz,
  status          text not null default 'active'  -- 'active' | 'deregistered' | 'fenced'
);

-- Service tokens (pre-provisioned per logical service)
create table cfg.service_token (
  token_hash    text primary key,        -- SHA-256 of token
  logical_id    text not null,
  instance_type text not null,
  env           text not null,
  expires_at    timestamptz,
  created_at    timestamptz not null default now()
);

-- Snowflake instance_id leases
create table cfg.instance_id_lease (
  env          text not null,
  instance_id  int not null,             -- 0–1023 Snowflake worker id
  logical_id   text not null,
  leased_until timestamptz not null,
  primary key (env, instance_id)
);
```

Architecture reference:

- [Topology Registration](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)

#### Token validation

For dev/test: `cfg.service_token` is pre-seeded with known tokens (from `01_seed.sql`).
For production: tokens generated by Pilot admin UI and distributed to operators.

Token check:
1. `SELECT * FROM cfg.service_token WHERE token_hash = sha256($token)`
2. assert `env` matches, `expires_at > now()`
3. assert `logical_id` matches claim

#### instance_id lease validation (for engines)

On `handle_register` for `instance_type == 'engine'`:
```sql
INSERT INTO cfg.instance_id_lease (env, instance_id, logical_id, leased_until)
VALUES ($env, $instance_id, $logical_id, now() + interval '5 minutes')
ON CONFLICT (env, instance_id) DO UPDATE
  SET logical_id   = EXCLUDED.logical_id,
      leased_until = EXCLUDED.leased_until
  WHERE cfg.instance_id_lease.leased_until < now()  -- only if expired
RETURNING *;
```
If `ON CONFLICT` row not updated (another active lease holds it), return error `INSTANCE_ID_CONFLICT`.

On `handle_deregister`:
```sql
DELETE FROM cfg.instance_id_lease WHERE env = $env AND instance_id = $instance_id AND logical_id = $logical_id;
```

Auto-expiry: a cleanup job runs every minute:
```sql
DELETE FROM cfg.instance_id_lease WHERE leased_until < now();
```

Generalization rule:

- `instance_id` leasing is an engine-specific Pilot policy layered on top of the generic bootstrap/session model
- adding new registration kinds should not require changing the generic KV discovery contract

### 4.3 `KvDiscoveryClient` — `zk-infra-rs`

Location: `zkbot/rust/crates/zk-infra-rs/src/nats_kv_discovery.rs`

Used by `trading_sdk` and any consumer watching the registry.

```rust
pub struct KvDiscoveryClient {
    kv: JetStreamKvStore,
    cache: Arc<RwLock<HashMap<String, ServiceRegistration>>>,
}

impl KvDiscoveryClient {
    /// Fetch initial snapshot and start background watch loop.
    pub async fn start_watch(&self) -> Result<()>;

    /// Resolve entries matching a predicate (e.g. account_id membership).
    pub fn resolve(&self, predicate: impl Fn(&ServiceRegistration) -> bool) -> Vec<ServiceRegistration>;

    /// Watch for changes to a specific key prefix.
    pub fn on_change(&self, prefix: &str, callback: impl Fn(KeyChange) + Send + 'static);
}
```

Architecture reference:

- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)

## Tests

### Unit tests

- `test_kv_registry_heartbeat_updates_expiry`: verify heartbeat increments `lease_expiry_ms` each cycle
- `test_kv_registry_cas_conflict_returns_error`: mock CAS failure → client returns `RegistrationError::CasFailed`
- `test_kv_discovery_resolve_by_account_id`: populate in-memory cache with two OMS entries; resolve by `account_id=9001` → returns correct entry
- `test_instance_id_lease_conflict_detection`: two requests with same `instance_id` in same env → second returns `INSTANCE_ID_CONFLICT`
- `test_bootstrap_token_validation_expired`: send request with expired token → returns `TOKEN_EXPIRED` error

### Integration tests (docker-compose)

- `test_register_and_heartbeat_roundtrip`:
  1. call `KvRegistryClient::register_and_heartbeat("svc.oms.oms_dev_1", payload)`
  2. watch KV bucket → confirm key present within 2s
  3. cancel handle → confirm key expires within 25s
- `test_bootstrap_register_subject`:
  1. publish to `zk.bootstrap.register` with valid dev token
  2. assert response contains `owner_session_id`, `kv_key`, `status=OK`
  3. assert `cfg.bootstrap_session` row created in PG
- `test_bootstrap_instance_id_lease`:
  1. register engine with `instance_id=5`
  2. attempt second register with `instance_id=5`, same env → assert `INSTANCE_ID_CONFLICT`
  3. deregister first → attempt again → succeeds
- `test_kv_discovery_watch_update`:
  1. start `KvDiscoveryClient`
  2. write new entry to KV for `svc.oms.oms_dev_1`
  3. assert `on_change` callback fires within 1s

## Exit criteria

- [ ] `cargo build -p zk-infra-rs` with `KvRegistryClient` and `KvDiscoveryClient` succeeds
- [ ] bootstrap service starts and subscribes to `zk.bootstrap.register` within docker-compose
- [ ] unit tests pass: `cargo test -p zk-infra-rs kv_registry` and `cargo test -p zk-infra-rs kv_discovery`
- [ ] integration test `test_register_and_heartbeat_roundtrip` passes
- [ ] integration test `test_bootstrap_register_subject` passes
- [ ] `test_bootstrap_instance_id_lease` conflict detection works correctly
- [ ] KV entry expires within 25s after client handle cancelled (TTL enforced by NATS JetStream)
- [ ] `cfg.bootstrap_session` and `cfg.instance_id_lease` tables exist in PG and are seeded
