pub mod simulator;

use std::sync::Arc;
use crate::venue_adapter::RtmdVenueAdapter;
use crate::types::RtmdError;

/// Build a venue adapter from a venue name string.
pub fn build_adapter(venue: &str) -> Result<Arc<dyn RtmdVenueAdapter>, RtmdError> {
    match venue {
        "simulator" => Ok(Arc::new(simulator::SimRtmdAdapter::new())),
        other => Err(RtmdError::Internal(format!("unknown venue: {other}"))),
    }
}
