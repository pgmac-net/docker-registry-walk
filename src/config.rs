use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

const EXAMPLE_CONFIG: &str = r#"# docker-registry-walk configuration
# See: https://github.com/pgmac-net/docker-registry-walk
#
# Secrets are stored in the OS keyring, never in this file and never in argv.
# Both flags below take no value — they prompt interactively, masked:
#   docker-registry-walk --registry <name> --password
#   docker-registry-walk --registry <name> --token

# Name of the registry to open on startup (optional).
# default_registry = "local"

[[registry]]
name = "local"
url = "http://localhost:5000"
# username = "admin"
# type = "standard"

# Artifactory instances host many Docker repo-keys under one server; `url`
# is the Artifactory base (not a /v2/ root) and the TUI lets you pick which
# repo-key to browse after switching to this registry.
# [[registry]]
# name = "artifactory"
# url = "https://artifactory.example.com/artifactory"
# username = "ci"
# type = "artifactory"

# Artifactory with a JFrog access token instead of a username/password.
# `auth = "token"` sends `Authorization: Bearer <token>` — the same way the
# Terraform jfrog/artifactory provider authenticates — so no username is
# needed. The token is read from $JFROG_ACCESS_TOKEN, then
# $ARTIFACTORY_ACCESS_TOKEN, then the OS keyring, then a masked prompt.
# [[registry]]
# name = "artifactory-token"
# url = "https://artifactory.example.com/artifactory"
# type = "artifactory"
# auth = "token"

# GitHub Container Registry. GHCR has no /v2/_catalog, so the repository list
# comes from the GitHub packages API instead; that needs a personal access
# token with the `read:packages` scope. The token is read from $CR_PAT, then
# $GITHUB_TOKEN, then $GH_TOKEN, then the OS keyring, then a masked prompt.
#
# `owner` is optional: leave it out to browse the token holder's own packages,
# or set it to any user or organisation to browse theirs.
# [[registry]]
# name = "ghcr"
# url = "https://ghcr.io"
# type = "ghcr"
# owner = "pgmac-net"

# AWS Elastic Container Registry. An ECR registry is one AWS account in one
# region, and its hostname (<account-id>.dkr.ecr.<region>.amazonaws.com) is
# derived at runtime — so `url` is omitted here rather than hand-typed.
#
# Credentials come from the ordinary AWS chain (SSO, assume-role, static keys),
# never the keyring: the registry password is a 12-hour token minted by
# ecr:GetAuthorizationToken. `aws_profile` and `region` are both optional and
# fall back to $AWS_PROFILE / $AWS_REGION; both can be switched from inside the
# TUI with Backspace, without editing this file.
#
# Needs ecr:GetAuthorizationToken and ecr:DescribeRepositories.
# [[registry]]
# name = "ecr"
# type = "ecr"
# aws_profile = "default"
# region = "ap-southeast-2"

# ECR Public (public.ecr.aws). One global registry rather than one per region,
# so `region` does not apply. The repository list is the set *you* publish,
# which needs AWS credentials; pulling anyone else's public image works
# anonymously, so a known repository can always be opened by name.
# [[registry]]
# name = "ecr-public"
# type = "ecr-public"
# aws_profile = "default"
"#;

/// Environment variables consulted for a JFrog access token, in order.
///
/// Same names the Terraform `jfrog/artifactory` provider reads.
const ARTIFACTORY_TOKEN_ENV_VARS: [&str; 2] = ["JFROG_ACCESS_TOKEN", "ARTIFACTORY_ACCESS_TOKEN"];

/// Environment variables consulted for a GitHub personal access token, in
/// order.
///
/// `CR_PAT` is GitHub's own documented name for a GHCR token, so it wins over
/// the more general `GITHUB_TOKEN` / `GH_TOKEN` that surrounding tooling
/// (`gh`, Actions) exports for unrelated reasons.
const GHCR_TOKEN_ENV_VARS: [&str; 3] = ["CR_PAT", "GITHUB_TOKEN", "GH_TOKEN"];

/// What kind of registry this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum RegistryType {
    /// A standard Docker Registry v2 endpoint.
    #[default]
    Standard,
    /// Docker Hub (hub.docker.com).  The catalog endpoint is not supported,
    /// so the TUI falls back to the hub search API to find repos.
    // `dockerhub`, not clap's default kebab-case `docker-hub`, so the CLI and
    // the config file share one vocabulary.
    #[value(name = "dockerhub")]
    DockerHub,
    /// A JFrog Artifactory instance hosting one or more Docker repositories.
    ///
    /// Unlike `Standard`, `url` here is the **Artifactory server base**
    /// (e.g. `https://artifactory.example.com/artifactory`), not a `/v2/`
    /// root — a single instance can host many independently-browsable
    /// Docker repo-keys. The TUI lists them via Artifactory's
    /// `/api/repositories` REST endpoint and lets the user pick one before
    /// browsing it as a normal registry.
    Artifactory,
    /// GitHub Container Registry (`ghcr.io`).
    ///
    /// Like Docker Hub, GHCR implements no `/v2/_catalog`, so the repository
    /// list has to come from somewhere else — here the GitHub packages API
    /// (`/user/packages`, `/users/<owner>/packages`), which is a *different
    /// host* from the registry and needs a PAT with `read:packages`. Browsing
    /// itself is ordinary Docker v2: GHCR answers each request with a 401
    /// carrying that endpoint's real scope, which the existing challenge
    /// re-exchange in `RegistryClient::send` already handles.
    Ghcr,
    /// AWS Elastic Container Registry (`<account>.dkr.ecr.<region>.amazonaws.com`).
    ///
    /// Two things make ECR unlike every type above. First, there is no
    /// `/v2/_catalog`: repositories are listed with `ecr:DescribeRepositories`,
    /// an AWS API on a different host from the registry. Second, there is no
    /// static credential — the registry password is a ~12-hour token minted by
    /// `ecr:GetAuthorizationToken` from whatever the AWS credential chain
    /// resolves, so nothing is ever stored in the keyring.
    ///
    /// A consequence of the first point: the registry hostname embeds the
    /// account ID, which the user should not have to look up. `url` is
    /// therefore optional for this type and derived from the `proxyEndpoint`
    /// that `GetAuthorizationToken` returns.
    Ecr,
    /// ECR Public (`public.ecr.aws`).
    ///
    /// A separate AWS service (`ecr-public`) from [`RegistryType::Ecr`], not a
    /// mode of it: one global registry rather than one per region, a fixed
    /// hostname with no account ID in it, and an API that only answers in
    /// `us-east-1`. Kept as its own variant so neither type's rules have to be
    /// read through an `if public` branch.
    // `ecr-public`, not the `rename_all = "lowercase"` default `ecrpublic`.
    #[serde(rename = "ecr-public")]
    #[value(name = "ecr-public")]
    EcrPublic,
}

/// How to authenticate to a registry, as configured.
///
/// This is the user's *intent*; the credential actually built also depends on
/// which secrets are available at runtime — see [`RegistryProfile::auth_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Infer from the registry type and which secrets are present. Preserves
    /// the behaviour that predates the `auth` field.
    #[default]
    Auto,
    /// Force HTTP Basic with `username` + keyring password.
    Basic,
    /// Force Docker v2 bearer-token exchange with `username` + keyring password.
    Bearer,
    /// Force `Authorization: Bearer <access token>`. Needs no username.
    Token,
}

impl AuthMode {
    /// Used by `skip_serializing_if` so an unset `auth` stays absent from
    /// written config rather than appearing as a redundant `auth = "auto"`.
    fn is_auto(&self) -> bool {
        *self == Self::Auto
    }

    /// Whether this mode authenticates with an access token, and so should
    /// have one resolved for it.
    ///
    /// Deliberately false for [`AuthMode::Auto`]: whether `Auto` wants a token
    /// depends on the registry type, which this type does not know. See
    /// [`RegistryProfile::wants_access_token`].
    fn is_token(&self) -> bool {
        *self == Self::Token
    }
}

/// The credential implementation to build for a profile, once it is known
/// which secrets are actually available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// Anonymous — no `Authorization` header.
    None,
    /// `registry::BasicCredentials`.
    Basic,
    /// `registry::BearerCredentials`.
    Bearer,
    /// `registry::AccessTokenCredentials`.
    AccessToken,
    /// `registry::GhcrCredentials` — sends no global header and exchanges the
    /// PAT (or nothing, anonymously) for a scope-bound token on each 401.
    Ghcr,
    /// `registry::EcrCredentials` — mints and refreshes an AWS authorization
    /// token, sent as HTTP Basic.
    Ecr,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryProfile {
    pub name: String,
    /// Registry base URL.
    ///
    /// `None` only for the ECR types, whose hostname embeds an AWS account ID
    /// and is resolved at runtime from `GetAuthorizationToken`'s
    /// `proxyEndpoint`. [`Config::validate`] rejects a missing URL for every
    /// other type, so the option is not a general "maybe configured" — it is
    /// specifically "derived, not written".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Registry flavour.  Controls how the UI interacts with it
    /// (e.g. Docker Hub uses the hub search API instead of /v2/_catalog).
    #[serde(rename = "type", default)]
    pub registry_type: RegistryType,
    /// How to authenticate.  Defaults to `auto`, which reproduces the
    /// behaviour from before this field existed.
    #[serde(default, skip_serializing_if = "AuthMode::is_auto")]
    pub auth: AuthMode,
    /// GHCR only: which user or organisation's packages to list.
    ///
    /// `None` means the authenticated token holder's own packages
    /// (`/user/packages`). Ignored for every other registry type, which has a
    /// server-side catalog and so needs no namespace to be named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// ECR only: which AWS named profile to resolve credentials from.
    ///
    /// `None` defers entirely to the AWS credential chain (`$AWS_PROFILE`, then
    /// `default`). Switchable at runtime from the TUI, which is why it is not
    /// simply read from the environment at startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_profile: Option<String>,
    /// ECR only: which AWS region's registry to browse.
    ///
    /// `None` defers to the chain (`$AWS_REGION`, then the named profile's
    /// configured region). Not meaningful for [`RegistryType::EcrPublic`],
    /// which is a single global registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

impl RegistryProfile {
    /// The configured URL's host, if there is a parseable URL at all.
    ///
    /// Shared by the URL-sniffing predicates below so each one states only its
    /// own hostname rule, and so a profile with no URL (ECR, before its
    /// registry is resolved) is uniformly "not detected as anything".
    fn url_host(&self) -> Option<String> {
        let url = self.url.as_deref()?;
        let parsed = url::Url::parse(url).ok()?;
        parsed.host_str().map(str::to_owned)
    }

    /// Returns `true` when the registry is Docker Hub (either explicitly
    /// configured or detected from the URL).
    pub fn is_dockerhub(&self) -> bool {
        match self.registry_type {
            RegistryType::DockerHub => true,
            RegistryType::Standard => {
                // Fall back to URL-based detection for backward compatibility.
                self.url_host().is_some_and(|host| {
                    matches!(
                        host.as_str(),
                        "registry-1.docker.io" | "docker.io" | "index.docker.io"
                    )
                })
            }
            RegistryType::Artifactory
            | RegistryType::Ghcr
            | RegistryType::Ecr
            | RegistryType::EcrPublic => false,
        }
    }

    /// Returns `true` when the registry is GitHub Container Registry (either
    /// explicitly configured or detected from the URL).
    ///
    /// URL-based detection mirrors [`Self::is_dockerhub`] rather than
    /// Artifactory's explicit-only rule: GHCR is a hosted service at one fixed
    /// hostname, so the host is conclusive. It also upgrades a profile already
    /// pointed at `ghcr.io` as `type = "standard"` — which today dead-ends at
    /// the catalog 401 — to the package picker, with no config change.
    pub fn is_ghcr(&self) -> bool {
        match self.registry_type {
            RegistryType::Ghcr => true,
            RegistryType::Standard => self.url_host().is_some_and(|h| h == "ghcr.io"),
            RegistryType::DockerHub
            | RegistryType::Artifactory
            | RegistryType::Ecr
            | RegistryType::EcrPublic => false,
        }
    }

    /// Returns `true` when the registry is a private AWS ECR registry.
    ///
    /// URL sniffing follows the same reasoning as [`Self::is_ghcr`]: the
    /// hostname shape `<account>.dkr.ecr.<region>.amazonaws.com` is owned by
    /// AWS and conclusive, so a profile already pointed at one as
    /// `type = "standard"` — which today dead-ends at the catalog 401 — gets
    /// the ECR flow with no config change. Deliberately narrower than "ends
    /// with amazonaws.com": that domain hosts every AWS service.
    pub fn is_ecr(&self) -> bool {
        match self.registry_type {
            RegistryType::Ecr => true,
            RegistryType::Standard => self
                .url_host()
                .is_some_and(|h| h.contains(".dkr.ecr.") && h.ends_with(".amazonaws.com")),
            RegistryType::DockerHub
            | RegistryType::Artifactory
            | RegistryType::Ghcr
            | RegistryType::EcrPublic => false,
        }
    }

    /// Returns `true` when the registry is ECR Public (`public.ecr.aws`).
    pub fn is_ecr_public(&self) -> bool {
        match self.registry_type {
            RegistryType::EcrPublic => true,
            RegistryType::Standard => self.url_host().is_some_and(|h| h == "public.ecr.aws"),
            RegistryType::DockerHub
            | RegistryType::Artifactory
            | RegistryType::Ghcr
            | RegistryType::Ecr => false,
        }
    }

    /// Returns `true` for either ECR flavour.
    ///
    /// The two differ in how they discover repositories and whether a region
    /// applies, but they share every rule about *credentials* — AWS chain only,
    /// no keyring, no interactive prompt — so the guards that enforce those
    /// rules ask this rather than both predicates.
    pub fn is_any_ecr(&self) -> bool {
        self.is_ecr() || self.is_ecr_public()
    }

    /// What to show wherever the registry's URL is displayed.
    ///
    /// An ECR profile has nothing to show until AWS answers, and a blank line
    /// in the header would read as a bug — so it names the account and region
    /// being resolved instead. Replaced by the real endpoint once
    /// `GetAuthorizationToken` returns it.
    pub fn display_url(&self) -> String {
        if let Some(url) = &self.url {
            return url.clone();
        }
        if !self.is_any_ecr() {
            return String::new();
        }
        let aws_profile = self.aws_profile.as_deref().unwrap_or("default");
        match (&self.region, self.is_ecr_public()) {
            (_, true) => format!("ecr-public ({aws_profile})"),
            (Some(region), false) => format!("ecr ({aws_profile} / {region})"),
            (None, false) => format!("ecr ({aws_profile})"),
        }
    }

    /// Returns `true` when the registry is a JFrog Artifactory instance
    /// hosting multiple Docker repo-keys. No URL-based auto-detection —
    /// Artifactory is self-hosted with no fixed hostname, so this must be
    /// set explicitly with `type = "artifactory"`.
    pub fn is_artifactory(&self) -> bool {
        self.registry_type == RegistryType::Artifactory
    }

    /// Whether an access token should be resolved for this profile at all.
    ///
    /// Gates the environment lookup, and is deliberately narrow: under
    /// `auth = "auto"` only Artifactory and GHCR consult the environment at
    /// all, and each consults only its own variables (see
    /// [`Self::token_env_vars`]). A stray JFrog variable exported in a shell
    /// must not change how anyone authenticates to docker.io — and, now that
    /// GHCR reads `GITHUB_TOKEN`, the same has to hold in reverse for a
    /// variable that is exported on most developer machines.
    /// ECR is excluded even under `auth = "token"`: its credential is minted by
    /// the AWS SDK, never supplied by the user, so resolving one would mean
    /// prompting for a secret that has nowhere to go.
    pub fn wants_access_token(&self) -> bool {
        if self.is_any_ecr() {
            return false;
        }
        self.auth.is_token()
            || (self.auth == AuthMode::Auto && (self.is_artifactory() || self.is_ghcr()))
    }

    /// Which environment variables may supply this profile's access token.
    ///
    /// Partitioned by registry type so the two token vocabularies can never
    /// cross-read: a `GITHUB_TOKEN` exported for `gh` must not become an
    /// Artifactory credential, and a `JFROG_ACCESS_TOKEN` must not become a
    /// GHCR one. Non-GHCR types keep the JFrog list they already used, so
    /// `auth = "token"` on any pre-existing profile resolves exactly as before.
    ///
    /// Pure, and paired with [`Self::auth_kind`]: the decision lives here, the
    /// `std::env` lookup stays in `registry::auth`.
    /// ECR reads *neither* list. Its credential comes from the AWS chain, which
    /// has its own environment vocabulary (`AWS_PROFILE`, `AWS_REGION`,
    /// `AWS_ACCESS_KEY_ID`, …) that the SDK reads for itself. An empty slice
    /// here is what keeps a `GITHUB_TOKEN` or `JFROG_ACCESS_TOKEN` exported for
    /// something else from being offered to AWS.
    pub fn token_env_vars(&self) -> &'static [&'static str] {
        if self.is_any_ecr() {
            &[]
        } else if self.is_ghcr() {
            &GHCR_TOKEN_ENV_VARS
        } else {
            &ARTIFACTORY_TOKEN_ENV_VARS
        }
    }

    /// Which credential to build, given which secrets turned out to be
    /// available.
    ///
    /// Pure, so the whole matrix is unit-testable without touching the
    /// keyring, the environment or the network. The caller is responsible for
    /// only resolving a token when [`Self::wants_access_token`] is true.
    ///
    /// Under [`AuthMode::Auto`] an access token is used only when Basic is not
    /// possible. That keeps upgrades behaviour-identical: a profile that
    /// authenticates with a username and password today keeps doing so even if
    /// a token also happens to be available. Force the other order with
    /// `auth = "token"`.
    pub fn auth_kind(&self, has_token: bool, has_password: bool) -> AuthKind {
        let can_basic = self.username.is_some() && has_password;

        match self.auth {
            // GHCR exchanges the token rather than sending it, and its
            // exchange also works with no token at all — so `token` mode stays
            // useful (as anonymous access) even when none was resolved.
            AuthMode::Token if self.is_ghcr() => AuthKind::Ghcr,
            // ECR has no user-supplied token to send, so `token` mode means
            // the same thing `auto` does: mint one from the AWS chain. It is
            // accepted rather than rejected so `--type ecr --auth token` is
            // merely redundant instead of an error.
            AuthMode::Token if self.is_any_ecr() => AuthKind::Ecr,
            AuthMode::Token => {
                if has_token {
                    AuthKind::AccessToken
                } else {
                    AuthKind::None
                }
            }
            AuthMode::Basic => {
                if can_basic {
                    AuthKind::Basic
                } else {
                    AuthKind::None
                }
            }
            AuthMode::Bearer => {
                if can_basic {
                    AuthKind::Bearer
                } else {
                    AuthKind::None
                }
            }
            // ECR: the AWS chain is the only credential source. Note this is
            // reached only under `auto` and `token` — an explicit `basic` or
            // `bearer` above still wins, which is the escape hatch for an
            // authenticating proxy sitting in front of an ECR endpoint.
            AuthMode::Auto if self.is_any_ecr() => AuthKind::Ecr,
            // Artifactory's Docker v2 endpoint and REST API both accept HTTP
            // Basic, so a username + password keeps working unchanged; a token
            // fills in when there is no password to send.
            AuthMode::Auto if self.is_artifactory() => {
                if can_basic {
                    AuthKind::Basic
                } else if has_token {
                    AuthKind::AccessToken
                } else {
                    AuthKind::None
                }
            }
            // GHCR: a PAT wins, because it is the only credential that also
            // unlocks repo *discovery* — the GitHub packages API takes the raw
            // token and has no username/password form at all.
            //
            // Note it maps to `Ghcr`, not `AccessToken`: GHCR rejects a raw PAT
            // on `/v2/` with 403 rather than a 401 challenge, so the token must
            // be exchanged for a scope-bound one instead of sent directly. See
            // `registry::GhcrCredentials`.
            //
            // With no token, a username + password still gets the ordinary v2
            // exchange (Bearer, not the Basic that Artifactory falls back to —
            // GHCR will not accept plain HTTP Basic on `/v2/`). With neither,
            // `Ghcr` again: its exchange works anonymously, which is the only
            // way to browse public packages without a PAT.
            AuthMode::Auto if self.is_ghcr() => {
                if !has_token && can_basic {
                    AuthKind::Bearer
                } else {
                    AuthKind::Ghcr
                }
            }
            // Standard / Docker Hub: bearer-token *exchange* from a username
            // and password. Not access-token auth, and not token-aware.
            AuthMode::Auto => {
                if can_basic {
                    AuthKind::Bearer
                } else {
                    AuthKind::None
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Registry to open on startup. Falls back to the first entry if absent or not found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    #[serde(default)]
    pub registry: Vec<RegistryProfile>,
}

impl Config {
    /// Load configuration from the default platform path.
    ///
    /// If the file does not exist, creates it with example content and returns
    /// an empty (default) config so the caller can still function.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::default_path();

        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, EXAMPLE_CONFIG)?;
            return Ok(Self::default());
        }

        let text = std::fs::read_to_string(&path)?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    /// Serialize the config to TOML and write it to the default path.
    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::default_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// Validate that all URLs are parseable and registry names are unique.
    pub fn validate(&self) -> anyhow::Result<()> {
        for profile in &self.registry {
            // ECR derives its hostname from AWS at runtime, so a missing URL is
            // the normal case there and an error everywhere else. An explicitly
            // written URL is still validated for every type.
            match profile.url.as_deref() {
                Some(url) => {
                    Url::parse(url).map_err(|e| {
                        anyhow::anyhow!("Registry '{}' has invalid URL '{url}': {e}", profile.name)
                    })?;
                }
                None if profile.is_any_ecr() => {}
                None => {
                    return Err(anyhow::anyhow!(
                        "Registry '{}' has no url. Only ECR registries may omit it.",
                        profile.name
                    ));
                }
            }

            // `#` separates the profile name from the repo-key in the TUI's
            // client-cache keys (`<profile>#<repo-key>`), so allowing it in a
            // name would make those keys ambiguous.
            if profile.name.contains('#') {
                return Err(anyhow::anyhow!(
                    "Registry name '{}' must not contain '#'",
                    profile.name
                ));
            }

            // Basic and Bearer both send a username; failing here beats
            // silently falling back to anonymous requests.
            if matches!(profile.auth, AuthMode::Basic | AuthMode::Bearer)
                && profile.username.is_none()
            {
                return Err(anyhow::anyhow!(
                    "Registry '{}' has auth = \"{}\" but no username. \
                     Set a username, or use auth = \"token\".",
                    profile.name,
                    if profile.auth == AuthMode::Basic {
                        "basic"
                    } else {
                        "bearer"
                    }
                ));
            }
        }

        let mut seen: HashSet<&str> = HashSet::new();
        for profile in &self.registry {
            if !seen.insert(profile.name.as_str()) {
                return Err(anyhow::anyhow!(
                    "Duplicate registry name: '{}'",
                    profile.name
                ));
            }
        }

        Ok(())
    }

    /// Platform-correct path to the config file.
    ///
    /// * Linux / macOS: `~/.config/docker-registry-walk/config.toml`
    /// * Windows:       `%APPDATA%\docker-registry-walk\config.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."))
            })
            .join("docker-registry-walk")
            .join("config.toml")
    }

    /// Index of the default registry in `self.registry`.
    ///
    /// Uses `default_registry` name if set and found; falls back to 0.
    pub fn default_idx(&self) -> usize {
        self.default_registry
            .as_ref()
            .and_then(|name| self.registry.iter().position(|r| &r.name == name))
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, url: &str) -> RegistryProfile {
        RegistryProfile {
            name: name.to_owned(),
            url: Some(url.to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        }
    }

    #[test]
    fn round_trip_empty() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&text).unwrap();
        assert_eq!(loaded.registry.len(), 0);
        assert!(loaded.default_registry.is_none());
    }

    #[test]
    fn round_trip_with_profiles() {
        let config = Config {
            default_registry: Some("prod".to_owned()),
            registry: vec![
                profile("local", "http://localhost:5000"),
                RegistryProfile {
                    name: "prod".to_owned(),
                    url: Some("https://registry.example.com".to_owned()),
                    username: Some("admin".to_owned()),
                    registry_type: RegistryType::Standard,
                    ..Default::default()
                },
            ],
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let loaded: Config = toml::from_str(&text).unwrap();
        assert_eq!(loaded.registry.len(), 2);
        assert_eq!(loaded.registry[1].name, "prod");
        assert_eq!(loaded.registry[1].username.as_deref(), Some("admin"));
        assert_eq!(loaded.default_registry.as_deref(), Some("prod"));
    }

    #[test]
    fn round_trip_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = Config {
            default_registry: Some("a".to_owned()),
            registry: vec![profile("a", "http://a.example.com")],
        };
        let text = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&path, &text).unwrap();

        let loaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.registry[0].name, "a");
        assert_eq!(loaded.default_registry.as_deref(), Some("a"));
    }

    #[test]
    fn validate_invalid_url() {
        let config = Config {
            default_registry: None,
            registry: vec![profile("bad", "not-a-url")],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_duplicate_names() {
        let config = Config {
            default_registry: None,
            registry: vec![
                profile("dup", "http://a.example.com"),
                profile("dup", "http://b.example.com"),
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_passes_clean_config() {
        let config = Config {
            default_registry: Some("local".to_owned()),
            registry: vec![
                profile("local", "http://localhost:5000"),
                profile("prod", "https://registry.example.com"),
            ],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn default_idx_by_name() {
        let config = Config {
            default_registry: Some("prod".to_owned()),
            registry: vec![
                profile("local", "http://localhost:5000"),
                profile("prod", "https://registry.example.com"),
            ],
        };
        assert_eq!(config.default_idx(), 1);
    }

    #[test]
    fn default_idx_missing_name_falls_back_to_zero() {
        let config = Config {
            default_registry: Some("nonexistent".to_owned()),
            registry: vec![profile("local", "http://localhost:5000")],
        };
        assert_eq!(config.default_idx(), 0);
    }

    #[test]
    fn artifactory_type_round_trips_and_is_detected() {
        let profile = RegistryProfile {
            name: "artifactory".to_owned(),
            url: Some("https://artifactory.example.com/artifactory".to_owned()),
            username: Some("ci".to_owned()),
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        let text = toml::to_string_pretty(&profile).unwrap();
        assert!(text.contains(r#"type = "artifactory""#));
        let loaded: RegistryProfile = toml::from_str(&text).unwrap();
        assert!(loaded.is_artifactory());
        assert!(!loaded.is_dockerhub());
    }

    #[test]
    fn standard_and_dockerhub_are_not_artifactory() {
        assert!(!profile("local", "http://localhost:5000").is_artifactory());
        let dockerhub = RegistryProfile {
            name: "hub".to_owned(),
            url: Some("https://registry-1.docker.io".to_owned()),
            username: None,
            registry_type: RegistryType::Standard,
            ..Default::default()
        };
        assert!(dockerhub.is_dockerhub());
        assert!(!dockerhub.is_artifactory());
    }

    #[test]
    fn default_idx_no_default_registry() {
        let config = Config {
            default_registry: None,
            registry: vec![
                profile("a", "http://a.example.com"),
                profile("b", "http://b.example.com"),
            ],
        };
        assert_eq!(config.default_idx(), 0);
    }

    // -----------------------------------------------------------------------
    // auth mode / auth_kind
    // -----------------------------------------------------------------------

    #[test]
    fn auth_mode_defaults_to_auto_and_is_omitted_when_serialized() {
        let p = profile("local", "http://localhost:5000");
        assert_eq!(p.auth, AuthMode::Auto);

        let text = toml::to_string_pretty(&p).unwrap();
        assert!(
            !text.contains("auth"),
            "an unset auth mode must not be written back to config: {text}"
        );
    }

    #[test]
    fn auth_mode_round_trips_lowercase() {
        for (mode, literal) in [
            (AuthMode::Basic, "basic"),
            (AuthMode::Bearer, "bearer"),
            (AuthMode::Token, "token"),
        ] {
            let p = RegistryProfile {
                name: "r".to_owned(),
                url: Some("https://r.example.com".to_owned()),
                username: Some("u".to_owned()),
                auth: mode,
                ..Default::default()
            };
            let text = toml::to_string_pretty(&p).unwrap();
            assert!(
                text.contains(&format!(r#"auth = "{literal}""#)),
                "expected auth = \"{literal}\" in: {text}"
            );

            let loaded: RegistryProfile = toml::from_str(&text).unwrap();
            assert_eq!(loaded.auth, mode);
        }
    }

    fn auth_profile(kind: RegistryType, mode: AuthMode, username: Option<&str>) -> RegistryProfile {
        RegistryProfile {
            name: "r".to_owned(),
            url: Some("https://r.example.com".to_owned()),
            username: username.map(str::to_owned),
            registry_type: kind,
            auth: mode,
            owner: None,
            aws_profile: None,
            region: None,
        }
    }

    #[test]
    fn validate_rejects_basic_without_username() {
        let config = Config {
            default_registry: None,
            registry: vec![auth_profile(RegistryType::Standard, AuthMode::Basic, None)],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_bearer_without_username() {
        let config = Config {
            default_registry: None,
            registry: vec![auth_profile(RegistryType::Standard, AuthMode::Bearer, None)],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_token_without_username() {
        // The whole point of token auth: no username needed.
        let config = Config {
            default_registry: None,
            registry: vec![auth_profile(
                RegistryType::Artifactory,
                AuthMode::Token,
                None,
            )],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unknown_auth_value_errors_and_names_the_offender() {
        // `main` reports a failed `Config::load` on stderr rather than silently
        // starting with an invented localhost profile. That is only useful if
        // the error identifies what was wrong, so pin it.
        let text = r#"
            [[registry]]
            name = "art"
            url = "https://art.example.com/artifactory"
            type = "artifactory"
            auth = "tokenn"
        "#;
        let err = toml::from_str::<Config>(text)
            .expect_err("an unknown auth value must not deserialize")
            .to_string();
        assert!(
            err.contains("tokenn"),
            "error must name the bad value: {err}"
        );
        assert!(
            err.contains("token"),
            "error must list the valid alternatives: {err}"
        );
    }

    #[test]
    fn validate_rejects_hash_in_registry_name() {
        // `#` is the profile/repo-key separator in the TUI's client-cache keys.
        let config = Config {
            default_registry: None,
            registry: vec![profile("art#docker-local", "https://a.example.com")],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn wants_access_token_only_for_token_mode_or_auto_artifactory() {
        use RegistryType::{Artifactory, DockerHub, Standard};

        // Explicit token mode: always, whatever the registry type.
        for kind in [Standard, DockerHub, Artifactory] {
            assert!(auth_profile(kind, AuthMode::Token, None).wants_access_token());
        }

        // Auto: Artifactory only. A stray JFROG_ACCESS_TOKEN in the shell must
        // not change how docker.io is authenticated.
        assert!(auth_profile(Artifactory, AuthMode::Auto, Some("u")).wants_access_token());
        assert!(!auth_profile(Standard, AuthMode::Auto, Some("u")).wants_access_token());
        assert!(!auth_profile(DockerHub, AuthMode::Auto, Some("u")).wants_access_token());

        // Explicit basic/bearer never want a token.
        assert!(!auth_profile(Artifactory, AuthMode::Basic, Some("u")).wants_access_token());
        assert!(!auth_profile(Artifactory, AuthMode::Bearer, Some("u")).wants_access_token());
    }

    #[test]
    fn auth_kind_matrix() {
        use AuthKind as K;
        use AuthMode as M;
        use RegistryType::{Artifactory, DockerHub, Ghcr, Standard};

        /// (registry type, auth mode, username, has_token, has_password) -> kind
        type Case = (
            RegistryType,
            AuthMode,
            Option<&'static str>,
            bool,
            bool,
            AuthKind,
        );

        let cases: &[Case] = &[
            // Artifactory + auto: Basic still wins when possible (unchanged).
            (Artifactory, M::Auto, Some("u"), false, true, K::Basic),
            (Artifactory, M::Auto, Some("u"), true, true, K::Basic),
            // ... and a token fills in when there is no password. Both of
            // these were NoCredentials before this feature.
            (Artifactory, M::Auto, Some("u"), true, false, K::AccessToken),
            (Artifactory, M::Auto, None, true, false, K::AccessToken),
            (Artifactory, M::Auto, None, false, false, K::None),
            (Artifactory, M::Auto, Some("u"), false, false, K::None),
            // Standard / Docker Hub + auto: bearer exchange, token-blind.
            (Standard, M::Auto, Some("u"), false, true, K::Bearer),
            (DockerHub, M::Auto, Some("u"), false, true, K::Bearer),
            (Standard, M::Auto, Some("u"), true, false, K::None),
            (Standard, M::Auto, None, true, true, K::None),
            // Explicit token mode: generic across registry types.
            (Standard, M::Token, None, true, false, K::AccessToken),
            (DockerHub, M::Token, Some("u"), true, true, K::AccessToken),
            (Artifactory, M::Token, None, true, false, K::AccessToken),
            (Artifactory, M::Token, None, false, true, K::None),
            // GHCR + auto: a PAT wins, because it is the only credential that
            // also unlocks repo discovery via the GitHub packages API. It maps
            // to `Ghcr`, never `AccessToken` — GHCR rejects a raw PAT on /v2/
            // with a 403 that `send` cannot re-challenge from, so the token has
            // to be exchanged for a scope-bound one.
            (Ghcr, M::Auto, None, true, false, K::Ghcr),
            (Ghcr, M::Auto, Some("u"), true, true, K::Ghcr),
            // Without one, fall back to the v2 token exchange — Bearer, not
            // the Basic that Artifactory falls back to, because GHCR is a real
            // Docker v2 registry and rejects plain Basic on /v2/.
            (Ghcr, M::Auto, Some("u"), false, true, K::Bearer),
            // With no credential at all, still `Ghcr`, not `None`: its exchange
            // works anonymously, and that is the only way to browse public
            // packages without a PAT.
            (Ghcr, M::Auto, None, false, false, K::Ghcr),
            (Ghcr, M::Auto, Some("u"), false, false, K::Ghcr),
            // Explicit token mode on GHCR, with and without a resolved token.
            (Ghcr, M::Token, None, true, false, K::Ghcr),
            (Ghcr, M::Token, None, false, false, K::Ghcr),
            // Explicit basic / bearer.
            (Artifactory, M::Basic, Some("u"), false, true, K::Basic),
            (Artifactory, M::Basic, Some("u"), true, false, K::None),
            (Standard, M::Bearer, Some("u"), false, true, K::Bearer),
            (Standard, M::Bearer, Some("u"), true, false, K::None),
        ];

        for &(kind, mode, username, has_token, has_password, expected) in cases {
            let p = auth_profile(kind, mode, username);
            assert_eq!(
                p.auth_kind(has_token, has_password),
                expected,
                "type={kind:?} auth={mode:?} username={username:?} \
                 token={has_token} password={has_password}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // GHCR
    // -----------------------------------------------------------------------

    #[test]
    fn ghcr_type_round_trips_and_is_detected() {
        let profile = RegistryProfile {
            name: "ghcr".to_owned(),
            url: Some("https://ghcr.io".to_owned()),
            registry_type: RegistryType::Ghcr,
            owner: Some("pgmac-net".to_owned()),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&profile).unwrap();
        assert!(text.contains(r#"type = "ghcr""#));
        assert!(text.contains(r#"owner = "pgmac-net""#));

        let loaded: RegistryProfile = toml::from_str(&text).unwrap();
        assert!(loaded.is_ghcr());
        assert_eq!(loaded.owner.as_deref(), Some("pgmac-net"));
        assert!(!loaded.is_dockerhub());
        assert!(!loaded.is_artifactory());
    }

    /// An `owner`-less profile must not write `owner = ""` into the config, or
    /// a round-trip would turn "my own packages" into a request for the
    /// packages of an empty-named user.
    #[test]
    fn absent_owner_stays_absent_through_a_round_trip() {
        let profile = RegistryProfile {
            name: "ghcr".to_owned(),
            url: Some("https://ghcr.io".to_owned()),
            registry_type: RegistryType::Ghcr,
            ..Default::default()
        };
        let text = toml::to_string_pretty(&profile).unwrap();
        assert!(!text.contains("owner"));
        assert!(
            toml::from_str::<RegistryProfile>(&text)
                .unwrap()
                .owner
                .is_none()
        );
    }

    /// URL detection upgrades a profile already pointed at ghcr.io as
    /// `standard` — which dead-ends at the catalog 401 — without a config edit.
    #[test]
    fn ghcr_is_detected_from_url_when_type_is_unset() {
        assert!(profile("gh", "https://ghcr.io").is_ghcr());
        assert!(profile("gh", "https://ghcr.io/v2/").is_ghcr());
    }

    #[test]
    fn other_registries_are_not_ghcr() {
        assert!(!profile("local", "http://localhost:5000").is_ghcr());
        assert!(!profile("hub", "https://registry-1.docker.io").is_ghcr());
        // A self-hosted Artifactory could sit at any hostname, so an explicit
        // type must never be overridden by URL sniffing.
        let artifactory = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://ghcr.io".to_owned()),
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };
        assert!(!artifactory.is_ghcr());
    }

    /// The two token vocabularies must not overlap in either direction: a
    /// `GITHUB_TOKEN` exported for `gh` must never authenticate Artifactory,
    /// and a `JFROG_ACCESS_TOKEN` must never authenticate GHCR.
    #[test]
    fn token_env_vars_are_disjoint_per_registry_type() {
        let ghcr = profile("gh", "https://ghcr.io");
        let artifactory = RegistryProfile {
            name: "art".to_owned(),
            url: Some("https://art.example.com/artifactory".to_owned()),
            registry_type: RegistryType::Artifactory,
            ..Default::default()
        };

        assert_eq!(
            ghcr.token_env_vars(),
            ["CR_PAT", "GITHUB_TOKEN", "GH_TOKEN"]
        );
        assert_eq!(
            artifactory.token_env_vars(),
            ["JFROG_ACCESS_TOKEN", "ARTIFACTORY_ACCESS_TOKEN"]
        );

        for var in ghcr.token_env_vars() {
            assert!(
                !artifactory.token_env_vars().contains(var),
                "{var} must not be readable by an Artifactory profile"
            );
        }
    }

    /// Standard and Docker Hub keep the list they already used, so
    /// `auth = "token"` on a pre-existing profile resolves exactly as before
    /// this partition existed.
    #[test]
    fn non_ghcr_types_keep_the_pre_existing_token_env_vars() {
        assert_eq!(
            profile("local", "http://localhost:5000").token_env_vars(),
            ["JFROG_ACCESS_TOKEN", "ARTIFACTORY_ACCESS_TOKEN"]
        );
    }

    #[test]
    fn ghcr_wants_a_token_under_auto() {
        // Auto only consults the environment for the two types that can use a
        // token; GHCR joins Artifactory there.
        assert!(profile("gh", "https://ghcr.io").wants_access_token());
        assert!(!profile("local", "http://localhost:5000").wants_access_token());
    }

    // -----------------------------------------------------------------------
    // AWS ECR
    // -----------------------------------------------------------------------

    fn ecr_profile(kind: RegistryType) -> RegistryProfile {
        RegistryProfile {
            name: "ecr".to_owned(),
            registry_type: kind,
            aws_profile: Some("pgmac".to_owned()),
            region: Some("ap-southeast-2".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn ecr_types_round_trip_and_are_detected() {
        let text = toml::to_string_pretty(&ecr_profile(RegistryType::Ecr)).unwrap();
        assert!(text.contains(r#"type = "ecr""#), "{text}");
        assert!(text.contains(r#"aws_profile = "pgmac""#), "{text}");
        assert!(text.contains(r#"region = "ap-southeast-2""#), "{text}");
        // No URL was set, and none may be invented on the way out.
        assert!(!text.contains("url"), "{text}");

        let loaded: RegistryProfile = toml::from_str(&text).unwrap();
        assert!(loaded.is_ecr());
        assert!(loaded.is_any_ecr());
        assert!(!loaded.is_ecr_public());
        assert!(!loaded.is_ghcr());
        assert!(!loaded.is_dockerhub());
        assert!(!loaded.is_artifactory());
    }

    /// `rename_all = "lowercase"` would spell this `ecrpublic`; the explicit
    /// rename is what keeps the config and CLI vocabularies readable.
    #[test]
    fn ecr_public_is_spelled_with_a_hyphen() {
        let text = toml::to_string_pretty(&ecr_profile(RegistryType::EcrPublic)).unwrap();
        assert!(text.contains(r#"type = "ecr-public""#), "{text}");

        let loaded: RegistryProfile = toml::from_str(&text).unwrap();
        assert!(loaded.is_ecr_public());
        assert!(loaded.is_any_ecr());
        assert!(!loaded.is_ecr());
    }

    #[test]
    fn ecr_is_detected_from_a_registry_url_when_type_is_unset() {
        assert!(profile("e", "https://012345678910.dkr.ecr.us-east-1.amazonaws.com").is_ecr());
        assert!(profile("p", "https://public.ecr.aws").is_ecr_public());
    }

    /// `amazonaws.com` hosts every AWS service, so the sniff must key on the
    /// registry-specific `.dkr.ecr.` infix rather than the domain alone.
    #[test]
    fn other_amazonaws_hosts_are_not_ecr() {
        assert!(!profile("s3", "https://s3.ap-southeast-2.amazonaws.com").is_ecr());
        assert!(!profile("api", "https://api.ecr.us-east-1.amazonaws.com").is_ecr());
        assert!(!profile("local", "http://localhost:5000").is_any_ecr());
    }

    /// The load-bearing half of the token-vocabulary partition for ECR: its
    /// credential comes from the AWS chain, so neither the JFrog nor the GitHub
    /// variables may be offered to it.
    #[test]
    fn ecr_reads_no_token_environment_variables() {
        for kind in [RegistryType::Ecr, RegistryType::EcrPublic] {
            let profile = ecr_profile(kind);
            assert!(
                profile.token_env_vars().is_empty(),
                "{kind:?} must read no token env vars, got {:?}",
                profile.token_env_vars()
            );
            assert!(!profile.wants_access_token());
        }
    }

    /// Even `auth = "token"`, which for every other type means "resolve a
    /// token", must not send ECR looking for one to prompt for.
    #[test]
    fn ecr_never_wants_a_resolved_token_even_in_token_mode() {
        let mut profile = ecr_profile(RegistryType::Ecr);
        profile.auth = AuthMode::Token;

        assert!(!profile.wants_access_token());
        assert_eq!(profile.auth_kind(false, false), AuthKind::Ecr);
    }

    #[test]
    fn ecr_auth_kind_ignores_username_and_password_under_auto() {
        let mut profile = ecr_profile(RegistryType::Ecr);
        profile.username = Some("someone".to_owned());

        assert_eq!(profile.auth_kind(true, true), AuthKind::Ecr);
        assert_eq!(profile.auth_kind(false, false), AuthKind::Ecr);
    }

    /// The escape hatch: an explicit `basic`/`bearer` still wins, for an
    /// authenticating proxy in front of an ECR endpoint.
    #[test]
    fn an_explicit_mode_overrides_the_ecr_default() {
        let mut profile = ecr_profile(RegistryType::Ecr);
        profile.username = Some("someone".to_owned());
        profile.auth = AuthMode::Basic;

        assert_eq!(profile.auth_kind(false, true), AuthKind::Basic);
    }

    #[test]
    fn only_ecr_may_omit_the_url() {
        let ok = Config {
            default_registry: None,
            registry: vec![ecr_profile(RegistryType::Ecr)],
        };
        assert!(ok.validate().is_ok());

        let bad = Config {
            default_registry: None,
            registry: vec![RegistryProfile {
                name: "standard".to_owned(),
                ..Default::default()
            }],
        };
        let err = bad.validate().unwrap_err().to_string();
        assert!(err.contains("has no url"), "{err}");
    }

    /// An ECR profile that *does* carry a URL is still validated like any
    /// other — omitting the field is a licence to derive it, not to skip the
    /// check when it is present.
    #[test]
    fn an_explicit_ecr_url_is_still_validated() {
        let mut profile = ecr_profile(RegistryType::Ecr);
        profile.url = Some("not-a-url".to_owned());
        let config = Config {
            default_registry: None,
            registry: vec![profile],
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn a_derived_url_displays_the_aws_target_instead_of_nothing() {
        assert_eq!(
            ecr_profile(RegistryType::Ecr).display_url(),
            "ecr (pgmac / ap-southeast-2)"
        );
        assert_eq!(
            ecr_profile(RegistryType::EcrPublic).display_url(),
            "ecr-public (pgmac)"
        );

        let mut chain = ecr_profile(RegistryType::Ecr);
        chain.aws_profile = None;
        chain.region = None;
        assert_eq!(chain.display_url(), "ecr (default)");
    }

    #[test]
    fn a_resolved_url_displaces_the_placeholder() {
        let mut profile = ecr_profile(RegistryType::Ecr);
        profile.url = Some("https://1.dkr.ecr.ap-southeast-2.amazonaws.com".to_owned());

        assert_eq!(
            profile.display_url(),
            "https://1.dkr.ecr.ap-southeast-2.amazonaws.com"
        );
    }
}
