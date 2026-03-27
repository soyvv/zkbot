-- Recorder schema additions — trd.order_oms partitioned table + maintenance procedures.

-- ─────────────────────────────────────────────────────────────────────────────
-- trd.order_oms — terminal OMS order state, monthly range-partitioned
-- ─────────────────────────────────────────────────────────────────────────────

create table trd.order_oms (
  order_id         bigint not null,
  account_id       bigint not null,
  oms_id           text not null,
  gw_id            text,
  strategy_id      text,
  execution_id     text,
  source_id        text,
  instrument_id    text not null,
  instrument_exch  text,
  side             text not null,
  open_close       text,
  order_type       text,
  order_status     text not null,
  price            numeric,
  qty              numeric not null,
  filled_qty       numeric not null default 0,
  filled_avg_price numeric,
  ext_order_ref    text,
  error_msg        text,
  terminal_at      timestamptz not null,
  created_at       timestamptz not null default now(),
  recorded_at      timestamptz not null default now(),
  raw_snapshot     jsonb,
  primary key (terminal_at, order_id)
) partition by range (terminal_at);

create unique index idx_order_oms_order_id
  on trd.order_oms(order_id, terminal_at);

create index idx_order_oms_account_time
  on trd.order_oms(account_id, terminal_at desc);

create index idx_order_oms_execution
  on trd.order_oms(execution_id)
  where execution_id is not null;

-- ─────────────────────────────────────────────────────────────────────────────
-- Deterministic fingerprint index for trade dedup when ext_trade_id is NULL
-- (inferred trades or venue trades without exchange trade IDs).
-- ─────────────────────────────────────────────────────────────────────────────

create unique index if not exists idx_trade_oms_fingerprint
  on trd.trade_oms(account_id, order_id, filled_qty, filled_price, filled_ts)
  where ext_trade_id is null;

-- ─────────────────────────────────────────────────────────────────────────────
-- Partition maintenance procedures
-- ─────────────────────────────────────────────────────────────────────────────

-- ensure_order_oms_partitions: create current month + next month partitions
-- if they do not already exist. Idempotent — safe to call repeatedly.
create or replace function trd.ensure_order_oms_partitions()
returns void as $$
declare
  m         date;
  part_name text;
  start_ts  text;
  end_ts    text;
begin
  -- current month and next month
  for m in
    select d from unnest(array[
      date_trunc('month', now())::date,
      (date_trunc('month', now()) + interval '1 month')::date
    ]) as d
  loop
    part_name := 'order_oms_' || to_char(m, 'YYYYMM');
    start_ts  := to_char(m, 'YYYY-MM-DD');
    end_ts    := to_char((m + interval '1 month')::date, 'YYYY-MM-DD');

    if not exists (
      select 1 from pg_class c
      join pg_namespace n on n.oid = c.relnamespace
      where n.nspname = 'trd' and c.relname = part_name
    ) then
      execute format(
        'create table trd.%I partition of trd.order_oms for values from (%L) to (%L)',
        part_name, start_ts, end_ts
      );
      raise notice 'created partition trd.%', part_name;
    end if;
  end loop;
end;
$$ language plpgsql;

-- drop_expired_order_oms_partitions: drop partitions older than the retention
-- window. Default retention is 3 months (current + 2 prior full months).
create or replace function trd.drop_expired_order_oms_partitions(
  months_to_keep int default 3
)
returns void as $$
declare
  cutoff     date;
  rec        record;
  part_month date;
begin
  cutoff := (date_trunc('month', now()) - (months_to_keep || ' months')::interval)::date;

  for rec in
    select c.relname
    from pg_inherits i
    join pg_class c on c.oid = i.inhrelid
    join pg_class p on p.oid = i.inhparent
    join pg_namespace pn on pn.oid = p.relnamespace
    where pn.nspname = 'trd' and p.relname = 'order_oms'
    order by c.relname
  loop
    -- extract YYYYMM suffix: order_oms_202603 → 202603
    begin
      part_month := to_date(right(rec.relname, 6), 'YYYYMM');
    exception when others then
      continue;  -- skip partitions with unexpected names
    end;

    if part_month < cutoff then
      execute format('drop table trd.%I', rec.relname);
      raise notice 'dropped expired partition trd.%', rec.relname;
    end if;
  end loop;
end;
$$ language plpgsql;

-- ─────────────────────────────────────────────────────────────────────────────
-- Bootstrap: create initial partitions
-- ─────────────────────────────────────────────────────────────────────────────
select trd.ensure_order_oms_partitions();
