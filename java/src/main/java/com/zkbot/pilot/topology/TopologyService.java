package com.zkbot.pilot.topology;

import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.ConfigDriftService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.ops.dto.RegistrationAuditEntry;
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

    private static final Set<String> OMS_WORKSPACE_TYPES = Set.of("OMS", "GW", "ENGINE", "REFDATA");
    private static final String TYPE_MDGW = "MDGW";

    private final TopologyRepository repository;
    private final DiscoveryCache discoveryCache;
    private final TokenService tokenService;
    private final PilotProperties props;
    private final Connection natsConnection;
    private final ConfigDriftService configDriftService;

    public TopologyService(TopologyRepository repository,
                           DiscoveryCache discoveryCache,
                           TokenService tokenService,
                           PilotProperties props,
                           Connection natsConnection,
                           ConfigDriftService configDriftService) {
        this.repository = repository;
        this.discoveryCache = discoveryCache;
        this.tokenService = tokenService;
        this.props = props;
        this.natsConnection = natsConnection;
        this.configDriftService = configDriftService;
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

        String storedType = (String) instance.get("instance_type");
        if (!serviceKind.equalsIgnoreCase(storedType)) {
            return null;
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

        String configDrift = null;
        if ("online".equals(liveStatus)) {
            var drift = configDriftService.computeDrift(logicalId, storedType);
            if (drift != null) {
                configDrift = drift.overallStatus();
            }
        }

        return new ServiceDetail(
                logicalId,
                storedType,
                storedType,
                desiredEnabled,
                liveStatus,
                endpoint,
                version,
                lastSeenAt,
                configDrift,
                bindings,
                sessions
        );
    }

    public List<BindingEntry> getServiceBindings(String logicalId) {
        return repository.getBindingsForService(logicalId).stream()
                .map(TopologyService::toBindingEntry)
                .toList();
    }

    public List<RegistrationAuditEntry> getServiceAudit(String logicalId, int limit) {
        return repository.listRegistrationAudit(logicalId, limit).stream()
                .map(TopologyService::toAuditEntry)
                .toList();
    }

    public BootstrapTokenResponse issueBootstrapToken(String serviceKind, String logicalId) {
        return tokenService.generateToken(logicalId, serviceKind, props.env(), 30);
    }

    public TopologyActionResponse reloadService(String serviceKind, String logicalId) {
        var instance = repository.getLogicalInstance(logicalId);
        if (instance == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + logicalId);
        }

        String storedType = (String) instance.get("instance_type");
        if (!serviceKind.equalsIgnoreCase(storedType)) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + serviceKind + "/" + logicalId);
        }

        var liveState = discoveryCache.getAll();
        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
        if (reg == null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Service is offline, cannot reload: " + logicalId);
        }

        String subject = "zk.control." + storedType + "." + logicalId + ".reload";
        natsConnection.publish(subject, "reload".getBytes(StandardCharsets.UTF_8));
        log.info("Published reload command to subject={}", subject);
        return new TopologyActionResponse(
                "dispatched",
                subject,
                "Reload dispatched to live runtime; completion is not guaranteed");
    }

    public List<SessionResponse> listSessions(String omsId) {
        var sessions = repository.listActiveSessions();
        var liveState = discoveryCache.getAll();

        Set<String> scopedLogicalIds;
        if (omsId != null && !omsId.isBlank()) {
            var instances = repository.listLogicalInstances(props.env(), null);
            var bindings = repository.listBindings(null, null);
            scopedLogicalIds = buildOmsWorkspaceScope(omsId, instances, bindings);
        } else {
            scopedLogicalIds = null;
        }

        return sessions.stream()
                .filter(s -> {
                    if (scopedLogicalIds == null) return true;
                    return scopedLogicalIds.contains(s.get("logical_id"));
                })
                .map(s -> {
                    String logicalId = (String) s.get("logical_id");
                    ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
                    return toSessionResponse(s, reg != null);
                })
                .toList();
    }

    private SessionResponse toSessionResponse(Map<String, Object> s, boolean kvLive) {
        return new SessionResponse(
                (String) s.get("owner_session_id"),
                (String) s.get("logical_id"),
                (String) s.get("instance_type"),
                (String) s.get("kv_key"),
                (String) s.get("lock_key"),
                (String) s.get("status"),
                toInstant(s.get("last_seen_at")),
                toInstant(s.get("expires_at")),
                s.get("lease_ttl_ms") != null ? ((Number) s.get("lease_ttl_ms")).longValue() : 0,
                kvLive
        );
    }

    private static Instant toInstant(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Instant i) return i;
        if (obj instanceof java.sql.Timestamp ts) return ts.toInstant();
        if (obj instanceof java.time.OffsetDateTime odt) return odt.toInstant();
        return null;
    }

    public void upsertBinding(String srcType, String srcId, String dstType, String dstId,
                               boolean enabled, Map<String, Object> metadata) {
        repository.upsertBinding(srcType, srcId, dstType, dstId, enabled, metadata);
    }

    // --- Service onboarding ---

    private static final Set<String> KNOWN_SERVICE_KINDS = Set.of("OMS", "GW", "MDGW", "ENGINE", "REFDATA");

    public CreateServiceResponse createService(String serviceKind, CreateServiceRequest request) {
        String normalizedKind = serviceKind.toUpperCase();
        if (!KNOWN_SERVICE_KINDS.contains(normalizedKind)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "Unknown service kind: " + serviceKind + ". Known: " + KNOWN_SERVICE_KINDS);
        }

        String logicalId = request.logicalId();
        if (logicalId == null || logicalId.isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "logicalId is required");
        }

        if (repository.getLogicalInstance(logicalId) != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Logical instance already exists: " + logicalId);
        }

        repository.createLogicalInstance(logicalId, normalizedKind, props.env(),
                request.metadata(), request.runtimeConfig(), request.enabled());

        switch (normalizedKind) {
            case "OMS" -> repository.createOmsInstance(logicalId);
            case "GW" -> {
                String venue = extractVenue(request.runtimeConfig());
                repository.createGatewayInstance(logicalId, venue);
            }
            case "MDGW" -> {
                String venue = extractVenue(request.runtimeConfig());
                repository.createMdgwInstance(logicalId, venue);
            }
            case "ENGINE" -> repository.createEngineInstance(logicalId);
        }

        if (request.bindings() != null) {
            for (var bindingSpec : request.bindings()) {
                repository.upsertBinding(normalizedKind, logicalId,
                        bindingSpec.dstType(), bindingSpec.dstId(),
                        bindingSpec.enabled(), Map.of());
            }
        }

        BootstrapTokenResponse token = tokenService.generateToken(
                logicalId, normalizedKind, props.env(), 30);

        return new CreateServiceResponse("created", logicalId, normalizedKind, token);
    }

    private static String extractVenue(Map<String, Object> runtimeConfig) {
        if (runtimeConfig != null && runtimeConfig.containsKey("venue")) {
            return runtimeConfig.get("venue").toString();
        }
        return "unknown";
    }

    // --- OMS workspace scope builder ---

    private Set<String> buildOmsWorkspaceScope(String omsId,
                                                List<Map<String, Object>> allInstances,
                                                List<Map<String, Object>> allBindings) {
        Set<String> scopedIds = new LinkedHashSet<>();
        scopedIds.add(omsId);

        Map<String, String> typeByLogicalId = new HashMap<>();
        for (var inst : allInstances) {
            typeByLogicalId.put((String) inst.get("logical_id"), (String) inst.get("instance_type"));
        }

        Set<String> directNeighbors = new HashSet<>();
        for (var binding : allBindings) {
            String srcId = (String) binding.get("src_id");
            String dstId = (String) binding.get("dst_id");
            if (omsId.equals(srcId)) directNeighbors.add(dstId);
            if (omsId.equals(dstId)) directNeighbors.add(srcId);
        }

        for (String neighborId : directNeighbors) {
            String type = typeByLogicalId.get(neighborId);
            if (type != null && OMS_WORKSPACE_TYPES.contains(type)) {
                scopedIds.add(neighborId);
            }
        }

        for (var inst : allInstances) {
            if ("REFDATA".equalsIgnoreCase((String) inst.get("instance_type"))) {
                scopedIds.add((String) inst.get("logical_id"));
            }
        }

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

    private static RegistrationAuditEntry toAuditEntry(Map<String, Object> row) {
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

    private static BindingEntry toBindingEntry(Map<String, Object> row) {
        long bindingId = row.get("binding_id") != null ? ((Number) row.get("binding_id")).longValue()
                : (row.get("audit_id") != null ? ((Number) row.get("audit_id")).longValue() : 0);
        return new BindingEntry(
                bindingId,
                (String) row.get("src_type"),
                (String) row.get("src_id"),
                (String) row.get("dst_type"),
                (String) row.get("dst_id"),
                Boolean.TRUE.equals(row.get("enabled")),
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at"))
        );
    }
}
