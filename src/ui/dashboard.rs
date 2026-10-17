//! The Dashboard's rules: facts the app already holds go in, rows and levels come out. Nothing here draws, reads a file or
//! runs a command, so every rule and every boundary is a plain unit test.

use crate::config::Thresholds;
use crate::zfs::host_health::{scrub_age_days, used_cell, Assessment, HostProbe, Level as HealthLevel, PoolProbe, ScanSummary};
use crate::zfs::host_resources::{cpu_percent_between, memory_used_percent, swap_used_percent, Resources};
use chrono::{DateTime, Utc};

/// How worried to be about one reading. `Unknown` means the zfsctl does not know (no answer yet, nothing to compare with): it is
/// never shown as fine. The order is the order of concern, so the worst of several is simply the maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Fine,
    Unknown,
    Watch,
    Issue,
}

impl Level {
    pub fn rank(self) -> u8 {
        match self {
            Level::Fine => 0,
            Level::Unknown => 1,
            Level::Watch => 2,
            Level::Issue => 3,
        }
    }
}

/// One value of the host table, with how to colour it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub level: Level,
    /// A percentage worth a small gauge (space, memory, CPU).
    pub gauge: Option<u8>,
    /// What the Needs attention list says about this cell when it is not fine (`zroot never scrubbed`).
    pub note: Option<String>,
}

impl Cell {
    fn new(text: impl Into<String>, level: Level) -> Cell {
        Cell { text: text.into(), level, gauge: None, note: None }
    }

    fn unknown() -> Cell {
        Cell::new("?", Level::Unknown)
    }

    fn none() -> Cell {
        Cell::new("-", Level::Fine)
    }

    fn with_gauge(mut self, percent: u8) -> Cell {
        self.gauge = Some(percent);
        self
    }

    fn with_note(mut self, note: impl Into<String>) -> Cell {
        self.note = Some(note.into());
        self
    }
}

/// One host of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub name: String,
    pub state: Cell,
    pub pools: Cell,
    pub space: Cell,
    pub memory: Cell,
    pub cpu: Cell,
    pub load: Cell,
    pub scrub: Cell,
    pub errors: Cell,
    /// What `assess()` itself says when it is not fine (unreachable, `tank DEGRADED`, `job failed`...).
    pub base: Option<(Level, String)>,
}

/// Everything the rules need to know about one host.
pub struct HostInput<'a> {
    pub name: &'a str,
    pub assessment: Assessment,
    pub probe: Option<&'a HostProbe>,
    /// The newest resource reading and the one before it.
    pub resources: Option<(&'a Resources, Option<&'a Resources>)>,
    pub utc_offset_secs: Option<i32>,
}

fn level_of(health: HealthLevel) -> Level {
    match health {
        HealthLevel::Probing => Level::Unknown,
        HealthLevel::Disconnected | HealthLevel::Critical => Level::Issue,
        HealthLevel::Warning => Level::Watch,
        HealthLevel::Ok => Level::Fine,
    }
}

fn by_threshold(value: u8, (warn, crit): (u8, u8)) -> Level {
    if value >= crit {
        Level::Issue
    } else if value >= warn {
        Level::Watch
    } else {
        Level::Fine
    }
}

fn space_cell(pools: &[PoolProbe]) -> Cell {
    let Some(fullest) = pools.iter().max_by_key(|p| p.capacity_percent) else { return Cell::none() };
    let (text, level) = used_cell(pools);
    let level = level_of(level);
    let cell = Cell::new(text, level).with_gauge(fullest.capacity_percent);
    if level == Level::Fine { cell } else { cell.with_note(format!("{} {}% full", fullest.name, fullest.capacity_percent)) }
}

fn errors_cell(pools: &[PoolProbe]) -> Cell {
    if pools.is_empty() {
        return Cell::none();
    }
    let known: Vec<(&PoolProbe, u64)> = pools.iter().filter_map(|p| p.errors.map(|n| (p, n))).collect();
    if known.is_empty() {
        return Cell::unknown();
    }
    let total: u64 = known.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return Cell::new("0", Level::Fine);
    }
    let named: Vec<String> = known.iter().filter(|(_, n)| *n > 0).map(|(p, n)| format!("{} {n} error{}", p.name, if *n == 1 { "" } else { "s" })).collect();
    Cell::new(total.to_string(), Level::Watch).with_note(named.join(", "))
}

/// How one pool's last scrub reads: its level, the cell text, how many days (to pick the worst) and the attention note.
fn pool_scrub(pool: &PoolProbe, utc_offset_secs: Option<i32>, t: &Thresholds, now: DateTime<Utc>) -> (Level, String, i64, Option<String>) {
    match pool.scan {
        Some(ScanSummary::Running { percent }) => return (Level::Fine, percent.map_or_else(|| "running".to_string(), |p| format!("running {p}%")), -1, None),
        Some(ScanSummary::Paused) => return (Level::Fine, "paused".to_string(), -1, None),
        _ => {}
    }
    let Some(when) = pool.last_scrub.as_deref() else {
        return (Level::Watch, "never".to_string(), i64::MAX, Some(format!("{} never scrubbed", pool.name)));
    };
    let Some(days) = utc_offset_secs.and_then(|o| scrub_age_days(when, o, now)) else { return (Level::Unknown, "?".to_string(), -1, None) };
    let level = if days >= i64::from(t.scrub_days.1) {
        Level::Issue
    } else if days >= i64::from(t.scrub_days.0) {
        Level::Watch
    } else {
        Level::Fine
    };
    let note = (level != Level::Fine).then(|| format!("{} last scrubbed {days} days ago", pool.name));
    (level, format!("{days} d ago"), days, note)
}

fn scrub_cell(pools: &[PoolProbe], utc_offset_secs: Option<i32>, t: &Thresholds, now: DateTime<Utc>) -> Cell {
    let judged: Vec<(Level, String, i64, Option<String>)> = pools.iter().filter(|p| !t.scrub_ignore.contains(&p.name)).map(|p| pool_scrub(p, utc_offset_secs, t, now)).collect();
    let Some((level, text, _, note)) = judged.into_iter().max_by_key(|(level, _, days, _)| (*level, *days)) else { return Cell::none() };
    let cell = Cell::new(text, level);
    match note {
        Some(note) => cell.with_note(note),
        None => cell,
    }
}

fn memory_cell(r: &Resources, t: &Thresholds) -> Cell {
    let memory = memory_used_percent(r);
    let memory_level = by_threshold(memory, t.memory);
    let swap = swap_used_percent(r);
    let swap_level = swap.map_or(Level::Fine, |s| by_threshold(s, t.swap));
    let mut text = format!("{memory}%");
    if let (Some(s), true) = (swap, swap_level != Level::Fine) {
        text.push_str(&format!(" (swap {s}%)"));
    }
    let level = memory_level.max(swap_level);
    let cell = Cell::new(text, level).with_gauge(memory);
    match (memory_level != Level::Fine, swap) {
        (true, _) => cell.with_note(format!("memory {memory}%")),
        (false, Some(s)) if swap_level != Level::Fine => cell.with_note(format!("swap {s}%")),
        _ => cell,
    }
}

fn cpu_cell(resources: Option<(&Resources, Option<&Resources>)>, t: &Thresholds) -> Cell {
    let Some(percent) = resources.and_then(|(now, before)| cpu_percent_between(before?, now)) else { return Cell::unknown() };
    let level = if percent >= t.cpu_warn { Level::Watch } else { Level::Fine };
    let cell = Cell::new(format!("{percent}%"), level).with_gauge(percent);
    if level == Level::Fine { cell } else { cell.with_note(format!("cpu {percent}%")) }
}

fn load_cell(resources: Option<(&Resources, Option<&Resources>)>) -> Cell {
    match resources.and_then(|(now, _)| now.load) {
        Some([one, ..]) => Cell::new(format!("{one:.2}"), Level::Fine),
        None => Cell::unknown(),
    }
}

/// Judges one host. The state starts from `assess()` and is only ever raised by the readings, never lowered.
pub fn host_row(input: &HostInput, t: &Thresholds, now: DateTime<Utc>) -> HostRow {
    let base_level = level_of(input.assessment.level);
    let base = (base_level >= Level::Watch).then(|| (base_level, input.assessment.reason.clone()));
    let (pools, space, errors, scrub) = match input.probe {
        Some(HostProbe::Reached { pools, .. }) => (
            if pools.is_empty() { Cell::none() } else { Cell::new(pools.len().to_string(), Level::Fine) },
            space_cell(pools),
            errors_cell(pools),
            scrub_cell(pools, input.utc_offset_secs, t, now),
        ),
        _ => (Cell::unknown(), Cell::unknown(), Cell::unknown(), Cell::unknown()),
    };
    let (memory, cpu, load) = match input.resources {
        Some((newest, _)) => (memory_cell(newest, t), cpu_cell(input.resources, t), load_cell(input.resources)),
        None => (Cell::unknown(), Cell::unknown(), Cell::unknown()),
    };
    let worst = [&space, &memory, &cpu, &scrub, &errors].into_iter().filter(|c| c.level >= Level::Watch).max_by_key(|c| c.level);
    let state = match worst {
        Some(cell) if cell.level > base_level => Cell::new(cell.note.clone().unwrap_or_else(|| cell.text.clone()), cell.level),
        _ => Cell::new(input.assessment.reason.clone(), base_level),
    };
    HostRow { name: input.name.to_string(), state, pools, space, memory, cpu, load, scrub, errors, base }
}

/// The lines a host adds to Needs attention: every cell that is not fine, plus what `assess()` itself says, unless a cell
/// already says the same (the space and error cells repeat `tank 85% full` and `tank 2 errors`). Unknown cells add nothing.
pub fn host_attention(row: &HostRow) -> Vec<(Level, String)> {
    let mut items: Vec<(Level, String)> = [&row.space, &row.memory, &row.cpu, &row.scrub, &row.errors]
        .into_iter()
        .filter(|c| c.level >= Level::Watch)
        .map(|c| (c.level, c.note.clone().unwrap_or_else(|| c.text.clone())))
        .collect();
    if let Some((level, reason)) = &row.base {
        let covered = (reason.ends_with("% full") && row.space.level >= Level::Watch) || (reason.contains(" error") && row.errors.level >= Level::Watch);
        if !covered {
            items.insert(0, (*level, reason.clone()));
        }
    }
    items
}

#[cfg(test)]
mod tests {
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
        HostProbe::Reached { rtt_ms: 3, pools, kind: None }
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
        }
    }

    /// A host whose probe says `probe`, with the given resource readings (newest, previous), judged with the defaults.
    fn row(probe: &HostProbe, now_r: Option<&Resources>, before: Option<&Resources>, t: &Thresholds) -> HostRow {
        let input = HostInput { name: "h", assessment: assess(Some(probe), false), probe: Some(probe), resources: now_r.map(|n| (n, before)), utc_offset_secs: Some(0) };
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
        let running = PoolProbe { scan: Some(ScanSummary::Running { percent: Some(34) }), ..pool("tank", 10, 0, None) };
        let r = default_row(&reached(vec![running]), Some(&reading(10, 0, 0, 100))).scrub;
        assert_eq!((r.level, r.text.as_str()), (Level::Fine, "running 34%"));
        let probe = reached(vec![pool("tank", 10, 0, Some(when(50)))]);
        let input = HostInput { name: "h", assessment: assess(Some(&probe), false), probe: Some(&probe), resources: None, utc_offset_secs: None };
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
        let never_probed = host_row(&HostInput { name: "h", assessment: assess(None, false), probe: None, resources: None, utc_offset_secs: None }, &Thresholds::default(), now());
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
}
