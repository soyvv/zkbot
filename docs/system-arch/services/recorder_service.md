# Recorder Service

## Scope

The recorder service consumes OMS, strategy, and RTMD events and writes durable history and
analytical state.

## Design Role

Responsibilities:

- immutable event recording
- trade and snapshot persistence
- reconciliation jobs against exchange history
- post-trade cleanup of terminal OMS state

## Data Ownership

Recorder writes:

- PostgreSQL analytical and reconciled trade tables
- MongoDB event history

It consumes NATS event streams and is not part of service discovery routing decisions beyond its
own optional runtime registration.

## Related Docs

- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
