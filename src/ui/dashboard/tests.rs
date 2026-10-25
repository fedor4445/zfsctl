//! Tests of `dashboard.rs`.
use super::*;
use crate::config::Thresholds;
use crate::zfs::host_health::{assess, HostProbe, PoolProbe, ScanSummary};
use crate::zfs::host_resources::Resources;
use crate::zfs::pool::PoolHealth;
use chrono::{DateTime, Duration, Utc};

const GIB: u64 = 1 << 30;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z").unwrap().with_timezone(&Utc)
}

/// `zpool status`'s own wording for a scrub that finished `days` ago (host time = UTC here).
fn when(days: i64) -> String {
    (now() - Duration::days(days)).format("%a %b %e %H:%M:%S %Y").to_string()
}

fn pool(name: &str, capacity: u8, errors: u64, last_scrub: Option<String>) -> PoolProbe {
    let scan = Some(last_scrub.as_ref().map_or(ScanSummary::Never, |w| ScanSummary::Finished { when: w.clone(), errors: 0 }));
    PoolProbe { name: name.into(), health: PoolHealth::Online, capacity_percent: capacity, frag_percent: None, errors: Some(errors), scan, last_scrub }
}

fn reached(pools: Vec<PoolProbe>) -> HostProbe {
    HostProbe::Reached { rtt_ms: 3, pools, kind: None, os: None, os_name: None }
}

fn reading(free_gib: u64, arc_gib: u64, busy: u64, idle: u64) -> Resources {
    Resources {
        cores: 4,
        physical: 20 * GIB,
        free: free_gib * GIB,
        inactive: 0,
        arc: arc_gib * GIB,
        load: Some([0.05, 0.1, 0.1]),
        cpu_ticks: Some(crate::zfs::host::CpuTimes { user: busy, nice: 0, sys: 0, intr: 0, idle }),
        swap: None,
        cache: None,
    }
}

/// A host whose probe says `probe`, with the given resource readings (newest, previous), judged with the defaults.
fn row(probe: &HostProbe, now_r: Option<&Resources>, before: Option<&Resources>, t: &Thresholds) -> HostRow {
    let input = HostInput { name: "h", assessment: assess(Some(probe), false), probe: Some(probe), resources: now_r.map(|n| (n, before)), utc_offset_secs: Some(0), os_name: None, tag: "" };
    host_row(&input, t, now())
}

fn default_row(probe: &HostProbe, now_r: Option<&Resources>) -> HostRow {
    row(probe, now_r, None, &Thresholds::default())
}

fn levels(r: &HostRow) -> [Level; 8] {
    [r.pools.level, r.space.level, r.memory.level, r.cpu.level, r.load.level, r.scrub.level, r.errors.level, r.state.level]
}

#[test]
fn levels_sort_worst_first() {
    let mut all = [Level::Fine, Level::Issue, Level::Unknown, Level::Watch];
    all.sort_by(|a, b| b.cmp(a));
    assert_eq!(all, [Level::Issue, Level::Watch, Level::Unknown, Level::Fine]);
    assert_eq!((Level::Issue.rank(), Level::Fine.rank()), (3, 0));
}

#[test]
fn space_is_fine_below_80_watch_from_80_and_an_issue_from_90() {
    let at = |c: u8| default_row(&reached(vec![pool("tank", c, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((at(79).space.level, at(80).space.level, at(89).space.level, at(90).space.level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue));
    assert_eq!((at(43).space.text.as_str(), at(43).space.gauge), ("43%", Some(43)));
}

#[test]
fn memory_is_fine_below_90_watch_from_90_and_an_issue_from_97() {
    // 20 GiB, nothing free or cached: used% is 100 minus the free share.
    let at = |free_tenths: u64| {
        let r = Resources { free: 20 * GIB * free_tenths / 100, ..reading(0, 0, 0, 100) };
        default_row(&reached(vec![]), Some(&r)).memory
    };
    assert_eq!((at(11).level, at(10).level, at(4).level, at(3).level, at(2).level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue, Level::Issue));
    assert_eq!(at(2).text, "98%");
    assert_eq!(at(2).gauge, Some(98));
}

#[test]
fn a_host_whose_cache_fills_the_ram_is_not_under_memory_pressure() {
    let r = reading(1, 19, 0, 100);
    let memory = default_row(&reached(vec![]), Some(&r)).memory;
    assert_eq!((memory.level, memory.text.as_str()), (Level::Fine, "0%"));
}

#[test]
fn swap_in_use_raises_the_memory_cell_and_names_itself_only_when_it_matters() {
    let with_swap = |used_pct: u64| Resources { swap: Some(crate::zfs::host::Swap { total: 100 * GIB, used: used_pct * GIB }), ..reading(10, 0, 0, 100) };
    let at = |p: u64| default_row(&reached(vec![]), Some(&with_swap(p))).memory;
    assert_eq!((at(49).level, at(49).text.as_str()), (Level::Fine, "50%"), "memory alone is 50% here and below its own watch line; swap at 49 is quiet");
    assert_eq!(at(50).level, Level::Watch);
    assert!(at(50).text.contains("swap 50%"), "{}", at(50).text);
    assert_eq!(at(90).level, Level::Issue);
    assert_eq!(default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100))).memory.level, Level::Fine, "no swap at all is fine");
}

#[test]
fn cpu_is_a_watch_only_from_95_between_two_readings_and_never_an_issue() {
    let t = Thresholds::default();
    let before = reading(10, 0, 0, 1000);
    let cpu = |busy: u64, idle: u64| row(&reached(vec![]), Some(&reading(10, 0, busy, 1000 + idle)), Some(&before), &t).cpu;
    assert_eq!(cpu(94, 6).level, Level::Fine);
    assert_eq!((cpu(95, 5).level, cpu(95, 5).text.as_str()), (Level::Watch, "95%"));
    assert_eq!(cpu(100, 0).level, Level::Watch, "CPU alone is never red");
    assert_eq!(cpu(40, 60).gauge, Some(40));
    let one = row(&reached(vec![]), Some(&reading(10, 0, 0, 1000)), None, &t).cpu;
    assert_eq!((one.level, one.text.as_str()), (Level::Unknown, "?"), "the first reading has nothing to compare with");
}

#[test]
fn a_scrub_is_fine_until_35_days_a_watch_from_35_and_an_issue_from_90() {
    let at = |days: i64| default_row(&reached(vec![pool("tank", 10, 0, Some(when(days)))]), Some(&reading(10, 0, 0, 100))).scrub;
    assert_eq!((at(34).level, at(35).level, at(89).level, at(90).level), (Level::Fine, Level::Watch, Level::Watch, Level::Issue));
    assert_eq!(at(8).text, "8 d ago");
    assert_eq!(at(0).text, "today");
    assert!(at(40).note.as_deref().unwrap().contains("tank") && at(40).note.as_deref().unwrap().contains("40"), "{:?}", at(40).note);
}

#[test]
fn a_pool_never_scrubbed_is_a_watch_unless_it_is_ignored_and_a_resilver_is_not_a_scrub() {
    let never = default_row(&reached(vec![pool("zroot", 10, 0, None)]), Some(&reading(10, 0, 0, 100))).scrub;
    assert_eq!((never.level, never.text.as_str()), (Level::Watch, "never"));
    assert!(never.note.as_deref().unwrap().contains("zroot never scrubbed"));
    let t = Thresholds { scrub_ignore: vec!["zroot".into()], ..Thresholds::default() };
    let ignored = row(&reached(vec![pool("zroot", 10, 0, None)]), Some(&reading(10, 0, 0, 100)), None, &t).scrub;
    assert_eq!(ignored.level, Level::Fine, "a pool the owner said not to judge");
    // `scan` says a scan finished (a resilver), but there was never a scrub.
    let resilvered = PoolProbe { scan: Some(ScanSummary::Finished { when: when(1), errors: 0 }), ..pool("tpool", 10, 0, None) };
    assert_eq!(default_row(&reached(vec![resilvered]), Some(&reading(10, 0, 0, 100))).scrub.level, Level::Watch);
}

#[test]
fn a_running_or_paused_scan_is_not_judged_and_no_utc_offset_means_unknown_age() {
    let running = PoolProbe { scan: Some(ScanSummary::Running { percent: Some(34), resilver: false }), ..pool("tank", 10, 0, None) };
    let r = default_row(&reached(vec![running]), Some(&reading(10, 0, 0, 100))).scrub;
    assert_eq!((r.level, r.text.as_str()), (Level::Fine, "running 34%"));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(50)))]);
    let input = HostInput { name: "h", assessment: assess(Some(&probe), false), probe: Some(&probe), resources: None, utc_offset_secs: None, os_name: None, tag: "" };
    let no_offset = host_row(&input, &Thresholds::default(), now()).scrub;
    assert_eq!((no_offset.level, no_offset.text.as_str()), (Level::Unknown, "?"), "an age needs the host's UTC offset");
}

#[test]
fn disk_errors_are_counted_over_the_pools_and_a_missing_count_is_unknown() {
    let r = default_row(&reached(vec![pool("a", 10, 2, Some(when(1))), pool("b", 10, 3, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((r.errors.level, r.errors.text.as_str()), (Level::Watch, "5"));
    let clean = default_row(&reached(vec![pool("a", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((clean.errors.level, clean.errors.text.as_str()), (Level::Fine, "0"));
    let blind = PoolProbe { errors: None, ..pool("a", 10, 0, Some(when(1))) };
    assert_eq!(default_row(&reached(vec![blind]), Some(&reading(10, 0, 0, 100))).errors.level, Level::Unknown, "zpool status was unavailable");
}

#[test]
fn the_state_starts_from_the_hosts_assessment_and_is_only_ever_raised() {
    let fine = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((fine.state.level, fine.state.text.as_str()), (Level::Fine, "ok"));
    // memory 98% on a host assess() calls fine: the state becomes an issue and says why.
    let hot = Resources { free: 20 * GIB * 2 / 100, ..reading(0, 0, 0, 100) };
    let raised = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&hot));
    assert_eq!((raised.state.level, raised.state.text.as_str()), (Level::Issue, "memory 98%"));
    // an issue from assess() stays an issue whatever the readings say.
    let degraded = PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) };
    let r = default_row(&reached(vec![degraded]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((r.state.level, r.state.text.as_str()), (Level::Issue, "tank DEGRADED"));
    // a Watch from assess() is raised by an Issue reading but not lowered by a fine one.
    let full = pool("tank", 85, 0, Some(when(1)));
    assert_eq!(default_row(&reached(vec![full.clone()]), Some(&reading(10, 0, 0, 100))).state.level, Level::Watch);
    assert_eq!(default_row(&reached(vec![full]), Some(&hot)).state.level, Level::Issue);
}

#[test]
fn a_host_that_cannot_be_reached_or_inspected_has_unknown_readings_never_fine_ones() {
    for probe in [HostProbe::Disconnected, HostProbe::Unavailable { reason: "sudo: a password is required".into() }] {
        let r = default_row(&probe, None);
        assert_ne!(r.state.level, Level::Fine, "{probe:?}");
        for cell in [&r.pools, &r.space, &r.memory, &r.cpu, &r.load, &r.scrub, &r.errors] {
            assert_eq!((cell.level, cell.text.as_str()), (Level::Unknown, "?"), "{probe:?}");
        }
    }
    let never_probed = host_row(&HostInput { name: "h", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None, os_name: None, tag: "" }, &Thresholds::default(), now());
    assert_eq!((never_probed.state.level, never_probed.state.text.as_str()), (Level::Unknown, "probing"));
}

#[test]
fn a_host_that_answered_but_has_no_resource_reading_shows_unknown_memory_cpu_and_load() {
    let r = default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), None);
    assert_eq!((r.memory.level, r.cpu.level, r.load.level), (Level::Unknown, Level::Unknown, Level::Unknown));
    assert_eq!(r.space.level, Level::Fine, "what the probe did tell is still shown");
}

#[test]
fn a_host_with_no_pools_is_fine_with_dashes() {
    let r = default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100)));
    for cell in [&r.pools, &r.space, &r.scrub, &r.errors] {
        assert_eq!((cell.level, cell.text.as_str()), (Level::Fine, "-"));
    }
    assert_eq!(levels(&r)[7], Level::Fine);
}

#[test]
fn the_load_is_shown_as_the_one_minute_average_and_never_judged() {
    let r = default_row(&reached(vec![]), Some(&reading(10, 0, 0, 100)));
    assert_eq!((r.load.text.as_str(), r.load.level), ("0.05", Level::Fine));
    let busy = Resources { load: Some([40.0, 30.0, 20.0]), ..reading(10, 0, 0, 100) };
    assert_eq!(default_row(&reached(vec![]), Some(&busy)).load.level, Level::Fine, "the spec has no load threshold");
}

#[test]
fn attention_lists_every_non_fine_cell_once_and_the_assessments_own_verdict_unless_a_cell_says_it() {
    let hot = Resources { free: 20 * GIB * 2 / 100, ..reading(0, 0, 0, 100) };
    // space 85% (assess says "tank 85% full"), never scrubbed, memory 98%.
    let r = default_row(&reached(vec![pool("tank", 85, 0, None)]), Some(&hot));
    let items = host_attention(&r);
    let texts: Vec<&str> = items.iter().map(|(_, w)| w.as_str()).collect();
    assert!(texts.contains(&"memory 98%") && texts.contains(&"tank 85% full") && texts.contains(&"tank never scrubbed"), "{texts:?}");
    assert_eq!(texts.iter().filter(|t| t.contains("85% full")).count(), 1, "the space cell and assess() say the same thing once");
    let degraded = PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) };
    let r = default_row(&reached(vec![degraded]), Some(&hot));
    let texts: Vec<String> = host_attention(&r).into_iter().map(|(_, w)| w).collect();
    assert!(texts.contains(&"tank DEGRADED".to_string()) && texts.contains(&"memory 98%".to_string()), "a cell must not hide the pool state: {texts:?}");
    assert!(host_attention(&default_row(&reached(vec![pool("tank", 10, 0, Some(when(1)))]), Some(&reading(10, 0, 0, 100)))).is_empty());
    let unreachable = default_row(&HostProbe::Disconnected, None);
    assert_eq!(host_attention(&unreachable), vec![(Level::Issue, "unreachable".to_string())], "unknown cells are not items of their own");
}

// ---- the other components, attention and the verdict ----

use crate::daemon::heartbeat::Heartbeat;
use crate::daemon::history::{Event, EventKind, History, RunResult};
use crate::jobs::Job;
use crate::ui::jobs::JobsView;
use crate::ui::state::Section;
use std::collections::HashMap;

fn mirror(id: &str, remote: &str) -> Job {
    Job::test_mirror(id, "tank/data", "hourly", remote)
}

fn scrub_job(id: &str) -> Job {
    let mut j = Job::test_snapshot(id, "tank/data", "hourly", 3);
    j.kind = crate::jobs::KIND_SCRUB.into();
    j.pool = Some("tank".into());
    j
}

/// A jobs view with these jobs and these finished runs `(job, result, message)`, all two hours before `now()`.
fn view(jobs: Vec<Job>, runs: &[(&str, RunResult, &str)], heartbeat: Option<Heartbeat>) -> JobsView {
    let dir = tempfile::tempdir().unwrap();
    let history = History::new(dir.path().join("history.jsonl"));
    for (i, (job, result, message)) in runs.iter().enumerate() {
        let when = now() - Duration::hours(2);
        let ev = |kind, result| Event { job_id: (*job).into(), run_id: format!("{job}-{i}"), event: kind, at: when, scheduled_for: None, result, message: (*message).into(), catch_up: false, summary: None };
        history.append(&ev(EventKind::Started, None)).unwrap();
        history.append(&ev(EventKind::Finished, Some(*result))).unwrap();
    }
    JobsView { jobs, runs: crate::daemon::history::runs(&history.read()), heartbeat, loaded: true, ..JobsView::default() }
}

fn beat(seconds_ago: i64) -> Option<Heartbeat> {
    Some(Heartbeat { pid: 4242, started_at: now() - Duration::days(1), last_tick: now() - Duration::seconds(seconds_ago) })
}

fn alerts(mail: bool) -> crate::ui::alerts::AlertsView {
    let mut a = crate::ui::alerts::AlertsView { loaded: true, ..Default::default() };
    if mail {
        let cfg = crate::config::NotifyConfig { mail_to: Some("me@example.com".into()), ..Default::default() };
        a.saved = cfg.clone();
        a.settings = cfg.resolve().ok();
    }
    a
}

struct Fixture {
    view: JobsView,
    alerts: crate::ui::alerts::AlertsView,
    cloud_rows: Vec<crate::ui::cloud::Row>,
    sizes: HashMap<String, crate::ui::cloud::Size>,
    caps: crate::ui::cloud::Caps,
    thresholds: Thresholds,
    protection: crate::config::ProtectionSettings,
    protection_problem: Option<String>,
    smart: crate::daemon::smart_check::SmartFile,
    shares: crate::daemon::shares_check::SharesFile,
    storage: crate::daemon::storage_check::StorageFile,
    marker: Option<crate::daemon::checks::Marker>,
    expand_groups: bool,
}

impl Fixture {
    fn new(view: JobsView) -> Fixture {
        Fixture { view, alerts: alerts(true), cloud_rows: vec![], sizes: HashMap::new(), caps: HashMap::new(), thresholds: Thresholds::default(), protection: crate::config::ProtectionSettings::default(), protection_problem: None, smart: Default::default(), shares: Default::default(), storage: Default::default(), marker: None, expand_groups: false }
    }

    fn build(&self, hosts: Vec<HostInput>) -> Dashboard {
        build(&Inputs { hosts, jobs: &self.view, cloud_rows: self.cloud_rows.clone(), cloud_sizes: &self.sizes, cloud_caps: &self.caps, alerts: &self.alerts, thresholds: &self.thresholds, protection: &self.protection, protection_problem: self.protection_problem.as_deref(), smart: &self.smart, shares: &self.shares, storage: &self.storage, running: self.marker.as_ref(), expand_groups: self.expand_groups, now: now() })
    }
}

fn fine_host<'a>(name: &'a str, probe: &'a HostProbe, r: &'a Resources, before: &'a Resources) -> HostInput<'a> {
    HostInput { name, assessment: assess(Some(probe), false), probe: Some(probe), resources: Some((r, Some(before))), utc_offset_secs: Some(0), os_name: None, tag: "" }
}

#[test]
fn the_daemon_line_says_running_with_the_heartbeat_age_or_that_nothing_will_fire() {
    let on = Fixture::new(view(vec![], &[], beat(3))).build(vec![]);
    assert_eq!((on.daemon.level, on.daemon.text.as_str()), (Level::Fine, "running (pid 4242), heartbeat 3s ago"));
    for heartbeat in [None, beat(600)] {
        let off = Fixture::new(view(vec![], &[], heartbeat)).build(vec![]);
        assert_eq!(off.daemon.level, Level::Issue);
        assert!(off.daemon.text.contains("not running"), "{}", off.daemon.text);
        assert!(off.attention.iter().any(|i| i.level == Level::Issue && i.subject == "daemon" && i.goto == Section::Jobs), "{:?}", off.attention);
    }
}

#[test]
fn the_jobs_line_counts_enabled_jobs_by_kind_and_names_the_failed_ones() {
    let mut disabled = scrub_job("old-scrub");
    disabled.enabled = false;
    let jobs = vec![scrub_job("scrub-a"), disabled, Job::test_backup("backup-a", "tank/data", "hourly", "/r", "/pw"), mirror("mirror-a", "b2:bkt/p")];
    let runs = [("scrub-a", RunResult::Ok, "ok"), ("backup-a", RunResult::Failed, "password file not readable\nmore"), ("old-scrub", RunResult::Failed, "ignored: disabled")];
    let d = Fixture::new(view(jobs, &runs, beat(1))).build(vec![]);
    assert_eq!(d.jobs.level, Level::Issue);
    assert!(d.jobs.text.contains("scrub 1/1 ok") && d.jobs.text.contains("backup 0/1 FAILED") && d.jobs.text.contains("cloud 1/1 ok"), "{}", d.jobs.text);
    assert_eq!(d.failed_jobs, ["backup-a"], "a disabled job is left out");
    let item = d.attention.iter().find(|i| i.subject == "backup-a").expect("the failed job is in Needs attention");
    assert_eq!((item.level, item.goto), (Level::Issue, Section::Jobs));
    assert!(item.what.contains("failed 2h ago") && item.what.contains("password file not readable") && !item.what.contains("more"), "{}", item.what);
}

#[test]
fn no_jobs_at_all_is_unknown_not_fine() {
    let d = Fixture::new(view(vec![], &[], beat(1))).build(vec![]);
    assert_eq!((d.jobs.level, d.jobs.text.as_str()), (Level::Unknown, "no jobs scheduled"));
    assert!(d.failed_jobs.is_empty());
}

#[test]
fn cloud_lines_follow_the_cloud_screen_and_never_trigger_a_measurement() {
    let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
    let mut known = crate::ui::cloud::Known::new();
    known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
    let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "mirrored")], beat(1)));
    fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
    let d = fx.build(vec![]);
    assert_eq!(d.cloud.len(), 2);
    assert_eq!(d.cloud[0].level, Level::Fine);
    assert!(d.cloud[0].text.contains("b2") && d.cloud[0].text.contains("ok 2h ago") && !d.cloud[0].text.contains("MiB"), "{}", d.cloud[0].text);
    assert_eq!((d.cloud[1].level, d.cloud[1].text.contains("no jobs")), (Level::Unknown, true), "a remote nobody uses");
    // measured this session: the size shows, with the cap when there is one.
    fx.sizes.insert("m1".into(), crate::ui::cloud::Size { current: Some((3, 1_992_294)), old: None, at: now(), problem: None });
    fx.caps.insert("b2".into(), Ok(10_000_000_000));
    assert!(fx.build(vec![]).cloud[0].text.contains("1.9 MiB of 10 GB (0.02%)"));
    // a plan at 85% is a Watch line and an item that goes to Cloud.
    fx.caps.insert("b2".into(), Ok(2_300_000));
    let d = fx.build(vec![]);
    assert_eq!(d.cloud[0].level, Level::Watch);
    assert!(d.attention.iter().any(|i| i.level == Level::Watch && i.subject == "b2" && i.goto == Section::Cloud), "{:?}", d.attention);
}

#[test]
fn a_cloud_line_starts_with_the_providers_name_and_then_the_remote() {
    let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
    let mut known = crate::ui::cloud::Known::new();
    known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
    known.insert("fbsd16".into(), vec![("aws".into(), "s3".into())]);
    let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "ok")], beat(1)));
    fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
    let d = fx.build(vec![]);
    let texts: Vec<&str> = d.cloud.iter().map(|l| l.text.as_str()).collect();
    // two spaces between the columns; the remote column is as wide as the longest remote (`fbsd16:aws`, 10)
    let cols = |provider: &str, remote: &str, rest: &str| format!("{provider:<16}  {remote:<10}  {rest}");
    assert!(texts[0].starts_with(&cols("Backblaze B2", "b2", "1 job    ok")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with(&cols("SFTP", "t1", "no jobs  -"))), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with(&cols("Amazon S3 (etc.)", "fbsd16:aws", "no jobs  -"))), "{texts:?}");
    // The header names the columns in the same places.
    let header = &d.cloud_header;
    let at = |name: &str| header.find(name).unwrap_or_else(|| panic!("{name} missing from {header:?}"));
    assert!(header.starts_with("Provider") && at("Remote") == 18 && at("Jobs") == 30 && at("Last run") > at("Jobs") && at("Next run") > at("Last run") && at("In bucket") > at("Next run"), "{header:?}");
    assert_eq!(&texts[0][18..28], "b2        ", "the remote starts under its header");
    assert!(texts.iter().all(|t| !t.contains("(b2)") && !t.contains("(sftp)")), "the raw type in parentheses is gone: {texts:?}");
}

#[test]
fn a_running_cloud_job_shows_its_rate_on_its_row_and_an_idle_one_shows_a_dotted_track() {
    let jobs = vec![mirror("m1", "b2:zfsctl/photos"), mirror("m2", "t1:zfsctl/x")];
    let mut known = crate::ui::cloud::Known::new();
    known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into()), ("zfsctl-aws".into(), "s3".into())]);
    let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "ok"), ("m2", RunResult::Ok, "ok")], beat(1)));
    fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
    let idle = fx.build(vec![]);
    assert!(idle.cloud_header.contains("Traffic") && idle.cloud_header.find("Traffic") > idle.cloud_header.find("Next run") && idle.cloud_header.find("Traffic") < idle.cloud_header.find("In bucket"), "{:?}", idle.cloud_header);
    assert!(idle.cloud.iter().take(2).all(|l| l.text.contains("┄┄┄┄┄┄")), "{:?}", idle.cloud);
    assert!(idle.cloud[2].text.contains("no jobs") && !idle.cloud[2].text.contains('┄'), "a remote nobody uses has no track: {}", idle.cloud[2].text);
    // m1 is running at 3 MiB/s: its row (b2) has the arrow and the rate, the other row does not.
    fx.view.runs.push(crate::daemon::history::Run { job_id: "m1".into(), run_id: "m1-live".into(), started_at: now(), scheduled_for: None, finished_at: None, result: None, message: String::new(), catch_up: false, summary: None });
    fx.view.progress.push(crate::daemon::progress::Progress { job_id: "m1".into(), bytes: 1, total: None, started_at: now(), updated_at: now(), rate: Some(3 * 1024 * 1024) });
    let busy = fx.build(vec![]);
    assert!(busy.cloud[0].text.contains('▸') && busy.cloud[0].text.contains("3.0 MiB/s"), "{}", busy.cloud[0].text);
    assert!(!busy.cloud[1].text.contains('▸') && !busy.cloud[1].text.contains("/s"), "{}", busy.cloud[1].text);
    // the traffic column is as wide for the idle row as for the busy one, so In bucket lines up
    assert_eq!(busy.cloud[0].text.chars().count(), busy.cloud[1].text.chars().count(), "{:?}", busy.cloud);
}

#[test]
fn a_job_that_stopped_succeeding_is_a_line_in_needs_attention_that_goes_to_the_job() {
    let snap = Job::test_snapshot("snap-a", "tank/data", "hourly", 24);
    let mut fx = Fixture::new(view(vec![snap], &[("snap-a", RunResult::Ok, "ok")], beat(1)));
    // `view` puts the finished runs two hours before now: an hourly job is not late yet
    assert!(fx.build(vec![]).attention.iter().all(|i| i.subject != "snap-a"), "{:?}", fx.build(vec![]).attention);
    for run in &mut fx.view.runs {
        run.finished_at = Some(now() - Duration::hours(6));
    }
    let item = fx.build(vec![]).attention.into_iter().find(|i| i.subject == "snap-a").expect("a late job is listed");
    assert_eq!((item.level, item.goto, item.target.as_deref()), (Level::Issue, Section::Jobs, Some("snap-a")));
    assert_eq!(item.what, "snapshot tank/data: last ok 6h ago (Hourly)");
    // with the daemon down the job is not blamed: the daemon's own line says why nothing runs
    let down = Fixture::new(JobsView { heartbeat: None, ..fx.view.clone() }).build(vec![]);
    assert!(down.attention.iter().all(|i| i.subject != "snap-a"), "{:?}", down.attention);
    assert!(down.attention.iter().any(|i| i.subject == "daemon"));
}

#[test]
fn the_arc_and_l2arc_cells_show_hit_rates_since_boot_and_a_dash_where_there_is_nothing() {
    use crate::zfs::host_resources::CacheStats;
    let probe = reached(vec![]);
    let with = |cache: Option<CacheStats>| Resources { cache, ..reading(10, 0, 0, 100) };
    let busy = with(Some(CacheStats { hits: 9990, misses: 10, l2_size: 2 * GIB, l2_hits: 300, l2_misses: 100 }));
    let r = default_row(&probe, Some(&busy));
    assert_eq!((r.arc.text.as_str(), r.l2arc.text.as_str()), ("99.9%", "2.0G 75.0%"));
    assert_eq!((r.arc.level, r.l2arc.level), (Level::Fine, Level::Fine), "these are never judged");
    let no_cache_device = default_row(&probe, Some(&with(Some(CacheStats { hits: 1, misses: 1, ..CacheStats::default() }))));
    assert_eq!((no_cache_device.arc.text.as_str(), no_cache_device.l2arc.text.as_str()), ("50.0%", "-"));
    let cold = default_row(&probe, Some(&with(Some(CacheStats { l2_size: GIB, ..CacheStats::default() }))));
    assert_eq!((cold.arc.text.as_str(), cold.l2arc.text.as_str()), ("-", "1.0G -"), "a cache nothing has read from yet has no rate");
    let no_zfs = default_row(&probe, Some(&with(None)));
    assert_eq!((no_zfs.arc.text.as_str(), no_zfs.l2arc.text.as_str()), ("-", "-"));
    let unknown = default_row(&HostProbe::Disconnected, None);
    assert_eq!((unknown.arc.text.as_str(), unknown.l2arc.text.as_str()), ("?", "?"));
}

#[test]
fn the_next_run_cell_is_the_soonest_enabled_job_and_a_dash_when_none() {
    let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
    let mut known = crate::ui::cloud::Known::new();
    known.insert("local".into(), vec![("b2".into(), "b2".into())]);
    let rows = crate::ui::cloud::rows(&jobs, &known);
    let fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Ok, "ok")], beat(1)));
    let schedule = jobs[0].schedule.clone();
    let want = crate::daemon::cron::Schedule::parse(&schedule).unwrap().next_after(&now()).unwrap().format("%a %m-%d %H:%M").to_string();
    assert_eq!(crate::ui::cloud::next_run_text(&rows[0], &fx.view, &now()), want);
    let mut off = jobs.clone();
    off[0].enabled = false;
    let view_off = view(off, &[("m1", RunResult::Ok, "ok")], beat(1));
    assert_eq!(crate::ui::cloud::next_run_text(&rows[0], &view_off, &now()), "-", "a disabled job has no next run");
}

#[test]
fn a_failed_cloud_job_is_one_item_not_two_and_its_cloud_line_is_red() {
    let jobs = vec![mirror("m1", "b2:zfsctl/photos")];
    let mut known = crate::ui::cloud::Known::new();
    known.insert("local".into(), vec![("b2".into(), "b2".into())]);
    let mut fx = Fixture::new(view(jobs.clone(), &[("m1", RunResult::Failed, "the keys were refused")], beat(1)));
    fx.cloud_rows = crate::ui::cloud::rows(&jobs, &known);
    let d = fx.build(vec![]);
    assert_eq!(d.cloud[0].level, Level::Issue);
    assert_eq!(d.attention.iter().filter(|i| i.subject == "m1" || i.subject == "b2").count(), 1, "{:?}", d.attention);
}

#[test]
fn a_resilvering_pool_is_one_yellow_line_and_a_scrub_or_a_finished_resilver_is_none() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let items = |scan: ScanSummary, health: PoolHealth| {
        let probe = reached(vec![PoolProbe { scan: Some(scan), health, last_scrub: Some(when(1)), ..pool("tank", 10, 0, Some(when(1))) }]);
        fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("resilver")).collect::<Vec<_>>()
    };
    let one = items(ScanSummary::Running { percent: Some(42), resilver: true }, PoolHealth::Online);
    assert_eq!(one.len(), 1, "{one:?}");
    assert_eq!((one[0].level, one[0].subject.as_str(), one[0].what.as_str(), one[0].goto, one[0].target.as_deref()), (Level::Watch, "h", "tank resilvering 42%", Section::Pools, Some("h")));
    assert_eq!(items(ScanSummary::Running { percent: None, resilver: true }, PoolHealth::Online)[0].what, "tank resilvering");
    assert_eq!(items(ScanSummary::Paused { resilver: true }, PoolHealth::Online)[0].what, "tank resilver paused");
    assert!(items(ScanSummary::Running { percent: Some(42), resilver: false }, PoolHealth::Online).is_empty(), "a scrub gets no line");
    assert!(items(ScanSummary::Finished { when: when(1), errors: 0 }, PoolHealth::Online).is_empty(), "a finished resilver gets no line");
    let degraded = items(ScanSummary::Running { percent: Some(5), resilver: true }, PoolHealth::Degraded);
    assert_eq!(degraded.len(), 1, "the resilver line shows once next to the pool's own health line");
}

fn smart_file(entries: &[(&str, i64, &[(&str, crate::smart::Severity, &str)])]) -> crate::daemon::smart_check::SmartFile {
    use crate::daemon::smart_check::{HostResult, SmartFile};
    let mut file = SmartFile::default();
    for (host, hours_ago, disks) in entries {
        let mut list: Vec<crate::smart::Disk> = Vec::new();
        for (dev, severity, text) in *disks {
            match list.iter_mut().find(|d| d.dev == *dev) {
                Some(d) => d.problems.push(crate::smart::Problem { severity: *severity, text: (*text).to_string() }),
                None => list.push(crate::smart::Disk { dev: (*dev).to_string(), problems: vec![crate::smart::Problem { severity: *severity, text: (*text).to_string() }] }),
            }
        }
        file.hosts.insert((*host).to_string(), HostResult { checked_at: now() - Duration::hours(*hours_ago), disks: list, error: None });
    }
    file
}

fn disk_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
    fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Disks || i.subject == "checks").collect()
}

#[test]
fn a_host_with_a_failing_disk_is_one_red_line_and_a_healthy_host_none() {
    use crate::smart::Severity::{Issue, Watch};
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.smart = smart_file(&[("h", 3, &[("ada0", Issue, "SMART health FAILED")]), ("ok", 3, &[])]);
    let items = disk_items(&fx, &["h", "ok"]);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!((items[0].level, items[0].subject.as_str(), items[0].goto, items[0].target.as_deref()), (Level::Issue, "h", Section::Disks, Some("h")));
    assert!(items[0].what.contains("ada0 SMART health FAILED") && items[0].what.contains("checked 3h ago"), "{}", items[0].what);
    fx.smart = smart_file(&[("h", 3, &[("ada0", Watch, "reallocated sectors 8"), ("ada4", Watch, "SSD wear 87%")])]);
    let items = disk_items(&fx, &["h"]);
    assert_eq!((items.len(), items[0].level), (1, Level::Watch), "several disks share one yellow line");
    assert!(items[0].what.contains("ada0 reallocated sectors 8; ada4 SSD wear 87%"), "{}", items[0].what);
}

#[test]
fn a_wearing_disk_says_what_it_is_for_when_the_storage_check_knows() {
    use crate::smart::Severity::Watch;
    use crate::zfs::pool_classes::{DeviceRole, PoolClasses};
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.smart = smart_file(&[("h", 3, &[("ada4", Watch, "SSD wear 87%"), ("ada0", Watch, "pending sectors 8")])]);
    let pools = vec![PoolClasses { name: "tank".into(), roles: vec![DeviceRole { role: "cache".into(), device: "ada4p1".into() }, DeviceRole { role: "log".into(), device: "gpt/slog0".into() }], ..PoolClasses::default() }];
    let result = crate::daemon::storage_check::StorageResult { checked_at: now(), datasets: vec![], seen: vec![], pools, zfs_version: None, locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None };
    fx.storage.hosts.insert("h".into(), result);
    let items = disk_items(&fx, &["h"]);
    assert!(items[0].what.contains("ada4 (cache of tank) SSD wear 87%; ada0 pending sectors 8"), "the partition ada4p1 is the disk ada4, and a disk with no known role keeps its bare name: {}", items[0].what);
}

#[test]
fn the_last_good_result_stays_after_a_failed_check_with_its_real_age() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.smart = smart_file(&[("h", 30, &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")])]);
    fx.smart.hosts.get_mut("h").unwrap().error = Some("no route".into());
    let items = disk_items(&fx, &["h"]);
    assert_eq!(items.len(), 1);
    assert!(items[0].what.contains("checked 1d ago"), "{}", items[0].what);
}

#[test]
fn a_check_older_than_three_days_while_the_daemon_runs_adds_one_yellow_line() {
    let problem: &[(&str, crate::smart::Severity, &str)] = &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")];
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.smart = smart_file(&[("h", 24 * 4, problem)]);
    let stale: Vec<Item> = disk_items(&fx, &["h"]).into_iter().filter(|i| i.subject == "checks").collect();
    assert_eq!(stale.len(), 1);
    assert_eq!((stale[0].level, stale[0].what.as_str()), (Level::Watch, "disk and shares check is 4 days old"));
    fx.smart.ran_at = Some(now() - Duration::hours(1));
    let still: Vec<Item> = disk_items(&fx, &["h"]).into_iter().filter(|i| i.subject == "checks").collect();
    assert_eq!(still.len(), 1, "the daemon ran an hour ago but no host has answered for 4 days: the check is broken, not fine");
    fx.smart.hosts.get_mut("h").unwrap().checked_at = now() - Duration::hours(24);
    assert!(disk_items(&fx, &["h"]).iter().all(|i| i.subject != "checks"), "a host answered yesterday");
    fx.smart.hosts.get_mut("h").unwrap().checked_at = now() - Duration::days(4);
    fx.smart.ran_at = Some(now() - Duration::days(5));
    let mut off = Fixture::new(view(vec![], &[], None));
    off.smart = fx.smart.clone();
    assert!(disk_items(&off, &["h"]).iter().all(|i| i.subject != "checks"), "the daemon is not running: its own line says so, and this one would only repeat it");
}

fn shares_entry(hours_ago: i64, nfs_running: bool, nfs_shares: usize, samba_running: bool, smb_shares: usize, smb_error: Option<&str>) -> crate::daemon::shares_check::SharesResult {
    crate::daemon::shares_check::SharesResult { checked_at: now() - Duration::hours(hours_ago), nfs_running, samba_running, nfs_shares, smb_shares, smb_error: smb_error.map(str::to_string), error: None}
}

fn share_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
    fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Shares).collect()
}

fn quota_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
    fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Datasets).collect()
}

fn storage_entry(minutes_ago: i64, name: &str, used_percent: u64) -> crate::daemon::storage_check::StorageResult {
    use crate::daemon::storage_check::{LimitedDataset, StorageResult};
    StorageResult { checked_at: now() - Duration::minutes(minutes_ago), datasets: vec![LimitedDataset { name: name.into(), used: used_percent, referenced: used_percent, quota: 100, refquota: 0 }], seen: vec![], pools: vec![], zfs_version: None, locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None }
}

#[test]
fn a_dataset_near_its_quota_is_a_line_that_goes_to_datasets_and_an_unreached_host_has_none() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.storage.hosts.insert("h".into(), storage_entry(4, "tank/home", 94));
    fx.storage.hosts.insert("red".into(), storage_entry(4, "tank/db", 99));
    fx.storage.hosts.insert("gone".into(), storage_entry(4, "tank/x", 99));
    let items = quota_items(&fx, &["h", "red"]);
    assert_eq!(items.len(), 2, "{items:?}");
    let by = |s: &str| items.iter().find(|i| i.subject == s).unwrap();
    assert_eq!((by("h").level, by("red").level), (Level::Watch, Level::Issue));
    assert!(by("h").what.starts_with("tank/home 94% of its 100 quota") && by("h").what.contains("checked 4m ago"), "{}", by("h").what);
    assert_eq!(by("h").target.as_deref(), Some("h"));
}

/// A storage file where each host has these datasets, all a week old, checked a minute ago.
fn with_datasets(fx: &mut Fixture, hosts: &[(&str, &[&str])]) {
    use crate::daemon::storage_check::SeenDataset;
    let created = Some((now() - chrono::Duration::days(7)).timestamp());
    for (host, names) in hosts {
        let mut entry = storage_entry(1, "x/x", 10);
        entry.datasets.clear();
        entry.seen = names.iter().map(|n| SeenDataset { name: (*n).into(), volume: false, created }).collect();
        fx.storage.hosts.insert((*host).into(), entry);
    }
}

fn protection_items(fx: &Fixture, names: &[&str]) -> Vec<Item> {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let hosts: Vec<HostInput> = names.iter().map(|n| fine_host(n, &probe, &r, &b)).collect();
    fx.build(hosts).attention.into_iter().filter(|i| i.goto == Section::Jobs && i.what.contains("no snapshot job")).collect()
}

#[test]
fn an_unprotected_dataset_is_a_red_line_and_an_os_one_amber() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos", "zroot/var"]), ("nas", &["tank/media"])]);
    let items = protection_items(&fx, &["local", "nas"]);
    let by = |s: &str| items.iter().find(|i| i.subject == s).unwrap_or_else(|| panic!("{s} in {items:?}"));
    assert_eq!(items.len(), 3, "{items:?}");
    assert_eq!((by("tank/photos").level, by("zroot/var").level, by("nas:tank/media").level), (Level::Issue, Level::Watch, Level::Issue));
    assert_eq!(by("tank/photos").what, "no snapshot job; no copy job");
    assert_eq!((by("tank/photos").goto, by("tank/photos").target.clone()), (Section::Jobs, None));
}

#[test]
fn a_host_that_is_not_reached_adds_no_protection_lines() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos"]), ("gone", &["tank/x"])]);
    let items = protection_items(&fx, &["local"]);
    assert_eq!(items.iter().map(|i| i.subject.as_str()).collect::<Vec<_>>(), ["tank/photos"]);
}

#[test]
fn no_protection_lines_while_the_daemon_is_down_or_stale_or_a_host_is_unreachable() {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let named = |d: &Dashboard| d.attention.iter().filter(|i| i.what.contains("no snapshot job")).count();
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos"])]);
    assert_eq!(named(&fx.build(vec![fine_host("local", &probe, &r, &b)])), 1, "control: a running daemon shows the line");
    for heartbeat in [None, beat(600)] {
        let mut down = Fixture::new(view(vec![], &[], heartbeat));
        with_datasets(&mut down, &[("local", &["tank/photos"])]);
        let d = down.build(vec![fine_host("local", &probe, &r, &b)]);
        assert_ne!(d.daemon.level, Level::Fine);
        assert_eq!(named(&d), 0, "{:?}", d.attention);
    }
    // A host that is listed but not reached says nothing.
    let off = HostProbe::Disconnected;
    let host = HostInput { name: "local", assessment: assess(Some(&off), false), probe: Some(&off), resources: None, utc_offset_secs: Some(0), os_name: None, tag: "" };
    assert_eq!(named(&fx.build(vec![host])), 0);
}

#[test]
fn the_verdict_counts_the_protection_lines() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos", "zroot/var"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert!(d.verdict.text == "1 issue, 1 to watch", "{}", d.verdict.text);
}

fn protected_view() -> JobsView {
    let jobs = vec![Job::test_snapshot("s", "tank/photos", "auto", 24), Job::test_replicate("r", "tank/photos", "rep", "nas", "tank/copy")];
    view(jobs, &[("s", RunResult::Ok, ""), ("r", RunResult::Ok, "")], beat(1))
}

#[test]
fn protected_datasets_add_nothing() {
    let mut fx = Fixture::new(protected_view());
    with_datasets(&mut fx, &[("local", &["tank/photos"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert!(d.attention.is_empty(), "{:?}", d.attention);
    assert_eq!(d.verdict.text, "all quiet");
}

#[test]
fn the_chosen_level_changes_the_verdict() {
    let mut fx = Fixture::new(protected_view());
    with_datasets(&mut fx, &[("local", &["tank/photos"])]);
    assert!(fx.build(vec![]).attention.is_empty());
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let at = |fx: &Fixture| fx.build(vec![fine_host("local", &probe, &r, &b)]).attention;
    assert!(at(&fx).is_empty(), "basic: one snapshot job and one copy");
    fx.protection = crate::config::ProtectionConfig { level: Some("careful".into()), ..Default::default() }.resolve().unwrap();
    let lines = at(&fx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].what, "1 of 3 copies on other machines; no online copy");
}

/// `local` has these datasets and nothing protects them; the screen is built for it.
fn five_unprotected(os_only_rest: bool) -> Dashboard {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let names: &[&str] = if os_only_rest { &["tank/a", "tank/b", "tank/c", "zroot/var", "zroot/tmp"] } else { &["tank/a", "tank/b", "tank/c", "tank/d", "zroot/var"] };
    with_datasets(&mut fx, &[("local", names)]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    fx.build(vec![fine_host("local", &probe, &r, &b)])
}

#[test]
fn up_to_three_unprotected_datasets_are_named_and_nothing_is_folded() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos", "tank/docs", "zroot/var"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert_eq!(d.attention.len(), 3, "{:?}", d.attention);
    assert!(d.protection_more.is_none());
    assert_eq!(d.verdict.text, "2 issues, 1 to watch");
}

#[test]
fn past_three_the_rest_fold_into_one_line_that_is_red_while_any_of_your_data_is_in_it() {
    let d = five_unprotected(false);
    let subjects: Vec<&str> = d.attention.iter().map(|i| i.subject.as_str()).collect();
    assert_eq!(subjects, ["tank/a", "tank/b", "tank/c", "protection"], "your data first; three named, then the rest as one line");
    let more = d.protection_more.clone().unwrap();
    assert_eq!((more.level, more.what.as_str(), more.goto, more.target.clone()), (Level::Issue, "and 2 more not protected (1 of your data)", Section::Jobs, None));
    assert_eq!(d.attention[3], more);
    assert_eq!(d.verdict.text, "4 issues", "each line counts once; the folded datasets are one");
    assert_eq!(d.unprotected.len(), 5, "the full list is kept for the pop-up");
}

#[test]
fn the_folded_line_is_amber_when_only_operating_system_data_is_in_it() {
    let d = five_unprotected(true);
    let more = d.protection_more.unwrap();
    assert_eq!((more.level, more.what.as_str()), (Level::Watch, "and 2 more not protected (0 of your data)"));
}

#[test]
fn a_named_line_goes_straight_to_its_fix_and_the_folded_line_opens_the_list_at_the_first_hidden_dataset() {
    let d = five_unprotected(false);
    let fix = d.protection_fix(&d.attention[1]).expect("a named line has a fix");
    assert_eq!(fix.dataset, "tank/b");
    assert!(d.protection_popup(&d.attention[1]).is_none(), "a named line opens no list");
    let more = d.attention[3].clone();
    assert!(d.protection_fix(&more).is_none());
    let popup = d.protection_popup(&more).expect("the folded line opens the list");
    let note: Vec<&str> = popup.note.lines().collect();
    assert_eq!(note[0], "5 datasets are not protected (4 of your data)");
    assert!(note[1].starts_with("Protected means snapshots plus a copy on another machine"), "{}", note[1]);
    assert_eq!(note[2], "Enter fixes the selected one.");
    assert!(note[3].starts_with("Space marks, a marks"), "{}", note[3]);
    assert_eq!(popup.rows.len(), 5);
    assert!(popup.rows[0].starts_with("issue") && popup.rows[0].contains("tank/a") && popup.rows[0].ends_with("no snapshot job; no copy job"), "{}", popup.rows[0]);
    assert!(popup.rows[4].starts_with("watch") && popup.rows[4].contains("zroot/var"), "{}", popup.rows[4]);
    assert_eq!(popup.selected, 3, "the first dataset without a line of its own");
    assert_eq!(popup.gaps.iter().map(|g| g.dataset.as_str()).collect::<Vec<_>>(), ["tank/a", "tank/b", "tank/c", "tank/d", "zroot/var"], "what each row lacks, in the rows' order");
    assert!(d.protection_popup(&Item { level: Level::Issue, subject: "daemon".into(), what: "x".into(), goto: Section::Jobs, target: None }).is_none());
}

#[test]
fn the_popup_groups_by_host_with_the_hub_first_and_a_heading_and_colours_per_group() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["zroot/var", "tank/photos"]), ("nas", &["tank/old", "tank/media"]), ("alpha", &["tank/x"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let (p1, p2, p3) = (reached(vec![]), reached(vec![]), reached(vec![]));
    let d = fx.build(vec![fine_host("local", &p1, &r, &b), fine_host("nas", &p2, &r, &b), fine_host("alpha", &p3, &r, &b)]);
    let popup = d.protection_popup(&d.attention[3]).expect("five datasets: the fourth line opens the list");
    let datasets: Vec<&str> = popup.gaps.iter().map(|g| g.dataset.as_str()).collect();
    assert_eq!(datasets, ["tank/photos", "zroot/var", "tank/x", "tank/media", "tank/old"], "the hub first, then hosts by name; your data before the OS inside each");
    assert_eq!(popup.gaps.iter().map(|g| g.host.as_str()).collect::<Vec<_>>(), ["local", "local", "alpha", "nas", "nas"]);
    assert_eq!(popup.look.heads, [(0, "local  (2 datasets, 1 of your data)".to_string()), (2, "alpha  (1 dataset, 1 of your data)".to_string()), (3, "nas  (2 datasets, 2 of your data)".to_string())]);
    assert_eq!(popup.look.levels, [Level::Issue, Level::Watch, Level::Issue, Level::Issue, Level::Issue]);
    assert!(popup.rows[0].starts_with("issue") && popup.rows[0].contains("tank/photos") && !popup.rows[0].contains("local"), "the host is in the heading, not the row: {}", popup.rows[0]);
    assert_eq!(popup.selected, 1, "zroot/var is the first listed of the two datasets (it and tank/old) the Dashboard has no line for");
}

#[test]
fn a_host_with_only_operating_system_datasets_says_so_in_its_heading() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/a", "tank/b", "tank/c", "zroot/var", "zroot/tmp"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let p = reached(vec![]);
    let d = fx.build(vec![fine_host("local", &p, &r, &b)]);
    let popup = d.protection_popup(&d.attention[3]).unwrap();
    assert_eq!(popup.look.heads, [(0, "local  (5 datasets, 3 of your data)".to_string())]);
}

#[test]
fn nothing_is_folded_or_named_when_every_dataset_is_protected() {
    let mut fx = Fixture::new(protected_view());
    with_datasets(&mut fx, &[("local", &["tank/photos"])]);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert!(d.attention.is_empty() && d.protection_more.is_none() && d.unprotected.is_empty());
}

#[test]
fn email_off_still_shows_the_dashboard_lines() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    with_datasets(&mut fx, &[("local", &["tank/photos"])]);
    fx.protection.email = crate::config::EmailTiming::Off;
    assert_eq!(protection_items(&fx, &["local"]).len(), 1);
}

#[test]
fn checksum_off_is_a_watch_line_that_goes_to_datasets_for_reached_hosts_only() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let mut entry = storage_entry(2, "tank/x", 10);
    entry.integrity.checksum_off = vec!["tank/scratch".into()];
    fx.storage.hosts.insert("h".into(), entry.clone());
    fx.storage.hosts.insert("gone".into(), entry);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("checksum is off")).collect();
    assert_eq!((items.len(), items[0].level, items[0].goto), (1, Level::Watch, Section::Datasets));
    assert!(items[0].what.starts_with("tank/scratch checksum is off"), "{}", items[0].what);
}

#[test]
fn a_locked_dataset_that_a_job_uses_is_a_watch_line_that_goes_to_datasets() {
    let mut job = Job::test_backup("b", "tank/secure", "hourly", "/r", "/p");
    job.host = "h".into();
    let mut fx = Fixture::new(view(vec![job], &[], beat(1)));
    let mut entry = storage_entry(2, "tank/x", 10);
    entry.locked = vec![crate::daemon::storage_check::LockedDataset { name: "tank/secure".into(), shared: false }, crate::daemon::storage_check::LockedDataset { name: "tank/idle".into(), shared: false }];
    fx.storage.hosts.insert("h".into(), entry.clone());
    fx.storage.hosts.insert("gone".into(), entry);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("is locked")).collect();
    assert_eq!((items.len(), items[0].level, items[0].goto), (1, Level::Watch, Section::Datasets));
    assert!(items[0].what.starts_with("tank/secure is locked (a job uses it)   (checked 2m ago)") && !items[0].what.contains("idle"), "{}", items[0].what);
}

#[test]
fn a_pool_with_a_lone_special_disk_is_a_red_line_that_goes_to_pools() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let mut entry = storage_entry(2, "tank/x", 10);
    entry.pools = vec![crate::zfs::pool_classes::PoolClasses { name: "tank".into(), unprotected: vec!["special".into()], log_bad: false, cache_bad: false, roles: vec![] }];
    fx.storage.hosts.insert("h".into(), entry);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let items: Vec<Item> = fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.goto == Section::Pools && i.what.contains("special")).collect();
    assert_eq!((items.len(), items[0].level), (1, Level::Issue));
    assert!(items[0].what.starts_with("tank special vdev has no redundancy"), "{}", items[0].what);
}

#[test]
fn a_quota_check_that_stopped_running_is_one_yellow_line_but_only_while_the_daemon_runs() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.storage.ran_at = Some(now() - Duration::minutes(12));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let stale = |fx: &Fixture| -> Vec<Item> { fx.build(vec![fine_host("h", &probe, &r, &b)]).attention.into_iter().filter(|i| i.what.contains("quota check")).collect() };
    assert!(stale(&fx).is_empty(), "12 minutes is within the 10-minute cadence's slack");
    fx.storage.ran_at = Some(now() - Duration::minutes(45));
    let items = stale(&fx);
    assert_eq!((items.len(), items[0].level), (1, Level::Watch));
    assert!(items[0].what.contains("45m"), "{}", items[0].what);
    let mut down = Fixture::new(view(vec![], &[], beat(1000)));
    down.storage.ran_at = Some(now() - Duration::minutes(45));
    assert!(stale(&down).is_empty(), "no daemon, no complaint about the daemon's check");
}

#[test]
fn the_quota_thresholds_come_from_the_dashboard_settings() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.storage.hosts.insert("h".into(), storage_entry(1, "tank/home", 85));
    assert!(quota_items(&fx, &["h"]).is_empty(), "85% is under the default 90");
    fx.thresholds.quota = (80, 95);
    assert_eq!(quota_items(&fx, &["h"]).len(), 1);
}

#[test]
fn a_stopped_nfsd_with_shares_is_one_red_line_that_goes_to_shares_and_a_healthy_host_none() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.shares.hosts.insert("h".into(), shares_entry(3, false, 2, true, 0, None));
    fx.shares.hosts.insert("ok".into(), shares_entry(3, true, 2, true, 1, None));
    let items = share_items(&fx, &["h", "ok"]);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!((items[0].level, items[0].subject.as_str(), items[0].target.as_deref()), (Level::Issue, "h", Some("h")));
    assert!(items[0].what.starts_with("NFS: 2 shares but nfsd is not running") && items[0].what.contains("checked 3h ago"), "{}", items[0].what);
}

#[test]
fn an_unreadable_smb_block_is_one_yellow_line() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.shares.hosts.insert("h".into(), shares_entry(1, true, 0, true, 0, Some("SMB: the managed block has no end")));
    let items = share_items(&fx, &["h"]);
    assert_eq!((items.len(), items[0].level), (1, Level::Watch));
    assert!(items[0].what.starts_with("Samba: "), "{}", items[0].what);
}

#[test]
fn the_last_good_share_result_stays_after_a_failed_check_with_its_real_age_and_an_unreachable_host_has_no_line() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let mut old = shares_entry(30, false, 1, true, 0, None);
    old.error = Some("no route".into());
    fx.shares.hosts.insert("h".into(), old);
    assert!(share_items(&fx, &["h"])[0].what.contains("checked 1d ago"));
    let down = HostProbe::Disconnected;
    let input = HostInput { name: "h", assessment: assess(Some(&down), false), probe: Some(&down), resources: None, utc_offset_secs: Some(0), os_name: None, tag: "" };
    assert!(fx.build(vec![input]).attention.iter().all(|i| i.goto != Section::Shares), "the Hosts row already says it is unreachable");
}

#[test]
fn the_stale_line_covers_both_files_and_a_missing_file_is_not_stale() {
    let problem: &[(&str, crate::smart::Severity, &str)] = &[("ada0", crate::smart::Severity::Watch, "pending sectors 2")];
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    fx.smart = smart_file(&[("h", 1, problem)]);
    let stale = |fx: &Fixture| disk_items(fx, &["h"]).into_iter().filter(|i| i.subject == "checks").count();
    assert_eq!(stale(&fx), 0, "a fresh smart.json and no shares.json at all: nothing is stale");
    fx.smart = smart_file(&[("h", 24 * 4, problem)]);
    assert_eq!(stale(&fx), 1);
    fx.shares.hosts.insert("h".into(), shares_entry(2, true, 1, true, 0, None));
    assert_eq!(stale(&fx), 0, "the shares check answered 2 hours ago: the checks work");
    let none = Fixture::new(view(vec![], &[], beat(1)));
    assert_eq!(stale(&none), 0, "no data in either file: no line");
}

#[test]
fn a_running_check_shows_a_red_notice_that_is_not_counted_in_the_verdict() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    assert_eq!(fx.build(vec![fine_host("h", &probe, &r, &b)]).running, None);
    fx.marker = Some(crate::daemon::checks::Marker { by: "daemon".into(), since: now(), beat: now(), host: None });
    let d = fx.build(vec![fine_host("h", &probe, &r, &b)]);
    assert!(d.running.as_deref().unwrap_or("").contains("daily disk and shares check since"), "{:?}", d.running);
    assert_eq!(d.verdict.text, "all quiet", "a running check is a notice, not a problem");
    fx.marker = Some(crate::daemon::checks::Marker { by: "tui".into(), since: now(), beat: now(), host: Some("fbsd16".into()) });
    assert!(fx.build(vec![]).running.unwrap().starts_with("checking fbsd16: disks and shares since"));
}

#[test]
fn an_unreachable_host_and_a_missing_file_show_no_disk_line_and_the_verdict_counts_the_rest() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    assert!(disk_items(&fx, &["h"]).is_empty(), "no smart.json");
    fx.smart = smart_file(&[("h", 3, &[("ada0", crate::smart::Severity::Issue, "SMART health FAILED")])]);
    let down = HostProbe::Disconnected;
    let input = HostInput { name: "h", assessment: assess(Some(&down), false), probe: Some(&down), resources: None, utc_offset_secs: Some(0), os_name: None, tag: "" };
    let d = fx.build(vec![input]);
    assert!(d.attention.iter().all(|i| i.goto != Section::Disks), "the Hosts row already says it is unreachable: {:?}", d.attention);
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("h", &probe, &r, &b)]);
    assert!(d.verdict.text.contains("1 issue"), "{}", d.verdict.text);
}

#[test]
fn the_alerts_line_says_whether_mail_can_go_out_and_when_the_last_one_did() {
    let on = Fixture::new(view(vec![], &[], beat(1)));
    let d = on.build(vec![]);
    assert_eq!(d.alerts.level, Level::Fine);
    assert!(d.alerts.text.contains("monitor on"), "{}", d.alerts.text);
    let mut with_report = Fixture::new(view(vec![], &[], beat(1)));
    with_report.alerts.last_report = Some("2026-10-17".into());
    with_report.alerts.events = vec![crate::daemon::health_text::HealthEvent { at: now() - Duration::hours(3), host: "fbsd16".into(), kind: "alert".into(), text: "zroot 86% full".into() }];
    let text = with_report.build(vec![]).alerts.text;
    assert!(text.contains("last alert") && text.contains("fbsd16") && text.contains("report 2026-10-17"), "{text}");
    // configured but the daemon is down: a watch.
    let down = Fixture::new(view(vec![], &[], None)).build(vec![]).alerts;
    assert_eq!(down.level, Level::Watch);
    assert!(down.text.contains("daemon"), "{}", down.text);
    // not configured: unknown, with a hint; an unusable section: watch, with the reason.
    let mut off = Fixture::new(view(vec![], &[], beat(1)));
    off.alerts = alerts(false);
    assert_eq!(off.build(vec![]).alerts.level, Level::Unknown);
    off.alerts.problem = Some("[notify] check_interval_minutes must be at least 1".into());
    let bad = off.build(vec![]).alerts;
    assert_eq!(bad.level, Level::Watch);
    assert!(bad.text.contains("check_interval_minutes"), "{}", bad.text);
}

#[test]
fn a_wrong_protection_section_is_a_watch_line_naming_config_toml() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    assert!(fx.build(vec![]).attention.iter().all(|i| i.subject != "config.toml"));
    fx.protection_problem = Some("[protection] level must be basic, careful or custom".into());
    let d = fx.build(vec![]);
    let item = d.attention.iter().find(|i| i.subject == "config.toml").expect("a line for the wrong section");
    assert_eq!((item.level, item.what.as_str(), item.goto), (Level::Watch, "[protection]: level must be basic, careful or custom; judging at Basic", Section::Settings));
    assert_eq!(d.verdict.text, "1 to watch", "the header counts it");
    // Also while the daemon is down (it is about the file, not the judging).
    let mut down = Fixture::new(view(vec![], &[], None));
    down.protection_problem = fx.protection_problem.clone();
    assert!(down.build(vec![]).attention.iter().any(|i| i.subject == "config.toml"));
}

#[test]
fn the_verdict_says_checking_until_every_host_has_answered_then_counts_issues_and_watches() {
    let fx = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], beat(1)));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let (r, before) = (reading(10, 0, 10, 1100), reading(10, 0, 0, 1000)); // about 9% busy
    let unanswered = HostInput { name: "fbsd16", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None, os_name: None, tag: "" };
    let checking = fx.build(vec![fine_host("local", &probe, &r, &before), unanswered]).verdict;
    assert_eq!((checking.level, checking.text.as_str()), (Level::Unknown, "checking..."));
    let quiet = fx.build(vec![fine_host("local", &probe, &r, &before)]);
    assert_eq!((quiet.verdict.level, quiet.verdict.text.as_str()), (Level::Fine, "all quiet"));
    assert!(quiet.attention.is_empty());
    // one watch (pool 85% full) and one issue (daemon down).
    let busy = reached(vec![pool("tank", 85, 0, Some(when(1)))]);
    let mixed = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], None)).build(vec![fine_host("local", &busy, &r, &before)]);
    assert_eq!((mixed.verdict.level, mixed.verdict.text.as_str()), (Level::Issue, "1 issue, 1 to watch"));
    let only_watch = fx.build(vec![fine_host("local", &busy, &r, &before)]).verdict;
    assert_eq!((only_watch.level, only_watch.text.as_str()), (Level::Watch, "1 to watch"));
}

#[test]
fn a_host_that_is_unreachable_is_an_issue_and_one_that_cannot_be_inspected_a_watch() {
    let fx = Fixture::new(view(vec![scrub_job("s")], &[("s", RunResult::Ok, "ok")], beat(1)));
    let down = HostProbe::Disconnected;
    let denied = HostProbe::Unavailable { reason: "sudo: a password is required".into() };
    let input = |name, probe: &'static HostProbe| HostInput { name, assessment: assess(Some(probe), false), probe: Some(probe), resources: None, utc_offset_secs: None, os_name: None, tag: "" };
    let down: &'static HostProbe = Box::leak(Box::new(down));
    let denied: &'static HostProbe = Box::leak(Box::new(denied));
    let d = fx.build(vec![input("fbsd16", down), input("fbsd16t1", denied)]);
    let level_of = |subject: &str| d.attention.iter().find(|i| i.subject == subject).map(|i| i.level);
    assert_eq!((level_of("fbsd16"), level_of("fbsd16t1")), (Some(Level::Issue), Some(Level::Watch)));
    assert_eq!(d.verdict.text, "1 issue, 1 to watch");
}

#[test]
fn attention_is_sorted_worst_first_then_by_subject_and_everything_builds_with_nothing() {
    let jobs = vec![scrub_job("zz-scrub"), scrub_job("aa-scrub")];
    let runs = [("zz-scrub", RunResult::Failed, "boom"), ("aa-scrub", RunResult::Failed, "bang")];
    let busy = reached(vec![pool("tank", 85, 0, Some(when(1)))]);
    let (r, before) = (reading(10, 0, 0, 1000), reading(10, 0, 0, 1000));
    let d = Fixture::new(view(jobs, &runs, beat(1))).build(vec![fine_host("local", &busy, &r, &before)]);
    let order: Vec<(Level, &str)> = d.attention.iter().map(|i| (i.level, i.subject.as_str())).collect();
    assert_eq!(order, [(Level::Issue, "aa-scrub"), (Level::Issue, "zz-scrub"), (Level::Watch, "local")]);
    let empty = Fixture::new(view(vec![], &[], beat(1))).build(vec![]);
    assert_eq!(empty.verdict.level, Level::Fine, "no hosts to doubt");
    let no_daemon = Fixture::new(JobsView::default()).build(vec![]);
    assert_eq!(no_daemon.verdict.level, Level::Issue, "nothing has ever run the daemon: that is the one problem");
    assert!(empty.hosts.is_empty() && empty.cloud.is_empty());
}

#[test]
fn protection_lines_come_after_the_other_items_of_their_level() {
    let mut fx = Fixture::new(view(vec![scrub_job("zz-scrub")], &[("zz-scrub", RunResult::Failed, "boom")], beat(1)));
    let names: Vec<String> = (0..30).map(|i| format!("aa/d{i:02}")).chain(["zroot/var".to_string()]).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    with_datasets(&mut fx, &[("local", &refs)]);
    let degraded = reached(vec![PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) }]);
    let (r, before) = (reading(10, 0, 0, 1000), reading(10, 0, 0, 1000));
    let d = fx.build(vec![fine_host("local", &degraded, &r, &before)]);
    let order: Vec<(Level, &str)> = d.attention.iter().map(|i| (i.level, i.subject.as_str())).collect();
    assert_eq!(order[..3], [(Level::Issue, "local"), (Level::Issue, "zz-scrub"), (Level::Issue, "aa/d00")], "the pool and the failed job first: {order:?}");
    assert_eq!(order[2..5].iter().map(|(_, s)| *s).collect::<Vec<_>>(), refs[..3], "then the named protection lines, in their own order");
    assert_eq!(order[5..], [(Level::Issue, "protection")], "the other 28 are one line, red because your data is among them: {order:?}");
    assert_eq!(d.attention[5].what, "and 28 more not protected (27 of your data)");
}

#[test]
fn an_unreadable_status_a_resilver_and_scrub_errors_are_not_misreported() {
    let t = Thresholds::default();
    let mut unreadable = pool("a", 10, 0, None);
    unreadable.scan = None;
    assert_eq!(pool_scrub(&unreadable, Some(0), &t, now()).0, Level::Unknown, "no zpool status is unknown, not 'never scrubbed'");
    let mut resilvered = pool("b", 10, 0, None);
    resilvered.scan = Some(ScanSummary::Finished { when: when(1), errors: 0 });
    let (level, _, _, note) = pool_scrub(&resilvered, Some(0), &t, now());
    assert_eq!(level, Level::Watch);
    assert!(!note.unwrap().contains("never scrubbed"));
    let mut bad = pool("c", 10, 0, Some(when(2)));
    bad.scan = Some(ScanSummary::Finished { when: when(2), errors: 3 });
    let (level, _, _, note) = pool_scrub(&bad, Some(0), &t, now());
    assert_eq!(level, Level::Watch);
    assert_eq!(note.unwrap(), "c last scrub found 3 errors");
}

fn tagged<'a>(input: HostInput<'a>, tag: &'a str) -> HostInput<'a> {
    HostInput { tag, ..input }
}

#[test]
fn hosts_without_tags_are_not_grouped_and_the_screen_stays_as_it_was() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![]);
    let d = fx.build(vec![fine_host("a", &probe, &r, &b), fine_host("b", &probe, &r, &b)]);
    assert!(d.groups.is_none());
    assert_eq!(d.hosts.len(), 2);
}

#[test]
fn tagged_hosts_roll_up_into_one_row_per_tag_with_the_hosts_counted_and_the_untagged_last() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let d = fx.build(vec![tagged(fine_host("a", &probe, &r, &b), "web"), tagged(fine_host("b", &probe, &r, &b), "web"), tagged(fine_host("c", &probe, &r, &b), "db"), fine_host("d", &probe, &r, &b)]);
    let groups = d.groups.expect("one host has a tag");
    assert_eq!(groups.iter().map(|g| g.row.name.as_str()).collect::<Vec<_>>(), ["db (1)", "web (2)", "(no tag) (1)"]);
    assert!(groups.iter().all(|g| g.row.state.text == "ok" && g.row.state.level == Level::Fine && g.members.is_empty()), "{groups:?}");
    assert_eq!(groups[1].row.pools.text, "2", "the pools of both web hosts");
    assert_eq!(d.hosts.len(), 4, "every host still has its own row, for the Hosts section and the attention list");
}

#[test]
fn a_group_shows_its_worst_state_comes_first_and_lists_only_the_hosts_that_need_a_look() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let fine = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let degraded = reached(vec![PoolProbe { health: PoolHealth::Degraded, ..pool("tank", 10, 0, Some(when(1))) }]);
    let d = fx.build(vec![tagged(fine_host("c", &fine, &r, &b), "db"), tagged(fine_host("a", &fine, &r, &b), "web"), tagged(fine_host("b", &degraded, &r, &b), "web")]);
    let groups = d.groups.unwrap();
    assert_eq!(groups.iter().map(|g| g.row.name.as_str()).collect::<Vec<_>>(), ["web (2)", "db (1)"], "the group with a problem first");
    assert_eq!((groups[0].row.state.level, groups[0].row.state.text.as_str()), (Level::Issue, "1 issue"));
    assert_eq!(groups[0].members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["b"], "only the host that needs a look");
    assert!(groups[1].members.is_empty());
}

#[test]
fn a_group_adds_up_its_pools_and_errors_and_takes_the_worst_reading_of_the_rest() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let calm = reached(vec![pool("tank", 40, 0, Some(when(1)))]);
    let busy = reached(vec![pool("tank", 85, 3, Some(when(1))), pool("data", 20, 1, Some(when(1)))]);
    let d = fx.build(vec![tagged(fine_host("a", &calm, &r, &b), "web"), tagged(fine_host("b", &busy, &r, &b), "web")]);
    let row = &d.groups.unwrap()[0].row;
    assert_eq!(row.pools.text, "3");
    assert_eq!((row.space.text.as_str(), row.space.level), ("85%", Level::Watch), "the fullest pool of the group");
    assert_eq!(row.errors.text, "4", "errors add up");
}

#[test]
fn a_host_that_has_not_answered_makes_its_group_unknown_not_fine() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let fine = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let silent = HostInput { name: "b", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None, os_name: None, tag: "web" };
    let d = fx.build(vec![tagged(fine_host("a", &fine, &r, &b), "web"), silent]);
    let group = &d.groups.unwrap()[0];
    assert_eq!(group.row.state.level, Level::Unknown);
    assert_eq!(group.row.state.text, "1 unknown");
    assert_eq!(group.members.len(), 1);
}

#[test]
fn expanded_groups_list_every_host_not_only_the_ones_that_need_a_look() {
    let mut fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let hosts = || vec![tagged(fine_host("a", &probe, &r, &b), "web"), tagged(fine_host("b", &probe, &r, &b), "web")];
    assert!(fx.build(hosts()).groups.unwrap()[0].members.is_empty(), "folded by default");
    fx.expand_groups = true;
    let groups = fx.build(hosts()).groups.unwrap();
    assert_eq!(groups[0].members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(groups[0].row.name, "web (2)");
}

#[test]
fn a_group_knows_its_tag_and_the_lines_to_move_through_are_the_attention_lines_then_the_groups() {
    let fx = Fixture::new(view(vec![], &[], beat(1)));
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![pool("tank", 10, 0, None)]);
    let d = fx.build(vec![tagged(fine_host("a", &probe, &r, &b), "web"), fine_host("b", &probe, &r, &b)]);
    let groups = d.groups.clone().unwrap();
    assert_eq!((groups[0].tag.as_str(), groups[1].tag.as_str()), ("web", ""), "the untagged group has an empty tag");
    let n = d.attention.len();
    assert!(n >= 1, "never scrubbed gives each host a line");
    assert_eq!(d.selectable(), n + 3, "the lines, the two groups and the data-safety line");
    assert!(d.group_at(n - 1).is_none() && d.group_at(n).map(|g| g.tag.as_str()) == Some("web") && d.group_at(n + 1).map(|g| g.tag.as_str()) == Some("") && d.group_at(n + 2).is_none());
    assert!(d.is_safety(n + 2) && !d.is_safety(n + 1));
    let flat = fx.build(vec![fine_host("a", &probe, &r, &b)]);
    assert_eq!(flat.selectable(), flat.attention.len() + 1, "without tags the attention lines and the data-safety line");
}

#[test]
fn the_dashboard_says_where_the_weakest_data_stands_or_that_nothing_has_a_copy_and_says_nothing_before_the_jobs_are_loaded() {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let jobs = vec![Job::test_replicate("r", "tank/data", "rep", "fbsd16", "tank/copy")];
    let runs = [("r", RunResult::Ok, "")];
    let with = Fixture::new(view(jobs, &runs, beat(1))).build(vec![fine_host("local", &probe, &r, &b)]);
    let line = with.safety.clone().expect("a replication makes a body");
    assert!(line.text.starts_with("weakest: tank/data") || line.text.starts_with("weakest: local:tank/data"), "{}", line.text);
    assert_eq!(with.bodies.len(), 1);
    assert!(with.safety_popup().contains("tank/copy") && with.safety_popup().contains("replica"));
    let none = Fixture::new(view(vec![], &[], beat(1))).build(vec![fine_host("local", &probe, &r, &b)]);
    assert_eq!((none.safety.as_ref().map(|l| l.text.as_str()), none.safety.as_ref().map(|l| l.level)), (Some("no dataset has a copy yet"), Some(Level::Unknown)));
    assert!(none.safety_popup().starts_with("No dataset has a copy yet"));
    let unloaded = Fixture::new(JobsView::default()).build(vec![]);
    assert!(unloaded.safety.is_none() && unloaded.bodies.is_empty(), "no line while the jobs are not loaded");
}

#[test]
fn hosts_that_share_a_site_are_one_failure_domain_on_the_data_safety_line() {
    let (r, b) = (reading(10, 0, 0, 100), reading(10, 0, 0, 100));
    let probe = reached(vec![pool("tank", 10, 0, Some(when(1)))]);
    let jobs = vec![Job::test_replicate("r", "tank/data", "rep", "fbsd16", "tank/copy")];
    let runs = [("r", RunResult::Ok, "")];
    let mut fx = Fixture::new(view(jobs, &runs, beat(1)));
    let apart = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert!(apart.safety.as_ref().unwrap().text.contains("tolerates 1 failure"), "{:?}", apart.safety);
    let host = |name: &str| crate::config::HostConfig { name: name.into(), ssh_target: Some("u@h".into()), tag: None, site: Some("rack1".into()) };
    let config = crate::config::Config { hosts: vec![host("local"), host("fbsd16")], ..Default::default() };
    fx.protection = config.protection_settings().0;
    let together = fx.build(vec![fine_host("local", &probe, &r, &b)]);
    assert!(together.safety.as_ref().unwrap().text.contains("tolerates no failure"), "{:?}", together.safety);
    assert_eq!(together.safety.as_ref().unwrap().level, Level::Watch, "one failure takes the original and its only copy");
}
