package com.zkbot.pilot.topology;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.networknt.schema.JsonSchema;
import com.networknt.schema.JsonSchemaFactory;
import com.networknt.schema.SpecVersion;
import com.networknt.schema.ValidationMessage;
import com.zkbot.pilot.bootstrap.TokenService;
import com.zkbot.pilot.config.ConfigDriftService;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.discovery.DiscoveryCache;
import com.zkbot.pilot.ops.dto.RegistrationAuditEntry;
import com.zkbot.pilot.schema.ConfigSchemaLocator;
import com.zkbot.pilot.schema.SchemaService;
import com.zkbot.pilot.schema.dto.SchemaResourceEntry;
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

    private static final Map<String, String> KIND_TO_CAPABILITY = Map.of(
            "GW", "gw",
            "MDGW", "rtmd",
            "REFDATA", "refdata"
    );

    private final TopologyRepository repository;
    private final DiscoveryCache discoveryCache;
    private final TokenService tokenService;
    private final PilotProperties props;
    private final Connection natsConnection;
    private final ConfigDriftService configDriftService;
    private final SchemaService schemaService;
    private final ConfigSchemaLocator configSchemaLocator;
    private final ObjectMapper objectMapper;

    public TopologyService(TopologyRepository repository,
                           DiscoveryCache discoveryCache,
                           TokenService tokenService,
                           PilotProperties props,
                           Connection natsConnection,
                           ConfigDriftService configDriftService,
                           SchemaService schemaService,
                           ConfigSchemaLocator configSchemaLocator,
                           ObjectMapper objectMapper) {
        this.repository = repository;
        this.discoveryCache = discoveryCache;
        this.tokenService = tokenService;
        this.props = props;
        this.natsConnection = natsConnection;
        this.configDriftService = configDriftService;
        this.schemaService = schemaService;
        this.configSchemaLocator = configSchemaLocator;
        this.objectMapper = objectMapper;
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

        // Load provided_config and venue from kind-specific table
        Map<String, Object> providedConfig = null;
        String venue = null;
        switch (storedType.toUpperCase()) {
            case "OMS" -> {
                var omsRow = repository.getOmsInstance(logicalId);
                if (omsRow != null) {
                    providedConfig = parseJsonField(omsRow.get("provided_config"));
                }
            }
            case "GW" -> {
                var gwRow = repository.getGatewayInstance(logicalId);
                if (gwRow != null) {
                    venue = (String) gwRow.get("venue");
                    providedConfig = parseJsonField(gwRow.get("provided_config"));
                }
            }
            case "MDGW" -> {
                var mdgwRow = repository.getMdgwInstance(logicalId);
                if (mdgwRow != null) {
                    venue = (String) mdgwRow.get("venue");
                    providedConfig = parseJsonField(mdgwRow.get("provided_config"));
                }
            }
            case "ENGINE" -> {
                var engRow = repository.getEngineInstance(logicalId);
                if (engRow != null) {
                    providedConfig = parseJsonField(engRow.get("provided_config"));
                }
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
                sessions,
                providedConfig,
                venue
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

    public TopologyActionResponse updateService(String serviceKind, String logicalId,
                                                    UpdateServiceRequest request) {
        String normalizedKind = serviceKind.toUpperCase();
        if ("REFDATA".equals(normalizedKind)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "Use PUT /v1/topology/refdata-venues/{logicalId}/config for REFDATA instances.");
        }

        var instance = repository.getLogicalInstance(logicalId);
        if (instance == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + logicalId);
        }

        String storedType = (String) instance.get("instance_type");
        if (!normalizedKind.equalsIgnoreCase(storedType)) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Service not found: " + serviceKind + "/" + logicalId);
        }

        // Block editing when online
        var liveState = discoveryCache.getAll();
        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
        if (reg != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Service is online. Stop the service before editing configuration.");
        }

        // Update enabled flag if provided
        if (request.enabled() != null) {
            repository.updateLogicalInstanceEnabled(logicalId, request.enabled());
        }

        // Update provided_config if provided
        if (request.providedConfig() != null) {
            // Validate config against active schema before persisting
            validateConfigAgainstSchema(logicalId, normalizedKind, request.providedConfig());

            String configJson = toJson(request.providedConfig());
            var hostProv = resolveSchemaProvenance("service_kind", normalizedKind.toLowerCase());

            switch (normalizedKind) {
                case "OMS" -> repository.updateOmsConfig(logicalId, configJson,
                        prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv));
                case "GW" -> {
                    var gwRow = repository.getGatewayInstance(logicalId);
                    String venue = gwRow != null ? (String) gwRow.get("venue") : "unknown";
                    var venueProv = resolveSchemaProvenance("venue_capability",
                            venue + "/" + KIND_TO_CAPABILITY.get("GW"));
                    repository.updateGatewayConfig(logicalId, configJson,
                            prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv),
                            prk(venueProv), pv(venueProv), ph(venueProv));
                }
                case "MDGW" -> {
                    var mdgwRow = repository.getMdgwInstance(logicalId);
                    String venue = mdgwRow != null ? (String) mdgwRow.get("venue") : "unknown";
                    var venueProv = resolveSchemaProvenance("venue_capability",
                            venue + "/" + KIND_TO_CAPABILITY.get("MDGW"));
                    repository.updateMdgwConfig(logicalId, configJson,
                            prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv),
                            prk(venueProv), pv(venueProv), ph(venueProv));
                }
                case "ENGINE" -> repository.updateEngineConfig(logicalId, configJson,
                        prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv));
                default -> throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                        "Unknown service kind for config update: " + normalizedKind);
            }
        }

        log.info("Updated service config: {}/{}", serviceKind, logicalId);
        return new TopologyActionResponse("updated", logicalId,
                "Service config updated. Restart/bootstrap required for changes to take effect.");
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

    private static final Set<String> KNOWN_SERVICE_KINDS = Set.of("OMS", "GW", "MDGW", "ENGINE");
    private static final Set<String> INLINE_VENUES = Set.of("simulator");

    public CreateServiceResponse createService(String serviceKind, CreateServiceRequest request) {
        String normalizedKind = serviceKind.toUpperCase();

        if ("REFDATA".equals(normalizedKind)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "Use POST /v1/topology/refdata-venues for REFDATA instances");
        }

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

        // Logical instance row: identity only, no config
        repository.createLogicalInstance(logicalId, normalizedKind, props.env(),
                request.metadata(), request.enabled());

        Map<String, Object> normalizedConfig = normalizeProvidedConfig(normalizedKind, request);
        String providedConfigJson = toJson(normalizedConfig);

        // Resolve schema provenance from active schema_resource
        var hostProv = resolveSchemaProvenance("service_kind", normalizedKind.toLowerCase());

        switch (normalizedKind) {
            case "OMS" -> repository.createOmsInstance(logicalId, providedConfigJson,
                    prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv));
            case "GW" -> {
                String venue = resolveVenue(request);
                var venueProv = resolveSchemaProvenance("venue_capability",
                        venue + "/" + KIND_TO_CAPABILITY.get("GW"));
                repository.createGatewayInstance(logicalId, venue, providedConfigJson,
                        prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv),
                        prk(venueProv), pv(venueProv), ph(venueProv));
            }
            case "MDGW" -> {
                String venue = resolveVenue(request);
                var venueProv = resolveSchemaProvenance("venue_capability",
                        venue + "/" + KIND_TO_CAPABILITY.get("MDGW"));
                repository.createMdgwInstance(logicalId, venue, providedConfigJson,
                        prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv),
                        prk(venueProv), pv(venueProv), ph(venueProv));
            }
            case "ENGINE" -> repository.createEngineInstance(logicalId, providedConfigJson,
                    prt(hostProv), prk(hostProv), pv(hostProv), ph(hostProv));
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

    // --- Refdata venue instances ---

    public RefdataVenueInstanceEntry createRefdataVenueInstance(CreateRefdataVenueRequest request) {
        if (request.logicalId() == null || request.logicalId().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "logicalId is required");
        }
        if (request.venue() == null || request.venue().isBlank()) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST, "venue is required");
        }

        // Check UNIQUE (env, venue) constraint before insert
        String existingOwner = repository.findRefdataVenueOwner(props.env(), request.venue());
        if (existingOwner != null && !existingOwner.equals(request.logicalId())) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Venue '" + request.venue() + "' in env '" + props.env()
                            + "' is already owned by logical_id '" + existingOwner + "'");
        }

        String providedConfigJson = toJson(request.providedConfig());
        String capability = KIND_TO_CAPABILITY.getOrDefault("REFDATA", "refdata");
        var prov = resolveSchemaProvenance("venue_capability",
                request.venue() + "/" + capability);

        // REFDATA topology model: one logical_instance per env (refdata_<env>_1), serving N venues.
        // Venue rows live only in cfg.refdata_venue_instance — no per-venue logical_instance row.
        boolean enabled = request.enabled() != null ? request.enabled() : true;

        repository.createRefdataVenueInstance(request.logicalId(), props.env(), request.venue(),
                request.description(), enabled,
                providedConfigJson, prk(prov), pv(prov), ph(prov));

        var row = repository.getRefdataVenueInstance(request.logicalId());
        return toRefdataVenueEntry(row);
    }

    public List<RefdataVenueInstanceEntry> listRefdataVenueInstances() {
        return repository.listRefdataVenueInstances().stream()
                .map(TopologyService::toRefdataVenueEntry)
                .toList();
    }

    public RefdataVenueInstanceEntry getRefdataVenueInstance(String logicalId) {
        var row = repository.getRefdataVenueInstance(logicalId);
        if (row == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Refdata venue instance not found: " + logicalId);
        }
        return toRefdataVenueEntry(row);
    }

    public RefdataVenueInstanceEntry updateRefdataVenueConfig(String logicalId,
                                                               Map<String, Object> providedConfig) {
        var existing = repository.getRefdataVenueInstance(logicalId);
        if (existing == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "Refdata venue instance not found: " + logicalId);
        }

        // Block editing when online
        var liveState = discoveryCache.getAll();
        ServiceRegistration reg = findLiveRegistration(logicalId, liveState);
        if (reg != null) {
            throw new ResponseStatusException(HttpStatus.CONFLICT,
                    "Service is online. Stop the service before editing configuration.");
        }

        // Validate config against active schema
        validateConfigAgainstSchema(logicalId, "REFDATA", providedConfig);

        String venue = (String) existing.get("venue");
        String capability = KIND_TO_CAPABILITY.getOrDefault("REFDATA", "refdata");
        var prov = resolveSchemaProvenance("venue_capability", venue + "/" + capability);

        String configJson = toJson(providedConfig);
        repository.updateRefdataVenueConfig(logicalId, configJson,
                prk(prov), pv(prov), ph(prov));

        var updated = repository.getRefdataVenueInstance(logicalId);
        return toRefdataVenueEntry(updated);
    }

    // --- Schema provenance resolution ---

    private SchemaResourceEntry resolveSchemaProvenance(String resourceType, String resourceKey) {
        if (resourceType == null || resourceKey == null) return null;
        try {
            if ("service_kind".equals(resourceType)) {
                return schemaService.getActiveServiceKindManifest(resourceKey);
            } else if ("venue_capability".equals(resourceType)) {
                String[] parts = resourceKey.split("/", 2);
                if (parts.length == 2) {
                    return schemaService.getActiveVenueCapabilityManifest(parts[0], parts[1]);
                }
            }
        } catch (Exception e) {
            log.debug("Could not resolve schema provenance for {}/{}: {}", resourceType, resourceKey, e.getMessage());
        }
        return null;
    }

    private static String prt(SchemaResourceEntry e) { return e != null ? e.resourceType() : null; }
    private static String prk(SchemaResourceEntry e) { return e != null ? e.resourceKey() : null; }
    private static Integer pv(SchemaResourceEntry e) { return e != null ? e.version() : null; }
    private static String ph(SchemaResourceEntry e) { return e != null ? e.contentHash() : null; }

    private void validateConfigAgainstSchema(String logicalId, String instanceType,
                                              Map<String, Object> providedConfig) {
        String schemaJson = configSchemaLocator.resolveConfigSchema(logicalId, instanceType);
        if (schemaJson == null) return;

        try {
            JsonSchema schema = JsonSchemaFactory.getInstance(SpecVersion.VersionFlag.V7)
                    .getSchema(schemaJson);
            JsonNode configNode = objectMapper.valueToTree(providedConfig);
            Set<ValidationMessage> errors = schema.validate(configNode);
            if (!errors.isEmpty()) {
                String msg = errors.stream().map(ValidationMessage::getMessage)
                        .collect(Collectors.joining("; "));
                throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                        "Config validation failed: " + msg);
            }
        } catch (ResponseStatusException e) {
            throw e;
        } catch (Exception e) {
            log.warn("Schema validation error for {}/{}: {}", instanceType, logicalId, e.getMessage());
        }
    }

    private String resolveVenue(CreateServiceRequest request) {
        if (request.venue() != null && !request.venue().isBlank()) {
            return request.venue();
        }
        return extractVenue(request.providedConfig());
    }

    private Map<String, Object> normalizeProvidedConfig(String serviceKind, CreateServiceRequest request) {
        Map<String, Object> config = request.providedConfig() != null
                ? new LinkedHashMap<>(request.providedConfig())
                : new LinkedHashMap<>();

        if (!KIND_TO_CAPABILITY.containsKey(serviceKind)) {
            return config;
        }

        String venue = resolveVenue(request);
        if (venue != null && !"unknown".equalsIgnoreCase(venue)) {
            config.put("venue", venue);
        }

        if (!INLINE_VENUES.contains(venue == null ? "" : venue.toLowerCase())) {
            Object venueConfig = config.get("venue_config");
            if (!(venueConfig instanceof Map<?, ?>)) {
                config.put("venue_config", new LinkedHashMap<String, Object>());
            }
        }

        return config;
    }

    private static String extractVenue(Map<String, Object> config) {
        if (config != null && config.containsKey("venue")) {
            return config.get("venue").toString();
        }
        return "unknown";
    }

    @SuppressWarnings("unchecked")
    private static RefdataVenueInstanceEntry toRefdataVenueEntry(Map<String, Object> row) {
        return new RefdataVenueInstanceEntry(
                (String) row.get("logical_id"),
                (String) row.get("env"),
                (String) row.get("venue"),
                (String) row.get("description"),
                Boolean.TRUE.equals(row.get("enabled")),
                row.get("provided_config") != null ? row.get("provided_config").toString() : "{}",
                row.get("config_version") != null ? ((Number) row.get("config_version")).intValue() : 1,
                (String) row.get("config_hash"),
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at"))
        );
    }

    @SuppressWarnings("unchecked")
    private Map<String, Object> parseJsonField(Object field) {
        if (field == null) return Map.of();
        if (field instanceof Map) return (Map<String, Object>) field;
        try {
            return objectMapper.readValue(field.toString(), Map.class);
        } catch (Exception e) {
            log.debug("Failed to parse JSON config field: {}", e.getMessage());
            return Map.of();
        }
    }

    private String toJson(Map<String, Object> map) {
        if (map == null || map.isEmpty()) return "{}";
        try {
            return objectMapper.writeValueAsString(map);
        } catch (JsonProcessingException e) {
            return "{}";
        }
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
