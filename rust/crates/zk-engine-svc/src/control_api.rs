//! Control API — gRPC handlers for Start/Stop/Pause/Resume.
//!
//! Control commands are sent through a dedicated high-priority channel
//! (`control_tx`) that is drained before the grace queue by the priority
//! forwarder. This ensures operator commands are never blocked behind
//! data-plane backlog.

use std::time::Duration;

use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing::info;

use zk_engine_rs::{ControlCommand, EngineEvent, EventEnvelope};
use zk_proto_rs::zk::common::v1::CommandAck;
use zk_proto_rs::zk::engine::v1::{
    PauseEngineRequest, ResumeEngineRequest, StartEngineRequest, StopEngineRequest,
};

/// Timeout for the slow-path send when the control queue is momentarily full.
const CONTROL_SEND_TIMEOUT: Duration = Duration::from_secs(1);

/// Shared state needed by control API handlers.
pub struct ControlApiState {
    /// Sender into the priority control channel (not the grace queue).
    pub control_tx: mpsc::Sender<EventEnvelope>,
    /// Engine instance ID for validation.
    pub engine_id: String,
}

impl ControlApiState {
    /// Handle Pause RPC.
    pub async fn pause(
        &self,
        request: Request<PauseEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        let req = request.into_inner();
        info!(instance_id = %req.instance_id, "gRPC Pause");

        let cmd = ControlCommand::Pause {
            reason: "gRPC Pause request".into(),
        };
        self.send_control(cmd).await
    }

    /// Handle Resume RPC.
    pub async fn resume(
        &self,
        request: Request<ResumeEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        let req = request.into_inner();
        info!(instance_id = %req.instance_id, "gRPC Resume");

        let cmd = ControlCommand::Resume {
            reason: "gRPC Resume request".into(),
        };
        self.send_control(cmd).await
    }

    /// Handle Stop RPC.
    pub async fn stop(
        &self,
        request: Request<StopEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        let req = request.into_inner();
        info!(instance_id = %req.instance_id, reason = %req.reason, "gRPC Stop");

        let cmd = ControlCommand::Stop { reason: req.reason };
        self.send_control(cmd).await
    }

    /// Handle Start RPC.
    ///
    /// Currently a no-op — the engine starts on process boot.
    /// Future: hot-reload strategy config.
    pub async fn start(
        &self,
        request: Request<StartEngineRequest>,
    ) -> Result<Response<CommandAck>, Status> {
        let req = request.into_inner();
        info!(instance_id = %req.instance_id, "gRPC Start (no-op — engine starts on boot)");

        Ok(Response::new(CommandAck {
            success: true,
            error: None,
            request_id: String::new(),
            idempotency_key: String::new(),
        }))
    }

    /// Send a control command through the priority control channel.
    ///
    /// Fast path: `try_send` (non-blocking).
    /// Slow path: `send().await` with a 1-second timeout.
    /// On timeout: return `RESOURCE_EXHAUSTED` so the operator gets a clear error.
    async fn send_control(&self, cmd: ControlCommand) -> Result<Response<CommandAck>, Status> {
        let envelope = EventEnvelope::now(EngineEvent::Control(cmd));

        match self.control_tx.try_send(envelope) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(envelope)) => {
                match tokio::time::timeout(CONTROL_SEND_TIMEOUT, self.control_tx.send(envelope))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        return Err(Status::unavailable("engine control channel closed"));
                    }
                    Err(_) => {
                        return Err(Status::resource_exhausted(
                            "control queue full — command timed out",
                        ));
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(Status::unavailable("engine control channel closed"));
            }
        }

        Ok(Response::new(CommandAck {
            success: true,
            error: None,
            request_id: String::new(),
            idempotency_key: String::new(),
        }))
    }
}
