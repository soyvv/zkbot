use std::collections::HashMap;

use crate::models::Reservation;

/// Manages per-order cash/inventory reservations.
///
/// Reservations are local execution guardrails — they prevent over-ordering
/// between exchange sync points. They are NOT canonical account balance.
pub struct ReservationManager {
    /// order_id → Reservation
    reservations: HashMap<i64, Reservation>,
    /// account_id → symbol → total reserved qty (fast lookup index)
    account_totals: HashMap<i64, HashMap<String, f64>>,
}

impl ReservationManager {
    pub fn new() -> Self {
        Self {
            reservations: HashMap::new(),
            account_totals: HashMap::new(),
        }
    }

    /// Create a reservation for an order.
    pub fn reserve(
        &mut self,
        order_id: i64,
        account_id: i64,
        symbol: String,
        qty: f64,
        is_position: bool,
    ) -> Result<(), String> {
        if self.reservations.contains_key(&order_id) {
            return Err(format!("reservation already exists for order {}", order_id));
        }
        self.reservations.insert(
            order_id,
            Reservation {
                order_id,
                account_id,
                symbol: symbol.clone(),
                reserved_qty: qty,
                is_position,
            },
        );
        *self
            .account_totals
            .entry(account_id)
            .or_default()
            .entry(symbol)
            .or_insert(0.0) += qty;
        Ok(())
    }

    /// Fully release a reservation (on cancel or terminal state).
    pub fn release(&mut self, order_id: i64) {
        if let Some(res) = self.reservations.remove(&order_id) {
            if let Some(sym_map) = self.account_totals.get_mut(&res.account_id) {
                if let Some(total) = sym_map.get_mut(&res.symbol) {
                    *total -= res.reserved_qty;
                    if *total <= 0.0 {
                        sym_map.remove(&res.symbol);
                    }
                }
            }
        }
    }

    /// Partially release a reservation (on partial fill).
    pub fn release_partial(&mut self, order_id: i64, filled_qty: f64) {
        if let Some(res) = self.reservations.get_mut(&order_id) {
            let release = filled_qty.min(res.reserved_qty);
            res.reserved_qty -= release;
            if let Some(sym_map) = self.account_totals.get_mut(&res.account_id) {
                if let Some(total) = sym_map.get_mut(&res.symbol) {
                    *total -= release;
                    if *total <= 0.0 {
                        sym_map.remove(&res.symbol);
                    }
                }
            }
            // Remove empty reservation
            if res.reserved_qty <= 0.0 {
                self.reservations.remove(&order_id);
            }
        }
    }

    /// Total reserved qty for an account+symbol across all active orders.
    pub fn total_reserved(&self, account_id: i64, symbol: &str) -> f64 {
        self.account_totals
            .get(&account_id)
            .and_then(|m| m.get(symbol))
            .copied()
            .unwrap_or(0.0)
    }

    /// Check if `required` qty is available given `available` minus existing reservations.
    pub fn check_available(
        &self,
        account_id: i64,
        symbol: &str,
        required: f64,
        available: f64,
    ) -> Result<(), String> {
        let reserved = self.total_reserved(account_id, symbol);
        let effective = available - reserved;
        if effective < required {
            Err(format!(
                "Not enough available balance for {}: need {}, have {} (available {} - reserved {})",
                symbol, required, effective, available, reserved,
            ))
        } else {
            Ok(())
        }
    }
}

impl Default for ReservationManager {
    fn default() -> Self {
        Self::new()
    }
}
