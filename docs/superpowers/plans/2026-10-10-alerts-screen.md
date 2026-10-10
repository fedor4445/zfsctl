# The Alerts screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new TUI section, Alerts, shows the saved `[notify]` settings, the monitor's status and the recent events, lets the operator edit the settings, send a test message and preview the report, and saves by replacing only the `[notify]` section of `config.toml`.

**Architecture:** `config.rs` gets a pure section splice and an atomic, backed-up file save. `ui/alerts.rs` loads the screen's data (settings, last report, events). `Section::Alerts` is wired like Jobs (sidebar, draw, hints, refresh). The edit form validates with the daemon's `NotifyConfig::resolve`, confirms with two keys, and saves through a new `MutationCall::SaveNotify`; the test message is `MutationCall::SendTestMessage`; the preview runs on a worker thread and shows an info pop-up.

**Tech Stack:** Rust, ratatui, toml 0.8, existing `notify_cli`, `monitor`, `health_text::EventLog`.

**Spec:** `docs/superpowers/specs/2026-10-10-alerts-screen-design.md`

## Global Constraints

- Saving changes **only** the `[notify]` section of `config.toml`; every other byte is kept. The result is parsed and verified before anything is written; the old file is copied to `config.toml.bak` first; the new text goes through `<file>.tmp` and a rename, with the original permissions.
- The hook (`[daemon] notify_command`) is shown read-only and is never written by the TUI.
- A blank form field writes nothing (the default stays a default). Validation is exactly `NotifyConfig::resolve` (same messages).
- `t` sends real mail: it always asks first (two keys) and names the recipient; the preview sends and records nothing.
- No new privileged command, no sudoers change.
- Adding a sidebar section changes tests that count or wrap sections: fix those tests in the task that adds the section.
- Run `cargo test` and `cargo clippy` (must stay at 0 warnings) before every commit that ends a task.

## Review Focus

- A save must never lose a comment, a host or another section of `config.toml`, and a failure at any step must leave the original file untouched (Task 1 tests).
- A config the user wrote with `notify.mail_to = ...` (dotted key) or an inline table must be refused with a message, never silently duplicated (Task 1).
- Declining the save confirmation must return the form with what was typed (Task 4); `t` must not send before the confirmation (Task 5).
- A slow test message or preview must not freeze the screen, and a second `p` while one runs must not start another (Task 5).
- With nothing configured the screen must say the monitor is off and `t` must not offer to send (Tasks 3 and 5).

---

### Task 1: Section-only save in `config.rs`

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Produces: `pub fn render_notify_section(c: &NotifyConfig) -> String` (`[notify]` plus one `key = "value"` line per set key, TOML-escaped, ending in a newline); `pub fn set_notify_section(text: &str, section: &str) -> Result<String, String>`; `pub fn save_notify_section(path: &Path, section: &str) -> Result<()>`.

- [ ] **Step 1: Write the failing tests** (`src/config.rs` tests)

```rust
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
    assert_eq!(after, "# my hosts\n[[hosts]]\nname = \"a\"   # the first\nssh_target = \"u@a\"\n\n[notify]\nmail_to = \"new@x.org\"\n\n[ui]\ntheme = \"mono\"\n");
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
    assert!(!dir.path().join("config.toml.tmp").exists());
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
fn a_missing_file_is_created_and_a_failing_write_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("config.toml");
    save_notify_section(&path, &render_notify_section(&cfg("n@x.org"))).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[notify]\nmail_to = \"n@x.org\"\n");
    assert!(!dir.path().join("sub").join("config.toml.bak").exists(), "nothing to back up the first time");
    // A directory where the temporary file must go: the write fails, the original stays.
    std::fs::create_dir(dir.path().join("sub").join("config.toml.tmp")).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let err = save_notify_section(&path, &render_notify_section(&cfg("other@x.org"))).unwrap_err().to_string();
    assert!(err.contains("config.toml.tmp"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test config:: 2>&1 | tail` → compile errors.

- [ ] **Step 3: Implement** in `src/config.rs`

```rust
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

/// Replaces (or appends) the `[notify]` section of `text`, keeping every other byte. Refuses what it cannot edit
/// safely (a dotted `notify.` key, an inline `notify = {...}`, a `[notify.x]` sub-table, two `[notify]` headers)
/// and anything whose result does not parse back to the settings asked for.
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
    let new_section = fix(section);
    let result = match headers.first() {
        Some(&start) => {
            let end = lines.iter().enumerate().skip(start + 1).find(|(_, l)| is_header(l)).map_or(lines.len(), |(i, _)| i);
            let mut out: String = lines[..start].concat();
            out.push_str(&new_section);
            if end < lines.len() {
                out.push_str(&fix("\n"));
                out.push_str(&lines[end..].concat());
            }
            out
        }
        None => {
            let mut out = text.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push_str(&fix("\n"));
                }
                out.push_str(&fix("\n"));
            }
            out.push_str(&new_section);
            out
        }
    };
    let want = toml::from_str::<Config>(section).map_err(|e| format!("the new [notify] section does not parse: {e}"))?.notify;
    let got = toml::from_str::<Config>(&result).map_err(|e| format!("config.toml would no longer parse: {e}"))?.notify;
    if got != want {
        return Err("config.toml would not say what was asked: edit it by hand".to_string());
    }
    Ok(result)
}

/// Writes the new `[notify]` section into the file: the old file is copied to `<file>.bak`, the new text goes to
/// `<file>.tmp` with the old permissions and is renamed over it. Any failure leaves the original untouched.
pub fn save_notify_section(path: &Path, section: &str) -> Result<()> {
    let name = path.file_name().map_or_else(|| "config.toml".to_string(), |n| n.to_string_lossy().into_owned());
    let old = match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::Config(format!("reading {}: {e}", path.display()))),
    };
    let new = set_notify_section(old.as_deref().unwrap_or(""), section).map_err(AppError::Config)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Config(format!("creating {}: {e}", parent.display())))?;
    }
    if old.is_some() {
        let bak = path.with_file_name(format!("{name}.bak"));
        std::fs::copy(path, &bak).map_err(|e| AppError::Config(format!("writing {}: {e}", bak.display())))?;
    }
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, new).map_err(|e| AppError::Config(format!("writing {}: {e}", tmp.display())))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AppError::Config(format!("replacing {}: {e}", path.display()))
    })
}
```

- [ ] **Step 4: Run** — `cargo test config:: 2>&1 | grep "test result"` → pass. Mutation checks: let a dotted key through (the refusal test fails); append without the blank line (the append test fails); skip the permissions copy (the mode test fails); skip the backup (the backup test fails).

- [ ] **Step 5: Commit** — `feat: save only the [notify] section of config.toml, atomically and with a backup` (+ trailer).

---

### Task 2: The Alerts data

**Files:**
- Create: `src/ui/alerts.rs`
- Modify: `src/ui/mod.rs` (`pub mod alerts;`)

**Interfaces:**
- Consumes: Task 1; `config::{Config, NotifyConfig, NotifySettings}`; `daemon::health_text::{EventLog, HealthEvent}`.
- Produces: `pub struct AlertsView { pub loaded: bool, pub problem: Option<String>, pub saved: NotifyConfig, pub settings: Option<NotifySettings>, pub hook: Option<String>, pub last_report: Option<String>, pub events: Vec<HealthEvent>, pub scroll: usize }` (`Default`, `Debug`); `pub fn load(config_path: &Path, state_dir: &Path) -> AlertsView`; `AlertsView::monitor_on(&self) -> bool`; `AlertsView::monitor_text(&self) -> String`; `AlertsView::setting_rows(&self) -> Vec<(String, String, bool)>` (label, value, is_default); `pub fn event_line(e: &HealthEvent) -> String`; `pub const EVENTS_SHOWN: usize = 15`.

- [ ] **Step 1: Write the failing tests** (`ui/alerts.rs` tests, using a temp dir with a `config.toml`, `notify.json` and `health-events.jsonl`)

```rust
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
    use crate::daemon::health_text::{EventLog, HealthEvent};
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
    assert!(event_line(&v.events[0]).contains("a") && event_line(&v.events[0]).contains("alert") && event_line(&v.events[0]).contains("n19"));
}

#[test]
fn with_no_state_files_there_is_no_report_and_no_events() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "").unwrap();
    let v = load(&dir.path().join("config.toml"), dir.path());
    assert_eq!((v.last_report, v.events.len()), (None, 0));
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement** `ui/alerts.rs`:
  - `load`: read the file (`NotFound` → empty text); parse with `toml::from_str::<Config>`: a parse error gives `problem = "config.toml could not be read: {e}"` and defaults; on success `saved = config.notify.clone()`, `hook = config.daemon.notify_command.clone()`, `settings = match saved.resolve() { Ok(s) => Some(s), Err(e) => { problem = Some(e); None } }`. `last_report` from `<state_dir>/notify.json` (`{"last_report":"..."}`, same parse as the monitor). `events` = `EventLog::new(<state_dir>/health-events.jsonl).read()` reversed (newest first). `loaded = true`. A previous `scroll` is kept by the caller.
  - `monitor_on`: `settings.is_some_and(has_mail) || hook.is_some()`.
  - `monitor_text`: on: `Monitor on (every {n} min, report at {HH:MM})` or `..., no daily report)`; off: `Monitor off — set an email address to turn alerts on`; with a `problem` that is not a read error: `Monitor off — {problem}`.
  - `setting_rows` (label, value, is_default): `Email to` (`mail_to` joined by `, ` or `(not set)`, default = unset), `Mail command` (`mail_command` or `msmtp -t`), `Mail from` (`mail_from` or `zfsctl@<hub>`), `Check every` (`{n} minutes` or `1 minute`), `Daily report` (`HH:MM` or `off`), `Remind every` (`{n} hours` / `1 hour` / `never`), `Hook command` (`{cmd}   (edit in config.toml)` or `(not set)   (edit in config.toml)`, is_default = hook unset). `is_default` is true when the saved key is unset. Values come from the saved `NotifyConfig` (not the resolved settings), so an invalid file still shows what is saved.
  - `event_line`: `{local time %m-%d %H:%M}  {host}  {kind}  {text}` (local time of `at`).

- [ ] **Step 4: Run** — `cargo test ui::alerts 2>&1 | grep "test result"` → pass.

- [ ] **Step 5: Commit** — `feat: the data behind the Alerts screen`.

---

### Task 3: The Alerts section (read-only screen)

**Files:**
- Modify: `src/ui/state.rs` (`Section::Alerts`, `App.config_path`, `App.alerts_view`, `App.last_alerts_refresh`, refresh functions, scroll keys), `src/ui/layout.rs` (sidebar label, `draw_alerts`, `draw_main` arm, no metrics panel), `src/ui/hints.rs`, `src/ui/mod.rs` (refresh hooks, `app.config_path`), tests that count sections.

**Interfaces:**
- Consumes: Task 2.
- Produces: `Section::Alerts`; `App.config_path: PathBuf`; `App.alerts_view: AlertsView`; `App::refresh_alerts(&mut self)`; `App::refresh_alerts_if_stale(&mut self, now: Instant)` (every 5 s while the section is open); Up/Down scroll `alerts_view.scroll` (clamped to `events.len() - 1`).

- [ ] **Step 1: Write the failing tests**

state.rs:
```rust
fn alerts_app(config: &str) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.config_path = dir.path().join("config.toml");
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
    app.refresh_alerts();
    (app, dir)
}

#[test]
fn alerts_is_the_last_sidebar_section_and_tab_reaches_it() {
    let app = App::new(vec!["local".to_string()]);
    assert_eq!(app.sections.last(), Some(&Section::Alerts));
    assert_eq!(app.sections[app.sections.len() - 2], Section::Jobs);
}

#[test]
fn selecting_alerts_loads_it_and_up_down_scroll_the_events_within_bounds() {
    let (mut app, dir) = alerts_app("");
    let log = crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl"));
    for i in 0..5 {
        log.append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "a".into(), kind: "alert".into(), text: format!("n{i}") }).unwrap();
    }
    app.refresh_alerts();
    assert_eq!(app.alerts_view.events.len(), 5);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.alerts_view.scroll, 0, "not above the first");
    for _ in 0..9 {
        app.handle_key(KeyCode::Down);
    }
    assert_eq!(app.alerts_view.scroll, 4, "not past the last");
    app.handle_key(KeyCode::Up);
    assert_eq!(app.alerts_view.scroll, 3);
}

#[test]
fn the_alerts_data_is_reloaded_every_five_seconds_only_while_the_section_is_open() {
    let (mut app, dir) = alerts_app("");
    let t0 = Instant::now();
    app.last_alerts_refresh = Some(t0);
    std::fs::write(dir.path().join("config.toml"), "[notify]\nmail_to = \"a@x.org\"\n").unwrap();
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(2));
    assert!(!app.alerts_view.monitor_on(), "too soon");
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(6));
    assert!(app.alerts_view.monitor_on());
    app.selected_section = 0;
    std::fs::write(dir.path().join("config.toml"), "").unwrap();
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(60));
    assert!(app.alerts_view.monitor_on(), "another section is open: no reload");
}
```
layout.rs:
```rust
fn alerts_render_app(config: &str) -> (App, tempfile::TempDir) { /* as alerts_app, using the layout test module's App::new */ }

#[test]
fn the_alerts_screen_shows_settings_status_and_events() {
    let (mut app, dir) = alerts_render_app("[daemon]\nnotify_command = \"curl -s x\"\n\n[notify]\nmail_to = \"me@x.org\"\n");
    std::fs::write(dir.path().join("notify.json"), "{\"last_report\":\"2026-10-10\"}").unwrap();
    crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl")).append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "fbsd16".into(), kind: "alert".into(), text: "tank DEGRADED".into() }).unwrap();
    app.refresh_alerts();
    let text = render(&mut app, 140, 40).join("\n");
    for want in ["Alerts", "Email to", "me@x.org", "Mail command", "msmtp -t", "(default)", "Hook command", "curl -s x", "edit in config.toml", "Monitor on", "Last report sent 2026-10-10", "fbsd16", "alert", "tank DEGRADED", "Alerts"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn nothing_configured_says_the_monitor_is_off_and_how_to_turn_it_on() {
    let (mut app, _dir) = alerts_render_app("");
    let text = render(&mut app, 140, 40).join("\n");
    assert!(text.contains("Monitor off") && text.contains("(not set)") && text.contains("Last report: never"), "{text}");
}

#[test]
fn an_unreadable_config_is_reported_on_the_screen() {
    let (mut app, _dir) = alerts_render_app("[notify\nbroken");
    let text = render(&mut app, 140, 40).join("\n");
    assert!(text.contains("could not be read"), "{text}");
}

#[test]
fn a_narrow_terminal_cuts_long_values_with_an_ellipsis_and_never_panics() {
    let (mut app, _dir) = alerts_render_app("[notify]\nmail_to = \"a@x.org\"\nmail_command = \"msmtp -a default -t --some --very --long --set --of --options\"\n");
    for (w, h) in [(60, 20), (80, 24), (30, 10)] {
        let text = render(&mut app, w, h).join("\n");
        assert!(!text.is_empty());
    }
    assert!(render(&mut app, 80, 24).join("\n").contains('…'));
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**
  - `Section::Alerts`; append it to `App::new`'s `sections`; sidebar label `"Alerts"`; `draw_main` arm `Section::Alerts => draw_alerts(f, app, area)`; `wants_metrics` is unchanged (not listed) so the screen gets the whole body; the "never in `App::sections`" arm is unchanged.
  - `App`: `pub config_path: PathBuf` (default `PathBuf::from(crate::config::DEFAULT_CONFIG_PATH)`), `pub alerts_view: crate::ui::alerts::AlertsView`, `last_alerts_refresh: Option<Instant>`. `refresh_alerts`: `let scroll = self.alerts_view.scroll; self.alerts_view = alerts::load(&self.config_path, &self.state_dir); self.alerts_view.scroll = scroll.min(self.alerts_view.events.len().saturating_sub(1));`. `refresh_alerts_if_stale(now)`: only when `current_section() == Section::Alerts`, every 5 s (and at once when `last_alerts_refresh` is `None`).
  - `refresh_visible` (state.rs ~599): `Section::Alerts => self.refresh_alerts()`. `ui/mod.rs`: both `Section::Jobs => app.refresh_jobs()` matches get `Section::Alerts => app.refresh_alerts()`; call `app.refresh_alerts_if_stale(Instant::now())` next to `refresh_jobs_if_stale`; in `run_app` after creating the App set `app.config_path = config_path.to_path_buf()`.
  - Keys: `KeyCode::Down`/`Up` `if self.current_section() == Section::Alerts` change `alerts_view.scroll` by one, clamped to `0..=events.len().saturating_sub(1)`.
  - `draw_alerts`: a bordered block titled `Alerts — daemon running (pid N)` / `Alerts — daemon NOT running` (from `app.jobs_view.daemon(now)`, the style of `draw_jobs`). Inside, lines: `Settings (config.toml)` heading dim; one line per `setting_rows()` as `{label:<14} {value}` with the value followed by `   (default)` dim when `is_default` (not for the hook row, whose text already says what it is); a blank; `app.alerts_view.problem` in the error style when set (this includes `could not be read`); the `monitor_text()`; `Last report sent {date}` or `Last report: never` (only when the monitor is on; when it is off the screen says `Last report: never` as well, see the test); a blank; `Recent events` heading; `event_line` for `events[scroll..]` up to the remaining height, with `(none yet)` when empty. Every line is cut with the existing ellipsis helper to the inner width.
  - `hints.rs`: `(Section::Alerts, _) => { if !app.alerts_view.events.is_empty() { v.push(h("↑↓", "Scroll", 3)); } v.push(h("r", "Refresh", 4)); }` (the `e`, `t`, `p` hints arrive in Tasks 4 and 5). Add `go(&mut a, Section::Alerts)` to the hints drift test's scenario list and the alerts scroll position to its `Fingerprint`.
  - Fix the existing tests that count or wrap sections (`wraps to the first section`, any that expect Jobs to be last) to the new list.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Mutation: stop clamping the scroll (the bounds test fails); reload while another section is open (the reload test fails).

- [ ] **Step 5: Commit** — `feat: the Alerts section shows the alert settings, the monitor status and the recent events`.

---

### Task 4: Editing and saving the settings

**Files:**
- Modify: `src/ui/modal.rs` (`FormKind::NotifySettings`, `MutationCall::SaveNotify`, `done_note`), `src/ui/state.rs` (form, build, confirm, done note), `src/ui/hints.rs`.

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `FormKind::NotifySettings`; `MutationCall::SaveNotify { config_path: PathBuf, section: String }` (tone Neutral, label `SAVE ALERT SETTINGS`, `run` calls `config::save_notify_section`); `MutationCall::done_note(&self) -> Option<String>` (`Some("Alert settings saved; the daemon applies them within seconds")` for `SaveNotify`, `None` for all others); `App.done_note: Option<String>`; `fn build_notify_action(fields: &[FormField], config_path: &Path) -> Result<(PendingAction, String), String>` (the action and the confirmation prompt); `App::alerts_form(&self) -> Option<Modal>`.

- [ ] **Step 1: Write the failing tests** (state.rs)

```rust
fn open_alerts_form(app: &mut App) -> Vec<String> {
    app.handle_key(KeyCode::Char('e'));
    form_values(app)
}

#[test]
fn e_opens_the_form_prefilled_with_the_saved_values_and_blank_for_unset() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 10\nreport_time = \"off\"\nremind_hours = 0\n");
    assert_eq!(open_alerts_form(&mut app), ["me@x.org", "", "", "10", "off", "0"]);
}

#[test]
fn e_is_refused_while_config_toml_cannot_be_read() {
    let (mut app, _dir) = alerts_app("[notify\nbroken");
    app.handle_key(KeyCode::Char('e'));
    assert!(app.modal.is_none());
    assert!(app.status_message.clone().unwrap_or_default().contains("could not be read"));
}

fn fill(app: &mut App, values: [&str; 6]) {
    if let Some(Modal::Form { fields, .. }) = &mut app.modal {
        for (f, v) in fields.iter_mut().zip(values) {
            f.value = v.to_string();
        }
    }
}

#[test]
fn a_bad_value_is_refused_with_the_daemons_message_and_the_form_stays_open() {
    for (values, want) in [
        (["not an address", "", "", "", "", ""], "mail_to"),
        (["a@x.org", "", "", "abc", "", ""], "whole number"),
        (["a@x.org", "", "", "0", "", ""], "check_interval_minutes"),
        (["a@x.org", "", "", "", "7am", ""], "report_time"),
        (["a@x.org", "", "", "", "", "-1"], "whole number"),
        (["a@x.org", "", "", "", "", "99999"], "remind_hours"),
    ] {
        let (mut app, _dir) = alerts_app("");
        open_alerts_form(&mut app);
        fill(&mut app, values);
        for _ in 0..6 {
            app.handle_modal_key(KeyCode::Enter);
        }
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?}: the form stays open");
        assert!(app.status_message.clone().unwrap_or_default().contains(want), "{values:?}: {:?}", app.status_message);
    }
}

#[test]
fn a_good_form_asks_for_the_two_key_confirmation_and_saves_only_the_notify_section() {
    let (mut app, dir) = alerts_app("# keep\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    open_alerts_form(&mut app);
    fill(&mut app, ["me@x.org", "", "", "10", "", ""]);
    let mut action = None;
    for _ in 0..6 {
        action = app.handle_modal_key(KeyCode::Enter);
    }
    assert!(action.is_none(), "no save before the confirmation");
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("me@x.org") && prompt.contains("10 min") && prompt.contains("07:00"), "{prompt}");
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter applies");
    let MutationCall::SaveNotify { config_path, section } = &action.call else { panic!("{:?}", action.call) };
    assert_eq!(section, "[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 10\n");
    crate::config::save_notify_section(config_path, section).unwrap();
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(saved.starts_with("# keep\n[[hosts]]") && saved.contains("mail_to = \"me@x.org\""), "{saved}");
}

#[test]
fn declining_the_save_returns_the_form_with_what_was_typed_and_writes_nothing() {
    let (mut app, dir) = alerts_app("");
    open_alerts_form(&mut app);
    fill(&mut app, ["me@x.org", "", "", "10", "", ""]);
    for _ in 0..6 {
        app.handle_modal_key(KeyCode::Enter);
    }
    app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert_eq!(form_values(&app)[0], "me@x.org");
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), "");
}

#[test]
fn clearing_the_address_is_allowed_and_turns_email_off_in_the_prompt() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\n");
    open_alerts_form(&mut app);
    fill(&mut app, ["", "", "", "", "", ""]);
    for _ in 0..6 {
        app.handle_modal_key(KeyCode::Enter);
    }
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("email off"), "{prompt}");
}

#[test]
fn a_saved_note_replaces_the_blank_status_after_a_successful_save_and_is_dropped_on_failure() {
    let (mut app, _dir) = alerts_app("");
    app.done_note = Some("Alert settings saved; the daemon applies them within seconds".to_string());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message.as_deref(), Some("Alert settings saved; the daemon applies them within seconds"));
    app.done_note = Some("x".into());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Err(crate::error::AppError::Config("boom".into())))).unwrap();
    app.drain_worker_messages();
    assert!(app.done_note.is_none());
}
```
modal.rs:
```rust
#[test]
fn save_notify_writes_the_section_through_the_config_helpers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    MutationCall::SaveNotify { config_path: path.clone(), section: "[notify]\nmail_to = \"a@x.org\"\n".into() }.run(&crate::exec::test_support::FakeRunner::new("")).unwrap();
    assert!(std::fs::read_to_string(path).unwrap().contains("a@x.org"));
    assert_eq!(MutationCall::SaveNotify { config_path: "x".into(), section: String::new() }.tone(), Tone::Neutral);
    assert!(MutationCall::SaveNotify { config_path: "x".into(), section: String::new() }.done_note().unwrap().contains("saved"));
    assert!(MutationCall::DeleteJob { state_dir: "x".into(), id: "j".into() }.done_note().is_none());
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**
  - `modal.rs`: add the variant to `MutationCall` (doc: "Replaces the `[notify]` section of `config.toml`. A local file edit: the runner is not used."), the `tone` arm (Neutral, with the other neutral calls), the `label` arm `"SAVE ALERT SETTINGS"`, the `run` arm `crate::config::save_notify_section(config_path, section)`, and `done_note`. Add `FormKind::NotifySettings` (doc: "Fields: `[email to, mail command, mail from, check every (minutes), daily report at (HH:MM or off), remind every (hours)]`; blank = unset").
  - `state.rs`: `App.done_note: Option<String>` (init `None`). Where a confirmed or direct `PendingAction` is started (the function that spawns the mutation thread, near `WorkerMsg::MutationDone` sends), set `self.done_note = action.call.done_note()` before spawning. In the `MutationDone` handler: `Ok(()) => { self.status_message = self.done_note.take(); ... }`, and `Err(_) => { self.done_note = None; ... }` (keep the existing error handling).
  - `alerts_form`: `None` when `alerts_view.problem` starts with `config.toml could not be read` (the key sets `status_message = Some(format!("{problem}: fix config.toml before editing here"))`); otherwise `Modal::Form { title: "Alert settings", fields: [Email to, Mail command, Mail from, Check every (minutes), Daily report at (HH:MM or off), Remind every (hours)] pre-filled from `alerts_view.saved` (`check_interval_minutes`/`remind_hours` as decimal text, unset as empty), focused: 0, description: Some("Blank means the default. Clear Email to to turn email off."), kind: FormKind::NotifySettings }`.
  - `KeyCode::Char('e') if self.current_section() == Section::Alerts => { match self.alerts_form() { Some(m) => self.modal = Some(m), None => <status message as above> } }`.
  - In the form Enter handler add the branch `else if matches!(kind, FormKind::NotifySettings)`: `match build_notify_action(&fields, &self.config_path) { Ok((mut action, prompt)) => { action.host = self.selected_host; self.confirm_restore = Some((prompt.clone(), Modal::Form { title, fields, focused, description, kind })); self.modal = Some(Modal::Confirm { choice: None, prompt, action }); None } Err(msg) => { self.set_error(msg); self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind }); None } }`.
  - `build_notify_action`: read each field trimmed (`blank` → `None`); `check_interval_minutes` and `remind_hours` parse as `u64` or fail with `Check every must be a whole number of minutes` / `Remind every must be a whole number of hours`; build `NotifyConfig`; `let settings = cfg.resolve()?` (the daemon's own message); `section = render_notify_section(&cfg)`; prompt: `Save alert settings?\n  {Email to a@x.org | email off}\n  Check every {n} min, report at {HH:MM | no daily report}, remind every {h} hours (or never)\nThe daemon applies them within seconds. (y/n)` from the resolved `settings`; action `PendingAction { host: 0, section: Section::Alerts, call: MutationCall::SaveNotify { config_path: config_path.to_path_buf(), section } }`.
  - After a successful save the post-mutation refresh reloads the screen (`Section::Alerts => app.refresh_alerts()` from Task 3).
  - `hints.rs`: `(Section::Alerts, _)` gets `h("e", "Edit settings", 1)` (first). Update the form-related drift tests if the compiler flags exhaustive matches over `FormKind`.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Mutation: skip the confirmation (the "no save before the confirmation" test fails); drop `confirm_restore` (the decline test fails); write blank fields (the section assertion fails).

- [ ] **Step 5: Commit** — `feat: edit and save the alert settings from the Alerts screen`.

---

### Task 5: Test message and report preview

**Files:**
- Modify: `src/ui/modal.rs` (`MutationCall::SendTestMessage`), `src/ui/state.rs` (`t`, `p`, `WorkerMsg::AlertsPreview`), `src/ui/hints.rs`.

**Interfaces:**
- Consumes: Tasks 3–4; `notify_cli::{notify_test, report_text_with}`, `monitor::{probe_all, Busy, PROBE_DEADLINE}`.
- Produces: `MutationCall::SendTestMessage { settings: Box<NotifySettings>, hook: Option<String> }` (tone Neutral, label `SEND TEST MESSAGE`, `run`: `notify_cli::notify_test`, `Ok(())` when its code is 0, else `Err(AppError::Config(text.trim()))`; `done_note` = `Some("Test message sent: the mail command accepted it")`); `WorkerMsg::AlertsPreview(String)`; `App.alerts_preview_busy: bool`.

- [ ] **Step 1: Write the failing tests**

modal.rs:
```rust
#[test]
fn a_test_message_succeeds_only_when_every_channel_took_it() {
    let ok = crate::config::NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("cat > /dev/null".into()), ..Default::default() }.resolve().unwrap();
    let run = |s: &crate::config::NotifySettings, hook: Option<&str>| MutationCall::SendTestMessage { settings: Box::new(s.clone()), hook: hook.map(str::to_string) }.run(&crate::exec::test_support::FakeRunner::new(""));
    assert!(run(&ok, Some("true")).is_ok());
    let bad = crate::config::NotifyConfig { mail_to: Some("a@x.org".into()), mail_command: Some("exit 78".into()), ..Default::default() }.resolve().unwrap();
    assert!(run(&bad, None).unwrap_err().to_string().contains("mail: failed: exited with status 78"));
}
```
state.rs:
```rust
#[test]
fn t_with_nothing_configured_says_so_and_asks_nothing() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none());
    assert!(app.status_message.clone().unwrap_or_default().contains("set an email address"), "{:?}", app.status_message);
}

#[test]
fn t_asks_first_naming_the_recipient_and_the_command_and_sends_only_after_y_then_enter() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\nmail_command = \"msmtp -a default -t\"\n");
    app.handle_key(KeyCode::Char('t'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("me@x.org") && prompt.contains("msmtp -a default -t"), "{prompt}");
    assert!(matches!(action.call, MutationCall::SendTestMessage { .. }));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter alone does nothing");
    app.handle_modal_key(KeyCode::Char('y'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some());
}

#[test]
fn t_uses_the_saved_settings_and_an_invalid_saved_section_refuses() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"nope\"\n");
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none() && app.status_message.clone().unwrap_or_default().contains("mail_to"), "{:?}", app.status_message);
}

#[test]
fn p_runs_the_preview_in_the_background_and_shows_it_in_a_popup_without_a_second_run_while_busy() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('p'));
    assert!(app.alerts_preview_busy);
    assert!(app.status_message.clone().unwrap_or_default().contains("Preparing"));
    app.handle_key(KeyCode::Char('p'));
    app.tx.send(WorkerMsg::AlertsPreview("Subject: x\n\nbody".to_string())).unwrap();
    app.drain_worker_messages();
    assert!(!app.alerts_preview_busy);
    let Some(Modal::Info { title, body }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(title.contains("nothing sent") && body.contains("Subject: x"), "{title}");
}
```
(For the busy assertion, count threads by checking that a second `p` while `alerts_preview_busy` leaves the status message unchanged and spawns nothing: assert via the flag staying true and no panic; the worker result in the test is injected through the channel, so the real worker's own result also arrives: tolerate it by draining until the modal is set.)

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**
  - `modal.rs`: the `SendTestMessage` variant (doc: "Sends a real test message through the saved channels (`zfsctl notify-test`). The runner is not used."), `tone` Neutral, `label` `SEND TEST MESSAGE`, `run` as above, `done_note`.
  - `state.rs`: `t`: `match &self.alerts_view.settings { Some(s) if self.alerts_view.monitor_on() => confirm, Some(_) | None => status }`: when `alerts_view.settings` is `None` because of an invalid section, `status_message = Some(problem)`; when no channel is configured `status_message = Some("Nothing to test: set an email address (e) or a notify_command in config.toml".into())`; otherwise open `Modal::Confirm { choice: None, prompt: "Send a test message?\n  To: {mail_to joined}\n  Command: {mail_command}\n  (and the notify hook)\nThis sends a real message. (y/n)", action: PendingAction { host: self.selected_host, section: Section::Alerts, call: MutationCall::SendTestMessage { settings: Box::new(s.clone()), hook: self.alerts_view.hook.clone() } } }` (the `To`/`Command` lines only when mail is configured; the hook line only when a hook is set).
  - `p`: if `alerts_preview_busy` return; set it, `status_message = Some("Preparing the report preview (probing the hosts)…")`, clone `config_path` and `state_dir`, spawn a thread: `let config = Config::load(&path)`; `text = match config { Ok(c) => notify_cli::report_text_with(&c, &state_dir, Utc::now(), &|h| probe_all(h, &|_| true, PROBE_DEADLINE, &Busy::default())), Err(e) => format!("config.toml could not be read: {e}") }`; send `WorkerMsg::AlertsPreview(text)`. Handler: `alerts_preview_busy = false; status_message = None; modal = Some(Modal::Info { title: "Report preview (nothing sent)".into(), body: text })`.
  - `hints.rs`: `(Section::Alerts, _)` gets `h("t", "Send test", 2)` and `h("p", "Preview report", 2)`. The drift tests require each advertised key to change the fingerprint: `t` with nothing configured sets a status message, `p` sets one too (both are covered by `fingerprint_with_status`).

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Mutation: send without the confirmation (the confirm test fails); allow a second preview while busy (add a counter in a test seam or assert the flag logic).

- [ ] **Step 5: Commit** — `feat: send a test message and preview the report from the Alerts screen`.

---

### Task 6: Docs, Help and the live check

**Files:**
- Modify: `README.md`, `src/ui/state.rs` (`help_text`)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn help_documents_the_alerts_section_and_its_keys() {
    let text = help_text();
    for want in ["Alerts", "edit the alert settings", "test message", "preview"] {
        assert!(text.contains(want), "missing {want:?}");
    }
}
```
Run it → FAIL.

- [ ] **Step 2: Implement** `help_text` gets an `Alerts` block (`e` edit the alert settings, saved into the `[notify]` section of config.toml only; `t` send a test message (real mail, asks first); `p` preview the daily report (sends nothing); `r` reload; Up/Down scroll the events). README: a short subsection under the health-alerts paragraph describing the Alerts screen, its keys, and the save method (only `[notify]` is replaced; `config.toml.bak` holds the previous file; a `[notify]` written as dotted keys or an inline table must be edited by hand; the hook stays in the file).

- [ ] **Step 3: Run the suite** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0.

- [ ] **Step 4: Live check on the hub** (deploy by tar over ssh and `cargo build --release` in `~/zfsctl`; run `target/release/zfsctl` driven by the `tui.py` pexpect+pyte harness as root; the installed daemon is not touched). **Ask the user before anything is sent.** Back up the hub's `config.toml` first and restore it afterwards.
  1. Select Alerts: the saved settings, `Monitor on`, the last report date and the events match `config.toml` and `/var/db/zfsctl`.
  2. `e`, change the check interval, confirm: the file diff against the backup shows only the `[notify]` lines changed (comments and other sections identical); `config.toml.bak` equals the backup; the daemon log shows `health monitor settings changed`.
  3. `e`, Esc and a declined confirmation write nothing.
  4. `p`: the preview pop-up appears and nothing is sent or recorded.
  5. With the user's agreement: `t`, confirm, and the test message arrives.
  6. Restore the config; record the observations in the spec's "Live results" and commit.

- [ ] **Step 5: Commit** — `docs: the Alerts screen` (+ trailer).
