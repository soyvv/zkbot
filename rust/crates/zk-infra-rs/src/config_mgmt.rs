//! Config management: envelope, normalization, diffing, drift classification, redaction.
//!
//! Shared by all service binaries for `GetCurrentConfig` introspection responses.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use zk_proto_rs::zk::config::v1::{
    ChangedField, ConfigDriftResult, ConfigMetadata, DriftStatus, GetCurrentConfigResponse,
    SecretRefStatus,
};

// ---------------------------------------------------------------------------
// Field descriptor model
// ---------------------------------------------------------------------------

/// Describes a config field's reloadability and secret classification.
#[derive(Debug, Clone)]
pub struct FieldDescriptor {
    /// JSON pointer path, e.g. "/risk_check_enabled".
    pub field_path: String,
    /// Whether a running service can pick up changes without restart.
    pub reloadable: bool,
    /// Field carries a logical secret reference (e.g. "oanda/main").
    pub is_secret_ref: bool,
    /// Field carries resolved secret material (never persisted).
    pub is_resolved_secret: bool,
}

// ---------------------------------------------------------------------------
// Config envelope
// ---------------------------------------------------------------------------

/// Wraps a typed config with metadata (version, hash, source, timestamp).
pub struct ConfigEnvelope<T: Serialize> {
    pub config: T,
    pub metadata: ConfigMetadata,
}

impl<T: Serialize> ConfigEnvelope<T> {
    /// Create a new envelope, computing hash and recording current time.
    pub fn new(config: T, source: &str) -> Self {
        let json = serde_json::to_value(&config).unwrap_or(Value::Null);
        let norm = normalize_json(&json);
        let hash = sha256_hex(norm.as_bytes());
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        Self {
            config,
            metadata: ConfigMetadata {
                config_version: "1".into(),
                config_hash: hash,
                loaded_at_ms: 0, // set by runtime when config becomes effective
                config_source: source.into(),
                issued_at_ms: now_ms,
            },
        }
    }

    /// Return the normalized JSON string of the config.
    pub fn normalized_json(&self) -> String {
        let json = serde_json::to_value(&self.config).unwrap_or(Value::Null);
        normalize_json(&json)
    }

    /// Return the SHA-256 hex hash of the normalized JSON.
    pub fn config_hash(&self) -> &str {
        &self.metadata.config_hash
    }
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/// Produce a deterministic JSON string: sorted keys, compact format.
pub fn normalize_json(value: &Value) -> String {
    let sorted = sort_keys(value);
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_keys(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Secret redaction
// ---------------------------------------------------------------------------

/// Replace values at the given JSON pointer paths with `"***REDACTED***"`.
/// `secret_paths` are JSON pointer strings like `"/secret_ref"`, `"/api_key"`.
pub fn redact_secrets(json: &mut Value, secret_paths: &[&str]) {
    for path in secret_paths {
        if let Some(target) = json.pointer_mut(path) {
            *target = Value::String("***REDACTED***".into());
        }
    }
}

// ---------------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------------

/// A single field-level difference between two JSON values.
#[derive(Debug, Clone)]
pub struct FieldDiff {
    /// JSON pointer path, e.g. "/risk_check_enabled".
    pub path: String,
    /// Old value (None if field was absent).
    pub old_value: Option<Value>,
    /// New value (None if field was removed).
    pub new_value: Option<Value>,
}

/// Compute field-level diff between two JSON values.
/// Returns a list of changed fields with their paths.
pub fn diff_configs(old: &Value, new: &Value) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    diff_recursive(old, new, String::new(), &mut diffs);
    diffs
}

fn diff_recursive(old: &Value, new: &Value, prefix: String, diffs: &mut Vec<FieldDiff>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            // Check all keys in old
            for (key, old_val) in a {
                let path = format!("{}/{}", prefix, key);
                match b.get(key) {
                    Some(new_val) => diff_recursive(old_val, new_val, path, diffs),
                    None => diffs.push(FieldDiff {
                        path,
                        old_value: Some(old_val.clone()),
                        new_value: None,
                    }),
                }
            }
            // Check keys only in new
            for (key, new_val) in b {
                if !a.contains_key(key) {
                    diffs.push(FieldDiff {
                        path: format!("{}/{}", prefix, key),
                        old_value: None,
                        new_value: Some(new_val.clone()),
                    });
                }
            }
        }
        _ => {
            if old != new {
                diffs.push(FieldDiff {
                    path: prefix,
                    old_value: Some(old.clone()),
                    new_value: Some(new.clone()),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Drift classification
// ---------------------------------------------------------------------------

/// Classify a set of diffs against field descriptors.
/// Returns the most restrictive drift status.
pub fn classify_drift(diffs: &[FieldDiff], descriptors: &[FieldDescriptor]) -> DriftStatus {
    if diffs.is_empty() {
        return DriftStatus::NoDiff;
    }

    let mut worst = DriftStatus::Reloadable;
    for diff in diffs {
        let descriptor = descriptors.iter().find(|d| d.field_path == diff.path);
        let status = match descriptor {
            Some(d) if d.is_secret_ref => DriftStatus::RestartRequired,
            Some(d) if d.reloadable => DriftStatus::Reloadable,
            Some(_) => DriftStatus::RestartRequired,
            // Unknown field → restart required (conservative)
            None => DriftStatus::RestartRequired,
        };
        if status == DriftStatus::RestartRequired {
            return DriftStatus::RestartRequired;
        }
        if (status as i32) > (worst as i32) {
            worst = status;
        }
    }
    worst
}

/// Build a full `ConfigDriftResult` from diffs and descriptors.
pub fn build_drift_result(
    diffs: &[FieldDiff],
    descriptors: &[FieldDescriptor],
) -> ConfigDriftResult {
    let overall = classify_drift(diffs, descriptors);
    let changed_fields = diffs
        .iter()
        .map(|d| {
            let descriptor = descriptors.iter().find(|fd| fd.field_path == d.path);
            let classification = match descriptor {
                Some(fd) if fd.is_secret_ref => DriftStatus::RestartRequired,
                Some(fd) if fd.reloadable => DriftStatus::Reloadable,
                Some(_) | None => DriftStatus::RestartRequired,
            };
            ChangedField {
                field_path: d.path.clone(),
                desired_value: d
                    .new_value
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                current_value: d
                    .old_value
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                classification: classification.into(),
            }
        })
        .collect();

    ConfigDriftResult {
        overall_status: overall.into(),
        changed_fields,
    }
}

// ---------------------------------------------------------------------------
// GetCurrentConfig response builder
// ---------------------------------------------------------------------------

/// Build a `GetCurrentConfigResponse` from a config envelope.
///
/// - `resolved_secret_paths`: fields with resolved secret material → redacted
/// - `secret_ref_paths`: fields with logical refs → kept as-is (safe to expose)
pub fn build_get_current_config_response<T: Serialize>(
    envelope: &ConfigEnvelope<T>,
    resolved_secret_paths: &[&str],
    service_kind: &str,
    logical_id: &str,
    secret_statuses: Vec<SecretRefStatus>,
) -> GetCurrentConfigResponse {
    let mut json = serde_json::to_value(&envelope.config).unwrap_or(Value::Null);

    // Redact resolved secrets (raw material), keep secret refs (logical names)
    redact_secrets(&mut json, resolved_secret_paths);

    let normalized = normalize_json(&json);

    GetCurrentConfigResponse {
        effective_config_json: normalized,
        metadata: Some(envelope.metadata.clone()),
        service_kind: service_kind.into(),
        logical_id: logical_id.into(),
        secret_ref_statuses: secret_statuses,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_normalize_json_sorts_keys() {
        let input = json!({"b": 2, "a": 1, "c": {"z": 3, "y": 4}});
        let result = normalize_json(&input);
        assert_eq!(result, r#"{"a":1,"b":2,"c":{"y":4,"z":3}}"#);
    }

    #[test]
    fn test_normalize_json_stability() {
        let input = json!({"x": 1, "y": [3, 2, 1]});
        let a = normalize_json(&input);
        let b = normalize_json(&input);
        assert_eq!(a, b);
    }

    #[test]
    fn test_redact_secrets() {
        let mut json = json!({"api_key": "secret123", "name": "test", "nested": {"token": "abc"}});
        redact_secrets(&mut json, &["/api_key", "/nested/token"]);
        assert_eq!(json["api_key"], "***REDACTED***");
        assert_eq!(json["name"], "test");
        assert_eq!(json["nested"]["token"], "***REDACTED***");
    }

    #[test]
    fn test_redact_missing_path_is_noop() {
        let mut json = json!({"name": "test"});
        redact_secrets(&mut json, &["/nonexistent"]);
        assert_eq!(json["name"], "test");
    }

    #[test]
    fn test_diff_configs_no_diff() {
        let a = json!({"x": 1, "y": "hello"});
        let b = json!({"x": 1, "y": "hello"});
        assert!(diff_configs(&a, &b).is_empty());
    }

    #[test]
    fn test_diff_configs_value_change() {
        let a = json!({"x": 1, "y": "hello"});
        let b = json!({"x": 2, "y": "hello"});
        let diffs = diff_configs(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "/x");
    }

    #[test]
    fn test_diff_configs_added_removed() {
        let a = json!({"x": 1});
        let b = json!({"y": 2});
        let diffs = diff_configs(&a, &b);
        assert_eq!(diffs.len(), 2);
        let paths: Vec<_> = diffs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"/x"));
        assert!(paths.contains(&"/y"));
    }

    #[test]
    fn test_classify_drift_no_diffs() {
        assert_eq!(classify_drift(&[], &[]), DriftStatus::NoDiff);
    }

    #[test]
    fn test_classify_drift_reloadable() {
        let diffs = vec![FieldDiff {
            path: "/risk_check_enabled".into(),
            old_value: Some(json!(true)),
            new_value: Some(json!(false)),
        }];
        let descriptors = vec![FieldDescriptor {
            field_path: "/risk_check_enabled".into(),
            reloadable: true,
            is_secret_ref: false,
            is_resolved_secret: false,
        }];
        assert_eq!(classify_drift(&diffs, &descriptors), DriftStatus::Reloadable);
    }

    #[test]
    fn test_classify_drift_restart_required() {
        let diffs = vec![FieldDiff {
            path: "/nats_url".into(),
            old_value: Some(json!("old")),
            new_value: Some(json!("new")),
        }];
        let descriptors = vec![FieldDescriptor {
            field_path: "/nats_url".into(),
            reloadable: false,
            is_secret_ref: false,
            is_resolved_secret: false,
        }];
        assert_eq!(
            classify_drift(&diffs, &descriptors),
            DriftStatus::RestartRequired
        );
    }

    #[test]
    fn test_classify_drift_secret_ref_change() {
        let diffs = vec![FieldDiff {
            path: "/secret_ref".into(),
            old_value: Some(json!("oanda/main")),
            new_value: Some(json!("oanda/backup")),
        }];
        let descriptors = vec![FieldDescriptor {
            field_path: "/secret_ref".into(),
            reloadable: false,
            is_secret_ref: true,
            is_resolved_secret: false,
        }];
        assert_eq!(
            classify_drift(&diffs, &descriptors),
            DriftStatus::RestartRequired
        );
    }

    #[test]
    fn test_classify_drift_unknown_field_conservative() {
        let diffs = vec![FieldDiff {
            path: "/unknown_field".into(),
            old_value: Some(json!(1)),
            new_value: Some(json!(2)),
        }];
        assert_eq!(
            classify_drift(&diffs, &[]),
            DriftStatus::RestartRequired
        );
    }
}
