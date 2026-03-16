use std::collections::HashMap;

use zk_proto_rs::zk::exch_gw::v1::PositionReport;

use crate::venue::simulator::error_injection::InjectedErrorRule;

// ─── Balance / Position entries ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BalanceEntry {
    pub total_qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
}

impl BalanceEntry {
    pub fn new(qty: f64) -> Self {
        Self {
            total_qty: qty,
            avail_qty: qty,
            frozen_qty: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PositionEntry {
    pub instrument_code: String,
    pub long_short_type: i32,
    pub total_qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
}

// ─── SimAccountState ────────────────────────────────────────────────────────

/// Simulator-owned account state: balances, positions, and initial snapshots for reset.
pub struct SimAccountState {
    pub account_id: i64,
    pub balances: HashMap<String, BalanceEntry>,
    pub positions: HashMap<String, PositionEntry>,
    initial_balances: HashMap<String, BalanceEntry>,
    initial_positions: HashMap<String, PositionEntry>,
}

impl SimAccountState {
    pub fn new(account_id: i64, initial_balances: HashMap<String, f64>) -> Self {
        let balances: HashMap<String, BalanceEntry> = initial_balances
            .iter()
            .map(|(k, &v)| (k.clone(), BalanceEntry::new(v)))
            .collect();
        let initial = balances.clone();
        Self {
            account_id,
            balances,
            positions: HashMap::new(),
            initial_balances: initial,
            initial_positions: HashMap::new(),
        }
    }

    /// Apply balance changes from a fill.
    /// For a BUY of "BASE-QUOTE" at price P, qty Q: BASE += Q, QUOTE -= P*Q.
    /// For a SELL: inverse.
    pub fn apply_fill_to_balances(&mut self, instrument: &str, side: i32, qty: f64, price: f64) {
        let (base, quote) = instrument
            .split_once('-')
            .map(|(b, q)| (b.to_string(), Some(q.to_string())))
            .unwrap_or((instrument.to_string(), None));

        let is_buy = side == 1; // BuySellType::BsBuy
        let value = price * qty;

        if is_buy {
            let e = self
                .balances
                .entry(base)
                .or_insert_with(|| BalanceEntry::new(0.0));
            e.total_qty += qty;
            e.avail_qty += qty;
            if let Some(q) = quote {
                let e = self
                    .balances
                    .entry(q)
                    .or_insert_with(|| BalanceEntry::new(0.0));
                e.total_qty -= value;
                e.avail_qty -= value;
            }
        } else {
            let e = self
                .balances
                .entry(base)
                .or_insert_with(|| BalanceEntry::new(0.0));
            e.total_qty -= qty;
            e.avail_qty -= qty;
            if let Some(q) = quote {
                let e = self
                    .balances
                    .entry(q)
                    .or_insert_with(|| BalanceEntry::new(0.0));
                e.total_qty += value;
                e.avail_qty += value;
            }
        }
    }

    /// Build a snapshot of current balances as `PositionReport` entries.
    pub fn balance_snapshot(&self) -> Vec<PositionReport> {
        use zk_proto_rs::zk::common::v1::InstrumentType;
        self.balances
            .iter()
            .map(|(symbol, entry)| PositionReport {
                instrument_code: symbol.clone(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                qty: entry.total_qty,
                avail_qty: entry.avail_qty,
                account_id: self.account_id,
                ..Default::default()
            })
            .collect()
    }

    /// Reset to initial state.
    pub fn reset(&mut self) {
        self.balances = self.initial_balances.clone();
        self.positions = self.initial_positions.clone();
    }
}

// ─── ManualControlState ─────────────────────────────────────────────────────

/// Test-harness control state for the simulator.
pub struct ManualControlState {
    pub error_rules: Vec<InjectedErrorRule>,
    pub matching_paused: bool,
    pub current_match_policy: String,
    initial_match_policy: String,
}

impl ManualControlState {
    pub fn new(initial_policy: String) -> Self {
        Self {
            error_rules: Vec::new(),
            matching_paused: false,
            current_match_policy: initial_policy.clone(),
            initial_match_policy: initial_policy,
        }
    }

    pub fn reset(&mut self) {
        self.error_rules.clear();
        self.matching_paused = false;
        self.current_match_policy = self.initial_match_policy.clone();
    }
}

// ─── Composite simulator state ──────────────────────────────────────────────

/// All simulator state behind one mutex.
pub struct SimulatorState {
    pub sim_core: zk_sim_core::simulator::SimulatorCore,
    pub account_state: SimAccountState,
    pub control_state: ManualControlState,
}
