-- Add strategy_type_key to strategy_definition.
-- This is the canonical implementation family selector (e.g. "smoke-test", "mm"),
-- distinct from strategy_id (authored identity) and code_ref (build artifact).

ALTER TABLE cfg.strategy_definition
  ADD COLUMN IF NOT EXISTS strategy_type_key text;

-- Backfill from code_ref for existing rows
UPDATE cfg.strategy_definition
  SET strategy_type_key = code_ref
  WHERE strategy_type_key IS NULL;

-- Enforce NOT NULL after backfill
ALTER TABLE cfg.strategy_definition
  ALTER COLUMN strategy_type_key SET NOT NULL;
