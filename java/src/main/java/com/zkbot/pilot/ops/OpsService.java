package com.zkbot.pilot.ops;

import com.zkbot.pilot.topology.TopologyRepository;
import org.springframework.stereotype.Service;

import java.util.List;
import java.util.Map;
import java.util.UUID;

@Service
public class OpsService {

    private final TopologyRepository repository;

    public OpsService(TopologyRepository repository) {
        this.repository = repository;
    }

    public List<Map<String, Object>> listRegistrationAudit(String logicalId, int limit) {
        return repository.listRegistrationAudit(logicalId, limit);
    }

    public List<Map<String, Object>> listReconciliationAudit(int limit) {
        return repository.listReconciliationAudit(limit);
    }

    public Map<String, Object> triggerReconcile(Map<String, Object> request) {
        String jobId = UUID.randomUUID().toString();
        String scope = request != null ? String.valueOf(request.getOrDefault("scope", "full")) : "full";
        return Map.of(
                "status", "accepted",
                "jobId", jobId,
                "scope", scope
        );
    }
}
