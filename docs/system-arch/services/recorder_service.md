# Recorder Service

## Scope

The recorder service consumes OMS, strategy, and RTMD events and writes durable history and
analytical state.

Implementation direction:

- service: `zk-recorder-svc`
- stack: `Rust`
- role: stateless high-throughput ingest

## Design Role

Responsibilities:

- Postgres-first persistence for OMS orders, fills, balances, positions, and strategy execution data
- append-only recording for trades and strategy events
- current-state upserts for OMS orders and execution status
- optional raw event/log archival in MongoDB for debugging and short retention

Non-responsibilities:

- reconciliation scheduling
- backfill orchestration
- operator-triggered repair workflows
- ad hoc post-trade exception handling

## Data Ownership

Recorder writes:

- PostgreSQL canonical operational history
- MongoDB raw event history only when explicitly enabled

It consumes NATS event streams and is not part of service discovery routing decisions beyond its
own optional runtime registration.

The recorder is the canonical hot-path writer. It should persist facts first and leave later repair
and workflow logic to the post-trade service.

## Current Reference Implementation

The current Python services split recorder responsibilities across several flows:

- [`oms_recorder.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/oms_recorder.py):
  records `OrderUpdateEvent.last_trade` and `last_fee`, writes Mongo first, and only writes a
  subset of trades into Postgres.
- [`order_event_recorder.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/order_event_recorder.py):
  stores raw OMS order update events into Mongo with TTL retention.
- [`post_trade_handler.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/post_trade_handler.py):
  waits for terminal state, reads the Redis order hash, and upserts an order snapshot into Mongo.
- [`oms_balance_collector.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/oms_balance_collector.py):
  periodically queries balances and positions and stores them into Redis.
- [`tq_strat_recorder.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_app/tq_strat_recorder.py):
  records strategy logs, lifecycle events, cancels, orders, and optionally trades into Mongo.
- [`trade_update_monitor.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/trade_update_monitor.py)
  and [`trade_backfiller.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/trade_backfiller.py):
  reconcile terminal orders against recorded fills and backfill missing trades from the gateway.

This split is workable but creates three structural problems:

- Mongo is still the source of truth for most order and strategy history.
- Postgres writes are partial and best-effort rather than canonical and idempotent.
- Reconciliation depends on Mongo-specific collections rather than canonical relational state.

## Proposed Direction

Use Postgres as the canonical store for recorder and post-trade state.

- OMS orders and trades live in Postgres.
- Strategy executions, strategy orders, lifecycle events, and derived execution activity live in
  Postgres.
- Mongo becomes optional for raw event payloads and logs, with retention and debugging as the main
  use cases.

This keeps the query path for Pilot, account views, and recon in one durable store and aligns with
the existing Java test schema in [`V1__schema.sql`](/Users/zzk/workspace/zklab/zkbot/java/src/test/resources/db/migration-test/V1__schema.sql)
and execution APIs in [`ExecutionRepository.java`](/Users/zzk/workspace/zklab/zkbot/java/src/main/java/com/zkbot/pilot/bot/ExecutionRepository.java).

## Service Boundary

The recorder is one side of the post-trade architecture, not the whole domain.

Target split:

- recorder service:
  Rust, queue-consumer, canonical writer
- post-trade service:
  Java, scheduler/operator workflows, recon, repair, and backfill orchestration

The recorder must emit or persist enough canonical state that the post-trade service never needs to
reconstruct truth from Mongo or Redis.

### 1. OMS Recorder

Consumes OMS order update subjects and persists canonical OMS state into Postgres.

Responsibilities:

- upsert current order state
- insert fills idempotently
- persist fee detail together with fills or in a dedicated fee table when no fill is present
- persist balance and position snapshots if/when those streams are moved off Redis
- enrich rows with account, gateway, venue, and instrument metadata

Input streams:

- `zk.oms.<oms_id>.order_update.<account_id>` or the current equivalent subject
- optional balance and position update streams

### 2. Strategy Recorder

Consumes strategy events and persists execution-facing state into Postgres.

Responsibilities:

- record strategy order intent
- record cancel intent
- record lifecycle events
- record logs if logs are kept in Postgres
- maintain `source_id -> execution_id/strategy_id` linkage used to enrich OMS orders and trades

Input streams:

- `tq.strategy.order.*`
- `tq.strategy.cancel.*`
- `tq.strategy.lifecycle.*`
- `tq.strategy.log.*`

Recon is intentionally not part of this service. See
[Post-Trade Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/post_trade_service.md).

## Storage Model

### Canonical Tables

Reuse the existing split between `cfg` and `trd` schemas.

Already aligned with this design:

- `cfg.strategy_instance`
- `trd.trade_oms`
- `trd.trade_recon`
- `trd.balance_snapshot`
- `trd.position_snapshot`

Recommended additions:

- `trd.order_oms`
  Current OMS order state keyed by `order_id`, with `account_id`, `oms_id`, `strategy_id`,
  `execution_id`, `source_id`, `instrument_id`, `ext_order_ref`, requested qty/price, filled qty,
  average fill price, status, timestamps, and raw snapshot jsonb.
- `trd.order_event_oms`
  Optional append-only order-update event table for short or medium retention. This replaces the
  current Mongo TTL collection if full auditability in Postgres is desired.
- `bot.strategy_order_event`
  Append-only strategy order intent table.
- `bot.strategy_cancel_event`
  Append-only strategy cancel table.
- `bot.strategy_lifecycle_event`
  Append-only lifecycle table.
- `bot.strategy_log_event`
  Optional log table. If log volume is high or ad hoc, keep this in Mongo first and defer the PG
  migration.
- `bot.source_binding`
  Maps `source_id` to `strategy_id`, `execution_id`, and `oms_id`. This is the join point between
  strategy-originated orders and OMS-executed fills.

Recorder write ownership:

- `trd.order_oms`
- `trd.trade_oms`
- `trd.balance_snapshot`
- `trd.position_snapshot`
- strategy event tables
- `bot.source_binding`

### Why Two Shapes

The recorder should keep both:

- append-only facts for trades and strategy events
- current-state rows for orders and execution state

The append-only side supports audit, replay, and debugging. The current-state side supports fast
UI/API queries and reconciliation scans.

## Linking Strategy and OMS Data

The key gap today is that strategy events know `execution_id`, but OMS trade persistence is driven
from order updates and Redis snapshots.

Proposed linkage:

- when the strategy recorder persists a `StrategyOrderEvent`, it also upserts `bot.source_binding`
  keyed by `source_id`
- the OMS recorder resolves `source_id` on each order update and fills in `strategy_id` and
  `execution_id` on `trd.order_oms` and `trd.trade_oms`

This is more reliable than trying to infer execution ownership later from time windows or account
joins.

## Order Recording Design

`post_trade_handler.py` currently waits, reads Redis, and writes the order snapshot to Mongo
because Redis holds fields not guaranteed to be present in the order update event.

For the new design, use one of two modes:

### Preferred

Publish a richer terminal order snapshot from OMS so the recorder does not need Redis reads.

### Compatibility Mode

Keep a small terminal-order hydrator that:

- listens for terminal order updates
- waits a short delay
- reads the Redis order hash
- upserts `trd.order_oms` with `gw_request`, `oms_request`, `oms_trades`, error messages, and raw
  snapshot payloads

The hydrator should enrich Postgres only. Mongo should not remain the canonical sink.

## Trade Recording Design

Trades should remain append-only and idempotent.

Insert rule:

- prefer unique key `(account_id, ext_trade_id)` when `ext_trade_id` is present
- otherwise fall back to a deterministic fingerprint from `(account_id, order_id, filled_ts,
  filled_qty, filled_price, side)`

Each fill row should include:

- `order_id`, `account_id`, `oms_id`
- `strategy_id`, `execution_id`, `source_id`
- `instrument_id`, `instrument_exch`
- `side`, `fill_type`
- `filled_qty`, `filled_price`, `filled_ts`
- fee summary and raw fee detail jsonb
- raw event jsonb or original payload jsonb when useful
- `is_backfilled` or equivalent provenance metadata

## Handoff To Post-Trade Service

The recorder must persist enough canonical state for the Java post-trade service to operate without
using Mongo or Redis as truth.

Required handoff fields:

- terminal order status and `filled_qty`
- `ext_order_ref`
- `instrument_id` and `instrument_exch`
- `account_id`, `oms_id`
- `source_id`, `strategy_id`, `execution_id`
- raw order snapshot jsonb for compatibility gaps

The recorder may set an initial recon marker on terminal orders, but it should not own scan,
repair, or backfill workflows.

## HA And Scaling

Recorder HA is based on stateless replicas and idempotent writes.

- multiple replicas consume via NATS queue groups
- duplicate delivery is expected and must be harmless
- unique keys and `upsert` semantics prevent duplicate trades/orders
- Postgres is canonical; in-memory caches are advisory only
- Redis may be used temporarily for enrichment compatibility, not truth

Failure model:

- if a replica crashes mid-message, replay must not create duplicate fills
- if enrichment metadata is unavailable, persist the core fact and mark enrichment partial rather
  than dropping the event

## Mongo Usage

Mongo becomes optional rather than primary.

Keep Mongo only for:

- raw protobuf/json event archival
- high-volume logs where retention is short and indexing/query requirements are loose
- emergency debugging during the migration period

Do not require Mongo for:

- order history
- trade history
- strategy execution queries
- reconciliation

## Query Surface

This design directly supports the currently stubbed Pilot APIs:

- execution activities
- execution lifecycles
- execution logs
- strategy logs
- account activities

Those endpoints can be backed by Postgres instead of the current Mongo-only recorder path.

## Rollout Plan

### Phase 1

- add canonical Postgres tables
- implement idempotent OMS trade recorder into Postgres
- implement strategy event recorder into Postgres
- maintain current Mongo writes in parallel

### Phase 2

- add `trd.order_oms` upsert flow
- keep Redis hydration only for fields not yet present in OMS events
- add `bot.source_binding` so fills can be linked to execution and strategy

### Phase 3

- port recon scans and gateway backfill to Postgres
- hand recon ownership to the Java post-trade service
- mark Postgres as the canonical source for order/trade consistency checks
- switch account and execution history APIs to Postgres

### Phase 4

- make Mongo writes optional behind config
- keep only raw event/log archival if still needed

## Open Decisions

- Whether strategy logs belong in Postgres on day one, or remain in Mongo until log volume and
  retention requirements are clearer.
- Whether OMS should publish a richer terminal snapshot so Redis hydration can be removed.
- Whether order event retention belongs in Postgres or stays in Mongo TTL storage for lower cost.
- Whether balance and position snapshots move directly to Postgres now or after the order/trade
  migration is stable.

## Related Docs

- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
- [Post-Trade Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/post_trade_service.md)
