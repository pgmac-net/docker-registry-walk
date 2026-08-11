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
"#;

/// What kind of registry this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RegistryType {
    /// A standard Docker Registry v2 endpoint.
    #[default]
    Standard,
    /// Docker Hub (hub.docker.com).  The catalog endpoint is not supported,
    /// so the TUI falls back to the hub search API to find repos.
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
}

/// How to authenticate to a registry, as configured.
///
/// This is the user's *intent*; the credential actually built also depends on
/// which secrets are available at runtime — see [`RegistryProfile::auth_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryProfile {
    pub name: String,
    pub url: String,
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
}

impl RegistryProfile {
    /// Returns `true` when the registry is Docker Hub (either explicitly
    /// configured or detected from the URL).
    pub fn is_dockerhub(&self) -> bool {
        match self.registry_type {
            RegistryType::DockerHub => true,
            RegistryType::Standard => {
                // Fall back to URL-based detection for backward compatibility.
                let Ok(u) = url::Url::parse(&self.url) else {
                    return false;
                };
                let Some(host) = u.host_str() else {
                    return false;
                };
                matches!(
                    host,
                    "registry-1.docker.io" | "docker.io" | "index.docker.io"
                )
            }
            RegistryType::Artifactory => false,
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
    /// `auth = "auto"` only Artifactory consults `JFROG_ACCESS_TOKEN` /
    /// `ARTIFACTORY_ACCESS_TOKEN`. A stray JFrog variable exported in a shell
    /// must not change how anyone authenticates to docker.io.
    pub fn wants_access_token(&self) -> bool {
        self.auth.is_token() || (self.auth == AuthMode::Auto && self.is_artifactory())
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
            Url::parse(&profile.url).map_err(|e| {
                anyhow::anyhow!(
                    "Registry '{}' has invalid URL '{}': {e}",
                    profile.name,
                    profile.url
                )
            })?;

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
            url: url.to_owned(),
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
                    url: "https://registry.example.com".to_owned(),
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
            url: "https://artifactory.example.com/artifactory".to_owned(),
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
            url: "https://registry-1.docker.io".to_owned(),
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
                url: "https://r.example.com".to_owned(),
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
            url: "https://r.example.com".to_owned(),
            username: username.map(str::to_owned),
            registry_type: kind,
            auth: mode,
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
        use RegistryType::{Artifactory, DockerHub, Standard};

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
}
