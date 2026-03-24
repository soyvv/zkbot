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
//! # async fn example(js: async_nats::jetstream::Context) -> Result<(), async_nats::Error> {
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

use crate::bootstrap::{PilotPayload, PilotRegistration};
use crate::nats_kv::{KvRegistryClient, REGISTRY_BUCKET};
use zk_proto_rs::zk::pilot::v1::{
    BootstrapDeregisterRequest, BootstrapRegisterRequest, BootstrapRegisterResponse,
};

// ── Pilot bootstrap grant (split-phase) ─────────────────────────────────────

/// Grant returned by [`ServiceRegistration::pilot_request`].
///
/// Contains the Pilot-issued KV key, lock key, and config payload. Used as
/// input to [`ServiceRegistration::register_kv_with_grant`] after the service
/// has assembled its runtime config (and therefore knows the full KV payload).
#[derive(Debug)]
pub struct PilotBootstrapGrant {
    /// KV key assigned by Pilot (e.g. `svc.gw.gw_okx_1`).
    pub kv_key: String,
    /// Lock key for CAS fencing.
    pub lock_key: String,
    /// Lease TTL in ms (informational; enforced by KV bucket `max_age`).
    pub lease_ttl_ms: u64,
    /// Unique session ID for this registration (from Pilot).
    pub owner_session_id: String,
    /// Scoped NATS credential (empty string if not issued).
    pub scoped_credential: String,
    /// Pilot-assigned Snowflake worker ID (engines only; 0 otherwise).
    pub instance_id: i32,
    /// Pilot-returned config payload (runtime_config JSON, secret refs, metadata).
    pub payload: PilotPayload,
}

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
    kv: Arc<KvRegistryClient>,
    grant: RegistrationGrant,
    _hb: JoinHandle<()>,
    /// NATS client kept for deregister notification in Pilot mode.
    nats: Option<async_nats::Client>,
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
            owner_session_id: session_id,
            lock_key: kv_key.clone(),
            lease_ttl_ms: ttl.as_millis() as u64,
            scoped_credential: None,
            instance_id: None,
            kv_key: kv_key.clone(),
        };

        let (fenced_tx, fenced_rx) = watch::channel(false);
        let hb =
            Arc::clone(&kv).heartbeat_loop(kv_key, value, heartbeat_interval, revision, fenced_tx);

        Ok(Self {
            kv,
            grant,
            _hb: hb,
            nats: None,
            fenced: fenced_rx,
        })
    }

    // ── Pilot-bootstrap mode ─────────────────────────────────────────────────

    /// Phase 1 of split-phase Pilot registration: send NATS request, get grant.
    ///
    /// Sends `BootstrapRegisterRequest` to `zk.bootstrap.register` and returns
    /// a [`PilotBootstrapGrant`] containing the Pilot-issued KV key, session,
    /// and config payload. **No KV write happens here.**
    ///
    /// Use this when the service needs the Pilot payload to assemble its
    /// runtime config before it can build the full KV registration proto.
    /// Follow with [`register_kv_with_grant`](Self::register_kv_with_grant).
    pub async fn pilot_request(
        nats: &async_nats::Client,
        token: &str,
        logical_id: &str,
        instance_type: &str,
        env: &str,
        runtime_info: std::collections::HashMap<String, String>,
    ) -> Result<PilotBootstrapGrant, RegistrationError> {
        let req = BootstrapRegisterRequest {
            token: token.to_string(),
            logical_id: logical_id.to_string(),
            instance_type: instance_type.to_string(),
            env: env.to_string(),
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
                status: resp.status,
                message: resp.error_message,
            });
        }

        let pilot_payload = PilotPayload {
            runtime_config_json: resp.runtime_config,
            config_metadata: resp.config_metadata,
            secret_refs: resp.secret_refs,
            server_time_ms: resp.server_time_ms,
        };

        Ok(PilotBootstrapGrant {
            kv_key: resp.kv_key,
            lock_key: resp.lock_key,
            lease_ttl_ms: resp.lease_ttl_ms as u64,
            owner_session_id: resp.owner_session_id,
            scoped_credential: resp.scoped_credential,
            instance_id: resp.instance_id,
            payload: pilot_payload,
        })
    }

    /// Phase 2 of split-phase Pilot registration: write KV entry and start heartbeat.
    ///
    /// Takes a [`PilotBootstrapGrant`] from [`pilot_request`](Self::pilot_request)
    /// and the fully-assembled KV registration proto. Writes to NATS KV and
    /// starts the CAS heartbeat loop.
    pub async fn register_kv_with_grant(
        nats: &async_nats::Client,
        js: &jetstream::Context,
        grant: &PilotBootstrapGrant,
        value: Bytes,
        heartbeat_interval: Duration,
    ) -> Result<Self, RegistrationError> {
        let ttl = Duration::from_millis(grant.lease_ttl_ms);
        let kv = Arc::new(KvRegistryClient::create(js, REGISTRY_BUCKET, ttl).await?);
        let revision = kv.put(&grant.kv_key, value.clone()).await?;
        info!(
            kv_key = grant.kv_key,
            owner_session_id = grant.owner_session_id,
            revision,
            "registered via Pilot (split-phase KV write)"
        );

        let reg_grant = RegistrationGrant {
            owner_session_id: grant.owner_session_id.clone(),
            kv_key: grant.kv_key.clone(),
            lock_key: grant.lock_key.clone(),
            lease_ttl_ms: grant.lease_ttl_ms,
            scoped_credential: if grant.scoped_credential.is_empty() {
                None
            } else {
                Some(grant.scoped_credential.clone())
            },
            instance_id: if grant.instance_id != 0 {
                Some(grant.instance_id)
            } else {
                None
            },
        };

        let (fenced_tx, fenced_rx) = watch::channel(false);
        let hb = Arc::clone(&kv).heartbeat_loop(
            grant.kv_key.clone(),
            value,
            heartbeat_interval,
            revision,
            fenced_tx,
        );

        Ok(Self {
            kv,
            grant: reg_grant,
            _hb: hb,
            nats: Some(nats.clone()),
            fenced: fenced_rx,
        })
    }

    /// Register via Pilot bootstrap NATS request/reply (one-shot convenience).
    ///
    /// Combines [`pilot_request`](Self::pilot_request) +
    /// [`register_kv_with_grant`](Self::register_kv_with_grant) into a single
    /// call. Use this when the KV registration proto can be built before
    /// knowing the Pilot payload (e.g. OMS).
    ///
    /// For services that need the Pilot payload to build the KV proto (e.g.
    /// gateway, where venue/account come from Pilot), use the split-phase API.
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
    ) -> Result<PilotRegistration, RegistrationError> {
        let grant =
            Self::pilot_request(nats, token, logical_id, instance_type, env, runtime_info).await?;
        let registration =
            Self::register_kv_with_grant(nats, js, &grant, value, heartbeat_interval).await?;

        Ok(PilotRegistration {
            registration,
            payload: grant.payload,
        })
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
                logical_id: String::new(),
                instance_type: String::new(),
                env: String::new(),
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
