package com.zkbot.pilot.topology;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.List;
import java.util.Map;

@Repository
public class TopologyRepository {

    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper;

    public TopologyRepository(JdbcTemplate jdbc, ObjectMapper objectMapper) {
        this.jdbc = jdbc;
        this.objectMapper = objectMapper;
    }

    // --- Logical instances ---

    public List<Map<String, Object>> listLogicalInstances(String env, String instanceType) {
        if (instanceType != null && !instanceType.isBlank()) {
            return jdbc.queryForList(
                    "SELECT * FROM cfg.logical_instance WHERE env = ? AND instance_type = ? ORDER BY logical_id",
                    env, instanceType);
        }
        return jdbc.queryForList(
                "SELECT * FROM cfg.logical_instance WHERE env = ? ORDER BY logical_id", env);
    }

    public Map<String, Object> getLogicalInstance(String logicalId) {
        var rows = jdbc.queryForList(
                "SELECT * FROM cfg.logical_instance WHERE logical_id = ?", logicalId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public void createLogicalInstance(String logicalId, String instanceType, String env,
                                      Map<String, Object> metadata, boolean enabled) {
        String metadataJson = toJson(metadata);
        jdbc.update("""
                INSERT INTO cfg.logical_instance (logical_id, instance_type, env, metadata, enabled)
                VALUES (?, ?, ?, ?::jsonb, ?)
                ON CONFLICT (logical_id) DO UPDATE
                  SET instance_type = EXCLUDED.instance_type,
                      env = EXCLUDED.env,
                      metadata = EXCLUDED.metadata,
                      enabled = EXCLUDED.enabled
                """, logicalId, instanceType, env, metadataJson, enabled);
    }

    // --- Service-specific instance rows ---

    public void createOmsInstance(String omsId, String providedConfigJson,
                                   String schemaResourceType, String schemaResourceKey,
                                   Integer schemaVersion, String schemaContentHash) {
        jdbc.update("""
                INSERT INTO cfg.oms_instance (oms_id, namespace, provided_config,
                    schema_resource_type, schema_resource_key, schema_version, schema_content_hash)
                VALUES (?, ?, ?::jsonb, ?, ?, ?, ?)
                ON CONFLICT (oms_id) DO NOTHING
                """, omsId, omsId, providedConfigJson,
                schemaResourceType, schemaResourceKey, schemaVersion, schemaContentHash);
    }

    public List<Map<String, Object>> listGatewayInstances() {
        return jdbc.queryForList("SELECT gw_id, venue, broker_type, account_type FROM cfg.gateway_instance ORDER BY gw_id");
    }

    public Map<String, Object> getGatewayInstance(String gwId) {
        var rows = jdbc.queryForList("SELECT gw_id, venue, broker_type, account_type FROM cfg.gateway_instance WHERE gw_id = ?", gwId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public void createGatewayInstance(String gwId, String venue, String providedConfigJson,
                                       String schemaResourceType, String schemaResourceKey,
                                       Integer schemaVersion, String schemaContentHash,
                                       String venueSchemaResourceKey, Integer venueSchemaVersion,
                                       String venueSchemaContentHash) {
        jdbc.update("""
                INSERT INTO cfg.gateway_instance (gw_id, venue, broker_type, account_type, provided_config,
                    schema_resource_type, schema_resource_key, schema_version, schema_content_hash,
                    venue_schema_resource_key, venue_schema_version, venue_schema_content_hash)
                VALUES (?, ?, 'default', 'default', ?::jsonb, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (gw_id) DO NOTHING
                """, gwId, venue, providedConfigJson,
                schemaResourceType, schemaResourceKey, schemaVersion, schemaContentHash,
                venueSchemaResourceKey, venueSchemaVersion, venueSchemaContentHash);
    }

    public void createEngineInstance(String engineId, String providedConfigJson,
                                      String schemaResourceType, String schemaResourceKey,
                                      Integer schemaVersion, String schemaContentHash) {
        jdbc.update("""
                INSERT INTO cfg.engine_instance (engine_id, provided_config,
                    schema_resource_type, schema_resource_key, schema_version, schema_content_hash)
                VALUES (?, ?::jsonb, ?, ?, ?, ?)
                ON CONFLICT (engine_id) DO NOTHING
                """, engineId, providedConfigJson,
                schemaResourceType, schemaResourceKey, schemaVersion, schemaContentHash);
    }

    public void createMdgwInstance(String mdgwId, String venue, String providedConfigJson,
                                    String schemaResourceType, String schemaResourceKey,
                                    Integer schemaVersion, String schemaContentHash,
                                    String venueSchemaResourceKey, Integer venueSchemaVersion,
                                    String venueSchemaContentHash) {
        jdbc.update("""
                INSERT INTO cfg.mdgw_instance (mdgw_id, venue, provided_config,
                    schema_resource_type, schema_resource_key, schema_version, schema_content_hash,
                    venue_schema_resource_key, venue_schema_version, venue_schema_content_hash)
                VALUES (?, ?, ?::jsonb, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (mdgw_id) DO NOTHING
                """, mdgwId, venue, providedConfigJson,
                schemaResourceType, schemaResourceKey, schemaVersion, schemaContentHash,
                venueSchemaResourceKey, venueSchemaVersion, venueSchemaContentHash);
    }

    // --- Refdata venue instances ---

    public void createRefdataVenueInstance(String logicalId, String env, String venue,
                                            String description, boolean enabled,
                                            String providedConfigJson,
                                            String schemaResourceKey, Integer schemaVersion,
                                            String schemaContentHash) {
        jdbc.update("""
                INSERT INTO cfg.refdata_venue_instance
                    (logical_id, env, venue, description, enabled, provided_config,
                     schema_resource_key, schema_version, schema_content_hash)
                VALUES (?, ?, ?, ?, ?, ?::jsonb, ?, ?, ?)
                ON CONFLICT (logical_id) DO UPDATE SET
                    env = EXCLUDED.env, venue = EXCLUDED.venue,
                    description = EXCLUDED.description, enabled = EXCLUDED.enabled,
                    provided_config = EXCLUDED.provided_config,
                    schema_resource_key = EXCLUDED.schema_resource_key,
                    schema_version = EXCLUDED.schema_version,
                    schema_content_hash = EXCLUDED.schema_content_hash
                """, logicalId, env, venue, description, enabled, providedConfigJson,
                schemaResourceKey, schemaVersion, schemaContentHash);
    }

    /**
     * Check if an (env, venue) pair already exists for a different logical_id.
     * Returns the owning logical_id, or null if the pair is free.
     */
    public String findRefdataVenueOwner(String env, String venue) {
        return jdbc.query("""
                SELECT logical_id FROM cfg.refdata_venue_instance
                WHERE env = ? AND venue = ?
                """, rs -> rs.next() ? rs.getString("logical_id") : null, env, venue);
    }

    public List<Map<String, Object>> listRefdataVenueInstances() {
        return jdbc.queryForList(
                "SELECT * FROM cfg.refdata_venue_instance ORDER BY venue, logical_id");
    }

    public Map<String, Object> getRefdataVenueInstance(String logicalId) {
        var rows = jdbc.queryForList(
                "SELECT * FROM cfg.refdata_venue_instance WHERE logical_id = ?", logicalId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    // --- Logical bindings ---

    public List<Map<String, Object>> listBindings(String srcId, String dstId) {
        if (srcId != null && dstId != null) {
            return jdbc.queryForList(
                    "SELECT * FROM cfg.logical_binding WHERE src_id = ? AND dst_id = ? ORDER BY src_id, dst_id",
                    srcId, dstId);
        }
        if (srcId != null) {
            return jdbc.queryForList(
                    "SELECT * FROM cfg.logical_binding WHERE src_id = ? ORDER BY src_id, dst_id", srcId);
        }
        if (dstId != null) {
            return jdbc.queryForList(
                    "SELECT * FROM cfg.logical_binding WHERE dst_id = ? ORDER BY src_id, dst_id", dstId);
        }
        return jdbc.queryForList("SELECT * FROM cfg.logical_binding ORDER BY src_id, dst_id");
    }

    public void upsertBinding(String srcType, String srcId, String dstType, String dstId,
                               boolean enabled, Map<String, Object> metadata) {
        String metadataJson = toJson(metadata);
        // No unique constraint on (src_id, dst_id) — use delete+insert pattern
        jdbc.update(
                "DELETE FROM cfg.logical_binding WHERE src_id = ? AND dst_id = ?",
                srcId, dstId);
        jdbc.update("""
                INSERT INTO cfg.logical_binding (src_type, src_id, dst_type, dst_id, enabled, metadata)
                VALUES (?, ?, ?, ?, ?, ?::jsonb)
                """, srcType, srcId, dstType, dstId, enabled, metadataJson);
    }

    public List<Map<String, Object>> getBindingsForService(String logicalId) {
        return jdbc.queryForList(
                "SELECT * FROM cfg.logical_binding WHERE src_id = ? OR dst_id = ? ORDER BY src_id, dst_id",
                logicalId, logicalId);
    }

    // --- Sessions ---

    public List<Map<String, Object>> listActiveSessions() {
        return jdbc.queryForList(
                "SELECT * FROM mon.active_session WHERE status = 'active' ORDER BY last_seen_at DESC");
    }

    // --- Audit ---

    public List<Map<String, Object>> listRegistrationAudit(String logicalId, int limit) {
        if (logicalId != null && !logicalId.isBlank()) {
            return jdbc.queryForList(
                    "SELECT * FROM mon.registration_audit WHERE logical_id = ? ORDER BY observed_at DESC LIMIT ?",
                    logicalId, limit);
        }
        return jdbc.queryForList(
                "SELECT * FROM mon.registration_audit ORDER BY observed_at DESC LIMIT ?", limit);
    }

    public List<Map<String, Object>> listReconciliationAudit(int limit) {
        return jdbc.queryForList(
                "SELECT * FROM mon.recon_run ORDER BY run_at DESC LIMIT ?", limit);
    }

    private String toJson(Map<String, Object> map) {
        if (map == null || map.isEmpty()) {
            return "{}";
        }
        try {
            return objectMapper.writeValueAsString(map);
        } catch (JsonProcessingException e) {
            return "{}";
        }
    }
}
