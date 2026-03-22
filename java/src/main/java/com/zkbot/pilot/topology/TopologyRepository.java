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
                                      Map<String, Object> metadata) {
        String metadataJson = toJson(metadata);
        jdbc.update("""
                INSERT INTO cfg.logical_instance (logical_id, instance_type, env, metadata)
                VALUES (?, ?, ?, ?::jsonb)
                ON CONFLICT (logical_id) DO UPDATE
                  SET instance_type = EXCLUDED.instance_type,
                      env = EXCLUDED.env,
                      metadata = EXCLUDED.metadata
                """, logicalId, instanceType, env, metadataJson);
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
