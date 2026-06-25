use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

const EXAMPLE_CONFIG: &str = r#"# docker-registry-walk configuration
# See: https://github.com/pgmac-net/docker-registry-walk
#
# Passwords are stored in the OS keyring, never in this file.
# Run: docker-registry-walk --url <url> --username <user> --password <pass>
# to populate the keyring on first use.

# Name of the registry to open on startup (optional).
# default_registry = "local"

[[registry]]
name = "local"
url = "http://localhost:5000"
# username = "admin"
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryProfile {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
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
}
