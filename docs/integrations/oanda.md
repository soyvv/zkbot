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
  python/
    gw.py
    rtmd.py
    refdata.py
  schemas/
    gw_config.schema.json
    rtmd_config.schema.json
    refdata_config.schema.json
```

Manifest language choices:

- `gw.language = python`
- `rtmd.language = python`
- `refdata.language = python`

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

### Refdata API mapping

Recommended initial mapping:

- account instruments endpoint for tradable instrument metadata
- instrument definitions used to populate canonical trading params

The refdata loader should migrate the existing older OANDA fetcher logic into the `zk-refdata-svc`
runtime described in
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

1. Port the existing OANDA refdata loader into the new `zk-refdata-svc` shape.
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
