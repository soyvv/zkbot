use std::collections::{HashMap, HashSet};

use zk_proto_rs::{
    common::InstrumentRefData,
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
};

/// Per-instrument trading behaviour flags for the OMS.
/// Mirrors Python `InstrumentTradingConfig`.
#[derive(Debug, Clone)]
pub struct InstrumentTradingConfig {
    pub instrument_code: String,
    /// OMS maintains a bookkeeping balance (frozen/avail/total) for this instrument.
    pub bookkeeping_balance: bool,
    /// Pre-trade balance check is enforced before order placement.
    pub balance_check: bool,
    pub publish_balance_on_trade: bool,
    pub publish_balance_on_book: bool,
    pub publish_balance_on_cancel: bool,
    pub use_margin: bool,
    pub margin_ratio: f64,
    pub max_order_size: Option<f64>,
}

impl InstrumentTradingConfig {
    pub fn default_for(instrument_code: impl Into<String>) -> Self {
        Self {
            instrument_code: instrument_code.into(),
            bookkeeping_balance: false,
            balance_check: false,
            publish_balance_on_trade: true,
            publish_balance_on_book: false,
            publish_balance_on_cancel: false,
            use_margin: false,
            margin_ratio: 1.0,
            max_order_size: None,
        }
    }
}

/// Resolved config and lookup tables for the OMS.
/// Mirrors Python `ConfdataManager`.
#[derive(Debug, Clone)]
pub struct ConfdataManager {
    pub oms_id: String,
    /// account_id → route
    pub account_routes: HashMap<i64, OmsRouteEntry>,
    /// gw_key → GwConfigEntry
    pub gw_configs: HashMap<String, GwConfigEntry>,
    /// exch_name → set of gw_keys
    pub exch_name_to_gw_keys: HashMap<String, HashSet<String>>,
    /// gw_key → account_ids
    pub gw_key_to_account_ids: HashMap<String, HashSet<i64>>,
    /// account_id → GwConfigEntry  (resolved shortcut)
    pub account_id_to_gw_config: HashMap<i64, GwConfigEntry>,
    /// instrument_id → InstrumentRefData (non-disabled)
    pub refdata: HashMap<String, InstrumentRefData>,
    /// gw_key → instrument_id_exchange → InstrumentRefData
    pub refdata_by_gw_key: HashMap<String, HashMap<String, InstrumentRefData>>,
    /// account_id → instrument_id_exchange → InstrumentRefData
    pub refdata_by_account_id: HashMap<i64, HashMap<String, InstrumentRefData>>,
    /// instrument_id → InstrumentTradingConfig
    pub trading_configs: HashMap<String, InstrumentTradingConfig>,
}

impl ConfdataManager {
    pub fn new(
        oms_config: OmsConfigEntry,
        account_routes: Vec<OmsRouteEntry>,
        gw_configs: Vec<GwConfigEntry>,
        refdata: Vec<InstrumentRefData>,
        trading_configs: Vec<InstrumentTradingConfig>,
    ) -> Self {
        let managed: HashSet<i64> = oms_config.managed_account_ids.iter().copied().collect();
        let routes: Vec<OmsRouteEntry> = account_routes
            .into_iter()
            .filter(|r| managed.contains(&r.account_id))
            .collect();

        // gw_key → config
        let gw_key_to_config: HashMap<String, GwConfigEntry> = {
            let relevant_gw_keys: HashSet<&str> = routes.iter().map(|r| r.gw_key.as_str()).collect();
            gw_configs
                .into_iter()
                .filter(|g| relevant_gw_keys.contains(g.gw_key.as_str()))
                .map(|g| (g.gw_key.clone(), g))
                .collect()
        };

        // exch_name → gw_keys
        let mut exch_name_to_gw_keys: HashMap<String, HashSet<String>> = HashMap::new();
        for (gw_key, cfg) in &gw_key_to_config {
            exch_name_to_gw_keys
                .entry(cfg.exch_name.clone())
                .or_default()
                .insert(gw_key.clone());
        }

        // route lookups
        let mut route_map: HashMap<i64, OmsRouteEntry> = HashMap::new();
        let mut gw_key_to_account_ids: HashMap<String, HashSet<i64>> = HashMap::new();
        let mut account_id_to_gw_config: HashMap<i64, GwConfigEntry> = HashMap::new();
        for r in &routes {
            route_map.insert(r.account_id, r.clone());
            gw_key_to_account_ids
                .entry(r.gw_key.clone())
                .or_default()
                .insert(r.account_id);
            if let Some(gw_cfg) = gw_key_to_config.get(&r.gw_key) {
                account_id_to_gw_config.insert(r.account_id, gw_cfg.clone());
            }
        }

        // refdata lookup by gw_key and account_id
        let refdata_enabled: Vec<&InstrumentRefData> =
            refdata.iter().filter(|r| !r.disabled).collect();

        let mut refdata_by_gw_key: HashMap<String, HashMap<String, InstrumentRefData>> =
            HashMap::new();
        let mut refdata_by_account_id: HashMap<i64, HashMap<String, InstrumentRefData>> =
            HashMap::new();

        for ref_entry in &refdata_enabled {
            let exch_name = &ref_entry.exchange_name;
            let gw_keys_for_exch = exch_name_to_gw_keys
                .get(exch_name)
                .cloned()
                .unwrap_or_default();

            for gw_key in &gw_keys_for_exch {
                refdata_by_gw_key
                    .entry(gw_key.clone())
                    .or_default()
                    .insert(ref_entry.instrument_id_exchange.clone(), (*ref_entry).clone());

                // also build account_id lookup
                for (acc_id, acc_route) in &route_map {
                    if acc_route.gw_key == *gw_key {
                        refdata_by_account_id
                            .entry(*acc_id)
                            .or_default()
                            .insert(ref_entry.instrument_id_exchange.clone(), (*ref_entry).clone());
                    }
                }
            }
        }

        let refdata_map: HashMap<String, InstrumentRefData> = refdata_enabled
            .into_iter()
            .map(|r| (r.instrument_id.clone(), r.clone()))
            .collect();

        // trading config — fill gaps with defaults
        let mut tc_map: HashMap<String, InstrumentTradingConfig> = trading_configs
            .into_iter()
            .map(|tc| (tc.instrument_code.clone(), tc))
            .collect();
        for id in refdata_map.keys() {
            tc_map
                .entry(id.clone())
                .or_insert_with(|| InstrumentTradingConfig::default_for(id));
        }

        Self {
            oms_id: oms_config.oms_id,
            account_routes: route_map,
            gw_configs: gw_key_to_config,
            exch_name_to_gw_keys,
            gw_key_to_account_ids,
            account_id_to_gw_config,
            refdata: refdata_map,
            refdata_by_gw_key,
            refdata_by_account_id,
            trading_configs: tc_map,
        }
    }

    pub fn get_trading_config(&self, instrument_code: &str) -> &InstrumentTradingConfig {
        self.trading_configs
            .get(instrument_code)
            .expect("trading config missing; should have been filled with defaults")
    }
}
