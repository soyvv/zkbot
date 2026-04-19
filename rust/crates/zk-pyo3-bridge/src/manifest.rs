use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::py_errors::PyBridgeError;

/// Canonical capability key for trading gateway.
pub const CAP_GW: &str = "gw";
/// Canonical capability key for real-time market data.
pub const CAP_RTMD: &str = "rtmd";
/// Canonical capability key for reference data loading.
pub const CAP_REFDATA: &str = "refdata";

/// A parsed venue manifest (from `manifest.yaml`).
#[derive(Debug, Deserialize)]
pub struct VenueManifest {
    pub venue: String,
    pub version: u32,
    pub capabilities: HashMap<String, CapabilityEntry>,
    pub metadata: Option<serde_json::Value>,
}

/// A field descriptor from the manifest's `field_descriptors` list.
#[derive(Debug, Deserialize)]
pub struct FieldDescriptor {
    /// JSON pointer path (e.g. "/secret_ref").
    pub path: String,
    /// True if this field holds a Vault secret reference.
    #[serde(default)]
    pub secret_ref: bool,
    /// True if the field can be changed without restarting the adaptor.
    #[serde(default)]
    pub reloadable: bool,
}

/// A single capability entry within the manifest.
#[derive(Debug, Deserialize)]
pub struct CapabilityEntry {
    pub language: String,
    pub entrypoint: String,
    pub config_schema: Option<String>,
    /// Field-level metadata declared per capability in the manifest.
    #[serde(default)]
    pub field_descriptors: Vec<FieldDescriptor>,
}

/// Return the JSON pointer paths of fields marked `secret_ref: true`.
pub fn secret_ref_paths(cap: &CapabilityEntry) -> Vec<&str> {
    cap.field_descriptors
        .iter()
        .filter(|fd| fd.secret_ref)
        .map(|fd| fd.path.as_str())
        .collect()
}

/// Parsed Python entrypoint from `"python:module.path:ClassName"`.
#[derive(Debug, Clone)]
pub struct PythonEntrypoint {
    pub module_path: String,
    pub class_name: String,
}

/// Load a venue manifest from `{venue_root}/{venue}/manifest.yaml`.
pub fn load_manifest(venue_root: &Path, venue: &str) -> Result<VenueManifest, PyBridgeError> {
    let path = venue_root.join(venue).join("manifest.yaml");
    let content = std::fs::read_to_string(&path).map_err(|e| {
        PyBridgeError::ManifestLoad(format!("failed to read {}: {e}", path.display()))
    })?;
    serde_yaml::from_str(&content).map_err(|e| {
        PyBridgeError::ManifestLoad(format!("failed to parse {}: {e}", path.display()))
    })
}

/// Look up a capability by key (e.g. `"gw"`, `"rtmd"`, `"refdata"`).
pub fn resolve_capability<'a>(
    manifest: &'a VenueManifest,
    cap: &str,
) -> Result<&'a CapabilityEntry, PyBridgeError> {
    manifest.capabilities.get(cap).ok_or_else(|| {
        PyBridgeError::ManifestLoad(format!(
            "capability '{}' not found in manifest for venue '{}'",
            cap, manifest.venue
        ))
    })
}

/// Parse a Python entrypoint string like `"python:oanda.gw:OandaGatewayAdaptor"`.
pub fn parse_python_entrypoint(raw: &str) -> Result<PythonEntrypoint, PyBridgeError> {
    let parts: Vec<&str> = raw.splitn(3, ':').collect();
    if parts.len() != 3 || parts[0] != "python" {
        return Err(PyBridgeError::ManifestLoad(format!(
            "invalid python entrypoint format: '{raw}' (expected 'python:<module>:<class>')"
        )));
    }
    let module_path = parts[1].to_string();
    let class_name = parts[2].to_string();
    if module_path.is_empty() || class_name.is_empty() {
        return Err(PyBridgeError::ManifestLoad(format!(
            "empty module or class in entrypoint: '{raw}'"
        )));
    }
    Ok(PythonEntrypoint {
        module_path,
        class_name,
    })
}

/// Validate a config value against the JSON schema declared in the manifest.
///
/// If `config_schema` is `None`, validation is skipped. If the schema file
/// does not exist or is invalid, this returns an error (fail-fast on broken manifests).
pub fn validate_config(
    venue_root: &Path,
    venue: &str,
    cap: &CapabilityEntry,
    config: &serde_json::Value,
) -> Result<(), PyBridgeError> {
    let schema_rel = match &cap.config_schema {
        Some(s) => s,
        None => return Ok(()),
    };

    let schema_path = venue_root.join(venue).join(schema_rel);
    let schema_str = std::fs::read_to_string(&schema_path).map_err(|e| {
        PyBridgeError::SchemaValidation(format!(
            "failed to read schema {}: {e}",
            schema_path.display()
        ))
    })?;
    let schema: serde_json::Value = serde_json::from_str(&schema_str).map_err(|e| {
        PyBridgeError::SchemaValidation(format!(
            "invalid JSON in schema {}: {e}",
            schema_path.display()
        ))
    })?;

    let validator = jsonschema::validator_for(&schema).map_err(|e| {
        PyBridgeError::SchemaValidation(format!(
            "invalid JSON Schema {}: {e}",
            schema_path.display()
        ))
    })?;

    if let Err(error) = validator.validate(config) {
        return Err(PyBridgeError::SchemaValidation(format!(
            "config validation failed against {}: {error}",
            schema_path.display(),
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_entrypoint_valid() {
        let ep = parse_python_entrypoint("python:oanda.gw:OandaGatewayAdaptor").unwrap();
        assert_eq!(ep.module_path, "oanda.gw");
        assert_eq!(ep.class_name, "OandaGatewayAdaptor");
    }

    #[test]
    fn test_parse_python_entrypoint_rejects_rust() {
        assert!(parse_python_entrypoint("rust::okx::gw::OkxGatewayAdaptor").is_err());
    }

    #[test]
    fn test_parse_python_entrypoint_rejects_missing_class() {
        assert!(parse_python_entrypoint("python:oanda.gw:").is_err());
    }

    #[test]
    fn test_parse_python_entrypoint_rejects_no_prefix() {
        assert!(parse_python_entrypoint("oanda.gw:OandaGatewayAdaptor").is_err());
    }

    #[test]
    fn test_resolve_capability_missing() {
        let manifest = VenueManifest {
            venue: "test".to_string(),
            version: 1,
            capabilities: HashMap::new(),
            metadata: None,
        };
        assert!(resolve_capability(&manifest, "gw").is_err());
    }

    #[test]
    fn test_field_descriptors_parsed() {
        let yaml = r#"
venue: test
version: 1
capabilities:
  gw:
    language: python
    entrypoint: "python:test.gw:TestGw"
    field_descriptors:
      - path: /secret_ref
        secret_ref: true
        reloadable: false
      - path: /environment
        reloadable: false
"#;
        let manifest: VenueManifest = serde_yaml::from_str(yaml).unwrap();
        let cap = resolve_capability(&manifest, "gw").unwrap();
        assert_eq!(cap.field_descriptors.len(), 2);
        assert!(cap.field_descriptors[0].secret_ref);
        assert!(!cap.field_descriptors[1].secret_ref);
    }

    #[test]
    fn test_secret_ref_paths() {
        let yaml = r#"
venue: test
version: 1
capabilities:
  gw:
    language: python
    entrypoint: "python:test.gw:TestGw"
    field_descriptors:
      - path: /secret_ref
        secret_ref: true
      - path: /environment
      - path: /api_key
        secret_ref: true
"#;
        let manifest: VenueManifest = serde_yaml::from_str(yaml).unwrap();
        let cap = resolve_capability(&manifest, "gw").unwrap();
        let paths = secret_ref_paths(cap);
        assert_eq!(paths, vec!["/secret_ref", "/api_key"]);
    }

    #[test]
    fn test_field_descriptors_default_empty() {
        let yaml = r#"
venue: test
version: 1
capabilities:
  gw:
    language: rust
    entrypoint: "rust::test::gw::TestGw"
"#;
        let manifest: VenueManifest = serde_yaml::from_str(yaml).unwrap();
        let cap = resolve_capability(&manifest, "gw").unwrap();
        assert!(cap.field_descriptors.is_empty());
        assert!(secret_ref_paths(cap).is_empty());
    }
}
