# RTMD Subscription Protocol

This protocol is independent from service discovery.

Purpose:

- let runtime clients express market-data interest quickly
- let RTMD gateways derive effective venue subscriptions
- let SDKs deterministically map market-data interests to NATS subjects
- let Pilot observe and optionally publish its own interest without becoming the runtime hub

## Separation From Service Discovery

- `zk-svc-registry-v1` is for service discovery and liveness
- `zk-rtmd-subs-v1` is for dynamic RTMD subscription leases

The RTMD subscription protocol does not resolve service endpoints. It only expresses market-data
interest and maps that interest to deterministic RTMD topics.

## Interest Model

A client expresses interest in a normalized RTMD stream such as:

- tick
- kline
- funding
- orderbook

Client-facing interest should be expressed in ZK symbol conventions, because the trading client is
already expected to maintain or query instrument refdata.

Preferred identity:

- `instrument_id` as the primary symbol key

Venue-native fields such as `venue` and `instrument_exch` are resolved from refdata and used as
transport-facing details, not as the primary client API.

Each interest record should contain:

- `subscriber_id`
- `scope`
- `instrument_id`
- `channel_type`
- optional channel parameters such as `interval` or `depth`
- resolved transport fields:
  - `venue`
  - `instrument_exch`
- `lease_expiry_ms`
- `updated_at_ms`

Suggested KV bucket:

- `zk-rtmd-subs-v1`

Suggested KV key shape:

- `sub.<scope>.<subscriber_id>.<subscription_id>`

Examples:

- `sub.venue.OKX.strategy_a.tick_btcusdt`
- `sub.logical.engine_strat_1.strategy_a.kline_btcusdt_1m`

## Multiple Subscribers For One Instrument

Many clients may express interest in the same logical market-data stream.

The protocol should treat each lease as an independent subscriber record, while the RTMD gateway
derives an aggregated effective stream set.

Aggregation key:

- `(scope, instrument_id, channel_type, channel_params)`

Examples:

- two tick subscribers for the same `instrument_id` collapse to one upstream venue tick subscription
- three `1m` kline subscribers collapse to one upstream `1m` kline subscription
- `1m` and `5m` kline interests remain separate aggregation keys
- orderbook interests with different depth or mode parameters remain separate aggregation keys

Reference-count rule:

- effective upstream subscription exists while at least one unexpired lease remains for the aggregation key
- upstream unsubscription happens only when the last lease for that key expires or is removed
- one subscriber, including Pilot, must not directly cancel another subscriber's lease

This gives:

- efficient upstream venue usage
- fast fanout to many internal clients
- no duplicate venue subscription churn for the same stream

## Subject Mapping

Subject mapping is deterministic and does not require Pilot or service discovery lookup.
It does require refdata resolution from `instrument_id` to venue-native transport keys.

Canonical mapping:

- `tick(venue, instrument_exch)` -> `zk.rtmd.tick.<venue>.<instrument_exch>`
- `kline(venue, instrument_exch, interval)` -> `zk.rtmd.kline.<venue>.<instrument_exch>.<interval>`
- `funding(venue, instrument_exch)` -> `zk.rtmd.funding.<venue>.<instrument_exch>`
- `orderbook(venue, instrument_exch)` -> `zk.rtmd.orderbook.<venue>.<instrument_exch>`

This gives the SDK a direct path:

1. build an RTMD interest spec using `instrument_id`
2. resolve `instrument_id -> (venue, instrument_exch)` from refdata
3. publish or refresh the lease in `zk-rtmd-subs-v1`
4. subscribe to the deterministic NATS subject for delivery

Recommended helper model:

- external/client API uses `instrument_id`
- internal subject-builder API uses resolved `(venue, instrument_exch[, interval])`

## SDK Responsibilities

`TradingClient` or a sibling RTMD client helper should provide:

- typed RTMD interest builders
- subject-builder helpers
- lease create/refresh/drop helpers for `zk-rtmd-subs-v1`
- direct NATS subscribe helpers for the mapped subject

Suggested helper surface:

```rust
rtmd_tick_interest(instrument_id) -> RtmdInterestSpec
rtmd_kline_interest(instrument_id, interval) -> RtmdInterestSpec
rtmd_funding_interest(instrument_id) -> RtmdInterestSpec
rtmd_orderbook_interest(instrument_id) -> RtmdInterestSpec

rtmd_tick_subject(venue, instrument_exch) -> String
rtmd_kline_subject(venue, instrument_exch, interval) -> String
rtmd_funding_subject(venue, instrument_exch) -> String
rtmd_orderbook_subject(venue, instrument_exch) -> String

register_rtmd_interest(spec: RtmdInterestSpec) -> Result<RtmdInterestLease>
refresh_rtmd_interest(lease: &RtmdInterestLease) -> Result<()>
drop_rtmd_interest(lease: RtmdInterestLease) -> Result<()>
```

## RTMD Gateway Responsibilities

The RTMD gateway watches `zk-rtmd-subs-v1` for its scope and:

1. groups active leases by aggregation key
2. builds the live union of effective stream interests
3. maps resolved transport fields to venue SDK subscriptions
4. subscribes/unsubscribes upstream quickly based on refcount transitions
5. publishes normalized RTMD payloads on the deterministic subjects

Pilot is not required for these steady-state runtime updates.

## Pilot Responsibilities

Pilot may:

- observe `zk-rtmd-subs-v1`
- expose topology/debug APIs over current RTMD interest
- publish and refresh Pilot-owned leases for baseline subscriptions
- remove Pilot-owned leases when that baseline interest is no longer desired
- trigger control reloads when related config changes

Pilot should not be required for every runtime interest add/remove.

Normal-source rule:

- Pilot is a normal lease producer when it wants RTMD baseline subscriptions
- Pilot may not directly delete another subscriber's live lease
- effective unsubscribe occurs only when the aggregated lease set for that stream reaches zero

## Sequence Diagrams

### 1. Client subscribes to a new RTMD stream

```mermaid
sequenceDiagram
    participant TC as TradingClient
    participant RF as Refdata cache
    participant SUBKV as zk-rtmd-subs-v1
    participant MDGW as RTMD gateway
    participant VENUE as Venue SDK / exchange
    participant NATS as NATS subjects

    TC->>RF: resolve(instrument_id)
    RF-->>TC: venue + instrument_exch
    TC->>SUBKV: put sub.<scope>.<subscriber>.<subscription_id>
    Note over TC,SUBKV: lease contains instrument_id, channel_type, params, resolved transport fields
    TC->>NATS: subscribe zk.rtmd.<channel>.<venue>.<instrument_exch>[.<interval>]
    SUBKV-->>MDGW: watch event for new lease
    MDGW-->>MDGW: aggregate by effective stream key
    alt refcount 0 -> 1
        MDGW->>VENUE: subscribe upstream stream
    end
    VENUE-->>MDGW: market data event
    MDGW->>NATS: publish normalized zk.rtmd.* payload
    NATS-->>TC: Tick/Kline/Funding/OrderBook
```

### 2. Multiple subscribers for the same stream

```mermaid
sequenceDiagram
    participant C1 as Client A
    participant C2 as Client B
    participant SUBKV as zk-rtmd-subs-v1
    participant MDGW as RTMD gateway
    participant VENUE as Venue SDK / exchange

    C1->>SUBKV: put lease for tick(BTC-USDT)
    SUBKV-->>MDGW: lease added
    MDGW-->>MDGW: refcount=1 for tick(BTC-USDT)
    MDGW->>VENUE: subscribe tick(BTC-USDT)

    C2->>SUBKV: put lease for tick(BTC-USDT)
    SUBKV-->>MDGW: lease added
    MDGW-->>MDGW: refcount=2 for tick(BTC-USDT)
    Note over MDGW,VENUE: no second upstream subscribe

    C1->>SUBKV: delete or let lease expire
    SUBKV-->>MDGW: lease removed
    MDGW-->>MDGW: refcount=1 for tick(BTC-USDT)
    Note over MDGW,VENUE: keep upstream subscription active

    C2->>SUBKV: delete or let lease expire
    SUBKV-->>MDGW: lease removed
    MDGW-->>MDGW: refcount=0 for tick(BTC-USDT)
    MDGW->>VENUE: unsubscribe tick(BTC-USDT)
```

### 3. Gateway restart rebuilds live interest

```mermaid
sequenceDiagram
    participant TC as TradingClient
    participant SUBKV as zk-rtmd-subs-v1
    participant MDGW1 as Old RTMD gateway
    participant MDGW2 as Restarted RTMD gateway
    participant VENUE as Venue SDK / exchange
    participant NATS as NATS subjects

    TC->>SUBKV: refresh active leases
    MDGW1->>VENUE: maintain upstream subscriptions
    Note over MDGW1: crash / restart

    MDGW2->>SUBKV: watch current snapshot
    SUBKV-->>MDGW2: active leases for scope
    MDGW2-->>MDGW2: rebuild effective stream set
    MDGW2->>VENUE: resubscribe upstream streams
    VENUE-->>MDGW2: market data event
    MDGW2->>NATS: publish normalized zk.rtmd.* payload
    NATS-->>TC: stream resumes
```

### 4. Pilot observes but does not mediate the hot path

```mermaid
sequenceDiagram
    participant TC as TradingClient
    participant SUBKV as zk-rtmd-subs-v1
    participant MDGW as RTMD gateway
    participant P as Pilot

    TC->>SUBKV: put or refresh RTMD lease
    SUBKV-->>MDGW: watch lease change
    MDGW-->>MDGW: update effective stream set
    SUBKV-->>P: watch lease change
    P-->>P: update topology / policy view
    Note over TC,MDGW: runtime subscription convergence does not wait for Pilot
```

### 5. Later-phase hot standby takeover

```mermaid
sequenceDiagram
    participant SUBKV as zk-rtmd-subs-v1
    participant A as Active RTMD gateway
    participant S as Standby RTMD gateway
    participant REG as zk-svc-registry-v1
    participant VENUE as Venue SDK / exchange
    participant NATS as NATS subjects

    A->>REG: own svc.mdgw.<logical_id>
    S->>SUBKV: watch active leases
    A->>SUBKV: watch active leases
    A->>VENUE: subscribe effective stream set
    A->>NATS: publish zk.rtmd.* payloads

    Note over A: fails
    REG-->>S: active owner gone
    S->>REG: claim svc.mdgw.<logical_id>
    S->>SUBKV: rebuild current active leases
    S->>VENUE: subscribe effective stream set
    S->>NATS: resume publishing zk.rtmd.* payloads
    Note over A,S: only one active publisher should exist at a time
```

## Related Docs

- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [SDK](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
