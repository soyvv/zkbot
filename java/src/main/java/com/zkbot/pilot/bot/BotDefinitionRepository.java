package com.zkbot.pilot.bot;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class BotDefinitionRepository {

    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper;

    public BotDefinitionRepository(JdbcTemplate jdbc, ObjectMapper objectMapper) {
        this.jdbc = jdbc;
        this.objectMapper = objectMapper;
    }

    public void createBotDefinition(String engineId, String strategyId, String targetOmsId,
                                     String description, Map<String, Object> providedConfig,
                                     boolean enabled) {
        jdbc.update("""
                INSERT INTO cfg.engine_instance
                  (engine_id, strategy_id, target_oms_id, description, provided_config, enabled)
                VALUES (?, ?, ?, ?, ?::jsonb, ?)
                """,
                engineId, strategyId, targetOmsId, description, toJson(providedConfig), enabled);
    }

    /**
     * Returns bot definition with current or last active run.
     * Prefers active runs (INITIALIZING/RUNNING/PAUSED) over terminal runs,
     * then falls back to the most recent run of any status.
     */
    public Map<String, Object> getBotDefinition(String engineId) {
        var rows = jdbc.queryForList("""
                SELECT ei.*, sd.runtime_type, sd.code_ref,
                       lr.execution_id AS current_execution_id,
                       lr.status AS current_execution_status,
                       lr.started_at AS current_execution_started_at,
                       lr.ended_at AS current_execution_ended_at
                FROM cfg.engine_instance ei
                LEFT JOIN cfg.strategy_definition sd ON sd.strategy_id = ei.strategy_id
                LEFT JOIN LATERAL (
                    SELECT execution_id, status, started_at, ended_at
                    FROM cfg.strategy_instance
                    WHERE engine_id = ei.engine_id
                    ORDER BY
                      CASE WHEN status IN ('INITIALIZING','RUNNING','PAUSED') THEN 0 ELSE 1 END,
                      started_at DESC
                    LIMIT 1
                ) lr ON true
                WHERE ei.engine_id = ?
                """, engineId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Lists bot definitions with current or last active run per engine.
     * Same run selection logic as getBotDefinition.
     */
    public List<Map<String, Object>> listBotDefinitions(int limit) {
        if (limit <= 0) limit = 50;
        return jdbc.queryForList("""
                SELECT ei.*, sd.runtime_type, sd.code_ref,
                       lr.execution_id AS current_execution_id,
                       lr.status AS current_execution_status,
                       lr.started_at AS current_execution_started_at,
                       lr.ended_at AS current_execution_ended_at
                FROM cfg.engine_instance ei
                LEFT JOIN cfg.strategy_definition sd ON sd.strategy_id = ei.strategy_id
                LEFT JOIN LATERAL (
                    SELECT execution_id, status, started_at, ended_at
                    FROM cfg.strategy_instance
                    WHERE engine_id = ei.engine_id
                    ORDER BY
                      CASE WHEN status IN ('INITIALIZING','RUNNING','PAUSED') THEN 0 ELSE 1 END,
                      started_at DESC
                    LIMIT 1
                ) lr ON true
                ORDER BY ei.engine_id
                LIMIT ?
                """, limit);
    }

    public void updateBotDefinition(String engineId, Map<String, Object> fields) {
        if (fields == null || fields.isEmpty()) return;

        var setClauses = new ArrayList<String>();
        var params = new ArrayList<Object>();

        if (fields.containsKey("description")) {
            setClauses.add("description = ?");
            params.add(fields.get("description"));
        }
        if (fields.containsKey("providedConfig")) {
            setClauses.add("provided_config = ?::jsonb");
            params.add(toJson(fields.get("providedConfig")));
        }
        if (fields.containsKey("enabled")) {
            setClauses.add("enabled = ?");
            params.add(fields.get("enabled"));
        }

        if (setClauses.isEmpty()) return;
        setClauses.add("updated_at = now()");
        params.add(engineId);

        String sql = "UPDATE cfg.engine_instance SET " +
                String.join(", ", setClauses) + " WHERE engine_id = ?";
        jdbc.update(sql, params.toArray());
    }

    public Map<String, Object> findRunningExecutionForEngine(String engineId) {
        var rows = jdbc.queryForList("""
                SELECT * FROM cfg.strategy_instance
                WHERE engine_id = ? AND status IN ('INITIALIZING', 'RUNNING', 'PAUSED')
                ORDER BY started_at DESC LIMIT 1
                """, engineId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    private String toJson(Object obj) {
        if (obj == null) return "{}";
        try {
            return objectMapper.writeValueAsString(obj);
        } catch (JsonProcessingException e) {
            return "{}";
        }
    }
}
