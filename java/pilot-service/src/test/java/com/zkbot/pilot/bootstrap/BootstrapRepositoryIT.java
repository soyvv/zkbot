package com.zkbot.pilot.bootstrap;

import com.zkbot.pilot.support.PostgresTestBase;
import com.zkbot.pilot.support.TestFixtures;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.boot.test.autoconfigure.jdbc.AutoConfigureTestDatabase;
import org.springframework.boot.test.autoconfigure.jdbc.JdbcTest;
import org.springframework.context.annotation.Import;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.test.context.ActiveProfiles;
import org.springframework.transaction.PlatformTransactionManager;

import static org.assertj.core.api.Assertions.assertThat;

@JdbcTest
@AutoConfigureTestDatabase(replace = AutoConfigureTestDatabase.Replace.NONE)
@ActiveProfiles("test")
@Import(BootstrapRepository.class)
class BootstrapRepositoryIT extends PostgresTestBase {

    @Autowired
    private JdbcTemplate jdbc;

    @Autowired
    private PlatformTransactionManager txManager;

    private BootstrapRepository repo;

    @BeforeEach
    void setUp() {
        repo = new BootstrapRepository(jdbc, txManager);
        // Clean state between tests
        jdbc.update("DELETE FROM mon.registration_audit");
        jdbc.update("DELETE FROM mon.active_session");
        jdbc.update("DELETE FROM cfg.instance_id_lease");
        jdbc.update("DELETE FROM cfg.instance_token");
        jdbc.update("DELETE FROM cfg.logical_instance");
    }

    // ── Token validation ──────────────────────────────────────────────────

    @Test
    void validate_token_returns_jti_for_valid_active_token() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertInstanceToken(jdbc, "jti-1", "oms_1", "OMS", "test-token-abc");

        String jti = repo.validateToken("test-token-abc", "oms_1", "OMS", "test");

        assertThat(jti).isEqualTo("jti-1");
    }

    @Test
    void validate_token_returns_null_for_expired_token() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertExpiredInstanceToken(jdbc, "jti-exp", "oms_1", "OMS", "expired-token");

        String jti = repo.validateToken("expired-token", "oms_1", "OMS", "test");

        assertThat(jti).isNull();
    }

    @Test
    void validate_token_returns_null_for_wrong_logical_id() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertLogicalInstance(jdbc, "oms_2", "OMS", "test");
        TestFixtures.insertInstanceToken(jdbc, "jti-1", "oms_1", "OMS", "my-token");

        String jti = repo.validateToken("my-token", "oms_2", "OMS", "test");

        assertThat(jti).isNull();
    }

    @Test
    void validate_token_returns_null_for_wrong_env() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertInstanceToken(jdbc, "jti-1", "oms_1", "OMS", "my-token");

        // Token matches oms_1 in env=test, but we query env=prod
        String jti = repo.validateToken("my-token", "oms_1", "OMS", "prod");

        assertThat(jti).isNull();
    }

    @Test
    void validate_token_returns_null_for_revoked_token() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertInstanceToken(jdbc, "jti-1", "oms_1", "OMS", "my-token", false);

        String jti = repo.validateToken("my-token", "oms_1", "OMS", "test");

        assertThat(jti).isNull();
    }

    @Test
    void validate_token_returns_null_for_wrong_hash() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertInstanceToken(jdbc, "jti-1", "oms_1", "OMS", "correct-token");

        String jti = repo.validateToken("wrong-token", "oms_1", "OMS", "test");

        assertThat(jti).isNull();
    }

    // ── Session management ────────────────────────────────────────────────

    @Test
    void record_session_inserts_active_session() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");

        repo.recordSession("sess-1", "oms_1", "OMS", "svc.oms.oms_1", "lock.oms.oms_1", 30000);

        var rows = jdbc.queryForList(
                "SELECT * FROM mon.active_session WHERE owner_session_id = 'sess-1'");
        assertThat(rows).hasSize(1);
        assertThat(rows.getFirst().get("status")).isEqualTo("active");
        assertThat(rows.getFirst().get("logical_id")).isEqualTo("oms_1");
    }

    @Test
    void record_session_is_idempotent_on_conflict() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");

        repo.recordSession("sess-1", "oms_1", "OMS", "svc.oms.oms_1", "lock.oms.oms_1", 30000);
        repo.recordSession("sess-1", "oms_1", "OMS", "svc.oms.oms_1", "lock.oms.oms_1", 30000);

        var count = jdbc.queryForObject(
                "SELECT count(*) FROM mon.active_session WHERE owner_session_id = 'sess-1'",
                Integer.class);
        assertThat(count).isEqualTo(1);
    }

    @Test
    void find_session_for_logical_returns_active_session() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS");

        var session = repo.findSessionForLogical("oms_1", "OMS");

        assertThat(session).isNotNull();
        assertThat(session.get("owner_session_id")).isEqualTo("sess-1");
    }

    @Test
    void find_session_for_logical_returns_null_when_no_active() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS", "fenced");

        var session = repo.findSessionForLogical("oms_1", "OMS");

        assertThat(session).isNull();
    }

    @Test
    void fence_session_sets_status_fenced() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS");

        repo.fenceSession("sess-1");

        var status = jdbc.queryForObject(
                "SELECT status FROM mon.active_session WHERE owner_session_id = 'sess-1'",
                String.class);
        assertThat(status).isEqualTo("fenced");
    }

    @Test
    void deregister_session_sets_status_deregistered() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS");

        repo.deregisterSession("sess-1");

        var status = jdbc.queryForObject(
                "SELECT status FROM mon.active_session WHERE owner_session_id = 'sess-1'",
                String.class);
        assertThat(status).isEqualTo("deregistered");
    }

    @Test
    void get_session_returns_metadata() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS");

        var session = repo.getSession("sess-1");

        assertThat(session).isNotNull();
        assertThat(session.get("logical_id")).isEqualTo("oms_1");
        assertThat(session.get("instance_type")).isEqualTo("OMS");
    }

    @Test
    void get_session_returns_null_for_missing() {
        var session = repo.getSession("nonexistent");
        assertThat(session).isNull();
    }

    // ── Instance ID lease ─────────────────────────────────────────────────

    @Test
    void acquire_instance_id_returns_0_when_pool_empty() {
        TestFixtures.insertLogicalInstance(jdbc, "engine_1", "ENGINE", "test");

        Integer id = repo.acquireInstanceId("test", "engine_1", 60);

        assertThat(id).isEqualTo(0);
    }

    @Test
    void acquire_instance_id_returns_lowest_available() {
        TestFixtures.insertLogicalInstance(jdbc, "engine_1", "ENGINE", "test");
        TestFixtures.insertLogicalInstance(jdbc, "engine_2", "ENGINE", "test");

        // Lease IDs 0 and 1 for engine_1
        jdbc.update("""
                INSERT INTO cfg.instance_id_lease (env, instance_id, logical_id, leased_until)
                VALUES ('test', 0, 'engine_1', now() + interval '1 hour'),
                       ('test', 1, 'engine_1', now() + interval '1 hour')
                """);

        Integer id = repo.acquireInstanceId("test", "engine_2", 60);

        assertThat(id).isEqualTo(2);
    }

    @Test
    void release_instance_id_deletes_lease() {
        TestFixtures.insertLogicalInstance(jdbc, "engine_1", "ENGINE", "test");

        repo.acquireInstanceId("test", "engine_1", 60);

        var countBefore = jdbc.queryForObject(
                "SELECT count(*) FROM cfg.instance_id_lease WHERE env = 'test' AND logical_id = 'engine_1'",
                Integer.class);
        assertThat(countBefore).isEqualTo(1);

        repo.releaseInstanceId("test", "engine_1");

        var countAfter = jdbc.queryForObject(
                "SELECT count(*) FROM cfg.instance_id_lease WHERE env = 'test' AND logical_id = 'engine_1'",
                Integer.class);
        assertThat(countAfter).isEqualTo(0);
    }

    @Test
    void find_active_session_by_kv_key() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS");

        var session = repo.findActiveSessionByKvKey("svc.oms.oms_1");

        assertThat(session).isNotNull();
        assertThat(session.get("owner_session_id")).isEqualTo("sess-1");
    }

    @Test
    void find_active_session_by_kv_key_returns_null_for_fenced() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS", "fenced");

        var session = repo.findActiveSessionByKvKey("svc.oms.oms_1");

        assertThat(session).isNull();
    }

    @Test
    void get_env_for_logical() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");

        String env = repo.getEnvForLogical("oms_1");

        assertThat(env).isEqualTo("test");
    }

    // ── Registration audit ────────────────────────────────────────────────

    @Test
    void record_audit_inserts_registration_audit() {
        repo.recordAudit("jti-1", "oms_1", "OMS", "sess-1", "ACCEPTED", "valid token");

        var rows = jdbc.queryForList(
                "SELECT * FROM mon.registration_audit WHERE logical_id = 'oms_1'");
        assertThat(rows).hasSize(1);
        assertThat(rows.getFirst().get("decision")).isEqualTo("ACCEPTED");
        assertThat(rows.getFirst().get("reason")).isEqualTo("valid token");
    }
}
