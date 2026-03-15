# Phase 9: Recorder + Reconciliation + Monitor

## Goal

Implement three Python services:
1. **Recorder** — subscribes to OMS/strategy NATS events; writes immutable records to MongoDB; writes OMS trades and hourly snapshots to PostgreSQL; handles Redis cleanup for terminal orders (replacing `zk.posttrade`)
2. **Reconciliation** — scheduled jobs that compare OMS trades against exchange trade history; flags discrepancies
3. **Monitor** — subscribes to OMS event streams; evaluates configurable risk/operational rules; sends alerts

## Prerequisites

- Phase 3 complete (OMS gRPC service publishing NATS events)
- Phase 7 complete (Engine publishing strategy NATS events)
- docker-compose stack running (NATS, PG, Mongo, Redis)

## Deliverables

### 7.1 Recorder service

Location: `zkbot/services/zk-recorder/`

Reference implementations:
- `tq_service_app/tq_strat_recorder.py`
- `tq_service_oms/oms_recorder.py`
- `tq_service_oms/order_event_recorder.py`
- `tq_service_oms/oms_balance_collector.py`
- `tq_service_oms/post_trade_handler.py`

#### NATS subscriptions

| Topic | Handler |
|---|---|
| `zk.oms.*.order_update.*` | `handle_order_update` |
| `zk.oms.*.balance_update.*` | `handle_balance_update` |
| `zk.oms.*.position_update.*` | `handle_position_update` |
| `zk.strategy.*.log` | `handle_strategy_log` |
| `zk.strategy.*.lifecycle` | `handle_strategy_lifecycle` |
| `zk.strategy.*.order` | `handle_strategy_order` |

#### `handle_order_update(event: OrderUpdateEvent)`

1. write to MongoDB `zk_events.order_update`:
   ```json
   {"event_type": "order_update", "oms_id": ..., "account_id": ..., "order_id": ...,
    "status": ..., "timestamp_ms": ..., "payload": <full event>}
   ```
2. if `event.last_trade` is present: write to PostgreSQL `trd.trade_oms`:
   ```sql
   INSERT INTO trd.trade_oms (trade_id, order_id, account_id, oms_id, instrument,
     side, qty, price, fee, fee_asset, trade_ts, raw_payload)
   VALUES (...)
   ON CONFLICT (trade_id) DO NOTHING;
   ```
3. if `event.status` is terminal (`FILLED`, `CANCELLED`, `REJECTED`):
   - Redis cleanup (replacing `zk.posttrade`):
     ```python
     redis.expire(f"oms:{oms_id}:order:{order_id}", 3600)  # 1h TTL
     redis.srem(f"oms:{oms_id}:open_orders:{account_id}", order_id)
     ```

#### `handle_balance_update(event: BalanceUpdateEvent)`

1. write to MongoDB `zk_events.balance_update`
2. hourly snapshot logic: if `floor(now / 3600) != floor(last_snapshot_ts / 3600)`:
   ```sql
   INSERT INTO trd.balance_snapshot (snapshot_ts, account_id, asset, available, locked, oms_id)
   VALUES (date_trunc('hour', now()), ...) ON CONFLICT DO NOTHING;
   ```

#### `handle_position_update(event: PositionUpdateEvent)`

1. write to MongoDB `zk_events.position_update`
2. hourly snapshot logic:
   ```sql
   INSERT INTO trd.position_snapshot (snapshot_ts, account_id, instrument, side, qty, avg_cost, oms_id)
   VALUES (...) ON CONFLICT DO NOTHING;
   ```

#### PostgreSQL schema additions

```sql
create table trd.trade_oms (
  trade_id     text primary key,
  order_id     bigint not null,
  account_id   bigint not null,
  oms_id       text not null,
  instrument   text not null,
  side         text not null,
  qty          numeric not null,
  price        numeric not null,
  fee          numeric not null default 0,
  fee_asset    text,
  trade_ts     timestamptz not null,
  raw_payload  jsonb,
  created_at   timestamptz not null default now()
);

create table trd.balance_snapshot (
  snapshot_ts  timestamptz not null,
  account_id   bigint not null,
  asset        text not null,
  available    numeric not null,
  locked       numeric not null,
  oms_id       text not null,
  primary key  (snapshot_ts, account_id, asset)
);

create table trd.position_snapshot (
  snapshot_ts  timestamptz not null,
  account_id   bigint not null,
  instrument   text not null,
  side         text not null,
  qty          numeric not null,
  avg_cost     numeric not null,
  oms_id       text not null,
  primary key  (snapshot_ts, account_id, instrument, side)
);
```

### 7.2 Reconciliation service

Location: `zkbot/services/zk-recorder/recon/`

Reference: `tq_service_oms/post_trade_handler.py`

#### Scheduled job: `run_trade_recon(account_id, start_ts, end_ts)`

1. fetch OMS trades from `trd.trade_oms` for window
2. fetch exchange trade history from gateway `QueryTrades` gRPC
3. match by `(exch_order_ref, trade_id)` — exact match preferred; fuzzy match by `(instrument, qty, price, ~timestamp)` as fallback
4. classify each trade:
   - `MATCHED`: found in both OMS and exchange
   - `OMS_ONLY`: in OMS but not in exchange
   - `EXCH_ONLY`: in exchange but not in OMS
   - `AMENDED`: found in both but fields differ (qty, price, fee)
5. write results to `trd.trade_recon`:
   ```sql
   create table trd.trade_recon (
     recon_id      bigserial primary key,
     recon_run_id  bigint not null references mon.recon_run(run_id),
     trade_id      text,
     exch_trade_id text,
     account_id    bigint not null,
     instrument    text not null,
     recon_status  text not null,   -- MATCHED | OMS_ONLY | EXCH_ONLY | AMENDED
     oms_qty       numeric,
     exch_qty      numeric,
     oms_price     numeric,
     exch_price    numeric,
     discrepancy   jsonb,
     created_at    timestamptz not null default now()
   );

   create table mon.recon_run (
     run_id       bigserial primary key,
     account_id   bigint not null,
     window_start timestamptz not null,
     window_end   timestamptz not null,
     status       text not null,   -- RUNNING | COMPLETED | FAILED
     matched      int not null default 0,
     oms_only     int not null default 0,
     exch_only    int not null default 0,
     amended      int not null default 0,
     started_at   timestamptz not null default now(),
     completed_at timestamptz
   );
   ```
6. log `mon.recon_run` with summary counts
7. if `OMS_ONLY` or `EXCH_ONLY` count > 0: publish notification to `zk.notify.recon_alert`

### 7.3 Monitor service

Location: `zkbot/services/zk-monitor/`

Reference implementations:
- `tq_service_riskmonitor/order_updates_monitor.py`
- `tq_service_riskmonitor/balance_updates_monitor.py`
- `tq_service_riskmonitor/pnl_monitor.py` + `pnl_calculator.py`
- `tq_service_riskmonitor/nats_channel_monitor.py`
- `tq_service_riskmonitor/open_order_monitor.py`

#### Watchers

**Rejection rate watcher** (from `order_updates_monitor.py`):
- sliding window (5min): if rejection rate > configurable threshold → publish `NotificationEvent{REJECTION_RATE_HIGH}`

**Fill rate watcher**:
- if open order older than 30min without fill → publish `NotificationEvent{ORDER_AGING}`

**Channel staleness watcher** (from `nats_channel_monitor.py`):
- if no message on `zk.oms.*.order_update.*` for > 60s → publish `NotificationEvent{CHANNEL_STALE}`

**PnL limit watcher** (from `pnl_monitor.py`):
- per-account realized PnL tracked from trade events
- if daily loss > configured limit → publish `NotificationEvent{PNL_LIMIT_BREACH}` and optionally trigger Panic via OMS gRPC

**Open order aging watcher** (from `open_order_monitor.py`):
- periodically scan open orders (from Redis or OMS query)
- flag orders older than configurable threshold

#### Notification hub

```python
# zk-monitor/notification_hub.py
async def publish_notification(channel: str, event: NotificationEvent):
    await nats.publish(f"zk.notify.{channel}", event.serialize())
    if event.severity >= Severity.HIGH:
        await send_slack_alert(event)
        # optional: PagerDuty for CRITICAL
```

## Tests

### Unit tests

- `test_recorder_order_update_terminal_redis_cleanup`: inject terminal `OrderUpdateEvent(FILLED)` → assert Redis `open_orders` key cleaned up
- `test_recorder_trade_write_on_last_trade`: `OrderUpdateEvent` with `last_trade` set → assert `trd.trade_oms` row inserted
- `test_recorder_balance_snapshot_hourly_boundary`: send two balance updates 1h apart → assert exactly one snapshot row written for each hour
- `test_recon_classify_oms_only`: OMS trade with no matching exchange record → `recon_status=OMS_ONLY`
- `test_recon_classify_matched`: OMS trade matches exchange record → `recon_status=MATCHED`
- `test_monitor_rejection_rate_alert`: inject 10 rejected orders in 5min → assert `NotificationEvent{REJECTION_RATE_HIGH}` published

### Integration tests (docker-compose)

- `test_recorder_full_order_lifecycle`:
  1. place order via OMS, fill via mock-gw
  2. assert MongoDB `zk_events.order_update` has 2 records (BOOKED, FILLED)
  3. assert `trd.trade_oms` has fill record
  4. assert Redis `open_orders` no longer contains order_id
- `test_recorder_hourly_snapshot_writes`:
  1. send balance update events
  2. advance mock clock past hour boundary
  3. assert `trd.balance_snapshot` row written for the hour
- `test_recon_run_full`:
  1. pre-populate `trd.trade_oms` with 3 trades
  2. configure mock-gw to return 2 matching + 1 extra exchange trade
  3. run recon job
  4. assert `mon.recon_run` shows `matched=2, oms_only=1, exch_only=1`
  5. assert `trd.trade_recon` has 4 rows
  6. assert `zk.notify.recon_alert` published
- `test_monitor_channel_stale_alert`:
  1. start monitor with 5s staleness threshold (test override)
  2. send no OMS events for 6s
  3. assert `NotificationEvent{CHANNEL_STALE}` published to `zk.notify.*`

## Exit criteria

- [ ] Recorder subscribes to all required NATS topics on startup
- [ ] `test_recorder_full_order_lifecycle` passes: MongoDB + PG trade write + Redis cleanup all correct
- [ ] `test_recorder_balance_snapshot_writes` passes: hourly dedup correct
- [ ] `test_recon_run_full` passes with correct classification counts
- [ ] `test_monitor_channel_stale_alert` passes within expected time threshold
- [ ] No `zk.posttrade` topic referenced anywhere in new code — cleanup handled by Recorder
- [ ] All Python unit tests pass: `pytest tests/unit/`
- [ ] All integration tests pass: `pytest tests/integration/ --docker`
- [ ] `trd.trade_oms`, `trd.balance_snapshot`, `trd.position_snapshot`, `trd.trade_recon`, `mon.recon_run` tables created in migration
