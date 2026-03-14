-- ZKBot PostgreSQL schema — run once on first container startup.
-- Schemas: cfg (config), trd (trading state), mon (monitoring).

-- ─────────────────────────────────────────────────────────────────────────────
-- cfg schema
-- ─────────────────────────────────────────────────────────────────────────────
create schema if not exists cfg;

create table cfg.oms_instance (
  oms_id       text primary key,
  namespace    text not null,
  description  text,
  enabled      boolean not null default true,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now()
);

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

create table cfg.account (
  account_id       bigint primary key,
  exch_account_id  text not null,
  venue            text not null,
  broker_type      text not null,
  account_type     text not null,
  base_currency    text,
  status           text not null default 'ACTIVE',
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create table cfg.account_binding (
  account_id    bigint not null references cfg.account(account_id),
  oms_id        text not null references cfg.oms_instance(oms_id),
  gw_id         text not null references cfg.gateway_instance(gw_id),
  startup_sync  boolean not null default true,
  priority      int not null default 100,
  enabled       boolean not null default true,
  primary key (account_id, oms_id, gw_id)
);

create table cfg.instrument_refdata (
  instrument_id      text primary key,
  instrument_exch    text not null,
  venue              text not null,
  instrument_type    text not null,
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

create table cfg.oms_risk_config (
  account_id             bigint not null references cfg.account(account_id),
  oms_id                 text not null references cfg.oms_instance(oms_id),
  max_daily_notional     numeric,
  max_net_position       numeric,
  max_order_rate_per_s   int,
  panic_on_reject_count  int,
  extra_config           jsonb not null default '{}'::jsonb,
  updated_at             timestamptz not null default now(),
  primary key (account_id, oms_id)
);

create table cfg.strategy_definition (
  strategy_id       text primary key,
  runtime_type      text not null,
  code_ref          text not null,
  description       text,
  default_accounts  bigint[] not null default '{}',
  default_symbols   text[] not null default '{}',
  config_json       jsonb not null default '{}'::jsonb,
  enabled           boolean not null default true,
  created_at        timestamptz not null default now(),
  updated_at        timestamptz not null default now()
);

create table cfg.strategy_instance (
  execution_id     text primary key,
  strategy_id      text not null references cfg.strategy_definition(strategy_id),
  target_oms_id    text not null references cfg.oms_instance(oms_id),
  status           text not null,
  error_message    text,
  started_at       timestamptz,
  ended_at         timestamptz,
  config_override  jsonb not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create index idx_strategy_instance_strategy on cfg.strategy_instance(strategy_id);
create index idx_strategy_instance_status   on cfg.strategy_instance(status);

create table cfg.logical_instance (
  logical_id       text primary key,
  instance_type    text not null,
  env              text not null,
  tenant           text,
  enabled          boolean not null default true,
  metadata         jsonb not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now()
);

create table cfg.logical_binding (
  binding_id  bigserial primary key,
  src_type    text not null,
  src_id      text not null,
  dst_type    text not null,
  dst_id      text not null,
  enabled     boolean not null default true,
  metadata    jsonb not null default '{}'::jsonb,
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now()
);

create index idx_logical_binding_src on cfg.logical_binding(src_type, src_id);
create index idx_logical_binding_dst on cfg.logical_binding(dst_type, dst_id);

create table cfg.instance_token (
  token_jti     text primary key,
  logical_id    text not null references cfg.logical_instance(logical_id),
  instance_type text not null,
  token_hash    text not null,
  status        text not null,
  expires_at    timestamptz not null,
  issued_at     timestamptz not null default now(),
  metadata      jsonb not null default '{}'::jsonb
);

create index idx_instance_token_logical on cfg.instance_token(logical_id, status);

create table cfg.mdgw_subscription (
  subscription_id  bigserial primary key,
  venue            text not null,
  instrument_exch  text not null,
  channel_types    text[] not null default '{}',
  kline_intervals  text[] not null default '{}',
  enabled          boolean not null default true,
  requested_by     text,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now(),
  unique (venue, instrument_exch)
);

create index idx_mdgw_subscription_venue on cfg.mdgw_subscription(venue, enabled);

-- ─────────────────────────────────────────────────────────────────────────────
-- trd schema
-- ─────────────────────────────────────────────────────────────────────────────
create schema if not exists trd;

create table trd.trade_oms (
  fill_id         bigserial primary key,
  order_id        bigint not null,
  account_id      bigint not null,
  oms_id          text not null,
  gw_id           text,
  strategy_id     text,
  execution_id    text,
  instrument_id   text not null,
  instrument_exch text,
  side            text not null,
  open_close      text,
  fill_type       text,
  ext_trade_id    text,
  filled_qty      numeric not null,
  filled_price    numeric not null,
  filled_ts       timestamptz not null,
  fee_total       numeric,
  fee_detail      jsonb,
  source_id       text,
  created_at      timestamptz not null default now()
);

create unique index idx_trade_oms_ext_trade on trd.trade_oms(account_id, ext_trade_id)
  where ext_trade_id is not null;
create index idx_trade_oms_account_time on trd.trade_oms(account_id, filled_ts desc);
create index idx_trade_oms_order        on trd.trade_oms(order_id);
create index idx_trade_oms_execution    on trd.trade_oms(execution_id);

create table trd.trade_recon (
  recon_fill_id   bigserial primary key,
  fill_id         bigint references trd.trade_oms(fill_id),
  account_id      bigint not null,
  oms_id          text,
  instrument_id   text not null,
  instrument_exch text,
  side            text not null,
  ext_trade_id    text not null,
  filled_qty      numeric not null,
  filled_price    numeric not null,
  filled_ts       timestamptz not null,
  fee_total       numeric,
  fee_detail      jsonb,
  realized_pnl    numeric,
  recon_status    text not null,
  recon_run_id    bigint not null,
  created_at      timestamptz not null default now()
);

create unique index idx_trade_recon_ext on trd.trade_recon(account_id, ext_trade_id);
create index idx_trade_recon_account_time on trd.trade_recon(account_id, filled_ts desc);
create index idx_trade_recon_run         on trd.trade_recon(recon_run_id);

create table trd.balance_snapshot (
  snapshot_id  bigserial primary key,
  account_id   bigint not null,
  asset        text not null,
  total_qty    numeric not null,
  avail_qty    numeric not null,
  frozen_qty   numeric not null,
  snapshot_ts  timestamptz not null,
  source       text not null default 'OMS',
  created_at   timestamptz not null default now()
);

create unique index idx_balance_snapshot_hour
  on trd.balance_snapshot(account_id, asset, snapshot_ts);
create index idx_balance_snapshot_account_time
  on trd.balance_snapshot(account_id, snapshot_ts desc);

create table trd.position_snapshot (
  snapshot_id            bigserial primary key,
  account_id             bigint not null,
  instrument_id          text not null,
  side                   text not null,
  instrument_type        text,
  total_qty              numeric not null,
  avail_qty              numeric not null,
  frozen_qty             numeric not null,
  contract_size          numeric,
  contract_size_adjusted boolean not null default false,
  avg_entry_price        numeric,
  unrealized_pnl         numeric,
  margin_info            jsonb,
  snapshot_ts            timestamptz not null,
  source                 text not null default 'OMS',
  created_at             timestamptz not null default now()
);

create unique index idx_position_snapshot_hour
  on trd.position_snapshot(account_id, instrument_id, side, snapshot_ts);
create index idx_position_snapshot_account_time
  on trd.position_snapshot(account_id, snapshot_ts desc);

-- ─────────────────────────────────────────────────────────────────────────────
-- mon schema
-- ─────────────────────────────────────────────────────────────────────────────
create schema if not exists mon;

create table mon.service_health (
  service_type  text not null,
  service_id    text not null,
  instance_id   text not null,
  status        text not null,
  detail        jsonb not null default '{}'::jsonb,
  observed_at   timestamptz not null default now(),
  primary key (service_type, service_id, instance_id, observed_at)
);

create table mon.recon_run (
  recon_run_id    bigserial primary key,
  recon_type      text not null,
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
  status          text not null,
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

create table mon.active_session (
  owner_session_id  text primary key,
  logical_id        text not null references cfg.logical_instance(logical_id),
  instance_type     text not null,
  kv_key            text not null,
  lock_key          text not null,
  lease_ttl_ms      int not null,
  status            text not null,
  last_seen_at      timestamptz not null default now(),
  expires_at        timestamptz not null,
  source_addr       text,
  metadata          jsonb not null default '{}'::jsonb
);

create index idx_active_session_logical on mon.active_session(logical_id, status);

-- Reconciler lookup: find session by kv_key when a KV Delete/Purge event fires.
create index idx_active_session_kv_key
  on mon.active_session(kv_key)
  where status = 'active';

-- At most one active session per (logical_id, instance_type) — DB-level guardrail.
create unique index idx_active_session_unique_active
  on mon.active_session (logical_id, instance_type)
  where status = 'active';

create table mon.registration_audit (
  audit_id          bigserial primary key,
  token_jti         text,
  logical_id        text,
  instance_type     text,
  owner_session_id  text,
  decision          text not null,
  reason            text,
  source_addr       text,
  observed_at       timestamptz not null default now(),
  detail            jsonb not null default '{}'::jsonb
);

create index idx_registration_audit_logical_time
  on mon.registration_audit(logical_id, observed_at desc);
