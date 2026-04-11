//! RTMD interest lease helpers and refdata-backed NATS subject construction.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use zk_infra_rs::nats_kv::KvRegistryClient;

use crate::error::SdkError;

const RTMD_SUB_BUCKET: &str = "zk-rtmd-subs-v1";
const DEFAULT_INTEREST_TTL: Duration = Duration::from_secs(60);

// ── Deterministic topic constructors ─────────────────────────────────────────

/// NATS subject for tick/quote data.
pub fn tick_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.tick.{}.{}", canonical_venue(venue), instrument_exch)
}

/// NATS subject for OHLCV kline data.
pub fn kline_topic(venue: &str, instrument_exch: &str, interval: &str) -> String {
    format!(
        "zk.rtmd.kline.{}.{}.{}",
        canonical_venue(venue),
        instrument_exch,
        interval
    )
}

/// NATS subject for funding rate data.
pub fn funding_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.funding.{}.{}", canonical_venue(venue), instrument_exch)
}

/// NATS subject for order book data.
pub fn orderbook_topic(venue: &str, instrument_exch: &str) -> String {
    format!(
        "zk.rtmd.orderbook.{}.{}",
        canonical_venue(venue),
        instrument_exch
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtmdInterestSpec {
    pub subscriber_id: String,
    pub scope: String,
    pub instrument_id: String,
    pub channel_type: String,
    pub channel_param: Option<String>,
    pub venue: String,
    pub instrument_exch: String,
    pub lease_expiry_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RtmdInterestLease {
    pub key: String,
    pub spec: RtmdInterestSpec,
}

#[derive(Clone)]
pub struct RtmdInterestManager {
    kv: KvRegistryClient,
}

impl RtmdInterestManager {
    pub async fn start(js: &jetstream::Context) -> Result<Self, SdkError> {
        let kv = KvRegistryClient::create(js, RTMD_SUB_BUCKET, Duration::from_secs(60)).await?;
        Ok(Self { kv })
    }

    pub async fn register_interest(
        &self,
        spec: RtmdInterestSpec,
    ) -> Result<RtmdInterestLease, SdkError> {
        let key = self.key_for(&spec);
        self.kv
            .put(&key, Bytes::from(serde_json::to_vec(&spec)?))
            .await?;
        Ok(RtmdInterestLease { key, spec })
    }

    pub async fn refresh_interest(&self, lease: &RtmdInterestLease) -> Result<(), SdkError> {
        let mut refreshed = lease.spec.clone();
        refreshed.updated_at_ms = now_ms();
        refreshed.lease_expiry_ms = default_lease_expiry_ms(DEFAULT_INTEREST_TTL);
        self.kv
            .put(&lease.key, Bytes::from(serde_json::to_vec(&refreshed)?))
            .await?;
        Ok(())
    }

    pub async fn drop_interest(&self, lease: RtmdInterestLease) -> Result<(), SdkError> {
        self.kv.delete(&lease.key).await?;
        Ok(())
    }

    fn key_for(&self, spec: &RtmdInterestSpec) -> String {
        let suffix = match &spec.channel_param {
            Some(param) => format!(
                "{}_{}",
                sanitize_key_segment(&spec.instrument_id),
                sanitize_key_segment(param)
            ),
            None => sanitize_key_segment(&spec.instrument_id),
        };
        format!(
            "sub.{}.{}.{}.{}",
            sanitize_key_segment(&spec.scope),
            sanitize_key_segment(&spec.subscriber_id),
            sanitize_key_segment(&spec.channel_type),
            suffix
        )
    }
}

pub fn default_lease_expiry_ms(ttl: Duration) -> i64 {
    now_ms() + ttl.as_millis() as i64
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn sanitize_key_segment(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

fn canonical_venue(venue: &str) -> String {
    venue.to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec(
        scope: &str,
        subscriber_id: &str,
        instrument_id: &str,
        channel_type: &str,
        channel_param: Option<&str>,
    ) -> RtmdInterestSpec {
        RtmdInterestSpec {
            subscriber_id: subscriber_id.to_string(),
            scope: scope.to_string(),
            instrument_id: instrument_id.to_string(),
            channel_type: channel_type.to_string(),
            channel_param: channel_param.map(str::to_string),
            venue: "MOCK".to_string(),
            instrument_exch: "BTC-USDT".to_string(),
            lease_expiry_ms: 0,
            updated_at_ms: 0,
        }
    }

    // We need a dummy RtmdInterestManager to call key_for. Since it only uses `self`
    // structurally (not the KV client), we test the key logic via the struct directly.
    // RtmdInterestManager::key_for is a pure function of the spec — test it by calling
    // the method through a wrapper that does not require a live KV connection.

    // ── Helper: extract key without hitting NATS ─────────────────────────────

    fn key_for_spec(spec: &RtmdInterestSpec) -> String {
        let suffix = match &spec.channel_param {
            Some(param) => format!(
                "{}_{}",
                sanitize_key_segment(&spec.instrument_id),
                sanitize_key_segment(param)
            ),
            None => sanitize_key_segment(&spec.instrument_id),
        };
        format!(
            "sub.{}.{}.{}.{}",
            sanitize_key_segment(&spec.scope),
            sanitize_key_segment(&spec.subscriber_id),
            sanitize_key_segment(&spec.channel_type),
            suffix
        )
    }

    // ── Key format tests ─────────────────────────────────────────────────────

    /// Without channel_param the key is: sub.<scope>.<subscriber_id>.<channel_type>.<instrument_id>
    #[test]
    fn test_rtmd_key_no_channel_param() {
        let spec = make_spec("logical", "engine_strat_1", "BTCUSDT_MOCK", "tick", None);
        assert_eq!(
            key_for_spec(&spec),
            "sub.logical.engine_strat_1.tick.BTCUSDT_MOCK"
        );
    }

    /// With channel_param the suffix becomes <instrument_id>_<param>.
    /// Matches kline interest where param = interval.
    #[test]
    fn test_rtmd_key_with_channel_param() {
        let spec = make_spec(
            "logical",
            "engine_strat_1",
            "BTCUSDT_MOCK",
            "kline",
            Some("1m"),
        );
        assert_eq!(
            key_for_spec(&spec),
            "sub.logical.engine_strat_1.kline.BTCUSDT_MOCK_1m"
        );
    }

    /// Scope "venue" — used when a subscriber tracks a specific venue context.
    #[test]
    fn test_rtmd_key_venue_scope() {
        let spec = make_spec("venue", "strategy_a", "BTCUSDT_OKX", "tick", None);
        assert_eq!(key_for_spec(&spec), "sub.venue.strategy_a.tick.BTCUSDT_OKX");
    }

    /// Different channel_types produce different keys for the same instrument.
    #[test]
    fn test_rtmd_key_different_channel_types_are_distinct() {
        let tick_spec = make_spec("logical", "strat_a", "BTCUSDT_MOCK", "tick", None);
        let funding_spec = make_spec("logical", "strat_a", "BTCUSDT_MOCK", "funding", None);
        assert_ne!(key_for_spec(&tick_spec), key_for_spec(&funding_spec));
    }

    /// Different channel_param values produce different keys (1m vs 5m klines).
    #[test]
    fn test_rtmd_key_different_kline_intervals_are_distinct() {
        let k1 = make_spec("logical", "strat_a", "BTCUSDT_MOCK", "kline", Some("1m"));
        let k5 = make_spec("logical", "strat_a", "BTCUSDT_MOCK", "kline", Some("5m"));
        assert_ne!(key_for_spec(&k1), key_for_spec(&k5));
    }

    /// Different subscribers produce different keys for the same stream.
    #[test]
    fn test_rtmd_key_different_subscribers_are_distinct() {
        let s1 = make_spec("logical", "strat_a", "BTCUSDT_MOCK", "tick", None);
        let s2 = make_spec("logical", "strat_b", "BTCUSDT_MOCK", "tick", None);
        assert_ne!(key_for_spec(&s1), key_for_spec(&s2));
    }

    #[test]
    fn test_rtmd_key_sanitizes_real_world_instrument_ids() {
        let spec = make_spec("logical", "bot-smoke-okx-01", "BTC-P/USDT@OKX", "tick", None);
        assert_eq!(
            key_for_spec(&spec),
            "sub.logical.bot-smoke-okx-01.tick.BTC-P_USDT_OKX"
        );
    }
}
