package com.zkbot.pilot.schema;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.schema.dto.SchemaResourceEntry;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Service;

import java.time.Instant;
import java.util.Collections;
import java.util.List;
import java.util.Map;
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
