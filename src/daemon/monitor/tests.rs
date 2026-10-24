//! Tests of `monitor.rs`.
use super::*;
use crate::config::NotifyConfig;
use crate::exec::test_support::FakeRunner;
use crate::exec::CommandOutput;

struct FakeSink {
    sent: Vec<Message>,
    ok: bool,
}

impl FakeSink {
    fn new() -> FakeSink {
        FakeSink { sent: Vec::new(), ok: true }
    }
    fn failing() -> FakeSink {
        FakeSink { sent: Vec::new(), ok: false }
    }
}

impl Sink for FakeSink {
    fn send(&mut self, msg: &Message) -> bool {
        self.sent.push(msg.clone());
        self.ok
    }
}

fn t(min: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(min)
}

fn local(min: i64) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 11).unwrap().and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::minutes(min)
}

fn at_local(h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 11).unwrap().and_hms_opt(h, m, 0).unwrap()
}

fn settings_with_report(report: &str) -> NotifySettings {
    NotifyConfig { mail_to: Some("me@x.org".into()), report_time: Some(report.into()), ..NotifyConfig::default() }.resolve().unwrap()
}

fn monitor() -> Monitor {
    let dir = tempfile::tempdir().unwrap();
    Monitor::new(settings_with_report("off"), "hub".into(), dir.keep(), t(0))
}

fn host(name: &str) -> HostInfo {
    let runner = FakeRunner::new("").route("zpool", "list", "tank\t1000\t200\t800\t5\tONLINE\n").route("zpool", "status", "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ttank  ONLINE  0 0 0\n");
    HostInfo { name: name.into(), address: format!("{name}.example"), tag: String::new(), runner: Arc::new(runner) }
}

fn probed_ok(name: &str) -> Probed {
    Probed { name: name.into(), probe: Some(HostProbe::Reached { rtt_ms: 3, pools: vec![], kind: None, os: None, os_name: None }) }
}

fn probed_bad(name: &str) -> Probed {
    Probed { name: name.into(), probe: Some(HostProbe::Disconnected) }
}

fn job(id: &str, kind: &str, host: &str) -> Job {
    let mut j = Job::test_backup(id, "tank/x", "p", "/r", "/pw");
    j.kind = kind.into();
    j.host = host.into();
    j
}

fn run(job: &str, result: Option<RunResult>, msg: &str, at: DateTime<Utc>) -> Run {
    Run { job_id: job.into(), run_id: format!("{job}-{}", at.timestamp()), started_at: at, scheduled_for: None, finished_at: result.map(|_| at), result, message: msg.into(), catch_up: false, summary: None }
}

#[test]
fn two_bad_probes_alert_once_and_two_good_ones_recover() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(sink.sent[0].subject, "[zfsctl] a: unreachable");
    m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), 1, "no repeat");
    for i in 3..5 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len(), 2);
    assert!(sink.sent[1].subject.starts_with("[zfsctl] a: recovered after"), "{}", sink.sent[1].subject);
}

#[test]
fn a_probe_that_never_answered_is_a_warning_not_a_silent_ok() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[Probed { name: "a".into(), probe: None }], &[], &[], &mut sink);
    }
    assert!(sink.sent[0].subject.contains("no answer"), "{}", sink.sent[0].subject);
    assert_eq!(sink.sent[0].level, "warning");
}

#[test]
fn a_failed_job_on_the_host_makes_it_a_warning() {
    let mut m = monitor();
    let hosts = [host("a")];
    let jobs = vec![job("backup-a", "backup", "a")];
    let runs = vec![run("backup-a", Some(RunResult::Failed), "restic exited 3", t(-10))];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
    }
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(sink.sent[0].level, "warning");
}

#[test]
fn a_delivery_that_fails_is_retried_on_later_ticks_at_most_six_times_then_dropped() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    let first = sink.sent.len();
    assert_eq!(first, 1);
    for i in 2..7 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len() - first, 5, "six attempts in all: one at first, five retries, then dropped");
    assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: unreachable"));
}

#[test]
fn an_alert_that_was_given_up_on_is_raised_again_instead_of_leaving_a_phantom_open_alert() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    for i in 2..7 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    let dropped_at = sink.sent.len();
    assert_eq!(dropped_at, 6, "six attempts, then dropped");
    sink.ok = true;
    m.observe(t(40), local(40), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), dropped_at, "one bad probe after forgetting is not enough");
    m.observe(t(45), local(45), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), dropped_at + 1);
    assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable", "the same alert, sent anew");
}

#[test]
fn a_dropped_recovery_or_reminder_does_not_reopen_anything() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut ok = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut ok);
    }
    let mut failing = FakeSink::failing();
    for i in 2..4 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
    }
    for i in 4..12 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
    }
    let n = failing.sent.len();
    failing.ok = true;
    m.observe(t(70), local(70), &hosts, &[probed_ok("a")], &[], &[], &mut failing);
    assert_eq!(failing.sent.len(), n, "the recovery was given up on and nothing is re-sent for a healthy host");
}

#[test]
fn notify_json_is_written_through_a_rename_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(std::fs::read_to_string(dir.path().join("notify.json")).unwrap(), "{\"last_report\":\"2026-10-11\"}\n");
    assert!(!dir.path().join("notify.json.tmp").exists() && !dir.path().join("notify.json.json.tmp").exists());
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_retry_that_succeeds_is_not_sent_again() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    sink.ok = true;
    m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    let n = sink.sent.len();
    m.observe(t(15), local(15), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), n);
}

#[test]
fn the_report_is_due_once_per_local_day_from_report_time_on() {
    let at = NaiveTime::from_hms_opt(7, 0, 0);
    let day = |d: u32, h: u32, mi: u32| NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, mi, 0).unwrap();
    let last = NaiveDate::from_ymd_opt(2026, 10, 10);
    assert!(!Monitor::report_due(day(11, 6, 59), at, last), "not yet");
    assert!(Monitor::report_due(day(11, 7, 0), at, last));
    assert!(Monitor::report_due(day(11, 15, 0), at, last), "the daemon was down at 07:00: sent on return");
    assert!(!Monitor::report_due(day(11, 15, 0), at, NaiveDate::from_ymd_opt(2026, 10, 11)), "already today");
    assert!(Monitor::report_due(day(12, 7, 0), at, NaiveDate::from_ymd_opt(2026, 10, 11)));
    assert!(!Monitor::report_due(day(11, 9, 0), None, None), "off");
}

fn reports(s: &FakeSink) -> usize {
    s.sent.iter().filter(|m| m.kind == "health-report").count()
}

#[test]
fn the_report_is_sent_once_and_remembered_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(reports(&sink), 1);
    m.observe(t(5), at_local(7, 6), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(reports(&sink), 1, "not twice");
    let mut again = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(60));
    again.observe(t(60), at_local(8, 0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(reports(&sink), 1, "a restarted monitor reads notify.json");
}

#[test]
fn a_failed_report_is_retried_and_only_marked_sent_once_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert!(!dir.path().join("notify.json").exists());
    sink.ok = true;
    m.observe(t(5), at_local(7, 6), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert!(dir.path().join("notify.json").exists());
    assert_eq!(reports(&sink), 2);
}

#[test]
fn a_report_that_keeps_failing_is_given_up_for_the_day_without_being_recorded_as_sent() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    for i in 0..10 {
        m.observe(t(i * 5), at_local(7, u32::try_from(i).unwrap()), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(reports(&sink), 6, "six attempts, then it stops asking for today");
    assert!(!dir.path().join("notify.json").exists());
}

#[test]
fn alerts_and_a_start_are_written_to_the_event_log_for_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.path().to_path_buf(), t(0));
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    let events = EventLog::new(dir.path().join("health-events.jsonl")).read();
    assert!(events.iter().any(|e| e.kind == "daemon-started"), "{events:?}");
    assert!(events.iter().any(|e| e.kind == "alert" && e.host == "a"), "{events:?}");
}

#[test]
fn the_type_is_remembered_so_it_is_asked_for_only_once() {
    let mut m = monitor();
    let hosts = [host("a")];
    assert!(m.needs_kind("a"));
    let probed = [Probed { name: "a".into(), probe: Some(HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Metal), os: None, os_name: None }) }];
    m.observe(t(0), local(0), &hosts, &probed, &[], &[], &mut FakeSink::new());
    assert!(!m.needs_kind("a"));
}

#[test]
fn a_host_whose_probe_is_still_running_is_not_probed_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Slow(Arc<AtomicUsize>);
    impl CommandRunner for Slow {
        fn run(&self, _: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            if args.first() == Some(&"list") {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(900));
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let hosts = vec![HostInfo { name: "slow".into(), address: "x".into(), tag: String::new(), runner: Arc::new(Slow(Arc::clone(&calls))) }];
    let busy = Busy::default();
    for _ in 0..5 {
        let probed = probe_all(&hosts, &|_| false, Duration::from_millis(100), &busy);
        assert!(probed[0].probe.is_none());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one probe, not five stacked up");
    std::thread::sleep(Duration::from_millis(2500));
    assert!(busy.lock().unwrap().is_empty(), "the host is free again once its probe returned");
    probe_all(&hosts, &|_| false, Duration::from_millis(100), &busy);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn changing_the_settings_keeps_the_alert_state_the_retry_queue_and_does_not_log_a_new_start() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.path().to_path_buf(), t(0));
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    }
    let mut fixed = settings_with_report("off");
    fixed.remind = Some(Duration::from_secs(3600));
    m.set_settings(fixed);
    sink.ok = true;
    m.observe(t(10), local(10), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable", "the queued alert was retried after the change");
    let n = sink.sent.len();
    m.observe(t(15), local(15), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), n, "and the host is not alerted as if it were new");
    for i in 4..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert!(sink.sent.last().unwrap().subject.contains("recovered"), "the recovery still comes");
    let starts = EventLog::new(dir.path().join("health-events.jsonl")).read().iter().filter(|e| e.kind == "daemon-started").count();
    assert_eq!(starts, 1);
}

use crate::config::{EmailTiming, ProtectionSettings};
use crate::daemon::storage_check::{self, Integrity, SeenDataset, StorageFile, StorageResult};

/// Writes `storage.json` with each host's datasets, created `created_ago` before `t(0)` and checked at `t(0)`.
fn write_storage(dir: &std::path::Path, hosts: &[(&str, &[&str])], created_ago: chrono::Duration) {
    let mut file = StorageFile::default();
    for (h, names) in hosts {
        let seen = names.iter().map(|n| SeenDataset { name: (*n).into(), volume: false, created: Some((t(0) - created_ago).timestamp()) }).collect();
        file.hosts.insert((*h).into(), StorageResult { checked_at: t(0), datasets: vec![], seen, pools: vec![], integrity: Integrity::default(), locked: vec![], integrity_stale: false, zfs_version: None, error: None });
    }
    file.save(&dir.join(storage_check::FILE)).unwrap();
}

fn week() -> chrono::Duration {
    chrono::Duration::days(7)
}

fn guarding(email: EmailTiming) -> (Monitor, PathBuf) {
    let dir = tempfile::tempdir().unwrap().keep();
    let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.clone(), t(0));
    m.set_protection(ProtectionSettings { email, ..ProtectionSettings::default() });
    (m, dir)
}

/// A current snapshot job and a current replication to another host for each dataset.
fn protecting(host: &str, datasets: &[&str]) -> (Vec<Job>, Vec<Run>) {
    let mut jobs = Vec::new();
    for (i, d) in datasets.iter().enumerate() {
        let mut s = Job::test_snapshot(&format!("s-{host}-{i}"), d, "auto", 24);
        s.host = host.into();
        let mut r = Job::test_replicate(&format!("r-{host}-{i}"), d, "rep", "elsewhere", &format!("backup/{i}"));
        r.host = host.into();
        jobs.push(s);
        jobs.push(r);
    }
    let runs = jobs.iter().map(|j| run(&j.id, Some(RunResult::Ok), "", t(-10))).collect();
    (jobs, runs)
}

fn protection_mail(s: &FakeSink) -> Vec<&Message> {
    s.sent.iter().filter(|m| m.subject.contains("protected")).collect()
}

#[test]
fn six_new_unprotected_datasets_on_one_host_send_one_message() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    let names = ["tank/a", "tank/b", "tank/c", "tank/d", "tank/e", "tank/f"];
    write_storage(&dir, &[("a", &names)], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    m.observe(t(0), local(0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert!(protection_mail(&sink).is_empty(), "one pass is not enough, as for hosts");
    m.observe(t(5), local(5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 1, "{:?}", sink.sent);
    assert_eq!(mail[0].subject, "[zfsctl] a: 6 datasets not protected");
    assert_eq!((mail[0].kind.as_str(), mail[0].host.as_str(), mail[0].level.as_str()), ("health-alert", "a", "critical"));
    for n in names {
        assert_eq!(mail[0].body.lines().filter(|l| l.trim_start().starts_with(&format!("{n}:"))).count(), 1, "{n} in\n{}", mail[0].body);
    }
    assert!(mail[0].body.contains("tank/a: no snapshot job; no copy job"), "{}", mail[0].body);
    assert_eq!(sink.sent.len(), 1, "the healthy host itself sends nothing");
    let events = EventLog::new(dir.join("health-events.jsonl")).read();
    assert_eq!(events.iter().filter(|e| e.kind == "alert" && e.host == "a").count(), 1, "one event for the message, not six: {events:?}");
}

#[test]
fn os_data_is_not_mailed_but_stays_in_the_report_list() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["zroot/var/log", "zroot/tmp"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..4 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert!(protection_mail(&sink).is_empty(), "{:?}", sink.sent);
    assert_eq!(m.unprotected.len(), 2, "still judged, for the report");

    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/photos", "zroot/var/log"])], week());
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].subject, "[zfsctl] a: 1 dataset not protected");
    assert!(!mail[0].body.contains("zroot/var/log"), "{}", mail[0].body);
}

#[test]
fn protection_reminders_are_at_most_daily() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    assert_eq!(m.settings().remind, Some(Duration::from_hours(6)), "the host reminders stay at six hours");
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let at = |min: i64, m: &mut Monitor, sink: &mut FakeSink| m.observe(t(min), local(min % 1440), &hosts, &[probed_ok("a")], &[], &[], sink);
    at(0, &mut m, &mut sink);
    at(5, &mut m, &mut sink);
    assert_eq!(protection_mail(&sink).len(), 1);
    for h in [7, 13, 19, 23] {
        at(h * 60, &mut m, &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "no six-hourly reminder: {:?}", sink.sent);
    at(24 * 60 + 10, &mut m, &mut sink);
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 2);
    assert!(mail[1].subject.starts_with("[zfsctl] a: 1 dataset still not protected"), "{}", mail[1].subject);
    // A longer configured interval is kept.
    let mut s = settings_with_report("off");
    s.remind = Some(Duration::from_hours(48));
    m.set_settings(s);
    at(49 * 60, &mut m, &mut sink);
    assert_eq!(protection_mail(&sink).len(), 2, "48 h asked: no reminder after 24 h");
}

#[test]
fn a_dataset_in_its_grace_sends_nothing() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/new"])], chrono::Duration::hours(1));
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..4 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert!(sink.sent.is_empty(), "{:?}", sink.sent);
}

#[test]
fn recovery_is_one_message_per_host() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x", "tank/y"]), ("b", &["tank/p", "tank/q"])], week());
    let hosts = [host("a"), host("b")];
    let probed = [probed_ok("a"), probed_ok("b")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 2, "one alert per host");
    let (mut jobs, mut runs) = protecting("a", &["tank/x", "tank/y"]);
    let (more_jobs, more_runs) = protecting("b", &["tank/p", "tank/q"]);
    jobs.extend(more_jobs);
    runs.extend(more_runs);
    m.observe(t(10), local(10), &hosts, &probed, &jobs, &runs, &mut sink);
    assert_eq!(protection_mail(&sink).len(), 2, "one good pass does not recover");
    m.observe(t(15), local(15), &hosts, &probed, &jobs, &runs, &mut sink);
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 4, "{:?}", sink.sent);
    let mut subjects: Vec<&str> = mail[2..].iter().map(|m| m.subject.as_str()).collect();
    subjects.sort_unstable();
    assert_eq!(subjects, ["[zfsctl] a: 2 datasets protected again", "[zfsctl] b: 2 datasets protected again"]);
    assert!(mail[2..].iter().all(|m| m.kind == "health-recovered" && m.level == "ok"));
    m.observe(t(20), local(20), &hosts, &probed, &jobs, &runs, &mut sink);
    assert_eq!(protection_mail(&sink).len(), 4, "once");
}

#[test]
fn a_second_pass_sends_no_duplicate() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 1, "{:?}", sink.sent);
    assert_eq!(mail[0].subject, "[zfsctl] a: 1 dataset not protected");
}

#[test]
fn a_failed_delivery_is_retried_then_alerts_again() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for i in 0..7 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len(), 6, "six attempts, then dropped: {:?}", sink.sent);
    assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: 2 datasets not protected"));
    sink.ok = true;
    m.observe(t(40), local(40), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), 6, "forgotten: one pass is not enough again");
    m.observe(t(45), local(45), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), 7);
    assert_eq!(sink.sent[6].subject, "[zfsctl] a: 2 datasets not protected", "the same alert, sent anew");
    // Giving up on the protection alert did not touch the host's own alert state: a bad host still alerts after two probes.
    m.observe(t(50), local(50), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    m.observe(t(55), local(55), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.last().unwrap().subject, "[zfsctl] a: unreachable");
}

#[test]
fn an_unreachable_host_sends_no_protection_mail() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"]), ("b", &["tank/y"])], week());
    let hosts = [host("a"), host("b")];
    let mut sink = FakeSink::new();
    for i in 0..3 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a"), Probed { name: "b".into(), probe: None }], &[], &[], &mut sink);
    }
    assert!(protection_mail(&sink).is_empty(), "{:?}", sink.sent);
    assert!(sink.sent.iter().any(|s| s.subject == "[zfsctl] a: unreachable"));

    // Alerted while reachable; while it is unreachable nothing changes, not even when its jobs are fixed.
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1);
    let (jobs, runs) = protecting("a", &["tank/x"]);
    for i in 2..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_bad("a")], &jobs, &runs, &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "no recovery while the host cannot be seen");
    for i in 6..8 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
    }
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 2, "{:?}", sink.sent);
    assert_eq!(mail[1].subject, "[zfsctl] a: 1 dataset protected again", "the open alert was kept and recovers");
}

#[test]
fn a_destroyed_dataset_recovers_silently() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1);
    write_storage(&dir, &[("a", &[])], week());
    for i in 2..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "no 'protected again' for datasets that are gone: {:?}", sink.sent);
    // Forgotten: a dataset of the same name made later is new.
    write_storage(&dir, &[("a", &["tank/x"])], week());
    for i in 6..8 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    let mail = protection_mail(&sink);
    assert_eq!(mail.len(), 2);
    assert_eq!(mail[1].subject, "[zfsctl] a: 1 dataset not protected");
}

#[test]
fn judge_reachable_says_which_hosts_were_not_checked_and_why() {
    let dir = tempfile::tempdir().unwrap();
    write_storage(dir.path(), &[("a", &["tank/x"]), ("down", &["tank/y"]), ("old", &["tank/z"])], week());
    let mut file = StorageFile::load(&dir.path().join(storage_check::FILE));
    file.hosts.get_mut("old").unwrap().checked_at = t(0) - chrono::Duration::days(3);
    let hosts = [host("a"), host("down"), host("new"), host("old")];
    let probed = [probed_ok("a"), probed_bad("down"), probed_ok("new"), probed_ok("old")];
    let rows = build_rows(&hosts, &probed, &HashMap::new(), &[], &[]);
    let judged = judge_reachable(&file, &probed, &rows, &[], &[], &ProtectionSettings::default(), t(0));
    assert_eq!(judged.found.iter().map(|u| (u.host.as_str(), u.dataset.as_str())).collect::<Vec<_>>(), [("a", "tank/x")]);
    assert_eq!(judged.known, HashSet::from(["a"]));
    assert_eq!(judged.unchecked, ["down (unreachable)", "new (no dataset list yet)", "old (no dataset list for 3d)"]);
    let section = judged.section(&|_| None, t(0));
    assert_eq!((section.judged, section.unchecked.len(), section.lines.len()), (1, 3, 1));
    // A probe that never answered is unreachable too.
    let probed = [probed_ok("a"), Probed { name: "down".into(), probe: None }, probed_ok("new"), probed_ok("old")];
    let rows = build_rows(&hosts, &probed, &HashMap::new(), &[], &[]);
    assert_eq!(judge_reachable(&file, &probed, &rows, &[], &[], &ProtectionSettings::default(), t(0)).unchecked[0], "down (unreachable)");
}

#[test]
fn the_daily_report_names_the_hosts_it_could_not_check() {
    let dir = tempfile::tempdir().unwrap().keep();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.clone(), t(0));
    m.set_protection(ProtectionSettings { email: EmailTiming::Report, ..ProtectionSettings::default() });
    let (jobs, runs) = protecting("a", &["tank/x"]);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a"), host("b")];
    let mut sink = FakeSink::new();
    m.observe(t(0), at_local(7, 1), &hosts, &[probed_ok("a"), probed_bad("b")], &jobs, &runs, &mut sink);
    let report = sink.sent.iter().find(|s| s.kind == "health-report").unwrap();
    assert!(report.body.contains("UNPROTECTED DATA\n  All datasets on 1 host are protected; not checked: b (unreachable)\n"), "{}", report.body);
    assert!(!report.body.contains("All datasets are protected."), "{}", report.body);
}

#[test]
fn a_dataset_destroyed_and_made_again_is_new_not_protected_again() {
    // Made again inside one storage-check gap: still listed, but in its grace.
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1);
    write_storage(&dir, &[("a", &["tank/x"])], chrono::Duration::hours(1));
    for i in 2..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "a new dataset in its grace is not 'protected again': {:?}", sink.sent);
    assert!(m.protection.bad_since("a\ttank/x").is_none() && !m.protection_keys.contains("a\ttank/x"), "forgotten");

    // Made again after the alert was sent (the alert's start) and past its grace, now protected: still new, no recovery mail.
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1);
    write_storage(&dir, &[("a", &["tank/x"])], chrono::Duration::minutes(-10));
    let later = t(26 * 60);
    let (jobs, _) = protecting("a", &["tank/x"]);
    let runs: Vec<Run> = jobs.iter().map(|j| run(&j.id, Some(RunResult::Ok), "", later - chrono::Duration::minutes(10))).collect();
    for i in 0..3 {
        m.observe(later + chrono::Duration::minutes(i * 5), local(26 * 60 + i * 5), &hosts, &[probed_ok("a")], &jobs, &runs, &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "{:?}", sink.sent);
}

#[test]
fn the_report_says_how_long_a_dataset_has_been_unprotected_in_report_timing_too() {
    let dir = tempfile::tempdir().unwrap().keep();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.clone(), t(0));
    m.set_protection(ProtectionSettings { email: EmailTiming::Report, ..ProtectionSettings::default() });
    write_storage(&dir, &[("a", &["tank/x", "zroot/var/log"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    m.observe(t(0), at_local(6, 0), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    m.observe(t(90), at_local(7, 30), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    let report = sink.sent.iter().find(|s| s.kind == "health-report").unwrap();
    assert!(report.body.contains("  a:tank/x (your data): no snapshot job; no copy job (for 1h 30m)\n"), "{}", report.body);
    assert!(report.body.contains("  a:zroot/var/log (OS data): no snapshot job; no copy job (for 1h 30m)\n"), "{}", report.body);
    // Protected or gone: forgotten, so it starts again from zero.
    write_storage(&dir, &[("a", &["tank/x"])], week());
    m.observe(t(100), at_local(7, 40), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert!(!m.first_seen.contains_key("a\tzroot/var/log") && m.first_seen.contains_key("a\ttank/x"));
    // An unreachable host is not a verdict: its datasets keep their first-seen time.
    m.observe(t(105), at_local(7, 45), &hosts, &[probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(m.first_seen.get("a\ttank/x"), Some(&t(0)));
    m.set_protection(ProtectionSettings { email: EmailTiming::Off, ..ProtectionSettings::default() });
    m.observe(t(110), at_local(7, 50), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    assert!(m.first_seen.is_empty(), "off keeps nothing");
}

#[test]
fn report_and_off_send_no_alerts() {
    for email in [EmailTiming::Report, EmailTiming::Off] {
        let (mut m, dir) = guarding(email);
        write_storage(&dir, &[("a", &["tank/x", "tank/y"])], week());
        let hosts = [host("a")];
        let mut sink = FakeSink::new();
        for i in 0..4 {
            m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
        }
        assert!(sink.sent.is_empty(), "{email:?}: {:?}", sink.sent);
        let listed: Vec<&str> = m.unprotected.iter().map(|u| u.dataset.as_str()).collect();
        if email == EmailTiming::Report {
            assert_eq!(listed, ["tank/x", "tank/y"], "kept for the daily report");
        } else {
            assert!(listed.is_empty(), "off: not even judged");
        }
    }
}

#[test]
fn switching_to_immediate_sends_one_message_per_host_for_the_current_list() {
    let (mut m, dir) = guarding(EmailTiming::Report);
    write_storage(&dir, &[("a", &["tank/1", "tank/2", "tank/3"]), ("b", &["tank/4", "tank/5", "tank/6"])], week());
    let hosts = [host("a"), host("b")];
    let probed = [probed_ok("a"), probed_ok("b")];
    let mut sink = FakeSink::new();
    for i in 0..3 {
        m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
    }
    assert!(sink.sent.is_empty());
    m.set_protection(ProtectionSettings { email: EmailTiming::Immediate, ..ProtectionSettings::default() });
    for i in 3..6 {
        m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
    }
    let mut subjects: Vec<&str> = protection_mail(&sink).iter().map(|m| m.subject.as_str()).collect();
    subjects.sort_unstable();
    assert_eq!(subjects, ["[zfsctl] a: 3 datasets not protected", "[zfsctl] b: 3 datasets not protected"]);

    // Off and on again: the old alerts are cleared, so it is again one message per host, and no recovery for the switch.
    m.set_protection(ProtectionSettings { email: EmailTiming::Off, ..ProtectionSettings::default() });
    m.observe(t(30), local(30), &hosts, &probed, &[], &[], &mut sink);
    assert_eq!(protection_mail(&sink).len(), 2, "no recovery mail for turning it off");
    m.set_protection(ProtectionSettings { email: EmailTiming::Immediate, ..ProtectionSettings::default() });
    for i in 7..9 {
        m.observe(t(i * 5), local(i * 5), &hosts, &probed, &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 4, "{:?}", sink.sent);
}

#[test]
fn report_wait_says_how_long_until_the_report_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    assert_eq!(m.report_wait(at_local(6, 0)), Some(Duration::from_secs(3600)));
    assert_eq!(m.report_wait(at_local(7, 0)), Some(Duration::ZERO), "due now");
    assert_eq!(m.report_wait(at_local(15, 0)), Some(Duration::ZERO), "overdue: sent as soon as a tick runs");
    m.mark_report_sent(at_local(7, 1).date(), false);
    assert_eq!(m.report_wait(at_local(8, 0)), None, "sent today");
    let next_day = at_local(0, 5) + chrono::Duration::days(1);
    assert_eq!(m.report_wait(next_day), Some(Duration::from_secs(6 * 3600 + 55 * 60)), "after midnight the wait is for the next 07:00");
    assert_eq!(monitor().report_wait(at_local(6, 0)), None, "no report configured");
}

#[test]
fn probe_all_collects_every_host_and_a_hung_one_comes_back_as_no_answer() {
    struct Hung;
    impl CommandRunner for Hung {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
            std::thread::sleep(Duration::from_secs(3));
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
    }
    let hosts = vec![HostInfo { name: "slow".into(), address: "x".into(), tag: String::new(), runner: Arc::new(Hung) }, host("fast")];
    let t0 = Instant::now();
    let probed = probe_all(&hosts, &|_| false, Duration::from_millis(500), &Busy::default());
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(probed.len(), 2);
    assert!(probed.iter().find(|p| p.name == "slow").unwrap().probe.is_none());
    assert!(matches!(probed.iter().find(|p| p.name == "fast").unwrap().probe, Some(HostProbe::Reached { .. })));
}

#[test]
fn a_big_fleet_is_probed_a_few_hosts_at_a_time_and_every_host_still_answers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(Arc<AtomicUsize>, Arc<AtomicUsize>);
    impl CommandRunner for Counting {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
            let now = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            self.1.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.0.fetch_sub(1, Ordering::SeqCst);
            Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(c, a)
        }
    }
    let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let hosts: Vec<HostInfo> = (0..60).map(|i| HostInfo { name: format!("h{i}"), address: "x".into(), tag: String::new(), runner: Arc::new(Counting(Arc::clone(&now), Arc::clone(&most))) }).collect();
    let probed = probe_all(&hosts, &|_| false, Duration::from_secs(20), &Busy::default());
    assert_eq!(probed.len(), 60);
    assert!(probed.iter().all(|p| p.probe.is_some()), "every host answered: waiting in the queue is not a failure");
    let most = most.load(Ordering::SeqCst);
    assert!(most <= PROBE_PARALLEL, "{most} probes at once, the limit is {PROBE_PARALLEL}");
    assert!(most >= 4, "but they do run side by side ({most})");
}

/// The settings of a `config.toml` whose hosts are `a` (tag `db`), `b` (tag `web`), `c` (no tag).
fn group_settings(toml: &str) -> ProtectionSettings {
    let text = format!("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\ntag = \"db\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\ntag = \"web\"\n\n[[hosts]]\nname = \"c\"\nssh_target = \"u@c\"\n\n{toml}");
    let (settings, problems) = toml::from_str::<crate::config::Config>(&text).unwrap().protection_settings();
    assert!(problems.is_empty(), "{problems:?}");
    settings
}

#[test]
fn a_group_with_email_off_is_neither_mailed_nor_reported_and_says_so_while_the_other_hosts_still_are() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    m.set_protection(group_settings("[groups.db.protection]\nemail = \"off\"\n"));
    write_storage(&dir, &[("a", &["tank/x"]), ("c", &["tank/y"])], week());
    let hosts = [host("a"), host("c")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a"), probed_ok("c")], &[], &[], &mut sink);
    }
    let mail = protection_mail(&sink);
    assert_eq!(mail.iter().map(|x| x.host.as_str()).collect::<Vec<_>>(), ["c"], "{:?}", sink.sent);
    assert_eq!(m.unprotected.iter().map(|u| u.host.as_str()).collect::<Vec<_>>(), ["c"], "the quiet group is not in the report list");
    assert!(m.unchecked.iter().any(|u| u == "a (email off)"), "and the report says it was not reported: {:?}", m.unchecked);
}

#[test]
fn a_group_with_report_timing_is_in_the_report_list_but_not_mailed_at_once() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    m.set_protection(group_settings("[groups.web.protection]\nemail = \"report\"\n"));
    write_storage(&dir, &[("b", &["tank/x"]), ("c", &["tank/y"])], week());
    let hosts = [host("b"), host("c")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("b"), probed_ok("c")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).iter().map(|x| x.host.as_str()).collect::<Vec<_>>(), ["c"], "{:?}", sink.sent);
    assert_eq!(m.unprotected.len(), 2, "both are in the list the daily report prints");
}

#[test]
fn moving_a_host_to_a_quiet_group_drops_its_alerts_without_a_recovery_mail() {
    let (mut m, dir) = guarding(EmailTiming::Immediate);
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for i in 0..2 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1);
    m.set_protection(group_settings("[groups.db.protection]\nemail = \"report\"\n"));
    for i in 2..5 {
        m.observe(t(i * 5), local(i * 5), &hosts, &[probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(protection_mail(&sink).len(), 1, "no recovery and no new alert: {:?}", sink.sent.iter().map(|x| &x.subject).collect::<Vec<_>>());
    assert_eq!(m.unprotected.len(), 1, "it is still listed for the report");
}

#[test]
fn when_every_host_is_quiet_nothing_is_judged_for_mail_at_all() {
    let (mut m, dir) = guarding(EmailTiming::Off);
    m.set_protection(ProtectionSettings { email: EmailTiming::Off, ..group_settings("[groups.db.protection]\nemail = \"off\"\n") });
    write_storage(&dir, &[("a", &["tank/x"])], week());
    let mut sink = FakeSink::new();
    m.observe(t(0), local(0), &[host("a")], &[probed_ok("a")], &[], &[], &mut sink);
    assert!(m.unprotected.is_empty() && protection_mail(&sink).is_empty());
}
