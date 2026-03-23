package com.zkbot.pilot.schema;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.config.PilotProperties;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Component;
import org.yaml.snakeyaml.Yaml;

import jakarta.annotation.PostConstruct;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.*;
import java.util.stream.Stream;

/**
 * Syncs bundled manifest files (service-manifests/ and venue-integrations/) into
 * the cfg.schema_resource DB table on Pilot startup.
 *
 * Bundled manifests are authoritative. The DB is a derived operational mirror.
 * Hash mismatch (same schema_id + version but different content) = fail closed.
 */
@Component
public class SchemaRegistrySyncer {

    private static final Logger log = LoggerFactory.getLogger(SchemaRegistrySyncer.class);

    private final SchemaRepository repository;
    private final ObjectMapper objectMapper;
    private final String serviceManifestsRoot;
    private final String venueIntegrationsRoot;

    public SchemaRegistrySyncer(
            SchemaRepository repository,
            ObjectMapper objectMapper,
            PilotProperties props) {
        this.repository = repository;
        this.objectMapper = objectMapper;
        this.serviceManifestsRoot = props.serviceManifestsRoot();
        this.venueIntegrationsRoot = props.venueIntegrationsRoot();
    }

    @PostConstruct
    public void sync() {
        int newCount = 0, unchangedCount = 0, mismatchCount = 0;

        // Sync service-kind manifests
        if (serviceManifestsRoot != null && !serviceManifestsRoot.isBlank()) {
            Path root = Path.of(serviceManifestsRoot);
            if (Files.isDirectory(root)) {
                var result = syncDirectory(root, "service_kind");
                newCount += result[0];
                unchangedCount += result[1];
                mismatchCount += result[2];
            } else {
                log.info("schema-sync: service-manifests-root not found: {}", root);
            }
        }

        // Sync venue-capability manifests
        if (venueIntegrationsRoot != null && !venueIntegrationsRoot.isBlank()) {
            Path root = Path.of(venueIntegrationsRoot);
            if (Files.isDirectory(root)) {
                var result = syncVenueManifests(root);
                newCount += result[0];
                unchangedCount += result[1];
                mismatchCount += result[2];
            }
        }

        log.info("schema-sync: complete — new={}, unchanged={}, mismatch={}", newCount, unchangedCount, mismatchCount);
        if (mismatchCount > 0) {
            log.error("schema-sync: {} SCHEMA_MISMATCH errors detected. " +
                    "Manifests were changed without bumping version. Fix in code and redeploy.", mismatchCount);
        }
    }

    /**
     * Sync service-kind manifests from a directory structure like:
     *   service-manifests/oms/manifest.yaml
     *   service-manifests/gw/manifest.yaml
     */
    private int[] syncDirectory(Path root, String resourceType) {
        int[] counts = new int[3]; // [new, unchanged, mismatch]
        try (Stream<Path> dirs = Files.list(root)) {
            dirs.filter(Files::isDirectory).forEach(dir -> {
                Path manifestFile = dir.resolve("manifest.yaml");
                if (Files.isRegularFile(manifestFile)) {
                    syncOneManifest(manifestFile, resourceType, dir.getFileName().toString(), counts);
                }
            });
        } catch (IOException e) {
            log.warn("schema-sync: error scanning {}: {}", root, e.getMessage());
        }
        return counts;
    }

    /**
     * Sync venue-capability manifests. Each venue manifest may define multiple capabilities.
     * Each capability becomes a separate schema_resource record:
     *   resource_type=venue_capability, resource_key=okx/gw, etc.
     */
    private int[] syncVenueManifests(Path root) {
        int[] counts = new int[3];
        try (Stream<Path> dirs = Files.list(root)) {
            dirs.filter(Files::isDirectory).forEach(venueDir -> {
                Path manifestFile = venueDir.resolve("manifest.yaml");
                if (Files.isRegularFile(manifestFile)) {
                    syncVenueCapabilities(manifestFile, venueDir, counts);
                }
            });
        } catch (IOException e) {
            log.warn("schema-sync: error scanning {}: {}", root, e.getMessage());
        }
        return counts;
    }

    @SuppressWarnings("unchecked")
    private void syncOneManifest(Path manifestFile, String resourceType, String resourceKey, int[] counts) {
        try {
            String content = Files.readString(manifestFile, StandardCharsets.UTF_8);
            Yaml yaml = new Yaml();
            Map<String, Object> doc = yaml.load(content);

            String schemaId = (String) doc.get("schema_id");
            int version = doc.containsKey("version") ? ((Number) doc.get("version")).intValue() : 1;

            if (schemaId == null) {
                log.debug("schema-sync: no schema_id in {}, skipping", manifestFile);
                return;
            }

            String manifestJson = objectMapper.writeValueAsString(doc);
            String contentHash = sha256Hex(manifestJson);

            // Load config schema if referenced
            String configSchemaJson = null;
            String configSchemaPath = (String) doc.get("config_schema");
            if (configSchemaPath != null) {
                Path schemaFile = manifestFile.getParent().resolve(configSchemaPath);
                if (Files.isRegularFile(schemaFile)) {
                    configSchemaJson = Files.readString(schemaFile, StandardCharsets.UTF_8);
                }
            }

            syncToDb(resourceType, resourceKey, schemaId, version, contentHash,
                    manifestJson, configSchemaJson, counts);

        } catch (Exception e) {
            log.warn("schema-sync: failed to process {}: {}", manifestFile, e.getMessage());
        }
    }

    @SuppressWarnings("unchecked")
    private void syncVenueCapabilities(Path manifestFile, Path venueDir, int[] counts) {
        try {
            String content = Files.readString(manifestFile, StandardCharsets.UTF_8);
            Yaml yaml = new Yaml();
            Map<String, Object> doc = yaml.load(content);

            String venue = (String) doc.get("venue");
            String schemaId = (String) doc.get("schema_id");
            int version = doc.containsKey("version") ? ((Number) doc.get("version")).intValue() : 1;

            if (schemaId == null || venue == null) {
                log.debug("schema-sync: no schema_id/venue in {}, skipping", manifestFile);
                return;
            }

            var capabilities = (Map<String, Map<String, Object>>) doc.get("capabilities");
            if (capabilities == null) return;

            for (var entry : capabilities.entrySet()) {
                String capName = entry.getKey();
                Map<String, Object> capData = entry.getValue();

                String resourceKey = venue + "/" + capName;

                // Build per-capability manifest JSON (capability data + venue-level metadata)
                Map<String, Object> capManifest = new LinkedHashMap<>();
                capManifest.put("venue", venue);
                capManifest.put("schema_id", schemaId);
                capManifest.put("version", version);
                capManifest.put("capability", capName);
                capManifest.putAll(capData);

                String manifestJson = objectMapper.writeValueAsString(capManifest);
                String contentHash = sha256Hex(manifestJson);

                // Load capability-specific config schema
                String configSchemaJson = null;
                String configSchemaPath = (String) capData.get("config_schema");
                if (configSchemaPath != null) {
                    Path schemaFile = venueDir.resolve(configSchemaPath);
                    if (Files.isRegularFile(schemaFile)) {
                        configSchemaJson = Files.readString(schemaFile, StandardCharsets.UTF_8);
                    }
                }

                syncToDb("venue_capability", resourceKey, schemaId, version, contentHash,
                        manifestJson, configSchemaJson, counts);
            }
        } catch (Exception e) {
            log.warn("schema-sync: failed to process venue manifest {}: {}", manifestFile, e.getMessage());
        }
    }

    private void syncToDb(String resourceType, String resourceKey, String schemaId,
                           int version, String contentHash, String manifestJson,
                           String configSchemaJson, int[] counts) {
        // Use primary key (resource_type, resource_key, version) for lookup — not schema_id,
        // because venue manifests share one schema_id across multiple capabilities.
        Map<String, Object> existing = repository.find(resourceType, resourceKey, version);
        if (existing != null) {
            String existingHash = (String) existing.get("content_hash");
            if (contentHash.equals(existingHash)) {
                counts[1]++; // unchanged
            } else {
                counts[2]++; // mismatch
                log.error("SCHEMA_MISMATCH: {}/{} version={} — bundled hash={} != db hash={}. " +
                        "Manifest changed without version bump.",
                        resourceType, resourceKey, version, contentHash, existingHash);
            }
        } else {
            repository.insert(resourceType, resourceKey, schemaId, version, contentHash,
                    manifestJson, configSchemaJson);
            counts[0]++; // new
            log.info("schema-sync: inserted {}/{} schema_id={} version={}", resourceType, resourceKey, schemaId, version);
        }
    }

    private static String sha256Hex(String input) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            byte[] digest = md.digest(input.getBytes(StandardCharsets.UTF_8));
            return HexFormat.of().formatHex(digest);
        } catch (NoSuchAlgorithmException e) {
            throw new RuntimeException("SHA-256 not available", e);
        }
    }
}
