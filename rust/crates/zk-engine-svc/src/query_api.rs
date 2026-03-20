//! Query API — gRPC handlers for GetStatus and HealthCheck.
//!
//! Reads from the engine's lock-free `EngineReadReplica` snapshot.

use std::time::Instant;

use tonic::{Request, Response, Status};

use zk_engine_rs::{EngineReadReplica, LifecycleState};
use zk_proto_rs::zk::common::v1::{DummyRequest, ServiceHealthResponse};
use zk_proto_rs::zk::engine::v1::{EngineStatusRequest, EngineStatusResponse};

/// Shared state needed by query API handlers.
pub struct QueryApiState {
    /// Lock-free read replica of the engine snapshot.
    pub replica: EngineReadReplica,
    /// Engine instance ID.
    pub engine_id: String,
    /// Service start time for health check uptime.
    pub start_time: Instant,
}

impl QueryApiState {
    /// Handle GetStatus RPC.
    pub async fn get_status(
        &self,
        _request: Request<EngineStatusRequest>,
    ) -> Result<Response<EngineStatusResponse>, Status> {
        let snap = self.replica.load();

        let state_str = match snap.lifecycle_state {
            LifecycleState::Running => "RUNNING",
            LifecycleState::Paused | LifecycleState::Pausing => "PAUSED",
            LifecycleState::Starting | LifecycleState::Resuming => "RUNNING",
            LifecycleState::Stopping | LifecycleState::Stopped => "IDLE",
            LifecycleState::Degraded => "ERROR",
            LifecycleState::Fenced | LifecycleState::Failed => "ERROR",
        };

        let detail = format!(
            "events={} orders={} coalesced={} avg_decision={}ns paused={}",
            snap.events_processed,
            snap.orders_dispatched,
            snap.ticks_coalesced,
            snap.avg_decision_ns,
            snap.paused,
        );

        Ok(Response::new(EngineStatusResponse {
            instance_id: self.engine_id.clone(),
            execution_id: snap.execution_id.clone(),
            state: state_str.into(),
            detail,
            uptime_ms: snap.uptime_ms,
        }))
    }

    /// Handle HealthCheck RPC.
    pub async fn health_check(
        &self,
        _request: Request<DummyRequest>,
    ) -> Result<Response<ServiceHealthResponse>, Status> {
        let snap = self.replica.load();

        let (status, detail) = match snap.lifecycle_state {
            LifecycleState::Running | LifecycleState::Paused => {
                ("SERVING".into(), "engine healthy".into())
            }
            LifecycleState::Starting | LifecycleState::Resuming | LifecycleState::Pausing => (
                "SERVING".into(),
                format!("state: {:?}", snap.lifecycle_state),
            ),
            _ => (
                "NOT_SERVING".into(),
                format!("state: {:?}", snap.lifecycle_state),
            ),
        };

        let uptime_ms = self.start_time.elapsed().as_millis() as i64;

        Ok(Response::new(ServiceHealthResponse {
            service_id: self.engine_id.clone(),
            status,
            detail,
            uptime_ms,
        }))
    }
}
