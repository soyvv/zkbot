use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
