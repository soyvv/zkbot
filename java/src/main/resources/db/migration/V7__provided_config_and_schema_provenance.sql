-- Add provided_config + schema provenance to service-specific tables.
-- Aligns Flyway-managed schema with devstack 00_schema.sql ground truth.
-- Uses ADD COLUMN IF NOT EXISTS for idempotency (fresh devstack DBs already have these).

-- ── OMS ──────────────────────────────────────────────────────────────────────
ALTER TABLE cfg.oms_instance
    ADD COLUMN IF NOT EXISTS provided_config       jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS schema_resource_type   text,
    ADD COLUMN IF NOT EXISTS schema_resource_key    text,
    ADD COLUMN IF NOT EXISTS schema_version         integer,
    ADD COLUMN IF NOT EXISTS schema_content_hash    text;

-- ── Gateway ──────────────────────────────────────────────────────────────────
ALTER TABLE cfg.gateway_instance
    ADD COLUMN IF NOT EXISTS provided_config              jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS enabled                       boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS schema_resource_type          text,
    ADD COLUMN IF NOT EXISTS schema_resource_key           text,
    ADD COLUMN IF NOT EXISTS schema_version                integer,
    ADD COLUMN IF NOT EXISTS schema_content_hash           text,
    ADD COLUMN IF NOT EXISTS venue_schema_resource_key     text,
    ADD COLUMN IF NOT EXISTS venue_schema_version          integer,
    ADD COLUMN IF NOT EXISTS venue_schema_content_hash     text,
    ADD COLUMN IF NOT EXISTS supports_batch_order          boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS supports_batch_cancel         boolean NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS supports_order_query          boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS supports_position_query       boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS supports_trade_history_query  boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS supports_fee_query            boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS cancel_required_fields        text[] NOT NULL DEFAULT '{}';

-- ── Engine ───────────────────────────────────────────────────────────────────
ALTER TABLE cfg.engine_instance
    ADD COLUMN IF NOT EXISTS provided_config       jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS schema_resource_type   text,
    ADD COLUMN IF NOT EXISTS schema_resource_key    text,
    ADD COLUMN IF NOT EXISTS schema_version         integer,
    ADD COLUMN IF NOT EXISTS schema_content_hash    text;

-- ── MDGW ─────────────────────────────────────────────────────────────────────
ALTER TABLE cfg.mdgw_instance
    ADD COLUMN IF NOT EXISTS provided_config              jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS schema_resource_type          text,
    ADD COLUMN IF NOT EXISTS schema_resource_key           text,
    ADD COLUMN IF NOT EXISTS schema_version                integer,
    ADD COLUMN IF NOT EXISTS schema_content_hash           text,
    ADD COLUMN IF NOT EXISTS venue_schema_resource_key     text,
    ADD COLUMN IF NOT EXISTS venue_schema_version          integer,
    ADD COLUMN IF NOT EXISTS venue_schema_content_hash     text;

-- ── Refdata venue instance (new table) ───────────────────────────────────────
CREATE TABLE IF NOT EXISTS cfg.refdata_venue_instance (
    logical_id            text PRIMARY KEY,
    env                   text NOT NULL,
    venue                 text NOT NULL,
    description           text,
    enabled               boolean NOT NULL DEFAULT true,
    provided_config       jsonb NOT NULL DEFAULT '{}'::jsonb,
    schema_resource_type  text NOT NULL DEFAULT 'venue_capability',
    schema_resource_key   text,
    schema_version        integer,
    schema_content_hash   text,
    config_version        integer NOT NULL DEFAULT 1,
    config_hash           text,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    UNIQUE (env, venue)
);

DROP TRIGGER IF EXISTS trg_refdata_venue_config_version ON cfg.refdata_venue_instance;
CREATE TRIGGER trg_refdata_venue_config_version
    BEFORE UPDATE ON cfg.refdata_venue_instance
    FOR EACH ROW EXECUTE FUNCTION cfg.bump_config_version();

-- ── Update trigger to prefer provided_config over runtime_config ─────────────
CREATE OR REPLACE FUNCTION cfg.bump_config_version()
RETURNS trigger AS $$
BEGIN
    IF to_jsonb(NEW) ? 'provided_config'
       AND NEW.provided_config IS DISTINCT FROM OLD.provided_config THEN
        NEW.config_version := OLD.config_version + 1;
        NEW.config_hash := encode(sha256(convert_to(
            NEW.provided_config::text, 'UTF8')), 'hex');
        NEW.updated_at := now();
    ELSIF to_jsonb(NEW) ? 'runtime_config'
       AND NEW.runtime_config IS DISTINCT FROM OLD.runtime_config THEN
        NEW.config_version := OLD.config_version + 1;
        NEW.config_hash := encode(sha256(convert_to(
            NEW.runtime_config::text, 'UTF8')), 'hex');
        NEW.updated_at := now();
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- ── Backfill provided_config from runtime_config where empty ─────────────────
UPDATE cfg.oms_instance SET provided_config = runtime_config
    WHERE provided_config = '{}'::jsonb AND runtime_config != '{}'::jsonb;
UPDATE cfg.gateway_instance SET provided_config = runtime_config
    WHERE provided_config = '{}'::jsonb AND runtime_config != '{}'::jsonb;
UPDATE cfg.engine_instance SET provided_config = runtime_config
    WHERE provided_config = '{}'::jsonb AND runtime_config != '{}'::jsonb;
UPDATE cfg.mdgw_instance SET provided_config = runtime_config
    WHERE provided_config = '{}'::jsonb AND runtime_config != '{}'::jsonb;
