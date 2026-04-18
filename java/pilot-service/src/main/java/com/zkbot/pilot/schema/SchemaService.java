package com.zkbot.pilot.schema;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.zkbot.pilot.schema.dto.SchemaResourceEntry;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.HttpStatus;
import org.springframework.stereotype.Service;
import org.springframework.web.server.ResponseStatusException;

import java.time.Instant;
import java.util.Collections;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

@Service
public class SchemaService {

    private static final Logger log = LoggerFactory.getLogger(SchemaService.class);

    private final SchemaRepository repository;
    private final ObjectMapper objectMapper;

    private final Map<String, Map<String, Object>> activeCache = new ConcurrentHashMap<>();

    public SchemaService(SchemaRepository repository, ObjectMapper objectMapper) {
        this.repository = repository;
        this.objectMapper = objectMapper;
    }

    public SchemaResourceEntry getActiveServiceKindManifest(String serviceKind) {
        Map<String, Object> row = getActive("service_kind", serviceKind.toLowerCase());
        return row != null ? toSchemaResourceEntry(row) : null;
    }

    public SchemaResourceEntry getActiveVenueCapabilityManifest(String venueId, String capability) {
        Map<String, Object> row = getActive("venue_capability", venueId + "/" + capability);
        return row != null ? toSchemaResourceEntry(row) : null;
    }

    public SchemaResourceEntry getActiveStrategyManifest(String strategyTypeKey) {
        Map<String, Object> row = getActive("strategy_kind", strategyTypeKey);
        return row != null ? toSchemaResourceEntry(row) : null;
    }

    @SuppressWarnings("unchecked")
    public List<Map<String, Object>> getFieldDescriptors(String resourceType, String resourceKey) {
        Map<String, Object> manifest = getActive(resourceType, resourceKey);
        if (manifest == null) return Collections.emptyList();

        String manifestJson = (String) manifest.get("manifest_json");
        if (manifestJson == null) return Collections.emptyList();

        try {
            Map<String, Object> parsed = objectMapper.readValue(manifestJson,
                    new TypeReference<Map<String, Object>>() {});
            var descriptors = (List<Map<String, Object>>) parsed.get("field_descriptors");
            return descriptors != null ? descriptors : Collections.emptyList();
        } catch (Exception e) {
            log.warn("schema: failed to parse field_descriptors for {}/{}: {}", resourceType, resourceKey, e.getMessage());
            return Collections.emptyList();
        }
    }

    public String getConfigSchema(String resourceType, String resourceKey) {
        Map<String, Object> manifest = getActive(resourceType, resourceKey);
        if (manifest == null) return null;
        return (String) manifest.get("config_schema");
    }

    private static final Set<String> VENUE_BACKED_KINDS = Set.of("gw", "mdgw");

    private static final Map<String, String> KIND_TO_CAPABILITY = Map.of(
            "gw", "gw",
            "mdgw", "rtmd"
    );

    /**
     * Simulator venues embed their config fields at the top level of GwSvcConfig
     * (ergonomic default). Real venues parse from the opaque venue_config JSON blob.
     */
    private static final Set<String> INLINE_VENUES = Set.of("simulator");

    /**
     * Build a merged JSON Schema for a venue-backed service kind.
     *
     * For simulator: venue fields are merged at top level (matches GwSvcConfig layout).
     * For real venues: venue fields are placed under a "venue_config" property.
     *
     * Throws 404 if service-kind or venue-capability schema is missing.
     * Throws 400 if the service kind is not venue-backed.
     */
    public String getMergedConfigSchema(String serviceKind, String venue) {
        String kind = serviceKind.toLowerCase();
        if (!VENUE_BACKED_KINDS.contains(kind)) {
            throw new ResponseStatusException(HttpStatus.BAD_REQUEST,
                    "Service kind '" + serviceKind + "' is not venue-backed; use the plain config endpoint");
        }

        String svcSchema = getConfigSchema("service_kind", kind);
        if (svcSchema == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No config schema for service kind: " + serviceKind);
        }

        String capability = KIND_TO_CAPABILITY.getOrDefault(kind, kind);
        String venueSchema = getConfigSchema("venue_capability", venue + "/" + capability);
        if (venueSchema == null) {
            throw new ResponseStatusException(HttpStatus.NOT_FOUND,
                    "No config schema for venue=" + venue + " capability=" + capability +
                    ". Register a venue manifest with config_schema before using this endpoint.");
        }

        try {
            ObjectNode svcNode = (ObjectNode) objectMapper.readTree(svcSchema);
            ObjectNode venueNode = (ObjectNode) objectMapper.readTree(venueSchema);

            if (INLINE_VENUES.contains(venue.toLowerCase())) {
                // Simulator: merge venue properties at top level
                ObjectNode svcProps = (ObjectNode) svcNode.path("properties");
                ObjectNode venueProps = (ObjectNode) venueNode.path("properties");
                if (svcProps != null && venueProps != null) {
                    venueProps.fields().forEachRemaining(e -> svcProps.set(e.getKey(), e.getValue()));
                }
            } else {
                // Real venue: nest venue schema under venue_config
                ObjectNode svcProps = (ObjectNode) svcNode.path("properties");
                if (svcProps != null) {
                    svcProps.set("venue_config", venueNode);
                }
            }

            svcNode.put("title", svcNode.path("title").asText("") + " + " + venueNode.path("title").asText(venue));
            return objectMapper.writeValueAsString(svcNode);
        } catch (Exception e) {
            log.error("schema: failed to merge config schemas for {}/{}: {}", serviceKind, venue, e.getMessage());
            throw new ResponseStatusException(HttpStatus.INTERNAL_SERVER_ERROR, "Failed to merge schemas");
        }
    }

    public List<SchemaResourceEntry> listResources(String resourceType) {
        return repository.list(resourceType).stream()
                .map(SchemaService::toSchemaResourceEntry)
                .toList();
    }

    public void activateVersion(String resourceType, String resourceKey, int version) {
        repository.activateVersion(resourceType, resourceKey, version);
        invalidateCache(resourceType, resourceKey);
        log.info("schema: activated version {} for {}/{}", version, resourceType, resourceKey);
    }

    public void deprecateVersion(String resourceType, String resourceKey, int version) {
        repository.deprecateVersion(resourceType, resourceKey, version);
        invalidateCache(resourceType, resourceKey);
        log.info("schema: deprecated version {} for {}/{}", version, resourceType, resourceKey);
    }

    public boolean validateContentHash(String schemaId, int version, String contentHash) {
        Map<String, Object> record = repository.findBySchemaIdAndVersion(schemaId, version);
        if (record == null) return false;
        return contentHash.equals(record.get("content_hash"));
    }

    // --- Cache management ---

    private Map<String, Object> getActive(String resourceType, String resourceKey) {
        String cacheKey = resourceType + ":" + resourceKey;
        return activeCache.computeIfAbsent(cacheKey, k -> repository.findActive(resourceType, resourceKey));
    }

    void invalidateCache(String resourceType, String resourceKey) {
        activeCache.remove(resourceType + ":" + resourceKey);
    }

    void invalidateAllCaches() {
        activeCache.clear();
    }

    private static Instant toInstant(Object obj) {
        if (obj == null) return null;
        if (obj instanceof Instant i) return i;
        if (obj instanceof java.sql.Timestamp ts) return ts.toInstant();
        if (obj instanceof java.time.OffsetDateTime odt) return odt.toInstant();
        return null;
    }

    private static SchemaResourceEntry toSchemaResourceEntry(Map<String, Object> row) {
        return new SchemaResourceEntry(
                (String) row.get("resource_type"),
                (String) row.get("resource_key"),
                (String) row.get("schema_id"),
                row.get("version") != null ? ((Number) row.get("version")).intValue() : 0,
                (String) row.get("content_hash"),
                Boolean.TRUE.equals(row.get("active")),
                (String) row.get("manifest_json"),
                (String) row.get("config_schema"),
                (String) row.get("synced_from"),
                toInstant(row.get("created_at")),
                toInstant(row.get("updated_at"))
        );
    }
}
