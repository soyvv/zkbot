-- Operational mirror of bundled manifests and config schemas.
-- Synced from repo-bundled manifest files on Pilot startup by SchemaRegistrySyncer.
-- Source of truth remains the bundled manifests in the build artifact.

CREATE TABLE IF NOT EXISTS cfg.schema_resource (
    resource_type    text NOT NULL,          -- 'service_kind' | 'venue_capability'
    resource_key     text NOT NULL,          -- 'oms' | 'okx/gw' | 'oanda/rtmd'
    schema_id        text NOT NULL,          -- manifest identity: 'svc/oms', 'venue/okx'
    version          integer NOT NULL,
    content_hash     text NOT NULL,          -- SHA-256 of normalized manifest
    active           boolean NOT NULL DEFAULT true,
    manifest_json    jsonb NOT NULL,         -- full manifest (field_descriptors, metadata)
    config_schema    jsonb,                  -- JSON Schema payload
    synced_from      text NOT NULL DEFAULT 'bundled',  -- 'bundled' | 'admin_import'
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (resource_type, resource_key, version)
);

CREATE INDEX idx_schema_resource_active
    ON cfg.schema_resource (resource_type, resource_key)
    WHERE active = true;

CREATE UNIQUE INDEX idx_schema_resource_schema_id_version
    ON cfg.schema_resource (schema_id, version);
