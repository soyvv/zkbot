# Refdata Loader Service

## Scope

The refdata loader imports and refreshes venue/instrument metadata into PostgreSQL.

## Design Role

Responsibilities:

- populate instrument reference data
- refresh venue metadata periodically or on demand
- trigger downstream reload signals when reference data changes

It is a control-plane support service and does not participate in trading-path service discovery
beyond any optional operational registration.

## Related Docs

- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
