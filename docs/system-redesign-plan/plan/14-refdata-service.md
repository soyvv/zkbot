# Phase 12: Full Refdata Service

## Goal

Implement the production refdata subsystem as a **Python service plus scheduled jobs**:

- `zk-refdata-svc` remains the runtime gRPC query service
- scheduled refresh jobs load and reconcile canonical refdata into PostgreSQL
- the service registers its gRPC endpoint in the shared NATS KV service-discovery registry
- downstream clients use the same registry/discovery mechanism already defined for Rust services

This phase replaces the "thin scaffold only" posture from
[07-scaffolding-services.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/07-scaffolding-services.md)
with a complete control-plane implementation.

## Why This Is A Separate Phase

Phase 6 intentionally delivered only enough refdata to unblock the Rust SDK:

- thin PostgreSQL-backed gRPC reads
- manual/dev seeding
- no venue refresh pipeline
- no lifecycle management
- no market calendar ownership

That is sufficient for early SDK work, but it is not a complete shared refdata service.

The production design in:

- [refdata_loader_service.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/refdata_loader_service.md)
- [data_layer.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [service_discovery.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)

requires a fuller runtime split and explicit lifecycle behavior.

## Prerequisites

- Phase 5 complete: registry + Pilot bootstrap contract available
- Phase 6 complete: initial `zk-refdata-svc` gRPC scaffold exists
- PostgreSQL `cfg.instrument_refdata` table available
- NATS KV registry bucket `zk-svc-registry-v1` available

## Runtime Shape

The recommended production runtime is:

```text
venue adaptors
  -> normalization / canonicalization
  -> diff + lifecycle engine
  -> PostgreSQL writer
  -> refdata change events

market calendar/session adaptors
  -> normalization
  -> session state writer
  -> market session events

zk-refdata-svc gRPC
  -> PostgreSQL reads
  -> optional in-process cache
  -> KV service registration
```

Design rules:

- PostgreSQL is the authority
- the gRPC service is the canonical shared lookup surface
- scheduled jobs own refresh and lifecycle transitions
- callers still own local caches and degraded-mode policy

## Deliverables

### 12.1 Python service layout

Keep the implementation in Python and split responsibilities cleanly:

```text
zkbot/services/zk-refdata-svc/
  src/zk_refdata_svc/
    main.py                -- gRPC server startup
    service.py             -- RefdataService RPC handlers
    repo.py                -- PostgreSQL query layer
    registry.py            -- NATS KV registration / heartbeat
    config.py              -- env + bootstrap config
    health.py              -- readiness / liveness helpers
    jobs/
      scheduler.py         -- APScheduler or equivalent
      refresh_refdata.py   -- venue refdata refresh job
      refresh_sessions.py  -- market calendar/session refresh job
      publish_changes.py   -- change/invalidation event publishing
    loaders/
      base.py              -- venue adaptor interface
      <venue>.py           -- reused/adapted venue fetchers
    normalize/
      instruments.py
      sessions.py
    lifecycle/
      diff.py
      policy.py
```

Implementation rule:

- keep `zk-refdata-loader` logic only as an adaptor/reference source
- do not keep MongoDB as part of the new runtime
- move to one canonical Python service package, even if some fetcher modules are reused from the current loader

### 12.2 Scheduled jobs

Use an in-process scheduler for the first production version. The service should run two job families:

1. Instrument refresh jobs
2. Market session/calendar refresh jobs

Recommended schedule:

- fast-changing crypto venue metadata: every 5-15 minutes
- slower TradFi/calendar sources: every 15-60 minutes
- manual admin-triggered refresh: supported in addition to the schedule

Job contract:

1. fetch raw venue/session data
2. normalize into canonical records
3. compare against current PostgreSQL state
4. classify `added`, `changed`, `disabled`, `deprecated`
5. commit updates
6. publish post-commit change notifications
7. update run metadata / watermark

Failure policy:

- a failed venue refresh does not corrupt prior canonical state
- partial success across venues is allowed
- per-venue failures must be visible through logs, metrics, and admin status

### 12.3 Canonical storage and lifecycle tables

`cfg.instrument_refdata` remains the canonical hot-path table, but this phase should add the minimum
supporting metadata needed for reliable lifecycle and auditing.

Recommended additions:

- `first_seen_at`
- `last_seen_at`
- `lifecycle_status`
- `disabled`
- `source_name`
- `source_run_id`
- `extra_properties`

Recommended companion tables:

- `cfg.refdata_refresh_run`
- `cfg.refdata_change_event`
- `cfg.market_session_state`
- `cfg.market_session_calendar`

The purpose is not full event-sourcing. It is to support:

- reliable cache invalidation
- operator visibility into refresh status
- lifecycle compatibility for historical instruments

### 12.4 gRPC contract completion

The production service should implement the full refdata API shape from
[api_contracts.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md):

- `QueryInstrumentRefdata`
- `QueryInstrumentByVenueSymbol`
- `ListInstruments`
- `QueryRefdataWatermark`
- `QueryMarketStatus`
- `QueryMarketCalendar`

The current scaffold method names may be kept temporarily for compatibility, but this phase should
align the protobuf and server implementation to the architecture contract.

Progressive disclosure must become real:

- `BASIC`: canonical trading identity and core trading params
- `EXTENDED`: lifecycle metadata, extra properties, session context
- `FULL`: provenance, aliases, operator/debug-oriented details

Hot-path callers should continue to use `BASIC`.

### 12.5 Service discovery registration for Python

The gRPC service must register through the same shared registry mechanism as other services.

Registration key:

```text
svc.refdata.<logical_id>
```

Bucket:

```text
zk-svc-registry-v1
```

Payload must follow the generic `ServiceRegistration` contract described in
[service_discovery.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md).

Minimum registration content:

- `service_type = "refdata"`
- `service_id`
- `instance_id`
- `transport.protocol = "grpc"`
- `transport.address` / `transport.authority`
- `capabilities`
- `lease_expiry_ms`
- `updated_at_ms`

Python gap to close in this phase:

- there is Rust-side registry/discovery support
- Python needs an equivalent lightweight registration helper for service runtimes

Recommended implementation:

- add a small reusable Python helper module for KV registration + CAS heartbeat
- keep it generic so Pilot, refdata, and any future Python service can share it
- do not bake refdata-specific semantics into the registry client

### 12.6 Python discovery support

The Rust SDK is already the primary consumer, but Python-side support is still useful for:

- Python services discovering refdata or OMS endpoints
- local tooling and admin scripts
- future Python SDK/runtime consumers

Scope for this phase:

- implement a minimal Python discovery client that watches or snapshots `zk-svc-registry-v1`
- support prefix lookup for `svc.refdata.*`
- expose raw registrations plus a small convenience resolver

This does not need to become a full Python trading SDK in this phase.

### 12.7 Change notification contract

The service should publish invalidation-oriented events after canonical writes commit.

Recommended subjects:

- `zk.control.refdata.updated`
- `zk.control.market_status.updated`

Recommended semantics:

- publish small, structured invalidation/update events
- do not publish full snapshot payloads
- downstream caches reload from gRPC after invalidation

The watermark returned by `QueryRefdataWatermark` should correspond to committed canonical state,
not just scheduler heartbeat time.

### 12.8 Admin/control surface

The production service should expose narrow operational controls:

- trigger full refresh
- trigger venue-scoped refresh
- inspect latest run status
- inspect failing venues/sources
- health/readiness endpoints

These controls may be gRPC admin methods, internal HTTP endpoints, or Pilot-mediated actions.
The design choice is less important than keeping the surface small and operational.

## Implementation Plan

### Step 1: Consolidate runtime ownership

- keep `zk-refdata-svc` as the canonical service directory
- treat `zk-refdata-loader` as source material to be migrated or imported from
- remove MongoDB dependency from the runtime path

Exit condition:

- one Python deployment unit can serve gRPC and run scheduled refresh jobs

### Step 2: Finish the protobuf and query model

- align RPC names with `api_contracts.md`
- add `QueryMarketCalendar`
- make disclosure levels meaningful
- add list/filter fields needed by Pilot and operator tooling

Exit condition:

- the protobuf surface matches the architecture docs and supports both SDK and operator lookups

### Step 3: Build the refresh pipeline

- port venue adaptor logic into the new runtime shape
- normalize all records to canonical `instrument_id`
- write diff/lifecycle logic
- write refresh-run metadata and change-event rows

Exit condition:

- scheduled refresh updates PostgreSQL canonically without blind overwrite

### Step 4: Add service registration and Python registry helper

- implement generic Python KV registration
- register `svc.refdata.<logical_id>` on startup
- heartbeat with CAS and shut down on fencing/ownership loss

Exit condition:

- Rust SDK and Python tooling can discover the running refdata service from KV

### Step 5: Add market-session ownership

- add session/calendar tables
- add refresh jobs for session sources
- implement real `QueryMarketStatus` and `QueryMarketCalendar`

Exit condition:

- market status is no longer a stub and is backed by canonical state

### Step 6: Integrate downstream invalidation

- publish refdata and market-status change events
- update Rust SDK assumptions if required
- provide a minimal Python consumer example/helper

Exit condition:

- downstreams reload from the service after invalidation rather than relying on preload or direct DB reads

## Tests

### Unit tests

- canonical instrument-id normalization from venue records
- lifecycle diff classification: add/change/disable/deprecate
- `(venue, instrument_exch)` uniqueness validation
- disclosure-level shaping
- registry heartbeat payload construction

### Integration tests

- scheduled refresh writes canonical rows into PostgreSQL
- gRPC query methods read the newest committed state
- service registers under `svc.refdata.<logical_id>` in `zk-svc-registry-v1`
- Rust SDK can resolve the gRPC endpoint from KV and perform a lookup
- change notification is emitted after commit, not before
- market-status query returns canonical session state once session jobs are enabled

## Exit Criteria

- [ ] `zk-refdata-svc` runs as a Python service with scheduled jobs enabled
- [ ] MongoDB is no longer required for the refdata runtime path
- [ ] full refdata gRPC API is implemented, including `QueryMarketCalendar`
- [ ] service discovery registration works through the shared KV mechanism
- [ ] a reusable Python registry helper exists for Python services
- [ ] a minimal Python discovery helper exists for `svc.refdata.*` lookup
- [ ] lifecycle-aware refresh updates PostgreSQL without blind overwrite
- [ ] refdata and market-status invalidation events are published post-commit
- [ ] Rust SDK discovery and lookup work against the service with no hard-coded endpoint

## Non-goals

- building a separate Python trading SDK in this phase
- pushing full refdata snapshots to clients
- making the refdata service responsible for caller-local cache policy
- introducing multi-source merge heuristics beyond explicit error-on-conflict behavior
