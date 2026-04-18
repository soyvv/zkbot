package com.zkbot.pilot.schema;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.web.server.ResponseStatusException;

import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;
import static org.mockito.Mockito.when;

@ExtendWith(MockitoExtension.class)
class MergedConfigSchemaTest {

    @Mock SchemaRepository repository;

    SchemaService service;
    ObjectMapper objectMapper = new ObjectMapper();

    static final String GW_SVC_SCHEMA = """
            {
              "title": "Gateway Service Config",
              "type": "object",
              "properties": {
                "gw_id": { "type": "string" },
                "venue": { "type": "string" },
                "grpc_port": { "type": "integer" },
                "venue_config": { "type": "object" }
              },
              "required": ["gw_id"]
            }
            """;

    static final String SIMULATOR_SCHEMA = """
            {
              "title": "Simulator Gateway Config",
              "type": "object",
              "properties": {
                "account_id": { "type": "integer", "minimum": 1 },
                "mock_balances": { "type": "string" },
                "fill_delay_ms": { "type": "integer" },
                "match_policy": { "type": "string", "enum": ["immediate", "fcfs"] }
              },
              "required": ["account_id"]
            }
            """;

    static final String OKX_SCHEMA = """
            {
              "title": "OKX Gateway Config",
              "type": "object",
              "properties": {
                "api_base_url": { "type": "string" },
                "secret_ref": { "type": "string", "x-zk-secret-ref": true }
              },
              "required": ["secret_ref"]
            }
            """;

    static final String IBKR_SCHEMA = """
            {
              "title": "IBKR Gateway Config",
              "type": "object",
              "properties": {
                "mode": { "type": "string", "enum": ["paper", "live"] },
                "host": { "type": "string" },
                "port": { "type": "integer" }
              },
              "required": ["mode"]
            }
            """;

    @BeforeEach
    void setUp() {
        service = new SchemaService(repository, objectMapper);
    }

    private void stubSchema(String resourceType, String resourceKey, String configSchema) {
        when(repository.findActive(resourceType, resourceKey))
                .thenReturn(Map.of("config_schema", configSchema));
    }

    // ── gw + simulator: venue fields merged at top level ──────────────────

    @Test
    void gw_simulator_merges_at_top_level() throws Exception {
        stubSchema("service_kind", "gw", GW_SVC_SCHEMA);
        stubSchema("venue_capability", "simulator/gw", SIMULATOR_SCHEMA);

        String merged = service.getMergedConfigSchema("gw", "simulator");
        JsonNode root = objectMapper.readTree(merged);

        // Common fields present at top level
        assertThat(root.at("/properties/gw_id/type").asText()).isEqualTo("string");
        assertThat(root.at("/properties/grpc_port/type").asText()).isEqualTo("integer");

        // Simulator fields merged at top level (not nested under venue_config)
        assertThat(root.at("/properties/account_id/type").asText()).isEqualTo("integer");
        assertThat(root.at("/properties/mock_balances/type").asText()).isEqualTo("string");
        assertThat(root.at("/properties/fill_delay_ms/type").asText()).isEqualTo("integer");
        assertThat(root.at("/properties/match_policy/enum")).isNotEmpty();

        // Title reflects merge
        assertThat(root.get("title").asText()).contains("Simulator");
    }

    // ── gw + okx: venue fields nested under venue_config ──────────────────

    @Test
    void gw_okx_nests_under_venue_config() throws Exception {
        stubSchema("service_kind", "gw", GW_SVC_SCHEMA);
        stubSchema("venue_capability", "okx/gw", OKX_SCHEMA);

        String merged = service.getMergedConfigSchema("gw", "okx");
        JsonNode root = objectMapper.readTree(merged);

        // Common fields at top level
        assertThat(root.at("/properties/gw_id/type").asText()).isEqualTo("string");
        assertThat(root.at("/properties/grpc_port/type").asText()).isEqualTo("integer");

        // OKX fields nested under venue_config as a sub-schema
        JsonNode venueConfig = root.at("/properties/venue_config");
        assertThat(venueConfig.get("title").asText()).isEqualTo("OKX Gateway Config");
        assertThat(venueConfig.at("/properties/api_base_url/type").asText()).isEqualTo("string");
        assertThat(venueConfig.at("/properties/secret_ref/x-zk-secret-ref").asBoolean()).isTrue();
        assertThat(venueConfig.at("/properties/passphrase_ref").isMissingNode()).isTrue();

        // OKX fields NOT at top level
        assertThat(root.at("/properties/api_base_url").isMissingNode()).isTrue();
        assertThat(root.at("/properties/secret_ref").isMissingNode()).isTrue();
    }

    // ── gw + ibkr: venue fields nested under venue_config ─────────────────

    @Test
    void gw_ibkr_nests_under_venue_config() throws Exception {
        stubSchema("service_kind", "gw", GW_SVC_SCHEMA);
        stubSchema("venue_capability", "ibkr/gw", IBKR_SCHEMA);

        String merged = service.getMergedConfigSchema("gw", "ibkr");
        JsonNode root = objectMapper.readTree(merged);

        // Common at top level
        assertThat(root.at("/properties/gw_id/type").asText()).isEqualTo("string");

        // IBKR nested under venue_config
        JsonNode venueConfig = root.at("/properties/venue_config");
        assertThat(venueConfig.get("title").asText()).isEqualTo("IBKR Gateway Config");
        assertThat(venueConfig.at("/properties/mode/enum")).isNotEmpty();
        assertThat(venueConfig.at("/properties/host/type").asText()).isEqualTo("string");

        // IBKR fields NOT at top level
        assertThat(root.at("/properties/mode").isMissingNode()).isTrue();
    }

    // ── missing venue capability schema: explicit error ───────────────────

    @Test
    void missing_venue_capability_throws_not_found() {
        stubSchema("service_kind", "gw", GW_SVC_SCHEMA);
        // No venue_capability for "binance/gw"

        assertThatThrownBy(() -> service.getMergedConfigSchema("gw", "binance"))
                .isInstanceOf(ResponseStatusException.class)
                .hasMessageContaining("No config schema for venue=binance capability=gw");
    }

    // ── non-venue-backed kind: explicit error ─────────────────────────────

    @Test
    void non_venue_backed_kind_throws_bad_request() {
        assertThatThrownBy(() -> service.getMergedConfigSchema("oms", "simulator"))
                .isInstanceOf(ResponseStatusException.class)
                .hasMessageContaining("not venue-backed");
    }

    // ── missing service-kind schema: explicit error ───────────────────────

    @Test
    void missing_service_kind_schema_throws_not_found() {
        // No service_kind for "gw"

        assertThatThrownBy(() -> service.getMergedConfigSchema("gw", "okx"))
                .isInstanceOf(ResponseStatusException.class)
                .hasMessageContaining("No config schema for service kind: gw");
    }
}
