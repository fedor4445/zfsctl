//! What the Alerts screen shows: the saved `[notify]` settings, whether the monitor is on, when the last daily
//! report went out and the recent health events. All of it is read from files, so a reload is cheap.

use crate::config::{Config, NotifyConfig, NotifySettings};
use crate::daemon::health_text::{EventLog, HealthEvent};
use std::path::Path;

/// Events shown at once (the screen scrolls through the rest).
pub const EVENTS_SHOWN: usize = 15;

#[derive(Debug, Default, Clone)]
pub struct AlertsView {
    pub loaded: bool,
    /// Why the settings cannot be used: `config.toml` could not be read, or its `[notify]` is invalid.
    pub problem: Option<String>,
    /// `[notify]` as written in the file (what the edit form starts from, also when it is invalid).
    pub saved: NotifyConfig,
    /// `saved` validated; `None` when it is invalid or the file could not be read.
    pub settings: Option<NotifySettings>,
    pub hook: Option<String>,
    pub last_report: Option<String>,
    /// Newest first.
    pub events: Vec<HealthEvent>,
    pub scroll: usize,
}

const UNREADABLE: &str = "config.toml could not be read";

pub fn load(config_path: &Path, state_dir: &Path) -> AlertsView {
    let mut view = AlertsView { loaded: true, ..AlertsView::default() };
    let text = match std::fs::read_to_string(config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            view.problem = Some(format!("{UNREADABLE}: {e}"));
            String::new()
        }
    };
    match toml::from_str::<Config>(&text) {
        Ok(config) => {
            view.saved = config.notify.clone();
            view.hook = config.daemon.notify_command.clone();
            match config.notify.resolve() {
                Ok(settings) => view.settings = Some(settings),
                Err(problem) => view.problem = Some(problem),
            }
        }
        Err(e) => {
            let first = e.to_string().lines().next().unwrap_or("").to_string();
            view.problem = Some(format!("{UNREADABLE}: {first}"));
        }
    }
    view.last_report = std::fs::read_to_string(state_dir.join("notify.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("last_report").and_then(|d| d.as_str().map(str::to_string)));
    let mut events = EventLog::new(state_dir.join("health-events.jsonl")).read();
    events.reverse();
    view.events = events;
    view
}

impl AlertsView {
    /// Whether the daemon's monitor runs with these settings: something can receive its messages.
    pub fn monitor_on(&self) -> bool {
        self.settings.as_ref().is_some_and(NotifySettings::has_mail) || (self.hook.is_some() && self.settings.is_some())
    }

    pub fn monitor_text(&self) -> String {
        if let Some(s) = self.settings.as_ref().filter(|_| self.monitor_on()) {
            let minutes = s.check_interval.as_secs() / 60;
            let report = s.report_time.map_or("no daily report".to_string(), |t| format!("report at {}", t.format("%H:%M")));
            return format!("Monitor on (every {minutes} min, {report})");
        }
        match &self.problem {
            Some(p) if !p.starts_with(UNREADABLE) => format!("Monitor off — {p}"),
            _ => "Monitor off — set an email address to turn alerts on".to_string(),
        }
    }

    /// `(label, value, is_default)` for the settings block; values come from the file, so an invalid section
    /// still shows what is saved.
    pub fn setting_rows(&self) -> Vec<(String, String, bool)> {
        let c = &self.saved;
        let plural = |n: u64, unit: &str| if n == 1 { format!("{n} {unit}") } else { format!("{n} {unit}s") };
        let row = |label: &str, value: Option<String>, default: String| (label.to_string(), value.clone().unwrap_or(default), value.is_none());
        let hook = self.hook.as_ref().map_or("(not set)".to_string(), |h| h.clone());
        vec![
            row("Email to", c.mail_to.clone(), "(not set)".to_string()),
            row("Mail command", c.mail_command.clone(), crate::config::DEFAULT_MAIL_COMMAND.to_string()),
            row("Mail from", c.mail_from.clone(), crate::daemon::deliver::default_from()),
            row("Check every", c.check_interval_minutes.map(|n| plural(n, "minute")), "5 minutes".to_string()),
            row("Daily report", c.report_time.clone(), "07:00".to_string()),
            row("Remind every", c.remind_hours.map(|h| if h == 0 { "never".to_string() } else { plural(h, "hour") }), "6 hours".to_string()),
            ("Hook command".to_string(), format!("{hook}   (edit in config.toml)"), self.hook.is_none()),
        ]
    }
}

/// `10-10 14:02  fbsd16  alert  tank DEGRADED`, in the hub's local time.
pub fn event_line(e: &HealthEvent) -> String {
    format!("{}  {}  {}  {}", e.at.with_timezone(&chrono::Local).format("%m-%d %H:%M"), e.host, e.kind, e.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_notify_loads_as_all_defaults_and_the_monitor_is_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
        let v = load(&dir.path().join("config.toml"), dir.path());
        assert!(v.loaded && v.problem.is_none());
        assert!(!v.monitor_on());
        assert!(v.monitor_text().starts_with("Monitor off"), "{}", v.monitor_text());
        let rows = v.setting_rows();
        assert_eq!(rows[0], ("Email to".to_string(), "(not set)".to_string(), true));
        assert!(rows.iter().any(|(l, val, d)| l == "Mail command" && val == "msmtp -t" && *d));
        assert!(rows.iter().any(|(l, val, d)| l == "Check every" && val == "5 minutes" && *d));
        assert!(rows.iter().any(|(l, val, d)| l == "Daily report" && val == "07:00" && *d));
        assert!(rows.iter().any(|(l, val, d)| l == "Remind every" && val == "6 hours" && *d));
    }

    #[test]
    fn set_values_are_shown_as_set_and_the_hook_is_read_only_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[daemon]\nnotify_command = \"curl -s x\"\n\n[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 10\nreport_time = \"off\"\nremind_hours = 0\n").unwrap();
        let v = load(&dir.path().join("config.toml"), dir.path());
        assert!(v.monitor_on());
        assert_eq!(v.monitor_text(), "Monitor on (every 10 min, no daily report)");
        let rows = v.setting_rows();
        assert!(rows.iter().any(|(l, val, d)| l == "Email to" && val == "me@x.org" && !*d));
        assert!(rows.iter().any(|(l, val, d)| l == "Daily report" && val == "off" && !*d));
        assert!(rows.iter().any(|(l, val, d)| l == "Remind every" && val == "never" && !*d));
        assert!(rows.iter().any(|(l, val, _)| l == "Hook command" && val.starts_with("curl -s x") && val.contains("edit in config.toml")), "{rows:?}");
    }

    #[test]
    fn a_hook_alone_turns_the_monitor_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[daemon]\nnotify_command = \"true\"\n").unwrap();
        assert!(load(&dir.path().join("config.toml"), dir.path()).monitor_on());
    }

    #[test]
    fn an_unreadable_or_invalid_config_is_a_problem_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[notify\nbroken").unwrap();
        let v = load(&p, dir.path());
        assert!(v.loaded && v.problem.as_deref().unwrap().contains("could not be read"), "{:?}", v.problem);
        std::fs::write(&p, "[notify]\nmail_to = \"not an address\"\n").unwrap();
        let v = load(&p, dir.path());
        assert!(v.problem.as_deref().unwrap().contains("mail_to"), "{:?}", v.problem);
        assert!(v.settings.is_none() && !v.monitor_on());
        assert_eq!(v.saved.mail_to.as_deref(), Some("not an address"), "the form can still show what is saved");
        let missing = load(&dir.path().join("nope.toml"), dir.path());
        assert!(missing.problem.is_none(), "a missing file is just an empty config");
    }

    #[test]
    fn the_last_report_and_the_newest_events_come_from_the_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        std::fs::write(dir.path().join("notify.json"), "{\"last_report\":\"2026-10-10\"}").unwrap();
        let log = EventLog::new(dir.path().join("health-events.jsonl"));
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-10-10T10:00:00Z").unwrap().with_timezone(&chrono::Utc);
        for i in 0..20 {
            log.append(&HealthEvent { at: t0 + chrono::Duration::minutes(i), host: "a".into(), kind: "alert".into(), text: format!("n{i}") }).unwrap();
        }
        let v = load(&dir.path().join("config.toml"), dir.path());
        assert_eq!(v.last_report.as_deref(), Some("2026-10-10"));
        assert_eq!(v.events.len(), 20, "all kept; the screen shows EVENTS_SHOWN from the scroll position");
        assert_eq!(v.events[0].text, "n19", "newest first");
        let line = event_line(&v.events[0]);
        assert!(line.contains("  a  alert  n19"), "{line}");
    }

    #[test]
    fn with_no_state_files_there_is_no_report_and_no_events() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        let v = load(&dir.path().join("config.toml"), dir.path());
        assert_eq!((v.last_report, v.events.len()), (None, 0));
    }
}
