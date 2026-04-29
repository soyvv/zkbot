# Lessons Learnt

## 2026-04-21: Pilot dev profile silently shadows ZK_VENUE_INTEGRATIONS_ROOT

### Problem
- Pilot UI dropdowns (GW/MDGW Onboard, Create Account) showed only a subset of
  venues (oanda/okx/simulator) with no IBKR, even though all four manifests
  existed under `zkbot/venue-integrations/` and the `pilot-java-run` Make target
  explicitly exports `ZK_VENUE_INTEGRATIONS_ROOT=$(CURDIR)/venue-integrations`.
- `application-dev.yml` hardcoded `pilot.venue-integrations-root: ../venue-integrations`
  and `service-manifests-root: ../service-manifests`. Under Spring Boot the
  profile-specific value overrides the default `${ZK_VENUE_INTEGRATIONS_ROOT:}`
  from `application.yml`, so the Makefile's env var was silently ignored.
- `bootRun` cwd is `zkbot/java/pilot-service`, so `../venue-integrations` tried
  to resolve to `zkbot/java/venue-integrations` (doesn't exist). Log showed
  `venue-manifest: path not found: ../venue-integrations` and the in-memory
  manifest map stayed empty.
- Downstream: `MetaService.buildVenueRefs` fell back to DB-distinct venues, so
  venues appeared without a `capabilities` array; the GW Onboard dropdown
  filters by `capabilities.includes("gw")` → empty list. The Create Account
  dropdown had no such filter so the DB rows leaked through without capabilities.

### Fix
- Changed `application-dev.yml` to `${ZK_VENUE_INTEGRATIONS_ROOT:../venue-integrations}`
  (and the same for `service-manifests-root`). Env var now wins when set,
  relative fallback preserved for plain `gradle bootRun`.
- Post-restart log confirms `venue-manifest: loaded 4 venue manifests: [oanda, ibkr, simulator, okx]`.

### Rule
- When Spring Boot config supports `${ENV_VAR:default}`, prefer that over a
  bare hardcoded value in profile YAML — otherwise the env-var escape hatch
  that higher layers rely on is silently defeated by profile activation.

## 2026-04-20: PyO3 embedded interpreter ignores PYO3_PYTHON at runtime

### Problem
- All `gw-*-paper` / `gw-*-demo` Makefile targets that load Python venue adapters via
  `--features python-venue` set `PYO3_PYTHON=$(WORKSPACE_PYTHON)` and
  `PYTHONHOME=$(PYO3_PYTHON_HOME)` (base prefix), then `unset VIRTUAL_ENV CONDA_PREFIX`.
- `pyo3::prepare_freethreaded_python()` does NOT honor `PYO3_PYTHON` at runtime (it is a
  build-time hint only). With `PYTHONHOME` pinned to the base prefix, `sys.prefix` becomes
  the base prefix and the workspace `.venv/lib/python3.13/site-packages` is never on
  `sys.path`. Result: `ModuleNotFoundError: No module named 'oanda'` / `'ibkr'`, even
  though `.venv/bin/python -c 'import ibkr'` works fine outside the embedded interpreter.
- The OANDA pilot success log from 2026-04-13 predates the 2026-04-19 "update structure"
  commit (`9c6433d`) that consolidated venue venvs into a single workspace `.venv`. No
  `python-venue` Rust host had actually run successfully against the new layout until IBKR.

### Fix
- `zk-pyo3-bridge::PyRuntime::initialize()` now promotes `PYO3_PYTHON` to
  `PYTHONEXECUTABLE` before `prepare_freethreaded_python()`. On macOS Python honors
  `PYTHONEXECUTABLE` to derive `sys.executable`, which lets Python read the venv's
  `pyvenv.cfg` and load all `.pth` files at init. With `PYTHONHOME` providing stdlib and
  `PYTHONEXECUTABLE` providing the venv shim, both stdlib and venv site-packages resolve.
- Single-line bridge change benefits every venue: OANDA, IBKR, future Python adapters.
- Stays within the dependency-contract (no `PYTHONPATH` in Makefile, no `sys.path`
  mutation).

## 2026-03-16: gw-sim devops enablement

### OrderReport.exchange field must be stamped by the gateway
- OMS uses `OrderReport.exchange` as `gw_key` to map reports to accounts
- If empty, reports are silently discarded (`no account mapping for gw_key; discarding`)
- Old `zk-mock-gw` hardcoded `exchange: "MOCK"` in fill reports
- Fix: `nats_publisher.rs` clones report and stamps `exchange = gw_id` before publishing

### Sim order lifecycle must always emit Booked before Fill
- OMS expects full lifecycle: Pending → Booked → Filled/Cancelled
- `ImmediateMatchPolicy` was skipping Booked when matches existed, going straight to Fill
- OMS never published a Booked `OrderUpdateEvent`, causing bench timeouts
- Fix: `simulator.rs` always emits `make_booked_report()` before processing match results

### Match policy matters for E2E test flows
- `immediate` policy fills orders in the same `on_new_order` call — cancel flow (Flow 2) can't work because the order is already filled before cancel arrives
- Solution: use `fcfs` policy (fills only on tick data), use admin `ForceMatch` RPC for deterministic fills in Flow 3
- Default dev stack match policy changed from `immediate` to `fcfs`

### Redis flush kills running OMS
- `docker exec redis-cli flushall` removes OMS's KV fencing/heartbeat state
- OMS becomes unresponsive — must be restarted after Redis flush
- Use `scripts/clear_oms_redis.sh` (pattern-based delete) instead of `flushall`

### Docker DNS vs localhost for rpc_endpoint
- When OMS runs on host (not Docker), Docker DNS names like `gw-sim:51051` don't resolve
- `02_oms_svc_compat.sql` must use `localhost:51051` for `make dev-up + make oms-run` workflow
- For `dev-up-full` (OMS in Docker), Docker DNS works but localhost doesn't

## 2026-03-23: OMS/GW order-request debugging

### OMS order validation must live in `zk-oms-rs`, not only `zk-oms-svc`
- gRPC-side validation alone protects one ingress path but does not harden the OMS domain model
- Invalid requests like `order_id <= 0` should be rejected by shared OMS logic so alternate callers and future service layers cannot bypass the guard
- Keep `oms-svc` as a thin consumer of `zk-oms-rs::validation`, with `OmsCoreV2` re-checking before mutating state

### Local OMS/GW debugging needs persisted logs by default-friendly targets
- Stdout-only local runs make it hard to correlate `order_id` across OMS ingress, writer dispatch, GW ingress, and GW exec shards after the fact
- A small wrapper script plus `make oms-run-log`, `make gw-run-log`, and `make dev-logs-save` is enough to create repeatable log capture without changing the usual runtime commands

## 2026-03-24: Pilot venue-backed onboarding config shape

### Venue-backed create flows must normalize config server-side
- The Pilot UI and backend drifted on create payload naming: UI posted `runtimeConfig`, Java expected `providedConfig`
- That mismatch silently stored `{}` for newly created GW/MDGW rows, which only showed up later at bootstrap time
- Fix: accept the legacy alias in the Java DTO and normalize venue-backed config in `TopologyService` before persistence

### Real-venue descriptors must match nested `venue_config` paths
- Pilot merges real venue schemas under `provided_config.venue_config`, but manifest field descriptors stay venue-local (`/secret_ref`, `/api_base_url`, ...)
- Reusing those raw paths in bootstrap/drift logic breaks secret-ref extraction and reload classification for GW/MDGW
- Fix: prefix venue-capability descriptor paths with `/venue_config` for non-inline venues when resolving instance descriptors

## 2026-03-25: OMS balance / position redesign catch-up

### Runtime separation can look complete before the live path is actually complete
- The OMS V2 stores already separate balances, managed positions, exchange positions, and reservations
- That is not enough by itself; the live runtime still lagged in startup reconcile, query semantics, and gateway plumbing
- The design review should always trace the full path: gateway query RPC -> OMS startup reconcile -> periodic recheck -> gRPC query -> event publication -> replay parity

### Spot exposure is the easiest place to accidentally reintroduce domain conflation
- Spot inventory legitimately appears in both domains: as exchange-owned `Balance` and as derived operational `Position`
- If the projection boundary is not explicit, code drifts back toward one overloaded balance-like model
- Keep the rule explicit in code and tests: canonical spot asset state is balance, derived sellable exposure is position

## 2026-04-14: Pilot bootstrap duplicate handling on service restart

### Rust Pilot clients should treat `DUPLICATE` bootstrap as transient for a bounded window
- Pilot duplicate rejection is correct while the prior KV lease is still present, especially after crash or abrupt session teardown
- Java Pilot currently learns TTL expiry through a periodic KV sweep, so a restarted service can be rejected for longer than the raw 30s lease
- The shared Rust bootstrap client should retry `DUPLICATE` for a bounded window rather than panic immediately; this preserves singleton ownership while making restart-after-crash workable
