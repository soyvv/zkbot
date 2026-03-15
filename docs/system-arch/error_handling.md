# Error Handling

## Scope

This note defines the cross-service direction for error classification, error codes, and handling
policy.

It should be applied consistently across:

- trading gateway
- OMS
- strategy engine
- Pilot and other managed services where relevant

The intent is to avoid each service inventing its own incompatible retry, alerting, and lifecycle
semantics.

## Shared Goals

- use stable, explicit error categories
- separate retriable transport failures from business-rule failures
- keep error propagation understandable across gRPC, NATS, and control-plane APIs
- align retry, reconciliation, and operator-action rules across services

## Shared Top-Level Categories

Recommended top-level categories:

- bootstrap/auth/config error
- dependency auth error
- business-rule / request reject error
- transport/session error
- rate-limit / throttling error
- temporary upstream availability error
- semantic inconsistency / recovery-required error
- internal service error

These categories should be mapped into service-specific codes rather than replaced by ad hoc
service-local vocabularies.

## Service Sections

### Gateway Errors

Gateway-specific focus:

- how venue errors surface over gRPC and NATS lifecycle/system events
- which classes are retriable by OMS
- which classes should trigger gateway reconnect, degraded mode, or operator action

Gateway design alignment:

- keep venue adaptors minimal
- keep gateway-side durable recovery logic minimal
- let OMS own broader retry/idempotency/dedup policy where needed

### OMS Errors

OMS-specific focus:

- command reject vs infrastructure failure distinction
- gateway dependency failure handling
- reconciliation-required states
- retry and dedup policy ownership

OMS design alignment:

- OMS should own durable command idempotency policy
- OMS should own durable downstream deduplication where needed
- OMS should decide when a dependency issue requires bounded resync versus operator escalation

### Strategy Errors

Strategy-specific focus:

- user/business logic error vs infrastructure error distinction
- when strategy runtime should stop, retry, or degrade
- how fencing, OMS dependency failure, and discovery/control-plane degradation are surfaced

Strategy design alignment:

- ownership fencing is always fatal
- supervision mode controls whether discovery degradation is fatal
- strategy-level business exceptions should not be conflated with platform/runtime failures

## TODO

- define canonical cross-service error code families
- define gRPC status mapping rules
- define NATS lifecycle/system error event schema
- define operator-visible alerting classes
- define retry policy matrix by category and service
- define which categories force `DEGRADED`, `RESYNCING`, `FENCED`, or shutdown transitions
- add concrete gateway error codes
- add concrete OMS error codes
- add concrete strategy/runtime error codes

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [OMS Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/oms_service.md)
- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
