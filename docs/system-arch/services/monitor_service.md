# Monitor Service

## Scope

The monitor service aggregates operational state, alerts, and health signals.

## Design Role

Responsibilities:

- subscribe to OMS and strategy events
- evaluate alert and SLO conditions
- publish operator notifications
- provide control-plane health context to Pilot or dashboards

It is an observer and alerting service, not a discovery authority.

## Related Docs

- [Operations](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/ops.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
