package com.zkbot.pilot.bot;

import com.zkbot.pilot.bot.dto.*;
import org.springframework.http.HttpStatus;
import org.springframework.web.bind.annotation.*;

import java.util.List;
import java.util.Map;

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
    public Map<String, Object> createStrategy(@RequestBody CreateStrategyRequest request) {
        return service.createStrategy(request);
    }

    @PutMapping("/strategies/{strategyKey}")
    public Map<String, Object> updateStrategy(@PathVariable String strategyKey,
                                               @RequestBody UpdateStrategyRequest request) {
        return service.updateStrategy(strategyKey, request);
    }

    @GetMapping("/strategies")
    public List<Map<String, Object>> listStrategies(
            @RequestParam(required = false) String status,
            @RequestParam(required = false) String venue,
            @RequestParam(required = false) String omsId,
            @RequestParam(required = false) String type,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listStrategies(status, venue, omsId, type, limit);
    }

    @GetMapping("/strategies/{strategyKey}")
    public Map<String, Object> getStrategy(@PathVariable String strategyKey) {
        return service.getStrategy(strategyKey);
    }

    @PostMapping("/strategies/{strategyKey}/validate")
    public Map<String, Object> validateStrategy(@PathVariable String strategyKey) {
        return service.validateStrategy(strategyKey);
    }

    @GetMapping("/strategies/{strategyKey}/executions")
    public List<Map<String, Object>> getStrategyExecutions(
            @PathVariable String strategyKey,
            @RequestParam(defaultValue = "50") int limit) {
        return service.getStrategyExecutions(strategyKey, limit);
    }

    @GetMapping("/strategies/{strategyKey}/logs")
    public List<Map<String, Object>> getStrategyLogs(
            @PathVariable String strategyKey,
            @RequestParam(defaultValue = "100") int limit) {
        return service.getStrategyLogs(strategyKey, limit);
    }

    // --- Execution endpoints ---

    @PostMapping("/executions/start")
    @ResponseStatus(HttpStatus.CREATED)
    public StartExecutionResponse startExecution(@RequestBody StartExecutionRequest request) {
        return service.startExecution(request);
    }

    @PostMapping("/executions/{executionId}/stop")
    public Map<String, Object> stopExecution(@PathVariable String executionId,
                                              @RequestBody(required = false) StopExecutionRequest request) {
        String reason = request != null ? request.reason() : "api-stop";
        return service.stopExecution(executionId, reason);
    }

    @PostMapping("/executions/{executionId}/pause")
    public Map<String, Object> pauseExecution(@PathVariable String executionId) {
        return service.pauseExecution(executionId);
    }

    @PostMapping("/executions/{executionId}/resume")
    public Map<String, Object> resumeExecution(@PathVariable String executionId) {
        return service.resumeExecution(executionId);
    }

    @PostMapping("/executions/{executionId}/restart")
    public StartExecutionResponse restartExecution(@PathVariable String executionId) {
        return service.restartExecution(executionId);
    }

    @GetMapping("/executions")
    public List<Map<String, Object>> listExecutions(
            @RequestParam(required = false) String strategyId,
            @RequestParam(required = false) String status,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listExecutions(strategyId, status, limit);
    }

    @GetMapping("/executions/{executionId}")
    public Map<String, Object> getExecution(@PathVariable String executionId) {
        return service.getExecution(executionId);
    }

    @GetMapping("/executions/{executionId}/orders/open")
    public List<Map<String, Object>> getExecutionOpenOrders(@PathVariable String executionId) {
        return service.getExecutionOpenOrders(executionId);
    }

    @GetMapping("/executions/{executionId}/activities")
    public List<Map<String, Object>> getExecutionActivities(
            @PathVariable String executionId,
            @RequestParam(defaultValue = "50") int limit) {
        return service.getExecutionActivities(executionId, limit);
    }

    @GetMapping("/executions/{executionId}/lifecycles")
    public List<Map<String, Object>> getExecutionLifecycles(@PathVariable String executionId) {
        return service.getExecutionLifecycles(executionId);
    }

    @GetMapping("/executions/{executionId}/logs")
    public List<Map<String, Object>> getExecutionLogs(
            @PathVariable String executionId,
            @RequestParam(defaultValue = "100") int limit) {
        return service.getExecutionLogs(executionId, limit);
    }
}
