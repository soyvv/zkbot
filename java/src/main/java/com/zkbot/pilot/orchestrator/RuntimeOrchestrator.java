package com.zkbot.pilot.orchestrator;

import java.util.List;
import java.util.Map;

public interface RuntimeOrchestrator {

    StartResult start(String logicalId, OrchestratorProfile profile);

    StopResult stop(String logicalId, String reason);

    RuntimeStatus status(String logicalId);

    record OrchestratorProfile(String command, List<String> args, String workingDir,
                               Map<String, String> env) {}

    record StartResult(boolean success, String message) {}

    record StopResult(boolean success, String message) {}

    record RuntimeStatus(String logicalId, boolean running, Integer exitCode) {}
}
