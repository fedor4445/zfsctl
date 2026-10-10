# Hosts columns, host type and tag, host filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Hosts table gets Type, Tag, Used, Pools, Scrub and Jobs columns, hosts get an optional config tag, and `/` filters the table.

**Architecture:** Host type is detected by one extra unprivileged `sysctl` call inside a host's first successful probe and cached in `HostProbeState`. Column cell texts are pure functions in `src/zfs/host_health.rs` (probe data) and `src/ui/layout.rs` (jobs). `draw_hosts` is rebuilt around a column list with one fitting rule. The filter lives in `App` as `host_filter`; `hosts`/`selected_host` stay global indices and only the drawn rows and Up/Down change.

**Tech Stack:** Rust, ratatui, serde/toml, existing `FakeRunner` tests.

**Spec:** `docs/superpowers/specs/2026-10-10-hosts-columns-design.md`

## Global Constraints

- `sysctl` is already a registered unprivileged remote command: add no registry entry and do not change the sudoers line.
- Existing `config.toml` files without `tag` must load and save unchanged (`#[serde(skip_serializing_if = "Option::is_none", default)]`).
- Name, Address and State are never dropped by the new columns (Address keeps today's rule: shown while `inner >= 16 + 22 + 18 + 2`). Optional columns drop in the order Rtt, Jobs, Scrub, Pools, Tag, Used, Type.
- Fixed widths: Type 6, Used 5, Pools 5, Scrub 13, Jobs 5, Rtt 6; Tag as wide as the longest tag, at most 14.
- `hosts`, `host_addresses` and `selected_host` stay index-aligned and global; the filter never reorders or removes them.
- Spec amendment (ruled here): the hypervisor name is kept in `HostKind::Vm(String)` but not displayed in v1; the bottom panel is unchanged.
- Run `cargo test` and `cargo clippy` (must stay at 0 warnings) before every commit that ends a task.

## Review Focus

- A filter that hides the selected host must move the selection to a visible host before any other section can act on it (Task 5 test).
- A filter with zero matches must not be committable and must not leave the selection on a hidden host (Task 5 test).
- A probe that fails after the type was cached must not erase the cached type (Task 2 test).
- `Unknown` type (non-FreeBSD, sysctl missing) must be cached so the call is not repeated every 30 s (Task 2 test).
- A tag containing only whitespace, or longer than 40 characters, must be stored as none / truncated (Task 1 test).

---

### Task 1: Tag in config, inventory, Add Host and App

**Files:**
- Modify: `src/config.rs` (struct `HostConfig`, 10 constructor sites across `src/`)
- Modify: `src/hosts.rs` (`Host`, `from_config`, `apply_new_host`)
- Modify: `src/ui/mod.rs` (startup `set_host_addresses` neighbour, `apply_pending_new_hosts`, two test `Host { .. }` literals)
- Modify: `src/ui/state.rs` (`App.host_tags`, `App.new_host_tags`, Add Host form, Enter handler)

**Interfaces:**
- Produces: `HostConfig.tag: Option<String>`, `Host.tag: Option<String>`, `pub fn clean_tag(raw: &str) -> Option<String>` in `src/config.rs`, `App.host_tags: Vec<String>` (index-aligned with `hosts`, empty string = no tag), `App.set_host_tags(Vec<String>)`, `App.host_tag(index) -> &str`, `App.new_host_tags: HashMap<String, String>` (lowercase name → tag), `HostInventory::apply_new_host(&mut self, config_path, name, target, tag: Option<String>)`.

- [ ] **Step 1: Write the failing tests**

In `src/config.rs` tests:

```rust
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
```

In `src/hosts.rs` tests (update the existing `apply_new_host...` call to pass `Some("lab".to_string())` and the expected `HostConfig` to carry `tag: Some("lab".to_string())`; add):

```rust
#[test]
fn from_config_carries_the_tag_to_the_host() {
    let config = Config { hosts: vec![HostConfig { name: "a".into(), ssh_target: Some("u@a".into()), tag: Some("backup".into()) }], ..Config::default() };
    let inv = HostInventory::from_config(&config);
    assert_eq!((inv.hosts[0].tag.clone(), inv.hosts[1].tag.clone()), (None, Some("backup".to_string())));
}
```

In `src/ui/state.rs` tests:

```rust
#[test]
fn add_host_has_an_optional_third_tag_field_and_submits_on_it() {
    let mut app = in_section(Section::Hosts);
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(form_values(&app).len(), 3);
    for c in "box".chars() { app.handle_modal_key(KeyCode::Char(c)); }
    app.handle_modal_key(KeyCode::Enter);
    for c in "u@box".chars() { app.handle_modal_key(KeyCode::Char(c)); }
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_some(), "Enter on the second field moves on, it does not submit");
    for c in " nas ".chars() { app.handle_modal_key(KeyCode::Char(c)); }
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.new_host_tags.get("box").map(String::as_str), Some("nas"));
}

#[test]
fn a_blank_tag_is_not_remembered() {
    let mut app = in_section(Section::Hosts);
    app.handle_key(KeyCode::Char('n'));
    for c in "box".chars() { app.handle_modal_key(KeyCode::Char(c)); }
    app.handle_modal_key(KeyCode::Enter);
    for c in "u@box".chars() { app.handle_modal_key(KeyCode::Char(c)); }
    app.handle_modal_key(KeyCode::Enter);
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.new_host_tags.is_empty());
}

#[test]
fn host_tags_default_to_empty_and_follow_set_host_tags() {
    let mut app = App::new(vec!["local".into(), "a".into()]);
    assert_eq!((app.host_tag(0), app.host_tag(1), app.host_tag(9)), ("", "", ""));
    app.set_host_tags(vec![String::new(), "backup".into()]);
    assert_eq!(app.host_tag(1), "backup");
}
```

In `src/ui/mod.rs` tests (extend `apply_pending_new_hosts_applies_every_queued_host...`): before the drain add `app.new_host_tags.insert("hosta".into(), "lab".into());` and after it assert `app.host_tags == vec!["", "lab", ""]` and `inventory.hosts[1].tag.as_deref() == Some("lab")`, and `app.new_host_tags.is_empty()`. (Use `vec!["".to_string(), "lab".to_string(), "".to_string()]`.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test clean_tag a_config_without add_host_has a_blank_tag host_tags from_config_carries apply_pending_new_hosts 2>&1 | tail -20`
Expected: compile errors (`tag` field, `clean_tag`, `host_tags` missing).

- [ ] **Step 3: Implement**

`src/config.rs`:

```rust
    /// A free label for this host (`backup`, `garage`), shown in the Hosts table and matched by its filter.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tag: Option<String>,
```
added to `HostConfig`, plus

```rust
/// A tag as stored: trimmed, at most 40 characters, `None` when blank.
pub fn clean_tag(raw: &str) -> Option<String> {
    let t: String = raw.trim().chars().take(40).collect();
    let t = t.trim_end().to_string();
    (!t.is_empty()).then_some(t)
}
```

Add `tag: None` to every existing `HostConfig { .. }` literal (`grep -rn "HostConfig {" src`). `src/hosts.rs`: add `pub tag: Option<String>` to `Host`, set `tag: None` for local, `tag: host_cfg.tag.clone()` in `from_config`, add the `tag: Option<String>` parameter to `apply_new_host` and use it in both the pushed `Host` and the saved `HostConfig`. Add `tag: None` to the two `Host { .. }` literals in `src/ui/mod.rs` tests.

`src/ui/state.rs`: add `pub host_tags: Vec<String>` (init to `vec![String::new(); hosts.len()]` next to `host_addresses`), `pub new_host_tags: HashMap<String, String>` (init `HashMap::new()`), and

```rust
    pub fn set_host_tags(&mut self, tags: Vec<String>) {
        self.host_tags = tags;
    }

    pub fn host_tag(&self, index: usize) -> &str {
        self.host_tags.get(index).map_or("", String::as_str)
    }
```

Add the third field to the `n` form: `FormField { label: "Tag (optional)".to_string(), value: String::new() }`. In the AddHost Enter branch, after the duplicate check passes and before `begin_add_host`: 

```rust
                            if let Some(tag) = crate::config::clean_tag(field_value(&fields, 2)) {
                                self.new_host_tags.insert(name_lower.clone(), tag);
                            }
```
(`name_lower` is already inserted into `pending_probes` on the line above; use `self.pending_probes.insert(name_lower.clone())`.)

`src/ui/mod.rs`: at startup after `set_host_addresses` add `app.set_host_tags(inventory.hosts.iter().map(|h| h.tag.clone().unwrap_or_default()).collect());`. In `apply_pending_new_hosts`, per host: `let tag = app.new_host_tags.remove(&name.to_lowercase());`, `app.host_tags.push(tag.clone().unwrap_or_default());`, and pass `tag` to `inventory.apply_new_host(config_path, name, target, tag)`. In `App::new`, a host-failed probe must also drop its stored tag: in the `WorkerMsg::HostProbe` `Err` arm add `self.new_host_tags.remove(&name.to_lowercase());`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: hosts carry an optional tag (config.toml and the Add Host form)

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Host type detection, cached per host

**Files:**
- Modify: `src/zfs/host_health.rs` (`HostKind`, `host_kind_from_sysctl`, `probe_host`, `HostProbe::Reached.kind`, 12 `Reached {` sites)
- Modify: `src/ui/state.rs` (`HostProbeState.kind`, `start_host_probe`, `HostHealth` handler)
- Modify: `src/ui/layout.rs` (test helper `Reached` literals)

**Interfaces:**
- Produces: `pub enum HostKind { Metal, Vm(String), Jail, Unknown }` (`Debug, Clone, PartialEq, Eq`), `pub fn host_kind_from_sysctl(out: &str) -> HostKind`, `pub fn probe_host(runner: &dyn CommandRunner, detect_kind: bool) -> HostProbe`, `pub fn probe_host_guarded(runner, detect_kind: bool)`, `HostProbe::Reached { rtt_ms, pools, kind: Option<HostKind> }`, `HostProbeState.kind: Option<HostKind>`, `HostKind::word(&self) -> &'static str` (`metal`/`vm`/`jail`/`-`).

- [ ] **Step 1: Write the failing tests** (in `src/zfs/host_health.rs` tests; the file already has `answers(list, status)`/`out(...)`, add a `sysctl` answer by extending the helper or using `FakeRunner::route`; follow the existing helper's style)

```rust
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
    let runner = answers(out(LIST, "", 0), out(STATUS, "", 0)); // plus an unprivileged route for sysctl answering "bhyve\n0\n"
    let HostProbe::Reached { kind, .. } = probe_host(&runner, true) else { panic!() };
    assert_eq!(kind, Some(HostKind::Vm("bhyve".into())));
    let unpriv = runner.unprivileged_calls.lock().unwrap().clone();
    assert_eq!(unpriv, vec![("sysctl".to_string(), vec!["-n".into(), "kern.vm_guest".into(), "security.jail.jailed".into()])]);
    let HostProbe::Reached { kind, .. } = probe_host(&runner, false) else { panic!() };
    assert_eq!(kind, None);
}
```

(If the existing `answers` helper cannot answer `run_unprivileged`, extend it so unprivileged `sysctl` returns a configurable stdout, default `"none\n0\n"`; keep every existing test passing.)

In `src/ui/state.rs` tests:

```rust
#[test]
fn the_type_is_cached_once_seen_and_a_failed_probe_does_not_erase_it() {
    use crate::zfs::host_health::{HostKind, HostProbe};
    let mut app = App::new(vec!["local".into(), "a".into()]);
    let reached = |kind| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind };
    app.tx.send(WorkerMsg::HostHealth("a".into(), reached(Some(HostKind::Vm("bhyve".into()))))).unwrap();
    app.drain_worker_messages();
    app.tx.send(WorkerMsg::HostHealth("a".into(), reached(None))).unwrap();
    app.tx.send(WorkerMsg::HostHealth("a".into(), HostProbe::Disconnected)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_probes["a"].kind, Some(HostKind::Vm("bhyve".into())));
}

#[test]
fn an_unknown_type_is_cached_so_it_is_not_asked_again() {
    use crate::zfs::host_health::{HostKind, HostProbe};
    let mut app = App::new(vec!["local".into(), "a".into()]);
    app.tx.send(WorkerMsg::HostHealth("a".into(), HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Unknown) })).unwrap();
    app.drain_worker_messages();
    assert!(app.host_probes["a"].kind.is_some());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test host_kind detection_runs the_type_is_cached an_unknown_type 2>&1 | tail -15`
Expected: compile errors.

- [ ] **Step 3: Implement**

`src/zfs/host_health.rs`:

```rust
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
```

Add `kind: Option<HostKind>` to `Reached`. `probe_host(runner, detect_kind)`: at the end, `let kind = detect_kind.then(|| runner.run_unprivileged("sysctl", &["-n", "kern.vm_guest", "security.jail.jailed"]).ok().filter(CommandOutput::success).map_or(HostKind::Unknown, |o| host_kind_from_sysctl(&o.stdout)));`. `probe_host_guarded` takes and forwards the flag. Update every `Reached { .. }` constructor (host_health 2 sites, layout 3, state 2) with `kind: None`; patterns using `..` need no change.

`src/ui/state.rs`: add `pub kind: Option<crate::zfs::host_health::HostKind>` to `HostProbeState` (derive `Default` keeps working; update the two `HostProbeState { .. }` literals in tests with `kind: None`, and the layout helper). In `start_host_probe`: `let detect = state.kind.is_none();` before spawning, and `probe_host_guarded(runner.as_ref(), detect)`. In the `HostHealth` handler: 

```rust
                        if let crate::zfs::host_health::HostProbe::Reached { kind: Some(k), .. } = &probe {
                            state.kind = Some(k.clone());
                        }
```
before `state.last = Some(probe)`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning`
Expected: all pass, 0.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: Hosts: detect each host's type (metal, vm, jail) once with one unprivileged sysctl

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Cell texts for Used, Pools, Scrub, Jobs

**Files:**
- Modify: `src/zfs/host_health.rs` (pure functions + tests)
- Modify: `src/ui/state.rs` (`App::host_jobs_counts`)

**Interfaces:**
- Produces: `pub fn used_cell(pools: &[PoolProbe]) -> (String, Level)` (level `Ok`/`Warning`/`Critical`), `pub fn pools_cell(pools: &[PoolProbe]) -> String`, `pub fn scan_cell(pools: &[PoolProbe]) -> String`, `pub fn jobs_cell(failed: usize, total: usize) -> (String, bool)` (bool = has failures), `App::host_jobs_counts(&self, name: &str) -> (usize, usize)` (failed, total over enabled jobs).

- [ ] **Step 1: Write the failing tests** (host_health.rs tests; a helper `pp(name, cap, scan)` building a `PoolProbe` with `health: Online`, `errors: Some(0)`)

```rust
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
fn pools_counts_and_dashes_when_there_are_none() {
    assert_eq!(pools_cell(&[pp("a", 1, None), pp("b", 1, None)]), "2");
    assert_eq!(pools_cell(&[]), "0");
}

#[test]
fn scrub_prefers_running_then_paused_then_never_then_the_oldest_date() {
    let fin = |w: &str| Some(ScanSummary::Finished { when: w.to_string(), errors: 0 });
    let run = Some(ScanSummary::Running { percent: Some(34) });
    assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, run.clone())]), "running 34%");
    assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, Some(ScanSummary::Paused))]), "paused");
    assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, Some(ScanSummary::Never))]), "never");
    assert_eq!(scan_cell(&[pp("a", 1, fin("Thu Oct  8 19:48:07 2026")), pp("b", 1, fin("Sat Sep 26 13:15:14 2026"))]), "Sep 26 2026");
    assert_eq!(scan_cell(&[pp("a", 1, None)]), "-");
    assert_eq!(scan_cell(&[pp("a", 1, fin("garbled"))]), "garbled");
}

#[test]
fn jobs_cell_is_failed_over_total() {
    assert_eq!(jobs_cell(0, 0), ("-".to_string(), false));
    assert_eq!(jobs_cell(0, 4), ("0/4".to_string(), false));
    assert_eq!(jobs_cell(1, 4), ("1/4".to_string(), true));
}
```

state.rs test:

```rust
#[test]
fn host_jobs_counts_total_enabled_jobs_and_failed_latest_runs_per_host() {
    let mut a = crate::jobs::Job::test_backup("j1", "tank/x", "p", "/r", "/pw");
    a.host = "fbsd16".into();
    let mut b = crate::jobs::Job::test_backup("j2", "tank/y", "p", "/r", "/pw");
    b.host = "fbsd16".into();
    let mut c = crate::jobs::Job::test_backup("j3", "tank/z", "p", "/r", "/pw");
    c.host = "fbsd16".into();
    c.enabled = false;
    let (mut app, dir) = app_with_jobs(&[a, b, c]);
    app.hosts = vec!["local".into(), "fbsd16".into()];
    app.refresh_jobs();
    let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
    for (job, run) in [("j1", "r1")] {
        h.append(&crate::daemon::history::Event { job_id: job.into(), run_id: run.into(), event: crate::daemon::history::EventKind::Finished, at: chrono::Utc::now(), scheduled_for: None, result: Some(crate::daemon::history::RunResult::Failed), message: String::new(), catch_up: false, summary: None }).unwrap();
    }
    app.refresh_jobs();
    assert_eq!(app.host_jobs_counts("fbsd16"), (1, 2), "the disabled job counts nowhere");
    assert_eq!(app.host_jobs_counts("local"), (0, 0));
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test used_is pools_counts scrub_prefers jobs_cell host_jobs_counts 2>&1 | tail` → compile errors.

- [ ] **Step 3: Implement**

`used_cell`: max `capacity_percent`; none → `("-", Ok)`; ≥90 Critical, ≥80 Warning, else Ok. `pools_cell`: `pools.len().to_string()`. `scan_cell`: empty or all `scan: None` → `-`; first any `Running` → `running N%` (or `running`), then any `Paused` → `paused`, then any `Never` → `never`, then the minimum of the `Finished` dates parsed with `chrono::NaiveDateTime::parse_from_str(when.trim(), "%a %b %e %H:%M:%S %Y")` rendered `%b %e %Y` with doubled spaces collapsed (`Oct  8 2026` → `Oct 8 2026`; the test expects `Sep 26 2026`, so format as `format!("{} {} {}", month, day, year)` using `%b`, `%-d`, `%Y`); if any date fails to parse return the first `Finished.when` unchanged (`garbled`). `Canceled` counts like a finished scan without a date: if only canceled pools → `canceled`. `jobs_cell(f, t)`: `t == 0` → `("-", false)`, else `(format!("{f}/{t}"), f > 0)`.

`App::host_jobs_counts(name)`: over `self.jobs_view.jobs.iter().filter(|j| j.enabled && j.host == name)`: total = count, failed = those whose `last_run(&j.id)` has `result == Some(RunResult::Failed)`.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"`; Expected all pass.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat: Hosts: cell texts for Used, Pools, Scrub and Jobs"` (with the Co-Authored-By trailer).

---

### Task 4: The column table

**Files:**
- Modify: `src/ui/theme.rs` (`alt` style)
- Modify: `src/ui/layout.rs` (`hosts_columns`, `draw_hosts`)

**Interfaces:**
- Consumes: Task 1 `App::host_tag`, Task 2 `HostProbeState.kind`/`HostKind::word`, Task 3 cell functions and `App::host_jobs_counts`.
- Produces: `Theme.alt: Style` (ansi: magenta; mono: plain underlined; solarized: magenta `c((211, 54, 130), 125)`), `pub fn fit_host_columns(inner: u16, name_w: u16, tag_w: u16) -> HostColumns` where `pub struct HostColumns { pub addr: bool, pub type_: bool, pub tag: bool, pub used: bool, pub pools: bool, pub scrub: bool, pub jobs: bool, pub rtt: bool }`.

- [ ] **Step 1: Write the failing tests** (layout.rs tests; the file's `hosts_app()` helper builds four hosts, extend it so each probe carries a `kind` and give `fbsd16` the tag `backup`)

```rust
#[test]
fn the_host_columns_drop_in_the_documented_order_and_the_required_three_stay() {
    // Array order is the drop order: Rtt first, Type last.
    let order = |c: &HostColumns| [c.rtt, c.jobs, c.scrub, c.pools, c.tag, c.used, c.type_];
    let wide = fit_host_columns(200, 10, 8);
    assert!(order(&wide).iter().all(|b| *b) && wide.addr, "everything fits at 200");
    for w in 30..200 {
        let on = order(&fit_host_columns(w, 10, 8));
        // a column that survives implies every column that drops after it survives too
        assert!(on.windows(2).all(|p| !(p[0] && !p[1])), "width {w}: {on:?}");
        // widening never removes a column
        let wider = order(&fit_host_columns(w + 1, 10, 8));
        assert!(on.iter().zip(wider).all(|(a, b)| !*a || b), "width {w}");
    }
    assert!(!order(&fit_host_columns(30, 10, 8)).iter().any(|b| *b), "at 30 columns only the required ones remain");
    assert!(order(&fit_host_columns(200, 10, 0))[4] == false, "no tags configured: no Tag column");
}

#[test]
fn the_hosts_table_shows_the_new_columns_and_cells() {
    let mut app = hosts_app();
    app.selected_host = 0;
    let text = render(&mut app, 170, 30).join("\n");
    for want in ["Type", "Tag", "Used", "Pools", "Scrub", "Jobs", "Rtt", "vm", "metal", "backup", "85%", "20%"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_type_words_have_their_own_colors_apart_from_the_health_colors() {
    let mut app = hosts_app();
    let vm = fg_of(&mut app, " vm", 170, 30);
    let ok = fg_of(&mut app, "ok", 170, 30);
    assert!(vm.is_some() && vm != ok, "{vm:?} vs {ok:?}");
}

#[test]
fn a_narrow_hosts_table_still_shows_name_address_and_state() {
    let mut app = hosts_app();
    let text = render(&mut app, 70, 30).join("\n");
    for want in ["Name", "Address", "State", "172.16.10.60"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}
```


- [ ] **Step 2: Run to verify they fail** — `cargo test the_host_columns the_hosts_table_shows_the_new the_type_words a_narrow_hosts 2>&1 | tail` → compile errors.

- [ ] **Step 3: Implement**

`Theme.alt` in all three constructors (ansi `fg(Color::Magenta)`, mono `plain.add_modifier(Modifier::UNDERLINED)`, solarized `fg(c((211, 54, 130), 125))`) and any other `Theme { .. }` literal the compiler flags.

`fit_host_columns(inner, name_w, tag_w)`:
 - `addr = inner >= 16 + 22 + 18 + 2` (today's rule, `name_w` unused for it).
 - Budget after the required columns: `rest = inner.saturating_sub(name_w + 2 /*marker*/ + 22 /*State min*/ + if addr {18 + 1} else {0})`, with one column gap per extra column.
 - Add optional columns in reverse drop order (Type, Used, Tag, Pools, Scrub, Jobs, Rtt) while `rest >= width + 1`, subtracting as they are added; stop at the first that does not fit (so a later, narrower column can never appear without an earlier one).
 - Widths: Type 6, Used 5, Tag `tag_w.clamp(3, 14)` (0 tags ⇒ `tag_w` passed as 0 means the Tag column is skipped entirely and does not consume budget), Pools 5, Scrub 13, Jobs 5, Rtt 6.
 - Because Tag is skipped when no host has a tag, compute `tag_w` in `draw_hosts` as the longest tag (0 if none).

`draw_hosts` rewrite: compute `tag_w`, call `fit_host_columns`, and build headers/widths/cells from the returned flags in this order: Name, Address, State, Type, Tag, Used, Pools, Scrub, Jobs, Rtt. Cells: Type = `kind.word()` styled `theme.alt` for jail, `theme.info` for vm, default for metal/`-` (from `app.host_probes[name].kind`); Tag plain; Used from `used_cell` (style by level: Warning → `theme.warn`, Critical → `theme.err`); Pools; Scrub; Jobs (failed → `theme.err`). With no probe, Used/Pools/Scrub show `-`. Keep the existing marker, name, State and row-selection styling, the `TableState` scrolling and the single-host hint unchanged. State column is `Constraint::Min(8)` as today.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Also confirm the existing Hosts render tests (`the_hosts_table_shows_name_address_state_and_rtt` etc.) still pass unchanged.

- [ ] **Step 5: Commit** — `feat: Hosts: Type, Tag, Used, Pools, Scrub and Jobs columns` (+ trailer).

---

### Task 5: The `/` filter

**Files:**
- Modify: `src/ui/state.rs` (`host_filter`, `host_filter_editing`, `visible_hosts`, `handle_key`, Up/Down, add-host clear)
- Modify: `src/ui/layout.rs` (`draw_hosts` rows and title)

**Interfaces:**
- Consumes: Task 1 `host_tag`, Task 2 `HostProbeState.kind`.
- Produces: `App.host_filter: String`, `App.host_filter_editing: bool`, `pub fn visible_hosts(&self) -> Vec<usize>` (global indices, in order; all hosts when the filter is empty), `pub fn host_matches(&self, index: usize, needle: &str) -> bool`.

- [ ] **Step 1: Write the failing tests** (state.rs tests; helper builds `App::new(["local","fbsd16","t1","nas"])` with addresses, tags `["", "", "", "backup"]` and a cached kind for `t1` = Vm)

```rust
fn filter_app() -> App {
    let mut app = in_section(Section::Hosts);
    app.hosts = vec!["local".into(), "fbsd16".into(), "t1".into(), "nas".into()];
    app.set_host_addresses(vec!["local".into(), "10.0.0.60".into(), "10.0.0.61".into(), "10.0.0.9".into()]);
    app.set_host_tags(vec![String::new(), String::new(), String::new(), "Backup".into()]);
    app.host_probes.insert("t1".into(), HostProbeState { kind: Some(crate::zfs::host_health::HostKind::Vm("bhyve".into())), ..HostProbeState::default() });
    app
}

fn type_filter(app: &mut App, text: &str) {
    app.handle_key(KeyCode::Char('/'));
    for c in text.chars() { app.handle_key(KeyCode::Char(c)); }
}

#[test]
fn slash_filters_live_by_name_address_tag_and_type_ignoring_case() {
    let mut app = filter_app();
    type_filter(&mut app, "back");
    assert_eq!(app.visible_hosts(), vec![3], "tag, case-insensitive");
    app.handle_key(KeyCode::Backspace); app.handle_key(KeyCode::Backspace); app.handle_key(KeyCode::Backspace); app.handle_key(KeyCode::Backspace);
    for c in "10.0.0.6".chars() { app.handle_key(KeyCode::Char(c)); }
    assert_eq!(app.visible_hosts(), vec![1, 2], "address");
    for _ in 0..8 { app.handle_key(KeyCode::Backspace); }
    for c in "vm".chars() { app.handle_key(KeyCode::Char(c)); }
    assert_eq!(app.visible_hosts(), vec![2], "type word");
}

#[test]
fn typing_in_the_filter_does_not_trigger_other_keys() {
    let mut app = filter_app();
    type_filter(&mut app, "qn");
    assert!(!app.should_quit && app.modal.is_none(), "q and n are text while the filter is open");
}

#[test]
fn the_selection_moves_to_a_visible_host_when_the_filter_hides_it() {
    let mut app = filter_app();
    app.selected_host = 0;
    type_filter(&mut app, "nas");
    assert_eq!(app.selected_host, 3);
}

#[test]
fn up_and_down_move_among_visible_hosts_and_wrap() {
    let mut app = filter_app();
    type_filter(&mut app, "10.0.0.");
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.visible_hosts(), vec![1, 2, 3]);
    assert_eq!(app.selected_host, 1);
    app.handle_key(KeyCode::Down); assert_eq!(app.selected_host, 2);
    app.handle_key(KeyCode::Down); app.handle_key(KeyCode::Down); assert_eq!(app.selected_host, 1, "wraps within the visible hosts");
    app.handle_key(KeyCode::Up); assert_eq!(app.selected_host, 3);
}

#[test]
fn enter_with_no_match_keeps_the_input_open_and_esc_clears_everything() {
    let mut app = filter_app();
    app.selected_host = 2;
    type_filter(&mut app, "zzz");
    assert!(app.visible_hosts().is_empty());
    assert_eq!(app.selected_host, 2, "no match: the selection stays where it was, on a host that exists");
    app.handle_key(KeyCode::Enter);
    assert!(app.host_filter_editing, "cannot commit a filter that matches nothing");
    app.handle_key(KeyCode::Esc);
    assert!((app.host_filter.is_empty(), app.host_filter_editing) == (true, false));
    assert_eq!(app.visible_hosts().len(), 4);
}

#[test]
fn esc_after_a_committed_filter_clears_it() {
    let mut app = filter_app();
    type_filter(&mut app, "nas");
    app.handle_key(KeyCode::Enter);
    assert!(!app.host_filter_editing && app.host_filter == "nas");
    app.handle_key(KeyCode::Esc);
    assert!(app.host_filter.is_empty());
}

#[test]
fn slash_outside_hosts_is_not_a_filter_and_adding_a_host_clears_it() {
    let mut app = in_section(Section::Pools);
    app.handle_key(KeyCode::Char('/'));
    assert!(!app.host_filter_editing);
    let mut app = filter_app();
    type_filter(&mut app, "nas");
    app.handle_key(KeyCode::Enter);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.host_filter.is_empty(), "opening Add Host clears the filter so the new host is visible");
}
```

Render test (layout.rs):

```rust
#[test]
fn the_hosts_title_shows_the_filter_and_only_matching_rows_are_drawn() {
    let mut app = hosts_app();
    app.host_filter = "dead".into();
    app.selected_host = 3;
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("deadbox") && !text.contains("fbsd16t1"), "{text}");
    assert!(text.contains("filter: dead") && text.contains("1/4"), "{text}");
    app.host_filter_editing = true;
    assert!(render(&mut app, 120, 30).join("\n").contains("filter: dead_"), "a cursor while typing");
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors on `visible_hosts`, `host_filter`.

- [ ] **Step 3: Implement**

`host_matches(index, needle)`: lowercase needle is a substring of the lowercase name, address, tag, or type word (`self.host_probes.get(name).and_then(|s| s.kind.as_ref()).map_or("", HostKind::word)`; the `-` word never matches because an empty-needle test returns early and `-` is only matched if the user types `-`: acceptable, but exclude `-` by mapping `Unknown` to `""`). `visible_hosts`: all indices when `host_filter` is empty, else those matching.

`ensure_visible_selection(&mut self)`: if the filter is non-empty and `visible_hosts()` is non-empty and does not contain `selected_host`, set `selected_host` to the first visible and call `clear_host_data()`.

`handle_key` start: `if self.host_filter_editing { self.handle_filter_key(key); return; }`. `KeyCode::Char('/') if self.current_section() == Section::Hosts && self.modal.is_none() && self.picker.is_none() && self.drilled_pool.is_none()` → `self.host_filter_editing = true` (keeps any existing filter text so it can be refined). `handle_filter_key`: `Char(c)` push + `ensure_visible_selection`; `Backspace` pop + ensure; `Esc` clear text and stop editing; `Enter` stop editing unless the filter is non-empty and `visible_hosts()` is empty (then stay open). A committed filter (not editing): `Esc` while `current_section() == Hosts && !host_filter.is_empty()` clears it — add this arm before the generic `KeyCode::Esc => status_message = None` arm and after the drilled/pane arms.

Up/Down in Hosts: replace the modulo arithmetic with movement within `visible_hosts()`: find the position of `selected_host` in the visible list (default 0), step ±1 with wrap, set `selected_host`, `clear_host_data()`; with an empty filter the visible list is every host so behavior is unchanged (existing Down/Up tests must pass untouched).

`n` (Add Host): clear `host_filter` and `host_filter_editing` before opening the form.

`draw_hosts`: iterate `app.visible_hosts()` (keep the global index `i` for assessment, address, tag, kind, and the selected-row style); `TableState` selected = position of `selected_host` within the visible list. Title: `Hosts` when no filter, else `Hosts (filter: {text}[_] — {visible}/{total})` where `_` is appended only while `host_filter_editing`. When the filter matches nothing, draw one dim row `no host matches` in the Name column.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Mutation check: make `ensure_visible_selection` a no-op and confirm `the_selection_moves_to_a_visible_host_when_the_filter_hides_it` fails; make Enter ignore the no-match rule and confirm `enter_with_no_match...` fails.

- [ ] **Step 5: Commit** — `feat: Hosts: / filters the table by name, address, tag and type` (+ trailer).

---

### Task 6: README, help text, live check

**Files:**
- Modify: `README.md` (Hosts paragraph, `config.toml` example, keys list), `src/ui/state.rs` (`help_text`)

- [ ] **Step 1: Write the failing test** (state.rs)

```rust
#[test]
fn help_documents_the_hosts_filter_key() {
    let text = help_text();
    assert!(text.contains("/") && text.to_lowercase().contains("filter"), "{text}");
}
```
Run: `cargo test help_documents_the_hosts_filter_key` → FAIL.

- [ ] **Step 2: Implement docs**

`help_text`: in the Hosts block add a line for `/` (`/ filter the host list by name, address, tag or type; Enter keeps it, Esc clears`) and the Add Host `n` line mentions the optional tag. README: the Hosts paragraph that starts `The Hosts list is a table:` becomes `● Name | Address | State | Type | Tag | Used | Pools | Scrub | Jobs | Rtt` with one sentence per new column (as in the spec's table), the drop order, the type detection (`sysctl -n kern.vm_guest security.jail.jailed`, once per host, no sudo), the `tag = "..."` config example (`[[hosts]]` with `tag = "backup"`; edit `config.toml` and restart to change it, or set it in the Add Host form), and the `/` filter semantics (matches name, address, tag, type; Enter keeps; Esc clears; the selection always stays on a visible host).

- [ ] **Step 3: Run the suite** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0.

- [ ] **Step 4: Live check on the hub** (deploy: `tar --exclude=target --exclude=.git --exclude=.superpowers -cf - . | ssh fb@172.16.10.90 'cd ~/zfsctl && tar xf - && cargo build --release'`; drive `target/release/zfsctl` with the scratchpad `tui.py` pexpect+pyte harness as root on the hub; do **not** install over `/usr/local/bin/zfsctl` until the user says so). Add `tag = "test"` to `fbsd16` in the hub's `config.toml` copy used by the run (restore after). Verify: Hosts shows Type `vm` for fbsd16/t1/t2 and the hub's real type, the Tag `test`, Used/Pools/Scrub/Jobs values equal to `zpool list`/`zpool status` read by hand; `/` `t1` narrows to t1 only and Esc restores; `/` `test` narrows to fbsd16; `/` `zzz` then Enter stays open; Down skips hidden hosts; at 110x40 and 80x24 the table keeps Name, Address, State. Record the observations in the spec's "Live results" section (add the section) and commit.

- [ ] **Step 5: Commit** — `docs: Hosts columns, type, tag and filter` (+ trailer).
