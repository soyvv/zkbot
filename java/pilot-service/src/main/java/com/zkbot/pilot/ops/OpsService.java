package com.zkbot.pilot.ops;

import com.zkbot.pilot.ops.dto.ReconRunEntry;
import com.zkbot.pilot.ops.dto.ReconcileResponse;
import com.zkbot.pilot.ops.dto.RegistrationAuditEntry;
import com.zkbot.pilot.topology.TopologyRepository;
import org.springframework.stereotype.Service;

import java.time.Instant;
import java.util.List;
import java.util.Map;
import java.util.UUID;

@Service
public class OpsService {

    private final TopologyRepository repository;

    public OpsService(TopologyRepository repository) {
        this.repository = repository;
    }

    public List<RegistrationAuditEntry> listRegistrationAudit(String logicalId, int limit) {
        return repository.listRegistrationAudit(logicalId, limit).stream()
                .map(this::toAuditEntry)
                .toList();
    }

    public List<ReconRunEntry> listReconciliationAudit(int limit) {
        return repository.listReconciliationAudit(limit).stream()
                .map(OpsService::toReconRunEntry)
                .toList();
    }

    public ReconcileResponse triggerReconcile(Map<String, Object> request) {
        String jobId = UUID.randomUUID().toString();
        String scope = request != null ? String.valueOf(request.getOrDefault("scope", "full")) : "full";
        return new ReconcileResponse("accepted", jobId, scope);
    }

    private RegistrationAuditEntry toAuditEntry(Map<String, Object> row) {
        return new RegistrationAuditEntry(
                (String) row.get("token_jti"),
                (String) row.get("logical_id"),
                (String) row.get("instance_type"),
                (String) row.get("owner_session_id"),
                (String) row.get("decision"),
                (String) row.get("reason"),
                toInstant(row.get("observed_at"))
        );
    }

    private static ReconRunEntry toReconRunEntry(Map<String, Object> row) {
        Long accountId = row.get("account_id") != null ? ((Number) row.get("account_id")).longValue() : null;
        return new ReconRunEntry(
                ((Number) row.get("recon_run_id")).longValue(),
                (String) row.get("recon_type"),
                accountId,
                (String) row.get("oms_id"),
                toInstant(row.get("period_start")),
                toInstant(row.get("period_end")),
                ((Number) row.get("oms_count")).intValue(),
                ((Number) row.get("exch_count")).intValue(),
                ((Number) row.get("matched_count")).intValue(),
                ((Number) row.get("oms_only_count")).intValue(),
                ((Number) row.get("exch_only_count")).intValue(),
                ((Number) row.get("amended_count")).intValue(),
                (String) row.get("status"),
                toInstant(row.get("run_at"))
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
