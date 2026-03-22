package com.zkbot.pilot.ops;

import org.springframework.web.bind.annotation.*;

import java.util.List;
import java.util.Map;

@RestController
@RequestMapping("/v1/ops")
public class OpsController {

    private final OpsService service;

    public OpsController(OpsService service) {
        this.service = service;
    }

    @GetMapping("/audit/registrations")
    public List<Map<String, Object>> registrationAudit(
            @RequestParam(required = false) String logical_id,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listRegistrationAudit(logical_id, limit);
    }

    @GetMapping("/audit/reconciliation")
    public List<Map<String, Object>> reconciliationAudit(
            @RequestParam(defaultValue = "50") int limit) {
        return service.listReconciliationAudit(limit);
    }

    @PostMapping("/reconcile")
    public Map<String, Object> triggerReconcile(@RequestBody Map<String, Object> request) {
        return service.triggerReconcile(request);
    }
}
