-- Pilot bootstrap schema additions.
-- Run after 00_schema.sql (which creates cfg.logical_instance and cfg.instance_token).

-- Snowflake instance_id leases — Pilot assigns from this pool; clients never pick.
create table if not exists cfg.instance_id_lease (
  env          text not null,
  instance_id  int  not null,    -- 0–1023 Snowflake worker id
  logical_id   text not null references cfg.logical_instance(logical_id),
  leased_until timestamptz not null,
  primary key (env, instance_id)
);

create index idx_instance_id_lease_logical
  on cfg.instance_id_lease(env, logical_id);

-- Dev bootstrap token for oms_dev_1 (plaintext token = "dev-oms-token-1").
-- Token hash: sha256("dev-oms-token-1") — verified by Pilot on registration.
insert into cfg.instance_token (token_jti, logical_id, instance_type, token_hash, status, expires_at)
values (
  'dev-oms-token-jti-1',
  'oms_dev_1',
  'OMS',
  encode(sha256('dev-oms-token-1'::bytea), 'hex'),
  'active',
  now() + interval '365 days'
)
on conflict (token_jti) do nothing;

-- Dev bootstrap token for gw_sim_1 (plaintext token = "dev-gw-token-1").
-- Token hash: sha256("dev-gw-token-1") — verified by Pilot on registration.
insert into cfg.instance_token (token_jti, logical_id, instance_type, token_hash, status, expires_at)
values (
  'dev-gw-token-jti-1',
  'gw_sim_1',
  'GW',
  encode(sha256('dev-gw-token-1'::bytea), 'hex'),
  'active',
  now() + interval '365 days'
)
on conflict (token_jti) do nothing;
