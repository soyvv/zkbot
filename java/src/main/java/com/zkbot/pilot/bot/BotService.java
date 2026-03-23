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

import java.time.Instant;
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

    public StrategyResponse createStrategy(CreateStrategyRequest request) {
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

        return toStrategyResponse(strategyRepo.getStrategy(request.strategyKey()));
    }

    public StrategyResponse updateStrategy(String strategyKey, UpdateStrategyRequest request) {
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
        return toStrategyResponse(strategyRepo.getStrategy(strategyKey));
    }

    public StrategyResponse getStrategy(String strategyKey) {
        var row = strategyRepo.findStrategyWithCurrentExecution(strategyKey);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "strategy '" + strategyKey + "' not found");
        }
        return toStrategyResponse(row);
    }

    public List<StrategyResponse> listStrategies(String status, String venue,
                                                   String omsId, String type, int limit) {
        return strategyRepo.listStrategies(status, venue, omsId, type, limit).stream()
                .map(BotService::toStrategyResponse)
                .toList();
    }

    public ValidationResult validateStrategy(String strategyKey) {
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

        return new ValidationResult(issues.isEmpty(), issues, strategyKey);
    }

    // --- Enriched config builder ---

    public Map<String, Object> buildEnrichedConfig(String strategyId) {
        var accountRows = jdbc.queryForList(
                "SELECT DISTINCT account_id FROM cfg.account_binding ORDER BY account_id");
        var accountIds = accountRows.stream()
                .map(r -> r.get("account_id"))
                .toList();

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

        var running = executionRepo.findRunningExecutionForStrategy(request.strategyKey());
        if (running != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "strategy '" + request.strategyKey() + "' already has a running execution: " +
                    running.get("execution_id"));
        }

        String executionId = UUID.randomUUID().toString();
        String targetOmsId = resolveOmsId(strategy);

        executionRepo.createExecution(executionId, request.strategyKey(),
                targetOmsId, request.runtimeParams());

        var enrichedConfig = buildEnrichedConfig(request.strategyKey());

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

        executionRepo.updateExecutionStatus(executionId, "RUNNING", null);

        log.info("Started execution {} for strategy {}", executionId, request.strategyKey());
        return new StartExecutionResponse(executionId, request.strategyKey(), "RUNNING", enrichedConfig);
    }

    public ExecutionResponse stopExecution(String executionId, String reason) {
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

        var orchStatus = orchestrator.status(executionId);
        if (orchStatus.running()) {
            orchestrator.stop(executionId, reason != null ? reason : "api-stop");
        }

        executionRepo.updateExecutionStatus(executionId, "STOPPED", reason);
        log.info("Stopped execution {} reason={}", executionId, reason);

        return toExecutionResponse(executionRepo.getExecution(executionId), null, null);
    }

    public ExecutionResponse pauseExecution(String executionId) {
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
        return toExecutionResponse(executionRepo.getExecution(executionId), null, null);
    }

    public ExecutionResponse resumeExecution(String executionId) {
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
        return toExecutionResponse(executionRepo.getExecution(executionId), null, null);
    }

    public StartExecutionResponse restartExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        String strategyId = (String) execution.get("strategy_id");

        String status = (String) execution.get("status");
        if ("RUNNING".equals(status) || "PAUSED".equals(status) || "INITIALIZING".equals(status)) {
            stopExecution(executionId, "restart");
        }

        @SuppressWarnings("unchecked")
        var runtimeParams = (Map<String, Object>) execution.get("config_override");
        return startExecution(new StartExecutionRequest(strategyId, "restart", runtimeParams, false));
    }

    // --- Execution queries ---

    public ExecutionResponse getExecution(String executionId) {
        var execution = executionRepo.getExecution(executionId);
        if (execution == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "execution '" + executionId + "' not found");
        }

        var orchStatus = orchestrator.status(executionId);
        return toExecutionResponse(execution, orchStatus.running(), orchStatus.exitCode());
    }

    public List<ExecutionResponse> listExecutions(String strategyId, String status, int limit) {
        return executionRepo.listExecutions(strategyId, status, limit).stream()
                .map(row -> toExecutionResponse(row, null, null))
                .toList();
    }

    public List<ExecutionOrderEntry> getExecutionOpenOrders(String executionId) {
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
            var orders = new ArrayList<ExecutionOrderEntry>();
            for (var order : response.getOrdersList()) {
                orders.add(new ExecutionOrderEntry(
                        order.getOrderId(), order.getInstrument(),
                        order.getBuySellType().name(), order.getPrice(),
                        order.getQty(), order.getOrderStatus().name()));
            }
            return orders;
        } catch (Exception e) {
            log.warn("Failed to query open orders for execution {}: {}", executionId, e.getMessage());
            return List.of();
        }
    }

    public List<ExecutionResponse> getExecutionActivities(String executionId, int limit) {
        return List.of();
    }

    public List<ExecutionResponse> getExecutionLifecycles(String executionId) {
        return List.of();
    }

    public List<ExecutionResponse> getExecutionLogs(String executionId, int limit) {
        return List.of();
    }

    public List<ExecutionResponse> getStrategyExecutions(String strategyKey, int limit) {
        return executionRepo.listExecutionsForStrategy(strategyKey, limit).stream()
                .map(row -> toExecutionResponse(row, null, null))
                .toList();
    }

    public List<ExecutionResponse> getStrategyLogs(String strategyKey, int limit) {
        return List.of();
    }

    // --- Helpers ---

    private String resolveOmsId(Map<String, Object> strategy) {
        Object configObj = strategy.get("config");
        if (configObj instanceof Map<?, ?> config) {
            Object omsId = config.get("target_oms_id");
            if (omsId != null) return omsId.toString();
        }
        var rows = jdbc.queryForList("SELECT oms_id FROM cfg.oms_instance LIMIT 1");
        if (rows.isEmpty()) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "no OMS instance configured");
        }
        return (String) rows.getFirst().get("oms_id");
    }

    @SuppressWarnings("unchecked")
    private static StrategyResponse toStrategyResponse(Map<String, Object> row) {
        Object configObj = row.get("config_json");
        Map<String, Object> config = null;
        if (configObj instanceof Map) {
            config = (Map<String, Object>) configObj;
        }

        Object accountsObj = row.get("default_accounts");
        List<Long> defaultAccounts = null;
        if (accountsObj instanceof Long[] arr) {
            defaultAccounts = List.of(arr);
        } else if (accountsObj instanceof Object[] arr) {
            defaultAccounts = Arrays.stream(arr)
                    .filter(Objects::nonNull)
                    .map(o -> ((Number) o).longValue())
                    .toList();
        }

        Object symbolsObj = row.get("default_symbols");
        List<String> defaultSymbols = null;
        if (symbolsObj instanceof String[] arr) {
            defaultSymbols = List.of(arr);
        } else if (symbolsObj instanceof Object[] arr) {
            defaultSymbols = Arrays.stream(arr)
                    .filter(Objects::nonNull)
                    .map(Object::toString)
                    .toList();
        }

        return new StrategyResponse(
                (String) row.get("strategy_id"),
                (String) row.get("runtime_type"),
                (String) row.get("code_ref"),
                (String) row.get("description"),
                defaultAccounts,
                defaultSymbols,
                config,
                Boolean.TRUE.equals(row.get("enabled")),
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at")),
                (String) row.get("execution_id"),
                (String) row.get("execution_status"),
                toInstant(row.get("execution_started_at")),
                toInstant(row.get("execution_ended_at"))
        );
    }

    @SuppressWarnings("unchecked")
    private static ExecutionResponse toExecutionResponse(Map<String, Object> row,
                                                          Boolean processRunning,
                                                          Integer processExitCode) {
        Object configObj = row.get("config_override");
        Map<String, Object> configOverride = null;
        if (configObj instanceof Map) {
            configOverride = (Map<String, Object>) configObj;
        }

        return new ExecutionResponse(
                (String) row.get("execution_id"),
                (String) row.get("strategy_id"),
                (String) row.get("target_oms_id"),
                (String) row.get("status"),
                (String) row.get("error_message"),
                toInstant(row.get("started_at")),
                toInstant(row.get("ended_at")),
                configOverride,
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at")),
                processRunning,
                processExitCode
        );
    }

    private static Instant toInstant(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Instant i) return i;
        if (obj instanceof java.sql.Timestamp ts) return ts.toInstant();
        if (obj instanceof java.time.OffsetDateTime odt) return odt.toInstant();
        return null;
    }
}
