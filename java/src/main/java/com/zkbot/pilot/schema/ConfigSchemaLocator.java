package com.zkbot.pilot.schema;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Component;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Shared schema resolution for a (logicalId, instanceType) pair.
 *
 * Venue-backed services (GW, MDGW) have composite config: host-level fields
 * from service_kind manifests (grpc_port, nats_url, bootstrap_token) PLUS
 * venue-specific fields from venue_capability manifests (secret_ref, api_base_url).
 * This locator merges both sets of descriptors.
 *
 * Resolution rules:
 *   - Non-venue services (OMS, ENGINE) → service_kind/{instanceType} only
 *   - Venue-backed services (GW, MDGW) → service_kind/{instanceType} + venue_capability/{venue}/{capability}
 *   - REFDATA → venue_capability/{venue}/refdata only (no service_kind layer)
 *
 * Used by BootstrapService, ConfigDriftService, and any other code that needs
 * field descriptors or config schemas for a given service instance.
 */
@Component
public class ConfigSchemaLocator {

    private static final Logger log = LoggerFactory.getLogger(ConfigSchemaLocator.class);

    /**
     * Instance types that have composite config (host + venue slices).
     * GW and MDGW have both a service_kind manifest and a venue_capability manifest.
     */
    private static final Set<String> VENUE_BACKED_TYPES = Set.of("GW", "MDGW");

    /**
     * Instance types that are venue-scoped but not composite (no service_kind layer).
     * REFDATA resolves descriptors from venue_capability/{venue}/refdata only.
     */
    private static final Set<String> VENUE_ONLY_TYPES = Set.of("REFDATA");

    /**
     * Maps instance_type → the capability name used in venue manifests.
     * GW → gw, MDGW → rtmd, REFDATA → refdata.
     */
    private static final Map<String, String> TYPE_TO_CAPABILITY = Map.of(
            "GW", "gw",
            "MDGW", "rtmd",
            "REFDATA", "refdata"
    );

    private final SchemaService schemaService;
    private final JdbcTemplate jdbc;

    public ConfigSchemaLocator(SchemaService schemaService, JdbcTemplate jdbc) {
        this.schemaService = schemaService;
        this.jdbc = jdbc;
    }

    /**
     * Resolve field descriptors for a service instance.
     *
     * For venue-backed types (GW, MDGW):
     *   1. Always include service_kind descriptors (host-level: grpc_port, bootstrap_token, etc.)
     *   2. Look up venue from service-specific DB table
     *   3. If venue found, merge in venue_capability descriptors (secret_ref, api_base_url, etc.)
     *   4. If venue not found, log warning and return service_kind descriptors only
     *
     * For venue-only types (REFDATA):
     *   - Look up venue, resolve venue_capability/{venue}/refdata descriptors only
     *   - No service_kind layer
     *
     * For non-venue types (OMS, ENGINE): return service_kind descriptors only.
     */
    public List<Map<String, Object>> resolveFieldDescriptors(String logicalId, String instanceType) {
        String upper = instanceType.toUpperCase();

        // REFDATA: venue-only, no service_kind layer
        if (VENUE_ONLY_TYPES.contains(upper)) {
            return resolveVenueOnlyDescriptors(logicalId, upper);
        }

        var svcDescriptors = schemaService.getFieldDescriptors("service_kind", upper.toLowerCase());

        if (!VENUE_BACKED_TYPES.contains(upper)) {
            return svcDescriptors;
        }

        // Venue-backed: merge service_kind + venue_capability descriptors
        String venue = lookupVenue(logicalId, upper);
        if (venue == null) {
            log.warn("schema-locator: no venue found for {} ({}), returning service_kind descriptors only. " +
                    "Venue-specific secret_ref fields will not be resolved.", logicalId, upper);
            return svcDescriptors;
        }

        String capability = TYPE_TO_CAPABILITY.getOrDefault(upper, upper.toLowerCase());
        String resourceKey = venue + "/" + capability;
        var venueDescriptors = schemaService.getFieldDescriptors("venue_capability", resourceKey);

        if (venueDescriptors.isEmpty()) {
            log.warn("schema-locator: no venue_capability descriptors for {}, " +
                    "returning service_kind/{} only. Venue-specific fields will not be classified.",
                    resourceKey, upper.toLowerCase());
            return svcDescriptors;
        }

        // Merge: service_kind first, then venue_capability
        var merged = new ArrayList<Map<String, Object>>(svcDescriptors.size() + venueDescriptors.size());
        merged.addAll(svcDescriptors);
        merged.addAll(venueDescriptors);
        return merged;
    }

    private List<Map<String, Object>> resolveVenueOnlyDescriptors(String logicalId, String instanceType) {
        String venue = lookupVenue(logicalId, instanceType);
        if (venue == null) {
            log.warn("schema-locator: no venue found for {} ({}), returning empty descriptors.", logicalId, instanceType);
            return List.of();
        }
        String capability = TYPE_TO_CAPABILITY.getOrDefault(instanceType, instanceType.toLowerCase());
        return schemaService.getFieldDescriptors("venue_capability", venue + "/" + capability);
    }

    /**
     * Resolve the config schema JSON for a service instance.
     * For venue-backed types, prefers venue_capability schema; falls back to service_kind.
     * For venue-only types (REFDATA), resolves from venue_capability only.
     */
    public String resolveConfigSchema(String logicalId, String instanceType) {
        String upper = instanceType.toUpperCase();

        if (VENUE_ONLY_TYPES.contains(upper)) {
            String venue = lookupVenue(logicalId, upper);
            if (venue != null) {
                String capability = TYPE_TO_CAPABILITY.getOrDefault(upper, upper.toLowerCase());
                return schemaService.getConfigSchema("venue_capability", venue + "/" + capability);
            }
            return null;
        }

        if (VENUE_BACKED_TYPES.contains(upper)) {
            String venue = lookupVenue(logicalId, upper);
            if (venue != null) {
                String capability = TYPE_TO_CAPABILITY.getOrDefault(upper, upper.toLowerCase());
                String schema = schemaService.getConfigSchema("venue_capability", venue + "/" + capability);
                if (schema != null) return schema;
            }
        }

        return schemaService.getConfigSchema("service_kind", upper.toLowerCase());
    }

    /**
     * Check whether an instance type is venue-backed (has composite config)
     * or venue-only (REFDATA).
     */
    public boolean isVenueBacked(String instanceType) {
        String upper = instanceType.toUpperCase();
        return VENUE_BACKED_TYPES.contains(upper) || VENUE_ONLY_TYPES.contains(upper);
    }

    /**
     * Look up the venue for a given logicalId and instanceType from the
     * appropriate service-specific table.
     */
    String lookupVenue(String logicalId, String instanceType) {
        String sql = switch (instanceType.toUpperCase()) {
            case "GW" -> "SELECT venue FROM cfg.gateway_instance WHERE gw_id = ?";
            case "MDGW" -> "SELECT venue FROM cfg.mdgw_instance WHERE mdgw_id = ?";
            case "REFDATA" -> "SELECT venue FROM cfg.refdata_venue_instance WHERE logical_id = ?";
            default -> null;
        };
        if (sql == null) return null;

        return jdbc.query(sql, rs -> rs.next() ? rs.getString("venue") : null, logicalId);
    }
}
