use std::collections::HashMap;

use crate::models::ExchBalanceSnapshot;

/// Exchange-owned balance cache using integer asset IDs.
pub struct BalanceStoreV2 {
    /// (account_id, asset_id) → exchange balance snapshot
    pub(crate) exch_balances: HashMap<(i64, u32), ExchBalanceSnapshot>,
    /// interned exch_account_sym → account_id (reverse map)
    pub(crate) account_id_by_exch_account_sym: HashMap<u32, i64>,
}

impl BalanceStoreV2 {
    pub fn new() -> Self {
        Self {
            exch_balances: HashMap::new(),
            account_id_by_exch_account_sym: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    /// Insert balances keyed by (account_id, asset_id).
    /// The `u32` in each tuple is the asset_id; `account_id` is read from the snapshot.
    pub fn init_balances(&mut self, balances: Vec<(u32, ExchBalanceSnapshot)>) {
        for (asset_id, snap) in balances {
            self.exch_balances
                .insert((snap.account_id, asset_id), snap);
        }
    }

    /// Set the reverse map from interned exch_account_sym to account_id.
    pub fn init_account_reverse_map(&mut self, map: HashMap<u32, i64>) {
        self.account_id_by_exch_account_sym = map;
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    pub fn get_balance(&self, account_id: i64, asset_id: u32) -> Option<&ExchBalanceSnapshot> {
        self.exch_balances.get(&(account_id, asset_id))
    }

    pub fn get_balance_mut(
        &mut self,
        account_id: i64,
        asset_id: u32,
    ) -> Option<&mut ExchBalanceSnapshot> {
        self.exch_balances.get_mut(&(account_id, asset_id))
    }

    /// Get or create a balance entry, inserting a default if missing.
    pub fn get_or_create_balance(
        &mut self,
        account_id: i64,
        asset_id: u32,
    ) -> &mut ExchBalanceSnapshot {
        self.exch_balances
            .entry((account_id, asset_id))
            .or_insert_with(|| ExchBalanceSnapshot {
                account_id,
                asset: String::new(),
                symbol_exch: None,
                balance_state: Default::default(),
                exch_data_raw: String::new(),
                sync_ts: 0,
            })
    }

    /// Reverse lookup: interned exch_account_sym → account_id.
    pub fn resolve_account_id(&self, exch_account_sym: u32) -> Option<i64> {
        self.account_id_by_exch_account_sym
            .get(&exch_account_sym)
            .copied()
    }

    /// Iterate all balance snapshots.
    pub fn all_balances(&self) -> impl Iterator<Item = &ExchBalanceSnapshot> {
        self.exch_balances.values()
    }

    /// Collect all balances for a given account_id.
    pub fn get_balances_for_account(&self, account_id: i64) -> Vec<&ExchBalanceSnapshot> {
        self.exch_balances
            .iter()
            .filter(|((aid, _), _)| *aid == account_id)
            .map(|(_, snap)| snap)
            .collect()
    }

    // ------------------------------------------------------------------
    // Write
    // ------------------------------------------------------------------

    /// Insert or overwrite a balance snapshot.
    pub fn set_balance(&mut self, account_id: i64, asset_id: u32, snap: ExchBalanceSnapshot) {
        self.exch_balances.insert((account_id, asset_id), snap);
    }
}

impl Default for BalanceStoreV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snap(account_id: i64) -> ExchBalanceSnapshot {
        ExchBalanceSnapshot {
            account_id,
            asset: "USDT".to_string(),
            symbol_exch: None,
            balance_state: Default::default(),
            exch_data_raw: String::new(),
            sync_ts: 0,
        }
    }

    #[test]
    fn basic_crud() {
        let mut store = BalanceStoreV2::new();

        // init_balances
        let snap = make_snap(1);
        store.init_balances(vec![(10, snap)]);
        assert!(store.get_balance(1, 10).is_some());
        assert!(store.get_balance(1, 99).is_none());

        // get_balance_mut
        let b = store.get_balance_mut(1, 10).unwrap();
        b.sync_ts = 42;
        assert_eq!(store.get_balance(1, 10).unwrap().sync_ts, 42);

        // get_or_create_balance inserts default
        let b = store.get_or_create_balance(1, 20);
        b.sync_ts = 100;
        assert_eq!(store.get_balance(1, 20).unwrap().sync_ts, 100);

        // set_balance overwrites
        let mut snap2 = make_snap(1);
        snap2.sync_ts = 999;
        store.set_balance(1, 10, snap2);
        assert_eq!(store.get_balance(1, 10).unwrap().sync_ts, 999);

        // get_balances_for_account
        let all = store.get_balances_for_account(1);
        assert_eq!(all.len(), 2);

        // resolve_account_id
        let mut map = HashMap::new();
        map.insert(5, 1_i64);
        store.init_account_reverse_map(map);
        assert_eq!(store.resolve_account_id(5), Some(1));
        assert_eq!(store.resolve_account_id(99), None);
    }
}
