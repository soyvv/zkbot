//! Snowflake order-ID generator.
//!
//! Format: timestamp_ms (41 bits) | instance_id (10 bits) | sequence (12 bits)
//!
//! `instance_id` is Pilot-granted in production (from `enriched_config.instance_id`).
//! `ZK_CLIENT_INSTANCE_ID` is a dev/test fallback only.

use std::sync::atomic::{AtomicI64, AtomicU16, Ordering};

use crate::error::SdkError;

const MAX_INSTANCE_ID: u16 = 1023; // 10 bits
const MAX_SEQUENCE: u16    = 4095; // 12 bits
const INSTANCE_ID_SHIFT: i64 = 12;
const TIMESTAMP_SHIFT: i64   = 22; // 10 + 12

/// Snowflake ID generator. Thread-safe.
pub struct SnowflakeIdGen {
    instance_id: u16,
    sequence:    AtomicU16,
    last_ms:     AtomicI64,
}

impl SnowflakeIdGen {
    /// Create a new generator. Returns `Err` if `instance_id > 1023`.
    pub fn new(instance_id: u16) -> Result<Self, SdkError> {
        if instance_id > MAX_INSTANCE_ID {
            return Err(SdkError::InstanceIdOutOfRange(instance_id));
        }
        Ok(Self {
            instance_id,
            sequence: AtomicU16::new(0),
            last_ms:  AtomicI64::new(0),
        })
    }

    /// Generate the next Snowflake ID.
    ///
    /// Thread-safe: CAS loop on `last_ms` serialises the first call per millisecond.
    pub fn next_id(&self) -> i64 {
        loop {
            let now_ms = current_ms();
            let last   = self.last_ms.load(Ordering::Acquire);

            if now_ms > last {
                // New millisecond: try to claim it atomically.
                if self.last_ms
                    .compare_exchange(last, now_ms, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.sequence.store(0, Ordering::Release);
                    return compose(now_ms, self.instance_id, 0);
                }
                // Another thread won the CAS; retry to pick up the new last_ms.
                continue;
            }

            // Same millisecond: increment sequence.
            let seq = self.sequence.fetch_add(1, Ordering::AcqRel);
            let next_seq = seq.wrapping_add(1);
            if next_seq <= MAX_SEQUENCE {
                return compose(now_ms, self.instance_id, next_seq);
            }

            // Sequence exhausted this ms; spin until the clock advances.
            while current_ms() <= self.last_ms.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        }
    }
}

#[inline]
fn current_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[inline]
fn compose(timestamp_ms: i64, instance_id: u16, sequence: u16) -> i64 {
    (timestamp_ms << TIMESTAMP_SHIFT)
        | ((instance_id as i64) << INSTANCE_ID_SHIFT)
        | (sequence as i64)
}
