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
- optional Python worker runtime
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

## Embedded RTMD Mode

An engine may host an embedded RTMD gateway runtime.

Rules:

- reuse the same RTMD runtime components as the standalone RTMD gateway
- register a separate `mdgw` role only if the embedded runtime publishes shared RTMD feeds
- keep private in-process RTMD behavior unregistered when it is not a shared publisher
- let Pilot metadata decide whether the runtime is engine-only, `engine+mdgw`, or a broader composite

The embedded RTMD path should not introduce a separate engine-specific discovery or subscription
protocol.

## Shutdown Model

Graceful shutdown:

1. stop accepting strategy work
2. publish `STOPPING`
3. delete KV registrations
4. drain runtime tasks
5. finalize with Pilot

Hard shutdown recovery relies on KV loss and Pilot reconciliation rather than explicit deregister.

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

Later design work should pin down:

- strategy handler backpressure and slow-consumer policy
- precise pause/resume semantics
- degraded-state reason model and health publication
- Python worker crash/restart policy
- timer model and timer-drift handling
- event-ordering expectations after reconnect or resync
- whether engine state snapshot/checkpoint support is needed
- strategy-side market-session gating policy using `RefdataSdk`
- minimal lifecycle event schema such as `STARTING`, `RUNNING`, `DEGRADED`, `STOPPING`, `STOPPED`,
  `FENCED`, and `FAILED`

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [SDK](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
