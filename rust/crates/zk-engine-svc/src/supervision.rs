//! Supervision — shutdown coordination, fencing detection.
//!
//! Manages the lifecycle between the engine event loop, gRPC server,
//! KV heartbeat, and OS signals.

use tokio::sync::mpsc;
use tracing::warn;

use zk_engine_rs::{ControlCommand, EngineEvent, EventEnvelope};

/// Inject a Stop command into the engine event channel to trigger graceful drain.
pub async fn stop_engine(event_tx: &mpsc::Sender<EventEnvelope>, reason: &str) {
    let cmd = ControlCommand::Stop {
        reason: reason.to_string(),
    };
    let envelope = EventEnvelope::now(EngineEvent::Control(cmd));
    if event_tx.send(envelope).await.is_err() {
        warn!("engine event channel already closed during shutdown");
    }
}
