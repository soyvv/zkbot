# Architecture Design Decisions — zkbot

## Bot domain split: strategy definition, bot definition, and run snapshot (2026-03-27)

The bot domain should be modeled as three separate control-plane entities:

- `strategy_definition`
- `engine_instance`
- `strategy_instance`

Rules:

- `strategy_definition` owns only strategy-specific config and metadata
- `strategy_key` is the logical authored strategy identity bound to an engine/bot run
- `strategy_type_key` is the engine implementation selector, for example `smoke-test`,
  `mm`, or `python-wrapper`
- `engine_instance` is the operator-facing bot definition and owns engine/common config, OMS
  workspace binding, connected accounts, interested instruments, and selected strategy
- each bot start creates one `strategy_instance` identified by `execution_id`
- `strategy_instance` must reference both:
  - the owning bot definition via `engine_id`
  - the selected strategy definition via `strategy_id`
- strategy config may use logical account and symbol names, but it should not replace the concrete
  engine connectivity config
- at runtime, the engine uses its configured OMS binding, accounts, and instruments as the physical
  runtime scope and the strategy resolves logical names against that scope
- bot start and bot stop are owned by Pilot's runtime orchestrator
- the current backend may use local process orchestration for development
- the production target backend should be Kubernetes orchestration
- engine bootstrap and self-registration happen after orchestrator launch and do not replace
  orchestrator ownership of lifecycle intent
- run snapshots must preserve the effective config used for that execution even if the strategy or
  bot definition is edited later
- bot management UI should anchor on `engine_instance` and show the current run or last active run
  from `strategy_instance`

Rationale:

- strategy reuse and bot onboarding are different operator concerns
- engine/common runtime config should not be mixed into strategy config
- execution history needs stable run snapshots and correlation across UI, engine, and recorder
- the current strategy-first Pilot model obscures the distinction between reusable strategy
  templates and runnable bot definitions

## OMS recorder publication boundary (2026-03-27)

Recorder-specific OMS persistence should use a dedicated JetStream-backed publish path rather than
the normal low-latency fanout subjects.

Rules:

- OMS keeps plain NATS `order_update` subjects as the low-latency fanout path for strategies and
  monitors
- OMS publishes recorder-facing terminal-order events to a dedicated JetStream subject
- OMS publishes recorder-facing trade events to a dedicated JetStream subject
- JetStream publication must stay outside the OMS writer critical path
- the writer may enqueue recorder publish actions, but it must not await JetStream publish ACKs or
  durable-write round trips inline with state mutation
- recorder owns downstream Postgres retention and partition rotation for `trd.order_oms`

Rationale:

- recorder needs replayable, durable consumption semantics
- strategies and monitors should not pay JetStream durability latency on the hot path
- partition lifecycle belongs to the storage owner, not the event producer

## KvReconciler: periodic sweep for TTL-expired keys (2026-03-22)

NATS KV `max_age` TTL expiry does not emit DELETE/PURGE watch events to KV watchers.
This means `KvReconciler.isKvLive()` returns stale `true` for keys that expired via TTL
(e.g. after an ungraceful shutdown where the heartbeat stops but no explicit delete is sent).

Fix: added a periodic sweep (every 15s) that iterates the `live` set and calls `kv.get(key)`
for each entry. If the key no longer exists, it is removed from `live` and `onKvLost()` is
called to fence the corresponding session.

Without this sweep, a crashed service's session stays `active` in `mon.active_session`
indefinitely, blocking re-registration (Pilot returns `DUPLICATE`).

Detection timeline for ungraceful shutdown: KV TTL expiry (max_age=45s) + sweep interval
(15s) = ~60s worst case.

## Pilot / Engine execution bootstrap (2026-03-21)

### Strategy startup supports both self-bootstrap and Pilot-initiated launch

For strategy execution runtimes, two startup modes are valid:

- self-bootstrap
  - an already deployed engine process starts by itself and calls the normal
    `zk.bootstrap.register` flow
- Pilot-initiated launch
  - Pilot asks the runtime orchestrator to start the process/container/pod
  - the launched engine still calls the same normal `zk.bootstrap.register` flow itself

The runtime orchestrator may start or stop the runtime, but it does not replace service bootstrap.
The engine remains responsible for authenticating to Pilot, fetching config, and registering itself
in KV.

### `execution_id` remains Pilot-owned

For strategy executions, Pilot owns singleton enforcement and `execution_id` allocation.

Rule:

- `POST /v1/strategy-executions/start` creates the pending execution claim
- engine bootstrap must bind to that pre-created claim
- bootstrap returns the already assigned `execution_id` and effective runtime config
- the engine must not allocate or negotiate a fresh independent execution during bootstrap

## Bootstrap hardening scope (2026-03-21)

### Phase-1 bootstrap stays minimal

For the current design and implementation phase, bootstrap is intentionally limited to:

- token validation
- Pilot authorization of the logical instance
- return of `owner_session_id`, `kv_key`, `lease_ttl_ms`, and effective runtime config
- direct runtime registration plus CAS heartbeat on `kv_key`

The current contract does not require:

- scoped runtime credentials
- a separate `lock_key`
- `zk.bootstrap.reissue`
- `zk.bootstrap.sessions.query`

Those remain later hardening topics and should not shape phase-1 Pilot or service implementations.

## Manifest-driven config management and runtime introspection (2026-03-22)

For bootstrap-managed services, desired config should be managed through manifest/schema contracts
and live config should be inspectable from the runtime.

Rule:

- config should be understood in three layers:
  - `bootstrap_config`
  - `provided_config`
  - `runtime_config`
- venue-backed services such as GW and MDGW use the venue integration manifest/schema
- non-venue bootstrap-managed services such as OMS and engine use an equivalent service-kind
  manifest/schema contract
- Pilot validates desired config against that contract before persistence
- the manifest/schema contract also classifies reloadable vs restart-required fields
- every bootstrap-managed runtime exposes a default `GetCurrentConfig` style query returning the
  effective config currently loaded by the process
- `GetCurrentConfig` must redact raw secret material

Pilot uses this combination to:

- render config-authoring forms
- compare desired config vs live effective config
- surface drift
- decide whether reload or restart is required

Compatibility rule:

- direct startup mode remains valid
- in direct mode, the same logical `runtime_config` is assembled locally
- the difference is only that `provided_config` is sourced from env vars/files instead of Pilot
  bootstrap

### Bundled manifests remain authoritative

For manifest/schema-driven config management:

- bundled manifests and schemas in the codebase/binary are the authoritative contract
- Pilot may keep an operational mirror/registry in DB for UI, version listing, activation state,
  and validation support
- that DB copy must be derived from or synchronized against the bundled manifests
- mismatch between bundled authoritative manifest/schema and active Pilot registry state should be
  treated as an error, not normal drift

### Schema/manifest authority model (2026-03-22)

Bundled manifests (repo/binary-baked) are the authoritative source of truth for schema semantics.
Pilot DB `cfg.schema_resource` is an operational mirror/registry derived from bundled manifests
via startup sync, used for UI reads, version tracking, activation state, and config validation.

Rules:

- every manifest includes identity metadata: `schema_id`, `version`, `content_hash`
- `SchemaRegistrySyncer` runs on Pilot startup, syncs bundled manifests to DB
- if bundled manifest hash does not match DB record for same schema_id + version, Pilot fails
  closed for that resource (refuses config operations, logs SCHEMA_MISMATCH)
- `/v1/schema` is primarily a read API; admin endpoints for activate/deprecate among provisioned
  versions only
- mismatch between bundled and DB is always an error, never a silent fallback

### Desired config ownership: service-specific tables (2026-03-22)

`cfg.logical_instance` is the identity/topology row only. It should not become the primary
config-version authority.

Rules:

- desired runtime config, `config_version`, and `config_hash` live on service-specific tables:
  `cfg.oms_instance`, `cfg.gateway_instance`, `cfg.engine_instance`, `cfg.mdgw_instance`
- `cfg.logical_instance.runtime_config` is deprecated as config authority (column stays for compat)
- `DesiredConfigRepository` routes by instance_type to the correct service-specific table
- drift detection compares desired config from service-specific table vs live effective config
  from runtime introspection

### Config editing binds to latest active schema by default (2026-03-24)

Normal control-plane config editing should resolve schema versions at edit time from the active
schema registry state, not from stale schema provenance stored on an existing instance row.

Rules:

- OMS and ENGINE config editing uses the latest active service-kind schema
- GW and MDGW config editing uses:
  - the latest active service-kind schema
  - the latest active venue-capability schema for the selected venue
- REFDATA config editing uses the latest active venue-capability schema for the selected venue
- the normal UI flow should show resolved schema versions as read-only metadata
- schema version selection should not be part of the normal operator-facing edit flow

Persistence rule:

- when desired config is edited and saved, Pilot should update both:
  - `provided_config`
  - schema provenance fields (`schema_resource_key`, `schema_version`, `schema_content_hash`,
    and venue-schema provenance where applicable)

Rationale:

- operators edit config, not schema release topology
- using the latest active schema keeps editing aligned with current control-plane policy
- showing resolved versions preserves auditability without exposing a confusing version matrix in the
  UI
- manual schema pinning may exist later as an advanced admin-only workflow, but it is not the
  default design

## zk-trading-sdk-rs (Phase 7)

### service_type case: case-insensitive matching with lowercase contract

`discovery.rs` filters KV entries using `eq_ignore_ascii_case` against `"oms"` / `"refdata"`.
The architectural contract (`api_contracts.md`, `service_discovery.md`) specifies lowercase
service_type values. The SDK intentionally accepts case-insensitive values for compatibility
with `zk-oms-svc` which currently registers uppercase `"OMS"`. The service-side migration to
lowercase remains deferred.

### oms_id source: service_id with key-suffix fallback

`build_account_map` sets `oms_id = reg.service_id` (from the `ServiceRegistration` proto field).
If `service_id` is empty, falls back to the last `.`-delimited segment of the KV key (e.g.
`svc.oms.oms_dev_1` → `"oms_dev_1"`). This prevents malformed topics like
`zk.oms.svc.oms.oms_dev_1.order_update.9001` which would result from using the full KV key.

### LiveRefdataGrpc: blocking connect (fail-fast)

`LiveRefdataGrpc::connect()` calls `connect().await?` — the TCP connection is established eagerly.
`RefdataSdk::start()` returns `Err` if the refdata gRPC server is unreachable at startup. This
was chosen over the originally planned `connect_lazy()` to surface misconfigured endpoints
immediately rather than silently serving stale cache until the first caller triggers a miss.
Trade-off: startup fails if refdata is temporarily down; retry logic is expected at the deployment
layer (systemd restarts, K8s liveness probes).

### RefdataSdk active-reload: inline in subscription loop

On `zk.control.refdata.updated` with `change_class=invalidate`, the NATS subscription task:
1. Calls `inner.invalidate(id, false)` (marks cache entry invalid)
2. Calls `let _ = inner.query_instrument(id).await` inline (refetch into cache)

The reload is inline (not spawned), meaning one slow or hung gRPC reload blocks processing of
subsequent invalidation messages on the same subscription loop. The result is discarded with
`let _ = ...` — no error logging. This is a known trade-off: inline reload is simpler and
guarantees ordering, but lacks backpressure handling. A future improvement could spawn the
reload or add a timeout. Matches `sdk.md §2.6` ("invalidate, then actively reload").

### config instance_id bounds: enforced at parse time

`TradingClientConfig::from_env()` validates `client_instance_id <= 1023` immediately after
parsing (before `SnowflakeIdGen::new()`). This surfaces misconfigured deployments at startup
rather than at the first `next_order_id()` call.

### TradingClient::from_config: fail-fast instance_id check before I/O

Step 1 of `from_config()` checks `client_instance_id > 1023` and returns
`SdkError::InstanceIdOutOfRange` before attempting any NATS connection. This allows unit tests
to verify the error path without a running NATS server.

### config_tests: OnceLock<Mutex<()>> for env var isolation

Config tests mutate process-global env vars (`ZK_*`). A static `OnceLock<Mutex<()>>` guard
serialises all tests in the binary, preventing parallel execution races.
Alternative (not chosen): `--test-threads=1` in CI — fragile, doesn't prevent cross-test-binary
races, and makes the isolation implicit rather than explicit.

### OmsDiscovery startup: fail if snapshot not ready and accounts configured

`OmsDiscovery::start()` returns `(Self, bool)` where the bool indicates whether the KV watcher
delivered at least one entry within a 2s window. `from_config()` treats `false` as a hard error
when `config.account_ids` is non-empty — this prevents startup on a system where the registry
hasn't delivered yet, which would cause `OmsNotFound` errors on the first order. If no accounts
are configured (SDK used only for refdata/RTMD), empty-snapshot startup is allowed.

### OmsDiscovery: pre-resolved account map cache (on-demand refresh)

`OmsDiscovery` wraps `KvDiscoveryClient` with a derived `HashMap<i64, OmsEndpoint>`. The cache
is NOT automatically rebuilt on watcher updates — it is refreshed on-demand by calling
`refresh_account_cache()`. This is called at: startup, `resolve_account_endpoint()` (every
order/query), and before building subscription subjects. Callers get O(1) lookup after refresh.
The cache is rebuilt from the full KV snapshot (not incrementally patched) to keep consistency
simple. Trade-off: no automatic reconnection on OMS endpoint churn; long-lived subscription
handles established before an OMS migration continue pointing at the old endpoint.

### Balance/Position separation

Balance (cash/spot inventory) and position (derivatives exposure) are fully separated across
the stack. Key decisions:

- **`Balance` proto**: No `long_short_type` field. Pure cash inventory keyed by asset (USDT, BTC).
  Fields: `account_id`, `asset`, `total_qty`, `frozen_qty`, `avail_qty`, timestamps, `is_from_exch`.
- **`BalanceUpdateEvent` proto**: Published on `zk.oms.<oms_id>.balance_update` topic.
- **OMS internal**: Keeps `OmsPosition` struct internally. Converts to `Balance` at publish/query
  boundary via `build_balance_snapshot()`.
- **`QueryAccountBalance` removed**: Replaced by `QueryBalances` RPC (request/response use
  `Balance` type). Hard cut, no compatibility shim.
- **Engine**: `EngineEvent::BalanceUpdate` and `EngineEvent::PositionUpdate` are independent
  variants routed through separate runner methods. They never share a code path.
- **Strategy SDK context**: `AccountState` has separate `balances: HashMap<String, Balance>` and
  `positions: HashMap<String, Position>`. `on_balance_update` only touches `acc.balances`;
  `on_position_update` only touches `acc.positions`. `balance_generation` and `position_generation`
  are independent counters.
- **Strategy trait**: `on_balance_update` and `on_position_update` are separate callbacks.
- **Trading SDK**: `subscribe_balance_updates()` wired to decode `BalanceUpdateEvent` from OMS
  NATS topic (no longer a stub).
- **`QueryBalances` gRPC**: Canonical query for asset inventory. Returns `Balance` messages
  converted from internal `OmsPosition` at the boundary.
- **`QueryPosition` gRPC**: Compatibility pass-through. Returns the internal OMS `Position`
  snapshots from `snap.balances` (default) or `snap.exch_balances` (`query_gw=true`). This is
  not a distinct position data source — it reads from the same balance ledger. New callers
  should prefer `QueryBalances`.
- **`ZkBalance` (PyO3)**: No `long_short_type` field. Existing Python strategies that read
  `balance.long_short_type` will break — fix is in PR 5 (Python migration).

### RTMD interest lease: not yet wired (gap)

`TradingClient::subscribe_ticks/klines/funding/orderbook` subscribe to the correct NATS subjects
but do NOT publish leases to `zk-rtmd-subs-v1`. `RtmdInterestManager` is implemented in
`rtmd_sub.rs` and correct. The wiring from `TradingClient` to `RtmdInterestManager` is deferred;
without it, the RTMD gateway will not maintain the upstream venue subscription unless another
client has already registered a lease. This is an architectural deviation to be addressed before
production use of RTMD subscriptions.

### RTMD interest key format

`RtmdInterestManager::key_for` produces:
`sub.<scope>.<subscriber_id>.<channel_type>.<suffix>`
where `suffix = instrument_id` (no param) or `instrument_id_param` (with channel param, e.g. kline interval).
This maps `<channel_type>.<suffix>` to the `<subscription_id>` slot in the protocol's suggested
shape `sub.<scope>.<subscriber_id>.<subscription_id>`. Different channel_types and different
channel_params produce distinct keys, which is required for the RTMD gateway's aggregation logic.

## OMS + Gateway: order_id sharding & queue-then-reply (2026-03-17)

### OMS gateway executor: shard by order_id (not gw_id)

`GwExecutorPool` was rewritten from `ShardedPool<u32, GwAction>` keyed by `gw_id` (one lazy-spawned
worker per gateway) to fixed N pre-spawned shards keyed by `order_id % shard_count`. This provides:
- **More concurrency**: N shards (default 4) vs 1 per gateway. A slow gateway no longer blocks all
  orders to that gateway.
- **Per-order ordering**: place/cancel for the same `order_id` always land on the same shard worker,
  preserving FIFO ordering within an order.
- **Non-blocking dispatch**: `dispatch(&self, order_id, action)` uses `try_send` — the writer loop
  never awaits gateway I/O. Queue full = sync `Err` returned to writer. Async failures surface via
  `OmsCommand::GatewaySendFailed` / `GatewayCancelSendFailed` feedback.
- **Shared client map**: `Arc<RwLock<HashMap<u32, (String, GatewayClient)>>>` — read-locked on hot
  path (cheap), write-locked only during startup/discovery.

Config: `ZK_GW_EXEC_SHARD_COUNT` (default 16), `ZK_GW_EXEC_QUEUE_CAPACITY` (default 256).

Batch routing: batches are partitioned by `order_id % shard_count`, one sub-batch per shard group.
This preserves per-order shard affinity while keeping the batch gRPC optimization (fewer OMS → GW
round trips). Individual sub-batch dispatch failures are handled inline.

### OMS ACK semantics (relaxed)

OMS gRPC reply is **always success** after core validation. The reply does NOT wait for gateway RTT
and does not guarantee the downstream action was already enqueued.

Queue-full dispatch drops are handled **inline** by calling `core.process_message(GatewaySendFailed)`
directly in the writer, which marks the order as synthetically rejected and emits persist + publish
actions. This uses the same failure path as async gateway RPC failures. No separate async feedback
loop is needed for dispatch drops.

### Gateway queue-then-reply semantics

`GrpcHandler` uses `dispatch_or_reject()` for all execution commands. The gRPC reply is **always
success** after request validation — "validated and accepted for asynchronous processing". It does
NOT mean venue acknowledged, enqueued to a worker, or even accepted into the internal queue.

Queue-full drops: `dispatch_or_reject()` spawns a task to publish a synthetic rejection `OrderReport`
directly to NATS, so the OMS receives it through the normal async report path.

Batch RPCs (`batch_place_orders`, `batch_cancel_orders`): loop and `dispatch_or_reject()` each item
individually. No atomic batch guarantee — individual drops get individual async failure reports.
This is intentional: the relaxed ACK contract means the caller doesn't need atomic batch semantics.

`GwExecPool`: pre-spawned N shard workers (default 4), each receiving `Arc<dyn VenueAdapter>` +
`Arc<NatsPublisher>`. Workers call `adapter.place_order()` / `adapter.cancel_order()` asynchronously.
On worker-level failure: builds synthetic rejection `OrderReport` with `ExecReport { exec_type:
Rejected }` and publishes directly to NATS (bypasses `SemanticPipeline` — correct because rejections
have no trades to dedup).

Config: `ZK_EXEC_SHARD_COUNT` (default 4), `ZK_EXEC_QUEUE_CAPACITY` (default 256).

Query RPCs (`query_account_balance`, etc.) still call adapter synchronously — they aren't execution
commands and don't need queue-then-reply.

### Latency extensions

Added `gw_exec_dequeue_ns` to `TimestampRecord` and `LatencyEvent::OrderSent`. New tag
`"t_gw_dequeue"` in published `LatencyMetric`. New derivable metric segment:
`gw_exec_queue_wait = t_gw_dequeue - writer_dispatch_ts`.

### Caveats

- `exch_order_ref` in GW gRPC response is empty in queue-then-reply model. OMS doesn't use it
  (comes via NATS reports). Safe.
- Synthetic rejections bypass `SemanticPipeline`. Correct because rejections have no trades.
- Dynamic GW registration acquires write lock — startup-only, not hot path.

## Language-rooted layout + unified proto codegen (zb-00028, 2026-04-18)

The zkbot repository is reorganized into top-level language roots (`python/`, `java/`, `rust/`) plus
cross-cutting top-level concerns (`protos/`, `venue-integrations/`, `devops/`, `scripts/`).
Protobuf codegen is unified behind a single entrypoint.

### Layout

```
zkbot/
  protos/                              schema source-of-truth (versioned tree zk/**/v1/ only)
  python/
    proto-pb/                          installs as `zk-proto-pb`; exposes top-level `zk` namespace
    proto-betterproto/                 installs as `zk-proto-betterproto`
    libs/{zk-client,zk-core,zk-data,zk-gw-utils,zk-rpc}/
    services/zk-refdata-svc/
    tools/                             installs as `zk-tools` (trade-doctor, loc-count, test_rtmd)
    legacy/                            reference-only; excluded from uv workspace
  java/                                multi-module Gradle build (`settings.gradle.kts`)
    zk-proto-java/                     shared generated Java protos — generated at build time, NOT committed
    pilot-service/                     Spring Boot Pilot service
  rust/                                Cargo workspace (unchanged)
  venue-integrations/                  cross-cutting: {oanda,ibkr,okx,simulator}
    legacy/                            reference-only snapshots (pre-refactor vendored pb2 trees)
  scripts/gen_proto.py                 single codegen driver
```

### Codegen contract

- Python pb2/grpc: `grpc_tools.protoc` generates from the versioned tree
  (`protos/zk/**/v1/*.proto`) into `python/proto-pb/src/zk/`. **Committed.**
  Consumers import `from zk.<pkg>.v1 import <pkg>_pb2` from the installed `zk-proto-pb` package.
- Python BetterProto: `grpc_tools.protoc` + BetterProto plugin generates from flat root
  `.proto` files into `python/proto-betterproto/src/zk_proto_betterproto/`.
  `scripts/gen_proto_legacy_compat.py` runs as a post-pass to add legacy enum aliases.
  **Committed.** Consumers import `from zk_proto_betterproto.<pkg> import <Message>`.
- Rust: `build.rs` + prost via `buf generate`, outputs `rust/crates/zk-proto-rs/src/`. **Committed.**
- Java: **NOT** committed. Gradle `com.google.protobuf` plugin in `java/zk-proto-java/`
  regenerates at every build from `../../protos/`. `java/pilot-service/` declares
  `implementation(project(":zk-proto-java"))` to consume the generated classes.

`make gen` → `uv run --python 3.13 python scripts/gen_proto.py` drives Python + Rust.
Gradle owns Java generation. C++ codegen is not required and has been retired.

### Forbidden patterns

- No `sys.path` injection, no `PYTHONPATH=` hacks for local code — dependencies are declared in
  `pyproject.toml`/`Cargo.toml`/Gradle, and `uv sync` / `cargo build` / Gradle resolves them.
- No vendored generated code under service or venue packages (the pre-refactor
  `venue-integrations/{oanda,ibkr}/*/proto/` trees are archived under
  `venue-integrations/legacy/vendored-proto/` and not imported).
- No on-demand codegen at import time (the old `trade_doctor.py` stamp cache at
  `tools/.trade_doctor_generated/` is deleted).
- Legacy trees (`python/legacy/`, `venue-integrations/legacy/`) are **not** uv workspace members
  and must not be referenced from active code.

See `docs/system-arch/dependency-matrix.md` for the full per-consumer mechanism table.
