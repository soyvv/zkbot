pub mod simulator;

use std::sync::Arc;

use crate::config::GwSvcConfig;
use crate::venue_adapter::VenueAdapter;

/// Build result containing the adapter and optional simulator-specific handles.
pub struct BuiltVenue {
    pub adapter: Arc<dyn VenueAdapter>,
    /// Simulator-only: shared state + concrete adapter for admin service wiring.
    pub simulator_handles: Option<SimulatorHandles>,
}

/// Simulator-specific handles exposed for admin gRPC wiring.
pub struct SimulatorHandles {
    pub sim_state: Arc<tokio::sync::Mutex<simulator::SimulatorState>>,
    pub sim_adapter: Arc<simulator::SimulatorVenueAdapter>,
}

/// Factory: build the venue adapter for the configured venue.
pub async fn build_adapter(cfg: &GwSvcConfig) -> anyhow::Result<BuiltVenue> {
    match cfg.venue.as_str() {
        "simulator" => {
            let balances = GwSvcConfig::parse_balances(&cfg.mock_balances);
            let match_policy = simulator::make_match_policy(&cfg.match_policy);
            let sim_core =
                zk_sim_core::simulator::SimulatorCore::new(match_policy, "SIM");
            let account_state =
                simulator::SimAccountState::new(cfg.account_id, balances);
            let control_state =
                simulator::ManualControlState::new(cfg.match_policy.clone());

            let sim_state = Arc::new(tokio::sync::Mutex::new(
                simulator::SimulatorState {
                    sim_core,
                    account_state,
                    control_state,
                },
            ));

            let sim_adapter = Arc::new(simulator::SimulatorVenueAdapter::new(
                Arc::clone(&sim_state),
            ));
            let adapter: Arc<dyn VenueAdapter> = Arc::clone(&sim_adapter) as _;

            Ok(BuiltVenue {
                adapter,
                simulator_handles: Some(SimulatorHandles {
                    sim_state,
                    sim_adapter,
                }),
            })
        }
        other => Err(anyhow::anyhow!("unsupported venue: {other}")),
    }
}
