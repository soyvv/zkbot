package com.zkbot.pilot.topology;

import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.topology.dto.*;
import io.nats.client.Connection;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;
import zk.discovery.v1.Discovery.ServiceRegistration;

import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.*;
import java.util.stream.Collectors;

@Service
public class TopologyService {

    private static final Logger log = LoggerFactory.getLogger(TopologyService.class);
    private static final List<String> VIEW_NAMES = List.of("full", "services", "connections");

    // Service families that form the OMS workspace scope
    private static final Set<String> OMS_WORKSPACE_TYPES = Set.of("OMS", "GW", "ENGINE", "REFDATA");
    private static final String TYPE_MDGW = "MDGW";

    private final TopologyRepository repository;
    private final DiscoveryCache discoveryCache;
    private final TokenService tokenService;
    private final PilotProperties props;
    private final Connection natsConnection;

    public TopologyService(TopologyRepository repository,
                           DiscoveryCache discoveryCache,
                           TokenService tokenService,
                           PilotProperties props,
                           Connection natsConnection) {
        this.repository = repository;
        this.discoveryCache = discoveryCache;
        this.tokenService = tokenService;
        this.props = props;
        this.natsConnection = natsConnection;
    }

    public TopologyView getTopology(String omsId) {
        var instances = repository.listLogicalInstances(props.env(), null);
        var bindings = repository.listBindings(null, null);
        var liveState = discoveryCache.getAll();

        Set<String> scopedIds;
        if (omsId != null && !omsId.isBlank()) {
            scopedIds = buildOmsWorkspaceScope(omsId, instances, bindings);
        } else {
            scopedIds = instances.stream()
                    .map(i -> (String) i.get("logical_id"))
                    .collect(Collectors.toSet());
        }

        List<ServiceNode> nodes = instances.stream()
                .filter(i -> scopedIds.contains(i.get("logical_id")))
                .map(i -> toServiceNode(i, liveState))
                .toList();

        // Only emit edges where both endpoints are in the scoped set
        List<Edge> edges = bindings.stream()
                .filter(b -> scopedIds.contains(b.get("src_id")) && scopedIds.contains(b.get("dst_id")))
                .map(this::toEdge)
                .toList();

        var scope = new TopologyView.Scope(omsId, Instant.now());
        return new TopologyView(scope, nodes, edges, List.of());
    }

    public List<String> listViews() {
        return VIEW_NAMES;
    }

    public TopologyView getView(String viewName, String omsId) {
        if (!VIEW_NAMES.contains(viewName)) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Unknown view: " + viewName + ". Available: " + VIEW_NAMES);
        }
        return getTopology(omsId);
    }

    public List<ServiceNode> listServices(String omsId, String serviceKind) {
        // Normalize to uppercase to match DB convention (OMS, GW, ENGINE, etc.)
        String normalizedKind = serviceKind != null ? serviceKind.toUpperCase() : null;
        var instances = repository.listLogicalInstances(props.env(), normalizedKind);
        var liveState = discoveryCache.getAll();

        if (omsId != null && !omsId.isBlank()) {
            var allInstances = repository.listLogicalInstances(props.env(), null);
            var bindings = repository.listBindings(null, null);
            Set<String> scopedIds = buildOmsWorkspaceScope(omsId, allInstances, bindings);
            instances = instances.stream()
                    .filter(i -> scopedIds.contains(i.get("logical_id")))
                    .toList();
        }

        return instances.stream()
                .map(i -> toServiceNode(i, liveState))
                .toList();
    }

    public ServiceDetail getServiceDetail(String serviceKind, String logicalId) {
        var instance = repository.getLogicalInstance(logicalId);
        if (instance == null) {
            return null;
        }

        // Validate service kind matches stored instance_type
        String storedType = (String) instance.get("instance_type");
        if (!serviceKind.equalsIgnoreCase(storedType)) {
            return null; // controller will map to 404
        }

        boolean desiredEnabled = Boolean.TRUE.equals(instance.get("enabled"));

        var liveState = discoveryCache.getAll();
        var bindings = repository.getBindingsForService(logicalId);
        var sessions = repository.listActiveSessions().stream()
                .filter(s -> logicalId.equals(s.get("logical_id")))
                .toList();

        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
        String liveStatus = reg != null ? "online" : "offline";
        String endpoint = reg != null && reg.hasEndpoint() ? reg.getEndpoint().getAddress() : null;
        String version = reg != null ? reg.getAttrsOrDefault("version", null) : null;
        Instant lastSeenAt = reg != null ? Instant.ofEpochMilli(reg.getUpdatedAtMs()) : null;

        return new ServiceDetail(
                logicalId,
                storedType,
                storedType,
                desiredEnabled,
                liveStatus,
                endpoint,
                version,
                lastSeenAt,
                null,
                bindings,
                sessions
        );
    }

    public List<Map<String, Object>> getServiceBindings(String logicalId) {
        return repository.getBindingsForService(logicalId);
    }

    public List<Map<String, Object>> getServiceAudit(String logicalId, int limit) {
        return repository.listRegistrationAudit(logicalId, limit);
    }

    public Map<String, Object> issueBootstrapToken(String serviceKind, String logicalId) {
        return tokenService.generateToken(logicalId, serviceKind, props.env(), 30);
    }

    public Map<String, String> reloadService(String serviceKind, String logicalId) {
        // Validate service exists
        var instance = repository.getLogicalInstance(logicalId);
        if (instance == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + logicalId);
        }

        // Validate service kind matches stored instance_type
        String storedType = (String) instance.get("instance_type");
        if (!serviceKind.equalsIgnoreCase(storedType)) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + serviceKind + "/" + logicalId);
        }

        // Validate service is online
        var liveState = discoveryCache.getAll();
        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
        if (reg == null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Service is offline, cannot reload: " + logicalId);
        }

        // Use stored type for NATS subject to ensure correct casing
        String subject = "zk.control." + storedType + "." + logicalId + ".reload";
        natsConnection.publish(subject, "reload".getBytes(StandardCharsets.UTF_8));
        log.info("Published reload command to subject={}", subject);
        return Map.of(
                "status", "dispatched",
                "subject", subject,
                "note", "Reload dispatched to live runtime; completion is not guaranteed");
    }

    public List<Map<String, Object>> listSessions(String omsId) {
        var sessions = repository.listActiveSessions();
        var liveState = discoveryCache.getAll();

        // If OMS-scoped, derive the scoped logical service set first
        Set<String> scopedLogicalIds;
        if (omsId != null && !omsId.isBlank()) {
            var instances = repository.listLogicalInstances(props.env(), null);
            var bindings = repository.listBindings(null, null);
            scopedLogicalIds = buildOmsWorkspaceScope(omsId, instances, bindings);
        } else {
            scopedLogicalIds = null; // no filter
        }

        return sessions.stream()
                .map(s -> {
                    Map<String, Object> enriched = new LinkedHashMap<>(s);
                    String logicalId = (String) s.get("logical_id");
                    ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
                    enriched.put("kv_live", reg != null);
                    return enriched;
                })
                .filter(s -> {
                    if (scopedLogicalIds == null) return true;
                    return scopedLogicalIds.contains(s.get("logical_id"));
                })
                .toList();
    }

    public void upsertBinding(String srcType, String srcId, String dstType, String dstId,
                               boolean enabled, Map<String, Object> metadata) {
        repository.upsertBinding(srcType, srcId, dstType, dstId, enabled, metadata);
    }

    // --- OMS workspace scope builder ---

    /**
     * Build the OMS-scoped workspace: the selected OMS + bound GWs + bound bots +
     * refdata services + MDGW only if subscribed/used by scoped services.
     */
    private Set<String> buildOmsWorkspaceScope(String omsId,
                                                List<Map<String, Object>> allInstances,
                                                List<Map<String, Object>> allBindings) {
        Set<String> scopedIds = new LinkedHashSet<>();

        // Always include the selected OMS
        scopedIds.add(omsId);

        // Build a lookup: logicalId -> instanceType
        Map<String, String> typeByLogicalId = new HashMap<>();
        for (var inst : allInstances) {
            typeByLogicalId.put((String) inst.get("logical_id"), (String) inst.get("instance_type"));
        }

        // Collect direct bindings of the OMS
        Set<String> directNeighbors = new HashSet<>();
        for (var binding : allBindings) {
            String srcId = (String) binding.get("src_id");
            String dstId = (String) binding.get("dst_id");
            if (omsId.equals(srcId)) directNeighbors.add(dstId);
            if (omsId.equals(dstId)) directNeighbors.add(srcId);
        }

        // Include bound GWs, ENGINEs (bots), and REFDATA from direct neighbors
        for (String neighborId : directNeighbors) {
            String type = typeByLogicalId.get(neighborId);
            if (type != null && OMS_WORKSPACE_TYPES.contains(type)) {
                scopedIds.add(neighborId);
            }
        }

        // Also include all REFDATA services in this env (always part of the workspace)
        for (var inst : allInstances) {
            if ("REFDATA".equalsIgnoreCase((String) inst.get("instance_type"))) {
                scopedIds.add((String) inst.get("logical_id"));
            }
        }

        // Include MDGW only if it is bound to any already-scoped service.
        // NOTE: This uses binding topology as a proxy for "actively subscribed/used".
        // True live subscription state (which GWs are actively consuming RTMD feeds)
        // is not queryable from pilot today. When MDGW publishes subscription interest
        // to the KV registry, this can be tightened to check live state instead.
        for (var binding : allBindings) {
            String srcId = (String) binding.get("src_id");
            String dstId = (String) binding.get("dst_id");
            String srcType = typeByLogicalId.get(srcId);
            String dstType = typeByLogicalId.get(dstId);

            if (TYPE_MDGW.equalsIgnoreCase(srcType) && scopedIds.contains(dstId)) {
                scopedIds.add(srcId);
            }
            if (TYPE_MDGW.equalsIgnoreCase(dstType) && scopedIds.contains(srcId)) {
                scopedIds.add(dstId);
            }
        }

        return scopedIds;
    }

    // --- private helpers ---

    private ServiceNode toServiceNode(Map<String, Object> instance,
                                       Map<String, ServiceRegistration> liveState) {
        String logicalId = (String) instance.get("logical_id");
        String instanceType = (String) instance.get("instance_type");
        boolean desiredEnabled = Boolean.TRUE.equals(instance.get("enabled"));
        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);

        String status = reg != null ? "online" : "offline";
        String endpoint = reg != null && reg.hasEndpoint() ? reg.getEndpoint().getAddress() : null;
        String version = reg != null ? reg.getAttrsOrDefault("version", null) : null;
        String sessionId = reg != null ? reg.getInstanceId() : null;

        return new ServiceNode(logicalId, instanceType, instanceType, status,
                endpoint, version, null, sessionId, desiredEnabled);
    }

    private Edge toEdge(Map<String, Object> binding) {
        String srcId = (String) binding.get("src_id");
        String dstId = (String) binding.get("dst_id");
        String srcType = (String) binding.get("src_type");
        Boolean enabled = (Boolean) binding.get("enabled");
        String status = Boolean.TRUE.equals(enabled) ? "active" : "disabled";
        return new Edge(srcType + "->" + binding.get("dst_type"), srcId, dstId, status);
    }

    private ServiceRegistration findLiveRegistration(String logicalId,
                                                      Map<String, ServiceRegistration> liveState) {
        for (var entry : liveState.entrySet()) {
            if (entry.getKey().endsWith("." + logicalId)) {
                return entry.getValue();
            }
            if (logicalId.equals(entry.getValue().getServiceId())) {
                return entry.getValue();
            }
        }
        return null;
    }
}
