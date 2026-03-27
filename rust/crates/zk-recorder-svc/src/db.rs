//! Postgres operations for the recorder service.
//!
//! - `upsert_order_oms`: idempotent terminal-order upsert (ON CONFLICT DO UPDATE)
//! - `insert_trade_oms`: idempotent trade insert with fingerprint fallback dedup
//! - `try_enrich_from_source_binding`: best-effort strategy_id/execution_id lookup

use sqlx::PgPool;
use tracing::{debug, warn};

use zk_proto_rs::zk::{
    common::v1::{BasicOrderType, BuySellType, FillType, OpenCloseType},
    oms::v1::OrderStatus,
    recorder::v1::{RecorderTerminalOrder, RecorderTradeEvent},
};

// ── Enrichment ──────────────────────────────────────────────────────────────

/// Result of a source_binding lookup.
#[derive(Default)]
pub struct SourceEnrichment {
    pub strategy_id: Option<String>,
    pub execution_id: Option<String>,
}

/// Best-effort lookup of strategy_id/execution_id from bot.source_binding.
///
/// Returns defaults if the table doesn't exist yet (Phase 2) or source_id is
/// empty. Recorder persists source_id unconditionally; enrichment columns are
/// populated when the binding is available.
pub async fn try_enrich_from_source_binding(
    pool: &PgPool,
    source_id: &str,
) -> SourceEnrichment {
    if source_id.is_empty() {
        return SourceEnrichment::default();
    }

    // bot.source_binding may not exist yet — graceful fallback on any error.
    let result = sqlx::query_as::<_, (String, String)>(
        "SELECT strategy_id, execution_id FROM bot.source_binding WHERE source_id = $1",
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await;

    match result {
        Ok(Some((strategy_id, execution_id))) => {
            debug!(source_id, strategy_id, execution_id, "enrichment resolved");
            SourceEnrichment {
                strategy_id: Some(strategy_id),
                execution_id: Some(execution_id),
            }
        }
        Ok(None) => {
            debug!(source_id, "no source_binding row found — enrichment skipped");
            SourceEnrichment::default()
        }
        Err(e) => {
            // Table may not exist yet (Phase 2). Log at debug, not warn.
            debug!(source_id, error = %e, "source_binding lookup failed — enrichment skipped");
            SourceEnrichment::default()
        }
    }
}

// ── Order upsert ────────────────────────────────────────────────────────────

/// Upsert a terminal order into `trd.order_oms`.
///
/// Uses `ON CONFLICT (terminal_at, order_id) DO UPDATE` so re-delivery or
/// late enrichment updates are applied idempotently. The `recorded_at` column
/// is always bumped to `now()` on update.
pub async fn upsert_order_oms(
    pool: &PgPool,
    msg: &RecorderTerminalOrder,
    oms_id: &str,
    enrichment: &SourceEnrichment,
) -> Result<(), sqlx::Error> {
    let terminal_at = ts_ms_to_dt(msg.terminal_at_ms);
    let created_at = ts_ms_to_dt(msg.created_at_ms);
    let side = buy_sell_str(msg.side);
    let open_close = open_close_str(msg.open_close);
    let order_type = order_type_str(msg.order_type);
    let order_status = order_status_str(msg.order_status);

    let raw_snapshot = build_order_snapshot(msg);

    sqlx::query(
        r#"
        INSERT INTO trd.order_oms (
            order_id, account_id, oms_id, gw_id,
            strategy_id, execution_id, source_id,
            instrument_id, instrument_exch,
            side, open_close, order_type, order_status,
            price, qty, filled_qty, filled_avg_price,
            ext_order_ref, error_msg,
            terminal_at, created_at, raw_snapshot
        ) VALUES (
            $1, $2, $3, $4,
            $5, $6, $7,
            $8, $9,
            $10, $11, $12, $13,
            $14, $15, $16, $17,
            $18, $19,
            $20, $21, $22
        )
        ON CONFLICT (terminal_at, order_id) DO UPDATE SET
            order_status     = EXCLUDED.order_status,
            filled_qty       = EXCLUDED.filled_qty,
            filled_avg_price = EXCLUDED.filled_avg_price,
            ext_order_ref    = COALESCE(EXCLUDED.ext_order_ref, trd.order_oms.ext_order_ref),
            error_msg        = EXCLUDED.error_msg,
            strategy_id      = COALESCE(EXCLUDED.strategy_id, trd.order_oms.strategy_id),
            execution_id     = COALESCE(EXCLUDED.execution_id, trd.order_oms.execution_id),
            raw_snapshot     = EXCLUDED.raw_snapshot,
            recorded_at      = now()
        "#,
    )
    .bind(msg.order_id)                       // $1
    .bind(msg.account_id)                     // $2
    .bind(oms_id)                             // $3
    .bind(&msg.gw_key)                        // $4
    .bind(&enrichment.strategy_id)            // $5
    .bind(&enrichment.execution_id)           // $6
    .bind(&msg.source_id)                     // $7
    .bind(&msg.instrument_code)               // $8
    .bind(&msg.instrument_exch)               // $9
    .bind(side)                               // $10
    .bind(open_close)                         // $11
    .bind(order_type)                         // $12
    .bind(order_status)                       // $13
    .bind(msg.price)                          // $14
    .bind(msg.qty)                            // $15
    .bind(msg.filled_qty)                     // $16
    .bind(msg.filled_avg_price)               // $17
    .bind(&msg.ext_order_ref)                 // $18
    .bind(&msg.error_msg)                     // $19
    .bind(terminal_at)                        // $20
    .bind(created_at)                         // $21
    .bind(raw_snapshot)                       // $22
    .execute(pool)
    .await?;

    Ok(())
}

// ── Trade insert ────────────────────────────────────────────────────────────

/// Insert a trade into `trd.trade_oms`.
///
/// Dedup strategy:
/// - When `ext_trade_id` is present: existing partial unique index
///   `(account_id, ext_trade_id) WHERE ext_trade_id IS NOT NULL` prevents dupes.
/// - When `ext_trade_id` is NULL (inferred trades): fingerprint unique index
///   `(account_id, order_id, filled_qty, filled_price, filled_ts)
///    WHERE ext_trade_id IS NULL` prevents dupes on redelivery.
///
/// Both branches use `ON CONFLICT DO NOTHING` for idempotency.
pub async fn insert_trade_oms(
    pool: &PgPool,
    msg: &RecorderTradeEvent,
    oms_id: &str,
    enrichment: &SourceEnrichment,
) -> Result<(), sqlx::Error> {
    let filled_ts = ts_ms_to_dt(msg.filled_ts_ms);
    let side = buy_sell_str(msg.side);
    let open_close = open_close_str(msg.open_close);
    let fill_type = fill_type_str(msg.fill_type);

    let fee_total: f64 = msg.fees.iter().map(|f| f.fee_amount).sum();
    let fee_detail = if msg.fees.is_empty() {
        None
    } else {
        serde_json::to_value(
            msg.fees
                .iter()
                .map(|f| FeeRow {
                    symbol: f.fee_symbol.clone(),
                    amount: f.fee_amount,
                })
                .collect::<Vec<_>>(),
        )
        .ok()
    };

    let has_ext_trade_id = !msg.ext_trade_id.is_empty();

    // ext_trade_id may be empty for inferred trades — use None for PG null
    let ext_trade_id = if has_ext_trade_id {
        Some(&msg.ext_trade_id)
    } else {
        None
    };

    // Two separate queries for the two dedup paths, because ON CONFLICT
    // targets a specific index and the two partial indexes have different columns.
    if has_ext_trade_id {
        // Path A: venue trade with ext_trade_id — dedup on (account_id, ext_trade_id)
        sqlx::query(
            r#"
            INSERT INTO trd.trade_oms (
                order_id, account_id, oms_id, gw_id,
                strategy_id, execution_id, source_id,
                instrument_id, instrument_exch,
                side, open_close, fill_type, ext_trade_id,
                filled_qty, filled_price, filled_ts,
                fee_total, fee_detail
            ) VALUES (
                $1, $2, $3, $4,
                $5, $6, $7,
                $8, $9,
                $10, $11, $12, $13,
                $14, $15, $16,
                $17, $18
            )
            ON CONFLICT (account_id, ext_trade_id)
            WHERE ext_trade_id IS NOT NULL
            DO NOTHING
            "#,
        )
        .bind(msg.order_id)                   // $1
        .bind(msg.account_id)                 // $2
        .bind(oms_id)                         // $3
        .bind(&msg.gw_key)                    // $4
        .bind(&enrichment.strategy_id)        // $5
        .bind(&enrichment.execution_id)       // $6
        .bind(&msg.source_id)                 // $7
        .bind(&msg.instrument_code)           // $8
        .bind(&msg.instrument_exch)           // $9
        .bind(side)                           // $10
        .bind(open_close)                     // $11
        .bind(fill_type)                      // $12
        .bind(ext_trade_id)                   // $13
        .bind(msg.filled_qty)                 // $14
        .bind(msg.filled_price)               // $15
        .bind(filled_ts)                      // $16
        .bind(fee_total)                      // $17
        .bind(fee_detail)                     // $18
        .execute(pool)
        .await?;
    } else {
        // Path B: inferred trade — dedup on fingerprint
        // (account_id, order_id, filled_qty, filled_price, filled_ts) WHERE ext_trade_id IS NULL
        sqlx::query(
            r#"
            INSERT INTO trd.trade_oms (
                order_id, account_id, oms_id, gw_id,
                strategy_id, execution_id, source_id,
                instrument_id, instrument_exch,
                side, open_close, fill_type, ext_trade_id,
                filled_qty, filled_price, filled_ts,
                fee_total, fee_detail
            ) VALUES (
                $1, $2, $3, $4,
                $5, $6, $7,
                $8, $9,
                $10, $11, $12, NULL,
                $13, $14, $15,
                $16, $17
            )
            ON CONFLICT (account_id, order_id, filled_qty, filled_price, filled_ts)
            WHERE ext_trade_id IS NULL
            DO NOTHING
            "#,
        )
        .bind(msg.order_id)                   // $1
        .bind(msg.account_id)                 // $2
        .bind(oms_id)                         // $3
        .bind(&msg.gw_key)                    // $4
        .bind(&enrichment.strategy_id)        // $5
        .bind(&enrichment.execution_id)       // $6
        .bind(&msg.source_id)                 // $7
        .bind(&msg.instrument_code)           // $8
        .bind(&msg.instrument_exch)           // $9
        .bind(side)                           // $10
        .bind(open_close)                     // $11
        .bind(fill_type)                      // $12
        .bind(msg.filled_qty)                 // $13
        .bind(msg.filled_price)               // $14
        .bind(filled_ts)                      // $15
        .bind(fee_total)                      // $16
        .bind(fee_detail)                     // $17
        .execute(pool)
        .await?;
    }

    Ok(())
}

// ── Partition maintenance ───────────────────────────────────────────────────

/// Call the SQL procedure to ensure current + next month partitions exist.
pub async fn ensure_partitions(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT trd.ensure_order_oms_partitions()")
        .execute(pool)
        .await?;
    Ok(())
}

/// Call the SQL procedure to drop expired partitions.
pub async fn drop_expired_partitions(pool: &PgPool, months_to_keep: u32) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT trd.drop_expired_order_oms_partitions($1)")
        .bind(months_to_keep as i32)
        .execute(pool)
        .await?;
    Ok(())
}

// ── Raw snapshot builder ────────────────────────────────────────────────────

/// Build a full terminal-order snapshot as JSON for the `raw_snapshot` column.
///
/// Stores the complete proto payload: all scalar fields, all trades, inferred
/// trades, and fees — so the raw event is available for repair/debug.
fn build_order_snapshot(msg: &RecorderTerminalOrder) -> Option<serde_json::Value> {
    serde_json::to_value(OrderSnapshot {
        order_id: msg.order_id,
        account_id: msg.account_id,
        oms_id: &msg.oms_id,
        gw_key: &msg.gw_key,
        source_id: &msg.source_id,
        instrument_code: &msg.instrument_code,
        instrument_exch: &msg.instrument_exch,
        side: msg.side,
        open_close: msg.open_close,
        order_type: msg.order_type,
        order_status: msg.order_status,
        price: msg.price,
        qty: msg.qty,
        filled_qty: msg.filled_qty,
        filled_avg_price: msg.filled_avg_price,
        ext_order_ref: &msg.ext_order_ref,
        error_msg: &msg.error_msg,
        terminal_at_ms: msg.terminal_at_ms,
        created_at_ms: msg.created_at_ms,
        trades: msg
            .trades
            .iter()
            .map(|t| TradeSnapshot {
                order_id: t.order_id,
                ext_order_ref: &t.ext_order_ref,
                ext_trade_id: &t.ext_trade_id,
                filled_qty: t.filled_qty,
                filled_price: t.filled_price,
                filled_ts: t.filled_ts,
                fill_type: t.fill_type,
                fees: t.fees.iter().map(|f| FeeSnapshot { symbol: &f.fee_symbol, amount: f.fee_amount }).collect(),
            })
            .collect(),
        inferred_trades: msg
            .inferred_trades
            .iter()
            .map(|t| TradeSnapshot {
                order_id: t.order_id,
                ext_order_ref: &t.ext_order_ref,
                ext_trade_id: &t.ext_trade_id,
                filled_qty: t.filled_qty,
                filled_price: t.filled_price,
                filled_ts: t.filled_ts,
                fill_type: t.fill_type,
                fees: t.fees.iter().map(|f| FeeSnapshot { symbol: &f.fee_symbol, amount: f.fee_amount }).collect(),
            })
            .collect(),
        fees: msg
            .fees
            .iter()
            .map(|f| FeeSnapshot {
                symbol: &f.fee_symbol,
                amount: f.fee_amount,
            })
            .collect(),
    })
    .ok()
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn ts_ms_to_dt(ms: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_else(|| {
        warn!(ms, "invalid timestamp millis, falling back to now()");
        chrono::Utc::now()
    })
}

fn buy_sell_str(v: i32) -> &'static str {
    match BuySellType::try_from(v) {
        Ok(BuySellType::BsBuy) => "buy",
        Ok(BuySellType::BsSell) => "sell",
        _ => "unknown",
    }
}

fn open_close_str(v: i32) -> &'static str {
    match OpenCloseType::try_from(v) {
        Ok(OpenCloseType::OcOpen) => "open",
        Ok(OpenCloseType::OcClose) => "close",
        _ => "unknown",
    }
}

fn order_type_str(v: i32) -> &'static str {
    match BasicOrderType::try_from(v) {
        Ok(BasicOrderType::OrdertypeLimit) => "limit",
        Ok(BasicOrderType::OrdertypeMarket) => "market",
        _ => "unknown",
    }
}

fn order_status_str(v: i32) -> &'static str {
    match OrderStatus::try_from(v) {
        Ok(OrderStatus::Filled) => "filled",
        Ok(OrderStatus::Cancelled) => "cancelled",
        Ok(OrderStatus::Rejected) => "rejected",
        Ok(OrderStatus::Pending) => "pending",
        Ok(OrderStatus::Booked) => "booked",
        Ok(OrderStatus::PartiallyFilled) => "partially_filled",
        _ => "unknown",
    }
}

fn fill_type_str(v: i32) -> &'static str {
    match FillType::try_from(v) {
        Ok(FillType::FillMaker) => "maker",
        Ok(FillType::FillTaker) => "taker",
        _ => "unknown",
    }
}

// ── Snapshot structs ────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct OrderSnapshot<'a> {
    order_id: i64,
    account_id: i64,
    oms_id: &'a str,
    gw_key: &'a str,
    source_id: &'a str,
    instrument_code: &'a str,
    instrument_exch: &'a str,
    side: i32,
    open_close: i32,
    order_type: i32,
    order_status: i32,
    price: f64,
    qty: f64,
    filled_qty: f64,
    filled_avg_price: f64,
    ext_order_ref: &'a str,
    error_msg: &'a str,
    terminal_at_ms: i64,
    created_at_ms: i64,
    trades: Vec<TradeSnapshot<'a>>,
    inferred_trades: Vec<TradeSnapshot<'a>>,
    fees: Vec<FeeSnapshot<'a>>,
}

#[derive(serde::Serialize)]
struct TradeSnapshot<'a> {
    order_id: i64,
    ext_order_ref: &'a str,
    ext_trade_id: &'a str,
    filled_qty: f64,
    filled_price: f64,
    filled_ts: i64,
    fill_type: i32,
    fees: Vec<FeeSnapshot<'a>>,
}

#[derive(serde::Serialize)]
struct FeeSnapshot<'a> {
    symbol: &'a str,
    amount: f64,
}

#[derive(serde::Serialize)]
struct FeeRow {
    symbol: String,
    amount: f64,
}
