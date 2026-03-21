# OANDA Integration Design

## Positioning

OANDA should be treated as a correctness-first Python venue.

Recommended capability split:

- `gw`: Python
- `rtmd`: Python
- `refdata`: Python

Reasoning:

- raw performance is not the priority for this venue
- OANDA’s API shape is more REST/stream hybrid than low-latency exchange-native
- the Python venue bridge is a good fit here

This venue should follow the shared Python path described in
[13-python-venue-bridge.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md).

## Module shape

Recommended module shape:

```text
venue-integrations/oanda/
  manifest.yaml
  oanda/
    __init__.py
    gw.py
    rtmd.py
    refdata.py
    oanda_client.py
    oanda_stream.py
    oanda_normalize.py
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
  `python:oanda.gw:OandaGatewayAdaptor`
- the Python code should therefore be organized as a real package under
  `venue-integrations/oanda/oanda/`, not as loose flat files under `python/`
- this matches the merged Python venue bridge and avoids cross-venue import collisions on generic
  names like `gw`, `rtmd`, and `refdata`

## Recommended library choice

Production recommendation:

- use direct async clients against the official OANDA v20 API
- use:
  - `httpx.AsyncClient` for REST
  - `websockets` for WebSocket-style streaming usage
  - persistent streaming HTTP handling where OANDA exposes stream endpoints that are not true WebSockets

Secondary compatibility option:

- OANDA `v20` Python bindings

Reasoning:

- the official docs are the stable source of truth
- the older `v20` Python bindings are useful as reference material, but the runtime should not depend
  on a stale library if direct async clients are cleaner

Why this stack:

- `httpx` is the cleanest modern async HTTP client for Python
- `websockets` is a focused async streaming client when a true WS transport is needed
- keeping the transport layer explicit is preferable to binding the runtime tightly to the older OANDA SDK

## Trading gateway design

Host:

- `zk-gw-svc`

Implementation path:

- Python adaptor loaded through the shared PyO3 bridge

Python adaptor implementation requirements:

- implement an async class `OandaGatewayAdaptor`
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
- the adaptor may safely keep long-lived state on that loop:
  - `httpx.AsyncClient`
  - streaming connections
  - asyncio queues
  - background polling tasks
- do not call `asyncio.run()` inside the adaptor
- use an internal `asyncio.Queue` for gateway facts/events and have `next_event()` await it

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

- the host passes `ZK_VENUE_CONFIG` as a JSON object to the Python adaptor constructor
- the Rust bridge validates that config against the manifest-declared JSON schema before class construction
- keep secret material out of that JSON when possible; prefer secret refs and let the host/bootstrap layer resolve them

Recommended adaptor mode:

- `hybrid`, biased toward query-backed correctness

Recommended OANDA behavior:

- use REST for place/cancel/query flows
- use streaming or transaction feed as a fact source, not as the sole semantic authority
- after local order actions, run query-after-action reconciliation
- use periodic polling for recovery and for state convergence after reconnects

Why this matters:

- OANDA’s trade, order, and position concepts do not map one-to-one onto the OMS model
- account-state causality is safer when balance/position are treated as query-backed snapshots

Suggested Python module split:

- `oanda_client.py`
  - auth, REST client construction, environment URL handling
- `oanda_stream.py`
  - transaction/pricing stream reader, reconnect, queue feed
- `oanda_normalize.py`
  - OANDA payloads -> gateway-facing dicts / protobuf bytes
- `gw.py`
  - `OandaGatewayAdaptor` orchestration, polling tasks, queue management

Adaptor-owned responsibilities:

- auth and environment selection (`practice` vs `live`)
- REST and pricing/transaction stream session handling
- mapping OANDA order/trade/transaction records into gateway-facing venue facts
- OANDA-specific order type behavior such as TP/SL attachments if supported later

Gateway host responsibilities remain:

- internal ACK semantics
- event publication guarantees
- trust-window handling
- normalized downstream report shapes

### Trading API mapping

Recommended initial mapping:

- `PlaceOrder`
  - `POST /accounts/{accountID}/orders`
- `CancelOrder`
  - cancel order endpoint for the target order
- `QueryOrder`
  - order endpoints for open/specific orders
- `QueryTrades`
  - trade and transaction endpoints
- `QueryBalance`
  - account details / summary style endpoints
- `QueryPositions`
  - positions endpoints

Primary semantic sources:

- REST order/trade/account queries
- transaction stream as a near-real-time fact source

Recommended correctness rule:

- maintain a local account snapshot and reconcile with poll/update endpoints after actions and after reconnect

## RTMD design

Host:

- `zk-rtmd-gw-svc`

Implementation path:

- Python adaptor through the shared bridge

Python adaptor implementation requirements:

- implement async class `OandaRtmdAdaptor`
- constructor signature:
  - `__init__(self, config: dict)`
- required async methods:
  - `connect()`
  - `subscribe(req: dict)`
  - `unsubscribe(req: dict)`
  - `snapshot_active() -> list[dict]`
  - `query_current_tick(instrument_id: str) -> bytes | dict`
  - `query_current_orderbook(instrument_id: str, depth: int | None = None) -> bytes | dict`
  - `query_current_funding(instrument_id: str) -> bytes | dict`
  - `query_klines(...) -> list[bytes] | list[dict]`
  - `next_event() -> dict`

Bridge/runtime constraints:

- keep subscription state on the persistent event loop
- drive live stream events into an internal queue consumed by `next_event()`
- use protobuf bytes for RTMD event payloads where the bridge expects proto-backed events

Recommended capabilities:

- `tick`
- `kline`
- current snapshot queries

Optional capability:

- `orderbook`

Recommended initial rule:

- do not promise orderbook unless semantics are acceptable for actual downstream use

Recommended runtime shape:

- one venue-scoped OANDA RTMD gateway per environment
- aggregate downstream lease interest from `zk.rtmd.subs.v1`
- dedupe upstream pricing subscriptions
- define query correctness in terms of venue-backed data, not local cache

### RTMD API mapping

Recommended initial mapping:

- `tick`
  - pricing stream
- `kline`
  - REST candles/instruments endpoints if available for the needed product set
- current snapshot queries
  - pricing or latest-price REST endpoints

Recommended initial non-goal:

- avoid advertising `orderbook` until the semantics are proven useful for downstream consumers

## Refdata design

Host:

- `zk-refdata-svc`

Implementation path:

- Python loader through the shared bridge
- loaded from the OANDA venue manifest `refdata` capability and validated against
  `schemas/refdata_config.schema.json`

Python loader implementation requirements:

- implement async class `OandaRefdataLoader`
- constructor signature:
  - `__init__(self, config: dict)`
- required async methods:
  - `load_instruments() -> list[dict]`
  - `load_market_sessions() -> list[dict]`

Suggested Python module split:

- `refdata.py`
  - class entrypoint plus orchestration
- `oanda_client.py`
  - instrument fetch requests
- `oanda_normalize.py`
  - canonical refdata row shaping for the refdata host

Integration rule:

- the entrypoint should be `python:oanda.refdata:OandaRefdataLoader`
- the generic refdata host should resolve that entrypoint from `venue-integrations/oanda/manifest.yaml`
- OANDA-specific refdata logic should not live in a service-local loader registry inside
  `zk-refdata-svc`

Source material:

- existing OANDA loader logic can be migrated from the older refdata loader code
- remove hardcoded credentials completely

Recommended refresh model:

- refresh instrument metadata every 15-60 minutes unless experience shows faster churn
- normalize into canonical instrument ids and canonical row shape
- write lifecycle-aware updates into PostgreSQL

Recommended canonical concerns:

- OANDA instrument naming and symbol normalization
- CFD / FX type mapping
- precision and lot-size fields
- environment-specific availability if applicable

Market-session design:

- OANDA is not a clean TradFi exchange-calendar venue, but it is not pure 24x7 crypto either
- keep session treatment simple initially
- implement bounded market-status semantics only where they materially help operator tooling
- provide any non-trivial session information through `load_market_sessions()` rather than relying
  on the host to mark OANDA as globally `open`

### Refdata API mapping

Recommended initial mapping:

- account instruments endpoint for tradable instrument metadata
- instrument definitions used to populate canonical trading params

The refdata loader should migrate the existing older OANDA fetcher logic into the
`venue-integrations/oanda/oanda/` package and have the generic `zk-refdata-svc` host resolve it
through the OANDA manifest, as described in
[14-refdata-service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/14-refdata-service.md).

## Venue config

Recommended gateway config fields:

- `environment`
  - `practice` or `live`
- `account_id`
- `token_ref`
- `api_base_url`
- `stream_base_url`
- `accept_datetime_format`

Recommended RTMD config fields:

- `environment`
- `account_id`
- `token_ref`
- `stream_base_url`

Recommended refdata config fields:

- `environment`
- `account_id`
- `token_ref`
- `api_base_url`

Operational note:

- keep credentials in Vault and pass only secret refs into the service config

Bridge-specific config note:

- the Python bridge is enabled in the Rust hosts with the `python-venue` cargo feature
- `ZK_VENUE_ROOT` must point at `venue-integrations/`
- `ZK_VENUE_CONFIG` must be valid against `schemas/gw_config.schema.json`,
  `schemas/rtmd_config.schema.json`, or `schemas/refdata_config.schema.json` as appropriate

## ID linkage mechanism

Recommended rule:

- use OANDA `clientExtensions.id` as the upstream `client_order_id`
- treat OANDA order ids and transaction ids as venue-native identifiers
- persist linkage from the first response/event that includes both the client extension and OANDA-native ids

Also capture:

- `relatedTransactionIDs`
- `lastTransactionID`

Why:

- OANDA order lifecycle is transaction-centric
- these fields make restart/reconciliation logic materially cleaner

Constraint:

- do not rely on `clientExtensions` for MT4-linked accounts where OANDA warns against modifying them

## Rate limiting handling

Recommended design:

- keep one limiter for REST request volume
- keep separate guards for streaming connection count and reconnect frequency
- prefer persistent HTTP sessions over bursty short-lived requests

Operational notes from OANDA docs:

- polling and stream quotas are distinct concerns
- keep the number of active streams bounded
- use best-practice polling/update flows rather than repeatedly reloading full account state

## Reconnect handling

Recommended design:

- maintain a local account snapshot
- resume from the last seen transaction/account watermark
- use poll/update endpoints to converge account state after reconnect

Recommended sequence:

1. reconnect HTTP/stream clients
2. rebuild the local streaming subscriptions
3. call account update/poll endpoint from the last seen transaction id
4. if state is uncertain, reload authoritative account/orders/positions snapshot
5. resume normal live processing

This should be the default recovery path even if the transaction or pricing stream appears healthy
again quickly.

## References

- OANDA v20 development guide: https://developer.oanda.com/rest-live-v20/development-guide/
- OANDA best practices: https://developer.oanda.com/rest-live-v20/best-practices/
- OANDA order endpoint: https://developer.oanda.com/rest-live-v20/order-ep/
- OANDA order definitions: https://developer.oanda.com/rest-live-v20/order-df/
- OANDA `v20` Python bindings: https://github.com/oanda/v20-python

## Runtime ownership split

Service hosts own:

- bootstrap
- registration
- generic publication/query contracts
- supervision

OANDA Python modules own:

- REST and streaming API behavior
- environment-specific configuration
- mapping quirks for orders, trades, and instruments

## Initial rollout order

1. Port the existing OANDA refdata loader into `venue-integrations/oanda/oanda/refdata.py` and wire it through the manifest-driven refdata host path.
2. Implement Python OANDA gateway adaptor with query-after-action and periodic recovery.
3. Implement Python OANDA RTMD adaptor with tick and kline support.
4. Add richer order-type coverage only after the core lifecycle path is stable.

## Main risks

- over-trusting streaming feeds that are not semantically authoritative enough for OMS
- leaking OANDA trade-position semantics directly into generic OMS assumptions
- embedding environment-specific behavior in the host instead of the adaptor

## Recommended tests

- place/cancel flows synthesize the correct follow-up facts after query-after-action reconciliation
- reconnect runs authoritative account/position queries before resuming normal publication
- refdata refresh migrates existing OANDA symbol normalization into canonical `instrument_id`
- RTMD lease dedupe avoids duplicate upstream pricing subscriptions
