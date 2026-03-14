//! Service registration and CAS heartbeat for zkbot runtime instances.
//!
//! # Architecture
//!
//! Two registration modes are supported:
//!
//! ## Direct mode (`register_direct`)
//! Used without Pilot. Writes `kv_key → value` directly to NATS KV and starts a
//! CAS heartbeat loop that re-puts the entry every `heartbeat_interval` using
//! `update(revision)`.
//!
//! ## Pilot-bootstrap mode (`register_with_pilot`)
//! Sends a NATS request to `zk.bootstrap.register` with a per-instance token.
//! Pilot validates topology + token claims and returns a `BootstrapRegisterResponse`
//! (owner session id, kv_key, lock_key, instance_id, lease_ttl_ms).
//! The instance then writes KV directly and heartbeats with CAS semantics.
//! See `docs/system-redesign-plan/plan/05-registry-and-pilot-bootstrap.md`.
//!
//! ## CAS fencing
//! If another writer modifies `kv_key` (CAS conflict), the heartbeat task fires
//! the `fenced` channel and terminates. The owning service must call
//! [`ServiceRegistration::wait_fenced`] and shut down when fencing is detected.
//!
//! # Usage (direct mode)
//! ```no_run
//! use zk_infra_rs::service_registry::ServiceRegistration;
//! use bytes::Bytes;
//! use std::time::Duration;
//!
//! # async fn example(js: async_nats::jetstream::Context) -> anyhow::Result<()> {
//! let mut reg = ServiceRegistration::register_direct(
//!     &js,
//!     "svc.oms.oms_dev_1",
//!     Bytes::from(r#"{"service_type":"OMS"}"#),
//!     Duration::from_secs(10),
//! ).await?;
//!
//! // In background: tokio::select! { _ = reg.wait_fenced() => { /* shut down */ } }
//!
//! // ... service runs ...
//! reg.deregister().await.ok();
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use bytes::Bytes;
use prost::Message;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info;

use crate::nats_kv::{KvRegistryClient, REGISTRY_BUCKET};
use zk_proto_rs::zk::pilot::v1::{
    BootstrapDeregisterRequest, BootstrapRegisterRequest, BootstrapRegisterResponse,
};

// ── Grant ─────────────────────────────────────────────────────────────────────

/// Opaque bootstrap grant — locally generated in direct mode, Pilot-issued in
/// Pilot mode.
#[derive(Debug, Clone)]
pub struct RegistrationGrant {
    /// Unique session identifier for this registration instance.
    pub owner_session_id: String,
    /// Primary KV key written by this service (e.g. `svc.oms.oms_dev_1`).
    pub kv_key: String,
    /// Lock key used for CAS fencing (e.g. `lock.oms.oms_dev_1`).
    /// In direct mode this equals the kv_key; reserved for Pilot mode.
    pub lock_key: String,
    /// Lease TTL in ms (informational; enforced by KV bucket `max_age`).
    pub lease_ttl_ms: u64,
    /// Scoped NATS credential string — `None` in direct mode (uses ambient connection).
    pub scoped_credential: Option<String>,
    /// Pilot-assigned Snowflake worker ID (engines only; `None` otherwise).
    pub instance_id: Option<i32>,
}

// ── ServiceRegistration ───────────────────────────────────────────────────────

/// Active registration with CAS heartbeat and fencing detection.
///
/// Drop order: call [`deregister`] before dropping to remove the KV entry
/// cleanly. If dropped without deregistering the entry expires on its TTL.
///
/// Services must also monitor fencing via [`wait_fenced`] and terminate if
/// the CAS heartbeat detects a conflicting writer.
pub struct ServiceRegistration {
    kv:     Arc<KvRegistryClient>,
    grant:  RegistrationGrant,
    _hb:    JoinHandle<()>,
    /// NATS client kept for deregister notification in Pilot mode.
    nats:   Option<async_nats::Client>,
    /// Receives `true` when the CAS heartbeat detects a conflicting writer.
    fenced: watch::Receiver<bool>,
}

impl ServiceRegistration {
    // ── Direct mode ─────────────────────────────────────────────────────────

    /// Register directly in NATS KV without Pilot.
    ///
    /// - Creates (or opens) the registry KV bucket.
    /// - Writes `kv_key → value`; captures the initial revision for CAS.
    /// - Spawns a CAS heartbeat task that uses `update(revision)`.
    ///
    /// The bucket TTL is `heartbeat_interval × 3` so stale entries expire
    /// if the process crashes without deregistering.
    pub async fn register_direct(
        js: &jetstream::Context,
        kv_key: impl Into<String>,
        value: Bytes,
        heartbeat_interval: Duration,
    ) -> Result<Self, async_nats::Error> {
        let kv_key = kv_key.into();
        let ttl = heartbeat_interval * 3;

        let kv = Arc::new(KvRegistryClient::create(js, REGISTRY_BUCKET, ttl).await?);
        let revision = kv.put(&kv_key, value.clone()).await?;
        info!(kv_key, revision, "registered in NATS KV (direct mode)");

        let session_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string();

        let grant = RegistrationGrant {
            owner_session_id:  session_id,
            lock_key:          kv_key.clone(),
            lease_ttl_ms:      ttl.as_millis() as u64,
            scoped_credential: None,
            instance_id:       None,
            kv_key:            kv_key.clone(),
        };

        let (fenced_tx, fenced_rx) = watch::channel(false);
        let hb = Arc::clone(&kv).heartbeat_loop(
            kv_key,
            value,
            heartbeat_interval,
            revision,
            fenced_tx,
        );

        Ok(Self { kv, grant, _hb: hb, nats: None, fenced: fenced_rx })
    }

    // ── Pilot-bootstrap mode ─────────────────────────────────────────────────

    /// Register via Pilot bootstrap NATS request/reply.
    ///
    /// 1. Sends `BootstrapRegisterRequest` proto to `zk.bootstrap.register`.
    /// 2. Receives `BootstrapRegisterResponse` from Pilot.
    /// 3. On `status == "OK"`: writes KV using the kv_key from the grant,
    ///    starts CAS heartbeat loop.
    ///
    /// `value` is the serialised [`ServiceRegistration`] proto payload for KV.
    ///
    /// [`ServiceRegistration`]: zk_proto_rs::zk::discovery::v1::ServiceRegistration
    pub async fn register_with_pilot(
        nats: &async_nats::Client,
        js: &jetstream::Context,
        token: &str,
        logical_id: &str,
        instance_type: &str,
        env: &str,
        runtime_info: std::collections::HashMap<String, String>,
        value: Bytes,
        heartbeat_interval: Duration,
    ) -> Result<Self, RegistrationError> {
        let req = BootstrapRegisterRequest {
            token:         token.to_string(),
            logical_id:    logical_id.to_string(),
            instance_type: instance_type.to_string(),
            env:           env.to_string(),
            runtime_info,
        };

        let payload = Bytes::from(req.encode_to_vec());
        let reply = nats
            .request("zk.bootstrap.register", payload)
            .await
            .map_err(|e| RegistrationError::Nats(e.into()))?;

        let resp = BootstrapRegisterResponse::decode(reply.payload.as_ref())
            .map_err(|e| RegistrationError::Proto(e.to_string()))?;

        if resp.status != "OK" {
            return Err(RegistrationError::Pilot {
                status:  resp.status,
                message: resp.error_message,
            });
        }

        let ttl = Duration::from_millis(resp.lease_ttl_ms as u64);
        let kv = Arc::new(KvRegistryClient::create(js, REGISTRY_BUCKET, ttl).await?);
        let revision = kv.put(&resp.kv_key, value.clone()).await?;
        info!(
            kv_key = resp.kv_key,
            owner_session_id = resp.owner_session_id,
            revision,
            "registered via Pilot"
        );

        let grant = RegistrationGrant {
            owner_session_id:  resp.owner_session_id,
            kv_key:            resp.kv_key.clone(),
            lock_key:          resp.lock_key,
            lease_ttl_ms:      resp.lease_ttl_ms as u64,
            scoped_credential: if resp.scoped_credential.is_empty() {
                None
            } else {
                Some(resp.scoped_credential)
            },
            instance_id: if resp.instance_id != 0 { Some(resp.instance_id) } else { None },
        };

        let (fenced_tx, fenced_rx) = watch::channel(false);
        let hb = Arc::clone(&kv).heartbeat_loop(
            resp.kv_key,
            value,
            heartbeat_interval,
            revision,
            fenced_tx,
        );

        Ok(Self { kv, grant, _hb: hb, nats: Some(nats.clone()), fenced: fenced_rx })
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Returns the registration grant (owner session ID, KV key, etc.).
    pub fn grant(&self) -> &RegistrationGrant {
        &self.grant
    }

    // ── Fencing ──────────────────────────────────────────────────────────────

    /// Waits until the CAS heartbeat detects a conflicting writer (fencing event).
    ///
    /// Returns immediately if already fenced. The caller should log the event
    /// and terminate the process — continuing to serve traffic after fencing
    /// means another instance may have taken ownership of this logical identity.
    pub async fn wait_fenced(&mut self) {
        while !*self.fenced.borrow() {
            if self.fenced.changed().await.is_err() {
                // Sender dropped (heartbeat task exited for any reason).
                break;
            }
        }
    }

    // ── Deregister ───────────────────────────────────────────────────────────

    /// Delete the KV entry and, if registered via Pilot, notify Pilot to release
    /// the session. Call before process exit for a clean shutdown.
    pub async fn deregister(&self) -> Result<(), async_nats::Error> {
        self.kv.delete(&self.grant.kv_key).await?;
        info!(kv_key = self.grant.kv_key, "deregistered from NATS KV");

        // Notify Pilot so it can release instance_id lease and update session state.
        // Pilot looks up session metadata by owner_session_id; other fields may be empty.
        if let Some(ref nats) = self.nats {
            let req = BootstrapDeregisterRequest {
                owner_session_id: self.grant.owner_session_id.clone(),
                logical_id:       String::new(),
                instance_type:    String::new(),
                env:              String::new(),
            };
            let payload = Bytes::from(req.encode_to_vec());
            let _ = nats.request("zk.bootstrap.deregister", payload).await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;

    #[tokio::test]
    async fn wait_fenced_returns_when_signal_is_sent() {
        let (tx, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = tx.send(true);
        });
        // Replicate wait_fenced logic
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        assert!(*rx.borrow(), "receiver should see true after sender fires");
    }
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("NATS error: {0}")]
    Nats(#[from] async_nats::Error),
    #[error("proto decode error: {0}")]
    Proto(String),
    #[error("Pilot rejected registration: status={status}, message={message}")]
    Pilot { status: String, message: String },
}
