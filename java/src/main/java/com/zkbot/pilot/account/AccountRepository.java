package com.zkbot.pilot.account;

import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Repository;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

@Repository
public class AccountRepository {

    private final JdbcTemplate jdbc;

    public AccountRepository(JdbcTemplate jdbc) {
        this.jdbc = jdbc;
    }

    public void createAccount(long accountId, String exchAccountId, String venue,
                               String brokerType, String accountType, String baseCurrency) {
        jdbc.update("""
                INSERT INTO cfg.account (account_id, exch_account_id, venue, broker_type, account_type, base_currency)
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT (account_id) DO UPDATE
                  SET exch_account_id = EXCLUDED.exch_account_id,
                      venue = EXCLUDED.venue,
                      broker_type = EXCLUDED.broker_type,
                      account_type = EXCLUDED.account_type,
                      base_currency = EXCLUDED.base_currency
                """, accountId, exchAccountId, venue, brokerType, accountType, baseCurrency);
    }

    public List<Map<String, Object>> listAccounts(String venue, String omsId, String status, int limit) {
        var params = new ArrayList<>();
        var sb = new StringBuilder("SELECT a.* FROM cfg.account a");

        if (omsId != null && !omsId.isBlank()) {
            sb.append(" JOIN cfg.account_binding ab ON a.account_id = ab.account_id");
            sb.append(" WHERE ab.oms_id = ?");
            params.add(omsId);
        } else {
            sb.append(" WHERE 1=1");
        }

        if (venue != null && !venue.isBlank()) {
            sb.append(" AND a.venue = ?");
            params.add(venue);
        }
        if (status != null && !status.isBlank()) {
            sb.append(" AND a.status = ?");
            params.add(status);
        }

        sb.append(" ORDER BY a.account_id LIMIT ?");
        params.add(limit);

        return jdbc.queryForList(sb.toString(), params.toArray());
    }

    public Map<String, Object> getAccount(long accountId) {
        var rows = jdbc.queryForList("SELECT * FROM cfg.account WHERE account_id = ?", accountId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public void updateAccount(long accountId, String status) {
        if (status == null) return;
        jdbc.update("UPDATE cfg.account SET status = ?, updated_at = now() WHERE account_id = ?",
                status, accountId);
    }

    public Map<String, Object> getAccountBinding(long accountId) {
        var rows = jdbc.queryForList("""
                SELECT ab.*, oi.oms_id AS oms_logical_id, gi.gw_id AS gw_logical_id
                FROM cfg.account_binding ab
                LEFT JOIN cfg.oms_instance oi ON ab.oms_id = oi.oms_id
                LEFT JOIN cfg.gateway_instance gi ON ab.gw_id = gi.gw_id
                WHERE ab.account_id = ?
                """, accountId);
        return rows.isEmpty() ? null : rows.getFirst();
    }

    public List<Map<String, Object>> listTradesForAccount(long accountId, int limit) {
        return jdbc.queryForList(
                "SELECT * FROM trd.trade_oms WHERE account_id = ? ORDER BY filled_ts DESC LIMIT ?",
                accountId, limit);
    }

    public List<Map<String, Object>> listActivitiesForAccount(long accountId, int limit) {
        return jdbc.queryForList("""
                (SELECT 'trade' AS activity_type, filled_ts AS ts, order_id, instrument_id, side,
                        filled_qty AS qty, filled_price AS price, NULL::text AS asset, NULL::numeric AS balance_change
                 FROM trd.trade_oms WHERE account_id = ? ORDER BY filled_ts DESC LIMIT ?)
                UNION ALL
                (SELECT 'balance' AS activity_type, snapshot_ts AS ts, NULL::bigint AS order_id, NULL::text AS instrument_id,
                        NULL::text AS side, NULL::numeric AS qty, NULL::numeric AS price, asset, total_qty AS balance_change
                 FROM trd.balance_snapshot WHERE account_id = ? ORDER BY snapshot_ts DESC LIMIT ?)
                ORDER BY ts DESC LIMIT ?
                """, accountId, limit, accountId, limit, limit);
    }
}
