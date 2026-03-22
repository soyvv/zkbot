# Post-Trade Service

## Scope

The post-trade service owns correctness workflows after the canonical event write path.

Implementation direction:

- service: `zk-post-trade-svc`
- stack: `Java`
- role: scheduled and operator-driven post-trade workflows

It reads canonical post-trade state from Postgres, validates it against external truth when needed,
and writes reconciliation and repair outcomes back to Postgres.

## Design Role

Responsibilities:

- reconciliation of terminal OMS orders against recorded fills
- gateway/exchange trade backfill orchestration
- post-trade repair workflows for missing or mismatched fills
- terminal-order hydration or exception repair if OMS event payloads are incomplete
- operator-facing post-trade APIs and job controls
- recording recon runs, outcomes, and repair provenance

Non-responsibilities:

- hot-path event ingestion
- primary streaming persistence from NATS
- acting as the source of truth for order/trade facts before they are recorded

## Relationship To Recorder

The recorder and post-trade service are separate service boundaries with different workload shapes.

- recorder service:
  Rust, stateless, high-throughput, queue-consumer, canonical writer
- post-trade service:
  Java, lower-throughput, scheduled/operator workflows, repair and exception handling

The post-trade service depends on the recorder to persist canonical state into Postgres first.
It should not reconstruct truth from Mongo collections or Redis hashes except during a temporary
migration compatibility phase.

## Current Reference Implementation

The current Python post-trade logic is spread across:

- [`post_trade_handler.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/post_trade_handler.py)
  for terminal-order hydration from Redis into Mongo
- [`trade_update_monitor.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/trade_update_monitor.py)
  for periodic mismatch detection
- [`trade_backfiller.py`](/Users/zzk/workspace/zklab/zkbot/services/zk-service/src/tq_service_oms/trade_backfiller.py)
  for gateway trade fetch and replacement

The new service consolidates those responsibilities behind Postgres-first workflows.

## Canonical Inputs

Primary inputs are Postgres tables written by the recorder:

- `trd.order_oms`
- `trd.trade_oms`
- `bot.source_binding`
- strategy event tables

Optional direct inputs:

- gateway RPC/query endpoints for order trade history
- OMS query APIs for open/terminal order details if a repair workflow needs confirmation

Mongo should be treated as optional debug storage, not an input dependency for correctness.

## Core Workflows

### 1. Trade Reconciliation

Validate recent terminal orders against recorded fills.

Loop:

1. scan recent terminal rows in `trd.order_oms`
2. aggregate recorded fills in `trd.trade_oms` by `order_id`
3. compare aggregated filled quantity against order filled quantity
4. mark matched rows as reconciled
5. escalate missing or mismatched rows into a repair step

Recommended order-level fields:

- `trade_recon_status`
- `trade_recon_reason`
- `last_reconciled_at`
- `last_recon_run_id`

Recommended statuses:

- `PENDING`
- `MATCHED`
- `MISSING_FILL`
- `QTY_MISMATCH`
- `BACKFILLED`
- `FAILED`

### 2. Gateway Trade Backfill

When recon detects missing or inconsistent fills:

1. query gateway trades using `ext_order_ref` and `instrument_exch`
2. normalize venue trade data into canonical trade rows
3. idempotently insert missing fills into `trd.trade_oms`
4. record repair outcome in `trd.trade_recon`
5. update order-level recon status

Default rule:

- do not delete and replace canonical trade history unless a venue forces that model and the
  replacement is recorded explicitly

Preferred repair rule:

- insert missing fills idempotently
- annotate repaired rows with provenance such as `is_backfilled`, `repair_source`, or
  `recon_run_id`

### 3. Terminal Order Hydration

If OMS events do not yet contain all fields needed for `trd.order_oms`, the post-trade service may
run a compatibility workflow that hydrates terminal orders from Redis or OMS query APIs.

This is transitional.

Preferred end state:

- OMS emits a rich enough terminal order snapshot
- recorder writes `trd.order_oms` directly
- post-trade no longer depends on Redis reads

### 4. Operator Repair APIs

The service may expose APIs or internal jobs for:

- re-run recon for an order, account, OMS, or time range
- backfill one order by `order_id`
- backfill all terminal mismatches in a time window
- inspect recon runs and repair outcomes

## Storage Ownership

The post-trade service should own:

- `trd.trade_recon`
- recon run metadata table such as `trd.recon_run`
- optional repair job table if workflows become durable jobs
- updates to recon status fields on `trd.order_oms`

It may also write repaired rows into:

- `trd.trade_oms`

with explicit provenance metadata.

It should not be the primary owner of:

- raw strategy event tables
- initial `trd.order_oms` and `trd.trade_oms` ingestion

## HA Model

The post-trade service has two HA modes.

### API/Read Path

- multiple replicas are fine
- stateless reads and operator-triggered writes should be safe behind normal service load balancing

### Scheduler/Worker Path

Only one scheduler should drive a given recurring recon workload at a time.

Recommended pattern:

- all replicas start
- recurring jobs acquire a Postgres advisory lock before running
- only the lock holder executes the scan
- if the leader dies, another replica acquires the lock and continues on the next interval

For higher scale, shard by a stable key:

- `account_id`
- `oms_id`
- venue

and assign one advisory lock per shard.

## Failure Model

The service must assume:

- recorder can deliver duplicate facts safely
- gateway queries may timeout or return partial trade history
- repairs may be retried

Therefore:

- recon runs must be resumable
- backfill writes must be idempotent
- run status must be persisted
- partial failures must not lose the mismatch signal

## Recommended Tables

Existing aligned table:

- `trd.trade_recon`

Recommended additions:

- `trd.recon_run`
  Stores one row per scheduled or operator-triggered recon run, including scope, status,
  counters, timestamps, and error summary.
- `trd.repair_job`
  Optional durable job table for manual backfill/repair requests if jobs become asynchronous and
  operator-visible.

## Integration With Pilot

Pilot and account APIs should read canonical Postgres state produced by recorder and post-trade.

Likely consumers:

- execution activity views
- account activity views
- recon status views
- repair action endpoints

The post-trade service may remain internal at first, with Pilot proxying selected workflows later.

## Rollout Plan

### Phase 1

- define `trd.order_oms` and recon status fields
- keep recorder as the only canonical ingest path
- add Java post-trade service skeleton with advisory-lock-based scheduler

### Phase 2

- move trade mismatch scans from Python monitor logic to Java
- move gateway trade backfill orchestration to Java
- write `trd.trade_recon` and order-level status updates in Postgres

### Phase 3

- add operator-triggered repair APIs
- remove Mongo/Redis dependencies from normal recon flows
- keep Redis-only hydration as temporary compatibility logic if still required

### Phase 4

- retire the old Python `trade_update_monitor.py`, `trade_backfiller.py`, and terminal Mongo repair
  path

## Open Decisions

- Whether terminal-order hydration belongs in the post-trade service long term or should be removed
  entirely once OMS events are richer.
- Whether operator-triggered repairs should be synchronous API calls or durable async jobs.
- Whether recon should run globally or by shard from day one.

## Related Docs

- [Recorder Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/recorder_service.md)
- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
- [Architecture Reconciliation Notes](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/reconcile.md)
