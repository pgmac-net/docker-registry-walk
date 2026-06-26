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

    /// Exchange credentials for a token at the given realm with optional service/scope.
    /// Returns the JSON body on success, or `None` on HTTP/network error.
    async fn exchange_token(
        &self,
        http: &Client,
        realm: &Url,
        service: Option<&str>,
        scope: Option<&str>,
    ) -> Option<serde_json::Value> {
        let mut url = realm.clone();
        {
            let mut q = url.query_pairs_mut();
            if let Some(svc) = service {
                q.append_pair("service", svc);
            }
            if let Some(s) = scope {
                q.append_pair("scope", s);
            }
        }
        http.get(url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()
    }

    /// Try to extract a token string from a token endpoint response body.
    fn extract_token(body: &serde_json::Value) -> Option<String> {
        body["token"]
            .as_str()
            .or_else(|| body["access_token"].as_str())
            .map(|s| s.to_owned())
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

        let token_url = Url::parse(&challenge.realm).ok()?;

        // Guard: only send credentials to a trusted realm host.
        if !is_trusted_realm(&token_url, &self.probe_url) {
            return None;
        }

        // Try with the scope from the challenge first.
        let mut body = self
            .exchange_token(
                http,
                &token_url,
                challenge.service.as_deref(),
                challenge.scope.as_deref(),
            )
            .await;

        // Some registries (e.g. Docker Hub) issue a scope in the `/v2/` 401 challenge
        // that their own token endpoint rejects. Retry without scope.
        if body.as_ref().and_then(Self::extract_token).is_none() {
            body = self
                .exchange_token(http, &token_url, challenge.service.as_deref(), None)
                .await;
        }

        let body = body?;
        let token_str = Self::extract_token(&body)?;

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

    async fn get_authorization_for_challenge(
        &self,
        http: &Client,
        www_auth: &str,
    ) -> Option<String> {
        // Exchange a fresh token using the scope from this specific 401 challenge.
        // This handles registries (e.g. Docker Hub) that issue per-endpoint scoped tokens.
        let challenge = parse_bearer_challenge(www_auth)?;

        let token_url = Url::parse(&challenge.realm).ok()?;

        // Guard: only send credentials to a trusted realm host.
        if !is_trusted_realm(&token_url, &self.probe_url) {
            return None;
        }

        // Try with the scope from the challenge first.
        let mut body = self
            .exchange_token(
                http,
                &token_url,
                challenge.service.as_deref(),
                challenge.scope.as_deref(),
            )
            .await;

        // Fall back to no scope if the token endpoint rejects the scope.
        if body.as_ref().and_then(Self::extract_token).is_none() {
            body = self
                .exchange_token(http, &token_url, challenge.service.as_deref(), None)
                .await;
        }

        let body = body?;
        let token_str = Self::extract_token(&body)?;

        // Don't cache: this token is scoped to one specific endpoint.  Caching
        // it would cause the fast-path in get_authorization to serve the wrong
        // (narrow) scope to other endpoints, triggering a cascade of 401s.
        Some(format!("Bearer {token_str}"))
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
// Realm trust validation
// ---------------------------------------------------------------------------

/// Returns `true` if `realm` is a host we should send credentials to.
///
/// Rules:
/// 1. Scheme must be `https`.  Plain `http` is allowed only for loopback
///    addresses (localhost / 127.0.0.1 / ::1) so local dev registries work.
/// 2. The realm host must either:
///    a. Exactly match the registry host, OR
///    b. Share the same registered domain (last two DNS labels, e.g. `docker.io`).
///    This is a heuristic that covers the common pattern of a separate auth
///    service under the same domain (e.g. `auth.docker.io` for `registry-1.docker.io`).
///    It does not handle multi-label public suffixes (e.g. `.co.uk`).
fn is_trusted_realm(realm: &Url, registry: &Url) -> bool {
    let loopback = ["localhost", "127.0.0.1", "::1"];

    match realm.scheme() {
        "https" => {}
        "http" => {
            if !loopback.contains(&realm.host_str().unwrap_or("")) {
                return false;
            }
        }
        _ => return false,
    }

    let realm_host = realm.host_str().unwrap_or("");
    let registry_host = registry.host_str().unwrap_or("");

    if realm_host.is_empty() || registry_host.is_empty() {
        return false;
    }

    if realm_host == registry_host {
        return true;
    }

    // For IP-addressed registries, only exact match is trusted.
    // The DNS-label heuristic below treats octets as labels, which would let
    // a domain like "evil.0.1" appear to share the registered domain with
    // a registry at "10.0.0.1".
    match registry.host() {
        Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)) => return false,
        _ => {}
    }

    // Compare last two DNS labels (e.g. "docker" + "io").
    // rsplitn(3, '.') on "auth.docker.io" yields ["io", "docker", "auth"].
    // Note: multi-label public suffixes (e.g. ".co.uk") are not handled.
    let r_parts: Vec<&str> = realm_host.rsplitn(3, '.').collect();
    let g_parts: Vec<&str> = registry_host.rsplitn(3, '.').collect();

    r_parts.len() >= 2 && g_parts.len() >= 2 && r_parts[0] == g_parts[0] && r_parts[1] == g_parts[1]
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

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn trusted_realm_same_host() {
        assert!(is_trusted_realm(
            &u("https://registry.example.com/token"),
            &u("https://registry.example.com/v2/")
        ));
    }

    #[test]
    fn trusted_realm_same_domain_different_subdomain() {
        // auth.docker.io is trusted for registry-1.docker.io
        assert!(is_trusted_realm(
            &u("https://auth.docker.io/token"),
            &u("https://registry-1.docker.io/v2/")
        ));
    }

    #[test]
    fn trusted_realm_loopback_http_allowed() {
        assert!(is_trusted_realm(
            &u("http://localhost:5001/token"),
            &u("http://localhost:5000/v2/")
        ));
        assert!(is_trusted_realm(
            &u("http://127.0.0.1:5001/token"),
            &u("http://127.0.0.1:5000/v2/")
        ));
    }

    #[test]
    fn untrusted_realm_different_domain() {
        assert!(!is_trusted_realm(
            &u("https://attacker.com/steal"),
            &u("https://registry.example.com/v2/")
        ));
    }

    #[test]
    fn untrusted_realm_http_non_loopback() {
        assert!(!is_trusted_realm(
            &u("http://auth.example.com/token"),
            &u("https://registry.example.com/v2/")
        ));
    }

    #[test]
    fn untrusted_realm_subdomain_of_attacker_sharing_tld() {
        // attacker.com must not pass even though both end in ".com"
        assert!(!is_trusted_realm(
            &u("https://attacker.com/token"),
            &u("https://registry.example.com/v2/")
        ));
    }

    #[test]
    fn untrusted_realm_different_host_for_ip_registry() {
        // IP-addressed registries require exact host match; the DNS-label
        // heuristic is disabled to avoid octet-spoofing attacks.
        assert!(!is_trusted_realm(
            &u("https://auth.example.com/token"),
            &u("https://10.0.0.1:5000/v2/")
        ));
    }

    #[test]
    fn trusted_realm_exact_ip_match() {
        assert!(is_trusted_realm(
            &u("https://10.0.0.1:5001/token"),
            &u("https://10.0.0.1:5000/v2/")
        ));
    }
}
