package com.zkbot.pilot.support;

import org.springframework.jdbc.core.JdbcTemplate;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.sql.Timestamp;
import java.time.Instant;
import java.time.temporal.ChronoUnit;
import java.util.HexFormat;

/**
 * Shared SQL insert helpers for test fixtures.
 * Every method inserts exactly one row with sensible defaults.
 */
public final class TestFixtures {

    private TestFixtures() {}

    // --- cfg.logical_instance ---

    public static void insertLogicalInstance(JdbcTemplate jdbc, String logicalId,
                                              String instanceType, String env) {
        jdbc.update("""
                INSERT INTO cfg.logical_instance (logical_id, instance_type, env)
                VALUES (?, ?, ?)
                ON CONFLICT (logical_id) DO NOTHING
                """, logicalId, instanceType, env);
    }

    public static void insertLogicalInstance(JdbcTemplate jdbc, String logicalId,
                                              String instanceType, String env, boolean enabled) {
        jdbc.update("""
                INSERT INTO cfg.logical_instance (logical_id, instance_type, env, enabled)
                VALUES (?, ?, ?, ?)
                ON CONFLICT (logical_id) DO UPDATE SET enabled = EXCLUDED.enabled
                """, logicalId, instanceType, env, enabled);
    }

    // --- cfg.logical_binding ---

    public static void insertBinding(JdbcTemplate jdbc, String srcType, String srcId,
                                      String dstType, String dstId) {
        jdbc.update("""
                INSERT INTO cfg.logical_binding (src_type, src_id, dst_type, dst_id, enabled)
                VALUES (?, ?, ?, ?, true)
                """, srcType, srcId, dstType, dstId);
    }

    public static void insertBinding(JdbcTemplate jdbc, String srcType, String srcId,
                                      String dstType, String dstId, boolean enabled) {
        jdbc.update("""
                INSERT INTO cfg.logical_binding (src_type, src_id, dst_type, dst_id, enabled)
                VALUES (?, ?, ?, ?, ?)
                """, srcType, srcId, dstType, dstId, enabled);
    }

    // --- cfg.oms_instance ---

    public static void insertOmsInstance(JdbcTemplate jdbc, String omsId) {
        jdbc.update("""
                INSERT INTO cfg.oms_instance (oms_id, namespace)
                VALUES (?, 'test')
                ON CONFLICT (oms_id) DO NOTHING
                """, omsId);
    }

    // --- cfg.gateway_instance ---

    public static void insertGatewayInstance(JdbcTemplate jdbc, String gwId, String venue) {
        jdbc.update("""
                INSERT INTO cfg.gateway_instance (gw_id, venue, broker_type, account_type)
                VALUES (?, ?, 'SIM', 'SPOT')
                ON CONFLICT (gw_id) DO NOTHING
                """, gwId, venue);
    }

    // --- cfg.account ---

    public static void insertAccount(JdbcTemplate jdbc, long accountId, String venue) {
        jdbc.update("""
                INSERT INTO cfg.account (account_id, exch_account_id, venue, broker_type, account_type, base_currency)
                VALUES (?, ?, ?, 'SIM', 'SPOT', 'USDT')
                ON CONFLICT (account_id) DO NOTHING
                """, accountId, "exch_" + accountId, venue);
    }

    // --- cfg.account_binding ---

    public static void insertAccountBinding(JdbcTemplate jdbc, long accountId,
                                              String omsId, String gwId) {
        jdbc.update("""
                INSERT INTO cfg.account_binding (account_id, oms_id, gw_id)
                VALUES (?, ?, ?)
                ON CONFLICT (account_id, oms_id, gw_id) DO NOTHING
                """, accountId, omsId, gwId);
    }

    // --- cfg.instance_token ---

    public static void insertInstanceToken(JdbcTemplate jdbc, String tokenJti,
                                             String logicalId, String instanceType,
                                             String plaintextToken) {
        insertInstanceToken(jdbc, tokenJti, logicalId, instanceType, plaintextToken, true);
    }

    public static void insertInstanceToken(JdbcTemplate jdbc, String tokenJti,
                                             String logicalId, String instanceType,
                                             String plaintextToken, boolean active) {
        String hash = sha256Hex(plaintextToken);
        Instant expiresAt = Instant.now().plus(365, ChronoUnit.DAYS);
        jdbc.update("""
                INSERT INTO cfg.instance_token (token_jti, logical_id, instance_type, token_hash, status, expires_at)
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT (token_jti) DO NOTHING
                """, tokenJti, logicalId, instanceType, hash,
                active ? "active" : "revoked", Timestamp.from(expiresAt));
    }

    public static void insertExpiredInstanceToken(JdbcTemplate jdbc, String tokenJti,
                                                    String logicalId, String instanceType,
                                                    String plaintextToken) {
        String hash = sha256Hex(plaintextToken);
        Instant expiresAt = Instant.now().minus(1, ChronoUnit.DAYS);
        jdbc.update("""
                INSERT INTO cfg.instance_token (token_jti, logical_id, instance_type, token_hash, status, expires_at)
                VALUES (?, ?, ?, ?, 'active', ?)
                ON CONFLICT (token_jti) DO NOTHING
                """, tokenJti, logicalId, instanceType, hash, Timestamp.from(expiresAt));
    }

    // --- mon.active_session ---

    public static void insertActiveSession(JdbcTemplate jdbc, String sessionId,
                                             String logicalId, String instanceType) {
        String kvKey = "svc." + instanceType.toLowerCase() + "." + logicalId;
        String lockKey = "lock." + instanceType.toLowerCase() + "." + logicalId;
        Instant expiresAt = Instant.now().plus(1, ChronoUnit.HOURS);
        jdbc.update("""
                INSERT INTO mon.active_session
                  (owner_session_id, logical_id, instance_type, kv_key, lock_key, lease_ttl_ms, status, expires_at)
                VALUES (?, ?, ?, ?, ?, 30000, 'active', ?)
                ON CONFLICT (owner_session_id) DO NOTHING
                """, sessionId, logicalId, instanceType, kvKey, lockKey, Timestamp.from(expiresAt));
    }

    public static void insertActiveSession(JdbcTemplate jdbc, String sessionId,
                                             String logicalId, String instanceType, String status) {
        String kvKey = "svc." + instanceType.toLowerCase() + "." + logicalId;
        String lockKey = "lock." + instanceType.toLowerCase() + "." + logicalId;
        Instant expiresAt = Instant.now().plus(1, ChronoUnit.HOURS);
        jdbc.update("""
                INSERT INTO mon.active_session
                  (owner_session_id, logical_id, instance_type, kv_key, lock_key, lease_ttl_ms, status, expires_at)
                VALUES (?, ?, ?, ?, ?, 30000, ?, ?)
                ON CONFLICT (owner_session_id) DO NOTHING
                """, sessionId, logicalId, instanceType, kvKey, lockKey, status, Timestamp.from(expiresAt));
    }

    // --- cfg.strategy_definition ---

    public static void insertStrategy(JdbcTemplate jdbc, String strategyId) {
        jdbc.update("""
                INSERT INTO cfg.strategy_definition (strategy_id, runtime_type, code_ref, description)
                VALUES (?, 'RUST', 'test_strategy', 'test strategy')
                ON CONFLICT (strategy_id) DO NOTHING
                """, strategyId);
    }

    // --- cfg.strategy_instance ---

    public static void insertExecution(JdbcTemplate jdbc, String executionId,
                                         String strategyId, String omsId, String status) {
        jdbc.update("""
                INSERT INTO cfg.strategy_instance (execution_id, strategy_id, target_oms_id, status, started_at)
                VALUES (?, ?, ?, ?, now())
                """, executionId, strategyId, omsId, status);
    }

    // --- cfg.instrument_refdata ---

    public static void insertInstrument(JdbcTemplate jdbc, String instrumentId, String venue) {
        jdbc.update("""
                INSERT INTO cfg.instrument_refdata
                  (instrument_id, instrument_exch, venue, instrument_type,
                   base_asset, quote_asset, price_tick_size, qty_lot_size, price_precision, qty_precision,
                   min_order_qty, max_order_qty)
                VALUES (?, ?, ?, 'SPOT', 'BTC', 'USDT', 0.01, 0.00001, 2, 5, 0.00001, 100.0)
                ON CONFLICT (instrument_id) DO NOTHING
                """, instrumentId, instrumentId, venue);
    }

    // --- trd.trade_oms ---

    public static void insertTrade(JdbcTemplate jdbc, long accountId, String omsId,
                                     String instrumentId) {
        jdbc.update("""
                INSERT INTO trd.trade_oms
                  (order_id, account_id, oms_id, instrument_id, side, filled_qty, filled_price, filled_ts)
                VALUES (1, ?, ?, ?, 'BUY', 1.0, 50000.0, now())
                """, accountId, omsId, instrumentId);
    }

    // --- trd.balance_snapshot ---

    public static void insertBalanceSnapshot(JdbcTemplate jdbc, long accountId) {
        jdbc.update("""
                INSERT INTO trd.balance_snapshot
                  (account_id, asset, total_qty, avail_qty, frozen_qty, snapshot_ts)
                VALUES (?, 'USDT', 10000.0, 9000.0, 1000.0, now())
                """, accountId);
    }

    // --- cfg.refdata_venue_instance ---

    public static void insertRefdataVenueInstance(JdbcTemplate jdbc, String logicalId,
                                                    String env, String venue) {
        jdbc.update("""
                INSERT INTO cfg.refdata_venue_instance (logical_id, env, venue)
                VALUES (?, ?, ?)
                ON CONFLICT (logical_id) DO NOTHING
                """, logicalId, env, venue);
    }

    // --- helpers ---

    private static String sha256Hex(String input) {
        try {
            MessageDigest md = MessageDigest.getInstance("SHA-256");
            byte[] digest = md.digest(input.getBytes(StandardCharsets.UTF_8));
            return HexFormat.of().formatHex(digest);
        } catch (NoSuchAlgorithmException e) {
            throw new RuntimeException(e);
        }
    }
}
