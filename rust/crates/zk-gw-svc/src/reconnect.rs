use std::fmt;

/// Gateway connection state machine.
///
/// Lifecycle: Starting → Connecting → Live → (Degraded → Resyncing → Live) → Stopping
///
/// For the simulator adapter, the lifecycle is always Starting → Connecting → Live.
/// The full state machine exists for real venue adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayState {
    /// Process initializing, not yet connected to venue.
    Starting,
    /// Establishing venue connectivity.
    Connecting,
    /// Fully connected and publishing events.
    Live,
    /// Venue connection lost; incremental events not trusted.
    Degraded,
    /// Re-establishing connectivity and running compensating queries.
    Resyncing,
    /// Shutting down.
    Stopping,
}

impl fmt::Display for GatewayState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayState::Starting => write!(f, "STARTING"),
            GatewayState::Connecting => write!(f, "CONNECTING"),
            GatewayState::Live => write!(f, "LIVE"),
            GatewayState::Degraded => write!(f, "DEGRADED"),
            GatewayState::Resyncing => write!(f, "RESYNCING"),
            GatewayState::Stopping => write!(f, "STOPPING"),
        }
    }
}

impl GatewayState {
    /// Whether the gateway is in a state where it can accept commands.
    pub fn is_serving(&self) -> bool {
        matches!(self, GatewayState::Live | GatewayState::Degraded)
    }

    /// Valid state transitions per the unified reconnect model.
    pub fn can_transition_to(&self, next: GatewayState) -> bool {
        use GatewayState::*;
        matches!(
            (self, next),
            (Starting, Connecting)
                | (Connecting, Live)
                | (Connecting, Stopping)
                | (Live, Degraded)
                | (Live, Stopping)
                | (Degraded, Resyncing)
                | (Degraded, Stopping)
                | (Resyncing, Live)
                | (Resyncing, Degraded)
                | (Resyncing, Stopping)
        )
    }
}

/// Maps gateway state to the proto GatewayEventType integer.
pub fn state_to_gw_event_type(state: GatewayState) -> i32 {
    use zk_proto_rs::zk::exch_gw::v1::GatewayEventType;
    match state {
        GatewayState::Starting | GatewayState::Connecting | GatewayState::Live => {
            GatewayEventType::GwEventStarted as i32
        }
        GatewayState::Degraded => GatewayEventType::GwEventExchDisconnected as i32,
        GatewayState::Resyncing => GatewayEventType::GwEventStarted as i32,
        GatewayState::Stopping => GatewayEventType::GwEventExchDisconnected as i32,
    }
}
