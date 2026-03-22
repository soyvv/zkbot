package com.zkbot.pilot.meta;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.annotation.Value;
import org.springframework.stereotype.Component;
import org.yaml.snakeyaml.Yaml;

import jakarta.annotation.PostConstruct;
import java.io.IOException;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.ConcurrentHashMap;
import java.util.stream.Stream;

/**
 * Loads venue manifest.yaml files from the venue-integrations directory.
 * Provides venue metadata and config schemas to MetaService and topology APIs.
 */
@Component
public class VenueManifestLoader {

    private static final Logger log = LoggerFactory.getLogger(VenueManifestLoader.class);

    private final String venueIntegrationsRoot;
    private final Map<String, VenueManifest> manifests = new ConcurrentHashMap<>();

    public VenueManifestLoader(
            @Value("${pilot.venue-integrations-root:}") String venueIntegrationsRoot) {
        this.venueIntegrationsRoot = venueIntegrationsRoot;
    }

    @PostConstruct
    public void load() {
        if (venueIntegrationsRoot == null || venueIntegrationsRoot.isBlank()) {
            log.info("venue-manifest: pilot.venue-integrations-root not set, skipping manifest load");
            return;
        }

        Path root = Path.of(venueIntegrationsRoot);
        if (!Files.isDirectory(root)) {
            log.warn("venue-manifest: path not found: {}", root);
            return;
        }

        try (Stream<Path> dirs = Files.list(root)) {
            dirs.filter(Files::isDirectory).forEach(venueDir -> {
                Path manifestFile = venueDir.resolve("manifest.yaml");
                if (Files.isRegularFile(manifestFile)) {
                    loadManifest(venueDir, manifestFile);
                }
            });
        } catch (IOException e) {
            log.warn("venue-manifest: error scanning {}: {}", root, e.getMessage());
        }

        log.info("venue-manifest: loaded {} venue manifests: {}", manifests.size(), manifests.keySet());
    }

    public Collection<VenueManifest> getAll() {
        return List.copyOf(manifests.values());
    }

    public VenueManifest get(String venueId) {
        return manifests.get(venueId);
    }

    /**
     * Load a JSON Schema file for a venue capability (e.g., gw, rtmd, refdata).
     * Returns the schema as a raw string, or null if not found.
     */
    public String getConfigSchema(String venueId, String capability) {
        VenueManifest manifest = manifests.get(venueId);
        if (manifest == null) return null;
        var cap = manifest.capabilities().get(capability);
        if (cap == null || cap.configSchemaPath() == null) return null;

        Path schemaFile = Path.of(venueIntegrationsRoot, venueId, cap.configSchemaPath());
        if (!Files.isRegularFile(schemaFile)) {
            log.warn("venue-manifest: schema file not found: {}", schemaFile);
            return null;
        }
        try {
            return Files.readString(schemaFile);
        } catch (IOException e) {
            log.warn("venue-manifest: error reading schema {}: {}", schemaFile, e.getMessage());
            return null;
        }
    }

    @SuppressWarnings("unchecked")
    private void loadManifest(Path venueDir, Path manifestFile) {
        try {
            Yaml yaml = new Yaml();
            Map<String, Object> doc = yaml.load(Files.newInputStream(manifestFile));

            String venue = (String) doc.get("venue");
            int version = doc.containsKey("version") ? ((Number) doc.get("version")).intValue() : 1;

            var capabilities = new LinkedHashMap<String, VenueCapability>();
            var capsMap = (Map<String, Map<String, Object>>) doc.get("capabilities");
            if (capsMap != null) {
                capsMap.forEach((capName, capData) -> {
                    String language = (String) capData.get("language");
                    String entrypoint = (String) capData.get("entrypoint");
                    String configSchema = (String) capData.get("config_schema");
                    capabilities.put(capName, new VenueCapability(language, entrypoint, configSchema));
                });
            }

            var metadataMap = (Map<String, Object>) doc.getOrDefault("metadata", Map.of());
            boolean supportsTradfiSessions = Boolean.TRUE.equals(metadataMap.get("supports_tradfi_sessions"));
            @SuppressWarnings("unchecked")
            List<String> notes = (List<String>) metadataMap.getOrDefault("notes", List.of());

            var manifest = new VenueManifest(venue, version, capabilities,
                    supportsTradfiSessions, notes, venueDir.toString());
            manifests.put(venue, manifest);

        } catch (Exception e) {
            log.warn("venue-manifest: failed to load {}: {}", manifestFile, e.getMessage());
        }
    }

    public record VenueManifest(
            String venue,
            int version,
            Map<String, VenueCapability> capabilities,
            boolean supportsTradfiSessions,
            List<String> notes,
            String path
    ) {}

    public record VenueCapability(
            String language,
            String entrypoint,
            String configSchemaPath
    ) {}
}
