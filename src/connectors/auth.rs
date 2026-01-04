//! Authentication utilities for Polymarket API.
//!
//! Polymarket uses HMAC-SHA256 signature authentication for API requests.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid API secret: {0}")]
    InvalidSecret(String),
    #[error("HMAC computation failed: {0}")]
    HmacError(String),
}

/// API credentials for Polymarket authentication.
#[derive(Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: Option<String>,
}

impl ApiCredentials {
    /// Creates new credentials from environment variables.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("POLYMARKET_API_KEY").ok()?;
        let api_secret = std::env::var("POLYMARKET_API_SECRET").ok()?;
        let passphrase = std::env::var("POLYMARKET_API_PASSPHRASE").ok();

        Some(Self {
            api_key,
            api_secret,
            passphrase,
        })
    }

    /// Creates new credentials from explicit values.
    pub fn new(api_key: String, api_secret: String, passphrase: Option<String>) -> Self {
        Self {
            api_key,
            api_secret,
            passphrase,
        }
    }

    /// Generates HMAC-SHA256 signature for a request.
    ///
    /// The signature is computed over: timestamp + method + path + body
    pub fn sign(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, AuthError> {
        let message = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);

        // Decode the base64-encoded secret
        let secret_bytes = BASE64.decode(&self.api_secret)
            .map_err(|e| AuthError::InvalidSecret(e.to_string()))?;

        let mut mac = HmacSha256::new_from_slice(&secret_bytes)
            .map_err(|e| AuthError::HmacError(e.to_string()))?;

        mac.update(message.as_bytes());

        let result = mac.finalize();
        Ok(BASE64.encode(result.into_bytes()))
    }

    /// Returns the current timestamp for signing.
    pub fn timestamp() -> String {
        Utc::now().timestamp().to_string()
    }

    /// Returns authentication headers for an HTTP request.
    pub fn auth_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Vec<(String, String)>, AuthError> {
        let timestamp = Self::timestamp();
        let signature = self.sign(&timestamp, method, path, body)?;

        let mut headers = vec![
            ("POLY_API_KEY".to_string(), self.api_key.clone()),
            ("POLY_SIGNATURE".to_string(), signature),
            ("POLY_TIMESTAMP".to_string(), timestamp),
        ];

        if let Some(ref passphrase) = self.passphrase {
            headers.push(("POLY_PASSPHRASE".to_string(), passphrase.clone()));
        }

        Ok(headers)
    }

    /// Returns the WebSocket authentication payload.
    pub fn ws_auth_payload(&self) -> serde_json::Value {
        let mut auth = serde_json::json!({
            "apiKey": self.api_key,
            "secret": self.api_secret,
        });

        if let Some(ref passphrase) = self.passphrase {
            auth["passphrase"] = serde_json::Value::String(passphrase.clone());
        }

        auth
    }
}

impl std::fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiCredentials")
            .field("api_key", &"[REDACTED]")
            .field("api_secret", &"[REDACTED]")
            .field("passphrase", &self.passphrase.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_debug_redacts() {
        let creds = ApiCredentials::new(
            "test_key".to_string(),
            "dGVzdF9zZWNyZXQ=".to_string(), // base64 encoded "test_secret"
            Some("test_pass".to_string()),
        );
        let debug_str = format!("{:?}", creds);
        assert!(!debug_str.contains("test_key"));
        assert!(!debug_str.contains("test_secret"));
        assert!(!debug_str.contains("test_pass"));
        assert!(debug_str.contains("[REDACTED]"));
    }
}
