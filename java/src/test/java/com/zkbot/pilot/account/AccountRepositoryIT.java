package com.zkbot.pilot.account;

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
@Import(AccountRepository.class)
class AccountRepositoryIT extends PostgresTestBase {

    @Autowired
    private JdbcTemplate jdbc;

    private AccountRepository repo;

    @BeforeEach
    void setUp() {
        repo = new AccountRepository(jdbc);
        jdbc.update("DELETE FROM trd.balance_snapshot");
        jdbc.update("DELETE FROM trd.trade_oms");
        jdbc.update("DELETE FROM cfg.account_binding");
        jdbc.update("DELETE FROM cfg.account");
        jdbc.update("DELETE FROM cfg.gateway_instance");
        jdbc.update("DELETE FROM cfg.oms_instance");
    }

    // ── Account CRUD ──────────────────────────────────────────────────────

    @Test
    void create_account_inserts_with_active_status() {
        repo.createAccount(9001, "exch_9001", "SIM", "SIM", "SPOT", "USDT");

        var row = repo.getAccount(9001);
        assertThat(row).isNotNull();
        assertThat(row.get("status")).isEqualTo("ACTIVE");
        assertThat(row.get("venue")).isEqualTo("SIM");
    }

    @Test
    void create_account_upserts_on_conflict() {
        repo.createAccount(9001, "exch_9001", "SIM", "SIM", "SPOT", "USDT");
        repo.createAccount(9001, "exch_9001_v2", "PROD", "REAL", "SPOT", "USD");

        var row = repo.getAccount(9001);
        assertThat(row.get("exch_account_id")).isEqualTo("exch_9001_v2");
        assertThat(row.get("venue")).isEqualTo("PROD");
    }

    @Test
    void get_account_returns_null_for_missing() {
        assertThat(repo.getAccount(99999)).isNull();
    }

    @Test
    void update_account_status() {
        repo.createAccount(9001, "exch_9001", "SIM", "SIM", "SPOT", "USDT");

        repo.updateAccount(9001, "DISABLED");

        var row = repo.getAccount(9001);
        assertThat(row.get("status")).isEqualTo("DISABLED");
    }

    // ── List with filters ─────────────────────────────────────────────────

    @Test
    void list_accounts_filters_by_venue() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        repo.createAccount(9002, "exch_2", "PROD", "REAL", "SPOT", "USD");

        var results = repo.listAccounts("SIM", null, null, 50);

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("account_id")).isEqualTo(9001L);
    }

    @Test
    void list_accounts_filters_by_oms_id_via_binding_join() {
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertOmsInstance(jdbc, "oms_2");
        TestFixtures.insertGatewayInstance(jdbc, "gw_1", "SIM");
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        repo.createAccount(9002, "exch_2", "SIM", "SIM", "SPOT", "USDT");
        TestFixtures.insertAccountBinding(jdbc, 9001, "oms_1", "gw_1");
        TestFixtures.insertAccountBinding(jdbc, 9002, "oms_2", "gw_1");

        var results = repo.listAccounts(null, "oms_1", null, 50);

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("account_id")).isEqualTo(9001L);
    }

    @Test
    void list_accounts_filters_by_status() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        repo.createAccount(9002, "exch_2", "SIM", "SIM", "SPOT", "USDT");
        repo.updateAccount(9002, "DISABLED");

        var results = repo.listAccounts(null, null, "ACTIVE", 50);

        assertThat(results).hasSize(1);
        assertThat(results.getFirst().get("account_id")).isEqualTo(9001L);
    }

    // ── Account binding ───────────────────────────────────────────────────

    @Test
    void get_account_binding_joins_oms_and_gw_tables() {
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertGatewayInstance(jdbc, "gw_sim", "SIM");
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        TestFixtures.insertAccountBinding(jdbc, 9001, "oms_1", "gw_sim");

        var binding = repo.getAccountBinding(9001);

        assertThat(binding).isNotNull();
        assertThat(binding.get("oms_id")).isEqualTo("oms_1");
        assertThat(binding.get("gw_id")).isEqualTo("gw_sim");
        assertThat(binding.get("oms_logical_id")).isEqualTo("oms_1");
        assertThat(binding.get("gw_logical_id")).isEqualTo("gw_sim");
    }

    @Test
    void get_account_binding_returns_null_for_unbound() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");

        assertThat(repo.getAccountBinding(9001)).isNull();
    }

    // ── Trades ────────────────────────────────────────────────────────────

    @Test
    void list_trades_for_account() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertInstrument(jdbc, "BTCUSDT_SIM", "SIM");
        TestFixtures.insertTrade(jdbc, 9001, "oms_1", "BTCUSDT_SIM");

        var trades = repo.listTradesForAccount(9001, 50);

        assertThat(trades).hasSize(1);
        assertThat(trades.getFirst().get("instrument_id")).isEqualTo("BTCUSDT_SIM");
    }

    // ── Activities (UNION query) ──────────────────────────────────────────

    @Test
    void list_activities_union_returns_trades_and_balance_snapshots() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");
        TestFixtures.insertOmsInstance(jdbc, "oms_1");
        TestFixtures.insertInstrument(jdbc, "BTCUSDT_SIM", "SIM");
        TestFixtures.insertTrade(jdbc, 9001, "oms_1", "BTCUSDT_SIM");
        TestFixtures.insertBalanceSnapshot(jdbc, 9001);

        var activities = repo.listActivitiesForAccount(9001, 50);

        assertThat(activities).hasSize(2);

        var types = activities.stream().map(a -> a.get("activity_type")).toList();
        assertThat(types).containsExactlyInAnyOrder("trade", "balance");
    }

    @Test
    void list_activities_returns_empty_for_no_data() {
        repo.createAccount(9001, "exch_1", "SIM", "SIM", "SPOT", "USDT");

        var activities = repo.listActivitiesForAccount(9001, 50);

        assertThat(activities).isEmpty();
    }
}
