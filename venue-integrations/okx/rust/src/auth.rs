use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Sign a REST request per OKX v5 API spec.
///
/// `signature = Base64(HMAC-SHA256(secret, timestamp + method + requestPath + body))`
pub fn sign_rest(secret: &str, timestamp: &str, method: &str, path: &str, body: &str) -> String {
    let prehash = format!("{timestamp}{method}{path}{body}");
    hmac_sha256_b64(secret, &prehash)
}

/// Sign a WebSocket login per OKX v5 API spec.
///
/// `signature = Base64(HMAC-SHA256(secret, timestamp + "GET" + "/users/self/verify"))`
pub fn sign_ws(secret: &str, timestamp: &str) -> String {
    let prehash = format!("{timestamp}GET/users/self/verify");
    hmac_sha256_b64(secret, &prehash)
}

fn hmac_sha256_b64(secret: &str, message: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();
    base64::engine::general_purpose::STANDARD.encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_rest_deterministic() {
        let sig1 = sign_rest("sec", "2024-01-01T00:00:00.000Z", "GET", "/api/v5/account/balance", "");
        let sig2 = sign_rest("sec", "2024-01-01T00:00:00.000Z", "GET", "/api/v5/account/balance", "");
        assert_eq!(sig1, sig2);
        assert!(!sig1.is_empty());
    }

    #[test]
    fn test_sign_ws_deterministic() {
        let sig1 = sign_ws("sec", "1704067200");
        let sig2 = sign_ws("sec", "1704067200");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_body_changes_signature() {
        let a = sign_rest("s", "ts", "POST", "/p", "");
        let b = sign_rest("s", "ts", "POST", "/p", r#"{"x":1}"#);
        assert_ne!(a, b);
    }
}
