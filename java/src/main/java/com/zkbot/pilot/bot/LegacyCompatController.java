package com.zkbot.pilot.bot;

import com.zkbot.pilot.bot.dto.*;
import org.springframework.http.HttpStatus;
import org.springframework.web.bind.annotation.*;

import java.util.Map;

/**
 * Backward-compatible aliases matching the Python scaffold endpoints.
 * Forwards to {@link BotService} for actual logic.
 */
@RestController
public class LegacyCompatController {

    private final BotService service;

    public LegacyCompatController(BotService service) {
        this.service = service;
    }

    @PostMapping("/v1/strategy-executions/start")
    @ResponseStatus(HttpStatus.CREATED)
    public StartExecutionResponse startExecution(@RequestBody LegacyStartRequest body) {
        var request = new StartExecutionRequest(
                body.strategy_id(), null, body.runtime_params(), false);
        return service.startExecution(request);
    }

    @PostMapping("/v1/strategy-executions/stop")
    public ExecutionResponse stopExecution(@RequestBody LegacyStopRequest body) {
        return service.stopExecution(body.execution_id(),
                body.stop_reason() != null ? body.stop_reason() : "graceful");
    }

    @GetMapping("/v1/strategies/{id}")
    public StrategyResponse getStrategy(@PathVariable String id) {
        return service.getStrategy(id);
    }

    // Legacy DTOs using snake_case to match Python API
    record LegacyStartRequest(String strategy_id, int instance_id, Map<String, Object> runtime_params) {
        LegacyStartRequest {
            if (runtime_params == null) runtime_params = Map.of();
        }
    }

    record LegacyStopRequest(String execution_id, String stop_reason) {}
}
