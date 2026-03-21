# IBKR Integration Design

## Positioning

IBKR should be treated as a session-constrained, query-heavy Python venue.

Recommended capability split:

- `gw`: Python
- `rtmd`: Python
- `refdata`: Python

Reasoning:

- IBKR is operationally shaped by TWS / IB Gateway and entitlement constraints
- performance is not the primary concern here
- correctness and recovery matter more than trying to force a native Rust-first implementation too early

This venue should use the shared Python bridge path described in
[13-python-venue-bridge.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md).

## Module shape

Recommended module shape:

```text
venue-integrations/ibkr/
  manifest.yaml
  ibkr/
    __init__.py
    gw.py
    rtmd.py
    refdata.py
    ibkr_client.py
    ibkr_contracts.py
    ibkr_normalize.py
  schemas/
    gw_config.schema.json
    rtmd_config.schema.json
    refdata_config.schema.json
```

Manifest language choices:

- `gw.language = python`
- `rtmd.language = python`
- `refdata.language = python`

Implementation note:

- the manifest entrypoint should use package-style module paths such as
  `python:ibkr.gw:IbkrGatewayAdaptor`
- the Python code should therefore live in a real package under
  `venue-integrations/ibkr/ibkr/`
- avoid flat shared module names under a common `python/` folder because the merged Python bridge
  resolves venue modules by package path and must not collide with other venues’ `gw.py` / `rtmd.py`

## Recommended library choice

Production recommendation:

- `ib_async`

Supporting stack:

- use `httpx` only for any ancillary HTTP control or operator tooling if needed
- do not build the core venue adaptor around generic REST/WS clients

Protocol source of truth:

- official IBKR TWS / IB Gateway API documentation

Reasoning:

- `ib_async` is the right fit for the Python venue bridge path
- it is materially cleaner for async integration than the low-level official client
- the official TWS API docs remain the authoritative source for connection, order, pacing, and recovery semantics

Why this stack:

- IBKR integration is centered on the TWS / IB Gateway session model, not a generic REST + WebSocket shape
- `ib_async` already models the connection and event flow at the right abstraction layer for this venue

## Trading gateway design

Host:

- `zk-gw-svc`

Implementation path:

- Python adaptor loaded through the shared bridge

Python adaptor implementation requirements:

- implement async class `IbkrGatewayAdaptor`
- constructor signature:
  - `__init__(self, config: dict)`
- required async methods:
  - `connect()`
  - `place_order(req: dict) -> dict`
  - `cancel_order(req: dict) -> dict`
  - `query_balance(req: dict) -> list[dict]`
  - `query_order(req: dict) -> list[dict]`
  - `query_open_orders(req: dict) -> list[dict]`
  - `query_trades(req: dict) -> list[dict]`
  - `query_funding_fees(req: dict) -> list[dict]`
  - `query_positions(req: dict) -> list[dict]`
  - `next_event() -> dict`

Bridge/runtime constraints:

- all async methods run on one persistent Python event loop managed by `zk-pyo3-bridge`
- keep `ib_async` session state, callback wiring, queues, and polling tasks on that loop
- do not call `asyncio.run()` inside the adaptor
- push normalized facts/events into an internal `asyncio.Queue`; `next_event()` should await that queue

Returned event shape:

- `order_report`
  - `{"event_type": "order_report", "payload_bytes": b"...protobuf bytes..."}`
- `balance`
  - `{"event_type": "balance", "payload": [...]}`
- `position`
  - `{"event_type": "position", "payload": [...]}`
- `system`
  - `{"event_type": "system", "payload": {...}}`

Config-loading rules:

- the host passes `ZK_VENUE_CONFIG` JSON into the adaptor constructor
- the Rust bridge validates that config against the manifest-declared schema before class construction
- keep live secrets and session details outside source-controlled config; pass secret refs or deployment config instead

Recommended adaptor mode:

- `hybrid`, heavily query-backed

Recommended IBKR behavior:

- treat TWS / IB Gateway as an external session dependency
- use callbacks and streaming where available
- do not rely on callbacks alone for correctness
- poll open orders, executions, positions, and account summary on bounded intervals
- run query-after-action after place/cancel for confirmation and state convergence

Why this matters:

- IBKR callback behavior is useful but not sufficient as the semantic source of truth
- order identity, fill state, and account updates may require compensating queries
- reconnect and session churn are part of normal operation

Suggested Python module split:

- `ibkr_client.py`
  - `ib_async` session setup, reconnect, callback registration
- `ibkr_contracts.py`
  - contract construction and canonical mapping helpers
- `ibkr_normalize.py`
  - callback/query output -> gateway-facing dicts / protobuf bytes
- `gw.py`
  - `IbkrGatewayAdaptor`, polling tasks, queue management, query-after-action logic

Adaptor-owned responsibilities:

- IB session establish/reconnect
- contract translation
- mapping order status / execution callbacks into venue facts
- rate-limit and entitlement-aware API usage
- translating account/position snapshots into canonical facts

Gateway host responsibilities remain:

- ACK semantics
- queueing and worker model
- downstream publication guarantees
- trust-window and compensating-query semantics

### Trading API mapping

Recommended initial mapping:

- `PlaceOrder`
  - TWS `placeOrder`
- `CancelOrder`
  - TWS cancel path for the active order id
- `QueryOrder`
  - open-order snapshots and order-status callbacks, with periodic resync
- `QueryTrades`
  - execution details / commissions plus periodic query path
- `QueryBalance`
  - account summary path
- `QueryPositions`
  - positions path

Primary fact sources:

- `openOrder`
- `orderStatus`
- `execDetails`
- account and position callbacks

Recommended correctness rule:

- do not rely on callbacks alone
- pair callbacks with bounded periodic queries and query-after-action recovery

## RTMD design

Host:

- `zk-rtmd-gw-svc`

Implementation path:

- Python adaptor through the shared bridge

Python adaptor implementation requirements:

- implement async class `IbkrRtmdAdaptor`
- constructor signature:
  - `__init__(self, config: dict)`
- required async methods:
  - `connect()`
  - `subscribe(req: dict)`
  - `unsubscribe(req: dict)`
  - `snapshot_active() -> list[dict]`
  - `instrument_exch_for(instrument_id: str) -> str | None`
  - `query_current_tick(instrument_id: str) -> bytes`
  - `query_current_orderbook(instrument_id: str, depth: int | None = None) -> bytes`
  - `query_current_funding(instrument_id: str) -> bytes`
  - `query_klines(...) -> list[bytes]`
  - `next_event() -> dict`

Bridge/runtime constraints:

- keep the `ib_async` connection, subscription state, and callback queue on the persistent bridge-managed loop
- publish RTMD events to an internal queue consumed by `next_event()`
- only advertise capabilities that the current session and entitlements actually support

Recommended capabilities:

- `tick`
- `kline`
- current snapshot queries

Optional capabilities:

- `orderbook`
- richer depth streams

Recommended initial rule:

- capability-gate market-depth and only advertise it if semantics and entitlements are acceptable

Recommended runtime scope:

- logical-instance-scoped or session-scoped RTMD publisher

Reasoning:

- IBKR market data is tightly tied to session and entitlement state
- a naive venue-wide shared publisher is less clean than for OKX

Recommended runtime behavior:

- dedupe downstream lease interest where possible
- keep capability advertisement honest per deployment/session
- surface entitlement or unavailable-data states as clear runtime/system signals

### RTMD API mapping

Recommended initial mapping:

- `tick`
  - real-time market data subscriptions
- `kline`
  - historical data requests for bounded recent history
- current snapshot queries
  - latest market data snapshot if available in the session

Optional capabilities:

- market depth / orderbook

Recommended rule:

- capability-gate depth and only expose it if the deployed account/session entitlements support it

## Refdata design

Host:

- `zk-refdata-svc`

Implementation path:

- Python loader through the shared bridge
- loaded from the IBKR venue manifest `refdata` capability and validated against
  `schemas/refdata_config.schema.json`

Python loader implementation requirements:

- implement async class `IbkrRefdataLoader`
- constructor signature:
  - `__init__(self, config: dict)`
- required async methods:
  - `load_instruments() -> list[dict]`
  - `load_market_sessions() -> list[dict]`

Suggested Python module split:

- `refdata.py`
  - loader entrypoint and orchestration
- `ibkr_client.py`
  - `ib_async` or other session-backed metadata lookups
- `ibkr_contracts.py`
  - canonical contract shaping
- `ibkr_normalize.py`
  - refdata row normalization for the refdata host

Integration rule:

- the entrypoint should be `python:ibkr.refdata:IbkrRefdataLoader`
- the generic refdata host should resolve that entrypoint from `venue-integrations/ibkr/manifest.yaml`
- IBKR-specific refdata logic should stay in the venue package, not in a service-local loader list

Recommended scope for first implementation:

- ETFs
- stocks

Later expansion:

- futures
- options

Refdata is more important for IBKR than for crypto venues because:

- contract identity is richer
- market/session metadata matters materially
- symbol ambiguity is higher

Recommended refresh model:

- scheduled canonical contract refresh
- explicit exchange / currency / contract-type normalization
- lifecycle-aware writes into PostgreSQL

Required canonical concerns:

- stable instrument id derivation
- contract exchange, currency, secType, multiplier
- trading-class / primary-exchange style metadata where needed
- market/session linkage suitable for `QueryMarketStatus` and `QueryMarketCalendar`

Session rule:

- IBKR session metadata must come from the IBKR refdata/session adaptor path
- the refdata host must not treat IBKR as an always-open venue

### Refdata API mapping

Recommended initial mapping:

- contract detail lookups for canonical security identity
- exchange / primary exchange / currency / security type normalization
- market/session metadata from the refdata/session pipeline described in
  [14-refdata-service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/14-refdata-service.md)

## Venue config

Recommended gateway config fields:

- `host`
- `port`
- `client_id`
- `account_code`
- `mode`
  - `paper` or `live`
- `read_only`
  - should be false for trading sessions
- optional `master_client_id` policy

Recommended RTMD config fields:

- `host`
- `port`
- `client_id`
- `mode`
- `market_data_type`

Recommended refdata config fields:

- `host`
- `port`
- `client_id`
- `account_code`

Operational notes:

- TWS or IB Gateway must be running and authenticated
- live and paper sessions use different defaults and should be configured explicitly
- the deployment must account for daily restart/reset behavior

Bridge-specific config note:

- the Python bridge is enabled in the Rust hosts with the `python-venue` cargo feature
- `ZK_VENUE_ROOT` must point at `venue-integrations/`
- `ZK_VENUE_CONFIG` must validate against the manifest-declared JSON schema before startup

## ID linkage mechanism

Recommended rule:

- use the locally allocated `orderId` only as the session-scoped submission key
- persist `permId` as the durable venue-native identifier
- maintain mapping:
  - `client_order_id -> orderId`
  - `client_order_id -> permId`
  - `permId -> latest known order state`

Why:

- `orderId` is required for placement and is derived from `nextValidId`
- `permId` is the durable unique identifier across reconnects and is the correct anchor for OMS reconciliation

Important operational notes:

- do not send orders before `nextValidId` handshake completes
- binding legacy/manual orders may require client id `0` and has side effects documented by IBKR

## Rate limiting handling

Recommended design:

- enforce a session-level outbound request limiter in the adaptor
- separate market-data subscription budgeting from trading/order management budgeting
- keep a safety margin under documented pacing ceilings

Important documented constraints:

- IBKR documents a max rate of 50 messages per second before pacing violation error `100`
- excessive rate can cause disconnection
- active market-data line limits and entitlement limits are separate from message pacing

Recommended rule:

- design around sustained rates well below the hard ceiling
- queue and coalesce low-priority refreshes when needed

## Reconnect handling

Recommended design:

- treat TWS / IB Gateway disconnects and daily resets as normal conditions
- reconnect, wait for handshake completion, then explicitly rebuild runtime state

Recommended sequence:

1. reconnect socket to TWS / IB Gateway
2. wait for `nextValidId`
3. re-establish order/account/position subscriptions
4. handle connectivity codes:
   - `1100` lost connectivity
   - `1101` connectivity restored, data lost
   - `1102` connectivity restored, data maintained
   - `1300` socket port changed, reconnect on the new port
5. after `1101`, resubmit market-data requests
6. run bounded open-order, execution, account-summary, and position reconciliation
7. only then return the adaptor to healthy state

Operational note:

- TWS / IB Gateway are designed to restart daily, so this path must be first-class rather than exceptional

## Market-session design

IBKR is the strongest consumer of the full refdata-service market-session plan from
[14-refdata-service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/14-refdata-service.md).

Recommended rule:

- treat market-session ownership as first-class for IBKR

That means:

- canonical market status should be backed by refdata/session state rather than client-local heuristics
- `QueryMarketStatus` and `QueryMarketCalendar` should be meaningful for IBKR-covered markets
- the gateway and RTMD layers may consult session metadata, but the refdata service remains the canonical source

## Runtime ownership split

Service hosts own:

- bootstrap
- registration
- supervision
- generic contracts

IBKR Python modules own:

- TWS / IB Gateway API interaction
- contract normalization details
- callback + polling integration
- session and entitlement quirks

## Initial rollout order

1. Implement IBKR refdata loader for ETFs and stocks first under `venue-integrations/ibkr/ibkr/refdata.py` and wire it through the manifest-driven host path.
2. Implement Python IBKR gateway adaptor with hybrid callback-plus-query recovery.
3. Implement Python IBKR RTMD adaptor with tick and bounded history support.
4. Add futures and other richer contract families after the first lifecycle path is stable.

## Main risks

- trying to model IBKR as a simple exchange-like venue and underestimating session/entitlement complexity
- using callbacks as semantic truth instead of as one input into a recovery-aware pipeline
- overpromising RTMD capability coverage before entitlements and semantics are proven

## Recommended tests

- query-after-action confirms order linkage and final gateway-visible order state
- periodic reconciliation catches callback gaps without duplicating downstream fills
- refdata normalization produces stable canonical instrument ids for ETFs and stocks
- market-status and market-calendar queries are backed by canonical session data rather than client-local guesses

## References

- IBKR TWS API docs: https://ibkrcampus.com/campus/ibkr-api-page/twsapi-doc/
- IBKR initial setup: https://interactivebrokers.github.io/tws-api/initial_setup.html
- `ib_async`: https://github.com/ib-api-reloaded/ib_async
