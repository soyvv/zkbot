package com.zkbot.pilot.config;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;

class JsonPointerHelperTest {

    private final ObjectMapper mapper = new ObjectMapper();

    // ── readPointer ───────────────────────────────────────────────────────

    @Test
    void readPointer_returns_value_at_top_level() throws Exception {
        JsonNode root = mapper.readTree("{\"key\":\"value\"}");
        JsonNode result = JsonPointerHelper.readPointer(root, "/key");
        assertThat(result.asText()).isEqualTo("value");
    }

    @Test
    void readPointer_returns_value_at_nested_path() throws Exception {
        JsonNode root = mapper.readTree("{\"a\":{\"b\":{\"c\":42}}}");
        JsonNode result = JsonPointerHelper.readPointer(root, "/a/b/c");
        assertThat(result.asInt()).isEqualTo(42);
    }

    @Test
    void readPointer_returns_null_for_missing_path() throws Exception {
        JsonNode root = mapper.readTree("{\"key\":\"value\"}");
        assertThat(JsonPointerHelper.readPointer(root, "/nonexistent")).isNull();
    }

    @Test
    void readPointer_returns_null_for_null_root() {
        assertThat(JsonPointerHelper.readPointer(null, "/key")).isNull();
    }

    // ── removePointer ─────────────────────────────────────────────────────

    @Test
    void removePointer_removes_top_level_field() throws Exception {
        JsonNode root = mapper.readTree("{\"a\":1,\"b\":2}");
        JsonNode removed = JsonPointerHelper.removePointer(root, "/a");
        assertThat(removed.asInt()).isEqualTo(1);
        assertThat(root.has("a")).isFalse();
        assertThat(root.has("b")).isTrue();
    }

    @Test
    void removePointer_removes_nested_field() throws Exception {
        JsonNode root = mapper.readTree("{\"venue_config\":{\"secret_ref\":\"okx/main\",\"url\":\"https://x\"}}");
        JsonNode removed = JsonPointerHelper.removePointer(root, "/venue_config/secret_ref");
        assertThat(removed.asText()).isEqualTo("okx/main");
        assertThat(root.at("/venue_config/secret_ref").isMissingNode()).isTrue();
        assertThat(root.at("/venue_config/url").asText()).isEqualTo("https://x");
    }

    @Test
    void removePointer_returns_null_for_missing_path() throws Exception {
        JsonNode root = mapper.readTree("{\"a\":1}");
        assertThat(JsonPointerHelper.removePointer(root, "/nonexistent")).isNull();
    }

    // ── extractSecretRefs ─────────────────────────────────────────────────

    @Test
    void extractSecretRefs_finds_secret_ref_fields() throws Exception {
        String json = "{\"secret_ref\":\"oanda/main\",\"passphrase_ref\":\"oanda/main\",\"url\":\"https://x\"}";
        List<Map<String, Object>> descriptors = List.of(
                Map.of("path", "/secret_ref", "secret_ref", true),
                Map.of("path", "/passphrase_ref", "secret_ref", true),
                Map.of("path", "/url", "reloadable", false)
        );

        var refs = JsonPointerHelper.extractSecretRefs(json, descriptors, mapper);
        assertThat(refs).hasSize(2);
        assertThat(refs.get(0).fieldKey()).isEqualTo("secret_ref");
        assertThat(refs.get(0).logicalRef()).isEqualTo("oanda/main");
        assertThat(refs.get(1).fieldKey()).isEqualTo("passphrase_ref");
    }

    @Test
    void extractSecretRefs_handles_nested_secret_ref() throws Exception {
        String json = "{\"venue_config\":{\"secret_ref\":\"okx/trading\"}}";
        List<Map<String, Object>> descriptors = List.of(
                Map.of("path", "/venue_config/secret_ref", "secret_ref", true)
        );

        var refs = JsonPointerHelper.extractSecretRefs(json, descriptors, mapper);
        assertThat(refs).hasSize(1);
        assertThat(refs.get(0).fieldKey()).isEqualTo("secret_ref");
        assertThat(refs.get(0).logicalRef()).isEqualTo("okx/trading");
    }

    @Test
    void extractSecretRefs_skips_blank_values() throws Exception {
        String json = "{\"secret_ref\":\"\"}";
        List<Map<String, Object>> descriptors = List.of(
                Map.of("path", "/secret_ref", "secret_ref", true)
        );

        var refs = JsonPointerHelper.extractSecretRefs(json, descriptors, mapper);
        assertThat(refs).isEmpty();
    }

    @Test
    void extractSecretRefs_returns_empty_for_no_descriptors() {
        var refs = JsonPointerHelper.extractSecretRefs("{\"a\":1}", List.of(), mapper);
        assertThat(refs).isEmpty();
    }

    // ── removeSecretFieldsForDrift ────────────────────────────────────────

    @Test
    void removeSecretFieldsForDrift_removes_secret_and_resolved_secret_fields() throws Exception {
        JsonNode desired = mapper.readTree("{\"secret_ref\":\"oanda/main\",\"url\":\"https://x\",\"api_key\":\"resolved\"}");
        JsonNode effective = mapper.readTree("{\"secret_ref\":\"oanda/main\",\"url\":\"https://x\",\"api_key\":\"resolved\"}");

        List<Map<String, Object>> descriptors = List.of(
                Map.of("path", "/secret_ref", "secret_ref", true),
                Map.of("path", "/api_key", "resolved_secret", true),
                Map.of("path", "/url", "reloadable", false)
        );

        boolean secretRefChanged = JsonPointerHelper.removeSecretFieldsForDrift(desired, effective, descriptors);
        assertThat(secretRefChanged).isFalse();
        assertThat(desired.has("secret_ref")).isFalse();
        assertThat(desired.has("api_key")).isFalse();
        assertThat(desired.has("url")).isTrue();
    }

    @Test
    void removeSecretFieldsForDrift_detects_secret_ref_change() throws Exception {
        JsonNode desired = mapper.readTree("{\"secret_ref\":\"oanda/backup\",\"url\":\"https://x\"}");
        JsonNode effective = mapper.readTree("{\"secret_ref\":\"oanda/main\",\"url\":\"https://x\"}");

        List<Map<String, Object>> descriptors = List.of(
                Map.of("path", "/secret_ref", "secret_ref", true)
        );

        boolean secretRefChanged = JsonPointerHelper.removeSecretFieldsForDrift(desired, effective, descriptors);
        assertThat(secretRefChanged).isTrue();
    }

    // ── classifyDrift ─────────────────────────────────────────────────────

    @Test
    void classifyDrift_returns_no_diff_for_empty_changes() {
        assertThat(JsonPointerHelper.classifyDrift(List.of(), List.of())).isEqualTo("NO_DIFF");
    }

    @Test
    void classifyDrift_returns_reloadable_when_all_changed_fields_are_reloadable() {
        var changed = List.of(new JsonPointerHelper.ChangedFieldEntry("/risk_check_enabled", "true", "false", ""));
        var descriptors = List.of(Map.<String, Object>of("path", "/risk_check_enabled", "reloadable", true));

        assertThat(JsonPointerHelper.classifyDrift(changed, descriptors)).isEqualTo("RELOADABLE");
    }

    @Test
    void classifyDrift_returns_restart_required_when_any_field_is_not_reloadable() {
        var changed = List.of(
                new JsonPointerHelper.ChangedFieldEntry("/risk_check_enabled", "true", "false", ""),
                new JsonPointerHelper.ChangedFieldEntry("/nats_url", "a", "b", "")
        );
        var descriptors = List.of(
                Map.<String, Object>of("path", "/risk_check_enabled", "reloadable", true),
                Map.<String, Object>of("path", "/nats_url", "reloadable", false)
        );

        assertThat(JsonPointerHelper.classifyDrift(changed, descriptors)).isEqualTo("RESTART_REQUIRED");
    }

    @Test
    void classifyDrift_returns_restart_required_for_unknown_fields() {
        var changed = List.of(new JsonPointerHelper.ChangedFieldEntry("/unknown_field", "a", "b", ""));
        var descriptors = List.of(Map.<String, Object>of("path", "/risk_check_enabled", "reloadable", true));

        assertThat(JsonPointerHelper.classifyDrift(changed, descriptors)).isEqualTo("RESTART_REQUIRED");
    }
}
