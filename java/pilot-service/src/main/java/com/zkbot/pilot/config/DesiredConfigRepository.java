package com.zkbot.pilot.config;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Abstracts access to service-specific provided config tables.
 * Routes by instance_type to the correct table (cfg.oms_instance, cfg.gateway_instance, etc.).
 *
 * Config authority is the {@code provided_config} column in each service-specific table.
 * {@code runtime_config} is live/effective state owned by the service, not the control-plane payload.
 * Unknown/unsupported instance types return null (no silent legacy fallback).
 */
@Repository
public class DesiredConfigRepository {

    private static final Logger log = LoggerFactory.getLogger(DesiredConfigRepository.class);

    /** Service kinds with managed config tables. */
    private static final Set<String> MANAGED_TYPES = Set.of("OMS", "GW", "ENGINE", "MDGW", "REFDATA");

    private final JdbcTemplate jdbc;
    private final ObjectMapper objectMapper = new ObjectMapper();

    public DesiredConfigRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    /**
     * Get provided config for a service from its service-specific table.
     * Returns null for unknown/unsupported instance types — no legacy fallback.
     *
     * REFDATA has a special shape: one logical_instance per env serves N venues.
     * Callers that need the bootstrap payload for REFDATA must use
     * {@link #getRefdataAggregate(String)} keyed by env instead.
     */
    public DesiredConfig getDesiredConfig(String logicalId, String instanceType) {
        String upper = instanceType.toUpperCase();
        String sql = switch (upper) {
            case "OMS" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.oms_instance WHERE oms_id = ?";
            case "GW" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.gateway_instance WHERE gw_id = ?";
            case "ENGINE" -> """
                    SELECT (
                        COALESCE(ei.provided_config, '{}'::jsonb) ||
                        jsonb_build_object(
                            'strategy_key', ei.strategy_id,
                            'strategy_type_key', COALESCE(sd.strategy_type_key, ''),
                            'strategy_config_json', COALESCE(sd.config_json::text, ''),
                            'oms_id', COALESCE(ei.target_oms_id, '')
                        )
                    )::text AS config,
                    ei.config_version,
                    ei.config_hash
                    FROM cfg.engine_instance ei
                    JOIN cfg.strategy_definition sd ON sd.strategy_id = ei.strategy_id
                    WHERE ei.engine_id = ?
                    """;
            case "MDGW" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.mdgw_instance WHERE mdgw_id = ?";
            case "REFDATA" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.refdata_venue_instance WHERE logical_id = ?";
            default -> {
                log.warn("desired-config: unsupported instance_type '{}' for logical_id '{}' — " +
                        "no service-specific table configured", instanceType, logicalId);
                yield null;
            }
        };

        if (sql == null) return null;

        return jdbc.query(sql, rs -> {
            if (!rs.next()) return null;
            return new DesiredConfig(
                    rs.getString("config"),
                    rs.getInt("config_version"),
                    rs.getString("config_hash")
            );
        }, logicalId);
    }

    /**
     * Build the REFDATA bootstrap payload for an env by aggregating all venue rows
     * into {@code {"venues": [{"venue":..., "enabled":..., "config":{...}}, ...]}},
     * the shape {@code zk-refdata-svc} expects.
     *
     * {@code config_version} aggregates as {@code max(config_version)} across rows;
     * {@code config_hash} is a synthetic SHA-256 over the serialized payload so it
     * changes deterministically when any venue row changes.
     *
     * Returns null when no venue rows exist for the given env.
     */
    public DesiredConfig getRefdataAggregate(String env) {
        String sql = "SELECT venue, enabled, provided_config::text AS config, config_version " +
                "FROM cfg.refdata_venue_instance WHERE env = ? ORDER BY venue";

        List<Map<String, Object>> rows = jdbc.queryForList(sql, env);
        if (rows.isEmpty()) return null;

        ArrayNode venues = objectMapper.createArrayNode();
        int maxVersion = 0;
        for (Map<String, Object> row : rows) {
            ObjectNode entry = objectMapper.createObjectNode();
            entry.put("venue", (String) row.get("venue"));
            entry.put("enabled", (Boolean) row.get("enabled"));
            String configJson = (String) row.get("config");
            try {
                entry.set("config", configJson == null || configJson.isBlank()
                        ? objectMapper.createObjectNode()
                        : objectMapper.readTree(configJson));
            } catch (JsonProcessingException e) {
                log.warn("refdata-aggregate: invalid JSON for venue '{}' env '{}' — using empty object",
                        row.get("venue"), env);
                entry.set("config", objectMapper.createObjectNode());
            }
            venues.add(entry);
            Number version = (Number) row.get("config_version");
            if (version != null) {
                maxVersion = Math.max(maxVersion, version.intValue());
            }
        }

        ObjectNode payload = objectMapper.createObjectNode();
        payload.set("venues", venues);

        String payloadJson;
        try {
            payloadJson = objectMapper.writeValueAsString(payload);
        } catch (JsonProcessingException e) {
            throw new IllegalStateException("refdata-aggregate: failed to serialize payload for env " + env, e);
        }

        return new DesiredConfig(payloadJson, maxVersion, sha256Hex(payloadJson));
    }

    private static String sha256Hex(String input) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            byte[] digest = md.digest(input.getBytes(StandardCharsets.UTF_8));
            return HexFormat.of().formatHex(digest);
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 not available", e);
        }
    }

    /**
     * Set provided config in the service-specific table.
     * The config_version trigger auto-bumps version and recomputes hash on provided_config change.
     * Throws IllegalArgumentException for unsupported instance types.
     * @deprecated Use TopologyService.updateService() or updateRefdataVenueConfig() instead,
     *             which also refresh schema provenance and enforce offline gate.
     */
    @Deprecated(forRemoval = true)
    public void setDesiredConfig(String logicalId, String instanceType, String configJson) {
        String upper = instanceType.toUpperCase();
        String sql = switch (upper) {
            case "OMS" -> "UPDATE cfg.oms_instance SET provided_config = ?::jsonb WHERE oms_id = ?";
            case "GW" -> "UPDATE cfg.gateway_instance SET provided_config = ?::jsonb WHERE gw_id = ?";
            case "ENGINE" -> "UPDATE cfg.engine_instance SET provided_config = ?::jsonb WHERE engine_id = ?";
            case "MDGW" -> "UPDATE cfg.mdgw_instance SET provided_config = ?::jsonb WHERE mdgw_id = ?";
            case "REFDATA" -> "UPDATE cfg.refdata_venue_instance SET provided_config = ?::jsonb WHERE logical_id = ?";
            default -> throw new IllegalArgumentException(
                    "Cannot set desired config: unsupported instance_type '" + instanceType + "'");
        };
        jdbc.update(sql, configJson, logicalId);
    }

    /**
     * Whether an instance type has a managed config table.
     */
    public boolean isManagedType(String instanceType) {
        return MANAGED_TYPES.contains(instanceType.toUpperCase());
    }

    public record DesiredConfig(String configJson, int configVersion, String configHash) {}
}
