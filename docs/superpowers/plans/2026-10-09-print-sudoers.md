# print-sudoers and check-host Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate and verify the sudoers entry a managed host needs (`zfsctl print-sudoers <host>`, `zfsctl check-host <host>`) from a single registry of every remote command zfsctl runs, with guards so a command cannot be used without being registered.

**Architecture:** A registry table (`src/exec/privilege.rs`) is the single source of truth for remote commands, their feature, and their sudo needs. `SshRunner` refuses unregistered commands and the test `FakeRunner` asserts registration, so drift fails in tests. A second module (`src/exec/sudoers.rs`) holds the pure pieces (user extraction, path vetting, rendering) and the two commands as functions over `&dyn CommandRunner`; `main.rs` adds two thin subcommands. Everything is read-only and uses the normal unprivileged ssh path.

**Tech Stack:** Rust (edition of the crate), existing deps only (`chrono` for the date, `thiserror` `AppError`); no new crates.

**Spec:** `docs/superpowers/specs/2026-10-09-print-sudoers-design.md`

## Global Constraints

- Default feature set is every feature except `restic`; `--with F` adds, `--without F` removes; no inference from `config.toml` or jobs.
- Both subcommands connect with the host's configured unprivileged `ssh_target` (`user@address`). No root ssh, no new privileged code, nothing is ever written to a sudoers file.
- Resolved paths must start with `/bin/`, `/sbin/`, `/usr/bin/`, `/usr/sbin/`, `/usr/local/bin/` or `/usr/local/sbin/`.
- Exit codes: `print-sudoers` 0 on success; `check-host` 0 only if every entry is ok; 1 for a problem, `MISSING` or `TOO BROAD`; 2 if the host is unreachable (ssh exit 255).
- Privileged names: `zpool zfs getfacl setfacl geom bectl service smbpasswd smbstatus cat tee restic`; pinned: `smbstatus -S`, `cat` and `tee` of `zfs::smb::CONF_PATH` (`/usr/local/etc/smb4.conf`).
- Match the surrounding code: terse doc comments that say why, `AppError::Config` for user-facing errors, tests next to the code.

## Review Focus

- `ssh_target` without a `user@` part (a `~/.ssh/config` alias): must be an error and no line printed (Task 3, Task 4).
- A resolved path in a user-writable place (`/home/x/restic`, `/tmp/zfs`, relative): must be refused, never emitted (Task 3, Task 4).
- Host unreachable (ssh exit 255): exit 2 after ONE ssh call, not one 10-second timeout per entry (Task 5).
- `command -v` answering with alias or builtin text instead of a path: must be refused as not-a-path (Task 3).
- Unknown feature name, or the same feature given to both `--with` and `--without`: clear error naming the valid features (Task 1).

---

### Task 1: The registry

**Files:**
- Create: `src/exec/privilege.rs`
- Modify: `src/exec/mod.rs` (add `pub mod privilege;` next to `pub mod local;`)

**Interfaces:**
- Consumes: `crate::error::{AppError, Result}`, `crate::zfs::smb::CONF_PATH`.
- Produces (used by every later task):
  - `pub enum Feature { Core, Acl, Disks, BootEnvs, Shares, Samba, Restic }` with `Feature::ALL: [Feature; 7]`, `fn name(self) -> &'static str`, `fn parse(&str) -> Option<Feature>` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub enum Kind { Privileged { pinned: &'static [&'static [&'static str]], probe: &'static str }, Unprivileged }`
  - `pub struct Entry { pub name: &'static str, pub features: &'static [Feature], pub kind: Kind }`
  - `pub const ENTRIES: &[Entry]`
  - `pub fn is_registered(name: &str) -> bool`
  - `pub fn require_registered(name: &str) -> Result<()>`
  - `pub fn select(with: &[String], without: &[String]) -> Result<Vec<Feature>>`
  - `pub fn privileged_for(features: &[Feature]) -> Vec<&'static Entry>` (registry order)
  - `pub fn unprivileged_for(features: &[Feature]) -> Vec<&'static Entry>`

- [ ] **Step 1: Write the failing tests**

Create `src/exec/privilege.rs` containing only the tests module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn names(entries: Vec<&'static Entry>) -> Vec<&'static str> {
        entries.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn the_default_selection_is_every_feature_but_restic() {
        let all = select(&[], &[]).unwrap();
        assert_eq!(all.len(), 6);
        assert!(!all.contains(&Feature::Restic));
        assert!(all.contains(&Feature::Samba) && all.contains(&Feature::Core));
    }

    #[test]
    fn with_adds_restic_and_without_removes_a_feature() {
        let f = select(&["restic".to_string()], &["samba".to_string()]).unwrap();
        assert!(f.contains(&Feature::Restic) && !f.contains(&Feature::Samba));
    }

    #[test]
    fn an_unknown_feature_is_an_error_naming_the_valid_ones() {
        let err = select(&["nfs".to_string()], &[]).unwrap_err().to_string();
        assert!(err.contains("unknown feature 'nfs'") && err.contains("core, acl, disks, bootenvs, shares, samba, restic"), "{err}");
        assert!(select(&[], &["nope".to_string()]).is_err());
    }

    #[test]
    fn a_feature_both_added_and_removed_is_an_error() {
        let err = select(&["restic".to_string()], &["restic".to_string()]).unwrap_err().to_string();
        assert!(err.contains("both added and removed"), "{err}");
    }

    #[test]
    fn the_default_privileged_set_is_the_eleven_binaries_in_registry_order() {
        let features = select(&[], &[]).unwrap();
        assert_eq!(
            names(privileged_for(&features)),
            ["zpool", "zfs", "getfacl", "setfacl", "geom", "bectl", "service", "smbpasswd", "smbstatus", "cat", "tee"]
        );
    }

    #[test]
    fn service_is_listed_once_when_both_shares_and_samba_are_selected() {
        let n = names(privileged_for(&[Feature::Shares, Feature::Samba]));
        assert_eq!(n.iter().filter(|x| **x == "service").count(), 1, "{n:?}");
        assert_eq!(names(privileged_for(&[Feature::Shares])), ["service"]);
    }

    #[test]
    fn restic_is_privileged_only_when_selected() {
        assert_eq!(names(privileged_for(&[Feature::Restic])), ["restic"]);
        assert!(!names(privileged_for(&select(&[], &[]).unwrap())).contains(&"restic"));
    }

    #[test]
    fn unprivileged_commands_never_appear_among_the_privileged_ones() {
        let all = select(&["restic".to_string()], &[]).unwrap();
        let privileged = names(privileged_for(&all));
        for unprivileged in names(unprivileged_for(&all)) {
            assert!(!privileged.contains(&unprivileged), "{unprivileged}");
        }
        assert!(names(unprivileged_for(&all)).contains(&"getent"));
    }

    #[test]
    fn the_tool_only_commands_are_registered_but_belong_to_no_feature() {
        assert!(is_registered("sh") && is_registered("sudo"));
        let all = select(&["restic".to_string()], &[]).unwrap();
        assert!(!names(unprivileged_for(&all)).contains(&"sh"));
        assert!(!names(unprivileged_for(&all)).contains(&"sudo"));
    }

    #[test]
    fn pinned_entries_carry_a_probe_and_no_pinned_argument_needs_sudoers_escaping() {
        for entry in ENTRIES {
            if let Kind::Privileged { pinned, probe } = &entry.kind {
                assert_eq!(pinned.is_empty(), probe.is_empty(), "{}: a pinned entry needs a probe, an open one has none", entry.name);
                for args in *pinned {
                    for arg in *args {
                        assert!(!arg.contains([' ', ',', '\\', ':', '=', '#']), "{}: {arg:?} would need escaping in sudoers", entry.name);
                    }
                }
            }
        }
    }

    #[test]
    fn require_registered_accepts_known_commands_and_refuses_others() {
        assert!(require_registered("zfs").is_ok());
        let err = require_registered("rm").unwrap_err().to_string();
        assert!(err.contains("rm is not a registered remote command"), "{err}");
    }

    #[test]
    fn no_name_is_registered_twice() {
        for (i, a) in ENTRIES.iter().enumerate() {
            assert!(ENTRIES[i + 1..].iter().all(|b| b.name != a.name), "{} appears twice", a.name);
        }
    }
}
```

Add `pub mod privilege;` to `src/exec/mod.rs` (after `pub mod local;`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test privilege 2>&1 | tail -15`
Expected: compile errors ("cannot find function `select`", "cannot find type `Entry`", ...).

- [ ] **Step 3: Write the implementation**

Put this at the top of `src/exec/privilege.rs`, above the tests module:

```rust
//! Every command zfsctl runs on a managed host, the feature that needs it, and what it needs from sudo.
//! This table is the one source for the generated sudoers line, the host check and the drift guards.

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Core,
    Acl,
    Disks,
    BootEnvs,
    Shares,
    Samba,
    Restic,
}

impl Feature {
    pub const ALL: [Feature; 7] = [Feature::Core, Feature::Acl, Feature::Disks, Feature::BootEnvs, Feature::Shares, Feature::Samba, Feature::Restic];

    pub fn name(self) -> &'static str {
        match self {
            Feature::Core => "core",
            Feature::Acl => "acl",
            Feature::Disks => "disks",
            Feature::BootEnvs => "bootenvs",
            Feature::Shares => "shares",
            Feature::Samba => "samba",
            Feature::Restic => "restic",
        }
    }

    pub fn parse(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }
}

pub enum Kind {
    /// Runs through `sudo -n`. With `pinned` empty any arguments are allowed; otherwise only exactly these
    /// argument lists, and `probe` is one extra argument that sudo must refuse (`check-host` uses it to
    /// notice an entry that is broader than pinned).
    Privileged { pinned: &'static [&'static [&'static str]], probe: &'static str },
    /// Runs without sudo; never in the sudoers line.
    Unprivileged,
}

pub struct Entry {
    pub name: &'static str,
    /// The features that need it. Empty: a command only `print-sudoers` / `check-host` themselves run.
    pub features: &'static [Feature],
    pub kind: Kind,
}

const ANY: Kind = Kind::Privileged { pinned: &[], probe: "" };
const SMB_CONF: Kind = Kind::Privileged { pinned: &[&[crate::zfs::smb::CONF_PATH]], probe: "/etc/master.passwd" };

pub const ENTRIES: &[Entry] = &[
    Entry { name: "zpool", features: &[Feature::Core], kind: ANY },
    Entry { name: "zfs", features: &[Feature::Core], kind: ANY },
    Entry { name: "getfacl", features: &[Feature::Acl], kind: ANY },
    Entry { name: "setfacl", features: &[Feature::Acl], kind: ANY },
    Entry { name: "geom", features: &[Feature::Disks], kind: ANY },
    Entry { name: "bectl", features: &[Feature::BootEnvs], kind: ANY },
    Entry { name: "service", features: &[Feature::Shares, Feature::Samba], kind: ANY },
    Entry { name: "smbpasswd", features: &[Feature::Samba], kind: ANY },
    Entry { name: "smbstatus", features: &[Feature::Samba], kind: Kind::Privileged { pinned: &[&["-S"]], probe: "-v" } },
    Entry { name: "cat", features: &[Feature::Samba], kind: SMB_CONF },
    Entry { name: "tee", features: &[Feature::Samba], kind: SMB_CONF },
    Entry { name: "restic", features: &[Feature::Restic], kind: ANY },
    Entry { name: "getent", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "uname", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "date", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "hostname", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "sysctl", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "swapinfo", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "iostat", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "showmount", features: &[Feature::Shares], kind: Kind::Unprivileged },
    Entry { name: "sh", features: &[], kind: Kind::Unprivileged },
    Entry { name: "sudo", features: &[], kind: Kind::Unprivileged },
];

pub fn lookup(name: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.name == name)
}

pub fn is_registered(name: &str) -> bool {
    lookup(name).is_some()
}

/// What `SshRunner` calls before it sends anything: a command nobody registered would have no sudoers
/// entry generated for it, so it is refused here instead of failing later on a real host.
pub fn require_registered(name: &str) -> Result<()> {
    if is_registered(name) {
        Ok(())
    } else {
        Err(AppError::Config(format!("{name} is not a registered remote command (add it to exec::privilege::ENTRIES)")))
    }
}

/// The features a run covers: all but `restic`, plus `with`, minus `without`.
pub fn select(with: &[String], without: &[String]) -> Result<Vec<Feature>> {
    let parse = |name: &String| {
        Feature::parse(name).ok_or_else(|| {
            let valid = Feature::ALL.map(Feature::name).join(", ");
            AppError::Config(format!("unknown feature '{name}'; valid features: {valid}"))
        })
    };
    let added: Vec<Feature> = with.iter().map(parse).collect::<Result<_>>()?;
    let removed: Vec<Feature> = without.iter().map(parse).collect::<Result<_>>()?;
    if let Some(f) = added.iter().find(|f| removed.contains(f)) {
        return Err(AppError::Config(format!("feature '{}' is both added and removed", f.name())));
    }
    Ok(Feature::ALL.into_iter().filter(|f| (*f != Feature::Restic || added.contains(f)) && !removed.contains(f)).collect())
}

fn serving(entry: &Entry, features: &[Feature]) -> bool {
    entry.features.iter().any(|f| features.contains(f))
}

pub fn privileged_for(features: &[Feature]) -> Vec<&'static Entry> {
    ENTRIES.iter().filter(|e| matches!(e.kind, Kind::Privileged { .. }) && serving(e, features)).collect()
}

pub fn unprivileged_for(features: &[Feature]) -> Vec<&'static Entry> {
    ENTRIES.iter().filter(|e| matches!(e.kind, Kind::Unprivileged) && serving(e, features)).collect()
}
```

If `crate::zfs::smb::CONF_PATH` is not reachable (module visibility), make the `smb` module `pub(crate)`/`pub` in `src/zfs/mod.rs`; do not copy the string.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test privilege 2>&1 | tail -8`
Expected: `test result: ok. 12 passed`

- [ ] **Step 5: Commit**

```bash
git add src/exec/privilege.rs src/exec/mod.rs src/zfs/mod.rs
git commit -m "feat: registry of remote commands with their features and sudo needs

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The guards in the runners

**Files:**
- Modify: `src/exec/ssh.rs` (the `impl CommandRunner for SshRunner` block, ~line 87)
- Modify: `src/exec/mod.rs` (`FakeRunner::run`, in `test_support`)

**Interfaces:**
- Consumes: `privilege::require_registered`, `privilege::is_registered`.
- Produces: `SshRunner` returns `Err(AppError::Config(..))` for an unregistered command before running ssh; the test `FakeRunner` panics when a test issues an unregistered command.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/exec/ssh.rs`:

```rust
    #[test]
    fn an_unregistered_command_is_refused_before_anything_is_sent() {
        let runner = SshRunner::new("host");
        for result in [
            runner.run("rm", &["-rf", "/"]),
            runner.run_unprivileged("rm", &[]),
            runner.run_with_stdin("rm", &[], "x"),
        ] {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("rm is not a registered remote command"), "{err}");
        }
        assert!(runner.command("rm", &[]).is_err());
    }
```

Append to the `tests` module of `src/exec/mod.rs` (or its `test_support` sibling tests; create a `#[cfg(test)] mod guard_tests` at the bottom if there is no suitable module):

```rust
#[cfg(test)]
mod guard_tests {
    use super::test_support::FakeRunner;
    use super::CommandRunner;

    #[test]
    #[should_panic(expected = "not registered")]
    fn the_fake_runner_panics_on_a_command_nobody_registered() {
        let _ = FakeRunner::new("").run("not-a-registered-command", &[]);
    }

    #[test]
    fn the_fake_runner_accepts_registered_commands() {
        assert!(FakeRunner::new("x").run("zpool", &["list"]).is_ok());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test an_unregistered_command_is_refused the_fake_runner 2>&1 | tail -15`
Expected: FAIL (ssh test: `rm` is attempted / `command` returns Ok; fake test: no panic).

- [ ] **Step 3: Implement**

In `src/exec/ssh.rs`, change the `CommandRunner for SshRunner` impl to check first:

```rust
impl CommandRunner for SshRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd)?;
        Self::run_ssh(&self.build_args(cmd, args))
    }

    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd)?;
        Self::run_ssh(&self.build_args_unprivileged(cmd, args))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], stdin: &str) -> Result<CommandOutput> {
        super::privilege::require_registered(cmd)?;
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        super::run_command_with_stdin(command, "ssh", stdin)
    }

    fn command(&self, cmd: &str, args: &[&str]) -> Result<Command> {
        super::privilege::require_registered(cmd)?;
        let mut command = Command::new("ssh");
        command.args(self.build_args(cmd, args));
        Ok(command)
    }
}
```

In `src/exec/mod.rs`, at the top of `FakeRunner::run` (before `self.calls.lock()`), add:

```rust
            assert!(
                crate::exec::privilege::is_registered(cmd),
                "{cmd} is not a registered remote command: add it to exec::privilege::ENTRIES, or use a registered command in this test"
            );
```

- [ ] **Step 4: Run the whole suite and fix the fallout**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED|not a registered" | head -20`

Expected: the two new tests pass. Any other test that panics with "is not a registered remote command" used a made-up command with `FakeRunner`. For each: if the command is one zfsctl really runs on hosts, register it in `ENTRIES` (with the right feature and kind); if it is a placeholder in a runner test (for example `"cmd"` or `"echo"`), change the test to a registered command such as `"zpool"`. Do NOT weaken the assertion. Re-run until `cargo test` is fully green.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "feat: SshRunner refuses unregistered commands; FakeRunner asserts registration

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The pure pieces (user, path vetting, resolution parsing, rendering)

**Files:**
- Create: `src/exec/sudoers.rs`
- Modify: `src/exec/mod.rs` (add `pub mod sudoers;`)

**Interfaces:**
- Consumes: Task 1 (`Entry`, `Feature`, `Kind`, `privileged_for`, `unprivileged_for`).
- Produces:
  - `pub enum Failure { Unreachable(String), Problem(String) }` with `exit_code(&self) -> i32` (2 / 1) and `message(&self) -> &str`
  - `pub struct Resolved { pub entry: &'static Entry, pub path: String }`
  - `pub fn user_of(ssh_target: &str) -> Result<&str, Failure>`
  - `pub fn vet_path(name: &str, path: &str) -> Result<(), Failure>`
  - `pub const RESOLVE_SCRIPT: &str`
  - `pub fn parse_resolution(host: &str, stdout: &str, wanted: &[&'static Entry]) -> Result<Vec<Resolved>, Failure>`
  - `pub fn sudoers_items(resolved: &[Resolved]) -> Vec<String>`
  - `pub fn render(host: &str, user: &str, features: &[Feature], resolved: &[Resolved], generated: &str) -> String`

- [ ] **Step 1: Write the failing tests**

Create `src/exec/sudoers.rs` with only this tests module for now (and add `pub mod sudoers;` to `src/exec/mod.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::privilege::{privileged_for, select};

    fn resolution_for(host: &str, lines: &str, features: &[Feature]) -> Result<Vec<Resolved>, Failure> {
        parse_resolution(host, lines, &privileged_for(features))
    }

    const DEFAULT_LINES: &str = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nsmbpasswd=/usr/local/bin/smbpasswd\nsmbstatus=/usr/local/bin/smbstatus\ncat=/bin/cat\ntee=/usr/bin/tee\n";

    #[test]
    fn the_user_is_the_part_before_the_at_sign() {
        assert_eq!(user_of("fbsd@172.16.10.60"), Ok("fbsd"));
        assert_eq!(user_of("zfs.admin_1@host"), Ok("zfs.admin_1"));
    }

    #[test]
    fn a_target_without_a_user_is_refused_because_the_user_would_be_guessed() {
        for bad in ["fb16", "172.16.10.60", "@host", "-oProxyCommand=x@host", "a b@host", "a,b@host", ""] {
            let err = user_of(bad).unwrap_err();
            assert_eq!(err.exit_code(), 1, "{bad:?}");
        }
        assert!(user_of("fb16").unwrap_err().message().contains("user@"));
    }

    #[test]
    fn system_paths_are_accepted() {
        for p in ["/sbin/zpool", "/bin/cat", "/usr/bin/tee", "/usr/sbin/service", "/usr/local/bin/restic", "/usr/local/sbin/x"] {
            assert!(vet_path("x", p).is_ok(), "{p}");
        }
    }

    #[test]
    fn paths_outside_the_system_directories_are_refused() {
        for p in ["/home/fbsd/restic", "/tmp/zfs", "/var/tmp/zfs", "zfs", "./zfs", "/usr/local/bin/../../tmp/x", "/usr/local/bin/a b", "/usr/local/bin/a,b", "/usr/local/bin/a\\b", ""] {
            assert!(vet_path("restic", p).is_err(), "{p:?} must be refused");
        }
        let err = vet_path("restic", "/home/fbsd/restic").unwrap_err();
        assert!(err.message().contains("restic") && err.message().contains("/home/fbsd/restic"), "{}", err.message());
    }

    #[test]
    fn a_shell_alias_or_builtin_answer_is_not_a_path() {
        let lines = "zpool=alias zpool='rm -rf /'\nzfs=/sbin/zfs\n";
        let wanted: Vec<_> = privileged_for(&[Feature::Core]);
        let err = parse_resolution("h", lines, &wanted).unwrap_err();
        assert!(err.message().contains("zpool"), "{}", err.message());
    }

    #[test]
    fn resolution_parses_name_equals_path_lines_in_registry_order() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("h", DEFAULT_LINES, &features).unwrap();
        assert_eq!(resolved.len(), 11);
        assert_eq!((resolved[0].entry.name, resolved[0].path.as_str()), ("zpool", "/sbin/zpool"));
        assert_eq!((resolved[10].entry.name, resolved[10].path.as_str()), ("tee", "/usr/bin/tee"));
    }

    #[test]
    fn a_missing_command_names_the_host_and_every_missing_command() {
        let lines = DEFAULT_LINES.replace("geom=/sbin/geom\n", "geom=\n").replace("bectl=/sbin/bectl\n", "");
        let err = resolution_for("fbsd16", &lines, &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("fbsd16") && err.message().contains("geom") && err.message().contains("bectl"), "{}", err.message());
    }

    #[test]
    fn the_items_are_paths_with_the_pinned_arguments_written_out() {
        let resolved = resolution_for("h", DEFAULT_LINES, &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(
            sudoers_items(&resolved),
            [
                "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
                "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
            ]
        );
    }

    #[test]
    fn the_rendered_entry_has_the_install_steps_and_one_continued_line() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("fbsd16", DEFAULT_LINES, &features).unwrap();
        let text = render("fbsd16", "fbsd", &features, &resolved, "2026-10-09");
        assert!(text.starts_with("# zfsctl sudoers entry for host fbsd16, generated 2026-10-09\n"), "{text}");
        assert!(text.contains("# features: core, acl, disks, bootenvs, shares, samba\n"), "{text}");
        assert!(text.contains("visudo -c -f /usr/local/etc/sudoers.d/zfsctl") && text.contains("mode 0440"), "{text}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool, \\\n    /sbin/zfs, \\\n"), "{text}");
        assert!(text.trim_end().ends_with("/usr/bin/tee /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.contains("without sudo") && text.contains("getent"), "unprivileged commands are listed in a comment: {text}");
        assert!(!text.contains("restic"), "restic was not selected: {text}");
        assert!(text.contains("as powerful as root"), "{text}");
    }

    #[test]
    fn restic_is_in_the_line_only_when_selected() {
        let features = select(&["restic".to_string()], &["samba".to_string()]).unwrap();
        let lines = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nrestic=/usr/local/bin/restic\n";
        let resolved = resolution_for("h", lines, &features).unwrap();
        let text = render("h", "fbsd", &features, &resolved, "d");
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
        assert!(!text.contains("smbpasswd") && !text.contains("tee"), "{text}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test sudoers 2>&1 | tail -15`
Expected: compile errors (missing `user_of`, `Resolved`, ...).

- [ ] **Step 3: Implement**

Put this above the tests module in `src/exec/sudoers.rs`:

```rust
//! The sudoers entry a managed host needs: generated from the registry (`print-sudoers`) and verified against
//! the host (`check-host`). Pure pieces first; the two commands are below.

use super::privilege::{self, Entry, Feature, Kind};

/// Why a run stopped. `Unreachable` (ssh exit 255) maps to exit code 2 so a script can tell "the host is
/// down" from "the host is misconfigured".
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Unreachable(String),
    Problem(String),
}

impl Failure {
    pub fn exit_code(&self) -> i32 {
        match self {
            Failure::Unreachable(_) => 2,
            Failure::Problem(_) => 1,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Failure::Unreachable(m) | Failure::Problem(m) => m,
        }
    }
}

fn problem(message: impl Into<String>) -> Failure {
    Failure::Problem(message.into())
}

pub struct Resolved {
    pub entry: &'static Entry,
    pub path: String,
}

/// Directories only root can write to. A resolved path anywhere else could be a binary the connecting user
/// planted earlier in `PATH`, and must never become a sudoers entry.
const SYSTEM_DIRS: [&str; 6] = ["/bin/", "/sbin/", "/usr/bin/", "/usr/sbin/", "/usr/local/bin/", "/usr/local/sbin/"];

/// Run as `sh -c RESOLVE_SCRIPT sh <name>...`: prints `name=path` per command (empty path when missing, which
/// keeps the lines aligned with the names). The extended `PATH` matters because a non-interactive ssh session
/// often has only `/usr/bin:/bin`; sudo itself resolves from its own `secure_path`.
pub const RESOLVE_SCRIPT: &str = r#"PATH="$PATH:/sbin:/usr/sbin:/usr/local/sbin:/usr/local/bin"; for c in "$@"; do printf '%s=%s\n' "$c" "$(command -v "$c")"; done"#;

/// The account zfsctl connects as: the part of `ssh_target` before `@`. An alias with no user part is refused:
/// the sudoers line would have to guess whose account it is for.
pub fn user_of(ssh_target: &str) -> Result<&str, Failure> {
    let ok = |user: &str| !user.is_empty() && !user.starts_with('-') && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    match ssh_target.split_once('@') {
        Some((user, _)) if ok(user) => Ok(user),
        _ => Err(problem(format!("ssh_target '{ssh_target}' needs a plain user@host form (letters, digits, . _ -), so the sudoers line knows which account it is for"))),
    }
}

pub fn vet_path(name: &str, path: &str) -> Result<(), Failure> {
    let plain = path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'));
    if plain && !path.contains("..") && SYSTEM_DIRS.iter().any(|dir| path.starts_with(dir)) {
        Ok(())
    } else {
        Err(problem(format!("{name} resolved to '{path}', which is not under a system directory ({}); refusing to put it in a sudoers entry", SYSTEM_DIRS.join(" "))))
    }
}

pub fn parse_resolution(host: &str, stdout: &str, wanted: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let found: std::collections::HashMap<&str, &str> = stdout.lines().filter_map(|line| line.split_once('=')).collect();
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for entry in wanted {
        match found.get(entry.name).copied().filter(|p| !p.is_empty()) {
            Some(path) => {
                vet_path(entry.name, path)?;
                resolved.push(Resolved { entry, path: path.to_string() });
            }
            None => missing.push(entry.name),
        }
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(problem(format!("not found on {host}: {} (install them, or leave out the feature that needs them with --without)", missing.join(", "))))
    }
}

/// One sudoers command spec per allowed form: the bare path for an open entry, `path args` for each pinned one.
pub fn sudoers_items(resolved: &[Resolved]) -> Vec<String> {
    let mut items = Vec::new();
    for r in resolved {
        match &r.entry.kind {
            Kind::Privileged { pinned, .. } if !pinned.is_empty() => items.extend(pinned.iter().map(|args| format!("{} {}", r.path, args.join(" ")))),
            _ => items.push(r.path.clone()),
        }
    }
    items
}

pub fn render(host: &str, user: &str, features: &[Feature], resolved: &[Resolved], generated: &str) -> String {
    let mut text = format!("# zfsctl sudoers entry for host {host}, generated {generated}\n");
    text.push_str(&format!("# features: {}\n", features.iter().map(|f| f.name()).collect::<Vec<_>>().join(", ")));
    if resolved.iter().any(|r| matches!(r.entry.name, "zfs" | "bectl" | "restic")) {
        text.push_str("# zfs, bectl and restic are as powerful as root: grant them only to hosts you trust zfsctl with.\n");
    }
    text.push_str("# Install as /usr/local/etc/sudoers.d/zfsctl (owner root, mode 0440) after checking it:\n#   visudo -c -f /usr/local/etc/sudoers.d/zfsctl\n");
    let unprivileged: Vec<&str> = privilege::unprivileged_for(features).iter().map(|e| e.name).collect();
    if !unprivileged.is_empty() {
        text.push_str(&format!("# These run without sudo (needed only if the ssh key is locked to a forced command): {}\n", unprivileged.join(", ")));
    }
    text.push_str(&format!("{user} ALL=(root) NOPASSWD: \\\n    {}\n", sudoers_items(resolved).join(", \\\n    ")));
    text
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test sudoers 2>&1 | tail -8`
Expected: `test result: ok. 9 passed`

- [ ] **Step 5: Commit**

```bash
git add src/exec/sudoers.rs src/exec/mod.rs
git commit -m "feat: sudoers line rendering, path vetting and resolution parsing

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `print_sudoers` over a runner

**Files:**
- Modify: `src/exec/sudoers.rs`

**Interfaces:**
- Consumes: Task 1 and Task 3 items; `crate::exec::CommandRunner`.
- Produces:
  - `pub fn resolve(runner: &dyn CommandRunner, host: &str, entries: &[&'static Entry]) -> Result<Vec<Resolved>, Failure>`
  - `pub fn print_sudoers(runner: &dyn CommandRunner, host: &str, ssh_target: &str, features: &[Feature], generated: &str) -> Result<String, Failure>`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/exec/sudoers.rs`:

```rust
    use crate::error::Result as AppResult;
    use crate::exec::{CommandOutput, CommandRunner};
    use std::sync::Mutex;

    /// Answers every call through `answer(cmd, args)` and records the call.
    struct Scripted {
        calls: Mutex<Vec<String>>,
        answer: Box<dyn Fn(&str, &[&str]) -> CommandOutput + Send + Sync>,
    }

    impl Scripted {
        fn new(answer: impl Fn(&str, &[&str]) -> CommandOutput + Send + Sync + 'static) -> Self {
            Scripted { calls: Mutex::new(Vec::new()), answer: Box::new(answer) }
        }
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            Ok((self.answer)(cmd, args))
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn out(stdout: &str, status: i32) -> CommandOutput {
        CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status }
    }

    /// `sh -c SCRIPT sh name...` answered from a table of where each command lives.
    fn host_with(paths: &'static [(&'static str, &'static str)]) -> Scripted {
        Scripted::new(move |cmd, args| {
            assert_eq!(cmd, "sh");
            assert_eq!(&args[..3], ["-c", RESOLVE_SCRIPT, "sh"]);
            let lines: String = args[3..].iter().map(|n| format!("{n}={}\n", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p))).collect();
            out(&lines, 0)
        })
    }

    const ALL_PATHS: &[(&str, &str)] = &[
        ("zpool", "/sbin/zpool"), ("zfs", "/sbin/zfs"), ("getfacl", "/bin/getfacl"), ("setfacl", "/bin/setfacl"), ("geom", "/sbin/geom"),
        ("bectl", "/sbin/bectl"), ("service", "/usr/sbin/service"), ("smbpasswd", "/usr/local/bin/smbpasswd"), ("smbstatus", "/usr/local/bin/smbstatus"),
        ("cat", "/bin/cat"), ("tee", "/usr/bin/tee"), ("restic", "/usr/local/bin/restic"),
    ];

    #[test]
    fn print_sudoers_asks_the_host_once_for_exactly_the_selected_commands() {
        let runner = host_with(ALL_PATHS);
        let features = select(&[], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@172.16.10.60", &features, "2026-10-09").unwrap();
        assert_eq!(runner.count(), 1);
        let call = runner.calls.lock().unwrap()[0].clone();
        assert!(call.ends_with("sh zpool zfs getfacl setfacl geom bectl service smbpasswd smbstatus cat tee"), "{call}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool"), "{text}");
        assert!(!text.contains("restic"), "{text}");
    }

    #[test]
    fn with_restic_the_host_is_asked_for_restic_too() {
        let runner = host_with(ALL_PATHS);
        let features = select(&["restic".to_string()], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@h", &features, "d").unwrap();
        assert!(runner.calls.lock().unwrap()[0].ends_with("tee restic"));
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
    }

    #[test]
    fn an_alias_target_is_refused_before_any_ssh_call() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "fb16", "fb16", &select(&[], &[]).unwrap(), "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0, "no line may be produced and nothing may be asked of the host");
    }

    #[test]
    fn a_user_writable_resolution_is_refused_and_no_line_is_printed() {
        let runner = host_with(&[("zpool", "/home/fbsd/bin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert!(err.message().contains("/home/fbsd/bin/zpool"), "{}", err.message());
    }

    #[test]
    fn a_missing_command_stops_the_run_with_exit_1() {
        let runner = host_with(&[("zpool", "/sbin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("zfs"), "{}", err.message());
    }

    #[test]
    fn an_unreachable_host_is_exit_2() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: connect to host h port 22: Operation timed out".to_string(), status: 255 });
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.message().contains("timed out"), "{}", err.message());
    }

    #[test]
    fn selecting_no_privileged_commands_is_a_problem_not_an_empty_line() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test sudoers 2>&1 | tail -15`
Expected: compile error (`print_sudoers` not found).

- [ ] **Step 3: Implement**

Add above the tests module in `src/exec/sudoers.rs` (and `use super::CommandRunner;` to the imports):

```rust
fn unreachable(host: &str, detail: impl std::fmt::Display) -> Failure {
    Failure::Unreachable(format!("cannot reach {host}: {detail}"))
}

/// Asks the host where each command lives, over the unprivileged ssh path (no sudo, so it works before
/// any sudoers entry exists), and vets the answers.
pub fn resolve(runner: &dyn CommandRunner, host: &str, entries: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let mut args: Vec<&str> = vec!["-c", RESOLVE_SCRIPT, "sh"];
    args.extend(entries.iter().map(|e| e.name));
    let out = runner.run_unprivileged("sh", &args).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    if !out.success() {
        return Err(problem(format!("looking up paths on {host} failed: {}", out.stderr.trim())));
    }
    parse_resolution(host, &out.stdout, entries)
}

pub fn print_sudoers(runner: &dyn CommandRunner, host: &str, ssh_target: &str, features: &[Feature], generated: &str) -> Result<String, Failure> {
    let user = user_of(ssh_target)?;
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to put in a sudoers entry"));
    }
    let resolved = resolve(runner, host, &entries)?;
    Ok(render(host, user, features, &resolved, generated))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test sudoers 2>&1 | tail -8`
Expected: `test result: ok. 16 passed`

- [ ] **Step 5: Commit**

```bash
git add src/exec/sudoers.rs
git commit -m "feat: print_sudoers resolves paths on the host and renders the entry

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `check_host`

**Files:**
- Modify: `src/exec/sudoers.rs`

**Interfaces:**
- Consumes: Task 4 `resolve`, `Failure`, `Resolved`; registry `Kind::Privileged { pinned, probe }`.
- Produces:
  - `pub enum Status { Ok, Missing, TooBroad }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct Line { pub name: &'static str, pub detail: String, pub status: Status }`
  - `pub struct Report { pub lines: Vec<Line> }` with `exit_code(&self) -> i32` and `render(&self) -> String`
  - `pub fn check_host(runner: &dyn CommandRunner, host: &str, features: &[Feature]) -> Result<Report, Failure>`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
    /// A host whose sudoers allows exactly `allowed` command lines (path plus arguments, space-joined).
    fn sudo_host(allowed: &'static [&'static str], paths: &'static [(&'static str, &'static str)]) -> Scripted {
        Scripted::new(move |cmd, args| match cmd {
            "sh" => {
                let lines: String = args[3..].iter().map(|n| format!("{n}={}\n", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p))).collect();
                out(&lines, 0)
            }
            "sudo" => {
                assert_eq!(&args[..2], ["-n", "-l"]);
                let asked = args[2..].join(" ");
                out(&asked, if allowed.contains(&asked.as_str()) { 0 } else { 1 })
            }
            other => panic!("unexpected {other}"),
        })
    }

    const EXACT: &[&str] = &[
        "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
        "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
    ];

    fn statuses(report: &Report) -> Vec<(&'static str, Status)> {
        report.lines.iter().map(|l| (l.name, l.status)).collect()
    }

    #[test]
    fn a_host_with_exactly_the_right_entries_is_all_ok() {
        let runner = sudo_host(EXACT, ALL_PATHS);
        let report = check_host(&runner, "fbsd16", &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(report.lines.len(), 11);
        assert!(report.lines.iter().all(|l| l.status == Status::Ok), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_missing_entry_is_reported_by_name_and_fails_the_run() {
        const WITHOUT_GEOM: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(WITHOUT_GEOM, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("geom", Status::Missing)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("MISSING") && report.render().contains("1 missing"), "{}", report.render());
    }

    #[test]
    fn a_plain_cat_entry_is_too_broad() {
        const BROAD_CAT: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/bin/cat /etc/master.passwd", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(BROAD_CAT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("cat", Status::TooBroad)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("TOO BROAD"), "{}", report.render());
    }

    #[test]
    fn restic_is_checked_only_when_selected() {
        let runner = sudo_host(&["/usr/local/bin/restic"], ALL_PATHS);
        let report = check_host(&runner, "h", &select(&["restic".to_string()], &["core", "acl", "disks", "bootenvs", "shares", "samba"].map(String::from)).unwrap()).unwrap();
        assert_eq!(statuses(&report), [("restic", Status::Ok)]);
    }

    #[test]
    fn an_unreachable_host_is_exit_2_after_a_single_ssh_call() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: Could not resolve hostname".to_string(), status: 255 });
        let err = check_host(&runner, "h", &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 1, "one timeout, not one per entry");
    }

    #[test]
    fn ssh_dropping_mid_run_is_exit_2_and_stops_asking() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let runner = Scripted::new(move |cmd, args| {
            if cmd == "sh" {
                let lines: String = args[3..].iter().map(|n| format!("{n}=/sbin/{n}\n")).collect();
                return out(&lines, 0);
            }
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { out("", 0) } else { CommandOutput { stdout: String::new(), stderr: "gone".to_string(), status: 255 } }
        });
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 3, "lookup, one answered check, the one that failed");
    }

    #[test]
    fn a_missing_binary_on_the_host_is_a_problem_before_any_sudo_check() {
        let runner = sudo_host(EXACT, &[("zpool", "/sbin/zpool")]);
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 1);
    }

    #[test]
    fn the_report_lists_one_line_per_entry_with_its_pinned_arguments() {
        let report = check_host(&sudo_host(EXACT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        let text = report.render();
        assert!(text.contains("ok") && text.contains("/bin/cat /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.trim_end().ends_with("11 ok, 0 missing, 0 too broad"), "{text}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test sudoers 2>&1 | tail -15`
Expected: compile error (`check_host`, `Report`, `Status` not found).

- [ ] **Step 3: Implement**

Add above the tests module in `src/exec/sudoers.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Missing,
    TooBroad,
}

pub struct Line {
    pub name: &'static str,
    /// The path and, for a pinned entry, its arguments: what sudoers has to allow.
    pub detail: String,
    pub status: Status,
}

pub struct Report {
    pub lines: Vec<Line>,
}

impl Report {
    pub fn exit_code(&self) -> i32 {
        i32::from(self.lines.iter().any(|l| l.status != Status::Ok))
    }

    pub fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            let label = match line.status {
                Status::Ok => "ok",
                Status::Missing => "MISSING",
                Status::TooBroad => "TOO BROAD",
            };
            text.push_str(&format!("{label:<10}{:<11}{}\n", line.name, line.detail));
        }
        let count = |s: Status| self.lines.iter().filter(|l| l.status == s).count();
        text.push_str(&format!("{} ok, {} missing, {} too broad\n", count(Status::Ok), count(Status::Missing), count(Status::TooBroad)));
        text
    }
}

/// Whether the host's sudo would run `path args` without a password (`sudo -n -l <command>` exits 0).
fn sudo_allows(runner: &dyn CommandRunner, host: &str, path: &str, args: &[&str]) -> Result<bool, Failure> {
    let mut full = vec!["-n", "-l", path];
    full.extend_from_slice(args);
    let out = runner.run_unprivileged("sudo", &full).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    Ok(out.success())
}

pub fn check_host(runner: &dyn CommandRunner, host: &str, features: &[Feature]) -> Result<Report, Failure> {
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to check"));
    }
    let resolved = resolve(runner, host, &entries)?;
    let mut lines = Vec::new();
    for r in &resolved {
        let forms: Vec<(&[&str], &str)> = match &r.entry.kind {
            Kind::Privileged { pinned, probe } if !pinned.is_empty() => pinned.iter().map(|args| (*args, *probe)).collect(),
            _ => vec![(&[][..], "")],
        };
        for (args, probe) in forms {
            // A pinned entry must refuse a different argument too, or the pin is not doing its job.
            let status = if !sudo_allows(runner, host, &r.path, args)? {
                Status::Missing
            } else if !probe.is_empty() && sudo_allows(runner, host, &r.path, &[probe])? {
                Status::TooBroad
            } else {
                Status::Ok
            };
            let detail = if args.is_empty() { r.path.clone() } else { format!("{} {}", r.path, args.join(" ")) };
            lines.push(Line { name: r.entry.name, detail, status });
        }
    }
    Ok(Report { lines })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test sudoers 2>&1 | tail -8`
Expected: `test result: ok. 24 passed`

- [ ] **Step 5: Commit**

```bash
git add src/exec/sudoers.rs
git commit -m "feat: check_host compares a host's sudo against the registry

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The command line

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: Task 4 `print_sudoers`, Task 5 `check_host`, `exec::privilege::select`, `hosts::HostInventory`, `config::Config`.
- Produces: `zfsctl print-sudoers HOST [--with F]... [--without F]...` and `zfsctl check-host HOST [...]`; `Command::PrintSudoers { host, with, without }`, `Command::CheckHost { host, with, without }`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/main.rs`:

```rust
    #[test]
    fn print_sudoers_and_check_host_take_a_host_and_repeatable_flags() {
        assert_eq!(
            parse_args(&args(&["print-sudoers", "fbsd16"])).unwrap(),
            Command::PrintSudoers { host: "fbsd16".to_string(), with: vec![], without: vec![] }
        );
        assert_eq!(
            parse_args(&args(&["check-host", "fbsd16", "--with", "restic", "--without", "samba", "--without", "acl"])).unwrap(),
            Command::CheckHost { host: "fbsd16".to_string(), with: vec!["restic".to_string()], without: vec!["samba".to_string(), "acl".to_string()] }
        );
    }

    #[test]
    fn the_admin_commands_reject_a_missing_host_a_dangling_flag_and_unknown_flags() {
        for bad in [&["print-sudoers"][..], &["check-host"], &["print-sudoers", "h", "--with"], &["print-sudoers", "h", "--wat", "x"], &["print-sudoers", "h", "extra"], &["check-host", "--with", "restic"]] {
            let err = parse_args(&args(bad)).unwrap_err().to_string();
            assert!(err.contains("usage"), "{bad:?}: {err}");
        }
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --bin zfsctl print_sudoers_and_check_host 2>&1 | tail -10`
Expected: compile error (`Command::PrintSudoers` not found).

- [ ] **Step 3: Implement**

In `src/main.rs`, extend the enum and parser:

```rust
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Tui,
    Daemon { state_dir: PathBuf },
    PrintSudoers { host: String, with: Vec<String>, without: Vec<String> },
    CheckHost { host: String, with: Vec<String>, without: Vec<String> },
}

const USAGE: &str = "usage: zfsctl [daemon [--state-dir DIR] | print-sudoers HOST [--with FEATURE] [--without FEATURE] | check-host HOST [--with FEATURE] [--without FEATURE]]";

/// `HOST [--with F]... [--without F]...` for the two admin commands.
fn parse_host_flags(rest: &[String]) -> Option<(String, Vec<String>, Vec<String>)> {
    let (host, flags) = rest.split_first()?;
    if host.starts_with('-') {
        return None;
    }
    let (mut with, mut without) = (Vec::new(), Vec::new());
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let value = it.next()?.clone();
        match flag.as_str() {
            "--with" => with.push(value),
            "--without" => without.push(value),
            _ => return None,
        }
    }
    Some((host.clone(), with, without))
}

fn parse_args(args: &[String]) -> error::Result<Command> {
    let usage = || error::AppError::Config(USAGE.to_string());
    match args {
        [] => Ok(Command::Tui),
        [cmd] if cmd == "daemon" => Ok(Command::Daemon { state_dir: PathBuf::from(daemon::engine::DEFAULT_STATE_DIR) }),
        [cmd, flag, dir] if cmd == "daemon" && flag == "--state-dir" => Ok(Command::Daemon { state_dir: PathBuf::from(dir) }),
        [cmd, rest @ ..] if cmd == "print-sudoers" => parse_host_flags(rest).map(|(host, with, without)| Command::PrintSudoers { host, with, without }).ok_or_else(usage),
        [cmd, rest @ ..] if cmd == "check-host" => parse_host_flags(rest).map(|(host, with, without)| Command::CheckHost { host, with, without }).ok_or_else(usage),
        _ => Err(usage()),
    }
}
```

Add the runner for the two commands and call it from `run()` right after the daemon branch (before `Config::load` for the TUI):

```rust
/// `print-sudoers` / `check-host`: read-only, over the host's normal unprivileged ssh path. Returns the exit code.
fn admin(command: &Command, config: &config::Config) -> error::Result<i32> {
    let (host, with, without, check) = match command {
        Command::PrintSudoers { host, with, without } => (host, with, without, false),
        Command::CheckHost { host, with, without } => (host, with, without, true),
        _ => unreachable!("admin() is only called for the two admin commands"),
    };
    let features = exec::privilege::select(with, without)?;
    let inventory = hosts::HostInventory::from_config(config);
    let Some(found) = inventory.get(host) else {
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        return Err(error::AppError::Config(format!("unknown host '{host}'; configured hosts: {}", names.join(", "))));
    };
    let Some(target) = found.ssh_target.as_deref() else {
        println!("{host} is the hub: zfsctl runs as root here, so there is nothing to configure.");
        return Ok(0);
    };
    let result = if check {
        exec::sudoers::check_host(found.runner.as_ref(), host, &features).map(|report| (report.render(), report.exit_code()))
    } else {
        let generated = chrono::Utc::now().format("%Y-%m-%d").to_string();
        exec::sudoers::print_sudoers(found.runner.as_ref(), host, target, &features, &generated).map(|text| (text, 0))
    };
    match result {
        Ok((text, code)) => {
            print!("{text}");
            Ok(code)
        }
        Err(failure) => {
            eprintln!("error: {}", failure.message());
            Ok(failure.exit_code())
        }
    }
}
```

In `run()`, after the `if let Command::Daemon { .. }` block and before the TUI `Config::load`:

```rust
    if matches!(command, Command::PrintSudoers { .. } | Command::CheckHost { .. }) {
        let config = config::Config::load(&config_path)?;
        let code = admin(&command, &config)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED"`; `cargo build 2>&1 | tail -3`
Expected: all green; the binary builds.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: zfsctl print-sudoers HOST and check-host HOST

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Drift guards and the README

**Files:**
- Modify: `src/exec/privilege.rs` (two tests)
- Modify: `README.md` (the "Remote host setup" section)

**Interfaces:**
- Consumes: `privilege::{ENTRIES, Kind, is_registered}`.
- Produces: tests `every_command_the_code_runs_is_registered`, `the_readme_names_every_privileged_command`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/exec/privilege.rs`:

```rust
    /// The first string literal after `(` of a run-style call, skipping `run_checked`'s leading runner argument.
    fn commands_in(src: &str) -> Vec<String> {
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        let mut found = Vec::new();
        for (marker, skip_runner) in [("run_checked(", true), (".run(", false), (".run_unprivileged(", false), (".run_with_stdin(", false), (".command(", false)] {
            for (at, _) in code.match_indices(marker) {
                let mut rest = &code[at + marker.len()..];
                if skip_runner {
                    match rest.find(',') {
                        Some(comma) => rest = &rest[comma + 1..],
                        None => continue,
                    }
                }
                let rest = rest.trim_start();
                if let Some(literal) = rest.strip_prefix('"') {
                    if let Some(end) = literal.find('"') {
                        let name = &literal[..end];
                        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-')) {
                            found.push(name.to_string());
                        }
                    }
                }
            }
        }
        found
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_command_the_code_runs_is_registered() {
        let mut files = Vec::new();
        rust_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut seen = std::collections::BTreeSet::new();
        for file in &files {
            for name in commands_in(&std::fs::read_to_string(file).unwrap()) {
                assert!(is_registered(&name), "{} runs '{name}', which is not in exec::privilege::ENTRIES", file.display());
                seen.insert(name);
            }
        }
        // If the scan stops finding the known commands, it has silently stopped guarding anything.
        for known in ["zpool", "zfs", "getfacl", "setfacl", "geom", "bectl", "service", "smbpasswd", "smbstatus", "cat", "tee", "restic", "getent", "sysctl"] {
            assert!(seen.contains(known), "the scan no longer finds '{known}': {seen:?}");
        }
    }

    #[test]
    fn the_readme_names_every_privileged_command() {
        let readme = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
        let start = readme.find("### Remote host setup").expect("the README has a Remote host setup section");
        let section = &readme[start..];
        let section = &section[..section.find("\n## ").unwrap_or(section.len())];
        for entry in ENTRIES {
            if matches!(entry.kind, Kind::Privileged { .. }) {
                assert!(section.contains(&format!("`{}`", entry.name)) || section.contains(&format!(" {} ", entry.name)) || section.contains(&format!("/{}", entry.name)), "the README's sudoers section does not mention {}", entry.name);
            }
        }
        assert!(section.contains("print-sudoers") && section.contains("check-host"), "the README must point to the generated entry");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test every_command the_readme_names 2>&1 | tail -15`
Expected: `the_readme_names_every_privileged_command` FAILS ("must point to the generated entry"). `every_command_the_code_runs_is_registered` should PASS if Task 2 registered everything; if it fails, it names the file and the unregistered command: register it in `ENTRIES` (right feature and kind) and re-run.

- [ ] **Step 3: Update the README**

In `README.md`, directly under the `### Remote host setup: unprivileged SSH + sudo` heading and its first explanatory paragraph(s), insert before the "zfsctl shells out to eleven privileged binaries" paragraph:

```markdown
**Let zfsctl write and check the entry.** On the hub, as root:

```sh
zfsctl print-sudoers fbsd16              # the exact sudoers line for that host, paths resolved on the host
zfsctl print-sudoers fbsd16 --with restic --without samba
zfsctl check-host fbsd16                 # ok / MISSING / TOO BROAD per entry; exit 0 only if all ok (2 = unreachable)
```

Both connect with the host's normal unprivileged `ssh_target` and only read: they never write a sudoers file. `print-sudoers` resolves each binary's real path on the host, refuses a path outside the system directories, and prints the line plus the `visudo -c -f` check to run before installing it as `/usr/local/etc/sudoers.d/zfsctl`. The default covers every feature except `restic` (`--with restic` adds it; `--without acl` etc. removes one). `check-host` asks the host's sudo what it would allow and also tries a wrong argument on the pinned `cat`/`tee`/`smbstatus` entries, so a plain `cat` entry is reported as TOO BROAD. The manual list below is what these commands generate, kept for reference.
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test 2>&1 | grep -E "^test result|FAILED"; cargo clippy --all-targets 2>&1 | grep -c "^warning"`
Expected: all tests pass; clippy warning count equals the baseline of 11 (no new warnings from this branch — run `git stash; cargo clippy ...; git stash pop` only if the count differs, and fix what the branch added).

- [ ] **Step 5: Commit**

```bash
git add src/exec/privilege.rs README.md
git commit -m "test: drift guards for registered commands and the README; document print-sudoers

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Live verification on the real hosts

**Files:** none (verification only; cleanup of anything created).

**Interfaces:** consumes the built binary on the hub.

- [ ] **Step 1: Deploy and build on the hub**

```bash
cd /home/me/dev/rust/zfsctl
tar czf - Cargo.toml Cargo.lock clippy.toml src README.md | ssh -o BatchMode=yes fb@172.16.10.90 'cd zfsctl && rm -rf src && tar xzf - && cargo build --release 2>&1 | tail -2'
```
Expected: `Finished release`. (Do not install it to `/usr/local/bin`; run `~/zfsctl/target/release/zfsctl` as root through `sudo`.)

- [ ] **Step 2: print-sudoers on each host and compare with what is installed**

```bash
for h in fbsd16 fbsd16t1 fbsd16t2; do echo "=== $h"; ssh -o BatchMode=yes fb@172.16.10.90 "sudo ./zfsctl/target/release/zfsctl print-sudoers $h --with restic"; done
```
Expected: for each host a header, a `visudo -c -f` line and one continued `fbsd ALL=(root) NOPASSWD:` line listing `/sbin/zpool` … `/usr/local/bin/restic` with `smbstatus -S` and `cat`/`tee` pinned to `/usr/local/etc/smb4.conf`. Compare with `sudo -l` on the host (`ssh fb@hub "sudo ssh fbsd@<ip> 'sudo -n -l'"` using the unprivileged user, never root): every printed path appears in the installed entry.

- [ ] **Step 3: check-host**

```bash
for h in fbsd16 fbsd16t1 fbsd16t2; do echo "=== $h"; ssh -o BatchMode=yes fb@172.16.10.90 "sudo ./zfsctl/target/release/zfsctl check-host $h --with restic"; echo "exit $?"; done
```
Expected: `ok` for every entry and `exit 0` where all are installed. If a host reports `MISSING` for a feature it genuinely lacks (for example `smbpasswd` where Samba is not installed), that is the tool working: note it and re-run with `--without samba`. `TOO BROAD` anywhere is a real finding: report it to the user, do not fix sudoers yourself.

- [ ] **Step 4: The error paths on the real system**

```bash
ssh -o BatchMode=yes fb@172.16.10.90 'sudo ./zfsctl/target/release/zfsctl print-sudoers local; echo "exit $?"; sudo ./zfsctl/target/release/zfsctl print-sudoers nosuchhost; echo "exit $?"; sudo ./zfsctl/target/release/zfsctl print-sudoers fbsd16 --with nfs; echo "exit $?"; sudo ./zfsctl/target/release/zfsctl print-sudoers fbsd16 --with restic --without restic; echo "exit $?"'
```
Expected: the "hub ... nothing to configure" message (exit 0); `unknown host 'nosuchhost'; configured hosts: ...` (exit 1); `unknown feature 'nfs'; valid features: ...` (exit 1); `both added and removed` (exit 1).

- [ ] **Step 5: TOO BROAD and MISSING against a real sudoers change (the operator does the root step)**

Ask the user to run, as root on `fbsd16` only (never via ssh root):

```sh
echo 'fbsd ALL=(root) NOPASSWD: /bin/cat' > /usr/local/etc/sudoers.d/zfsctl-broadtest && chmod 440 /usr/local/etc/sudoers.d/zfsctl-broadtest && visudo -c -f /usr/local/etc/sudoers.d/zfsctl-broadtest
```

Then run `check-host fbsd16 --without core,acl,disks,bootenvs,shares` (use repeated `--without`) and expect `cat` to show `TOO BROAD`. Ask the user to remove the drop-in (`rm /usr/local/etc/sudoers.d/zfsctl-broadtest`) and re-run to see `ok` again. Also run once with an unreachable address: `sudo ./zfsctl/target/release/zfsctl check-host <a host whose ssh_target is down>` is not available in the real inventory, so this case stays covered by the unit test.

- [ ] **Step 6: Whole-branch gate and report**

```bash
cd /home/me/dev/rust/zfsctl && cargo test 2>&1 | grep -E "^test result|FAILED"; cargo clippy --all-targets 2>&1 | grep -c "^warning"
```
Expected: all tests pass; warnings equal the baseline (11). Report to the user: the generated line versus the installed entries per host, any MISSING/TOO BROAD found, and that the branch `print-sudoers` is ready to merge on their word. Do not merge.
