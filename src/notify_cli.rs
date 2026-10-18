//! `zfsctl notify-test` and `zfsctl report`: let the operator check the health notifications without waiting.

use crate::config::{Config, NotifySettings};
use crate::daemon::deliver::{self, Message};
use crate::daemon::health_text::{report_message, EventLog};
use crate::daemon::history::History;
use crate::daemon::monitor::{build_rows, probe_all, HostInfo, Probed, PROBE_DEADLINE};
use crate::error::Result;
use crate::hosts::HostInventory;
use crate::zfs::host_health::{HostKind, HostProbe};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// Sends a short test message on every configured channel and says what happened to each. Exit code 0 only if
/// every configured channel took it; 1 if one failed or none is configured.
pub fn notify_test(settings: &NotifySettings, hook: Option<&str>, now: DateTime<Utc>) -> (String, i32) {
    let msg = Message {
        kind: "health-test".to_string(),
        host: deliver::default_from().trim_start_matches("zfsctl@").to_string(),
        level: "ok".to_string(),
        subject: "[zfsctl] test message".to_string(),
        body: "This is a test message from `zfsctl notify-test`.\nIf you can read it, health alerts and the daily report can reach you.\n".to_string(),
    };
    let results = deliver::send(settings, hook, &msg, now, deliver::TIMEOUT);
    if results.is_empty() {
        return ("no channel is configured: set [notify] mail_to or [daemon] notify_command in config.toml\n".to_string(), 1);
    }
    let mut text = String::new();
    for d in &results {
        let name = match d.channel {
            deliver::Channel::Mail => "mail",
            deliver::Channel::Hook => "hook",
        };
        match &d.result {
            Ok(()) => {
                let _ = writeln!(text, "{name}: ok");
            }
            Err(e) => {
                let _ = writeln!(text, "{name}: failed: {e}");
            }
        }
    }
    let code = i32::from(results.iter().any(|d| d.result.is_err()));
    (text, code)
}

pub fn notify_test_command(config: &Config) -> Result<i32> {
    let settings = config.notify.resolve().map_err(crate::error::AppError::Config)?;
    let (text, code) = notify_test(&settings, config.daemon.notify_command.as_deref(), Utc::now());
    print!("{text}");
    Ok(code)
}

/// The report as it would be sent now, as `Subject: ...` then a blank line then the body. `probe` is how the hosts
/// are probed (the real one by default, a fake in tests); nothing is sent.
pub fn report_text_with(config: &Config, state_dir: &Path, now: DateTime<Utc>, probe: &dyn Fn(&[HostInfo]) -> Vec<Probed>) -> String {
    let inventory = HostInventory::from_config(config);
    let hosts: Vec<HostInfo> = inventory
        .hosts
        .iter()
        .map(|h| HostInfo { name: h.name.clone(), address: crate::ui::state::address_of(h.ssh_target.as_deref()), tag: h.tag.clone().unwrap_or_default(), runner: std::sync::Arc::clone(&h.runner) })
        .collect();
    let probed = probe(&hosts);
    let kinds: HashMap<String, HostKind> = probed
        .iter()
        .filter_map(|p| match &p.probe {
            Some(HostProbe::Reached { kind: Some(k), .. }) => Some((p.name.clone(), k.clone())),
            _ => None,
        })
        .collect();
    let jobs = crate::jobs::load(&state_dir.join("jobs.toml")).unwrap_or_default();
    let runs = crate::daemon::history::runs(&History::new(state_dir.join("history.jsonl")).read());
    let rows = build_rows(&hosts, &probed, &kinds, &jobs, &runs);
    let events = EventLog::new(state_dir.join("health-events.jsonl")).read();
    let hub = deliver::default_from().trim_start_matches("zfsctl@").to_string();
    let local = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let msg = report_message(now, &local, &hub, &rows, &|_| None, now - chrono::Duration::hours(24), &runs, &jobs, &events, &crate::daemon::health_text::check_lines(state_dir));
    format!("Subject: {}\n\n{}", msg.subject, msg.body)
}

pub fn report_command(config: &Config, state_dir: &Path) -> Result<i32> {
    config.notify.resolve().map_err(crate::error::AppError::Config)?;
    let text = report_text_with(config, state_dir, Utc::now(), &|hosts| probe_all(hosts, &|_| true, PROBE_DEADLINE, &crate::daemon::monitor::Busy::default()));
    print!("{text}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotifyConfig;

    #[test]
    fn notify_test_reports_each_channel_and_the_exit_code() {
        let s = NotifyConfig { mail_to: Some("me@x.org".into()), mail_command: Some("cat > /dev/null".into()), ..NotifyConfig::default() }.resolve().unwrap();
        let (text, code) = notify_test(&s, Some("true"), Utc::now());
        assert_eq!(code, 0);
        assert!(text.contains("mail: ok") && text.contains("hook: ok"), "{text}");
        let s = NotifyConfig { mail_to: Some("me@x.org".into()), mail_command: Some("exit 78".into()), ..NotifyConfig::default() }.resolve().unwrap();
        let (text, code) = notify_test(&s, None, Utc::now());
        assert_eq!(code, 1);
        assert!(text.contains("mail: failed: exited with status 78"), "{text}");
        let (text, code) = notify_test(&NotifySettings::default(), None, Utc::now());
        assert_eq!(code, 1);
        assert!(text.contains("no channel is configured"), "{text}");
    }

    #[test]
    fn one_failing_channel_fails_the_test_even_if_the_other_works() {
        let s = NotifyConfig { mail_to: Some("me@x.org".into()), mail_command: Some("exit 1".into()), ..NotifyConfig::default() }.resolve().unwrap();
        let (text, code) = notify_test(&s, Some("true"), Utc::now());
        assert_eq!(code, 1);
        assert!(text.contains("mail: failed") && text.contains("hook: ok"), "{text}");
    }

    fn fake_probe(hosts: &[HostInfo]) -> Vec<Probed> {
        hosts.iter().map(|h| Probed { name: h.name.clone(), probe: Some(HostProbe::Reached { rtt_ms: 2, pools: vec![], kind: Some(HostKind::Metal), os: None, os_name: None }) }).collect()
    }

    #[test]
    fn the_report_command_builds_the_report_without_sending_anything() {
        let dir = tempfile::tempdir().unwrap();
        let text = report_text_with(&Config::default(), dir.path(), Utc::now(), &fake_probe);
        assert!(text.starts_with("Subject: [zfsctl] daily report: all 1 hosts ok"), "{text}");
        assert!(text.contains("\n\nzfsctl daily report\n") && text.contains("local") && text.contains("metal"), "{text}");
        assert!(!dir.path().join("notify.json").exists(), "a preview records nothing");
    }

    #[test]
    fn the_report_preview_lists_a_failed_job_from_the_state_directory() {
        use crate::daemon::history::{Event, EventKind, RunResult};
        let dir = tempfile::tempdir().unwrap();
        let mut job = crate::jobs::Job::test_backup("backup-x", "tank/x", "p", "/r", "/pw");
        job.host = "local".into();
        crate::jobs::save(&dir.path().join("jobs.toml"), &[job]).unwrap();
        let now = Utc::now();
        History::new(dir.path().join("history.jsonl"))
            .append(&Event { job_id: "backup-x".into(), run_id: "r1".into(), event: EventKind::Finished, at: now - chrono::Duration::hours(1), scheduled_for: None, result: Some(RunResult::Failed), message: "restic exited 3".into(), catch_up: false, summary: None })
            .unwrap();
        let text = report_text_with(&Config::default(), dir.path(), now, &fake_probe);
        assert!(text.contains("failed: backup-x on local") && text.contains("restic exited 3"), "{text}");
        assert!(text.contains("warning"), "a failed job makes the host a warning: {text}");
    }
}
