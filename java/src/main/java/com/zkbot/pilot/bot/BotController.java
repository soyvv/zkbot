package com.zkbot.pilot.bot;

import com.zkbot.pilot.bot.dto.*;
import com.zkbot.pilot.topology.dto.BootstrapTokenResponse;
import org.springframework.http.HttpStatus;
import org.springframework.web.bind.annotation.*;

import java.util.List;

@RestController
@RequestMapping("/v1/bot")
public class BotController {

    private final BotService service;

    public BotController(BotService service) {
        this.service = service;
    }

    // --- Strategy endpoints ---

    @PostMapping("/strategies")
    @ResponseStatus(HttpStatus.CREATED)
    public StrategyResponse createStrategy(@RequestBody CreateStrategyRequest request) {
        return service.createStrategy(request);
    }

    @PutMapping("/strategies/{strategyKey}")
    public StrategyResponse updateStrategy(@PathVariable String strategyKey,
                                            @RequestBody UpdateStrategyRequest request) {
        return service.updateStrategy(strategyKey, request);
    }

    @GetMapping("/strategies")
    public List<StrategyResponse> listStrategies(
            @RequestParam(required = false) String status,
            @RequestParam(required = false) String venue,
            @RequestParam(required = false) String omsId,
            @RequestParam(required = false) String type,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listStrategies(status, venue, omsId, type, limit);
    }

    @GetMapping("/strategies/{strategyKey}")
    public StrategyResponse getStrategy(@PathVariable String strategyKey) {
        return service.getStrategy(strategyKey);
    }

    @PostMapping("/strategies/{strategyKey}/validate")
    public ValidationResult validateStrategy(@PathVariable String strategyKey) {
        return service.validateStrategy(strategyKey);
    }

    @GetMapping("/strategies/{strategyKey}/executions")
    public List<ExecutionResponse> getStrategyExecutions(
            @PathVariable String strategyKey,
            @RequestParam(defaultValue = "50") int limit) {
        return service.getStrategyExecutions(strategyKey, limit);
    }

    @GetMapping("/strategies/{strategyKey}/logs")
    public List<ExecutionResponse> getStrategyLogs(
            @PathVariable String strategyKey,
            @RequestParam(defaultValue = "100") int limit) {
        return service.getStrategyLogs(strategyKey, limit);
    }

    // --- Bot Definition endpoints ---

    @PostMapping("/definitions")
    @ResponseStatus(HttpStatus.CREATED)
    public BotDefinitionResponse createBotDefinition(@RequestBody CreateBotDefinitionRequest request) {
        return service.createBotDefinition(request);
    }

    @GetMapping("/definitions")
    public List<BotDefinitionResponse> listBotDefinitions(
            @RequestParam(defaultValue = "50") int limit) {
        return service.listBotDefinitions(limit);
    }

    @GetMapping("/definitions/{engineId}")
    public BotDefinitionResponse getBotDefinition(@PathVariable String engineId) {
        return service.getBotDefinition(engineId);
    }

    @PutMapping("/definitions/{engineId}")
    public BotDefinitionResponse updateBotDefinition(@PathVariable String engineId,
                                                      @RequestBody UpdateBotDefinitionRequest request) {
        return service.updateBotDefinition(engineId, request);
    }

    @PostMapping("/definitions/{engineId}/issue-bootstrap-token")
    public BootstrapTokenResponse issueBotBootstrapToken(@PathVariable String engineId) {
        return service.issueBotBootstrapToken(engineId);
    }

    @PostMapping("/definitions/{engineId}/start")
    @ResponseStatus(HttpStatus.CREATED)
    public StartExecutionResponse startBot(@PathVariable String engineId) {
        return service.startBot(engineId);
    }

    @PostMapping("/definitions/{engineId}/stop")
    public ExecutionResponse stopBot(@PathVariable String engineId,
                                      @RequestBody(required = false) StopExecutionRequest request) {
        String reason = request != null ? request.reason() : "api-stop";
        return service.stopBot(engineId, reason);
    }

    // --- Execution endpoints ---

    @PostMapping("/executions/start")
    @ResponseStatus(HttpStatus.CREATED)
    public StartExecutionResponse startExecution(@RequestBody StartExecutionRequest request) {
        return service.startExecution(request);
    }

    @PostMapping("/executions/{executionId}/stop")
    public ExecutionResponse stopExecution(@PathVariable String executionId,
                                            @RequestBody(required = false) StopExecutionRequest request) {
        String reason = request != null ? request.reason() : "api-stop";
        return service.stopExecution(executionId, reason);
    }

    @PostMapping("/executions/{executionId}/pause")
    public ExecutionResponse pauseExecution(@PathVariable String executionId) {
        return service.pauseExecution(executionId);
    }

    @PostMapping("/executions/{executionId}/resume")
    public ExecutionResponse resumeExecution(@PathVariable String executionId) {
        return service.resumeExecution(executionId);
    }

    @PostMapping("/executions/{executionId}/restart")
    public StartExecutionResponse restartExecution(@PathVariable String executionId) {
        return service.restartExecution(executionId);
    }

    @GetMapping("/executions")
    public List<ExecutionResponse> listExecutions(
            @RequestParam(required = false) String strategyId,
            @RequestParam(required = false) String status,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listExecutions(strategyId, status, limit);
    }

    @GetMapping("/executions/{executionId}")
    public ExecutionResponse getExecution(@PathVariable String executionId) {
        return service.getExecution(executionId);
    }

    @GetMapping("/executions/{executionId}/orders/open")
    public List<ExecutionOrderEntry> getExecutionOpenOrders(@PathVariable String executionId) {
        return service.getExecutionOpenOrders(executionId);
    }

    @GetMapping("/executions/{executionId}/activities")
    public List<ExecutionResponse> getExecutionActivities(
            @PathVariable String executionId,
            @RequestParam(defaultValue = "50") int limit) {
        return service.getExecutionActivities(executionId, limit);
    }

    @GetMapping("/executions/{executionId}/lifecycles")
    public List<ExecutionResponse> getExecutionLifecycles(@PathVariable String executionId) {
        return service.getExecutionLifecycles(executionId);
    }

    @GetMapping("/executions/{executionId}/logs")
    public List<ExecutionResponse> getExecutionLogs(
            @PathVariable String executionId,
            @RequestParam(defaultValue = "100") int limit) {
        return service.getExecutionLogs(executionId, limit);
    }
}
