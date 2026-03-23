package com.zkbot.pilot.config;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.Set;

/**
 * Abstracts access to service-specific desired config tables.
 * Routes by instance_type to the correct table (cfg.oms_instance, cfg.gateway_instance, etc.).
 *
 * These service-specific tables are the config authority — NOT cfg.logical_instance.
 * Unknown/unsupported instance types return null (no silent legacy fallback).
 */
@Repository
public class DesiredConfigRepository {

    private static final Logger log = LoggerFactory.getLogger(DesiredConfigRepository.class);

    /** Service kinds with managed desired-config tables. */
    private static final Set<String> MANAGED_TYPES = Set.of("OMS", "GW", "ENGINE", "MDGW");

    private final JdbcTemplate jdbc;

    public DesiredConfigRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    /**
     * Get desired config for a service from its service-specific table.
     * Returns null for unknown/unsupported instance types — no legacy fallback.
     */
    public DesiredConfig getDesiredConfig(String logicalId, String instanceType) {
        String upper = instanceType.toUpperCase();
        String sql = switch (upper) {
            case "OMS" -> "SELECT runtime_config::text AS config, config_version, config_hash " +
                    "FROM cfg.oms_instance WHERE oms_id = ?";
            case "GW" -> "SELECT runtime_config::text AS config, config_version, config_hash " +
                    "FROM cfg.gateway_instance WHERE gw_id = ?";
            case "ENGINE" -> "SELECT runtime_config::text AS config, config_version, config_hash " +
                    "FROM cfg.engine_instance WHERE engine_id = ?";
            case "MDGW" -> "SELECT runtime_config::text AS config, config_version, config_hash " +
                    "FROM cfg.mdgw_instance WHERE mdgw_id = ?";
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
     * Set desired config in the service-specific table.
     * The config_version trigger auto-bumps version and recomputes hash.
     * Throws IllegalArgumentException for unsupported instance types.
     */
    public void setDesiredConfig(String logicalId, String instanceType, String configJson) {
        String upper = instanceType.toUpperCase();
        String sql = switch (upper) {
            case "OMS" -> "UPDATE cfg.oms_instance SET runtime_config = ?::jsonb WHERE oms_id = ?";
            case "GW" -> "UPDATE cfg.gateway_instance SET runtime_config = ?::jsonb WHERE gw_id = ?";
            case "ENGINE" -> "UPDATE cfg.engine_instance SET runtime_config = ?::jsonb WHERE engine_id = ?";
            case "MDGW" -> "UPDATE cfg.mdgw_instance SET runtime_config = ?::jsonb WHERE mdgw_id = ?";
            default -> throw new IllegalArgumentException(
                    "Cannot set desired config: unsupported instance_type '" + instanceType + "'");
        };
        jdbc.update(sql, configJson, logicalId);
    }

    /**
     * Whether an instance type has a managed desired-config table.
     */
    public boolean isManagedType(String instanceType) {
        return MANAGED_TYPES.contains(instanceType.toUpperCase());
    }

    public record DesiredConfig(String configJson, int configVersion, String configHash) {}
}
