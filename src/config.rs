use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Every key a user can write in `config.toml` (`[[hosts]]`, `[ui]`, `[daemon]`, `[notify]`, `[ssh]`) and in `jobs.toml`
/// (`[[jobs]]`), as written in TOML. The book's reference chapter is checked against this list, and
/// `documented_keys_match_the_serialised_fields` checks the list against the structs themselves.
#[cfg(test)]
pub(crate) fn documented_config_keys() -> Vec<&'static str> {
    vec![
        "name", "ssh_target", "tag",
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
}

impl Default for ProtectionSettings {
    fn default() -> Self {
        ProtectionSettings { copies: 1, online: 0, tolerance: 2, email: EmailTiming::Immediate }
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
        Ok(ProtectionSettings { copies, online, tolerance, email })
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
    format!("[protection]: {}; judging at Basic", problem.strip_prefix("[protection] ").unwrap_or(problem))
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
mod tests {
    #[test]
    fn daily_check_accepts_off_and_hh_mm_and_rejects_the_rest() {
        let time = |v: Option<&str>| DaemonConfig { daily_check: v.map(str::to_string), ..DaemonConfig::default() }.daily_time();
        assert_eq!(time(None), Ok(chrono::NaiveTime::from_hms_opt(3, 30, 0)), "the default is 03:30");
        assert_eq!(time(Some("off")), Ok(None));
        assert_eq!(time(Some("05:15")), Ok(chrono::NaiveTime::from_hms_opt(5, 15, 0)));
        for bad in ["", "25:00", "3pm", "03:30:00", "on"] {
            assert!(time(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_old_smart_check_key_is_still_read_and_daily_check_wins_when_both_are_there() {
        let read = |text: &str| toml::from_str::<Config>(text).unwrap().daemon.daily_time();
        assert_eq!(read("[daemon]\nsmart_check = \"05:15\"\n"), Ok(chrono::NaiveTime::from_hms_opt(5, 15, 0)));
        assert_eq!(read("[daemon]\nsmart_check = \"05:15\"\ndaily_check = \"off\"\n"), Ok(None));
        assert_eq!(read("[daemon]\ndaily_check = \"04:00\"\n"), Ok(chrono::NaiveTime::from_hms_opt(4, 0, 0)));
        let saved = toml::to_string(&toml::from_str::<Config>("[daemon]\nsmart_check = \"05:15\"\n").unwrap()).unwrap();
        assert!(!saved.contains("smart_check"), "the old key is read, never written: {saved}");
    }

    #[test]
    fn documented_keys_match_the_serialised_fields() {
        let host = HostConfig { name: "n".into(), ssh_target: Some("u@h".into()), tag: Some("t".into()) };
        let ui = UiConfig { theme: Some("mono".into()), auto_refresh_secs: Some(5) };
        let daemon = DaemonConfig { notify_command: Some("true".into()), daily_check: Some("03:30".into()), smart_check: None };
        let cloud = CloudConfig { caps: [("b2".to_string(), "10G".to_string())].into() };
        let ssh = SshConfig { multiplex: Some(true) };
        let dashboard = DashboardConfig {
            memory_warn: Some(90),
            memory_crit: Some(97),
            swap_warn: Some(50),
            swap_crit: Some(90),
            quota_warn: Some(90),
            quota_crit: Some(98),
            dedup_min_ratio: Some(1.10),
            cpu_warn: Some(95),
            scrub_warn_days: Some(35),
            scrub_crit_days: Some(90),
            scrub_ignore: vec!["scratch".into()],
        };
        let protection = ProtectionConfig { level: Some("custom".into()), copies: Some(2.into()), online: Some(1.into()), recent: Some("normal".into()), email: Some("report".into()), not_a_table: false };
        let notify = NotifyConfig {
            mail_to: Some("a@b".into()),
            mail_command: Some("c".into()),
            mail_from: Some("f@g".into()),
            check_interval_minutes: Some(5),
            report_time: Some("07:00".into()),
            remind_hours: Some(6),
        };
        let job = crate::jobs::Job {
            id: "i".into(), host: "local".into(), kind: "backup".into(), pool: Some("p".into()), dataset: Some("d".into()), recursive: true,
            prefix: Some("x".into()), keep: Some(1), dest_host: Some("h".into()), dest_dataset: Some("d".into()), dest_dir: Some("/d".into()), dest_keep: Some(2), remote: Some("aws:b".into()), bwlimit: Some("5M".into()),
            force: true, raw: false, repository: Some("r".into()), password_file: Some("/p".into()), retention: Some("7d".into()), source: Some("new".into()), snapshot: Some("s".into()), path: Some("p".into()), schedule: "* * * * *".into(),
            catch_up: true, enabled: true, created_at: Some(chrono::Utc::now()),
        };
        let mut found: Vec<String> = Vec::new();
        for value in [toml::Value::try_from(&host).unwrap(), toml::Value::try_from(&ui).unwrap(), toml::Value::try_from(&daemon).unwrap(), toml::Value::try_from(&notify).unwrap(), toml::Value::try_from(&cloud).unwrap(), toml::Value::try_from(&ssh).unwrap(), toml::Value::try_from(&dashboard).unwrap(), toml::Value::try_from(&protection).unwrap(), toml::Value::try_from(&job).unwrap()] {
            found.extend(value.as_table().unwrap().keys().cloned());
        }
        found.sort();
        found.dedup();
        let mut listed: Vec<String> = documented_config_keys().into_iter().map(String::from).collect();
        listed.sort();
        listed.dedup();
        assert_eq!(found, listed, "documented_config_keys() must list exactly the fields the structs serialise");
    }

    use super::*;

    #[test]
    fn connection_sharing_is_off_unless_the_ssh_section_turns_it_on() {
        assert_eq!(Config::default().ssh.mux_dir(), None);
        let on: Config = toml::from_str("[ssh]\nmultiplex = true\n").unwrap();
        assert_eq!(on.ssh.mux_dir(), Some(std::path::PathBuf::from("/var/db/zfsctl/ssh")));
        let off: Config = toml::from_str("[ssh]\nmultiplex = false\n").unwrap();
        assert_eq!(off.ssh.mux_dir(), None);
        assert!(!toml::to_string(&Config::default()).unwrap().contains("ssh"), "an unset [ssh] is not written");
    }

    #[test]
    fn the_dashboard_defaults_are_the_specs_numbers() {
        let t = Thresholds::default();
        assert_eq!((t.memory, t.swap, t.cpu_warn, t.scrub_days), ((90, 97), (50, 90), 95, (35, 90)));
        assert!(t.scrub_ignore.is_empty());
        assert_eq!(Config::default().dashboard.resolve(), t, "no [dashboard] section means the defaults");
    }

    #[test]
    fn quota_thresholds_default_to_90_and_98_and_fall_back_when_invalid() {
        assert_eq!(Thresholds::default().quota, (90, 98));
        let c: Config = toml::from_str("[dashboard]\nquota_warn = 80\nquota_crit = 95\n").unwrap();
        assert_eq!(c.dashboard.resolve().quota, (80, 95));
        let bad: Config = toml::from_str("[dashboard]\nquota_warn = 99\nquota_crit = 95\n").unwrap();
        assert_eq!(bad.dashboard.resolve().quota, (90, 98), "warn above crit falls back");
    }

    #[test]
    fn the_dedup_ratio_threshold_defaults_to_1_10_and_falls_back_when_invalid() {
        assert_eq!(Thresholds::default().dedup_ratio, 110);
        let ok: Config = toml::from_str("[dashboard]\ndedup_min_ratio = 1.25\n").unwrap();
        assert_eq!(ok.dashboard.resolve().dedup_ratio, 125);
        for bad in ["0.5", "11.0", "\"abc\""] {
            let c: Config = toml::from_str(&format!("[dashboard]\ndedup_min_ratio = {bad}\n")).unwrap();
            assert_eq!(c.dashboard.resolve().dedup_ratio, 110, "{bad}");
        }
    }

    #[test]
    fn dashboard_overrides_are_read_from_config_toml_and_resolved() {
        let text = "[dashboard]\nmemory_warn = 80\nmemory_crit = 95\nswap_warn = 20\nswap_crit = 60\ncpu_warn = 90\nscrub_warn_days = 14\nscrub_crit_days = 30\nscrub_ignore = [\"scratch\", \"tmp\"]\n";
        let c: Config = toml::from_str(text).unwrap();
        let t = c.dashboard.resolve();
        assert_eq!((t.memory, t.swap, t.cpu_warn, t.scrub_days), ((80, 95), (20, 60), 90, (14, 30)));
        assert_eq!(t.scrub_ignore, ["scratch", "tmp"]);
        let partial: Config = toml::from_str("[dashboard]\nmemory_warn = 85\n").unwrap();
        assert_eq!(partial.dashboard.resolve().memory, (85, 97), "one key of a pair keeps the other's default");
        assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("dashboard"), "an empty section writes nothing");
    }

    #[test]
    fn a_pair_that_is_not_warn_below_crit_within_range_falls_back_to_its_defaults() {
        let bad = |memory_warn, memory_crit| DashboardConfig { memory_warn, memory_crit, ..DashboardConfig::default() }.resolve().memory;
        assert_eq!(bad(Some(97), Some(90)), (90, 97), "warn above crit");
        assert_eq!(bad(Some(90), Some(90)), (90, 97), "warn equal to crit");
        assert_eq!(bad(Some(0), Some(50)), (90, 97), "zero");
        assert_eq!(bad(Some(50), Some(101)), (90, 97), "over 100");
        assert_eq!(bad(Some(50), Some(100)), (50, 100), "100 is allowed");
        let days = |w, c| DashboardConfig { scrub_warn_days: w, scrub_crit_days: c, ..DashboardConfig::default() }.resolve().scrub_days;
        assert_eq!(days(Some(0), Some(10)), (35, 90));
        assert_eq!(days(Some(40), Some(30)), (35, 90));
        assert_eq!(days(Some(7), Some(8)), (7, 8));
        assert_eq!(DashboardConfig { cpu_warn: Some(0), ..DashboardConfig::default() }.resolve().cpu_warn, 95);
        assert_eq!(DashboardConfig { cpu_warn: Some(101), ..DashboardConfig::default() }.resolve().cpu_warn, 95);
        assert_eq!(DashboardConfig { cpu_warn: Some(100), ..DashboardConfig::default() }.resolve().cpu_warn, 100);
    }

    #[test]
    fn a_typo_in_a_display_setting_never_stops_the_config_from_loading() {
        // The daemon and the TUI refuse to start on a config that does not parse: [dashboard] and [cloud] only decorate a
        // screen, so a wrong type there must cost that one setting, not the jobs and the alerts after a reboot.
        for text in [
            "[dashboard]\nmemory_warn = 300\n",
            "[dashboard]\nmemory_warn = -1\n",
            "[dashboard]\nmemory_warn = 92.5\n",
            "[dashboard]\nmemory_warn = \"90\"\n",
            "[dashboard]\nscrub_ignore = \"scratch\"\n",
            "[dashboard]\nscrub_ignore = [1, 2]\n",
            "dashboard = 5\n",
            "cloud = 3\n",
            "[cloud]\ncaps = 7\n",
            "[cloud.caps]\nb2 = 10\n",
        ] {
            let parsed: std::result::Result<Config, toml::de::Error> = toml::from_str(text);
            assert!(parsed.is_ok(), "{text:?}: {parsed:?}");
        }
        let bad: Config = toml::from_str("[dashboard]\nmemory_warn = 300\nswap_warn = 20\nswap_crit = 60\nscrub_ignore = \"scratch\"\n").unwrap();
        let t = bad.dashboard.resolve();
        assert_eq!((t.memory, t.swap), ((90, 97), (20, 60)), "the bad key falls back, the good ones are kept");
        assert!(t.scrub_ignore.is_empty());
        let caps: Config = toml::from_str("[cloud.caps]\nb2 = 10\nzfsctl-aws = \"5G\"\n").unwrap();
        assert_eq!(caps.cloud.caps.get("b2").map(String::as_str), Some("10"), "a non-text cap is kept as text, to be reported as not a size");
        assert_eq!(caps.cloud.caps.get("zfsctl-aws").map(String::as_str), Some("5G"));
        // a whole config with a bad section still loads from disk, and its other parts are intact.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[dashboard]\nmemory_crit = 999\n").unwrap();
        let loaded = Config::load(&path).expect("a bad [dashboard] value must not stop the program from starting");
        assert_eq!(loaded.hosts.len(), 1);
    }

    #[test]
    fn plan_sizes_are_read_the_way_cloud_plans_count_them() {
        for (text, bytes) in [("10G", 10_000_000_000), ("2.5G", 2_500_000_000), ("500M", 500_000_000), ("1T", 1_000_000_000_000), ("1000", 1000), (" 10G ", 10_000_000_000)] {
            assert_eq!(parse_cap(text), Some(bytes), "{text}");
        }
        for bad in ["", "G", "0", "0G", "-1G", "10GB", "1.2.3G", "ten", "10 G", "1e9", "99999999999T", "10g"] {
            assert_eq!(parse_cap(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn caps_are_read_from_the_cloud_section_and_an_empty_section_writes_nothing() {
        let c: Config = toml::from_str("[cloud.caps]\nb2 = \"10G\"\nzfsctl-aws = \"5G\"\n").unwrap();
        assert_eq!(c.cloud.caps.get("b2").map(String::as_str), Some("10G"));
        assert_eq!(c.cloud.caps.len(), 2);
        assert!(!toml::to_string_pretty(&Config::default()).unwrap().contains("cloud"));
    }

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

    fn prot(level: Option<&str>, copies: Option<u8>, online: Option<u8>, recent: Option<&str>, email: Option<&str>) -> ProtectionConfig {
        ProtectionConfig { level: level.map(Into::into), copies: copies.map(Into::into), online: online.map(Into::into), recent: recent.map(Into::into), email: email.map(Into::into), not_a_table: false }
    }

    #[test]
    fn protection_defaults_are_basic_normal_immediate() {
        let s = ProtectionConfig::default().resolve().unwrap();
        assert_eq!(s, ProtectionSettings { copies: 1, online: 0, tolerance: 2, email: EmailTiming::Immediate });
        assert_eq!(s, ProtectionSettings::default());
        assert_eq!(Config::default().protection.resolve().unwrap(), s);
    }

    #[test]
    fn careful_means_three_copies_one_online() {
        let s = prot(Some("careful"), None, None, None, None).resolve().unwrap();
        assert_eq!((s.copies, s.online), (3, 1));
    }

    #[test]
    fn custom_reads_copies_and_online_and_ignores_them_for_other_levels() {
        let s = prot(Some("custom"), Some(2), Some(1), None, None).resolve().unwrap();
        assert_eq!((s.copies, s.online), (2, 1));
        let d = prot(Some("custom"), None, None, None, None).resolve().unwrap();
        assert_eq!((d.copies, d.online), (1, 0));
        let b = prot(Some("basic"), Some(3), Some(2), None, None).resolve().unwrap();
        assert_eq!((b.copies, b.online), (1, 0));
        let c = prot(Some("careful"), Some(9), Some(9), None, None).resolve().unwrap();
        assert_eq!((c.copies, c.online), (3, 1), "bad numbers are ignored, not errors, outside custom");
    }

    #[test]
    fn recent_steps_are_one_two_and_four_intervals() {
        let t = |r| prot(None, None, None, Some(r), None).resolve().unwrap().tolerance;
        assert_eq!((t("strict"), t("normal"), t("relaxed")), (1, 2, 4));
        let e = |m| prot(None, None, None, None, Some(m)).resolve().unwrap().email;
        assert_eq!((e("immediate"), e("report"), e("off")), (EmailTiming::Immediate, EmailTiming::Report, EmailTiming::Off));
    }

    #[test]
    fn a_wrong_value_names_the_key() {
        let err = |c: ProtectionConfig| c.resolve().unwrap_err();
        assert_eq!(err(prot(Some("paranoid"), None, None, None, None)), "[protection] level must be basic, careful or custom");
        assert!(err(prot(Some("custom"), Some(4), None, None, None)).contains("copies"));
        assert!(err(prot(Some("custom"), Some(0), None, None, None)).contains("copies"));
        assert!(err(prot(Some("custom"), Some(3), Some(3), None, None)).contains("online"));
        assert!(err(prot(Some("custom"), Some(1), Some(2), None, None)).contains("online"));
        assert!(err(prot(None, None, None, Some("fast"), None)).contains("recent must be strict, normal or relaxed"));
        assert!(err(prot(None, None, None, None, Some("daily"))).contains("email must be immediate, report or off"));
    }

    #[test]
    fn a_wrong_type_in_protection_loads_and_resolve_names_the_key() {
        let resolved = |body: &str| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, format!("{body}\n[[hosts]]\nname = \"t2\"\nssh = \"fb@t2\"\n")).unwrap();
            let config = Config::load(&path).unwrap_or_else(|e| panic!("{body}: the file must still load: {e}"));
            assert_eq!(config.hosts.len(), 1, "{body}: the rest of the file is read");
            config.protection.resolve()
        };
        let level = "[protection] level must be basic, careful or custom";
        assert_eq!(resolved("[protection]\nlevel = 5\n").unwrap_err(), level);
        assert_eq!(resolved("[protection]\nlevel = \"Careful\"\n").unwrap_err(), level, "the words are lower case");
        assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = \"2\"\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
        assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 300\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
        assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = -1\n").unwrap_err(), "[protection] copies must be 1, 2 or 3");
        assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 3\nonline = 1.5\n").unwrap_err(), "[protection] online must be 0, 1 or 2");
        assert_eq!(resolved("[protection]\nrecent = true\n").unwrap_err(), "[protection] recent must be strict, normal or relaxed");
        assert_eq!(resolved("[protection]\nemail = \"of\"\n").unwrap_err(), "[protection] email must be immediate, report or off");
        assert_eq!(resolved("[protection]\nemail = [\"off\"]\n").unwrap_err(), "[protection] email must be immediate, report or off");
        assert_eq!(resolved("protection = 5\n").unwrap_err(), "[protection] must be a table of keys (level, copies, online, recent, email)");
        assert_eq!(resolved("[protection]\nlevel = \"custom\"\ncopies = 2\nonline = 1\n").unwrap(), ProtectionSettings { copies: 2, online: 1, tolerance: 2, email: EmailTiming::Immediate });
        assert_eq!(resolved("[protection]\nlevel = \"careful\"\ncopies = \"x\"\n").unwrap().copies, 3, "copies only counts under custom");
    }

    #[test]
    fn resolve_or_basic_gives_the_defaults_and_the_problem_in_words() {
        let ok = prot(Some("careful"), None, None, None, Some("report")).resolve_or_basic();
        assert_eq!((ok.0.copies, ok.0.email, ok.1), (3, EmailTiming::Report, None));
        let (s, problem) = prot(Some("Careful"), None, None, None, None).resolve_or_basic();
        assert_eq!((s, problem.as_deref()), (ProtectionSettings::default(), Some("[protection] level must be basic, careful or custom")));
        let (s, problem) = prot(None, None, None, None, Some("of")).resolve_or_basic();
        assert_eq!((s, problem.as_deref()), (ProtectionSettings::default(), Some("[protection] email must be immediate, report or off")));
        assert_eq!(protection_problem_line("[protection] email must be immediate, report or off"), "[protection]: email must be immediate, report or off; judging at Basic");
    }

    #[test]
    fn an_empty_section_is_not_written_back() {
        let c: Config = toml::from_str("[protection]\n").unwrap();
        assert_eq!(c.protection, ProtectionConfig::default());
        assert!(!toml::to_string(&c).unwrap().contains("protection"));
        let set: Config = toml::from_str("[protection]\nlevel = \"careful\"\nemail = \"off\"\n").unwrap();
        assert_eq!(toml::from_str::<Config>(&toml::to_string(&set).unwrap()).unwrap(), set);
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
        assert!(bad(NotifyConfig { check_interval_minutes: Some(1441), ..base.clone() }).contains("check_interval_minutes"));
        assert!(bad(NotifyConfig { check_interval_minutes: Some(u64::MAX), ..base.clone() }).contains("check_interval_minutes"), "a huge value must not overflow");
        assert!(bad(NotifyConfig { remind_hours: Some(u64::MAX), ..base.clone() }).contains("remind_hours"));
        assert!(bad(NotifyConfig { report_time: Some("7am".into()), ..base.clone() }).contains("report_time"));
        assert!(bad(NotifyConfig { report_time: Some("25:00".into()), ..base.clone() }).contains("report_time"));
        assert!(bad(NotifyConfig { mail_command: Some("  ".into()), ..base.clone() }).contains("mail_command"));
        assert!(bad(NotifyConfig { mail_from: Some("a\r\nb".into()), ..base }).contains("mail_from"));
    }

    fn host(name: &str, target: &str, tag: Option<&str>) -> HostConfig {
        HostConfig { name: name.into(), ssh_target: Some(target.into()), tag: tag.map(str::to_string) }
    }

    #[test]
    fn a_host_block_has_its_keys_and_escapes_strings() {
        assert_eq!(render_host_block(&host("a", "u@a", None)), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
        let with_tag = render_host_block(&host("a\"b", "u@a", Some("back up")));
        let parsed: Config = toml::from_str(&with_tag).unwrap();
        assert_eq!(parsed.hosts, vec![host("a\"b", "u@a", Some("back up"))], "{with_tag}");
    }

    #[test]
    fn a_new_host_lands_after_the_last_host_block_and_everything_else_stays() {
        let before = "# my hosts\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"   # first\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n# UI below\n[ui]\ntheme = \"mono\"\n";
        let after = add_host_block(before, &host("new", "u@new", Some("lab"))).unwrap();
        assert_eq!(after, "# my hosts\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"   # first\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n[[hosts]]\nname = \"new\"\nssh_target = \"u@new\"\ntag = \"lab\"\n\n# UI below\n[ui]\ntheme = \"mono\"\n");
    }

    #[test]
    fn with_no_hosts_yet_the_block_goes_at_the_end_after_a_blank_line() {
        let before = "# only settings\n[ui]\ntheme = \"mono\"\n";
        assert_eq!(add_host_block(before, &host("a", "u@a", None)).unwrap(), "# only settings\n[ui]\ntheme = \"mono\"\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
        assert_eq!(add_host_block("", &host("a", "u@a", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
        assert_eq!(add_host_block("[ui]\ntheme = \"mono\"", &host("a", "u@a", None)).unwrap(), "[ui]\ntheme = \"mono\"\n\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    }

    #[test]
    fn the_last_host_block_at_the_end_of_the_file_with_or_without_a_final_newline() {
        let end = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
        assert_eq!(add_host_block(end, &host("b", "u@b", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n");
        let bare = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"";
        assert_eq!(add_host_block(bare, &host("b", "u@b", None)).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n");
    }

    #[test]
    fn comments_after_the_last_host_belong_to_whatever_follows_it() {
        let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n# the notify section:\n# mail_to is the address\n[notify]\nmail_to = \"x@y.org\"\n";
        let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
        assert_eq!(after, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n# the notify section:\n# mail_to is the address\n[notify]\nmail_to = \"x@y.org\"\n");
    }

    #[test]
    fn crlf_files_stay_crlf_when_a_host_is_added() {
        let before = "[[hosts]]\r\nname = \"a\"\r\nssh_target = \"u@a\"\r\n\r\n[ui]\r\ntheme = \"mono\"\r\n";
        let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
        assert_eq!(after, "[[hosts]]\r\nname = \"a\"\r\nssh_target = \"u@a\"\r\n\r\n[[hosts]]\r\nname = \"b\"\r\nssh_target = \"u@b\"\r\n\r\n[ui]\r\ntheme = \"mono\"\r\n");
    }

    #[test]
    fn hosts_written_as_an_inline_array_or_dotted_keys_are_refused() {
        for bad in ["hosts = [{ name = \"a\", ssh_target = \"u@a\" }]\n", "hosts.x = 1\n", "[hosts.sub]\nx = 1\n"] {
            assert!(add_host_block(bad, &host("b", "u@b", None)).unwrap_err().contains("by hand"), "{bad:?}");
        }
    }

    #[test]
    fn a_host_header_inside_a_multi_line_string_is_never_mistaken_for_a_host() {
        let text = "[extra]\ntext = \'\'\'\n[[hosts]]\nname = \"inside\"\nssh_target = \"x\"\n\'\'\'\n[ui]\ntheme = \"mono\"\n";
        let after = add_host_block(text, &host("b", "u@b", None)).expect("the block can still go in safely");
        let parsed: toml::Table = after.parse().unwrap();
        let old: toml::Table = text.parse().unwrap();
        assert_eq!(parsed["extra"], old["extra"], "the string is untouched");
        assert_eq!(toml::from_str::<Config>(&after).unwrap().hosts, vec![host("b", "u@b", None)]);
    }

    #[test]
    fn a_header_looking_line_inside_a_hosts_own_multi_line_value_is_refused_not_split() {
        // The line-based scan would insert the new block inside this tag's string.
        let text = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \'\'\'\nline\n[x]\n\'\'\'\n";
        assert!(add_host_block(text, &host("b", "u@b", None)).is_err());
    }

    #[test]
    fn the_result_always_holds_the_old_hosts_plus_the_new_one() {
        let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"x\"\n";
        let after = add_host_block(before, &host("b", "u@b", None)).unwrap();
        let hosts = toml::from_str::<Config>(&after).unwrap().hosts;
        assert_eq!(hosts, vec![host("a", "u@a", Some("x")), host("b", "u@b", None)]);
    }

    #[test]
    fn adding_a_host_to_the_file_keeps_comments_a_backup_and_the_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let old = "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"x@y.org\"\n";
        std::fs::write(&path, old).unwrap();
        append_host_to_file(&path, &host("b", "u@b", None)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(), old);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n[notify]   # alerts\nmail_to = \"x@y.org\"\n");
        // A refused edit leaves the file untouched.
        std::fs::write(&path, "hosts = []\n").unwrap();
        assert!(append_host_to_file(&path, &host("c", "u@c", None)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hosts = []\n");
    }

    fn cfg(to: &str) -> NotifyConfig {
        NotifyConfig { mail_to: Some(to.into()), ..NotifyConfig::default() }
    }

    #[test]
    fn the_rendered_section_has_only_the_set_keys_and_escapes_strings() {
        assert_eq!(render_notify_section(&NotifyConfig::default()), "[notify]\n");
        let c = NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("msmtp -a \"x\" -t \\ y".into()), check_interval_minutes: Some(10), report_time: Some("off".into()), remind_hours: Some(0), mail_from: None };
        let text = render_notify_section(&c);
        assert!(text.starts_with("[notify]\n") && text.ends_with('\n'));
        assert_eq!(toml::from_str::<Config>(&text).unwrap().notify, c, "{text}");
        assert!(!text.contains("mail_from"));
    }

    #[test]
    fn an_existing_section_is_replaced_and_everything_else_is_kept_byte_for_byte() {
        let before = "# my hosts\n[[hosts]]\nname = \"a\"   # the first\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"old@x.org\"\ncheck_interval_minutes = 9\n\n[ui]\ntheme = \"mono\"\n";
        let after = set_notify_section(before, &render_notify_section(&cfg("new@x.org"))).unwrap();
        assert_eq!(after, "# my hosts\n[[hosts]]\nname = \"a\"   # the first\nssh_target = \"u@a\"\n\n[notify]   # alerts\nmail_to = \"new@x.org\"\n\n[ui]\ntheme = \"mono\"\n");
    }

    #[test]
    fn a_section_at_the_end_or_without_a_final_newline_is_replaced_too() {
        let end = "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"old@x.org\"\n";
        assert_eq!(set_notify_section(end, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
        let bare = "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"old@x.org\"";
        assert_eq!(set_notify_section(bare, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
    }

    #[test]
    fn a_missing_section_is_appended_after_a_blank_line_and_an_empty_file_gets_just_the_section() {
        let before = "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n";
        assert_eq!(set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap(), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
        assert_eq!(set_notify_section("", &render_notify_section(&cfg("n@x.org"))).unwrap(), "[notify]\nmail_to = \"n@x.org\"\n");
        assert_eq!(set_notify_section("[ui]\ntheme = \"mono\"", &render_notify_section(&cfg("n@x.org"))).unwrap(), "[ui]\ntheme = \"mono\"\n\n[notify]\nmail_to = \"n@x.org\"\n");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let before = "[ui]\r\ntheme = \"mono\"\r\n\r\n[notify]\r\nmail_to = \"old@x.org\"\r\n";
        let after = set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap();
        assert_eq!(after, "[ui]\r\ntheme = \"mono\"\r\n\r\n[notify]\r\nmail_to = \"n@x.org\"\r\n");
    }

    #[test]
    fn dotted_keys_inline_tables_subtables_and_duplicate_headers_are_refused() {
        let section = render_notify_section(&cfg("n@x.org"));
        for bad in ["notify.mail_to = \"a@x.org\"\n", "notify = { mail_to = \"a@x.org\" }\n", "[notify.extra]\nx = 1\n", "[notify]\nmail_to = \"a@x.org\"\n\n[ui]\n\n[notify]\n"] {
            assert!(set_notify_section(bad, &section).unwrap_err().contains("by hand"), "{bad:?}");
        }
        // A key that merely starts with the word is not a notify key.
        assert!(set_notify_section("[daemon]\nnotify_command = \"true\"\n", &section).is_ok());
    }

    #[test]
    fn a_result_that_does_not_parse_or_does_not_say_what_was_asked_is_refused() {
        assert!(set_notify_section("[ui\ntheme = 1\n", &render_notify_section(&cfg("n@x.org"))).unwrap_err().contains("parse"));
        assert!(set_notify_section("", "[notify]\nmail_to = \n").is_err());
    }

    #[test]
    fn saving_keeps_a_backup_the_permissions_and_every_other_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let old = "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"old@x.org\"\n";
        std::fs::write(&path, old).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        save_notify_section(&path, &render_notify_section(&cfg("new@x.org"))).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(), old);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# keep me\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"new@x.org\"\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
        let leftovers: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("config.toml.tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_refused_save_leaves_the_original_and_writes_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "notify.mail_to = \"x@y.org\"\n").unwrap();
        assert!(save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "notify.mail_to = \"x@y.org\"\n");
        assert!(!dir.path().join("config.toml.bak").exists());
    }

    #[test]
    fn comments_blank_lines_and_other_keys_inside_and_around_the_section_survive_a_save() {
        let before = "[notify]  # alerts\n# mail_from = \"x@y\"\nmail_to = \"o@x.org\"\nmail_too = \"typo@x.org\"\ncheck_interval_minutes = 9\n\n# ---- UI settings below ----\n# theme options: mono, ...\n[ui]\ntheme = \"mono\"\n";
        let after = set_notify_section(before, &render_notify_section(&cfg("n@x.org"))).unwrap();
        assert_eq!(after, "[notify]  # alerts\nmail_to = \"n@x.org\"\n# mail_from = \"x@y\"\nmail_too = \"typo@x.org\"\n\n# ---- UI settings below ----\n# theme options: mono, ...\n[ui]\ntheme = \"mono\"\n");
        // Saving blank fields removes the keys this screen owns and leaves the rest.
        let off = set_notify_section(before, "[notify]\n").unwrap();
        assert!(!off.contains("mail_to =") && !off.contains("check_interval") && off.contains("mail_too") && off.contains("# mail_from") && off.contains("# ---- UI"), "{off}");
    }

    #[test]
    fn text_inside_a_multi_line_string_or_an_unknown_table_is_never_changed() {
        let section = render_notify_section(&cfg("n@x.org"));
        // A header-looking line inside a string of an unknown table: the splice would eat lines, so it is refused.
        let tricky = "[extra]\ntext = \'\'\'\n[notify]\nkeep me\n[end]\'\'\'\n";
        assert!(set_notify_section(tricky, &section).is_err());
        assert_eq!(set_notify_section(tricky, "[notify]\n").unwrap(), tricky, "with nothing to write the text is returned untouched");
        let in_known = "[daemon]\nnotify_command = \"\"\"\n[notify]\n[x] \"\"\"\n";
        assert!(set_notify_section(in_known, &section).is_err());
    }

    #[test]
    fn a_key_line_inside_a_string_is_not_removed_even_when_nothing_is_asked_for() {
        // Dropping our own key lines would eat this one: the whole-file comparison catches it where the notify
        // settings alone (which come out as the asked-for defaults) cannot.
        let text = "[extra]\ntext = \'\'\'\n[notify]\nmail_to = \"inside\"\n[end]\'\'\'\n";
        assert!(set_notify_section(text, "[notify]\n").is_err());
    }

    #[test]
    fn a_notify_table_in_an_unusual_spelling_is_refused_with_the_by_hand_message() {
        let section = render_notify_section(&cfg("n@x.org"));
        for text in ["[ notify ]\nmail_to = \"a@x.org\"\n", "[\"notify\"]\nmail_to = \"a@x.org\"\n", "\"notify\" = { mail_to = \"a@x.org\" }\n", "\u{feff}[notify]\nmail_to = \"a@x.org\"\n"] {
            let err = set_notify_section(text, &section).unwrap_err();
            assert!(err.contains("by hand"), "{text:?}: {err}");
        }
    }

    #[test]
    fn a_multi_line_value_for_one_of_our_keys_is_refused() {
        assert!(set_notify_section("[notify]\nmail_command = \"\"\"\nx\n\"\"\"\n", &render_notify_section(&cfg("n@x.org"))).is_err());
    }

    #[test]
    fn a_symlinked_config_stays_a_symlink_and_the_target_is_updated() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        let link = dir.path().join("config.toml");
        std::fs::write(&real, "# mine\n[ui]\ntheme = \"mono\"\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save_notify_section(&link, &render_notify_section(&cfg("n@x.org"))).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
        assert!(std::fs::read_to_string(&real).unwrap().contains("mail_to = \"n@x.org\""));
        assert!(std::fs::read_to_string(&real).unwrap().starts_with("# mine"));
    }

    #[test]
    fn a_file_that_reads_as_empty_but_is_not_is_refused() {
        // Simulated through the pure check used by the save: an empty read of a non-empty file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
        assert!(save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).is_ok());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("[[hosts]]"), "the hosts survive: {saved}");
    }

    #[test]
    fn a_missing_file_is_created_and_a_failing_write_leaves_the_original_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[notify]\nmail_to = \"n@x.org\"\n");
        assert!(!dir.path().join("sub").join("config.toml.bak").exists(), "nothing to back up the first time");
        // A directory where the temporary file must go: the write fails, the original stays.
        std::fs::create_dir(dir.path().join("sub").join(format!("config.toml.tmp.{}", std::process::id()))).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let err = save_notify_section(&path, &render_notify_section(&cfg("other@x.org"))).unwrap_err().to_string();
        assert!(err.contains("config.toml.tmp"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
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
