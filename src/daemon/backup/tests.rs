//! Tests of `backup.rs`.
use super::*;
use crate::daemon::history::{runs, History};
use crate::daemon::scrub::{Clock, Context, Locks};
use crate::error::Result;
use crate::exec::CommandOutput;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

struct FixedClock(DateTime<Utc>);
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
    fn sleep(&self, _: Duration) {}
}

const A: &str = "hourly-20261008T100000Z";
const B: &str = "hourly-20261008T110000Z";
const OK_SCRIPT: &str = r#"echo '{"message_type":"status","percent_done":0.5,"total_bytes":1000,"bytes_done":500}'; echo '{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":40,"data_added":2048}'"#;

/// One host: answers zfs and restic queries from fixed strings, remembers whether the clone
/// exists, hands out a scripted shell for `restic backup`, and records every call.
// A scripted test double: its switches are independent flags, not a state machine.
#[allow(clippy::struct_excessive_bools)]
struct Host {
    listing: String,
    restic_snapshots: std::result::Result<String, String>,
    backup_script: String,
    date_works: bool,
    clone_fails: bool,
    /// `readlink` answers for these full paths as for a symlink (a target on stdout, exit 0); any other is exit 1.
    symlinks: Vec<String>,
    /// `readlink` is refused by sudo.
    readlink_refused: bool,
    /// `zfs snapshot` fails with this message.
    snapshot_error: Option<&'static str>,
    /// The source's `keystatus` when it is encrypted.
    keystatus: Option<&'static str>,
    /// Destroying a snapshot (not the clone) fails.
    snapshot_destroy_fails: bool,
    /// Snapshots (full names) that report one hold.
    held: Vec<String>,
    /// `zfs get ... userrefs` fails (the hold check cannot run).
    check_fails: bool,
    destroy_fails: bool,
    forget_fails: bool,
    /// What a successful `restic forget` does.
    forget_script: String,
    /// While true `restic forget` fails like a repository with a stale lock; `restic unlock` clears it.
    forget_locked: Mutex<bool>,
    unlock_fails: bool,
    /// `hostname` answers this (None: it fails).
    hostname: Option<&'static str>,
    /// The next N `zfs destroy` calls fail, then they succeed.
    destroy_flaky: Mutex<u32>,
    clone_exists: Mutex<bool>,
    calls: Mutex<Vec<String>>,
    backup_args: Mutex<Vec<Vec<String>>>,
}

impl Host {
    fn new() -> Self {
        Host {
            listing: format!("tank/data@{A}\t1\t10\ntank/data@{B}\t2\t20\n"),
            restic_snapshots: Ok("[]\n".to_string()),
            backup_script: OK_SCRIPT.to_string(),
            date_works: true,
            clone_fails: false,
            symlinks: vec![],
            readlink_refused: false,
            snapshot_error: None,
            keystatus: None,
            snapshot_destroy_fails: false,
            held: vec![],
            check_fails: false,
            destroy_fails: false,
            forget_fails: false,
            forget_script: "exit 0".to_string(),
            forget_locked: Mutex::new(false),
            unlock_fails: false,
            hostname: Some("fb"),
            destroy_flaky: Mutex::new(0),
            clone_exists: Mutex::new(false),
            calls: Mutex::new(vec![]),
            backup_args: Mutex::new(vec![]),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn called(&self, prefix: &str) -> bool {
        self.calls().iter().any(|c| c.starts_with(prefix))
    }
    fn clone_exists(&self) -> bool {
        *self.clone_exists.lock().unwrap()
    }
}

impl CommandRunner for Host {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
        let ok = |s: &str| Ok(CommandOutput { stdout: s.to_string(), stderr: String::new(), status: 0 });
        let fail = |s: &str| Ok(CommandOutput { stdout: String::new(), stderr: s.to_string(), status: 1 });
        match (cmd, args.first().copied()) {
            ("zfs", Some("list")) if args.contains(&"snapshot") => ok(&self.listing),
            ("zfs", Some("list")) => {
                if self.clone_exists() { ok("tank/zit-restic-job\n") } else { fail("dataset does not exist") }
            }
            ("zfs", Some("get")) if args.contains(&"name,property,value") => ok(&crate::daemon::guard::fake_answer("tank/data", self.keystatus)),
            ("zfs", Some("get")) if args.contains(&"userrefs") && self.check_fails => fail("get is broken"),
            ("zfs", Some("get")) if args.contains(&"userrefs") => {
                ok(&self.listing.lines().filter_map(|l| l.split('\t').next()).map(|n| format!("{n}\t{}\n", u8::from(self.held.iter().any(|h| h == n)))).collect::<Vec<_>>().concat())
            }
            ("zfs", Some("holds")) => ok(""),
            ("zfs", Some("get")) => ok("1791500000\n"),
            ("zfs", Some("clone")) => {
                if self.clone_fails {
                    return fail("cannot mount '/var/run/zfsctl/job': failed to create mountpoint");
                }
                *self.clone_exists.lock().unwrap() = true;
                ok("")
            }
            ("zfs", Some("snapshot")) => self.snapshot_error.map_or_else(|| ok(""), fail),
            ("zfs", Some("destroy")) if args.last().is_some_and(|a| a.contains('@')) => {
                if self.snapshot_destroy_fails { fail("snapshot has holds") } else { ok("") }
            }
            ("zfs", Some("destroy")) => {
                if self.destroy_fails {
                    return fail("dataset is busy");
                }
                let mut flaky = self.destroy_flaky.lock().unwrap();
                if *flaky > 0 {
                    *flaky -= 1;
                    return fail("dataset is busy");
                }
                drop(flaky);
                *self.clone_exists.lock().unwrap() = false;
                ok("")
            }
            ("restic", Some("snapshots")) => match &self.restic_snapshots {
                Ok(json) => ok(json),
                Err(e) => Ok(CommandOutput { stdout: String::new(), stderr: e.clone(), status: 12 }),
            },
            ("restic", Some("unlock")) => {
                if self.unlock_fails {
                    return fail("Fatal: unlock broke");
                }
                *self.forget_locked.lock().unwrap() = false;
                ok("")
            }
            ("readlink", Some(path)) => {
                if self.readlink_refused {
                    return Ok(CommandOutput { stdout: String::new(), stderr: "sudo: a password is required".into(), status: 1 });
                }
                if self.symlinks.iter().any(|l| l == path) { ok("/\n") } else { Ok(CommandOutput { stdout: String::new(), stderr: String::new(), status: 1 }) }
            }
            other => panic!("unexpected call {other:?}"),
        }
    }
    fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> Result<CommandOutput> {
        self.run(cmd, args)
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
        if cmd == "hostname" {
            return Ok(match self.hostname {
                Some(name) => CommandOutput { stdout: format!("{name}\n"), stderr: String::new(), status: 0 },
                None => CommandOutput { stdout: String::new(), stderr: "no hostname".to_string(), status: 1 },
            });
        }
        assert_eq!(cmd, "date");
        let status = i32::from(!self.date_works);
        Ok(CommandOutput { stdout: if self.date_works { "2026-10-08 19:33:20\n".to_string() } else { String::new() }, stderr: String::new(), status })
    }
    fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
        assert_eq!(cmd, "restic");
        self.calls.lock().unwrap().push(format!("restic {}", args.join(" ")));
        let script = if args.first() == Some(&"forget") {
            if *self.forget_locked.lock().unwrap() {
                "echo 'unable to create lock in backend: repository is already locked by PID 14568 on fb by root' >&2; exit 1".to_string()
            } else if self.forget_fails {
                "echo 'Fatal: unable to save snapshot' >&2; exit 1".to_string()
            } else {
                self.forget_script.clone()
            }
        } else {
            self.backup_args.lock().unwrap().push(args.iter().map(|a| (*a).to_string()).collect());
            self.backup_script.clone()
        };
        let mut c = Command::new("sh");
        c.args(["-c", &script]);
        Ok(c)
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-08T23:30:00Z").unwrap().with_timezone(&Utc)
}

struct Rig {
    state: tempfile::TempDir,
    locks: Locks,
    cancel: AtomicBool,
}

impl Rig {
    fn new() -> Self {
        Rig { state: tempfile::tempdir().unwrap(), locks: Locks::default(), cancel: AtomicBool::new(false) }
    }
    fn run(&self, host: &Host, job: &Job) -> (Outcome, Vec<crate::daemon::history::Run>) {
        let history = History::new(self.state.path().join("history.jsonl"));
        let (clock, stop) = (FixedClock(now()), AtomicBool::new(false));
        let ctx = Context { history: &history, clock: &clock, locks: &self.locks, poll: Duration::from_secs(1), stop: &stop };
        let progress = self.state.path().join("progress");
        let state = self.state.path().join("replication.json");
        let env = Env { ctx: &ctx, progress_dir: &progress, cancel: &self.cancel, replication_state: &state };
        let outcome = run_backup(&env, host, job, now(), false, "run-1");
        (outcome, runs(&history.read()))
    }
}

fn job() -> Job {
    Job::test_backup("job", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw")
}

#[test]
fn names_are_derived_from_the_pool_and_the_job_id() {
    assert_eq!(clone_name("tank/data/sub", "backup-1"), "tank/zit-restic-backup-1");
    assert_eq!(clone_name("tank", "b"), "tank/zit-restic-b");
    assert_eq!(mountpoint("backup-1"), "/var/run/zfsctl/backup-1");
}

#[test]
fn clone_name_for_uses_the_given_prefix() {
    assert_eq!(clone_name_for("zit-mirror", "tank/data", "j"), "tank/zit-mirror-j");
    assert_eq!(clone_name("tank/data", "j"), "tank/zit-restic-j");
}

#[test]
fn a_run_clones_the_newest_snapshot_backs_up_the_clone_destroys_it_and_reports_the_counts() {
    let rig = Rig::new();
    let host = Host::new();
    let (outcome, history) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert_eq!(outcome.message, format!("backed up {B} (3 new, 2 changed, 40 unmodified), 2.0 KiB added"));
    let calls = host.calls();
    let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call starting {prefix:?} in {calls:?}"));
    assert!(at("restic snapshots") < at("zfs clone") && at("zfs clone") < at("restic backup") && at("restic backup") < at("zfs destroy"));
    assert!(calls.contains(&format!("zfs clone -o mountpoint=/var/run/zfsctl/job -o readonly=on -o com.sun:auto-snapshot=false -- tank/data@{B} tank/zit-restic-job")), "{calls:?}");
    assert!(calls.contains(&"zfs destroy -r -- tank/zit-restic-job".to_string()), "recursive: a pool-wide recursive snapshot may have given the clone snapshots of its own");
    let backup = host.backup_args.lock().unwrap()[0].clone();
    assert!(backup.contains(&format!("--tag=zfsctl-snapshot:{B}")) && backup.contains(&"--tag=zfsctl:job".to_string()));
    assert!(backup.contains(&"--time=2026-10-08 19:33:20".to_string()), "{backup:?}");
    assert_eq!(backup.last().map(String::as_str), Some("/var/run/zfsctl/job"));
    assert!(!host.clone_exists());
    assert!(!host.called("restic forget"), "no retention, no forget");
    assert_eq!((history.len(), history[0].result), (1, Some(RunResult::Ok)));
    assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
}

const FRESH: &str = "zit-job-20261008T233000Z";

fn fresh_job() -> Job {
    let mut j = job();
    j.source = Some(crate::jobs::SOURCE_NEW.into());
    j
}

/// A `zfs list -t snapshot` answer for these snapshot names of `tank/data`.
fn listing(names: &[&str]) -> String {
    names.iter().map(|n| format!("tank/data@{n}\t1\t10\n")).collect::<Vec<_>>().concat()
}

fn destroyed_snapshots(host: &Host) -> Vec<String> {
    host.calls().iter().filter(|c| c.starts_with("zfs destroy -- ")).map(|c| c.trim_start_matches("zfs destroy -- ").to_string()).collect()
}

#[test]
fn a_new_source_takes_a_snapshot_then_backs_that_one_up_through_a_clone() {
    let rig = Rig::new();
    let host = Host::new();
    let (outcome, _) = rig.run(&host, &fresh_job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(outcome.message.starts_with(&format!("backed up {FRESH} ")), "{}", outcome.message);
    let calls = host.calls();
    let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap_or_else(|| panic!("no call starting {prefix:?} in {calls:?}"));
    assert!(at("zfs snapshot") < at("restic snapshots") && at("restic snapshots") < at("zfs clone"));
    assert!(calls.contains(&format!("zfs snapshot -- tank/data@{FRESH}")), "{calls:?}");
    assert!(calls.iter().any(|c| c.starts_with("zfs clone") && c.contains(&format!("tank/data@{FRESH} "))), "{calls:?}");
    assert!(host.backup_args.lock().unwrap()[0].contains(&format!("--tag=zfsctl-snapshot:{FRESH}")));
    assert!(destroyed_snapshots(&host).is_empty(), "nothing to prune yet: {:?}", host.calls());
}

#[test]
fn a_new_source_keeps_the_last_few_of_its_own_snapshots_and_touches_nothing_else() {
    let rig = Rig::new();
    let mut host = Host::new();
    let names = ["zit-job-20261001T000000Z", "zit-job-20261002T000000Z", "zit-job-20261003T000000Z", "zit-job-20261004T000000Z"];
    host.listing = listing(&[&names[..], &["manual", "zit-other-20261001T000000Z", "zit-job-notes", B]].concat());
    let mut j = fresh_job();
    j.keep = Some(3);
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    // keep 3 = the new one plus the two newest older ones: the two oldest go.
    assert_eq!(destroyed_snapshots(&host), vec!["tank/data@zit-job-20261001T000000Z".to_string(), "tank/data@zit-job-20261002T000000Z".to_string()]);
}

#[test]
fn a_new_source_with_no_keep_set_keeps_three() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = listing(&["zit-job-20261001T000000Z", "zit-job-20261002T000000Z", "zit-job-20261003T000000Z", "zit-job-20261004T000000Z", "zit-job-20261005T000000Z"]);
    let (_, _) = rig.run(&host, &fresh_job());
    assert_eq!(destroyed_snapshots(&host).len(), 3, "{:?}", host.calls());
}

#[test]
fn a_snapshot_that_cannot_be_pruned_is_reported_but_the_backup_still_counts() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = "tank/data@zit-job-20261001T000000Z\t1\t10\n".to_string();
    host.snapshot_destroy_fails = true;
    let mut j = fresh_job();
    j.keep = Some(1);
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(outcome.message.contains("could not remove the old snapshot zit-job-20261001T000000Z"), "{}", outcome.message);
}

#[test]
fn prune_fresh_leaves_a_held_snapshot_and_says_so() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = listing(&["zit-job-20261001T000000Z", "zit-job-20261002T000000Z", "zit-job-20261003T000000Z"]);
    host.held = vec!["tank/data@zit-job-20261001T000000Z".to_string()];
    let mut j = fresh_job();
    j.keep = Some(1);
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(outcome.message.contains("kept the old snapshot zit-job-20261001T000000Z (held)"), "{}", outcome.message);
    assert!(outcome.message.contains("has 1 hold"), "the reason is shown: {}", outcome.message);
    assert!(!outcome.message.contains("could not remove"), "{}", outcome.message);
    // the held one is never destroyed; the later excess ones still are
    assert_eq!(destroyed_snapshots(&host), vec!["tank/data@zit-job-20261002T000000Z".to_string(), "tank/data@zit-job-20261003T000000Z".to_string()]);
}

#[test]
fn prune_fresh_reports_a_hold_check_that_cannot_run_as_a_failed_removal() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = listing(&["zit-job-20261001T000000Z", "zit-job-20261002T000000Z"]);
    host.check_fails = true;
    let mut j = fresh_job();
    j.keep = Some(1);
    let (outcome, _) = rig.run(&host, &j);
    assert!(outcome.message.contains("could not remove the old snapshot zit-job-20261001T000000Z (could not check holds"), "{}", outcome.message);
    assert!(!outcome.message.contains("(held)"), "{}", outcome.message);
    assert!(destroyed_snapshots(&host).is_empty());
}

#[test]
fn a_new_source_that_cannot_snapshot_fails_before_restic_and_one_that_already_exists_is_skipped() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.snapshot_error = Some("cannot create snapshot: out of space");
    let (outcome, _) = rig.run(&host, &fresh_job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("cannot snapshot tank/data") && outcome.message.contains("out of space"), "{}", outcome.message);
    assert!(!host.called("restic") && !host.called("zfs clone"));
    host.snapshot_error = Some("dataset already exists");
    let (outcome, _) = rig.run(&host, &fresh_job());
    assert_eq!(outcome.result, RunResult::Skipped, "{}", outcome.message);
}

#[test]
fn a_pinned_snapshot_is_backed_up_whatever_it_is_called() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = format!("tank/data@snap4\t1\t10\ntank/data@{B}\t2\t20\n");
    let mut j = job();
    j.source = Some(crate::jobs::SOURCE_SNAPSHOT.into());
    j.snapshot = Some("snap4".into());
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(outcome.message.starts_with("backed up snap4 "), "{}", outcome.message);
    assert!(host.calls().iter().any(|c| c.starts_with("zfs clone") && c.contains("tank/data@snap4 ")));
    assert!(!host.called("zfs snapshot"), "a pinned source never takes a snapshot");
}

#[test]
fn a_pinned_snapshot_that_is_gone_fails_the_run_instead_of_skipping_it() {
    let rig = Rig::new();
    let host = Host::new();
    let mut j = job();
    j.source = Some(crate::jobs::SOURCE_SNAPSHOT.into());
    j.snapshot = Some("snap4".into());
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("tank/data@snap4 no longer exists"), "{}", outcome.message);
    assert!(!host.called("zfs clone") && !host.called("restic"));
}

#[test]
fn a_path_backs_up_that_directory_of_the_clone_and_nothing_else() {
    let rig = Rig::new();
    let host = Host::new();
    let mut j = job();
    j.path = Some("photos/2026/".into());
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert_eq!(host.backup_args.lock().unwrap()[0].last().map(String::as_str), Some("/var/run/zfsctl/job/photos/2026"));
    assert!(host.calls().iter().any(|c| c.contains("mountpoint=/var/run/zfsctl/job ")), "the clone itself is still mounted at the job's fixed root");
}

#[test]
fn a_path_through_a_symlink_is_refused_before_restic_reads_anything() {
    let rig = Rig::new();
    let mut host = Host::new();
    // `photos` is real, `photos/out` is a link (to `/`, say): restic would follow it as root.
    host.symlinks = vec!["/var/run/zfsctl/job/photos/out".into()];
    let mut j = job();
    j.path = Some("photos/out/etc".into());
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("goes through a symlink ('out')") && outcome.message.contains("nothing was backed up"), "{}", outcome.message);
    assert!(host.backup_args.lock().unwrap().is_empty());
    assert!(!host.clone_exists(), "the clone is destroyed");
    let checked: Vec<String> = host.calls().into_iter().filter(|c| c.starts_with("readlink")).collect();
    assert_eq!(checked, ["readlink /var/run/zfsctl/job/photos", "readlink /var/run/zfsctl/job/photos/out"], "it stops at the first link");
}

#[test]
fn a_host_whose_sudoers_does_not_allow_readlink_fails_the_run_safely_and_says_what_to_do() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.readlink_refused = true;
    let mut j = job();
    j.path = Some("photos".into());
    let (outcome, _) = rig.run(&host, &j);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("cannot check the path") && outcome.message.contains("print-sudoers") && outcome.message.contains("--with restic"), "{}", outcome.message);
    assert!(host.backup_args.lock().unwrap().is_empty());
    assert!(!host.clone_exists());
}

#[test]
fn a_job_without_a_path_runs_no_readlink_at_all() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.readlink_refused = true;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(!host.called("readlink"));
}

#[test]
fn without_a_working_date_the_backup_runs_without_dash_dash_time() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.date_works = false;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(!host.backup_args.lock().unwrap()[0].iter().any(|a| a.starts_with("--time")));
}

#[test]
fn a_source_without_matching_snapshots_is_skipped_and_nothing_is_created() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.listing = "tank/data@manual\t9\t1\n".to_string();
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Skipped);
    assert!(outcome.message.contains("no snapshots"), "{}", outcome.message);
    assert!(!host.called("zfs clone") && !host.called("restic"));
}

#[test]
fn a_snapshot_already_in_the_repository_is_skipped_before_any_clone() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.restic_snapshots = Ok("[{\"id\":\"abc\"}]".to_string());
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Skipped);
    assert!(outcome.message.contains("up to date") && outcome.message.contains(B), "{}", outcome.message);
    assert!(!host.called("zfs clone"));
}

#[test]
fn an_unreachable_or_wrongly_locked_repository_fails_the_run_before_cloning() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.restic_snapshots = Err("Fatal: wrong password or no key found".to_string());
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("wrong password"), "{}", outcome.message);
    assert!(!host.called("zfs clone"));
}

#[test]
fn a_leftover_clone_from_a_crashed_run_is_destroyed_first() {
    let rig = Rig::new();
    let host = Host::new();
    *host.clone_exists.lock().unwrap() = true;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    let calls = host.calls();
    let first_destroy = calls.iter().position(|c| c.starts_with("zfs destroy")).unwrap();
    let clone = calls.iter().position(|c| c.starts_with("zfs clone")).unwrap();
    assert!(first_destroy < clone, "{calls:?}");
}

#[test]
fn a_leftover_clone_that_cannot_be_removed_fails_the_run_without_backing_up() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.destroy_fails = true;
    *host.clone_exists.lock().unwrap() = true;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("left by an earlier run") && outcome.message.contains("busy"), "{}", outcome.message);
    assert!(!host.called("zfs clone") && !host.called("restic backup"));
}

#[test]
fn restic_backs_up_an_open_encrypted_source_and_stops_at_a_locked_one() {
    let rig = Rig::new();
    let mut open = Host::new();
    open.keystatus = Some("available");
    let (outcome, _) = rig.run(&open, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    let mut locked = Host::new();
    locked.keystatus = Some("unavailable");
    let (outcome, _) = rig.run(&locked, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert_eq!(outcome.message, "tank/data is locked (key not loaded): load the key first");
    assert!(!locked.called("zfs clone") && !locked.called("zfs snapshot") && !locked.called("restic"), "{:?}", locked.calls());
}

#[test]
fn a_failed_clone_is_a_failed_run_with_zfss_message_and_no_backup() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.clone_fails = true;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("failed to create mountpoint"), "{}", outcome.message);
    assert!(!host.called("restic backup"));
}

#[test]
fn a_failing_backup_is_failed_with_resticss_message_destroys_the_clone_and_never_forgets() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.backup_script = r#"echo '{"message_type":"summary","files_new":1,"files_changed":0,"files_unmodified":0,"data_added":1}'; echo '{"message_type":"exit_error","code":1,"message":"Fatal: unable to save snapshot: disk full"}'; exit 1"#.to_string();
    let mut with_policy = job();
    with_policy.retention = Some("7d".to_string());
    let (outcome, history) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Failed, "{}", outcome.message);
    assert!(outcome.message.contains("disk full"), "{}", outcome.message);
    assert!(!host.clone_exists());
    assert!(!host.called("restic forget"));
    assert_eq!(history[0].result, Some(RunResult::Failed));
}

#[test]
fn exit_code_three_means_the_snapshot_exists_but_files_were_unreadable_and_is_a_failure() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.backup_script = format!("{OK_SCRIPT}; echo 'error: permission denied' >&2; exit 3");
    let mut with_policy = job();
    with_policy.retention = Some("7d".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("could not read some files"), "{}", outcome.message);
    assert!(!host.clone_exists() && !host.called("restic forget"));
}

#[test]
fn a_cancelled_run_is_recorded_cancelled_destroys_the_clone_and_does_not_forget() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.backup_script = "sleep 60".to_string();
    rig.cancel.store(true, Ordering::SeqCst);
    let mut with_policy = job();
    with_policy.retention = Some("7d".to_string());
    let (outcome, history) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
    assert_eq!(history[0].result, Some(RunResult::Cancelled));
    assert!(!host.clone_exists() && !host.called("restic forget"));
    assert!(crate::daemon::progress::read_all(&rig.state.path().join("progress")).is_empty());
}

#[test]
fn a_successful_backup_with_a_policy_forgets_only_this_jobs_backups_afterwards() {
    let rig = Rig::new();
    let host = Host::new();
    let mut with_policy = job();
    with_policy.retention = Some("7d 4w".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    let calls = host.calls();
    let forget = calls.iter().find(|c| c.starts_with("restic forget")).expect("forget ran");
    assert_eq!(forget, "restic forget --repo=/mnt/usb/restic --password-file=/root/pw --retry-lock=10m --prune --tag=zfsctl:job --host=fb --keep-daily=7 --keep-weekly=4");
    assert!(calls.iter().position(|c| c.starts_with("zfs destroy")) < calls.iter().position(|c| c.starts_with("restic forget")), "the clone is gone before the slow prune");
}

#[test]
fn a_blank_policy_forgets_nothing() {
    let rig = Rig::new();
    let host = Host::new();
    let mut blank = job();
    blank.retention = Some("   ".to_string());
    rig.run(&host, &blank);
    assert!(!host.called("restic forget"));
}

#[test]
fn a_failed_forget_is_a_failed_run_that_says_the_backup_exists() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.forget_fails = true;
    let mut with_policy = job();
    with_policy.retention = Some("7d".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains(&format!("backed up {B}")) && outcome.message.contains("forget failed"), "{}", outcome.message);
}

#[test]
fn a_clone_that_cannot_be_destroyed_is_reported_and_the_next_run_retries() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.destroy_fails = true;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("could not remove the clone") && outcome.message.contains("next run"), "{}", outcome.message);
}

#[test]
fn the_repository_never_appears_in_a_run_message() {
    let rig = Rig::new();
    let host = Host::new();
    let mut secretive = job();
    secretive.repository = Some("sftp:me:hunter2@host:/srv/restic".to_string());
    let (ok, history) = rig.run(&host, &secretive);
    assert!(!ok.message.contains("hunter2") && !format!("{history:?}").contains("hunter2"));
    let mut failing = Host::new();
    failing.clone_fails = true;
    let (bad, _) = rig.run(&failing, &secretive);
    assert!(!bad.message.contains("hunter2"), "{}", bad.message);
}

#[test]
fn a_busy_job_is_skipped_and_touches_nothing() {
    let rig = Rig::new();
    let host = Host::new();
    let _held = rig.locks.try_acquire("local", "#bak@job").unwrap();
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Skipped);
    assert!(outcome.message.contains("already running"), "{}", outcome.message);
    assert!(host.calls().is_empty());
}

#[test]
fn two_jobs_for_one_dataset_use_separate_clones_and_mountpoints() {
    let mut other = job();
    other.id = "job-two".to_string();
    assert_ne!(clone_name("tank/data", &job().id), clone_name("tank/data", &other.id));
    assert_ne!(mountpoint(&job().id), mountpoint(&other.id));
    let rig = Rig::new();
    let _held = rig.locks.try_acquire("local", "#bak@job");
    let host = Host::new();
    let (outcome, _) = rig.run(&host, &other);
    assert_ne!(outcome.result, RunResult::Skipped, "the other job's lock does not block this one: {}", outcome.message);
}

#[test]
fn a_stale_lock_left_by_an_earlier_cancelled_run_is_cleared_before_forget_runs_once() {
    let rig = Rig::new();
    let host = Host::new();
    *host.forget_locked.lock().unwrap() = true;
    let mut with_policy = job();
    with_policy.retention = Some("1l".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    let calls = host.calls();
    let forgets: Vec<usize> = calls.iter().enumerate().filter(|(_, c)| c.starts_with("restic forget")).map(|(i, _)| i).collect();
    let unlocks: Vec<usize> = calls.iter().enumerate().filter(|(_, c)| c.starts_with("restic unlock")).map(|(i, _)| i).collect();
    assert_eq!((forgets.len(), unlocks.len()), (1, 1), "{calls:?}");
    assert!(unlocks[0] < forgets[0], "unlock must come first, or forget waits out --retry-lock: {calls:?}");
    assert_eq!(calls[unlocks[0]], "restic unlock --repo=/mnt/usb/restic --password-file=/root/pw", "only stale locks: no --remove-all");
}

#[test]
fn a_job_without_retention_never_unlocks() {
    let rig = Rig::new();
    let host = Host::new();
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(!host.called("restic unlock") && !host.called("restic forget"));
}

#[test]
fn a_forget_that_fails_for_another_reason_is_reported_and_not_retried() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.forget_fails = true;
    let mut with_policy = job();
    with_policy.retention = Some("1l".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Failed);
    assert_eq!(host.calls().iter().filter(|c| c.starts_with("restic forget")).count(), 1);
}

#[test]
fn a_failing_unlock_is_ignored_and_the_forgets_own_lock_error_is_reported() {
    let rig = Rig::new();
    let mut host = Host::new();
    *host.forget_locked.lock().unwrap() = true;
    host.unlock_fails = true;
    let mut with_policy = job();
    with_policy.retention = Some("1l".to_string());
    let (outcome, _) = rig.run(&host, &with_policy);
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(outcome.message.contains("already locked") && outcome.message.contains("forget failed"), "{}", outcome.message);
}

#[test]
fn the_backup_is_labelled_with_the_machines_real_name_and_the_already_done_query_is_limited_to_it() {
    let rig = Rig::new();
    let host = Host::new();
    rig.run(&host, &job());
    let calls = host.calls();
    assert!(calls.iter().any(|c| c.starts_with("restic snapshots") && c.contains("--host=fb")), "{calls:?}");
    assert!(host.backup_args.lock().unwrap()[0].contains(&"--host=fb".to_string()));
}

#[test]
fn without_a_hostname_the_job_hosts_label_is_used() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.hostname = None;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert!(host.backup_args.lock().unwrap()[0].contains(&"--host=local".to_string()));
}

#[test]
fn the_clone_opts_out_of_auto_snapshot_tools() {
    let rig = Rig::new();
    let host = Host::new();
    rig.run(&host, &job());
    assert!(host.calls().iter().any(|c| c.starts_with("zfs clone") && c.contains("-o com.sun:auto-snapshot=false")));
}

#[test]
fn a_destroy_that_fails_briefly_is_retried_so_a_slow_to_die_remote_restic_does_not_strand_the_clone() {
    let rig = Rig::new();
    let host = Host::new();
    *host.destroy_flaky.lock().unwrap() = 2;
    let (outcome, _) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Ok, "{}", outcome.message);
    assert_eq!(host.calls().iter().filter(|c| c.starts_with("zfs destroy")).count(), 3);
    assert!(!host.clone_exists());
}

#[test]
fn a_destroy_that_keeps_failing_is_given_up_on_after_a_few_tries() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.destroy_fails = true;
    rig.run(&host, &job());
    let tries = host.calls().iter().filter(|c| c.starts_with("zfs destroy")).count();
    assert!((2..=6).contains(&tries), "{tries}");
}

#[test]
fn the_repository_is_removed_from_restics_own_error_text() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.restic_snapshots = Err("Fatal: repository does not exist: unable to open config file\nIs there a repository at the following location?\n/mnt/usb/restic".to_string());
    let (outcome, history) = rig.run(&host, &job());
    assert_eq!(outcome.result, RunResult::Failed);
    assert!(!outcome.message.contains("/mnt/usb/restic") && outcome.message.contains("<repository>"), "{}", outcome.message);
    assert!(!format!("{history:?}").contains("/mnt/usb/restic"));
    let mut failing = Host::new();
    failing.backup_script = "echo 'Fatal: cannot write to /mnt/usb/restic/data' >&2; exit 1".to_string();
    let (outcome, _) = rig.run(&failing, &job());
    assert!(!outcome.message.contains("/mnt/usb/restic"), "{}", outcome.message);
}


#[test]
fn cancelling_during_retention_stops_the_prune_and_keeps_the_finished_backup() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.forget_script = "sleep 60".to_string();
    let mut with_policy = job();
    with_policy.retention = Some("1l".to_string());
    let (outcome, history) = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !host.called("restic forget") {
                std::thread::sleep(Duration::from_millis(20));
            }
            rig.cancel.store(true, Ordering::SeqCst);
        });
        rig.run(&host, &with_policy)
    });
    assert_eq!(outcome.result, RunResult::Cancelled, "{}", outcome.message);
    assert!(outcome.message.contains(&format!("backed up {B}")) && outcome.message.contains("retention cancelled"), "{}", outcome.message);
    assert_eq!(history.last().and_then(|r| r.result), Some(RunResult::Cancelled));
    assert!(!host.clone_exists());
}


#[test]
fn a_cancelled_retention_says_it_is_applied_after_the_next_successful_backup() {
    let rig = Rig::new();
    let mut host = Host::new();
    host.forget_script = "sleep 60".to_string();
    let mut with_policy = job();
    with_policy.retention = Some("1l".to_string());
    let (outcome, _) = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !host.called("restic forget") {
                std::thread::sleep(Duration::from_millis(20));
            }
            rig.cancel.store(true, Ordering::SeqCst);
        });
        rig.run(&host, &with_policy)
    });
    assert!(outcome.message.contains("applied after the next successful backup"), "{}", outcome.message);
    assert!(!outcome.message.contains("the next run applies it"), "the next run is usually 'up to date' and never forgets");
}
