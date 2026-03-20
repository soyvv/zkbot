# Agent Prompt: Build The Full Refdata Service

Use this prompt with a coding agent.

---

Implement the full production refdata service for zkbot as a **Python service with scheduled jobs**
under `zkbot/services/zk-refdata-svc`.

This is not the thin scaffold from the earlier redesign phase. Build the fuller control-plane
service described in the redesign and architecture docs.

## Primary references

- `zkbot/docs/system-redesign-plan/plan/13-refdata-service.md`
- `zkbot/docs/system-arch/services/refdata_loader_service.md`
- `zkbot/docs/system-arch/api_contracts.md`
- `zkbot/docs/system-arch/data_layer.md`
- `zkbot/docs/system-arch/service_discovery.md`
- `zkbot/docs/domains/instrument_convention.md`

## Existing implementation references

- `zkbot/services/zk-refdata-svc/`
- `zkbot/services/zk-refdata-loader/`
- `zkbot/docs/system-redesign-plan/plan/07-scaffolding-services.md`

## Goal

Turn the current scaffold into a production-oriented refdata subsystem that:

- serves canonical instrument and market-session data over gRPC
- refreshes venue refdata via scheduled jobs
- writes canonical state into PostgreSQL
- manages lifecycle transitions such as `active`, `disabled`, and `deprecated`
- emits refdata invalidation / market-status update events after commit
- registers its gRPC endpoint in the shared NATS KV service-discovery registry

## Hard constraints

1. Keep this a Python service.
2. Use PostgreSQL as the authority. Do not keep MongoDB in the runtime path.
3. Keep the gRPC service discoverable through the generic service-discovery contract.
4. Respect the canonical `instrument_id` convention from the design docs.
5. Do not invent a second source of truth outside PostgreSQL.
6. Keep venue adaptors narrow. They fetch and normalize; they do not own lifecycle policy.
7. Keep the registry/discovery logic generic so it can be reused by future Python services.
8. Use ASCII only unless a target file already uses non-ASCII.

## Required deliverables

### 1. Service structure

Refactor or extend `zkbot/services/zk-refdata-svc` so it contains:

- gRPC server startup
- PostgreSQL query/repository layer
- scheduled jobs
- venue loader adaptor boundary
- normalization layer
- lifecycle diff / reconciliation logic
- NATS KV registration / heartbeat helper
- optional Python KV discovery helper for local tooling or future Python consumers

If code should be shared outside the service package, keep the shared module small and generic.

### 2. Scheduled jobs

Implement in-process scheduled jobs for:

- instrument refdata refresh
- market-session / calendar refresh

The jobs must:

1. fetch source data
2. normalize it into canonical records
3. compare against current PostgreSQL state
4. classify adds / changes / disables / deprecations
5. commit the canonical updates
6. emit post-commit change notifications
7. record refresh-run metadata

### 3. Canonical storage support

Use `cfg.instrument_refdata` as the hot-path canonical table and add any minimal supporting schema
needed for:

- refresh run metadata
- lifecycle tracking
- change-event / watermark support
- market-session state / calendar support

Do not add speculative schema that is not justified by the referenced docs.

### 4. gRPC API

Align the service implementation with the architecture contract in
`zkbot/docs/system-arch/api_contracts.md`.

Implement the full query surface:

- `QueryInstrumentRefdata`
- `QueryInstrumentByVenueSymbol`
- `ListInstruments`
- `QueryRefdataWatermark`
- `QueryMarketStatus`
- `QueryMarketCalendar`

Make progressive disclosure real:

- `BASIC`
- `EXTENDED`
- `FULL`

The default/hot-path shape should remain compact.

### 5. Service discovery

Register the service in NATS KV under:

```text
svc.refdata.<logical_id>
```

using the generic registry bucket:

```text
zk-svc-registry-v1
```

The registration payload must follow the design docs and include:

- `service_type = "refdata"`
- transport metadata for the gRPC endpoint
- capabilities
- lease / heartbeat timestamps

If Python-side registry/discovery helpers do not exist yet, implement the minimum reusable version
needed for this service. Do not hard-code refdata-specific logic into that shared helper.

### 6. Tests

Add strong tests. At minimum cover:

- canonical instrument-id normalization
- lifecycle diff classification
- disabled/deprecated handling
- query by `instrument_id`
- query by `(venue, instrument_exch)`
- list/filter behavior
- watermark behavior
- post-commit event publication
- KV registration / heartbeat behavior
- market-status query behavior

Include integration coverage for:

- scheduled refresh writing PostgreSQL state
- gRPC queries reading committed state
- service registration in KV

## Implementation guidance

- Reuse useful loader logic from `zk-refdata-loader`, but do not preserve its Mongo-oriented design.
- Keep refresh and query responsibilities separate even if they run in one process.
- Prefer a small in-process cache for query hot paths only if it does not weaken PostgreSQL authority.
- Do not turn the service into a push-based snapshot distributor; downstreams should reload after invalidation.
- Error on conflicting upstream canonical mappings rather than inventing merge heuristics.

## Required review loop

You must use Codex to review the work against the design docs before considering the task complete.

Review loop requirements:

1. Implement a coherent slice of the service.
2. Call Codex to review the diff, explicitly referencing these docs:
   - `zkbot/docs/system-redesign-plan/plan/13-refdata-service.md`
   - `zkbot/docs/system-arch/services/refdata_loader_service.md`
   - `zkbot/docs/system-arch/api_contracts.md`
   - `zkbot/docs/system-arch/data_layer.md`
   - `zkbot/docs/system-arch/service_discovery.md`
   - `zkbot/docs/domains/instrument_convention.md`
3. Treat design mismatches, lifecycle holes, broken discovery behavior, incorrect API shape, weak tests,
   and unsafe schema assumptions as major issues.
4. Fix the issues Codex finds.
5. Repeat the Codex review loop until Codex reports no major issues.

If Codex finds only minor or optional follow-ups, note them clearly but do not block completion on
them.

## Suggested Codex review prompt

Use something close to this when calling Codex for review:

```text
Review this refdata service change against the following design docs:
- zkbot/docs/system-redesign-plan/plan/13-refdata-service.md
- zkbot/docs/system-arch/services/refdata_loader_service.md
- zkbot/docs/system-arch/api_contracts.md
- zkbot/docs/system-arch/data_layer.md
- zkbot/docs/system-arch/service_discovery.md
- zkbot/docs/domains/instrument_convention.md

Focus on major issues only:
- architecture/design drift from the docs
- incorrect canonical instrument identity handling
- broken or incomplete service discovery integration
- missing lifecycle handling
- unsafe or unjustified schema changes
- invalid gRPC contract shape
- missing critical tests or serious behavioral regressions
```

## Out of scope

Do not:

- build a full Python trading SDK
- make clients depend on direct PostgreSQL reads
- reintroduce MongoDB into the runtime path
- invent multi-source merge precedence rules unless the docs require them
- add unrelated Pilot or OMS redesign work

## Verification

Run the relevant tests and any service-level smoke checks you add. At minimum, report:

1. what schema changes were introduced
2. which gRPC methods were implemented or changed
3. how KV registration / heartbeat works
4. what Codex review findings were fixed
5. final test results

## Expected output from the agent

1. concise summary of the implementation
2. concise summary of schema/API/discovery changes
3. Codex review findings and fixes
4. final verification results
5. any intentionally deferred minor follow-ups
