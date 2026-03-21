use serde::Deserialize;

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
        self.api_key = resolve_env_ref(&self.api_key, "api_key")?;
        self.secret_key = resolve_env_ref(&self.secret_key, "secret_key")?;
        self.passphrase = resolve_env_ref(&self.passphrase, "passphrase")?;

        // Fill WS URL default based on demo_mode if not explicitly set.
        if self.ws_private_url.is_none() {
            self.ws_private_url = Some(if self.demo_mode {
                "wss://wspap.okx.com:8443/ws/v5/private?brokerId=9999".to_string()
            } else {
                "wss://ws.okx.com:8443/ws/v5/private".to_string()
            });
        }

        if self.ws_public_url.is_none() {
            self.ws_public_url = Some(if self.demo_mode {
                "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999".to_string()
            } else {
                "wss://ws.okx.com:8443/ws/v5/public".to_string()
            });
        }

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
            demo_mode,
            api_base_url: default_api_base(),
            ws_private_url: None,
            ws_public_url: None,
        }
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
    fn test_public_only() {
        let cfg = OkxConfig::public_only(true);
        assert!(cfg.demo_mode);
        assert!(cfg.api_key.is_empty());
        let cfg = cfg.resolve_secrets().unwrap();
        assert!(cfg.ws_public_url().contains("wspap.okx.com"));
    }
}
