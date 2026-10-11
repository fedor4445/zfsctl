# Backup Preflight Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pressing F5 in the backup job form checks, read-only and in the background, whether the repository opens with the password file on the form's host, and shows one plain-language result inside the form.

**Architecture:** One privileged call, `restic cat config --no-cache`, run through the host's existing runner (already allowed by the `restic` sudoers entry, so no new permission). A pure `classify_preflight` in `src/restic.rs` turns exit status and text into a message; a small `src/preflight.rs` runs the call on a worker thread with a deadline; the TUI keeps the result in `App` (not in the `Modal`) and shows it only while it still describes the fields on screen.

**Tech Stack:** Rust, ratatui, std threads and `mpsc` (the same worker pattern as `fetch_pool_names`), `FakeRunner` for tests, the test hub `fb@172.16.10.90` with VMs t1/t2 for live checks.

**Spec:** `docs/superpowers/specs/2026-10-10-backup-preflight-design.md`

## Global Constraints

- Read-only: the check never runs `restic init`, never reads or prints the password, never writes anywhere (`--no-cache` keeps restic from creating `/root/.cache/restic`).
- No new sudoers entry and no change to `exec::privilege::ENTRIES`: `restic` is already registered (`Entry { name: "restic", ... kind: ANY }`).
- The check never blocks saving, never runs by itself, and never runs when the host, repository or password file fails the same validators the save uses (`restic::valid_repository`, `restic::valid_password_file`, a known host).
- The repository text is masked as `<repository>` in every message (same rule as `daemon::backup::redact`).
- Key: **F5**, shown in the backup form's hint line only.
- Deadline: 45 s; a late or stale result is discarded.
- Every restic option is passed as `--flag=value` (module rule in `src/restic.rs`).
- Code style: match the surrounding code (one-line doc comments, no extra comments, `format!` messages in plain sentences).
- Run `cargo fmt --check` (if the repo uses it: see `.rustfmt.toml`/CI notes), `cargo clippy --all-targets`, and `cargo test` before each commit; the whole suite must stay green, including the `book_check` tests.

## Review Focus

- A repository with a password in its URL (`rest:https://user:pass@host/`): F5 must refuse it and run nothing (it would put the secret on a process command line). Pinned in Task 3.
- F5 pressed twice in a row, or pressed again while a check is running: exactly one `restic` call is queued. Pinned in Task 3.
- A result that arrives after the user edited the repository, password file or host: it must not be shown as the current state. Pinned in Tasks 3 and 4.
- A restic error that quotes the repository or is very long: shown masked (`<repository>`) and capped to one line. Pinned in Task 2.
- A target that never answers (unplugged disk, dead SFTP host, hung ssh): the form says so after the deadline and stays usable meanwhile. Pinned in Task 2.
- A host where restic is missing or not allowed by sudoers: a message that points at `zfsctl print-sudoers`, not a raw `sudo` error. Pinned in Task 2.

---

### Task 1: Record restic's real failure output on the hub

The patterns in Task 2 must rest on observed text. This task only observes; it changes no repo files.

**Files:**
- Create: `/tmp/claude-1000/-home-me-dev-rust-zfsctl/29b90d99-bce0-40cd-ba92-d8bede1f4fa7/scratchpad/preflight-recordings.txt` (scratch notes; not committed)

**Interfaces:**
- Produces: for each case below, the exit status and the exact stderr/stdout of `restic cat config`, which Task 2 pastes into its test fixtures.

Run on `fb` as root (`ssh fb@172.16.10.90`, then `sudo -i`; never ssh as root). Root's shell there is plain `sh`: the commands below avoid bash-only features. `rclone` remote `t1` exists (sftp to `fbsd@172.16.10.61`). Use a throwaway scratch area and clean up at the end.

- [ ] **Step 1: Prepare throwaway fixtures**

```sh
mkdir -p /root/zit-pre
printf 'preflight-test-pw\n' > /root/zit-pre/pw && chmod 600 /root/zit-pre/pw
printf 'a-different-pw\n' > /root/zit-pre/pw-wrong && chmod 600 /root/zit-pre/pw-wrong
restic init -r /root/zit-pre/repo --password-file /root/zit-pre/pw
rclone mkdir t1:zit-pre-repo
restic init -r rclone:t1:zit-pre-repo --password-file /root/zit-pre/pw
```

Expected: both `init` calls print "created restic repository".

- [ ] **Step 2: Record every case** (each as `cmd; echo "exit=$?"`, saving stdout and stderr both):

```sh
R=/root/zit-pre; C='restic cat config --no-cache'
$C -r $R/repo --password-file $R/pw 2>&1; echo "exit=$?"                  # A: working local repo
$C -r rclone:t1:zit-pre-repo --password-file $R/pw 2>&1; echo "exit=$?"   # B: working rclone repo
$C -r $R/repo --password-file $R/no-such-file 2>&1; echo "exit=$?"        # C: password file missing
$C -r $R/repo --password-file $R/pw-wrong 2>&1; echo "exit=$?"            # D: wrong password
$C -r $R/no-repo --password-file $R/pw 2>&1; echo "exit=$?"               # E: local repository missing
$C -r rclone:t1:zit-pre-missing --password-file $R/pw 2>&1; echo "exit=$?" # F: rclone repository missing
$C -r rclone:nosuchremote:x --password-file $R/pw 2>&1; echo "exit=$?"    # G: rclone remote not defined
PATH=/usr/bin:/bin /usr/local/bin/restic cat config --no-cache -r rclone:t1:zit-pre-repo --password-file $R/pw 2>&1; echo "exit=$?"   # H: rclone not on PATH
chmod 000 $R/pw; $C -r $R/repo --password-file $R/pw 2>&1; echo "exit=$?"; chmod 600 $R/pw   # I: password file unreadable (as root this may still read it; record what happens)
```

Then, on `fb` as the normal user, for a VM whose sudoers entry has no restic line (t1 or t2; if both already allow it, record that and skip J):

```sh
ssh fbsd@172.16.10.61 'sudo -n /usr/local/bin/restic version' 2>&1; echo "exit=$?"   # J: restic not allowed / not installed
```

Finally an unreachable target (K), with a time limit so it cannot hang you:

```sh
rclone config create zit-dead sftp host=172.16.10.250 user=x key_file=/root/.ssh/id_rclone known_hosts_file=/root/.ssh/known_hosts host_key_algorithms=ssh-ed25519
time timeout 90 restic cat config --no-cache -r rclone:zit-dead:x --password-file $R/pw 2>&1; echo "exit=$?"   # K: dead host
```

Write each case's exit status and full output into the scratch notes file above.

- [ ] **Step 3: Clean up**

```sh
rclone purge t1:zit-pre-repo
rclone config delete zit-dead
rm -r /root/zit-pre
```

Check `rclone lsd t1:` no longer lists `zit-pre-repo`. Check `ls /root/zit-pre` fails.

- [ ] **Step 4: Compare with the plan's assumptions**

For each of C, D, E, F, G, H, J: note the exit status and the **distinguishing text**. Task 2's `classify_preflight` assumes: D exits 12 and says `wrong password or no key found`; E and F exit 10 (or say `Is there a repository at the following location?`); G contains `didn't find section in config file`; H mentions `rclone` with `executable file not found`; C mentions `no such file or directory` and the password file path; J mentions `sudo` and a missing permission or command. Where the recording differs, Task 2 Step 1 uses the recorded text instead, and the marker in Step 3 follows it.

No commit (nothing in the repo changed).

---

### Task 2: Pure builder, classifier and the runner with a deadline

**Files:**
- Modify: `src/restic.rs` (add `Preflight`, `cat_config_args`, `redact`, `classify_preflight`, tests)
- Modify: `src/daemon/backup.rs:39-45` (make the local `redact` delegate to `restic::redact`)
- Create: `src/preflight.rs`
- Modify: `src/main.rs:3` (add `mod preflight;` next to `mod restic;`)

**Interfaces:**
- Produces:
  - `restic::Preflight` = `enum { Ok, Problem(String) }` (derives `Debug, Clone, PartialEq, Eq`).
  - `restic::cat_config_args(repo: &str, password_file: &str) -> Vec<String>`.
  - `restic::redact(text: &str, repo: &str) -> String`.
  - `restic::classify_preflight(status: i32, stdout: &str, stderr: &str, repo: &str, host: &str, password_file: &str) -> Preflight`.
  - `preflight::DEADLINE: Duration` (45 s) and `preflight::run(runner: Arc<dyn CommandRunner>, host: &str, repo: &str, password_file: &str, deadline: Duration) -> Preflight`.
- Consumes: `restic::failure_text(fatal: Option<&str>, stderr: &str) -> String` (existing, one line, capped), `CommandRunner::run`.

- [ ] **Step 1: Write the failing tests in `src/restic.rs`'s `mod tests`**

Use the exact recorded text from Task 1 in place of the `stderr` fixtures below wherever it differs (these are the expected shapes).

```rust
    #[test]
    fn cat_config_args_are_flag_style_and_never_write_a_cache() {
        assert_eq!(cat_config_args("/repo", "/pw"), vec!["cat", "--repo=/repo", "--password-file=/pw", "--no-cache", "config"]);
    }

    #[test]
    fn redact_masks_the_repository_everywhere_and_leaves_other_text() {
        assert_eq!(redact("open /mnt/usb/r: gone, at /mnt/usb/r", "/mnt/usb/r"), "open <repository>: gone, at <repository>");
        assert_eq!(redact("anything", ""), "anything");
    }

    fn pre(status: i32, stderr: &str) -> Preflight {
        classify_preflight(status, "", stderr, "rclone:t1:restic-tank", "fb", "/root/.restic-password")
    }

    fn problem(p: Preflight) -> String {
        match p {
            Preflight::Problem(m) => m,
            Preflight::Ok => panic!("expected a problem"),
        }
    }

    #[test]
    fn a_zero_status_is_ok() {
        assert_eq!(classify_preflight(0, "{\"id\":\"c8\"}", "", "/r", "fb", "/pw"), Preflight::Ok);
    }

    #[test]
    fn each_known_failure_gets_its_own_message_with_a_next_step() {
        let m = problem(pre(12, "Fatal: wrong password or no key found"));
        assert!(m.contains("does not unlock"), "{m}");
        let m = problem(classify_preflight(10, "", "Fatal: unable to open config file: Stat: stat /mnt/usb/restic/config: no such file or directory\nIs there a repository at the following location?\n/mnt/usb/restic", "/mnt/usb/restic", "fb", "/root/.restic-password"));
        assert!(m.contains("restic init") && !m.contains("/mnt/usb/restic"), "{m}");
        let m = problem(classify_preflight(1, "", "2026/10/10 21:45:04 CRITICAL: Failed to create file system for \"nosuch:x\": didn't find section in config file (\"nosuch\")\nFatal: unable to open repository at rclone:nosuch:x: error talking HTTP to rclone: exit status 1", "rclone:nosuch:x", "fb", "/root/.restic-password"));
        assert!(m.contains("rclone remote 'nosuch'") && m.contains("fb") && m.contains("rclone config"), "{m}");
        let m = problem(pre(1, "Fatal: unable to open repository at <x>: error talking HTTP to rclone: exec: \"rclone\": executable file not found in $PATH"));
        assert!(m.contains("rclone is not installed on fb"), "{m}");
        let m = problem(pre(1, "Fatal: Resolving password failed: open /root/.restic-password: no such file or directory"));
        assert!(m.contains("no readable password file at /root/.restic-password on fb"), "{m}");
        let m = problem(pre(1, "sudo: a password is required"));
        assert!(m.contains("print-sudoers") && m.contains("fb"), "{m}");
    }

    #[test]
    fn an_unrecognised_failure_passes_restics_line_through_masked_and_on_one_line() {
        let m = problem(classify_preflight(1, "", "Fatal: odd thing at rclone:t1:restic-tank/locks\nsecond line", "rclone:t1:restic-tank", "fb", "/pw"));
        assert_eq!(m, "Fatal: odd thing at <repository>/locks second line");
        let long = format!("Fatal: {}", "x".repeat(2000));
        assert!(problem(pre(1, &long)).chars().count() < 600);
    }

    #[test]
    fn an_empty_failure_still_says_something() {
        assert_eq!(problem(pre(1, "")), "restic failed");
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test restic::tests -- --nocapture`
Expected: compile errors (`cat_config_args`, `redact`, `classify_preflight`, `Preflight` not found).

- [ ] **Step 3: Implement in `src/restic.rs`**

Add after `unlock_args` (before `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Summary`):

```rust
/// `restic cat config`: opens the repository (finding the program, the rclone remote, the password file, the
/// password and the repository in one go) without locking it or writing a cache. What the preflight check runs.
pub fn cat_config_args(repo: &str, password_file: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    vec!["cat".to_string(), r, p, "--no-cache".to_string(), "config".to_string()]
}

/// restic quotes the repository in many of its error messages; a message shown to the user never carries it.
pub fn redact(text: &str, repo: &str) -> String {
    if repo.is_empty() {
        text.to_string()
    } else {
        text.replace(repo, "<repository>")
    }
}

/// What the preflight check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    Ok,
    /// One plain sentence saying what is wrong and what to do.
    Problem(String),
}

/// Turns the outcome of `cat_config_args` into a finding. restic 0.17+ exits 10 for "no repository" and 12 for
/// "wrong password"; the texts are matched too, for older restic. Anything not recognised passes restic's own line
/// through, masked: a wrong guess here must never hide a problem.
pub fn classify_preflight(status: i32, stdout: &str, stderr: &str, repo: &str, host: &str, password_file: &str) -> Preflight {
    if status == 0 {
        return Preflight::Ok;
    }
    let lower = format!("{stderr}\n{stdout}").to_lowercase();
    let has = |s: &str| lower.contains(s);
    let remote = repo.strip_prefix("rclone:").and_then(|r| r.split(':').next()).unwrap_or("");
    let problem = if has("sudo:") && (has("password is required") || has("not allowed") || has("command not found")) {
        format!("restic is not installed on {host}, or its sudoers entry does not allow it (run `zfsctl print-sudoers {host}`)")
    } else if has("didn't find section in config file") {
        format!("rclone remote '{remote}' is not in root's rclone config on {host} (create it as root with `rclone config`)")
    } else if has("executable file not found") && has("rclone") {
        format!("rclone is not installed on {host}")
    } else if has("no such file or directory") && has(&password_file.to_lowercase()) {
        format!("no readable password file at {password_file} on {host}")
    } else if status == 12 || has("wrong password") {
        "the password file does not unlock this repository (wrong password)".to_string()
    } else if status == 10 || has("is there a repository at the following location") {
        "no repository at that address yet: create it once with `restic init`".to_string()
    } else {
        redact(&failure_text(None, stderr), repo)
    };
    Preflight::Problem(problem)
}
```

Then in `src/daemon/backup.rs`, replace the body of the private `redact` (lines 39-45) so there is one masking rule:

```rust
fn redact(text: &str, repo: &str) -> String {
    restic::redact(text, repo)
}
```

(keep the doc comment above it.)

- [ ] **Step 4: Run to see them pass**

Run: `cargo test restic::tests daemon::backup`
Expected: PASS. If a recorded fixture from Task 1 differs from the assumed text, fix the marker in `classify_preflight` (not the fixture), then re-run.

- [ ] **Step 5: Write the failing tests for the runner in `src/preflight.rs`**

Create `src/preflight.rs` with the tests first (the function is added in Step 7):

```rust
//! The backup form's preflight check: runs `restic cat config` on a host's runner and says what it found.
//! The command and the reading of its output are in `restic`; this adds the worker and the deadline.

use crate::exec::{CommandOutput, CommandRunner};
use crate::restic::{self, Preflight};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// How long the form waits for an answer before saying there was none.
pub const DEADLINE: Duration = Duration::from_secs(45);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    /// A runner whose commands take `delay` and then fail like a missing remote.
    struct SlowRunner {
        delay: Duration,
    }

    impl CommandRunner for SlowRunner {
        fn run(&self, _cmd: &str, _args: &[&str]) -> crate::error::Result<CommandOutput> {
            std::thread::sleep(self.delay);
            Ok(CommandOutput { stdout: String::new(), stderr: "Fatal: late".to_string(), status: 1 })
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<CommandOutput> {
            self.run(cmd, args)
        }
    }

    #[test]
    fn a_working_repository_runs_one_read_only_restic_call_and_is_ok() {
        let runner = Arc::new(FakeRunner::new("{\"version\":2,\"id\":\"c87554e4\"}"));
        let r = run(runner.clone(), "fb", "rclone:t1:restic-tank", "/root/.restic-password", Duration::from_secs(5));
        assert_eq!(r, Preflight::Ok);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].0, "restic");
        assert_eq!(calls[0].1, restic::cat_config_args("rclone:t1:restic-tank", "/root/.restic-password"));
    }

    #[test]
    fn a_failing_call_is_classified_and_masked() {
        let runner = Arc::new(FakeRunner::failing("Fatal: wrong password or no key found", 12));
        let r = run(runner, "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5));
        assert!(matches!(&r, Preflight::Problem(m) if m.contains("does not unlock")), "{r:?}");
    }

    #[test]
    fn a_runner_error_says_so_without_the_repository() {
        struct Broken;
        impl CommandRunner for Broken {
            fn run(&self, _c: &str, _a: &[&str]) -> crate::error::Result<CommandOutput> {
                Err(crate::error::AppError::Config("ssh to /mnt/usb/restic failed".to_string()))
            }
            fn run_with_stdin(&self, c: &str, a: &[&str], _s: &str) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
            fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<CommandOutput> {
                self.run(c, a)
            }
        }
        let r = run(Arc::new(Broken), "fb", "/mnt/usb/restic", "/root/pw", Duration::from_secs(5));
        let Preflight::Problem(m) = r else { panic!("expected a problem") };
        assert!(m.contains("could not run restic on fb") && !m.contains("/mnt/usb/restic"), "{m}");
    }

    #[test]
    fn a_target_that_never_answers_is_reported_after_the_deadline() {
        let start = std::time::Instant::now();
        let r = run(Arc::new(SlowRunner { delay: Duration::from_secs(3) }), "fb", "/r", "/pw", Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(2), "must not wait for the slow command");
        assert!(matches!(&r, Preflight::Problem(m) if m.contains("no answer from fb")), "{r:?}");
    }
}
```

- [ ] **Step 6: Register the module and see the tests fail**

In `src/main.rs`, add `mod preflight;` directly after `mod restic;` (line 3 area, keep the alphabetical/existing order).

Run: `cargo test preflight::`
Expected: compile error, `run` not found.

- [ ] **Step 7: Implement `preflight::run`**

Insert before `#[cfg(test)]` in `src/preflight.rs`:

```rust
/// Runs the check on `runner` and gives up after `deadline`. The command runs on its own thread so a hung
/// connection cannot hold the caller; a late result is simply dropped.
pub fn run(runner: Arc<dyn CommandRunner>, host: &str, repo: &str, password_file: &str, deadline: Duration) -> Preflight {
    let (tx, rx) = mpsc::channel();
    let (r, p) = (repo.to_string(), password_file.to_string());
    std::thread::spawn(move || {
        let args = restic::cat_config_args(&r, &p);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = tx.send(runner.run("restic", &refs));
    });
    match rx.recv_timeout(deadline) {
        Ok(Ok(out)) => restic::classify_preflight(out.status, &out.stdout, &out.stderr, repo, host, password_file),
        Ok(Err(e)) => Preflight::Problem(format!("could not run restic on {host}: {}", restic::redact(&e.to_string(), repo))),
        Err(_) => Preflight::Problem(format!("no answer from {host} in {} s", deadline.as_secs())),
    }
}
```

- [ ] **Step 8: Run to see them pass, then lint**

Run: `cargo test preflight:: restic:: daemon::backup && cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings. (If `mod preflight` triggers `dead_code` before Task 3 uses it, add `#[allow(dead_code)]` only if clippy fails the build here, and remove it in Task 3.)

- [ ] **Step 9: Commit**

```bash
git add src/restic.rs src/preflight.rs src/main.rs src/daemon/backup.rs
git commit -m "feat: preflight classification and runner for backup repositories

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: F5 in the backup form, the worker and the app state

**Files:**
- Modify: `src/ui/state.rs` (struct `PreflightCheck`, `PendingPreflight`; fields on `App`; `WorkerMsg::Preflight`; F5 arm; `request_backup_preflight`, `run_backup_preflight`, `fail_backup_preflight`; drain arm; tests)
- Modify: `src/ui/mod.rs` (`apply_pending_preflights`, call it in `event_loop`; a test)
- Modify: `src/ui/hints.rs` (F5 hint on the backup form; a test)

**Interfaces:**
- Consumes: `restic::{Preflight, valid_repository, valid_password_file}`, `preflight::{run, DEADLINE}`, `field_value(fields, i)`, `App::set_error`, form field order `[0 host, 3 repository, 4 password file]`.
- Produces (used by Task 4):
  - `pub struct PreflightCheck { pub host: usize, pub repository: String, pub password_file: String, pub result: Option<crate::restic::Preflight> }` (`result: None` = still checking), on `App.backup_preflight: Option<PreflightCheck>`.
  - `pub struct PendingPreflight { pub host: usize, pub host_name: String, pub repository: String, pub password_file: String }` on `App.pending_preflight: Vec<PendingPreflight>`.
  - `WorkerMsg::Preflight(usize, String, String, crate::restic::Preflight)` (host index, repository, password file, result).

- [ ] **Step 1: Write the failing tests** in `src/ui/state.rs`'s `mod tests`, next to the other backup-form tests (they reuse `BACKUP_GOOD`, `backup_fields`, `backup_kind`):

```rust
    fn backup_form_app(values: [&str; 8]) -> App {
        let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
        app.modal = Some(Modal::Form { title: "x".into(), fields: backup_fields(values), focused: 3, description: None, kind: backup_kind(None, vec![]) });
        app
    }

    #[test]
    fn f5_in_the_backup_form_queues_one_check_for_the_forms_host_and_marks_it_checking() {
        let mut app = backup_form_app(BACKUP_GOOD);
        assert_eq!(app.handle_modal_key(KeyCode::F(5)), None);
        assert_eq!(app.pending_preflight, vec![PendingPreflight { host: 0, host_name: "local".into(), repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into() }]);
        assert_eq!(app.backup_preflight, Some(PreflightCheck { host: 0, repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into(), result: None }));
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "the form stays open");
    }

    #[test]
    fn f5_twice_while_checking_queues_only_one_check() {
        let mut app = backup_form_app(BACKUP_GOOD);
        app.handle_modal_key(KeyCode::F(5));
        app.handle_modal_key(KeyCode::F(5));
        assert_eq!(app.pending_preflight.len(), 1);
    }

    #[test]
    fn f5_after_a_result_checks_again() {
        let mut app = backup_form_app(BACKUP_GOOD);
        app.handle_modal_key(KeyCode::F(5));
        app.pending_preflight.clear();
        app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok)).unwrap();
        app.drain_worker_messages();
        app.handle_modal_key(KeyCode::F(5));
        assert_eq!(app.pending_preflight.len(), 1);
        assert_eq!(app.backup_preflight.as_ref().unwrap().result, None, "a new check clears the old answer");
    }

    #[test]
    fn f5_runs_nothing_for_bad_fields_and_says_why() {
        for (values, want) in [
            (["nohost", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw", "", "30 3 * * *", "y"], "Unknown host 'nohost'"),
            (["local", "tank/data", "hourly", "", "/root/pw", "", "30 3 * * *", "y"], "repository"),
            (["local", "tank/data", "hourly", "/mnt/usb/restic", "pw", "", "30 3 * * *", "y"], "password file"),
            (["local", "tank/data", "hourly", "rest:https://user:secret@host/r", "/root/pw", "", "30 3 * * *", "y"], "repository"),
        ] {
            let mut app = backup_form_app(values);
            app.handle_modal_key(KeyCode::F(5));
            assert!(app.pending_preflight.is_empty(), "{values:?}");
            assert!(app.backup_preflight.is_none(), "{values:?}");
            assert!(app.status_message.as_deref().is_some_and(|m| m.contains(want)), "{values:?}: {:?}", app.status_message);
        }
    }

    #[test]
    fn f5_does_nothing_in_other_forms() {
        let mut app = App::new(vec!["local".to_string()]);
        app.modal = Some(Modal::Form { title: "x".into(), fields: vec![FormField { label: "x".into(), value: "/r".into() }], focused: 0, description: None, kind: FormKind::CreateDataset { host: 0 } });
        app.handle_modal_key(KeyCode::F(5));
        assert!(app.pending_preflight.is_empty() && app.backup_preflight.is_none());
    }

    #[test]
    fn a_result_for_other_values_is_discarded_and_a_matching_one_is_kept() {
        let mut app = backup_form_app(BACKUP_GOOD);
        app.handle_modal_key(KeyCode::F(5));
        let stale = crate::restic::Preflight::Problem("old".into());
        app.tx.send(WorkerMsg::Preflight(0, "/some/other/repo".into(), "/root/pw".into(), stale)).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.backup_preflight.as_ref().unwrap().result, None);
        app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok)).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.backup_preflight.as_ref().unwrap().result, Some(crate::restic::Preflight::Ok));
    }
```

In `src/ui/hints.rs` tests, add:

```rust
    #[test]
    fn the_backup_form_advertises_f5_and_no_other_form_does() {
        let mut a = base();
        a.modal = Some(Modal::Form {
            title: "t".into(),
            fields: (0..8).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect(),
            focused: 3,
            description: None,
            kind: FormKind::BackupJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
        });
        assert!(keys(&a).contains(&"F5"));
        a.modal = Some(Modal::Form {
            title: "t".into(),
            fields: (0..7).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect(),
            focused: 3,
            description: None,
            kind: FormKind::SnapshotJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
        });
        assert!(!keys(&a).contains(&"F5"));
    }
```

(Use the local helpers `base()` and `keys()` this test module already defines for the F2 test, and import `FormField`/`FormKind` the way that test does.)

In `src/ui/mod.rs` tests, next to `apply_pending_pool_fetches_lists_dataset_names_for_a_dataset_fetch`:

```rust
    #[test]
    fn apply_pending_preflights_runs_the_check_on_the_hosts_runner_and_reports_it() {
        use crate::exec::test_support::FakeRunner;
        use crate::hosts::Host;
        use std::sync::Arc;
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![Host { name: "local".into(), runner: Arc::new(FakeRunner::new("{\"id\":\"x\"}")), ssh_target: None, tag: None }] };
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "/r".into(), password_file: "/pw".into(), result: None });
        app.pending_preflight = vec![crate::ui::state::PendingPreflight { host: 0, host_name: "local".into(), repository: "/r".into(), password_file: "/pw".into() }];
        apply_pending_preflights(&mut app, &inventory);
        assert!(app.pending_preflight.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.backup_preflight.as_ref().is_some_and(|c| c.result.is_none()) && std::time::Instant::now() < deadline {
            app.drain_worker_messages();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.backup_preflight.unwrap().result, Some(crate::restic::Preflight::Ok));
    }

    #[test]
    fn a_preflight_for_an_unknown_host_index_reports_a_problem_instead_of_waiting_forever() {
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory { hosts: vec![] };
        app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 3, repository: "/r".into(), password_file: "/pw".into(), result: None });
        app.pending_preflight = vec![crate::ui::state::PendingPreflight { host: 3, host_name: "gone".into(), repository: "/r".into(), password_file: "/pw".into() }];
        apply_pending_preflights(&mut app, &inventory);
        app.drain_worker_messages();
        assert!(matches!(app.backup_preflight.unwrap().result, Some(crate::restic::Preflight::Problem(m)) if m.contains("unknown host")));
    }
```

(Make `PreflightCheck`, `PendingPreflight` and the `App` fields `pub`; `tx` is private to `state.rs`, which is why the unknown-host case goes through `fail_backup_preflight`.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo test ui::state ui::hints ui::tests 2>&1 | tail -30` (or `cargo test f5_ preflight`)
Expected: compile errors (types and fields missing).

- [ ] **Step 3: Implement the state in `src/ui/state.rs`**

Add near `WorkerMsg` (before `pub enum WorkerMsg`):

```rust
/// What the backup form's F5 check was asked about, and what it found (`None` while it still runs).
#[derive(Debug, Clone, PartialEq)]
pub struct PreflightCheck {
    pub host: usize,
    pub repository: String,
    pub password_file: String,
    pub result: Option<crate::restic::Preflight>,
}

/// A check waiting for the event loop to hand it its host's runner (`App` cannot resolve a host to one itself).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPreflight {
    pub host: usize,
    pub host_name: String,
    pub repository: String,
    pub password_file: String,
}
```

Add to `WorkerMsg` (after `PoolNames`):

```rust
    /// The outcome of the backup form's F5 check: the host index, the repository and password file it was asked
    /// about (so a result for values since edited is dropped), and what it found.
    Preflight(usize, String, String, crate::restic::Preflight),
```

Add to `pub struct App` (after `pending_pool_fetch`) and to `App::new` (after `pending_pool_fetch: Vec::new(),`):

```rust
    pub backup_preflight: Option<PreflightCheck>,
    pub pending_preflight: Vec<PendingPreflight>,
```
```rust
            backup_preflight: None,
            pending_preflight: Vec::new(),
```

Add the F5 arm in `handle_modal_key`'s `Modal::Form` match, directly before `KeyCode::F(2) => {`:

```rust
                KeyCode::F(5) if matches!(kind, FormKind::BackupJob { .. }) => {
                    self.request_backup_preflight(field_value(&fields, 0).trim(), field_value(&fields, 3).trim(), field_value(&fields, 4).trim());
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
```

Add these methods to `impl App`, next to `fail_pool_fetch`:

```rust
    /// F5 in the backup form: validates what the save would validate, then queues the check for the event loop.
    /// A check already running for the same values is left alone.
    fn request_backup_preflight(&mut self, host_name: &str, repository: &str, password_file: &str) {
        let Some(host) = self.hosts.iter().position(|h| h == host_name) else {
            self.set_error(format!("Unknown host '{host_name}'. Configured hosts: {}", self.hosts.join(", ")));
            return;
        };
        if !crate::restic::valid_repository(repository) {
            self.set_error("Check: enter the repository first (a path, sftp:user@host:/path or rclone:remote:path, with no password in it)".to_string());
            return;
        }
        if !crate::restic::valid_password_file(password_file) {
            self.set_error("Check: the password file must be an absolute path on the host, such as /root/.restic-password".to_string());
            return;
        }
        let same = |c: &PreflightCheck| c.host == host && c.repository == repository && c.password_file == password_file;
        if self.backup_preflight.as_ref().is_some_and(|c| same(c) && c.result.is_none()) {
            return;
        }
        self.backup_preflight = Some(PreflightCheck { host, repository: repository.to_string(), password_file: password_file.to_string(), result: None });
        self.pending_preflight.push(PendingPreflight { host, host_name: host_name.to_string(), repository: repository.to_string(), password_file: password_file.to_string() });
    }

    /// Runs one queued check on a worker thread and reports it as `WorkerMsg::Preflight`.
    pub fn run_backup_preflight(&mut self, runner: Arc<dyn CommandRunner>, pending: PendingPreflight) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::preflight::run(runner, &pending.host_name, &pending.repository, &pending.password_file, crate::preflight::DEADLINE);
            let _ = tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, result));
        });
    }

    /// Reports a check that could not start (the host index no longer resolves) through the same channel.
    pub fn fail_backup_preflight(&self, pending: PendingPreflight, message: String) {
        let _ = self.tx.send(WorkerMsg::Preflight(pending.host, pending.repository, pending.password_file, crate::restic::Preflight::Problem(message)));
    }
```

Add the drain arm next to `WorkerMsg::PoolNames(...)` in `drain_worker_messages`:

```rust
                WorkerMsg::Preflight(host, repository, password_file, result) => {
                    // Only the answer to the question still asked is kept: a result for edited values is dropped.
                    if let Some(check) = self.backup_preflight.as_mut().filter(|c| c.host == host && c.repository == repository && c.password_file == password_file && c.result.is_none()) {
                        check.result = Some(result);
                    }
                }
```

If `drain_worker_messages`'s `match` has a catch-all that returns the refreshed sections, mirror how the `PoolNames` arm contributes nothing to it.

- [ ] **Step 4: Event loop wiring in `src/ui/mod.rs`**

Add after `apply_pending_pool_fetches`:

```rust
/// Hands each queued backup-form check its host's runner (same split as the pickers' fetches).
fn apply_pending_preflights(app: &mut App, inventory: &HostInventory) {
    for pending in std::mem::take(&mut app.pending_preflight) {
        if let Some(h) = inventory.hosts.get(pending.host) {
            app.run_backup_preflight(h.runner.clone(), pending);
        } else {
            let message = format!("unknown host '{}'", pending.host_name);
            app.fail_backup_preflight(pending, message);
        }
    }
}
```

and call it in `event_loop` right after `apply_pending_pool_fetches(app, inventory);`:

```rust
        apply_pending_preflights(app, inventory);
```

- [ ] **Step 5: The hint in `src/ui/hints.rs`**

In `modal_hints`, in the `Modal::Form` arm, before `v.push(h("Esc", "Cancel", 0));`:

```rust
            if matches!(kind, crate::ui::modal::FormKind::BackupJob { .. }) {
                v.push(h("F5", "Check", 1));
            }
```

(use the import style the file already uses for `FormKind`).

- [ ] **Step 6: Run to see them pass, then lint**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all PASS. The `book_check` tests may now fail on the stale hint line in chapter 15's capture; they only compare chapter text with capture *files*, so they should still pass until Task 5 regenerates both. If any test compares the hint set with the book, note it for Task 5.

- [ ] **Step 7: Commit**

```bash
git add src/ui/state.rs src/ui/mod.rs src/ui/hints.rs
git commit -m "feat: F5 in the backup job form runs the repository preflight check

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Show the result inside the form

**Files:**
- Modify: `src/ui/layout.rs` (`draw_modal` takes the check; new pure `preflight_status`; line count; styled line; tests)

**Interfaces:**
- Consumes: `app.backup_preflight: Option<PreflightCheck>`, `restic::Preflight`, `FormKind::BackupJob { hosts, .. }`, `Theme` fields `danger`, `dim()`.
- Produces: `fn preflight_status(kind: &FormKind, fields: &[FormField], check: Option<&PreflightCheck>) -> Option<(String, bool)>`: the text to show and whether it is a problem. `None` when there is nothing to show (not a backup form, no check, or the check no longer matches the fields on screen).

- [ ] **Step 1: Write the failing tests** in `src/ui/layout.rs`'s test module:

```rust
    fn backup_form_fields(host: &str, repo: &str, pw: &str) -> Vec<crate::ui::modal::FormField> {
        let values = [host, "tank/data", "auto", repo, pw, "", "30 3 * * *", "y"];
        values.iter().map(|v| crate::ui::modal::FormField { label: "x".into(), value: (*v).to_string() }).collect()
    }

    fn backup_kind_for_status() -> crate::ui::modal::FormKind {
        crate::ui::modal::FormKind::BackupJob { hosts: vec!["local".into(), "fbsd16".into()], existing: None, jobs: vec![] }
    }

    fn check(host: usize, repo: &str, pw: &str, result: Option<crate::restic::Preflight>) -> crate::ui::state::PreflightCheck {
        crate::ui::state::PreflightCheck { host, repository: repo.into(), password_file: pw.into(), result }
    }

    #[test]
    fn the_check_line_shows_checking_ok_and_a_problem_for_the_values_on_screen() {
        let fields = backup_form_fields("fbsd16", "/r", "/pw");
        let kind = backup_kind_for_status();
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", None))), Some(("Check: checking the repository...".to_string(), false)));
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok)))), Some(("Check: OK, the repository opens.".to_string(), false)));
        let problem = crate::restic::Preflight::Problem("no readable password file at /pw on fbsd16".to_string());
        assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(problem)))), Some(("Check: no readable password file at /pw on fbsd16".to_string(), true)));
    }

    #[test]
    fn the_check_line_disappears_when_a_field_it_depends_on_changes() {
        let kind = backup_kind_for_status();
        let ok = check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok));
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r2", "/pw"), Some(&ok)).is_none(), "repository edited");
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r", "/pw2"), Some(&ok)).is_none(), "password file edited");
        assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none(), "host edited");
        assert!(preflight_status(&kind, &backup_form_fields("fbsd16", " /r ", "/pw"), Some(&ok)).is_some(), "surrounding spaces are trimmed like the check does");
    }

    #[test]
    fn only_the_backup_form_shows_a_check_line() {
        let ok = check(0, "/r", "/pw", Some(crate::restic::Preflight::Ok));
        let kind = crate::ui::modal::FormKind::CreateDataset { host: 0 };
        assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none());
        assert!(preflight_status(&backup_kind_for_status(), &backup_form_fields("local", "/r", "/pw"), None).is_none());
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test ui::layout::tests::the_check_line ui::layout::tests::only_the_backup`
Expected: compile error, `preflight_status` not found.

- [ ] **Step 3: Implement**

In `src/ui/layout.rs`, next to `form_preview`:

```rust
/// The backup form's F5 result as one line, and whether it is a problem. Shown only while the check still
/// describes what is on screen: editing the host, repository or password file hides it.
fn preflight_status(kind: &crate::ui::modal::FormKind, fields: &[crate::ui::modal::FormField], check: Option<&crate::ui::state::PreflightCheck>) -> Option<(String, bool)> {
    let crate::ui::modal::FormKind::BackupJob { hosts, .. } = kind else { return None };
    let check = check?;
    let value = |i: usize| fields.get(i).map_or("", |f| f.value.as_str()).trim();
    if hosts.get(check.host).map(String::as_str) != Some(value(0)) || check.repository != value(3) || check.password_file != value(4) {
        return None;
    }
    Some(match &check.result {
        None => ("Check: checking the repository...".to_string(), false),
        Some(crate::restic::Preflight::Ok) => ("Check: OK, the repository opens.".to_string(), false),
        Some(crate::restic::Preflight::Problem(m)) => (format!("Check: {m}"), true),
    })
}
```

Change the call site (line ~79) to `draw_modal(f, &modal, app.backup_preflight.as_ref(), &app.theme, area);` and the signature to:

```rust
fn draw_modal(f: &mut Frame, modal: &Modal, check: Option<&crate::ui::state::PreflightCheck>, theme: &Theme, area: Rect) {
```

In the `content_lines` `Modal::Form` arm, count the extra rows (a blank separator plus the wrapped line) inside `fields_lines`:

```rust
            let status_lines: u16 = preflight_status(kind, fields, check).map_or(0, |(text, _)| 1 + wrapped_line_count(&text, inner_width));
            let fields_lines: u16 = preview_lines
                + status_lines
                + fields
                    .iter()
                    .map(|field| wrapped_line_count(&format!("  {}: {}", field.label, field.value), inner_width))
                    .sum::<u16>();
```

In the drawing arm for `Modal::Form`, build styled lines instead of one joined string, so a problem uses the theme's danger style. Replace the `let mut lines = Vec::new(); ... Paragraph::new(lines.join("\n"))` body with:

```rust
        Modal::Form { title, fields, focused, description, kind } => {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(desc) = description {
                lines.push(Line::raw(desc.clone()));
                lines.push(Line::raw(String::new()));
            }
            for (i, field) in fields.iter().enumerate() {
                let marker = if i == *focused { ">" } else { " " };
                let shown = if kind.masks_field(i) { "*".repeat(field.value.chars().count()) } else { field.value.clone() };
                lines.push(Line::raw(format!("{marker} {}: {shown}", field.label)));
            }
            let preview = form_preview(kind, fields);
            if !preview.is_empty() {
                lines.push(Line::raw(String::new()));
                lines.extend(preview.into_iter().map(Line::raw));
            }
            if let Some((text, problem)) = preflight_status(kind, fields, check) {
                lines.push(Line::raw(String::new()));
                lines.push(if problem { Line::styled(text, theme.danger) } else { Line::raw(text) });
            }
            let p = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
            f.render_widget(p, popup_area);
        }
```

(`Line` is already imported in `layout.rs`: it is used by the Confirm arm. A `\n` inside a description still renders as before because `Line::raw` of a multi-line string is not split; if a description contains `\n`, split it first with `desc.lines().map(|l| Line::raw(l.to_string()))`. The current descriptions are single-line, so keep `Line::raw(desc.clone())` unless a test shows otherwise.)

- [ ] **Step 4: Run to see them pass; check nothing else moved**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS. A form with no check renders exactly as before (the new lines only appear when `preflight_status` returns `Some`), so existing layout tests and the book captures are unaffected.

- [ ] **Step 5: Commit**

```bash
git add src/ui/layout.rs
git commit -m "feat: show the F5 repository check result inside the backup form

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Hub verification, docs, book and notes

**Files:**
- Modify: `src/ui/state.rs` (`help_text()`: one line about F5 in the Jobs/backup part; the `help_documents_the_backup_key` test at ~10815 shows the pattern)
- Modify: `README.md` (backup section: one short paragraph)
- Modify: `docs/book/15-backups.md` (a "Check before you save" part; hint line in the form capture; a new capture)
- Modify: `docs/book/tools/capture.py` (support `<F5>` in `--keys`)
- Create/Update: `docs/book/captures/15-backup-form.txt` (regenerated), `docs/book/captures/15-backup-check.txt` (new)
- Modify: `/home/me/.claude/projects/-home-me-dev-rust-zfsctl/memory/` (a project memory note and the `MEMORY.md` line)

**Interfaces:**
- Consumes: everything above, on the real hub.

- [ ] **Step 1: Help text and a test**

Add to `help_text()`, in the Jobs section next to the backup-job line, a line saying `F5 in the backup form checks the repository and password file on the host (read-only)`. Add a test beside `help_documents_the_backup_key`:

```rust
    #[test]
    fn help_documents_the_backup_form_check_key() {
        assert!(help_text().contains("F5"));
    }
```

Run: `cargo test help_documents` — Expected: PASS after the edit (fails before it).

- [ ] **Step 2: Build and deploy to the hub**

Use the project's existing deploy routine for the hub (see the project memory "zfsctl FreeBSD test boxes" and how earlier plans built `zfsctl` on `fb`: build in `~/zfsctl` on the hub with `cargo build --release`, install to `/usr/local/bin/zfsctl`, restart the rc daemon). Do not `ssh root@`; use `fb@172.16.10.90` with sudo.

Expected: `zfsctl` on the hub runs the new build (`/usr/local/bin/zfsctl --help` still prints the usage line).

- [ ] **Step 3: Verify live, case by case, in the real TUI**

Drive it by hand or with the pexpect+pyte harness (memory: "Driving the zfsctl TUI live from a script"). Re-create the throwaway fixtures of Task 1 Step 1, open Jobs, `B`, and fill Host `local`, Dataset any, Repository and Password file per case, then press `F5`:

| Case | Repository / password file | Expect in the form |
|---|---|---|
| working local | `/root/zit-pre/repo` / `/root/zit-pre/pw` | `Check: OK, the repository opens.` |
| working rclone | `rclone:t1:zit-pre-repo` / `/root/zit-pre/pw` | OK |
| wrong password | repo above / `/root/zit-pre/pw-wrong` | `does not unlock` |
| no repository | `/root/zit-pre/no-repo` / pw | `restic init` |
| remote not defined | `rclone:nosuchremote:x` / pw | `rclone remote 'nosuchremote' is not in root's rclone config` |
| password file missing | repo / `/root/zit-pre/none` | `no readable password file` |
| restic not allowed | Host = a VM without the restic sudoers line | `print-sudoers` |
| dead host (needs `rclone config create zit-dead ...` as in Task 1 K) | `rclone:zit-dead:x` | `no answer from local in 45 s` after 45 s, form usable meanwhile |

Also check: typing in the repository field hides the line; F5 twice shows one check; the TUI stays responsive during the dead-host wait (move focus with Tab); saving the form still works whatever the result. Confirm no `/root/.cache/restic` was created by the check (`ls -ld /root/.cache/restic` before and after on a host that never had one, or check its mtime).

Where a message differs from the table, fix `classify_preflight` (add the recorded text to its test first), re-run `cargo test`, redeploy, repeat. Clean up the fixtures afterwards exactly as in Task 1 Step 3.

- [ ] **Step 4: Capture tool and book**

In `docs/book/tools/capture.py`, add `<F5>` to the key expansion (xterm sequence `\x1b[15~`) next to `<Tab>`/`<Enter>`; keep the existing style of that table.

Regenerate the form capture and take the new one with the tool, following `docs/book/tools/README.md` and the arguments the existing `15-backup-form` capture used (find them with `git log -S15-backup-form --oneline` and the plan `docs/superpowers/plans/2026-10-10-zfsctl-book.md`): `15-backup-form` (now with `F5 Check` in the hint line) and `15-backup-check` (the form after F5 with a result line, using a throwaway repository so no real host name, address or secret appears; the tool already strips lines containing an email or the word `password`, so choose a repository path that does not, and check the capture by eye).

In `docs/book/15-backups.md`, in "The backup job", replace the old embedded form block with the regenerated `15-backup-form.txt` text, and add after the field list a short part, "Check before you save", with the new capture and these points: press `F5` on the form to try the repository and password file on that host; it only reads, never creates a repository, and saving is never blocked; what the messages mean in one line each (the table in the spec); a failure is not a reason to panic, fix it and press `F5` again. Keep the chapter's required headings and the callout style (`> **Tip:**`).

Run: `cargo test book_check` — Expected: PASS (embedded blocks equal capture files, links resolve, callouts intact). Fix anything the drift tests report by regenerating, never by hand-editing a capture.

- [ ] **Step 5: README**

In the backup section of `README.md`, after the paragraph on sftp/rclone repositories, add: "**Check before you save.** `F5` in the backup form opens the repository with the password file on the job's host (`restic cat config`, read-only) and says what is missing: the rclone remote, the password file, a wrong password, a repository not yet created with `restic init`, or a restic that sudoers does not allow. It never creates anything and never blocks saving." Run `cargo test` (the README/book consistency tests) — Expected: PASS.

- [ ] **Step 6: Whole-suite check and final commit**

Run: `cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo test`
Expected: all green.

```bash
git add -A src docs README.md
git commit -m "docs: the backup form check in Help, README and the book; capture tool learns F5

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Memory**

Write a project memory file `project_zfsctl_backup_preflight.md` (frontmatter `type: project`) recording: built on branch `backup-preflight`, what the check is (F5, one `restic cat config --no-cache`, read-only, no new sudoers), which messages were verified live and any live-only findings from Step 3, and that merging is the user's call. Add its one-line pointer to `MEMORY.md`. Link `[[project_zfsctl_restic_backups]]`.

- [ ] **Step 8: Hand back**

Do not merge. Report: tests green, which live cases passed and any that did not, and ask whether to merge `backup-preflight` into `master` (use superpowers:finishing-a-development-branch).
