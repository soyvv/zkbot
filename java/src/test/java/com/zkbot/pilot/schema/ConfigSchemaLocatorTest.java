package com.zkbot.pilot.schema;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.jdbc.core.ResultSetExtractor;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.mockito.ArgumentMatchers.*;
import static org.mockito.Mockito.when;

@ExtendWith(MockitoExtension.class)
class ConfigSchemaLocatorTest {

    @Mock SchemaService schemaService;
    @Mock JdbcTemplate jdbc;

    ConfigSchemaLocator locator;

    @BeforeEach
    void setUp() {
        locator = new ConfigSchemaLocator(schemaService, jdbc);
    }

    // ── Non-venue types resolve via service_kind only ─────────────────────

    @Test
    void resolveFieldDescriptors_oms_uses_service_kind() {
        var descriptors = List.of(Map.<String, Object>of("path", "/nats_url", "reloadable", false));
        when(schemaService.getFieldDescriptors("service_kind", "oms")).thenReturn(descriptors);

        var result = locator.resolveFieldDescriptors("oms_1", "OMS");
        assertThat(result).isEqualTo(descriptors);
    }

    @Test
    void resolveFieldDescriptors_engine_uses_service_kind() {
        var descriptors = List.of(Map.<String, Object>of("path", "/grpc_port", "reloadable", false));
        when(schemaService.getFieldDescriptors("service_kind", "engine")).thenReturn(descriptors);

        var result = locator.resolveFieldDescriptors("engine_1", "ENGINE");
        assertThat(result).isEqualTo(descriptors);
    }

    // ── Venue-backed types merge service_kind + venue_capability ──────────

    @SuppressWarnings("unchecked")
    @Test
    void resolveFieldDescriptors_gw_merges_service_kind_and_venue_capability() {
        when(jdbc.query(contains("cfg.gateway_instance"), any(ResultSetExtractor.class), eq("gw_okx_1")))
                .thenReturn("okx");

        var svcDescriptors = List.of(
                Map.<String, Object>of("path", "/grpc_port", "reloadable", false),
                Map.<String, Object>of("path", "/bootstrap_token", "reloadable", false, "resolved_secret", true)
        );
        when(schemaService.getFieldDescriptors("service_kind", "gw")).thenReturn(svcDescriptors);

        var venueDescriptors = List.of(
                Map.<String, Object>of("path", "/secret_ref", "secret_ref", true, "reloadable", false),
                Map.<String, Object>of("path", "/api_base_url", "reloadable", false)
        );
        when(schemaService.getFieldDescriptors("venue_capability", "okx/gw")).thenReturn(venueDescriptors);

        var result = locator.resolveFieldDescriptors("gw_okx_1", "GW");

        // Should contain all descriptors from both sources
        assertThat(result).hasSize(4);
        // Service-kind first, then venue-capability
        assertThat(result.get(0).get("path")).isEqualTo("/grpc_port");
        assertThat(result.get(1).get("path")).isEqualTo("/bootstrap_token");
        assertThat(result.get(2).get("path")).isEqualTo("/secret_ref");
        assertThat(result.get(3).get("path")).isEqualTo("/api_base_url");
    }

    @SuppressWarnings("unchecked")
    @Test
    void resolveFieldDescriptors_mdgw_merges_with_rtmd_capability() {
        when(jdbc.query(contains("cfg.mdgw_instance"), any(ResultSetExtractor.class), eq("mdgw_oanda_1")))
                .thenReturn("oanda");

        var svcDescriptors = List.of(
                Map.<String, Object>of("path", "/grpc_port", "reloadable", false),
                Map.<String, Object>of("path", "/nats_url", "reloadable", false)
        );
        when(schemaService.getFieldDescriptors("service_kind", "mdgw")).thenReturn(svcDescriptors);

        var venueDescriptors = List.of(
                Map.<String, Object>of("path", "/secret_ref", "secret_ref", true, "reloadable", false)
        );
        when(schemaService.getFieldDescriptors("venue_capability", "oanda/rtmd")).thenReturn(venueDescriptors);

        var result = locator.resolveFieldDescriptors("mdgw_oanda_1", "MDGW");
        assertThat(result).hasSize(3);
        assertThat(result.get(0).get("path")).isEqualTo("/grpc_port");
        assertThat(result.get(2).get("path")).isEqualTo("/secret_ref");
    }

    // ── Venue-backed: no venue found → service_kind only (with warning) ──

    @SuppressWarnings("unchecked")
    @Test
    void resolveFieldDescriptors_gw_returns_service_kind_only_when_no_venue() {
        when(jdbc.query(contains("cfg.gateway_instance"), any(ResultSetExtractor.class), eq("gw_sim_1")))
                .thenReturn(null);

        var svcDescriptors = List.of(Map.<String, Object>of("path", "/grpc_port", "reloadable", false));
        when(schemaService.getFieldDescriptors("service_kind", "gw")).thenReturn(svcDescriptors);

        var result = locator.resolveFieldDescriptors("gw_sim_1", "GW");
        assertThat(result).isEqualTo(svcDescriptors);
    }

    // ── Venue-backed: venue found but no venue_capability descriptors → service_kind only ──

    @SuppressWarnings("unchecked")
    @Test
    void resolveFieldDescriptors_gw_returns_service_kind_when_venue_capability_empty() {
        when(jdbc.query(contains("cfg.gateway_instance"), any(ResultSetExtractor.class), eq("gw_ibkr_1")))
                .thenReturn("ibkr");

        when(schemaService.getFieldDescriptors("venue_capability", "ibkr/gw")).thenReturn(List.of());

        var svcDescriptors = List.of(Map.<String, Object>of("path", "/grpc_port", "reloadable", false));
        when(schemaService.getFieldDescriptors("service_kind", "gw")).thenReturn(svcDescriptors);

        var result = locator.resolveFieldDescriptors("gw_ibkr_1", "GW");
        assertThat(result).isEqualTo(svcDescriptors);
    }

    // ── isVenueBacked ─────────────────────────────────────────────────────

    @Test
    void isVenueBacked_returns_true_for_venue_types() {
        assertThat(locator.isVenueBacked("GW")).isTrue();
        assertThat(locator.isVenueBacked("MDGW")).isTrue();
        assertThat(locator.isVenueBacked("gw")).isTrue(); // case insensitive
    }

    @Test
    void isVenueBacked_returns_false_for_non_venue_types() {
        assertThat(locator.isVenueBacked("OMS")).isFalse();
        assertThat(locator.isVenueBacked("ENGINE")).isFalse();
        assertThat(locator.isVenueBacked("REFDATA")).isFalse(); // REFDATA not Pilot-managed
    }
}
