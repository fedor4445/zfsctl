use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Every key a user can write in `config.toml` (`[[hosts]]`, `[ui]`, `[daemon]`, `[notify]`, `[ssh]`) and in `jobs.toml`
/// (`[[jobs]]`), as written in TOML. The book's reference chapter is checked against this list, and
/// `documented_keys_match_the_serialised_fields` checks the list against the structs themselves.
#[cfg(test)]
pub(crate) fn documented_config_keys() -> Vec<&'static str> {
    vec![
        "name", "ssh_target", "tag", "site",
        "theme", "auto_refresh_secs",
        "multiplex",
        "notify_command", "daily_check",
        "mail_to", "mail_command", "mail_from", "check_interval_minutes", "report_time", "remind_hours",
        "caps",
        "memory_warn", "memory_crit", "swap_warn", "swap_crit", "quota_warn", "quota_crit", "dedup_min_ratio", "cpu_warn", "scrub_warn_days", "scrub_crit_days", "scrub_ignore",
        "level", "copies", "online", "recent", "email",
        "id", "host", "kind", "pool", "dataset", "recursive", "prefix", "keep", "dest_host", "dest_dataset", "dest_dir", "dest_keep", "remote", "bwlimit", "force",
        "repository", "password_file", "retention", "source", "snapshot", "path", "schedule", "catch_up", "enabled", "created_at",
    ]
}

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
    #[serde(default, skip_serializing_if = "SshConfig::is_default")]
    pub ssh: SshConfig,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "CloudConfig::is_default")]
    pub cloud: CloudConfig,
    #[serde(default, deserialize_with = "lenient", skip_serializing_if = "DashboardConfig::is_default")]
    pub dashboard: DashboardConfig,
    #[serde(default, deserialize_with = "lenient_protection", skip_serializing_if = "ProtectionConfig::is_default")]
    pub protection: ProtectionConfig,
    /// `[groups.<tag>]`: what the hosts with that `tag` set for themselves (protection, mail recipients), kept as TOML so a wrong
    /// value is reported by `protection_settings` and `group_mail` instead of stopping the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<toml::Value>,
}

/// What one group may set (`[groups.<tag>]`): the keys of `[protection]` for its hosts and where its hosts' alerts go.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GroupConfig {
    #[serde(default, deserialize_with = "lenient_protection", skip_serializing_if = "ProtectionConfig::is_default")]
    pub protection: ProtectionConfig,
    #[serde(default)]
    pub notify: GroupNotify,
}

/// `[groups.<tag>.notify]`: only the recipients; everything else about mail is global.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GroupNotify {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_to: Option<toml::Value>,
}

/// What counts as protected (`[protection]`): how many copies of a dataset, how many of them online, how recent, and when to
/// email. Wrong values are not defaulted: `resolve` names the key and the allowed values. Each key is kept as the TOML value
/// the file has, so a value of the wrong type (`copies = "2"`, `level = 5`) still loads (the daemon must never stay down over
/// it) and `resolve` reports it like any other wrong value.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ProtectionConfig {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub level: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub copies: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub online: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recent: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<toml::Value>,
    /// `protection = 5`: the section is not a table at all.
    #[serde(skip)]
    pub not_a_table: bool,
}

/// `[protection]` as a table of any values (see `ProtectionConfig`); anything else is kept as `not_a_table`, for `resolve`.
fn lenient_protection<'de, D>(deserializer: D) -> std::result::Result<ProtectionConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value {
        toml::Value::Table(_) => value.try_into().unwrap_or(ProtectionConfig { not_a_table: true, ..ProtectionConfig::default() }),
        _ => ProtectionConfig { not_a_table: true, ..ProtectionConfig::default() },
    })
}

/// A comma-separated list of plain addresses, or the first one that is not (`user@host`, no spaces or control characters).
pub fn parse_addresses(list: &str) -> std::result::Result<Vec<String>, String> {
    let valid_address = |a: &str| {
        let mut parts = a.split('@');
        matches!((parts.next(), parts.next(), parts.next()), (Some(l), Some(d), None) if !l.is_empty() && !d.is_empty())
            && !a.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '<' | '>' | '"'))
    };
    let parts: Vec<String> = list.split(',').map(|p| p.trim().to_string()).collect();
    match parts.iter().find(|p| !valid_address(p)) {
        Some(bad) => Err(bad.clone()),
        None => Ok(parts),
    }
}

/// How a `[protection]` value reads on the screen: a string as it is, anything else as its TOML text (`5`, `true`).
pub fn protection_text(value: Option<&toml::Value>) -> Option<String> {
    value.map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
}

/// When the unprotected-datasets alert is emailed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmailTiming {
    #[default]
    Immediate,
    Report,
    Off,
}

/// `ProtectionConfig` validated, with the defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionSettings {
    /// Copies a dataset needs (1 to 3).
    pub copies: u8,
    /// Of those, copies that must be online (0 to `copies`).
    pub online: u8,
    /// How many intervals old a copy may be and still count as recent.
    pub tolerance: i32,
    pub email: EmailTiming,
    /// Which group each host is in, and what each group sets for itself.
    pub groups: GroupRules,
}

/// What applies to one host once its group's keys are laid over the global ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effective {
    pub copies: u8,
    pub online: u8,
    pub tolerance: i32,
    pub email: EmailTiming,
}

/// The `tag` of each configured host and the settings of each group that has its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupRules {
    host_tag: std::collections::BTreeMap<String, String>,
    host_site: std::collections::BTreeMap<String, String>,
    by_tag: std::collections::BTreeMap<String, Effective>,
}

impl ProtectionSettings {
    /// The failure domain of `host`: its `site` when the config names one, else the host itself.
    pub fn domain_of(&self, host: &str) -> String {
        self.groups.host_site.get(host).cloned().unwrap_or_else(|| host.to_string())
    }

    /// The settings that apply to `host`: its group's, or the global ones (a host without a tag, or in a group that sets
    /// nothing, and `local`, the hub).
    pub fn for_host(&self, host: &str) -> Effective {
        self.groups.host_tag.get(host).and_then(|tag| self.groups.by_tag.get(tag)).copied().unwrap_or(Effective { copies: self.copies, online: self.online, tolerance: self.tolerance, email: self.email })
    }
}

impl Default for ProtectionSettings {
    fn default() -> Self {
        ProtectionSettings { copies: 1, online: 0, tolerance: 2, email: EmailTiming::Immediate, groups: GroupRules::default() }
    }
}

impl ProtectionConfig {
    fn is_default(&self) -> bool {
        // A section that is not a table has nothing to write back.
        ProtectionConfig { not_a_table: false, ..self.clone() } == ProtectionConfig::default()
    }

    pub fn resolve(&self) -> std::result::Result<ProtectionSettings, String> {
        if self.not_a_table {
            return Err("[protection] must be a table of keys (level, copies, online, recent, email)".into());
        }
        // A word key: absent is the default, a string is checked by the caller, any other type is wrong.
        let word = |v: &Option<toml::Value>, default: &'static str, wrong: &str| -> std::result::Result<String, String> {
            match v {
                None => Ok(default.to_string()),
                Some(toml::Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(wrong.to_string()),
            }
        };
        // A number key: absent is the default, else a whole number in `range`.
        let count = |v: &Option<toml::Value>, default: u8, range: std::ops::RangeInclusive<u8>, wrong: &str| -> std::result::Result<u8, String> {
            match v {
                None => Ok(default),
                Some(toml::Value::Integer(n)) => u8::try_from(*n).ok().filter(|n| range.contains(n)).ok_or_else(|| wrong.to_string()),
                Some(_) => Err(wrong.to_string()),
            }
        };
        let level_wrong = "[protection] level must be basic, careful or custom";
        let (copies, online) = match word(&self.level, "basic", level_wrong)?.as_str() {
            "basic" => (1, 0),
            "careful" => (3, 1),
            "custom" => {
                let copies = count(&self.copies, 1, 1..=3, "[protection] copies must be 1, 2 or 3")?;
                let online = count(&self.online, 0, 0..=2, "[protection] online must be 0, 1 or 2")?;
                if online > copies {
                    return Err("[protection] online must not be more than copies".into());
                }
                (copies, online)
            }
            _ => return Err(level_wrong.into()),
        };
        let recent_wrong = "[protection] recent must be strict, normal or relaxed";
        let tolerance = match word(&self.recent, "normal", recent_wrong)?.as_str() {
            "strict" => 1,
            "normal" => 2,
            "relaxed" => 4,
            _ => return Err(recent_wrong.into()),
        };
        let email_wrong = "[protection] email must be immediate, report or off";
        let email = match word(&self.email, "immediate", email_wrong)?.as_str() {
            "immediate" => EmailTiming::Immediate,
            "report" => EmailTiming::Report,
            "off" => EmailTiming::Off,
            _ => return Err(email_wrong.into()),
        };
        Ok(ProtectionSettings { copies, online, tolerance, email, groups: GroupRules::default() })
    }

    /// The settings to judge with on a screen: `resolve`, or Basic's defaults with the problem in words.
    pub fn resolve_or_basic(&self) -> (ProtectionSettings, Option<String>) {
        match self.resolve() {
            Ok(settings) => (settings, None),
            Err(problem) => (ProtectionSettings::default(), Some(problem)),
        }
    }
}

/// How a screen states a wrong `[protection]`: `[protection]: email must be immediate, report or off; judging at Basic`.
pub fn protection_problem_line(problem: &str) -> String {
    match problem.strip_prefix("[protection] ") {
        Some(rest) => format!("[protection]: {rest}; judging at Basic"),
        // A wrong group: its hosts are judged by the global settings meanwhile.
        None => format!("{problem}; those hosts use the global settings"),
    }
}

/// When the Dashboard turns a reading yellow (watch) and red (issue) (`[dashboard]`). Unset keys take the defaults of
/// `Thresholds::default`; a pair that does not make sense (warn not below crit, zero, over 100) is ignored as a pair.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DashboardConfig {
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub memory_warn: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub memory_crit: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub swap_warn: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub swap_crit: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub quota_warn: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub quota_crit: Option<u8>,
    /// Warn when deduplication saves less than this factor (`1.10` = 10%); a pool paying RAM for nothing.
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub dedup_min_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub cpu_warn: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub scrub_warn_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default, deserialize_with = "lenient")]
    pub scrub_crit_days: Option<u32>,
    /// Pools the Dashboard does not judge by scrub age (scratch pools nobody scrubs).
    #[serde(skip_serializing_if = "Vec::is_empty", default, deserialize_with = "lenient")]
    pub scrub_ignore: Vec<String>,
}

/// The Dashboard's thresholds with every default filled in and every pair checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thresholds {
    /// Memory in use, percent: (watch, issue).
    pub memory: (u8, u8),
    /// Swap in use, percent: (watch, issue).
    pub swap: (u8, u8),
    /// A dataset's use of its quota, percent: (watch, issue).
    pub quota: (u8, u8),
    /// The dedup ratio below which a pool with dedup on is listed, in hundredths (`110` = 1.10x).
    pub dedup_ratio: u16,
    /// CPU busy, percent, over the ~30 s between the last two probes: watch only, never an issue.
    pub cpu_warn: u8,
    /// Days since the last scrub: (watch, issue).
    pub scrub_days: (u32, u32),
    pub scrub_ignore: Vec<String>,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { memory: (90, 97), swap: (50, 90), quota: (90, 98), dedup_ratio: 110, cpu_warn: 95, scrub_days: (35, 90), scrub_ignore: Vec::new() }
    }
}

impl DashboardConfig {
    fn is_default(&self) -> bool {
        *self == DashboardConfig::default()
    }

    pub fn resolve(&self) -> Thresholds {
        let d = Thresholds::default();
        let percent_pair = |warn: Option<u8>, crit: Option<u8>, default: (u8, u8)| {
            let pair = (warn.unwrap_or(default.0), crit.unwrap_or(default.1));
            if pair.0 > 0 && pair.0 < pair.1 && pair.1 <= 100 { pair } else { default }
        };
        let days = (self.scrub_warn_days.unwrap_or(d.scrub_days.0), self.scrub_crit_days.unwrap_or(d.scrub_days.1));
        Thresholds {
            memory: percent_pair(self.memory_warn, self.memory_crit, d.memory),
            swap: percent_pair(self.swap_warn, self.swap_crit, d.swap),
            quota: percent_pair(self.quota_warn, self.quota_crit, d.quota),
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1.00..=10.00 times 100
            dedup_ratio: self.dedup_min_ratio.filter(|r| (1.0..=10.0).contains(r)).map_or(d.dedup_ratio, |r| (r * 100.0).round() as u16),
            cpu_warn: self.cpu_warn.filter(|c| (1..=100).contains(c)).unwrap_or(d.cpu_warn),
            scrub_days: if days.0 > 0 && days.0 < days.1 { days } else { d.scrub_days },
            scrub_ignore: self.scrub_ignore.clone(),
        }
    }
}

/// For settings that only decorate a screen: a value of the wrong type (`memory_warn = 300`, `= "90"`, a table where a list
/// belongs) becomes the default instead of an error, because the daemon and the TUI refuse to start on a config that does not
/// parse, and a typo in a display setting must never take the jobs and the alerts down with it.
fn lenient<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = toml::Value::deserialize(deserializer)?;
    Ok(value.try_into().unwrap_or_default())
}

/// `[cloud.caps]`: a cap that is not text (`b2 = 10`) is kept as its text, so the Cloud screen can say it is not a size.
fn lenient_caps<'de, D>(deserializer: D) -> std::result::Result<std::collections::BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value {
        toml::Value::Table(table) => table.into_iter().map(|(name, v)| (name, v.as_str().map_or_else(|| v.to_string(), str::to_string))).collect(),
        _ => std::collections::BTreeMap::new(),
    })
}

/// What the Cloud screen needs to know about your cloud accounts (`[cloud]`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CloudConfig {
    /// `[cloud.caps]`: the room a plan gives you, by rclone remote name, such as `b2 = "10G"`. The Cloud screen shows how much
    /// of it a remote's buckets use. Only a size; nothing here can reach the account.
    #[serde(default, deserialize_with = "lenient_caps", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub caps: std::collections::BTreeMap<String, String>,
}

impl CloudConfig {
    fn is_default(&self) -> bool {
        self.caps.is_empty()
    }
}

/// A plan size: digits (a decimal point is allowed) and an optional `K`, `M`, `G` or `T`, counted the way cloud plans
/// count (`10G` is 10 000 000 000 bytes). `None` for anything else, zero included.
pub fn parse_cap(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().last()? {
        (i, c @ ('K' | 'M' | 'G' | 'T')) => (&text[..i], match c { 'K' => 1e3, 'M' => 1e6, 'G' => 1e9, _ => 1e12 }),
        _ => (text, 1.0),
    };
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit() || c == '.') || number.matches('.').count() > 1 {
        return None;
    }
    let bytes = number.parse::<f64>().ok()? * unit;
    (bytes >= 1.0 && bytes < 1e18).then(|| bytes.round() as u64)
}

/// How zfsctl talks to the other hosts (`[ssh]`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SshConfig {
    /// `true` makes the short commands to one host share a single ssh connection instead of opening one each, which
    /// is about four times faster per command. Unset means `false`. Read when zfsctl or the daemon starts.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub multiplex: Option<bool>,
}

impl SshConfig {
    fn is_default(&self) -> bool {
        self.multiplex.is_none()
    }

    /// Where the shared connections' sockets live, when sharing is on.
    pub fn mux_dir(&self) -> Option<std::path::PathBuf> {
        self.multiplex.unwrap_or(false).then(|| std::path::Path::new(crate::daemon::engine::DEFAULT_STATE_DIR).join("ssh"))
    }
}

/// Settings for `zfsctl daemon`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DaemonConfig {
    /// A shell command run when a scheduled job fails. The job's details
    /// arrive in `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT`
    /// and `ZFSCTL_MESSAGE`; never on the command line.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notify_command: Option<String>,
    /// When the daemon runs the daily checks (disks with `smartctl`, file sharing services): `"HH:MM"` (hub local time),
    /// or `"off"`. Unset is `03:30`. Read when the daemon starts.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub daily_check: Option<String>,
    /// The name this setting had for a day, when it covered the disks only: still read, never written.
    #[serde(skip_serializing, default)]
    pub smart_check: Option<String>,
}

pub const DEFAULT_DAILY_CHECK: &str = "03:30";

impl DaemonConfig {
    fn is_default(&self) -> bool {
        self.notify_command.is_none() && self.daily_check.is_none() && self.smart_check.is_none()
    }

    /// The time of the daily checks, `None` when they are switched off; an error says what is wrong with the value.
    pub fn daily_time(&self) -> std::result::Result<Option<chrono::NaiveTime>, String> {
        let text = self.daily_check.as_deref().or(self.smart_check.as_deref()).unwrap_or(DEFAULT_DAILY_CHECK).trim();
        if text == "off" {
            return Ok(None);
        }
        let well_formed = text.len() == 5 && text.as_bytes().get(2) == Some(&b':');
        chrono::NaiveTime::parse_from_str(text, "%H:%M").ok().filter(|_| well_formed).map(Some).ok_or_else(|| format!("daily_check '{text}' is not HH:MM or \"off\""))
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
        let mail_to: Vec<String> = match self.mail_to.as_deref() {
            None => Vec::new(),
            Some(list) => parse_addresses(list).map_err(|bad| format!("notify.mail_to: '{bad}' is not a plain address (user@host, no spaces or control characters)"))?,
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
        if !(1..=1440).contains(&minutes) {
            return Err("notify.check_interval_minutes must be between 1 and 1440".to_string());
        }
        let report_time = match self.report_time.as_deref() {
            None => chrono::NaiveTime::from_hms_opt(7, 0, 0),
            Some("off") => None,
            Some(t) => Some(chrono::NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| format!("notify.report_time '{t}' is not HH:MM (or off)"))?),
        };
        let remind = match self.remind_hours.unwrap_or(6) {
            0 => None,
            h if h > 8760 => return Err("notify.remind_hours must be at most 8760 (a year); 0 means never".to_string()),
            h => Some(std::time::Duration::from_secs(h * 3600)),
        };
        Ok(NotifySettings { mail_to, mail_command, mail_from: self.mail_from.clone(), check_interval: std::time::Duration::from_secs(minutes * 60), report_time, remind })
    }
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The `[notify]` section for the keys that are set: a blank field is a default and writes nothing.
pub fn render_notify_section(c: &NotifyConfig) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[notify]\n");
    let mut put = |key: &str, value: String| {
        let _ = writeln!(out, "{key} = {value}");
    };
    if let Some(v) = &c.mail_to {
        put("mail_to", toml_string(v));
    }
    if let Some(v) = &c.mail_command {
        put("mail_command", toml_string(v));
    }
    if let Some(v) = &c.mail_from {
        put("mail_from", toml_string(v));
    }
    if let Some(v) = c.check_interval_minutes {
        put("check_interval_minutes", v.to_string());
    }
    if let Some(v) = &c.report_time {
        put("report_time", toml_string(v));
    }
    if let Some(v) = c.remind_hours {
        put("remind_hours", v.to_string());
    }
    out
}

/// A `[[hosts]]` block for `host`, one `key = "value"` line per set key, ending in a newline.
pub fn render_host_block(host: &HostConfig) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[[hosts]]\n");
    let _ = writeln!(out, "name = {}", toml_string(&host.name));
    if let Some(target) = &host.ssh_target {
        let _ = writeln!(out, "ssh_target = {}", toml_string(target));
    }
    if let Some(tag) = &host.tag {
        let _ = writeln!(out, "tag = {}", toml_string(tag));
    }
    if let Some(site) = &host.site {
        let _ = writeln!(out, "site = {}", toml_string(site));
    }
    out
}

/// Adds a `[[hosts]]` block for `host` right after the last existing host block (so the hosts stay together),
/// or at the end of the file when there is none, keeping every other byte: the comments that follow the last host
/// belong to what comes next and stay below the new block. Refuses `hosts` written as an inline array or dotted keys,
/// and anything that would change the file beyond adding the one host.
pub fn add_host_block(text: &str, host: &HostConfig) -> std::result::Result<String, String> {
    const BY_HAND: &str = "config.toml writes hosts in a form this screen cannot edit safely: edit it by hand";
    let crlf = text.contains("\r\n");
    let fix = |s: &str| if crlf { s.replace('\n', "\r\n") } else { s.to_string() };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_header = |l: &str| l.trim_start().starts_with('[');
    let is_hosts_header = |l: &str| {
        let t = l.trim();
        t == "[[hosts]]" || t.strip_prefix("[[hosts]]").is_some_and(|rest| rest.trim_start().starts_with('#'))
    };
    for l in &lines {
        let t = l.trim_start();
        let key_is_hosts = t.starts_with("hosts.") || t.strip_prefix("hosts").is_some_and(|r| r.trim_start().starts_with('='));
        if key_is_hosts || t.starts_with("[hosts.") || (t.starts_with("[hosts]") && !t.starts_with("[[")) {
            return Err(BY_HAND.to_string());
        }
    }
    let block = fix(&render_host_block(host));
    let result = if let Some(last) = lines.iter().rposition(|l| is_hosts_header(l)) {
        let end = lines.iter().enumerate().skip(last + 1).find(|(_, l)| is_header(l)).map_or(lines.len(), |(i, _)| i);
        // After the last line of real content: trailing blank and comment lines belong to what follows.
        let insert_at = (last..end).rev().find(|&i| lines.get(i).is_some_and(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))).map_or(last + 1, |i| i + 1);
        let mut out: String = lines.iter().take(insert_at).copied().collect();
        if !out.ends_with('\n') {
            out.push_str(&fix("\n"));
        }
        out.push_str(&fix("\n"));
        out.push_str(&block);
        out.push_str(&lines.iter().skip(insert_at).copied().collect::<String>());
        out
    } else {
        let mut out = text.to_string();
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push_str(&fix("\n"));
            }
            out.push_str(&fix("\n"));
        }
        out.push_str(&block);
        out
    };
    let parse = |t: &str| -> std::result::Result<(toml::Table, Vec<HostConfig>), String> {
        let mut table: toml::Table = t.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
        table.remove("hosts");
        let hosts = toml::from_str::<Config>(t).map_err(|e| format!("config.toml does not parse: {e}"))?.hosts;
        Ok((table, hosts))
    };
    let (old_rest, old_hosts) = parse(text)?;
    let (new_rest, new_hosts) = parse(&result)?;
    let mut want = old_hosts;
    want.push(host.clone());
    if old_rest != new_rest || new_hosts != want {
        return Err(BY_HAND.to_string());
    }
    Ok(result)
}

const NOTIFY_KEYS: [&str; 6] = ["mail_to", "mail_command", "mail_from", "check_interval_minutes", "report_time", "remind_hours"];

/// A line that sets one of the keys this screen owns (`mail_to = ...`).
fn is_notify_key_line(line: &str) -> bool {
    let t = line.trim_start();
    NOTIFY_KEYS.iter().any(|k| t.strip_prefix(k).is_some_and(|rest| rest.trim_start().starts_with('=')))
}

/// A value that opens a multi-line string it does not close on the same line (not something these keys have).
fn opens_multiline_string(line: &str) -> bool {
    let value = line.split_once('=').map_or("", |(_, v)| v.trim_start());
    ["\"\"\"", "'''"].iter().any(|q| value.starts_with(q) && !value[q.len()..].contains(q))
}

/// Replaces (or appends) the `[notify]` section of `text`, keeping every other byte: inside an existing section only
/// the lines that set this screen's own keys are replaced, so its comments, blank lines and any other key stay.
/// Refuses what it cannot edit safely (a dotted `notify.` key, an inline `notify = {...}`, a `[notify.x]` sub-table,
/// two `[notify]` headers) and anything that would change the file beyond `[notify]` or not say what was asked.
pub fn set_notify_section(text: &str, section: &str) -> std::result::Result<String, String> {
    const BY_HAND: &str = "config.toml writes [notify] in a form this screen cannot edit safely: edit it by hand";
    let crlf = text.contains("\r\n");
    let fix = |s: &str| if crlf { s.replace('\n', "\r\n") } else { s.to_string() };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let is_header = |l: &str| l.trim_start().starts_with('[');
    let is_notify_header = |l: &str| {
        let t = l.trim();
        t == "[notify]" || t.strip_prefix("[notify]").is_some_and(|rest| rest.trim_start().starts_with('#'))
    };
    for l in &lines {
        let t = l.trim_start();
        let key_is_notify = t.starts_with("notify.") || t.strip_prefix("notify").is_some_and(|r| r.trim_start().starts_with('='));
        if key_is_notify || t.starts_with("[notify.") {
            return Err(BY_HAND.to_string());
        }
    }
    let headers: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_notify_header(l)).map(|(i, _)| i).collect();
    if headers.len() > 1 {
        return Err(BY_HAND.to_string());
    }
    // A `notify` table written some other way (`[ notify ]`, `["notify"]`, a quoted key, behind a byte-order mark)
    // that the line scan did not see as the header: appending a second one would be refused later with a confusing
    // "duplicate key", so say what is going on.
    if headers.is_empty() && text.parse::<toml::Table>().is_ok_and(|t| t.contains_key("notify")) {
        return Err(BY_HAND.to_string());
    }
    let new_keys = fix(&section.split_inclusive('\n').skip(1).collect::<String>());
    let result = if let Some(&start) = headers.first() {
        let end = lines.iter().enumerate().skip(start + 1).find(|(_, l)| is_header(l)).map_or(lines.len(), |(i, _)| i);
        let mut out: String = lines.iter().take(start + 1).copied().collect();
        if !out.ends_with('\n') {
            out.push_str(&fix("\n"));
        }
        out.push_str(&new_keys);
        for l in lines.iter().take(end).skip(start + 1) {
            if is_notify_key_line(l) {
                if opens_multiline_string(l) {
                    return Err(BY_HAND.to_string());
                }
                continue;
            }
            out.push_str(l);
        }
        out.push_str(&lines.iter().skip(end).copied().collect::<String>());
        out
    } else {
        let mut out = text.to_string();
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push_str(&fix("\n"));
            }
            out.push_str(&fix("\n"));
        }
        out.push_str(&fix(section));
        out
    };
    let want = toml::from_str::<Config>(section).map_err(|e| format!("the new [notify] section does not parse: {e}"))?.notify;
    let got = toml::from_str::<Config>(&result).map_err(|e| format!("config.toml would no longer parse: {e}"))?.notify;
    if got != want {
        return Err("config.toml would not say what was asked: edit it by hand".to_string());
    }
    // Nothing outside [notify] may change: compare the whole file as data, so a surprise in a string or an unknown
    // table is caught too.
    let rest = |t: &str| -> std::result::Result<toml::Table, String> {
        let mut table: toml::Table = t.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
        table.remove("notify");
        Ok(table)
    };
    if rest(text)? != rest(&result)? {
        return Err(BY_HAND.to_string());
    }
    Ok(result)
}

/// Writes the new `[notify]` section into the file: the old file is copied to `<file>.bak`, the new text goes to a
/// private temporary file next to it (same permissions and owner, flushed to disk) and is renamed over it. A
/// symlinked `config.toml` is followed, so the link stays a link. Any failure leaves the original untouched.
pub fn save_notify_section(path: &Path, section: &str) -> Result<()> {
    rewrite_config(path, &|text| set_notify_section(text, section))
}

/// Adds one host to `config.toml` with the same safe write as the `[notify]` save (backup, private temporary file,
/// original owner and mode, change guard): nothing but the new `[[hosts]]` block is written.
pub fn append_host_to_file(path: &Path, host: &HostConfig) -> Result<()> {
    rewrite_config(path, &|text| add_host_block(text, host))
}

/// The safe rewrite both saves share: `edit` turns the old text into the new text (or refuses), and the file is
/// replaced only if every step succeeds; any failure leaves the original untouched.
pub(crate) fn rewrite_config(path: &Path, edit: &dyn Fn(&str) -> std::result::Result<String, String>) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = real.file_name().map_or_else(|| "config.toml".to_string(), |n| n.to_string_lossy().into_owned());
    let before = std::fs::metadata(&real).ok();
    let old = match std::fs::read_to_string(&real) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::Config(format!("reading {}: {e}", real.display()))),
    };
    if before.as_ref().is_some_and(|m| m.len() > 0) && old.as_deref().is_some_and(str::is_empty) {
        return Err(AppError::Config(format!("{} reads as empty although it is not: it is being rewritten; try again", real.display())));
    }
    let new = edit(old.as_deref().unwrap_or("")).map_err(AppError::Config)?;
    if let Some(parent) = real.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Config(format!("creating {}: {e}", parent.display())))?;
    }
    if old.is_some() {
        let bak = real.with_file_name(format!("{name}.bak"));
        std::fs::copy(&real, &bak).map_err(|e| AppError::Config(format!("writing {}: {e}", bak.display())))?;
    }
    let tmp = real.with_file_name(format!("{name}.tmp.{}", std::process::id()));
    let mode = before.as_ref().map_or(0o644, |m| m.mode() & 0o7777);
    let fail = |what: &str, e: std::io::Error| {
        let _ = std::fs::remove_file(&tmp);
        AppError::Config(format!("{what} {}: {e}", tmp.display()))
    };
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).map_err(|e| AppError::Config(format!("writing {}: {e}", tmp.display())))?;
    file.write_all(new.as_bytes()).map_err(|e| fail("writing", e))?;
    file.sync_all().map_err(|e| fail("flushing", e))?;
    drop(file);
    // The mode given at creation is cut by the umask: set it exactly. A failure is a failure.
    std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(mode)).map_err(|e| fail("setting the permissions of", e))?;
    if let Some(m) = &before {
        if m.uid() != nix::unistd::geteuid().as_raw() || m.gid() != nix::unistd::getegid().as_raw() {
            nix::unistd::chown(&tmp, Some(nix::unistd::Uid::from_raw(m.uid())), Some(nix::unistd::Gid::from_raw(m.gid()))).map_err(|e| fail("giving the original owner to", e.into()))?;
        }
    }
    // Refuse to overwrite a change made while we worked (an editor, Add Host, a second TUI).
    let now = std::fs::metadata(&real).ok();
    if before.as_ref().map(|m| (m.len(), m.mtime(), m.mtime_nsec())) != now.as_ref().map(|m| (m.len(), m.mtime(), m.mtime_nsec())) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::Config(format!("{} changed while it was being saved: nothing was written, try again", real.display())));
    }
    std::fs::rename(&tmp, &real).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AppError::Config(format!("replacing {}: {e}", real.display()))
    })?;
    if let Some(dir) = real.parent().and_then(|d| std::fs::File::open(d).ok()) {
        let _ = dir.sync_all();
    }
    Ok(())
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
    /// What this host shares its failure with (`rack1`, `office`): hosts with the same `site` count as one place when the
    /// data-safety graph asks how many separate failures the data survives. Unset, a host is its own place.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub site: Option<String>,
}

/// A tag as stored: trimmed, at most 40 characters, `None` when blank.
pub fn clean_tag(raw: &str) -> Option<String> {
    let t: String = raw.trim().chars().take(40).collect();
    let t = t.trim_end().to_string();
    (!t.is_empty()).then_some(t)
}

impl Config {
    /// The groups of `[groups]`, each with its tag and what it sets, and what is wrong with the section.
    fn group_entries(&self) -> (Vec<(String, GroupConfig)>, Vec<String>) {
        let (mut groups, mut problems) = (Vec::new(), Vec::new());
        match &self.groups {
            None => {}
            Some(toml::Value::Table(table)) => {
                for (tag, entry) in table {
                    match entry {
                        toml::Value::Table(_) => match entry.clone().try_into::<GroupConfig>() {
                            Ok(group) => groups.push((tag.clone(), group)),
                            Err(e) => problems.push(format!("[groups.{tag}] {}", e.message())),
                        },
                        _ => problems.push(format!("[groups.{tag}] must be a table")),
                    }
                }
            }
            Some(_) => problems.push("[groups] must be a table of groups, one per tag".to_string()),
        }
        (groups, problems)
    }

    /// The hosts that carry `tag`.
    fn hosts_tagged(&self, tag: &str) -> impl Iterator<Item = &str> {
        let tag = tag.to_string();
        self.hosts.iter().filter(move |h| h.tag.as_deref().is_some_and(|t| t.trim() == tag)).map(|h| h.name.as_str())
    }

    /// `[protection]` with the `[groups.<tag>.protection]` keys laid over it for each group, and what is wrong (the global
    /// section first). A wrong group falls back to the global settings: never looser than what the operator wrote.
    pub fn protection_settings(&self) -> (ProtectionSettings, Vec<String>) {
        let (mut settings, global_problem) = self.protection.resolve_or_basic();
        let mut problems: Vec<String> = global_problem.iter().cloned().collect();
        let (groups, group_problems) = self.group_entries();
        problems.extend(group_problems);
        let over = |mine: &Option<toml::Value>, base: &Option<toml::Value>| mine.clone().or_else(|| base.clone());
        let mut rules = GroupRules::default();
        for host in &self.hosts {
            if let Some(tag) = host.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                rules.host_tag.insert(host.name.clone(), tag.to_string());
            }
            if let Some(site) = host.site.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                rules.host_site.insert(host.name.clone(), site.to_string());
            }
        }
        for (tag, group) in groups {
            let (g, b) = (&group.protection, &self.protection);
            if g.is_default() {
                continue;
            }
            let merged = ProtectionConfig { level: over(&g.level, &b.level), copies: over(&g.copies, &b.copies), online: over(&g.online, &b.online), recent: over(&g.recent, &b.recent), email: over(&g.email, &b.email), not_a_table: false };
            match merged.resolve() {
                Ok(s) => {
                    rules.by_tag.insert(tag, Effective { copies: s.copies, online: s.online, tolerance: s.tolerance, email: s.email });
                }
                // The global section is already reported, and the group would only repeat it.
                Err(e) if global_problem.as_deref() == Some(e.as_str()) => {}
                Err(e) => problems.push(format!("[groups.{tag}.protection]{}", e.strip_prefix("[protection]").unwrap_or(&e))),
            }
        }
        settings.groups = rules;
        (settings, problems)
    }

    /// The recipients of the alerts about each host whose group has its own `mail_to`, and what is wrong with them. A host
    /// that is not in the map is mailed at the global address.
    pub fn group_mail(&self) -> (std::collections::BTreeMap<String, Vec<String>>, Vec<String>) {
        let (groups, mut problems) = self.group_entries();
        let mut mail = std::collections::BTreeMap::new();
        for (tag, group) in groups {
            let Some(value) = group.notify.mail_to else { continue };
            let parsed = match &value {
                toml::Value::String(list) => parse_addresses(list).map_err(|bad| format!("[groups.{tag}.notify] mail_to: '{bad}' is not a plain address (user@host, no spaces or control characters)")),
                _ => Err(format!("[groups.{tag}.notify] mail_to must be an address, or several separated by commas")),
            };
            match parsed {
                Ok(addresses) => {
                    for host in self.hosts_tagged(&tag) {
                        mail.insert(host.to_string(), addresses.clone());
                    }
                }
                Err(problem) => problems.push(problem),
            }
        }
        (mail, problems)
    }

    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AppError::Config(format!("reading {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| AppError::Config(format!("parsing {}: {e}", path.display())))
    }

    /// Serialises the whole config (comments and order are lost): only for tests; the program writes `config.toml`
    /// through `append_host_to_file` and `save_notify_section`, which keep everything else.
    #[cfg(test)]
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
mod tests;
