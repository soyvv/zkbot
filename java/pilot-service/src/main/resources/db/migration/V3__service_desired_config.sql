-- Add desired runtime config + versioning to service-specific tables.
-- These tables become the config authority (not cfg.logical_instance.runtime_config).

-- OMS: add desired runtime config + versioning
ALTER TABLE cfg.oms_instance
    ADD COLUMN IF NOT EXISTS runtime_config  jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS config_version  integer NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS config_hash     text;

-- GW: add desired runtime config + versioning
ALTER TABLE cfg.gateway_instance
    ADD COLUMN IF NOT EXISTS runtime_config  jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS config_version  integer NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS config_hash     text;

-- Engine: new table
CREATE TABLE IF NOT EXISTS cfg.engine_instance (
    engine_id        text PRIMARY KEY,
    description      text,
    enabled          boolean NOT NULL DEFAULT true,
    runtime_config   jsonb NOT NULL DEFAULT '{}'::jsonb,
    config_version   integer NOT NULL DEFAULT 1,
    config_hash      text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- MDGW: new table
CREATE TABLE IF NOT EXISTS cfg.mdgw_instance (
    mdgw_id          text PRIMARY KEY,
    venue            text NOT NULL,
    description      text,
    enabled          boolean NOT NULL DEFAULT true,
    runtime_config   jsonb NOT NULL DEFAULT '{}'::jsonb,
    config_version   integer NOT NULL DEFAULT 1,
    config_hash      text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Auto-bump config_version on runtime_config change
CREATE OR REPLACE FUNCTION cfg.bump_config_version()
RETURNS trigger AS $$
BEGIN
    IF NEW.runtime_config IS DISTINCT FROM OLD.runtime_config THEN
        NEW.config_version := OLD.config_version + 1;
        NEW.config_hash := encode(sha256(convert_to(
            NEW.runtime_config::text, 'UTF8')), 'hex');
        NEW.updated_at := now();
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_oms_config_version ON cfg.oms_instance;
CREATE TRIGGER trg_oms_config_version
    BEFORE UPDATE ON cfg.oms_instance
    FOR EACH ROW EXECUTE FUNCTION cfg.bump_config_version();

DROP TRIGGER IF EXISTS trg_gw_config_version ON cfg.gateway_instance;
CREATE TRIGGER trg_gw_config_version
    BEFORE UPDATE ON cfg.gateway_instance
    FOR EACH ROW EXECUTE FUNCTION cfg.bump_config_version();

DROP TRIGGER IF EXISTS trg_engine_config_version ON cfg.engine_instance;
CREATE TRIGGER trg_engine_config_version
    BEFORE UPDATE ON cfg.engine_instance
    FOR EACH ROW EXECUTE FUNCTION cfg.bump_config_version();

DROP TRIGGER IF EXISTS trg_mdgw_config_version ON cfg.mdgw_instance;
CREATE TRIGGER trg_mdgw_config_version
    BEFORE UPDATE ON cfg.mdgw_instance
    FOR EACH ROW EXECUTE FUNCTION cfg.bump_config_version();

COMMENT ON COLUMN cfg.oms_instance.runtime_config IS
    'Desired runtime config (authoritative). Validated against svc/oms schema.';
COMMENT ON COLUMN cfg.gateway_instance.runtime_config IS
    'Desired runtime config (authoritative). Validated against svc/gw schema.';
