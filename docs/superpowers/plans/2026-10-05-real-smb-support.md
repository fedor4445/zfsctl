# Real SMB Support (smb4.conf) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the dead `sharesmb`-property SMB handling with real, connectable SMB sharing: `zfsctl` owns one marker-delimited block in `/usr/local/etc/smb4.conf`, sets the share user's Samba password, and reloads Samba.

**Architecture:** A new `CommandRunner::run_with_stdin` (needed for `smbpasswd -a -s` and for writing the file via `tee`) underpins a new `src/zfs/smb.rs` (pure block parse/render + I/O orchestration with rollback). `Share::Smb` is re-sourced from that block, and the Datasets `'M'` key, the Shares `Enter`/`d` keys and the hint bar are wired to two new `MutationCall` variants.

**Tech Stack:** Rust, ratatui/crossterm, `thiserror`; Samba 4 on FreeBSD (`smbpasswd`, `service samba_server reload`).

**Spec:** `docs/superpowers/specs/2026-10-04-real-smb-support-design.md`

## Global Constraints

- Managed block markers, verbatim: `## zfsctl managed shares -- do not edit by hand -- start` / `... -- end`.
- Config path: `/usr/local/etc/smb4.conf`. Everything outside the markers is never modified, byte for byte.
- One managed SMB share per dataset; one optional "Valid user" per share; `browseable = yes` hardcoded.
- Share name charset: letters, digits, `-`, `_` only.
- Password is piped as `printf '<pw>\n<pw>\n' | smbpasswd -a -s <user>` (stdin only, never an argument).
- Every remote command goes through `sudo -n` (existing `SshRunner`). **Never** write the file via `sh -c`; use path-pinned `tee`/`cat`.
- `'M'` in Datasets (Middle focus) is the new key; the old `sharesmb`-based `Share::Smb` is removed entirely.
- Existing project rules: verify end-to-end on the real box (a clean config round-trip proves nothing); derive exhaustive lists (sudoers) from the code.

## Review Focus

Failure modes the spec implies but that are easy to miss; each has a pinning test in the task named.

1. smb4.conf with a start marker but no end marker, two blocks, or end-before-start must be **refused**, never "fixed" by appending a second block (Task 2).
2. Empty file, file without a trailing newline, and a *missing* file (`No such file or directory`, i.e. first-ever share on a host) must all work (Tasks 2, 3).
3. A share name equal (case-insensitively) to an unmanaged stanza such as `[alice]`/`[global]` or to another managed share would corrupt Samba's config — refuse (Tasks 3, 5).
4. Reload failing after the file was written must restore the previous file and report both facts distinctly (Task 3).
5. A Valid user starting with `-` (flag smuggling into `smbpasswd`), and a password with leading/trailing spaces or shell metacharacters, must be handled verbatim and never shown in the form or in `Debug` output (Tasks 1, 2, 5).
6. A host without Samba, or whose sudoers hasn't been updated yet, must not break the NFS half of the Shares view (Task 4).

## File Structure

- Modify `src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs` — `run_with_stdin`.
- Modify `src/error.rs` — `AppError::Smb`.
- Create `src/zfs/smb.rs` — `SmbShare`, block parse/render, validation, `read_conf`/`list_managed`/`set_share`/`remove_share`.
- Modify `src/zfs/mod.rs` — `pub mod smb;`.
- Modify `src/zfs/share.rs` — new `Share::Smb` shape, drop `sharesmb`, `SharesSnapshot.smb_error`.
- Modify `src/ui/modal.rs` — `Secret`, `MutationCall::{SetSmbShare, RemoveSmbShare}`, `FormKind::ShareSmb`.
- Modify `src/ui/state.rs`, `src/ui/layout.rs`, `src/ui/hints.rs` — key handlers, form, rendering, hints.
- Modify `README.md` — sudoers + SMB docs.

---

### Task 1: `CommandRunner::run_with_stdin`

**Files:**
- Modify: `src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs`

**Interfaces:**
- Produces: `CommandRunner::run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput>`; `FakeRunner::{stdin_calls: Mutex<Vec<(String, String)>> /* (cmd, stdin) */, route_failure(cmd, first_arg, stderr, status) -> Self}`; `pub(crate) fn exec::run_command_with_stdin(Command, &str, &str) -> Result<CommandOutput>`. `FakeRunner::run_with_stdin` also appends to `calls` and honours `route`/`route_failure`.

- [ ] **Step 1: Write failing tests** in `src/exec/local.rs` `mod tests`:

```rust
    #[test]
    fn run_with_stdin_feeds_the_child_its_stdin() {
        let output = LocalRunner.run_with_stdin("cat", &[], "hello\nworld\n").unwrap();
        assert_eq!(output.stdout, "hello\nworld\n");
        assert!(output.success());
    }

    #[test]
    fn run_with_stdin_does_not_deadlock_on_input_larger_than_a_pipe_buffer() {
        let big = "x".repeat(1_000_000);
        let output = LocalRunner.run_with_stdin("cat", &[], &big).unwrap();
        assert_eq!(output.stdout.len(), big.len());
    }

    #[test]
    fn run_with_stdin_reports_exit_status_when_the_child_ignores_stdin() {
        let output = LocalRunner.run_with_stdin("false", &[], &"y".repeat(1_000_000)).unwrap();
        assert!(!output.success());
    }
```

and in `src/exec/mod.rs` `mod tests`:

```rust
    #[test]
    fn fake_runner_records_stdin_and_honours_failure_routes() {
        let runner = FakeRunner::new("").route_failure("smbpasswd", "-a", "no such user", 1);
        let out = runner.run_with_stdin("smbpasswd", &["-a", "-s", "bob"], "pw\npw\n").unwrap();
        assert_eq!(out.status, 1);
        assert_eq!(out.stderr, "no such user");
        assert_eq!(runner.stdin_calls.lock().unwrap()[0], ("smbpasswd".to_string(), "pw\npw\n".to_string()));
        assert_eq!(runner.calls.lock().unwrap()[0].0, "smbpasswd");
    }
```

- [ ] **Step 2: Run** `cargo test exec::` — Expected: compile FAIL (`run_with_stdin` not found).

- [ ] **Step 3: Implement.** In `src/exec/mod.rs` add to the trait and add the shared helper:

```rust
pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
    /// Like `run`, but writes `stdin` to the child's standard input and
    /// then closes it. Needed for `smbpasswd -s` (password on stdin, never
    /// an argument) and for writing a file via `tee`.
    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput>;
}

/// Shared by `LocalRunner` and `SshRunner`. Stdin is written from a
/// separate thread: `tee` echoes its input to stdout, so writing it all
/// before reading any output would deadlock once the input exceeds a
/// pipe buffer. A write error (child exited without reading everything)
/// is ignored on purpose: the exit status is the real signal.
pub(crate) fn run_command_with_stdin(
    mut command: std::process::Command,
    spawn_name: &str,
    stdin: &str,
) -> Result<CommandOutput> {
    use crate::error::AppError;
    use std::io::Write;
    use std::process::Stdio;

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AppError::Spawn { cmd: spawn_name.to_string(), source })?;
    let mut pipe = child.stdin.take().expect("stdin was requested as piped");
    let data = stdin.to_string();
    let writer = std::thread::spawn(move || {
        let _ = pipe.write_all(data.as_bytes());
    });
    let output = child
        .wait_with_output()
        .map_err(|source| AppError::Spawn { cmd: spawn_name.to_string(), source })?;
    let _ = writer.join();
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        status: output.status.code().unwrap_or(-1),
    })
}
```

`FakeRunner`: add field `pub stdin_calls: Mutex<Vec<(String, String)>>` (initialise `Mutex::new(Vec::new())` in both `new` and `failing`), add the builder, and the trait method:

```rust
        /// Like `route`, but scripts a failing response. Builder-style.
        pub fn route_failure(mut self, cmd: &str, first_arg: &str, stderr: &str, status: i32) -> Self {
            self.routes.insert(
                (cmd.to_string(), first_arg.to_string()),
                CommandOutput { stdout: String::new(), stderr: stderr.to_string(), status },
            );
            self
        }
```

```rust
        fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
            self.stdin_calls.lock().unwrap().push((cmd.to_string(), stdin.to_string()));
            self.run(cmd, args)
        }
```

`src/exec/local.rs`:

```rust
    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        let mut command = Command::new(cmd);
        command.args(args);
        super::run_command_with_stdin(command, cmd, stdin)
    }
```

`src/exec/ssh.rs`:

```rust
    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        super::run_command_with_stdin(command, "ssh", stdin)
    }
```

- [ ] **Step 4: Run** `cargo test` — Expected: all pass (compile errors elsewhere mean another `CommandRunner` impl exists; add the method there).

- [ ] **Step 5: Commit**

```bash
git add src/exec && git commit -m "feat: CommandRunner::run_with_stdin for stdin-fed commands

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `src/zfs/smb.rs` — pure block parsing, rendering, validation

**Files:**
- Create: `src/zfs/smb.rs`; Modify: `src/zfs/mod.rs` (add `pub mod smb;`), `src/error.rs`

**Interfaces:**
- Produces (all in `crate::zfs::smb`): `CONF_PATH: &str`; `SmbShare { name, dataset, path: String, valid_user: Option<String>, read_only: bool }` (derive `Debug, Clone, PartialEq`); `parse_managed(&str) -> Result<Vec<SmbShare>>`; `render_with_shares(&str, &[SmbShare]) -> Result<String>`; `outside_block_stanza_names(&str) -> Result<Vec<String>>` (lowercased); `validate_share_name(&str) -> std::result::Result<(), String>`; `validate_user(&str) -> std::result::Result<(), String>`. `AppError::Smb(String)` displayed as `SMB: {0}`.

Managed stanza format (the dataset comment is how a stanza is tied to its dataset; a stanza without it inside the block is an error, never silently dropped):

```
[media]
    # zfsctl-dataset = tank/media
    path = /tank/media
    valid users = carol
    browseable = yes
    read only = no
```

- [ ] **Step 1: Add the error variant** in `src/error.rs`:

```rust
    #[error("SMB: {0}")]
    Smb(String),
```

- [ ] **Step 2: Write failing tests.** Create `src/zfs/smb.rs` containing only the module doc, the constants, the `SmbShare` struct, `todo!()`-bodied signatures for the functions above, and this test module (the fixture is the real `fb` file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const FB_CONF: &str = "[global]\n    workgroup = WORKGROUP\n    security = user\n    map to guest = Bad User\n\n[alice]\n    path = /data/share/alice\n    valid users = alice\n    browseable = yes\n    read only = no\n\n[carol]\n    path = /data/share/carol\n    valid users = carol\n    browseable = yes\n    read only = no\n";

    pub(crate) fn share(name: &str, dataset: &str, user: Option<&str>, ro: bool) -> SmbShare {
        SmbShare {
            name: name.to_string(),
            dataset: dataset.to_string(),
            path: format!("/{dataset}"),
            valid_user: user.map(str::to_string),
            read_only: ro,
        }
    }

    #[test]
    fn no_block_means_no_managed_shares() {
        assert_eq!(parse_managed(FB_CONF).unwrap(), vec![]);
        assert_eq!(parse_managed("").unwrap(), vec![]);
    }

    #[test]
    fn render_appends_a_block_to_a_file_without_one_and_preserves_every_original_byte() {
        let out = render_with_shares(FB_CONF, &[share("media", "tank/media", Some("carol"), false)]).unwrap();
        assert!(out.starts_with(FB_CONF), "original bytes must be an untouched prefix");
        assert_eq!(parse_managed(&out).unwrap(), vec![share("media", "tank/media", Some("carol"), false)]);
        assert_eq!(out.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn render_handles_empty_file_and_file_without_trailing_newline() {
        let out = render_with_shares("", &[share("a", "tank/a", None, true)]).unwrap();
        assert!(out.starts_with(BLOCK_START));
        let out = render_with_shares("[global]\n  x = y", &[share("a", "tank/a", None, true)]).unwrap();
        assert!(out.starts_with("[global]\n  x = y\n"));
        assert_eq!(parse_managed(&out).unwrap().len(), 1);
    }

    #[test]
    fn render_replaces_an_existing_block_in_place_leaving_before_and_after_untouched() {
        let before = "[global]\n  a = b\n\n";
        let after = "\n[alice]\n  path = /x\n";
        let first = render_with_shares(&format!("{before}{after}"), &[share("one", "tank/one", None, true)]).unwrap();
        // Hand-place text after the block, as a real file could have.
        let with_tail = format!("{first}[tail]\n  path = /tail\n");
        let second = render_with_shares(
            &with_tail,
            &[share("one", "tank/one", None, true), share("two", "tank/two", Some("bob"), false)],
        )
        .unwrap();
        assert!(second.starts_with(before));
        assert!(second.ends_with("[tail]\n  path = /tail\n"));
        assert_eq!(second.matches(BLOCK_START).count(), 1, "must never append a second block");
        assert_eq!(parse_managed(&second).unwrap().len(), 2);
    }

    #[test]
    fn render_with_no_shares_keeps_an_empty_block_and_is_idempotent() {
        let once = render_with_shares(FB_CONF, &[]).unwrap();
        let twice = render_with_shares(&once, &[]).unwrap();
        assert_eq!(once, twice);
        assert_eq!(parse_managed(&once).unwrap(), vec![]);
    }

    #[test]
    fn parse_ignores_unmanaged_stanzas_entirely() {
        let conf = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let names: Vec<String> = parse_managed(&conf).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["media".to_string()]);
    }

    #[test]
    fn unterminated_duplicate_or_reversed_markers_are_refused() {
        let start_only = format!("{FB_CONF}{BLOCK_START}\n");
        assert!(parse_managed(&start_only).is_err());
        assert!(render_with_shares(&start_only, &[]).is_err(), "must not append a second block");
        let two = format!("{BLOCK_START}\n{BLOCK_END}\n{BLOCK_START}\n{BLOCK_END}\n");
        assert!(parse_managed(&two).is_err());
        let reversed = format!("{BLOCK_END}\n{BLOCK_START}\n");
        assert!(parse_managed(&reversed).is_err());
    }

    #[test]
    fn a_malformed_block_is_an_error_not_a_silent_drop() {
        let wrap = |body: &str| format!("{BLOCK_START}\n{body}{BLOCK_END}\n");
        assert!(parse_managed(&wrap("[a]\n    path = /a\n")).is_err(), "missing dataset comment");
        assert!(parse_managed(&wrap("[a]\n    # zfsctl-dataset = tank/a\n")).is_err(), "missing path");
        assert!(parse_managed(&wrap("[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n    guest ok = yes\n")).is_err(), "unknown key");
        assert!(parse_managed(&wrap("path = /a\n")).is_err(), "key before any stanza");
        let dup = "[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n[A]\n    # zfsctl-dataset = tank/b\n    path = /b\n";
        assert!(parse_managed(&wrap(dup)).is_err(), "duplicate share name, case-insensitive");
        let multi = "[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n    valid users = x, y\n";
        assert!(parse_managed(&wrap(multi)).is_err(), "multiple users unsupported");
    }

    #[test]
    fn read_only_round_trips_and_defaults_to_samba_semantics() {
        let conf = render_with_shares("", &[share("a", "tank/a", None, true), share("b", "tank/b", None, false)]).unwrap();
        let parsed = parse_managed(&conf).unwrap();
        assert!(parsed[0].read_only);
        assert!(!parsed[1].read_only);
    }

    #[test]
    fn outside_block_names_are_lowercased_and_exclude_managed_ones() {
        let conf = render_with_shares(FB_CONF, &[share("Media", "tank/media", None, true)]).unwrap();
        let names = outside_block_stanza_names(&conf).unwrap();
        assert_eq!(names, vec!["global".to_string(), "alice".to_string(), "carol".to_string()]);
    }

    #[test]
    fn share_name_validation() {
        for ok in ["media", "My-Share_2"] {
            assert!(validate_share_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "has space", "br[ack]et", "a/b", "global", "HOMES", "printers", "ünï"] {
            assert!(validate_share_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_share_name(&"a".repeat(81)).is_err());
    }

    #[test]
    fn user_validation_rejects_flag_lookalikes_and_odd_characters() {
        for ok in ["carol", "carol.s", "_svc", "a-b", "u1"] {
            assert!(validate_user(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "--x", "a b", "a,b", "a\nb", "a;b", "a/b"] {
            assert!(validate_user(bad).is_err(), "{bad:?}");
        }
    }
}
```

- [ ] **Step 3: Run** `cargo test zfs::smb` — Expected: FAIL (`todo!()` panics).

- [ ] **Step 4: Implement** (replace the stubs):

```rust
use crate::error::{AppError, Result};

pub const CONF_PATH: &str = "/usr/local/etc/smb4.conf";
pub const BLOCK_START: &str = "## zfsctl managed shares -- do not edit by hand -- start";
pub const BLOCK_END: &str = "## zfsctl managed shares -- do not edit by hand -- end";
const DATASET_KEY: &str = "# zfsctl-dataset";

/// One managed share. `dataset` is the identity (one share per dataset);
/// `name` is the `[stanza]` label Windows clients see.
#[derive(Debug, Clone, PartialEq)]
pub struct SmbShare {
    pub name: String,
    pub dataset: String,
    pub path: String,
    pub valid_user: Option<String>,
    pub read_only: bool,
}

fn smb_err(msg: impl Into<String>) -> AppError {
    AppError::Smb(msg.into())
}

/// Byte range `[start, end)` covering the START line through the END
/// line's newline.
struct BlockSpan {
    start: usize,
    end: usize,
}

fn locate_block(conf: &str) -> Result<Option<BlockSpan>> {
    let (mut offset, mut start, mut end) = (0usize, None, None);
    for line in conf.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']).trim_end();
        if text == BLOCK_START {
            if start.is_some() {
                return Err(smb_err("smb4.conf has more than one zfsctl start marker; fix it by hand"));
            }
            start = Some(offset);
        } else if text == BLOCK_END {
            if start.is_none() {
                return Err(smb_err("smb4.conf has a zfsctl end marker before any start marker; fix it by hand"));
            }
            if end.is_some() {
                return Err(smb_err("smb4.conf has more than one zfsctl end marker; fix it by hand"));
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }
    match (start, end) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => Ok(Some(BlockSpan { start, end })),
        _ => Err(smb_err("smb4.conf has a zfsctl start marker without an end marker; fix it by hand")),
    }
}

#[derive(Default)]
struct Partial {
    name: String,
    dataset: Option<String>,
    path: Option<String>,
    valid_user: Option<String>,
    read_only: Option<bool>,
}

fn finish(p: Partial, out: &mut Vec<SmbShare>) -> Result<()> {
    let dataset = p.dataset.ok_or_else(|| smb_err(format!("managed share [{}] has no '# zfsctl-dataset' line", p.name)))?;
    let path = p.path.ok_or_else(|| smb_err(format!("managed share [{}] has no 'path'", p.name)))?;
    if out.iter().any(|s| s.name.eq_ignore_ascii_case(&p.name)) {
        return Err(smb_err(format!("managed share name [{}] appears twice", p.name)));
    }
    if out.iter().any(|s| s.dataset == dataset) {
        return Err(smb_err(format!("dataset '{dataset}' has more than one managed share")));
    }
    out.push(SmbShare { name: p.name, dataset, path, valid_user: p.valid_user, read_only: p.read_only.unwrap_or(true) });
    Ok(())
}

/// Parses only what lies strictly between the markers. Strict on
/// purpose: anything unrecognised inside the block is an error, because
/// the next write rewrites the whole block and would silently drop it.
pub fn parse_managed(conf: &str) -> Result<Vec<SmbShare>> {
    let Some(span) = locate_block(conf)? else { return Ok(Vec::new()) };
    let lines: Vec<&str> = conf[span.start..span.end].lines().collect();
    let body = &lines[1..lines.len() - 1];
    let mut shares = Vec::new();
    let mut current: Option<Partial> = None;
    for raw in body {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(p) = current.take() {
                finish(p, &mut shares)?;
            }
            current = Some(Partial { name: line[1..line.len() - 1].trim().to_string(), ..Default::default() });
            continue;
        }
        let Some(p) = current.as_mut() else {
            return Err(smb_err(format!("unexpected line in managed block before any [share]: '{line}'")));
        };
        if let Some(rest) = line.strip_prefix(DATASET_KEY) {
            let value = rest.trim_start().strip_prefix('=').map(str::trim).filter(|v| !v.is_empty());
            p.dataset = Some(value.ok_or_else(|| smb_err(format!("bad '# zfsctl-dataset' line: '{line}'")))?.to_string());
        } else if line.starts_with('#') || line.starts_with(';') {
            // ordinary comment
        } else if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim().to_lowercase(), value.trim());
            match key.as_str() {
                "path" => p.path = Some(value.to_string()),
                "valid users" => {
                    if value.contains(|c: char| c == ',' || c.is_whitespace()) {
                        return Err(smb_err(format!("[{}] lists several valid users; only one is supported", p.name)));
                    }
                    p.valid_user = if value.is_empty() { None } else { Some(value.to_string()) };
                }
                "browseable" => {}
                "read only" => p.read_only = Some(!value.eq_ignore_ascii_case("no")),
                _ => return Err(smb_err(format!("unrecognised setting in managed block [{}]: '{line}'", p.name))),
            }
        } else {
            return Err(smb_err(format!("unrecognised line in managed block: '{line}'")));
        }
    }
    if let Some(p) = current.take() {
        finish(p, &mut shares)?;
    }
    Ok(shares)
}

fn render_block(shares: &[SmbShare]) -> String {
    let mut out = format!("{BLOCK_START}\n");
    for s in shares {
        out.push_str(&format!("[{}]\n    {DATASET_KEY} = {}\n    path = {}\n", s.name, s.dataset, s.path));
        if let Some(user) = &s.valid_user {
            out.push_str(&format!("    valid users = {user}\n"));
        }
        out.push_str(&format!("    browseable = yes\n    read only = {}\n", if s.read_only { "yes" } else { "no" }));
    }
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

/// Returns `conf` with the managed block replaced by `shares` (or
/// appended if there is none yet). Bytes outside the markers are never
/// altered. Refuses a malformed marker layout rather than guessing.
pub fn render_with_shares(conf: &str, shares: &[SmbShare]) -> Result<String> {
    let block = render_block(shares);
    match locate_block(conf)? {
        Some(span) => Ok(format!("{}{}{}", &conf[..span.start], block, &conf[span.end..])),
        None => {
            let mut out = conf.to_string();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&block);
            Ok(out)
        }
    }
}

/// Lowercased `[stanza]` names found outside the managed block (so a new
/// managed share can't collide with `[alice]`, `[global]`, ...).
pub fn outside_block_stanza_names(conf: &str) -> Result<Vec<String>> {
    let outside = match locate_block(conf)? {
        Some(span) => format!("{}{}", &conf[..span.start], &conf[span.end..]),
        None => conf.to_string(),
    };
    Ok(outside
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('[') && l.ends_with(']'))
        .map(|l| l[1..l.len() - 1].trim().to_lowercase())
        .collect())
}

pub fn validate_share_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("Share name is required".to_string());
    }
    if name.chars().count() > 80 {
        return Err("Share name is too long (80 characters max)".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Share name may only contain letters, digits, '-' and '_'".to_string());
    }
    if ["global", "homes", "printers"].iter().any(|r| name.eq_ignore_ascii_case(r)) {
        return Err(format!("'{name}' is a reserved Samba section name"));
    }
    Ok(())
}

/// Conservative Unix-account charset. A leading `-` is rejected so a
/// username can never be parsed as an option by `smbpasswd`.
pub fn validate_user(user: &str) -> std::result::Result<(), String> {
    let mut chars = user.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || user.chars().count() > 32 || !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(format!("'{user}' is not a valid Unix username"));
    }
    Ok(())
}
```

- [ ] **Step 5: Run** `cargo test zfs::smb` — Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add src/zfs/smb.rs src/zfs/mod.rs src/error.rs && git commit -m "feat: parse and render zfsctl's managed smb4.conf block

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `smb.rs` I/O — read, set, remove, with rollback

**Files:** Modify `src/zfs/smb.rs`

**Interfaces:**
- Consumes: Task 1 (`run_with_stdin`, `FakeRunner::{route, route_failure, stdin_calls}`), Task 2 items.
- Produces: `read_conf(&dyn CommandRunner) -> Result<String>`; `list_managed(&dyn CommandRunner) -> Result<Vec<SmbShare>>`; `set_share(&dyn CommandRunner, &SmbShare, password: Option<&str>) -> Result<()>`; `remove_share(&dyn CommandRunner, dataset: &str) -> Result<()>`.

Order inside `set_share` (deliberately differs from the spec's data-flow, flagged to the user): validate → read → `smbpasswd` → write → reload (restore on reload failure). `smbpasswd` goes *first* so a nonexistent user fails before any config changes ("fail clearly rather than silently writing a stanza that can never authenticate"). `remove_share` does **not** run `smbpasswd -x`: that Samba password may also back a hand-written stanza such as `[carol]` outside the block, and deleting it would break a share this tool promises never to touch.

- [ ] **Step 1: Write failing tests** (append to `mod tests` in `smb.rs`; `FakeRunner` import: `use crate::exec::test_support::FakeRunner;`):

```rust
    fn calls(r: &FakeRunner) -> Vec<String> {
        r.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).collect()
    }
    fn tee_writes(r: &FakeRunner) -> Vec<String> {
        r.stdin_calls.lock().unwrap().iter().filter(|(c, _)| c == "tee").map(|(_, s)| s.clone()).collect()
    }
    fn runner_with(conf: &str) -> FakeRunner {
        FakeRunner::new("").route("cat", CONF_PATH, conf)
    }

    #[test]
    fn read_conf_treats_a_missing_file_as_empty_but_other_failures_as_errors() {
        let missing = FakeRunner::new("").route_failure("cat", CONF_PATH, "cat: /usr/local/etc/smb4.conf: No such file or directory", 1);
        assert_eq!(read_conf(&missing).unwrap(), "");
        let denied = FakeRunner::new("").route_failure("cat", CONF_PATH, "sudo: a password is required", 1);
        assert!(read_conf(&denied).is_err());
    }

    #[test]
    fn set_share_create_runs_cat_smbpasswd_tee_reload_in_that_order() {
        let r = runner_with(FB_CONF);
        set_share(&r, &share("media", "tank/media", Some("carol"), false), Some("p w$1")).unwrap();
        assert_eq!(calls(&r), vec!["cat", "smbpasswd", "tee", "service"]);
        let all = r.calls.lock().unwrap();
        assert_eq!(all[1].1, vec!["-a", "-s", "carol"]);
        assert_eq!(all[2].1, vec![CONF_PATH]);
        assert_eq!(all[3].1, vec!["samba_server", "reload"]);
        let stdin = r.stdin_calls.lock().unwrap();
        assert_eq!(stdin[0], ("smbpasswd".to_string(), "p w$1\np w$1\n".to_string()), "password verbatim, doubled, on stdin only");
        assert!(all.iter().all(|(_, args)| args.iter().all(|a| !a.contains("p w$1"))), "password must never appear in an argument");
        let written = &stdin[1].1;
        assert!(written.starts_with(FB_CONF), "bytes outside the block are preserved");
        assert!(written.contains("valid users = carol"));
    }

    #[test]
    fn set_share_without_a_user_or_without_a_password_skips_smbpasswd() {
        let r = runner_with(FB_CONF);
        set_share(&r, &share("pub", "tank/pub", None, true), Some("ignored")).unwrap();
        assert!(!calls(&r).contains(&"smbpasswd".to_string()));
        let r = runner_with(FB_CONF);
        set_share(&r, &share("media", "tank/media", Some("carol"), true), None).unwrap();
        assert!(!calls(&r).contains(&"smbpasswd".to_string()), "blank password on edit = keep the existing one");
    }

    #[test]
    fn set_share_bad_user_fails_before_the_config_is_touched() {
        let r = runner_with(FB_CONF).route_failure("smbpasswd", "-a", "Failed to add entry for user nosuch.", 1);
        let err = set_share(&r, &share("media", "tank/media", Some("nosuch"), false), Some("pw")).unwrap_err().to_string();
        assert!(err.contains("nosuch") && err.contains("not changed"), "{err}");
        assert!(tee_writes(&r).is_empty());
        assert!(!calls(&r).contains(&"service".to_string()));
    }

    #[test]
    fn set_share_edit_replaces_the_dataset_s_stanza_instead_of_adding_a_second_block() {
        let first = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let r = runner_with(&first);
        set_share(&r, &share("renamed", "tank/media", None, false), None).unwrap();
        let written = tee_writes(&r).remove(0);
        let parsed = parse_managed(&written).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "renamed");
        assert_eq!(written.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn set_share_refuses_names_that_collide_with_unmanaged_or_other_managed_stanzas() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("Alice", "tank/a", None, true), None).is_err());
        assert!(set_share(&r, &share("global", "tank/a", None, true), None).is_err());
        assert!(tee_writes(&r).is_empty());
        let first = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let r = runner_with(&first);
        assert!(set_share(&r, &share("MEDIA", "tank/other", None, true), None).is_err());
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn set_share_validates_its_inputs_even_if_the_ui_did_not() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("bad name", "tank/a", None, true), None).is_err());
        assert!(set_share(&r, &share("ok", "tank/a", Some("-evil"), true), Some("pw")).is_err());
        assert!(set_share(&r, &share("ok", "tank/a", Some("bob"), true), Some("a\nb")).is_err(), "newline would break the stdin protocol");
        assert!(calls(&r).iter().all(|c| c != "smbpasswd" && c != "tee"));
    }

    #[test]
    fn set_share_works_on_a_host_with_no_smb4_conf_yet() {
        let r = FakeRunner::new("").route_failure("cat", CONF_PATH, "cat: x: No such file or directory", 1);
        set_share(&r, &share("a", "tank/a", None, true), None).unwrap();
        assert!(tee_writes(&r)[0].starts_with(BLOCK_START));
    }

    #[test]
    fn set_share_write_failure_is_reported_and_skips_reload() {
        let r = runner_with(FB_CONF).route_failure("tee", CONF_PATH, "Permission denied", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("smb4.conf") && err.contains("Permission denied"), "{err}");
        assert!(!calls(&r).contains(&"service".to_string()));
    }

    #[test]
    fn set_share_reload_failure_restores_the_previous_file_and_says_both_things() {
        let r = runner_with(FB_CONF).route_failure("service", "samba_server", "smbd: invalid config", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("reload") && err.contains("restored"), "{err}");
        let writes = tee_writes(&r);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[1], FB_CONF, "the original bytes are written back");
    }

    #[test]
    fn remove_share_drops_only_that_dataset_and_never_runs_smbpasswd() {
        let both = render_with_shares(FB_CONF, &[share("a", "tank/a", Some("carol"), true), share("b", "tank/b", None, true)]).unwrap();
        let r = runner_with(&both);
        remove_share(&r, "tank/a").unwrap();
        let written = tee_writes(&r).remove(0);
        let left = parse_managed(&written).unwrap();
        assert_eq!(left.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["b"]);
        assert!(written.starts_with(FB_CONF));
        assert!(!calls(&r).contains(&"smbpasswd".to_string()));
        assert_eq!(calls(&r).last().unwrap(), "service");
    }

    #[test]
    fn remove_share_for_an_unknown_dataset_is_an_error_and_changes_nothing() {
        let r = runner_with(FB_CONF);
        assert!(remove_share(&r, "tank/none").is_err());
        assert!(tee_writes(&r).is_empty());
    }
```

- [ ] **Step 2: Run** `cargo test zfs::smb` — Expected: compile FAIL (functions missing).

- [ ] **Step 3: Implement** (add to `smb.rs`; add `use crate::exec::CommandRunner;` at the top):

```rust
pub fn read_conf(runner: &dyn CommandRunner) -> Result<String> {
    let out = runner.run("cat", &[CONF_PATH])?;
    if out.success() {
        return Ok(out.stdout);
    }
    if out.stderr.contains("No such file or directory") {
        return Ok(String::new());
    }
    Err(AppError::CommandFailed { cmd: format!("cat {CONF_PATH}"), status: out.status, stderr: out.stderr })
}

pub fn list_managed(runner: &dyn CommandRunner) -> Result<Vec<SmbShare>> {
    parse_managed(&read_conf(runner)?)
}

fn write_conf(runner: &dyn CommandRunner, content: &str) -> Result<()> {
    let out = runner.run_with_stdin("tee", &[CONF_PATH], content)?;
    if out.success() {
        Ok(())
    } else {
        Err(smb_err(format!("could not write {CONF_PATH}: {}", out.stderr.trim())))
    }
}

/// Reloads Samba; if that fails, puts `previous` back so the file on
/// disk never stays in a state the running daemon rejected.
fn reload_or_restore(runner: &dyn CommandRunner, previous: &str) -> Result<()> {
    let out = runner.run("service", &["samba_server", "reload"])?;
    if out.success() {
        return Ok(());
    }
    let reload_msg = out.stderr.trim().to_string();
    match write_conf(runner, previous) {
        Ok(()) => Err(smb_err(format!(
            "'service samba_server reload' failed ({reload_msg}); the previous smb4.conf has been restored"
        ))),
        Err(restore) => Err(smb_err(format!(
            "'service samba_server reload' failed ({reload_msg}) AND restoring the previous smb4.conf also failed ({restore}); fix {CONF_PATH} by hand"
        ))),
    }
}

pub fn set_share(runner: &dyn CommandRunner, share: &SmbShare, password: Option<&str>) -> Result<()> {
    validate_share_name(&share.name).map_err(smb_err)?;
    if let Some(user) = &share.valid_user {
        validate_user(user).map_err(smb_err)?;
    }
    if share.path.is_empty() || share.path.chars().any(char::is_control) {
        return Err(smb_err("share path is empty or contains control characters"));
    }
    if password.is_some_and(|p| p.contains(['\n', '\r'])) {
        return Err(smb_err("password must not contain a line break"));
    }

    let original = read_conf(runner)?;
    let mut shares = parse_managed(&original)?;
    if outside_block_stanza_names(&original)?.contains(&share.name.to_lowercase()) {
        return Err(smb_err(format!("a share named '{}' already exists in smb4.conf outside zfsctl's managed block", share.name)));
    }
    if shares.iter().any(|s| s.dataset != share.dataset && s.name.eq_ignore_ascii_case(&share.name)) {
        return Err(smb_err(format!("another managed share is already named '{}'", share.name)));
    }
    match shares.iter_mut().find(|s| s.dataset == share.dataset) {
        Some(slot) => *slot = share.clone(),
        None => shares.push(share.clone()),
    }
    let updated = render_with_shares(&original, &shares)?;

    if let (Some(user), Some(pw)) = (&share.valid_user, password) {
        let out = runner.run_with_stdin("smbpasswd", &["-a", "-s", user], &format!("{pw}\n{pw}\n"))?;
        if !out.success() {
            return Err(smb_err(format!(
                "could not set the Samba password for '{user}' ({}); smb4.conf was not changed",
                out.stderr.trim()
            )));
        }
    }
    write_conf(runner, &updated)?;
    reload_or_restore(runner, &original)
}

pub fn remove_share(runner: &dyn CommandRunner, dataset: &str) -> Result<()> {
    let original = read_conf(runner)?;
    let mut shares = parse_managed(&original)?;
    let before = shares.len();
    shares.retain(|s| s.dataset != dataset);
    if shares.len() == before {
        return Err(smb_err(format!("no managed SMB share for dataset '{dataset}'")));
    }
    write_conf(runner, &render_with_shares(&original, &shares)?)?;
    reload_or_restore(runner, &original)
}
```

- [ ] **Step 4: Run** `cargo test zfs::smb` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/zfs/smb.rs && git commit -m "feat: set/remove managed SMB shares with smbpasswd, reload and rollback

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Retire `sharesmb`; source `Share::Smb` from the managed block

**Files:** Modify `src/zfs/share.rs`, `src/ui/layout.rs`, `src/ui/state.rs` (Shares worker handler + existing tests), `src/ui/hints.rs` (test fixture)

**Interfaces:**
- Consumes: `smb::{list_managed, SmbShare}`.
- Produces: `Share::Smb { dataset: String, name: String, path: String, valid_user: Option<String>, read_only: bool }`; `SharesSnapshot { shares, nfs_available, samba_available, smb_error: Option<String> }`.

- [ ] **Step 1: Update/write tests in `src/zfs/share.rs`.**
  - In `SHARES_FIXTURE` delete every `...sharesmb...` line.
  - Replace `parse_shares_groups_by_dataset_and_drops_fully_unshared_datasets` with: expected vec has `tank/both` (Nfs `on`), `tank/nfsonly`, `tank/weird` only, as `Share::Nfs`; `tank/data` (off) and `tank/data@snap1` (`-`) absent; and `tank/smbonly` (its `sharenfs` is `off`) absent. Same assertion message about `-alldirs`.
  - In `list_shares_uses_correct_command_and_parses_result` change the scripted stdout to `"tank/data\tsharenfs\ton\tlocal\n"` and the expected last arg to `"sharenfs"`.
  - Replace `list_shares_with_availability_composes_all_three_calls` and add two more:

```rust
    const MANAGED_CONF: &str = "[global]\n  x = y\n\n## zfsctl managed shares -- do not edit by hand -- start\n[media]\n    # zfsctl-dataset = tank/media\n    path = /tank/media\n    valid users = carol\n    browseable = yes\n    read only = no\n## zfsctl managed shares -- do not edit by hand -- end\n";

    #[test]
    fn list_shares_with_availability_merges_nfs_and_managed_smb_shares() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("cat", crate::zfs::smb::CONF_PATH, MANAGED_CONF);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.nfs_available, true);
        assert_eq!(snapshot.samba_available, true);
        assert_eq!(snapshot.smb_error, None);
        assert_eq!(snapshot.shares.len(), 2);
        assert_eq!(
            snapshot.shares[1],
            Share::Smb {
                dataset: "tank/media".to_string(),
                name: "media".to_string(),
                path: "/tank/media".to_string(),
                valid_user: Some("carol".to_string()),
                read_only: false,
            }
        );
    }

    #[test]
    fn smb_conf_is_not_read_when_samba_is_not_running() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "")
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is not running.\n")
            .route_failure("cat", crate::zfs::smb::CONF_PATH, "sudo: a password is required", 1);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.smb_error, None);
        assert!(!runner.calls.lock().unwrap().iter().any(|(c, _)| c == "cat"));
    }

    #[test]
    fn an_unreadable_smb_conf_leaves_nfs_shares_intact_and_reports_the_problem() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route_failure("cat", crate::zfs::smb::CONF_PATH, "sudo: a password is required", 1);
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.shares.len(), 1);
        assert!(snapshot.smb_error.unwrap().contains("password is required"));
    }
```

- [ ] **Step 2: Run** `cargo test zfs::share` — Expected: compile FAIL.

- [ ] **Step 3: Implement in `share.rs`.** Change the enum variant and snapshot:

```rust
    Smb {
        dataset: String,
        name: String,
        path: String,
        valid_user: Option<String>,
        read_only: bool,
    },
```

```rust
pub struct SharesSnapshot {
    pub shares: Vec<Share>,
    pub nfs_available: bool,
    pub samba_available: bool,
    /// Set when the managed SMB block could not be read; NFS shares are
    /// still returned so one broken half never hides the other.
    pub smb_error: Option<String>,
}
```

In `parse_shares` drop the `sharesmb` tuple slot and the `Share::Smb` push (keep only `sharenfs`; the `by_dataset` map can become `BTreeMap<String, String>`). Update its doc comment (SMB is no longer derived from `zfs get`). In `list_shares` use the property list `"sharenfs"` and the display string `"zfs get sharenfs"`. Replace `list_shares_with_availability`:

```rust
pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot> {
    let mut shares = list_shares(runner)?;
    let nfs_available = nfs_server_available(runner)?;
    let samba_available = samba_available(runner)?;
    let mut smb_error = None;
    // Only read smb4.conf when Samba is up: a host without Samba (or with a
    // sudoers file that predates SMB support) must not report an error on
    // every refresh.
    if samba_available {
        match super::smb::list_managed(runner) {
            Ok(managed) => shares.extend(managed.into_iter().map(|s| Share::Smb {
                dataset: s.dataset,
                name: s.name,
                path: s.path,
                valid_user: s.valid_user,
                read_only: s.read_only,
            })),
            Err(e) => smb_error = Some(e.to_string()),
        }
    }
    Ok(SharesSnapshot { shares, nfs_available, samba_available, smb_error })
}
```

- [ ] **Step 4: Fix the UI compile fallout.**
  - `layout.rs` `draw_shares`: replace the `Share::Smb` row with
    `Share::Smb { dataset, name, valid_user, read_only, .. } => Row::new(vec![dataset.clone(), "SMB".to_string(), format!("\\\\{name}  user={}  {}", valid_user.as_deref().unwrap_or("(guest)"), if *read_only { "ro" } else { "rw" })])`; and add `if app.samba_available == Some(false) { hints.push("SMB unavailable: smbd not running"); }` next to the NFS hint.
  - `state.rs` Shares handler: after `self.status_message = None;` add `if let Some(e) = snapshot.smb_error { self.set_error(format!("SMB shares unavailable: {e}")); }` (move `snapshot.shares`/availability reads above it so `snapshot` isn't partially moved — destructure the struct first).
  - `state.rs` (temporary, finished in Task 5): in the Shares `Enter` arm keep an `Smb { .. }` arm returning the existing "not supported" status for now; in `'d'` change the `Smb` arm key to a placeholder that still compiles (`(dataset.clone(), "sharesmb", "SMB")`) — both are replaced in Task 5. In `share_edit_modal` the `Smb { .. }` arm stays `unreachable!`.
  - Update every test fixture that builds `Share::Smb { dataset, name: ... }` (`state.rs` ~4793, 4806, 4856, 4896; `hints.rs` `shares()` fixture) to the new shape via a helper added to each test module:

```rust
    fn smb(dataset: &str) -> crate::zfs::share::Share {
        crate::zfs::share::Share::Smb {
            dataset: dataset.to_string(),
            name: "media".to_string(),
            path: format!("/{dataset}"),
            valid_user: Some("carol".to_string()),
            read_only: false,
        }
    }
```
  and add `smb_error: None` to the `SharesSnapshot { .. }` literal in `shares_result_for_a_stale_host_is_discarded`. In `refresh_shares_populates_app_state_from_fake_runner` change the `zfs get` route stdout to `"tank/data\tsharenfs\ton\tlocal\n"`. Temporarily mark `enter_on_smb_share_row_sets_hint_instead_of_opening_a_form` and `handle_key_d_on_smb_share_row_opens_remove_confirm_modal` as using `smb("tank/data")`; both are rewritten in Task 5.

- [ ] **Step 5: Run** `cargo test` — Expected: all PASS (475 + new).

- [ ] **Step 6: Commit**

```bash
git add -A src && git commit -m "refactor: source SMB shares from the managed smb4.conf block, retire sharesmb

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: UI — `'M'` form, Shares `Enter`/`d`, hints

**Files:** Modify `src/ui/modal.rs`, `src/ui/state.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`

**Interfaces:**
- Consumes: `smb::{SmbShare, validate_share_name, validate_user}`, `Share::Smb { .. }`.
- Produces: `Secret(pub String)` (Debug prints `Secret(***)`); `MutationCall::SetSmbShare { share: SmbShare, password: Option<Secret> }`, `MutationCall::RemoveSmbShare { dataset: String }`; `FormKind::ShareSmb { host: usize, dataset: String, path: String, existing_user: Option<String> }` with fields `[Share name, Valid user, Password, Read-only (y/n)]` and `FormKind::masks_field(&self, index: usize) -> bool`; `fn smb_edit_modal(existing: Option<&Share>, dataset: &str, path: &str, host: usize) -> Modal`; `fn build_smb_action(kind: &FormKind, fields: &[FormField], shares: &[Share]) -> Result<PendingAction, String>` (state.rs, private).

- [ ] **Step 1: Write failing tests.**

`modal.rs` tests:

```rust
    #[test]
    fn secret_never_appears_in_debug_output() {
        let call = MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare { name: "a".into(), dataset: "tank/a".into(), path: "/tank/a".into(), valid_user: Some("bob".into()), read_only: false },
            password: Some(Secret("hunter2".to_string())),
        };
        assert!(!format!("{call:?}").contains("hunter2"));
    }

    #[test]
    fn smb_calls_run_the_library_and_removal_is_warn_toned() {
        use crate::exec::test_support::FakeRunner;
        let runner = FakeRunner::new("");
        MutationCall::RemoveSmbShare { dataset: "tank/none".into() }.run(&runner).unwrap_err(); // nothing managed there
        assert_eq!(MutationCall::RemoveSmbShare { dataset: "x".into() }.tone(), Tone::Warn);
    }

    #[test]
    fn only_the_smb_password_field_is_masked() {
        let kind = FormKind::ShareSmb { host: 0, dataset: "d".into(), path: "/d".into(), existing_user: None };
        assert!(kind.masks_field(2));
        assert!(!kind.masks_field(0) && !kind.masks_field(1) && !kind.masks_field(3));
        assert!(!FormKind::CreatePool { host: 0 }.masks_field(2));
    }
```

`state.rs` tests (replace the two temporary Smb tests; add the rest; `smb()` helper from Task 4; helper `fn smb_form(app: &App) -> (&Vec<FormField>, &FormKind)` unwraps `Modal::Form`):

```rust
    fn datasets_app_with_mount(mountpoint: Option<&str>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/media".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string) }];
        app.datasets_table.select(Some(0));
        app.shares_loaded = true;
        app.samba_available = Some(true);
        app
    }

    #[test]
    fn shift_m_on_datasets_opens_a_blank_smb_form_for_an_unshared_dataset() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.handle_key(KeyCode::Char('M'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/media".into(), path: "/tank/media".into(), existing_user: None });
                assert_eq!(fields.len(), 4);
                assert_eq!(fields[0].value, "media", "suggests the dataset's last path component");
                assert_eq!(fields[1].value, "");
            }
            other => panic!("expected Form, got {other:?}"),
        }
    }

    #[test]
    fn shift_m_prefills_from_the_existing_managed_share() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")];
        app.handle_key(KeyCode::Char('M'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields[0].value, "media");
                assert_eq!(fields[1].value, "carol");
                assert_eq!(fields[2].value, "", "the password is never pre-filled");
                assert_eq!(fields[3].value, "n");
                assert!(matches!(kind, FormKind::ShareSmb { existing_user: Some(u), .. } if u == "carol"));
            }
            other => panic!("expected Form, got {other:?}"),
        }
    }

    #[test]
    fn shift_m_is_refused_without_a_mountpoint_before_shares_load_or_when_smbd_is_down() {
        let mut app = datasets_app_with_mount(None);
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none() && app.status_message.is_some());
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares_loaded = false;
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none() && app.status_message.is_some());
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.samba_available = Some(false);
        app.handle_key(KeyCode::Char('M'));
        assert!(app.modal.is_none() && app.status_message.is_some());
    }

    fn type_into_smb_form(app: &mut App, values: [&str; 4]) {
        for v in values {
            for c in v.chars() {
                app.handle_key(KeyCode::Char(c));
            }
            app.handle_key(KeyCode::Enter); // advances; the 4th Enter submits
        }
    }

    fn open_blank_smb_form() -> App {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.handle_key(KeyCode::Char('M'));
        // clear the suggested name so each test types its own
        for _ in 0..5 {
            app.handle_key(KeyCode::Backspace);
        }
        app
    }

    #[test]
    fn submitting_a_valid_smb_form_builds_a_set_smb_share_action() {
        let mut app = open_blank_smb_form();
        type_into_smb_form(&mut app, ["media", "carol", " pw 1 ", "y"]);
        // the action is returned from handle_key's modal path; capture via pending_action()
        let action = app.take_submitted_action_for_test().expect("submitted");
        assert_eq!(
            action.call,
            MutationCall::SetSmbShare {
                share: crate::zfs::smb::SmbShare { name: "media".into(), dataset: "tank/media".into(), path: "/tank/media".into(), valid_user: Some("carol".into()), read_only: true },
                password: Some(Secret(" pw 1 ".to_string())),
            },
            "the password is kept verbatim, not trimmed"
        );
        assert_eq!(action.section, Section::Shares);
    }

    #[test]
    fn smb_form_validation_errors_keep_the_form_open_with_a_specific_message() {
        let cases: [([&str; 4], &str); 5] = [
            (["", "", "", "n"], "required"),
            (["bad name", "", "", "n"], "letters, digits"),
            (["media", "-evil", "pw", "n"], "valid Unix username"),
            (["media", "carol", "", "n"], "Samba password is required"),
            (["alice", "", "", "n"], "already"),
        ];
        for (values, expect) in cases {
            let mut app = open_blank_smb_form();
            app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/other".into(), name: "alice".into(), path: "/x".into(), valid_user: None, read_only: true }];
            type_into_smb_form(&mut app, values);
            assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?} must keep the form open");
            let msg = app.status_message.clone().unwrap_or_default();
            assert!(msg.contains(expect), "{values:?}: '{msg}' should contain '{expect}'");
        }
    }

    #[test]
    fn editing_with_a_blank_password_is_allowed_only_when_the_user_is_unchanged() {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")]; // existing user: carol
        app.handle_key(KeyCode::Char('M'));
        for _ in 0..3 {
            app.handle_key(KeyCode::Enter);
        }
        app.handle_key(KeyCode::Enter); // submit unchanged
        let action = app.take_submitted_action_for_test().expect("unchanged user + blank password is fine");
        assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, .. }));

        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.shares = vec![smb("tank/media")];
        app.handle_key(KeyCode::Char('M'));
        app.handle_key(KeyCode::Enter); // to Valid user
        for _ in 0..5 {
            app.handle_key(KeyCode::Backspace);
        }
        for c in "bob".chars() {
            app.handle_key(KeyCode::Char(c));
        }
        for _ in 0..3 {
            app.handle_key(KeyCode::Enter);
        }
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "changing the user requires a password");
    }

    #[test]
    fn a_blank_user_ignores_any_typed_password() {
        let mut app = open_blank_smb_form();
        type_into_smb_form(&mut app, ["pub", "", "ignored", "n"]);
        let action = app.take_submitted_action_for_test().unwrap();
        assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.valid_user.is_none()));
    }

    #[test]
    fn enter_on_an_smb_share_row_opens_its_prefilled_form_using_the_stanza_path() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        } // -> Shares
        app.shares = vec![smb("tank/data")];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Enter);
        assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::ShareSmb { path, .. }, .. }) if path == "/tank/data"));
    }

    #[test]
    fn handle_key_d_on_smb_share_row_opens_a_remove_confirm_for_the_managed_share() {
        let mut app = App::new(vec!["local".to_string()]);
        for _ in 0..5 {
            app.handle_key(KeyCode::Tab);
        }
        app.shares = vec![smb("tank/data")];
        app.shares_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => assert_eq!(action.call, MutationCall::RemoveSmbShare { dataset: "tank/data".to_string() }),
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
```

If no accessor like `take_submitted_action_for_test` exists, look at how existing form-submit tests (e.g. around `FormKind::ShareNfs` at `state.rs` ~4965/5956) obtain the `PendingAction`, and use that same mechanism instead of inventing one — `handle_modal_key` returns `Option<PendingAction>`; the helper is just a thin wrapper over whatever those tests call.

`hints.rs`: add `'M'` to the `probes` array in `keys_that_are_not_advertised_are_no_ops`; add a content test:

```rust
    #[test]
    fn datasets_middle_advertises_smb_share_only_when_it_can_work() {
        let mut a = datasets_middle(true);
        a.samba_available = Some(true);
        assert!(keys(&a).contains(&"M"));
        a.samba_available = Some(false);
        assert!(!keys(&a).contains(&"M"));
        let mut b = datasets_middle(false);
        b.samba_available = Some(true);
        assert!(!keys(&b).contains(&"M"), "not before shares have loaded");
    }
```

- [ ] **Step 2: Run** `cargo test ui::` — Expected: compile FAIL.

- [ ] **Step 3: Implement `modal.rs`.** Add `Secret`; the two `MutationCall` variants; `tone()` (`SetSmbShare` → Neutral, `RemoveSmbShare` → Warn); `label()` (`"SET SMB SHARE"`, `"REMOVE SMB SHARE"`); `run()`:

```rust
            MutationCall::SetSmbShare { share, password } => {
                crate::zfs::smb::set_share(runner, share, password.as_ref().map(|p| p.0.as_str()))
            }
            MutationCall::RemoveSmbShare { dataset } => crate::zfs::smb::remove_share(runner, dataset),
```

```rust
/// A password held only until it is piped to `smbpasswd`. `Debug` never
/// prints it, so a logged or panicking `PendingAction` can't leak it.
#[derive(Clone, PartialEq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}
```

`FormKind::ShareSmb` (doc: `Fields: [name, valid_user, password, read_only]`) plus

```rust
impl FormKind {
    /// Whether the form field at `index` must be shown as `*` characters.
    pub fn masks_field(&self, index: usize) -> bool {
        matches!(self, FormKind::ShareSmb { .. }) && index == 2
    }
}
```

Fix any other exhaustive `match` on `MutationCall`/`FormKind` the compiler reports (e.g. in `layout.rs`).

- [ ] **Step 4: Implement `layout.rs` masking.** In `draw_modal`'s `Modal::Form` arm bind `kind` and render

```rust
let shown = if kind.masks_field(i) { "*".repeat(field.value.chars().count()) } else { field.value.clone() };
lines.push(format!("{marker} {}: {shown}", field.label));
```

- [ ] **Step 5: Implement `state.rs`.**

`smb_edit_modal` (and route `share_edit_modal`'s `Smb` arm to it, deleting the `unreachable!` and updating the doc comment to say SMB is now supported):

```rust
fn smb_edit_modal(existing: Option<&crate::zfs::share::Share>, dataset: &str, path: &str, host: usize) -> Modal {
    let (name, user, read_only, existing_user) = match existing {
        Some(crate::zfs::share::Share::Smb { name, valid_user, read_only, .. }) => {
            (name.clone(), valid_user.clone().unwrap_or_default(), *read_only, valid_user.clone())
        }
        _ => {
            let suggestion = dataset.rsplit('/').next().unwrap_or("").to_string();
            let suggestion = if crate::zfs::smb::validate_share_name(&suggestion).is_ok() { suggestion } else { String::new() };
            (suggestion, String::new(), false, None)
        }
    };
    Modal::Form {
        title: format!("SMB share: '{dataset}'"),
        fields: vec![
            FormField { label: "Share name".to_string(), value: name },
            FormField { label: "Valid user (blank = guest)".to_string(), value: user },
            FormField { label: "Password (blank on edit = unchanged)".to_string(), value: String::new() },
            FormField { label: "Read-only (y/n)".to_string(), value: if read_only { "y".to_string() } else { "n".to_string() } },
        ],
        focused: 0,
        description: Some(format!("Shares {path} as \\\\<host>\\<name>. The Valid user must already be a Unix account; the password is its separate Samba password.")),
        kind: FormKind::ShareSmb { host, dataset: dataset.to_string(), path: path.to_string(), existing_user },
    }
}
```

`build_smb_action`:

```rust
fn build_smb_action(kind: &FormKind, fields: &[FormField], shares: &[crate::zfs::share::Share]) -> std::result::Result<PendingAction, String> {
    let FormKind::ShareSmb { host, dataset, path, existing_user } = kind else {
        return Err("not an SMB form".to_string());
    };
    let name = fields[0].value.trim();
    crate::zfs::smb::validate_share_name(name)?;
    let user = fields[1].value.trim();
    if !user.is_empty() {
        crate::zfs::smb::validate_user(user)?;
    }
    let password = &fields[2].value; // verbatim: spaces can be part of a password
    let read_only = fields[3].value.trim().to_lowercase().starts_with('y');
    let clash = shares.iter().any(|s| matches!(s, crate::zfs::share::Share::Smb { dataset: d, name: n, .. } if d != dataset && n.eq_ignore_ascii_case(name)));
    if clash {
        return Err(format!("A share named '{name}' already exists on another dataset"));
    }
    let password = if user.is_empty() {
        None
    } else if password.is_empty() {
        if existing_user.as_deref() == Some(user) {
            None
        } else {
            return Err("A Samba password is required when setting or changing the valid user".to_string());
        }
    } else {
        Some(crate::ui::modal::Secret(password.clone()))
    };
    Ok(PendingAction {
        host: *host,
        section: Section::Shares,
        call: MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare {
                name: name.to_string(),
                dataset: dataset.clone(),
                path: path.clone(),
                valid_user: if user.is_empty() { None } else { Some(user.to_string()) },
                read_only,
            },
            password,
        },
    })
}
```

In `handle_modal_key`'s `Modal::Form` `Enter` arm, insert between the `AddHost` branch and the final `else`:

```rust
                    } else if matches!(kind, FormKind::ShareSmb { .. }) {
                        match build_smb_action(&kind, &fields, &self.shares) {
                            Ok(action) => Some(action),
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
```

Add to `build_pending_action`: `FormKind::ShareSmb { .. } => None,` with a comment that `handle_modal_key` routes it through `build_smb_action` (needs the current share list for the duplicate-name check).

The `'M'` handler, placed right after the `'N'` handler:

```rust
            KeyCode::Char('M') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if !self.shares_loaded {
                    self.status_message = Some("Press 'r' in the Shares section at least once (to load current share state) before creating/editing a share here".to_string());
                } else if self.samba_available == Some(false) {
                    self.status_message = Some("Samba (smbd) is not running on this host".to_string());
                } else if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        match &ds.mountpoint {
                            None => self.status_message = Some(format!("'{}' has no mounted mountpoint to share", ds.name)),
                            Some(path) => {
                                let existing = self.shares.iter().find(|s| matches!(s, crate::zfs::share::Share::Smb { dataset, .. } if dataset == &ds.name));
                                self.modal = Some(smb_edit_modal(existing, &ds.name, path, self.selected_host));
                            }
                        }
                    }
                }
            }
```

Shares `Enter`: merge the two arms (`Nfs`/`Smb` both → `share_edit_modal(share, self.selected_host)`); `share_edit_modal`'s `Smb { dataset, path, .. }` arm → `smb_edit_modal(Some(share), dataset, path, host)`. Shares `'d'`: replace the `(dataset, key, protocol)` tuple match with:

```rust
                        let (prompt, call) = match share {
                            crate::zfs::share::Share::Nfs { dataset, .. } => (
                                format!("Remove NFS share for '{dataset}'? (y/n)"),
                                MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value: "off".to_string() },
                            ),
                            crate::zfs::share::Share::Smb { dataset, name, .. } => (
                                format!("Remove SMB share '{name}' for '{dataset}'? Connected clients lose access. (y/n)"),
                                MutationCall::RemoveSmbShare { dataset: dataset.clone() },
                            ),
                        };
                        self.modal = Some(Modal::Confirm { prompt, action: PendingAction { host: self.selected_host, section: Section::Shares, call } });
```

- [ ] **Step 6: Implement `hints.rs`.** In `(Section::Datasets, false)` after the `N` hint:

```rust
                if app.shares_loaded && app.samba_available != Some(false) && app.selected_dataset_mountpoint().is_some() {
                    v.push(h("M", "SMB share", 3));
                }
```

- [ ] **Step 7: Run** `cargo test` and `cargo clippy --all-targets` — Expected: all PASS, no new warnings. The drift guards must pass in both directions; if `'M'` is advertised in a scenario where pressing it changes nothing, fix the *condition*, not the guard.

- [ ] **Step 8: Commit**

```bash
git add -A src && git commit -m "feat: 'M' creates/edits managed SMB shares; Shares Enter/d use them

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Docs, sudoers, and live end-to-end verification

**Files:** Modify `README.md`

- [ ] **Step 1: README.** Document `'M'`, the managed block (verbatim markers, "never edit inside it; everything outside is never touched"), that a Valid user must be an existing Unix account and gets a Samba password via `smbpasswd -a -s`, that removing a share leaves the Samba password in place (it may also back a hand-written share), and one-user-per-share. In the sudoers section extend the command list from six to nine entries — **path-pinned** for the two file commands:

```
fbsd ALL=(root) NOPASSWD: /sbin/zpool, /sbin/zfs, /bin/getfacl, /bin/setfacl, /sbin/geom, /usr/sbin/service, /usr/local/bin/smbpasswd, /bin/cat /usr/local/etc/smb4.conf, /usr/bin/tee /usr/local/etc/smb4.conf
```

and explain why not bare `cat`/`tee`/`sh` (unpinned `sudo cat` reads any file as root; `sudo sh` is unrestricted root). Update the "six binaries" sentence — derive the count from `grep -rn 'run(\|run_checked(\|run_with_stdin(' src` rather than trusting this plan (project rule: exhaustive lists come from the code).

- [ ] **Step 2: Full verification.** `cargo test && cargo clippy --all-targets && cargo build --release` — all green.

- [ ] **Step 3: Commit docs**

```bash
git add README.md && git commit -m "docs: document SMB shares and the extended sudoers entry

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 4: Live check on the hub** (`fb@172.16.10.90`, `NOPASSWD: ALL`; deploy with `scp -r src Cargo.toml Cargo.lock README.md fb@172.16.10.90:zfsctl/` then `cargo build --release` there — no `rsync`, no `. ~/.cargo/env`). Before starting: `ssh fb@172.16.10.90 'sudo cp /usr/local/etc/smb4.conf /tmp/smb4.conf.before'`. In the TUI, with a scratch dataset mounted and a disposable Unix user:
  1. Shares → `r`, then Datasets → `M`: create `scratch` with the user and a password. `smbclient -L localhost -U <user>%<pw>` lists it and `smbclient //localhost/scratch -U <user>%<pw> -c ls` connects (**the end-to-end proof; a clean config round-trip alone proves nothing**).
  2. `sudo diff` the file against `/tmp/smb4.conf.before`: only the managed block was added; `[alice]`/`[carol]` byte-identical.
  3. `M` again: edit read-only → `y`, blank password; confirm `smbclient` write now fails. Change user → password required.
  4. Nonexistent user → clear error, file unchanged. Name `alice` → refused.
  5. Break the config deliberately (e.g. edit the stanza by hand inside the block to add `bogus line`) → Shares shows the SMB error and NFS rows remain; restore.
  6. Shares → `d` removes it; block empty, `[alice]`/`[carol]` untouched, share no longer listed by `smbclient -L`.
  7. Hint bar shows `M SMB share` in Datasets only when it works; `Enter`/`d` in Shares behave as advertised.

- [ ] **Step 5: Live check under narrow sudoers.** On each managed VM (`fbsd16`, `fbsd16t1`, `fbsd16t2`; targets in the `zfsctl_freebsd_test_box` memory) add the extended sudoers entry via `visudo`, install Samba if absent (`pkg install samba423`, enable `samba_server`), then repeat step 1's create/remove through the TUI against that VM. Before updating one VM's sudoers, open Shares on it once to confirm the SMB error surfaces while NFS rows still render (Review Focus 6).

- [ ] **Step 6: Record discrepancies as failing tests first, fix, re-run Step 2.** Then hand the branch back for review/merge.

---

## Self-Review (done while writing)

**Spec coverage**
- Managed-block parse/write, markers, append-if-absent, outside bytes untouched → Tasks 2, 3.
- Reload after change (`service samba_server reload`; supported subcommand and `configtest` precmd confirmed in `/usr/local/etc/rc.d/samba_server` on the hub) → Task 3, live-checked Task 6.
- Password via `smbpasswd -a -s`, create-requires / edit-blank-keeps, blank user ignores password → Tasks 3, 5.
- `'M'` form with the four fields, 1:1 dataset model, pre-fill, name charset validation → Task 5.
- Shares rows from the managed block, `Enter` edit / `d` remove → Tasks 4, 5.
- Retire `sharesmb`/old `Share::Smb` → Task 4.
- `run_with_stdin` gap, `tee`/`cat` path-pinned sudoers instead of `sh -c`, three new sudoers entries → Tasks 1, 3, 6.
- Spec Review-focus list: block find/replace exactness (Task 2), nonexistent user fails clearly (Task 3), orphaned password decision (below), reload-vs-write distinct reporting (Task 3), removal leaves others untouched (Task 3).

**Deviations from the spec and from the earlier session notes (flag to the user at handoff)**
1. **`smbpasswd` runs before the config write**, not after, so a bad user fails before anything changes.
2. **Removing a share does NOT run `smbpasswd -x`** (this reverses a note from the earlier planning session). The same user may own a hand-written share outside the block (`fb`'s `[carol]` has `valid users = carol`); deleting that Samba password would silently break a share this feature promises never to touch. Blanking "Valid user" on edit likewise leaves the old password. Easy to add later with a "no other stanza references this user" check.
3. Each managed stanza carries a `# zfsctl-dataset = <dataset>` comment — the spec says the dataset is the identity but stanzas only have `path`, and a mountpoint can change.
4. Anything unrecognised *inside* the managed block is a hard error (surfaced in the status line) instead of being skipped, because the next write would drop it.
5. SMB rows are read only when `smbd` is running, so hosts without Samba (or with an old sudoers) don't error on every refresh.
6. Reload failure restores the previous `smb4.conf` (not in the spec).
7. `FormKind::ShareSmb` carries `existing_user`: a blank password on edit is accepted only if the user is unchanged — otherwise a newly named user would have no Samba password and the share would be unconnectable.

**Placeholder scan:** none; the one conditional instruction (reusing the existing form-submit test mechanism in Task 5 Step 1) names exactly where to look.

**Type consistency:** `SmbShare{name,dataset,path,valid_user,read_only}`, `Share::Smb{dataset,name,path,valid_user,read_only}`, `MutationCall::{SetSmbShare{share,password},RemoveSmbShare{dataset}}`, `FormKind::ShareSmb{host,dataset,path,existing_user}`, `set_share(runner,&SmbShare,Option<&str>)`, `remove_share(runner,&str)`, `SharesSnapshot.smb_error` are named identically in every task.
