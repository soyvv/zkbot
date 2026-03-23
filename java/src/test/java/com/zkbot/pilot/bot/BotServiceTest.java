package com.zkbot.pilot.bot;

import com.zkbot.pilot.bot.dto.CreateStrategyRequest;
import com.zkbot.pilot.bot.dto.StartExecutionRequest;
import com.zkbot.pilot.bot.dto.UpdateStrategyRequest;
import com.zkbot.pilot.discovery.DiscoveryResolver;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import com.zkbot.pilot.orchestrator.ProcessOrchestrator;
import com.zkbot.pilot.orchestrator.RuntimeOrchestrator.RuntimeStatus;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.web.server.ResponseStatusException;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;
import static org.mockito.ArgumentMatchers.*;
import static org.mockito.Mockito.*;

@ExtendWith(MockitoExtension.class)
class BotServiceTest {

    @Mock StrategyRepository strategyRepo;
    @Mock ExecutionRepository executionRepo;
    @Mock ProcessOrchestrator orchestrator;
    @Mock OmsGrpcClient omsClient;
    @Mock DiscoveryResolver discoveryResolver;
    @Mock JdbcTemplate jdbc;

    BotService service;

    @BeforeEach
    void setUp() {
        service = new BotService(strategyRepo, executionRepo, orchestrator,
                omsClient, discoveryResolver, jdbc);
    }

    // ── Strategy CRUD ─────────────────────────────────────────────────────

    @Nested
    class StrategyCrud {

        @Test
        void create_strategy_rejects_blank_key() {
            var req = new CreateStrategyRequest("", "RUST", "ref", "desc", null, null, null);

            assertThatThrownBy(() -> service.createStrategy(req))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("strategyKey");
        }

        @Test
        void create_strategy_rejects_duplicate() {
            var req = new CreateStrategyRequest("strat_1", "RUST", "ref", "desc", null, null, null);
            when(strategyRepo.getStrategy("strat_1")).thenReturn(Map.of("strategy_id", "strat_1"));

            assertThatThrownBy(() -> service.createStrategy(req))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("already exists");
        }

        @Test
        void update_strategy_throws_404_for_missing() {
            when(strategyRepo.getStrategy("nonexistent")).thenReturn(null);

            assertThatThrownBy(() -> service.updateStrategy("nonexistent",
                    new UpdateStrategyRequest("desc", null, null, null, null)))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }

        @Test
        void get_strategy_throws_404_for_missing() {
            when(strategyRepo.findStrategyWithCurrentExecution("nonexistent")).thenReturn(null);

            assertThatThrownBy(() -> service.getStrategy("nonexistent"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not found");
        }
    }

    // ── Execution lifecycle ───────────────────────────────────────────────

    @Nested
    class ExecutionLifecycle {

        @Test
        void start_execution_returns_409_when_strategy_already_has_running_instance() {
            when(strategyRepo.getStrategy("strat_1")).thenReturn(
                    strategyMap("strat_1", true));
            when(executionRepo.findRunningExecutionForStrategy("strat_1")).thenReturn(
                    Map.of("execution_id", "existing-exec"));

            var req = new StartExecutionRequest("strat_1", "test", null, false);

            assertThatThrownBy(() -> service.startExecution(req))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("already has a running execution");
        }

        @Test
        void start_execution_rejects_disabled_strategy() {
            when(strategyRepo.getStrategy("strat_dis")).thenReturn(
                    strategyMap("strat_dis", false));

            var req = new StartExecutionRequest("strat_dis", "test", null, false);

            assertThatThrownBy(() -> service.startExecution(req))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not enabled");
        }

        @Test
        void start_execution_creates_execution_and_returns_running() {
            when(strategyRepo.getStrategy("strat_1")).thenReturn(
                    strategyMap("strat_1", true));
            when(executionRepo.findRunningExecutionForStrategy("strat_1")).thenReturn(null);
            when(jdbc.queryForList(anyString())).thenReturn(
                    List.of(Map.of("oms_id", (Object) "oms_1")));
            when(jdbc.queryForList(startsWith("SELECT DISTINCT account_id"))).thenReturn(List.of());

            var req = new StartExecutionRequest("strat_1", "test", null, false);
            var response = service.startExecution(req);

            assertThat(response.status()).isEqualTo("RUNNING");
            assertThat(response.executionId()).isNotNull();
            verify(executionRepo).createExecution(anyString(), eq("strat_1"), eq("oms_1"), isNull());
            verify(executionRepo).updateExecutionStatus(anyString(), eq("RUNNING"), isNull());
        }

        @Test
        void start_execution_calls_orchestrator_when_orchestrated() {
            when(strategyRepo.getStrategy("strat_1")).thenReturn(
                    strategyMapWithCodeRef("strat_1", true, "my_binary"));
            when(executionRepo.findRunningExecutionForStrategy("strat_1")).thenReturn(null);
            when(jdbc.queryForList(anyString())).thenReturn(
                    List.of(Map.of("oms_id", (Object) "oms_1")));
            when(jdbc.queryForList(startsWith("SELECT DISTINCT account_id"))).thenReturn(List.of());
            when(orchestrator.start(anyString(), any())).thenReturn(
                    new com.zkbot.pilot.orchestrator.RuntimeOrchestrator.StartResult(true, "started"));

            var req = new StartExecutionRequest("strat_1", "test", null, true);
            service.startExecution(req);

            verify(orchestrator).start(anyString(), any());
        }

        @Test
        void start_execution_skips_orchestrator_when_not_orchestrated() {
            when(strategyRepo.getStrategy("strat_1")).thenReturn(
                    strategyMap("strat_1", true));
            when(executionRepo.findRunningExecutionForStrategy("strat_1")).thenReturn(null);
            when(jdbc.queryForList(anyString())).thenReturn(
                    List.of(Map.of("oms_id", (Object) "oms_1")));
            when(jdbc.queryForList(startsWith("SELECT DISTINCT account_id"))).thenReturn(List.of());

            var req = new StartExecutionRequest("strat_1", "test", null, false);
            service.startExecution(req);

            verify(orchestrator, never()).start(anyString(), any());
        }

        @Test
        void stop_execution_updates_status_and_stops_orchestrator() {
            when(executionRepo.getExecution("exec-1")).thenReturn(
                    executionMap("exec-1", "RUNNING"));
            when(orchestrator.status("exec-1")).thenReturn(new RuntimeStatus("exec-1", true, null));

            service.stopExecution("exec-1", "user stop");

            verify(orchestrator).stop("exec-1", "user stop");
            verify(executionRepo).updateExecutionStatus("exec-1", "STOPPED", "user stop");
        }

        @Test
        void stop_execution_rejects_already_stopped() {
            when(executionRepo.getExecution("exec-1")).thenReturn(
                    executionMap("exec-1", "STOPPED"));

            assertThatThrownBy(() -> service.stopExecution("exec-1", "reason"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not in a stoppable state");
        }

        @Test
        void pause_sets_status_paused() {
            when(executionRepo.getExecution("exec-1"))
                    .thenReturn(executionMap("exec-1", "RUNNING"))
                    .thenReturn(executionMap("exec-1", "PAUSED"));

            service.pauseExecution("exec-1");

            verify(executionRepo).updateExecutionStatus("exec-1", "PAUSED", null);
        }

        @Test
        void pause_rejects_non_running() {
            when(executionRepo.getExecution("exec-1")).thenReturn(
                    executionMap("exec-1", "PAUSED"));

            assertThatThrownBy(() -> service.pauseExecution("exec-1"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not RUNNING");
        }

        @Test
        void resume_sets_status_running_from_paused() {
            when(executionRepo.getExecution("exec-1"))
                    .thenReturn(executionMap("exec-1", "PAUSED"))
                    .thenReturn(executionMap("exec-1", "RUNNING"));

            service.resumeExecution("exec-1");

            verify(executionRepo).updateExecutionStatus("exec-1", "RUNNING", null);
        }

        @Test
        void resume_rejects_non_paused() {
            when(executionRepo.getExecution("exec-1")).thenReturn(
                    executionMap("exec-1", "RUNNING"));

            assertThatThrownBy(() -> service.resumeExecution("exec-1"))
                    .isInstanceOf(ResponseStatusException.class)
                    .hasMessageContaining("not PAUSED");
        }
    }

    // ── Execution queries ─────────────────────────────────────────────────

    @Test
    void get_execution_enriches_with_orchestrator_status() {
        var execMap = new HashMap<String, Object>();
        execMap.put("execution_id", "exec-1");
        execMap.put("status", "RUNNING");
        when(executionRepo.getExecution("exec-1")).thenReturn(execMap);
        when(orchestrator.status("exec-1")).thenReturn(new RuntimeStatus("exec-1", true, null));

        var result = service.getExecution("exec-1");

        assertThat(result.processRunning()).isEqualTo(true);
    }

    @Test
    void get_execution_includes_exit_code_when_process_dead() {
        var execMap = new HashMap<String, Object>();
        execMap.put("execution_id", "exec-1");
        execMap.put("status", "STOPPED");
        when(executionRepo.getExecution("exec-1")).thenReturn(execMap);
        when(orchestrator.status("exec-1")).thenReturn(new RuntimeStatus("exec-1", false, 137));

        var result = service.getExecution("exec-1");

        assertThat(result.processRunning()).isEqualTo(false);
        assertThat(result.processExitCode()).isEqualTo(137);
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    private Map<String, Object> strategyMap(String id, boolean enabled) {
        var m = new HashMap<String, Object>();
        m.put("strategy_id", id);
        m.put("runtime_type", "RUST");
        m.put("code_ref", "ref");
        m.put("enabled", enabled);
        return m;
    }

    private Map<String, Object> strategyMapWithCodeRef(String id, boolean enabled, String codeRef) {
        var m = strategyMap(id, enabled);
        m.put("code_ref", codeRef);
        return m;
    }

    private Map<String, Object> executionMap(String id, String status) {
        var m = new HashMap<String, Object>();
        m.put("execution_id", id);
        m.put("strategy_id", "strat_1");
        m.put("status", status);
        return m;
    }
}
