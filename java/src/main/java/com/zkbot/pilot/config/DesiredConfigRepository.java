package com.zkbot.pilot.config;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

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

    public DesiredConfigRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    /**
     * Get provided config for a service from its service-specific table.
     * Returns null for unknown/unsupported instance types — no legacy fallback.
     */
    public DesiredConfig getDesiredConfig(String logicalId, String instanceType) {
        String upper = instanceType.toUpperCase();
        String sql = switch (upper) {
            case "OMS" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.oms_instance WHERE oms_id = ?";
            case "GW" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.gateway_instance WHERE gw_id = ?";
            case "ENGINE" -> "SELECT provided_config::text AS config, config_version, config_hash " +
                    "FROM cfg.engine_instance WHERE engine_id = ?";
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
     * Set provided config in the service-specific table.
     * The config_version trigger auto-bumps version and recomputes hash on provided_config change.
     * Throws IllegalArgumentException for unsupported instance types.
     */
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
