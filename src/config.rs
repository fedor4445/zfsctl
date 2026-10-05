use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_CONFIG_PATH: &str = "/usr/local/etc/zfsctl/config.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
    #[serde(default, skip_serializing_if = "UiConfig::is_default")]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UiConfig {
    /// One of `ui::theme::THEME_NAMES`; validated at startup by
    /// `ui::theme::resolve`, not here, so the error can list the choices.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub theme: Option<String>,
}

impl UiConfig {
    fn is_default(&self) -> bool {
        self.theme.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub name: String,
    /// SSH destination (e.g. `user@host`, or a `~/.ssh/config` alias).
    /// `None` means this entry is a duplicate/manual reference to the
    /// local host and is not expected in practice — real remote hosts
    /// always set this.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ssh_target: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AppError::Config(format!("reading {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| AppError::Config(format!("parsing {}: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| AppError::Config(format!("serializing config: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Config(format!("creating {}: {e}", parent.display())))?;
        }
        std::fs::write(path, text)
            .map_err(|e| AppError::Config(format!("writing {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let config = Config::load(&path).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_malformed_toml_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();
        let result = Config::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = Config {
            hosts: vec![
                HostConfig { name: "local".to_string(), ssh_target: None },
                HostConfig {
                    name: "backup".to_string(),
                    ssh_target: Some("backup.example.com".to_string()),
                },
            ],
            ..Default::default()
        };
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn ui_theme_defaults_to_none_and_is_not_serialized_when_unset() {
        let config = Config::default();
        assert_eq!(config.ui.theme, None);
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(!text.contains("[ui]"), "{text}");
        assert!(!text.contains("theme"), "{text}");
    }

    #[test]
    fn ui_theme_parses_from_toml_alongside_hosts() {
        let text = "[ui]\ntheme = \"solarized-dark\"\n\n[[hosts]]\nname = \"backup\"\nssh_target = \"backup.example.com\"\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.ui.theme.as_deref(), Some("solarized-dark"));
        assert_eq!(config.hosts.len(), 1);
    }

    #[test]
    fn ui_theme_survives_a_save_load_round_trip_with_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
            ui: UiConfig { theme: Some("mono".to_string()) },
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }
}
