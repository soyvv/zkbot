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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use bytes::Bytes;
use prost::Message;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::bootstrap::{PilotPayload, PilotRegistration};
use crate::nats_kv::{KvRegistryClient, REGISTRY_BUCKET};
use zk_proto_rs::zk::pilot::v1::{
    BootstrapDeregisterRequest, BootstrapRegisterRequest, BootstrapRegisterResponse,
};

const PILOT_DUPLICATE_RETRY_DELAY: Duration = Duration::from_secs(2);
const PILOT_DUPLICATE_RETRY_WINDOW: Duration = Duration::from_secs(50);

fn duplicate_retry_delay(
    status: &str,
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> Option<Duration> {
    if status != "DUPLICATE" || now >= deadline {
        return None;
    }

    Some(std::cmp::min(
        PILOT_DUPLICATE_RETRY_DELAY,
        deadline.saturating_duration_since(now),
    ))
}

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
        let deadline = tokio::time::Instant::now() + PILOT_DUPLICATE_RETRY_WINDOW;

        loop {
            let reply = nats
                .request("zk.bootstrap.register", payload.clone())
                .await
                .map_err(|e| RegistrationError::Nats(e.into()))?;

            let resp = BootstrapRegisterResponse::decode(reply.payload.as_ref())
                .map_err(|e| RegistrationError::Proto(e.to_string()))?;

            if resp.status == "OK" {
                let pilot_payload = PilotPayload {
                    runtime_config_json: resp.runtime_config,
                    config_metadata: resp.config_metadata,
                    secret_refs: resp.secret_refs,
                    server_time_ms: resp.server_time_ms,
                };

                return Ok(PilotBootstrapGrant {
                    kv_key: resp.kv_key,
                    lock_key: resp.lock_key,
                    lease_ttl_ms: resp.lease_ttl_ms as u64,
                    owner_session_id: resp.owner_session_id,
                    scoped_credential: resp.scoped_credential,
                    instance_id: resp.instance_id,
                    payload: pilot_payload,
                });
            }

            let now = tokio::time::Instant::now();
            if let Some(delay) = duplicate_retry_delay(&resp.status, now, deadline) {
                warn!(
                    logical_id,
                    instance_type,
                    env,
                    retry_in_ms = delay.as_millis() as u64,
                    deadline_in_ms = deadline.saturating_duration_since(now).as_millis() as u64,
                    error = %resp.error_message,
                    "Pilot reported duplicate live session during bootstrap; retrying"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            return Err(RegistrationError::Pilot {
                status: resp.status,
                message: resp.error_message,
            });
        }
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

    // ── Shutdown ─────────────────────────────────────────────────────────

    /// Wait for a termination signal (SIGINT, SIGTERM, SIGHUP) or KV fencing.
    ///
    /// Returns the reason for shutdown. Does **not** call [`deregister`](Self::deregister) —
    /// the caller controls when to remove the service from discovery, allowing
    /// internal task drain before deregistration.
    ///
    /// # Crash fallback
    /// SIGKILL and abnormal crashes bypass signal handlers entirely. The KV
    /// entry expires via its TTL and Pilot's KvReconciler cleans up the stale
    /// session. This is by design.
    pub async fn wait_shutdown(&mut self) -> ShutdownReason {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received SIGINT");
                    ShutdownReason::Signal(ShutdownSignal::SigInt)
                }
                _ = sigterm.recv() => {
                    info!("received SIGTERM");
                    ShutdownReason::Signal(ShutdownSignal::SigTerm)
                }
                _ = sighup.recv() => {
                    info!("received SIGHUP");
                    ShutdownReason::Signal(ShutdownSignal::SigHup)
                }
                _ = self.wait_fenced() => {
                    warn!("KV fencing detected — another instance owns this identity");
                    ShutdownReason::Fenced
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received SIGINT");
                    ShutdownReason::Signal(ShutdownSignal::SigInt)
                }
                _ = self.wait_fenced() => {
                    warn!("KV fencing detected — another instance owns this identity");
                    ShutdownReason::Fenced
                }
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

// ── Shutdown reason ──────────────────────────────────────────────────────────

/// OS signal that triggered shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    SigInt,
    SigTerm,
    SigHup,
}

/// Reason a registered service is shutting down.
///
/// Returned by [`ServiceRegistration::wait_shutdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// Clean shutdown requested via an OS signal.
    Signal(ShutdownSignal),
    /// CAS heartbeat detected a conflicting writer (fenced by another instance).
    Fenced,
}

impl ShutdownReason {
    /// Returns `true` if the shutdown was caused by KV fencing.
    pub fn is_fenced(self) -> bool {
        matches!(self, Self::Fenced)
    }
}

impl fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signal(ShutdownSignal::SigInt) => write!(f, "SIGINT"),
            Self::Signal(ShutdownSignal::SigTerm) => write!(f, "SIGTERM"),
            Self::Signal(ShutdownSignal::SigHup) => write!(f, "SIGHUP"),
            Self::Fenced => write!(f, "fenced"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    #[test]
    fn duplicate_retry_delay_only_retries_duplicate_before_deadline() {
        let now = tokio::time::Instant::now();
        let deadline = now + Duration::from_secs(5);

        assert_eq!(
            duplicate_retry_delay("DUPLICATE", now, deadline),
            Some(PILOT_DUPLICATE_RETRY_DELAY)
        );
        assert_eq!(duplicate_retry_delay("ERROR", now, deadline), None);
        assert_eq!(duplicate_retry_delay("DUPLICATE", deadline, deadline), None);
    }

    #[test]
    fn duplicate_retry_delay_caps_sleep_at_remaining_window() {
        let now = tokio::time::Instant::now();
        let deadline = now + Duration::from_millis(750);

        assert_eq!(
            duplicate_retry_delay("DUPLICATE", now, deadline),
            Some(Duration::from_millis(750))
        );
    }

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

    #[test]
    fn shutdown_reason_is_fenced() {
        assert!(ShutdownReason::Fenced.is_fenced());
        assert!(!ShutdownReason::Signal(ShutdownSignal::SigInt).is_fenced());
        assert!(!ShutdownReason::Signal(ShutdownSignal::SigTerm).is_fenced());
        assert!(!ShutdownReason::Signal(ShutdownSignal::SigHup).is_fenced());
    }

    #[test]
    fn shutdown_reason_display() {
        assert_eq!(
            ShutdownReason::Signal(ShutdownSignal::SigInt).to_string(),
            "SIGINT"
        );
        assert_eq!(
            ShutdownReason::Signal(ShutdownSignal::SigTerm).to_string(),
            "SIGTERM"
        );
        assert_eq!(
            ShutdownReason::Signal(ShutdownSignal::SigHup).to_string(),
            "SIGHUP"
        );
        assert_eq!(ShutdownReason::Fenced.to_string(), "fenced");
    }

    #[test]
    fn shutdown_reason_equality() {
        assert_eq!(
            ShutdownReason::Signal(ShutdownSignal::SigTerm),
            ShutdownReason::Signal(ShutdownSignal::SigTerm)
        );
        assert_ne!(
            ShutdownReason::Signal(ShutdownSignal::SigInt),
            ShutdownReason::Fenced
        );
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
