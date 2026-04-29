//! PostgreSQL loaders for `ConfdataManager` data.
//!
//! Reads from the `cfg.*` tables defined in the schema at
//! `devops/init/postgres/00_schema.sql`.
//!
//! Uses dynamic `sqlx::query()` (not the `query!` macro) so the crate can be
//! compiled without a live `DATABASE_URL` env var.
//!
//! # Tables queried
//! ```text
//! cfg.oms_instance       (oms_id TEXT, ...)
//! cfg.account_binding    (account_id BIGINT, oms_id TEXT, gw_id TEXT)
//! cfg.account            (account_id BIGINT, exch_account_id TEXT, ...)
//! cfg.gateway_instance   (gw_id TEXT, venue TEXT, rpc_endpoint TEXT, ...)
//! cfg.instrument_refdata (instrument_id TEXT, instrument_exch TEXT, venue TEXT,
//!                         instrument_type TEXT, disabled BOOL, contract_size NUMERIC)
//! cfg.instrument_trading_config  (optional — empty vec if table absent)
//! ```

use sqlx::{PgPool, Row};
use tracing::info;

use zk_oms_rs::config::InstrumentTradingConfig;
use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::common::v1::{InstrumentRefData, InstrumentType},
};

// ── Public loader ─────────────────────────────────────────────────────────────

pub struct RawConfigData {
    pub oms_config: OmsConfigEntry,
    pub account_routes: Vec<OmsRouteEntry>,
    pub gw_configs: Vec<GwConfigEntry>,
    pub refdata: Vec<InstrumentRefData>,
    pub trading_configs: Vec<InstrumentTradingConfig>,
}

/// Load all config data needed to build a `ConfdataManager` for `oms_id`.
pub async fn load_config(pool: &PgPool, oms_id: &str) -> Result<RawConfigData, sqlx::Error> {
    info!(oms_id, "loading ConfdataManager from PostgreSQL");

    let oms_config = load_oms_config(pool, oms_id).await?;
    let account_routes = load_account_routes(pool, oms_id).await?;
    let gw_configs = load_gw_configs(pool).await?;
    let refdata = load_refdata(pool).await?;
    let trading_configs = load_trading_configs(pool).await?;

    Ok(RawConfigData {
        oms_config,
        account_routes,
        gw_configs,
        refdata,
        trading_configs,
    })
}

// ── Loaders ───────────────────────────────────────────────────────────────────

async fn load_oms_config(pool: &PgPool, oms_id: &str) -> Result<OmsConfigEntry, sqlx::Error> {
    // Verify OMS instance exists.
    let _row = sqlx::query("SELECT oms_id FROM cfg.oms_instance WHERE oms_id = $1")
        .bind(oms_id)
        .fetch_one(pool)
        .await?;

    // Derive managed_account_ids from account_binding.
    let id_rows = sqlx::query("SELECT account_id FROM cfg.account_binding WHERE oms_id = $1")
        .bind(oms_id)
        .fetch_all(pool)
        .await?;

    let managed_account_ids: Vec<i64> = id_rows
        .iter()
        .map(|r| r.try_get::<i64, _>("account_id").unwrap_or(0))
        .collect();

    Ok(OmsConfigEntry {
        oms_id: oms_id.to_string(),
        managed_account_ids,
        ..Default::default()
    })
}

async fn load_account_routes(
    pool: &PgPool,
    oms_id: &str,
) -> Result<Vec<OmsRouteEntry>, sqlx::Error> {
    // JOIN with cfg.account to get exch_account_id.
    let rows = sqlx::query(
        "SELECT ab.account_id, ab.gw_id, a.exch_account_id \
         FROM cfg.account_binding ab \
         JOIN cfg.account a ON a.account_id = ab.account_id \
         WHERE ab.oms_id = $1",
    )
    .bind(oms_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| OmsRouteEntry {
            account_id: r.try_get::<i64, _>("account_id").unwrap_or(0),
            gw_key: r.try_get("gw_id").unwrap_or_default(),
            exch_account_id: r.try_get("exch_account_id").unwrap_or_default(),
            ..Default::default()
        })
        .collect())
}

async fn load_gw_configs(pool: &PgPool) -> Result<Vec<GwConfigEntry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT gw_id, venue, rpc_endpoint FROM cfg.gateway_instance WHERE enabled = true",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| GwConfigEntry {
            gw_key: r.try_get("gw_id").unwrap_or_default(),
            exch_name: r.try_get("venue").unwrap_or_default(),
            rpc_endpoint: r.try_get("rpc_endpoint").unwrap_or_default(),
            ..Default::default()
        })
        .collect())
}

async fn load_refdata(pool: &PgPool) -> Result<Vec<InstrumentRefData>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT instrument_id, instrument_exch, venue, \
         instrument_type, disabled, COALESCE(contract_size, 1.0)::float8 AS contract_size \
         FROM cfg.instrument_refdata WHERE disabled = false",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let inst_type_str: String = r.try_get("instrument_type").unwrap_or_default();
            InstrumentRefData {
                instrument_id: r.try_get("instrument_id").unwrap_or_default(),
                instrument_id_exchange: r.try_get("instrument_exch").unwrap_or_default(),
                exchange_name: r.try_get("venue").unwrap_or_default(),
                instrument_type: parse_instrument_type(&inst_type_str),
                disabled: r.try_get::<bool, _>("disabled").unwrap_or(false),
                contract_size: r.try_get::<f64, _>("contract_size").unwrap_or(1.0),
                ..Default::default()
            }
        })
        .collect())
}

async fn load_trading_configs(pool: &PgPool) -> Result<Vec<InstrumentTradingConfig>, sqlx::Error> {
    // Table is optional — return empty list if absent.
    let rows = sqlx::query(
        "SELECT instrument_code, bookkeeping_balance, balance_check, \
         publish_balance_on_trade, use_margin, margin_ratio, max_order_size \
         FROM cfg.instrument_trading_config",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(rows
        .iter()
        .map(|r| InstrumentTradingConfig {
            instrument_code: r.try_get("instrument_code").unwrap_or_default(),
            bookkeeping_balance: r.try_get::<bool, _>("bookkeeping_balance").unwrap_or(false),
            balance_check: r.try_get::<bool, _>("balance_check").unwrap_or(false),
            publish_balance_on_trade: r
                .try_get::<bool, _>("publish_balance_on_trade")
                .unwrap_or(true),
            publish_balance_on_book: false,
            publish_balance_on_cancel: false,
            use_margin: r.try_get::<bool, _>("use_margin").unwrap_or(false),
            margin_ratio: r.try_get::<f64, _>("margin_ratio").unwrap_or(1.0),
            max_order_size: r
                .try_get::<Option<f64>, _>("max_order_size")
                .unwrap_or(None),
        })
        .collect())
}

/// Derive the gateway gRPC address from a `GwConfigEntry`.
///
/// `rpc_endpoint` should contain `host:port`; prefixes `http://` if absent.
pub fn gw_grpc_addr(gw: &GwConfigEntry) -> String {
    let ep = &gw.rpc_endpoint;
    if ep.starts_with("http://") || ep.starts_with("https://") {
        ep.clone()
    } else {
        format!("http://{ep}")
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert the canonical proto-aligned `instrument_type` string from
/// `cfg.instrument_refdata` into the `zk.common.v1.InstrumentType` proto int.
///
/// Source of truth: zkbot/protos/zk/common/v1/common.proto. The DB CHECK
/// constraint guarantees inputs are UPPERCASE and in the known set;
/// `to_uppercase()` is defensive for legacy callers / tests. Any unknown
/// value logs a warning so silent type drops are detected.
fn parse_instrument_type(s: &str) -> i32 {
    let upper = s.trim().to_uppercase();
    match upper.as_str() {
        "SPOT" => InstrumentType::InstTypeSpot as i32,
        "PERP" => InstrumentType::InstTypePerp as i32,
        "FUTURE" => InstrumentType::InstTypeFuture as i32,
        "CFD" => InstrumentType::InstTypeCfd as i32,
        "OPTION" => InstrumentType::InstTypeOption as i32,
        "ETF" => InstrumentType::InstTypeEtf as i32,
        "STOCK" => InstrumentType::InstTypeStock as i32,
        "UNSPECIFIED" | "" => InstrumentType::InstTypeUnspecified as i32,
        _ => {
            tracing::warn!(
                instrument_type = s,
                "unknown instrument_type from refdata; defaulting to UNSPECIFIED"
            );
            InstrumentType::InstTypeUnspecified as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_instrument_type;
    use zk_proto_rs::zk::common::v1::InstrumentType;

    #[test]
    fn covers_every_proto_variant() {
        assert_eq!(parse_instrument_type("SPOT"), InstrumentType::InstTypeSpot as i32);
        assert_eq!(parse_instrument_type("PERP"), InstrumentType::InstTypePerp as i32);
        assert_eq!(parse_instrument_type("FUTURE"), InstrumentType::InstTypeFuture as i32);
        assert_eq!(parse_instrument_type("CFD"), InstrumentType::InstTypeCfd as i32);
        assert_eq!(parse_instrument_type("OPTION"), InstrumentType::InstTypeOption as i32);
        assert_eq!(parse_instrument_type("ETF"), InstrumentType::InstTypeEtf as i32);
        assert_eq!(parse_instrument_type("STOCK"), InstrumentType::InstTypeStock as i32);
        assert_eq!(parse_instrument_type("UNSPECIFIED"), InstrumentType::InstTypeUnspecified as i32);
        assert_eq!(parse_instrument_type(""), InstrumentType::InstTypeUnspecified as i32);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse_instrument_type("stock"), InstrumentType::InstTypeStock as i32);
        assert_eq!(parse_instrument_type("Etf"), InstrumentType::InstTypeEtf as i32);
        assert_eq!(parse_instrument_type("  spot  "), InstrumentType::InstTypeSpot as i32);
    }

    #[test]
    fn unknown_falls_back_to_unspecified() {
        assert_eq!(parse_instrument_type("foo"), InstrumentType::InstTypeUnspecified as i32);
    }
}
