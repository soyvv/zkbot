package com.zkbot.pilot.orchestrator;

import com.zkbot.pilot.config.PilotProperties;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.stereotype.Component;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.LocalDateTime;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

@Component
public class ProcessOrchestrator implements RuntimeOrchestrator, DisposableBean {

    private static final Logger log = LoggerFactory.getLogger(ProcessOrchestrator.class);
    private static final long STOP_TIMEOUT_SECONDS = 10;
    private static final DateTimeFormatter LOG_TS = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss");

    private final ConcurrentHashMap<String, Process> managed = new ConcurrentHashMap<>();
    private final PilotProperties props;

    public ProcessOrchestrator(PilotProperties props) {
        this.props = props;
    }

    @Override
    public StartResult start(String logicalId, OrchestratorProfile profile) {
        if (managed.containsKey(logicalId)) {
            Process existing = managed.get(logicalId);
            if (existing != null && existing.isAlive()) {
                return new StartResult(false, "process already running for " + logicalId);
            }
        }

        try {
            List<String> cmdLine = new ArrayList<>();
            cmdLine.add(profile.command());
            if (profile.args() != null) {
                cmdLine.addAll(profile.args());
            }

            ProcessBuilder pb = new ProcessBuilder(cmdLine);
            pb.redirectErrorStream(true);

            if (profile.workingDir() != null) {
                pb.directory(new File(profile.workingDir()));
            }
            if (profile.env() != null) {
                pb.environment().putAll(profile.env());
            }

            // Capture logs following devops/scripts/run-with-log.sh convention:
            //   <logDir>/engine-<logicalId>/<timestamp>.log  +  latest.log symlink
            File logFile = setupLogFile(logicalId);
            if (logFile != null) {
                pb.redirectOutput(ProcessBuilder.Redirect.appendTo(logFile));
                log.info("orchestrator: capturing '{}' logs to {}", logicalId, logFile.getAbsolutePath());
            } else {
                pb.inheritIO();
            }

            Process process = pb.start();
            managed.put(logicalId, process);
            log.info("orchestrator: started '{}' pid={}", logicalId, process.pid());
            return new StartResult(true, "started pid=" + process.pid());

        } catch (Exception e) {
            log.error("orchestrator: failed to start '{}': {}", logicalId, e.getMessage());
            return new StartResult(false, "start failed: " + e.getMessage());
        }
    }

    private File setupLogFile(String logicalId) {
        try {
            Path serviceLogDir = Path.of(props.engineLogDir(), "engine-" + logicalId);
            Files.createDirectories(serviceLogDir);

            String timestamp = LocalDateTime.now().format(LOG_TS);
            Path logFile = serviceLogDir.resolve(timestamp + ".log");

            // Create latest.log symlink
            Path latestLink = serviceLogDir.resolve("latest.log");
            Files.deleteIfExists(latestLink);
            Files.createSymbolicLink(latestLink, logFile.getFileName());

            return logFile.toFile();
        } catch (IOException e) {
            log.warn("orchestrator: could not set up log dir for '{}': {}", logicalId, e.getMessage());
            return null;
        }
    }

    @Override
    public StopResult stop(String logicalId, String reason) {
        Process process = managed.remove(logicalId);
        if (process == null) {
            return new StopResult(false, "no managed process for " + logicalId);
        }

        if (!process.isAlive()) {
            return new StopResult(true, "already terminated, exit=" + process.exitValue());
        }

        log.info("orchestrator: stopping '{}' reason='{}' pid={}", logicalId, reason, process.pid());
        process.destroy(); // SIGTERM

        try {
            if (!process.waitFor(STOP_TIMEOUT_SECONDS, TimeUnit.SECONDS)) {
                log.warn("orchestrator: '{}' did not exit after {}s, forcing kill",
                        logicalId, STOP_TIMEOUT_SECONDS);
                process.destroyForcibly(); // SIGKILL
                process.waitFor(5, TimeUnit.SECONDS);
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            process.destroyForcibly();
            return new StopResult(false, "interrupted while stopping");
        }

        int exitCode = process.isAlive() ? -1 : process.exitValue();
        return new StopResult(true, "stopped, exit=" + exitCode);
    }

    @Override
    public RuntimeStatus status(String logicalId) {
        Process process = managed.get(logicalId);
        if (process == null) {
            return new RuntimeStatus(logicalId, false, null);
        }
        if (process.isAlive()) {
            return new RuntimeStatus(logicalId, true, null);
        }
        return new RuntimeStatus(logicalId, false, process.exitValue());
    }

    @Override
    public void destroy() {
        log.info("orchestrator: shutting down, stopping {} managed processes", managed.size());
        for (Map.Entry<String, Process> entry : managed.entrySet()) {
            try {
                stop(entry.getKey(), "application shutdown");
            } catch (Exception e) {
                log.warn("orchestrator: error stopping '{}': {}", entry.getKey(), e.getMessage());
            }
        }
    }
}
