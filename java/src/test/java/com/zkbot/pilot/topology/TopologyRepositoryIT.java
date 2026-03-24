package com.zkbot.pilot.topology;

import com.fasterxml.jackson.databind.ObjectMapper;
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

import static org.assertj.core.api.Assertions.assertThat;

@JdbcTest
@AutoConfigureTestDatabase(replace = AutoConfigureTestDatabase.Replace.NONE)
@ActiveProfiles("test")
@Import({TopologyRepository.class, ObjectMapper.class})
class TopologyRepositoryIT extends PostgresTestBase {

    @Autowired
    private JdbcTemplate jdbc;

    private TopologyRepository repo;

    @BeforeEach
    void setUp() {
        repo = new TopologyRepository(jdbc, new ObjectMapper());
        jdbc.update("DELETE FROM cfg.logical_binding");
        jdbc.update("DELETE FROM mon.registration_audit");
        jdbc.update("DELETE FROM mon.active_session");
        jdbc.update("DELETE FROM cfg.logical_instance");
    }

    // ── Logical instances ─────────────────────────────────────────────────

    @Test
    void list_logical_instances_filters_by_env() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_dev", "OMS", "dev");
        TestFixtures.insertLogicalInstance(jdbc, "oms_prod", "OMS", "prod");

        var results = repo.listLogicalInstances("dev", null);

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("logical_id")).isEqualTo("oms_dev");
    }

    @Test
    void list_logical_instances_filters_by_env_and_type() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_dev", "OMS", "dev");
        TestFixtures.insertLogicalInstance(jdbc, "gw_dev", "GW", "dev");

        var results = repo.listLogicalInstances("dev", "GW");

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("logical_id")).isEqualTo("gw_dev");
    }

    @Test
    void get_logical_instance_returns_instance() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");

        var result = repo.getLogicalInstance("oms_1");

        assertThat(result).isNotNull();
        assertThat(result.get("instance_type")).isEqualTo("OMS");
    }

    @Test
    void get_logical_instance_returns_null_for_missing() {
        var result = repo.getLogicalInstance("nonexistent");
        assertThat(result).isNull();
    }

    @Test
    void create_logical_instance_upserts() {
        repo.createLogicalInstance("oms_1", "OMS", "dev", null, true);

        var result = repo.getLogicalInstance("oms_1");
        assertThat(result.get("instance_type")).isEqualTo("OMS");

        // Update type via upsert
        repo.createLogicalInstance("oms_1", "GW", "dev", null, true);
        result = repo.getLogicalInstance("oms_1");
        assertThat(result.get("instance_type")).isEqualTo("GW");
    }

    // ── Bindings ──────────────────────────────────────────────────────────

    @Test
    void upsert_binding_replaces_existing_pair() {
        TestFixtures.insertLogicalInstance(jdbc, "a", "OMS", "test");
        TestFixtures.insertLogicalInstance(jdbc, "b", "GW", "test");

        repo.upsertBinding("OMS", "a", "GW", "b", true, null);

        var bindings = repo.listBindings("a", "b");
        assertThat(bindings).hasSize(1);
        assertThat(bindings.getFirst().get("enabled")).isEqualTo(true);

        // Upsert with enabled=false
        repo.upsertBinding("OMS", "a", "GW", "b", false, null);

        bindings = repo.listBindings("a", "b");
        assertThat(bindings).hasSize(1);
        assertThat(bindings.getFirst().get("enabled")).isEqualTo(false);
    }

    @Test
    void list_bindings_all() {
        TestFixtures.insertBinding(jdbc, "OMS", "a", "GW", "b");
        TestFixtures.insertBinding(jdbc, "OMS", "a", "GW", "c");

        var all = repo.listBindings(null, null);
        assertThat(all).hasSize(2);
    }

    @Test
    void list_bindings_by_src() {
        TestFixtures.insertBinding(jdbc, "OMS", "a", "GW", "b");
        TestFixtures.insertBinding(jdbc, "OMS", "x", "GW", "y");

        var filtered = repo.listBindings("a", null);
        assertThat(filtered).hasSize(1);
        assertThat(filtered.getFirst().get("dst_id")).isEqualTo("b");
    }

    @Test
    void get_bindings_for_service() {
        TestFixtures.insertBinding(jdbc, "OMS", "a", "GW", "b");
        TestFixtures.insertBinding(jdbc, "GW", "b", "ENGINE", "c");

        // "b" is both dst of first binding and src of second
        var bindings = repo.getBindingsForService("b");
        assertThat(bindings).hasSize(2);
    }

    // ── Sessions ──────────────────────────────────────────────────────────

    @Test
    void list_active_sessions_returns_only_active_status() {
        TestFixtures.insertLogicalInstance(jdbc, "oms_1", "OMS", "test");
        TestFixtures.insertLogicalInstance(jdbc, "gw_1", "GW", "test");
        TestFixtures.insertActiveSession(jdbc, "sess-1", "oms_1", "OMS", "active");
        TestFixtures.insertActiveSession(jdbc, "sess-2", "gw_1", "GW", "fenced");

        var sessions = repo.listActiveSessions();

        assertThat(sessions).hasSize(1);
        assertThat(sessions.getFirst().get("owner_session_id")).isEqualTo("sess-1");
    }

    // ── Audit ─────────────────────────────────────────────────────────────

    @Test
    void list_registration_audit_ordered_by_time_desc() {
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision, observed_at) VALUES ('a', 'OMS', 'ACCEPTED', now() - interval '2 min')");
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision, observed_at) VALUES ('a', 'OMS', 'REJECTED', now() - interval '1 min')");
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision, observed_at) VALUES ('a', 'OMS', 'FENCED', now())");

        var results = repo.listRegistrationAudit("a", 10);

        assertThat(results).hasSize(3);
        assertThat(results.get(0).get("decision")).isEqualTo("FENCED");
        assertThat(results.get(1).get("decision")).isEqualTo("REJECTED");
        assertThat(results.get(2).get("decision")).isEqualTo("ACCEPTED");
    }

    @Test
    void list_registration_audit_by_limit() {
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision) VALUES ('a', 'OMS', 'A')");
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision) VALUES ('a', 'OMS', 'B')");
        jdbc.update("INSERT INTO mon.registration_audit (logical_id, instance_type, decision) VALUES ('a', 'OMS', 'C')");

        var results = repo.listRegistrationAudit(null, 2);

        assertThat(results).hasSize(2);
    }

    @Test
    void list_reconciliation_audit() {
        jdbc.update("""
                INSERT INTO mon.recon_run
                  (recon_type, period_start, period_end, status)
                VALUES ('trade', now() - interval '1 day', now(), 'COMPLETED')
                """);

        var results = repo.listReconciliationAudit(10);

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("status")).isEqualTo("COMPLETED");
    }
}
