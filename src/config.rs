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
    /// Seconds between background refreshes of the list you are looking at.
    /// Unset means `DEFAULT_AUTO_REFRESH_SECS`; `0` turns auto-refresh off.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub auto_refresh_secs: Option<u64>,
}

pub const DEFAULT_AUTO_REFRESH_SECS: u64 = 10;

impl UiConfig {
    fn is_default(&self) -> bool {
        self.theme.is_none() && self.auto_refresh_secs.is_none()
    }

    /// The auto-refresh period, or `None` when it is switched off (`0`).
    pub fn auto_refresh_interval(&self) -> Option<std::time::Duration> {
        match self.auto_refresh_secs.unwrap_or(DEFAULT_AUTO_REFRESH_SECS) {
            0 => None,
            secs => Some(std::time::Duration::from_secs(secs)),
        }
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
            ui: UiConfig { theme: Some("mono".to_string()), auto_refresh_secs: None },
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn auto_refresh_defaults_to_ten_seconds_and_zero_turns_it_off() {
        use std::time::Duration;
        let ui = |secs| UiConfig { theme: None, auto_refresh_secs: secs };
        assert_eq!(ui(None).auto_refresh_interval(), Some(Duration::from_secs(10)));
        assert_eq!(ui(Some(30)).auto_refresh_interval(), Some(Duration::from_secs(30)));
        assert_eq!(ui(Some(0)).auto_refresh_interval(), None);
    }

    #[test]
    fn auto_refresh_secs_parses_from_toml_and_is_only_written_back_when_set() {
        let config: Config = toml::from_str("[ui]\nauto_refresh_secs = 0\n").unwrap();
        assert_eq!(config.ui.auto_refresh_interval(), None);
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("auto_refresh_secs = 0"), "{text}");
        assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("auto_refresh"));
    }
}
