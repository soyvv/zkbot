package com.zkbot.pilot.meta;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.List;
import java.util.Map;

@Repository
public class MetaRepository {

    private final JdbcTemplate jdbc;

    public MetaRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    public List<Map<String, Object>> listOmsInstances() {
        return jdbc.queryForList(
                "SELECT oms_id, namespace, description, enabled FROM cfg.oms_instance ORDER BY oms_id");
    }

    public List<Map<String, Object>> listGatewayInstances() {
        return jdbc.queryForList(
                "SELECT gw_id, venue, broker_type, account_type, enabled FROM cfg.gateway_instance ORDER BY gw_id");
    }

    public List<Map<String, Object>> listAccounts() {
        return jdbc.queryForList("""
                SELECT a.account_id, a.exch_account_id, a.venue, a.status,
                       ab.oms_id, ab.gw_id, ab.enabled AS binding_enabled
                FROM cfg.account a
                LEFT JOIN cfg.account_binding ab ON a.account_id = ab.account_id
                ORDER BY a.account_id
                """);
    }

    public List<Map<String, Object>> listStrategies() {
        return jdbc.queryForList("""
                SELECT sd.strategy_id, sd.runtime_type, sd.description, sd.enabled,
                       si.execution_id, si.status AS execution_status, si.target_oms_id
                FROM cfg.strategy_definition sd
                LEFT JOIN cfg.strategy_instance si
                  ON si.strategy_id = sd.strategy_id
                  AND si.status IN ('INITIALIZING', 'RUNNING', 'PAUSED')
                ORDER BY sd.strategy_id
                """);
    }

    public List<String> listDistinctVenues() {
        return jdbc.queryForList(
                "SELECT DISTINCT venue FROM cfg.gateway_instance WHERE venue IS NOT NULL ORDER BY venue",
                String.class);
    }

    public List<Map<String, Object>> listLogicalInstances(String env) {
        return jdbc.queryForList(
                "SELECT logical_id, instance_type, env, enabled FROM cfg.logical_instance WHERE env = ? ORDER BY logical_id",
                env);
    }
}
