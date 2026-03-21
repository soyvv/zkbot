# Refdata Loader Service

## Scope

The refdata loader imports, refreshes, and serves venue/instrument metadata and market session
status.

## Design Role

Responsibilities:

- populate instrument reference data into PostgreSQL
- refresh venue metadata periodically or on demand
- manage refdata lifecycle transitions such as added, changed, disabled, and deprecated instruments
- maintain market open/close session state and session-calendar events for TradFi markets
- expose a gRPC query service for refdata lookup
- trigger downstream reload or cache-invalidation signals when reference data changes

It is a control-plane support service. Because it exposes a query API, it should register its live
endpoint in service discovery when running as a shared service.

## Reference Implementation Notes

`zkbot/services/zk-refdata-loader` is a useful reference for:

- per-venue loader modularization
- a minimal exchange adaptor boundary
- normalization/enrichment of venue instrument metadata before storage

The current implementation is still batch-oriented and Mongo-oriented. The new service design should
keep the venue-loader shape but use PostgreSQL as the authoritative store and add lifecycle/query
behavior on top.

Bottom-line design constraint:

- the refdata service is important, but it should not become unnecessarily important
- it is the canonical shared lookup and update source
- callers should still own their local cache, staleness handling policy, and recovery behavior

## Runtime Shape

Recommended runtime split:

- venue-integration facade
- venue loader adaptors
- normalization/enrichment layer
- PostgreSQL writer
- change detector
- gRPC query service
- control/reload loop

Suggested logical shape:

```text
venue manifest/config -> venue-integration facade -> venue loader adaptor
                                                   -> normalize/enrich -> compare with cfg.instrument_refdata
                                                                          |
                                                                          +-> write inserts/updates/disables
                                                                          +-> emit refdata change event
                                                                          +-> serve query API from PostgreSQL

venue manifest/config -> venue-integration facade -> market-session adaptor path
                                                   -> normalize/enrich -> compare with canonical market session state
                                                                          |
                                                                          +-> write open/close/session updates
                                                                          +-> emit market session event
                                                                          +-> serve market-status query API
```

## Venue Loader Adaptor

The venue-specific adaptor should stay narrow, similar to the current Python implementation.

The refdata loader adaptor should be resolved through the shared venue-integration mechanism
described in
[Venue Integration Modules](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md).

Design rule:

- the refdata service host must not hardcode a separate per-venue loader registry
- venue-specific refdata code should live under `zkbot/venue-integrations/<venue>/`
- the host should resolve the `refdata` capability from the venue manifest and instantiate the
  declared Python entrypoint
- per-venue refdata config should be validated against the manifest-declared
  `schemas/refdata_config.schema.json` before the adaptor is instantiated

Recommended host-side loading sequence:

1. load `venue-integrations/<venue>/manifest.yaml`
2. confirm the venue exposes the `refdata` capability
3. confirm `refdata.language == python`
4. validate the provided config against `refdata.config_schema`
5. resolve and instantiate the manifest-declared Python loader class
6. call the adaptor through the stable refdata-loader interface

This keeps the refdata host generic and aligned with the gateway/RTMD venue-integration pattern.

Suggested responsibilities:

- fetch venue instrument metadata
- map venue fields into normalized `InstrumentRefdata`-like records
- expose venue-specific extra properties without leaking transport details into callers
- optionally provide market-session or market-calendar source data for that venue

Suggested non-responsibilities:

- lifecycle policy
- global diffing
- cache distribution
- query serving
- service registration/discovery
- scheduler ownership

For TradFi-oriented venues or exchanges, a parallel loader/adaptor path is also needed for market
session metadata:

- holiday calendar
- trading day exceptions
- session open/close schedule
- intraday halt or special-session status where available

Recommended adaptor interface shape:

```python
class SomeRefdataLoader:
    def __init__(self, config: dict | None = None): ...
    async def load_instruments(self) -> list[dict]: ...
    async def load_market_sessions(self) -> list[dict]: ...
```

`load_market_sessions()` may return an empty list for always-open crypto venues.

## Source Conflict Policy

The initial design assumes there is no multi-source conflict-resolution layer.

Rule:

- if two upstream sources disagree for the same canonical mapping, the loader should error out
- silent merge or precedence rules should not be invented implicitly

This keeps the service simple until a real source-priority design is needed.

## Refdata Lifecycle Management

Refdata needs explicit lifecycle treatment rather than only blind overwrite.

Recommended lifecycle states:

- `active`
- `disabled`
- `deprecated`

Recommended rules:

- newly discovered instruments are inserted as `active`
- changed instruments update the canonical row and bump `updated_at`
- missing instruments should not be deleted immediately; they should usually transition to
  `deprecated` or `disabled` based on policy
- disabled/deprecated instruments remain queryable for historical compatibility unless explicitly
  purged by an offline maintenance workflow

Change classes worth detecting:

- instrument added
- instrument metadata changed
- instrument disabled
- instrument deprecated/removed from venue listing

Downstream consequence:

- services and SDK caches should treat refdata as versioned control-plane data, not immutable static
  compile-time data

## Market Session Lifecycle

For TradFi markets, the refdata service should also be the canonical owner of market open/close
status and session-calendar events.

Recommended responsibilities:

- maintain canonical trading-calendar state for each market/venue
- determine whether a market is currently open, closed, halted, pre-open, or in a special session
- emit update events when market session status changes

Typical change classes:

- market opened
- market closed
- holiday or special-closure applied
- session schedule updated
- intraday halt / resume

Design rule:

- market session state is control-plane reference state, not best-effort client-local logic
- downstream services and SDKs may cache it, but the refdata service remains the canonical source
- timestamps should be represented as Unix timestamps; caller-side timezone/display interpretation is
  the caller's responsibility
- venue-specific market-session truth should come from the same venue-integration boundary as
  refdata, not from a blanket host-wide assumption that every venue is always open

Recommended host behavior:

- if a venue adaptor provides session/calendar data, use it
- if manifest metadata indicates session-constrained operation, do not synthesize a default
  `open` state
- only explicit always-open venues should use the trivial default-open path

## gRPC Query Service

The refdata service should expose a gRPC query interface for canonical lookups.

Recommended query families:

- query by `instrument_id`
- query by `(venue, instrument_exch)`
- list/filter instruments by venue, asset, type, or enabled state
- query refdata version/update watermark
- query market status by venue/market
- query market session calendar or next open/close window

The query service should read from PostgreSQL, optionally fronted by an in-process cache, but
PostgreSQL remains the authority.

Query freshness rule:

- queries should return the newest canonical state available at query time on a best-effort basis
- during an in-flight refresh, callers may observe partially updated newest state rather than a
  separate snapshot transaction model
- "no change" should remain "no change"; unchanged rows should not be churned just to mark refresh

## Progressive Disclosure

The refdata service should support progressive disclosure so different consumers can request
different levels of detail from the same canonical source.

Why this matters:

- Pilot often needs compact operational views
- SDK/runtime code usually needs a focused canonical instrument record
- AI/operator tooling may need richer explanatory or expanded metadata views

Recommended model:

- default query responses should stay compact and canonical
- callers may request richer disclosure levels or expansion flags
- richer views should be derived from the same canonical PostgreSQL state, not from a separate
  shadow store

Suggested disclosure levels:

- `BASIC`
  - core instrument identity, venue mapping, enabled/disabled status, and primary trading params
- `EXTENDED`
  - `BASIC` plus extra properties, lifecycle metadata, market-session context, and normalized venue
    annotations
- `FULL`
  - `EXTENDED` plus operator/AI-oriented explanatory fields, provenance, and optional related-market
    or alias information where available

Design rules:

- progressive disclosure changes response richness, not source-of-truth ownership
- SDK hot-path lookups should usually use `BASIC` or a small focused shape and should load on demand
- Pilot and AI-oriented readers may request `EXTENDED` or `FULL`
- expanded views should remain bounded and queryable; avoid turning the refdata service into an
  unstructured document dump

## Registration

If the query service is deployed as a shared runtime, it should register in KV using the generic
service discovery contract.

Recommended registration shape:

- `svc.refdata.<logical_id>`
- `service_type = "refdata"`
- gRPC endpoint in the generic transport field
- capabilities such as:
  - `query_instrument_refdata`
  - `query_refdata_by_venue_symbol`
  - `list_instruments`
  - `query_refdata_watermark`
  - `query_market_status`
  - `query_market_calendar`

## Cache Distribution To SDK

The trading SDK needs local refdata lookup anyway for:

- RTMD subject construction
- venue symbol resolution
- client-facing instrument metadata lookup

Recommended SDK model:

- SDK keeps a local in-memory refdata cache
- SDK always loads refdata entries from the refdata gRPC service
- refdata change notifications are used to invalidate or deprecate cached entries, not to push full
  replacement data
- after invalidation, the SDK actively reloads the canonical data itself from the refdata gRPC
  service
- SDK loads refdata on demand rather than relying on mandatory bulk preload
- progressive disclosure lets Pilot/manager tooling load broader market views when needed without
  forcing the hot-path SDK to do the same

Recommended invalidation path:

- refdata service emits a control topic such as `zk.control.refdata.updated`
- payload carries enough information to support selective refresh, for example:
  - `instrument_id`
  - venue
  - change class such as `invalidate` or `deprecate`
  - update watermark/version
- SDK marks the local entry invalid or deprecated
- SDK then actively refreshes the affected entry from the refdata gRPC service

For market status:

- refdata service should emit a control topic such as `zk.control.market_status.updated`
- payload should identify the venue/market and the new session state
- SDK or services that care about session gating should refresh or update their local market-status
  cache accordingly

The SDK should not treat Pilot as the primary runtime path for refdata lookup.

Recommended client shape:

- the trading stack should expose a dedicated `RefdataSdk`
- `TradingClient` should use `RefdataSdk` directly for instrument and market-status lookup
- refdata-specific cache and invalidation logic should live in `RefdataSdk`, not be reimplemented
  ad hoc in each higher-level client

Shared-service rule:

- refdata should always be a shared service
- SDKs and runtime services should query the shared refdata service rather than embedding separate
  canonical refdata authorities

## Operational Notes

- a full refresh job and a targeted venue refresh should both be supported
- refresh should be idempotent at the service level even if venue fetches are not
- change publication should happen only after PostgreSQL commits the new canonical state
- market session transitions should be published from canonical state changes, not inferred
  independently by each consumer
- if market status becomes stale, the caller decides how to react; the mapping entry itself should
  not be removed merely because freshness is poor

## Draft Event Schema

Recommended shape for `zk.control.refdata.updated`:

- `event_type`
  - `refdata_updated`
- `instrument_id`
- `venue`
- `change_class`
  - `invalidate`
  - `deprecate`
- `watermark`
- `updated_at_ms`

Recommended shape for `zk.control.market_status.updated`:

- `event_type`
  - `market_status_updated`
- `venue`
- `market`
- `session_state`
  - `open`
  - `closed`
  - `halted`
  - `pre_open`
  - `special`
- `effective_at_ms`
- `updated_at_ms`

## TODO

- add provenance/fetch metadata model
- define watermark semantics in more detail
- evaluate whether a separate `cfg.instrument_refdata_history` table is worthwhile
- add alias/symbol-rename handling
- add corporate-action / roll-chain handling where needed
- consider a dedicated refdata SDK helper/client
- define whether Pilot proxies refdata queries for operator/UI convenience or always delegates to the
  refdata service
- add health/SLO and staleness policy details

## Related Docs

- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [SDK](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/sdk.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
