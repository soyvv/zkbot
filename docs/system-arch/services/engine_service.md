# Engine Service

## Scope

`zk-engine-svc` executes one strategy runtime and interacts with OMS, RTMD feeds, and Pilot.

## Design Role

The engine is a data-plane runtime with control-plane startup and ownership rules:

- runtime config comes from Pilot
- `execution_id` identifies one concrete run of a strategy
- only one active execution is allowed per `strategy_key`
- registration in KV uses a stable logical engine key, not `execution_id`
- `TradingClient` is the OMS/RTMD runtime dependency
- `RefdataSdk` is the refdata and market-status dependency used by the engine through `TradingClient`

The bootstrap flow should follow the shared contract in
[Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md).

## Runtime Composition

`zk-engine-svc` should be a thin runtime wrapper around:

- Pilot execution claim and runtime config
- `TradingClient`
- `RefdataSdk`
- `zk-engine-rs` strategy event-loop/domain logic
- optional embedded Python strategy runtime
- optional embedded RTMD publisher runtime

Recommended composition:

```text
Pilot claim/config
      |
      +-> execution_id
      +-> strategy config
      +-> registration grant
      |
      v
TradingClient ----> RefdataSdk ----> Refdata Service
      |
      +-> OMS connectivity
      +-> RTMD stream access
      |
      v
zk-engine-rs event loop
```

Design rule:

- the engine should stay relatively thin
- infra behavior should remain in Pilot, discovery, `TradingClient`, and `RefdataSdk`
- engine-specific logic should focus on lifecycle, supervision, and strategy event dispatch

## Engine-Svc Responsibilities

`zk-engine-rs` and `zk-engine-svc` should have a clear split.

`zk-engine-rs` should remain a library crate responsible for:

- the strategy event loop
- event envelope and trigger-context handling
- tick coalescing and event dispatch
- timer advancement logic
- strategy callback execution
- action dispatch interfaces
- hot-path latency tracking
- runtime snapshot data structures and read replica

`zk-engine-svc` should be the production process/runtime responsible for:

- bootstrap from minimal local config
- execution claim and runtime-config fetch from Pilot
- ownership fencing and supervision policy
- service discovery registration and deregistration
- `TradingClient` construction and connectivity management
- OMS/RTMD subscription setup and reconnect handling
- OMS-based state rehydration on startup and reconnect
- concrete `ActionDispatcher` implementation for OMS requests
- control-plane ingress such as pause/resume/stop
- query gRPC service backed by the engine read replica
- metrics/tracing export and lifecycle/health publication

Design consequence:

- infrastructure integration and process lifecycle should not be pushed down into `zk-engine-rs`
- strategy/runtime semantics should not be reimplemented in `zk-engine-svc`
- `zk-engine-svc` should mostly translate external APIs and streams into `EventEnvelope`s and feed
  them into `zk-engine-rs`

Suggested `zk-engine-svc` module layout:

- `config.rs`
- `bootstrap.rs`
- `runtime.rs`
- `subscriptions.rs`
- `rehydration.rs`
- `dispatcher.rs`
- `control_api.rs`
- `query_api.rs`
- `supervision.rs`
- `main.rs`

Suggested runtime startup flow:

1. load minimal deployment config
2. connect to Pilot and claim one execution
3. receive authoritative runtime config and ownership metadata
4. initialize `TradingClient`
5. query OMS live state and build startup state
6. construct strategy runtime and `LiveEngine`
7. wire concrete `ActionDispatcher`, timer task, control ingress, and query service
8. register the engine instance in discovery/KV
9. start feed subscriptions and enter the supervised run loop

Suggested reconnect flow:

1. mark the engine `DEGRADED` or `RECONNECTING`
2. pause normal market-event delivery
3. re-establish transport sessions and subscriptions
4. query OMS live state again and rebuild strategy-facing runtime state
5. refresh the read replica / health state
6. resume event delivery

## Strategy Runtime Integration

Recommended default:

- support Rust-native strategies directly in `zk-engine-rs`
- support Python strategies by embedding CPython through `pyo3`, similar to the backtester path
- avoid a separate Python worker process as the default architecture

Rationale:

- this class of strategy is usually not the most latency-critical path compared with OMS and market-data
  transport
- embedding Python keeps one engine process, one ownership model, and one in-memory state model
- the Python worker design adds avoidable complexity around RPC boundaries, lifecycle supervision,
  and duplicated state copies between Rust and Python

Guidance:

- keep the canonical event queue, timer scheduler, lifecycle state, and broker/OMS integration in Rust
- call Python strategy handlers synchronously from the engine dispatch path through `pyo3`
- keep strategy-visible state as a single logical state model owned by the engine runtime, with Python
  receiving handles/views instead of maintaining a second canonical copy
- revisit a separate Python worker only if isolation, dependency management, or GIL contention becomes
  a demonstrated production issue

## Config Model

The engine should follow the common config split:

- minimal deployment config
  - enough to reach Pilot/NATS and identify the logical instance
- Pilot-enriched runtime config
  - strategy runtime config
  - execution metadata
  - account/symbol bindings
  - registration grants
  - embedded RTMD profile if applicable

The engine should not treat local env/file settings as the authoritative strategy runtime config.

Manifest/config rule:

- Pilot should validate desired engine/bot config against a bot/engine manifest/schema contract
- that contract should define runtime config shape, capability flags, and reloadable vs
  restart-required fields
- the engine should apply the effective config received from Pilot rather than treating local
  runtime files/env as the source of truth

### Engine And Strategy Manifest Model

The engine config model should mirror the gateway pattern:

- one service-kind manifest/schema for the engine runtime itself
- one strategy manifest/schema per strategy family selected by `strategy_type_key`

This keeps engine hosting concerns separate from strategy behavior concerns.

#### 1. Engine Service-Kind Manifest

The engine service manifest should define only engine-owned `provided_config`.

Examples:

- target OMS binding metadata when that is part of engine provided-config
- connected account IDs used by `TradingClient`
- interested instruments used for RTMD subscription setup
- runtime mode
- supervision mode
- timer defaults that belong to the engine host
- logging and observability controls
- embedded RTMD host flags
- restart-required vs reloadable classification

It should not be the place for:

- strategy-specific parameters
- strategy-family schema selection rules
- Python strategy payload details beyond the generic wrapper contract

Design rule:

- `service-manifests/engine/manifest.yaml` and its JSON Schema are the authoritative contract for
  engine-owned config
- Pilot should load and validate bot `provided_config` against this service-kind schema the same way
  it does for gateway config loading

#### 2. Strategy Manifest Registry

Each selectable strategy should have its own manifest/schema contract keyed by
`strategy_type_key`.

Recommended manifest contents:

- `strategy_type_key`
- `runtime_kind`
  - `rust_native`
  - `python_wrapper`
- human metadata such as label, description, and tags
- config schema location
- optional defaults
- capability flags
- optional refdata / RTMD subscription hints
- compatibility metadata such as required engine features

Design rule:

- strategy authoring should be driven by the selected strategy manifest/schema, not by one generic
  free-form JSON blob for all strategies
- Pilot should validate strategy config against the schema for the selected `strategy_type_key`

#### 3. Rust-Native Strategy Manifests

For Rust-native strategies such as market making, arbitrage, or smoke-test:

- `strategy_type_key` selects the strategy family
- the manifest identifies the Rust strategy implementation
- the strategy schema defines the strategy's own config payload

Examples of strategy-owned fields:

- quoting widths
- inventory controls
- trigger thresholds
- sampling windows
- research/model flags
- logical names such as `mm_main_accnt` or `hedge_symbol`

Logical-name rule:

- strategy config may refer to logical account or symbol names meaningful to the strategy
- the engine service config must carry the concrete connected accounts and interested instruments
  used to satisfy those logical references
- operator validation should ensure the selected engine config can satisfy the strategy's logical
  requirements

#### 4. Python Wrapper Strategy Manifest

The system should also support one umbrella Python-wrapping strategy family.

Recommended behavior:

- from Pilot's perspective, it is just another selectable `strategy_type_key`
- its manifest declares `runtime_kind = python_wrapper`
- the engine still hosts it through the normal engine lifecycle
- the wrapper config includes Python runtime location metadata plus one JSON payload for the wrapped
  Python strategy config

Recommended wrapper shape:

```json
{
  "python_module": "my_pkg.my_strategy",
  "python_class": "MyStrategy",
  "python_search_path": "/opt/zkbot/strategies",
  "python_strategy_config": {
    "lookback": 20,
    "threshold_bps": 5
  }
}
```

Design rule:

- the wrapper fields are generic and belong to the Python-wrapper strategy manifest
- `python_strategy_config` is a nested JSON object owned by the wrapped Python strategy contract
- Pilot should validate both:
  - the outer wrapper schema
  - the nested Python strategy config schema when that schema is available

#### 5. Effective Runtime Config Assembly

The engine should assemble runtime config from two manifest-governed inputs:

- engine host config from the engine service-kind schema
- strategy config from the selected strategy manifest/schema

Recommended effective shape:

```text
runtime_config
  = bootstrap_config
  + engine_provided_config
  + strategy_definition
  + execution metadata
  + runtime-derived fields
```

Where:

- `engine_provided_config` is validated by the engine service-kind schema
- `strategy_definition.config_json` is validated by the strategy manifest/schema for
  `strategy_type_key`
- concrete engine connectivity such as connected accounts and interested instruments belongs to the
  engine service-kind config
- logical aliases used by the strategy belong to the strategy config
- the engine runtime applies both, but does not collapse them into one authoring contract

#### 6. Pilot Authoring Flow

Pilot should resolve two schema layers during bot onboarding:

1. engine schema from the engine service manifest
2. strategy schema from the selected `strategy_type_key` manifest

Operator workflow:

- strategy authoring uses the strategy manifest/schema
- bot onboarding uses the engine service-kind manifest/schema
- start validation checks the combined runtime feasibility

This is the engine-side equivalent of how gateway onboarding loads shared gateway host config from
the service-kind schema and venue-specific config from the venue capability manifest.

Runtime introspection rule:

- the engine should expose a default `GetCurrentConfig` style query
- the response should return the normalized effective runtime config currently loaded by the
  execution process, plus metadata such as `loaded_at` and config revision if available
- Pilot uses that response to compare desired vs live config and to classify drift before reload or
  restart
- raw secret values must not be returned

#### 7. Schema And Dynamic Option Metadata

JSON Schema should remain the base contract for:

- field types
- required/default values
- range and pattern validation
- nested object structure

But JSON Schema alone is not enough for operator-facing engine and strategy forms because many
fields depend on live Pilot metadata.

Examples:

- `target_oms_id` should come from the current OMS list
- `account_ids` should come from accounts reachable under the selected OMS / gateway scope
- `instruments` should come from live refdata or OMS-visible instrument scope
- `strategy_type_key` should come from the synced strategy manifest registry
- logical strategy fields such as `mm_main_accnt` may need a resolved account-option list, not a
  free-text box

Design rule:

- authoring should use `JSON Schema + field_descriptors + Pilot metadata APIs`
- schema defines the shape
- field descriptors define how the UI should source and present values
- Pilot metadata APIs provide the dynamic option lists

Recommended manifest extension:

```yaml
field_descriptors:
  - path: /strategy_type_key
    reloadable: false
    ui:
      widget: select
      options_source: meta.strategy_types
      value_key: value
      label_key: label

  - path: /target_oms_id
    reloadable: false
    ui:
      widget: select
      options_source: meta.oms
      value_key: value
      label_key: label

  - path: /account_ids
    reloadable: false
    ui:
      widget: multi_select
      options_source: topology.oms_accounts
      depends_on:
        - /target_oms_id
      value_key: account_id
      label_key: display_name

  - path: /instruments
    reloadable: false
    ui:
      widget: multi_select
      options_source: topology.oms_instruments
      depends_on:
        - /target_oms_id
        - /account_ids
      value_key: instrument_id
      label_key: instrument_id
```

Recommended `ui` descriptor fields:

- `widget`
  - `select`
  - `multi_select`
  - `autocomplete`
  - `json_editor`
  - `hidden`
- `options_source`
  - identifies which Pilot metadata source to query
- `depends_on`
  - one or more config paths that must be resolved first
- `value_key`
  - field name in each option object used as the stored value
- `label_key`
  - field name used for operator-facing display
- `placeholder`
- `help_text`
- `order`

Metadata source model:

- `meta.*`
  - broad dropdown-ready lists returned by `/v1/meta`
  - examples: OMS list, strategy-type list, runtime kinds
- `topology.*`
  - scoped option lists that depend on a selected OMS, gateway, account, or venue
  - examples: accounts under one OMS, instruments available for one runtime scope
- `strategy.*`
  - optional strategy-family helper lookups if a strategy manifest needs special logical aliases

Recommended source examples:

- `meta.oms`
- `meta.strategy_types`
- `topology.oms_accounts`
- `topology.oms_instruments`
- `topology.gateway_symbols`

Dependency rule:

- the UI should resolve option sources only after all declared `depends_on` fields are populated
- when an upstream dependency changes, dependent fields should be cleared or revalidated
- Pilot should still perform authoritative backend validation even if the UI uses filtered lists

Engine-form guidance:

- engine config should be as metadata-driven as possible
- `target_oms_id`, `account_ids`, and `instruments` should normally be rendered from dynamic option
  sources rather than free-form text
- `strategy_type_key` should usually be hidden or auto-filled from the selected strategy definition,
  not operator-entered separately during bot onboarding

Strategy-form guidance:

- strategy config should still validate through the selected strategy schema
- strategy fields that represent logical aliases may use dynamic selects when their valid choices
  can be derived from current Pilot metadata
- when a strategy field cannot be safely constrained from live metadata, the UI may fall back to a
  normal schema-driven text/JSON input

Fallback rule:

- schema-driven authoring should remain the default
- a schema-free/manual JSON path may still exist for advanced or bootstrap workflows
- even in manual mode, Pilot should preserve descriptor metadata when available so the UI can show
  what automation was skipped

## Startup Model

1. claim execution from Pilot
2. receive authoritative `execution_id`, strategy config, account/symbol bindings, and registration metadata
3. initialize `TradingClient`
4. use `TradingClient.refdata()` for instrument and market-status lookup as needed
5. initialize strategy runtime
6. register the engine in KV
7. run with ownership supervision

Registration guidance:

- keep the KV discovery contract generic
- include `strategy_key`, `execution_id`, and role/profile metadata in the payload
- do not encode strategy-specific ownership rules into the generic discovery client

Refdata guidance:

- the engine should not maintain a second canonical refdata cache beside `RefdataSdk`
- symbol resolution and market-session lookup should go through `TradingClient.refdata()`
- in the current design, strategy-facing refdata in `StrategyContext` can be treated as effectively
  static for the life of the strategy run
- this is acceptable for now because refdata changes are expected to be low-frequency control-plane
  events rather than hot-path market events
- future design should add a strategy callback such as
  `on_refdata_change(old_refdata, new_refdata, meta)` so strategies can react explicitly before the
  exposed strategy-side cache/view is updated
- once that hook exists, the engine/runtime should update the strategy-visible refdata cache only
  after the callback boundary, preserving deterministic strategy behavior around refdata transitions

The engine supports two supervision modes:

- `BEST_EFFORT`
- `CONTROLLED`

Ownership fencing is always fatal. Discovery-health degradation is fatal only in `CONTROLLED`
mode after a grace period.

## Strategy Control Semantics

Pause and resume should be defined per algorithm instance, not as a process-wide switch.

Rules:

- Pilot control commands target one `execution_id` / `strategy_key`
- pausing one algorithm must not implicitly pause other algorithms hosted by other engine processes
- pause/resume is a control-plane state transition, not a best-effort hint

Expected callback surface:

- add explicit strategy callbacks such as `on_pause(reason)` and `on_resume(reason)`
- the engine should stop delivering new market/timer/signal events to the paused strategy after the
  pause barrier is reached
- OMS/order-update events should still be processed while paused so the strategy state remains
  consistent with actual account state
- timer behavior while paused should be policy-driven; the default should be to suppress timer
  callbacks during pause and resume from current wall-clock time rather than replaying a backlog

Operational note:

- the engine should publish lifecycle/control state transitions such as `PAUSING`, `PAUSED`,
  `RESUMING`, and `RUNNING`
- if the strategy needs custom pause cleanup or resume revalidation, that logic belongs in the
  callback boundary rather than in Pilot

## Embedded RTMD Mode

An engine may host an embedded RTMD gateway runtime.

Rules:

- reuse the same RTMD runtime components as the standalone RTMD gateway
- register a separate `mdgw` role only if the embedded runtime publishes shared RTMD feeds
- keep private in-process RTMD behavior unregistered when it is not a shared publisher
- let Pilot metadata decide whether the runtime is engine-only, `engine+mdgw`, or a broader composite

The embedded RTMD path should not introduce a separate engine-specific discovery or subscription
protocol.

## Event Queue And Overload Policy

The current Python reference engine already applies one important overload rule:

- when the queue has multiple pending `TickData` items, keep only the latest tick per
  `(instrument_code, exchange)` and preserve non-tick events

That should remain the baseline policy in `zk-engine-rs`.

Rules:

- tick events are lossy under overload and may be coalesced to the latest value per instrument
- order updates, fills, balance/position updates, control commands, and lifecycle events should not be
  dropped
- timer events should generally not be dropped silently; if overload is severe, the engine should
  coalesce overdue timer firings into at most one immediate callback per timer key
- bar/kline events should follow the non-lossy path by default; unlike ticks, they should not be
  silently dropped during ordinary queue pressure

Recommended implementation:

- maintain event-type-aware queueing rather than a single blind FIFO
- allow a tick coalescing stage ahead of strategy dispatch
- preserve arrival order across non-tick event classes
- in `zk-engine-svc`, use a bounded grace-wait queue for non-lossy events before they reach the final
  engine event loop channel
- expose queue depth, tick-drop/coalesce counters, grace-queue depth, grace-queue overflows, and
  event lag as runtime metrics

Grace-wait queue policy:

- the final `LiveEngine` input channel may remain small and hot-path-oriented
- ticks should attempt direct enqueue to the final engine channel and may be dropped on full
- non-lossy events should first attempt direct enqueue; on failure they should be placed into a
  bounded grace-wait queue
- a dedicated forwarder task should drain the grace-wait queue into the final engine channel using
  backpressure-aware `send().await`
- this avoids unbounded per-event async task creation while preserving important events during short
  bursts
- if the grace-wait queue itself overflows, the engine should log loudly, increment an overflow
  counter, and surface a degraded reason; this should never be a silent loss path

Tradeoff:

- coalescing ticks means the strategy may miss intermediate microstructure states
- for this engine class that is acceptable when the alternative is unbounded queue growth and stale
  processing
- the grace-wait queue improves correctness for OMS/bar/control events at the cost of bounded extra
  latency under overload
- this is acceptable because those events are more correctness-sensitive than raw tick throughput

If overload persists beyond configured thresholds, the engine should mark itself `DEGRADED` and
surface the reason through health and query APIs.

## Query API

The engine should expose a query gRPC API for low-rate runtime introspection.

Primary use cases:

- current lifecycle state
- effective runtime config after Pilot enrichment
- execution metadata such as `strategy_key`, `execution_id`, account bindings, and symbol bindings
- queue depth, lag, and degraded reasons
- pause/resume state
- recent control-plane timestamps such as startup, last reconnect, and last successful OMS sync

Recommended default query:

- `GetCurrentConfig`
  - returns the effective runtime config currently loaded by the engine for Pilot drift inspection

Design rule:

- query handling must not read hot-loop mutable state through contended locks on every request

Recommended mechanism:

- maintain a read-optimized runtime snapshot object owned by a side task
- the hot loop publishes compact state deltas or metric updates into a lightweight channel
- the side task materializes a periodically refreshed snapshot, for example every 200 ms to 1 s
- the gRPC query service reads only that snapshot

This gives an eventually consistent query plane with bounded overhead. A delay of up to about one
second is acceptable for this API.

Snapshot contents should be descriptive rather than exhaustive:

- do not expose full hot-path event buffers
- prefer counters, timestamps, effective config, lifecycle state, and current strategy-visible state
  summary

## Latency Observability

The engine should capture hot-path latency metrics, especially for the tick-to-trade path.

Primary goal:

- provide an end-to-end latency breakdown that can be correlated with OMS-side metrics to explain
  where time was spent from market-data ingress to order submission/acknowledgement

Recommended timestamp boundaries in the engine:

- `md_recv_ts`: when the engine receives the tick from RTMD/`TradingClient`
- `queue_enqueued_ts`: when the tick is admitted to the strategy event queue
- `strategy_dispatch_ts`: when strategy processing for that tick starts
- `strategy_decision_ts`: when the strategy produces an order action
- `oms_submit_ts`: when the engine hands the order to `TradingClient` / OMS client

Current implementation mapping in `zk-engine-rs`:

- market-data ingress happens before `EngineEvent::Tick` is pushed into the engine channel
- queue drain and tick coalescing happen in
  [live_engine.rs](/Users/zzk/workspace/zklab/zkbot/rust/crates/zk-engine-rs/src/live_engine.rs)
  and [engine_event.rs](/Users/zzk/workspace/zklab/zkbot/rust/crates/zk-engine-rs/src/engine_event.rs)
- strategy dispatch starts in `LiveEngine::process_event()`
- order decision crosses into side effects in `LiveEngine::dispatch_actions()`
- OMS submission starts at the production `ActionDispatcher::place_order()` implementation boundary in
  [action_dispatcher.rs](/Users/zzk/workspace/zklab/zkbot/rust/crates/zk-engine-rs/src/action_dispatcher.rs)

From these timestamps the engine can derive:

- market-data ingress to queue latency
- queue wait latency
- strategy decision latency
- engine submit latency
- engine-local tick-to-submit latency

Correlation requirement:

- every order generated from a market-data-triggered decision should carry the timestamps and
  correlation metadata of the signal that actually triggered it
- this should apply to ticks, klines, realtime signals, and any future event type that can directly
  cause an order decision
- the order payload or attached order metadata should include at least the triggering event type,
  instrument, source timestamp if present, engine receive timestamp, strategy decision timestamp, and
  `execution_id`
- when the order enters OMS, these trigger timestamps should be carried forward unchanged and OMS
  should append its own stage timestamps rather than reconstruct the earlier path indirectly

Recommended implementation in the current Rust design:

- extend `EngineEvent::Tick` ingestion with an engine-local receive timestamp and optional source
  timestamp from RTMD
- capture queue-enqueue and strategy-dispatch timestamps inside `LiveEngine::run()`
- maintain the current triggering event context while the engine is inside strategy dispatch
- when `process_event()` returns `SAction::PlaceOrder`, attach the triggering event timestamps and
  correlation context before entering `dispatch_actions()`
- have the concrete production `ActionDispatcher` stamp `oms_submit_ts` immediately before publish/RPC
  to OMS
- propagate the correlation context through order metadata so OMS can emit matching ingress and
  internal-processing metrics

With OMS participation, the full breakdown becomes:

- tick received by engine
- queue + strategy decision inside engine
- engine to OMS submit
- OMS ingress to risk/validation
- OMS internal processing
- OMS egress / venue handoff
- optional venue acknowledgement / first order update back to engine

Instrumentation constraints:

- metric collection must be allocation-light and non-blocking on the hot path
- prefer direct histogram/counter updates and compact timestamp capture over structured logging per
  tick
- do not require cloning or retaining full tick payloads just for metrics
- sample all orders but allow configurable sampling for pure tick-processing spans if cardinality or
  overhead becomes a concern
- do not put high-cardinality identifiers directly into metrics labels; emit them through tracing or
  sampled event records instead

Recommended observability split:

- metrics for aggregate latency distributions and queue depth
- tracing or sampled structured events for per-decision correlation
- a small rolling in-memory ring buffer of recent tick-to-trade samples can be exposed through the
  query API for debugging, but it should remain outside the hot loop

Suggested baseline metric families:

- `engine_tick_queue_wait_ms`
- `engine_strategy_decision_ms`
- `engine_tick_to_submit_ms`
- `engine_order_submit_ms`
- `engine_tick_coalesced_total`
- `engine_hotloop_queue_depth`

If possible, the engine and OMS should share a common correlation schema defined once in the API
contract rather than inventing separate local tags.

Suggested correlation payload fields:

- `execution_id`
- `strategy_key`
- `trigger_event_type`
- `instrument_code`
- `trigger_source_ts`
- `trigger_recv_ts`
- `trigger_dispatch_ts`
- `trigger_decision_ts`
- `decision_seq` or another engine-local monotonic decision id
- `client_order_id`

This is enough to join engine and OMS telemetry without turning Prometheus labels into a
high-cardinality mess.

## Shutdown Model

Graceful shutdown:

1. stop accepting strategy work
2. publish `STOPPING`
3. delete KV registrations
4. drain runtime tasks
5. finalize with Pilot

Hard shutdown recovery relies on KV loss and Pilot reconciliation rather than explicit deregister.

## Timer Model And Drift Handling

The engine should treat timers as scheduled intents, not as a strict guarantee that callbacks run at
the exact wall-clock instant.

Recommended model:

- use a monotonic clock for internal waiting/sleep calculations
- keep wall-clock timestamps only for user-facing schedule definitions and emitted metadata
- represent each timer with `next_due_at`
- after each firing, compute the next schedule from the timer definition, not from the actual delayed
  dispatch time

This prevents cumulative drift for periodic timers.

Policy for delayed execution:

- if the engine wakes up late, fire the timer once with metadata indicating scheduled time and actual
  dispatch time
- do not replay an unbounded number of missed firings
- for cron-like timers, advance directly to the next valid schedule point after the current time
- for fixed-interval timers, optionally support a bounded catch-up count, but default to one immediate
  firing and then reschedule from the original cadence

The query/metrics surface should expose timer lag so persistent scheduler starvation is observable.

## Restart And Reconnect Semantics

On engine restart or connectivity recovery, strategy state should always be rebuilt from OMS-queried
live state before normal event processing resumes.

This matches the current Python reference implementation in
[engine.py](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_strategy/engine.py),
which initializes strategy state from queried open orders and account balances prior to entering the
main event loop.

Rules:

- OMS is the source of truth for open orders, fills, positions, and balances after restart/reconnect
- the engine should query and rebuild runtime state before delivering new market events to the
  strategy
- any local pre-failure in-memory state is advisory only and should not be trusted as authoritative
- strategy startup and reconnect should share the same rehydration path as much as possible

Suggested reconnect sequence:

1. mark the engine `DEGRADED` or `RECONNECTING`
2. pause strategy event delivery
3. re-establish transport sessions
4. query OMS/account state and rebuild the strategy-facing state view
5. re-establish subscriptions
6. resume dispatch from the fresh live state baseline

Open point:

- if the system later needs faster warm recovery, add optional local snapshots as a cache only; the
  final reconciliation step should still be against OMS truth

## Supervision Model

The engine supports two supervision modes:

- `BEST_EFFORT`
- `CONTROLLED`

Rules:

- ownership fencing is always fatal
- discovery-health degradation is fatal only in `CONTROLLED` mode after a grace period
- hard shutdown recovery is handled by KV loss and Pilot reconciliation

This keeps singleton enforcement in the control plane while leaving runtime supervision explicit in
the engine process.

## TODO

Later design work should still pin down:

- degraded-state reason model and health publication details
- exact reconnect event-ordering guarantees after resubscription
- whether local state snapshot/checkpoint support is worth adding beyond OMS rehydration
- strategy-side market-session gating policy using `RefdataSdk`
- minimal lifecycle event schema such as `STARTING`, `RUNNING`, `DEGRADED`, `PAUSING`, `PAUSED`,
  `RESUMING`, `STOPPING`, `STOPPED`, `FENCED`, and `FAILED`

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [SDK](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
