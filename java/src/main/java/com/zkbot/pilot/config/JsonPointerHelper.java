package com.zkbot.pilot.config;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;

import java.util.*;

/**
 * Shared JSON pointer operations for config field access.
 *
 * All field descriptor paths use JSON pointer syntax (e.g. "/secret_ref", "/nested/api_key").
 * This helper provides consistent traversal for:
 *   - Reading values from config JSON
 *   - Removing fields for drift exclusion
 *   - Extracting secret_ref logical refs
 */
public final class JsonPointerHelper {

    private JsonPointerHelper() {}

    /**
     * Read a value at a JSON pointer path from a parsed JsonNode tree.
     * Returns null if the path doesn't exist or any intermediate node is missing.
     *
     * @param root JSON object tree
     * @param pointer JSON pointer path, e.g. "/secret_ref" or "/venue_config/api_key"
     */
    public static JsonNode readPointer(JsonNode root, String pointer) {
        if (pointer == null || pointer.isEmpty() || root == null) return null;
        JsonNode node = root.at(pointer);
        return node.isMissingNode() ? null : node;
    }

    /**
     * Read a string value at a JSON pointer path. Returns null if not a string or missing.
     */
    public static String readStringPointer(JsonNode root, String pointer) {
        JsonNode node = readPointer(root, pointer);
        return (node != null && node.isTextual()) ? node.asText() : null;
    }

    /**
     * Remove a value at a JSON pointer path from a mutable JsonNode tree.
     * Returns the removed value, or null if the path didn't exist.
     *
     * For a path like "/venue_config/secret_ref", traverses to the parent
     * "/venue_config" and removes the "secret_ref" field from it.
     */
    public static JsonNode removePointer(JsonNode root, String pointer) {
        if (pointer == null || pointer.isEmpty() || root == null) return null;

        // Split pointer into parent path + leaf field name
        int lastSlash = pointer.lastIndexOf('/');
        if (lastSlash < 0) return null;

        String parentPath = pointer.substring(0, lastSlash);
        String fieldName = pointer.substring(lastSlash + 1);

        // Navigate to parent node
        JsonNode parent;
        if (parentPath.isEmpty()) {
            parent = root;
        } else {
            parent = root.at(parentPath);
            if (parent.isMissingNode()) return null;
        }

        if (parent instanceof ObjectNode objectNode) {
            return objectNode.remove(fieldName);
        }
        return null;
    }

    /**
     * Extract secret_ref logical refs from a config JSON using field descriptors.
     *
     * Returns a list of (fieldKey, logicalRef) pairs for fields marked as secret_ref
     * that have non-blank string values.
     *
     * @param configJson the runtime config JSON string
     * @param descriptors field descriptor maps with "path" and "secret_ref" keys
     * @param objectMapper shared ObjectMapper
     */
    public static List<SecretRefEntry> extractSecretRefs(
            String configJson, List<Map<String, Object>> descriptors, ObjectMapper objectMapper) {
        List<SecretRefEntry> refs = new ArrayList<>();
        if (configJson == null || configJson.isBlank() || descriptors.isEmpty()) return refs;

        try {
            JsonNode root = objectMapper.readTree(configJson);

            for (var fd : descriptors) {
                if (!Boolean.TRUE.equals(fd.get("secret_ref"))) continue;

                String path = (String) fd.get("path");
                if (path == null) continue;

                // Ensure path starts with /
                if (!path.startsWith("/")) path = "/" + path;

                String logicalRef = readStringPointer(root, path);
                if (logicalRef != null && !logicalRef.isBlank()) {
                    // field_key is the leaf name (last segment of the pointer)
                    String fieldKey = path.substring(path.lastIndexOf('/') + 1);
                    refs.add(new SecretRefEntry(fieldKey, logicalRef));
                }
            }
        } catch (Exception e) {
            // Caller handles logging
        }
        return refs;
    }

    /**
     * Remove all secret and resolved-secret fields from two JsonNode trees (for drift comparison).
     * Returns true if any secret_ref logical value changed between desired and effective.
     *
     * @param desired mutable desired config tree
     * @param effective mutable effective config tree
     * @param descriptors field descriptors with "path", "secret_ref", "resolved_secret" keys
     */
    public static boolean removeSecretFieldsForDrift(
            JsonNode desired, JsonNode effective, List<Map<String, Object>> descriptors) {
        boolean secretRefChanged = false;

        for (var fd : descriptors) {
            boolean isSecretRef = Boolean.TRUE.equals(fd.get("secret_ref"));
            boolean isResolvedSecret = Boolean.TRUE.equals(fd.get("resolved_secret"));

            if (!isSecretRef && !isResolvedSecret) continue;

            String path = (String) fd.get("path");
            if (path == null) continue;
            if (!path.startsWith("/")) path = "/" + path;

            JsonNode desiredVal = removePointer(desired, path);
            JsonNode effectiveVal = removePointer(effective, path);

            // If this is a secret_ref field and the logical ref changed, flag it
            if (isSecretRef) {
                String desiredStr = (desiredVal != null && desiredVal.isTextual()) ? desiredVal.asText() : null;
                String effectiveStr = (effectiveVal != null && effectiveVal.isTextual()) ? effectiveVal.asText() : null;
                if (!Objects.equals(desiredStr, effectiveStr)) {
                    secretRefChanged = true;
                }
            }
        }

        return secretRefChanged;
    }

    /**
     * Classify drift for a set of field diffs using field descriptors.
     *
     * @param changedPaths set of JSON pointer paths that differ between desired and effective
     * @param descriptors field descriptors with "path" and "reloadable" keys
     * @return "NO_DIFF", "RELOADABLE", or "RESTART_REQUIRED"
     */
    public static String classifyDrift(
            List<ChangedFieldEntry> changedFields, List<Map<String, Object>> descriptors) {
        if (changedFields.isEmpty()) return "NO_DIFF";

        // Build path → reloadable lookup
        Map<String, Boolean> reloadableByPath = new HashMap<>();
        for (var fd : descriptors) {
            String path = (String) fd.get("path");
            if (path != null) {
                if (!path.startsWith("/")) path = "/" + path;
                reloadableByPath.put(path, Boolean.TRUE.equals(fd.get("reloadable")));
            }
        }

        boolean anyRestartRequired = false;
        for (var field : changedFields) {
            Boolean reloadable = reloadableByPath.get(field.path());
            if (!Boolean.TRUE.equals(reloadable)) {
                anyRestartRequired = true;
                break;
            }
        }

        return anyRestartRequired ? "RESTART_REQUIRED" : "RELOADABLE";
    }

    // --- Value types ---

    public record SecretRefEntry(String fieldKey, String logicalRef) {}

    public record ChangedFieldEntry(String path, String desiredValue, String currentValue,
                                    String classification) {}
}
