-- Add runtime_config column to cfg.logical_instance
-- Stores service-specific config as JSONB (venue_config for GW/MDGW, account bindings for OMS, etc.)
-- Pilot delivers this config to services during bootstrap registration.
ALTER TABLE cfg.logical_instance
    ADD COLUMN IF NOT EXISTS runtime_config jsonb DEFAULT '{}'::jsonb;

COMMENT ON COLUMN cfg.logical_instance.runtime_config IS
    'Service-specific runtime config delivered during bootstrap. Schema varies by instance_type.';
