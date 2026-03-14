# Market Data Gateway Service

## Scope

`zk-rtmd-gw-svc` is the realtime market data gateway runtime.

## Design Role

It wraps venue SDK subscriptions and publishes normalized RTMD events.

Responsibilities:

- subscribe to venue market data channels
- normalize tick, kline, funding, and orderbook payloads
- expose a query API for current snapshots and bounded history
- reconcile venue subscriptions against live subscription interest plus Pilot-managed policy/defaults
- register an `mdgw` role in KV when acting as a shared RTMD publisher

## Registration

RTMD gateways use the generic registry contract. The service-specific meaning is carried in Pilot
metadata and registration payload metadata.

Typical registration fields:

- `service_type = "mdgw"`
- `service_id = <logical_id>`
- `venue`
- `capabilities = ["tick", "kline", "funding", "orderbook", "query_current", "query_history"]`
- query gRPC transport endpoint in the generic transport field
- `metadata.registration_kind = "mdgw" | "engine+mdgw" | "aio+mdgw"`
- `metadata.publisher_mode = "standalone" | "embedded"`
- `metadata.subscription_scope = "global" | "strategy_scoped" | "logical_instance_scoped"`
- `metadata.query_types = ["current_tick", "current_orderbook", "current_funding", "kline_history"]`

This allows consumers to discover not only that an RTMD gateway is live, but also:

- whether it serves query RPCs
- which query types it supports
- which transport endpoint to call

## Dynamic Subscription Model

Pilot should not be the steady-state hub for RTMD subscription changes.

The runtime model should be split into:

- live subscription interest: direct, fast-moving, runtime-path state
- Pilot policy/defaults: control-plane state for bootstrap, override, inspection, and governance

### Live Subscription Interest

Clients that need RTMD should publish or refresh their subscription interest directly through NATS,
not through synchronous Pilot mediation.

Recommended shape:

- per-client or per-session subscription lease records in a dedicated RTMD subscription KV bucket
- recommended bucket: `zk.rtmd.subs.v1`
- each interest record carries:
  - subscriber identity
  - venue
  - instrument
  - channel type
  - optional interval/depth parameters
  - lease/refresh expiry

This KV space should be separate from the service registry bucket:

- `zk.svc.registry.v1` for service discovery and liveness
- `zk.rtmd.subs.v1` for dynamic RTMD subscription leases

Suggested key shape:

- `sub.<scope>.<subscriber_id>.<subscription_id>`

Where scope is typically:

- `venue.<venue>`
- `logical.<logical_id>`

The wire/runtime contract for these leases is documented in
[rtmd_subscription_protocol.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rtmd_subscription_protocol.md).

The RTMD gateway watches this live interest set and:

1. groups leases by effective stream key
2. builds the effective live union for its scope
3. resolves or validates transport-facing `(venue, instrument_exch)` fields from refdata as needed
4. diffs it against the active venue SDK subscriptions
5. applies subscribe/unsubscribe quickly
6. expires abandoned interest by lease timeout

This keeps live subscription pickup fast when a client starts watching a new symbol.

### Multiple Subscribers And Upstream Deduplication

The RTMD gateway should not create one upstream venue subscription per client lease.

Instead it should aggregate leases by effective stream key, typically:

- `instrument_id`
- `channel_type`
- channel parameters such as `interval`, `depth`, or mode
- scope

Rules:

- one or many downstream client leases may map to one upstream venue subscription
- upstream subscribe happens on refcount transition `0 -> 1`
- upstream unsubscribe happens on refcount transition `1 -> 0`
- the gateway continues publishing one normalized NATS stream per effective stream key

This means ten clients subscribing to the same tick stream still result in one venue tick feed and
ten NATS consumers, not ten venue subscriptions.

### Pilot Role

Pilot remains important, but as a control-plane authority and observer:

- bootstrap and topology authority
- optional default subscription policy
- admission / override / disable controls
- global monitoring and inspection of current RTMD topology

Pilot may watch the same `zk.rtmd.subs.v1` bucket and derive a global view,
but the RTMD gateway must not depend on Pilot to notice each runtime subscription change.

### Effective Subscription Rule

The RTMD gateway computes its effective subscription set from:

1. live subscription interest for its scope
2. Pilot-managed policy/default rows
3. local runtime capability constraints

Recommended semantics:

- live subscription interest drives fast adds/removes
- Pilot policy may force allow/deny/default behavior
- standalone RTMD gateways usually aggregate a venue-scoped union
- embedded RTMD runtimes usually aggregate a logical-instance-scoped union
- loss of Pilot should not prevent already-running RTMD gateways from adapting to new live subscription interest

## Shared Runtime Components

The RTMD runtime should be factored into reusable components:

- venue adapter
- subscription manager
- publisher
- registry integration
- control/reload loop

These components are shared by standalone RTMD gateways and embedded AIO runtimes.

Suggested split:

```text
rtmd-runtime/
  adapter.rs / adapter.py
  subscription_manager.rs / subscription_manager.py
  publisher.rs / publisher.py
  query.rs / query.py
  registry.rs / registry.py
  control.rs / control.py
```

Rust trait sketch:

```rust
#[async_trait]
pub trait RtmdVenueAdapter: Send + Sync {
    async fn connect(&self) -> Result<()>;
    async fn subscribe(&self, req: RtmdSubscriptionSpec) -> Result<()>;
    async fn unsubscribe(&self, req: RtmdSubscriptionSpec) -> Result<()>;
    async fn snapshot_active(&self) -> Result<Vec<RtmdSubscriptionSpec>>;
    async fn query_current_tick(&self, instrument_id: &str) -> Result<TickData>;
    async fn query_current_orderbook(&self, instrument_id: &str, depth: Option<u32>) -> Result<OrderBook>;
    async fn query_current_funding(&self, instrument_id: &str) -> Result<FundingRate>;
    async fn query_klines(
        &self,
        instrument_id: &str,
        interval: &str,
        limit: u32,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<Vec<Kline>>;
}
```

Python implementations should follow the same logical contract even if the concrete async API differs.

`SubscriptionManager` should therefore merge two inputs:

- a fast live-interest watcher
- a slower Pilot policy/default loader

## Query API

The RTMD gateway should expose a gRPC query service for cases where a caller needs current
snapshots or bounded recent history instead of only subscribing to streams.

Recommended query categories:

- current snapshot
  - latest tick
  - current orderbook
  - current funding rate
- bounded recent history
  - klines for a requested interval and limit/window

Query scope is intentionally limited:

- current snapshots should come from the venue side, not from a semantically authoritative gateway cache
- bounded recent history should come from venue-side query capability when supported
- this is not a general historical market-data warehouse

Semantic rule:

- the RTMD gateway may keep transient runtime state needed to operate adapters and publish streams
- but it should not be treated as the semantic source of truth for current or historical market data
- query correctness should be defined by venue-backed data, not by gateway-local cache semantics

The gRPC endpoint and supported query types should be discoverable through the service registration
payload so clients can resolve:

- whether an `mdgw` supports queries at all
- which query families are available
- which endpoint to dial

## Fanout And Backpressure

Different RTMD channel families need different transport behavior.

Decisions:

- `tick` and `orderbook`
  - publish on plain NATS subjects
  - no JetStream durability requirement
  - if consumers are slow, events may be dropped
- `kline`
  - publish through JetStream-backed flow
  - intended to support replay/recovery and bounded history workflows

Adaptor-specific behavior:

- exact orderbook semantics, upstream batching, and venue-side recovery behavior remain adaptor-specific
- rate limiting and upstream subscription throttling are handled per adaptor implementation

## Adaptor-Specific Semantics

The following are intentionally adaptor-specific rather than globally fixed in this document:

- orderbook snapshot vs delta behavior
- venue-specific history query support
- upstream rate-limit strategy
- venue field normalization edge cases

The shared RTMD gateway contract should expose capabilities and common control flow, while each
adaptor documents venue-specific behavior where needed.

## Startup Model

Standalone RTMD gateway startup:

1. load runtime config and connect to NATS + Pilot
2. claim the RTMD logical instance from Pilot
3. receive registration metadata/profile and initial policy/defaults
4. initialize the venue adapter
5. start the RTMD gRPC query server
6. register `svc.mdgw.<logical_id>` in KV with query capabilities and transport endpoint
7. start the live-interest watcher plus policy/default watcher
8. reconcile subscriptions from live interest + policy/defaults
9. start publisher + heartbeat supervision

Pilot may be unavailable later without blocking runtime subscription convergence, as long as the
live-interest path remains healthy.

## Embedded Mode

An AIO engine may embed the RTMD gateway runtime.

If it publishes shared RTMD topics for other services, it should also expose an `mdgw` KV
registration. If it is local-only, Pilot may keep it engine-internal.

## High Availability

RTMD HA should be staged rather than forced into the first implementation.

### Phase 1: Single Active Publisher

Initial goal:

- one active RTMD gateway publisher per scope
- fast pickup of live interest changes
- correct lease expiry and upstream deduplication

Failure behavior:

- if the RTMD gateway fails, the stream pauses until a replacement starts
- subscribers keep their `zk.rtmd.subs.v1` leases alive
- the replacement gateway rebuilds the effective interest set from KV and resubscribes upstream

This is the minimum correct model.

### Phase 2: Hot Standby / Fast Recovery

Recommended next goal:

- allow a standby RTMD gateway for the same scope
- keep only one active publisher at a time via Pilot bootstrap + KV ownership
- standby watches live interest and is ready to take over quickly

Takeover model:

- active gateway owns `svc.mdgw.<logical_id>`
- standby does not publish RTMD payloads while passive
- on active loss, standby claims ownership, rebuilds upstream subscriptions from `zk.rtmd.subs.v1`, and starts publishing

This improves recovery time without introducing duplicate publishers in normal operation.

### Phase 3: Redundant Ingestion / Publisher Fencing

Only after Phase 2 is stable:

- support dual venue connections or multi-instance ingestion for the same scope
- require strict publisher fencing so only one normalized NATS publisher is active per stream key
- optionally add sequence/epoch metadata if consumers need to detect publisher turnover

Important constraint:

- do not allow two active publishers to emit the same normalized RTMD subject without an explicit dedup/fencing design
- HA should preserve one logical publisher per `zk.rtmd.*` subject, even if multiple instances are connected upstream

Recommended rollout:

- Phase 1 in initial RTMD gateway delivery
- Phase 2 as the first HA milestone
- Phase 3 only if recovery time or venue-connectivity requirements justify the added complexity

## TODO Checklist

- [ ] define RTMD health information model and how gateway health/degradation is published to Pilot/ops
- [ ] decide scope partitioning strategy for large venue deployments as part of the HA/scaling design
- [ ] document refdata-change handling in the refdata service design and link it back here
- [ ] add adaptor-specific notes for venues that need special orderbook semantics or query limitations

## Related Docs

- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [RTMD Subscription Protocol](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rtmd_subscription_protocol.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
