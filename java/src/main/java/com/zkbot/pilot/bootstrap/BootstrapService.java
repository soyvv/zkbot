package com.zkbot.pilot.bootstrap;

import com.zkbot.pilot.config.PilotProperties;
import io.nats.client.Connection;
import io.nats.client.Dispatcher;
import io.nats.client.Message;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.context.SmartLifecycle;
import org.springframework.stereotype.Component;
import zk.pilot.v1.Bootstrap.BootstrapDeregisterRequest;
import zk.pilot.v1.Bootstrap.BootstrapDeregisterResponse;
import zk.pilot.v1.Bootstrap.BootstrapRegisterRequest;
import zk.pilot.v1.Bootstrap.BootstrapRegisterResponse;

import java.util.Map;
import java.util.UUID;

@Component
public class BootstrapService implements SmartLifecycle {

    private static final Logger log = LoggerFactory.getLogger(BootstrapService.class);
    private static final int LEASE_TTL_MS = 30_000;

    private final Connection natsConnection;
    private final BootstrapRepository repository;
    private final TokenService tokenService;
    private final KvReconciler kvReconciler;
    private final PilotProperties props;

    private volatile boolean running;
    private Dispatcher dispatcher;

    public BootstrapService(Connection natsConnection,
                            BootstrapRepository repository,
                            TokenService tokenService,
                            KvReconciler kvReconciler,
                            PilotProperties props) {
        this.natsConnection = natsConnection;
        this.repository = repository;
        this.tokenService = tokenService;
        this.kvReconciler = kvReconciler;
        this.props = props;
    }

    @Override
    public int getPhase() {
        return 20;
    }

    @Override
    public void start() {
        try {
            kvReconciler.waitReady();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            log.error("Interrupted waiting for KvReconciler");
            return;
        }

        dispatcher = natsConnection.createDispatcher();
        dispatcher.subscribe("zk.bootstrap.register", this::handleRegister);
        dispatcher.subscribe("zk.bootstrap.deregister", this::handleDeregister);

        running = true;
        log.info("BootstrapService started, subscribed to zk.bootstrap.register and zk.bootstrap.deregister");
    }

    @Override
    public void stop() {
        running = false;
        if (dispatcher != null) {
            natsConnection.closeDispatcher(dispatcher);
        }
    }

    @Override
    public boolean isRunning() {
        return running;
    }

    private void handleRegister(Message msg) {
        try {
            BootstrapRegisterRequest req = BootstrapRegisterRequest.parseFrom(msg.getData());

            log.info("bootstrap: register request logical_id='{}' instance_type='{}' env='{}'",
                    req.getLogicalId(), req.getInstanceType(), req.getEnv());

            // 1. Validate token
            String jti = tokenService.validate(
                    req.getToken(), req.getLogicalId(), req.getInstanceType(), req.getEnv());
            if (jti == null) {
                repository.recordAudit(null, req.getLogicalId(), req.getInstanceType(),
                        null, "REJECTED", "Token invalid, expired, or does not match");
                reply(msg, BootstrapRegisterResponse.newBuilder()
                        .setStatus("TOKEN_EXPIRED")
                        .setErrorMessage("Token invalid, expired, or does not match "
                                + "logical_id / instance_type / env")
                        .build());
                return;
            }

            // 2. Duplicate check: DB + KV liveness
            Map<String, Object> session = repository.findSessionForLogical(
                    req.getLogicalId(), req.getInstanceType());
            if (session != null) {
                String existingKvKey = (String) session.get("kv_key");
                if (kvReconciler.isKvLive(existingKvKey)) {
                    repository.recordAudit(jti, req.getLogicalId(), req.getInstanceType(),
                            (String) session.get("owner_session_id"), "DUPLICATE",
                            "Active session still live in KV");
                    reply(msg, BootstrapRegisterResponse.newBuilder()
                            .setStatus("DUPLICATE")
                            .setErrorMessage("Active session still live in KV: "
                                    + session.get("owner_session_id"))
                            .build());
                    return;
                } else {
                    // DB says active but KV says gone -- fence stale row and continue
                    String staleSessionId = (String) session.get("owner_session_id");
                    repository.fenceSession(staleSessionId);
                    repository.recordAudit(jti, req.getLogicalId(), req.getInstanceType(),
                            staleSessionId, "FENCED", "KV key absent, fencing stale session");
                    log.info("bootstrap: fenced stale session '{}' for '{}' (KV key absent)",
                            staleSessionId, req.getLogicalId());

                    String staleInstanceType = (String) session.get("instance_type");
                    if ("ENGINE".equalsIgnoreCase(staleInstanceType)) {
                        String env = repository.getEnvForLogical(req.getLogicalId());
                        if (env != null) {
                            repository.releaseInstanceId(env, req.getLogicalId());
                        }
                    }
                }
            }

            // 3. Assign instance_id for engine instances
            int instanceId = 0;
            if ("ENGINE".equalsIgnoreCase(req.getInstanceType())) {
                Integer assigned = repository.acquireInstanceId(
                        req.getEnv(), req.getLogicalId(), props.leaseTtlMinutes());
                if (assigned == null) {
                    reply(msg, BootstrapRegisterResponse.newBuilder()
                            .setStatus("NO_INSTANCE_ID_AVAILABLE")
                            .setErrorMessage("All Snowflake worker IDs (0-1023) are leased")
                            .build());
                    return;
                }
                instanceId = assigned;
            }

            // 4. Build grant
            String sessionId = UUID.randomUUID().toString();
            String kvKey = "svc." + req.getInstanceType().toLowerCase() + "." + req.getLogicalId();
            String lockKey = "lock." + req.getInstanceType().toLowerCase() + "." + req.getLogicalId();

            // 5. Record session
            repository.recordSession(
                    sessionId, req.getLogicalId(), req.getInstanceType(),
                    kvKey, lockKey, LEASE_TTL_MS);

            repository.recordAudit(jti, req.getLogicalId(), req.getInstanceType(),
                    sessionId, "ACCEPTED", "Registered successfully");
            log.info("bootstrap: registered logical_id='{}' session={} kv_key='{}' instance_id={}",
                    req.getLogicalId(), sessionId, kvKey, instanceId);

            reply(msg, BootstrapRegisterResponse.newBuilder()
                    .setOwnerSessionId(sessionId)
                    .setKvKey(kvKey)
                    .setLockKey(lockKey)
                    .setLeaseTtlMs(LEASE_TTL_MS)
                    .setInstanceId(instanceId)
                    .setScopedCredential("")
                    .setStatus("OK")
                    .build());

        } catch (Exception e) {
            log.error("bootstrap: error handling register request", e);
            try {
                reply(msg, BootstrapRegisterResponse.newBuilder()
                        .setStatus("ERROR")
                        .setErrorMessage(e.getMessage())
                        .build());
            } catch (Exception ignored) {
            }
        }
    }

    private void handleDeregister(Message msg) {
        try {
            BootstrapDeregisterRequest req = BootstrapDeregisterRequest.parseFrom(msg.getData());

            log.info("bootstrap: deregister session='{}'", req.getOwnerSessionId());

            // Look up session metadata
            Map<String, Object> session = repository.getSession(req.getOwnerSessionId());
            repository.deregisterSession(req.getOwnerSessionId());

            if (session != null && "ENGINE".equalsIgnoreCase((String) session.get("instance_type"))) {
                String logicalId = (String) session.get("logical_id");
                String env = repository.getEnvForLogical(logicalId);
                if (env != null) {
                    repository.releaseInstanceId(env, logicalId);
                }
            }

            reply(msg, BootstrapDeregisterResponse.newBuilder()
                    .setSuccess(true)
                    .build());

        } catch (Exception e) {
            log.error("bootstrap: error handling deregister request", e);
        }
    }

    private void reply(Message msg, com.google.protobuf.MessageLite response) {
        String replyTo = msg.getReplyTo();
        if (replyTo == null) {
            log.warn("bootstrap: no reply-to on message, dropping response");
            return;
        }
        natsConnection.publish(replyTo, response.toByteArray());
    }
}
