// Re-export simulation types from zk-sim-core
pub use zk_sim_core::models::{MatchResult, SimOrder, SimResult};

/// Backtester OMS output: one OMS event to re-queue.
#[derive(Debug, Clone)]
pub struct BtOmsOutput {
    pub ts_ms: i64,
    pub order_update: Option<zk_proto_rs::zk::oms::v1::OrderUpdateEvent>,
    pub balance_update: Option<zk_proto_rs::zk::oms::v1::BalanceUpdateEvent>,
    pub position_update: Option<zk_proto_rs::zk::oms::v1::PositionUpdateEvent>,
}
