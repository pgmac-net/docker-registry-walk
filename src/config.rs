use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryProfile {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub registry: Vec<RegistryProfile>,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::default_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn default_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_else(|_| PathBuf::from(".config"))
            });
        base.join("docker-registry-walk").join("config.toml")
    }
}
