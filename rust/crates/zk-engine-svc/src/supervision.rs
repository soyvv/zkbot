//! Supervision — shutdown coordination, fencing detection.
//!
//! Manages the lifecycle between the engine event loop, gRPC server,
//! KV heartbeat, and OS signals.

use tokio::sync::mpsc;
use tracing::{info, warn};

use zk_engine_rs::{ControlCommand, EngineEvent, EventEnvelope};
use zk_infra_rs::service_registry::ServiceRegistration;

/// Outcome of the supervision select loop.
pub enum ShutdownReason {
    /// Clean shutdown requested via OS signal (SIGTERM/SIGINT).
    Signal,
    /// CAS heartbeat detected a conflicting writer (fenced by another instance).
    Fenced,
}

/// Wait for shutdown trigger: either an OS signal or a KV fencing event.
///
/// Returns the reason so the caller can decide whether to deregister.
pub async fn wait_for_shutdown(registration: &mut ServiceRegistration) -> ShutdownReason {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received (Ctrl-C / SIGTERM)");
            ShutdownReason::Signal
        }
        _ = registration.wait_fenced() => {
            warn!("KV fencing detected — another instance owns this identity");
            ShutdownReason::Fenced
        }
    }
}

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
