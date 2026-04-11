use zk_proto_rs::zk::common::v1::{BuySellType, OpenCloseType};
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, StrategyOrder},
    strategy::Strategy,
};

use zk_backtest_rs::testutils::{
    instrument_refdata, StrategyTestBuilder, TestEventSequenceBuilder, TestMatchPolicyKind,
};

struct BuyOnTick {
    placed: bool,
}

impl Strategy for BuyOnTick {
    fn on_tick(
        &mut self,
        _tick: &zk_proto_rs::zk::rtmd::v1::TickData,
        _ctx: &StrategyContext,
    ) -> Vec<SAction> {
        if self.placed {
            return vec![];
        }
        self.placed = true;
        vec![SAction::PlaceOrder(StrategyOrder {
            order_id: 1,
            symbol: "BTC/USD@SIM1".to_string(),
            price: 100.0,
            qty: 1.0,
            side: BuySellType::BsBuy as i32,
            open_close_type: OpenCloseType::OcOpen as i32,
            account_id: 101,
        })]
    }
}

#[test]
fn test_event_sequence_builder_and_harness() {
    let events = TestEventSequenceBuilder::new()
        .start_at_ms(1_000)
        .add_tick_ob("BTC/USD@SIM1", 99.0, 100.0)
        .build();

    let mut harness = StrategyTestBuilder::new()
        .with_accounts(vec![101])
        .with_refdata(vec![instrument_refdata("BTC/USD@SIM1")])
        .with_match_policy(TestMatchPolicyKind::Immediate)
        .with_event_sequence(events)
        .build();

    let result = harness.run(&mut BuyOnTick { placed: false });
    assert_eq!(result.order_placements.len(), 1);
    assert_eq!(result.trades.len(), 1);
}
