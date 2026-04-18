package com.zkbot.pilot.bot;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class StrategyRepository {

    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper;

    public StrategyRepository(JdbcTemplate jdbc, ObjectMapper objectMapper) {
        this.jdbc = jdbc;
        this.objectMapper = objectMapper;
    }

    public void createStrategy(String strategyId, String runtimeType, String codeRef,
                                String strategyTypeKey, String description,
                                List<Long> defaultAccounts, List<String> defaultSymbols,
                                Map<String, Object> configJson) {
        Long[] accountsArr = defaultAccounts != null ? defaultAccounts.toArray(Long[]::new) : new Long[0];
        String[] symbolsArr = defaultSymbols != null ? defaultSymbols.toArray(String[]::new) : new String[0];
        jdbc.update(con -> {
            var ps = con.prepareStatement("""
                    INSERT INTO cfg.strategy_definition
                      (strategy_id, runtime_type, code_ref, strategy_type_key, description,
                       default_accounts, default_symbols, config_json, enabled)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?::jsonb, true)
                    """);
            ps.setString(1, strategyId);
            ps.setString(2, runtimeType);
            ps.setString(3, codeRef);
            ps.setString(4, strategyTypeKey);
            ps.setString(5, description);
            ps.setArray(6, con.createArrayOf("bigint", accountsArr));
            ps.setArray(7, con.createArrayOf("text", symbolsArr));
            ps.setString(8, toJson(configJson));
            return ps;
        });
    }

    @SuppressWarnings("unchecked")
    public void updateStrategy(String strategyId, Map<String, Object> fields) {
        if (fields == null || fields.isEmpty()) return;

        var setClauses = new ArrayList<String>();
        boolean hasAccounts = fields.containsKey("defaultAccounts");
        boolean hasSymbols = fields.containsKey("defaultSymbols");

        if (fields.containsKey("description")) setClauses.add("description = ?");
        if (fields.containsKey("strategyTypeKey")) setClauses.add("strategy_type_key = ?");
        if (hasAccounts) setClauses.add("default_accounts = ?");
        if (hasSymbols) setClauses.add("default_symbols = ?");
        if (fields.containsKey("config")) setClauses.add("config_json = ?::jsonb");
        if (fields.containsKey("enabled")) setClauses.add("enabled = ?");

        if (setClauses.isEmpty()) return;
        setClauses.add("updated_at = now()");

        String sql = "UPDATE cfg.strategy_definition SET " +
                String.join(", ", setClauses) + " WHERE strategy_id = ?";

        jdbc.update(con -> {
            var ps = con.prepareStatement(sql);
            int idx = 1;
            if (fields.containsKey("description")) ps.setString(idx++, (String) fields.get("description"));
            if (fields.containsKey("strategyTypeKey")) ps.setString(idx++, (String) fields.get("strategyTypeKey"));
            if (hasAccounts) {
                var list = (List<Long>) fields.get("defaultAccounts");
                ps.setArray(idx++, con.createArrayOf("bigint", list != null ? list.toArray(Long[]::new) : new Long[0]));
            }
            if (hasSymbols) {
                var list = (List<String>) fields.get("defaultSymbols");
                ps.setArray(idx++, con.createArrayOf("text", list != null ? list.toArray(String[]::new) : new String[0]));
            }
            if (fields.containsKey("config")) ps.setString(idx++, toJson(fields.get("config")));
            if (fields.containsKey("enabled")) ps.setBoolean(idx++, (Boolean) fields.get("enabled"));
            ps.setString(idx, strategyId);
            return ps;
        });
    }

    public Map<String, Object> getStrategy(String strategyId) {
        var rows = jdbc.queryForList(
                "SELECT * FROM cfg.strategy_definition WHERE strategy_id = ?", strategyId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public List<Map<String, Object>> listStrategies(String status, String venue,
                                                     String omsId, String type, int limit) {
        var conditions = new ArrayList<String>();
        var params = new ArrayList<Object>();

        conditions.add("1=1");

        if (status != null && !status.isBlank()) {
            conditions.add("sd.enabled = ?");
            params.add("enabled".equalsIgnoreCase(status));
        }
        if (type != null && !type.isBlank()) {
            conditions.add("sd.runtime_type = ?");
            params.add(type);
        }

        boolean needOmsJoin = omsId != null && !omsId.isBlank();
        boolean needVenueJoin = venue != null && !venue.isBlank();
        String from = "cfg.strategy_definition sd";
        if (needOmsJoin || needVenueJoin) {
            from += " JOIN cfg.strategy_instance si ON si.strategy_id = sd.strategy_id";
            if (needOmsJoin) {
                conditions.add("si.target_oms_id = ?");
                params.add(omsId);
            }
            if (needVenueJoin) {
                // venue lives on gateway_instance, reachable via account_binding
                from += " JOIN cfg.account_binding ab ON ab.oms_id = si.target_oms_id"
                      + " JOIN cfg.gateway_instance gi ON gi.gw_id = ab.gw_id";
                conditions.add("gi.venue = ?");
                params.add(venue);
            }
        }

        if (limit <= 0) limit = 50;
        params.add(limit);

        String sql = "SELECT DISTINCT sd.* FROM " + from +
                " WHERE " + String.join(" AND ", conditions) +
                " ORDER BY sd.strategy_id LIMIT ?";
        return jdbc.queryForList(sql, params.toArray());
    }

    public Map<String, Object> findStrategyWithCurrentExecution(String strategyId) {
        var rows = jdbc.queryForList("""
                SELECT sd.*, si.execution_id, si.status AS execution_status,
                       si.started_at AS execution_started_at, si.ended_at AS execution_ended_at
                FROM cfg.strategy_definition sd
                LEFT JOIN cfg.strategy_instance si
                  ON si.strategy_id = sd.strategy_id
                  AND si.status IN ('INITIALIZING', 'RUNNING', 'PAUSED')
                WHERE sd.strategy_id = ?
                ORDER BY si.started_at DESC NULLS LAST
                LIMIT 1
                """, strategyId);
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
