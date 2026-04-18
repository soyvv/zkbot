package com.zkbot.pilot.orchestrator;

import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.orchestrator.RuntimeOrchestrator.OrchestratorProfile;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Timeout;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;

@Timeout(15)
class ProcessOrchestratorTest {

    private final ProcessOrchestrator orchestrator = new ProcessOrchestrator(
            new PilotProperties("nats://localhost:4222", "test", "test", 60, null, null, "zk-engine-svc", "logs"));

    @AfterEach
    void tearDown() {
        orchestrator.destroy();
    }

    @Test
    void start_creates_child_process_and_tracks_it() {
        var profile = new OrchestratorProfile("sleep", List.of("60"), null, null);

        var result = orchestrator.start("test-1", profile);

        assertThat(result.success()).isTrue();
        assertThat(orchestrator.status("test-1").running()).isTrue();
    }

    @Test
    void stop_terminates_running_process() {
        var profile = new OrchestratorProfile("sleep", List.of("60"), null, null);
        orchestrator.start("test-1", profile);

        var result = orchestrator.stop("test-1", "test cleanup");

        assertThat(result.success()).isTrue();
        assertThat(orchestrator.status("test-1").running()).isFalse();
    }

    @Test
    void stop_returns_failure_for_unknown_logical_id() {
        var result = orchestrator.stop("nonexistent", "reason");

        assertThat(result.success()).isFalse();
        assertThat(result.message()).contains("no managed process");
    }

    @Test
    void status_returns_not_running_for_unknown_logical_id() {
        var status = orchestrator.status("nonexistent");

        assertThat(status.running()).isFalse();
        assertThat(status.exitCode()).isNull();
    }

    @Test
    void start_rejects_duplicate_running_process() {
        var profile = new OrchestratorProfile("sleep", List.of("60"), null, null);
        orchestrator.start("test-1", profile);

        var result = orchestrator.start("test-1", profile);

        assertThat(result.success()).isFalse();
        assertThat(result.message()).contains("already running");
    }

    @Test
    void status_includes_exit_code_after_process_terminates() throws Exception {
        // Use "true" which exits immediately with code 0
        var profile = new OrchestratorProfile("true", List.of(), null, null);
        orchestrator.start("test-exit", profile);

        // Wait for process to exit
        Thread.sleep(500);

        var status = orchestrator.status("test-exit");
        assertThat(status.running()).isFalse();
        assertThat(status.exitCode()).isEqualTo(0);
    }

    @Test
    void start_allows_reuse_after_previous_process_terminates() throws Exception {
        var shortProfile = new OrchestratorProfile("true", List.of(), null, null);
        orchestrator.start("test-reuse", shortProfile);
        Thread.sleep(500); // Wait for exit

        // Previous process terminated — should allow new start
        var longProfile = new OrchestratorProfile("sleep", List.of("60"), null, null);
        var result = orchestrator.start("test-reuse", longProfile);

        assertThat(result.success()).isTrue();
        assertThat(orchestrator.status("test-reuse").running()).isTrue();
    }

    @Test
    void start_passes_environment_variables() {
        var env = Map.of("MY_VAR", "my_value");
        var profile = new OrchestratorProfile("sleep", List.of("60"), null, env);

        var result = orchestrator.start("test-env", profile);

        assertThat(result.success()).isTrue();
    }

    @Test
    void destroy_terminates_all_tracked_processes() {
        var profile = new OrchestratorProfile("sleep", List.of("60"), null, null);
        orchestrator.start("p1", profile);
        orchestrator.start("p2", profile);
        orchestrator.start("p3", profile);

        orchestrator.destroy();

        // After destroy, all should be gone from the map
        assertThat(orchestrator.status("p1").running()).isFalse();
        assertThat(orchestrator.status("p2").running()).isFalse();
        assertThat(orchestrator.status("p3").running()).isFalse();
    }
}
