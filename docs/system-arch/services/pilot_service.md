# Pilot Service

## Scope

`zk-pilot` is the control-plane service for topology, bootstrap authorization, runtime claims,
and dynamic configuration.

It owns:

- bootstrap registration and deregistration over NATS request/reply
- control-plane metadata in PostgreSQL
- RTMD policy/default state and RTMD subscription observability
- topology reconciliation from NATS KV
- operator-facing REST and internal gRPC control/query APIs

## Design Role

Pilot is not on the steady-state data path for normal trading or RTMD publishing.

Pilot is authoritative for:

- whether a logical instance may start
- what runtime metadata/profile a service receives
- singleton policy for strategies and other logical instances
- RTMD policy/defaults and global RTMD topology visibility

Pilot is not the liveness source of truth. Runtime liveness comes from KV registry state and is
reconciled back into Pilot state.

Pilot should also not be the steady-state hub or single forwarding point for RTMD subscription
changes while the system is running.

## Key Interfaces

- Bootstrap NATS subjects:
  - `zk.bootstrap.register`
  - `zk.bootstrap.deregister`
  - `zk.bootstrap.reissue`
  - `zk.bootstrap.sessions.query`
- REST:
  - strategy execution start/stop
  - OMS reload
  - RTMD subscription CRUD and reload
  - topology and refdata queries
- gRPC:
  - `zk.pilot.v1.PilotService`

## Topology And Registration Policy

Pilot interprets business meaning from `cfg.logical_instance.metadata`.

Examples:

- `registration_kind: "gw"`
- `registration_kind: "oms"`
- `registration_kind: "strategy"`
- `registration_kind: "engine+mdgw"`
- `registration_kind: "oms+gw+strategy"`

This keeps the registry contract generic while allowing Pilot to enforce:

- strategy singleton ownership
- composite deployment constraints
- venue/account scope validation
- embedded RTMD publisher policy

## RTMD Subscription Management

Pilot manages control-plane RTMD policy/default state in `cfg.mdgw_subscription`.

Pilot should not be required on the hot path when a client subscribes a new symbol.

Recommended split:

- clients publish live subscription interest directly into a dedicated KV space such as `zk.rtmd.subs.v1`
- RTMD gateways watch that live interest and react quickly
- Pilot watches the same live interest for observability and policy enforcement

Pilot responsibilities:

- maintain defaults / allowlists / disables
- inspect current RTMD subscription topology
- publish control overrides or reloads when operator policy changes
- expose admin APIs for subscription inspection and management

Separation rule:

- `zk.svc.registry.v1` remains the service discovery/liveness bucket
- `zk.rtmd.subs.v1` is used for live RTMD subscription leases

Pilot triggers reconciliation through:

- `zk.control.mdgw.<venue>.reload`
- `zk.control.mdgw.<logical_id>.reload`

Pilot may derive effective views for venue-wide and logical-instance-scoped RTMD runtimes, but it
should not be the only component that notices live runtime subscription adds/removes.

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [RTMD Subscription Protocol](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rtmd_subscription_protocol.md)
- [Topology Registration](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
