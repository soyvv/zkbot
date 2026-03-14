-- Adds columns required by zk-oms-svc db.rs.
-- Safe to apply on top of 00_schema.sql + 01_seed.sql.

-- rpc_endpoint: gRPC address of each gateway (e.g. "mock-gw:51051")
ALTER TABLE cfg.gateway_instance
  ADD COLUMN IF NOT EXISTS rpc_endpoint text NOT NULL DEFAULT '';

-- Populate for the dev mock gateway.
UPDATE cfg.gateway_instance
  SET rpc_endpoint = 'mock-gw:51051'
  WHERE gw_id = 'gw_mock_1';
