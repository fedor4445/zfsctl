# Health alerts and a daily report by email Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon probes every host, alerts by email (msmtp) and the existing notify hook when a host turns bad, escalates, recovers or stays bad, and sends a daily all-hosts report; `zfsctl notify-test` and `zfsctl report` let the operator check it.

**Architecture:** A `[notify]` config section resolves to `NotifySettings`. `daemon/deliver.rs` composes mail and runs the mail command and the hook (message text on stdin and in environment variables only). `daemon/health.rs` holds the pure alert `Tracker`. `daemon/health_text.rs` builds alert and report text from plain data. `daemon/monitor.rs` probes hosts in parallel, feeds the tracker, queues and retries deliveries and schedules the daily report. `daemon::run` starts the monitor thread when a channel is configured.

**Tech Stack:** Rust, chrono, serde/toml, existing `host_health` (`probe_host_guarded`, `assess`, cell functions), `FakeRunner` tests.

**Spec:** `docs/superpowers/specs/2026-10-10-health-notify-design.md`

## Global Constraints

- No new privileged command and no sudoers change: hosts are probed with the existing `zpool list` / `zpool status` through the existing runners.
- Message text reaches the mail command and the hook only on stdin and in environment variables, never on a command line.
- Mail headers are single-line by construction (CR/LF and other control characters removed); subjects are ASCII (anything else becomes `?`).
- A config without `[notify]` loads and saves unchanged; `[daemon] notify_command` keeps its name and meaning, and old hooks that read only `ZFSCTL_JOB/HOST/KIND/RESULT/MESSAGE` keep working.
- The monitor does no probing and no ssh unless a channel is configured (`mail_to` set, or `notify_command` set).
- Defaults: `mail_command = "msmtp -t"`, `check_interval_minutes = 5` (minimum 1), `report_time = "07:00"` (`"off"` disables), `remind_hours = 6` (`0` = never). Confirmation: 2 consecutive probes. Delivery retries: at most 6. Delivery timeout 30 s. Probe give-up 60 s. Event log trimmed to 2000 lines. Report sections cut at 40 entries with `(+N more)`.
- Run `cargo test` and `cargo clippy` (must stay at 0 warnings) before every commit that ends a task.

## Review Focus

- A single bad probe (blip) must never alert; a recovery must never be sent for a host that never alerted (Task 3 tests).
- A host name or job message containing CR/LF or non-ASCII must not add a header or break the subject (Task 2 test).
- A mail command that hangs or fails must not stop the hook, the monitor tick or the daemon's job threads (Task 2 and 5 tests).
- The daily report must be sent once per local day, also when the daemon was down at `report_time`, and never twice (Task 5 test).
- A bad `[notify]` section must keep the previous settings and be logged, not crash the daemon (Task 6 test).

---

### Task 1: The `[notify]` configuration

**Files:**
- Modify: `src/config.rs` (new `NotifyConfig`, `NotifySettings`, `Config.notify`)

**Interfaces:**
- Produces: `Config.notify: NotifyConfig`; `NotifyConfig::resolve(&self) -> Result<NotifySettings, String>`; `NotifySettings { mail_to: Vec<String>, mail_command: String, mail_from: Option<String>, check_interval: Duration, report_time: Option<chrono::NaiveTime>, remind: Option<Duration> }` (derives `Debug, Clone, PartialEq`, and `Default` = nothing configured with the documented defaults); `NotifySettings::has_mail(&self) -> bool`; `pub const DEFAULT_MAIL_COMMAND: &str = "msmtp -t"`.

- [ ] **Step 1: Write the failing tests** (in `src/config.rs` tests)

```rust
#[test]
fn a_config_without_notify_loads_saves_unchanged_and_resolves_to_the_defaults() {
    let config: Config = toml::from_str("[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n").unwrap();
    assert_eq!(config.notify, NotifyConfig::default());
    assert!(!toml::to_string_pretty(&config).unwrap().contains("notify"));
    let s = config.notify.resolve().unwrap();
    assert!(s.mail_to.is_empty() && !s.has_mail());
    assert_eq!(s.mail_command, "msmtp -t");
    assert_eq!(s.check_interval, std::time::Duration::from_secs(300));
    assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap()));
    assert_eq!(s.remind, Some(std::time::Duration::from_secs(6 * 3600)));
    assert_eq!(s.mail_from, None);
}

#[test]
fn every_notify_key_parses_and_round_trips() {
    let text = "[notify]\nmail_to = \"a@x.org, b@y.org\"\nmail_command = \"msmtp -a default -t\"\nmail_from = \"zfsctl@hub\"\ncheck_interval_minutes = 10\nreport_time = \"06:30\"\nremind_hours = 0\n";
    let config: Config = toml::from_str(text).unwrap();
    let s = config.notify.resolve().unwrap();
    assert_eq!(s.mail_to, ["a@x.org", "b@y.org"]);
    assert_eq!((s.mail_command.as_str(), s.mail_from.as_deref()), ("msmtp -a default -t", Some("zfsctl@hub")));
    assert_eq!(s.check_interval, std::time::Duration::from_secs(600));
    assert_eq!(s.report_time, Some(chrono::NaiveTime::from_hms_opt(6, 30, 0).unwrap()));
    assert_eq!(s.remind, None, "0 hours means never");
    assert_eq!(toml::from_str::<Config>(&toml::to_string_pretty(&config).unwrap()).unwrap(), config);
}

#[test]
fn report_time_off_disables_the_report() {
    let c = NotifyConfig { report_time: Some("off".into()), ..NotifyConfig::default() };
    assert_eq!(c.resolve().unwrap().report_time, None);
}

#[test]
fn bad_notify_values_are_refused_with_the_reason() {
    let bad = |c: NotifyConfig| c.resolve().unwrap_err();
    let base = NotifyConfig::default();
    for to in ["nobody", "a@", "@x.org", "a b@x.org", "a@x.org\nBcc: z@z.org", "a@x@y", "<a@x.org>", "a@x.org,,b@y.org"] {
        assert!(bad(NotifyConfig { mail_to: Some(to.into()), ..base.clone() }).contains("mail_to"), "{to:?}");
    }
    assert!(bad(NotifyConfig { check_interval_minutes: Some(0), ..base.clone() }).contains("check_interval_minutes"));
    assert!(bad(NotifyConfig { report_time: Some("7am".into()), ..base.clone() }).contains("report_time"));
    assert!(bad(NotifyConfig { report_time: Some("25:00".into()), ..base.clone() }).contains("report_time"));
    assert!(bad(NotifyConfig { mail_command: Some("  ".into()), ..base.clone() }).contains("mail_command"));
    assert!(bad(NotifyConfig { mail_from: Some("a\r\nb".into()), ..base }).contains("mail_from"));
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test config:: 2>&1 | tail` → compile errors (`NotifyConfig` missing).

- [ ] **Step 3: Implement** in `src/config.rs`

```rust
pub const DEFAULT_MAIL_COMMAND: &str = "msmtp -t";

/// Health alerts and the daily report (`[notify]`). Unset keys take the documented defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NotifyConfig {
    /// One address or several, comma separated. Unset means no email.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_to: Option<String>,
    /// Run through `sh -c` with the whole message (headers included) on stdin. Default `msmtp -t`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mail_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub check_interval_minutes: Option<u64>,
    /// `HH:MM` on the hub's clock, or `off`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub report_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub remind_hours: Option<u64>,
}

impl NotifyConfig {
    fn is_default(&self) -> bool {
        *self == NotifyConfig::default()
    }

    /// Validates the section and fills in the defaults.
    pub fn resolve(&self) -> std::result::Result<NotifySettings, String> {
        let single_line = |s: &str| !s.chars().any(char::is_control);
        let valid_address = |a: &str| {
            let mut parts = a.split('@');
            matches!((parts.next(), parts.next(), parts.next()), (Some(l), Some(d), None) if !l.is_empty() && !d.is_empty())
                && !a.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | '<' | '>' | '"'))
        };
        let mail_to: Vec<String> = match self.mail_to.as_deref() {
            None => Vec::new(),
            Some(list) => {
                let parts: Vec<String> = list.split(',').map(|p| p.trim().to_string()).collect();
                if let Some(bad) = parts.iter().find(|p| !valid_address(p)) {
                    return Err(format!("notify.mail_to: '{bad}' is not a plain address (user@host, no spaces or control characters)"));
                }
                parts
            }
        };
        let mail_command = match self.mail_command.as_deref() {
            None => DEFAULT_MAIL_COMMAND.to_string(),
            Some(c) if c.trim().is_empty() => return Err("notify.mail_command is empty".to_string()),
            Some(c) => c.to_string(),
        };
        if let Some(from) = self.mail_from.as_deref() {
            if from.trim().is_empty() || !single_line(from) {
                return Err("notify.mail_from must be one line".to_string());
            }
        }
        let minutes = self.check_interval_minutes.unwrap_or(5);
        if minutes < 1 {
            return Err("notify.check_interval_minutes must be at least 1".to_string());
        }
        let report_time = match self.report_time.as_deref() {
            None => chrono::NaiveTime::from_hms_opt(7, 0, 0),
            Some("off") => None,
            Some(t) => Some(chrono::NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| format!("notify.report_time '{t}' is not HH:MM (or off)"))?),
        };
        let remind = match self.remind_hours.unwrap_or(6) {
            0 => None,
            h => Some(std::time::Duration::from_secs(h * 3600)),
        };
        Ok(NotifySettings { mail_to, mail_command, mail_from: self.mail_from.clone(), check_interval: std::time::Duration::from_secs(minutes * 60), report_time, remind })
    }
}

/// `NotifyConfig` validated, with the defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct NotifySettings {
    pub mail_to: Vec<String>,
    pub mail_command: String,
    pub mail_from: Option<String>,
    pub check_interval: std::time::Duration,
    pub report_time: Option<chrono::NaiveTime>,
    pub remind: Option<std::time::Duration>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifyConfig::default().resolve().unwrap_or_else(|_| unreachable!("the defaults always validate"))
    }
}

impl NotifySettings {
    pub fn has_mail(&self) -> bool {
        !self.mail_to.is_empty()
    }
}
```
and in `Config`:
```rust
    #[serde(default, skip_serializing_if = "NotifyConfig::is_default")]
    pub notify: NotifyConfig,
```
Fix any `Config { .. }` literal the compiler flags with `notify: NotifyConfig::default()` (most use `..Default::default()`).

- [ ] **Step 4: Run** — `cargo test config:: 2>&1 | grep "test result"` → pass. Mutation check: let an address with a comma inside through and watch `bad_notify_values_are_refused_with_the_reason` fail.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat: [notify] configuration for health alerts and the daily report"` (+ trailer).

---

### Task 2: Composing and delivering messages

**Files:**
- Create: `src/daemon/deliver.rs`
- Modify: `src/daemon/mod.rs` (`pub mod deliver;`), `src/daemon/notify.rs` (generalise the command runner), `src/exec/local.rs` (expose the child preparation)

**Interfaces:**
- Consumes: Task 1 `NotifySettings`.
- Produces: in `exec/local.rs` `pub fn prepare_child(command: &mut std::process::Command)` (PATH plus package dirs, account HOME; what `local()` already does); in `notify.rs` `pub fn run_command(command: &str, env: &[(&str, &str)], stdin: &str, timeout: Duration) -> Result<(), NotifyError>`; in `deliver.rs`:
  - `pub struct Message { pub kind: String, pub host: String, pub level: String, pub subject: String, pub body: String }`
  - `pub fn ascii_line(s: &str) -> String`
  - `pub fn compose_mail(from: &str, to: &[String], subject: &str, body: &str, now: DateTime<Utc>) -> String`
  - `pub fn default_from() -> String` (`zfsctl@<hostname>`)
  - `#[derive(Debug, PartialEq, Eq, Clone, Copy)] pub enum Channel { Mail, Hook }`
  - `pub struct Delivery { pub channel: Channel, pub result: Result<(), NotifyError> }`
  - `pub fn send(settings: &NotifySettings, hook: Option<&str>, msg: &Message, now: DateTime<Utc>, timeout: Duration) -> Vec<Delivery>` (one entry per configured channel; empty when none)
  - `pub fn delivered(results: &[Delivery]) -> bool` (at least one channel ok).

- [ ] **Step 1: Write the failing tests** (`src/daemon/deliver.rs` tests)

```rust
use super::*;
use crate::config::NotifyConfig;

fn settings(command: &str, to: &str) -> NotifySettings {
    NotifyConfig { mail_to: Some(to.into()), mail_command: Some(command.into()), ..NotifyConfig::default() }.resolve().unwrap()
}

fn msg(subject: &str, body: &str) -> Message {
    Message { kind: "health-alert".into(), host: "fbsd16".into(), level: "critical".into(), subject: subject.into(), body: body.into() }
}

fn at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-11T07:00:00Z").unwrap().with_timezone(&Utc)
}

#[test]
fn a_composed_mail_has_every_header_then_a_blank_line_then_the_body_verbatim() {
    let text = compose_mail("zfsctl@hub", &["a@x.org".into(), "b@y.org".into()], "[zfsctl] fbsd16: tank DEGRADED", "line one\nline two\n", at());
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let lines: Vec<&str> = head.split("\r\n").collect();
    assert_eq!(lines[0], "From: zfsctl@hub");
    assert_eq!(lines[1], "To: a@x.org, b@y.org");
    assert_eq!(lines[2], "Subject: [zfsctl] fbsd16: tank DEGRADED");
    assert!(lines[3].starts_with("Date: ") && lines[3].contains("2026"));
    assert!(lines.contains(&"MIME-Version: 1.0") && lines.contains(&"Content-Type: text/plain; charset=utf-8") && lines.contains(&"Content-Transfer-Encoding: 8bit"));
    assert_eq!(body, "line one\nline two\n");
}

#[test]
fn a_subject_or_sender_cannot_add_a_header_or_carry_non_ascii() {
    assert_eq!(ascii_line("a\r\nBcc: z@z.org\tb"), "a Bcc: z@z.org b");
    assert_eq!(ascii_line("héllo ✓"), "h?llo ?");
    let text = compose_mail("evil\r\nBcc: z@z.org", &["a@x.org".into()], "x\r\nBcc: z@z.org", "body", at());
    let head = text.split_once("\r\n\r\n").unwrap().0;
    assert!(!head.lines().any(|l| l.starts_with("Bcc:")), "{head}");
    assert_eq!(head.lines().filter(|l| l.starts_with("Subject:")).count(), 1);
}

#[test]
fn the_mail_command_receives_the_whole_message_on_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mail");
    let s = settings(&format!("cat > {}", out.display()), "me@x.org");
    let r = send(&s, None, &msg("[zfsctl] fbsd16: down", "body text"), at(), Duration::from_secs(10));
    assert_eq!(r.len(), 1);
    assert!(delivered(&r));
    let got = std::fs::read_to_string(out).unwrap();
    assert!(got.contains("To: me@x.org") && got.contains("Subject: [zfsctl] fbsd16: down") && got.ends_with("body text"), "{got}");
}

#[test]
fn the_hook_gets_the_text_in_the_environment_and_on_stdin_and_runs_even_if_mail_fails() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("hook");
    let hook = format!("printf '%s|%s|%s|%s|%s|' \"$ZFSCTL_KIND\" \"$ZFSCTL_HOST\" \"$ZFSCTL_RESULT\" \"$ZFSCTL_MESSAGE\" \"$ZFSCTL_BODY\" > {0}; cat >> {0}", out.display());
    let s = settings("exit 3", "me@x.org");
    let r = send(&s, Some(&hook), &msg("[zfsctl] fbsd16: down", "the body"), at(), Duration::from_secs(10));
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].result, Err(NotifyError::Exit(3)));
    assert!(r[1].result.is_ok() && delivered(&r), "the hook is independent of the mail command");
    assert_eq!(std::fs::read_to_string(out).unwrap(), "health-alert|fbsd16|critical|[zfsctl] fbsd16: down|the body|the body");
}

#[test]
fn no_channel_means_nothing_is_sent_and_nothing_counts_as_delivered() {
    let s = NotifySettings::default();
    let r = send(&s, None, &msg("s", "b"), at(), Duration::from_secs(1));
    assert!(r.is_empty() && !delivered(&r));
}

#[test]
fn a_hanging_mail_command_is_killed_at_the_timeout() {
    let s = settings("sleep 30", "me@x.org");
    let t = std::time::Instant::now();
    let r = send(&s, None, &msg("s", "b"), at(), Duration::from_millis(300));
    assert_eq!(r[0].result, Err(NotifyError::TimedOut));
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_command_that_ignores_stdin_does_not_block_a_large_body() {
    let s = settings("true", "me@x.org");
    let big = "x".repeat(2_000_000);
    assert!(delivered(&send(&s, None, &msg("s", &big), at(), Duration::from_secs(10))));
}

#[test]
fn the_default_sender_names_the_hub() {
    assert!(default_from().starts_with("zfsctl@") && default_from().len() > "zfsctl@".len());
}
```
Also in `notify.rs` tests keep the existing ones passing unchanged.

- [ ] **Step 2: Run to verify they fail** — `cargo test deliver 2>&1 | tail` → compile errors.

- [ ] **Step 3: Implement**

`exec/local.rs`: add
```rust
/// What `local()` does to every program it runs: the package directories on `PATH` and the account's real home.
pub fn prepare_child(command: &mut Command) {
    with_extra_path(command, PACKAGE_DIRS);
    with_account_home(command);
}
```
and make `local()` call it.

`daemon/notify.rs`: replace the body of `run` with a call to a new
```rust
pub fn run_command(command: &str, env: &[(&str, &str)], stdin: &str, timeout: Duration) -> Result<(), NotifyError> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).stdin(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    crate::exec::local::prepare_child(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| NotifyError::Spawn(e.to_string()))?;
    // Feed stdin on its own thread: a command that never reads it must not block us (nor a full pipe).
    let feeder = child.stdin.take().map(|mut pipe| {
        let text = stdin.to_string();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
        })
    });
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => break Err(NotifyError::Exit(status.code().unwrap_or(-1))),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(NotifyError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(NotifyError::Spawn(e.to_string())),
        }
    };
    if let Some(f) = feeder {
        let _ = f.join();
    }
    result
}
```
and `run(command, notice, timeout)` becomes `run_command(command, &[("ZFSCTL_JOB", &notice.job), ("ZFSCTL_HOST", &notice.host), ("ZFSCTL_KIND", &notice.kind), ("ZFSCTL_RESULT", &notice.result), ("ZFSCTL_MESSAGE", &notice.message)], "", timeout)`. (`NotifyError` needs `Clone` for the tests' `assert_eq!` on `Result`: it already derives `PartialEq, Eq, Debug`; add `Clone`.)

`daemon/deliver.rs`:
```rust
//! Getting a message to the operator: email through their own command, and the notify hook.

use super::notify::{run_command, NotifyError};
use crate::config::NotifySettings;
use chrono::{DateTime, Utc};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: String,
    pub host: String,
    pub level: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Channel {
    Mail,
    Hook,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Delivery {
    pub channel: Channel,
    pub result: Result<(), NotifyError>,
}

/// One printable ASCII line: control characters become spaces, anything else outside ASCII becomes `?`.
pub fn ascii_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else if c.is_ascii() { c } else { '?' }).collect()
}

pub fn default_from() -> String {
    let host = nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok()).filter(|h| !h.is_empty()).unwrap_or_else(|| "localhost".to_string());
    format!("zfsctl@{}", ascii_line(&host))
}

pub fn compose_mail(from: &str, to: &[String], subject: &str, body: &str, now: DateTime<Utc>) -> String {
    let to: Vec<String> = to.iter().map(|a| ascii_line(a)).collect();
    format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}",
        ascii_line(from),
        to.join(", "),
        ascii_line(subject),
        now.to_rfc2822(),
    )
}

pub fn send(settings: &NotifySettings, hook: Option<&str>, msg: &Message, now: DateTime<Utc>, timeout: Duration) -> Vec<Delivery> {
    let mut out = Vec::new();
    if settings.has_mail() {
        let from = settings.mail_from.clone().unwrap_or_else(default_from);
        let text = compose_mail(&from, &settings.mail_to, &msg.subject, &msg.body, now);
        out.push(Delivery { channel: Channel::Mail, result: run_command(&settings.mail_command, &[], &text, timeout) });
    }
    if let Some(command) = hook {
        let env = [("ZFSCTL_KIND", msg.kind.as_str()), ("ZFSCTL_HOST", msg.host.as_str()), ("ZFSCTL_RESULT", msg.level.as_str()), ("ZFSCTL_MESSAGE", msg.subject.as_str()), ("ZFSCTL_BODY", msg.body.as_str())];
        out.push(Delivery { channel: Channel::Hook, result: run_command(command, &env, &msg.body, timeout) });
    }
    out
}

pub fn delivered(results: &[Delivery]) -> bool {
    results.iter().any(|d| d.result.is_ok())
}
```
(Environment variables are limited in size on some systems (ARG_MAX counts env): `ZFSCTL_BODY` is capped: in `send`, pass `ZFSCTL_BODY` as the body truncated to 32 KiB at a char boundary; stdin carries the full text.) `daemon/mod.rs`: `pub mod deliver;`. Add the `tests` module above.

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"` → all pass (existing notify tests too). Mutation checks: drop `ascii_line` from the subject (header test fails); skip joining the feeder thread or write stdin on the main thread (the 2 MB test hangs or fails); let the hook run only if mail succeeded (the independence test fails).

- [ ] **Step 5: Commit** — `feat: compose mail and deliver it through the mail command and the notify hook`.

---

### Task 3: The alert tracker

**Files:**
- Create: `src/daemon/health.rs`
- Modify: `src/daemon/mod.rs` (`pub mod health;`)

**Interfaces:**
- Consumes: `crate::zfs::host_health::Level`.
- Produces: `pub enum Severity { Ok, Warning, Critical }` (`PartialOrd, Ord`, `Debug, Clone, Copy, PartialEq, Eq`), `pub fn severity(level: Level) -> Option<Severity>` (`Probing` is `None`); `pub enum AlertKind { Alert, Escalation, Reminder, Recovery }`; `pub struct Alert { pub host: String, pub kind: AlertKind, pub severity: Severity, pub reason: String, pub bad_for: Option<Duration> }`; `pub struct Tracker`; `Tracker::new(confirm: u32, remind: Option<Duration>) -> Tracker`; `Tracker::observe(&mut self, host: &str, level: Level, reason: &str, now: DateTime<Utc>) -> Option<Alert>`; `Tracker::bad_since(&self, host: &str) -> Option<DateTime<Utc>>`; `Tracker::forget_except(&mut self, hosts: &[String])`.

- [ ] **Step 1: Write the failing tests** (`health.rs` tests; a helper `t(min)` = base time plus `min` minutes, `Level::Critical`/`Warning`/`Ok`/`Disconnected`/`Probing`)

```rust
fn t(min: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z").unwrap().with_timezone(&Utc) + chrono::Duration::minutes(min)
}

fn tracker() -> Tracker {
    Tracker::new(2, Some(Duration::from_secs(6 * 3600)))
}

#[test]
fn a_single_bad_probe_never_alerts_but_two_in_a_row_do() {
    let mut tr = tracker();
    assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(0)).is_none());
    assert!(tr.observe("a", Level::Ok, "ok", t(5)).is_none(), "the blip is over");
    assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(10)).is_none(), "the streak restarted");
    let alert = tr.observe("a", Level::Critical, "tank DEGRADED", t(15)).unwrap();
    assert_eq!((alert.kind, alert.severity, alert.reason.as_str()), (AlertKind::Alert, Severity::Critical, "tank DEGRADED"));
}

#[test]
fn disconnected_counts_as_critical_and_probing_counts_for_nothing() {
    let mut tr = tracker();
    assert!(tr.observe("a", Level::Probing, "probing", t(0)).is_none());
    assert!(tr.observe("a", Level::Disconnected, "unreachable", t(1)).is_none());
    assert!(tr.observe("a", Level::Probing, "probing", t(2)).is_none(), "does not break the streak");
    assert_eq!(tr.observe("a", Level::Disconnected, "unreachable", t(3)).unwrap().severity, Severity::Critical);
}

#[test]
fn no_repeat_until_the_reminder_is_due_then_one_reminder_that_says_how_long() {
    let mut tr = tracker();
    tr.observe("a", Level::Warning, "tank 85% full", t(0));
    assert!(tr.observe("a", Level::Warning, "tank 85% full", t(5)).is_some());
    for m in [10, 60, 300] {
        assert!(tr.observe("a", Level::Warning, "tank 85% full", t(m)).is_none(), "minute {m}");
    }
    let r = tr.observe("a", Level::Warning, "tank 86% full", t(5 + 361)).unwrap();
    assert_eq!((r.kind, r.reason.as_str()), (AlertKind::Reminder, "tank 86% full"));
    assert_eq!(r.bad_for, Some(Duration::from_secs(361 * 60)));
    assert!(tr.observe("a", Level::Warning, "tank 86% full", t(5 + 362)).is_none(), "the clock restarts after a reminder");
}

#[test]
fn a_reminder_interval_of_none_never_reminds() {
    let mut tr = Tracker::new(2, None);
    tr.observe("a", Level::Warning, "x", t(0));
    tr.observe("a", Level::Warning, "x", t(5));
    assert!(tr.observe("a", Level::Warning, "x", t(60 * 48)).is_none());
}

#[test]
fn escalation_needs_its_own_two_probes_and_a_changed_reason_alone_is_silent() {
    let mut tr = tracker();
    tr.observe("a", Level::Warning, "tank 85% full", t(0));
    tr.observe("a", Level::Warning, "tank 85% full", t(5));
    assert!(tr.observe("a", Level::Warning, "tank 3 errors", t(10)).is_none(), "same level, new reason: silent");
    assert!(tr.observe("a", Level::Critical, "tank DEGRADED", t(15)).is_none());
    let e = tr.observe("a", Level::Critical, "tank DEGRADED", t(20)).unwrap();
    assert_eq!((e.kind, e.severity), (AlertKind::Escalation, Severity::Critical));
}

#[test]
fn recovery_needs_two_good_probes_and_an_earlier_alert() {
    let mut tr = tracker();
    assert!(tr.observe("never", Level::Ok, "ok", t(0)).is_none() && tr.observe("never", Level::Ok, "ok", t(5)).is_none(), "no alert, so no recovery");
    tr.observe("a", Level::Critical, "x", t(0));
    tr.observe("a", Level::Critical, "x", t(5));
    assert!(tr.observe("a", Level::Ok, "ok", t(10)).is_none());
    let r = tr.observe("a", Level::Ok, "ok", t(15)).unwrap();
    assert_eq!((r.kind, r.bad_for), (AlertKind::Recovery, Some(Duration::from_secs(15 * 60))));
    assert!(tr.observe("a", Level::Ok, "ok", t(20)).is_none(), "once");
    assert!(tr.bad_since("a").is_none());
}

#[test]
fn a_good_blip_while_alerted_does_not_recover_and_a_return_to_bad_does_not_realert() {
    let mut tr = tracker();
    tr.observe("a", Level::Critical, "x", t(0));
    tr.observe("a", Level::Critical, "x", t(5));
    assert!(tr.observe("a", Level::Ok, "ok", t(10)).is_none());
    assert!(tr.observe("a", Level::Critical, "x", t(15)).is_none());
    assert!(tr.observe("a", Level::Critical, "x", t(20)).is_none(), "still the same alert");
    assert_eq!(tr.bad_since("a"), Some(t(5)));
}

#[test]
fn hosts_are_tracked_independently_and_removed_hosts_are_forgotten() {
    let mut tr = tracker();
    tr.observe("a", Level::Critical, "x", t(0));
    tr.observe("b", Level::Critical, "y", t(0));
    assert!(tr.observe("a", Level::Critical, "x", t(5)).is_some());
    tr.forget_except(&["b".to_string()]);
    assert!(tr.bad_since("a").is_none());
    assert!(tr.observe("b", Level::Critical, "y", t(5)).is_some());
}

#[test]
fn a_de_escalation_is_silent_and_remembered() {
    let mut tr = tracker();
    tr.observe("a", Level::Critical, "x", t(0));
    tr.observe("a", Level::Critical, "x", t(5));
    tr.observe("a", Level::Warning, "w", t(10));
    assert!(tr.observe("a", Level::Warning, "w", t(15)).is_none());
    tr.observe("a", Level::Critical, "x", t(20));
    let e = tr.observe("a", Level::Critical, "x", t(25)).unwrap();
    assert_eq!(e.kind, AlertKind::Escalation, "going back up is an escalation again");
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test daemon::health 2>&1 | tail` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! When to tell the operator about a host: the alert state machine, with no clock and no I/O of its own.

use crate::zfs::host_health::Level;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Warning,
    Critical,
}

/// `None` for a host whose first probe has not finished: it says nothing either way.
pub fn severity(level: Level) -> Option<Severity> {
    match level {
        Level::Probing => None,
        Level::Ok => Some(Severity::Ok),
        Level::Warning => Some(Severity::Warning),
        Level::Critical | Level::Disconnected => Some(Severity::Critical),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Alert,
    Escalation,
    Reminder,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub host: String,
    pub kind: AlertKind,
    pub severity: Severity,
    pub reason: String,
    /// How long the host has been bad (reminders and recoveries).
    pub bad_for: Option<Duration>,
}

#[derive(Debug, Clone)]
struct Alerted {
    severity: Severity,
    since: DateTime<Utc>,
    last_sent: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
struct HostState {
    streak_of: Option<Severity>,
    streak: u32,
    alerted: Option<Alerted>,
}

pub struct Tracker {
    confirm: u32,
    remind: Option<Duration>,
    hosts: HashMap<String, HostState>,
}

fn elapsed(from: DateTime<Utc>, to: DateTime<Utc>) -> Duration {
    (to - from).to_std().unwrap_or_default()
}

impl Tracker {
    pub fn new(confirm: u32, remind: Option<Duration>) -> Tracker {
        Tracker { confirm: confirm.max(1), remind, hosts: HashMap::new() }
    }

    pub fn bad_since(&self, host: &str) -> Option<DateTime<Utc>> {
        self.hosts.get(host).and_then(|s| s.alerted.as_ref()).map(|a| a.since)
    }

    pub fn forget_except(&mut self, hosts: &[String]) {
        self.hosts.retain(|name, _| hosts.contains(name));
    }

    pub fn observe(&mut self, host: &str, level: Level, reason: &str, now: DateTime<Utc>) -> Option<Alert> {
        let sev = severity(level)?;
        let (confirm, remind) = (self.confirm, self.remind);
        let state = self.hosts.entry(host.to_string()).or_default();
        if state.streak_of == Some(sev) {
            state.streak += 1;
        } else {
            state.streak_of = Some(sev);
            state.streak = 1;
        }
        let confirmed = state.streak >= confirm;
        let alert = |kind, severity, bad_for| Alert { host: host.to_string(), kind, severity, reason: reason.to_string(), bad_for };
        if sev == Severity::Ok {
            if confirmed {
                if let Some(a) = state.alerted.take() {
                    return Some(alert(AlertKind::Recovery, Severity::Ok, Some(elapsed(a.since, now))));
                }
            }
            return None;
        }
        match state.alerted.as_mut() {
            None if confirmed => {
                state.alerted = Some(Alerted { severity: sev, since: now, last_sent: now });
                Some(alert(AlertKind::Alert, sev, None))
            }
            None => None,
            Some(a) if sev > a.severity && confirmed => {
                a.severity = sev;
                a.last_sent = now;
                Some(alert(AlertKind::Escalation, sev, Some(elapsed(a.since, now))))
            }
            Some(a) => {
                if sev < a.severity && confirmed {
                    a.severity = sev;
                }
                match remind {
                    Some(every) if elapsed(a.last_sent, now) >= every => {
                        a.last_sent = now;
                        Some(alert(AlertKind::Reminder, a.severity, Some(elapsed(a.since, now))))
                    }
                    _ => None,
                }
            }
        }
    }
}
```
`daemon/mod.rs`: `pub mod health;`.

- [ ] **Step 4: Run** — `cargo test daemon::health` → all pass. Mutations: `confirm` check removed (the blip test fails); recovery without `alerted` (the "never" test fails); drop the Probing early return (the probing test fails).

- [ ] **Step 5: Commit** — `feat: the alert tracker (two-probe confirmation, escalation, reminders, recovery)`.

---

### Task 4: Alert and report text, and the event log

**Files:**
- Create: `src/daemon/health_text.rs`
- Modify: `src/daemon/mod.rs` (`pub mod health_text;`)

**Interfaces:**
- Consumes: Task 3 `Alert`, `AlertKind`, `Severity`; Task 2 `Message`; `host_health::{Level, PoolProbe, ScanSummary, HostKind, used_cell, pools_cell, scan_cell, jobs_cell}`; `history::Run`; `jobs::Job`.
- Produces:
  - `pub struct HostRow { pub name: String, pub address: String, pub level: Level, pub reason: String, pub kind: Option<HostKind>, pub tag: String, pub pools: Vec<PoolProbe>, pub rtt_ms: Option<u64>, pub jobs_failed: usize, pub jobs_total: usize }`
  - `pub fn alert_message(row: &HostRow, alert: &Alert, now: DateTime<Utc>, hub: &str) -> Message`
  - `pub struct HealthEvent { pub at: DateTime<Utc>, pub host: String, pub kind: String, pub text: String }` (`Serialize, Deserialize`), `pub struct EventLog(PathBuf)` with `EventLog::new(path)`, `append(&self, &HealthEvent) -> io::Result<()>` (trims to the last 2000 lines when it grows past 2200), `read(&self) -> Vec<HealthEvent>`
  - `pub fn report_message(now: DateTime<Utc>, local: &str, hub: &str, rows: &[HostRow], bad_since: &dyn Fn(&str) -> Option<DateTime<Utc>>, since: DateTime<Utc>, runs: &[Run], jobs: &[Job], events: &[HealthEvent]) -> Message` (`local` is the hub-local timestamp string for the header).

- [ ] **Step 1: Write the failing tests** (`health_text.rs` tests, with helpers `row(name, level, reason)`, `pool(name, cap, scan)`, `run(job, result, msg)`)

```rust
#[test]
fn an_alert_names_the_host_the_reason_and_the_pools() {
    let mut r = row("fbsd16", Level::Critical, "tank DEGRADED");
    r.pools = vec![pool("tank", 91, Some(ScanSummary::Never))];
    let alert = Alert { host: "fbsd16".into(), kind: AlertKind::Alert, severity: Severity::Critical, reason: "tank DEGRADED".into(), bad_for: None };
    let m = alert_message(&r, &alert, t(0), "hub");
    assert_eq!(m.subject, "[zfsctl] fbsd16: tank DEGRADED");
    assert_eq!((m.kind.as_str(), m.host.as_str(), m.level.as_str()), ("health-alert", "fbsd16", "critical"));
    for want in ["Host:     fbsd16 (172.16.10.60)", "State:    critical: tank DEGRADED", "tank", "91%"] {
        assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
    }
}

#[test]
fn escalations_reminders_and_recoveries_have_their_own_subjects_and_kinds() {
    let r = row("fbsd16", Level::Ok, "ok");
    let mk = |kind, sev, bad_for| Alert { host: "fbsd16".into(), kind, severity: sev, reason: "tank 90% full".into(), bad_for };
    let e = alert_message(&r, &mk(AlertKind::Escalation, Severity::Critical, Some(Duration::from_secs(3600))), t(0), "hub");
    assert_eq!((e.subject.as_str(), e.kind.as_str()), ("[zfsctl] fbsd16: now critical: tank 90% full", "health-alert"));
    let m = alert_message(&r, &mk(AlertKind::Reminder, Severity::Warning, Some(Duration::from_secs(6 * 3600 + 120))), t(0), "hub");
    assert_eq!((m.subject.as_str(), m.kind.as_str()), ("[zfsctl] fbsd16: still: tank 90% full (for 6h 2m)", "health-reminder"));
    let ok = alert_message(&r, &mk(AlertKind::Recovery, Severity::Ok, Some(Duration::from_secs(90 * 60))), t(0), "hub");
    assert_eq!((ok.subject.as_str(), ok.kind.as_str(), ok.level.as_str()), ("[zfsctl] fbsd16: recovered after 1h 30m", "health-recovered", "ok"));
}

#[test]
fn a_host_name_with_a_newline_cannot_break_the_subject() {
    let r = row("bad\nname", Level::Critical, "x");
    let a = Alert { host: "bad\nname".into(), kind: AlertKind::Alert, severity: Severity::Critical, reason: "x\r\ny".into(), bad_for: None };
    let m = alert_message(&r, &a, t(0), "hub");
    assert!(!m.subject.contains('\n') && !m.subject.contains('\r'), "{:?}", m.subject);
}

#[test]
fn the_report_has_a_summary_a_table_and_what_needs_attention() {
    let rows = vec![row("hub", Level::Ok, "ok"), row("fbsd16", Level::Critical, "tank DEGRADED"), row("t1", Level::Warning, "tank 85% full"), row("t2", Level::Disconnected, "unreachable")];
    let m = report_message(t(60 * 7), "2026-10-11 07:00", "hub", &rows, &|h| (h == "fbsd16").then(|| t(60 * 2)), t(0), &[], &[], &[]);
    assert_eq!(m.subject, "[zfsctl] daily report: 1 ok, 1 warning, 2 critical");
    assert_eq!((m.kind.as_str(), m.host.as_str()), ("health-report", "hub"));
    for want in ["4 hosts: 1 ok, 1 warning, 2 critical", "HOST", "ADDRESS", "STATE", "USED", "SCRUB", "JOBS", "fbsd16", "NEEDS ATTENTION", "fbsd16: tank DEGRADED (since 2026-10-11 02:00 UTC)", "t2: unreachable"] {
        assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
    }
    let attention = m.body.split("NEEDS ATTENTION").nth(1).unwrap();
    assert!(!attention.contains("hub:"), "a healthy host is not listed: {attention}");
}

#[test]
fn an_all_green_report_says_so_in_the_subject_and_still_has_a_body() {
    let rows = vec![row("hub", Level::Ok, "ok"), row("t1", Level::Ok, "ok")];
    let m = report_message(t(0), "2026-10-11 07:00", "hub", &rows, &|_| None, t(-60 * 24), &[], &[], &[]);
    assert_eq!(m.subject, "[zfsctl] daily report: all 2 hosts ok");
    assert!(m.body.contains("Nothing needs attention") && m.body.contains("EVENTS since"), "{}", m.body);
}

#[test]
fn events_count_job_runs_since_the_window_and_list_failures_and_alerts() {
    let jobs = vec![job("backup-a", "backup", "fbsd16"), job("scrub-t", "scrub", "hub")];
    let runs = vec![
        run("backup-a", Some(RunResult::Ok), "", t(10)),
        run("backup-a", Some(RunResult::Failed), "restic exited 3", t(20)),
        run("scrub-t", Some(RunResult::Ok), "scrub repaired 0B with 0 errors", t(30)),
        run("backup-a", Some(RunResult::Failed), "too old", t(-5000)),
        run("scrub-t", None, "", t(40)),
    ];
    let events = vec![HealthEvent { at: t(15), host: "fbsd16".into(), kind: "alert".into(), text: "tank DEGRADED".into() }, HealthEvent { at: t(-9000), host: "t1".into(), kind: "alert".into(), text: "old".into() }];
    let m = report_message(t(60), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &runs, &jobs, &events);
    for want in ["backup: 2 runs, 1 failed", "scrub: 1 run, 0 failed", "failed: backup-a on fbsd16", "restic exited 3", "fbsd16 alert: tank DEGRADED"] {
        assert!(m.body.contains(want), "{want:?} in\n{}", m.body);
    }
    assert!(!m.body.contains("too old") && !m.body.contains("old\n"), "outside the window");
}

#[test]
fn long_event_lists_are_cut_with_a_count() {
    let jobs = vec![job("j", "snapshot", "hub")];
    let runs: Vec<Run> = (0..60).map(|i| run("j", Some(RunResult::Failed), &format!("boom {i}"), t(i))).collect();
    let m = report_message(t(120), "2026-10-11 07:00", "hub", &[row("hub", Level::Ok, "ok")], &|_| None, t(0), &runs, &jobs, &[]);
    assert!(m.body.contains("(+20 more)"), "{}", m.body);
}

#[test]
fn the_event_log_round_trips_and_keeps_only_the_newest_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::new(dir.path().join("health-events.jsonl"));
    for i in 0..2300 {
        log.append(&HealthEvent { at: t(i), host: "a".into(), kind: "alert".into(), text: format!("n{i}") }).unwrap();
    }
    let all = log.read();
    assert!(all.len() <= 2200 && all.len() >= 2000, "{}", all.len());
    assert_eq!(all.last().unwrap().text, "n2299");
    assert_eq!(EventLog::new(dir.path().join("none")).read(), Vec::<HealthEvent>::new());
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement** `health_text.rs`:

- `fn lvl(level: Level) -> &'static str` = `"ok" | "warning" | "critical"` (Disconnected → `critical`, Probing → `unknown`).
- `fn dur(d: Duration) -> String`: `"6h 2m"`, `"1h 30m"`, `"45m"`, `"3d 2h"` (days if ≥ 24 h; drop a zero minute part: `"2h"`).
- `alert_message`: subjects as in the tests (use `ascii_line` on host and reason: the subject is built from `ascii_line(&alert.host)` and `ascii_line(&alert.reason)`): Alert `[zfsctl] {host}: {reason}`; Escalation `[zfsctl] {host}: now {critical|warning}: {reason}`; Reminder `[zfsctl] {host}: still: {reason} (for {dur})`; Recovery `[zfsctl] {host}: recovered after {dur}` (`bad_for` missing → `recovered`). Kinds `health-alert` (Alert, Escalation), `health-reminder`, `health-recovered`; `level` = `lvl` of the alert's severity. Body lines: `Host:     {name} ({address})`, `Hub:      {hub}`, `Time:     {now as "%Y-%m-%d %H:%M UTC"}`, `State:    {lvl}: {reason}`, then (when `bad_for`) `Bad for:  {dur}`, a blank line, the row line (below) and one line per pool: `  {name:<10} {health?}` — use `{name:<12} {cap}%  frag {frag|-}  errors {n|-}  scrub {scan_cell-like text}` using `scan_cell(std::slice::from_ref(pool))`.
- Table line `fn table_row(r: &HostRow) -> String` with fixed columns: `format!("{:<12} {:<16} {:<9} {:<6} {:<8} {:>5} {:>5} {:<13} {:>5}", name, address, lvl, kind_word, tag, used, pools, scan, jobs)` truncating each text to its width with `…` (use a local `cut(s, n)`), `used`/`pools`/`scan`/`jobs` from `used_cell`, `pools_cell`, `scan_cell`, `jobs_cell` (no probe → `-`), type `kind.word()` or `-`. A header line with `HOST ADDRESS STATE TYPE TAG USED POOLS SCRUB JOBS` in the same widths.
- `report_message`: counts by `severity` (Critical includes Disconnected; Probing counts as ok-unknown: count under none); summary line `{n} hosts: {ok} ok[, {w} warning][, {c} critical]` and subject `[zfsctl] daily report: {ok} ok[, {w} warning][, {c} critical]` or `all {n} hosts ok` when w + c == 0 (a zero count is omitted in both). Sections: summary + `Hub:` / `Time:` lines, blank, table, blank, `NEEDS ATTENTION` then either lines `  {name}: {reason}` plus ` (since {bad_since as "%Y-%m-%d %H:%M UTC"})` when `bad_since(name)` is `Some`, or `  Nothing needs attention.`; blank; `EVENTS since {since "%Y-%m-%d %H:%M UTC"}`; `Jobs:` one line per job kind in order of first appearance: `  {kind}: {n} run(s), {f} failed` counting only runs with `result.is_some()` (finished) and `started_at >= since` (singular `1 run`); `Failed runs:` lines `  failed: {job} on {host} at {HH:MM}: {message}` for `Failed` results (host from the job in `jobs`, `?` if unknown); `Health:` lines `  {host} {kind}: {text}` for events with `at >= since`; each of the lists cut at 40 with `  (+N more)`. Sections with nothing get `  none`. The body ends with a newline. Job runs finished with result `Skipped`/`Cancelled` count as runs but not failures.
- `EventLog`: `append` writes one JSON line (`serde_json`), then if the file has more than 2200 lines rewrites it with the last 2000 (write to a temp file and rename). `read` skips unparsable lines.

- [ ] **Step 4: Run** — `cargo test daemon::health_text` and the whole suite → pass. Mutations: omit `ascii_line` on the subject (the newline test fails); count runs older than `since` (the events test fails); skip the cut (the cut test fails).

- [ ] **Step 5: Commit** — `feat: alert and daily report text, and the health event log`.

---

### Task 5: The monitor

**Files:**
- Create: `src/daemon/monitor.rs`
- Modify: `src/daemon/mod.rs` (`pub mod monitor;`)

**Interfaces:**
- Consumes: Tasks 1–4; `host_health::{probe_host_guarded, assess, HostProbe, HostKind}`; `jobs::Job`; `history::Run`.
- Produces:
  - `pub struct HostInfo { pub name: String, pub address: String, pub tag: String, pub runner: Arc<dyn CommandRunner> }`
  - `pub struct Probed { pub name: String, pub probe: Option<HostProbe> }` (`None` = no answer within the deadline)
  - `pub fn probe_all(hosts: &[HostInfo], detect: &dyn Fn(&str) -> bool, deadline: Duration) -> Vec<Probed>` (one thread per host, results collected until the deadline)
  - `pub trait Sink { fn send(&mut self, msg: &Message) -> bool }` (true = delivered on at least one channel)
  - `pub struct Monitor`; `Monitor::new(settings: NotifySettings, hub: String, state_dir: PathBuf, now: DateTime<Utc>) -> Monitor`; `Monitor::observe(&mut self, now: DateTime<Utc>, local: chrono::NaiveDateTime, hosts: &[HostInfo], probed: Vec<Probed>, jobs: &[Job], runs: &[Run], sink: &mut dyn Sink)`; `Monitor::needs_kind(&self, host: &str) -> bool`; `Monitor::report_due(local: chrono::NaiveDateTime, at: Option<chrono::NaiveTime>, last: Option<chrono::NaiveDate>) -> bool` (associated fn); `Monitor::build_rows(...)`.
  - Constants `CONFIRM: u32 = 2`, `MAX_ATTEMPTS: u32 = 6`, `PROBE_DEADLINE: Duration = 60 s`.
  - State file `<state_dir>/notify.json` = `{"last_report":"2026-10-11"}`.

- [ ] **Step 1: Write the failing tests** (`monitor.rs` tests). Use a `FakeSink { sent: Vec<Message>, ok: bool }`, a helper `host(name, runner)` and `probed_ok(name)` = `Probed { name, probe: Some(HostProbe::Reached { rtt_ms: 3, pools: vec![], kind: None }) }`, `probed_bad(name)` = `Probed { name, probe: Some(HostProbe::Disconnected) }`.

```rust
#[test]
fn two_bad_probes_alert_once_and_two_good_ones_recover() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(sink.sent[0].subject, "[zfsctl] a: unreachable");
    m.observe(t(10), local(10), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), 1, "no repeat");
    for min in 3..5 {
        m.observe(t(min * 5), local(min * 5), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len(), 2);
    assert!(sink.sent[1].subject.starts_with("[zfsctl] a: recovered after"), "{}", sink.sent[1].subject);
}

#[test]
fn a_probe_that_never_answered_is_a_warning_not_a_silent_ok() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![Probed { name: "a".into(), probe: None }], &[], &[], &mut sink);
    }
    assert!(sink.sent[0].subject.contains("no answer"), "{}", sink.sent[0].subject);
}

#[test]
fn a_failed_job_on_the_host_makes_it_a_warning() {
    let mut m = monitor();
    let hosts = [host("a")];
    let jobs = vec![job("backup-a", "backup", "a")];
    let runs = vec![run("backup-a", Some(RunResult::Failed), "restic exited 3", t(-10))];
    let mut sink = FakeSink::new();
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![probed_ok("a")], &jobs, &runs, &mut sink);
    }
    assert_eq!(sink.sent.len(), 1);
    assert_eq!(sink.sent[0].level, "warning");
}

#[test]
fn a_delivery_that_fails_is_retried_on_later_ticks_at_most_six_times_then_dropped() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    }
    let first = sink.sent.len();
    for min in 2..20 {
        m.observe(t(min * 5), local(min * 5), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    }
    assert_eq!(sink.sent.len() - first, 5, "six attempts in all: one at first, five retries, then dropped");
    assert!(sink.sent.iter().all(|s| s.subject == "[zfsctl] a: unreachable"));
}

#[test]
fn a_retry_that_succeeds_is_not_sent_again() {
    let mut m = monitor();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    }
    sink.ok = true;
    m.observe(t(10), local(10), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    let n = sink.sent.len();
    m.observe(t(15), local(15), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    assert_eq!(sink.sent.len(), n);
}

#[test]
fn the_report_is_due_once_per_local_day_from_report_time_on() {
    use chrono::{NaiveDate, NaiveTime};
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

#[test]
fn the_report_is_sent_once_and_remembered_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    m.observe(t(0), at_local(7, 1), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    let reports = |s: &FakeSink| s.sent.iter().filter(|m| m.kind == "health-report").count();
    assert_eq!(reports(&sink), 1);
    m.observe(t(5), at_local(7, 6), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(reports(&sink), 1, "not twice");
    let mut again = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(60));
    again.observe(t(60), at_local(8, 0), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    assert_eq!(reports(&sink), 1, "a restarted monitor reads notify.json");
}

#[test]
fn a_failed_report_is_retried_and_only_marked_sent_once_delivered_or_given_up() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::failing();
    let mut m = Monitor::new(settings_with_report("07:00"), "hub".into(), dir.path().to_path_buf(), t(0));
    m.observe(t(0), at_local(7, 1), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    assert!(!dir.path().join("notify.json").exists());
    sink.ok = true;
    m.observe(t(5), at_local(7, 6), &hosts, vec![probed_ok("a")], &[], &[], &mut sink);
    assert!(dir.path().join("notify.json").exists());
    assert_eq!(sink.sent.iter().filter(|m| m.kind == "health-report").count(), 2);
}

#[test]
fn alerts_and_recoveries_are_written_to_the_event_log_for_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let hosts = [host("a")];
    let mut sink = FakeSink::new();
    let mut m = Monitor::new(settings_with_report("off"), "hub".into(), dir.path().to_path_buf(), t(0));
    for min in 0..2 {
        m.observe(t(min * 5), local(min), &hosts, vec![probed_bad("a")], &[], &[], &mut sink);
    }
    let events = EventLog::new(dir.path().join("health-events.jsonl")).read();
    assert!(events.iter().any(|e| e.kind == "daemon-started"), "{events:?}");
    assert!(events.iter().any(|e| e.kind == "alert" && e.host == "a"), "{events:?}");
}

#[test]
fn probe_all_collects_every_host_and_a_hung_one_comes_back_as_no_answer() {
    struct Hung;
    impl CommandRunner for Hung {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<CommandOutput> { std::thread::sleep(Duration::from_secs(5)); unreachable!() }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<CommandOutput> { self.run(c, a) }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> { self.run(c, a) }
    }
    let hosts = vec![HostInfo { name: "slow".into(), address: "x".into(), tag: String::new(), runner: Arc::new(Hung) }, host("fast")];
    let t0 = std::time::Instant::now();
    let probed = probe_all(&hosts, &|_| false, Duration::from_millis(500));
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert_eq!(probed.len(), 2);
    assert!(probed.iter().find(|p| p.name == "slow").unwrap().probe.is_none());
}
```
(Helpers: `monitor()` = `Monitor::new(NotifySettings{mail_to:["me@x.org"], report_time: None, ..default}, "hub", tempdir path, t(0))`; `local(n)` = a NaiveDateTime on 2026-10-11 at 00:n minutes; `host(name)` builds a `HostInfo` with a `FakeRunner` that answers the two `zpool` calls with an ONLINE 20% pool (so `probe_all` works) — reuse the fixtures from `host_health` tests or write the two route strings inline.)

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement** `monitor.rs`:

`probe_all`: for each host spawn a thread `probe_host_guarded(runner.as_ref(), detect(&name))` sending `(name, probe)` on an `mpsc` channel; receive with `recv_timeout` against a single deadline; hosts missing at the deadline come back as `Probed { probe: None }`; threads are left to finish on their own (as the TUI's probe does).

`Monitor` fields: `settings`, `hub`, `state_dir`, `tracker: Tracker::new(CONFIRM, settings.remind)`, `pending: Vec<(Message, u32, Option<HealthEvent>)>`, `last_report: Option<NaiveDate>` (read from `<state_dir>/notify.json` in `new`; ignore a missing or bad file), `report_attempts: u32`, `last_report_at: Option<DateTime<Utc>>` (initially `now - 24h`; set to the send time after a report), `kinds: HashMap<String, HostKind>`, `events: EventLog`. `new` appends a `daemon-started` event (`host` = hub name, `text` = `"zfsctl daemon started"`).

`observe(now, local, hosts, probed, jobs, runs, sink)`:
1. Update cached kinds from `Reached { kind: Some(k) }`.
2. Build `rows`: for each host in `hosts` (same order), find its `Probed`; `probe: None` gives `Assessment { level: Warning, reason: "no answer for 60s" }`, otherwise `assess(Some(&probe), job_failed)` with `job_failed` = any enabled job of that host whose newest run (by `started_at`) has `result == Some(Failed)`; `Reached { pools, rtt_ms, .. }` fills `pools`/`rtt_ms`; `jobs_failed`/`jobs_total` from the same job/run data (`jobs_total` = enabled jobs of the host).
3. `tracker.forget_except(&names)`; for each row call `tracker.observe(name, level, reason, now)`; each `Some(alert)` becomes `alert_message(row, &alert, now, &hub)` plus a `HealthEvent { kind: "alert" | "escalation" | "reminder" | "recovery", host, text: reason, at: now }`, appended to the event log at once and queued with attempts 0.
4. Retry/deliver: iterate the queue in order: `sink.send(&msg)`; on `true` drop it; on `false` increment its attempts and drop it when attempts reaches `MAX_ATTEMPTS` (log nothing here: the caller logs).  A message queued this tick is sent in this same pass (first attempt) so an alert is not delayed a tick.
5. Report: if `Monitor::report_due(local, settings.report_time, last_report)` then build `report_message(now, &local.format("%Y-%m-%d %H:%M"), &hub, &rows, &|h| tracker.bad_since(h), self.last_report_at.unwrap_or(now - 24h), runs, jobs, &events.read())` and `sink.send`; on success set `last_report = Some(local.date())`, write `notify.json` (`{"last_report":"YYYY-MM-DD"}`), set `last_report_at = Some(now)`, reset `report_attempts`; on failure `report_attempts += 1` and after `MAX_ATTEMPTS` mark it as sent for the day without writing success (so it stops retrying: set `last_report = Some(local.date())` only in memory).
`report_due(local, at, last)`: `at.is_some_and(|at| local.time() >= at) && last != Some(local.date())`.
`needs_kind(host)`: `!self.kinds.contains_key(host)`.
`Monitor::sink_dispatch` is not needed: `Sink` is implemented by the daemon in Task 6 and by `FakeSink` in tests.

- [ ] **Step 4: Run** — `cargo test daemon::monitor` and the suite → pass. Mutations: remove `forget_except`'s effect; mark the report sent before delivery succeeds (the retry test fails); drop the `last != Some(date)` test (the once-per-day test fails); never drop at 6 attempts (the retry-limit test fails).

- [ ] **Step 5: Commit** — `feat: the health monitor (probing, alert queue with retries, daily report scheduling)`.

---

### Task 6: Daemon wiring and job-failure mail

**Files:**
- Modify: `src/daemon/mod.rs` (`Loaded`, `Shared`, `load_hosts`, `apply`, `notify`, `run`, tests), `src/hosts.rs` is unchanged.

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces: `Loaded { hosts, notify_command, notify: NotifySettings, info: Vec<(String, String, String)> }` (name, address, tag; `address` = `state::address_of`-style host part of `ssh_target`, `local` for the hub), `Shared.settings: RwLock<NotifySettings>` and `Shared.host_info: RwLock<Vec<(String, String, String)>>`, `struct DaemonSink<'a>(&'a Shared)` implementing `Sink`, `fn monitor_loop(shared: Arc<Shared>, state_dir: PathBuf, stop: Arc<AtomicBool>)`.

- [ ] **Step 1: Write the failing tests** (`daemon/mod.rs` tests; extend the `shared()` helper with `settings: RwLock::new(NotifySettings::default())` and `host_info: RwLock::new(vec![])`, and add `shared_with_mail(dir, runner, mail_command)`)

```rust
#[test]
fn a_failed_job_is_also_mailed_with_the_job_and_message() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mail");
    let shared = shared_with_mail(&dir, FakeRunner::failing("cannot open 'tank': no such pool", 1), &format!("cat > {}", out.display()));
    shared.execute(Work::Run(due()));
    let got = std::fs::read_to_string(out).unwrap();
    assert!(got.contains("Subject: [zfsctl] job scrub-tank failed on local") && got.contains("no such pool"), "{got}");
}

#[test]
fn a_failing_mail_command_does_not_disturb_the_recorded_run_or_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let hook_out = dir.path().join("hook");
    let shared = shared_with_mail(&dir, FakeRunner::failing("no such pool", 1), "exit 7");
    *shared.notify_command.write().unwrap() = Some(format!("printf '%s' \"$ZFSCTL_JOB\" > {}", hook_out.display()));
    shared.execute(Work::Run(due()));
    assert_eq!(history::runs(&shared.history.read())[0].result, Some(RunResult::Failed));
    assert_eq!(std::fs::read_to_string(hook_out).unwrap(), "scrub-tank");
}

#[test]
fn load_hosts_reads_the_notify_section_and_the_host_addresses_and_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[[hosts]]\nname = \"fbsd16\"\nssh_target = \"fbsd@172.16.10.60\"\ntag = \"vm\"\n\n[notify]\nmail_to = \"me@x.org\"\n").unwrap();
    let l = load_hosts(&path).unwrap();
    assert_eq!(l.notify.mail_to, ["me@x.org"]);
    assert_eq!(l.info, [("local".to_string(), "local".to_string(), String::new()), ("fbsd16".to_string(), "172.16.10.60".to_string(), "vm".to_string())]);
}

#[test]
fn a_bad_notify_section_is_an_error_load_hosts_reports_so_the_caller_keeps_the_old_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[notify]\nmail_to = \"not an address\"\n").unwrap();
    assert!(load_hosts(&path).unwrap_err().to_string().contains("mail_to"));
}

#[test]
fn the_monitor_runs_only_when_a_channel_is_configured() {
    assert!(!monitor_enabled(&NotifySettings::default(), None));
    assert!(monitor_enabled(&NotifySettings::default(), Some("true")));
    let mail = NotifyConfig { mail_to: Some("me@x.org".into()), ..NotifyConfig::default() }.resolve().unwrap();
    assert!(monitor_enabled(&mail, None));
}
```

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**

- `load_hosts`: after `Config::load`, `let notify = config.notify.resolve().map_err(AppError::Config)?;` (a bad section fails the whole load, which the run loop already treats as "keeping the previous hosts and notify command": update its log text to `config.toml problem, keeping the previous hosts and notify settings`). `info`: `inventory.hosts` mapped to `(name, ui::state::address_of(ssh_target.as_deref()), tag.unwrap_or_default())`.
- `Shared.apply(loaded)` also replaces `settings` and `host_info`.
- `Shared::notify(job, outcome)` (failed runs only, as now): keep the hook call; then, when `settings.has_mail()`, build `Message { kind: job.kind, host: job.host, level: "failed", subject: format!("[zfsctl] job {} failed on {}", ascii_line(&job.id), ascii_line(&job.host)), body: format!("Job {} ({}) on {} failed.\n\n{}\n", job.id, job.kind, job.host, outcome.message) }` and `deliver::send` with only the mail channel (pass `None` as the hook: the hook was already run with the job env above) and log the outcome (`mailed about {id}` / `mail for {id} {error}`).
- `fn monitor_enabled(settings: &NotifySettings, hook: Option<&str>) -> bool { settings.has_mail() || hook.is_some() }`.
- `struct DaemonSink<'a>(&'a Shared)`: `send` reads `settings` and `notify_command`, calls `deliver::send(.., Utc::now(), deliver::TIMEOUT)`, logs each failed channel (`mail failed: exited with status 7`, never the message text) and returns `deliver::delivered(&results)`. Add `pub const TIMEOUT: Duration = Duration::from_secs(30)` to `deliver.rs`.
- `fn monitor_loop(shared, state_dir, stop)`: `let mut monitor: Option<Monitor> = None; let mut next = Instant::now();` loop until `stop`: sleep in 1-second steps; when `Instant::now() >= next`: read `settings` and `notify_command`; if `!monitor_enabled` set `monitor = None` and `next = now + 30 s`, continue; otherwise create the `Monitor` if absent or the settings changed (`Monitor::new(settings.clone(), hub_name, state_dir.clone(), Utc::now())`, hub name from `deliver::default_from()` after the `@`), build `HostInfo` from `shared.hosts` and `host_info`, `jobs = jobs::load(&state_dir.join("jobs.toml")).unwrap_or_default()`, `runs = history::runs(&shared.history.read())`, `probed = probe_all(&hosts, &|n| monitor.needs_kind(n), PROBE_DEADLINE)`, `monitor.observe(Utc::now(), chrono::Local::now().naive_local(), &hosts, probed, &jobs, &runs, &mut DaemonSink(&shared))`, then `next = now + settings.check_interval`. Log `health monitor started (every N min, report at HH:MM)` once when created.
- In `run`: spawn `monitor_loop` on a thread named `health` (`std::thread::Builder`), before the main loop; join not needed (it exits on `stop`).

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0. Mutation: send the hook only when mail succeeds (the "does not disturb" test fails).

- [ ] **Step 5: Commit** — `feat: the daemon runs the health monitor and mails job failures`.

---

### Task 7: `notify-test`, `report`, docs and the live check

**Files:**
- Modify: `src/main.rs` (commands), `README.md`, `src/daemon/mod.rs` or a small `src/notify_cli.rs` for the two commands' bodies.

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces: `Command::NotifyTest`, `Command::Report`; `pub fn notify_test(settings: &NotifySettings, hook: Option<&str>, now) -> (String, i32)` (text with one line per channel; exit code); `pub fn report_text(config: &Config, state_dir: &Path, now: DateTime<Utc>) -> Result<String>` (probes hosts as the monitor does; returns `Subject: ...\n\nbody`).

- [ ] **Step 1: Write the failing tests**

```rust
// main.rs tests
#[test]
fn notify_test_and_report_take_no_arguments() {
    assert_eq!(parse_args(&["notify-test".into()]).unwrap(), Command::NotifyTest);
    assert_eq!(parse_args(&["report".into()]).unwrap(), Command::Report);
    assert!(parse_args(&["report".into(), "extra".into()]).is_err());
}

// notify_cli.rs tests
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
fn the_report_command_builds_the_report_without_sending_anything() {
    // A config with only the hub; the output starts with a Subject line and lists the hub as a table row.
    let dir = tempfile::tempdir().unwrap();
    let text = report_text_with(&Config::default(), dir.path(), Utc::now(), &fake_probe_ok).unwrap();
    assert!(text.starts_with("Subject: [zfsctl] daily report"), "{text}");
    assert!(text.contains("local"), "{text}");
}
```
(`report_text_with` takes the probing function so the test needs no real `zpool`; `report_text` calls it with `probe_all`.)

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**
  - `notify_test`: sends a message `Message { kind: "health-test", host: hub, level: "ok", subject: "[zfsctl] test message", body: "This is a test from zfsctl notify-test.\n..." }` through `deliver::send`; one line per delivery (`mail: ok` or `mail: failed: <NotifyError display>`, `hook: ...`); no deliveries gives `no channel is configured: set [notify] mail_to or [daemon] notify_command in config.toml` and exit 1; exit 0 only if every configured channel succeeded.
  - `report_text_with`: load `jobs.toml` and `history.jsonl` from the state dir, build `HostInfo` from the config (the inventory plus info like `load_hosts`), probe with the supplied function (deadline 60 s, detect kind for all), compute rows exactly as `Monitor::observe` does (factor the row building into `pub fn build_rows(...)` shared by both), then `report_message(...)` with an empty tracker (`bad_since` always `None`) and the event log from the state dir; return `format!("Subject: {}\n\n{}", subject, body)`.
  - `main.rs`: `Command::NotifyTest` / `Command::Report` parsed from `[cmd] if cmd == "notify-test"` / `"report"`; both run after the root check and config load: print the text and exit with the code (`report` exits 0; a config whose `[notify]` is invalid is an error with its reason). Update `USAGE`.
  - README: a "Health alerts and the daily report" section under the daemon docs: the `[notify]` keys and defaults, what triggers an alert (the Hosts table's states, two probes), reminders and recovery, the report contents and the once-per-day rule, the hook variables (`ZFSCTL_KIND` values, `ZFSCTL_BODY`, stdin) with a Telegram `curl` example (`curl -s --data-urlencode "text=$ZFSCTL_MESSAGE" -d chat_id=... https://api.telegram.org/bot<token>/sendMessage`, token read from a root-only file), msmtp notes (`HOME`/PATH are handled; `-a default`), `zfsctl notify-test` and `zfsctl report`, and what it does not do (no acknowledge, state not kept across restarts, a dead daemon sends nothing).

- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → pass, 0.

- [ ] **Step 5: Live check on the hub** (deploy as before: tar over ssh and `cargo build --release` in `~/zfsctl`; use `target/release/zfsctl`, not the installed daemon, until the user says to install). **Ask the user before sending any real email** and for the address and msmtp account to use.
  1. `sudo ./target/release/zfsctl report` against the hub, fbsd16, t1, t2: compare the table with the Hosts table by eye.
  2. With the user's OK: a throwaway `[notify]` in a copy of the config (or the real one with the user's address, restored afterwards), `sudo ./target/release/zfsctl notify-test`: confirm the mail arrives and the headers look right.
  3. Run a foreground daemon with a temporary state dir (`zfsctl daemon --state-dir /tmp/zit-state`) and a short interval is not possible below one minute, so use `check_interval_minutes = 1`, add a throwaway host entry that cannot be reached, and confirm: no mail after the first probe, the alert after the second (about 2 minutes), then remove the host and see the recovery after two more probes.
  4. Set `report_time` two minutes ahead and confirm the report mail arrives once and not again after a restart of the foreground daemon.
  5. Record the observations in the spec's "Live results" section and commit; remove temporary state, config edits and throwaway hosts.

- [ ] **Step 6: Commit** — `feat: notify-test and report commands, docs`.
