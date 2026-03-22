package com.zkbot.pilot.bootstrap;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;
import org.springframework.transaction.PlatformTransactionManager;
import org.springframework.transaction.TransactionDefinition;
import org.springframework.transaction.support.TransactionTemplate;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.sql.Timestamp;
import java.time.Instant;
import java.util.HexFormat;
import java.util.Map;

@Repository
public class BootstrapRepository {

    private static final Logger log = LoggerFactory.getLogger(BootstrapRepository.class);

    private final JdbcTemplate jdbc;
    private final TransactionTemplate serializableTx;

    public BootstrapRepository(JdbcTemplate jdbc, PlatformTransactionManager txManager) {
        this.jdbc = jdbc;
        this.serializableTx = new TransactionTemplate(txManager);
        this.serializableTx.setIsolationLevel(TransactionDefinition.ISOLATION_SERIALIZABLE);
    }

    // ── Token validation ─────────────────────────────────────────────────────

    /**
     * Validate token against DB. Returns tokenJti if valid, null otherwise.
     */
    public String validateToken(String token, String logicalId, String instanceType, String env) {
        String hash = sha256Hex(token);
        return jdbc.query("""
                SELECT t.token_jti
                FROM cfg.instance_token t
                JOIN cfg.logical_instance li ON li.logical_id = t.logical_id
                WHERE t.token_hash = ?
                  AND t.status = 'active'
                  AND t.expires_at > now()
                  AND t.logical_id = ?
                  AND t.instance_type = ?
                  AND li.env = ?
                """,
                rs -> rs.next() ? rs.getString("token_jti") : null,
                hash, logicalId, instanceType, env);
    }

    // ── Session management ───────────────────────────────────────────────────

    /**
     * Find the active session for (logicalId, instanceType), or null.
     */
    public Map<String, Object> findSessionForLogical(String logicalId, String instanceType) {
        var rows = jdbc.queryForList("""
                SELECT owner_session_id, kv_key, instance_type
                FROM mon.active_session
                WHERE logical_id = ? AND instance_type = ? AND status = 'active'
                LIMIT 1
                """, logicalId, instanceType);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Return session metadata for deregister/lease-release lookups.
     */
    public Map<String, Object> getSession(String ownerSessionId) {
        var rows = jdbc.queryForList(
                "SELECT logical_id, instance_type FROM mon.active_session WHERE owner_session_id = ?",
                ownerSessionId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Record a new session. ON CONFLICT DO NOTHING.
     */
    public void recordSession(String sessionId, String logicalId, String instanceType,
                              String kvKey, String lockKey, int leaseTtlMs) {
        Instant expiresAt = Instant.now().plusMillis(leaseTtlMs);
        jdbc.update("""
                INSERT INTO mon.active_session
                  (owner_session_id, logical_id, instance_type, kv_key, lock_key,
                   lease_ttl_ms, status, expires_at)
                VALUES (?, ?, ?, ?, ?, ?, 'active', ?)
                ON CONFLICT (owner_session_id) DO NOTHING
                """,
                sessionId, logicalId, instanceType, kvKey, lockKey,
                leaseTtlMs, Timestamp.from(expiresAt));
    }

    /**
     * Mark session as deregistered.
     */
    public void deregisterSession(String ownerSessionId) {
        jdbc.update("""
                UPDATE mon.active_session
                SET status = 'deregistered', last_seen_at = now()
                WHERE owner_session_id = ?
                """, ownerSessionId);
    }

    /**
     * Mark session as fenced (stale writer, superseded).
     */
    public void fenceSession(String ownerSessionId) {
        jdbc.update(
                "UPDATE mon.active_session SET status = 'fenced', last_seen_at = now() "
                + "WHERE owner_session_id = ?",
                ownerSessionId);
    }

    // ── Instance ID lease ────────────────────────────────────────────────────

    /**
     * Acquire the lowest available Snowflake worker ID (0-1023) in a SERIALIZABLE tx.
     * Returns instance_id or null if all taken.
     */
    public Integer acquireInstanceId(String env, String logicalId, int leaseTtlMinutes) {
        int maxRetries = 3;
        for (int attempt = 1; attempt <= maxRetries; attempt++) {
            try {
                return doAcquireInstanceId(env, logicalId, leaseTtlMinutes);
            } catch (org.springframework.dao.CannotSerializeTransactionException e) {
                if (attempt == maxRetries) throw e;
                log.warn("Serialization failure on acquireInstanceId attempt {}/{}, retrying", attempt, maxRetries);
            }
        }
        return null; // unreachable
    }

    private Integer doAcquireInstanceId(String env, String logicalId, int leaseTtlMinutes) {
        return serializableTx.execute(status -> {
            // Delete expired leases for this logical_id
            jdbc.update("""
                    DELETE FROM cfg.instance_id_lease
                    WHERE env = ? AND logical_id = ? AND leased_until < now()
                    """, env, logicalId);

            // Find the lowest available instance_id
            var rows = jdbc.queryForList("""
                    SELECT s.id AS instance_id
                    FROM generate_series(0, 1023) AS s(id)
                    WHERE NOT EXISTS (
                        SELECT 1 FROM cfg.instance_id_lease
                        WHERE env = ? AND instance_id = s.id AND leased_until >= now()
                    )
                    ORDER BY s.id
                    LIMIT 1
                    """, env);

            if (rows.isEmpty()) {
                return null;
            }

            int instanceId = ((Number) rows.getFirst().get("instance_id")).intValue();

            jdbc.update("""
                    INSERT INTO cfg.instance_id_lease (env, instance_id, logical_id, leased_until)
                    VALUES (?, ?, ?, now() + ? * interval '1 minute')
                    ON CONFLICT (env, instance_id)
                    DO UPDATE SET logical_id = EXCLUDED.logical_id,
                                  leased_until = EXCLUDED.leased_until
                    """, env, instanceId, logicalId, leaseTtlMinutes);

            return instanceId;
        });
    }

    /**
     * Release instance_id lease for the given env + logical_id.
     */
    public void releaseInstanceId(String env, String logicalId) {
        jdbc.update("""
                DELETE FROM cfg.instance_id_lease
                WHERE env = ? AND logical_id = ?
                """, env, logicalId);
    }

    /**
     * Find active session by kv_key. Used by KvReconciler when a key is lost.
     */
    public Map<String, Object> findActiveSessionByKvKey(String kvKey) {
        var rows = jdbc.queryForList(
                "SELECT owner_session_id, logical_id, instance_type "
                + "FROM mon.active_session WHERE kv_key = ? AND status = 'active'",
                kvKey);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    /**
     * Get env for a logical_id from cfg.logical_instance.
     */
    public String getEnvForLogical(String logicalId) {
        return jdbc.query(
                "SELECT env FROM cfg.logical_instance WHERE logical_id = ?",
                rs -> rs.next() ? rs.getString("env") : null,
                logicalId);
    }

    // ── Registration audit ─────────────────────────────────────────────────

    public void recordAudit(String tokenJti, String logicalId, String instanceType,
                             String sessionId, String decision, String reason) {
        jdbc.update("""
                INSERT INTO mon.registration_audit
                  (token_jti, logical_id, instance_type, owner_session_id, decision, reason)
                VALUES (?, ?, ?, ?, ?, ?)
                """, tokenJti, logicalId, instanceType, sessionId, decision, reason);
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    static String sha256Hex(String input) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            byte[] digest = md.digest(input.getBytes(StandardCharsets.UTF_8));
            return HexFormat.of().formatHex(digest);
        } catch (NoSuchAlgorithmException e) {
            throw new RuntimeException("SHA-256 not available", e);
        }
    }
}
