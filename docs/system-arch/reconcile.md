# Architecture Reconciliation Notes

This note captures cross-document inconsistencies and unresolved design drift found during a
consistency review of the current architecture docs.

Scope of this pass:

- included:
  - bootstrap/runtime config
  - service discovery
  - topology registration
  - SDK
  - Pilot
  - OMS
  - trading gateway
  - RTMD gateway
  - refdata service
  - engine service
  - venue integration
- excluded for now:
  - recorder service
  - reconciliation service

This file is intentionally a review queue, not a replacement for the settled architecture notes.

## Review Outcome

Obvious stale contract issues were already corrected in-place in:

- [arch.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [ops.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/ops.md)
- [pilot_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

The items below still need explicit reconciliation.

## Open Reconciliation Items

### 1. Current Bootstrap Implementation vs Target `lock_key` Design

Status:
- accepted direction

Docs involved:
- [service_discovery.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [topology_registration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)
- [api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)

Issue:
- the implemented design is currently `kv_key` + CAS heartbeat driven
- the broader topology-registration note still presents `lock_key` ownership and scoped runtime
  credentials as part of the main target model
- these are not the same maturity level

Why it matters:
- readers cannot tell which parts are current contract and which are later hardening steps

Decision:
- accepted

Suggested resolution:
- split the topology-registration note into:
  - `current implemented contract`
  - `future hardening options`
- treat `lock_key` and scoped runtime credentials as phase-later hardening unless/until implemented

### 2. Strategy Identity Naming Is Still Mixed

Status:
- accepted direction

Docs involved:
- [engine_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [pilot_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [topology_registration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/topology_registration.md)
- [data_layer.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [proto.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/proto.md)

Issue:
- newer service docs use `strategy_key` + `execution_id`
- some older schema/API notes still use `strategy_id`
- the distinction between stable strategy identity and one execution run is correct in some docs and
  blurred in others

Why it matters:
- this affects KV key shape, DB schema, strategy lifecycle APIs, correlation IDs, and event names

Decision:
- accepted

Suggested resolution:
- standardize on:
  - `strategy_key` for stable logical strategy identity
  - `execution_id` for one run
- allow proto/internal legacy fields to remain temporarily only if clearly documented as compatibility
  debt

### 3. Data-Layer Strategy Tables Need Alignment With The New Control Model

Status:
- accepted direction

Docs involved:
- [data_layer.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [engine_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [pilot_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

Issue:
- `cfg.strategy_definition` / `cfg.strategy_instance` still reflect an older model
- the newer design uses Pilot-issued execution claims, singleton strategy ownership, and stable
  strategy-scoped KV identity

Why it matters:
- the schema note is still the place other docs will copy from

Decision:
- accepted

Suggested resolution:
- revise the strategy tables around:
  - stable strategy definition
  - execution records
  - desired-vs-actual runtime config
  - singleton ownership metadata

### 4. Discovery Client Startup Contract Still Needs One Clear Architecture-Level Rule

Status:
- accepted direction

Docs involved:
- [service_discovery.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [sdk.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- service-specific startup notes that depend on KV watchers

Issue:
- the service-discovery note documents the current `KvDiscoveryClient` contract as:
  - `start()` returns empty
  - caller must start the watch loop
- higher-level docs often read as if discovery is immediately snapshot-ready at startup

Why it matters:
- this is a real readiness and startup-order contract, not just an implementation detail

Decision:
- accepted
- choose snapshot-ready / ready-on-start behavior

Suggested resolution:
- pick one architecture-level rule:
  - snapshot-ready on `start()`
- then align SDK/service startup docs to that rule

### 5. RTMD Policy Table vs Live Subscription Protocol Boundary Should Be Sharpened

Status:
- accepted direction with changed resolution

Docs involved:
- [market_data_gateway_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [rtmd_subscription_protocol.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rtmd_subscription_protocol.md)
- [data_layer.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [pilot_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

Issue:
- the architecture is mostly clear that live RTMD interest is direct and Pilot owns slower
  policy/defaults
- but `cfg.mdgw_subscription` can still be misread as the runtime hot-path interest source

Why it matters:
- this is a core design boundary for keeping Pilot out of the RTMD steady-state path

Decision:
- reject the earlier proposed framing
- for now, remove control-plane override language from the main RTMD contract and keep it only as a
  later consideration/TODO

Suggested resolution:
- restate `cfg.mdgw_subscription` and related Pilot sections as slow control-plane policy/default
  state only
- keep live RTMD interest exclusively in the dedicated RTMD subscription protocol
- move any future override/force-subscribe ideas into TODO/considerations, not the main runtime
  contract

### 6. Pilot Health / Async Jobs / Partial Failure Model Is Intentionally Deferred But Not Yet Framed

Status:
- accepted direction

Docs involved:
- [pilot_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [ops.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/ops.md)

Issue:
- these are listed as TODOs, which is correct
- but the architecture does not yet say whether future Pilot jobs are:
  - purely internal
  - durable DB-backed
  - externally visible as job resources

Why it matters:
- many future Pilot APIs depend on whether workflows are synchronous, accepted-async, or durable

Decision:
- accepted

Suggested resolution:
- add one short cross-cutting design note later for:
  - job resource model
  - partial-failure reporting
  - control-plane audit expansion

### 7. Refdata Progressive Disclosure Needs More Exact Response-Bounding Rules

Status:
- accepted direction

Docs involved:
- [refdata_loader_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/refdata_loader_service.md)
- [sdk.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)

Issue:
- the concept is defined
- the exact response-shaping mechanism is still unspecified

Why it matters:
- Pilot/operator/AI consumers and SDK/runtime consumers should not drift into incompatible query
  styles

Decision:
- accepted
- support both:
  - enum disclosure level
  - bounded expansion flags

Suggested resolution:
- define one explicit response-shaping contract that includes:
  - enum disclosure level
  - bounded expansion flags

### 8. Venue Integration Packaging Needs One Repo-Level Convention

Status:
- accepted direction

Docs involved:
- [venue_integration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md)
- service docs that refer to generic service hosts

Issue:
- the integration mechanism is well described conceptually
- the actual repo layout and ownership convention for manifests, schemas, Rust modules, and Python
  packages is still not fixed

Why it matters:
- this will affect how new venues are added and tested in practice

Decision:
- accepted

Suggested resolution:
- define a concrete repository convention later:
  - where manifests live
  - where schemas live
  - how Rust registry wiring is declared
  - how Python bridge entrypoints are packaged

## Low-Severity Notes

### A. Legacy ODS Mentions

Status:
- mostly acceptable

Issue:
- some docs still mention ODS in migration context

Resolution guidance:
- leave migration-context mentions in place
- remove only if they are phrased as current architecture behavior

### B. Gateway / OMS Health RPC Notes

Status:
- acceptable for now

Issue:
- some ops notes still assume generic `QueryHealth` gRPC patterns
- this does not currently conflict with the service docs, but the exact health API contract remains
  underspecified

## Suggested Review Order

1. strategy identity naming and data-layer alignment
2. bootstrap current-vs-target contract split
3. discovery client readiness contract
4. RTMD policy/default table boundary
5. Pilot job / partial-failure / audit model
6. refdata disclosure exact query shape
7. venue integration repo convention
