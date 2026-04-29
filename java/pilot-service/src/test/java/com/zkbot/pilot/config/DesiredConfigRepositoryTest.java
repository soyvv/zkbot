package com.zkbot.pilot.config;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import org.springframework.jdbc.core.JdbcTemplate;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.assertj.core.api.Assertions.assertThatThrownBy;
import static org.mockito.ArgumentMatchers.anyString;
import static org.mockito.ArgumentMatchers.eq;
import static org.mockito.Mockito.when;

@ExtendWith(MockitoExtension.class)
class DesiredConfigRepositoryTest {

    @Mock JdbcTemplate jdbc;

    DesiredConfigRepository repo;

    @BeforeEach
    void setUp() {
        repo = new DesiredConfigRepository(jdbc);
    }

    @Test
    void isManagedType_returns_true_for_known_types() {
        assertThat(repo.isManagedType("OMS")).isTrue();
        assertThat(repo.isManagedType("GW")).isTrue();
        assertThat(repo.isManagedType("ENGINE")).isTrue();
        assertThat(repo.isManagedType("MDGW")).isTrue();
        assertThat(repo.isManagedType("oms")).isTrue(); // case insensitive
    }

    @Test
    void isManagedType_returns_true_for_refdata() {
        assertThat(repo.isManagedType("REFDATA")).isTrue();
        assertThat(repo.isManagedType("refdata")).isTrue();
    }

    @Test
    void isManagedType_returns_false_for_unknown_types() {
        assertThat(repo.isManagedType("UNKNOWN")).isFalse();
    }

    @Test
    void getDesiredConfig_returns_null_for_unsupported_type() {
        assertThat(repo.getDesiredConfig("foo_1", "UNKNOWN")).isNull();
    }

    @Test
    void setDesiredConfig_throws_for_unsupported_type() {
        assertThatThrownBy(() -> repo.setDesiredConfig("foo_1", "UNKNOWN", "{}"))
                .isInstanceOf(IllegalArgumentException.class)
                .hasMessageContaining("unsupported instance_type");
    }

    @Test
    void getRefdataAggregate_returns_null_when_no_venue_rows() {
        when(jdbc.queryForList(anyString(), eq("prod"))).thenReturn(List.of());

        assertThat(repo.getRefdataAggregate("prod")).isNull();
    }

    @Test
    void getRefdataAggregate_builds_venues_payload_and_takes_max_version() throws Exception {
        when(jdbc.queryForList(anyString(), eq("dev"))).thenReturn(List.of(
                Map.of("venue", "ibkr", "enabled", true,
                        "config", "{\"host\":\"127.0.0.1\",\"port\":4004}",
                        "config_version", 5),
                Map.of("venue", "okx", "enabled", false,
                        "config", "{\"api_base_url\":\"https://www.okx.com\"}",
                        "config_version", 2)
        ));

        var result = repo.getRefdataAggregate("dev");
        assertThat(result).isNotNull();
        assertThat(result.configVersion()).isEqualTo(5);
        assertThat(result.configHash()).isNotBlank();

        JsonNode payload = new ObjectMapper().readTree(result.configJson());
        assertThat(payload.has("venues")).isTrue();
        JsonNode venues = payload.get("venues");
        assertThat(venues.size()).isEqualTo(2);
        assertThat(venues.get(0).get("venue").asText()).isEqualTo("ibkr");
        assertThat(venues.get(0).get("enabled").asBoolean()).isTrue();
        assertThat(venues.get(0).get("config").get("host").asText()).isEqualTo("127.0.0.1");
        assertThat(venues.get(1).get("venue").asText()).isEqualTo("okx");
        assertThat(venues.get(1).get("enabled").asBoolean()).isFalse();
    }

    @Test
    void getRefdataAggregate_hash_changes_when_config_changes() {
        when(jdbc.queryForList(anyString(), eq("dev")))
                .thenReturn(List.of(Map.of("venue", "ibkr", "enabled", true,
                        "config", "{\"host\":\"a\"}", "config_version", 1)))
                .thenReturn(List.of(Map.of("venue", "ibkr", "enabled", true,
                        "config", "{\"host\":\"b\"}", "config_version", 1)));

        var first = repo.getRefdataAggregate("dev");
        var second = repo.getRefdataAggregate("dev");
        assertThat(first.configHash()).isNotEqualTo(second.configHash());
    }

    @Test
    void getRefdataAggregate_handles_invalid_json_by_emitting_empty_object() throws Exception {
        when(jdbc.queryForList(anyString(), eq("dev"))).thenReturn(List.of(
                Map.of("venue", "ibkr", "enabled", true,
                        "config", "{not json",
                        "config_version", 1)
        ));

        var result = repo.getRefdataAggregate("dev");
        JsonNode payload = new ObjectMapper().readTree(result.configJson());
        assertThat(payload.get("venues").get(0).get("config").isObject()).isTrue();
        assertThat(payload.get("venues").get(0).get("config").size()).isEqualTo(0);
    }
}
