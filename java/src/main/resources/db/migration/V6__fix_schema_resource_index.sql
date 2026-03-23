-- Drop the unique index on (schema_id, version) — venue manifests share one schema_id
-- across multiple capabilities (e.g. venue/okx version=2 has gw, rtmd, refdata entries).
-- The primary key (resource_type, resource_key, version) is sufficient.

DROP INDEX IF EXISTS cfg.idx_schema_resource_schema_id_version;
