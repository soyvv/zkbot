# Engine Service

## Scope

`zk-engine-svc` executes one strategy runtime and interacts with OMS, RTMD feeds, and Pilot.

## Design Role

The engine is a data-plane runtime with control-plane startup and ownership rules:

- runtime config comes from Pilot
- `execution_id` identifies one concrete run of a strategy
- only one active execution is allowed per `strategy_key`
- registration in KV uses a stable logical engine key, not `execution_id`

## Startup Model

1. claim execution from Pilot
2. receive authoritative `execution_id`, strategy config, and registration metadata
3. initialize SDK and runtime
4. register the engine in KV
5. run with ownership supervision

Registration guidance:

- keep the KV discovery contract generic
- include `strategy_key`, `execution_id`, and role/profile metadata in the payload
- do not encode strategy-specific ownership rules into the generic discovery client

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

## Shutdown Model

Graceful shutdown:

1. stop accepting strategy work
2. publish `STOPPING`
3. delete KV registrations
4. drain runtime tasks
5. finalize with Pilot

Hard shutdown recovery relies on KV loss and Pilot reconciliation rather than explicit deregister.

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [SDK](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
