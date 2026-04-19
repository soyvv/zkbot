package com.zkbot.pilot.bot;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.bot.dto.*;
import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryResolver;
import com.zkbot.pilot.grpc.OmsGrpcClient;
import com.zkbot.pilot.orchestrator.ProcessOrchestrator;
import com.zkbot.pilot.orchestrator.RuntimeOrchestrator.OrchestratorProfile;
import com.zkbot.pilot.topology.dto.BootstrapTokenResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.oms.v1.Oms;

import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Instant;
import java.util.*;

@Service
public class BotService {

    private static final Logger log = LoggerFactory.getLogger(BotService.class);

    private final StrategyRepository strategyRepo;
    private final ExecutionRepository executionRepo;
    private final BotDefinitionRepository botDefRepo;
    private final ProcessOrchestrator orchestrator;
    private final OmsGrpcClient omsClient;
    private final DiscoveryResolver discoveryResolver;
    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper;
    private final PilotProperties props;
    private final TokenService tokenService;

    public BotService(StrategyRepository strategyRepo, ExecutionRepository executionRepo,
                      BotDefinitionRepository botDefRepo,
                      ProcessOrchestrator orchestrator, OmsGrpcClient omsClient,
                      DiscoveryResolver discoveryResolver, JdbcTemplate jdbc,
                      ObjectMapper objectMapper, PilotProperties props, TokenService tokenService) {
        this.strategyRepo = strategyRepo;
        this.executionRepo = executionRepo;
        this.botDefRepo = botDefRepo;
        this.orchestrator = orchestrator;
        this.omsClient = omsClient;
        this.discoveryResolver = discoveryResolver;
        this.jdbc = jdbc;
        this.objectMapper = objectMapper;
        this.props = props;
        this.tokenService = tokenService;
    }

    // --- Strategy CRUD ---

    public StrategyResponse createStrategy(CreateStrategyRequest request) {
        if (request.strategyKey() == null || request.strategyKey().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "strategyKey is required");
        }
        if (request.runtimeType() == null || request.runtimeType().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "runtimeType is required");
        }
        if (request.strategyTypeKey() == null || request.strategyTypeKey().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "strategyTypeKey is required");
        }

        var existing = strategyRepo.getStrategy(request.strategyKey());
        if (existing != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "strategy '" + request.strategyKey() + "' already exists");
        }

        strategyRepo.createStrategy(
                request.strategyKey(), request.runtimeType(), request.codeRef(),
                request.strategyTypeKey(), request.description(),
                request.defaultAccounts(), request.defaultSymbols(),
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
        if (request.strategyTypeKey() != null) fields.put("strategyTypeKey", request.strategyTypeKey());
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
                .map(this::toStrategyResponse)
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

    // --- Runtime scope extraction ---

    /**
     * Extract concrete runtime scope (accounts, instruments, timing) from
     * the bot's provided_config. Returns null if required fields are missing.
     *
     * Strategy identity (strategy_key, strategy_type_key) is NOT included —
     * those come from the strategy row, not operator-entered bot config.
     */
    public Map<String, Object> extractRuntimeScope(Map<String, Object> providedConfig) {
        if (providedConfig == null) return null;

        List<?> accountIds = toList(providedConfig.get("account_ids"));
        List<?> instruments = toList(providedConfig.get("instruments"));

        if (accountIds == null || accountIds.isEmpty()) return null;
        if (instruments == null || instruments.isEmpty()) return null;

        var result = new HashMap<String, Object>();
        result.put("account_ids", accountIds);
        result.put("instruments", instruments);
        result.put("discovery_bucket", "zk-svc-registry-v1");

        // Pass through optional timing config
        if (providedConfig.containsKey("timer_interval_ms"))
            result.put("timer_interval_ms", providedConfig.get("timer_interval_ms"));
        if (providedConfig.containsKey("kline_interval"))
            result.put("kline_interval", providedConfig.get("kline_interval"));

        return result;
    }

    // LEGACY: deprecated, uses global scope. Kept for startExecution() backward compat.
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

        // LEGACY: deprecated, uses global scope. Will be removed when startExecution is retired.
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
        return new StartExecutionResponse(executionId, request.strategyKey(), null, "RUNNING", enrichedConfig);
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

    // --- Bot Definition CRUD ---

    public BotDefinitionResponse createBotDefinition(CreateBotDefinitionRequest request) {
        if (request.engineId() == null || request.engineId().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "engineId is required");
        }
        if (request.strategyId() == null || request.strategyId().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "strategyId is required");
        }
        if (request.targetOmsId() == null || request.targetOmsId().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "targetOmsId is required");
        }

        var existingBot = botDefRepo.getBotDefinition(request.engineId());
        if (existingBot != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "bot definition '" + request.engineId() + "' already exists");
        }

        var strategy = strategyRepo.getStrategy(request.strategyId());
        if (strategy == null) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "strategy '" + request.strategyId() + "' not found");
        }

        var omsRows = jdbc.queryForList(
                "SELECT 1 FROM cfg.oms_instance WHERE oms_id = ?", request.targetOmsId());
        if (omsRows.isEmpty()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "OMS '" + request.targetOmsId() + "' not found");
        }

        botDefRepo.createBotDefinition(request.engineId(), request.strategyId(),
                request.targetOmsId(), request.description(),
                request.providedConfig(), request.enabled());
        syncBotLogicalInstance(request.engineId(), request.strategyId(), request.targetOmsId(), request.enabled());

        return toBotDefinitionResponse(botDefRepo.getBotDefinition(request.engineId()));
    }

    public BotDefinitionResponse updateBotDefinition(String engineId, UpdateBotDefinitionRequest request) {
        var existing = botDefRepo.getBotDefinition(engineId);
        if (existing == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "bot definition '" + engineId + "' not found");
        }

        var fields = new HashMap<String, Object>();
        if (request.description() != null) fields.put("description", request.description());
        if (request.providedConfig() != null) fields.put("providedConfig", request.providedConfig());
        if (request.enabled() != null) fields.put("enabled", request.enabled());

        botDefRepo.updateBotDefinition(engineId, fields);
        var updated = botDefRepo.getBotDefinition(engineId);
        syncBotLogicalInstance(
                engineId,
                (String) updated.get("strategy_id"),
                (String) updated.get("target_oms_id"),
                Boolean.TRUE.equals(updated.get("enabled")));
        return toBotDefinitionResponse(updated);
    }

    public BootstrapTokenResponse issueBotBootstrapToken(String engineId) {
        var row = botDefRepo.getBotDefinition(engineId);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "bot definition '" + engineId + "' not found");
        }
        syncBotLogicalInstance(
                engineId,
                (String) row.get("strategy_id"),
                (String) row.get("target_oms_id"),
                Boolean.TRUE.equals(row.get("enabled")));
        return tokenService.generateToken(engineId, "ENGINE", props.env(), 30);
    }

    public BotDefinitionResponse getBotDefinition(String engineId) {
        var row = botDefRepo.getBotDefinition(engineId);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "bot definition '" + engineId + "' not found");
        }
        return toBotDefinitionResponse(row);
    }

    public List<BotDefinitionResponse> listBotDefinitions(int limit) {
        return botDefRepo.listBotDefinitions(limit).stream()
                .map(this::toBotDefinitionResponse)
                .toList();
    }

    // --- Bot-level Start/Stop ---

    public StartExecutionResponse startBot(String engineId) {
        var botDef = botDefRepo.getBotDefinition(engineId);
        if (botDef == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "bot definition '" + engineId + "' not found");
        }
        if (!Boolean.TRUE.equals(botDef.get("enabled"))) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "bot '" + engineId + "' is not enabled");
        }

        String strategyId = (String) botDef.get("strategy_id");
        if (strategyId == null || strategyId.isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "bot '" + engineId + "' has no strategy_id set");
        }

        var strategy = strategyRepo.getStrategy(strategyId);
        if (strategy == null) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "strategy '" + strategyId + "' not found");
        }
        if (!Boolean.TRUE.equals(strategy.get("enabled"))) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "strategy '" + strategyId + "' is not enabled");
        }

        String targetOmsId = (String) botDef.get("target_oms_id");
        if (targetOmsId != null) {
            var omsRows = jdbc.queryForList(
                    "SELECT 1 FROM cfg.oms_instance WHERE oms_id = ?", targetOmsId);
            if (omsRows.isEmpty()) {
                throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                        "target OMS '" + targetOmsId + "' not found");
            }
        }

        var running = executionRepo.findRunningExecutionForEngine(engineId);
        if (running != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "bot '" + engineId + "' already has a running execution: " +
                    running.get("execution_id"));
        }

        // Fail-fast: strategy_type_key is required on the strategy row.
        String strategyTypeKey = (String) strategy.get("strategy_type_key");
        if (strategyTypeKey == null || strategyTypeKey.isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "bot '" + engineId + "': strategy '" + strategyId
                    + "' has no strategy_type_key set — set it before starting");
        }

        // Extract runtime scope from bot's provided_config.
        var engineConfig = parseJsonbColumn(botDef.get("provided_config"));
        var runtimeScope = extractRuntimeScope(engineConfig);
        if (runtimeScope == null) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "bot '" + engineId + "' has no account_ids or instruments in provided_config");
        }

        String executionId = UUID.randomUUID().toString();

        // Build snapshots for execution record.
        var strategyConfig = parseJsonbColumn(strategy.get("config_json"));
        var strategyConfigSnapshot = new HashMap<String, Object>();
        if (strategyConfig != null) strategyConfigSnapshot.putAll(strategyConfig);

        var engineConfigSnapshot = new HashMap<String, Object>();
        if (engineConfig != null) engineConfigSnapshot.putAll(engineConfig);

        var bindingSnapshot = new HashMap<>(runtimeScope);

        executionRepo.createExecution(executionId, strategyId, engineId, targetOmsId,
                null, strategyConfigSnapshot, engineConfigSnapshot, bindingSnapshot);

        // Build env vars for direct-mode engine launch (temporary bridge).
        var env = new HashMap<String, String>();

        // -- Bootstrap vars (from PilotProperties / deployment) --
        env.put("ZK_NATS_URL", props.natsUrl());
        env.put("ZK_ENV", props.env());
        env.put("ZK_ENGINE_ID", engineId);
        env.put("ZK_BOOTSTRAP_TOKEN", "");              // empty = direct mode
        env.put("ZK_DISCOVERY_BUCKET", "zk-svc-registry-v1");
        env.put("ZK_GRPC_PORT", "0");                  // OS assigns; engine registers actual in KV
        env.put("ZK_GRPC_HOST", "127.0.0.1");
        env.put("ZK_KV_HEARTBEAT_SECS", "10");

        // -- Strategy identity (from strategy row, NOT from bot's provided_config) --
        env.put("EXECUTION_ID", executionId);
        env.put("ZK_STRATEGY_KEY", strategyId);         // strategy_key == strategy_id
        env.put("ZK_STRATEGY_TYPE_KEY", strategyTypeKey);
        if (strategyConfig != null && !strategyConfig.isEmpty()) {
            env.put("ZK_STRATEGY_CONFIG_JSON", toJson(strategyConfig));
        }
        if (targetOmsId != null) env.put("ZK_OMS_ID", targetOmsId);

        // -- Engine scope (from bot's provided_config via runtimeScope) --
        @SuppressWarnings("unchecked")
        List<Object> accountIds = (List<Object>) runtimeScope.get("account_ids");
        env.put("ZK_ACCOUNT_IDS", accountIds.stream()
                .map(String::valueOf).collect(java.util.stream.Collectors.joining(",")));

        @SuppressWarnings("unchecked")
        List<Object> instruments = (List<Object>) runtimeScope.get("instruments");
        env.put("ZK_INSTRUMENTS", instruments.stream()
                .map(String::valueOf).collect(java.util.stream.Collectors.joining(",")));

        // -- Optional timing config --
        Object timerMs = runtimeScope.get("timer_interval_ms");
        if (timerMs != null) env.put("ZK_TIMER_INTERVAL_MS", String.valueOf(timerMs));
        Object klineInterval = runtimeScope.get("kline_interval");
        if (klineInterval != null) env.put("ZK_KLINE_INTERVAL", String.valueOf(klineInterval));

        // -- Launch engine process --
        // engineBinaryPath selects the host runtime binary.
        // strategy_type_key (in env) selects the strategy implementation inside that host.
        String engineCommand = resolveEngineBinaryPath();
        var profile = new OrchestratorProfile(
                engineCommand, List.of(), null, env);
        var result = orchestrator.start(engineId, profile);
        if (!result.success()) {
            executionRepo.updateExecutionStatus(executionId, "FAILED", result.message());
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "failed to start process: " + result.message());
        }

        log.info("Started bot {} execution {} strategy={} type_key={}",
                engineId, executionId, strategyId, strategyTypeKey);
        return new StartExecutionResponse(executionId, strategyId, engineId, "INITIALIZING", runtimeScope);
    }

    public ExecutionResponse stopBot(String engineId, String reason) {
        var running = executionRepo.findRunningExecutionForEngine(engineId);
        if (running == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "no running execution found for bot '" + engineId + "'");
        }

        String executionId = (String) running.get("execution_id");

        var orchStatus = orchestrator.status(engineId);
        if (orchStatus.running()) {
            orchestrator.stop(engineId, reason != null ? reason : "api-stop");
        }

        executionRepo.updateExecutionStatus(executionId, "STOPPED", reason);
        log.info("Stopped bot {} execution {} reason={}", engineId, executionId, reason);

        return toExecutionResponse(executionRepo.getExecution(executionId), null, null);
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
    private StrategyResponse toStrategyResponse(Map<String, Object> row) {
        Map<String, Object> config = parseJsonbColumn(row.get("config_json"));

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
                (String) row.get("strategy_type_key"),
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

    @SuppressWarnings("unchecked")
    private BotDefinitionResponse toBotDefinitionResponse(Map<String, Object> row) {
        Map<String, Object> providedConfig = parseJsonbColumn(row.get("provided_config"));

        return new BotDefinitionResponse(
                (String) row.get("engine_id"),
                (String) row.get("strategy_id"),
                (String) row.get("target_oms_id"),
                (String) row.get("description"),
                Boolean.TRUE.equals(row.get("enabled")),
                providedConfig,
                (String) row.get("runtime_type"),
                (String) row.get("code_ref"),
                (String) row.get("current_execution_id"),
                (String) row.get("current_execution_status"),
                toInstant(row.get("current_execution_started_at")),
                toInstant(row.get("current_execution_ended_at")),
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at"))
        );
    }

    private static Instant toInstant(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Instant i) return i;
        if (obj instanceof java.sql.Timestamp ts) return ts.toInstant();
        if (obj instanceof java.time.OffsetDateTime odt) return odt.toInstant();
        return null;
    }

    /**
     * Parse a JSONB column value from JDBC. PostgreSQL returns JSONB as PGobject
     * (a string wrapper), not as a Map. This handles both cases.
     */
    @SuppressWarnings("unchecked")
    private Map<String, Object> parseJsonbColumn(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Map) return (Map<String, Object>) obj;
        // PGobject or other string-like JSONB representation
        String json = obj.toString();
        if (json.isBlank() || "{}".equals(json)) return null;
        try {
            return objectMapper.readValue(json, new TypeReference<Map<String, Object>>() {});
        } catch (Exception e) {
            log.warn("Failed to parse JSONB column: {}", e.getMessage());
            return null;
        }
    }

    /** Safely extract a List from a value that may be a List, Collection, or null. */
    @SuppressWarnings("unchecked")
    private static List<?> toList(Object obj) {
        if (obj == null) return null;
        if (obj instanceof List<?> list) return list;
        if (obj instanceof Collection<?> col) return new ArrayList<>(col);
        return null;
    }

    private String toJson(Object obj) {
        try {
            return objectMapper.writeValueAsString(obj);
        } catch (Exception e) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "failed to serialize strategy config for engine launch");
        }
    }

    private void syncBotLogicalInstance(String engineId, String strategyId, String targetOmsId, boolean enabled) {
        var metadata = new HashMap<String, Object>();
        if (strategyId != null) metadata.put("strategy_id", strategyId);
        if (targetOmsId != null) metadata.put("target_oms_id", targetOmsId);
        metadata.put("domain", "bot");

        jdbc.update("""
                INSERT INTO cfg.logical_instance (logical_id, instance_type, env, metadata, enabled)
                VALUES (?, 'ENGINE', ?, ?::jsonb, ?)
                ON CONFLICT (logical_id) DO UPDATE
                  SET instance_type = EXCLUDED.instance_type,
                      env = EXCLUDED.env,
                      metadata = EXCLUDED.metadata,
                      enabled = EXCLUDED.enabled
                """, engineId, props.env(), toJson(metadata), enabled);
    }

    private String resolveEngineBinaryPath() {
        String configured = props.engineBinaryPath();
        if (configured == null || configured.isBlank()) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "pilot.engine-binary-path is not configured");
        }
        Path bin = Path.of(configured);
        if (!bin.isAbsolute() || !Files.isRegularFile(bin) || !Files.isExecutable(bin)) {
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR,
                    "pilot.engine-binary-path must be absolute and executable: " + configured);
        }
        return bin.toAbsolutePath().normalize().toString();
    }
}
