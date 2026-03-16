use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use zk_proto_rs::zk::common::v1::InstrumentRefData;

use crate::models::OmsManagedPosition;

/// Instrument type classified as spot-like (balance domain, not position domain).
/// Matches `InstrumentType::InstTypeSpot` (proto value 1).
pub const SPOT_INSTRUMENT_TYPE: i32 = 1;

/// Current wall-clock time in milliseconds since Unix epoch.
pub fn gen_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock went backwards")
        .as_millis() as i64
}

/// Truncate `val` to `precision` decimal places (same semantics as the Python OMS).
/// e.g. `round_to_precision(1.2349, 2)` → `1.23`
pub fn round_to_precision(val: f64, precision: i64) -> f64 {
    let factor = 10_f64.powi(precision as i32);
    (val * factor).floor() / factor
}

// ---------------------------------------------------------------------------
// Simple monotonic ID generator for external (non-OMS) orders.
// ---------------------------------------------------------------------------
static COUNTER: AtomicI64 = AtomicI64::new(0);

pub fn init_id_gen() {
    // Seed with lower 32 bits of current microsecond time to avoid collisions
    // across restarts (same approach as Python's snowflake seed).
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_micros() as i64;
    COUNTER.store(seed << 20, Ordering::Relaxed);
}

pub fn gen_order_id() -> i64 {
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Spot position derivation from balances
// ---------------------------------------------------------------------------

/// Collect the unique `base_asset` values from SPOT instruments in refdata.
///
/// This is the preprocessing step: scan refdata once to determine which assets
/// are eligible to become sellable spot positions. The result can be reused
/// across multiple accounts without re-scanning refdata.
pub fn collect_spot_position_assets(refdata: &[InstrumentRefData]) -> HashSet<String> {
    refdata
        .iter()
        .filter(|rd| rd.instrument_type == SPOT_INSTRUMENT_TYPE && !rd.base_asset.is_empty())
        .map(|rd| rd.base_asset.clone())
        .collect()
}

/// Derive sellable spot-inventory positions from asset balances.
///
/// For each asset in `spot_assets` that has a positive balance, produces one
/// `OmsManagedPosition` keyed by that asset (the OMS `pos_symbol` for spot).
///
/// This is an operational projection, not canonical exchange truth.
/// GW publishes balances; OMS derives sellable inventory from them.
///
/// Rules:
/// - Only assets present in `spot_assets` are considered
/// - Non-positive balances are skipped (no margin/borrow semantics)
pub fn derive_spot_positions_from_balances(
    account_id: i64,
    balances: &HashMap<String, f64>,
    spot_assets: &HashSet<String>,
) -> Vec<OmsManagedPosition> {
    let mut result = Vec::new();
    for (asset, &qty) in balances {
        if qty > 0.0 && spot_assets.contains(asset) {
            let mut pos =
                OmsManagedPosition::new(account_id, asset.clone(), SPOT_INSTRUMENT_TYPE, false);
            pos.qty_total = qty;
            pos.qty_available = qty;
            result.push(pos);
        }
    }
    result
}

/// Convenience: derive spot positions directly from refdata (combines both steps).
pub fn derive_spot_positions_from_balances_with_refdata(
    account_id: i64,
    balances: &HashMap<String, f64>,
    refdata: &[InstrumentRefData],
) -> Vec<OmsManagedPosition> {
    let spot_assets = collect_spot_position_assets(refdata);
    derive_spot_positions_from_balances(account_id, balances, &spot_assets)
}

/// Merge derived positions with explicit positions.
/// Explicit entries override derived ones for the same `(account_id, instrument_code)`.
pub fn merge_positions_with_override(
    derived: Vec<OmsManagedPosition>,
    explicit: Vec<OmsManagedPosition>,
) -> Vec<OmsManagedPosition> {
    let mut map: HashMap<(i64, String), OmsManagedPosition> = HashMap::new();
    for pos in derived {
        map.insert((pos.account_id, pos.instrument_code.clone()), pos);
    }
    for pos in explicit {
        map.insert((pos.account_id, pos.instrument_code.clone()), pos);
    }
    map.into_values().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use zk_proto_rs::zk::common::v1::{InstrumentRefData, InstrumentType};

    fn spot_refdata(id: &str, base: &str, quote: &str) -> InstrumentRefData {
        InstrumentRefData {
            instrument_id: id.to_string(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            instrument_type: InstrumentType::InstTypeSpot as i32,
            ..Default::default()
        }
    }

    fn perp_refdata(id: &str, base: &str) -> InstrumentRefData {
        InstrumentRefData {
            instrument_id: id.to_string(),
            base_asset: base.to_string(),
            instrument_type: InstrumentType::InstTypePerp as i32,
            ..Default::default()
        }
    }

    #[test]
    fn collect_spot_assets_from_refdata() {
        let rd = vec![
            spot_refdata("BTC-USDT@EX", "BTC", "USDT"),
            spot_refdata("ETH-USDT@EX", "ETH", "USDT"),
            perp_refdata("BTC-USDT-PERP@EX", "BTC"),
        ];
        let assets = collect_spot_position_assets(&rd);
        assert_eq!(assets.len(), 2);
        assert!(assets.contains("BTC"));
        assert!(assets.contains("ETH"));
    }

    #[test]
    fn collect_spot_assets_dedup() {
        let rd = vec![
            spot_refdata("BTC-USDT@EX", "BTC", "USDT"),
            spot_refdata("BTC-EUR@EX", "BTC", "EUR"),
        ];
        let assets = collect_spot_position_assets(&rd);
        assert_eq!(assets.len(), 1);
        assert!(assets.contains("BTC"));
    }

    #[test]
    fn btc_balance_derives_spot_position() {
        let bals: HashMap<String, f64> =
            [("BTC".into(), 1.5), ("USDT".into(), 10_000.0)].into();
        let rd = vec![spot_refdata("BTC-USDT@EX", "BTC", "USDT")];
        let positions = derive_spot_positions_from_balances_with_refdata(1, &bals, &rd);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].instrument_code, "BTC");
        assert_eq!(positions[0].qty_total, 1.5);
        assert_eq!(positions[0].qty_available, 1.5);
        assert!(!positions[0].is_short);
    }

    #[test]
    fn quote_asset_not_derived() {
        let bals: HashMap<String, f64> = [("USDT".into(), 10_000.0)].into();
        let rd = vec![spot_refdata("BTC-USDT@EX", "BTC", "USDT")];
        let positions = derive_spot_positions_from_balances_with_refdata(1, &bals, &rd);
        assert!(positions.is_empty(), "USDT is quote, not base — should not derive");
    }

    #[test]
    fn quote_as_base_in_other_pair() {
        let bals: HashMap<String, f64> = [("USDT".into(), 500.0)].into();
        let rd = vec![
            spot_refdata("BTC-USDT@EX", "BTC", "USDT"),
            spot_refdata("USDT-EUR@EX", "USDT", "EUR"),
        ];
        let positions = derive_spot_positions_from_balances_with_refdata(1, &bals, &rd);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].instrument_code, "USDT");
        assert_eq!(positions[0].qty_total, 500.0);
    }

    #[test]
    fn non_spot_ignored() {
        let bals: HashMap<String, f64> = [("BTC".into(), 1.0)].into();
        let rd = vec![perp_refdata("BTC-USDT-PERP@EX", "BTC")];
        let positions = derive_spot_positions_from_balances_with_refdata(1, &bals, &rd);
        assert!(positions.is_empty());
    }

    #[test]
    fn zero_balance_skipped() {
        let bals: HashMap<String, f64> = [("BTC".into(), 0.0)].into();
        let spot_assets = collect_spot_position_assets(&[spot_refdata("BTC-USDT@EX", "BTC", "USDT")]);
        let positions = derive_spot_positions_from_balances(1, &bals, &spot_assets);
        assert!(positions.is_empty());
    }

    #[test]
    fn negative_balance_skipped() {
        let bals: HashMap<String, f64> = [("BTC".into(), -0.5)].into();
        let spot_assets = collect_spot_position_assets(&[spot_refdata("BTC-USDT@EX", "BTC", "USDT")]);
        let positions = derive_spot_positions_from_balances(1, &bals, &spot_assets);
        assert!(positions.is_empty());
    }

    #[test]
    fn dedup_same_base_asset() {
        let bals: HashMap<String, f64> = [("BTC".into(), 2.0)].into();
        let rd = vec![
            spot_refdata("BTC-USDT@EX", "BTC", "USDT"),
            spot_refdata("BTC-EUR@EX", "BTC", "EUR"),
        ];
        let positions = derive_spot_positions_from_balances_with_refdata(1, &bals, &rd);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].qty_total, 2.0);
    }

    #[test]
    fn merge_explicit_overrides_derived() {
        let derived = vec![{
            let mut p = OmsManagedPosition::new(1, "BTC", SPOT_INSTRUMENT_TYPE, false);
            p.qty_total = 1.5;
            p.qty_available = 1.5;
            p
        }];
        let explicit = vec![{
            let mut p = OmsManagedPosition::new(1, "BTC", SPOT_INSTRUMENT_TYPE, false);
            p.qty_total = 3.0;
            p.qty_available = 3.0;
            p
        }];
        let merged = merge_positions_with_override(derived, explicit);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].qty_total, 3.0);
    }

    #[test]
    fn merge_keeps_non_overlapping() {
        let derived = vec![{
            let mut p = OmsManagedPosition::new(1, "BTC", SPOT_INSTRUMENT_TYPE, false);
            p.qty_total = 1.0;
            p
        }];
        let explicit = vec![{
            let mut p = OmsManagedPosition::new(1, "ETH-PERP", InstrumentType::InstTypePerp as i32, false);
            p.qty_total = 5.0;
            p
        }];
        let merged = merge_positions_with_override(derived, explicit);
        assert_eq!(merged.len(), 2);
    }
}
