//! Vault client: AppRole login, secret resolution, and dev-mode fallback.
//!
//! All runtime services use this module to resolve logical secret references
//! to raw secret values. Resolved secrets are held in memory only.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

use zk_proto_rs::zk::config::v1::SecretRef;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum SecretError {
    #[error("Vault login failed: {0}")]
    LoginFailed(String),

    #[error("Vault path not found: {0}")]
    PathNotFound(String),

    #[error("Field '{field}' not found at path '{path}'")]
    FieldNotFound { path: String, field: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("No Vault identity configured")]
    NoIdentity,
}

// ---------------------------------------------------------------------------
// Vault identity (injected by orchestrator)
// ---------------------------------------------------------------------------

/// Vault auth identity injected by orchestrator backend.
pub struct VaultIdentity {
    pub vault_addr: String,
    pub auth: VaultAuth,
}

pub enum VaultAuth {
    /// Production: AppRole login with short-lived secret_id.
    AppRole { role_id: String, secret_id: String },
    /// Dev shortcut: pre-existing Vault token (from VAULT_TOKEN env var).
    Token(String),
}

impl VaultIdentity {
    /// Load from process environment. Priority:
    /// 1. VAULT_ADDR + VAULT_ROLE_ID + VAULT_SECRET_ID → AppRole
    /// 2. VAULT_ADDR + VAULT_TOKEN → Token (dev shortcut)
    /// 3. Neither → None (no Vault, dev/direct mode)
    pub fn from_env() -> Option<Self> {
        let vault_addr = env::var("VAULT_ADDR").ok()?;

        if let (Ok(role_id), Ok(secret_id)) =
            (env::var("VAULT_ROLE_ID"), env::var("VAULT_SECRET_ID"))
        {
            return Some(Self {
                vault_addr,
                auth: VaultAuth::AppRole { role_id, secret_id },
            });
        }

        if let Ok(token) = env::var("VAULT_TOKEN") {
            return Some(Self {
                vault_addr,
                auth: VaultAuth::Token(token),
            });
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Logical ref expansion
// ---------------------------------------------------------------------------

/// Context for expanding logical secret refs into Vault paths.
///
/// All runtimes must populate this from their bootstrap/env config.
/// The expansion function uses these fields to build the correct Vault path.
pub struct RefContext {
    /// Venue name if this is a venue-backed service (e.g. "okx", "oanda").
    pub venue: Option<String>,
    /// Account identifier within the venue (e.g. "main", "trading_primary").
    pub account_id: Option<String>,
    /// Service kind: "gw", "oms", "engine", "mdgw".
    pub service_kind: String,
    /// Logical instance ID (e.g. "gw_okx_1", "oms_dev_1").
    pub logical_id: String,
}

/// Expand a logical ref to a Vault KV path, using RefContext for disambiguation.
///
/// Expansion rules:
///   1. Two-part ref "venue/account" → `kv/zkbot/{env}/venues/{venue}/accounts/{account}`
///      If the ref's venue part doesn't match context.venue, uses the ref's venue
///      (the ref is authoritative for which venue's secrets to fetch).
///   2. Single-part ref:
///      - If context has a venue → `kv/zkbot/{env}/venues/{venue}/secrets/{label}`
///        (venue-scoped secret, e.g. a shared API token)
///      - Otherwise → `kv/zkbot/{env}/services/{service_kind}/{label}`
///        (service-scoped secret, e.g. OMS bootstrap token)
///
/// # Examples
/// ```
/// # use zk_infra_rs::vault::{expand_logical_ref, RefContext};
/// let ctx = RefContext {
///     venue: Some("oanda".into()),
///     account_id: Some("main".into()),
///     service_kind: "gw".into(),
///     logical_id: "gw_oanda_1".into(),
/// };
/// assert_eq!(
///     expand_logical_ref("oanda/main", "dev", &ctx),
///     "kv/zkbot/dev/venues/oanda/accounts/main"
/// );
/// ```
pub fn expand_logical_ref(logical_ref: &str, env: &str, context: &RefContext) -> String {
    let parts: Vec<&str> = logical_ref.splitn(2, '/').collect();
    match parts.as_slice() {
        [venue, account_label] => {
            // Two-part ref: venue/account — ref is authoritative for venue
            format!("kv/zkbot/{env}/venues/{venue}/accounts/{account_label}")
        }
        [label] => {
            // Single-part ref: use context to determine scope
            if let Some(venue) = &context.venue {
                // Venue-backed service → venue-scoped secret
                format!("kv/zkbot/{env}/venues/{venue}/secrets/{label}")
            } else {
                // Non-venue service → service-kind-scoped secret
                format!("kv/zkbot/{env}/services/{}/{}", context.service_kind, label)
            }
        }
        _ => format!("kv/zkbot/{env}/refs/{logical_ref}"),
    }
}

// ---------------------------------------------------------------------------
// Secret resolver trait
// ---------------------------------------------------------------------------

/// Result of resolving a single secret ref.
pub struct ResolvedSecret {
    pub logical_ref: String,
    pub field_key: String,
    /// Raw secret value — in-memory only, never serialized or logged.
    pub value: String,
    pub resolved_at_ms: i64,
}

/// Async trait for resolving secret references.
#[async_trait::async_trait]
pub trait SecretResolver: Send + Sync {
    /// Resolve a single field from a secret path.
    async fn resolve(&self, logical_ref: &str, field_key: &str) -> Result<String, SecretError>;

    /// Read all fields from a secret path as a string map.
    async fn read_document(&self, path: &str) -> Result<HashMap<String, String>, SecretError>;

    async fn resolve_all(&self, refs: &[SecretRef]) -> Vec<Result<ResolvedSecret, SecretError>> {
        let mut results = Vec::with_capacity(refs.len());
        for r in refs {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            match self.resolve(&r.logical_ref, &r.field_key).await {
                Ok(value) => results.push(Ok(ResolvedSecret {
                    logical_ref: r.logical_ref.clone(),
                    field_key: r.field_key.clone(),
                    value,
                    resolved_at_ms: now_ms,
                })),
                Err(e) => results.push(Err(e)),
            }
        }
        results
    }
}

// ---------------------------------------------------------------------------
// Vault-backed resolver
// ---------------------------------------------------------------------------

/// HTTP-based Vault KV v2 resolver. Authenticates via AppRole or token.
pub struct VaultSecretResolver {
    client: reqwest::Client,
    vault_addr: String,
    token: String,
}

#[derive(Deserialize)]
struct VaultLoginResponse {
    auth: VaultLoginAuth,
}

#[derive(Deserialize)]
struct VaultLoginAuth {
    client_token: String,
}

#[derive(Deserialize)]
struct VaultReadResponse {
    data: VaultReadData,
}

#[derive(Deserialize)]
struct VaultReadData {
    data: HashMap<String, serde_json::Value>,
}

impl VaultSecretResolver {
    /// Authenticate to Vault. Fails fast if login fails.
    pub async fn login(identity: &VaultIdentity) -> Result<Self, SecretError> {
        let client = reqwest::Client::new();

        let token = match &identity.auth {
            VaultAuth::AppRole { role_id, secret_id } => {
                #[derive(Serialize)]
                struct LoginPayload<'a> {
                    role_id: &'a str,
                    secret_id: &'a str,
                }

                let url = format!("{}/v1/auth/approle/login", identity.vault_addr);
                let resp = client
                    .post(&url)
                    .json(&LoginPayload { role_id, secret_id })
                    .send()
                    .await
                    .map_err(|e| SecretError::LoginFailed(e.to_string()))?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SecretError::LoginFailed(format!("HTTP {status}: {body}")));
                }

                let login: VaultLoginResponse = resp
                    .json()
                    .await
                    .map_err(|e| SecretError::LoginFailed(e.to_string()))?;
                login.auth.client_token
            }
            VaultAuth::Token(t) => t.clone(),
        };

        Ok(Self {
            client,
            vault_addr: identity.vault_addr.clone(),
            token,
        })
    }

    /// Read all fields from a Vault KV v2 secret path as a string map.
    ///
    /// Returns the full secret document. Callers pick which fields they need.
    pub async fn read_document(&self, path: &str) -> Result<HashMap<String, String>, SecretError> {
        let (mount, rest) = path
            .split_once('/')
            .ok_or_else(|| SecretError::PathNotFound(format!("invalid vault path: {path}")))?;
        let url = format!("{}/v1/{}/data/{}", self.vault_addr, mount, rest);

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| SecretError::NetworkError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Err(SecretError::PathNotFound(path.into()));
        }

        if !resp.status().is_success() {
            return Err(SecretError::NetworkError(format!(
                "HTTP {}: reading {}",
                resp.status(),
                path
            )));
        }

        let data: VaultReadResponse = resp
            .json()
            .await
            .map_err(|e| SecretError::NetworkError(e.to_string()))?;

        Ok(data
            .data
            .data
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect())
    }

    /// Read a single field from a Vault KV v2 secret path.
    async fn read_kv(&self, path: &str, field_key: &str) -> Result<String, SecretError> {
        // KV v2 read URL: /v1/{mount}/data/{path}
        // For "kv/zkbot/dev/venues/oanda/accounts/main":
        //   mount = "kv", path = "zkbot/dev/venues/oanda/accounts/main"
        let (mount, rest) = path
            .split_once('/')
            .ok_or_else(|| SecretError::PathNotFound(format!("invalid vault path: {path}")))?;
        let url = format!("{}/v1/{}/data/{}", self.vault_addr, mount, rest);

        let resp = self
            .client
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|e| SecretError::NetworkError(e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Err(SecretError::PathNotFound(path.into()));
        }

        if !resp.status().is_success() {
            return Err(SecretError::NetworkError(format!(
                "HTTP {}: reading {}",
                resp.status(),
                path
            )));
        }

        let data: VaultReadResponse = resp
            .json()
            .await
            .map_err(|e| SecretError::NetworkError(e.to_string()))?;

        data.data
            .data
            .get(field_key)
            .and_then(|v| v.as_str().map(String::from))
            .ok_or(SecretError::FieldNotFound {
                path: path.into(),
                field: field_key.into(),
            })
    }
}

#[async_trait::async_trait]
impl SecretResolver for VaultSecretResolver {
    async fn resolve(&self, logical_ref: &str, field_key: &str) -> Result<String, SecretError> {
        self.read_kv(logical_ref, field_key).await
    }

    async fn read_document(&self, path: &str) -> Result<HashMap<String, String>, SecretError> {
        self.read_document(path).await
    }
}

// ---------------------------------------------------------------------------
// Dev-mode resolver (env-var / file fallback)
// ---------------------------------------------------------------------------

/// For dev/test: resolves secrets from environment variables.
///
/// Convention: `ZK_SECRET_{LOGICAL_REF}_{FIELD_KEY}` (uppercased, `/` → `_`, `-` → `_`).
/// Example: `ZK_SECRET_OANDA_MAIN_API_KEY` for logical_ref="oanda/main", field_key="api_key".
pub struct DevSecretResolver;

impl DevSecretResolver {
    pub fn new() -> Self {
        Self
    }

    fn env_key(logical_ref: &str, field_key: &str) -> String {
        let ref_part = logical_ref
            .replace('/', "_")
            .replace('-', "_")
            .to_uppercase();
        let field_part = field_key.replace('-', "_").to_uppercase();
        format!("ZK_SECRET_{ref_part}_{field_part}")
    }
}

impl Default for DevSecretResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SecretResolver for DevSecretResolver {
    async fn resolve(&self, logical_ref: &str, field_key: &str) -> Result<String, SecretError> {
        let key = Self::env_key(logical_ref, field_key);
        env::var(&key).map_err(|_| SecretError::FieldNotFound {
            path: key,
            field: field_key.into(),
        })
    }

    async fn read_document(&self, path: &str) -> Result<HashMap<String, String>, SecretError> {
        // Scan env vars matching ZK_SECRET_{PATH}_* and collect them.
        let prefix = {
            let ref_part = path.replace('/', "_").replace('-', "_").to_uppercase();
            format!("ZK_SECRET_{ref_part}_")
        };
        let mut doc = HashMap::new();
        for (key, value) in env::vars() {
            if let Some(suffix) = key.strip_prefix(&prefix) {
                doc.insert(suffix.to_lowercase(), value);
            }
        }
        if doc.is_empty() {
            return Err(SecretError::PathNotFound(format!(
                "no env vars with prefix {prefix}"
            )));
        }
        Ok(doc)
    }
}

// ---------------------------------------------------------------------------
// Secret document helpers
// ---------------------------------------------------------------------------

/// Extract a Vault path from `venue_config` at a given JSON pointer.
///
/// Returns `None` if the pointer is absent or the value is empty.
pub fn extract_secret_ref(venue_config: &serde_json::Value, pointer: &str) -> Option<String> {
    venue_config
        .pointer(pointer)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Two-part ref: venue/account → venue-scoped path ---
    #[test]
    fn test_expand_logical_ref_venue_account() {
        let ctx = RefContext {
            venue: Some("oanda".into()),
            account_id: Some("main".into()),
            service_kind: "gw".into(),
            logical_id: "gw_oanda_1".into(),
        };
        let path = expand_logical_ref("oanda/main", "dev", &ctx);
        assert_eq!(path, "kv/zkbot/dev/venues/oanda/accounts/main");
    }

    // --- Two-part ref: ref venue differs from context venue (ref is authoritative) ---
    #[test]
    fn test_expand_logical_ref_cross_venue() {
        let ctx = RefContext {
            venue: Some("okx".into()),
            account_id: Some("primary".into()),
            service_kind: "gw".into(),
            logical_id: "gw_okx_1".into(),
        };
        // Ref says "oanda/backup" but context is okx — ref wins
        let path = expand_logical_ref("oanda/backup", "prod", &ctx);
        assert_eq!(path, "kv/zkbot/prod/venues/oanda/accounts/backup");
    }

    // --- Single-part ref with venue context → venue-scoped secret ---
    #[test]
    fn test_expand_logical_ref_single_part_with_venue() {
        let ctx = RefContext {
            venue: Some("okx".into()),
            account_id: None,
            service_kind: "gw".into(),
            logical_id: "gw_okx_1".into(),
        };
        let path = expand_logical_ref("shared_token", "dev", &ctx);
        assert_eq!(path, "kv/zkbot/dev/venues/okx/secrets/shared_token");
    }

    // --- Single-part ref without venue context → service-kind-scoped ---
    #[test]
    fn test_expand_logical_ref_single_part_no_venue() {
        let ctx = RefContext {
            venue: None,
            account_id: None,
            service_kind: "oms".into(),
            logical_id: "oms_dev_1".into(),
        };
        let path = expand_logical_ref("bootstrap", "prod", &ctx);
        assert_eq!(path, "kv/zkbot/prod/services/oms/bootstrap");
    }

    #[test]
    fn test_dev_secret_resolver_env_key() {
        assert_eq!(
            DevSecretResolver::env_key("oanda/main", "api_key"),
            "ZK_SECRET_OANDA_MAIN_API_KEY"
        );
        assert_eq!(
            DevSecretResolver::env_key("okx/trading-primary", "secret_key"),
            "ZK_SECRET_OKX_TRADING_PRIMARY_SECRET_KEY"
        );
    }

    #[test]
    fn test_extract_secret_ref_present() {
        let config = serde_json::json!({
            "secret_ref": "kv/trading/gw/8003"
        });
        assert_eq!(
            extract_secret_ref(&config, "/secret_ref"),
            Some("kv/trading/gw/8003".into())
        );
    }

    #[test]
    fn test_extract_secret_ref_empty() {
        let config = serde_json::json!({ "secret_ref": "" });
        assert_eq!(extract_secret_ref(&config, "/secret_ref"), None);
    }

    #[test]
    fn test_extract_secret_ref_missing() {
        let config = serde_json::json!({ "environment": "practice" });
        assert_eq!(extract_secret_ref(&config, "/secret_ref"), None);
    }

    #[tokio::test]
    async fn test_dev_resolver_read_document() {
        let key1 = "ZK_SECRET_KV_TRADING_GW_8003_APIKEY";
        let key2 = "ZK_SECRET_KV_TRADING_GW_8003_EXTRA";
        env::set_var(key1, "my-token");
        env::set_var(key2, "extra-val");

        let resolver = DevSecretResolver::new();
        let doc = resolver.read_document("kv/trading/gw/8003").await.unwrap();
        assert_eq!(doc.get("apikey").unwrap(), "my-token");
        assert_eq!(doc.get("extra").unwrap(), "extra-val");

        env::remove_var(key1);
        env::remove_var(key2);
    }

    #[tokio::test]
    async fn test_dev_resolver_read_document_empty() {
        let resolver = DevSecretResolver::new();
        let result = resolver.read_document("kv/nonexistent/path/999").await;
        assert!(result.is_err());
    }
}
