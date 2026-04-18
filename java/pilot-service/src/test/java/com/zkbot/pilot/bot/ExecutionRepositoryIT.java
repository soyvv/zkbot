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

import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;

@JdbcTest
@AutoConfigureTestDatabase(replace = AutoConfigureTestDatabase.Replace.NONE)
@ActiveProfiles("test")
@Import({ExecutionRepository.class, ObjectMapper.class})
class ExecutionRepositoryIT extends PostgresTestBase {

    @Autowired
    private JdbcTemplate jdbc;

    private ExecutionRepository repo;

    @BeforeEach
    void setUp() {
        repo = new ExecutionRepository(jdbc, new ObjectMapper());
        jdbc.update("DELETE FROM cfg.strategy_instance");
        jdbc.update("DELETE FROM cfg.strategy_definition");
        jdbc.update("DELETE FROM cfg.oms_instance");

        // Seed required parent rows
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertStrategy(jdbc, "strat_1");
    }

    @Test
    void create_execution_inserts_with_initializing_status() {
        repo.createExecution("exec-1", "strat_1", "oms_1", null);

        var row = repo.getExecution("exec-1");
        assertThat(row).isNotNull();
        assertThat(row.get("status")).isEqualTo("INITIALIZING");
        assertThat(row.get("strategy_id")).isEqualTo("strat_1");
        assertThat(row.get("target_oms_id")).isEqualTo("oms_1");
        assertThat(row.get("started_at")).isNotNull();
    }

    @Test
    void create_execution_with_config_override() {
        Map<String, Object> config = Map.of("key", "value");
        repo.createExecution("exec-cfg", "strat_1", "oms_1", config);

        var row = repo.getExecution("exec-cfg");
        assertThat(row.get("config_override").toString()).contains("key");
    }

    @Test
    void get_execution_returns_null_for_missing() {
        assertThat(repo.getExecution("nonexistent")).isNull();
    }

    @Test
    void update_execution_status_to_running() {
        repo.createExecution("exec-1", "strat_1", "oms_1", null);

        repo.updateExecutionStatus("exec-1", "RUNNING", null);

        var row = repo.getExecution("exec-1");
        assertThat(row.get("status")).isEqualTo("RUNNING");
        assertThat(row.get("ended_at")).isNull(); // non-terminal
    }

    @Test
    void update_execution_status_sets_ended_at_for_terminal_states() {
        repo.createExecution("exec-term", "strat_1", "oms_1", null);

        repo.updateExecutionStatus("exec-term", "STOPPED", "user requested");

        var row = repo.getExecution("exec-term");
        assertThat(row.get("status")).isEqualTo("STOPPED");
        assertThat(row.get("ended_at")).isNotNull();
        assertThat(row.get("error_message")).isEqualTo("user requested");
    }

    @Test
    void update_execution_status_sets_ended_at_for_failed() {
        repo.createExecution("exec-fail", "strat_1", "oms_1", null);

        repo.updateExecutionStatus("exec-fail", "FAILED", "process crashed");

        var row = repo.getExecution("exec-fail");
        assertThat(row.get("status")).isEqualTo("FAILED");
        assertThat(row.get("ended_at")).isNotNull();
    }

    @Test
    void find_running_execution_returns_null_when_none_running() {
        var result = repo.findRunningExecutionForStrategy("strat_1");
        assertThat(result).isNull();
    }

    @Test
    void find_running_execution_returns_initializing() {
        repo.createExecution("exec-init", "strat_1", "oms_1", null);
        // Status is INITIALIZING by default

        var result = repo.findRunningExecutionForStrategy("strat_1");
        assertThat(result).isNotNull();
        assertThat(result.get("execution_id")).isEqualTo("exec-init");
    }

    @Test
    void find_running_execution_returns_running() {
        repo.createExecution("exec-run", "strat_1", "oms_1", null);
        repo.updateExecutionStatus("exec-run", "RUNNING", null);

        var result = repo.findRunningExecutionForStrategy("strat_1");
        assertThat(result).isNotNull();
    }

    @Test
    void find_running_execution_returns_paused() {
        repo.createExecution("exec-pause", "strat_1", "oms_1", null);
        repo.updateExecutionStatus("exec-pause", "RUNNING", null);
        repo.updateExecutionStatus("exec-pause", "PAUSED", null);

        var result = repo.findRunningExecutionForStrategy("strat_1");
        assertThat(result).isNotNull();
    }

    @Test
    void find_running_execution_ignores_stopped() {
        repo.createExecution("exec-stopped", "strat_1", "oms_1", null);
        repo.updateExecutionStatus("exec-stopped", "STOPPED", null);

        var result = repo.findRunningExecutionForStrategy("strat_1");
        assertThat(result).isNull();
    }

    @Test
    void list_executions_filters_by_strategy() {
        TestFixtures.insertStrategy(jdbc, "strat_2");
        repo.createExecution("exec-s1", "strat_1", "oms_1", null);
        repo.createExecution("exec-s2", "strat_2", "oms_1", null);

        var results = repo.listExecutions("strat_1", null, 50);
        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("execution_id")).isEqualTo("exec-s1");
    }

    @Test
    void list_executions_filters_by_status() {
        repo.createExecution("exec-a", "strat_1", "oms_1", null);
        repo.createExecution("exec-b", "strat_1", "oms_1", null);
        repo.updateExecutionStatus("exec-b", "RUNNING", null);

        var results = repo.listExecutions(null, "INITIALIZING", 50);
        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("execution_id")).isEqualTo("exec-a");
    }

    @Test
    void list_executions_for_strategy_ordered_by_started_at_desc() {
        repo.createExecution("exec-old", "strat_1", "oms_1", null);
        repo.updateExecutionStatus("exec-old", "STOPPED", null);
        // Force distinct timestamps so ORDER BY started_at DESC is deterministic
        jdbc.update("UPDATE cfg.strategy_instance SET started_at = now() - interval '1 hour' WHERE execution_id = 'exec-old'");
        repo.createExecution("exec-new", "strat_1", "oms_1", null);

        var results = repo.listExecutionsForStrategy("strat_1", 50);
        assertThat(results).hasSize(2);
        // Newest first
        assertThat(results.getFirst().get("execution_id")).isEqualTo("exec-new");
    }
}
