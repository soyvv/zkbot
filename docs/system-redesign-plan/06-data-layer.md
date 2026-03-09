# ZKBot Data Layer Design

PostgreSQL schema, MongoDB event store, and Redis usage.

## 1. Storage Roles

| Store | Role |
|---|---|
| PostgreSQL | authoritative relational state: config, trades (raw + reconciled), balance/position snapshots, refdata, monitoring |
| MongoDB | append-first immutable event store: audit, replay, strategy logs |
| Redis | fast cache + non-critical read replica: OMS open orders, recent closed orders, positions, RTMD orderbook, counters |
| NATS KV | live service registry only (see [05-api-contracts.md](05-api-contracts.md)) |

**OMS writes to Redis on every state transition** (order placed, booked, filled, cancelled, balance updated).
On startup, OMS warm-loads from Redis, then reconciles with live gateway state via `GatewayService.QueryOrder` / `QueryBalance`.
Redis is the non-critical read replica for UI/Pilot/ops tooling (open orders, positions, balances).

The **Recorder** service writes trade records and periodic snapshots to PostgreSQL from NATS events.
Post-trade Redis cleanup (evicting terminal orders) is done by the Recorder on terminal `OrderUpdateEvent` (FILLED / CANCELLED / REJECTED).

## 2. PostgreSQL Schema

Schema namespaces:
- `cfg` — configuration metadata
- `trd` — transactional trading state
- `mon` — monitoring and SLO data

### 2.1 Config tables (`cfg`)

```sql
create schema if not exists cfg;

-- OMS instances
create table cfg.oms_instance (
  oms_id       text primary key,
  namespace    text not null,
  description  text,
  enabled      boolean not null default true,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now()
);

-- Gateway instances
-- Migrated from MongoDB refdata_gw_config
create table cfg.gateway_instance (
  gw_id                         text primary key,
  venue                         text not null,
  broker_type                   text not null,
  account_type                  text not null,
  supports_batch_order          boolean not null default false,
  supports_batch_cancel         boolean not null default false,
  supports_order_query          boolean not null default true,
  supports_position_query       boolean not null default true,
  supports_trade_history_query  boolean not null default true,
  supports_fee_query            boolean not null default true,
  cancel_required_fields        text[] not null default '{}',
  enabled                       boolean not null default true,
  created_at                    timestamptz not null default now(),
  updated_at                    timestamptz not null default now()
);

-- Accounts
create table cfg.account (
  account_id       bigint primary key,
  exch_account_id  text not null,
  venue            text not null,
  broker_type      text not null,
  account_type     text not null,
  base_currency    text,
  status           text not null default 'ACTIVE',  -- ACTIVE | SUSPENDED | CLOSED
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

-- Account ↔ OMS ↔ gateway binding
create table cfg.account_binding (
  account_id    bigint not null references cfg.account(account_id),
  oms_id        text not null references cfg.oms_instance(oms_id),
  gw_id         text not null references cfg.gateway_instance(gw_id),
  startup_sync  boolean not null default true,
  priority      int not null default 100,
  enabled       boolean not null default true,
  primary key (account_id, oms_id, gw_id)
);

-- Instrument refdata
-- Replaces MongoDB refdata_instrument_basic + ODS query_instrument_refdata
create table cfg.instrument_refdata (
  instrument_id      text primary key,
  instrument_exch    text not null,
  venue              text not null,
  instrument_type    text not null,  -- SPOT | PERP | FUTURE | ...
  base_asset         text,
  quote_asset        text,
  settlement_asset   text,
  contract_size      numeric,
  price_tick_size    numeric,
  qty_lot_size       numeric,
  min_notional       numeric,
  max_notional       numeric,
  min_order_qty      numeric,
  max_order_qty      numeric,
  max_mkt_order_qty  numeric,
  price_precision    int,
  qty_precision      int,
  disabled           boolean not null default false,
  extra_properties   jsonb not null default '{}'::jsonb,
  updated_at         timestamptz not null default now()
);

create index idx_instrument_refdata_venue_exch
  on cfg.instrument_refdata(venue, instrument_exch);

-- Per-account instrument config (OMS risk/trading limits)
-- Replaces MongoDB refdata_account_mapping + oms_config
create table cfg.account_instrument_config (
  account_id          bigint not null references cfg.account(account_id),
  instrument_id       text not null references cfg.instrument_refdata(instrument_id),
  max_position        numeric,
  max_order_qty       numeric,
  max_open_orders     int,
  tick_size_override  numeric,
  lot_size_override   numeric,
  enabled             boolean not null default true,
  extra_config        jsonb not null default '{}'::jsonb,
  updated_at          timestamptz not null default now(),
  primary key (account_id, instrument_id)
);

-- OMS risk params per account
create table cfg.oms_risk_config (
  account_id             bigint not null references cfg.account(account_id),
  oms_id                 text not null references cfg.oms_instance(oms_id),
  max_daily_notional     numeric,
  max_net_position       numeric,
  max_order_rate_per_s   int,
  panic_on_reject_count  int,    -- trigger panic after N consecutive rejects
  extra_config           jsonb not null default '{}'::jsonb,
  updated_at             timestamptz not null default now(),
  primary key (account_id, oms_id)
);

-- Strategy definitions
create table cfg.strategy_definition (
  strategy_id       text primary key,
  runtime_type      text not null,           -- RUST | PY_EMBEDDED | PY_WORKER
  code_ref          text not null,           -- module path or binary reference
  description       text,
  default_accounts  bigint[] not null default '{}',
  default_symbols   text[] not null default '{}',
  config_json       jsonb not null default '{}'::jsonb,
  enabled           boolean not null default true,
  created_at        timestamptz not null default now(),
  updated_at        timestamptz not null default now()
);

-- Strategy instances (live executions)
create table cfg.strategy_instance (
  execution_id     text primary key,
  strategy_id      text not null references cfg.strategy_definition(strategy_id),
  target_oms_id    text not null references cfg.oms_instance(oms_id),
  status           text not null,  -- INITIALIZING | RUNNING | PAUSED | STOPPED | ERROR
  error_message    text,
  started_at       timestamptz,
  ended_at         timestamptz,
  config_override  jsonb not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create index idx_strategy_instance_strategy on cfg.strategy_instance(strategy_id);
create index idx_strategy_instance_status   on cfg.strategy_instance(status);
```

### 2.2 Registration control tables (`cfg`)

These tables support Pilot-managed topology and token-gated bootstrap over NATS request/reply.

```sql
-- Logical runtime instances managed by Pilot
create table cfg.logical_instance (
  logical_id       text primary key,            -- gw_id / oms_id / engine_id / composite
  instance_type    text not null,               -- GW | OMS | ENGINE | OMS_WITH_GW | AIO
  env              text not null,
  tenant           text,
  enabled          boolean not null default true,
  metadata         jsonb not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

-- Allowed topology edges
create table cfg.logical_binding (
  binding_id       bigserial primary key,
  src_type         text not null,               -- ENGINE | OMS | STRATEGY
  src_id           text not null,
  dst_type         text not null,               -- OMS | GW | ENGINE
  dst_id           text not null,
  enabled          boolean not null default true,
  metadata         jsonb not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create index idx_logical_binding_src on cfg.logical_binding(src_type, src_id);
create index idx_logical_binding_dst on cfg.logical_binding(dst_type, dst_id);

-- Issued bootstrap tokens (store hash/jti only, never raw token)
create table cfg.instance_token (
  token_jti        text primary key,
  logical_id       text not null references cfg.logical_instance(logical_id),
  instance_type    text not null,
  token_hash       text not null,
  status           text not null,               -- ACTIVE | REVOKED | EXPIRED
  expires_at       timestamptz not null,
  issued_at        timestamptz not null default now(),
  metadata         jsonb not null default '{}'::jsonb
);

create index idx_instance_token_logical on cfg.instance_token(logical_id, status);

-- MDGW subscription management — written by Pilot admin UI
-- MDGW reads this table on startup and on zk.control.mdgw.<venue>.reload
create table cfg.mdgw_subscription (
  subscription_id  bigserial primary key,
  venue            text not null,
  instrument_exch  text not null,
  channel_types    text[] not null default '{}',  -- TICK | KLINE | FUNDING | ORDERBOOK
  kline_intervals  text[] not null default '{}',  -- 1m, 5m, 1h etc. (only when KLINE in channel_types)
  enabled          boolean not null default true,
  requested_by     text,                          -- strategy_id or operator
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now(),
  unique (venue, instrument_exch)
);

create index idx_mdgw_subscription_venue on cfg.mdgw_subscription(venue, enabled);
```

### 2.3 Trading state tables (`trd`)

There are no PostgreSQL live order/balance/position tables for OMS runtime state.
OMS runtime state is in-memory, warm-started from Redis, then reconciled against gateway state.

Two trade tables exist — one for raw OMS-generated trades, one for reconciled trades produced by the
reconciliation job. Hourly balance and position snapshots are written by the Recorder for analytical use.

```sql
create schema if not exists trd;

-- Raw OMS-generated trades
-- Written by Recorder from oms.OrderUpdateEvent NATS stream when last_trade is present
-- Represents OMS's view of fills as they arrive from the gateway
create table trd.trade_oms (
  fill_id        bigserial primary key,
  order_id       bigint not null,          -- order ID (Snowflake; generated by SDK/client or OMS utility)
  account_id     bigint not null,
  oms_id         text not null,
  gw_id          text,
  strategy_id    text,
  execution_id   text,
  instrument_id  text not null,
  instrument_exch text,
  side           text not null,            -- BUY | SELL
  open_close     text,                     -- OPEN | CLOSE
  fill_type      text,                     -- TRADE | FEE | LINKAGE
  ext_trade_id   text,                     -- exchange-assigned trade ID
  filled_qty     numeric not null,
  filled_price   numeric not null,
  filled_ts      timestamptz not null,
  fee_total      numeric,
  fee_detail     jsonb,
  source_id      text,
  created_at     timestamptz not null default now()
);

create unique index idx_trade_oms_ext_trade on trd.trade_oms(account_id, ext_trade_id)
  where ext_trade_id is not null;
create index idx_trade_oms_account_time  on trd.trade_oms(account_id, filled_ts desc);
create index idx_trade_oms_order         on trd.trade_oms(order_id);
create index idx_trade_oms_execution     on trd.trade_oms(execution_id);

-- Reconciled trades
-- Written by the Reconciliation service after matching OMS trades against exchange trade history
-- Contains resolved_pnl and authoritative ext_trade_id linkage
-- A recon trade may supersede, correct, or supplement an OMS trade
create table trd.trade_recon (
  recon_fill_id  bigserial primary key,
  fill_id        bigint references trd.trade_oms(fill_id),  -- nullable: missing in OMS but found on exchange
  account_id     bigint not null,
  oms_id         text,
  instrument_id  text not null,
  instrument_exch text,
  side           text not null,
  ext_trade_id   text not null,            -- authoritative exchange trade ID
  filled_qty     numeric not null,
  filled_price   numeric not null,
  filled_ts      timestamptz not null,
  fee_total      numeric,
  fee_detail     jsonb,
  realized_pnl   numeric,
  recon_status   text not null,            -- MATCHED | OMS_ONLY | EXCH_ONLY | AMENDED
  recon_run_id   bigint not null,          -- links to mon.recon_run
  created_at     timestamptz not null default now()
);

create unique index idx_trade_recon_ext on trd.trade_recon(account_id, ext_trade_id);
create index idx_trade_recon_account_time on trd.trade_recon(account_id, filled_ts desc);
create index idx_trade_recon_run         on trd.trade_recon(recon_run_id);

-- Hourly balance snapshot (per asset)
-- Written by Recorder on the hour from oms.PositionUpdateEvent stream
create table trd.balance_snapshot (
  snapshot_id   bigserial primary key,
  account_id    bigint not null,
  asset         text not null,
  total_qty     numeric not null,
  avail_qty     numeric not null,
  frozen_qty    numeric not null,
  snapshot_ts   timestamptz not null,      -- truncated to the hour
  source        text not null default 'OMS',  -- OMS | EXCH | RECON
  created_at    timestamptz not null default now()
);

create unique index idx_balance_snapshot_hour
  on trd.balance_snapshot(account_id, asset, snapshot_ts);
create index idx_balance_snapshot_account_time
  on trd.balance_snapshot(account_id, snapshot_ts desc);

-- Hourly position snapshot (per instrument + side)
-- Written by Recorder on the hour from oms.PositionUpdateEvent stream
create table trd.position_snapshot (
  snapshot_id            bigserial primary key,
  account_id             bigint not null,
  instrument_id          text not null,
  side                   text not null,      -- LONG | SHORT | NET
  instrument_type        text,
  total_qty              numeric not null,
  avail_qty              numeric not null,
  frozen_qty             numeric not null,
  contract_size          numeric,
  contract_size_adjusted boolean not null default false,
  avg_entry_price        numeric,
  unrealized_pnl         numeric,
  margin_info            jsonb,
  snapshot_ts            timestamptz not null,  -- truncated to the hour
  source                 text not null default 'OMS',  -- OMS | EXCH | RECON
  created_at             timestamptz not null default now()
);

create unique index idx_position_snapshot_hour
  on trd.position_snapshot(account_id, instrument_id, side, snapshot_ts);
create index idx_position_snapshot_account_time
  on trd.position_snapshot(account_id, snapshot_ts desc);
```

### 2.4 Monitoring tables (`mon`)

```sql
create schema if not exists mon;

create table mon.service_health (
  service_type  text not null,
  service_id    text not null,
  instance_id   text not null,
  status        text not null,  -- OK | DEGRADED | DOWN
  detail        jsonb not null default '{}'::jsonb,
  observed_at   timestamptz not null default now(),
  primary key (service_type, service_id, instance_id, observed_at)
);

-- One row per reconciliation job run
-- Referenced by trd.trade_recon.recon_run_id
create table mon.recon_run (
  recon_run_id    bigserial primary key,
  recon_type      text not null,  -- TRADE | BALANCE | POSITION
  account_id      bigint,
  oms_id          text,
  period_start    timestamptz not null,
  period_end      timestamptz not null,
  oms_count       int not null default 0,
  exch_count      int not null default 0,
  matched_count   int not null default 0,
  oms_only_count  int not null default 0,
  exch_only_count int not null default 0,
  amended_count   int not null default 0,
  status          text not null,  -- RUNNING | COMPLETE | FAILED
  detail          jsonb not null default '{}'::jsonb,
  run_at          timestamptz not null default now()
);

create index idx_recon_run_account_time on mon.recon_run(account_id, run_at desc);

create table mon.latency_metric (
  metric_id     bigserial primary key,
  service_type  text not null,
  service_id    text not null,
  metric_name   text not null,
  tags          jsonb not null default '{}'::jsonb,
  value_ms      numeric not null,
  observed_at   timestamptz not null default now()
);

-- Active bootstrap sessions used by direct KV registration/heartbeat ownership checks
create table mon.active_session (
  owner_session_id   text primary key,
  logical_id         text not null references cfg.logical_instance(logical_id),
  instance_type      text not null,
  kv_key             text not null,
  lock_key           text not null,
  lease_ttl_ms       int not null,
  status             text not null,            -- ACTIVE | FENCED | EXPIRED | CLOSED
  last_seen_at       timestamptz not null default now(),
  expires_at         timestamptz not null,
  source_addr        text,
  metadata           jsonb not null default '{}'::jsonb
);

create index idx_active_session_logical on mon.active_session(logical_id, status);

-- Registration decision audit for bootstrap and fencing events
create table mon.registration_audit (
  audit_id           bigserial primary key,
  token_jti          text,
  logical_id         text,
  instance_type      text,
  owner_session_id   text,
  decision           text not null,            -- ACCEPTED | REJECTED | FENCED | REISSUED | DEREGISTERED
  reason             text,
  source_addr        text,
  observed_at        timestamptz not null default now(),
  detail             jsonb not null default '{}'::jsonb
);

create index idx_registration_audit_logical_time on mon.registration_audit(logical_id, observed_at desc);
```

## 3. MongoDB Event Store

Database: `zk_events`

### 3.1 Collections and retention

| Collection | Retention |
|---|---|
| `oms_order_update_event` | 90 days |
| `oms_balance_update_event` | 30 days |
| `oms_system_event` | 30 days |
| `strategy_lifecycle_event` | 180 days |
| `strategy_order_event` | 90 days |
| `strategy_cancel_event` | 90 days |
| `strategy_log_event` | 30 days |
| `strategy_signal_event` | 30 days |
| `rtmd_tick_event` | 7 days (sampled) |
| `recon_result_event` | 180 days |
| `monitor_alert_event` | 90 days |

### 3.2 Common event envelope

All documents share these top-level fields:

| Field | Type | Notes |
|---|---|---|
| `_id` | ObjectId | auto-generated |
| `event_type` | string | e.g. `oms_order_update`, `strategy_log` |
| `event_version` | int | schema version for evolution |
| `event_ts` | ISODate | domain event timestamp |
| `ingest_ts` | ISODate | recorder write timestamp |
| `service_id` | string | emitting service |
| `correlation_id` | string | `order_id` or `execution_id` |
| `account_id` | int64 (nullable) | |
| `oms_id` | string (nullable) | |
| `payload` | BSON doc | proto JSON or direct field map |
| `tags` | object | arbitrary search keys |

### 3.3 Indexes (per collection)

```
{ event_type: 1, event_ts: -1 }
{ correlation_id: 1, event_ts: -1 }
{ account_id: 1, event_ts: -1 }
{ ingest_ts: 1 }  -- TTL index with per-collection expireAfterSeconds
```

## 4. Redis Usage

Redis roles:
- OMS warm-start cache for open orders and positions
- non-critical read replica for Pilot/UI reads
- short-retention cache for recently closed orders
- RTMD and operational counters

| Key pattern | Content | Owner | TTL |
|---|---|---|---|
| `oms:<oms_id>:order:<order_id>` | order state snapshot | OMS (writes on every state change) | 1d for open; 1h for terminal |
| `oms:<oms_id>:open_orders:<account_id>` | set of open order IDs | OMS | no TTL (managed set) |
| `oms:<oms_id>:balance:<account_id>:<asset>` | balance snapshot per asset | OMS | no TTL |
| `oms:<oms_id>:position:<account_id>:<instrument>:<side>` | position snapshot | OMS | no TTL |
| `rtmd:orderbook:<venue>:<instrument_exch>` | serialized `OrderBook` snapshot | MDGW | 30s |
| `rate:<service_id>:<op>` | atomic counter for rate limiting | OMS/GW | sliding window |

Write path: OMS is the **sole writer** to all `oms:*` Redis keys. It writes on every state transition (order placed, booked, filled, cancelled; balance/position updated).

Cleanup: The **Recorder** removes terminal orders from `open_orders:<account_id>` sets and lets the 1h TTL on `oms:<oms_id>:order:<order_id>` handle key expiry — this replaces the old `zk.posttrade` topic role.

Startup/read behavior:
- OMS startup reads Redis first (warm cache), then reconciles against gateway (authoritative source).
- Pilot/UI read Redis for low-latency snapshots; fallback to DB/event paths if key is missing.
