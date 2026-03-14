# Gateway Service

## Scope

`zk-gw-svc` is the trading gateway service for one venue/account runtime scope.

## Design Role

The trading gateway wraps venue order and account APIs and exposes a stable internal gRPC contract.

Responsibilities:

- place and cancel orders
- query balances, orders, trades, and account settings
- publish normalized gateway events to NATS
- register a live endpoint in KV

## Registration

Trading gateways register under `svc.gw.<gw_id>`.

The registration payload remains generic and carries:

- `service_type = "gw"`
- transport endpoint
- venue
- account scope
- capabilities

## Related Docs

- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
