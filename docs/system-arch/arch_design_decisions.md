# Architecture Design Decisions — zkbot

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
but do NOT publish leases to `zk.rtmd.subs.v1`. `RtmdInterestManager` is implemented in
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
