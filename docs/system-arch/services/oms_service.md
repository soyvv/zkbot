# OMS Service

## Scope

`zk-oms-svc` is the order-management and risk-control service.

## Design Role

OMS is the command and state authority for:

- order submission and cancellation
- balance and position state
- risk checks and panic controls
- normalized order-update fanout

OMS discovers gateways through the generic KV registry and does not depend on a separate ODS.

## Runtime Model

OMS should use a single-writer actor plus read-replica pattern.

Intent:

- one writer task owns `OmsCore` mutations
- queries read lock-free snapshots
- gateway dispatch, Redis writes, and NATS publication happen on the mutation path

Logical shape:

```text
gRPC mutations / GW reports
        |
        v
  OmsCore writer task
        |
        +-- gateway actions
        +-- NATS publish
        +-- Redis sync
        |
        v
   snapshot swap
        |
        v
   query readers
```

This keeps mutation order deterministic while making read APIs cheap.

## Startup Model

OMS startup should:

1. load config from PostgreSQL
2. warm-load order, balance, and position state from Redis
3. discover bound gateways from KV
4. reconcile gateway state through query calls
5. start gRPC serving
6. register `svc.oms.<oms_id>` in KV
7. subscribe to gateway report/system topics

Warm start plus reconcile is required so OMS can recover fast from process restarts without treating
Redis as the final authority.

## State And Event Flow

Mutation inputs:

- gRPC commands such as `PlaceOrder`, `CancelOrder`, `Panic`, `ReloadConfig`
- gateway reports from NATS
- periodic resync events

Mutation outputs:

- gateway RPC actions
- NATS `OrderUpdateEvent`, balance, position, and system events
- Redis order/open-order/balance/position state updates
- a fresh immutable `OmsSnapshot`

On gateway restart or reconnect, OMS should trigger a bounded resync for affected accounts.

## Interfaces

- gRPC: `zk.oms.v1.OMSService`
- NATS topics for order, balance, position, and system events
- Redis for fast operational state
- PostgreSQL for config lookup

## Registration

OMS registers itself in KV under `svc.oms.<oms_id>` and publishes a generic discovery payload
containing transport, venue/account scope, and capabilities.

## Persistence Boundaries

- PostgreSQL holds configuration and authoritative control-plane data
- Redis is the warm-start and operational read store for live OMS state
- NATS carries normalized events for strategies, recorder, and monitor
- OMS runtime memory remains the active mutation authority while the process is live

## Related Docs

- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
