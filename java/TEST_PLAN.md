# Java Pilot Service — Test Plan

## 1. Test Strategy by Layer

### Layer 1: Unit Tests (plain JUnit + Mockito)

**What belongs here:** Pure business logic that doesn't touch Spring context, JDBC, NATS, or gRPC. Constructor-injected services with mocked collaborators.

**What does NOT belong here:** SQL correctness, HTTP routing, auth rules, serialization, real KV watch behavior.

**Setup:** Plain JUnit 5. Mockito for collaborators. No `@SpringBootTest`, no `@WebMvcTest`, no Testcontainers. Tests instantiate the class under test directly.

**Weight:** ~45% of test count. Fast — entire layer runs in <5 seconds.

**Targets:**
- `TopologyService` — workspace scoping logic, node/edge mapping, view name validation
- `BootstrapService` — register/deregister decision tree (token valid? duplicate? stale? ENGINE?)
- `BotService` — execution lifecycle state machine, enriched config assembly, singleton enforcement
- `ManualService` — order preview validation, OMS routing, panic flow
- `AccountService` — binding enrichment, balance/position key construction
- `TokenService` — SHA-256 hashing, token generation format
- `ProcessOrchestrator` — start/stop/status lifecycle
- `DiscoveryResolver` — key pattern matching, type filtering
- `KvReconciler` — live set management, fence trigger logic

### Layer 2: Repository / SQL Tests (Testcontainers PostgreSQL)

**What belongs here:** Every SQL query in every `*Repository` class. These are the highest-value tests in the suite — the codebase has no ORM, so SQL is hand-written and the compiler can't catch column/type mismatches.

**What does NOT belong here:** Business logic above the repository. Don't test service orchestration here.

**Setup:**
- `@JdbcTest` with `@Testcontainers` — injects `JdbcTemplate` and `NamedParameterJdbcTemplate`
- Flyway runs schema migration on container startup (`00_schema.sql` + `03_bootstrap.sql` as `V1__baseline.sql`)
- Each test method inserts its own fixtures via a shared helper, then asserts query results
- Use `@Transactional` + rollback (default in `@JdbcTest`) for test isolation — no cleanup needed

**Weight:** ~25% of test count. Runs in ~10-15 seconds (container startup is one-time via `@Container` static field).

**Targets:**
- `TopologyRepository` — all 9 methods
- `BootstrapRepository` — token validation, session CRUD, instance ID leasing, audit recording
- `AccountRepository` — account CRUD, binding joins, trade/activity queries
- `StrategyRepository` — strategy CRUD with array columns, execution join, filtered listing
- `ExecutionRepository` — execution CRUD, status transitions, singleton query

### Layer 3: Controller / Security Tests (@WebMvcTest + MockMvc)

**What belongs here:** HTTP routing correctness, request/response serialization, auth enforcement (role-based access), input validation, error response shapes.

**What does NOT belong here:** Business logic (mock the service layer). SQL (repositories are mocked).

**Setup:**
- `@WebMvcTest(SomeController.class)` with `@MockBean` for the service
- `@Import(SecurityConfig.class, DevSecurityConfig.class)` to load auth rules
- Use `MockMvc` with `@WithMockUser(roles = "ADMIN")` / `"OPS"` / `"TRADER"` / no user
- Use `SecurityMockMvcRequestPostProcessors.httpBasic("admin", "admin")` for auth header tests
- Assert HTTP status codes, JSON paths, content types

**Weight:** ~20% of test count. Runs in ~5-8 seconds (no DB, no external services).

**Targets:**
- Every controller class (6 controllers + 1 legacy compat)
- Focus on: 401/403 for wrong roles, 404/409 response shapes, JSON field names matching DTOs

### Layer 4: Integration Tests (full SpringBoot + Testcontainers)

**What belongs here:** Cross-cutting flows that span multiple layers and catch wiring bugs that unit tests miss. A small number of high-confidence smoke tests, not exhaustive coverage.

**What does NOT belong here:** Exhaustive edge cases — those belong in layers 1-3. Don't duplicate repository SQL tests.

**Setup:**
- `@SpringBootTest(webEnvironment = RANDOM_PORT)` with `@Testcontainers`
- PostgreSQL container (shared across integration tests via abstract base class)
- NATS: use real `nats-server` container (Testcontainers generic) — KV watch semantics are too complex to fake
- Redis: `GenericContainer` with redis image for balance/position reads
- gRPC OMS: in-process `Server` with a fake `OMSServiceImplBase` — no container needed, just `grpc-testing`
- HTTP client: `TestRestTemplate` with basic auth

**Weight:** ~10% of test count. Runs in ~30-60 seconds.

**Targets:**
- Bootstrap register → session recorded → KV reconciler fences on key delete
- Topology GET scoped by OMS → returns correct nodes/edges from live DB + discovery
- Manual order submit → gRPC call hits fake OMS → response mapped correctly
- Bot execution start → singleton enforced → stop → re-startable
- Security: unauthenticated request → 401, TRADER accessing /ops → 403


## 2. Test Scope by Domain

### Bootstrap / Registration / KV Reconciliation

**Key behaviors:**
- Token validation: valid → accept, expired → reject, wrong logical_id → reject, revoked → reject
- Duplicate detection: active session + KV live → reject; active session + KV dead → fence then accept
- ENGINE instance ID allocation: acquires lowest available 0-1023, releases on deregister
- Session recording in `mon.active_session` with correct fields
- Audit trail: every decision (ACCEPTED, REJECTED, DUPLICATE, FENCED) recorded in `mon.registration_audit`
- KV reconciler: snapshot load, live set tracking, fence on DELETE/PURGE

**Highest-risk failure modes:**
- Instance ID leak (ENGINE deregisters but ID not released) — tests must verify release
- Stale session not fenced (KV key gone but session still "active") — test fence path
- Serialization failure in `acquireInstanceId()` not retried — test retry behavior
- NATS reply to null `replyTo` — test null-safe reply

**Best layer:**
| Behavior | Layer |
|----------|-------|
| Token hash matching | Unit (TokenService) |
| Register decision tree | Unit (BootstrapService) |
| Duplicate + fence logic | Unit (BootstrapService) |
| `validateToken()` SQL | Repository IT |
| `acquireInstanceId()` SQL + serialization | Repository IT |
| `recordSession()` / `fenceSession()` SQL | Repository IT |
| Register → session → fence end-to-end | Integration |

### Discovery Cache / Topology

**Key behaviors:**
- OMS-scoped workspace: OMS + bound GWs/ENGINEs + all REFDATA + MDGW only if bound to scoped service
- Unscoped topology returns all instances
- `getServiceDetail()` validates serviceKind against stored `instance_type` (case-insensitive)
- `reloadService()` validates kind match, checks liveness, uses stored type for NATS subject
- `listServices()` normalizes kind to uppercase
- `getView()` rejects unknown view names with 404
- Edge filtering: only edges where both endpoints are in scoped set
- Node status: "online"/"offline" from DiscoveryCache, `desiredEnabled` from DB `enabled` column
- Session listing scoped by OMS workspace

**Highest-risk failure modes:**
- MDGW included when it shouldn't be (bound to out-of-scope service)
- REFDATA not included (always should be, regardless of bindings)
- Reload publishes to wrong NATS subject (path serviceKind vs stored type)
- Case mismatch: API caller sends "gw", DB stores "GW" → empty result

**Best layer:**
| Behavior | Layer |
|----------|-------|
| `buildOmsWorkspaceScope()` logic | Unit |
| `toServiceNode()` / `toEdge()` mapping | Unit |
| View name validation | Unit |
| Reload kind validation | Unit |
| Service kind normalization | Unit |
| `listLogicalInstances()` SQL | Repository IT |
| `listBindings()` / `upsertBinding()` SQL | Repository IT |
| GET topology → scoped response | Controller (MockMvc) |

### Manual Trading

**Key behaviors:**
- Order preview: validates instrument exists, account bound, qty/price within refdata limits
- Order submit: resolves OMS via account binding → discovery, builds proto, calls gRPC
- Batch order: multiple orders in one call, individual failures don't block others
- Cancel: idempotency key per cancel, maps to gRPC CancelOrderRequest
- Panic / clear panic: routes to correct OMS
- Audit metadata: `source_id = "pilot-manual"` set on all orders

**Highest-risk failure modes:**
- OMS not found for account (binding missing or OMS offline) → should 404/503, not NPE
- gRPC channel stale after OMS restart → channel cache should invalidate on address change
- Batch order partial failure silently swallowed

**Best layer:**
| Behavior | Layer |
|----------|-------|
| Preview validation logic | Unit |
| OMS routing (account → binding → OMS) | Unit |
| Proto message construction | Unit |
| gRPC call + response mapping | Integration (in-process gRPC) |
| Auth: TRADER can submit, OPS can submit, no-auth → 401 | Controller |

### Accounts

**Key behaviors:**
- CRUD: create account, update status, list with filters (venue, oms_id, status)
- Runtime binding: joins `account_binding` + `oms_instance` + `gateway_instance` + live discovery
- Balances: reads Redis keys `oms:{omsId}:balance:{accountId}:*`
- Positions: reads Redis keys `oms:{omsId}:position:{accountId}:*:*`
- Activities: UNION of trades + balance snapshots, paginated, ordered by time

**Highest-risk failure modes:**
- Redis key pattern mismatch (wrong separator or prefix) → empty balances
- Activities UNION type mismatch (columns must align across subqueries)
- `getAccountBinding()` join uses wrong FK columns

**Best layer:**
| Behavior | Layer |
|----------|-------|
| Binding enrichment with discovery status | Unit |
| Redis key construction | Unit |
| `listAccounts()` dynamic filter SQL | Repository IT |
| `getAccountBinding()` join SQL | Repository IT |
| `listActivitiesForAccount()` UNION SQL | Repository IT |
| Balance/position Redis reads | Integration |

### Bot / Strategy Execution

**Key behaviors:**
- Strategy CRUD with array columns (`bigint[]`, `text[]`) and `config_json` (jsonb)
- Execution singleton: 409 if INITIALIZING/RUNNING/PAUSED execution exists for same strategy
- Execution lifecycle: INITIALIZING → RUNNING → PAUSED → RUNNING → STOPPED
- Enriched config: resolves accounts, instruments, discovery bucket
- Orchestrator integration: `orchestrated=true` → ProcessOrchestrator.start()
- Restart: stop current + start new execution

**Highest-risk failure modes:**
- Array column insert fails (JDBC Array type mismatch with PG `bigint[]`)
- Singleton check race condition (two concurrent starts)
- Status transition to invalid state (e.g., STOPPED → RUNNING)
- Orchestrator process not stopped on execution stop

**Best layer:**
| Behavior | Layer |
|----------|-------|
| Singleton enforcement logic | Unit |
| Status transition validation | Unit |
| Enriched config assembly | Unit |
| Orchestrator delegation | Unit |
| `createStrategy()` with arrays SQL | Repository IT |
| `findRunningExecutionForStrategy()` SQL | Repository IT |
| Start → stop → restart lifecycle | Integration |

### Ops / Reconcile

**Key behaviors:**
- Registration audit listing (paginated by `observed_at desc`)
- Reconciliation audit listing (from `mon.recon_run`)
- Trigger reconcile (stub → returns ACCEPTED + job ID)

**Highest-risk failure modes:** Low risk — mostly read-only queries. The `triggerReconcile()` is a stub.

**Best layer:**
| Behavior | Layer |
|----------|-------|
| Audit query SQL | Repository IT |
| Auth: only ADMIN/OPS | Controller |
| Stub response shape | Unit |

### Orchestrator

**Key behaviors:**
- Start: `ProcessBuilder` creates child process, tracked in map
- Stop: SIGTERM → wait 10s → SIGKILL
- Status: process alive check, exit code
- Cleanup: `destroy()` terminates all on shutdown

**Highest-risk failure modes:**
- Process handle leak (start succeeds, map entry lost)
- Stop blocks forever (process ignores SIGTERM and SIGKILL timeout logic broken)
- Duplicate start for same logicalId (should reject or kill existing first)

**Best layer:** All unit tests. Use `sleep 60` or `cat` as the child process for real subprocess testing. No Spring context needed.

### Security / Auth

**Key behaviors:**
- Unauthenticated → 401 on all endpoints except `/health`, `/actuator/health`
- Role enforcement:
  - TRADER: manual trading, account GET, bot execution lifecycle, legacy compat
  - OPS: everything TRADER can do + topology, account write, bot management, ops
  - ADMIN: everything
- CSRF disabled (API-only service)

**Highest-risk failure modes:**
- New endpoint added without auth rule → defaults to ADMIN (safe) but may be wrong
- Health endpoint requires auth → breaks readiness probes
- Role upgrade: TRADER accesses ADMIN-only endpoint

**Best layer:** All controller tests (MockMvc). Each controller test class should include a `*_requires_auth` and `*_forbidden_for_wrong_role` test.


## 3. Concrete Test Cases

### Bootstrap

```
BootstrapServiceTest (unit):
  register_accepts_valid_token_and_records_session
    Setup: mock repo.validateToken() → valid tokenJti, no existing session
    Result: reply with status=OK, session recorded, audit=ACCEPTED

  register_rejects_expired_token
    Setup: mock repo.validateToken() → null (token expired or invalid)
    Result: reply with status=TOKEN_EXPIRED, audit=REJECTED

  register_rejects_duplicate_active_session_when_kv_is_live
    Setup: mock repo.findSessionForLogical() → active session,
           mock reconciler.isKvLive(kvKey) → true
    Result: reply with status=DUPLICATE, no fence, audit=DUPLICATE

  register_fences_stale_session_and_accepts_when_kv_is_dead
    Setup: mock repo.findSessionForLogical() → active session,
           mock reconciler.isKvLive(kvKey) → false
    Result: repo.fenceSession() called, new session recorded, audit=FENCED+ACCEPTED

  register_acquires_instance_id_for_engine_type
    Setup: valid token, instanceType=ENGINE, mock repo.acquireInstanceId() → 42
    Result: reply includes instanceId=42

  register_skips_instance_id_for_non_engine_type
    Setup: valid token, instanceType=OMS
    Result: reply instanceId absent, acquireInstanceId() never called

  deregister_marks_session_and_releases_engine_instance_id
    Setup: mock repo.getSession() → active ENGINE session with instanceId=42
    Result: deregisterSession() + releaseInstanceId() called

  deregister_skips_instance_id_release_for_non_engine
    Setup: mock repo.getSession() → active GW session
    Result: deregisterSession() called, releaseInstanceId() NOT called

  reply_handles_null_reply_to_without_throwing
    Setup: NATS message with null replyTo
    Result: no exception, log warning
```

```
BootstrapRepositoryIT (Testcontainers PG):
  validate_token_returns_jti_for_valid_active_token
    Setup: insert logical_instance + instance_token (SHA-256 of "test-token")
    Result: validateToken("test-token", logicalId, type, env) → tokenJti

  validate_token_returns_null_for_expired_token
    Setup: insert token with expires_at in the past
    Result: validateToken() → null

  validate_token_returns_null_for_wrong_logical_id
    Setup: insert token for logical_id "oms_1"
    Result: validateToken(token, "oms_2", ...) → null

  record_session_inserts_active_session
    Setup: insert logical_instance
    Result: recordSession() → row in mon.active_session with status=active

  record_session_is_idempotent_on_conflict
    Setup: recordSession() twice with same owner_session_id
    Result: no error, one row

  fence_session_sets_status_fenced
    Setup: recordSession(), then fenceSession()
    Result: status='fenced' in DB

  acquire_instance_id_returns_lowest_available
    Setup: insert logical_instance, lease IDs 0 and 1
    Result: acquireInstanceId() → 2

  acquire_instance_id_returns_0_when_pool_empty
    Setup: no leases
    Result: acquireInstanceId() → 0

  release_instance_id_deletes_lease
    Setup: acquireInstanceId() → leaseId, then releaseInstanceId()
    Result: lease row deleted

  record_audit_inserts_registration_audit
    Setup: call recordAudit(logicalId, type, sessionId, "ACCEPTED", "valid token")
    Result: row in mon.registration_audit with correct fields
```

### Topology

```
TopologyServiceTest (unit):
  topology_scoped_by_oms_includes_only_workspace_services
    Setup: instances=[oms_1(OMS), gw_1(GW), gw_2(GW), engine_1(ENGINE), refdata_1(REFDATA)]
           bindings=[(oms_1→gw_1), (oms_1→engine_1)]
    Result: scopedIds = {oms_1, gw_1, engine_1, refdata_1}. gw_2 excluded.

  topology_scope_includes_refdata_regardless_of_bindings
    Setup: instances=[oms_1(OMS), refdata_1(REFDATA)], bindings=[]
    Result: scopedIds includes refdata_1

  topology_scope_includes_mdgw_only_when_bound_to_scoped_service
    Setup: instances=[oms_1(OMS), gw_1(GW), mdgw_1(MDGW)]
           bindings=[(oms_1→gw_1), (mdgw_1→gw_1)]
    Result: mdgw_1 included (bound to gw_1 which is in scope)

  topology_scope_excludes_mdgw_bound_to_out_of_scope_service
    Setup: instances=[oms_1(OMS), gw_2(GW), mdgw_1(MDGW)]
           bindings=[(mdgw_1→gw_2)]  (gw_2 not bound to oms_1)
    Result: mdgw_1 excluded

  topology_unscoped_returns_all_instances
    Setup: 5 instances, omsId=null
    Result: all 5 returned as nodes

  edges_filtered_to_scoped_endpoints
    Setup: bindings between in-scope and out-of-scope services
    Result: only edges with both src and dst in scope

  get_service_detail_returns_null_when_kind_mismatches_stored_type
    Setup: instance with type=GW, request serviceKind=OMS
    Result: null (controller maps to 404)

  get_service_detail_maps_desired_enabled_from_db
    Setup: instance with enabled=false
    Result: ServiceDetail.desiredEnabled == false

  reload_validates_kind_against_stored_type
    Setup: instance type=GW, path serviceKind=OMS
    Result: ResponseStatusException 404

  reload_returns_409_when_service_exists_but_is_offline
    Setup: instance exists, discoveryCache returns no registration
    Result: ResponseStatusException 409 CONFLICT

  reload_uses_stored_type_for_nats_subject
    Setup: instance type=GW, path serviceKind=gw (lowercase)
    Result: NATS subject = "zk.control.GW.{logicalId}.reload"

  list_services_normalizes_kind_to_uppercase
    Setup: call listServices(null, "gw")
    Result: repository called with serviceKind="GW"

  get_view_rejects_unknown_view_name
    Setup: viewName = "nonexistent"
    Result: ResponseStatusException 404 with available views listed

  get_view_accepts_known_view_names
    Setup: viewName ∈ {"full", "services", "connections"}
    Result: no error, returns topology

  to_service_node_maps_online_status_from_discovery
    Setup: instance in DB, matching registration in liveState
    Result: ServiceNode.status = "online", endpoint populated

  to_service_node_maps_offline_when_not_in_discovery
    Setup: instance in DB, no matching registration
    Result: ServiceNode.status = "offline", endpoint = null

  list_sessions_filters_by_oms_workspace_scope
    Setup: sessions for oms_1 and oms_2, request omsId=oms_1
    Result: only sessions for services in oms_1's workspace
```

```
TopologyRepositoryIT (Testcontainers PG):
  list_logical_instances_filters_by_env_and_type
    Setup: insert instances with env=dev/prod and type=OMS/GW
    Result: filter by env=dev, type=GW → only dev GW instances

  list_bindings_filters_by_src_and_dst_type
    Setup: insert bindings with various types
    Result: filter by srcType/dstType returns correct subset

  upsert_binding_replaces_existing_pair
    Setup: insert binding (A→B, enabled=true), upsert (A→B, enabled=false)
    Result: one row with enabled=false (delete+insert pattern)

  get_logical_instance_returns_null_for_missing_id
    Result: getLogicalInstance("nonexistent") → null

  list_active_sessions_returns_only_active_status
    Setup: insert sessions with status active/fenced/deregistered
    Result: only active sessions returned

  list_registration_audit_ordered_by_time_desc
    Setup: insert 3 audit entries with different observed_at
    Result: returned in descending time order
```

### Controller / Security

```
TopologyControllerTest (@WebMvcTest):
  get_topology_returns_200_for_ops
    Setup: @WithMockUser(roles="OPS"), mock service returns view
    Result: 200, JSON contains scope/nodes/edges/groups

  get_topology_returns_401_without_auth
    Result: 401

  get_topology_returns_403_for_trader
    Setup: @WithMockUser(roles="TRADER")
    Result: 403

  get_service_detail_returns_404_when_null
    Setup: mock service returns null
    Result: 404 with error body

  put_binding_returns_200_for_admin
    Setup: @WithMockUser(roles="ADMIN"), valid BindingRequest JSON
    Result: 200, {"status": "ok"}

ManualControllerTest (@WebMvcTest):
  submit_order_returns_200_for_trader
    Setup: @WithMockUser(roles="TRADER"), valid OrderRequest
    Result: 200

  submit_order_returns_403_without_role
    Setup: @WithMockUser(roles={})  (authenticated but no roles)
    Result: 403

  panic_returns_200_for_ops
    Setup: @WithMockUser(roles="OPS")
    Result: 200

AccountControllerTest (@WebMvcTest):
  list_accounts_returns_200_for_trader
    Setup: @WithMockUser(roles="TRADER")
    Result: 200

  create_account_returns_403_for_trader
    Setup: @WithMockUser(roles="TRADER"), POST
    Result: 403

  create_account_returns_200_for_ops
    Setup: @WithMockUser(roles="OPS")
    Result: 200

BotControllerTest (@WebMvcTest):
  start_execution_allowed_for_trader
    Setup: @WithMockUser(roles="TRADER"), POST /v1/bot/executions/start
    Result: 200

  create_strategy_forbidden_for_trader
    Setup: @WithMockUser(roles="TRADER"), POST /v1/bot/strategies
    Result: 403

OpsControllerTest (@WebMvcTest):
  list_registration_audit_forbidden_for_trader
    Setup: @WithMockUser(roles="TRADER")
    Result: 403

  list_registration_audit_allowed_for_ops
    Setup: @WithMockUser(roles="OPS")
    Result: 200

HealthControllerTest (@WebMvcTest):
  health_returns_200_without_auth
    Setup: no authentication
    Result: 200
```

### Bot / Strategy

```
BotServiceTest (unit):
  start_execution_returns_409_when_strategy_already_has_running_instance
    Setup: mock executionRepo.findRunningExecutionForStrategy() → existing execution
    Result: ResponseStatusException 409

  start_execution_creates_execution_with_initializing_status
    Setup: strategy exists, no running execution
    Result: executionRepo.createExecution() called with status=INITIALIZING

  start_execution_calls_orchestrator_when_orchestrated_flag_true
    Setup: orchestrated=true in request
    Result: orchestrator.start() called

  start_execution_skips_orchestrator_when_orchestrated_flag_false
    Setup: orchestrated=false
    Result: orchestrator.start() NOT called

  stop_execution_updates_status_and_stops_orchestrator
    Setup: execution in RUNNING state
    Result: status → STOPPED, orchestrator.stop() called

  pause_sets_status_paused
  resume_sets_status_running_from_paused

  restart_stops_then_starts_new_execution
    Setup: existing RUNNING execution
    Result: old stopped, new created with INITIALIZING

  get_execution_enriches_with_orchestrator_status
    Setup: executionRepo returns execution, orchestrator.status() → running=true
    Result: response includes process_running=true

  validate_strategy_checks_account_binding_exists
    Setup: strategy references account 9001, no binding for that account
    Result: validation failure returned
```

```
StrategyRepositoryIT (Testcontainers PG):
  create_strategy_persists_array_columns
    Setup: create with defaultAccounts=[9001], defaultSymbols=["BTCUSDT"]
    Result: read back → arrays match

  create_strategy_persists_config_json
    Setup: create with config = {"param": "value"}
    Result: read back config_json → matches

  update_strategy_updates_only_provided_fields
    Setup: create strategy, update only description
    Result: other fields unchanged

  find_strategy_with_current_execution_joins_running_instance
    Setup: create strategy + execution with status=RUNNING
    Result: returned map includes execution fields

  list_strategies_filters_by_venue_via_account_binding_join
    Setup: strategy bound to account on SIM venue
    Result: filter venue=SIM → included, venue=PROD → excluded

ExecutionRepositoryIT (Testcontainers PG):
  create_execution_inserts_with_initializing_status
  find_running_execution_returns_null_when_none_running
  find_running_execution_returns_execution_when_initializing_or_running_or_paused
  update_execution_status_sets_ended_at_for_terminal_states
  list_executions_filters_by_strategy_and_status
```

### Account

```
AccountRepositoryIT (Testcontainers PG):
  create_account_inserts_with_active_status
  list_accounts_filters_by_venue
  list_accounts_filters_by_oms_id_via_binding_join
  get_account_binding_joins_oms_and_gw_tables
    Setup: insert account + oms_instance + gateway_instance + binding
    Result: returned map has oms fields (oms_id, namespace) and gw fields (gw_id, venue)

  list_activities_union_returns_trades_and_balance_snapshots
    Setup: insert trade + balance snapshot for same account
    Result: both appear, ordered by time desc

  update_account_status_changes_only_status_field
```

### Orchestrator

```
ProcessOrchestratorTest (unit, real subprocess):
  start_creates_child_process_and_tracks_it
    Setup: start("test-1", profile with command="sleep", args=["60"])
    Result: status("test-1").running == true

  stop_terminates_running_process
    Setup: start, then stop
    Result: status("test-1").running == false, exit code present

  stop_returns_failure_for_unknown_logical_id
    Result: StopResult.success == false

  status_returns_not_running_for_unknown_logical_id
    Result: RuntimeStatus.running == false

  start_rejects_duplicate_logical_id
    Setup: start("test-1", ...) twice
    Result: second start returns success=false or replaces (verify actual behavior)

  destroy_terminates_all_tracked_processes
    Setup: start 3 processes, call destroy()
    Result: all terminated
```

### Integration (end-to-end)

```
BootstrapIntegrationIT (@SpringBootTest + NATS + PG):
  register_end_to_end_with_valid_token
    Setup: seed DB with logical_instance + instance_token,
           publish BootstrapRegisterRequest to zk.bootstrap.register
    Result: reply has status=OK, session in mon.active_session

TopologyIntegrationIT (@SpringBootTest + PG):
  get_topology_scoped_by_oms_returns_correct_graph
    Setup: seed instances + bindings, populate discovery cache
    Result: GET /v1/topology?oms_id=oms_dev_1 (with auth) →
            nodes for oms_dev_1 + gw_sim_1, edges between them

ManualTradingIntegrationIT (@SpringBootTest + PG + in-process gRPC):
  submit_order_routes_through_grpc_to_oms
    Setup: seed account + binding, start in-process gRPC OMS server,
           register OMS in discovery cache
    Result: POST /v1/manual/orders → gRPC PlaceOrderRequest received by fake OMS

BotExecutionIntegrationIT (@SpringBootTest + PG):
  execution_lifecycle_start_stop_restart
    Setup: seed strategy_definition + oms_instance + account + binding
    Result: start → execution created (INITIALIZING),
            stop → status=STOPPED,
            re-start → new execution created
```


## 4. Infra and Tooling Recommendations

### JUnit 5

Already in `build.gradle.kts` via `spring-boot-starter-test`. Use `@Nested` classes to group related tests within a test class. Use `@DisplayName` sparingly — test method names should be self-documenting.

### Mockito

Use for unit tests — it comes with `spring-boot-starter-test`. Use `@Mock` + `@InjectMocks` for service tests. Prefer `when(...).thenReturn(...)` over `doReturn(...).when(...)` unless stubbing void methods.

Do NOT use Mockito for repository tests — those must hit real Postgres.

### MockMvc / @WebMvcTest

Use for controller tests. Already available via `spring-boot-starter-test`. This is the right tool for auth/security testing — it loads the security filter chain without starting a full server.

Add `@Import({SecurityConfig.class, DevSecurityConfig.class})` to load auth rules in the sliced context.

### @SpringBootTest

Reserve for integration tests only. Use `webEnvironment = RANDOM_PORT` with `TestRestTemplate`.

### Testcontainers — PostgreSQL

Already declared in `build.gradle.kts`. Create a shared abstract base class:

```java
@Testcontainers
abstract class PostgresTestBase {
    @Container
    static final PostgreSQLContainer<?> pg =
        new PostgreSQLContainer<>("postgres:16-alpine")
            .withDatabaseName("zkbot_test")
            .withUsername("test")
            .withPassword("test");

    @DynamicPropertySource
    static void pgProps(DynamicPropertyRegistry reg) {
        reg.add("spring.datasource.url", pg::getJdbcUrl);
        reg.add("spring.datasource.username", pg::getUsername);
        reg.add("spring.datasource.password", pg::getPassword);
    }
}
```

Flyway runs automatically on container startup using the existing migration files.

### Testcontainers — NATS

Use a real NATS container. KV watch behavior (snapshot sentinel, PUT/DELETE events) is too nuanced to fake correctly, and the `jnats` library has no in-process test mode.

```java
@Container
static final GenericContainer<?> nats =
    new GenericContainer<>("nats:2.10-alpine")
        .withCommand("-js")
        .withExposedPorts(4222);
```

Wire `spring.nats.url` via `@DynamicPropertySource`. Only needed for integration tests — unit tests mock `Connection`.

### gRPC OMS testing

Use `io.grpc:grpc-testing` (add to test dependencies). Start an in-process server with a fake `OMSServiceImplBase` that records received requests and returns canned responses. No container needed.

```java
// In build.gradle.kts:
testImplementation("io.grpc:grpc-testing:$grpcVersion")

// In test:
Server fakeOms = InProcessServerBuilder.forName("test-oms")
    .addService(new FakeOmsService())
    .build().start();
```

Configure `DiscoveryCache` to return the in-process channel address for the test OMS.

### ProcessOrchestrator testing

Test with real subprocesses using safe commands:
- `sleep 60` for a long-running process (to test start/status)
- `true` for an immediately-exiting process (to test exit code)
- `false` for a failing process

Use `@Timeout(10)` on tests to prevent hangs. Call `destroy()` in `@AfterEach` to clean up child processes.

### Additional test dependency needed

Add to `build.gradle.kts`:

```kotlin
testImplementation("io.grpc:grpc-testing:$grpcVersion")
testImplementation("org.testcontainers:testcontainers")  // generic container support
```


## 5. Test Data Strategy

### PostgreSQL schema setup

Flyway handles schema creation automatically in test context. Combine `00_schema.sql` and `03_bootstrap.sql` into the Flyway migration baseline that already exists. No manual DDL in tests.

### Fixture insertion strategy by test layer

| Layer | Strategy |
|-------|----------|
| Repository IT | Direct SQL via `JdbcTemplate` in `@BeforeEach` or helper methods |
| Unit tests | Not applicable — repositories are mocked |
| Integration tests | SQL fixtures loaded via `@Sql` annotation or helper class |

### Recommended: `TestFixtures` helper class

Create one shared class with static factory methods for inserting common entities:

```java
class TestFixtures {
    static void insertLogicalInstance(JdbcTemplate jdbc, String logicalId,
                                      String type, String env) { ... }
    static void insertBinding(JdbcTemplate jdbc, String srcType, String srcId,
                               String dstType, String dstId) { ... }
    static void insertOmsInstance(JdbcTemplate jdbc, String omsId) { ... }
    static void insertGatewayInstance(JdbcTemplate jdbc, String gwId,
                                       String venue) { ... }
    static void insertAccount(JdbcTemplate jdbc, long accountId,
                                String venue) { ... }
    static void insertAccountBinding(JdbcTemplate jdbc, long accountId,
                                       String omsId, String gwId) { ... }
    static void insertInstanceToken(JdbcTemplate jdbc, String tokenJti,
                                      String logicalId, String type,
                                      String plaintextToken) { ... }
    static void insertStrategy(JdbcTemplate jdbc, String strategyId) { ... }
    static void insertExecution(JdbcTemplate jdbc, String executionId,
                                  String strategyId, String status) { ... }
    static void insertActiveSession(JdbcTemplate jdbc, String sessionId,
                                      String logicalId, String type) { ... }
    static void insertTrade(JdbcTemplate jdbc, long accountId) { ... }
    static void insertBalanceSnapshot(JdbcTemplate jdbc, long accountId) { ... }
}
```

This avoids duplicating insert SQL across test classes and provides a single place to update when schema changes.

### Discovery / KV fixtures

For unit tests: build `ServiceRegistration` protobuf messages directly in Java and put them into a `Map<String, ServiceRegistration>`:

```java
var reg = ServiceRegistration.newBuilder()
    .setServiceId("oms_dev_1")
    .setServiceType("OMS")
    .setEndpoint(Endpoint.newBuilder().setAddress("localhost:9090"))
    .putAttrs("version", "1.0")
    .setUpdatedAtMs(System.currentTimeMillis())
    .build();
Map<String, ServiceRegistration> liveState = Map.of("svc.oms.oms_dev_1", reg);
```

For integration tests: insert into real NATS KV bucket via `KeyValue.put()`.

### Redis fixtures

For integration tests with Redis container: use `RedisTemplate` or `Lettuce` commands directly:

```java
redis.opsForValue().set("oms:oms_dev_1:balance:9001:USDT", serializedBalance);
```

For unit tests: mock `StringRedisTemplate` / `RedisTemplate`.


## 6. Test Package / Class Layout

```
src/test/java/com/zkbot/pilot/
├── support/
│   ├── PostgresTestBase.java          -- shared PG container + Flyway
│   ├── TestFixtures.java              -- SQL insert helpers
│   └── FakeOmsService.java            -- in-process gRPC stub
├── bootstrap/
│   ├── BootstrapServiceTest.java      -- unit: register/deregister decision tree
│   ├── BootstrapRepositoryIT.java     -- SQL: token, session, instance ID, audit
│   ├── TokenServiceTest.java          -- unit: hash, generate
│   └── KvReconcilerTest.java          -- unit: live set, fence triggers
├── topology/
│   ├── TopologyServiceTest.java       -- unit: workspace scope, node/edge mapping,
│   │                                     view validation, reload guards
│   ├── TopologyRepositoryIT.java      -- SQL: instances, bindings, sessions, audit
│   └── TopologyControllerTest.java    -- MockMvc: routing, auth, error shapes
├── manual/
│   ├── ManualServiceTest.java         -- unit: preview, routing, proto build
│   └── ManualControllerTest.java      -- MockMvc: auth, request validation
├── account/
│   ├── AccountServiceTest.java        -- unit: binding enrichment, key patterns
│   ├── AccountRepositoryIT.java       -- SQL: CRUD, joins, UNION activities
│   └── AccountControllerTest.java     -- MockMvc: auth, role enforcement
├── bot/
│   ├── BotServiceTest.java            -- unit: lifecycle, singleton, enriched config
│   ├── StrategyRepositoryIT.java      -- SQL: array columns, config_json, filters
│   ├── ExecutionRepositoryIT.java     -- SQL: CRUD, status, singleton query
│   └── BotControllerTest.java         -- MockMvc: auth, role enforcement
├── ops/
│   ├── OpsServiceTest.java            -- unit: stub responses
│   └── OpsControllerTest.java         -- MockMvc: auth
├── orchestrator/
│   └── ProcessOrchestratorTest.java   -- unit: start/stop/status with real subprocess
├── discovery/
│   ├── DiscoveryCacheTest.java        -- unit: put/get/getByType thread safety
│   └── DiscoveryResolverTest.java     -- unit: key matching, type lookup
├── grpc/
│   └── OmsGrpcClientTest.java         -- unit: channel cache, address invalidation
├── config/
│   └── SecurityConfigTest.java        -- MockMvc: health public, role matrix
├── common/
│   └── GlobalExceptionHandlerTest.java -- MockMvc: error response shapes
└── integration/
    ├── BootstrapIntegrationIT.java     -- full: NATS register → DB session
    ├── TopologyIntegrationIT.java      -- full: GET scoped topology
    ├── ManualTradingIntegrationIT.java -- full: order submit → gRPC
    └── BotExecutionIntegrationIT.java  -- full: start/stop/restart lifecycle
```

Naming convention:
- `*Test.java` — unit tests (no Spring context or Testcontainers)
- `*IT.java` — integration tests (Testcontainers or full Spring context)


## 7. CI-Friendly Scope Split

### Every local build (`./gradlew test`)

All unit tests (`*Test.java`). Run in <10 seconds. No containers, no external dependencies.

Configure via Gradle:

```kotlin
tasks.withType<Test> {
    useJUnitPlatform {
        excludeTags("integration", "slow")
    }
}

tasks.register<Test>("integrationTest") {
    useJUnitPlatform {
        includeTags("integration")
    }
}
```

Tag integration tests with `@Tag("integration")` on the class.

### Every PR in CI

Unit tests + Repository ITs + Controller tests. These catch the two most dangerous bug classes: SQL mismatches and auth gaps. Target: <60 seconds including PG container startup.

### Full integration profile (nightly or pre-merge)

All of the above + `integration/` package tests (NATS container, Redis container, in-process gRPC). These are the end-to-end smoke tests. Target: <3 minutes.

### Gradle task summary

| Command | What runs | When |
|---------|-----------|------|
| `./gradlew test` | Unit tests only | Every local build |
| `./gradlew test integrationTest` | Unit + all ITs | CI on every PR |
| `./gradlew test integrationTest -Dfull=true` | Everything | Nightly / pre-merge |


## 8. Implementation Priority

Start with these — they catch the most bugs per test written:

1. **`BootstrapRepositoryIT`** — every SQL query in the bootstrap path is hand-written and critical for wire compat with Rust services. One wrong column name breaks registration for the entire fleet.

2. **`TopologyServiceTest`** — workspace scoping logic is the most complex pure-logic code in the service. It's been revised multiple times; unit tests lock down the current behavior.

3. **`StrategyRepositoryIT`** — `bigint[]` and `text[]` JDBC handling is fragile. If array serialization breaks, strategy CRUD is dead.

4. **`SecurityConfigTest`** — a role matrix test that hits every endpoint pattern with every role. Catches the "new endpoint defaults to ADMIN but should allow TRADER" class of bugs.

5. **`AccountRepositoryIT`** — the activities UNION query and binding join have had column mismatches before. SQL tests prevent regression.

Everything else fills in from there, but these five test classes cover the highest-risk code paths.
