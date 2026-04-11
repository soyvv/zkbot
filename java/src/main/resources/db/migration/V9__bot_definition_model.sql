-- V9: Link engine_instance to strategy_definition and oms_instance,
--     add engine_id + run snapshots to strategy_instance.

-- Link engine_instance to its strategy template
ALTER TABLE cfg.engine_instance
  ADD COLUMN strategy_id text REFERENCES cfg.strategy_definition(strategy_id);

-- Link engine_instance to its target OMS
ALTER TABLE cfg.engine_instance
  ADD COLUMN target_oms_id text REFERENCES cfg.oms_instance(oms_id);

-- Track which bot spawned each execution run
ALTER TABLE cfg.strategy_instance
  ADD COLUMN engine_id text REFERENCES cfg.engine_instance(engine_id);

CREATE INDEX idx_strategy_instance_engine ON cfg.strategy_instance(engine_id);

-- Run snapshot columns
ALTER TABLE cfg.strategy_instance
  ADD COLUMN strategy_config_snapshot jsonb,
  ADD COLUMN engine_config_snapshot jsonb,
  ADD COLUMN binding_snapshot jsonb;
