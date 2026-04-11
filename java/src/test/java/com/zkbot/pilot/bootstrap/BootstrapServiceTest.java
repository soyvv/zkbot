package com.zkbot.pilot.bootstrap;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.zkbot.pilot.config.DesiredConfigRepository;
import com.zkbot.pilot.config.PilotProperties;
import com.zkbot.pilot.schema.ConfigSchemaLocator;
import io.nats.client.Connection;
import io.nats.client.Dispatcher;
import io.nats.client.Message;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.extension.ExtendWith;
import org.mockito.ArgumentCaptor;
import org.mockito.Mock;
import org.mockito.junit.jupiter.MockitoExtension;
import zk.pilot.v1.Bootstrap.BootstrapDeregisterRequest;
import zk.pilot.v1.Bootstrap.BootstrapDeregisterResponse;
import zk.pilot.v1.Bootstrap.BootstrapRegisterRequest;
import zk.pilot.v1.Bootstrap.BootstrapRegisterResponse;

import java.util.List;
import java.util.Map;

import static org.assertj.core.api.Assertions.assertThat;
import static org.mockito.ArgumentMatchers.*;
import static org.mockito.Mockito.*;

@ExtendWith(MockitoExtension.class)
class BootstrapServiceTest {

    @Mock Connection natsConnection;
    @Mock BootstrapRepository repository;
    @Mock TokenService tokenService;
    @Mock KvReconciler kvReconciler;
    @Mock DesiredConfigRepository desiredConfigRepo;
    @Mock ConfigSchemaLocator schemaLocator;
    @Mock Dispatcher dispatcher;

    PilotProperties props;
    BootstrapService service;

    @BeforeEach
    void setUp() {
        props = new PilotProperties("nats://localhost:4222", "test", "pilot_test", 60, null, null, "zk-engine-svc", "logs");
        service = new BootstrapService(natsConnection, repository, tokenService, kvReconciler,
                props, desiredConfigRepo, schemaLocator, new ObjectMapper());
    }

    // ── Register: accept ──────────────────────────────────────────────────

    @Test
    void register_accepts_valid_token_and_records_session() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "valid-token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("valid-token", "oms_1", "OMS", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("oms_1", "OMS")).thenReturn(null);
        when(desiredConfigRepo.getDesiredConfig("oms_1", "OMS"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig("{\"nats_url\":\"nats://localhost\"}", 1, "abc"));

        invokeHandleRegister(msg);

        verify(repository).recordSession(anyString(), eq("oms_1"), eq("OMS"),
                eq("svc.oms.oms_1"), eq("lock.oms.oms_1"), eq(30000));
        verify(repository).recordAudit(eq("jti-1"), eq("oms_1"), eq("OMS"),
                anyString(), eq("ACCEPTED"), anyString());

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("OK");
        assertThat(response.getKvKey()).isEqualTo("svc.oms.oms_1");
        assertThat(response.getRuntimeConfig()).isEqualTo("{\"nats_url\":\"nats://localhost\"}");
        assertThat(response.getConfigMetadata().getConfigVersion()).isEqualTo("1");
        assertThat(response.getConfigMetadata().getConfigSource()).isEqualTo("bootstrap");
        assertThat(response.getConfigMetadata().getIssuedAtMs()).isGreaterThan(0);
        assertThat(response.getConfigMetadata().getLoadedAtMs()).isEqualTo(0);
    }

    // ── Register: reject expired token ────────────────────────────────────

    @Test
    void register_rejects_expired_token() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "bad-token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("bad-token", "oms_1", "OMS", "test")).thenReturn(null);

        invokeHandleRegister(msg);

        verify(repository, never()).recordSession(any(), any(), any(), any(), any(), anyInt());
        verify(repository).recordAudit(isNull(), eq("oms_1"), eq("OMS"),
                isNull(), eq("REJECTED"), anyString());

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("TOKEN_EXPIRED");
    }

    // ── Register: duplicate with live KV ──────────────────────────────────

    @Test
    void register_rejects_duplicate_active_session_when_kv_is_live() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "oms_1", "OMS", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("oms_1", "OMS")).thenReturn(
                Map.of("owner_session_id", "existing-sess", "kv_key", "svc.oms.oms_1", "instance_type", "OMS"));
        when(kvReconciler.isKvLive("svc.oms.oms_1")).thenReturn(true);

        invokeHandleRegister(msg);

        verify(repository, never()).fenceSession(any());
        verify(repository, never()).recordSession(any(), any(), any(), any(), any(), anyInt());

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("DUPLICATE");
    }

    // ── Register: fence stale + accept ────────────────────────────────────

    @Test
    void register_fences_stale_session_and_accepts_when_kv_is_dead() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "oms_1", "OMS", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("oms_1", "OMS")).thenReturn(
                Map.of("owner_session_id", "stale-sess", "kv_key", "svc.oms.oms_1", "instance_type", "OMS"));
        when(kvReconciler.isKvLive("svc.oms.oms_1")).thenReturn(false);
        when(desiredConfigRepo.getDesiredConfig("oms_1", "OMS"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig("{}", 1, "abc"));

        invokeHandleRegister(msg);

        verify(repository).fenceSession("stale-sess");
        verify(repository).recordSession(anyString(), eq("oms_1"), eq("OMS"),
                anyString(), anyString(), eq(30000));

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("OK");
    }

    // ── Register: ENGINE gets instance ID ─────────────────────────────────

    @Test
    void register_acquires_instance_id_for_engine_type() throws Exception {
        var req = registerRequest("engine_1", "ENGINE", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "engine_1", "ENGINE", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("engine_1", "ENGINE")).thenReturn(null);
        when(repository.acquireInstanceId("test", "engine_1", 60)).thenReturn(42);
        when(desiredConfigRepo.getDesiredConfig("engine_1", "ENGINE"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig("{}", 1, "abc"));

        invokeHandleRegister(msg);

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("OK");
        assertThat(response.getInstanceId()).isEqualTo(42);
    }

    @Test
    void register_skips_instance_id_for_non_engine_type() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "oms_1", "OMS", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("oms_1", "OMS")).thenReturn(null);
        when(desiredConfigRepo.getDesiredConfig("oms_1", "OMS"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig("{}", 1, "abc"));

        invokeHandleRegister(msg);

        verify(repository, never()).acquireInstanceId(any(), any(), anyInt());

        var response = captureRegisterResponse(msg);
        assertThat(response.getInstanceId()).isEqualTo(0);
    }

    // ── Register: stale ENGINE session releases instance ID ───────────────

    @Test
    void register_releases_instance_id_when_fencing_stale_engine() throws Exception {
        var req = registerRequest("engine_1", "ENGINE", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "engine_1", "ENGINE", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("engine_1", "ENGINE")).thenReturn(
                Map.of("owner_session_id", "stale-sess", "kv_key", "svc.engine.engine_1", "instance_type", "ENGINE"));
        when(kvReconciler.isKvLive("svc.engine.engine_1")).thenReturn(false);
        when(repository.getEnvForLogical("engine_1")).thenReturn("test");
        when(repository.acquireInstanceId("test", "engine_1", 60)).thenReturn(5);

        invokeHandleRegister(msg);

        verify(repository).releaseInstanceId("test", "engine_1");
        verify(repository).fenceSession("stale-sess");
    }

    // ── Deregister ────────────────────────────────────────────────────────

    @Test
    void deregister_marks_session_and_releases_engine_instance_id() throws Exception {
        var req = BootstrapDeregisterRequest.newBuilder()
                .setOwnerSessionId("sess-1")
                .build();
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(repository.getSession("sess-1")).thenReturn(
                Map.of("logical_id", "engine_1", "instance_type", "ENGINE"));
        when(repository.getEnvForLogical("engine_1")).thenReturn("test");

        invokeHandleDeregister(msg);

        verify(repository).deregisterSession("sess-1");
        verify(repository).releaseInstanceId("test", "engine_1");
    }

    @Test
    void deregister_skips_instance_id_release_for_non_engine() throws Exception {
        var req = BootstrapDeregisterRequest.newBuilder()
                .setOwnerSessionId("sess-1")
                .build();
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(repository.getSession("sess-1")).thenReturn(
                Map.of("logical_id", "oms_1", "instance_type", "OMS"));

        invokeHandleDeregister(msg);

        verify(repository).deregisterSession("sess-1");
        verify(repository, never()).releaseInstanceId(any(), any());
    }

    // ── Null reply-to ─────────────────────────────────────────────────────

    @Test
    void register_handles_null_reply_to_without_throwing() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "bad-token");
        var msg = mockMessage(req.toByteArray(), null);

        when(tokenService.validate("bad-token", "oms_1", "OMS", "test")).thenReturn(null);

        // Should not throw
        invokeHandleRegister(msg);

        verify(natsConnection, never()).publish(any(), any(byte[].class));
    }

    // ── Register: missing desired config → ERROR (no legacy fallback) ────

    @Test
    void register_returns_error_when_desired_config_missing() throws Exception {
        var req = registerRequest("oms_1", "OMS", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "oms_1", "OMS", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("oms_1", "OMS")).thenReturn(null);
        when(desiredConfigRepo.getDesiredConfig("oms_1", "OMS")).thenReturn(null);

        invokeHandleRegister(msg);

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("ERROR");
        assertThat(response.getErrorMessage()).contains("No desired config found");
    }

    @Test
    void register_returns_error_when_desired_config_has_null_json() throws Exception {
        var req = registerRequest("gw_1", "GW", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "gw_1", "GW", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("gw_1", "GW")).thenReturn(null);
        when(desiredConfigRepo.getDesiredConfig("gw_1", "GW"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig(null, 0, null));

        invokeHandleRegister(msg);

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("ERROR");
        assertThat(response.getErrorMessage()).contains("No desired config found");
    }

    // ── Register: secret_refs extracted from config ──────────────────────

    @Test
    void register_extracts_secret_refs_from_venue_config() throws Exception {
        var req = registerRequest("gw_okx_1", "GW", "test", "token");
        var msg = mockMessage(req.toByteArray(), "reply-subject");

        when(tokenService.validate("token", "gw_okx_1", "GW", "test")).thenReturn("jti-1");
        when(repository.findSessionForLogical("gw_okx_1", "GW")).thenReturn(null);

        String venueConfig = """
                {"venue":"okx","venue_config":{"secret_ref":"okx/main","api_base_url":"https://example.com"}}
                """;
        when(desiredConfigRepo.getDesiredConfig("gw_okx_1", "GW"))
                .thenReturn(new DesiredConfigRepository.DesiredConfig(venueConfig, 1, "abc"));

        // Schema locator returns venue_capability descriptors with secret_ref fields
        when(schemaLocator.resolveFieldDescriptors("gw_okx_1", "GW")).thenReturn(List.of(
                Map.of("path", "/venue_config/secret_ref", "secret_ref", true, "reloadable", false),
                Map.of("path", "/venue_config/api_base_url", "reloadable", false)
        ));

        invokeHandleRegister(msg);

        var response = captureRegisterResponse(msg);
        assertThat(response.getStatus()).isEqualTo("OK");
        assertThat(response.getSecretRefsList()).hasSize(1);
        assertThat(response.getSecretRefsList().get(0).getLogicalRef()).isEqualTo("okx/main");
        assertThat(response.getSecretRefsList().get(0).getFieldKey()).isEqualTo("secret_ref");
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    private BootstrapRegisterRequest registerRequest(String logicalId, String instanceType,
                                                       String env, String token) {
        return BootstrapRegisterRequest.newBuilder()
                .setLogicalId(logicalId)
                .setInstanceType(instanceType)
                .setEnv(env)
                .setToken(token)
                .build();
    }

    private Message mockMessage(byte[] data, String replyTo) {
        Message msg = mock(Message.class);
        when(msg.getData()).thenReturn(data);
        when(msg.getReplyTo()).thenReturn(replyTo);
        return msg;
    }

    /**
     * Invoke the private handleRegister method via reflection.
     */
    private void invokeHandleRegister(Message msg) throws Exception {
        var method = BootstrapService.class.getDeclaredMethod("handleRegister", Message.class);
        method.setAccessible(true);
        method.invoke(service, msg);
    }

    private void invokeHandleDeregister(Message msg) throws Exception {
        var method = BootstrapService.class.getDeclaredMethod("handleDeregister", Message.class);
        method.setAccessible(true);
        method.invoke(service, msg);
    }

    private BootstrapRegisterResponse captureRegisterResponse(Message msg) throws Exception {
        var replyTo = msg.getReplyTo();
        if (replyTo == null) {
            throw new AssertionError("No reply-to on message");
        }
        ArgumentCaptor<byte[]> captor = ArgumentCaptor.forClass(byte[].class);
        verify(natsConnection).publish(eq(replyTo), captor.capture());
        return BootstrapRegisterResponse.parseFrom(captor.getValue());
    }
}
