# Boot Environments (bectl) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A "Boot Envs" section that lists, creates, activates (permanently or next-boot-only), renames, mounts/unmounts and destroys FreeBSD boot environments on any managed host via `bectl`.

**Architecture:** New `src/zfs/bootenv.rs` (pure parsing + argv-only `bectl` calls through `CommandRunner`, same split as `smb.rs`). Seven new `MutationCall` variants run through the existing confirm/form/`dispatch_mutation` machinery. A new `Section::BootEnvs` follows the Shares pattern (worker-thread refresh keyed by host, quiet auto-refresh, stale-host discard, `pin_selection`). What each key may do is decided once, by `BootEnv::can_*` predicates, shared by the key handlers and the hint bar so they cannot drift.

**Tech Stack:** Rust, ratatui, crossterm `KeyCode`, `FakeRunner` test double. Baseline before this plan: `cargo test` = 977 passed; `cargo clippy` = 1 pre-existing warning (do not add new ones).

**Spec:** `docs/superpowers/specs/2026-10-07-boot-environments-design.md`

## Global Constraints

- Every `bectl` call is argv-only through `CommandRunner` — never `sh -c` (remote hosts allow only listed binaries under `sudo -n`).
- Boot environment names are validated before they reach argv: non-empty, must not start with `-`, only ASCII letters/digits and `_ - . :`.
- Only `bectl list -H` is parsed. `list -a` is never used.
- A malformed `bectl list -H` row fails the whole fetch (never silently skipped).
- The periodic refresh must not be gated on already-detected state.
- Lints are `pedantic` plus `unwrap_used`/`indexing_slicing` warnings: no `unwrap()`/indexing in non-test code.
- Commit messages end with: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- Work on branch `boot-environments`; default branch is `master`, merge with `--no-ff`, there is no remote.

## Deviations from the spec (found while reading the code; Task 5 amends the spec)

1. **Rename is `R`, not `r`** — `r` is the global refresh key.
2. **Mount path** is shown in the table's Mountpoint column after the re-list, not in the status bar: `MutationCall::run` returns `Result<()>`, and `bectl list -H` already reports `/tmp/be_mount.XXXX`.
3. **Destroy uses the typed confirm** (type the name, like snapshots), with `d` = destroy and `D` = destroy and remove the origin snapshot (`bectl destroy -o`).
4. **"d first says it will unmount"** is dropped: nothing verified what bectl does for a mounted BE, so bectl's own error is shown verbatim and Task 6 records the real behavior in the README.

## Review Focus

The spec implies these inputs but no happy-path test covers them; each has a test in the named task.

1. A BE name beginning with `-` (argument injection into `bectl`) — Task 1 (I/O refuses) and Task 4 (form refuses).
2. `bectl` missing or failing on a host, then working again — Task 3 (section shows Unavailable, then recovers on the next refresh without being gated).
3. A BE that is `R` (activated for the next boot) but not `N`, and one that is `T` — Task 1 predicates and Task 4 hint/key agreement (`a`, `t`, `T`, `d` must advertise exactly what they do).
4. Zero BEs, or a refresh that removes the selected row — Task 3 (`pin_selection` clamps; empty is not an error).
5. A terminal too narrow for all columns — Task 3 (Mountpoint then Created drop; Name/Active/Space stay).

## File Structure

- Create `src/zfs/bootenv.rs` — types, `parse_bootenvs`, `validate_name`, predicates, the eight `bectl` calls.
- Modify `src/zfs/mod.rs` — `pub mod bootenv;`.
- Modify `src/ui/modal.rs` — `MutationCall` variants (tone/label/run), `FormKind::{CreateBootEnv, RenameBootEnv}`.
- Modify `src/ui/state.rs` — `Section::BootEnvs`, `WorkerMsg::BootEnvs`, `App` fields, refresh, drain, host-switch clear, key handlers, `build_bootenv_action`, help text.
- Modify `src/ui/mod.rs` — two refresh dispatch sites.
- Modify `src/ui/layout.rs` — sidebar label, `draw_main` arm, `draw_bootenvs`.
- Modify `src/ui/hints.rs` — hints arm, scenarios, drift-guard probes.
- Modify `src/ui/picker.rs` — F2 for the clone-from field.
- Modify `README.md` — section docs, keys, sudoers.
- Modify the spec — the four deviations above.

---

### Task 1: The `bootenv` module

**Files:**
- Create: `src/zfs/bootenv.rs`
- Modify: `src/zfs/mod.rs:1-15` (add `pub mod bootenv;` alphabetically, before `pub mod dataset;`)

**Interfaces:**
- Produces (later tasks rely on these exact names):
  - `struct ActiveFlags { pub now: bool, pub reboot: bool, pub temp: bool }` (Copy, Default) with `fn label(self) -> String`
  - `struct BootEnv { pub name: String, pub flags: ActiveFlags, pub mountpoint: Option<String>, pub space: String, pub created: String }` with `can_activate`, `can_activate_temporarily`, `can_clear_temporary`, `can_destroy`, `can_mount`, `can_unmount` (all `&self -> bool`)
  - `fn parse_bootenvs(&str) -> Result<Vec<BootEnv>>`
  - `fn validate_name(&str) -> std::result::Result<(), String>`
  - `fn list(&dyn CommandRunner) -> Result<Vec<BootEnv>>`
  - `fn create(&dyn CommandRunner, name: &str, from: Option<&str>) -> Result<()>`
  - `fn activate(&dyn CommandRunner, name: &str, temporary: bool) -> Result<()>`
  - `fn clear_temporary(&dyn CommandRunner, name: &str) -> Result<()>`
  - `fn rename(&dyn CommandRunner, from: &str, to: &str) -> Result<()>`
  - `fn mount(&dyn CommandRunner, name: &str) -> Result<()>`
  - `fn unmount(&dyn CommandRunner, name: &str) -> Result<()>`
  - `fn destroy(&dyn CommandRunner, name: &str, origin: bool) -> Result<()>`

- [ ] **Step 1: Create the branch**

```bash
cd /home/me/dev/rust/zfsctl && git switch -c boot-environments
```

- [ ] **Step 2: Write the module with its tests first (tests will not compile until Step 3 — that is the failing state)**

Create `src/zfs/bootenv.rs` containing only the test module below, preceded by `use crate::error::{AppError, Result};` and `use crate::exec::CommandRunner;` (the tests use `super::*`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    /// Real `bectl list -H` rows captured on FreeBSD 15.1 (tab separated), plus a
    /// mounted BE and a temporarily-activated one as seen in the 2026-10-07 probe.
    const LIST: &str = "default\tNR\t/\t5.18G\t2026-07-24 18:48\nzit-probe1\t-\t-\t8K\t2026-10-07 19:26\nzit-probe2\tT\t/tmp/be_mount.US6e\t0\t2026-10-07 19:26\n";

    fn be(flags: &str) -> BootEnv {
        parse_bootenvs(&format!("x\t{flags}\t-\t1M\td\n")).unwrap().remove(0)
    }

    fn args(runner: &FakeRunner) -> Vec<String> {
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one command: {calls:?}");
        assert_eq!(calls[0].0, "bectl");
        calls[0].1.clone()
    }

    #[test]
    fn parses_the_real_list_output() {
        let envs = parse_bootenvs(LIST).unwrap();
        assert_eq!(envs.len(), 3);
        assert_eq!(envs[0].name, "default");
        assert_eq!(envs[0].flags, ActiveFlags { now: true, reboot: true, temp: false });
        assert_eq!(envs[0].mountpoint.as_deref(), Some("/"));
        assert_eq!(envs[0].space, "5.18G");
        assert_eq!(envs[0].created, "2026-07-24 18:48");
        assert_eq!(envs[1].flags, ActiveFlags::default());
        assert_eq!(envs[1].mountpoint, None);
        assert_eq!(envs[1].space, "8K");
        assert_eq!(envs[2].flags, ActiveFlags { now: false, reboot: false, temp: true });
        assert_eq!(envs[2].mountpoint.as_deref(), Some("/tmp/be_mount.US6e"));
        assert_eq!(envs[2].space, "0");
    }

    #[test]
    fn empty_output_is_no_boot_environments_not_an_error() {
        assert_eq!(parse_bootenvs("").unwrap(), vec![]);
        assert_eq!(parse_bootenvs("\n\n").unwrap(), vec![]);
    }

    #[test]
    fn a_malformed_row_fails_the_whole_parse() {
        assert!(parse_bootenvs("default\tNR\t/\t5.18G\n").is_err(), "four fields");
        assert!(parse_bootenvs("default\tNR\t/\t5.18G\td\textra\n").is_err(), "six fields");
        assert!(parse_bootenvs("default\tX\t/\t5.18G\td\n").is_err(), "unknown flag");
        assert!(parse_bootenvs("default\t\t/\t5.18G\td\n").is_err(), "empty flags");
        assert!(parse_bootenvs("\tNR\t/\t5.18G\td\n").is_err(), "empty name");
        assert!(parse_bootenvs("good\tNR\t/\t5G\td\nbad row\n").is_err(), "one bad row poisons the fetch");
    }

    #[test]
    fn flags_decode_to_labels() {
        assert_eq!(be("NR").flags.label(), "Now+Reboot");
        assert_eq!(be("N").flags.label(), "Now");
        assert_eq!(be("R").flags.label(), "Reboot");
        assert_eq!(be("T").flags.label(), "Temp");
        assert_eq!(be("-").flags.label(), "");
    }

    #[test]
    fn predicates_follow_the_flags_and_mount_state() {
        let running = parse_bootenvs("default\tNR\t/\t5G\td\n").unwrap().remove(0);
        assert!(!running.can_activate() && !running.can_destroy() && !running.can_mount() && !running.can_unmount());
        assert!(!running.can_activate_temporarily() && !running.can_clear_temporary());

        let pending = be("R"); // activated for the next boot, not running
        assert!(!pending.can_activate() && !pending.can_destroy());
        assert!(pending.can_activate_temporarily());

        let temp = be("T");
        assert!(temp.can_activate() && temp.can_destroy() && temp.can_clear_temporary());
        assert!(!temp.can_activate_temporarily());

        let spare = be("-");
        assert!(spare.can_activate() && spare.can_activate_temporarily() && spare.can_destroy());
        assert!(spare.can_mount() && !spare.can_unmount() && !spare.can_clear_temporary());

        let mounted = parse_bootenvs("m\t-\t/tmp/be_mount.X\t1M\td\n").unwrap().remove(0);
        assert!(mounted.can_unmount() && !mounted.can_mount());
    }

    #[test]
    fn names_are_validated() {
        for ok in ["default", "pre-upgrade_15.1", "be:1", "A9"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "--force", "a b", "a/b", "a@b", "a\tb", "naïve"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn list_runs_bectl_list_h() {
        let runner = FakeRunner::new(LIST);
        assert_eq!(list(&runner).unwrap().len(), 3);
        assert_eq!(args(&runner), vec!["list", "-H"]);
    }

    #[test]
    fn list_surfaces_a_failing_bectl() {
        assert!(list(&FakeRunner::failing("bectl: not found", 127)).is_err());
    }

    #[test]
    fn create_runs_with_and_without_a_clone_source() {
        let runner = FakeRunner::new("");
        create(&runner, "new", None).unwrap();
        assert_eq!(args(&runner), vec!["create", "new"]);
        let runner = FakeRunner::new("");
        create(&runner, "new", Some("old")).unwrap();
        assert_eq!(args(&runner), vec!["create", "-e", "old", "new"]);
    }

    #[test]
    fn activate_destroy_rename_mount_unmount_build_the_probed_argv() {
        let check = |f: &dyn Fn(&FakeRunner), want: &[&str]| {
            let runner = FakeRunner::new("");
            f(&runner);
            assert_eq!(args(&runner), want);
        };
        check(&|r| activate(r, "a", false).unwrap(), &["activate", "a"]);
        check(&|r| activate(r, "a", true).unwrap(), &["activate", "-t", "a"]);
        check(&|r| clear_temporary(r, "a").unwrap(), &["activate", "-T", "a"]);
        check(&|r| rename(r, "a", "b").unwrap(), &["rename", "a", "b"]);
        check(&|r| mount(r, "a").unwrap(), &["mount", "a"]);
        check(&|r| unmount(r, "a").unwrap(), &["umount", "a"]);
        check(&|r| destroy(r, "a", false).unwrap(), &["destroy", "a"]);
        check(&|r| destroy(r, "a", true).unwrap(), &["destroy", "-o", "a"]);
    }

    #[test]
    fn a_name_that_looks_like_an_option_never_reaches_bectl() {
        let runner = FakeRunner::new("");
        assert!(create(&runner, "-F", None).is_err());
        assert!(create(&runner, "ok", Some("-e")).is_err());
        assert!(activate(&runner, "-t", false).is_err());
        assert!(clear_temporary(&runner, "-t").is_err());
        assert!(rename(&runner, "ok", "-x").is_err());
        assert!(rename(&runner, "-x", "ok").is_err());
        assert!(mount(&runner, "-x").is_err());
        assert!(unmount(&runner, "-x").is_err());
        assert!(destroy(&runner, "-F", false).is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "nothing was run");
    }

    #[test]
    fn bectls_own_refusal_text_is_surfaced() {
        let runner = FakeRunner::failing("cannot destroy active boot environment", 5);
        let err = destroy(&runner, "default", false).unwrap_err().to_string();
        assert!(err.contains("cannot destroy active boot environment"), "{err}");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test bootenv 2>&1 | tail -15`
Expected: compile errors (`cannot find function parse_bootenvs` etc.).

- [ ] **Step 4: Write the implementation above the test module**

```rust
//! Boot environments via FreeBSD's `bectl`. Only `bectl list -H` is parsed (one
//! tab-separated row per BE: name, active flags, mountpoint, space, created); the
//! multi-line `list -a` form is never used. Every call is argv-only.

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

/// What `bectl list` reports in its flags column: `N` active now, `R` active on
/// reboot, `T` activated for the next boot only, `-` none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActiveFlags {
    pub now: bool,
    pub reboot: bool,
    pub temp: bool,
}

impl ActiveFlags {
    /// `Now+Reboot`, `Now`, `Reboot`, `Temp` (joined with `+` when several), or empty.
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.now {
            parts.push("Now");
        }
        if self.reboot {
            parts.push("Reboot");
        }
        if self.temp {
            parts.push("Temp");
        }
        parts.join("+")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEnv {
    pub name: String,
    pub flags: ActiveFlags,
    /// `None` when not mounted (`bectl` prints `-`).
    pub mountpoint: Option<String>,
    /// As bectl prints it (`5.18G`, `8K`, `0`).
    pub space: String,
    pub created: String,
}

impl BootEnv {
    /// Permanent activation is pointless when it is already the one used on reboot.
    pub fn can_activate(&self) -> bool {
        !self.flags.reboot
    }

    /// A next-boot-only activation of the running or already-temporary BE does nothing.
    pub fn can_activate_temporarily(&self) -> bool {
        !self.flags.now && !self.flags.temp
    }

    pub fn can_clear_temporary(&self) -> bool {
        self.flags.temp
    }

    /// Neither the running BE nor the one booted next may be destroyed (bectl refuses too).
    pub fn can_destroy(&self) -> bool {
        !self.flags.now && !self.flags.reboot
    }

    pub fn can_mount(&self) -> bool {
        self.mountpoint.is_none()
    }

    /// Mounted somewhere other than `/` (the running BE is mounted at `/` and cannot be unmounted).
    pub fn can_unmount(&self) -> bool {
        self.mountpoint.as_deref().is_some_and(|m| m != "/")
    }
}

fn parse_flags(flags: &str, line: &str) -> Result<ActiveFlags> {
    if flags.is_empty() {
        return Err(AppError::Parse(format!("empty active flags in bectl list line: {line:?}")));
    }
    if flags == "-" {
        return Ok(ActiveFlags::default());
    }
    let mut parsed = ActiveFlags::default();
    for c in flags.chars() {
        match c {
            'N' => parsed.now = true,
            'R' => parsed.reboot = true,
            'T' => parsed.temp = true,
            _ => return Err(AppError::Parse(format!("unknown active flag {c:?} in bectl list line: {line:?}"))),
        }
    }
    Ok(parsed)
}

/// Parses `bectl list -H`. A malformed row fails the whole parse: silently skipping one
/// would hide a boot environment the user thinks they have.
pub fn parse_bootenvs(output: &str) -> Result<Vec<BootEnv>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[name, flags, mountpoint, space, created] = fields.as_slice() else {
                return Err(AppError::Parse(format!("expected 5 fields in bectl list line: {line:?}")));
            };
            if name.is_empty() {
                return Err(AppError::Parse(format!("empty name in bectl list line: {line:?}")));
            }
            Ok(BootEnv {
                name: name.to_string(),
                flags: parse_flags(flags, line)?,
                mountpoint: (mountpoint != "-").then(|| mountpoint.to_string()),
                space: space.to_string(),
                created: created.to_string(),
            })
        })
        .collect()
}

/// A name must be non-empty, must not start with `-` (it would be read as an option) and
/// may contain only ASCII letters, digits and `_ - . :` (ZFS dataset-name characters).
pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("The boot environment name cannot be empty".to_string());
    }
    if name.starts_with('-') {
        return Err("A boot environment name cannot start with '-'".to_string());
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))) {
        return Err(format!("{c:?} is not allowed in a boot environment name (letters, digits and _ - . : only)"));
    }
    Ok(())
}

fn checked(name: &str) -> Result<&str> {
    validate_name(name).map_err(AppError::Parse)?;
    Ok(name)
}

pub fn list(runner: &dyn CommandRunner) -> Result<Vec<BootEnv>> {
    let output = super::run_checked(runner, "bectl", &["list", "-H"], "bectl list")?;
    parse_bootenvs(&output.stdout)
}

/// `bectl create [-e <from>] <name>`; without `from` bectl clones the running BE.
pub fn create(runner: &dyn CommandRunner, name: &str, from: Option<&str>) -> Result<()> {
    let name = checked(name)?;
    let mut args = vec!["create"];
    if let Some(from) = from {
        args.extend(["-e", checked(from)?]);
    }
    args.push(name);
    super::run_checked(runner, "bectl", &args, &format!("bectl create {name}"))?;
    Ok(())
}

/// `bectl activate [-t] <name>`: permanent, or for the next boot only with `temporary`.
pub fn activate(runner: &dyn CommandRunner, name: &str, temporary: bool) -> Result<()> {
    let name = checked(name)?;
    let mut args = vec!["activate"];
    if temporary {
        args.push("-t");
    }
    args.push(name);
    super::run_checked(runner, "bectl", &args, &format!("bectl activate {name}"))?;
    Ok(())
}

/// `bectl activate -T <name>`: removes a temporary activation.
pub fn clear_temporary(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let name = checked(name)?;
    super::run_checked(runner, "bectl", &["activate", "-T", name], &format!("bectl activate -T {name}"))?;
    Ok(())
}

pub fn rename(runner: &dyn CommandRunner, from: &str, to: &str) -> Result<()> {
    let (from, to) = (checked(from)?, checked(to)?);
    super::run_checked(runner, "bectl", &["rename", from, to], &format!("bectl rename {from} {to}"))?;
    Ok(())
}

/// `bectl mount <name>`; bectl picks a `/tmp/be_mount.XXXX` path, which the next list shows.
pub fn mount(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let name = checked(name)?;
    super::run_checked(runner, "bectl", &["mount", name], &format!("bectl mount {name}"))?;
    Ok(())
}

pub fn unmount(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let name = checked(name)?;
    super::run_checked(runner, "bectl", &["umount", name], &format!("bectl umount {name}"))?;
    Ok(())
}

/// `bectl destroy [-o] <name>`; `-o` also removes the origin snapshot.
pub fn destroy(runner: &dyn CommandRunner, name: &str, origin: bool) -> Result<()> {
    let name = checked(name)?;
    let mut args = vec!["destroy"];
    if origin {
        args.push("-o");
    }
    args.push(name);
    super::run_checked(runner, "bectl", &args, &format!("bectl destroy {name}"))?;
    Ok(())
}
```

- [ ] **Step 5: Register the module**

In `src/zfs/mod.rs` add `pub mod bootenv;` as the first line of the `pub mod` list (before `pub mod dataset;`).

- [ ] **Step 6: Run the tests**

Run: `cargo test bootenv 2>&1 | tail -20`
Expected: all `zfs::bootenv::tests::*` PASS. Then `cargo clippy 2>&1 | tail -5` — expected: still only the 1 pre-existing warning.

- [ ] **Step 7: Commit**

```bash
git add src/zfs/bootenv.rs src/zfs/mod.rs
git commit -m "feat: bootenv module: parse bectl list -H and the bectl calls

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Mutation calls

**Files:**
- Modify: `src/ui/modal.rs` — `MutationCall` enum (line ~11), `tone` (~62), `label` (~100), `run` (~140), test module.

**Interfaces:**
- Consumes: Task 1's `bootenv::{create, activate, clear_temporary, rename, mount, unmount, destroy}`.
- Produces: `MutationCall::{CreateBootEnv { name: String, from: Option<String> }, ActivateBootEnv { name: String, temporary: bool }, ClearTemporaryBootEnv { name: String }, RenameBootEnv { from: String, to: String }, MountBootEnv { name: String }, UnmountBootEnv { name: String }, DestroyBootEnv { name: String, origin: bool }}`.

- [ ] **Step 1: Write the failing tests** — append inside `mod tests` in `src/ui/modal.rs`:

```rust
    #[test]
    fn boot_environment_calls_run_bectl_with_the_right_argv() {
        let cases: Vec<(MutationCall, Vec<&str>)> = vec![
            (MutationCall::CreateBootEnv { name: "n".into(), from: None }, vec!["create", "n"]),
            (MutationCall::CreateBootEnv { name: "n".into(), from: Some("o".into()) }, vec!["create", "-e", "o", "n"]),
            (MutationCall::ActivateBootEnv { name: "n".into(), temporary: false }, vec!["activate", "n"]),
            (MutationCall::ActivateBootEnv { name: "n".into(), temporary: true }, vec!["activate", "-t", "n"]),
            (MutationCall::ClearTemporaryBootEnv { name: "n".into() }, vec!["activate", "-T", "n"]),
            (MutationCall::RenameBootEnv { from: "a".into(), to: "b".into() }, vec!["rename", "a", "b"]),
            (MutationCall::MountBootEnv { name: "n".into() }, vec!["mount", "n"]),
            (MutationCall::UnmountBootEnv { name: "n".into() }, vec!["umount", "n"]),
            (MutationCall::DestroyBootEnv { name: "n".into(), origin: false }, vec!["destroy", "n"]),
            (MutationCall::DestroyBootEnv { name: "n".into(), origin: true }, vec!["destroy", "-o", "n"]),
        ];
        for (call, want) in cases {
            let runner = FakeRunner::new("");
            call.run(&runner).unwrap();
            let calls = runner.calls.lock().unwrap();
            assert_eq!(calls[0].0, "bectl", "{call:?}");
            assert_eq!(calls[0].1, want, "{call:?}");
        }
    }

    #[test]
    fn boot_environment_tones_match_how_hard_each_is_to_undo() {
        let tone = |c: MutationCall| c.tone();
        assert_eq!(tone(MutationCall::DestroyBootEnv { name: "n".into(), origin: false }), Tone::Danger);
        assert_eq!(tone(MutationCall::ActivateBootEnv { name: "n".into(), temporary: false }), Tone::Warn);
        assert_eq!(tone(MutationCall::ActivateBootEnv { name: "n".into(), temporary: true }), Tone::Neutral);
        for call in [
            MutationCall::CreateBootEnv { name: "n".into(), from: None },
            MutationCall::ClearTemporaryBootEnv { name: "n".into() },
            MutationCall::RenameBootEnv { from: "a".into(), to: "b".into() },
            MutationCall::MountBootEnv { name: "n".into() },
            MutationCall::UnmountBootEnv { name: "n".into() },
        ] {
            assert_eq!(call.tone(), Tone::Neutral, "{call:?}");
        }
    }

    #[test]
    fn a_failing_bectl_surfaces_through_the_mutation() {
        let runner = FakeRunner::failing("cannot destroy active boot environment", 5);
        let err = MutationCall::DestroyBootEnv { name: "default".into(), origin: false }.run(&runner).unwrap_err().to_string();
        assert!(err.contains("cannot destroy active boot environment"), "{err}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test boot_environment 2>&1 | tail -8`
Expected: compile error, `no variant named CreateBootEnv`.

- [ ] **Step 3: Add the variants and match arms**

In the `MutationCall` enum, after `RemoveSmbShare { dataset: String },` add:

```rust
    CreateBootEnv { name: String, from: Option<String> },
    ActivateBootEnv { name: String, temporary: bool },
    ClearTemporaryBootEnv { name: String },
    RenameBootEnv { from: String, to: String },
    MountBootEnv { name: String },
    UnmountBootEnv { name: String },
    DestroyBootEnv { name: String, origin: bool },
```

In `tone()`: add `| MutationCall::DestroyBootEnv { .. }` to the `Tone::Danger` arm; add `| MutationCall::ActivateBootEnv { temporary: false, .. }` to the `Tone::Warn` arm; add to the `Tone::Neutral` arm:

```rust
            | MutationCall::ActivateBootEnv { temporary: true, .. }
            | MutationCall::CreateBootEnv { .. }
            | MutationCall::ClearTemporaryBootEnv { .. }
            | MutationCall::RenameBootEnv { .. }
            | MutationCall::MountBootEnv { .. }
            | MutationCall::UnmountBootEnv { .. }
```

In `label()` add:

```rust
            MutationCall::CreateBootEnv { .. } => "CREATE BOOT ENV",
            MutationCall::ActivateBootEnv { .. } => "ACTIVATE BOOT ENV",
            MutationCall::ClearTemporaryBootEnv { .. } => "CLEAR TEMPORARY BOOT ENV",
            MutationCall::RenameBootEnv { .. } => "RENAME BOOT ENV",
            MutationCall::MountBootEnv { .. } => "MOUNT BOOT ENV",
            MutationCall::UnmountBootEnv { .. } => "UNMOUNT BOOT ENV",
            MutationCall::DestroyBootEnv { .. } => "DESTROY BOOT ENV",
```

In `run()` add:

```rust
            MutationCall::CreateBootEnv { name, from } => crate::zfs::bootenv::create(runner, name, from.as_deref()),
            MutationCall::ActivateBootEnv { name, temporary } => crate::zfs::bootenv::activate(runner, name, *temporary),
            MutationCall::ClearTemporaryBootEnv { name } => crate::zfs::bootenv::clear_temporary(runner, name),
            MutationCall::RenameBootEnv { from, to } => crate::zfs::bootenv::rename(runner, from, to),
            MutationCall::MountBootEnv { name } => crate::zfs::bootenv::mount(runner, name),
            MutationCall::UnmountBootEnv { name } => crate::zfs::bootenv::unmount(runner, name),
            MutationCall::DestroyBootEnv { name, origin } => crate::zfs::bootenv::destroy(runner, name, *origin),
```

- [ ] **Step 4: Run all tests**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|error"`
Expected: `test result: ok.` with the baseline 977 plus Task 1's and Task 2's new tests, no failures.

- [ ] **Step 5: Commit**

```bash
git add src/ui/modal.rs
git commit -m "feat: boot environment mutation calls

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The section, its refresh and its table

**Files:**
- Modify: `src/ui/state.rs` — `Section` enum (~line 23), `WorkerMsg` (~116), `App` struct (~169) and `App::new` (~285, ~298), `auto_refresh_section` (~414), `auto_refresh` (~430), `refresh_visible` (~498), `clear_host_data` (~575), new `refresh_bootenvs` (next to `refresh_shares` ~1768), `drain_worker_messages` (next to the `Shares` arm ~2029), tests.
- Modify: `src/ui/mod.rs` — the two `Section::Shares => app.refresh_shares(...)` dispatch sites (~191 and ~279).
- Modify: `src/ui/layout.rs` — sidebar label (~163), `draw_main` (~208), new `draw_bootenvs`.
- Modify: `src/ui/hints.rs` — a minimal `(Section::BootEnvs, _)` arm (~line 242) so the match stays exhaustive.

**Interfaces:**
- Consumes: `bootenv::{BootEnv, list}`.
- Produces: `Section::BootEnvs` (in `App::sections` between Shares and Jobs), `App::bootenvs: Vec<BootEnv>`, `App::bootenvs_table: TableState`, `App::bootenvs_unavailable: Option<String>`, `App::refresh_bootenvs(&mut self, Arc<dyn CommandRunner>)`, `WorkerMsg::BootEnvs(usize, Result<Vec<BootEnv>>)`, `App::selected_bootenv(&self) -> Option<&BootEnv>`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/ui/state.rs` (helpers `in_section`, `runner`, `wait_until`, `TEN` already exist):

```rust
    fn be(name: &str, now: bool, reboot: bool, temp: bool, mount: Option<&str>) -> crate::zfs::bootenv::BootEnv {
        crate::zfs::bootenv::BootEnv {
            name: name.to_string(),
            flags: crate::zfs::bootenv::ActiveFlags { now, reboot, temp },
            mountpoint: mount.map(str::to_string),
            space: "1M".to_string(),
            created: "2026-10-07 19:26".to_string(),
        }
    }

    #[test]
    fn boot_envs_sits_between_shares_and_jobs() {
        let app = App::new(vec!["local".to_string()]);
        let at = |s| app.sections.iter().position(|x| *x == s).unwrap();
        assert_eq!(at(Section::BootEnvs), at(Section::Shares) + 1);
        assert_eq!(at(Section::Jobs), at(Section::BootEnvs) + 1);
        assert_eq!(app.sections.last(), Some(&Section::Jobs));
    }

    #[test]
    fn refresh_bootenvs_populates_app_state() {
        let mut app = App::new(vec!["local".to_string()]);
        app.refresh_bootenvs(runner("default\tNR\t/\t5.18G\t2026-07-24 18:48\nspare\t-\t-\t8K\t2026-10-07 19:26\n"));
        wait_until(&mut app, |a| !a.bootenvs.is_empty());
        assert_eq!(app.bootenvs.len(), 2);
        assert!(app.bootenvs[0].flags.now && app.bootenvs[0].flags.reboot);
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn a_failing_bectl_marks_the_section_unavailable_and_the_next_good_refresh_recovers() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("stale", false, false, false, None)];
        app.bootenvs_table.select(Some(0));
        app.tx.send(WorkerMsg::BootEnvs(0, Err(crate::error::AppError::CommandFailed { cmd: "bectl list".into(), status: 127, stderr: "bectl: not found".into() }))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty(), "a stale list must not look current");
        assert_eq!(app.bootenvs_table.selected(), None);
        assert!(app.bootenvs_unavailable.as_deref().is_some_and(|m| m.contains("bectl: not found")));
        // Not gated on the earlier failure: the periodic refresh still runs and recovers.
        app.auto_refresh(runner("default\tNR\t/\t1G\td\n"), Instant::now(), TEN);
        wait_until(&mut app, |a| !a.bootenvs.is_empty());
        assert_eq!(app.bootenvs_unavailable, None);
        assert_eq!(app.bootenvs.len(), 1);
    }

    #[test]
    fn a_host_with_zero_boot_environments_is_empty_not_unavailable() {
        let mut app = in_section(Section::BootEnvs);
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty());
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn bootenvs_result_for_a_stale_host_is_discarded() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.tx.send(WorkerMsg::BootEnvs(1, Ok(vec![be("x", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert!(app.bootenvs.is_empty());
    }

    #[test]
    fn switching_host_clears_boot_environment_state() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.bootenvs = vec![be("x", false, false, false, None)];
        app.bootenvs_table.select(Some(0));
        app.bootenvs_unavailable = Some("boom".to_string());
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::BackTab); // -> Hosts
        app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
        assert!(app.bootenvs.is_empty());
        assert_eq!(app.bootenvs_table.selected(), None);
        assert_eq!(app.bootenvs_unavailable, None);
    }

    #[test]
    fn a_refresh_keeps_the_selection_on_the_same_boot_environment_and_clamps_when_it_vanishes() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
        app.bootenvs_table.select(Some(1));
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("0", false, false, false, None), be("a", false, false, false, None), be("b", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), Some(2), "still on b");
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("a", false, false, false, None)]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), Some(0), "b vanished: clamped into range");
        app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.bootenvs_table.selected(), None);
    }

    #[test]
    fn up_and_down_move_the_boot_environment_selection() {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
        app.handle_key(KeyCode::Down);
        assert_eq!(app.bootenvs_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("b"));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.bootenvs_table.selected(), Some(0));
    }
```

Also extend the two existing tests: in `auto_refresh_targets_only_the_visible_list_sections` add `(Section::BootEnvs, Some(Section::BootEnvs)),` after the Shares tuple, and in `auto_refresh_starts_the_refresh_that_belongs_to_the_visible_section` add `Section::BootEnvs` to its `for section in [...]` array.

Append to `mod tests` in `src/ui/layout.rs`:

```rust
    #[test]
    fn the_boot_envs_table_decodes_activity_and_drops_columns_when_narrow() {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs = vec![
            BootEnv { name: "default".into(), flags: ActiveFlags { now: true, reboot: true, temp: false }, mountpoint: Some("/".into()), space: "5.18G".into(), created: "2026-07-24 18:48".into() },
            BootEnv { name: "spare".into(), flags: ActiveFlags { now: false, reboot: false, temp: true }, mountpoint: None, space: "8K".into(), created: "2026-10-07 19:26".into() },
        ];
        let wide = render(&mut app, 120, 24).join("\n");
        for want in ["Boot Environments", "Name", "Active", "Space", "Mountpoint", "Created", "default", "Now+Reboot", "Temp", "5.18G", "2026-07-24 18:48"] {
            assert!(wide.contains(want), "missing {want:?} in:\n{wide}");
        }
        let narrow = render(&mut app, 80, 24).join("\n");
        for want in ["Name", "Active", "Space", "default"] {
            assert!(narrow.contains(want), "missing {want:?} in:\n{narrow}");
        }
        assert!(!narrow.contains("Mountpoint") && !narrow.contains("Created"), "optional columns must drop:\n{narrow}");
    }

    #[test]
    fn an_unavailable_boot_envs_section_says_why_in_its_title() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
        app.bootenvs_unavailable = Some("command 'bectl list' failed with status 127: bectl: not found".to_string());
        let text = render(&mut app, 140, 24).join("\n");
        assert!(text.contains("[unavailable: command 'bectl list' failed"), "{text}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" | head -5`
Expected: compile errors for `Section::BootEnvs`, `refresh_bootenvs`, `bootenvs`.

- [ ] **Step 3: Implement the state plumbing in `src/ui/state.rs`**

1. `Section` enum: add `BootEnvs,` after `Shares,`.
2. `WorkerMsg`: after `Shares(usize, Result<SharesSnapshot>),` add `BootEnvs(usize, Result<Vec<crate::zfs::bootenv::BootEnv>>),`.
3. `App` struct, after the `shares_loaded` field, add:

```rust
    pub bootenvs: Vec<crate::zfs::bootenv::BootEnv>,
    pub bootenvs_table: TableState,
    /// Why the Boot Envs section cannot list on this host (e.g. no `bectl`, not a boot-environment
    /// layout, an unreachable host), shown in its title. `None` once a list succeeds.
    pub bootenvs_unavailable: Option<String>,
```

4. `App::new`: change `sections: vec![...]` to `vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs, Section::Jobs]`, and next to `shares_loaded: false,` add `bootenvs: Vec::new(), bootenvs_table: TableState::default(), bootenvs_unavailable: None,`.
5. `clear_host_data`: after `self.shares_loaded = false;` add `self.bootenvs.clear(); self.bootenvs_table.select(None); self.bootenvs_unavailable = None;`.
6. `auto_refresh_section`: add `| Section::BootEnvs` to the or-pattern. `auto_refresh` and `refresh_visible`: add `Section::BootEnvs => self.refresh_bootenvs(runner),` after the `Section::Shares` arm.
7. Add next to `refresh_shares`:

```rust
    /// See `refresh_pools` for why this queues rather than drops a request while in flight.
    pub fn refresh_bootenvs(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::BootEnvs, host) {
            self.pending_refresh.insert((Section::BootEnvs, host));
            return;
        }
        self.begin_refresh(Section::BootEnvs, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::bootenv::list(runner.as_ref());
            let _ = tx.send(WorkerMsg::BootEnvs(host, result));
        });
    }

    pub fn selected_bootenv(&self) -> Option<&crate::zfs::bootenv::BootEnv> {
        self.bootenvs_table.selected().and_then(|i| self.bootenvs.get(i))
    }
```

8. `drain_worker_messages`, after the `WorkerMsg::Shares` arm:

```rust
                WorkerMsg::BootEnvs(host, result) => {
                    let quiet = self.end_refresh(Section::BootEnvs, host);
                    let had_pending = self.pending_refresh.remove(&(Section::BootEnvs, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::BootEnvs);
                    }
                    match result {
                        Ok(envs) => {
                            let previous = self.selected_bootenv().map(|b| b.name.clone());
                            self.bootenvs = envs;
                            pin_selection(&mut self.bootenvs_table, previous, &self.bootenvs, |b| b.name.clone());
                            self.bootenvs_unavailable = None;
                            self.clear_status_after_refresh(quiet);
                        }
                        Err(e) => {
                            // Not `set_error`: a host without bectl would otherwise raise a fresh
                            // error every auto-refresh. The title carries the reason instead.
                            self.bootenvs.clear();
                            self.bootenvs_table.select(None);
                            self.bootenvs_unavailable = Some(e.to_string());
                        }
                    }
                }
```

Add `Down`/`Up` arms next to the Shares ones:

```rust
            KeyCode::Down if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::BootEnvs => {
                move_table_selection(&mut self.bootenvs_table, self.bootenvs.len(), false);
            }
```

- [ ] **Step 4: Dispatch sites in `src/ui/mod.rs`**

At both places that have `Section::Shares => app.refresh_shares(host.runner.clone()),` add the next line `Section::BootEnvs => app.refresh_bootenvs(host.runner.clone()),`.

- [ ] **Step 5: Layout in `src/ui/layout.rs`**

In `draw_sidebar` add `Section::BootEnvs => "Boot Envs",` after `Section::Shares`. In `draw_main` add `Section::BootEnvs => draw_bootenvs(f, app, area),` after the Shares arm. Add, after `draw_shares`:

```rust
fn draw_bootenvs(f: &mut Frame, app: &mut App, area: Rect) {
    const NAME_W: u16 = 24;
    const ACTIVE_W: u16 = 12;
    const SPACE_W: u16 = 8;
    const MOUNT_W: u16 = 24;
    const CREATED_W: u16 = 16;
    let theme = app.theme.clone();
    // Name, Active and Space always show; Mountpoint then Created only while they fit.
    let inner = area.width.saturating_sub(2);
    let mut used = NAME_W + ACTIVE_W + SPACE_W + 2;
    let show_mount = used + MOUNT_W < inner;
    if show_mount {
        used += MOUNT_W + 1;
    }
    let show_created = show_mount && used + CREATED_W < inner;
    let rows = app.bootenvs.iter().map(|b| {
        let mut cells = vec![sanitise_for_display(&b.name), b.flags.label(), b.space.clone()];
        if show_mount {
            cells.push(sanitise_for_display(b.mountpoint.as_deref().unwrap_or("-")));
        }
        if show_created {
            cells.push(sanitise_for_display(&b.created));
        }
        Row::new(cells)
    });
    let mut widths = vec![Constraint::Length(NAME_W), Constraint::Length(ACTIVE_W), Constraint::Length(SPACE_W)];
    let mut headers = vec!["Name", "Active", "Space"];
    if show_mount {
        widths.push(Constraint::Length(MOUNT_W));
        headers.push("Mountpoint");
    }
    if show_created {
        widths.push(Constraint::Min(CREATED_W));
        headers.push("Created");
    }
    let suffix = app
        .bootenvs_unavailable
        .as_ref()
        .map(|m| format!(" [unavailable: {}]", sanitise_for_display(m.lines().next().unwrap_or(""))))
        .unwrap_or_default();
    let title = format!("Boot Environments — {} ('r' to refresh){suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(headers))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.bootenvs_table);
}
```

- [ ] **Step 6: Minimal hints arm in `src/ui/hints.rs`**

After the `(Section::Shares, _) => { ... }` arm add:

```rust
        (Section::BootEnvs, _) => {
            if !app.bootenvs.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            v.push(h("r", "Refresh", 4));
        }
```

- [ ] **Step 7: Run everything**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error"`
Expected: all pass. If a test elsewhere enumerates every `Section` and fails to compile or assert, extend it to include `Section::BootEnvs` the same way the Shares entry is handled. Then `cargo clippy 2>&1 | tail -4` — no new warnings.

- [ ] **Step 8: Commit**

```bash
git add src/ui/state.rs src/ui/mod.rs src/ui/layout.rs src/ui/hints.rs
git commit -m "feat: Boot Envs section: refresh, unavailable state and table

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Keys, forms and hints

**Files:**
- Modify: `src/ui/modal.rs` — `FormKind` (add two variants).
- Modify: `src/ui/state.rs` — key handlers (next to the Shares/Jobs handlers), the form-submit branch (~1473), new `build_bootenv_action`, the `None` arm of `build_pending_action` (~2740), tests.
- Modify: `src/ui/picker.rs` — `build_picker_state` (F2 on the clone-from field).
- Modify: `src/ui/hints.rs` — the real `(Section::BootEnvs, _)` arm, scenarios, fingerprint, probes.

**Interfaces:**
- Consumes: Task 1's predicates and `validate_name`; Task 2's `MutationCall` variants; Task 3's `selected_bootenv`, `bootenvs_unavailable`.
- Produces: `FormKind::CreateBootEnv { host: usize, existing: Vec<String> }` (fields `[name, clone_from]`), `FormKind::RenameBootEnv { host: usize, from: String, existing: Vec<String> }` (fields `[new_name]`); keys in the Boot Envs section: `n` new, `a` activate (confirm), `t` next boot only, `T` clear temporary, `R` rename, `m` mount, `u` unmount, `d` destroy (typed confirm), `D` destroy + origin.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/ui/state.rs` (reuses `be`, `in_section`, `form_values`, and `Modal`/`MutationCall` imports already in scope there):

```rust
    fn bootenv_app(rows: Vec<crate::zfs::bootenv::BootEnv>, select: usize) -> App {
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs = rows;
        app.bootenvs_table.select(Some(select));
        app
    }

    fn running() -> crate::zfs::bootenv::BootEnv {
        be("default", true, true, false, Some("/"))
    }

    fn spare() -> crate::zfs::bootenv::BootEnv {
        be("spare", false, false, false, None)
    }

    fn only_pending(app: &App) -> &MutationCall {
        let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
        assert_eq!(action.section, Section::BootEnvs);
        &action.call
    }

    #[test]
    fn n_opens_the_create_form_listing_existing_names_for_the_picker() {
        let mut app = bootenv_app(vec![running(), spare()], 0);
        app.handle_key(KeyCode::Char('n'));
        assert_eq!(form_values(&app), ["", ""]);
        let Some(Modal::Form { kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
        assert_eq!(*kind, FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string(), "spare".to_string()] });
    }

    #[test]
    fn n_works_with_zero_boot_environments_but_not_when_unavailable() {
        let mut app = in_section(Section::BootEnvs);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_some());
        let mut app = in_section(Section::BootEnvs);
        app.bootenvs_unavailable = Some("no bectl".to_string());
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none());
    }

    /// Opens the create form, types both fields and presses Enter on the last one.
    fn submit(app: &mut App, name: &str, from: &str) -> Option<PendingAction> {
        app.handle_key(KeyCode::Char('n'));
        for c in name.chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Tab);
        for c in from.chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::Enter)
    }

    #[test]
    fn the_create_form_returns_the_create_call_with_or_without_a_clone_source() {
        let mut app = bootenv_app(vec![running(), spare()], 0);
        let action = submit(&mut app, "pre-upgrade", "spare").expect("submit returns the action");
        assert_eq!((action.host, action.section), (0, Section::BootEnvs));
        assert_eq!(action.call, MutationCall::CreateBootEnv { name: "pre-upgrade".to_string(), from: Some("spare".to_string()) });
        let mut app = bootenv_app(vec![running(), spare()], 0);
        let action = submit(&mut app, "pre-upgrade", "").expect("submit returns the action");
        assert_eq!(action.call, MutationCall::CreateBootEnv { name: "pre-upgrade".to_string(), from: None });
        assert!(app.modal.is_none());
    }

    #[test]
    fn the_create_form_refuses_bad_names_duplicates_and_unknown_sources_with_a_reason() {
        for (name, from, why) in [
            ("", "", "cannot be empty"),
            ("-F", "", "cannot start with '-'"),
            ("a b", "", "not allowed"),
            ("default", "", "already exists"),
            ("ok", "nope", "no boot environment named 'nope'"),
            ("ok", "-e", "no boot environment named '-e'"),
        ] {
            let mut app = bootenv_app(vec![running(), spare()], 0);
            assert!(submit(&mut app, name, from).is_none(), "{name:?}/{from:?}: nothing is submitted");
            assert!(app.modal.is_some(), "{name:?}/{from:?}: the form stays open");
            assert!(app.status_message.as_deref().is_some_and(|m| m.contains(why)), "{name:?}/{from:?}: {:?}", app.status_message);
        }
    }

    #[test]
    fn build_bootenv_action_makes_the_expected_calls() {
        let create = FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string()] };
        let field = |v: &str| FormField { label: "l".to_string(), value: v.to_string() };
        let a = build_bootenv_action(&create, &[field("new"), field("default")]).unwrap();
        assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: Some("default".to_string()) });
        assert_eq!(a.section, Section::BootEnvs);
        let a = build_bootenv_action(&create, &[field("new"), field("")]).unwrap();
        assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: None });
        let rename = FormKind::RenameBootEnv { host: 2, from: "old".to_string(), existing: vec!["old".to_string(), "taken".to_string()] };
        let a = build_bootenv_action(&rename, &[field("fresh")]).unwrap();
        assert_eq!((a.host, a.call), (2, MutationCall::RenameBootEnv { from: "old".to_string(), to: "fresh".to_string() }));
        assert!(build_bootenv_action(&rename, &[field("taken")]).is_err());
        assert!(build_bootenv_action(&rename, &[field("old")]).is_err(), "renaming to itself");
        assert!(build_bootenv_action(&rename, &[field("-x")]).is_err());
    }

    #[test]
    fn a_asks_to_confirm_then_activates_permanently_and_only_when_it_would_change_something() {
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char('a'));
        let Some(Modal::Confirm { prompt, action }) = &app.modal else { panic!("{:?}", app.modal) };
        assert!(prompt.contains("'spare'"), "{prompt}");
        assert_eq!(action.call, MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: false });
        let mut app = bootenv_app(vec![running(), spare()], 0);
        app.handle_key(KeyCode::Char('a'));
        assert!(app.modal.is_none(), "already the BE used on reboot");
    }

    #[test]
    fn t_activates_for_the_next_boot_without_a_confirm_and_big_t_clears_it() {
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char('t'));
        assert!(app.modal.is_none());
        assert_eq!(only_pending(&app), &MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: true });
        assert!(app.status_message.is_some());
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char('T'));
        assert!(app.pending_mutations.is_empty(), "spare has no temporary activation to clear");
        let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
        app.handle_key(KeyCode::Char('T'));
        assert_eq!(only_pending(&app), &MutationCall::ClearTemporaryBootEnv { name: "trial".to_string() });
        let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
        app.handle_key(KeyCode::Char('t'));
        assert!(app.pending_mutations.is_empty(), "already temporary");
        let mut app = bootenv_app(vec![running()], 0);
        app.handle_key(KeyCode::Char('t'));
        assert!(app.pending_mutations.is_empty(), "the running BE");
    }

    #[test]
    fn m_mounts_an_unmounted_be_and_u_unmounts_a_mounted_one() {
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char('m'));
        assert_eq!(only_pending(&app), &MutationCall::MountBootEnv { name: "spare".to_string() });
        app.handle_key(KeyCode::Char('u'));
        assert_eq!(app.pending_mutations.len(), 1, "an unmounted BE cannot be unmounted");
        let mut app = bootenv_app(vec![running(), be("m", false, false, false, Some("/tmp/be_mount.X"))], 1);
        app.handle_key(KeyCode::Char('u'));
        assert_eq!(only_pending(&app), &MutationCall::UnmountBootEnv { name: "m".to_string() });
        app.handle_key(KeyCode::Char('m'));
        assert_eq!(app.pending_mutations.len(), 1, "already mounted");
        let mut app = bootenv_app(vec![running()], 0);
        app.handle_key(KeyCode::Char('m'));
        app.handle_key(KeyCode::Char('u'));
        assert!(app.pending_mutations.is_empty(), "the running BE is mounted at / and cannot be remounted or unmounted");
    }

    #[test]
    fn big_r_opens_the_rename_form() {
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char('R'));
        assert_eq!(form_values(&app), [""]);
        let Some(Modal::Form { kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
        assert_eq!(*kind, FormKind::RenameBootEnv { host: 0, from: "spare".to_string(), existing: vec!["default".to_string(), "spare".to_string()] });
    }

    #[test]
    fn d_and_big_d_ask_for_the_name_and_the_running_be_is_refused_with_a_reason() {
        for (key, origin) in [('d', false), ('D', true)] {
            let mut app = bootenv_app(vec![running(), spare()], 1);
            app.handle_key(KeyCode::Char(key));
            let Some(Modal::ConfirmTyped { expected, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
            assert_eq!(expected, "spare");
            assert_eq!(action.call, MutationCall::DestroyBootEnv { name: "spare".to_string(), origin });
        }
        for blocked in [running(), be("next", false, true, false, None)] {
            let mut app = bootenv_app(vec![blocked.clone()], 0);
            app.handle_key(KeyCode::Char('d'));
            assert!(app.modal.is_none());
            assert!(app.status_message.as_deref().is_some_and(|m| m.contains(&blocked.name) && m.contains("cannot be destroyed")), "{:?}", app.status_message);
        }
    }

    #[test]
    fn a_successful_boot_environment_mutation_queues_a_re_list() {
        let mut app = in_section(Section::BootEnvs);
        app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
        assert_eq!(app.drain_worker_messages(), vec![Section::BootEnvs]);
    }

    #[test]
    fn help_documents_the_boot_environment_keys() {
        let text = help_text();
        for want in ["Boot Envs", "next boot only", "rename", "mount", "destroy"] {
            assert!(text.contains(want), "missing {want:?}");
        }
    }
```

Add to `mod tests` in `src/ui/picker.rs`:

```rust
    #[test]
    fn the_clone_from_field_offers_the_existing_boot_environments_and_the_name_field_does_not() {
        let kind = FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string(), "spare".to_string()] };
        assert!(build_picker_state(&kind, 0, "").is_none());
        let picker = build_picker_state(&kind, 1, "spare").unwrap();
        assert_eq!(picker.values, vec!["default", "spare"]);
        assert_eq!(picker.selected, 1, "pre-seeded from what is typed");
        assert!(!picker.multi_select);
        let rename = FormKind::RenameBootEnv { host: 0, from: "a".to_string(), existing: vec![] };
        assert!(build_picker_state(&rename, 0, "").is_none());
    }
```

In `src/ui/hints.rs` tests: add `a.bootenvs_table.selected(),` after `a.shares_table.selected(),` in `fingerprint`; add `'t', 'T', 'm', 'u', 'D'` to the `probes` array in `keys_that_are_not_advertised_are_no_ops`; add these helpers next to `shares()` and the scenarios to `scenarios()` (`("boot envs, running selected", boot_envs(0)), ("boot envs, spare selected", boot_envs(1)), ("boot envs, temporary selected", boot_envs(2)), ("boot envs, mounted selected", boot_envs(3)), ("boot envs, next-boot selected", boot_envs(4)), ("boot envs, none selected", boot_envs_none()), ("boot envs, unavailable", boot_envs_unavailable()),`):

```rust
    fn boot_envs(selected: usize) -> App {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let row = |name: &str, now, reboot, temp, mount: Option<&str>| BootEnv {
            name: name.to_string(),
            flags: ActiveFlags { now, reboot, temp },
            mountpoint: mount.map(str::to_string),
            space: "1M".to_string(),
            created: "2026-10-07 19:26".to_string(),
        };
        let mut a = base();
        go(&mut a, Section::BootEnvs);
        a.bootenvs = vec![
            row("default", true, true, false, Some("/")),
            row("spare", false, false, false, None),
            row("trial", false, false, true, None),
            row("mounted", false, false, false, Some("/tmp/be_mount.X")),
            row("next", false, true, false, None),
        ];
        a.bootenvs_table.select(Some(selected));
        a
    }

    fn boot_envs_none() -> App {
        let mut a = boot_envs(0);
        a.bootenvs_table.select(None);
        a
    }

    fn boot_envs_unavailable() -> App {
        let mut a = base();
        go(&mut a, Section::BootEnvs);
        a.bootenvs_unavailable = Some("bectl: not found".to_string());
        a
    }

    #[test]
    fn boot_envs_advertise_exactly_what_the_selected_row_allows() {
        // `context_hints` is the section's own hints, without the always-present `! A ? q`.
        let k = |a: &App| context_hints(a).iter().map(|h| h.key).filter(|k| k.chars().count() == 1 && *k != "r").collect::<Vec<_>>();
        assert_eq!(k(&boot_envs(0)), vec!["R", "n"], "running: rename and new only");
        assert_eq!(k(&boot_envs(1)), vec!["a", "t", "R", "m", "d", "D", "n"]);
        assert_eq!(k(&boot_envs(2)), vec!["a", "T", "R", "m", "d", "D", "n"]);
        assert_eq!(k(&boot_envs(3)), vec!["a", "t", "R", "u", "d", "D", "n"]);
        assert_eq!(k(&boot_envs(4)), vec!["t", "R", "m", "n"]);
        assert_eq!(k(&boot_envs_none()), vec!["n"]);
        assert!(k(&boot_envs_unavailable()).is_empty(), "nothing can be done to a section that cannot list");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test 2>&1 | grep -E "^error" | head -5`
Expected: compile errors for `FormKind::CreateBootEnv`, `build_bootenv_action`.

- [ ] **Step 3: Add the form kinds** in `src/ui/modal.rs`, in `enum FormKind` before `AddHost`:

```rust
    /// Fields: `[name, clone_from]` -- a blank `clone_from` clones the running boot environment
    /// (bectl's default). `existing` is every current name, for the duplicate check and the F2 picker.
    CreateBootEnv { host: usize, existing: Vec<String> },
    /// Fields: `[new_name]`; `from` is fixed at modal-open time.
    RenameBootEnv { host: usize, from: String, existing: Vec<String> },
```

- [ ] **Step 4: Add `build_bootenv_action`** in `src/ui/state.rs` next to `build_smb_action`:

```rust
fn build_bootenv_action(kind: &FormKind, fields: &[FormField]) -> std::result::Result<PendingAction, String> {
    use crate::zfs::bootenv::validate_name;
    match kind {
        FormKind::CreateBootEnv { host, existing } => {
            let name = field_value(fields, 0).trim();
            validate_name(name)?;
            if existing.iter().any(|e| e == name) {
                return Err(format!("A boot environment named '{name}' already exists"));
            }
            let from = field_value(fields, 1).trim();
            let from = if from.is_empty() {
                None
            } else if existing.iter().any(|e| e == from) {
                Some(from.to_string())
            } else {
                return Err(format!("There is no boot environment named '{from}' to clone"));
            };
            Ok(PendingAction { host: *host, section: Section::BootEnvs, call: MutationCall::CreateBootEnv { name: name.to_string(), from } })
        }
        FormKind::RenameBootEnv { host, from, existing } => {
            let to = field_value(fields, 0).trim();
            validate_name(to)?;
            if to == from {
                return Err("That is already its name".to_string());
            }
            if existing.iter().any(|e| e == to) {
                return Err(format!("A boot environment named '{to}' already exists"));
            }
            Ok(PendingAction { host: *host, section: Section::BootEnvs, call: MutationCall::RenameBootEnv { from: from.clone(), to: to.to_string() } })
        }
        _ => Err("not a boot environment form".to_string()),
    }
}
```

In `build_pending_action`'s final arm, extend the or-pattern: `FormKind::AddHost | FormKind::ShareSmb { .. } | FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. } | FormKind::Job { .. } | ...` (so it stays exhaustive and returns `None`).

In the form-submit chain in `handle_modal_key` (the `else if matches!(kind, FormKind::ShareSmb { .. })` block), add an equivalent branch before the final `else if let Some(action) = build_pending_action(...)`:

```rust
                    } else if matches!(kind, FormKind::CreateBootEnv { .. } | FormKind::RenameBootEnv { .. }) {
                        match build_bootenv_action(&kind, &fields) {
                            Ok(action) => Some(action),
                            Err(msg) => {
                                self.set_error(msg);
                                self.modal = Some(Modal::Form { title, fields, focused: 0, description, kind });
                                None
                            }
                        }
```

- [ ] **Step 5: Add the key handlers** in `src/ui/state.rs`'s `handle_key`, directly before `KeyCode::Char(' ') if self.current_section() == Section::Disks`:

```rust
            KeyCode::Char('n') if self.current_section() == Section::BootEnvs && self.bootenvs_unavailable.is_none() => {
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: "New boot environment".to_string(),
                    fields: vec![
                        FormField { label: "Name (letters, digits, _ - . :)".to_string(), value: String::new() },
                        FormField { label: "Clone from (F2 to pick; blank = the running boot environment)".to_string(), value: String::new() },
                    ],
                    focused: 0,
                    description: None,
                    kind: FormKind::CreateBootEnv { host: self.selected_host, existing },
                });
            }
            KeyCode::Char('a') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate()).map(|b| b.name.clone()) else { return };
                let prompt = format!("Make '{name}' the boot environment for every future boot? (y/n)");
                let call = MutationCall::ActivateBootEnv { name, temporary: false };
                self.modal = Some(Modal::Confirm { prompt, action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call } });
            }
            KeyCode::Char('t') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_activate_temporarily()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Activating '{name}' for the next boot only…"));
                let call = MutationCall::ActivateBootEnv { name, temporary: true };
                self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
            }
            KeyCode::Char('T') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_clear_temporary()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Clearing the next-boot activation of '{name}'…"));
                let call = MutationCall::ClearTemporaryBootEnv { name };
                self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
            }
            KeyCode::Char('R') if self.current_section() == Section::BootEnvs => {
                let Some(from) = self.selected_bootenv().map(|b| b.name.clone()) else { return };
                let existing: Vec<String> = self.bootenvs.iter().map(|b| b.name.clone()).collect();
                self.modal = Some(Modal::Form {
                    title: format!("Rename boot environment '{from}'"),
                    fields: vec![FormField { label: "New name (letters, digits, _ - . :)".to_string(), value: String::new() }],
                    focused: 0,
                    description: None,
                    kind: FormKind::RenameBootEnv { host: self.selected_host, from, existing },
                });
            }
            KeyCode::Char('m') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_mount()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Mounting '{name}'…"));
                let call = MutationCall::MountBootEnv { name };
                self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
            }
            KeyCode::Char('u') if self.current_section() == Section::BootEnvs => {
                let Some(name) = self.selected_bootenv().filter(|b| b.can_unmount()).map(|b| b.name.clone()) else { return };
                self.status_message = Some(format!("Unmounting '{name}'…"));
                let call = MutationCall::UnmountBootEnv { name };
                self.pending_mutations.push(PendingAction { host: self.selected_host, section: Section::BootEnvs, call });
            }
            KeyCode::Char(key @ ('d' | 'D')) if self.current_section() == Section::BootEnvs => {
                let Some(be) = self.selected_bootenv() else { return };
                let name = be.name.clone();
                if !be.can_destroy() {
                    self.status_message = Some(format!("'{name}' cannot be destroyed: it is the running or next-boot boot environment"));
                    return;
                }
                let origin = key == 'D';
                let also = if origin { " and its origin snapshot" } else { "" };
                self.modal = Some(Modal::ConfirmTyped {
                    prompt: format!("Type the boot environment name to destroy '{name}'{also}:"),
                    expected: name.clone(),
                    input: String::new(),
                    action: PendingAction { host: self.selected_host, section: Section::BootEnvs, call: MutationCall::DestroyBootEnv { name, origin } },
                });
            }
```

Also add to `help_text()` (after the `Shares` block):

```
Boot Envs
  n                new boot environment (F2 on the second field picks one to clone)
  a                make the selected one the boot environment for every future boot (confirms)
  t / T            use it for the next boot only / clear that
  R                rename
  m / u            mount / unmount (the Mountpoint column shows where)
  d / D            destroy (type its name) / destroy and remove its origin snapshot
```

- [ ] **Step 6: F2 picker for the clone-from field** in `src/ui/picker.rs`, in `build_picker_state` immediately before `let (values, multi_select) = vocabulary_for(kind, field_index)?;`:

```rust
    if let FormKind::CreateBootEnv { existing, .. } = kind {
        if field_index != 1 {
            return None;
        }
        let selected = existing.iter().position(|e| e == current_value.trim()).unwrap_or(0);
        return Some(PickerState { values: existing.clone(), filter: String::new(), selected, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Ready, note: None, account: None, pool_host: None });
    }
```

- [ ] **Step 7: The real hints arm** in `src/ui/hints.rs`, replacing Task 3's minimal one:

```rust
        (Section::BootEnvs, _) => {
            if !app.bootenvs.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if let Some(be) = app.selected_bootenv() {
                if be.can_activate() {
                    v.push(h("a", "Activate", 2).tone(Tone::Warn));
                }
                if be.can_activate_temporarily() {
                    v.push(h("t", "Next boot only", 2));
                }
                if be.can_clear_temporary() {
                    v.push(h("T", "Clear next-boot", 2));
                }
                v.push(h("R", "Rename", 3));
                if be.can_mount() {
                    v.push(h("m", "Mount", 3));
                }
                if be.can_unmount() {
                    v.push(h("u", "Unmount", 3));
                }
                if be.can_destroy() {
                    v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                    v.push(h("D", "Destroy + origin", 3).tone(Tone::Danger));
                }
            }
            if app.bootenvs_unavailable.is_none() {
                v.push(h("n", "New", 2));
            }
            v.push(h("r", "Refresh", 4));
        }
```

- [ ] **Step 8: Run everything**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|^error|panicked"`
Expected: all pass, including the drift guards (`every_advertised_key_actually_does_something`, `keys_that_are_not_advertised_are_no_ops`). If a drift guard fails for a boot-env scenario, the handler and the hint disagree: fix the handler/predicate so they match the hint, not the test. Then `cargo clippy 2>&1 | tail -4` — no new warnings.

- [ ] **Step 9: Commit**

```bash
git add -A src/
git commit -m "feat: Boot Envs keys, forms, clone picker and hints

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Docs and spec amendments

**Files:**
- Modify: `README.md` — sudoers section (lines ~64-72), `## Keys` Tab list (~line 85), new `### Boot Environments` section after `### Shares`/before `### Jobs`.
- Modify: `docs/superpowers/specs/2026-10-07-boot-environments-design.md` — the four deviations.

- [ ] **Step 1: README sudoers.** In the paragraph beginning "`zfsctl` shells out to ten privileged binaries", change `ten` to `eleven` (both occurrences: "ten privileged binaries" and "all ten need the sudoers entry"), add `` `bectl` backs the Boot Envs section, `` to the list of what backs what, add `/sbin/bectl` to the "typically" path list, add `bectl` to the `which ...` command, and add `/sbin/bectl, ` to the example `fbsd ALL=(root) NOPASSWD:` line after `/sbin/geom, `.

- [ ] **Step 2: README keys + section.** In the `Tab` bullet add `Boot Envs` after `Shares` in the section list. Insert before `### Jobs`:

```markdown
### Boot Envs

Lists the host's boot environments (`bectl list`): Name, Active (`Now`, `Reboot`, `Now+Reboot`, `Temp`), Space, Mountpoint and Created; Mountpoint then Created drop on a narrow terminal. If the host has no `bectl` (or is not laid out for boot environments) the section title says `[unavailable: ...]` with the reason, and recovers on its own once it works. Keys act only on what is possible for the selected row, and the hint bar shows exactly those:

- `n`: new boot environment (name; `F2` on the second field picks one to clone, blank clones the running one).
- `a`: make it the boot environment for every future boot (asks first). `t`: use it for the next boot only; `T`: clear that. Neither the running nor the next-boot one is destroyable.
- `R`: rename. `m` / `u`: mount / unmount; the Mountpoint column shows the `/tmp/be_mount.XXXX` path.
- `d`: destroy (type its name). `D`: destroy and remove its origin snapshot (`bectl destroy -o`).

`zfsctl` never reboots the host. bectl's own error text is shown verbatim when it refuses something.
```

- [ ] **Step 3: Amend the spec.** In `## UI` of the spec: change "`r` rename" to "`R` rename"; replace "`m` mount (path shown in the status bar)" with "`m` mount (the path appears in the Mountpoint column after the re-list)"; replace the `d` destroy wording with "`d` destroy and `D` destroy + origin snapshot (`-o`), both with a type-the-name confirm"; delete the bullet "If the BE is mounted, `d` says it will unmount first; ..." and replace it with "Destroying a mounted BE is left to bectl; its own error is shown verbatim."

- [ ] **Step 4: Verify and commit**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED"` — expected pass (docs only; this guards against an accidental edit). Then:

```bash
git add README.md docs/superpowers/specs/2026-10-07-boot-environments-design.md
git commit -m "docs: boot environments; sudoers gains bectl

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Deploy and verify live

Not unit-testable; this is where "looks right but isn't functional" would show ([[feedback-verify-feature-works-end-to-end]]). Deploy is per-file `scp` to `/home/fb/zfsctl` and `cargo build --release` + `cargo test` on the hub ([[project-zfsctl-freebsd-test-box]]); drive the TUI with [[reference-driving-tui-with-pexpect-pyte]]. Use throwaway BE names starting `zit-` and clean them up; never act on `default`.

- [ ] **Step 1: Build and test on the hub.** `scp` every changed file under `src/` and `README.md`, then on the hub: `cargo test 2>&1 | grep "^test result"` and `cargo build --release`. Expected: all pass on FreeBSD 15.1.

- [ ] **Step 2: Local-host walkthrough on the hub (as root, real TUI).** Open Boot Envs: `default` shows `Now+Reboot` mounted at `/`. Then, checking each result with `bectl list` and `zfs list -r zroot/ROOT` (datasets AND origin snapshots really appear/disappear):
  1. `n`, name `zit-a`, blank clone → `zit-a` appears with no flags.
  2. `n`, name `zit-b`, `F2` on Clone-from, pick `zit-a` → created; `zfs get origin zroot/ROOT/zit-b` names `zit-a`'s snapshot.
  3. `R` on `zit-b` → `zit-c`.
  4. `t` on `zit-a` → shows `Temp`; `bectl list` shows `T`; `T` → cleared. (Check `kenv`/`zpool get bootfs zroot` is unchanged for the temporary case; a real reboot is NOT performed.)
  5. `m` on `zit-a` → Mountpoint shows `/tmp/be_mount.*`; `ls` it; `u` → `-`.
  6. `m` then `d` on the mounted `zit-a`: record exactly what bectl does (destroys, or refuses with what message) and write that sentence into the README Boot Envs section.
  7. `d` on `default` and on a BE with `R` → status explains; `D` on `zit-b` → both the dataset and its origin snapshot are gone in `zfs list -t all -r zroot/ROOT`; `d` on `zit-c` → dataset gone.
  8. Do NOT `a`-activate anything real on the hub. Activation is verified on a VM (Step 4).

- [ ] **Step 3: Sudoers on the VMs.** For each of `.60`, `.61`, `.62`: find the drop-in with `grep -l NOPASSWD /usr/local/etc/sudoers.d/* /usr/local/etc/sudoers 2>/dev/null` (as root on the VM), add `/sbin/bectl` after `/sbin/geom,`, check with `visudo -c`, then `sudo -n bectl list -H` as the `fbsd` user must print rows instead of "a password is required".

- [ ] **Step 4: Remote walkthrough and activation.** From the hub's TUI select a VM host, repeat the Step 2 flow over `sudo -n` SSH. Then on that VM only: `a` (confirm) on `zit-a` → `bectl list` shows `R` on it and `N` on the old one; `zpool get bootfs zroot` points at it; flip back with `a` on the original `default`. Do NOT reboot unless the user asks. Remove every `zit-*` BE. Note in the final report that a real reboot into an activated BE was not tested.

- [ ] **Step 5: Unavailable state.** Point a host entry at a machine without bectl (or temporarily run the hub TUI with `PATH` lacking `/sbin`): the title shows `[unavailable: ...]`, `n` does nothing, and the section recovers on its own once bectl is reachable again.

- [ ] **Step 6: Record and finish.** Update the memory notes (status file + MEMORY.md) with what shipped and the live results, then use superpowers:finishing-a-development-branch to merge `boot-environments` into `master` with `--no-ff`, after one fresh whole-branch review on the most capable model (the workflow used for the previous features), applying its fixes first.
