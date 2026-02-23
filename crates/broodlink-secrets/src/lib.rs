/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 *
 * This program is free software: you can redistribute it
 * and/or modify it under the terms of the GNU Affero
 * General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your
 * option) any later version.
 *
 * This program is distributed in the hope that it will be
 * useful, but WITHOUT ANY WARRANTY; without even the
 * implied warranty of MERCHANTABILITY or FITNESS FOR A
 * PARTICULAR PURPOSE. See the GNU Affero General Public
 * License for more details.
 *
 * You should have received a copy of the GNU Affero General
 * Public License along with this program. If not, see
 * <https://www.gnu.org/licenses/>.
 */

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Errors from secrets operations.
#[derive(thiserror::Error, Debug)]
pub enum SecretsError {
    #[error("secret not found: {0}")]
    NotFound(String),
    #[error("decrypt failed: {0}")]
    Decrypt(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
}

/// Trait for secrets providers.
#[async_trait::async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get(&self, key: &str) -> Result<String, SecretsError>;
    async fn list(&self) -> Result<Vec<String>, SecretsError>;
}

struct CachedSecret {
    value: String,
    expires_at: Instant,
}

/// SOPS-based secrets provider for development.
pub struct SopsProvider {
    secrets_file: PathBuf,
    identity: PathBuf,
    cache: Arc<RwLock<HashMap<String, CachedSecret>>>,
    cache_ttl: Duration,
    timeout: Duration,
}

impl SopsProvider {
    #[must_use]
    pub fn new(secrets_file: PathBuf, identity: PathBuf) -> Self {
        Self {
            secrets_file,
            identity,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_secs(300),
            timeout: Duration::from_secs(5),
        }
    }
}

#[async_trait::async_trait]
impl SecretsProvider for SopsProvider {
    async fn get(&self, key: &str) -> Result<String, SecretsError> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(key) {
                if entry.expires_at > Instant::now() {
                    return Ok(entry.value.clone());
                }
            }
        }

        let extract_arg = format!("[\"{key}\"]");
        let result = tokio::time::timeout(
            self.timeout,
            tokio::process::Command::new("sops")
                .arg("--decrypt")
                .arg("--extract")
                .arg(&extract_arg)
                .arg(&self.secrets_file)
                .env("SOPS_AGE_KEY_FILE", &self.identity)
                .output(),
        )
        .await
        .map_err(|_| SecretsError::Unavailable("sops command timed out".to_string()))?
        .map_err(|e| SecretsError::Decrypt(format!("sops exec failed: {e}")))?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            if stderr.contains("not found") || stderr.contains("does not exist") {
                return Err(SecretsError::NotFound(key.to_string()));
            }
            return Err(SecretsError::Decrypt(format!(
                "sops returned {}: {stderr}",
                result.status
            )));
        }

        let value = String::from_utf8_lossy(&result.stdout).trim().to_string();

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(
                key.to_string(),
                CachedSecret {
                    value: value.clone(),
                    expires_at: Instant::now() + self.cache_ttl,
                },
            );
        }

        Ok(value)
    }

    async fn list(&self) -> Result<Vec<String>, SecretsError> {
        let result = tokio::time::timeout(
            self.timeout,
            tokio::process::Command::new("sops")
                .arg("--decrypt")
                .arg("--output-type")
                .arg("json")
                .arg(&self.secrets_file)
                .env("SOPS_AGE_KEY_FILE", &self.identity)
                .output(),
        )
        .await
        .map_err(|_| SecretsError::Unavailable("sops command timed out".to_string()))?
        .map_err(|e| SecretsError::Decrypt(format!("sops exec failed: {e}")))?;

        if !result.status.success() {
            return Err(SecretsError::Decrypt(
                String::from_utf8_lossy(&result.stderr).to_string(),
            ));
        }

        let json: serde_json::Value = serde_json::from_slice(&result.stdout)
            .map_err(|e| SecretsError::Decrypt(format!("failed to parse sops output: {e}")))?;

        let mut keys = Vec::new();
        if let Some(obj) = json.as_object() {
            for (section, value) in obj {
                if let Some(inner) = value.as_object() {
                    for inner_key in inner.keys() {
                        keys.push(format!("{section}.{inner_key}"));
                    }
                }
            }
        }

        Ok(keys)
    }
}

/// Infisical-based secrets provider for production.
pub struct InfisicalProvider {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl InfisicalProvider {
    /// Create a new Infisical provider.
    ///
    /// # Errors
    ///
    /// Returns `SecretsError::Unavailable` if the HTTP client cannot be built.
    pub fn new(base_url: String, token: String) -> Result<Self, SecretsError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| SecretsError::Unavailable(format!("failed to create HTTP client: {e}")))?;
        Ok(Self {
            base_url,
            token,
            client,
        })
    }
}

#[async_trait::async_trait]
impl SecretsProvider for InfisicalProvider {
    async fn get(&self, key: &str) -> Result<String, SecretsError> {
        let url = format!("{}/api/v3/secrets/raw/{key}", self.base_url);

        let mut last_err = None;
        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = Duration::from_millis(100 * 2u64.pow(attempt));
                tokio::time::sleep(delay).await;
            }

            match self.client.get(&url).bearer_auth(&self.token).send().await {
                Ok(resp) => {
                    if resp.status() == reqwest::StatusCode::NOT_FOUND {
                        return Err(SecretsError::NotFound(key.to_string()));
                    }
                    if resp.status().is_server_error() {
                        last_err = Some(format!("server error: {}", resp.status()));
                        continue;
                    }
                    let body: serde_json::Value = resp
                        .json()
                        .await
                        .map_err(|e| SecretsError::Decrypt(format!("parse error: {e}")))?;
                    return body["secret"]["secretValue"]
                        .as_str()
                        .map(String::from)
                        .ok_or_else(|| {
                            SecretsError::Decrypt("missing secretValue in response".to_string())
                        });
                }
                Err(e) => {
                    last_err = Some(format!("request failed: {e}"));
                }
            }
        }

        Err(SecretsError::Unavailable(
            last_err.unwrap_or_else(|| "unknown error".to_string()),
        ))
    }

    async fn list(&self) -> Result<Vec<String>, SecretsError> {
        let url = format!("{}/api/v3/secrets/raw", self.base_url);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| SecretsError::Unavailable(format!("request failed: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SecretsError::Decrypt(format!("parse error: {e}")))?;

        let mut keys = Vec::new();
        if let Some(secrets) = body["secrets"].as_array() {
            for secret in secrets {
                if let Some(key) = secret["secretKey"].as_str() {
                    keys.push(key.to_string());
                }
            }
        }

        Ok(keys)
    }
}

/// Create a secrets provider based on the provider name.
///
/// # Errors
///
/// Returns `SecretsError` if the provider cannot be created.
/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// # Errors
/// Returns `SecretsError` if the provider name is unknown or required parameters are missing.
pub fn create_provider(
    provider: &str,
    sops_file: Option<&str>,
    age_identity: Option<&str>,
    infisical_url: Option<&str>,
    infisical_token: Option<&str>,
) -> Result<Box<dyn SecretsProvider>, SecretsError> {
    match provider {
        "sops" => {
            let file = sops_file
                .ok_or_else(|| SecretsError::Unavailable("sops_file not configured".to_string()))?;
            let identity = age_identity.ok_or_else(|| {
                SecretsError::Unavailable("age_identity not configured".to_string())
            })?;
            Ok(Box::new(SopsProvider::new(
                expand_tilde(file),
                expand_tilde(identity),
            )))
        }
        "infisical" => {
            let url = infisical_url.ok_or_else(|| {
                SecretsError::Unavailable("infisical_url not configured".to_string())
            })?;
            let token = infisical_token.ok_or_else(|| {
                SecretsError::Unavailable("infisical_token not configured".to_string())
            })?;
            Ok(Box::new(InfisicalProvider::new(
                url.to_string(),
                token.to_string(),
            )?))
        }
        other => Err(SecretsError::Unavailable(format!(
            "unknown provider: {other}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_sops_provider_creation_succeeds() {
        let result = create_provider(
            "sops",
            Some("./secrets.enc.json"),
            Some("~/.config/sops/age/keys.txt"),
            None,
            None,
        );
        assert!(
            result.is_ok(),
            "sops provider with valid args should succeed"
        );
    }

    #[test]
    fn test_sops_missing_sops_file() {
        let result = create_provider("sops", None, Some("key.txt"), None, None);
        match result {
            Err(e) => assert!(
                e.to_string().contains("sops_file"),
                "error should mention sops_file"
            ),
            Ok(_) => panic!("sops without sops_file should fail"),
        }
    }

    #[test]
    fn test_sops_missing_age_identity() {
        let result = create_provider("sops", Some("secrets.json"), None, None, None);
        match result {
            Err(e) => assert!(
                e.to_string().contains("age_identity"),
                "error should mention age_identity"
            ),
            Ok(_) => panic!("sops without age_identity should fail"),
        }
    }

    #[test]
    fn test_unknown_provider() {
        let result = create_provider("vault", None, None, None, None);
        match result {
            Err(e) => assert!(
                e.to_string().contains("unknown provider"),
                "error should say unknown provider"
            ),
            Ok(_) => panic!("unknown provider should fail"),
        }
    }

    #[test]
    fn test_expand_tilde() {
        let path = expand_tilde("~/foo/bar");
        // Should not start with ~ anymore (expanded to HOME)
        assert!(!path.starts_with("~"), "tilde should be expanded");
        assert!(path.to_string_lossy().ends_with("foo/bar"));
    }
}
