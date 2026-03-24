//! Shared bootstrap and config assembly for bootstrap-managed services.
//!
//! # Architecture
//!
//! Provides shared bootstrap transport, payload extraction, orchestration, and
//! error handling.  Each service implements [`ServiceBootstrap`] to own its
//! typed config decode, load, and assembly logic.
//!
//! ## Ownership split
//!
//! **Shared lib owns:**
//! - Pilot payload extraction from `BootstrapRegisterResponse`
//! - Error type ([`BootstrapConfigError`])
//! - Config source tagging ([`ConfigSource`])
//! - Optional hash validation (conditional on normalization contract)
//! - Orchestration ([`bootstrap_runtime_config`])
//! - Envelope bridging ([`wrap_in_envelope`])
//!
//! **Each service owns:**
//! - `BootstrapConfig`, `ProvidedConfig`, `RuntimeConfig` types
//! - Pilot JSON decode rules
//! - Direct-mode config load rules
//! - Typed assembly from bootstrap + provided → runtime
//! - Service-specific validation and invariants
//!
//! # Usage
//!
//! ```no_run
//! use zk_infra_rs::bootstrap::*;
//!
//! // Each service defines its own config types and implements ServiceBootstrap.
//! // Then calls the shared orchestrator at startup:
//! //
//! //   let outcome = bootstrap_runtime_config::<MyService>(&boot_cfg, mode)?;
//! //   let envelope = wrap_in_envelope(outcome.runtime_config, &outcome.source);
//! ```

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

use zk_proto_rs::zk::config::v1::{ConfigMetadata, SecretRef};

use crate::config_mgmt;
use crate::service_registry::ServiceRegistration;

// ── Error ────────────────────────────────────────────────────────────────────

/// Explicit error type for bootstrap and config assembly failures.
///
/// Services map their own errors (envy, io, custom validation) into
/// [`Validation`] or [`Custom`].
#[derive(Debug, thiserror::Error)]
pub enum BootstrapConfigError {
    #[error("invalid JSON in Pilot config payload: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("missing required config field: {field}")]
    MissingField { field: String },

    #[error("config hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("config validation failed: {0}")]
    Validation(String),

    #[error("{0}")]
    Custom(String),
}

// ── Config source ────────────────────────────────────────────────────────────

/// How the provided config was sourced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Config loaded locally (env vars, files) without Pilot.
    Direct,
    /// Config returned by Pilot at bootstrap time.
    Pilot {
        config_version: String,
        config_hash: String,
        issued_at_ms: i64,
    },
}

impl ConfigSource {
    /// Returns the string tag used in `ConfigMetadata.config_source`.
    pub fn as_metadata_source(&self) -> &str {
        match self {
            Self::Direct => "env_direct",
            Self::Pilot { .. } => "bootstrap",
        }
    }
}

// ── Pilot payload ────────────────────────────────────────────────────────────

/// Config payload extracted from `BootstrapRegisterResponse`.
///
/// Produced by [`ServiceRegistration::register_with_pilot`]; consumed by
/// service [`ServiceBootstrap`] trait implementations.
#[derive(Debug, Clone)]
pub struct PilotPayload {
    /// Raw JSON string from `BootstrapRegisterResponse.runtime_config`.
    pub runtime_config_json: String,
    /// Config metadata from Pilot (version, hash, timestamps).
    pub config_metadata: Option<ConfigMetadata>,
    /// Logical secret refs to resolve via Vault.
    pub secret_refs: Vec<SecretRef>,
    /// Server timestamp for clock-skew detection.
    pub server_time_ms: i64,
}

// ── Named registration result ────────────────────────────────────────────────

/// Named result from Pilot bootstrap registration.
///
/// Returned by [`ServiceRegistration::register_with_pilot`] instead of a
/// raw tuple.
pub struct PilotRegistration {
    /// Active KV registration with CAS heartbeat.
    pub registration: ServiceRegistration,
    /// Pilot-returned config payload.
    pub payload: PilotPayload,
}

// ── Bootstrap mode ───────────────────────────────────────────────────────────

/// Input to the shared orchestrator: how was provided config sourced?
pub enum BootstrapMode {
    /// Pilot returned a payload — use it.
    Pilot {
        payload: PilotPayload,
        /// Enable hash validation.  Set to `true` only when the Pilot/Rust
        /// normalization contract is confirmed to use the same canonical JSON
        /// representation (sorted keys, compact, no whitespace).
        validate_hash: bool,
    },
    /// No Pilot — service loads config locally.
    Direct,
}

// ── Service trait ────────────────────────────────────────────────────────────

/// Contract that each service implements to participate in the shared
/// bootstrap path.
///
/// The shared orchestrator ([`bootstrap_runtime_config`]) calls these methods
/// in order:
///
/// 1. [`decode_pilot_config`](ServiceBootstrap::decode_pilot_config) (Pilot mode)
///    or [`load_direct_config`](ServiceBootstrap::load_direct_config) (direct mode)
/// 2. [`assemble_runtime_config`](ServiceBootstrap::assemble_runtime_config) (both modes)
///
/// Services own all typed decode, load, assembly, and validation logic.
pub trait ServiceBootstrap {
    /// Minimal startup inputs: NATS URL, token, logical_id, env, Vault identity.
    type BootstrapConfig;

    /// Pilot-managed config or direct-mode equivalent.
    type ProvidedConfig;

    /// Effective assembled config used at runtime.
    type RuntimeConfig: std::fmt::Debug;

    /// Decode and validate the Pilot-returned config JSON into typed
    /// `ProvidedConfig`.
    ///
    /// Must fail-fast on malformed or incomplete config — return
    /// [`BootstrapConfigError`].
    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<Self::ProvidedConfig, BootstrapConfigError>;

    /// Load `ProvidedConfig` in direct mode (no Pilot).
    ///
    /// The service owns the loading mechanism (envy, manual env::var, file,
    /// etc.).  Must produce the same logical `ProvidedConfig` shape as Pilot
    /// mode.
    fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError>;

    /// Assemble the effective runtime config from bootstrap + provided inputs.
    ///
    /// Called once at startup after provided config is resolved.  Fail-fast on
    /// cross-field inconsistencies or missing derived values.
    fn assemble_runtime_config(
        bootstrap: &Self::BootstrapConfig,
        provided: Self::ProvidedConfig,
    ) -> Result<Self::RuntimeConfig, BootstrapConfigError>;
}

// ── Bootstrap outcome ────────────────────────────────────────────────────────

/// Typed result of the full bootstrap sequence.
///
/// Contains the service's `RuntimeConfig`, not raw payload.
#[derive(Debug)]
pub struct BootstrapOutcome<R: std::fmt::Debug> {
    /// The service's assembled runtime config.
    pub runtime_config: R,
    /// How this config was sourced.
    pub source: ConfigSource,
    /// Config metadata (from Pilot, or locally computed for direct mode).
    pub metadata: ConfigMetadata,
    /// Secret refs to resolve via Vault (from Pilot; empty in direct mode).
    pub secret_refs: Vec<SecretRef>,
}

// ── Shared orchestrator ──────────────────────────────────────────────────────

/// Run the shared bootstrap config assembly sequence.
///
/// **Pilot mode:**
/// 1. Validate hash (if `validate_hash` is true and metadata has a hash)
/// 2. Call `S::decode_pilot_config()`
/// 3. Call `S::assemble_runtime_config()`
/// 4. Return typed `BootstrapOutcome<S::RuntimeConfig>`
///
/// **Direct mode:**
/// 1. Call `S::load_direct_config()`
/// 2. Call `S::assemble_runtime_config()`
/// 3. Return typed `BootstrapOutcome<S::RuntimeConfig>`
///
/// Fails early on any error — no fallback from Pilot to direct mode.
pub fn bootstrap_runtime_config<S: ServiceBootstrap>(
    bootstrap: &S::BootstrapConfig,
    mode: BootstrapMode,
) -> Result<BootstrapOutcome<S::RuntimeConfig>, BootstrapConfigError> {
    match mode {
        BootstrapMode::Pilot {
            payload,
            validate_hash,
        } => {
            // 1. Hash validation (conditional).
            if validate_hash {
                if let Some(ref meta) = payload.config_metadata {
                    validate_config_hash(&payload.runtime_config_json, meta)?;
                }
            }

            // 2. Service decodes Pilot JSON → typed ProvidedConfig.
            let provided = S::decode_pilot_config(&payload)?;

            // 3. Service assembles runtime config.
            let runtime = S::assemble_runtime_config(bootstrap, provided)?;

            // 4. Build source tag and metadata.
            let source = config_source_from_metadata(&payload.config_metadata);
            let metadata = pilot_metadata_or_default(payload.config_metadata);

            Ok(BootstrapOutcome {
                runtime_config: runtime,
                source,
                metadata,
                secret_refs: payload.secret_refs,
            })
        }
        BootstrapMode::Direct => {
            // 1. Service loads provided config locally.
            let provided = S::load_direct_config()?;

            // 2. Service assembles runtime config.
            let runtime = S::assemble_runtime_config(bootstrap, provided)?;

            Ok(BootstrapOutcome {
                runtime_config: runtime,
                source: ConfigSource::Direct,
                metadata: ConfigMetadata {
                    config_source: "env_direct".into(),
                    loaded_at_ms: now_ms(),
                    ..Default::default()
                },
                secret_refs: vec![],
            })
        }
    }
}

// ── Hash validation ──────────────────────────────────────────────────────────

/// Validate that the Pilot-provided config hash matches the normalized JSON.
///
/// Uses `config_mgmt::normalize_json` (sorted keys, compact) + SHA-256.
/// Skips validation if the metadata hash is empty.
///
/// **Dependency:** Pilot must compute the hash using the same normalization.
fn validate_config_hash(
    json: &str,
    metadata: &ConfigMetadata,
) -> Result<(), BootstrapConfigError> {
    if metadata.config_hash.is_empty() {
        return Ok(());
    }

    let value: serde_json::Value = serde_json::from_str(json)?;
    let normalized = config_mgmt::normalize_json(&value);
    let actual = config_mgmt::sha256_hex(normalized.as_bytes());

    if actual != metadata.config_hash {
        return Err(BootstrapConfigError::HashMismatch {
            expected: metadata.config_hash.clone(),
            actual,
        });
    }
    Ok(())
}

// ── Envelope bridging ────────────────────────────────────────────────────────

/// Wrap a typed runtime config into a [`ConfigEnvelope`] for introspection.
///
/// Bridges into the existing `config_mgmt::ConfigEnvelope<T>` with the
/// correct `config_source` tag.
///
/// [`ConfigEnvelope`]: crate::config_mgmt::ConfigEnvelope
pub fn wrap_in_envelope<T: Serialize>(
    config: T,
    source: &ConfigSource,
) -> config_mgmt::ConfigEnvelope<T> {
    config_mgmt::ConfigEnvelope::new(config, source.as_metadata_source())
}

/// Build a `ConfigMetadata` proto from a typed config and source.
///
/// For direct mode: computes hash from the serialized config.
/// For Pilot mode: copies version/hash/issued_at from the source tag.
pub fn build_config_metadata<T: Serialize>(
    config: &T,
    source: &ConfigSource,
) -> ConfigMetadata {
    let now = now_ms();
    match source {
        ConfigSource::Direct => {
            let json = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
            let normalized = config_mgmt::normalize_json(&json);
            let hash = config_mgmt::sha256_hex(normalized.as_bytes());
            ConfigMetadata {
                config_version: String::new(),
                config_hash: hash,
                loaded_at_ms: now,
                config_source: "env_direct".into(),
                issued_at_ms: now,
            }
        }
        ConfigSource::Pilot {
            config_version,
            config_hash,
            issued_at_ms,
        } => ConfigMetadata {
            config_version: config_version.clone(),
            config_hash: config_hash.clone(),
            loaded_at_ms: now,
            config_source: "bootstrap".into(),
            issued_at_ms: *issued_at_ms,
        },
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn config_source_from_metadata(meta: &Option<ConfigMetadata>) -> ConfigSource {
    match meta {
        Some(m) => ConfigSource::Pilot {
            config_version: m.config_version.clone(),
            config_hash: m.config_hash.clone(),
            issued_at_ms: m.issued_at_ms,
        },
        None => ConfigSource::Pilot {
            config_version: String::new(),
            config_hash: String::new(),
            issued_at_ms: 0,
        },
    }
}

fn pilot_metadata_or_default(meta: Option<ConfigMetadata>) -> ConfigMetadata {
    let now = now_ms();
    let mut m = meta.unwrap_or_default();
    m.loaded_at_ms = now;
    if m.config_source.is_empty() {
        m.config_source = "bootstrap".into();
    }
    m
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    // -- Test service types (NOT real service configs) -------------------------

    #[derive(Debug, Clone)]
    struct TestBootstrapConfig {
        env: String,
        nats_url: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestProvidedConfig {
        venue: String,
        port: u16,
    }

    #[derive(Debug, Clone, Serialize)]
    struct TestRuntimeConfig {
        env: String,
        nats_url: String,
        venue: String,
        port: u16,
    }

    struct TestService;

    impl ServiceBootstrap for TestService {
        type BootstrapConfig = TestBootstrapConfig;
        type ProvidedConfig = TestProvidedConfig;
        type RuntimeConfig = TestRuntimeConfig;

        fn decode_pilot_config(
            payload: &PilotPayload,
        ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
            let cfg: TestProvidedConfig =
                serde_json::from_str(&payload.runtime_config_json)?;
            if cfg.venue.is_empty() {
                return Err(BootstrapConfigError::MissingField {
                    field: "venue".into(),
                });
            }
            Ok(cfg)
        }

        fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
            Ok(TestProvidedConfig {
                venue: "simulator".into(),
                port: 8080,
            })
        }

        fn assemble_runtime_config(
            bootstrap: &Self::BootstrapConfig,
            provided: Self::ProvidedConfig,
        ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
            Ok(TestRuntimeConfig {
                env: bootstrap.env.clone(),
                nats_url: bootstrap.nats_url.clone(),
                venue: provided.venue,
                port: provided.port,
            })
        }
    }

    /// Service that fails on direct-mode load (for testing error propagation).
    struct FailingDirectService;

    impl ServiceBootstrap for FailingDirectService {
        type BootstrapConfig = TestBootstrapConfig;
        type ProvidedConfig = TestProvidedConfig;
        type RuntimeConfig = TestRuntimeConfig;

        fn decode_pilot_config(
            _payload: &PilotPayload,
        ) -> Result<Self::ProvidedConfig, BootstrapConfigError> {
            unreachable!()
        }

        fn load_direct_config() -> Result<Self::ProvidedConfig, BootstrapConfigError> {
            Err(BootstrapConfigError::Validation(
                "ZK_VENUE not set".into(),
            ))
        }

        fn assemble_runtime_config(
            _bootstrap: &Self::BootstrapConfig,
            _provided: Self::ProvidedConfig,
        ) -> Result<Self::RuntimeConfig, BootstrapConfigError> {
            unreachable!()
        }
    }

    fn test_boot_cfg() -> TestBootstrapConfig {
        TestBootstrapConfig {
            env: "dev".into(),
            nats_url: "nats://localhost:4222".into(),
        }
    }

    fn pilot_payload(json: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: None,
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn pilot_payload_with_hash(json: &str, hash: &str) -> PilotPayload {
        PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: Some(ConfigMetadata {
                config_version: "1".into(),
                config_hash: hash.into(),
                config_source: "bootstrap".into(),
                issued_at_ms: 1700000000000,
                loaded_at_ms: 0,
            }),
            secret_refs: vec![],
            server_time_ms: 1700000000000,
        }
    }

    fn compute_hash(json: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let normalized = config_mgmt::normalize_json(&value);
        config_mgmt::sha256_hex(normalized.as_bytes())
    }

    // -- Pilot mode: decode + assemble ----------------------------------------

    #[test]
    fn pilot_mode_valid() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"venue":"okx","port":9090}"#),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.venue, "okx");
        assert_eq!(outcome.runtime_config.port, 9090);
        assert_eq!(outcome.runtime_config.env, "dev");
        assert_eq!(outcome.runtime_config.nats_url, "nats://localhost:4222");
        assert!(matches!(outcome.source, ConfigSource::Pilot { .. }));
    }

    #[test]
    fn pilot_mode_empty_json() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(""),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn pilot_mode_invalid_json() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload("{not valid json}"),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::InvalidJson(_)));
    }

    #[test]
    fn pilot_mode_missing_field() {
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"venue":"","port":8080}"#),
            validate_hash: false,
        };
        let err = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::MissingField { .. }));
    }

    #[test]
    fn pilot_mode_assembly_carries_bootstrap_fields() {
        let boot = TestBootstrapConfig {
            env: "prod".into(),
            nats_url: "nats://prod:4222".into(),
        };
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload(r#"{"venue":"ibkr","port":5555}"#),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.env, "prod");
        assert_eq!(outcome.runtime_config.nats_url, "nats://prod:4222");
        assert_eq!(outcome.runtime_config.venue, "ibkr");
    }

    // -- Pilot mode: hash validation ------------------------------------------

    #[test]
    fn pilot_mode_hash_match() {
        let json = r#"{"venue":"okx","port":9090}"#;
        let hash = compute_hash(json);
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, &hash),
            validate_hash: true,
        };
        let outcome = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.venue, "okx");
    }

    #[test]
    fn pilot_mode_hash_mismatch() {
        let json = r#"{"venue":"okx","port":9090}"#;
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, "badhash"),
            validate_hash: true,
        };
        let err = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap_err();
        assert!(matches!(err, BootstrapConfigError::HashMismatch { .. }));
    }

    #[test]
    fn pilot_mode_hash_empty_skips_validation() {
        let json = r#"{"venue":"okx","port":9090}"#;
        let boot = test_boot_cfg();
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, ""),
            validate_hash: true,
        };
        let outcome = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.venue, "okx");
    }

    #[test]
    fn pilot_mode_validate_hash_false_skips() {
        let json = r#"{"venue":"okx","port":9090}"#;
        let boot = test_boot_cfg();
        // Hash is wrong but validate_hash is false → should succeed.
        let mode = BootstrapMode::Pilot {
            payload: pilot_payload_with_hash(json, "definitely_wrong"),
            validate_hash: false,
        };
        let outcome = bootstrap_runtime_config::<TestService>(&boot, mode).unwrap();
        assert_eq!(outcome.runtime_config.venue, "okx");
    }

    // -- Direct mode ----------------------------------------------------------

    #[test]
    fn direct_mode_succeeds() {
        let boot = test_boot_cfg();
        let outcome =
            bootstrap_runtime_config::<TestService>(&boot, BootstrapMode::Direct).unwrap();
        assert_eq!(outcome.runtime_config.venue, "simulator");
        assert_eq!(outcome.runtime_config.port, 8080);
        assert_eq!(outcome.runtime_config.env, "dev");
    }

    #[test]
    fn direct_mode_source_tag() {
        let boot = test_boot_cfg();
        let outcome =
            bootstrap_runtime_config::<TestService>(&boot, BootstrapMode::Direct).unwrap();
        assert_eq!(outcome.source, ConfigSource::Direct);
        assert_eq!(outcome.metadata.config_source, "env_direct");
    }

    #[test]
    fn direct_mode_failure_propagates() {
        let boot = test_boot_cfg();
        let err =
            bootstrap_runtime_config::<FailingDirectService>(&boot, BootstrapMode::Direct)
                .unwrap_err();
        assert!(matches!(err, BootstrapConfigError::Validation(_)));
    }

    // -- Envelope bridging ----------------------------------------------------

    #[test]
    fn wrap_in_envelope_direct() {
        let cfg = TestRuntimeConfig {
            env: "dev".into(),
            nats_url: "nats://localhost:4222".into(),
            venue: "sim".into(),
            port: 8080,
        };
        let envelope = wrap_in_envelope(cfg, &ConfigSource::Direct);
        assert_eq!(envelope.metadata.config_source, "env_direct");
        assert!(!envelope.config_hash().is_empty());
    }

    #[test]
    fn wrap_in_envelope_pilot() {
        let cfg = TestRuntimeConfig {
            env: "prod".into(),
            nats_url: "nats://prod:4222".into(),
            venue: "okx".into(),
            port: 9090,
        };
        let source = ConfigSource::Pilot {
            config_version: "3".into(),
            config_hash: "abc123".into(),
            issued_at_ms: 1700000000000,
        };
        let envelope = wrap_in_envelope(cfg, &source);
        assert_eq!(envelope.metadata.config_source, "bootstrap");
    }

    // -- Full integration pattern ---------------------------------------------

    #[test]
    fn full_bootstrap_pattern_pilot_mode() {
        let boot = test_boot_cfg();
        let json = r#"{"venue":"okx","port":9090}"#;
        let hash = compute_hash(json);

        let payload = PilotPayload {
            runtime_config_json: json.into(),
            config_metadata: Some(ConfigMetadata {
                config_version: "2".into(),
                config_hash: hash,
                config_source: "bootstrap".into(),
                issued_at_ms: 1700000000000,
                loaded_at_ms: 0,
            }),
            secret_refs: vec![SecretRef {
                logical_ref: "okx/main".into(),
                field_key: "api_key".into(),
            }],
            server_time_ms: 1700000000000,
        };

        // Shared orchestrator: hash validation → service decode → assembly → outcome
        let outcome = bootstrap_runtime_config::<TestService>(
            &boot,
            BootstrapMode::Pilot {
                payload,
                validate_hash: true,
            },
        )
        .unwrap();

        // Outcome carries typed RuntimeConfig, not raw JSON.
        assert_eq!(outcome.runtime_config.venue, "okx");
        assert_eq!(outcome.runtime_config.port, 9090);
        assert_eq!(outcome.runtime_config.env, "dev");
        assert_eq!(outcome.runtime_config.nats_url, "nats://localhost:4222");
        assert_eq!(outcome.secret_refs.len(), 1);
        assert_eq!(outcome.secret_refs[0].logical_ref, "okx/main");
        assert!(matches!(
            &outcome.source,
            ConfigSource::Pilot {
                config_version,
                ..
            } if config_version == "2"
        ));

        // Wrap for introspection.
        let envelope = wrap_in_envelope(outcome.runtime_config, &outcome.source);
        assert_eq!(envelope.metadata.config_source, "bootstrap");
    }
}
