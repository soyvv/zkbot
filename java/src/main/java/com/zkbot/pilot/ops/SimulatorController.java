package com.zkbot.pilot.ops;

import com.zkbot.pilot.ops.dto.sim.*;
import org.springframework.web.bind.annotation.*;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/v1/ops/simulators")
public class SimulatorController {

    private final SimulatorService service;

    public SimulatorController(SimulatorService service) {
        this.service = service;
    }

    @GetMapping
    public List<SimulatorSummary> listSimulators() {
        return service.listSimulators();
    }

    @GetMapping("/{gwId}")
    public SimulatorStateResponse getSimulatorState(@PathVariable String gwId) {
        return service.getSimulatorState(gwId);
    }

    @PostMapping("/{gwId}/force-match")
    public SimActionResponse forceMatch(@PathVariable String gwId, @RequestBody ForceMatchRequest request) {
        return service.forceMatch(gwId, request);
    }

    @PostMapping("/{gwId}/inject-error")
    public SimActionResponse injectError(@PathVariable String gwId, @RequestBody InjectErrorRequest request) {
        return service.injectError(gwId, request);
    }

    @GetMapping("/{gwId}/injected-errors")
    public List<InjectedErrorEntry> listInjectedErrors(@PathVariable String gwId) {
        return service.listInjectedErrors(gwId);
    }

    @DeleteMapping("/{gwId}/injected-errors")
    public SimActionResponse clearAllInjectedErrors(@PathVariable String gwId) {
        return service.clearInjectedError(gwId, "");
    }

    @DeleteMapping("/{gwId}/injected-errors/{errorId}")
    public SimActionResponse clearInjectedError(@PathVariable String gwId, @PathVariable String errorId) {
        return service.clearInjectedError(gwId, errorId);
    }

    @PostMapping("/{gwId}/pause-matching")
    public SimActionResponse pauseMatching(@PathVariable String gwId, @RequestBody(required = false) Map<String, String> body) {
        String reason = body != null ? body.get("reason") : null;
        return service.pauseMatching(gwId, reason);
    }

    @PostMapping("/{gwId}/resume-matching")
    public SimActionResponse resumeMatching(@PathVariable String gwId, @RequestBody(required = false) Map<String, String> body) {
        String reason = body != null ? body.get("reason") : null;
        return service.resumeMatching(gwId, reason);
    }

    @PostMapping("/{gwId}/set-match-policy")
    public SimActionResponse setMatchPolicy(@PathVariable String gwId, @RequestBody SetMatchPolicyRequest request) {
        return service.setMatchPolicy(gwId, request);
    }

    @PostMapping("/{gwId}/account-state")
    public SimActionResponse setAccountState(@PathVariable String gwId, @RequestBody SetAccountStateRequest request) {
        return service.setAccountState(gwId, request);
    }

    @PostMapping("/{gwId}/synthetic-ticks")
    public SimActionResponse submitSyntheticTick(@PathVariable String gwId, @RequestBody SubmitSyntheticTickRequest request) {
        return service.submitSyntheticTick(gwId, request);
    }

    @PostMapping("/{gwId}/reset")
    public SimActionResponse resetSimulator(@PathVariable String gwId, @RequestBody(required = false) Map<String, String> body) {
        String reason = body != null ? body.get("reason") : null;
        return service.resetSimulator(gwId, reason);
    }

    @GetMapping("/{gwId}/open-orders")
    public List<SimOpenOrderEntry> listOpenOrders(@PathVariable String gwId) {
        return service.listOpenOrders(gwId);
    }
}
