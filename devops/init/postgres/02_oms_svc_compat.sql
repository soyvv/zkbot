-- Adds columns required by zk-oms-svc db.rs.
-- Safe to apply on top of 00_schema.sql + 01_seed.sql.

-- rpc_endpoint: gRPC address of each gateway (e.g. "gw-sim:51051")
ALTER TABLE cfg.gateway_instance
  ADD COLUMN IF NOT EXISTS rpc_endpoint text NOT NULL DEFAULT '';

-- Populate for the dev gateway simulator.
-- Uses localhost because the default workflow (make dev-up + make oms-run)
-- runs OMS on the host, connecting to gw-sim via Docker port-forward.
-- For fully-dockerised mode (make dev-up-full), oms-svc overrides this
-- via its own Docker DNS (gw-sim:51051).
UPDATE cfg.gateway_instance
  SET rpc_endpoint = 'localhost:51051'
  WHERE gw_id = 'gw_sim_1';
