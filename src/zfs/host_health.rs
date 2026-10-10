//! Whether a managed host is reachable and healthy, from one cheap probe (`zpool list` + `zpool status`),
//! and the pure rules that turn a probe (plus "did a job of this host fail?") into a level and a reason.

use super::pool::{parse_pool_scan_status, Pool, PoolHealth, ScanStatus};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSummary {
    Never,
    Running { percent: Option<u8> },
    /// `zpool scrub -p`: neither running nor finished.
    Paused,
    /// The last scrub or resilver; `when` is as `zpool status` prints it (no time zone, so no age).
    Finished { when: String, errors: u64 },
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolProbe {
    pub name: String,
    pub health: PoolHealth,
    pub capacity_percent: u8,
    pub frag_percent: Option<u8>,
    /// Read + write + checksum errors of the pool's top-level line; `None` when `zpool status` was unavailable.
    pub errors: Option<u64>,
    pub scan: Option<ScanSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKind {
    Metal,
    /// The hypervisor name as `kern.vm_guest` reports it (kept for later; the table shows only `vm`).
    Vm(String),
    Jail,
    Unknown,
}

impl HostKind {
    pub fn word(&self) -> &'static str {
        match self {
            HostKind::Metal => "metal",
            HostKind::Vm(_) => "vm",
            HostKind::Jail => "jail",
            HostKind::Unknown => "-",
        }
    }
}

/// Reads `sysctl -n kern.vm_guest security.jail.jailed`: line 1 the guest kind, line 2 whether this is a jail.
pub fn host_kind_from_sysctl(out: &str) -> HostKind {
    let mut lines = out.lines().map(str::trim);
    let (Some(guest), Some(jailed)) = (lines.next(), lines.next()) else { return HostKind::Unknown };
    if !matches!(jailed, "0" | "1") || guest.is_empty() || guest.contains(char::is_whitespace) {
        return HostKind::Unknown;
    }
    if jailed == "1" {
        HostKind::Jail
    } else if guest == "none" {
        HostKind::Metal
    } else {
        HostKind::Vm(guest.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostProbe {
    /// The SSH client could not reach the host (exit status 255).
    Disconnected,
    /// The host answered at the network level but ssh refused the login (key not accepted, host key unknown or changed).
    LoginRefused { reason: &'static str },
    /// The host answered but cannot be inspected (sudo refused, no `zpool`, unparsable output).
    Unavailable { reason: String },
    /// `kind` is `Some` only on a probe that was asked to detect the type (the first successful one).
    Reached { rtt_ms: u64, pools: Vec<PoolProbe>, kind: Option<HostKind> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Probing,
    Disconnected,
    Critical,
    Warning,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub level: Level,
    pub reason: String,
}

const LIST_ARGS: &[&str] = &["list", "-Hp", "-o", "name,size,alloc,free,frag,health"];

/// Parses `zpool list -Hp -o name,size,alloc,free,frag,health`; fragmentation `-` is `None`.
pub fn parse_pool_list_frag(output: &str) -> Result<Vec<(Pool, Option<u8>)>> {
    let mut rows = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        let &[name, size, alloc, free, frag, health] = fields.as_slice() else {
            return Err(AppError::Parse(format!("expected 6 fields in zpool list line: {line:?}")));
        };
        let num = |s: &str| super::parse_numeric_or_dash(s).ok_or_else(|| AppError::Parse(format!("bad number {s:?} in: {line:?}")));
        let pool = Pool { name: name.to_string(), size: num(size)?, allocated: num(alloc)?, free: num(free)?, health: PoolHealth::parse(health) };
        rows.push((pool, frag.parse::<u8>().ok()));
    }
    Ok(rows)
}

/// What `zpool status` says about one pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFacts {
    pub name: String,
    pub errors: Option<u64>,
    pub scan: ScanSummary,
}

/// A READ/WRITE/CKSUM token: a number, or a count with a suffix (`1.2K` is 1200). Anything else non-zero is at least 1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a rounded, non-negative display count
fn count_token(token: &str) -> u64 {
    if let Ok(n) = token.parse() {
        return n;
    }
    let (digits, factor) = match token.chars().last() {
        Some('K') => (&token[..token.len() - 1], 1e3),
        Some('M') => (&token[..token.len() - 1], 1e6),
        Some('G') => (&token[..token.len() - 1], 1e9),
        Some('T') => (&token[..token.len() - 1], 1e12),
        _ => (token, 0.0),
    };
    digits.parse::<f64>().ok().filter(|_| factor > 0.0).map_or_else(|| u64::from(token != "0"), |n| (n * factor).round() as u64)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100
fn scan_summary(block: &str) -> ScanSummary {
    match parse_pool_scan_status(block) {
        None => ScanSummary::Never,
        Some(ScanStatus::InProgress { percent_done, .. }) => ScanSummary::Running { percent: percent_done.map(|p| p.clamp(0.0, 100.0) as u8) },
        Some(ScanStatus::Completed { summary, errors }) => {
            if summary.starts_with("scrub paused") || summary.starts_with("resilver paused") {
                return ScanSummary::Paused;
            }
            if summary.contains("canceled") {
                return ScanSummary::Canceled;
            }
            let when = summary.split_once(" on ").map_or_else(|| summary.clone(), |(_, w)| w.trim().to_string());
            let n = summary.split(" with ").nth(1).and_then(|rest| rest.split_whitespace().next()).and_then(|t| t.parse().ok());
            ScanSummary::Finished { when, errors: n.unwrap_or_else(|| u64::from(errors)) }
        }
    }
}

/// Splits `zpool status` (all pools) at its `pool:` lines and reads each block on its own.
pub fn parse_status_all(output: &str) -> Vec<StatusFacts> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in output.lines() {
        if let Some(name) = line.trim_start().strip_prefix("pool:") {
            blocks.push((name.trim().to_string(), vec![line]));
        } else if let Some((_, lines)) = blocks.last_mut() {
            lines.push(line);
        }
    }
    blocks
        .into_iter()
        .map(|(name, lines)| {
            let text = lines.join("\n");
            // Every row of the config tree counts, not just the pool's own line: ZFS corrects errors from one
            // mirror leg and reports a clean pool while that disk is failing.
            let mut rows = lines.iter().skip_while(|l| !l.trim_start().starts_with("NAME")).skip(1).take_while(|l| !l.trim().is_empty() && !l.trim_start().starts_with("errors:")).peekable();
            let has_rows = rows.peek().is_some();
            let device_errors: u64 = rows
                .filter_map(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    (f.len() >= 5).then(|| f.get(2..5).map_or(0, |t| t.iter().map(|x| count_token(x)).sum::<u64>()))
                })
                .sum();
            let data_errors = lines.iter().find_map(|l| l.trim_start().strip_prefix("errors:")).map_or(0, |rest| {
                let rest = rest.trim();
                if rest.starts_with("No known data errors") || rest.is_empty() {
                    0
                } else {
                    rest.split_whitespace().next().and_then(|t| t.parse().ok()).unwrap_or(1)
                }
            });
            let errors = has_rows.then(|| device_errors + data_errors);
            StatusFacts { scan: scan_summary(&text), name, errors }
        })
        .collect()
}

/// `probe_host`, but a panic inside it becomes `Unavailable` instead of a worker thread that never reports back.
pub fn probe_host_guarded(runner: &dyn CommandRunner, detect_kind: bool) -> HostProbe {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe_host(runner, detect_kind))).unwrap_or_else(|_| HostProbe::Unavailable { reason: "the probe crashed".to_string() })
}

fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

/// One probe: `zpool list` first (its duration is the round trip); an SSH-level failure ends it there.
pub fn probe_host(runner: &dyn CommandRunner, detect_kind: bool) -> HostProbe {
    let started = Instant::now();
    let listed = match runner.run("zpool", LIST_ARGS) {
        Ok(out) => out,
        Err(e) => return HostProbe::Unavailable { reason: e.to_string() },
    };
    let rtt_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if listed.status == 255 {
        let said = listed.stderr.to_lowercase();
        if said.contains("permission denied") {
            return HostProbe::LoginRefused { reason: "ssh login refused" };
        }
        if said.contains("host key verification failed") || said.contains("remote host identification has changed") {
            return HostProbe::LoginRefused { reason: "host key not accepted" };
        }
        return HostProbe::Disconnected;
    }
    if !listed.success() {
        let why = first_line(&listed.stderr);
        return HostProbe::Unavailable { reason: if why.is_empty() { format!("zpool list exited {}", listed.status) } else { why } };
    }
    let rows = match parse_pool_list_frag(&listed.stdout) {
        Ok(rows) => rows,
        Err(e) => return HostProbe::Unavailable { reason: e.to_string() },
    };
    let facts = runner.run("zpool", &["status"]).ok().filter(crate::exec::CommandOutput::success).map(|o| parse_status_all(&o.stdout));
    let pools = rows
        .into_iter()
        .map(|(pool, frag)| {
            let status = facts.as_ref().and_then(|all| all.iter().find(|f| f.name == pool.name));
            let capacity = if pool.size == 0 { 0 } else { u8::try_from(u128::from(pool.allocated) * 100 / u128::from(pool.size)).unwrap_or(100).min(100) };
            PoolProbe { name: pool.name, health: pool.health, capacity_percent: capacity, frag_percent: frag, errors: status.and_then(|s| s.errors), scan: status.map(|s| s.scan.clone()) }
        })
        .collect();
    let kind = detect_kind.then(|| runner.run_unprivileged("sysctl", &["-n", "kern.vm_guest", "security.jail.jailed"]).ok().filter(crate::exec::CommandOutput::success).map_or(HostKind::Unknown, |o| host_kind_from_sysctl(&o.stdout)));
    HostProbe::Reached { rtt_ms, pools, kind }
}

fn health_text(health: &PoolHealth) -> String {
    match health {
        PoolHealth::Other(s) => s.clone(),
        other => format!("{other:?}").to_uppercase(),
    }
}

/// The level and the reason to show for a host. The worst applicable level wins, and its reason is the
/// first applicable one, in the order: not ONLINE, 90% full | errors, scrub errors, 80% full, job failed.
pub fn assess(probe: Option<&HostProbe>, job_failed: bool) -> Assessment {
    let say = |level, reason: &str| Assessment { level, reason: reason.to_string() };
    let Some(probe) = probe else { return say(Level::Probing, "probing") };
    let (pools, rtt_known) = match probe {
        HostProbe::Disconnected => return say(Level::Disconnected, "unreachable"),
        HostProbe::LoginRefused { reason } => return say(Level::Disconnected, reason),
        HostProbe::Unavailable { reason } => return say(Level::Warning, &format!("cannot inspect: {reason}")),
        HostProbe::Reached { pools, rtt_ms, .. } => (pools, *rtt_ms),
    };
    let _ = rtt_known;
    if let Some(p) = pools.iter().find(|p| p.health != PoolHealth::Online) {
        return say(Level::Critical, &format!("{} {}", p.name, health_text(&p.health)));
    }
    if let Some(p) = pools.iter().find(|p| p.capacity_percent >= 90) {
        return say(Level::Critical, &format!("{} {}% full", p.name, p.capacity_percent));
    }
    if let Some(p) = pools.iter().find(|p| p.errors.is_some_and(|e| e > 0)) {
        let n = p.errors.unwrap_or(0);
        return say(Level::Warning, &format!("{} {n} error{}", p.name, if n == 1 { "" } else { "s" }));
    }
    if let Some(p) = pools.iter().find(|p| matches!(p.scan, Some(ScanSummary::Finished { errors, .. }) if errors > 0)) {
        return say(Level::Warning, &format!("scrub found errors on {}", p.name));
    }
    if let Some(p) = pools.iter().find(|p| p.capacity_percent >= 80) {
        return say(Level::Warning, &format!("{} {}% full", p.name, p.capacity_percent));
    }
    if job_failed {
        return say(Level::Warning, "job failed");
    }
    say(Level::Ok, "ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::CommandOutput;
    use crate::zfs::pool::PoolHealth;
    use std::sync::Mutex;

    // Real `zpool list -Hp -o name,size,alloc,free,frag,health` and `zpool status` output, hub, 2026-10-09.
    const LIST: &str = "scratch\t251658240\t106573824\t145084416\t39\tONLINE\ntpool\t20937965568\t8663924736\t12274040832\t2\tONLINE\nzroot\t45634027520\t11182317568\t34451709952\t33\tONLINE\n";
    const STATUS: &str = "  pool: scratch\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:01 with 0 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME                            STATE     READ WRITE CKSUM\n\tscratch                         ONLINE       0     0     0\n\t  mirror-0                      ONLINE       0     0     0\n\t    /root/zfsctl-scratch/vdev3  ONLINE       0     0     0\n\nerrors: No known data errors\n\n  pool: tpool\n state: ONLINE\n  scan: resilvered 501M in 00:00:30 with 0 errors on Sat Sep 26 13:15:14 2026\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttpool       ONLINE       0     0     0\n\t  raidz2-0  ONLINE       0     0     0\n\nerrors: No known data errors\n\n  pool: zroot\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\tzroot       ONLINE       0     0     0\n\t  ada0p4    ONLINE       0     0     0\n\nerrors: No known data errors\n";

    fn pool(name: &str, health: PoolHealth, capacity: u8) -> PoolProbe {
        PoolProbe { name: name.to_string(), health, capacity_percent: capacity, frag_percent: Some(1), errors: Some(0), scan: Some(ScanSummary::Never) }
    }

    fn reached(pools: Vec<PoolProbe>) -> HostProbe {
        HostProbe::Reached { rtt_ms: 5, pools, kind: None }
    }

    #[test]
    fn the_pool_list_with_fragmentation_parses_including_a_dash() {
        let rows = parse_pool_list_frag(LIST).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].0.name.as_str(), rows[0].0.size, rows[0].0.allocated, rows[0].1), ("scratch", 251_658_240, 106_573_824, Some(39)));
        assert_eq!(rows[2].1, Some(33));
        let dash = parse_pool_list_frag("tank\t100\t10\t90\t-\tONLINE\n").unwrap();
        assert_eq!(dash[0].1, None, "a dash means unknown fragmentation");
        assert!(parse_pool_list_frag("tank\t100\t10\n").is_err());
        assert_eq!(parse_pool_list_frag("").unwrap().len(), 0);
    }

    #[test]
    fn zpool_status_gives_each_pools_errors_and_last_scrub() {
        let facts = parse_status_all(STATUS);
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].name, "scratch");
        assert_eq!(facts[0].errors, Some(0));
        assert_eq!(facts[0].scan, ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".to_string(), errors: 0 });
        assert_eq!(facts[1].scan, ScanSummary::Finished { when: "Sat Sep 26 13:15:14 2026".to_string(), errors: 0 }, "a resilver counts as the last scan");
        assert_eq!(facts[2].scan, ScanSummary::Never, "no scan: line at all");
    }

    #[test]
    fn scan_variants_and_error_counts_are_read_from_their_own_pool_block() {
        let status = "  pool: a\n state: DEGRADED\n  scan: scrub in progress since Thu Oct  8 19:00:00 2026\n\t1G scanned at 100M/s, 500M issued at 50M/s, 2G total\n\t0B repaired, 24.40% done, 00:00:30 to go\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     DEGRADED  3  0  1.2K\n\n  pool: ab\n state: ONLINE\n  scan: scrub repaired 0B in 00:00:01 with 4 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tab    ONLINE  0  0  0\n\n  pool: c\n state: ONLINE\n  scan: scrub canceled on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tc     ONLINE  0  0  0\n";
        let facts = parse_status_all(status);
        assert_eq!(facts.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["a", "ab", "c"], "a pool whose name is a prefix of another stays separate");
        assert!(matches!(facts[0].scan, ScanSummary::Running { percent: Some(24) }), "{:?}", facts[0].scan);
        assert_eq!(facts[0].errors, Some(3 + 1200), "READ 3 plus a CKSUM of 1.2K");
        assert_eq!(facts[1].scan, ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".to_string(), errors: 4 });
        assert_eq!(facts[2].scan, ScanSummary::Canceled);
    }

    /// A runner answering `zpool list` and `zpool status` from fixed outputs, recording the commands.
    struct Answers {
        list: CommandOutput,
        status: CommandOutput,
        calls: Mutex<Vec<String>>,
        sysctl: CommandOutput,
        unprivileged: Mutex<Vec<(String, Vec<String>)>>,
    }

    fn out(stdout: &str, stderr: &str, status: i32) -> CommandOutput {
        CommandOutput { stdout: stdout.to_string(), stderr: stderr.to_string(), status }
    }

    impl crate::exec::CommandRunner for Answers {
        fn run(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.first().copied().unwrap_or("")));
            match (cmd, args.first().copied()) {
                ("zpool", Some("list")) => Ok(self.list.clone()),
                ("zpool", Some("status")) => Ok(self.status.clone()),
                other => panic!("unexpected {other:?}"),
            }
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.unprivileged.lock().unwrap().push((cmd.to_string(), args.iter().map(ToString::to_string).collect()));
            match cmd {
                "sysctl" => Ok(self.sysctl.clone()),
                _ => self.run(cmd, args),
            }
        }
    }

    fn answers(list: CommandOutput, status: CommandOutput) -> Answers {
        Answers { list, status, calls: Mutex::new(vec![]), sysctl: out("bhyve\n0\n", "", 0), unprivileged: Mutex::new(vec![]) }
    }

    #[test]
    fn a_reachable_host_gives_pools_with_their_status_joined_by_name() {
        let runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        let HostProbe::Reached { pools, .. } = probe_host(&runner, false) else { panic!() };
        assert_eq!(pools.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["scratch", "tpool", "zroot"]);
        assert_eq!(pools[0].capacity_percent, 42);
        assert_eq!(pools[0].frag_percent, Some(39));
        assert_eq!(pools[0].errors, Some(0));
        assert_eq!(pools[2].scan, Some(ScanSummary::Never));
    }

    #[test]
    fn the_sysctl_answer_is_read_as_metal_vm_jail_or_unknown() {
        assert_eq!(host_kind_from_sysctl("none\n0\n"), HostKind::Metal);
        assert_eq!(host_kind_from_sysctl("bhyve\n0\n"), HostKind::Vm("bhyve".into()));
        assert_eq!(host_kind_from_sysctl("kvm\n1\n"), HostKind::Jail, "a jail wins over the guest value");
        assert_eq!(host_kind_from_sysctl(""), HostKind::Unknown);
        assert_eq!(host_kind_from_sysctl("sysctl: unknown oid\n"), HostKind::Unknown);
        assert_eq!(HostKind::Metal.word(), "metal");
        assert_eq!(HostKind::Vm("xen".into()).word(), "vm");
        assert_eq!(HostKind::Unknown.word(), "-");
    }

    #[test]
    fn detection_runs_one_unprivileged_sysctl_only_when_asked() {
        let runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        let HostProbe::Reached { kind, .. } = probe_host(&runner, true) else { panic!() };
        assert_eq!(kind, Some(HostKind::Vm("bhyve".into())));
        let unpriv = runner.unprivileged.lock().unwrap().clone();
        assert_eq!(unpriv, vec![("sysctl".to_string(), vec!["-n".to_string(), "kern.vm_guest".to_string(), "security.jail.jailed".to_string()])]);
        let HostProbe::Reached { kind, .. } = probe_host(&runner, false) else { panic!() };
        assert_eq!(kind, None);
        assert_eq!(runner.unprivileged.lock().unwrap().len(), 1, "not asked, not run");
    }

    #[test]
    fn a_failed_sysctl_is_an_unknown_type_not_a_failed_probe() {
        let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        runner.sysctl = out("", "sysctl: unknown oid 'kern.vm_guest'", 1);
        let HostProbe::Reached { kind, .. } = probe_host(&runner, true) else { panic!("the probe must still succeed") };
        assert_eq!(kind, Some(HostKind::Unknown));
    }

    #[test]
    fn an_ssh_level_failure_is_disconnected_and_skips_the_second_command() {
        let runner = answers(out("", "ssh: connect to host 10.0.0.9 port 22: Connection timed out", 255), out(STATUS, "", 0));
        assert_eq!(probe_host(&runner, false), HostProbe::Disconnected);
        assert_eq!(runner.calls.lock().unwrap().len(), 1, "one timeout, not two");
    }

    #[test]
    fn an_ssh_login_failure_is_told_apart_from_an_unreachable_host() {
        for (stderr, want) in [
            ("fbsd@10.0.0.9: Permission denied (publickey,password).", "ssh login refused"),
            ("Host key verification failed.", "host key not accepted"),
            ("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\n", "host key not accepted"),
        ] {
            let runner = answers(out("", stderr, 255), out(STATUS, "", 0));
            assert_eq!(probe_host(&runner, false), HostProbe::LoginRefused { reason: want }, "{stderr}");
            assert_eq!(assess(Some(&probe_host(&runner, false)), false), Assessment { level: Level::Disconnected, reason: want.to_string() });
        }
    }

    #[test]
    fn a_paused_scrub_is_paused_not_finished() {
        let status = "  pool: a\n state: ONLINE\n  scan: scrub paused since Thu Oct  8 19:00:00 2026\n\tscrub started on Thu Oct  8 18:00:00 2026\n\t1G scanned, 50.00% done\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     ONLINE  0  0  0\n";
        assert_eq!(parse_status_all(status)[0].scan, ScanSummary::Paused);
    }

    #[test]
    fn any_other_failure_is_unavailable_with_the_reason() {
        let sudo = answers(out("", "sudo: a password is required\n", 1), out("", "", 0));
        assert_eq!(probe_host(&sudo, false), HostProbe::Unavailable { reason: "sudo: a password is required".to_string() });
        let none = answers(out("", "", 127), out("", "", 0));
        assert!(matches!(probe_host(&none, false), HostProbe::Unavailable { reason } if reason.contains("127")));
        let garbled = answers(out("tank\t1\n", "", 0), out("", "", 0));
        assert!(matches!(probe_host(&garbled, false), HostProbe::Unavailable { .. }));
    }

    #[test]
    fn a_process_that_cannot_be_spawned_is_unavailable_not_disconnected() {
        struct Broken;
        impl crate::exec::CommandRunner for Broken {
            fn run(&self, cmd: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
                Err(crate::error::AppError::Spawn { cmd: cmd.to_string(), source: std::io::Error::other("no such file") })
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        assert!(matches!(probe_host(&Broken, false), HostProbe::Unavailable { reason } if reason.contains("zpool")));
    }

    #[test]
    fn when_zpool_status_fails_the_pools_still_count_but_errors_and_scans_are_unknown() {
        let runner = answers(out(LIST, "", 0), out("", "boom", 1));
        let HostProbe::Reached { pools, .. } = probe_host(&runner, false) else { panic!() };
        assert_eq!(pools.len(), 3);
        assert!(pools.iter().all(|p| p.errors.is_none() && p.scan.is_none()));
    }

    fn level(probe: Option<&HostProbe>, job_failed: bool) -> (Level, String) {
        let a = assess(probe, job_failed);
        (a.level, a.reason)
    }

    #[test]
    fn health_levels_and_reasons() {
        assert_eq!(level(None, false), (Level::Probing, "probing".to_string()));
        assert_eq!(level(Some(&HostProbe::Disconnected), true), (Level::Disconnected, "unreachable".to_string()));
        assert_eq!(level(Some(&HostProbe::Unavailable { reason: "sudo: a password is required".into() }), false), (Level::Warning, "cannot inspect: sudo: a password is required".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("tank", PoolHealth::Online, 10)])), false), (Level::Ok, "ok".to_string()));
        assert_eq!(level(Some(&reached(vec![])), false), (Level::Ok, "ok".to_string()), "no pools is not a problem");
    }

    #[test]
    fn capacity_boundaries_are_79_80_89_90() {
        let at = |c| level(Some(&reached(vec![pool("tank", PoolHealth::Online, c)])), false);
        assert_eq!(at(79).0, Level::Ok);
        assert_eq!(at(80), (Level::Warning, "tank 80% full".to_string()));
        assert_eq!(at(89).0, Level::Warning);
        assert_eq!(at(90), (Level::Critical, "tank 90% full".to_string()));
        assert_eq!(at(100).0, Level::Critical);
    }

    #[test]
    fn a_pool_that_is_not_online_is_critical_and_outranks_everything_else() {
        let mut p = pool("tank", PoolHealth::Degraded, 95);
        p.errors = Some(5);
        assert_eq!(level(Some(&reached(vec![p])), true), (Level::Critical, "tank DEGRADED".to_string()), "the first reason of the worst level");
        for health in [PoolHealth::Faulted, PoolHealth::Offline, PoolHealth::Unavail, PoolHealth::Removed] {
            assert_eq!(level(Some(&reached(vec![pool("t", health, 1)])), false).0, Level::Critical);
        }
        assert_eq!(level(Some(&reached(vec![pool("t", PoolHealth::Other("SUSPENDED".into()), 1)])), false), (Level::Critical, "t SUSPENDED".to_string()));
    }

    #[test]
    fn warnings_in_order_errors_then_scrub_then_capacity_then_job() {
        let mut errs = pool("a", PoolHealth::Online, 85);
        errs.errors = Some(3);
        errs.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 2 });
        assert_eq!(level(Some(&reached(vec![errs])), true), (Level::Warning, "a 3 errors".to_string()));
        let mut scrub = pool("b", PoolHealth::Online, 85);
        scrub.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 2 });
        assert_eq!(level(Some(&reached(vec![scrub])), true), (Level::Warning, "scrub found errors on b".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("c", PoolHealth::Online, 85)])), true), (Level::Warning, "c 85% full".to_string()));
        assert_eq!(level(Some(&reached(vec![pool("d", PoolHealth::Online, 10)])), true), (Level::Warning, "job failed".to_string()));
        let mut clean = pool("e", PoolHealth::Online, 10);
        clean.scan = Some(ScanSummary::Finished { when: "x".into(), errors: 0 });
        clean.errors = Some(0);
        assert_eq!(level(Some(&reached(vec![clean])), false).0, Level::Ok);
        let mut unknown = pool("f", PoolHealth::Online, 10);
        unknown.errors = None;
        unknown.scan = None;
        assert_eq!(level(Some(&reached(vec![unknown])), false).0, Level::Ok, "unknown errors/scan never raise a warning");
    }

    #[test]
    fn the_worst_pool_decides_when_there_are_several() {
        let probe = reached(vec![pool("fine", PoolHealth::Online, 10), pool("full", PoolHealth::Online, 91), pool("warm", PoolHealth::Online, 85)]);
        assert_eq!(level(Some(&probe), false), (Level::Critical, "full 91% full".to_string()));
    }

    const MIRROR_BAD_LEAF: &str = "  pool: tank\n state: ONLINE\nstatus: One or more devices has experienced an unrecoverable error.\n  scan: scrub repaired 0B in 00:00:01 with 0 errors on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  mirror-0  ONLINE       0     0     0\n\t    ada1    ONLINE       0     0    12\n\t    ada2    ONLINE       0     0     0\n\nerrors: No known data errors\n";

    #[test]
    fn errors_on_a_member_device_count_even_when_the_pool_line_is_clean() {
        assert_eq!(parse_status_all(MIRROR_BAD_LEAF)[0].errors, Some(12), "ZFS corrected them, but the disk is failing");
    }

    #[test]
    fn permanent_data_errors_are_errors_and_a_clean_pool_is_still_zero() {
        let bad = MIRROR_BAD_LEAF.replace("    12", "     0").replace("No known data errors", "3 data errors, use '-v' for a list");
        assert_eq!(parse_status_all(&bad)[0].errors, Some(3));
        let clean = MIRROR_BAD_LEAF.replace("    12", "     0");
        assert_eq!(parse_status_all(&clean)[0].errors, Some(0));
        let unnumbered = clean.replace("No known data errors", "Permanent errors have been detected in the following files:");
        assert_eq!(parse_status_all(&unnumbered)[0].errors, Some(1), "an errors: line that is not the clean one is at least one error");
    }

    #[test]
    fn the_reason_says_error_in_the_singular() {
        let mut p = pool("tank", PoolHealth::Online, 10);
        p.errors = Some(1);
        assert_eq!(level(Some(&reached(vec![p])), false), (Level::Warning, "tank 1 error".to_string()));
    }

    #[test]
    fn a_probe_that_panics_is_unavailable_not_a_dead_thread() {
        struct Boom;
        impl crate::exec::CommandRunner for Boom {
            fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> {
                panic!("boom")
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        assert!(matches!(probe_host_guarded(&Boom, false), HostProbe::Unavailable { reason } if reason.contains("crashed")));
    }
}
