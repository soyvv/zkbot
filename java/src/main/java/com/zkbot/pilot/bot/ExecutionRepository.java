package com.zkbot.pilot.bot;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class ExecutionRepository {

    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper;

    public ExecutionRepository(JdbcTemplate jdbc, ObjectMapper objectMapper) {
        this.jdbc = jdbc;
        this.objectMapper = objectMapper;
    }

    public void createExecution(String executionId, String strategyId,
                                 String targetOmsId, Map<String, Object> configOverride) {
        createExecution(executionId, strategyId, null, targetOmsId, configOverride, null, null, null);
    }

    public void createExecution(String executionId, String strategyId, String engineId,
                                 String targetOmsId, Map<String, Object> configOverride,
                                 Map<String, Object> strategyConfigSnapshot,
                                 Map<String, Object> engineConfigSnapshot,
                                 Map<String, Object> bindingSnapshot) {
        jdbc.update("""
                INSERT INTO cfg.strategy_instance
                  (execution_id, strategy_id, engine_id, target_oms_id, status,
                   config_override, strategy_config_snapshot, engine_config_snapshot,
                   binding_snapshot, started_at)
                VALUES (?, ?, ?, ?, 'INITIALIZING', ?::jsonb, ?::jsonb, ?::jsonb, ?::jsonb, now())
                """,
                executionId, strategyId, engineId, targetOmsId,
                toJson(configOverride), toJson(strategyConfigSnapshot),
                toJson(engineConfigSnapshot), toJson(bindingSnapshot));
    }

    public void updateExecutionStatus(String executionId, String status, String errorMessage) {
        boolean terminal = "STOPPED".equals(status) || "FAILED".equals(status) || "CRASHED".equals(status);
        if (terminal) {
            jdbc.update("""
                    UPDATE cfg.strategy_instance
                    SET status = ?, error_message = ?, ended_at = now()
                    WHERE execution_id = ?
                    """, status, errorMessage, executionId);
        } else {
            jdbc.update("""
                    UPDATE cfg.strategy_instance
                    SET status = ?, error_message = ?
                    WHERE execution_id = ?
                    """, status, errorMessage, executionId);
        }
    }

    public Map<String, Object> getExecution(String executionId) {
        var rows = jdbc.queryForList(
                "SELECT * FROM cfg.strategy_instance WHERE execution_id = ?", executionId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public List<Map<String, Object>> listExecutions(String strategyId, String status, int limit) {
        var conditions = new ArrayList<String>();
        var params = new ArrayList<Object>();

        conditions.add("1=1");

        if (strategyId != null && !strategyId.isBlank()) {
            conditions.add("strategy_id = ?");
            params.add(strategyId);
        }
        if (status != null && !status.isBlank()) {
            conditions.add("status = ?");
            params.add(status);
        }

        if (limit <= 0) limit = 50;
        params.add(limit);

        String sql = "SELECT * FROM cfg.strategy_instance WHERE " +
                String.join(" AND ", conditions) +
                " ORDER BY started_at DESC LIMIT ?";
        return jdbc.queryForList(sql, params.toArray());
    }

    public Map<String, Object> findRunningExecutionForStrategy(String strategyId) {
        var rows = jdbc.queryForList("""
                SELECT * FROM cfg.strategy_instance
                WHERE strategy_id = ? AND status IN ('INITIALIZING', 'RUNNING', 'PAUSED')
                ORDER BY started_at DESC LIMIT 1
                """, strategyId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public Map<String, Object> findRunningExecutionForEngine(String engineId) {
        var rows = jdbc.queryForList("""
                SELECT * FROM cfg.strategy_instance
                WHERE engine_id = ? AND status IN ('INITIALIZING', 'RUNNING', 'PAUSED')
                ORDER BY started_at DESC LIMIT 1
                """, engineId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public List<Map<String, Object>> listExecutionsForStrategy(String strategyId, int limit) {
        if (limit <= 0) limit = 50;
        return jdbc.queryForList("""
                SELECT * FROM cfg.strategy_instance
                WHERE strategy_id = ?
                ORDER BY started_at DESC LIMIT ?
                """, strategyId, limit);
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
