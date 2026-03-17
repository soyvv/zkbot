use std::collections::HashMap;

use crate::models_v2::{ReservationRecordV2, ReserveKind};

/// Manages per-order cash/inventory reservations using integer IDs.
///
/// Reservations are local execution guardrails — they prevent over-ordering
/// between exchange sync points. They are NOT canonical account balance.
pub struct ReservationStoreV2 {
    /// order_id → reservation
    reservations: HashMap<i64, ReservationRecordV2>,
    /// (account_id, target_id) → total reserved qty
    account_totals: HashMap<(i64, u32), f64>,
}

impl ReservationStoreV2 {
    pub fn new() -> Self {
        Self {
            reservations: HashMap::new(),
            account_totals: HashMap::new(),
        }
    }

    /// Create a reservation for an order. Fails if a reservation already exists for this order_id.
    pub fn reserve(
        &mut self,
        order_id: i64,
        account_id: i64,
        kind: ReserveKind,
        target_id: u32,
        qty: f64,
    ) -> Result<(), String> {
        if self.reservations.contains_key(&order_id) {
            return Err(format!("reservation already exists for order {}", order_id));
        }
        self.reservations.insert(
            order_id,
            ReservationRecordV2 {
                order_id,
                account_id,
                kind,
                target_id,
                reserved_qty: qty,
            },
        );
        *self
            .account_totals
            .entry((account_id, target_id))
            .or_insert(0.0) += qty;
        Ok(())
    }

    /// Fully release a reservation (on cancel or terminal state).
    pub fn release(&mut self, order_id: i64) {
        if let Some(res) = self.reservations.remove(&order_id) {
            if let Some(total) = self.account_totals.get_mut(&(res.account_id, res.target_id)) {
                *total -= res.reserved_qty;
                if *total <= 0.0 {
                    self.account_totals
                        .remove(&(res.account_id, res.target_id));
                }
            }
        }
    }

    /// Partially release a reservation (on partial fill). Reduces by filled_qty, removes if zero.
    pub fn release_partial(&mut self, order_id: i64, filled_qty: f64) {
        if let Some(res) = self.reservations.get_mut(&order_id) {
            let release = filled_qty.min(res.reserved_qty);
            res.reserved_qty -= release;
            let key = (res.account_id, res.target_id);
            if let Some(total) = self.account_totals.get_mut(&key) {
                *total -= release;
                if *total <= 0.0 {
                    self.account_totals.remove(&key);
                }
            }
            // Remove empty reservation
            if res.reserved_qty <= 0.0 {
                self.reservations.remove(&order_id);
            }
        }
    }

    /// Total reserved qty for an account+target across all active orders.
    pub fn total_reserved(&self, account_id: i64, target_id: u32) -> f64 {
        self.account_totals
            .get(&(account_id, target_id))
            .copied()
            .unwrap_or(0.0)
    }

    /// Check if `required` qty is available given `available` minus existing reservations.
    pub fn check_available(
        &self,
        account_id: i64,
        target_id: u32,
        required: f64,
        available: f64,
    ) -> Result<(), String> {
        let reserved = self.total_reserved(account_id, target_id);
        let effective = available - reserved;
        if effective < required {
            Err(format!(
                "Not enough available for target {}: need {}, have {} (available {} - reserved {})",
                target_id, required, effective, available, reserved,
            ))
        } else {
            Ok(())
        }
    }
}

impl Default for ReservationStoreV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_and_release_roundtrip() {
        let mut store = ReservationStoreV2::new();
        store
            .reserve(1, 100, ReserveKind::CashAsset, 10, 50.0)
            .unwrap();
        assert_eq!(store.total_reserved(100, 10), 50.0);

        store.release(1);
        assert_eq!(store.total_reserved(100, 10), 0.0);
        assert!(store.reservations.is_empty());
    }

    #[test]
    fn release_partial() {
        let mut store = ReservationStoreV2::new();
        store
            .reserve(1, 100, ReserveKind::InventoryInstrument, 10, 100.0)
            .unwrap();

        store.release_partial(1, 40.0);
        assert_eq!(store.total_reserved(100, 10), 60.0);
        assert!(store.reservations.contains_key(&1));

        // Release remaining
        store.release_partial(1, 60.0);
        assert_eq!(store.total_reserved(100, 10), 0.0);
        assert!(!store.reservations.contains_key(&1));
    }

    #[test]
    fn check_available_passes_and_fails() {
        let mut store = ReservationStoreV2::new();
        store
            .reserve(1, 100, ReserveKind::CashAsset, 10, 30.0)
            .unwrap();

        // available=100, reserved=30 → effective=70, need 50 → OK
        assert!(store.check_available(100, 10, 50.0, 100.0).is_ok());

        // available=100, reserved=30 → effective=70, need 80 → fail
        assert!(store.check_available(100, 10, 80.0, 100.0).is_err());
    }

    #[test]
    fn duplicate_reserve_fails() {
        let mut store = ReservationStoreV2::new();
        store
            .reserve(1, 100, ReserveKind::CashAsset, 10, 50.0)
            .unwrap();
        let result = store.reserve(1, 100, ReserveKind::CashAsset, 10, 25.0);
        assert!(result.is_err());
        // Total should remain unchanged
        assert_eq!(store.total_reserved(100, 10), 50.0);
    }
}
