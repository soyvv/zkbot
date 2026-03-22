package com.zkbot.pilot.bot;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.support.PostgresTestBase;
import com.zkbot.pilot.support.TestFixtures;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.autoconfigure.jdbc.AutoConfigureTestDatabase;
import org.springframework.boot.test.autoconfigure.jdbc.JdbcTest;
import org.springframework.context.annotation.Import;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.test.context.ActiveProfiles;

import java.sql.Array;
import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;

@JdbcTest
@AutoConfigureTestDatabase(replace = AutoConfigureTestDatabase.Replace.NONE)
@ActiveProfiles("test")
@Import({StrategyRepository.class, ObjectMapper.class})
class StrategyRepositoryIT extends PostgresTestBase {

    @Autowired
    private JdbcTemplate jdbc;

    private StrategyRepository repo;

    @BeforeEach
    void setUp() {
        repo = new StrategyRepository(jdbc, new ObjectMapper());
        jdbc.update("DELETE FROM cfg.strategy_instance");
        jdbc.update("DELETE FROM cfg.strategy_definition");
        // Need supporting tables for venue filter tests
        jdbc.update("DELETE FROM cfg.account_binding");
        jdbc.update("DELETE FROM cfg.account");
        jdbc.update("DELETE FROM cfg.gateway_instance");
        jdbc.update("DELETE FROM cfg.oms_instance");
    }

    @Test
    void create_strategy_persists_basic_fields() {
        repo.createStrategy("strat_1", "RUST", "path::to::strategy",
                "test desc", null, null, null);

        var row = repo.getStrategy("strat_1");
        assertThat(row).isNotNull();
        assertThat(row.get("strategy_id")).isEqualTo("strat_1");
        assertThat(row.get("runtime_type")).isEqualTo("RUST");
        assertThat(row.get("code_ref")).isEqualTo("path::to::strategy");
        assertThat(row.get("description")).isEqualTo("test desc");
        assertThat(row.get("enabled")).isEqualTo(true);
    }

    @Test
    void create_strategy_persists_array_columns() {
        repo.createStrategy("strat_arr", "RUST", "code_ref",
                "desc", List.of(9001L, 9002L), List.of("BTCUSDT", "ETHUSDT"), null);

        var row = jdbc.queryForMap("SELECT default_accounts, default_symbols FROM cfg.strategy_definition WHERE strategy_id = 'strat_arr'");

        // JDBC returns java.sql.Array for PostgreSQL arrays
        Array accountsArr = (Array) row.get("default_accounts");
        Array symbolsArr = (Array) row.get("default_symbols");

        assertThat(accountsArr).isNotNull();
        assertThat(symbolsArr).isNotNull();

        try {
            Long[] accounts = (Long[]) accountsArr.getArray();
            String[] symbols = (String[]) symbolsArr.getArray();
            assertThat(accounts).containsExactly(9001L, 9002L);
            assertThat(symbols).containsExactly("BTCUSDT", "ETHUSDT");
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    @Test
    void create_strategy_persists_config_json() {
        Map<String, Object> config = Map.of("param1", "value1", "param2", 42);
        repo.createStrategy("strat_cfg", "RUST", "code_ref", "desc",
                null, null, config);

        var row = jdbc.queryForMap("SELECT config_json FROM cfg.strategy_definition WHERE strategy_id = 'strat_cfg'");

        // config_json comes back as PGobject or String depending on driver
        String configJson = row.get("config_json").toString();
        assertThat(configJson).contains("param1");
        assertThat(configJson).contains("value1");
    }

    @Test
    void get_strategy_returns_null_for_missing() {
        var row = repo.getStrategy("nonexistent");
        assertThat(row).isNull();
    }

    @Test
    void update_strategy_updates_only_provided_fields() {
        repo.createStrategy("strat_upd", "RUST", "code_ref", "original desc",
                null, null, null);

        repo.updateStrategy("strat_upd", Map.of("description", "updated desc"));

        var row = repo.getStrategy("strat_upd");
        assertThat(row.get("description")).isEqualTo("updated desc");
        assertThat(row.get("runtime_type")).isEqualTo("RUST"); // unchanged
    }

    @Test
    void update_strategy_can_disable() {
        repo.createStrategy("strat_dis", "RUST", "code_ref", "desc",
                null, null, null);

        repo.updateStrategy("strat_dis", Map.of("enabled", false));

        var row = repo.getStrategy("strat_dis");
        assertThat(row.get("enabled")).isEqualTo(false);
    }

    @Test
    void find_strategy_with_current_execution_joins_running_instance() {
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        repo.createStrategy("strat_exec", "RUST", "code_ref", "desc",
                null, null, null);

        jdbc.update("""
                INSERT INTO cfg.strategy_instance (execution_id, strategy_id, target_oms_id, status, started_at)
                VALUES ('exec-1', 'strat_exec', 'oms_1', 'RUNNING', now())
                """);

        var row = repo.findStrategyWithCurrentExecution("strat_exec");

        assertThat(row).isNotNull();
        assertThat(row.get("execution_id")).isEqualTo("exec-1");
        assertThat(row.get("execution_status")).isEqualTo("RUNNING");
    }

    @Test
    void find_strategy_with_current_execution_ignores_stopped() {
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        repo.createStrategy("strat_stop", "RUST", "code_ref", "desc",
                null, null, null);

        jdbc.update("""
                INSERT INTO cfg.strategy_instance (execution_id, strategy_id, target_oms_id, status, started_at, ended_at)
                VALUES ('exec-1', 'strat_stop', 'oms_1', 'STOPPED', now() - interval '1 hour', now())
                """);

        var row = repo.findStrategyWithCurrentExecution("strat_stop");

        assertThat(row).isNotNull();
        assertThat(row.get("execution_id")).isNull(); // LEFT JOIN, no match for RUNNING/PAUSED/INITIALIZING
    }

    @Test
    void list_strategies_filters_by_status() {
        repo.createStrategy("strat_enabled", "RUST", "code_ref", "desc", null, null, null);
        repo.createStrategy("strat_disabled", "RUST", "code_ref", "desc", null, null, null);
        repo.updateStrategy("strat_disabled", Map.of("enabled", false));

        var enabled = repo.listStrategies("enabled", null, null, null, 50);
        assertThat(enabled).hasSize(1);
        assertThat(enabled.getFirst().get("strategy_id")).isEqualTo("strat_enabled");
    }

    @Test
    void list_strategies_filters_by_type() {
        repo.createStrategy("strat_rust", "RUST", "code_ref", "desc", null, null, null);
        repo.createStrategy("strat_py", "PYTHON", "code_ref", "desc", null, null, null);

        var results = repo.listStrategies(null, null, null, "RUST", 50);
        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("strategy_id")).isEqualTo("strat_rust");
    }

    @Test
    void list_strategies_filters_by_venue_via_account_binding_join() {
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertGatewayInstance(jdbc, "gw_sim", "SIM");
        TestFixtures.insertGatewayInstance(jdbc, "gw_prod", "PROD");
        TestFixtures.insertAccount(jdbc, 9001, "SIM");
        TestFixtures.insertAccountBinding(jdbc, 9001, "oms_1", "gw_sim");

        repo.createStrategy("strat_sim", "RUST", "code_ref", "desc", null, null, null);

        // Create an execution to link strategy to OMS (venue filter requires this join)
        jdbc.update("""
                INSERT INTO cfg.strategy_instance (execution_id, strategy_id, target_oms_id, status, started_at)
                VALUES ('exec-sim', 'strat_sim', 'oms_1', 'RUNNING', now())
                """);

        var simResults = repo.listStrategies(null, "SIM", null, null, 50);
        assertThat(simResults).hasSize(1);

        var prodResults = repo.listStrategies(null, "PROD", null, null, 50);
        assertThat(prodResults).isEmpty();
    }
}
