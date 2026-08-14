#![allow(dead_code)]

use std::fmt;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64_URL},
};
use reqwest::Client;
use tokio::sync::Mutex;
use url::Url;

use crate::registry::client::Credentials;

/// Hosts for which plain `http` is acceptable, so local dev registries work.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Keyring account under which an access token is stored.
///
/// A token-authenticated profile has no username, so the account name cannot
/// be derived from one. Used even when a username *is* configured, so a stored
/// password and a stored token coexist instead of overwriting each other.
pub const TOKEN_ACCOUNT: &str = "__token__";

// ---------------------------------------------------------------------------
// Secret wrapper
// ---------------------------------------------------------------------------

/// A credential that must not appear in debug output.
///
/// `AppEvent` derives `Debug` and carries entered credentials between the key
/// handler and the event loop, so a stray `{:?}` — or a panic payload — would
/// otherwise print the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the underlying value. Named to make call sites conspicuous.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

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

        let token_url = Url::parse(&challenge.realm).ok()?;

        // Guard: only send credentials to a trusted realm host.
        if !is_trusted_realm(&token_url, &self.probe_url) {
            return None;
        }

        let (token_str, ttl) = exchange_with_scope_fallback(
            http,
            &token_url,
            challenge.service.as_deref(),
            challenge.scope.as_deref(),
            RealmAuth::Basic {
                username: &self.username,
                password: &self.password,
            },
        )
        .await?;

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

        let (token_str, _ttl) = exchange_with_scope_fallback(
            http,
            &token_url,
            challenge.service.as_deref(),
            challenge.scope.as_deref(),
            RealmAuth::Basic {
                username: &self.username,
                password: &self.password,
            },
        )
        .await?;

        // Don't cache: this token is scoped to one specific endpoint.  Caching
        // it would cause the fast-path in get_authorization to serve the wrong
        // (narrow) scope to other endpoints, triggering a cascade of 401s.
        Some(format!("Bearer {token_str}"))
    }
}

// ---------------------------------------------------------------------------
// Token realm exchange (shared by BearerCredentials and AccessTokenCredentials)
// ---------------------------------------------------------------------------

/// How to present credentials to a Docker v2 token realm.
#[derive(Clone, Copy)]
enum RealmAuth<'a> {
    Basic {
        username: &'a str,
        password: &'a str,
    },
    /// Pass a token straight through. Needs no username, which is what makes
    /// it usable by a token-only profile.
    Bearer(&'a str),
    /// Send no credentials at all. Public registries still mint a (read-only,
    /// scope-bound) token this way — it is how an unauthenticated `docker pull`
    /// works — so it is the anonymous half of `GhcrCredentials`.
    Anonymous,
}

/// Build the token-endpoint URL for a challenge.
///
/// Appends `service`/`scope` rather than replacing the query, so a realm that
/// already carries query parameters keeps them.
fn realm_url(realm: &Url, service: Option<&str>, scope: Option<&str>) -> Url {
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
    url
}

/// Try to extract a token string from a token endpoint response body.
fn extract_token(body: &serde_json::Value) -> Option<String> {
    body["token"]
        .as_str()
        .or_else(|| body["access_token"].as_str())
        .map(|s| s.to_owned())
}

/// Lifetime of a freshly-minted token, less an allowance for clock skew and
/// latency. Defaults to the Docker registry spec's 300 s when unstated.
fn token_ttl(body: &serde_json::Value) -> Duration {
    let expires_in = body["expires_in"].as_u64().unwrap_or(300);
    Duration::from_secs(expires_in.saturating_sub(10))
}

/// GET a token endpoint with the given credentials and parse the JSON body.
/// Returns `None` on HTTP/network/parse error.
async fn fetch_token_body(
    http: &Client,
    url: Url,
    auth: RealmAuth<'_>,
) -> Option<serde_json::Value> {
    let req = http.get(url);
    let req = match auth {
        RealmAuth::Basic { username, password } => req.basic_auth(username, Some(password)),
        RealmAuth::Bearer(token) => req.bearer_auth(token),
        RealmAuth::Anonymous => req,
    };
    req.send().await.ok()?.json().await.ok()
}

/// Exchange credentials for a token, retrying without the scope.
///
/// Some registries (e.g. Docker Hub) issue a scope in the `/v2/` 401 challenge
/// that their own token endpoint then rejects. The retry is skipped when there
/// was no scope to drop, since it would repeat an identical request.
async fn exchange_with_scope_fallback(
    http: &Client,
    realm: &Url,
    service: Option<&str>,
    scope: Option<&str>,
    auth: RealmAuth<'_>,
) -> Option<(String, Duration)> {
    if let Some(body) = fetch_token_body(http, realm_url(realm, service, scope), auth).await
        && let Some(token) = extract_token(&body)
    {
        return Some((token, token_ttl(&body)));
    }

    if scope.is_some()
        && let Some(body) = fetch_token_body(http, realm_url(realm, service, None), auth).await
        && let Some(token) = extract_token(&body)
    {
        return Some((token, token_ttl(&body)));
    }

    None
}

// ---------------------------------------------------------------------------
// Access token auth
// ---------------------------------------------------------------------------

fn bearer_header(token: &str) -> String {
    format!("Bearer {token}")
}

/// How an access token was successfully presented to a Docker v2 token realm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealmMode {
    /// Passed straight through as `Authorization: Bearer`.
    Passthrough,
    /// Basic, with the configured username and the token as the password.
    BasicConfiguredUser,
    /// Basic, with the username decoded from the token's `sub` claim.
    BasicTokenSubject,
    /// Basic, with the token as both username and password — Docker's
    /// identity-token convention.
    BasicTokenAsUser,
}

/// Which realm presentations to try, in order.
///
/// Pass-through comes first: it is the most likely to work against
/// Artifactory (the realm is the same front door that accepts the token) and
/// it needs no username, which matters because a token-only profile has none.
///
/// Once a mode is known to work it is the only one attempted. Besides saving
/// round trips, that stops repeated failing Basic attempts carrying a *real*
/// username from counting against Artifactory's "Max Failed Login Attempts"
/// policy for that user.
fn realm_attempt_order(
    sticky: Option<RealmMode>,
    has_username: bool,
    has_subject: bool,
) -> Vec<RealmMode> {
    if let Some(mode) = sticky {
        return vec![mode];
    }

    let mut order = vec![RealmMode::Passthrough];
    if has_username {
        order.push(RealmMode::BasicConfiguredUser);
    }
    if has_subject {
        order.push(RealmMode::BasicTokenSubject);
    }
    order.push(RealmMode::BasicTokenAsUser);
    order
}

/// A JFrog-style access token sent as `Authorization: Bearer <token>` — the
/// way the Terraform `jfrog/artifactory` provider authenticates.
///
/// Needs no username, which is the point: an Artifactory profile can
/// authenticate with a token alone.
///
/// **`get_authorization` is unconditional and stateless, and must stay that
/// way.** `RegistryClient::for_artifactory_repo` shares one
/// `Arc<dyn Credentials>` between the client for the Artifactory REST API
/// (`/api/repositories`) and the client for a Docker repo-key
/// (`/api/docker/<key>/v2/...`). Those endpoints have different auth
/// verifiers, and the raw access token is the only value valid at both.
///
/// So the challenge path below must never write state that `get_authorization`
/// reads: caching a repo-scoped token minted by the Docker realm would make
/// the next REST call present the wrong credential and 401. That is the same
/// failure as the Docker Hub scope cascade — see `BearerCredentials` — reached
/// by a different route. Only *which presentation worked* is remembered, which
/// is neither a secret nor scoped to an endpoint.
pub struct AccessTokenCredentials {
    /// Trust anchor for realm checks. Only the host and port are compared, so
    /// this stays valid for path-scoped derivations of the same client.
    registry: Url,
    token: String,
    /// Optional, and only ever used to fill in the username of a Basic
    /// fallback against the token realm.
    username: Option<String>,
    realm_mode: Mutex<Option<RealmMode>>,
}

impl AccessTokenCredentials {
    pub fn new(registry: &Url, token: String, username: Option<String>) -> Self {
        Self {
            registry: registry.clone(),
            token,
            username,
            realm_mode: Mutex::new(None),
        }
    }
}

#[async_trait]
impl Credentials for AccessTokenCredentials {
    async fn get_authorization(&self, _http: &Client) -> Option<String> {
        Some(bearer_header(&self.token))
    }

    async fn get_authorization_for_challenge(
        &self,
        http: &Client,
        www_auth: &str,
    ) -> Option<String> {
        // Reached only when the registry rejected the raw access token and
        // asked for a Docker v2 scoped token instead.
        let challenge = parse_bearer_challenge(www_auth)?;
        let token_url = Url::parse(&challenge.realm).ok()?;

        // Guard: an access token is a platform-wide credential, so require the
        // realm to be the registry's own origin — not merely a related domain.
        if !is_same_origin_realm(&token_url, &self.registry) {
            return None;
        }

        let subject = jwt_subject_username(&self.token);
        let sticky = *self.realm_mode.lock().await;

        for mode in realm_attempt_order(sticky, self.username.is_some(), subject.is_some()) {
            let auth = match mode {
                RealmMode::Passthrough => RealmAuth::Bearer(&self.token),
                RealmMode::BasicConfiguredUser => match self.username.as_deref() {
                    Some(username) => RealmAuth::Basic {
                        username,
                        password: &self.token,
                    },
                    None => continue,
                },
                RealmMode::BasicTokenSubject => match subject.as_deref() {
                    Some(username) => RealmAuth::Basic {
                        username,
                        password: &self.token,
                    },
                    None => continue,
                },
                RealmMode::BasicTokenAsUser => RealmAuth::Basic {
                    username: &self.token,
                    password: &self.token,
                },
            };

            if let Some((minted, _ttl)) = exchange_with_scope_fallback(
                http,
                &token_url,
                challenge.service.as_deref(),
                challenge.scope.as_deref(),
                auth,
            )
            .await
            {
                *self.realm_mode.lock().await = Some(mode);
                // Deliberately not cached — see the note on the struct.
                return Some(bearer_header(&minted));
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// GHCR auth
// ---------------------------------------------------------------------------

/// GitHub Container Registry credentials.
///
/// **`get_authorization` returns `None` on purpose, and that is the whole
/// point of this type.** GHCR does not accept a raw personal access token on
/// its `/v2/` API: it answers `403 Forbidden`, *not* a `401` with a
/// `WWW-Authenticate` challenge. `RegistryClient::send` only re-challenges on
/// 401, so a 403 is terminal — presenting the PAT eagerly (as
/// [`AccessTokenCredentials`] does, correctly, for Artifactory) turns every
/// request into an unrecoverable failure, even for a package that is readable
/// anonymously.
///
/// Sending nothing instead draws the 401 GHCR issues for an unauthenticated
/// request, which carries that endpoint's *real* scope
/// (`repository:homebrew/core/git:pull`, not the canned
/// `repository:user/image:pull` the bare `/v2/` probe returns). The exchange
/// below then trades the PAT for a token valid for exactly that scope.
///
/// `token` is optional because the same exchange works with no credentials at
/// all: that is how an unauthenticated `docker pull` of a public image works,
/// and it is the only way to browse public GHCR packages without a PAT.
///
/// Like [`BearerCredentials`], the minted token is deliberately **not** cached:
/// it is scoped to one repository, so reusing it elsewhere would reproduce the
/// Docker Hub scope cascade.
pub struct GhcrCredentials {
    /// Trust anchor for the realm check.
    registry: Url,
    token: Option<String>,
}

impl GhcrCredentials {
    pub fn new(registry: &Url, token: Option<String>) -> Self {
        Self {
            registry: registry.clone(),
            token,
        }
    }
}

#[async_trait]
impl Credentials for GhcrCredentials {
    async fn get_authorization(&self, _http: &Client) -> Option<String> {
        None
    }

    async fn get_authorization_for_challenge(
        &self,
        http: &Client,
        www_auth: &str,
    ) -> Option<String> {
        let challenge = parse_bearer_challenge(www_auth)?;
        let token_url = Url::parse(&challenge.realm).ok()?;

        // A GitHub PAT is an account-wide credential, so require the realm to
        // be the registry's own origin — the same rule access tokens use, and
        // for the same reason.
        if !is_same_origin_realm(&token_url, &self.registry) {
            return None;
        }

        let auth = match self.token.as_deref() {
            Some(token) => RealmAuth::Bearer(token),
            None => RealmAuth::Anonymous,
        };

        let (minted, _ttl) = exchange_with_scope_fallback(
            http,
            &token_url,
            challenge.service.as_deref(),
            challenge.scope.as_deref(),
            auth,
        )
        .await?;

        Some(bearer_header(&minted))
    }
}

// ---------------------------------------------------------------------------
// AWS ECR
// ---------------------------------------------------------------------------

/// AWS Elastic Container Registry credentials.
///
/// **This is the exact opposite of [`GhcrCredentials`], and the contrast is
/// deliberate.** GHCR must send nothing up front because a raw credential
/// earns a terminal 403; ECR accepts HTTP Basic on `/v2/` directly, so the
/// credential is sent on every request and no challenge round trip is needed.
/// Do not "unify" the two — each is shaped by how its registry answers an
/// unsolicited credential.
///
/// What is unusual here is not *how* the credential is sent but where it comes
/// from: `ecr:GetAuthorizationToken` mints it from the AWS credential chain,
/// and it expires in about twelve hours. So unlike [`BasicCredentials`], which
/// is a precomputed constant, this type owns a refresh: the cached token is
/// re-minted once it is inside [`EXPIRY_SKEW`] of expiring. That cache is
/// global rather than per-scope — legitimately, because an ECR authorization
/// token is *registry*-wide, not repository-scoped, so there is no Docker Hub
/// style scope cascade to reproduce.
///
/// The AWS SDK does its own credential resolution, retry and caching beneath
/// this, which is why a failure here is reported rather than retried.
pub struct EcrCredentials {
    target: crate::registry::ecr::EcrTarget,
    public: bool,
    cached: Mutex<Option<crate::registry::ecr::EcrAuthorization>>,
}

impl EcrCredentials {
    /// `authorization` is the token already minted while resolving the
    /// registry's endpoint — the connect path has one in hand, and reusing it
    /// saves a redundant `GetAuthorizationToken` on the first request.
    pub fn new(
        target: crate::registry::ecr::EcrTarget,
        public: bool,
        authorization: Option<crate::registry::ecr::EcrAuthorization>,
    ) -> Self {
        Self {
            target,
            public,
            cached: Mutex::new(authorization),
        }
    }
}

#[async_trait]
impl Credentials for EcrCredentials {
    async fn get_authorization(&self, _http: &Client) -> Option<String> {
        let mut cached = self.cached.lock().await;

        if let Some(auth) = cached.as_ref()
            && auth.is_valid_at(std::time::SystemTime::now())
        {
            return Some(auth.header_value());
        }

        let minted = if self.public {
            crate::registry::ecr::authorize_public(&self.target).await
        } else {
            crate::registry::ecr::authorize(&self.target).await
        };

        // A failed refresh degrades to an anonymous request rather than an
        // error: for ECR Public that is a working anonymous pull, and for a
        // private registry it produces the 401 the TUI already knows how to
        // report.
        let minted = minted.ok()?;
        let header = minted.header_value();
        *cached = Some(minted);
        Some(header)
    }

    /// Anonymous token exchange, for ECR Public only.
    ///
    /// Private ECR never needs this — it accepts Basic on `/v2/`, so a 401
    /// there means the AWS credential itself was refused and no realm exchange
    /// can rescue it. ECR Public is different: pulling a public image needs no
    /// AWS account at all, and that path runs entirely through the registry's
    /// own token service. Without this, a user with no AWS credentials could
    /// not open `public.ecr.aws/<namespace>/<image>` by name — the very thing
    /// the empty repository list is supposed to leave possible.
    ///
    /// Anonymous rather than presenting the minted token: the token, when there
    /// is one, already went out as Basic above. Same-origin guarded like
    /// [`GhcrCredentials`], and likewise uncached, since the minted token is
    /// scoped to one repository.
    async fn get_authorization_for_challenge(
        &self,
        http: &Client,
        www_auth: &str,
    ) -> Option<String> {
        if !self.public {
            return None;
        }

        let challenge = parse_bearer_challenge(www_auth)?;
        let token_url = Url::parse(&challenge.realm).ok()?;
        let registry = Url::parse(crate::registry::ecr::ECR_PUBLIC_REGISTRY_URL).ok()?;

        if !is_same_origin_realm(&token_url, &registry) {
            return None;
        }

        let (minted, _ttl) = exchange_with_scope_fallback(
            http,
            &token_url,
            challenge.service.as_deref(),
            challenge.scope.as_deref(),
            RealmAuth::Anonymous,
        )
        .await?;

        Some(bearer_header(&minted))
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
        self.get_password_keyring(username)
            .or_else(|| self.get_password_secret_tool(username))
    }

    /// A keyring miss is a routine, expected outcome — it falls through to
    /// `get_password_secret_tool` and then, if that also misses, to an
    /// anonymous request or a credential prompt. Logging the error here has
    /// no channel that doesn't corrupt the display: this runs while the TUI
    /// holds the alternate screen in raw mode, so writing to stderr paints
    /// garbage over the rendered frame instead of appearing in a terminal
    /// scrollback anyone would see.
    fn get_password_keyring(&self, username: &str) -> Option<String> {
        keyring::Entry::new(&self.service, username)
            .and_then(|e| e.get_password())
            .ok()
    }

    fn get_password_secret_tool(&self, username: &str) -> Option<String> {
        let output = Command::new("secret-tool")
            .args(["lookup", "service", &self.service, "username", username])
            .output()
            .ok()?;
        if output.status.success() {
            let s = String::from_utf8(output.stdout).ok()?;
            let s = s.trim().to_owned();
            if s.is_empty() { None } else { Some(s) }
        } else {
            None
        }
    }

    /// Store `password` for `username` in the OS keychain.
    pub fn set_password(&self, username: &str, password: &str) -> anyhow::Result<()> {
        self.set_password_keyring(username, password)
            .or_else(|_| self.set_password_secret_tool(username, password))
    }

    fn set_password_keyring(&self, username: &str, password: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, username)?.set_password(password)?;
        Ok(())
    }

    fn set_password_secret_tool(&self, username: &str, password: &str) -> anyhow::Result<()> {
        let label = format!("keyring:{}@{}", username, self.service);
        let mut child = Command::new("secret-tool")
            .args([
                "store",
                "--label",
                &label,
                "service",
                &self.service,
                "username",
                username,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(password.as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("secret-tool store failed with status: {status}")
        }
    }

    /// Remove the stored credential for `username`.
    pub fn delete_password(&self, username: &str) -> anyhow::Result<()> {
        self.delete_password_keyring(username)
            .or_else(|_| self.delete_password_secret_tool(username))
    }

    fn delete_password_keyring(&self, username: &str) -> anyhow::Result<()> {
        keyring::Entry::new(&self.service, username)?.delete_credential()?;
        Ok(())
    }

    fn delete_password_secret_tool(&self, username: &str) -> anyhow::Result<()> {
        let output = Command::new("secret-tool")
            .args(["clear", "service", &self.service, "username", username])
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("secret-tool clear failed: {stderr}")
        }
    }
}

// ---------------------------------------------------------------------------
// Secret prompt / resolution
// ---------------------------------------------------------------------------

/// Prompt for a secret on the terminal with input masking.
pub fn prompt_secret(label: &str) -> anyhow::Result<String> {
    rpassword::prompt_password(format!("{label}: ")).map_err(Into::into)
}

/// Prompt for a password on the terminal with input masking.
pub fn prompt_password(username: &str) -> anyhow::Result<String> {
    prompt_secret(&format!("Password for {username}"))
}

/// Resolve a secret using the following priority:
/// 1. Already provided (e.g. from a CLI flag).
/// 2. OS keychain lookup via `KeyringStore`, under `account`.
/// 3. Interactive terminal prompt (masked).
///
/// If `store_on_prompt` is true and the secret came from the prompt, it is
/// saved to the keychain for future sessions.
///
/// `account` is the keyring account name: a username for a password, or
/// [`TOKEN_ACCOUNT`] for an access token.
pub fn resolve_secret(
    account: &str,
    prompt_label: &str,
    provided: Option<&str>,
    keyring: &KeyringStore,
    store_on_prompt: bool,
) -> anyhow::Result<String> {
    if let Some(secret) = provided {
        return Ok(secret.to_owned());
    }

    if let Some(secret) = keyring.get_password(account) {
        return Ok(secret);
    }

    let secret = prompt_secret(prompt_label)?;
    if store_on_prompt && !secret.is_empty() {
        let _ = keyring.set_password(account, &secret);
    }
    Ok(secret)
}

// ---------------------------------------------------------------------------
// Access token resolution
// ---------------------------------------------------------------------------

/// First candidate that is non-empty once trimmed.
fn first_non_empty<'a>(candidates: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// An access token from the environment, if any of `vars` is set.
///
/// `vars` is supplied by the caller rather than fixed here, because which
/// variables are legitimate depends on the registry type — see
/// `RegistryProfile::token_env_vars`, which owns that decision so it stays
/// pure and testable.
fn env_access_token(vars: &[&str]) -> Option<String> {
    let values: Vec<Option<String>> = vars.iter().map(|key| std::env::var(key).ok()).collect();
    first_non_empty(values.iter().map(Option::as_deref))
}

/// Resolve an access token: environment first, then the OS keychain.
///
/// The environment wins so a token can be overridden for one invocation
/// without disturbing what is stored. A token that came from the environment
/// is deliberately *not* written to the keychain — an ambient override should
/// not silently become persistent state.
///
/// Returns `None` when neither source has one; the caller decides whether to
/// prompt (interactively) or fall back to anonymous access.
pub fn resolve_access_token(keyring: &KeyringStore, env_vars: &[&str]) -> Option<String> {
    if let Some(token) = env_access_token(env_vars) {
        return Some(token);
    }
    let stored = keyring.get_password(TOKEN_ACCOUNT)?;
    first_non_empty([Some(stored.as_str())])
}

/// Clean up a token as pasted by a human.
///
/// Pasted tokens routinely arrive wrapped in quotes from a shell snippet, with
/// a leading `Bearer ` from a copied header, or with trailing whitespace. Any
/// of those produces a 401 that is indistinguishable from a wrong token, so
/// strip them rather than making the user debug it.
pub fn sanitize_pasted_token(raw: &str) -> String {
    let mut s = raw.trim();

    for quote in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(quote) && s.ends_with(quote) {
            s = &s[1..s.len() - 1];
            break;
        }
    }

    let s = s.trim();
    let s = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
        .unwrap_or(s);

    s.trim().to_owned()
}

/// The username embedded in a JFrog access token's `sub` claim.
///
/// JFrog access tokens are JWTs whose subject looks like
/// `jfrt@<instance-id>/users/<name>`; the trailing segment is the username.
/// Used only to fill in the username field of a Basic fallback against a token
/// realm — never as a trust decision, so the signature is deliberately not
/// verified and the payload is not otherwise inspected.
///
/// Returns `None` for JFrog *reference* tokens (short opaque strings, not
/// JWTs) and for anything malformed.
fn jwt_subject_username(token: &str) -> Option<String> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None; // more than three segments: not a JWT
    }

    let bytes = B64_URL.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let subject = claims["sub"].as_str()?;

    let name = subject.rsplit('/').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Realm trust validation
// ---------------------------------------------------------------------------

/// `https`, or plain `http` only for a loopback host so local dev registries
/// keep working.
fn realm_scheme_ok(realm: &Url) -> bool {
    match realm.scheme() {
        "https" => true,
        "http" => LOOPBACK_HOSTS.contains(&realm.host_str().unwrap_or("")),
        _ => false,
    }
}

/// Returns `true` if `realm` is the *same origin* as `registry`.
///
/// Stricter than [`is_trusted_realm`], and used for access-token auth only.
/// That function's same-registered-domain heuristic exists because Docker Hub
/// splits `auth.docker.io` from `registry-1.docker.io`; it means a registry at
/// `artifactory.corp.example.com` would also trust every other host under
/// `example.com` — a marketing site, a shared-hosting subdomain, a dangling
/// CNAME open to takeover.
///
/// What would leak there is not one repository's pull token but a JFrog
/// platform access token, valid across the whole REST API. Artifactory's token
/// realm is always same-origin, so the relaxation buys nothing here. Fail
/// closed: an unusual proxy that moves the realm to another host or port gets a
/// visible auth error instead of silently disclosing the token.
fn is_same_origin_realm(realm: &Url, registry: &Url) -> bool {
    realm_scheme_ok(realm) && crate::registry::client::same_host_and_port(realm, registry)
}

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
    if !realm_scheme_ok(realm) {
        return false;
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

    // -----------------------------------------------------------------------
    // Realm exchange helpers
    // -----------------------------------------------------------------------

    #[test]
    fn realm_url_appends_service_and_scope() {
        let url = realm_url(
            &u("https://auth.example.com/token"),
            Some("registry.example.com"),
            Some("repository:nginx:pull"),
        );
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("service".to_owned(), "registry.example.com".to_owned()),
                ("scope".to_owned(), "repository:nginx:pull".to_owned()),
            ]
        );
    }

    #[test]
    fn realm_url_preserves_existing_realm_query() {
        // A realm that already carries query parameters must keep them —
        // `query_pairs_mut` appends rather than replacing.
        let url = realm_url(
            &u("https://auth.example.com/token?tenant=acme"),
            Some("svc"),
            None,
        );
        let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        assert_eq!(keys, vec!["tenant".to_owned(), "service".to_owned()]);
    }

    #[test]
    fn extract_token_prefers_token_over_access_token() {
        let body = serde_json::json!({ "token": "aaa", "access_token": "bbb" });
        assert_eq!(extract_token(&body).as_deref(), Some("aaa"));

        let only_access = serde_json::json!({ "access_token": "bbb" });
        assert_eq!(extract_token(&only_access).as_deref(), Some("bbb"));

        assert!(extract_token(&serde_json::json!({})).is_none());
    }

    #[test]
    fn token_ttl_subtracts_ten_second_skew() {
        let body = serde_json::json!({ "expires_in": 600 });
        assert_eq!(token_ttl(&body), Duration::from_secs(590));

        // Must not underflow for an implausibly short lifetime.
        let tiny = serde_json::json!({ "expires_in": 3 });
        assert_eq!(token_ttl(&tiny), Duration::from_secs(0));
    }

    #[test]
    fn token_ttl_defaults_when_expires_in_absent() {
        assert_eq!(token_ttl(&serde_json::json!({})), Duration::from_secs(290));
    }

    // -----------------------------------------------------------------------
    // Same-origin realm guard
    // -----------------------------------------------------------------------

    #[test]
    fn same_origin_realm_requires_exact_host() {
        assert!(is_same_origin_realm(
            &u("https://artifactory.example.com/artifactory/api/docker/foo/v2/token"),
            &u("https://artifactory.example.com/artifactory/")
        ));
    }

    #[test]
    fn same_origin_realm_rejects_sibling_subdomain() {
        // `is_trusted_realm` would allow this (shared registered domain), which
        // is exactly the leak this guard exists to prevent: the credential at
        // stake is a platform-wide JFrog access token.
        let realm = u("https://marketing.example.com/token");
        let registry = u("https://artifactory.example.com/artifactory/");
        assert!(is_trusted_realm(&realm, &registry));
        assert!(!is_same_origin_realm(&realm, &registry));
    }

    #[test]
    fn same_origin_realm_rejects_port_mismatch() {
        assert!(!is_same_origin_realm(
            &u("https://artifactory.example.com:8443/token"),
            &u("https://artifactory.example.com/artifactory/")
        ));
    }

    #[test]
    fn same_origin_realm_allows_loopback_http() {
        assert!(is_same_origin_realm(
            &u("http://localhost:5000/v2/token"),
            &u("http://localhost:5000/")
        ));
        assert!(!is_same_origin_realm(
            &u("http://artifactory.example.com/token"),
            &u("http://artifactory.example.com/")
        ));
    }

    // -----------------------------------------------------------------------
    // Token helpers
    // -----------------------------------------------------------------------

    #[test]
    fn first_non_empty_skips_blank_and_trims() {
        assert_eq!(
            first_non_empty([None, Some(""), Some("   "), Some("  tok  "), Some("later")])
                .as_deref(),
            Some("tok")
        );
        assert!(first_non_empty([None, Some(""), Some("\t\n")]).is_none());
    }

    #[test]
    fn sanitize_pasted_token_strips_whitespace_and_bearer_prefix() {
        assert_eq!(sanitize_pasted_token("  abc123\n"), "abc123");
        assert_eq!(sanitize_pasted_token("Bearer abc123"), "abc123");
        assert_eq!(sanitize_pasted_token("bearer abc123"), "abc123");
        assert_eq!(sanitize_pasted_token("\"abc123\""), "abc123");
        assert_eq!(sanitize_pasted_token("'abc123'"), "abc123");
        assert_eq!(sanitize_pasted_token("\"Bearer abc123\""), "abc123");
        // A token that merely contains the word must survive intact.
        assert_eq!(sanitize_pasted_token("Bearerish"), "Bearerish");
    }

    /// Build an unsigned JWT with the given payload — enough for
    /// `jwt_subject_username`, which never verifies the signature.
    fn fake_jwt(payload: serde_json::Value) -> String {
        let header = B64_URL.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = B64_URL.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.not-a-real-signature")
    }

    #[test]
    fn jwt_subject_extracts_username_from_jfrog_sub() {
        let token = fake_jwt(serde_json::json!({
            "sub": "jfrt@01h2x3/users/ci-bot",
        }));
        assert_eq!(jwt_subject_username(&token).as_deref(), Some("ci-bot"));
    }

    #[test]
    fn jwt_subject_none_for_opaque_reference_token() {
        // JFrog reference tokens are short opaque strings, not JWTs.
        assert!(jwt_subject_username("cmVmdGtuOjAxOjE3MjgwMDA").is_none());
    }

    #[test]
    fn jwt_subject_none_for_malformed_token() {
        assert!(jwt_subject_username("").is_none());
        assert!(jwt_subject_username("a.b").is_none());
        assert!(jwt_subject_username("a.b.c.d").is_none());
        assert!(jwt_subject_username("a.!!!not-base64!!!.c").is_none());
        // Valid base64 that is not JSON.
        let not_json = B64_URL.encode(b"plain text");
        assert!(jwt_subject_username(&format!("a.{not_json}.c")).is_none());
        // JSON without a `sub` claim.
        assert!(jwt_subject_username(&fake_jwt(serde_json::json!({ "iss": "x" }))).is_none());
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("super-secret-token");
        let rendered = format!("{s:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "Debug must not disclose the secret, got: {rendered}"
        );
        assert_eq!(rendered, "Secret(***)");
        // The value is still reachable deliberately.
        assert_eq!(s.expose(), "super-secret-token");
    }

    // -----------------------------------------------------------------------
    // Access token credentials
    // -----------------------------------------------------------------------

    #[test]
    fn bearer_header_formats_authorization_value() {
        assert_eq!(bearer_header("abc123"), "Bearer abc123");
    }

    #[test]
    fn access_token_credentials_always_send_the_raw_token() {
        // The REST client and the Docker-repo-scoped client share this
        // credential, so the fast path must be stateless and unconditional.
        let creds = AccessTokenCredentials::new(
            &u("https://artifactory.example.com/artifactory/"),
            "tok-abc".to_owned(),
            None,
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = Client::new();
        let first = rt.block_on(creds.get_authorization(&http));
        let second = rt.block_on(creds.get_authorization(&http));
        assert_eq!(first.as_deref(), Some("Bearer tok-abc"));
        assert_eq!(first, second, "must not drift between calls");
    }

    /// The opposite of `AccessTokenCredentials`, and deliberately so.
    ///
    /// GHCR answers a raw PAT on `/v2/` with **403**, not a 401 challenge, and
    /// `RegistryClient::send` only re-challenges on 401 — so eagerly sending
    /// the token makes every request fail terminally, including for packages
    /// that are readable anonymously. Sending nothing draws the 401 that
    /// carries the endpoint's real scope, which is what the challenge exchange
    /// needs. If this ever starts returning a header, GHCR browsing breaks.
    #[test]
    fn ghcr_credentials_never_send_a_global_authorization_header() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = Client::new();

        let with_token =
            GhcrCredentials::new(&u("https://ghcr.io/"), Some("ghp_example".to_owned()));
        assert_eq!(
            rt.block_on(with_token.get_authorization(&http)),
            None,
            "a PAT must be exchanged at the token realm, never sent to /v2/"
        );

        let anonymous = GhcrCredentials::new(&u("https://ghcr.io/"), None);
        assert_eq!(rt.block_on(anonymous.get_authorization(&http)), None);
    }

    /// The counterpart to the GHCR test above, and the reason the two types are
    /// not merged: ECR accepts Basic on `/v2/` directly, so a token already in
    /// hand is sent eagerly rather than withheld for a challenge.
    #[test]
    fn ecr_credentials_send_the_minted_token_as_basic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = Client::new();

        let creds = EcrCredentials::new(
            crate::registry::ecr::EcrTarget::new(Some("pgmac".to_owned()), None),
            false,
            Some(crate::registry::ecr::EcrAuthorization {
                registry_url: "https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned(),
                authorization_token: "QVdTOnB3".to_owned(),
                // Far enough out that the skew window cannot expire it.
                expires_at: Some(std::time::SystemTime::now() + Duration::from_secs(3600)),
            }),
        );

        assert_eq!(
            rt.block_on(creds.get_authorization(&http)),
            Some("Basic QVdTOnB3".to_owned()),
            "an unexpired ECR token must be presented without a challenge"
        );
    }

    /// A GitHub PAT is an account-wide credential, so it must not be offered
    /// to a realm on another host even if the registry points there.
    #[test]
    fn ghcr_credentials_reject_an_off_origin_realm() {
        let creds = GhcrCredentials::new(&u("https://ghcr.io/"), Some("ghp_example".to_owned()));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let http = Client::new();

        let challenge = r#"Bearer realm="https://evil.example.com/token",service="ghcr.io""#;
        assert_eq!(
            rt.block_on(creds.get_authorization_for_challenge(&http, challenge)),
            None
        );
    }

    #[test]
    fn realm_attempt_order_prefers_bearer_passthrough() {
        // No username, not a JWT: pass-through, then the identity-token
        // convention as a last resort.
        assert_eq!(
            realm_attempt_order(None, false, false),
            vec![RealmMode::Passthrough, RealmMode::BasicTokenAsUser]
        );

        assert_eq!(
            realm_attempt_order(None, true, true),
            vec![
                RealmMode::Passthrough,
                RealmMode::BasicConfiguredUser,
                RealmMode::BasicTokenSubject,
                RealmMode::BasicTokenAsUser,
            ]
        );
    }

    #[test]
    fn realm_attempt_order_sticks_to_a_known_good_mode() {
        // Avoids re-sending failing Basic attempts with a real username, which
        // can trip Artifactory's failed-login lockout.
        assert_eq!(
            realm_attempt_order(Some(RealmMode::BasicConfiguredUser), true, true),
            vec![RealmMode::BasicConfiguredUser]
        );
    }
}
