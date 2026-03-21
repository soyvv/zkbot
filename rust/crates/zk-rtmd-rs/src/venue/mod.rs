pub mod simulator;

#[cfg(feature = "venue-okx")]
pub mod okx;

use std::sync::Arc;
use crate::venue_adapter::RtmdVenueAdapter;
use crate::types::RtmdError;

/// Build a venue adapter from a venue name and optional config JSON.
pub fn build_adapter(
    venue: &str,
    config: &serde_json::Value,
) -> Result<Arc<dyn RtmdVenueAdapter>, RtmdError> {
    let _ = config; // used only by venue-specific arms
    match venue {
        "simulator" => Ok(Arc::new(simulator::SimRtmdAdapter::new())),
        #[cfg(feature = "venue-okx")]
        "okx" => {
            let okx_cfg: zk_venue_okx::config::OkxConfig =
                serde_json::from_value(config.clone())
                    .map_err(|e| RtmdError::Internal(format!("OKX config: {e}")))?;
            let okx_cfg = okx_cfg
                .resolve_secrets()
                .map_err(|e| RtmdError::Internal(format!("OKX secrets: {e}")))?;
            Ok(Arc::new(okx::OkxRtmdAdapter::new(okx_cfg)))
        }
        other => Err(RtmdError::Internal(format!("unknown venue: {other}"))),
    }
}
