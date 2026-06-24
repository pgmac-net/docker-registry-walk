#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use reqwest::Client;
use tokio::sync::Mutex;
use url::Url;

use crate::registry::client::Credentials;

// ---------------------------------------------------------------------------
// Basic auth
// ---------------------------------------------------------------------------

/// Static `Authorization: Basic …` credentials.
pub struct BasicCredentials {
    header_value: String,
}

impl BasicCredentials {
    pub fn new(username: &str, password: &str) -> Self {
        let encoded = B64.encode(format!("{username}:{password}"));
        Self {
            header_value: format!("Basic {encoded}"),
        }
    }
}

#[async_trait]
impl Credentials for BasicCredentials {
    async fn get_authorization(&self, _http: &Client) -> Option<String> {
        Some(self.header_value.clone())
    }
}

// ---------------------------------------------------------------------------
// Bearer token auth
// ---------------------------------------------------------------------------

struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// Bearer token credentials with automatic token exchange and caching.
///
/// On the first call to `get_authorization`, probes `<base_url>/v2/` to obtain
/// the `WWW-Authenticate` challenge, exchanges credentials for a token at the
/// `realm` URL, and caches it. Refreshes automatically when the token expires.
pub struct BearerCredentials {
    probe_url: Url,
    username: String,
    password: String,
    token: Arc<Mutex<Option<CachedToken>>>,
}

impl BearerCredentials {
    pub fn new(base_url: &Url, username: String, password: String) -> Self {
        let probe_url = base_url.join("/v2/").unwrap_or_else(|_| base_url.clone());
        Self {
            probe_url,
            username,
            password,
            token: Arc::new(Mutex::new(None)),
        }
    }

    async fn refresh(&self, http: &Client) -> Option<String> {
        // Probe /v2/ to get the Bearer challenge.
        let resp = http.get(self.probe_url.clone()).send().await.ok()?;

        if resp.status() == reqwest::StatusCode::OK {
            // Registry is open — no token needed.
            return None;
        }

        let www_auth = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())?
            .to_owned();

        let challenge = parse_bearer_challenge(&www_auth)?;

        let mut token_url = Url::parse(&challenge.realm).ok()?;
        {
            let mut q = token_url.query_pairs_mut();
            if let Some(svc) = &challenge.service {
                q.append_pair("service", svc);
            }
            if let Some(scope) = &challenge.scope {
                q.append_pair("scope", scope);
            }
        }

        let token_resp = http
            .get(token_url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .ok()?;

        let body: serde_json::Value = token_resp.json().await.ok()?;

        let token_str = body["token"]
            .as_str()
            .or_else(|| body["access_token"].as_str())?
            .to_owned();

        let expires_in = body["expires_in"].as_u64().unwrap_or(300);
        // Subtract 10 s to account for clock skew and latency.
        let ttl = Duration::from_secs(expires_in.saturating_sub(10));

        let mut guard = self.token.lock().await;
        *guard = Some(CachedToken {
            value: token_str.clone(),
            expires_at: Instant::now() + ttl,
        });

        Some(token_str)
    }
}

#[async_trait]
impl Credentials for BearerCredentials {
    async fn get_authorization(&self, http: &Client) -> Option<String> {
        // Fast path: valid cached token.
        {
            let guard = self.token.lock().await;
            if let Some(cached) = &*guard
                && cached.expires_at > Instant::now()
            {
                return Some(format!("Bearer {}", cached.value));
            }
        }

        // Slow path: fetch / refresh.
        self.refresh(http).await.map(|t| format!("Bearer {t}"))
    }
}

// ---------------------------------------------------------------------------
// WWW-Authenticate parser
// ---------------------------------------------------------------------------

struct BearerChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

fn parse_bearer_challenge(header: &str) -> Option<BearerChallenge> {
    let rest = header.strip_prefix("Bearer ")?;

    let mut realm = None;
    let mut service = None;
    let mut scope = None;

    for (key, value) in parse_challenge_params(rest) {
        match key.as_str() {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            "scope" => scope = Some(value),
            _ => {}
        }
    }

    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

/// Parse `key="value",key="value"` pairs from a `WWW-Authenticate` challenge.
fn parse_challenge_params(s: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut rest = s;

    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_owned();
        rest = rest[eq + 1..].trim_start();

        if rest.starts_with('"') {
            rest = &rest[1..];
            let close = rest.find('"').unwrap_or(rest.len());
            params.push((key, rest[..close].to_owned()));
            rest = rest[close + 1..].trim_start_matches(',').trim_start();
        }
    }

    params
}

// ---------------------------------------------------------------------------
// Keyring
// ---------------------------------------------------------------------------

/// Stores and retrieves per-registry passwords from the OS keychain.
///
/// Service name format: `docker-registry-walk/<registry-name>`
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new(registry_name: &str) -> Self {
        Self {
            service: format!("docker-registry-walk/{registry_name}"),
        }
    }

    /// Retrieve the stored password for `username`, if any.
    pub fn get_password(&self, username: &str) -> Option<String> {
        keyring::Entry::new(&self.service, username)
            .ok()
            .and_then(|e| e.get_password().ok())
    }

    /// Store `password` for `username` in the OS keychain.
    pub fn set_password(&self, username: &str, password: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, username)?.set_password(password)?;
        Ok(())
    }

    /// Remove the stored credential for `username`.
    pub fn delete_password(&self, username: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, username)?.delete_credential()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Password prompt
// ---------------------------------------------------------------------------

/// Prompt for a password on the terminal with input masking.
///
/// Falls back to an empty string on I/O error (caller should treat that as
/// "no password provided").
pub fn prompt_password(username: &str) -> anyhow::Result<String> {
    rpassword::prompt_password(format!("Password for {username}: ")).map_err(Into::into)
}

/// Resolve a password using the following priority:
/// 1. Already provided (e.g. from `--password` CLI flag).
/// 2. OS keychain lookup via `KeyringStore`.
/// 3. Interactive terminal prompt (masked).
///
/// If `store_on_prompt` is true and the password came from the prompt, it is
/// saved to the keychain for future sessions.
pub fn resolve_password(
    username: &str,
    provided: Option<&str>,
    keyring: &KeyringStore,
    store_on_prompt: bool,
) -> anyhow::Result<String> {
    if let Some(pw) = provided {
        return Ok(pw.to_owned());
    }

    if let Some(pw) = keyring.get_password(username) {
        return Ok(pw);
    }

    let pw = prompt_password(username)?;
    if store_on_prompt && !pw.is_empty() {
        let _ = keyring.set_password(username, &pw);
    }
    Ok(pw)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_credentials_encodes_correctly() {
        let creds = BasicCredentials::new("user", "pass");
        // "user:pass" in base64 is "dXNlcjpwYXNz"
        assert_eq!(creds.header_value, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn parse_challenge_params_standard() {
        let params = parse_challenge_params(
            r#"realm="https://auth.example.com/token",service="registry.example.com",scope="repository:foo:pull""#,
        );
        assert_eq!(params.len(), 3);
        assert_eq!(
            params[0],
            ("realm".into(), "https://auth.example.com/token".into())
        );
        assert_eq!(params[1], ("service".into(), "registry.example.com".into()));
        assert_eq!(params[2], ("scope".into(), "repository:foo:pull".into()));
    }

    #[test]
    fn parse_bearer_challenge_extracts_fields() {
        let header = r#"Bearer realm="https://auth.example.com/token",service="registry.example.com",scope="repository:nginx:pull,push""#;
        let c = parse_bearer_challenge(header).unwrap();
        assert_eq!(c.realm, "https://auth.example.com/token");
        assert_eq!(c.service.as_deref(), Some("registry.example.com"));
        assert_eq!(c.scope.as_deref(), Some("repository:nginx:pull,push"));
    }

    #[test]
    fn parse_bearer_challenge_returns_none_for_basic() {
        assert!(parse_bearer_challenge("Basic realm=\"registry\"").is_none());
    }
}
