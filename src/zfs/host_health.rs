//! Whether a managed host is reachable and healthy, from one cheap probe (`zpool list` + `zpool status`),
//! and the pure rules that turn a probe (plus "did a job of this host fail?") into a level and a reason.

use super::pool::{parse_pool_scan_status, Pool, PoolHealth, ScanStatus};
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSummary {
    Never,
    /// `resilver` is true when it is a resilver (a replaced or returned disk catching up), not a scrub.
    Running { percent: Option<u8>, resilver: bool },
    /// `zpool scrub -p` (or a paused resilver): neither running nor finished.
    Paused { resilver: bool },
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
    /// When the last **scrub** finished, as `zpool status` prints it (the host's local time, no zone). A resilver, a canceled
    /// or a running scrub is not a finished scrub, so this stays `None` for them. `scrub_age_days` turns it into an age.
    pub last_scrub: Option<String>,
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

/// The operating system of a host, as `uname -s` says. What each section can do depends on it (`ui::support`); a host
/// whose OS is unknown or not yet read behaves like FreeBSD, so nothing changes for a setup that worked before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    FreeBsd,
    Linux,
    Illumos,
    Other,
}

impl HostOs {
    pub fn word(self) -> &'static str {
        match self {
            HostOs::FreeBsd => "FreeBSD",
            HostOs::Linux => "Linux",
            HostOs::Illumos => "illumos",
            HostOs::Other => "unknown",
        }
    }
}

/// `uname -s`: `FreeBSD`, `Linux`, `SunOS` (illumos); anything else is `Other`.
pub fn host_os_from_uname(out: &str) -> HostOs {
    match out.lines().next().map(str::trim) {
        Some("FreeBSD") => HostOs::FreeBsd,
        Some("Linux") => HostOs::Linux,
        Some("SunOS") => HostOs::Illumos,
        _ => HostOs::Other,
    }
}

/// What the first probe asks, in one round trip: the OS, then (errors silenced) the guest kind and whether this is a jail
/// (FreeBSD `sysctl`), and the `@@c`/`@@z`/`@@m`/`@@s` tags that say the same on Linux (`systemd-detect-virt`, the DMI vendor)
/// and illumos (`zonename`, `smbios`). The trailing `true` keeps the exit status for ssh failures only: a missing `sysctl` is an answer.
pub const DETECT_SCRIPT: &str = "uname -s; sysctl -n kern.vm_guest security.jail.jailed 2>/dev/null; echo \"@@r $(uname -r)\"; echo \"@@v $(uname -v | cut -c1-60)\"; \
[ -r /etc/os-release ] && . /etc/os-release && echo \"@@d $NAME $VERSION_ID\"; \
echo \"@@c $(systemd-detect-virt -c 2>/dev/null)\"; echo \"@@z $(zonename 2>/dev/null)\"; echo \"@@m $(systemd-detect-virt -v 2>/dev/null)\"; \
echo \"@@s $( (cat /sys/class/dmi/id/sys_vendor || smbios -t SMB_TYPE_SYSTEM | sed -n 's/^ *Manufacturer: *//p') 2>/dev/null | head -1)\"; true";

/// The short name the Hosts table shows, from the `@@` lines of `DETECT_SCRIPT`: `FreeBSD 16.0`, `Linux 6.8 (Ubuntu 24)`,
/// `illumos omnios-r151058`. `None` when the host gave nothing to name it by (or the OS is not one we know).
pub fn os_name(os: HostOs, out: &str) -> Option<String> {
    let line = |tag: &str| out.lines().find_map(|l| l.strip_prefix(tag)).map(str::trim).filter(|s| !s.is_empty());
    let release = line("@@r ");
    // "6.8.0-45-generic" and "16.0-CURRENT" are "6.8" and "16.0".
    let short = |r: &str, parts: usize| r.split(['-', ' ']).next().unwrap_or(r).split('.').take(parts).collect::<Vec<_>>().join(".");
    match os {
        HostOs::FreeBsd => release.map(|r| format!("FreeBSD {}", short(r, 2))),
        HostOs::Linux => {
            let kernel = short(release?, 2);
            // "Ubuntu 24.04" is "Ubuntu 24"; "Debian GNU/Linux 12" stays.
            let distro = line("@@d ").map(|d| match d.rsplit_once(' ') {
                Some((name, version)) if version.chars().next().is_some_and(|c| c.is_ascii_digit()) => format!("{name} {}", version.split('.').next().unwrap_or(version)),
                _ => d.to_string(),
            });
            Some(match distro {
                Some(d) => format!("Linux {kernel} ({d})"),
                None => format!("Linux {kernel}"),
            })
        }
        // "omnios-r151058-3b4c5d6e7f": the distribution and its release.
        HostOs::Illumos => line("@@v ").map(|v| format!("illumos {}", v.split('-').take(2).collect::<Vec<_>>().join("-"))),
        HostOs::Other => None,
    }
}

/// The OS and the host type from `DETECT_SCRIPT`'s output: FreeBSD from `sysctl`, Linux and illumos from the `@@` tags.
fn parse_detection(out: &str) -> (HostOs, HostKind) {
    let os = host_os_from_uname(out);
    let rest = out.split_once('\n').map_or("", |(_, rest)| rest);
    let kind = match os {
        HostOs::FreeBsd => host_kind_from_sysctl(rest),
        HostOs::Linux | HostOs::Illumos => host_kind_from_tags(rest),
        HostOs::Other => HostKind::Unknown,
    };
    (os, kind)
}

/// System manufacturers (DMI/SMBIOS) that only ever name a virtual machine, matched case-insensitively as a substring.
const VIRTUAL_VENDORS: [&str; 10] = ["qemu", "vmware", "innotek", "virtualbox", "xen", "bochs", "parallels", "bhyve", "amazon ec2", "google"];

/// Reads the `@@c` (systemd container), `@@z` (illumos zone), `@@m` (systemd hypervisor) and `@@s` (system manufacturer)
/// lines. A container or a non-global zone is a `Jail`; a hypervisor name is a `Vm`; otherwise the manufacturer decides
/// (a virtual vendor is a `Vm`, any other name is `Metal`); nothing readable is `Unknown`.
pub fn host_kind_from_tags(out: &str) -> HostKind {
    let tag = |t: &str| out.lines().find_map(|l| l.strip_prefix(t)).map_or("", str::trim);
    let (container, zone, vm, vendor) = (tag("@@c"), tag("@@z"), tag("@@m"), tag("@@s"));
    if (!container.is_empty() && container != "none") || (!zone.is_empty() && zone != "global") {
        return HostKind::Jail;
    }
    if !vm.is_empty() && vm != "none" {
        return HostKind::Vm(vm.to_string());
    }
    let lower = vendor.to_lowercase();
    if VIRTUAL_VENDORS.iter().any(|v| lower.contains(v)) {
        HostKind::Vm(vendor.to_string())
    } else if !vendor.is_empty() || vm == "none" {
        HostKind::Metal
    } else {
        HostKind::Unknown
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
    /// `os` is `Some` only on a probe that was asked to detect it, like `kind`.
    Reached { rtt_ms: u64, pools: Vec<PoolProbe>, kind: Option<HostKind>, os: Option<HostOs>, os_name: Option<String> },
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
    pub last_scrub: Option<String>,
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
pub(crate) fn scan_summary(block: &str) -> ScanSummary {
    match parse_pool_scan_status(block) {
        None => ScanSummary::Never,
        Some(ScanStatus::InProgress { summary, percent_done, .. }) => ScanSummary::Running { percent: percent_done.map(|p| p.clamp(0.0, 100.0) as u8), resilver: summary.starts_with("resilver") },
        Some(ScanStatus::Completed { summary, errors }) => {
            if summary.starts_with("scrub paused") || summary.starts_with("resilver paused") {
                return ScanSummary::Paused { resilver: summary.starts_with("resilver") };
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

/// The time of the last finished scrub in one pool's `zpool status` block: only a `scrub repaired ... on <time>` line counts.
fn last_scrub_when(block: &str) -> Option<String> {
    match parse_pool_scan_status(block)? {
        ScanStatus::Completed { summary, .. } if summary.starts_with("scrub repaired") => summary.split_once(" on ").map(|(_, when)| when.trim().to_string()).filter(|w| !w.is_empty()),
        _ => None,
    }
}

/// How many whole days ago a scrub finished. `when` is the host's local time as `zpool status` prints it and
/// `utc_offset_secs` is that host's offset from UTC (`date +%z`). Never negative (a skewed clock reads as today); `None` when
/// `when` does not parse.
pub fn scrub_age_days(when: &str, utc_offset_secs: i32, now_utc: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let local = chrono::NaiveDateTime::parse_from_str(&when.split_whitespace().collect::<Vec<_>>().join(" "), "%a %b %d %H:%M:%S %Y").ok()?;
    let finished = local.and_utc() - chrono::Duration::seconds(i64::from(utc_offset_secs));
    Some((now_utc - finished).num_days().max(0))
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
            StatusFacts { scan: scan_summary(&text), last_scrub: last_scrub_when(&text), name, errors }
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
            PoolProbe { name: pool.name, health: pool.health, capacity_percent: capacity, frag_percent: frag, errors: status.and_then(|s| s.errors), scan: status.map(|s| s.scan.clone()), last_scrub: status.and_then(|s| s.last_scrub.clone()) }
        })
        .collect();
    // An ssh-level failure (255) says nothing about the host's type: leave it undetected so the next probe asks again.
    let (kind, os, name) = if detect_kind {
        match runner.run_unprivileged("sh", &["-c", DETECT_SCRIPT]) {
            Ok(o) if o.status == 255 => (None, None, None),
            Ok(o) if o.success() => {
                let (os, kind) = parse_detection(&o.stdout);
                (Some(kind), Some(os), os_name(os, &o.stdout))
            }
            _ => (Some(HostKind::Unknown), Some(HostOs::Other), None),
        }
    } else {
        (None, None, None)
    };
    HostProbe::Reached { rtt_ms, pools, kind, os, os_name: name }
}

/// The Used cell: the fullest pool's capacity, with the level the State column uses for it.
pub fn used_cell(pools: &[PoolProbe]) -> (String, Level) {
    let Some(max) = pools.iter().map(|p| p.capacity_percent).max() else { return ("-".to_string(), Level::Ok) };
    let level = if max >= 90 {
        Level::Critical
    } else if max >= 80 {
        Level::Warning
    } else {
        Level::Ok
    };
    (format!("{max}%"), level)
}

pub fn pools_cell(pools: &[PoolProbe]) -> String {
    pools.len().to_string()
}

/// The Scrub cell: what is happening now (running, then paused), else `never` if any pool never scanned, else
/// the oldest last scan (`Sep 26 2026`), `canceled` if that is all there is.
pub fn scan_cell(pools: &[PoolProbe]) -> String {
    let scans: Vec<&ScanSummary> = pools.iter().filter_map(|p| p.scan.as_ref()).collect();
    if scans.is_empty() {
        return "-".to_string();
    }
    if let Some(running) = scans.iter().find_map(|s| match s {
        ScanSummary::Running { percent, .. } => Some(*percent),
        _ => None,
    }) {
        return running.map_or_else(|| "running".to_string(), |p| format!("running {p}%"));
    }
    if scans.iter().any(|s| matches!(s, ScanSummary::Paused { .. })) {
        return "paused".to_string();
    }
    if scans.iter().any(|s| matches!(s, ScanSummary::Never)) {
        return "never".to_string();
    }
    let whens: Vec<&str> = scans
        .iter()
        .filter_map(|s| match s {
            ScanSummary::Finished { when, .. } => Some(when.as_str()),
            _ => None,
        })
        .collect();
    if whens.is_empty() {
        return "canceled".to_string();
    }
    let parse = |w: &str| chrono::NaiveDateTime::parse_from_str(&w.split_whitespace().collect::<Vec<_>>().join(" "), "%a %b %d %H:%M:%S %Y").ok();
    let dated: Vec<chrono::NaiveDateTime> = whens.iter().filter_map(|w| parse(w)).collect();
    let first = whens.first().copied().unwrap_or("-").to_string();
    dated.iter().min().map_or(first, |d| d.format("%b %-d %Y").to_string())
}

/// The Jobs cell: `failed/total` of a host's enabled jobs, and whether any failed.
pub fn jobs_cell(failed: usize, total: usize) -> (String, bool) {
    if total == 0 { ("-".to_string(), false) } else { (format!("{failed}/{total}"), failed > 0) }
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
        PoolProbe { name: name.to_string(), health, capacity_percent: capacity, frag_percent: Some(1), errors: Some(0), scan: Some(ScanSummary::Never), last_scrub: None }
    }

    fn reached(pools: Vec<PoolProbe>) -> HostProbe {
        HostProbe::Reached { rtt_ms: 5, pools, kind: None, os: None, os_name: None }
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
        assert!(matches!(facts[0].scan, ScanSummary::Running { percent: Some(24), resilver: false }), "{:?}", facts[0].scan);
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
                "sh" => Ok(self.sysctl.clone()),
                _ => self.run(cmd, args),
            }
        }
    }

    fn answers(list: CommandOutput, status: CommandOutput) -> Answers {
        Answers { list, status, calls: Mutex::new(vec![]), sysctl: out("FreeBSD\nbhyve\n0\n", "", 0), unprivileged: Mutex::new(vec![]) }
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

    fn pp(name: &str, cap: u8, scan: Option<ScanSummary>) -> PoolProbe {
        PoolProbe { name: name.to_string(), health: PoolHealth::Online, capacity_percent: cap, frag_percent: None, errors: Some(0), scan, last_scrub: None }
    }

    #[test]
    fn used_is_the_fullest_pool_with_the_state_thresholds() {
        let p = |c| vec![pp("a", 10, None), pp("b", c, None)];
        assert_eq!(used_cell(&p(79)), ("79%".to_string(), Level::Ok));
        assert_eq!(used_cell(&p(80)), ("80%".to_string(), Level::Warning));
        assert_eq!(used_cell(&p(89)), ("89%".to_string(), Level::Warning));
        assert_eq!(used_cell(&p(90)), ("90%".to_string(), Level::Critical));
        assert_eq!(used_cell(&[]), ("-".to_string(), Level::Ok));
    }

    #[test]
    fn pools_counts_the_pools() {
        assert_eq!(pools_cell(&[pp("a", 1, None), pp("b", 1, None)]), "2");
        assert_eq!(pools_cell(&[]), "0");
    }

    #[test]
    fn scrub_prefers_running_then_paused_then_never_then_the_oldest_date() {
        let fin = |w: &str| Some(ScanSummary::Finished { when: w.to_string(), errors: 0 });
        let run = Some(ScanSummary::Running { percent: Some(34), resilver: false });
        assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, run)]), "running 34%");
        assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, Some(ScanSummary::Paused { resilver: false }))]), "paused");
        assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, Some(ScanSummary::Never))]), "never");
        assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, fin("Sat Sep 26 13:15:14 2026"))]), "Sep 26 2026");
        assert_eq!(scan_cell(&[pp("a", 1, None)]), "-");
        assert_eq!(scan_cell(&[]), "-");
        assert_eq!(scan_cell(&[pp("a", 1, fin("garbled"))]), "garbled");
        assert_eq!(scan_cell(&[pp("a", 1, fin("garbled")), pp("b", 1, fin("Sat Sep 26 13:15:14 2026")), pp("c", 1, fin("Thu Oct  8 19:48:07 2026"))]), "Sep 26 2026", "an unreadable date does not hide the oldest readable one");
        assert_eq!(scan_cell(&[pp("a", 1, Some(ScanSummary::Canceled))]), "canceled");
    }

    #[test]
    fn jobs_cell_is_failed_over_total() {
        assert_eq!(jobs_cell(0, 0), ("-".to_string(), false));
        assert_eq!(jobs_cell(0, 4), ("0/4".to_string(), false));
        assert_eq!(jobs_cell(1, 4), ("1/4".to_string(), true));
    }

    #[test]
    fn the_short_os_name_comes_from_uname_and_os_release() {
        let name = |os, text: &str| os_name(os, text);
        assert_eq!(name(HostOs::FreeBsd, "FreeBSD\nbhyve\n0\n@@r 16.0-CURRENT\n@@v FreeBSD 16.0-CURRENT #3: Sat\n"), Some("FreeBSD 16.0".into()));
        assert_eq!(name(HostOs::Linux, "Linux\n@@r 6.8.0-45-generic\n@@v #45-Ubuntu SMP\n@@d Ubuntu 24.04\n"), Some("Linux 6.8 (Ubuntu 24)".into()));
        assert_eq!(name(HostOs::Linux, "Linux\n@@r 6.8.0-45-generic\n"), Some("Linux 6.8".into()), "no os-release");
        assert_eq!(name(HostOs::Linux, "Linux\n@@r 6.1.0\n@@d Debian GNU/Linux 12\n"), Some("Linux 6.1 (Debian GNU/Linux 12)".into()));
        assert_eq!(name(HostOs::Illumos, "SunOS\n@@r 5.11\n@@v omnios-r151058-3b4c5d6e7f\n"), Some("illumos omnios-r151058".into()));
        assert_eq!(name(HostOs::Other, "Darwin\n@@r 23.1\n"), None);
        assert_eq!(name(HostOs::Linux, "Linux\n"), None, "nothing to name it by");
    }

    #[test]
    fn the_detection_asks_for_the_release_in_the_same_round_trip() {
        let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        runner.sysctl = out("Linux\n@@r 6.8.0-45-generic\n@@d Ubuntu 24.04\n", "", 0);
        let HostProbe::Reached { os_name, .. } = probe_host(&runner, true) else { panic!() };
        assert_eq!(os_name.as_deref(), Some("Linux 6.8 (Ubuntu 24)"));
        assert_eq!(runner.unprivileged.lock().unwrap().len(), 1);
        assert!(DETECT_SCRIPT.starts_with("uname -s;"), "the first line stays the OS word");
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
    fn linux_and_illumos_say_metal_vm_or_container_from_the_probe_tags() {
        // @@c systemd's container, @@z the illumos zone, @@m systemd's hypervisor, @@s the system manufacturer.
        for (tags, want) in [
            ("@@c none\n@@z \n@@m kvm\n@@s QEMU\n", HostKind::Vm("kvm".into())),
            ("@@c none\n@@z \n@@m none\n@@s Dell Inc.\n", HostKind::Metal),
            ("@@c lxc\n@@z \n@@m none\n@@s \n", HostKind::Jail),
            ("@@c \n@@z sparse\n@@m \n@@s QEMU\n", HostKind::Jail),
            ("@@c \n@@z global\n@@m \n@@s QEMU\n", HostKind::Vm("QEMU".into())),
            ("@@c \n@@z global\n@@m \n@@s Supermicro\n", HostKind::Metal),
            ("@@c \n@@z \n@@m \n@@s VMware, Inc.\n", HostKind::Vm("VMware, Inc.".into())),
            ("@@c none\n@@z \n@@m none\n@@s \n", HostKind::Metal),
            ("@@c \n@@z \n@@m \n@@s \n", HostKind::Unknown),
            ("@@c\n@@z\n@@m\n@@s", HostKind::Unknown),
            ("", HostKind::Unknown),
        ] {
            assert_eq!(host_kind_from_tags(tags), want, "{tags:?}");
        }
    }

    #[test]
    fn a_hypervisor_name_from_systemd_wins_over_the_manufacturer_and_a_container_over_both() {
        assert_eq!(host_kind_from_tags("@@c docker\n@@m kvm\n@@s QEMU\n"), HostKind::Jail);
        assert_eq!(host_kind_from_tags("@@c none\n@@m microsoft\n@@s Microsoft Corporation\n"), HostKind::Vm("microsoft".into()));
        assert_eq!(host_kind_from_tags("@@m none\n@@s QEMU\n"), HostKind::Vm("QEMU".into()), "systemd says none but the board says QEMU");
    }

    #[test]
    fn the_first_probe_names_a_linux_vm_and_an_illumos_zone() {
        for (said, os, want) in [
            ("Linux\n@@r 6.8.0\n@@c none\n@@z \n@@m kvm\n@@s QEMU\n", HostOs::Linux, HostKind::Vm("kvm".into())),
            ("SunOS\n@@r 5.11\n@@c \n@@z sparse\n@@m \n@@s \n", HostOs::Illumos, HostKind::Jail),
            ("FreeBSD\nbhyve\n0\n@@r 15.1\n@@c \n@@z \n@@m \n@@s \n", HostOs::FreeBsd, HostKind::Vm("bhyve".into())),
        ] {
            let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
            runner.sysctl = out(said, "", 0);
            let HostProbe::Reached { kind, os: found, .. } = probe_host(&runner, true) else { panic!() };
            assert_eq!((kind, found), (Some(want), Some(os)), "{said:?}");
        }
        assert!(["systemd-detect-virt", "zonename", "smbios", "sys_vendor"].iter().all(|w| DETECT_SCRIPT.contains(w)), "{DETECT_SCRIPT}");
    }

    #[test]
    fn uname_output_maps_to_the_os() {
        for (text, os) in [("FreeBSD\n", HostOs::FreeBsd), ("Linux\n", HostOs::Linux), ("SunOS\n", HostOs::Illumos), ("Darwin\n", HostOs::Other), ("", HostOs::Other), ("what is this", HostOs::Other)] {
            assert_eq!(host_os_from_uname(text), os, "{text:?}");
        }
        assert_eq!([HostOs::FreeBsd, HostOs::Linux, HostOs::Illumos, HostOs::Other].map(HostOs::word), ["FreeBSD", "Linux", "illumos", "unknown"]);
    }

    #[test]
    fn the_first_probe_asks_os_and_kind_in_one_round_trip_and_only_when_asked() {
        let runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        let HostProbe::Reached { kind, os, .. } = probe_host(&runner, true) else { panic!() };
        assert_eq!((kind, os), (Some(HostKind::Vm("bhyve".into())), Some(HostOs::FreeBsd)));
        let unpriv = runner.unprivileged.lock().unwrap().clone();
        assert_eq!(unpriv.len(), 1, "one round trip for both");
        assert_eq!(unpriv[0].0, "sh");
        assert!(unpriv[0].1.last().unwrap().contains("uname -s") && unpriv[0].1.last().unwrap().contains("kern.vm_guest"), "{unpriv:?}");
        let HostProbe::Reached { kind, os, .. } = probe_host(&runner, false) else { panic!() };
        assert_eq!((kind, os), (None, None));
        assert_eq!(runner.unprivileged.lock().unwrap().len(), 1, "not asked, not run");
    }

    #[test]
    fn on_linux_and_illumos_a_probe_without_the_tags_leaves_the_type_unknown() {
        for (said, os) in [("Linux\n", HostOs::Linux), ("SunOS\n", HostOs::Illumos)] {
            let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
            runner.sysctl = out(said, "", 0);
            let HostProbe::Reached { kind, os: found, .. } = probe_host(&runner, true) else { panic!() };
            assert_eq!((kind, found), (Some(HostKind::Unknown), Some(os)), "{said:?}");
        }
    }

    #[test]
    fn an_ssh_glitch_on_the_detection_leaves_it_undetected_so_it_is_asked_again() {
        let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        runner.sysctl = out("", "ssh: connect to host 10.0.0.9 port 22: Connection reset", 255);
        let HostProbe::Reached { kind, os, .. } = probe_host(&runner, true) else { panic!() };
        assert_eq!((kind, os), (None, None), "not Unknown: that would be cached for the whole session");
    }

    #[test]
    fn a_failed_detection_is_an_unknown_os_and_type_not_a_failed_probe() {
        let mut runner = answers(out(LIST, "", 0), out(STATUS, "", 0));
        runner.sysctl = out("", "sh: uname: not found", 127);
        let HostProbe::Reached { kind, os, .. } = probe_host(&runner, true) else { panic!("the probe must still succeed") };
        assert_eq!((kind, os), (Some(HostKind::Unknown), Some(HostOs::Other)));
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
    fn a_resilver_is_told_from_a_scrub_and_pauses_the_same_way() {
        let block = |scan: &str| format!("  pool: a\n state: ONLINE\n  scan: {scan}\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     ONLINE  0  0  0\n");
        let resilver = block("resilver in progress since Sat Oct 17 10:00:00 2026\n\t5G scanned at 100M/s, 1G issued at 50M/s, 5G total\n\t1G resilvered, 42.00% done, 00:00:30 to go");
        assert_eq!(parse_status_all(&resilver)[0].scan, ScanSummary::Running { percent: Some(42), resilver: true });
        let scrub = block("scrub in progress since Sat Oct 17 10:00:00 2026\n\t5G scanned at 100M/s, 1G issued at 50M/s, 5G total\n\t0B repaired, 24.00% done, 00:00:30 to go");
        assert_eq!(parse_status_all(&scrub)[0].scan, ScanSummary::Running { percent: Some(24), resilver: false });
        let paused = block("resilver paused since Sat Oct 17 10:00:00 2026\n\t1G resilvered, 50.00% done");
        assert_eq!(parse_status_all(&paused)[0].scan, ScanSummary::Paused { resilver: true });
    }

    #[test]
    fn a_paused_scrub_is_paused_not_finished() {
        let status = "  pool: a\n state: ONLINE\n  scan: scrub paused since Thu Oct  8 19:00:00 2026\n\tscrub started on Thu Oct  8 18:00:00 2026\n\t1G scanned, 50.00% done\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     ONLINE  0  0  0\n";
        assert_eq!(parse_status_all(status)[0].scan, ScanSummary::Paused { resilver: false });
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

    #[test]
    fn the_last_scrub_is_remembered_only_for_a_finished_scrub() {
        let facts = parse_status_all(STATUS);
        let by = |n: &str| facts.iter().find(|f| f.name == n).unwrap().last_scrub.clone();
        assert_eq!(by("scratch"), Some("Thu Oct  8 19:48:07 2026".to_string()), "a finished scrub");
        assert_eq!(by("tpool"), None, "a resilver is not a scrub");
        assert_eq!(by("zroot"), None, "nothing ever ran");
        let canceled = "  pool: c\n state: ONLINE\n  scan: scrub canceled on Thu Oct  8 19:48:07 2026\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tc     ONLINE  0  0  0\n";
        assert_eq!(parse_status_all(canceled)[0].last_scrub, None, "a canceled scrub did not finish");
        let running = "  pool: a\n state: ONLINE\n  scan: scrub in progress since Thu Oct  8 19:00:00 2026\n\t1G scanned\n\t0B repaired, 24.40% done, 00:00:30 to go\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\ta     ONLINE  0  0  0\n";
        assert_eq!(parse_status_all(running)[0].last_scrub, None);
    }

    #[test]
    fn scrub_age_counts_whole_days_in_the_hosts_own_time() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-17T20:00:00Z").unwrap().with_timezone(&chrono::Utc);
        // 19:48:07 host time at UTC+2 is 17:48:07 UTC on Oct 8: nine days and two hours before `now`.
        assert_eq!(scrub_age_days("Thu Oct  8 19:48:07 2026", 7200, now), Some(9));
        // At UTC-5 the same wall-clock time is 00:48:07 UTC on Oct 9: eight days and 19 hours ago.
        assert_eq!(scrub_age_days("Thu Oct  8 19:48:07 2026", -3600 * 5, now), Some(8));
        assert_eq!(scrub_age_days("Sat Oct 17 21:00:00 2026", 0, now), Some(0), "a scrub 'in the future' (clock skew) is today, never negative");
        assert_eq!(scrub_age_days("Thu Oct 15 20:00:00 2026", 0, now), Some(2));
        assert_eq!(scrub_age_days("garbage", 0, now), None);
        assert_eq!(scrub_age_days("", 0, now), None);
    }
}
