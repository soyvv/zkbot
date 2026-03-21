//! Integration test for OKX RTMD adapter.
//! Requires network access to OKX public endpoints.
//! Run with: cargo test -p zk-rtmd-rs --features venue-okx -- --ignored

#[cfg(feature = "venue-okx")]
mod okx_tests {
    use std::time::Duration;

    use zk_rtmd_rs::types::{ChannelType, RtmdEvent, RtmdSubscriptionSpec, StreamKey};
    use zk_rtmd_rs::venue::okx::OkxRtmdAdapter;
    use zk_rtmd_rs::venue_adapter::RtmdVenueAdapter;
    use zk_venue_okx::config::OkxConfig;

    #[tokio::test]
    #[ignore]
    async fn test_okx_rtmd_connect_and_tick() {
        let config = OkxConfig::public_only(true)
            .resolve_secrets()
            .expect("resolve_secrets failed");
        let adapter = OkxRtmdAdapter::new(config);
        adapter.connect().await.expect("connect failed");

        let spec = RtmdSubscriptionSpec {
            stream_key: StreamKey {
                instrument_code: "BTC-USDT".into(),
                channel: ChannelType::Tick,
            },
            instrument_exch: "BTC-USDT".into(),
            venue: "okx".into(),
        };
        adapter.subscribe(spec).await.expect("subscribe failed");

        let event = tokio::time::timeout(Duration::from_secs(15), adapter.next_event())
            .await
            .expect("timeout waiting for tick")
            .expect("event error");

        match event {
            RtmdEvent::Tick(t) => {
                assert_eq!(t.instrument_code, "BTC-USDT");
                assert!(t.latest_trade_price > 0.0, "expected positive price");
            }
            other => panic!("expected Tick, got {:?}", other),
        }

        assert_eq!(
            adapter.instrument_exch_for("BTC-USDT"),
            Some("BTC-USDT".to_string())
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_okx_rtmd_rest_ticker() {
        let config = OkxConfig::public_only(false)
            .resolve_secrets()
            .expect("resolve_secrets failed");
        let adapter = OkxRtmdAdapter::new(config);
        // No connect needed for REST queries, but we need the exch_map populated.
        // Subscribe first to populate the mapping, then query.
        // For REST-only, we can just query directly using instrument_code as inst_id.
        adapter.connect().await.expect("connect failed");

        let tick = adapter
            .query_current_tick("BTC-USDT")
            .await
            .expect("query_current_tick failed");
        assert_eq!(tick.instrument_code, "BTC-USDT");
        assert!(tick.latest_trade_price > 0.0);
    }
}
