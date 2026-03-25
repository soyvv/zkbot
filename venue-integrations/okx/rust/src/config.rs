use serde::Deserialize;
use serde_json::Value;

fn default_api_base() -> String {
    "https://www.okx.com".to_string()
}

/// OKX venue adapter configuration.
///
/// Parsed from the `ZK_VENUE_CONFIG` JSON blob. Fields whose value starts with
/// `env:` are resolved from the named environment variable at startup.
///
/// When `ws_private_url` is not explicitly set, it defaults based on `demo_mode`:
/// - demo: `wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999`
/// - live: `wss://ws.okx.com:8443/ws/v5/private`
#[derive(Debug, Clone, Deserialize)]
pub struct OkxConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default)]
    pub passphrase: String,
    #[serde(default)]
    pub secret_ref: String,
    #[serde(default)]
    pub passphrase_ref: String,
    #[serde(default)]
    pub demo_mode: bool,
    #[serde(default = "default_api_base")]
    pub api_base_url: String,
    /// If omitted, derived from `demo_mode` in `resolve_secrets()`.
    #[serde(default)]
    pub ws_private_url: Option<String>,
    /// If omitted, derived from `demo_mode` in `resolve_secrets()`.
    #[serde(default)]
    pub ws_public_url: Option<String>,
}

impl OkxConfig {
    /// Resolve `env:VAR_NAME` references in credential fields and fill WS URL default.
    pub fn resolve_secrets(mut self) -> anyhow::Result<Self> {
        if (!self.secret_ref.is_empty()) && (self.api_key.is_empty() || self.secret_key.is_empty() || self.passphrase.is_empty()) {
            let secret_doc = read_vault_secret(&self.secret_ref)?;
            if self.api_key.is_empty() {
                self.api_key = read_secret_field(&secret_doc, &self.secret_ref, "apikey")?;
            }
            if self.secret_key.is_empty() {
                self.secret_key = read_secret_field(&secret_doc, &self.secret_ref, "secretkey")?;
            }
            if self.passphrase.is_empty() && self.passphrase_ref.is_empty() {
                self.passphrase = read_secret_field(&secret_doc, &self.secret_ref, "passphrase")?;
            }
        }

        if self.passphrase.is_empty() && !self.passphrase_ref.is_empty() {
            let passphrase_doc = read_vault_secret(&self.passphrase_ref)?;
            self.passphrase = read_secret_field(&passphrase_doc, &self.passphrase_ref, "passphrase")?;
        }

        self.api_key = resolve_env_ref(&self.api_key, "api_key")?;
        self.secret_key = resolve_env_ref(&self.secret_key, "secret_key")?;
        self.passphrase = resolve_env_ref(&self.passphrase, "passphrase")?;

        self.ws_private_url = Some(resolve_ws_url(
            self.ws_private_url.take(),
            self.demo_mode,
            WsChannel::Private,
        ));
        self.ws_public_url = Some(resolve_ws_url(
            self.ws_public_url.take(),
            self.demo_mode,
            WsChannel::Public,
        ));

        Ok(self)
    }

    /// Returns the resolved private WS URL. Panics if called before `resolve_secrets()`.
    pub fn ws_url(&self) -> &str {
        self.ws_private_url
            .as_deref()
            .expect("ws_private_url not resolved — call resolve_secrets() first")
    }

    /// Returns the resolved public WS URL. Panics if called before `resolve_secrets()`.
    pub fn ws_public_url(&self) -> &str {
        self.ws_public_url
            .as_deref()
            .expect("ws_public_url not resolved — call resolve_secrets() first")
    }

    /// Config for public-only access (no credentials required).
    pub fn public_only(demo_mode: bool) -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            passphrase: String::new(),
            secret_ref: String::new(),
            passphrase_ref: String::new(),
            demo_mode,
            api_base_url: default_api_base(),
            ws_private_url: None,
            ws_public_url: None,
        }
    }
}

#[derive(Clone, Copy)]
enum WsChannel {
    Private,
    Public,
}

fn resolve_ws_url(url: Option<String>, demo_mode: bool, channel: WsChannel) -> String {
    match url {
        Some(value) if !value.trim().is_empty() => value,
        _ => match (demo_mode, channel) {
            (true, WsChannel::Private) => "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999".to_string(),
            (false, WsChannel::Private) => "wss://ws.okx.com:8443/ws/v5/private".to_string(),
            (true, WsChannel::Public) => "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999".to_string(),
            (false, WsChannel::Public) => "wss://ws.okx.com:8443/ws/v5/public".to_string(),
        },
    }
}

fn resolve_env_ref(value: &str, field_name: &str) -> anyhow::Result<String> {
    if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).map_err(|_| {
            anyhow::anyhow!(
                "OKX config field `{field_name}` references env var `{var_name}` which is not set"
            )
        })
    } else {
        Ok(value.to_string())
    }
}

fn normalize_vault_path(path: &str) -> String {
    if path.starts_with("kv/") {
        path.to_string()
    } else {
        format!("kv/{path}")
    }
}

fn read_vault_secret(secret_path: &str) -> anyhow::Result<Value> {
    let vault_addr = std::env::var("VAULT_ADDR")
        .map_err(|_| anyhow::anyhow!("VAULT_ADDR is required to resolve OKX secret_ref `{secret_path}`"))?;
    let vault_token = std::env::var("VAULT_TOKEN")
        .map_err(|_| anyhow::anyhow!("VAULT_TOKEN is required to resolve OKX secret_ref `{secret_path}`"))?;

    let normalized = normalize_vault_path(secret_path);
    let (mount, rest) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid Vault path `{secret_path}`"))?;
    let url = format!("{}/v1/{}/data/{}", vault_addr.trim_end_matches('/'), mount, rest);

    let response = reqwest::blocking::Client::new()
        .get(url)
        .header("X-Vault-Token", vault_token)
        .send()
        .map_err(|e| anyhow::anyhow!("failed to read Vault secret `{secret_path}`: {e}"))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "failed to read Vault secret `{secret_path}`: HTTP {}",
            response.status()
        ));
    }

    response
        .json::<Value>()
        .map_err(|e| anyhow::anyhow!("failed to parse Vault response for `{secret_path}`: {e}"))
}

fn read_secret_field(doc: &Value, secret_path: &str, field: &str) -> anyhow::Result<String> {
    doc.get("data")
        .and_then(|v| v.get("data"))
        .and_then(|v| v.get(field))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Vault secret `{secret_path}` missing field `{field}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_defaults() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.demo_mode);
        assert_eq!(cfg.api_base_url, "https://www.okx.com");
        assert!(cfg.ws_private_url.is_none()); // not yet resolved
    }

    #[test]
    fn test_ws_url_demo() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert!(cfg.ws_url().contains("wspap.okx.com"));
    }

    #[test]
    fn test_ws_url_live() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":false}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert_eq!(cfg.ws_url(), "wss://ws.okx.com:8443/ws/v5/private");
    }

    #[test]
    fn test_ws_url_explicit_override() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true,"ws_private_url":"wss://custom:1234"}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert_eq!(cfg.ws_url(), "wss://custom:1234");
    }

    #[test]
    fn test_resolve_literal() {
        assert_eq!(resolve_env_ref("literal", "f").unwrap(), "literal");
    }

    #[test]
    fn test_resolve_env() {
        std::env::set_var("_ZK_OKX_TEST", "val");
        assert_eq!(resolve_env_ref("env:_ZK_OKX_TEST", "f").unwrap(), "val");
        std::env::remove_var("_ZK_OKX_TEST");
    }

    #[test]
    fn test_resolve_env_missing() {
        assert!(resolve_env_ref("env:_ZK_NONEXISTENT", "f").is_err());
    }

    #[test]
    fn test_ws_public_url_demo() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert!(cfg.ws_public_url().contains("wspap.okx.com"));
        assert!(cfg.ws_public_url().contains("/public"));
    }

    #[test]
    fn test_ws_public_url_live() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":false}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert_eq!(cfg.ws_public_url(), "wss://ws.okx.com:8443/ws/v5/public");
    }

    #[test]
    fn test_blank_private_ws_url_defaults_from_demo_mode() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true,"ws_private_url":""}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert_eq!(cfg.ws_url(), "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999");
    }

    #[test]
    fn test_blank_public_ws_url_defaults_from_demo_mode() {
        let json = r#"{"api_key":"k","secret_key":"s","passphrase":"p","demo_mode":true,"ws_public_url":"   "}"#;
        let cfg: OkxConfig = serde_json::from_str(json).unwrap();
        let cfg = cfg.resolve_secrets().unwrap();
        assert_eq!(cfg.ws_public_url(), "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999");
    }

    #[test]
    fn test_public_only() {
        let cfg = OkxConfig::public_only(true);
        assert!(cfg.demo_mode);
        assert!(cfg.api_key.is_empty());
        let cfg = cfg.resolve_secrets().unwrap();
        assert!(cfg.ws_public_url().contains("wspap.okx.com"));
    }

    #[test]
    fn test_normalize_vault_path() {
        assert_eq!(normalize_vault_path("trading/gw/8002"), "kv/trading/gw/8002");
        assert_eq!(normalize_vault_path("kv/trading/gw/8002"), "kv/trading/gw/8002");
    }

    #[test]
    fn test_read_secret_field() {
        let doc: Value = serde_json::from_str(r#"{"data":{"data":{"apikey":"a","secretkey":"b","passphrase":"c"}}}"#).unwrap();
        assert_eq!(read_secret_field(&doc, "trading/gw/8002", "apikey").unwrap(), "a");
        assert_eq!(read_secret_field(&doc, "trading/gw/8002", "secretkey").unwrap(), "b");
        assert_eq!(read_secret_field(&doc, "trading/gw/8002", "passphrase").unwrap(), "c");
    }
}
