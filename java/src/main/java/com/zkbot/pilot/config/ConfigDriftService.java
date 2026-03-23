package com.zkbot.pilot.config;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.grpc.ConfigIntrospectionClient;
import com.zkbot.pilot.schema.ConfigSchemaLocator;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Service;
import zk.config.v1.Config.GetCurrentConfigResponse;

import java.util.*;

/**
 * Detects config drift between desired (DB) and effective (live service) config.
 *
 * Uses the shared ConfigSchemaLocator to resolve the correct schema resource
 * (service_kind or venue_capability) and JsonPointerHelper for proper JSON
 * pointer traversal of nested config structures.
 *
 * Comparison rule:
 * 1. Load desired config from service-specific table via DesiredConfigRepository
 * 2. Call GetCurrentConfig on live service via ConfigIntrospectionClient
 * 3. Exclude secret_ref and resolved_secret fields from value comparison
 *    (but detect if a secret_ref logical name changed → RESTART_REQUIRED)
 * 4. Diff remaining fields; classify each using field descriptors
 * 5. Return drift result
 */
@Service
public class ConfigDriftService {

    private static final Logger log = LoggerFactory.getLogger(ConfigDriftService.class);

    private final DesiredConfigRepository desiredConfigRepo;
    private final ConfigIntrospectionClient introspectionClient;
    private final ConfigSchemaLocator schemaLocator;
    private final ObjectMapper objectMapper;

    public ConfigDriftService(DesiredConfigRepository desiredConfigRepo,
                               ConfigIntrospectionClient introspectionClient,
                               ConfigSchemaLocator schemaLocator,
                               ObjectMapper objectMapper) {
        this.desiredConfigRepo = desiredConfigRepo;
        this.introspectionClient = introspectionClient;
        this.schemaLocator = schemaLocator;
        this.objectMapper = objectMapper;
    }

    /**
     * Compute drift for a service. Returns null if drift cannot be determined
     * (service offline, no desired config, etc.).
     */
    public DriftResult computeDrift(String logicalId, String instanceType) {
        // 1. Load desired config
        var desired = desiredConfigRepo.getDesiredConfig(logicalId, instanceType);
        if (desired == null || desired.configJson() == null || desired.configJson().equals("{}")) {
            return null;
        }

        // 2. Call GetCurrentConfig on live service
        GetCurrentConfigResponse liveConfig = introspectionClient.getCurrentConfig(logicalId);
        if (liveConfig == null) {
            return null;
        }

        try {
            // 3. Parse both JSONs as mutable trees
            JsonNode desiredTree = objectMapper.readTree(desired.configJson());
            JsonNode effectiveTree = objectMapper.readTree(liveConfig.getEffectiveConfigJson());

            // 4. Get field descriptors via shared locator (venue-aware)
            var descriptors = schemaLocator.resolveFieldDescriptors(logicalId, instanceType);

            // 5. Remove secret fields from both trees, detect secret_ref changes
            boolean secretRefChanged = JsonPointerHelper.removeSecretFieldsForDrift(
                    desiredTree, effectiveTree, descriptors);

            // 6. Diff remaining fields (recursive)
            List<JsonPointerHelper.ChangedFieldEntry> changedFields = new ArrayList<>();
            diffNodes(desiredTree, effectiveTree, "", changedFields);

            // 7. Classify each changed field
            Map<String, Boolean> reloadableByPath = buildReloadableMap(descriptors);
            List<JsonPointerHelper.ChangedFieldEntry> classifiedFields = new ArrayList<>();
            for (var field : changedFields) {
                Boolean reloadable = reloadableByPath.get(field.path());
                String classification = Boolean.TRUE.equals(reloadable) ? "RELOADABLE" : "RESTART_REQUIRED";
                classifiedFields.add(new JsonPointerHelper.ChangedFieldEntry(
                        field.path(), field.desiredValue(), field.currentValue(), classification));
            }

            // 8. Overall status
            String overallStatus;
            if (secretRefChanged) {
                overallStatus = "RESTART_REQUIRED";
            } else if (classifiedFields.isEmpty()) {
                overallStatus = "NO_DIFF";
            } else {
                boolean anyRestartRequired = classifiedFields.stream()
                        .anyMatch(f -> "RESTART_REQUIRED".equals(f.classification()));
                overallStatus = anyRestartRequired ? "RESTART_REQUIRED" : "RELOADABLE";
            }

            return new DriftResult(overallStatus, classifiedFields);

        } catch (Exception e) {
            log.warn("drift: error computing drift for {}: {}", logicalId, e.getMessage());
            return null;
        }
    }

    /**
     * Recursively diff two JsonNode trees, producing ChangedFieldEntry records
     * with JSON pointer paths.
     */
    private void diffNodes(JsonNode desired, JsonNode effective, String prefix,
                           List<JsonPointerHelper.ChangedFieldEntry> result) {
        if (desired.isObject() && effective.isObject()) {
            Set<String> allKeys = new LinkedHashSet<>();
            desired.fieldNames().forEachRemaining(allKeys::add);
            effective.fieldNames().forEachRemaining(allKeys::add);

            for (String key : allKeys) {
                String path = prefix + "/" + key;
                JsonNode dVal = desired.get(key);
                JsonNode eVal = effective.get(key);

                if (dVal == null && eVal != null) {
                    result.add(new JsonPointerHelper.ChangedFieldEntry(path, "", eVal.toString(), ""));
                } else if (dVal != null && eVal == null) {
                    result.add(new JsonPointerHelper.ChangedFieldEntry(path, dVal.toString(), "", ""));
                } else if (dVal != null) {
                    if (dVal.isObject() && eVal.isObject()) {
                        diffNodes(dVal, eVal, path, result);
                    } else if (!dVal.equals(eVal)) {
                        result.add(new JsonPointerHelper.ChangedFieldEntry(
                                path, dVal.toString(), eVal.toString(), ""));
                    }
                }
            }
        } else if (!desired.equals(effective)) {
            result.add(new JsonPointerHelper.ChangedFieldEntry(
                    prefix, desired.toString(), effective.toString(), ""));
        }
    }

    private Map<String, Boolean> buildReloadableMap(List<Map<String, Object>> descriptors) {
        Map<String, Boolean> map = new HashMap<>();
        for (var fd : descriptors) {
            String path = (String) fd.get("path");
            if (path != null) {
                if (!path.startsWith("/")) path = "/" + path;
                map.put(path, Boolean.TRUE.equals(fd.get("reloadable")));
            }
        }
        return map;
    }

    public record DriftResult(String overallStatus,
                               List<JsonPointerHelper.ChangedFieldEntry> changedFields) {}
}
