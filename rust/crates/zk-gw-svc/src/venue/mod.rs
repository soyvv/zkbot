pub mod ibkr;
pub mod oanda;
pub mod okx;
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
    // ── Manifest-driven Python venue loading ────────────────────────────
    #[cfg(feature = "python-venue")]
    if let Some(ref venue_root) = cfg.venue_root {
        use std::path::PathBuf;
        let root = PathBuf::from(venue_root);
        // Fail-fast: if venue_root is set, manifest must load successfully.
        let manifest = zk_pyo3_bridge::manifest::load_manifest(&root, &cfg.venue)?;
        // Capability miss is OK — venue may only declare refdata/rtmd, not gw.
        if let Ok(cap) = zk_pyo3_bridge::manifest::resolve_capability(
            &manifest,
            zk_pyo3_bridge::manifest::CAP_GW,
        ) {
            if cap.language == "python" {
                zk_pyo3_bridge::manifest::validate_config(
                    &root,
                    &cfg.venue,
                    cap,
                    &cfg.venue_config,
                )?;
                let ep = zk_pyo3_bridge::manifest::parse_python_entrypoint(&cap.entrypoint)?;
                let rt = zk_pyo3_bridge::py_runtime::PyRuntime::initialize(&root)?;
                let handle = rt.load_class(&ep, cfg.venue_config.clone(), Some(&cfg.venue))?;
                let adapter: Arc<dyn VenueAdapter> =
                    Arc::new(zk_pyo3_bridge::gw_adapter::PyVenueAdapter::new(handle));
                return Ok(BuiltVenue {
                    adapter,
                    simulator_handles: None,
                });
            }
        }
    }

    match cfg.venue.as_str() {
        "simulator" => {
            let balances = GwSvcConfig::parse_balances(&cfg.mock_balances);
            let match_policy = simulator::make_match_policy(&cfg.match_policy);
            let sim_core = zk_sim_core::simulator::SimulatorCore::new(match_policy, "SIM");
            let account_state = simulator::SimAccountState::new(cfg.account_id, balances);
            let control_state = simulator::ManualControlState::new(cfg.match_policy.clone());

            let sim_state = Arc::new(tokio::sync::Mutex::new(simulator::SimulatorState {
                sim_core,
                account_state,
                control_state,
            }));

            let sim_adapter = Arc::new(simulator::SimulatorVenueAdapter::new(Arc::clone(
                &sim_state,
            )));
            let adapter: Arc<dyn VenueAdapter> = Arc::clone(&sim_adapter) as _;

            Ok(BuiltVenue {
                adapter,
                simulator_handles: Some(SimulatorHandles {
                    sim_state,
                    sim_adapter,
                }),
            })
        }
        "okx" => {
            let okx_cfg: zk_venue_okx::config::OkxConfig =
                serde_json::from_value(cfg.venue_config.clone())
                    .map_err(|e| anyhow::anyhow!("invalid OKX venue config: {e}"))?;
            let okx_cfg = okx_cfg.resolve_secrets()?;

            let adapter = Arc::new(okx::adapter::OkxVenueAdapter::new(
                okx_cfg,
                cfg.gw_id.clone(),
                cfg.account_id,
            ));

            Ok(BuiltVenue {
                adapter,
                simulator_handles: None,
            })
        }
        "ibkr" => Err(anyhow::anyhow!(
            "ibkr venue requires manifest-driven loading: set ZK_VENUE_ROOT to the venue-integrations directory"
        )),
        "oanda" => Err(anyhow::anyhow!(
            "venue adaptor placeholder exists for oanda but implementation is not wired yet"
        )),
        other => Err(anyhow::anyhow!("unsupported venue: {other}")),
    }
}
