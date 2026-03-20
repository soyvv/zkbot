//! OMS state rehydration — query OMS for open orders, balances, positions
//! and inject them as init data into the engine before startup.
//!
//! Currently stubbed: returns empty state. Will be implemented when the
//! TradingClient exposes query APIs for initial state.

use tracing::info;

use zk_trading_sdk_rs::client::TradingClient;

/// Rehydrated state to inject into the engine before startup.
#[derive(Debug, Default)]
pub struct RehydratedState {
    // TODO: open orders, positions, balances
    // These will be Box<dyn Any + Send> init_data for LiveEngine::set_init_data().
}

/// Query OMS for initial state.
///
/// Currently stubbed — returns empty state.
pub async fn rehydrate(_client: &TradingClient, _account_ids: &[i64]) -> RehydratedState {
    // TODO(phase5): Query open orders, balances, positions per account
    // via TradingClient query APIs when available.
    info!("rehydration: stubbed — starting with empty state");
    RehydratedState::default()
}
