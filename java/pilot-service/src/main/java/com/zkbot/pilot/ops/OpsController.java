package com.zkbot.pilot.ops;

import com.zkbot.pilot.ops.dto.ReconRunEntry;
import com.zkbot.pilot.ops.dto.ReconcileResponse;
import com.zkbot.pilot.ops.dto.RegistrationAuditEntry;
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
    public List<RegistrationAuditEntry> registrationAudit(
            @RequestParam(required = false) String logical_id,
            @RequestParam(defaultValue = "50") int limit) {
        return service.listRegistrationAudit(logical_id, limit);
    }

    @GetMapping("/audit/reconciliation")
    public List<ReconRunEntry> reconciliationAudit(
            @RequestParam(defaultValue = "50") int limit) {
        return service.listReconciliationAudit(limit);
    }

    @PostMapping("/reconcile")
    public ReconcileResponse triggerReconcile(@RequestBody Map<String, Object> request) {
        return service.triggerReconcile(request);
    }
}
