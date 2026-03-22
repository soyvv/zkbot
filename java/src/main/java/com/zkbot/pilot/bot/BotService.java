package com.zkbot.pilot.bot;

import com.zkbot.pilot.bot.dto.*;
import com.zkbot.pilot.discovery.DiscoveryResolver;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import com.zkbot.pilot.orchestrator.ProcessOrchestrator;
import com.zkbot.pilot.orchestrator.RuntimeOrchestrator.OrchestratorProfile;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.oms.v1.Oms;

import java.util.*;

@Service
public class BotService {

    private static final Logger log = LoggerFactory.getLogger(BotService.class);

    private final StrategyRepository strategyRepo;
    private final ExecutionRepository executionRepo;
    private final ProcessOrchestrator orchestrator;
    private final OmsGrpcClient omsClient;
    private final DiscoveryResolver discoveryResolver;
    private final JdbcTemplate jdbc;

    public BotService(StrategyRepository strategyRepo, ExecutionRepository executionRepo,
                      ProcessOrchestrator orchestrator, OmsGrpcClient omsClient,
                      DiscoveryResolver discoveryResolver, JdbcTemplate jdbc) {
        this.strategyRepo = strategyRepo;
        this.executionRepo = executionRepo;
        this.orchestrator = orchestrator;
        this.omsClient = omsClient;
        this.discoveryResolver = discoveryResolver;
        this.jdbc = jdbc;
    }

    // --- Strategy CRUD ---

    public Map<String, Object> createStrategy(CreateStrategyRequest request) {
        if (request.strategyKey() == null || request.strategyKey().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "strategyKey is required");
        }
        if (request.runtimeType() == null || request.runtimeType().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "runtimeType is required");
        }

        var existing = strategyRepo.getStrategy(request.strategyKey());
        if (existing != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "strategy '" + request.strategyKey() + "' already exists");
        }

        strategyRepo.createStrategy(
                request.strategyKey(), request.runtimeType(), request.codeRef(),
                request.description(), request.defaultAccounts(), request.defaultSymbols(),
                request.config());

        return strategyRepo.getStrategy(request.strategyKey());
    }

    public Map<String, Object> updateStrategy(String strategyKey, UpdateStrategyRequest request) {
        var existing = strategyRepo.getStrategy(strategyKey);
        if (existing == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "strategy '" + strategyKey + "' not found");
        }

        var fields = new HashMap<String, Object>();
        if (request.description() != null) fields.put("description", request.description());
        if (request.defaultAccounts() != null) fields.put("defaultAccounts", request.defaultAccounts());
        if (request.defaultSymbols() != null) fields.put("defaultSymbols", request.defaultSymbols());
        if (request.config() != null) fields.put("config", request.config());
        if (request.enabled() != null) fields.put("enabled", request.enabled());

        strategyRepo.updateStrategy(strategyKey, fields);
        return strategyRepo.getStrategy(strategyKey);
    }

    public Map<String, Object> getStrategy(String strategyKey) {
        var row = strategyRepo.findStrategyWithCurrentExecution(strategyKey);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "strategy '" + strategyKey + "' not found");
        }
        return row;
    }

    public List<Map<String, Object>> listStrategies(String status, String venue,
                                                     String omsId, String type, int limit) {
        return strategyRepo.listStrategies(status, venue, omsId, type, limit);
    }

    public Map<String, Object> validateStrategy(String strategyKey) {
        var strategy = strategyRepo.getStrategy(strategyKey);
        if (strategy == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "strategy '" + strategyKey + "' not found");
        }

        var issues = new ArrayList<String>();

        if (strategy.get("code_ref") == null) {
            issues.add("code_ref is not set");
        }
        if (strategy.get("runtime_type") == null) {
            issues.add("runtime_type is not set");
        }

        boolean valid = issues.isEmpty();
        return Map.of("valid", valid, "issues", issues, "strategyKey", strategyKey);
    }

    // --- Enriched config builder ---

    public Map<String, Object> buildEnrichedConfig(String strategyId) {
        // Account IDs from account bindings
        var accountRows = jdbc.queryForList(
                "SELECT DISTINCT account_id FROM cfg.account_binding ORDER BY account_id");
        var accountIds = accountRows.stream()
                .map(r -> r.get("account_id"))
                .toList();

        // Instruments for those accounts
        List<Map<String, Object>> instrumentRows;
        if (!accountIds.isEmpty()) {
            instrumentRows = jdbc.queryForList("""
                    SELECT DISTINCT instrument_id FROM cfg.account_instrument_config
                    WHERE account_id = ANY(?::bigint[]) AND enabled = true
                    """, accountIds.stream()
                    .map(String::valueOf)
                    .reduce((a, b) -> a + "," + b)
                    .map(s -> "{" + s + "}")
                    .orElse("{}"));
        } else {
            instrumentRows = List.of();
        }

        var instruments = instrumentRows.stream()
                .map(r -> r.get("instrument_id"))
                .toList();

        // Fallback to all instruments if no per-account config
        List<?> finalInstruments;
        if (instruments.isEmpty()) {
            var fallback = jdbc.queryForList(
                    "SELECT instrument_id FROM cfg.instrument_refdata WHERE disabled = false");
            finalInstruments = fallback.stream().map(r -> r.get("instrument_id")).toList();
        } else {
            finalInstruments = instruments;
        }

        return Map.of(
                "account_ids", accountIds,
                "instruments", finalInstruments,
                "discovery_bucket", "zk-svc-registry-v1",
                "secret_refs", Map.of("vault_path", "kv/trading/gw")
        );
    }

    // --- Execution lifecycle ---

    public StartExecutionResponse startExecution(StartExecutionRequest request) {
        var strategy = strategyRepo.getStrategy(request.strategyKey());
        if (strategy == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "strategy '" + request.strategyKey() + "' not found");
        }

        if (!Boolean.TRUE.equals(strategy.get("enabled"))) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "strategy '" + request.strategyKey() + "' is not enabled");
        }

        // Singleton check
        var running = executionRepo.findRunningExecutionForStrategy(request.strategyKey());
        if (running != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "strategy '" + request.strategyKey() + "' already has a running execution: " +
                    running.get("execution_id"));
        }

        String executionId = UUID.randomUUID().toString();

        // Resolve target OMS
        String targetOmsId = resolveOmsId(strategy);

        // Insert execution
        executionRepo.createExecution(executionId, request.strategyKey(),
                targetOmsId, request.runtimeParams());

        // Build enriched config
        var enrichedConfig = buildEnrichedConfig(request.strategyKey());

        // Optionally start orchestrated process
        if (request.orchestrated()) {
            String codeRef = (String) strategy.get("code_ref");
            if (codeRef != null && !codeRef.isBlank()) {
                var profile = new OrchestratorProfile(
                        codeRef, List.of("--execution-id", executionId), null,
                        Map.of("EXECUTION_ID", executionId, "STRATEGY_ID", request.strategyKey()));
                var result = orchestrator.start(executionId, profile);
                if (!result.success()) {
                    executionRepo.updateExecutionStatus(executionId, "FAILED", result.message());
                    throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                            "failed to start process: " + result.message());
                }
            }
        }

        // Update status to RUNNING
        executionRepo.updateExecutionStatus(executionId, "RUNNING", null);

        log.info("Started execution {} for strategy {}", executionId, request.strategyKey());
        return new StartExecutionResponse(executionId, request.strategyKey(), "RUNNING", enrichedConfig);
    }

    public Map<String, Object> stopExecution(String executionId, String reason) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        String status = (String) execution.get("status");
        if (!"RUNNING".equals(status) && !"PAUSED".equals(status) && !"INITIALIZING".equals(status)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "execution '" + executionId + "' is not in a stoppable state: " + status);
        }

        // Stop orchestrated process if managed
        var orchStatus = orchestrator.status(executionId);
        if (orchStatus.running()) {
            orchestrator.stop(executionId, reason != null ? reason : "api-stop");
        }

        executionRepo.updateExecutionStatus(executionId, "STOPPED", reason);
        log.info("Stopped execution {} reason={}", executionId, reason);

        return executionRepo.getExecution(executionId);
    }

    public Map<String, Object> pauseExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }
        if (!"RUNNING".equals(execution.get("status"))) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "execution '" + executionId + "' is not RUNNING");
        }
        executionRepo.updateExecutionStatus(executionId, "PAUSED", null);
        return executionRepo.getExecution(executionId);
    }

    public Map<String, Object> resumeExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }
        if (!"PAUSED".equals(execution.get("status"))) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "execution '" + executionId + "' is not PAUSED");
        }
        executionRepo.updateExecutionStatus(executionId, "RUNNING", null);
        return executionRepo.getExecution(executionId);
    }

    public StartExecutionResponse restartExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        String strategyId = (String) execution.get("strategy_id");

        // Stop current execution
        String status = (String) execution.get("status");
        if ("RUNNING".equals(status) || "PAUSED".equals(status) || "INITIALIZING".equals(status)) {
            stopExecution(executionId, "restart");
        }

        // Start new execution with same config
        @SuppressWarnings("unchecked")
        var runtimeParams = (Map<String, Object>) execution.get("config_override");
        return startExecution(new StartExecutionRequest(strategyId, "restart", runtimeParams, false));
    }

    // --- Execution queries ---

    public Map<String, Object> getExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        // Enrich with live orchestrator status
        var orchStatus = orchestrator.status(executionId);
        execution.put("process_running", orchStatus.running());
        if (orchStatus.exitCode() != null) {
            execution.put("process_exit_code", orchStatus.exitCode());
        }

        return execution;
    }

    public List<Map<String, Object>> listExecutions(String strategyId, String status, int limit) {
        return executionRepo.listExecutions(strategyId, status, limit);
    }

    public List<Map<String, Object>> getExecutionOpenOrders(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        String omsId = (String) execution.get("target_oms_id");
        if (omsId == null) {
            return List.of();
        }

        try {
            var request = Oms.QueryOpenOrderRequest.newBuilder().build();
            var response = omsClient.queryOpenOrders(omsId, request);
            // Filter and convert to maps — simplified, returns raw order details
            var orders = new ArrayList<Map<String, Object>>();
            for (var order : response.getOrdersList()) {
                var m = new HashMap<String, Object>();
                m.put("order_id", order.getOrderId());
                m.put("instrument", order.getInstrument());
                m.put("side", order.getBuySellType().name());
                m.put("price", order.getPrice());
                m.put("qty", order.getQty());
                m.put("status", order.getOrderStatus().name());
                orders.add(m);
            }
            return orders;
        } catch (Exception e) {
            log.warn("Failed to query open orders for execution {}: {}", executionId, e.getMessage());
            return List.of();
        }
    }

    public List<Map<String, Object>> getExecutionActivities(String executionId, int limit) {
        // Stub — MongoDB query deferred
        return List.of();
    }

    public List<Map<String, Object>> getExecutionLifecycles(String executionId) {
        // Stub — status change history deferred
        return List.of();
    }

    public List<Map<String, Object>> getExecutionLogs(String executionId, int limit) {
        // Stub — MongoDB query deferred
        return List.of();
    }

    public List<Map<String, Object>> getStrategyExecutions(String strategyKey, int limit) {
        return executionRepo.listExecutionsForStrategy(strategyKey, limit);
    }

    public List<Map<String, Object>> getStrategyLogs(String strategyKey, int limit) {
        // Stub — MongoDB query deferred
        return List.of();
    }

    // --- Helpers ---

    private String resolveOmsId(Map<String, Object> strategy) {
        // Try strategy config for target_oms_id, otherwise pick default
        Object configObj = strategy.get("config");
        if (configObj instanceof Map<?, ?> config) {
            Object omsId = config.get("target_oms_id");
            if (omsId != null) return omsId.toString();
        }
        // Fallback: pick first OMS instance
        var rows = jdbc.queryForList("SELECT oms_id FROM cfg.oms_instance LIMIT 1");
        if (rows.isEmpty()) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "no OMS instance configured");
        }
        return (String) rows.getFirst().get("oms_id");
    }
}
