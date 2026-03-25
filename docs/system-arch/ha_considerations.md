# HA Considerations

This note captures high-availability considerations for the bootstrap, service discovery, and
control-plane model.

It is intentionally a design note, not an implementation-commitment document. Where the current
implementation differs, this document describes the preferred direction for future deployment
shapes such as Kubernetes.

Related docs:

- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Topology Registration](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)
- [Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md)
- [Registry Security](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/registry_security.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

## 1. Goals

- Keep runtime liveness and discovery available even when Pilot is unavailable.
- Allow future Kubernetes-style multi-replica deployments without making consumers discover
  multiple conflicting endpoints for one logical service.
- Keep bootstrap, config delivery, liveness, and leader election as separate concerns.
- Avoid turning Pilot into the steady-state heartbeat or traffic-forwarding path.

## 2. Current Baseline

The current architecture already establishes an important HA boundary:

- Pilot authorizes startup and returns effective config.
- runtime services register and heartbeat directly in NATS KV
- KV state, not Pilot DB state, is the liveness source of truth

This is the correct baseline for HA because steady-state service presence does not depend on Pilot
remaining in the request path.

Current implementation caveat:

- bootstrap policy is still singleton-oriented for one active session per logical instance
- active-active Pilot bootstrap handling is not yet the implemented contract

That means the current code is HA-friendly in steady state, but not yet fully HA-capable for all
future deployment shapes.

## 3. HA Principle: Separate Four Concerns

The design should keep these concerns separate:

1. bootstrap admission
2. config delivery
3. live service discovery
4. leader election or active-replica selection

These should not be collapsed into a single key or one overloaded registration flow.

Recommended ownership:

- Pilot owns bootstrap admission and effective config delivery.
- Vault owns secret material.
- NATS KV owns live discovery records.
- a dedicated lease/election mechanism owns leader selection when multiple replicas exist.

## 4. Target Multi-Replica Model

For a future Kubernetes deployment, a logical service may be deployed as multiple runtime pods
while still appearing to Pilot and to downstream consumers as one logical instance.

Preferred model:

- Pilot authorizes the logical service identity, not just one concrete pod.
- multiple replicas may bootstrap and receive the same effective config for that logical service
- only one replica is leader for the stable discoverable service identity at a time
- followers do not present themselves as equivalent active endpoints for the stable logical key

This allows:

- rolling upgrades
- faster failover
- warm standby replicas
- one stable logical discovery contract for consumers

## 5. Discovery Rule For Multi-Replica Services

For any service kind that supports replicated runtime instances, the stable discovery key should be
leader-owned only.

Recommended rule:

- `svc.<type>.<logical_id>` is the stable leader-owned discovery key
- only the elected leader may publish or renew that stable key
- followers either:
  - do not publish a discoverable service key, or
  - publish replica-scoped keys such as `replica.<type>.<logical_id>.<replica_id>` for operator
    visibility only

Consumers should continue resolving the stable logical key and should not need replica-awareness
for ordinary routing.

This keeps failover internal to the service group rather than leaking replica topology into every
client.

## 6. Bootstrap Rule For Multi-Replica Services

Two-phase bootstrap remains useful, but its role should be explicit:

- phase 1: Pilot admission + config grant
- phase 2: runtime registration after the replica has initialized and knows whether it is allowed
  to become live

For replicated services, Pilot should be able to grant bootstrap/config to multiple replicas of one
logical service without implying that all of them may publish the stable discovery key.

Important distinction:

- "allowed to start" is not the same as "allowed to own the stable service endpoint"

The latter should be controlled by leader-election policy, not by every replica competing on the
same discovery key as a first-class design.

## 7. Leader Election Guidance

If a service kind needs multi-replica warm-standby or active-passive behavior, leader election
should use a dedicated lease/lock primitive.

Recommended properties:

- leader selection is independent from ordinary client discovery reads
- leadership is revocable and lease-based
- leadership transitions are observable by Pilot and operators
- the elected leader is responsible for publishing the stable discovery key

Avoid using the ordinary service discovery record itself as the election primitive.

Why:

- it overloads one key with two meanings: "who is leader" and "what endpoint should clients use"
- it makes operator visibility and failure diagnosis harder
- it couples client routing semantics to internal replica churn

## 8. Pilot Availability Model

Pilot HA is a separate concern from runtime service HA.

The intended availability model should remain:

- if Pilot is down, already-running services continue
- if Pilot is down, new bootstrap or re-bootstrap is blocked unless an explicit fallback mode exists

This is acceptable as long as:

- Pilot is not on the steady-state heartbeat path
- Pilot is not the hot-path query/transport dependency for runtime-to-runtime communication

Future Pilot active-active support should address:

- bootstrap request fan-in semantics
- DB-level atomic bootstrap/session admission
- reconciler/watch behavior across multiple Pilot replicas
- consistent control-plane reads and writes across replicas

## 9. Shared Dependencies Still Need HA

Separating Pilot from steady-state liveness does not remove the need to harden shared infrastructure.

For a production HA story, the following dependencies also need an HA deployment model:

- NATS / JetStream
- PostgreSQL
- Vault
- Kubernetes control plane and node placement

If those layers are single-instance, the overall system is still not HA regardless of how the
service registration contract is shaped.

## 10. Service-Kind Policy

Not every service kind needs the same HA model.

Recommended policy categories:

- singleton-only
  - exactly one runtime instance per logical service
- leader-elected
  - multiple replicas may run, but only one owns the stable discovery identity
- sharded
  - multiple logical instances are intended and each has its own stable discovery identity

This policy should be explicit in Pilot-controlled topology metadata rather than implicit in ad hoc
service behavior.

## 11. Current Contract Gap

The current bootstrap/session model is still centered on one active session per logical instance.

That is acceptable for now, but it is not sufficient for the future replicated-runtime model
described here.

To support that model cleanly, the architecture will eventually need to distinguish:

- logical service identity
- replica identity
- bootstrap session
- current elected leader
- stable discoverable endpoint

Those are related, but they are not the same thing.

## 12. Recommended Direction

For future HA-oriented runtime deployment:

- keep Pilot out of the steady-state heartbeat path
- keep NATS KV as the runtime liveness source of truth
- allow multiple replicas to receive bootstrap/config when the service policy permits it
- keep one stable leader-owned discovery key for ordinary consumers
- use a dedicated election/lease mechanism for leader ownership
- treat Pilot active-active as a separate follow-up design topic

This preserves the current architectural strengths while leaving a clean path toward Kubernetes
multi-replica deployment.
