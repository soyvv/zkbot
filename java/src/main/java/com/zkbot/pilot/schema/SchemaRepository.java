package com.zkbot.pilot.schema;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.List;
import java.util.Map;

/**
 * CRUD on cfg.schema_resource — the operational mirror of bundled manifests.
 */
@Repository
public class SchemaRepository {

    private final JdbcTemplate jdbc;

    public SchemaRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    /**
     * Find a schema resource by (resourceType, resourceKey, version).
     */
    public Map<String, Object> find(String resourceType, String resourceKey, int version) {
        var rows = jdbc.queryForList("""
                SELECT resource_type, resource_key, schema_id, version, content_hash,
                       active, manifest_json::text AS manifest_json,
                       config_schema::text AS config_schema, synced_from,
                       created_at, updated_at
                FROM cfg.schema_resource
                WHERE resource_type = ? AND resource_key = ? AND version = ?
                """, resourceType, resourceKey, version);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Find the active version for a (resourceType, resourceKey).
     */
    public Map<String, Object> findActive(String resourceType, String resourceKey) {
        var rows = jdbc.queryForList("""
                SELECT resource_type, resource_key, schema_id, version, content_hash,
                       active, manifest_json::text AS manifest_json,
                       config_schema::text AS config_schema, synced_from,
                       created_at, updated_at
                FROM cfg.schema_resource
                WHERE resource_type = ? AND resource_key = ? AND active = true
                ORDER BY version DESC
                LIMIT 1
                """, resourceType, resourceKey);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Find by schema_id and version (unique index).
     */
    public Map<String, Object> findBySchemaIdAndVersion(String schemaId, int version) {
        var rows = jdbc.queryForList("""
                SELECT resource_type, resource_key, schema_id, version, content_hash,
                       active, manifest_json::text AS manifest_json,
                       config_schema::text AS config_schema, synced_from,
                       created_at, updated_at
                FROM cfg.schema_resource
                WHERE schema_id = ? AND version = ?
                """, schemaId, version);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * List all schema resources, optionally filtered by resource_type.
     */
    public List<Map<String, Object>> list(String resourceType) {
        if (resourceType != null) {
            return jdbc.queryForList("""
                    SELECT resource_type, resource_key, schema_id, version, content_hash,
                           active, synced_from, created_at, updated_at
                    FROM cfg.schema_resource
                    WHERE resource_type = ?
                    ORDER BY resource_type, resource_key, version
                    """, resourceType);
        }
        return jdbc.queryForList("""
                SELECT resource_type, resource_key, schema_id, version, content_hash,
                       active, synced_from, created_at, updated_at
                FROM cfg.schema_resource
                ORDER BY resource_type, resource_key, version
                """);
    }

    /**
     * Insert a new schema resource record.
     */
    public void insert(String resourceType, String resourceKey, String schemaId,
                       int version, String contentHash, String manifestJson,
                       String configSchemaJson) {
        jdbc.update("""
                INSERT INTO cfg.schema_resource
                  (resource_type, resource_key, schema_id, version, content_hash,
                   active, manifest_json, config_schema, synced_from)
                VALUES (?, ?, ?, ?, ?, true, ?::jsonb, ?::jsonb, 'bundled')
                """,
                resourceType, resourceKey, schemaId, version, contentHash,
                manifestJson, configSchemaJson);
    }

    /**
     * Activate a specific version and deactivate all others for the same resource.
     */
    public void activateVersion(String resourceType, String resourceKey, int version) {
        jdbc.update("""
                UPDATE cfg.schema_resource
                SET active = (version = ?), updated_at = now()
                WHERE resource_type = ? AND resource_key = ?
                """, version, resourceType, resourceKey);
    }

    /**
     * Deprecate (deactivate) a specific version.
     */
    public void deprecateVersion(String resourceType, String resourceKey, int version) {
        jdbc.update("""
                UPDATE cfg.schema_resource
                SET active = false, updated_at = now()
                WHERE resource_type = ? AND resource_key = ? AND version = ?
                """, resourceType, resourceKey, version);
    }
}
