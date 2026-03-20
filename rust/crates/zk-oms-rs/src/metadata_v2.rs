use std::collections::HashMap;

use crate::config::ConfdataManager;
use crate::intern_v2::InternTable;

// ---------------------------------------------------------------------------
// Metadata structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InstrumentMeta {
    pub instrument_id: u32,
    pub instrument_code_sym: u32,
    pub instrument_exch_sym: u32,
    pub instrument_type: i32,
    pub base_asset_id: Option<u32>,
    pub base_asset_sym: Option<u32>,
    pub quote_asset_id: Option<u32>,
    pub settlement_asset_id: Option<u32>,
    pub qty_precision: i32,
    pub price_precision: i32,
}

#[derive(Debug, Clone)]
pub struct RouteMeta {
    pub account_id: i64,
    pub gw_id: u32,
    pub gw_key_sym: u32,
    pub exch_account_sym: u32,
}

#[derive(Debug, Clone)]
pub struct GwMeta {
    pub gw_id: u32,
    pub gw_key_sym: u32,
    pub exch_name_sym: u32,
    pub cancel_required_fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AssetMeta {
    pub asset_id: u32,
    pub asset_sym: u32,
}

#[derive(Debug, Clone)]
pub struct TradingMeta {
    pub instrument_id: u32,
    pub bookkeeping_balance: bool,
    pub balance_check: bool,
    pub use_margin: bool,
    pub margin_ratio: f64,
    pub max_order_size: Option<f64>,
    pub publish_balance_on_trade: bool,
    pub publish_balance_on_book: bool,
    pub publish_balance_on_cancel: bool,
}

// ---------------------------------------------------------------------------
// MetadataTables
// ---------------------------------------------------------------------------

pub struct MetadataTables {
    pub strings: InternTable,

    // Instrument lookups
    pub instruments: Vec<InstrumentMeta>,
    pub instrument_by_code: HashMap<u32, u32>,
    pub instrument_by_gw_symbol: HashMap<(u32, u32), u32>,

    // Gateway lookups
    pub gws: Vec<GwMeta>,
    pub gw_by_key: HashMap<u32, u32>,

    // Route lookups
    pub route_by_account: HashMap<i64, RouteMeta>,

    // Asset lookups
    pub assets: Vec<AssetMeta>,
    pub asset_by_symbol: HashMap<u32, u32>,

    // Trading config
    pub trading_by_instrument: HashMap<u32, TradingMeta>,
}

impl MetadataTables {
    /// Build integer-ID lookup tables from the v1 string-keyed config.
    pub fn from_confdata(config: &ConfdataManager) -> Self {
        let mut strings = InternTable::default();

        // --- 1. Gateways ---
        let mut gws: Vec<GwMeta> = Vec::new();
        let mut gw_by_key: HashMap<u32, u32> = HashMap::new();

        for (_gw_key, gw_cfg) in &config.gw_configs {
            let gw_id = gws.len() as u32;
            let gw_key_sym = strings.intern(&gw_cfg.gw_key);
            let exch_name_sym = strings.intern(&gw_cfg.exch_name);
            gw_by_key.insert(gw_key_sym, gw_id);
            gws.push(GwMeta {
                gw_id,
                gw_key_sym,
                exch_name_sym,
                cancel_required_fields: gw_cfg.cancel_required_fields.clone(),
            });
        }

        // --- 2. Routes ---
        let mut route_by_account: HashMap<i64, RouteMeta> = HashMap::new();

        for (account_id, route) in &config.account_routes {
            let gw_key_sym = strings.intern(&route.gw_key);
            let gw_id = match gw_by_key.get(&gw_key_sym) {
                Some(id) => *id,
                None => continue, // gateway not in config, skip
            };
            let exch_account_sym = strings.intern(&route.exch_account_id);
            route_by_account.insert(
                *account_id,
                RouteMeta {
                    account_id: *account_id,
                    gw_id,
                    gw_key_sym,
                    exch_account_sym,
                },
            );
        }

        // --- 3. Assets (collect unique from refdata) ---
        let mut assets: Vec<AssetMeta> = Vec::new();
        let mut asset_by_symbol: HashMap<u32, u32> = HashMap::new();

        let ensure_asset = |strings: &mut InternTable,
                            assets: &mut Vec<AssetMeta>,
                            asset_by_symbol: &mut HashMap<u32, u32>,
                            name: &str|
         -> Option<u32> {
            if name.is_empty() {
                return None;
            }
            let sym = strings.intern(name);
            if let std::collections::hash_map::Entry::Vacant(e) = asset_by_symbol.entry(sym) {
                let id = assets.len() as u32;
                e.insert(id);
                assets.push(AssetMeta {
                    asset_id: id,
                    asset_sym: sym,
                });
            }
            Some(*asset_by_symbol.get(&sym).unwrap())
        };

        for ref_data in config.refdata.values() {
            ensure_asset(
                &mut strings,
                &mut assets,
                &mut asset_by_symbol,
                &ref_data.base_asset,
            );
            ensure_asset(
                &mut strings,
                &mut assets,
                &mut asset_by_symbol,
                &ref_data.quote_asset,
            );
            ensure_asset(
                &mut strings,
                &mut assets,
                &mut asset_by_symbol,
                &ref_data.settlement_asset,
            );
        }

        // --- 4. Instruments ---
        let mut instruments: Vec<InstrumentMeta> = Vec::new();
        let mut instrument_by_code: HashMap<u32, u32> = HashMap::new();
        let mut instrument_by_gw_symbol: HashMap<(u32, u32), u32> = HashMap::new();

        for ref_data in config.refdata.values() {
            let inst_id = instruments.len() as u32;
            let code_sym = strings.intern(&ref_data.instrument_id);
            let exch_sym = strings.intern(&ref_data.instrument_id_exchange);

            let (base_asset_id, base_asset_sym) = if ref_data.base_asset.is_empty() {
                (None, None)
            } else {
                let sym = strings.intern(&ref_data.base_asset);
                (asset_by_symbol.get(&sym).copied(), Some(sym))
            };
            let quote_asset_id = if ref_data.quote_asset.is_empty() {
                None
            } else {
                let sym = strings.intern(&ref_data.quote_asset);
                asset_by_symbol.get(&sym).copied()
            };
            let settlement_asset_id = if ref_data.settlement_asset.is_empty() {
                None
            } else {
                let sym = strings.intern(&ref_data.settlement_asset);
                asset_by_symbol.get(&sym).copied()
            };

            let meta = InstrumentMeta {
                instrument_id: inst_id,
                instrument_code_sym: code_sym,
                instrument_exch_sym: exch_sym,
                instrument_type: ref_data.instrument_type,
                base_asset_id,
                base_asset_sym,
                quote_asset_id,
                settlement_asset_id,
                qty_precision: ref_data.qty_precision as i32,
                price_precision: ref_data.price_precision as i32,
            };

            instrument_by_code.insert(code_sym, inst_id);
            instruments.push(meta);

            // Map (gw_id, exch_sym) -> instrument_id for every gateway serving this exchange.
            if let Some(gw_keys) = config.exch_name_to_gw_keys.get(&ref_data.exchange_name) {
                for gw_key in gw_keys {
                    let gk_sym = strings.intern(gw_key);
                    if let Some(&gw_id) = gw_by_key.get(&gk_sym) {
                        instrument_by_gw_symbol.insert((gw_id, exch_sym), inst_id);
                    }
                }
            }
        }

        // --- 5. Trading configs ---
        let mut trading_by_instrument: HashMap<u32, TradingMeta> = HashMap::new();

        for (instrument_code, tc) in &config.trading_configs {
            let code_sym = strings.intern(instrument_code);
            if let Some(&inst_id) = instrument_by_code.get(&code_sym) {
                trading_by_instrument.insert(
                    inst_id,
                    TradingMeta {
                        instrument_id: inst_id,
                        bookkeeping_balance: tc.bookkeeping_balance,
                        balance_check: tc.balance_check,
                        use_margin: tc.use_margin,
                        margin_ratio: tc.margin_ratio,
                        max_order_size: tc.max_order_size,
                        publish_balance_on_trade: tc.publish_balance_on_trade,
                        publish_balance_on_book: tc.publish_balance_on_book,
                        publish_balance_on_cancel: tc.publish_balance_on_cancel,
                    },
                );
            }
        }

        Self {
            strings,
            instruments,
            instrument_by_code,
            instrument_by_gw_symbol,
            gws,
            gw_by_key,
            route_by_account,
            assets,
            asset_by_symbol,
            trading_by_instrument,
        }
    }

    /// Rebuild metadata from a new config, reusing the existing `InternTable`.
    /// This preserves ID stability: same strings keep the same IDs.
    /// New strings get new IDs appended. Removed config entries leave their
    /// IDs allocated (no compaction) — existing LiveOrder references remain valid.
    pub fn rebuild_from_config(&mut self, config: &ConfdataManager) {
        // Reuse self.strings — don't create a new InternTable.
        // Preserve existing IDs: consult reverse-lookup maps before assigning.
        // Existing keys keep their old IDs; new keys get appended at the end.

        // --- 1. Gateways (ID-stable) ---
        let mut new_gw_by_key: HashMap<u32, u32> = HashMap::new();
        let mut next_gw_id = self.gws.len() as u32;

        for gw_cfg in config.gw_configs.values() {
            let gw_key_sym = self.strings.intern(&gw_cfg.gw_key);
            let exch_name_sym = self.strings.intern(&gw_cfg.exch_name);
            let meta = GwMeta {
                gw_id: 0, // placeholder, set below
                gw_key_sym,
                exch_name_sym,
                cancel_required_fields: gw_cfg.cancel_required_fields.clone(),
            };

            if let Some(&existing_id) = self.gw_by_key.get(&gw_key_sym) {
                // Reuse existing ID, update in place.
                let mut m = meta;
                m.gw_id = existing_id;
                self.gws[existing_id as usize] = m;
                new_gw_by_key.insert(gw_key_sym, existing_id);
            } else {
                // New gateway — append.
                let id = next_gw_id;
                next_gw_id += 1;
                let mut m = meta;
                m.gw_id = id;
                self.gws.push(m);
                new_gw_by_key.insert(gw_key_sym, id);
            }
        }
        self.gw_by_key = new_gw_by_key;

        // --- 2. Routes (rebuilt from scratch using stable gw_ids) ---
        let mut route_by_account: HashMap<i64, RouteMeta> = HashMap::new();

        for (account_id, route) in &config.account_routes {
            let gw_key_sym = self.strings.intern(&route.gw_key);
            let gw_id = match self.gw_by_key.get(&gw_key_sym) {
                Some(id) => *id,
                None => continue,
            };
            let exch_account_sym = self.strings.intern(&route.exch_account_id);
            route_by_account.insert(
                *account_id,
                RouteMeta {
                    account_id: *account_id,
                    gw_id,
                    gw_key_sym,
                    exch_account_sym,
                },
            );
        }

        // --- 3. Assets (ID-stable) ---
        let mut new_asset_by_symbol: HashMap<u32, u32> = HashMap::new();
        let mut next_asset_id = self.assets.len() as u32;

        let ensure_asset_stable = |strings: &mut InternTable,
                                   assets: &mut Vec<AssetMeta>,
                                   existing_map: &HashMap<u32, u32>,
                                   new_map: &mut HashMap<u32, u32>,
                                   next_id: &mut u32,
                                   name: &str|
         -> Option<u32> {
            if name.is_empty() {
                return None;
            }
            let sym = strings.intern(name);
            // Already processed in this rebuild pass?
            if let Some(&id) = new_map.get(&sym) {
                return Some(id);
            }
            // Existed before rebuild?
            if let Some(&existing_id) = existing_map.get(&sym) {
                new_map.insert(sym, existing_id);
                // Update in place (asset content is trivial, but stay consistent).
                assets[existing_id as usize] = AssetMeta {
                    asset_id: existing_id,
                    asset_sym: sym,
                };
                return Some(existing_id);
            }
            // New asset — append.
            let id = *next_id;
            *next_id += 1;
            new_map.insert(sym, id);
            assets.push(AssetMeta {
                asset_id: id,
                asset_sym: sym,
            });
            Some(id)
        };

        let old_asset_map = std::mem::take(&mut self.asset_by_symbol);
        for ref_data in config.refdata.values() {
            ensure_asset_stable(
                &mut self.strings,
                &mut self.assets,
                &old_asset_map,
                &mut new_asset_by_symbol,
                &mut next_asset_id,
                &ref_data.base_asset,
            );
            ensure_asset_stable(
                &mut self.strings,
                &mut self.assets,
                &old_asset_map,
                &mut new_asset_by_symbol,
                &mut next_asset_id,
                &ref_data.quote_asset,
            );
            ensure_asset_stable(
                &mut self.strings,
                &mut self.assets,
                &old_asset_map,
                &mut new_asset_by_symbol,
                &mut next_asset_id,
                &ref_data.settlement_asset,
            );
        }
        self.asset_by_symbol = new_asset_by_symbol;

        // --- 4. Instruments (ID-stable) ---
        let mut new_instrument_by_code: HashMap<u32, u32> = HashMap::new();
        let mut instrument_by_gw_symbol: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next_inst_id = self.instruments.len() as u32;

        for ref_data in config.refdata.values() {
            let code_sym = self.strings.intern(&ref_data.instrument_id);
            let exch_sym = self.strings.intern(&ref_data.instrument_id_exchange);

            let (base_asset_id, base_asset_sym) = if ref_data.base_asset.is_empty() {
                (None, None)
            } else {
                let sym = self.strings.intern(&ref_data.base_asset);
                (self.asset_by_symbol.get(&sym).copied(), Some(sym))
            };
            let quote_asset_id = if ref_data.quote_asset.is_empty() {
                None
            } else {
                let sym = self.strings.intern(&ref_data.quote_asset);
                self.asset_by_symbol.get(&sym).copied()
            };
            let settlement_asset_id = if ref_data.settlement_asset.is_empty() {
                None
            } else {
                let sym = self.strings.intern(&ref_data.settlement_asset);
                self.asset_by_symbol.get(&sym).copied()
            };

            // Reuse existing ID or append.
            let inst_id = if let Some(&existing_id) = self.instrument_by_code.get(&code_sym) {
                // Update in place — config fields may have changed.
                self.instruments[existing_id as usize] = InstrumentMeta {
                    instrument_id: existing_id,
                    instrument_code_sym: code_sym,
                    instrument_exch_sym: exch_sym,
                    instrument_type: ref_data.instrument_type,
                    base_asset_id,
                    base_asset_sym,
                    quote_asset_id,
                    settlement_asset_id,
                    qty_precision: ref_data.qty_precision as i32,
                    price_precision: ref_data.price_precision as i32,
                };
                existing_id
            } else {
                let id = next_inst_id;
                next_inst_id += 1;
                self.instruments.push(InstrumentMeta {
                    instrument_id: id,
                    instrument_code_sym: code_sym,
                    instrument_exch_sym: exch_sym,
                    instrument_type: ref_data.instrument_type,
                    base_asset_id,
                    base_asset_sym,
                    quote_asset_id,
                    settlement_asset_id,
                    qty_precision: ref_data.qty_precision as i32,
                    price_precision: ref_data.price_precision as i32,
                });
                id
            };

            new_instrument_by_code.insert(code_sym, inst_id);

            // Rebuild (gw_id, exch_sym) -> instrument_id index from scratch.
            if let Some(gw_keys) = config.exch_name_to_gw_keys.get(&ref_data.exchange_name) {
                for gw_key in gw_keys {
                    let gk_sym = self.strings.intern(gw_key);
                    if let Some(&gw_id) = self.gw_by_key.get(&gk_sym) {
                        instrument_by_gw_symbol.insert((gw_id, exch_sym), inst_id);
                    }
                }
            }
        }
        self.instrument_by_code = new_instrument_by_code;
        self.instrument_by_gw_symbol = instrument_by_gw_symbol;

        // --- 5. Trading configs (rebuilt from scratch using stable instrument IDs) ---
        let mut trading_by_instrument: HashMap<u32, TradingMeta> = HashMap::new();

        for (instrument_code, tc) in &config.trading_configs {
            let code_sym = self.strings.intern(instrument_code);
            if let Some(&inst_id) = self.instrument_by_code.get(&code_sym) {
                trading_by_instrument.insert(
                    inst_id,
                    TradingMeta {
                        instrument_id: inst_id,
                        bookkeeping_balance: tc.bookkeeping_balance,
                        balance_check: tc.balance_check,
                        use_margin: tc.use_margin,
                        margin_ratio: tc.margin_ratio,
                        max_order_size: tc.max_order_size,
                        publish_balance_on_trade: tc.publish_balance_on_trade,
                        publish_balance_on_book: tc.publish_balance_on_book,
                        publish_balance_on_cancel: tc.publish_balance_on_cancel,
                    },
                );
            }
        }

        // Replace derived lookups; vectors (gws, assets, instruments) were updated in place.
        self.route_by_account = route_by_account;
        self.trading_by_instrument = trading_by_instrument;
    }

    /// Resolve instrument by OMS instrument code string.
    pub fn resolve_instrument(&self, instrument_code: &str) -> Option<&InstrumentMeta> {
        let code_sym = self.strings.lookup(instrument_code)?;
        let inst_id = self.instrument_by_code.get(&code_sym)?;
        self.instruments.get(*inst_id as usize)
    }

    /// Resolve instrument by gateway exchange symbol.
    pub fn resolve_instrument_by_gw(
        &self,
        gw_id: u32,
        exch_symbol: &str,
    ) -> Option<&InstrumentMeta> {
        let sym = self.strings.lookup(exch_symbol)?;
        let inst_id = self.instrument_by_gw_symbol.get(&(gw_id, sym))?;
        self.instruments.get(*inst_id as usize)
    }

    /// Get route for an account.
    pub fn route(&self, account_id: i64) -> Option<&RouteMeta> {
        self.route_by_account.get(&account_id)
    }

    /// Get trading config for an instrument.
    pub fn trading(&self, instrument_id: u32) -> Option<&TradingMeta> {
        self.trading_by_instrument.get(&instrument_id)
    }

    /// Get gateway by ID.
    pub fn gw(&self, gw_id: u32) -> Option<&GwMeta> {
        self.gws.get(gw_id as usize)
    }

    /// Get instrument by ID.
    pub fn instrument(&self, instrument_id: u32) -> Option<&InstrumentMeta> {
        self.instruments.get(instrument_id as usize)
    }

    /// Get asset by ID.
    pub fn asset(&self, asset_id: u32) -> Option<&AssetMeta> {
        self.assets.get(asset_id as usize)
    }

    /// Resolve an interned string ID back to string.
    pub fn str_resolve(&self, id: u32) -> &str {
        self.strings.resolve(id)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfdataManager, InstrumentTradingConfig};
    use zk_proto_rs::{
        ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
        zk::common::v1::{InstrumentRefData, InstrumentType},
    };

    fn build_test_confdata() -> ConfdataManager {
        let oms_config = OmsConfigEntry {
            oms_id: "TEST_OMS".into(),
            managed_account_ids: vec![100, 101],
            namespace: String::new(),
            desc: String::new(),
        };

        let account_routes = vec![
            OmsRouteEntry {
                account_id: 100,
                exch_account_id: "TEST1".into(),
                gw_key: "GW1".into(),
                startup_sync: false,
                desc: String::new(),
                broker_type: 0,
            },
            OmsRouteEntry {
                account_id: 101,
                exch_account_id: "TEST2".into(),
                gw_key: "GW2".into(),
                startup_sync: false,
                desc: String::new(),
                broker_type: 0,
            },
        ];

        let gw_configs = vec![
            GwConfigEntry {
                gw_key: "GW1".into(),
                exch_name: "EX1".into(),
                cancel_required_fields: vec!["instrument_id_exchange".into()],
                ..Default::default()
            },
            GwConfigEntry {
                gw_key: "GW2".into(),
                exch_name: "EX2".into(),
                cancel_required_fields: vec![],
                ..Default::default()
            },
        ];

        let refdata = vec![
            InstrumentRefData {
                instrument_id: "ETH-P/USDC@EX1".into(),
                instrument_id_exchange: "ETH".into(),
                exchange_name: "EX1".into(),
                instrument_type: InstrumentType::InstTypePerp as i32,
                base_asset: "ETH".into(),
                quote_asset: "USDC".into(),
                settlement_asset: "USDC".into(),
                qty_precision: 3,
                price_precision: 2,
                disabled: false,
                ..Default::default()
            },
            InstrumentRefData {
                instrument_id: "ETH/USD@EX2".into(),
                instrument_id_exchange: "ETH".into(),
                exchange_name: "EX2".into(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                base_asset: "ETH".into(),
                quote_asset: "USD".into(),
                settlement_asset: String::new(),
                qty_precision: 4,
                price_precision: 2,
                disabled: false,
                ..Default::default()
            },
        ];

        let trading_configs = vec![
            InstrumentTradingConfig {
                instrument_code: "ETH-P/USDC@EX1".into(),
                bookkeeping_balance: true,
                balance_check: true,
                publish_balance_on_trade: true,
                publish_balance_on_book: false,
                publish_balance_on_cancel: false,
                use_margin: true,
                margin_ratio: 0.1,
                max_order_size: Some(100.0),
            },
            InstrumentTradingConfig::default_for("ETH/USD@EX2"),
        ];

        ConfdataManager::new(
            oms_config,
            account_routes,
            gw_configs,
            refdata,
            trading_configs,
        )
    }

    #[test]
    fn test_resolve_instrument_by_code() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        let inst = tables.resolve_instrument("ETH-P/USDC@EX1").unwrap();
        assert_eq!(inst.instrument_type, InstrumentType::InstTypePerp as i32);
        assert_eq!(
            tables.str_resolve(inst.instrument_code_sym),
            "ETH-P/USDC@EX1"
        );
        assert_eq!(tables.str_resolve(inst.instrument_exch_sym), "ETH");
        assert_eq!(inst.qty_precision, 3);
        assert_eq!(inst.price_precision, 2);

        let inst2 = tables.resolve_instrument("ETH/USD@EX2").unwrap();
        assert_eq!(inst2.instrument_type, InstrumentType::InstTypeSpot as i32);

        assert!(tables.resolve_instrument("DOES_NOT_EXIST").is_none());
    }

    #[test]
    fn test_route_lookup() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        let r100 = tables.route(100).unwrap();
        assert_eq!(tables.str_resolve(r100.gw_key_sym), "GW1");
        assert_eq!(tables.str_resolve(r100.exch_account_sym), "TEST1");

        let gw1 = tables.gw(r100.gw_id).unwrap();
        assert_eq!(tables.str_resolve(gw1.gw_key_sym), "GW1");
        assert_eq!(tables.str_resolve(gw1.exch_name_sym), "EX1");

        let r101 = tables.route(101).unwrap();
        assert_eq!(tables.str_resolve(r101.gw_key_sym), "GW2");

        assert!(tables.route(999).is_none());
    }

    #[test]
    fn test_trading_config() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        let inst = tables.resolve_instrument("ETH-P/USDC@EX1").unwrap();
        let tc = tables.trading(inst.instrument_id).unwrap();
        assert!(tc.bookkeeping_balance);
        assert!(tc.balance_check);
        assert!(tc.use_margin);
        assert!((tc.margin_ratio - 0.1).abs() < f64::EPSILON);
        assert_eq!(tc.max_order_size, Some(100.0));
        assert!(tc.publish_balance_on_trade);
        assert!(!tc.publish_balance_on_book);
    }

    #[test]
    fn test_resolve_instrument_by_gw() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        // GW1 serves EX1 -> "ETH" should resolve to the perp
        let r100 = tables.route(100).unwrap();
        let inst = tables.resolve_instrument_by_gw(r100.gw_id, "ETH").unwrap();
        assert_eq!(inst.instrument_type, InstrumentType::InstTypePerp as i32);

        // GW2 serves EX2 -> "ETH" should resolve to the spot
        let r101 = tables.route(101).unwrap();
        let inst2 = tables.resolve_instrument_by_gw(r101.gw_id, "ETH").unwrap();
        assert_eq!(inst2.instrument_type, InstrumentType::InstTypeSpot as i32);

        // Unknown symbol
        assert!(tables.resolve_instrument_by_gw(r100.gw_id, "BTC").is_none());
    }

    #[test]
    fn test_assets_deduped() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        // "ETH" appears as base_asset in both instruments but should be one entry.
        let eth_sym = tables.strings.lookup("ETH").unwrap();
        let eth_id = tables.asset_by_symbol.get(&eth_sym).unwrap();
        let eth_meta = &tables.assets[*eth_id as usize];
        assert_eq!(tables.str_resolve(eth_meta.asset_sym), "ETH");

        // Unique assets: ETH, USDC, USD (settlement_asset "" is skipped)
        // Count unique: ETH, USDC, USD = 3
        assert_eq!(tables.assets.len(), 3);
    }

    #[test]
    fn test_gw_cancel_required_fields() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        let r100 = tables.route(100).unwrap();
        let gw1 = tables.gw(r100.gw_id).unwrap();
        assert_eq!(gw1.cancel_required_fields, vec!["instrument_id_exchange"]);

        let r101 = tables.route(101).unwrap();
        let gw2 = tables.gw(r101.gw_id).unwrap();
        assert!(gw2.cancel_required_fields.is_empty());
    }

    #[test]
    fn test_instrument_asset_links() {
        let config = build_test_confdata();
        let tables = MetadataTables::from_confdata(&config);

        let inst = tables.resolve_instrument("ETH-P/USDC@EX1").unwrap();
        let base_id = inst.base_asset_id.unwrap();
        let quote_id = inst.quote_asset_id.unwrap();
        let settle_id = inst.settlement_asset_id.unwrap();

        assert_eq!(
            tables.str_resolve(tables.assets[base_id as usize].asset_sym),
            "ETH"
        );
        assert_eq!(
            tables.str_resolve(tables.assets[quote_id as usize].asset_sym),
            "USDC"
        );
        assert_eq!(
            tables.str_resolve(tables.assets[settle_id as usize].asset_sym),
            "USDC"
        );

        // Spot instrument has no settlement_asset (empty string -> None)
        let inst2 = tables.resolve_instrument("ETH/USD@EX2").unwrap();
        assert!(inst2.settlement_asset_id.is_none());
    }

    #[test]
    fn test_reload_preserves_ids() {
        let config = build_test_confdata();
        let mut tables = MetadataTables::from_confdata(&config);

        // Record original IDs.
        let gw1_id = tables.gw_by_key[&tables.strings.lookup("GW1").unwrap()];
        let gw2_id = tables.gw_by_key[&tables.strings.lookup("GW2").unwrap()];
        let eth_perp_id = tables
            .resolve_instrument("ETH-P/USDC@EX1")
            .unwrap()
            .instrument_id;
        let eth_spot_id = tables
            .resolve_instrument("ETH/USD@EX2")
            .unwrap()
            .instrument_id;
        let eth_asset_id = tables.asset_by_symbol[&tables.strings.lookup("ETH").unwrap()];
        let usdc_asset_id = tables.asset_by_symbol[&tables.strings.lookup("USDC").unwrap()];

        // Build a new config: reorder + add a new instrument and gateway.
        let mut config2 = build_test_confdata();
        // Add GW3.
        config2.gw_configs.insert(
            "GW3".into(),
            GwConfigEntry {
                gw_key: "GW3".into(),
                exch_name: "EX3".into(),
                cancel_required_fields: vec![],
                ..Default::default()
            },
        );
        // Add a new instrument.
        config2.refdata.insert(
            "BTC/USDT@EX3".into(),
            InstrumentRefData {
                instrument_id: "BTC/USDT@EX3".into(),
                instrument_id_exchange: "BTC".into(),
                exchange_name: "EX3".into(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                base_asset: "BTC".into(),
                quote_asset: "USDT".into(),
                settlement_asset: String::new(),
                qty_precision: 8,
                price_precision: 2,
                disabled: false,
                ..Default::default()
            },
        );
        config2
            .exch_name_to_gw_keys
            .insert("EX3".into(), ["GW3".into()].into_iter().collect());

        tables.rebuild_from_config(&config2);

        // Existing IDs must be preserved.
        assert_eq!(
            tables.gw_by_key[&tables.strings.lookup("GW1").unwrap()],
            gw1_id,
            "GW1 ID changed after rebuild",
        );
        assert_eq!(
            tables.gw_by_key[&tables.strings.lookup("GW2").unwrap()],
            gw2_id,
            "GW2 ID changed after rebuild",
        );
        assert_eq!(
            tables
                .resolve_instrument("ETH-P/USDC@EX1")
                .unwrap()
                .instrument_id,
            eth_perp_id,
            "ETH-P/USDC@EX1 instrument_id changed after rebuild",
        );
        assert_eq!(
            tables
                .resolve_instrument("ETH/USD@EX2")
                .unwrap()
                .instrument_id,
            eth_spot_id,
            "ETH/USD@EX2 instrument_id changed after rebuild",
        );
        assert_eq!(
            tables.asset_by_symbol[&tables.strings.lookup("ETH").unwrap()],
            eth_asset_id,
            "ETH asset_id changed after rebuild",
        );
        assert_eq!(
            tables.asset_by_symbol[&tables.strings.lookup("USDC").unwrap()],
            usdc_asset_id,
            "USDC asset_id changed after rebuild",
        );

        // New entries got appended IDs (larger than any pre-existing).
        let gw3_id = tables.gw_by_key[&tables.strings.lookup("GW3").unwrap()];
        assert!(
            gw3_id > gw1_id && gw3_id > gw2_id,
            "GW3 should have a new appended ID"
        );

        let btc_inst = tables.resolve_instrument("BTC/USDT@EX3").unwrap();
        assert!(
            btc_inst.instrument_id > eth_perp_id && btc_inst.instrument_id > eth_spot_id,
            "BTC/USDT@EX3 should have a new appended instrument_id",
        );

        let btc_asset_id = tables.asset_by_symbol[&tables.strings.lookup("BTC").unwrap()];
        assert!(btc_asset_id > eth_asset_id && btc_asset_id > usdc_asset_id);

        // instrument_by_gw_symbol still works for new gateway.
        let gw3_inst = tables.resolve_instrument_by_gw(gw3_id, "BTC").unwrap();
        assert_eq!(gw3_inst.instrument_id, btc_inst.instrument_id);
    }
}
