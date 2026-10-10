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
    #[serde(default, skip_serializing_if = "DaemonConfig::is_default")]
    pub daemon: DaemonConfig,
    #[serde(default, skip_serializing_if = "NotifyConfig::is_default")]
    pub notify: NotifyConfig,
}

/// Settings for `zfsctl daemon`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DaemonConfig {
    /// A shell command run when a scheduled job fails. The job's details
    /// arrive in `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT`
    /// and `ZFSCTL_MESSAGE`; never on the command line.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notify_command: Option<String>,
}

impl DaemonConfig {
    fn is_default(&self) -> bool {
        self.notify_command.is_none()
    }
}

pub const DEFAULT_MAIL_COMMAND: &str = "msmtp -t";

/// Health alerts and the daily report (`[notify]`). Unset keys take the documented defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NotifyConfig {
    /// One address or several, comma separated. Unset means no email.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_to: Option<String>,
    /// Run through `sh -c` with the whole message (headers included) on stdin. Default `msmtp -t`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub check_interval_minutes: Option<u64>,
    /// `HH:MM` on the hub's clock, or `off`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub report_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub remind_hours: Option<u64>,
}

impl NotifyConfig {
    fn is_default(&self) -> bool {
        *self == NotifyConfig::default()
    }

    /// Validates the section and fills in the defaults.
    pub fn resolve(&self) -> std::result::Result<NotifySettings, String> {
        let valid_address = |a: &str| {
            let mut parts = a.split('@');
            matches!((parts.next(), parts.next(), parts.next()), (Some(l), Some(d), None) if !l.is_empty() && !d.is_empty())
                && !a.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '<' | '>' | '"'))
        };
        let mail_to: Vec<String> = match self.mail_to.as_deref() {
            None => Vec::new(),
            Some(list) => {
                let parts: Vec<String> = list.split(',').map(|p| p.trim().to_string()).collect();
                if let Some(bad) = parts.iter().find(|p| !valid_address(p)) {
                    return Err(format!("notify.mail_to: '{bad}' is not a plain address (user@host, no spaces or control characters)"));
                }
                parts
            }
        };
        let mail_command = match self.mail_command.as_deref() {
            None => DEFAULT_MAIL_COMMAND.to_string(),
            Some(c) if c.trim().is_empty() => return Err("notify.mail_command is empty".to_string()),
            Some(c) => c.to_string(),
        };
        if let Some(from) = self.mail_from.as_deref() {
            if from.trim().is_empty() || from.chars().any(char::is_control) {
                return Err("notify.mail_from must be one line".to_string());
            }
        }
        let minutes = self.check_interval_minutes.unwrap_or(5);
        if minutes < 1 {
            return Err("notify.check_interval_minutes must be at least 1".to_string());
        }
        let report_time = match self.report_time.as_deref() {
            None => chrono::NaiveTime::from_hms_opt(7, 0, 0),
            Some("off") => None,
            Some(t) => Some(chrono::NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| format!("notify.report_time '{t}' is not HH:MM (or off)"))?),
        };
        let remind = match self.remind_hours.unwrap_or(6) {
            0 => None,
            h => Some(std::time::Duration::from_secs(h * 3600)),
        };
        Ok(NotifySettings { mail_to, mail_command, mail_from: self.mail_from.clone(), check_interval: std::time::Duration::from_secs(minutes * 60), report_time, remind })
    }
}

/// `NotifyConfig` validated, with the defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct NotifySettings {
    pub mail_to: Vec<String>,
    pub mail_command: String,
    pub mail_from: Option<String>,
    pub check_interval: std::time::Duration,
    pub report_time: Option<chrono::NaiveTime>,
    pub remind: Option<std::time::Duration>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifyConfig::default().resolve().unwrap_or_else(|_| unreachable!("the defaults always validate"))
    }
}

impl NotifySettings {
    pub fn has_mail(&self) -> bool {
        !self.mail_to.is_empty()
    }
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
    /// A free label for this host (`backup`, `garage`), shown in the Hosts table and matched by its filter.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tag: Option<String>,
}

/// A tag as stored: trimmed, at most 40 characters, `None` when blank.
pub fn clean_tag(raw: &str) -> Option<String> {
    let t: String = raw.trim().chars().take(40).collect();
    let t = t.trim_end().to_string();
    (!t.is_empty()).then_some(t)
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
                HostConfig { name: "local".to_string(), ssh_target: None, tag: None },
                HostConfig {
                    name: "backup".to_string(),
                    ssh_target: Some("backup.example.com".to_string()),
                    tag: None,
                },
            ],
            ..Default::default()
        };
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn clean_tag_trims_drops_blank_and_limits_to_forty_characters() {
        assert_eq!(clean_tag("  backup "), Some("backup".to_string()));
        assert_eq!(clean_tag("   "), None);
        assert_eq!(clean_tag(&"x".repeat(60)).unwrap().chars().count(), 40);
    }

    #[test]
    fn a_config_without_a_tag_loads_and_a_tag_round_trips_and_is_omitted_when_none() {
        let old = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
        let c: Config = toml::from_str(old).unwrap();
        assert_eq!(c.hosts[0].tag, None);
        assert!(!toml::to_string(&c).unwrap().contains("tag"));
        let with = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"backup\"\n";
        let c: Config = toml::from_str(with).unwrap();
        assert_eq!(c.hosts[0].tag.as_deref(), Some("backup"));
        assert!(toml::to_string(&c).unwrap().contains("tag = \"backup\""));
    }

    #[test]
    fn a_config_without_notify_loads_saves_unchanged_and_resolves_to_the_defaults() {
        let config: Config = toml::from_str("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
        assert_eq!(config.notify, NotifyConfig::default());
        assert!(!toml::to_string_pretty(&config).unwrap().contains("notify"));
        let s = config.notify.resolve().unwrap();
        assert!(s.mail_to.is_empty() && !s.has_mail());
        assert_eq!(s.mail_command, "msmtp -t");
        assert_eq!(s.check_interval, std::time::Duration::from_secs(300));
        assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap()));
        assert_eq!(s.remind, Some(std::time::Duration::from_secs(6 * 3600)));
        assert_eq!(s.mail_from, None);
    }

    #[test]
    fn every_notify_key_parses_and_round_trips() {
        let text = "[notify]\nmail_to = \"a@x.org, b@y.org\"\nmail_command = \"msmtp -a default -t\"\nmail_from = \"zfsctl@hub\"\ncheck_interval_minutes = 10\nreport_time = \"06:30\"\nremind_hours = 0\n";
        let config: Config = toml::from_str(text).unwrap();
        let s = config.notify.resolve().unwrap();
        assert_eq!(s.mail_to, ["a@x.org", "b@y.org"]);
        assert_eq!((s.mail_command.as_str(), s.mail_from.as_deref()), ("msmtp -a default -t", Some("zfsctl@hub")));
        assert_eq!(s.check_interval, std::time::Duration::from_secs(600));
        assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(6, 30, 0).unwrap()));
        assert_eq!(s.remind, None, "0 hours means never");
        assert_eq!(toml::from_str::<Config>(&toml::to_string_pretty(&config).unwrap()).unwrap(), config);
    }

    #[test]
    fn report_time_off_disables_the_report() {
        let c = NotifyConfig { report_time: Some("off".into()), ..NotifyConfig::default() };
        assert_eq!(c.resolve().unwrap().report_time, None);
    }

    #[test]
    fn bad_notify_values_are_refused_with_the_reason() {
        let bad = |c: NotifyConfig| c.resolve().unwrap_err();
        let base = NotifyConfig::default();
        for to in ["nobody", "a@", "@x.org", "a b@x.org", "a@x.org\nBcc: z@z.org", "a@x@y", "<a@x.org>", "a@x.org,,b@y.org"] {
            assert!(bad(NotifyConfig { mail_to: Some(to.into()), ..base.clone() }).contains("mail_to"), "{to:?}");
        }
        assert!(bad(NotifyConfig { check_interval_minutes: Some(0), ..base.clone() }).contains("check_interval_minutes"));
        assert!(bad(NotifyConfig { report_time: Some("7am".into()), ..base.clone() }).contains("report_time"));
        assert!(bad(NotifyConfig { report_time: Some("25:00".into()), ..base.clone() }).contains("report_time"));
        assert!(bad(NotifyConfig { mail_command: Some("  ".into()), ..base.clone() }).contains("mail_command"));
        assert!(bad(NotifyConfig { mail_from: Some("a\r\nb".into()), ..base }).contains("mail_from"));
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
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()), tag: None }],
            ui: UiConfig { theme: Some("mono".to_string()), auto_refresh_secs: None },
            ..Default::default()
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

    #[test]
    fn daemon_notify_command_parses_and_is_only_written_back_when_set() {
        let config: Config = toml::from_str("[daemon]\nnotify_command = \"curl -s ntfy.sh/zfs\"\n").unwrap();
        assert_eq!(config.daemon.notify_command.as_deref(), Some("curl -s ntfy.sh/zfs"));
        assert!(toml::to_string_pretty(&config).unwrap().contains("notify_command"));
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!text.contains("daemon") && !text.contains("notify"), "{text}");
    }
}
