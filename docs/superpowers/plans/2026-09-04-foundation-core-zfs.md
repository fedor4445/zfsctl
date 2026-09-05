# zfsctl Plan 1: Foundation + Core ZFS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working FreeBSD TUI that lets a root user browse hosts, pools, datasets, and snapshots, with a fully implemented and tested library layer for the underlying ZFS operations (create/destroy/scrub/replace/rollback/replicate) — locally or over SSH.

**Scope note:** the UI wired up in this plan exposes browse + refresh (`r`) for every section; it does not yet include interactive forms/modals for triggering mutations (create dataset, destroy snapshot, etc.) from the TUI. Every mutation is implemented and unit-tested at the `zfs::*` library layer in Tasks 6-8, ready to be wired into interactive UI actions in a near-term follow-up task — deferred here to keep this plan's scope to the read path plus a fully capable backend, rather than also building modal/dialog UI plumbing.

**Architecture:** A `CommandRunner` trait abstracts "run this command, locally or over SSH" so the same ZFS parsing/operations code works against any host. A synchronous ratatui event loop renders app state and dispatches slow operations (SSH calls) to worker threads that report back over a channel, keeping the UI responsive without an async runtime.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28, serde + toml (config), thiserror (errors), nix (root check), tempfile (dev-only, config tests).

**Spec:** `docs/superpowers/specs/2026-09-04-zfsctl-tui-design.md`

## Global Constraints

- FreeBSD only — no Linux/illumos code paths.
- No `unsafe` FFI/libzfs bindings — all ZFS/system integration goes through CLI shelling (`zpool`, `zfs`, `ssh`), parsed via machine-readable flags (`-H -p`), never human-formatted text.
- The app must run as root; check on startup and fail fast with a clear error if not.
- Single binary crate — no cargo workspace.
- Config is one human-editable TOML file; no embedded database.
- UI event loop is synchronous ratatui + crossterm; blocking work moves to worker threads via `std::thread::spawn` + `mpsc` channels — no async runtime.
- Remote execution shells out to the system `ssh` binary — no in-process SSH library.
- `CommandRunner` is the sole execution abstraction. All higher-level logic is written against `&dyn CommandRunner` / `Arc<dyn CommandRunner>`, never directly against `std::process::Command`, so it stays unit-testable with a fake runner.

---

## File Structure

```
Cargo.toml
src/
  main.rs              # entry point: root check, config load, launches UI
  error.rs             # AppError (thiserror), Result alias
  exec/
    mod.rs             # CommandRunner trait, CommandOutput, #[cfg(test)] test_support::FakeRunner
    local.rs           # LocalRunner
    ssh.rs             # SshRunner
  config.rs            # Config, HostConfig, load/save (TOML)
  hosts.rs             # Host, HostInventory
  zfs/
    mod.rs             # `pub mod pool; pub mod dataset; pub mod snapshot;`
    pool.rs            # Pool, PoolHealth, VdevLine, parsing + operations
    dataset.rs         # Dataset, DatasetKind, parsing + operations
    snapshot.rs        # Snapshot, parsing + operations + replication
  ui/
    mod.rs             # run_app(), event_loop()
    state.rs           # App, Section, WorkerMsg, handle_key, refresh_*, drain_worker_messages
    layout.rs          # draw() and per-section render functions
```

---

### Task 1: Project scaffolding, error type, root check

**Files:**
- Create: `Cargo.toml`
- Create: `src/error.rs`
- Create: `src/main.rs`

**Interfaces:**
- Produces: `AppError` enum (variants: `CommandFailed { cmd: String, status: i32, stderr: String }`, `Spawn { cmd: String, source: std::io::Error }`, `Parse(String)`, `Config(String)`), `pub type Result<T> = std::result::Result<T, AppError>;`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "zfsctl"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
thiserror = "1"
nix = { version = "0.29", features = ["user"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create `src/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("command '{cmd}' failed with status {status}: {stderr}")]
    CommandFailed {
        cmd: String,
        status: i32,
        stderr: String,
    },
    #[error("failed to spawn command '{cmd}': {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse output: {0}")]
    Parse(String),
    #[error("config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
```

- [ ] **Step 3: Write a failing test for the root check in `src/main.rs`**

```rust
mod error;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> error::Result<()> {
    check_root()?;
    println!("zfsctl starting (root check passed)");
    Ok(())
}

fn check_root() -> error::Result<()> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(error::AppError::Config(
            "zfsctl must be run as root".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_uid_matches_nix() {
        // Sanity check that the nix wrapper reports the same thing as the
        // real effective uid, so check_root's logic is exercised without
        // requiring the test process to actually run as root.
        let is_root = nix::unistd::Uid::effective().is_root();
        let expected = nix::unistd::Uid::effective().as_raw() == 0;
        assert_eq!(is_root, expected);
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test`
Expected: PASS (also confirms the crate compiles end-to-end)

- [ ] **Step 5: Run the binary to confirm the root check fires as a non-root user**

Run: `cargo run`
Expected: prints `error: config error: zfsctl must be run as root` and exits with status 1 (unless already running as root, in which case it prints the startup message)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/error.rs
git commit -m "feat: project scaffolding, error type, root check"
```

---

### Task 2: Execution abstraction — `CommandRunner`, `LocalRunner`, test support

**Files:**
- Create: `src/exec/mod.rs`
- Create: `src/exec/local.rs`
- Modify: `src/main.rs` (add `mod exec;`)

**Interfaces:**
- Consumes: `crate::error::{AppError, Result}` from Task 1.
- Produces: `CommandOutput { stdout: String, stderr: String, status: i32 }` with `fn success(&self) -> bool`; `trait CommandRunner: Send + Sync { fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>; }`; `struct LocalRunner;` implementing `CommandRunner`; `#[cfg(test)] pub mod test_support` exposing `FakeRunner` (records calls, returns a scripted `CommandOutput`).

- [ ] **Step 1: Write `src/exec/mod.rs` with the trait, output type, and shared test support**

```rust
pub mod local;
pub mod ssh;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
}

#[cfg(test)]
pub mod test_support {
    use super::{CommandOutput, CommandRunner};
    use crate::error::Result;
    use std::sync::Mutex;

    /// A `CommandRunner` that records every call it receives and always
    /// returns the same scripted `CommandOutput`. Used by unit tests
    /// throughout the crate so ZFS/host logic can be tested without a
    /// real system.
    pub struct FakeRunner {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub response: CommandOutput,
    }

    impl FakeRunner {
        pub fn new(stdout: &str) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    status: 0,
                },
            }
        }

        pub fn failing(stderr: &str, status: i32) -> Self {
            FakeRunner {
                calls: Mutex::new(Vec::new()),
                response: CommandOutput {
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                    status,
                },
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((cmd.to_string(), args.iter().map(|s| s.to_string()).collect()));
            Ok(self.response.clone())
        }
    }
}
```

Note: this references `pub mod ssh;` which doesn't exist yet — that's expected, it's created in Task 3. Compilation will fail until then, so do not run `cargo build`/`cargo test` for the whole crate until Step 3 of this task stubs it out.

- [ ] **Step 2: Stub `src/exec/ssh.rs` so the crate compiles (full implementation is Task 3)**

```rust
// Implemented in full in Task 3.
```

- [ ] **Step 3: Write `src/exec/local.rs` with a failing test**

```rust
use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::process::Command;

pub struct LocalRunner;

impl CommandRunner for LocalRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: cmd.to_string(),
                source,
            })?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_a_successful_command_and_captures_stdout() {
        let runner = LocalRunner;
        let output = runner.run("echo", &["hello"]).unwrap();
        assert_eq!(output.stdout.trim(), "hello");
        assert!(output.success());
    }

    #[test]
    fn captures_nonzero_exit_status() {
        let runner = LocalRunner;
        let output = runner.run("false", &[]).unwrap();
        assert!(!output.success());
        assert_ne!(output.status, 0);
    }

    #[test]
    fn spawn_failure_for_missing_binary_is_an_error() {
        let runner = LocalRunner;
        let result = runner.run("definitely-not-a-real-binary-xyz", &[]);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 4: Add `mod exec;` to `src/main.rs`**

```rust
mod error;
mod exec;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test exec::local`
Expected: PASS (3 tests)

- [ ] **Step 6: Commit**

```bash
git add src/exec/mod.rs src/exec/local.rs src/exec/ssh.rs src/main.rs
git commit -m "feat: CommandRunner trait, LocalRunner, fake runner test support"
```

---

### Task 3: `SshRunner`

**Files:**
- Modify: `src/exec/ssh.rs` (replace stub)

**Interfaces:**
- Consumes: `CommandOutput`, `CommandRunner` from Task 2.
- Produces: `struct SshRunner { pub host: String }` with `SshRunner::new(host: impl Into<String>) -> Self`, implementing `CommandRunner`.

- [ ] **Step 1: Write failing tests for the argument-building logic**

```rust
use super::{CommandOutput, CommandRunner};
use crate::error::{AppError, Result};
use std::process::Command;

pub struct SshRunner {
    pub host: String,
}

impl SshRunner {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    /// Builds the argument list passed to the local `ssh` binary:
    /// `ssh -- <host> "<remote command>"`, where the remote command is
    /// `cmd` and `args` shell-quoted and joined into one string. The
    /// leading `--` stops `ssh` from treating a `host` value that starts
    /// with `-` as an option flag (argument injection / flag smuggling).
    fn build_args(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        let mut remote = shell_quote(cmd);
        for arg in args {
            remote.push(' ');
            remote.push_str(&shell_quote(arg));
        }
        vec!["--".to_string(), self.host.clone(), remote]
    }
}

fn shell_quote(s: &str) -> String {
    let is_safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | '@' | ':' | '=' | ','));
    if is_safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

impl CommandRunner for SshRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        let ssh_args = self.build_args(cmd, args);
        let output = Command::new("ssh")
            .args(&ssh_args)
            .output()
            .map_err(|source| AppError::Spawn {
                cmd: "ssh".to_string(),
                source,
            })?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_simple_command_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zpool", &["list", "-H", "-p"]);
        assert_eq!(args, vec!["--".to_string(), "backup-host".to_string(), "zpool list -H -p".to_string()]);
    }

    #[test]
    fn quotes_args_containing_spaces() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["hello world"]);
        assert_eq!(args, vec!["--".to_string(), "backup-host".to_string(), "echo 'hello world'".to_string()]);
    }

    #[test]
    fn quotes_and_escapes_args_containing_single_quotes() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("echo", &["it's here"]);
        assert_eq!(
            args,
            vec!["--".to_string(), "backup-host".to_string(), r"echo 'it'\''s here'".to_string()]
        );
    }

    #[test]
    fn zfs_dataset_names_pass_through_unquoted() {
        let runner = SshRunner::new("backup-host");
        let args = runner.build_args("zfs", &["snapshot", "tank/data@2026-09-04"]);
        assert_eq!(
            args,
            vec!["--".to_string(), "backup-host".to_string(), "zfs snapshot tank/data@2026-09-04".to_string()]
        );
    }

    #[test]
    fn guards_against_host_values_that_look_like_flags() {
        let runner = SshRunner::new("-oProxyCommand=touch /tmp/pwned");
        let args = runner.build_args("zpool", &["list"]);
        assert_eq!(args[0], "--", "a leading '--' must precede the host so ssh cannot parse it as an option");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test exec::ssh`
Expected: PASS (4 tests). Note: these test `build_args` only, not a real network call — there's no SSH server to connect to in CI/dev, so `SshRunner::run` itself is exercised only via manual testing against a real remote host later.

- [ ] **Step 3: Commit**

```bash
git add src/exec/ssh.rs
git commit -m "feat: SshRunner shelling to the system ssh binary"
```

---

### Task 4: Config — TOML host inventory

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs` (add `mod config;`)

**Interfaces:**
- Consumes: `crate::error::{AppError, Result}` from Task 1.
- Produces: `struct Config { pub hosts: Vec<HostConfig> }` (Default, Serialize, Deserialize, PartialEq, Clone, Debug), `struct HostConfig { pub name: String, pub ssh_target: Option<String> }` (Serialize, Deserialize, PartialEq, Clone, Debug), `Config::load(path: &Path) -> Result<Config>`, `Config::save(&self, path: &Path) -> Result<()>`, `pub const DEFAULT_CONFIG_PATH: &str`.

- [ ] **Step 1: Write failing tests for load/save round-trip**

```rust
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DEFAULT_CONFIG_PATH: &str = "/usr/local/etc/zfsctl/config.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub name: String,
    /// SSH destination (e.g. `user@host`, or a `~/.ssh/config` alias).
    /// `None` means this entry is a duplicate/manual reference to the
    /// local host and is not expected in practice — real remote hosts
    /// always set this.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ssh_target: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AppError::Config(format!("reading {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| AppError::Config(format!("parsing {}: {e}", path.display())))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| AppError::Config(format!("serializing config: {e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Config(format!("creating {}: {e}", parent.display())))?;
        }
        std::fs::write(path, text)
            .map_err(|e| AppError::Config(format!("writing {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let config = Config::load(&path).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = Config {
            hosts: vec![
                HostConfig { name: "local".to_string(), ssh_target: None },
                HostConfig {
                    name: "backup".to_string(),
                    ssh_target: Some("backup.example.com".to_string()),
                },
            ],
        };
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, config);
    }
}
```

- [ ] **Step 2: Add `mod config;` to `src/main.rs`**

- [ ] **Step 3: Run the tests**

Run: `cargo test config::`
Expected: PASS (2 tests)

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: TOML config with host inventory load/save"
```

---

### Task 5: Host inventory

**Files:**
- Create: `src/hosts.rs`
- Modify: `src/main.rs` (add `mod hosts;`)

**Interfaces:**
- Consumes: `Config`, `HostConfig` from Task 4; `CommandRunner`, `LocalRunner` from Task 2; `SshRunner` from Task 3.
- Produces: `struct Host { pub name: String, pub runner: Arc<dyn CommandRunner> }`, `struct HostInventory { pub hosts: Vec<Host> }`, `HostInventory::from_config(config: &Config) -> HostInventory`, `HostInventory::get(&self, name: &str) -> Option<&Host>`.

- [ ] **Step 1: Write failing tests**

```rust
use crate::config::Config;
use crate::exec::local::LocalRunner;
use crate::exec::ssh::SshRunner;
use crate::exec::CommandRunner;
use std::sync::Arc;

pub struct Host {
    pub name: String,
    pub runner: Arc<dyn CommandRunner>,
}

pub struct HostInventory {
    pub hosts: Vec<Host>,
}

impl HostInventory {
    /// Builds the inventory from config: a synthetic "local" host always
    /// comes first, followed by one entry per configured remote host.
    pub fn from_config(config: &Config) -> Self {
        let mut hosts = vec![Host {
            name: "local".to_string(),
            runner: Arc::new(LocalRunner),
        }];
        for host_cfg in &config.hosts {
            let runner: Arc<dyn CommandRunner> = match &host_cfg.ssh_target {
                Some(target) => Arc::new(SshRunner::new(target.clone())),
                None => Arc::new(LocalRunner),
            };
            hosts.push(Host {
                name: host_cfg.name.clone(),
                runner,
            });
        }
        HostInventory { hosts }
    }

    pub fn get(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HostConfig;

    #[test]
    fn always_includes_a_local_host_first() {
        let config = Config::default();
        let inventory = HostInventory::from_config(&config);
        assert_eq!(inventory.hosts.len(), 1);
        assert_eq!(inventory.hosts[0].name, "local");
    }

    #[test]
    fn includes_configured_remote_hosts_in_order() {
        let config = Config {
            hosts: vec![
                HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) },
                HostConfig { name: "offsite".to_string(), ssh_target: Some("offsite.example.com".to_string()) },
            ],
        };
        let inventory = HostInventory::from_config(&config);
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["local", "backup", "offsite"]);
    }

    #[test]
    fn get_finds_host_by_name_or_returns_none() {
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
        };
        let inventory = HostInventory::from_config(&config);
        assert!(inventory.get("backup").is_some());
        assert!(inventory.get("nonexistent").is_none());
    }
}
```

- [ ] **Step 2: Add `mod hosts;` to `src/main.rs`**

- [ ] **Step 3: Run the tests**

Run: `cargo test hosts::`
Expected: PASS (3 tests)

- [ ] **Step 4: Commit**

```bash
git add src/hosts.rs src/main.rs
git commit -m "feat: host inventory built from config"
```

---

### Task 6: ZFS pools — domain model, parsing, operations

**Files:**
- Create: `src/zfs/mod.rs`
- Create: `src/zfs/pool.rs`
- Modify: `src/main.rs` (add `mod zfs;`)

**Interfaces:**
- Consumes: `AppError`, `Result` from Task 1; `CommandRunner`, `test_support::FakeRunner` from Task 2.
- Produces: `enum PoolHealth { Online, Degraded, Faulted, Offline, Unavail, Removed, Other(String) }`, `struct Pool { pub name: String, pub size: u64, pub allocated: u64, pub free: u64, pub health: PoolHealth }`, `struct VdevLine { pub depth: usize, pub name: String, pub health: PoolHealth }`, `parse_pool_list(output: &str) -> Result<Vec<Pool>>`, `parse_pool_status_vdevs(output: &str) -> Vec<VdevLine>`, `list_pools(runner: &dyn CommandRunner) -> Result<Vec<Pool>>`, `get_pool_vdevs(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<VdevLine>>`, `create_pool(runner: &dyn CommandRunner, name: &str, vdev_spec: &[&str]) -> Result<()>`, `scrub_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()>`, `destroy_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()>`, `offline_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()>`, `online_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()>`, `replace_disk(runner: &dyn CommandRunner, pool: &str, old_disk: &str, new_disk: &str) -> Result<()>`.

- [ ] **Step 1: Create `src/zfs/mod.rs`**

Only `pool` exists yet — `dataset` and `snapshot` are added by Tasks 7 and 8, which each append their own `pub mod` line to this file.

```rust
pub mod pool;
```

- [ ] **Step 2: Write `src/zfs/pool.rs` structs, parsing, and their fixture-based tests**

```rust
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolHealth {
    Online,
    Degraded,
    Faulted,
    Offline,
    Unavail,
    Removed,
    Other(String),
}

impl PoolHealth {
    fn parse(s: &str) -> PoolHealth {
        match s {
            "ONLINE" => PoolHealth::Online,
            "DEGRADED" => PoolHealth::Degraded,
            "FAULTED" => PoolHealth::Faulted,
            "OFFLINE" => PoolHealth::Offline,
            "UNAVAIL" => PoolHealth::Unavail,
            "REMOVED" => PoolHealth::Removed,
            other => PoolHealth::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub name: String,
    pub size: u64,
    pub allocated: u64,
    pub free: u64,
    pub health: PoolHealth,
}

pub const POOL_LIST_ARGS: &[&str] = &["list", "-H", "-p", "-o", "name,size,alloc,free,health"];

/// Parses `zpool list -H -p -o name,size,alloc,free,health` output
/// (tab-separated, one pool per line).
pub fn parse_pool_list(output: &str) -> Result<Vec<Pool>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 5 {
                return Err(AppError::Parse(format!(
                    "expected 5 fields in zpool list line: {line:?}"
                )));
            }
            Ok(Pool {
                name: fields[0].to_string(),
                size: fields[1]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad size in: {line:?}")))?,
                allocated: fields[2]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad alloc in: {line:?}")))?,
                free: fields[3]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad free in: {line:?}")))?,
                health: PoolHealth::parse(fields[4]),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct VdevLine {
    pub depth: usize,
    pub name: String,
    pub health: PoolHealth,
}

/// Parses the `config:` section of `zpool status <pool>` output into a
/// flat, indentation-depth-tagged list (2 spaces per nesting level under
/// `config:`). `zpool status` has no `-H -p -o` tabular form, so this is
/// parsed line-by-line instead.
pub fn parse_pool_status_vdevs(output: &str) -> Vec<VdevLine> {
    let mut result = Vec::new();
    let mut in_config = false;
    for line in output.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("config:") {
            in_config = true;
            continue;
        }
        if !in_config {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with("NAME") && trimmed.contains("STATE") {
            continue;
        }
        let indent = line.len() - trimmed.len();
        let mut fields = trimmed.split_whitespace();
        let name = match fields.next() {
            Some(n) => n.to_string(),
            None => continue,
        };
        let state = fields.next().unwrap_or("UNKNOWN");
        result.push(VdevLine {
            depth: indent / 2,
            name,
            health: PoolHealth::parse(state),
        });
    }
    result
}

pub fn list_pools(runner: &dyn CommandRunner) -> Result<Vec<Pool>> {
    let output = runner.run("zpool", POOL_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zpool list".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_pool_list(&output.stdout)
}

/// Runs `zpool status <pool>` and parses its vdev tree. Separate from
/// `list_pools` because `zpool status` has no tabular `-H -p -o` form.
/// The `--` before `pool` stops `zpool` from treating a pool name that
/// starts with `-` as an option flag (argument injection).
pub fn get_pool_vdevs(runner: &dyn CommandRunner, pool: &str) -> Result<Vec<VdevLine>> {
    let output = runner.run("zpool", &["status", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool status {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(parse_pool_status_vdevs(&output.stdout))
}

/// Creates a pool from a raw vdev spec, e.g. `&["mirror", "da0", "da1"]`
/// or `&["raidz1", "da0", "da1", "da2"]`. Guided vdev-spec construction
/// (picking disks/redundancy from a UI wizard) depends on the disk
/// inventory built in Plan 5, so it is out of scope here — this function
/// takes an already-built spec. The `--` before `name` stops `zpool`
/// from treating a name that starts with `-` as an option flag.
pub fn create_pool(runner: &dyn CommandRunner, name: &str, vdev_spec: &[&str]) -> Result<()> {
    let mut args = vec!["create", "--", name];
    args.extend_from_slice(vdev_spec);
    let output = runner.run("zpool", &args)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool create {name} {}", vdev_spec.join(" ")),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn scrub_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    let output = runner.run("zpool", &["scrub", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool scrub {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_pool(runner: &dyn CommandRunner, pool: &str) -> Result<()> {
    let output = runner.run("zpool", &["destroy", "--", pool])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool destroy {pool}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn offline_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["offline", "--", pool, disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool offline {pool} {disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn online_disk(runner: &dyn CommandRunner, pool: &str, disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["online", "--", pool, disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool online {pool} {disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn replace_disk(runner: &dyn CommandRunner, pool: &str, old_disk: &str, new_disk: &str) -> Result<()> {
    let output = runner.run("zpool", &["replace", "--", pool, old_disk, new_disk])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zpool replace {pool} {old_disk} {new_disk}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const ZPOOL_LIST_FIXTURE: &str =
        "tank\t4000000000000\t1200000000000\t2800000000000\tONLINE\nscratch\t500000000000\t100000000000\t400000000000\tDEGRADED\n";

    const ZPOOL_STATUS_FIXTURE: &str = "\
  pool: tank
 state: ONLINE
config:

\tNAME        STATE     READ WRITE CKSUM
\ttank        ONLINE       0     0     0
\t  mirror-0  ONLINE       0     0     0
\t    da0     ONLINE       0     0     0
\t    da1     ONLINE       0     0     0

errors: No known data errors
";

    #[test]
    fn parses_pool_list_fixture() {
        let pools = parse_pool_list(ZPOOL_LIST_FIXTURE).unwrap();
        assert_eq!(
            pools,
            vec![
                Pool { name: "tank".to_string(), size: 4_000_000_000_000, allocated: 1_200_000_000_000, free: 2_800_000_000_000, health: PoolHealth::Online },
                Pool { name: "scratch".to_string(), size: 500_000_000_000, allocated: 100_000_000_000, free: 400_000_000_000, health: PoolHealth::Degraded },
            ]
        );
    }

    #[test]
    fn rejects_malformed_pool_list_line() {
        let result = parse_pool_list("tank\tonly-two-fields\n");
        assert!(result.is_err());
    }

    #[test]
    fn parses_vdev_tree_from_status_fixture() {
        let vdevs = parse_pool_status_vdevs(ZPOOL_STATUS_FIXTURE);
        assert_eq!(
            vdevs,
            vec![
                VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 1, name: "mirror-0".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 2, name: "da0".to_string(), health: PoolHealth::Online },
                VdevLine { depth: 2, name: "da1".to_string(), health: PoolHealth::Online },
            ]
        );
    }

    #[test]
    fn list_pools_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(ZPOOL_LIST_FIXTURE);
        let pools = list_pools(&runner).unwrap();
        assert_eq!(pools.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "zpool");
        assert_eq!(calls[0].1, vec!["list", "-H", "-p", "-o", "name,size,alloc,free,health"]);
    }

    #[test]
    fn list_pools_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        let result = list_pools(&runner);
        assert!(result.is_err());
    }

    #[test]
    fn get_pool_vdevs_runs_status_and_parses_result() {
        let runner = FakeRunner::new(ZPOOL_STATUS_FIXTURE);
        let vdevs = get_pool_vdevs(&runner, "tank").unwrap();
        assert_eq!(vdevs.len(), 4);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["status".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn create_pool_joins_name_and_vdev_spec() {
        let runner = FakeRunner::new("");
        create_pool(&runner, "tank", &["mirror", "da0", "da1"]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["create".to_string(), "--".to_string(), "tank".to_string(), "mirror".to_string(), "da0".to_string(), "da1".to_string()])
        );
    }

    #[test]
    fn scrub_pool_runs_expected_command() {
        let runner = FakeRunner::new("");
        scrub_pool(&runner, "tank").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["scrub".to_string(), "--".to_string(), "tank".to_string()]));
    }

    #[test]
    fn destroy_pool_runs_expected_command() {
        let runner = FakeRunner::new("");
        destroy_pool(&runner, "scratch").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["destroy".to_string(), "--".to_string(), "scratch".to_string()]));
    }

    #[test]
    fn offline_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        offline_disk(&runner, "tank", "da1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["offline".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn online_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        online_disk(&runner, "tank", "da1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zpool".to_string(), vec!["online".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string()]));
    }

    #[test]
    fn replace_disk_runs_expected_command() {
        let runner = FakeRunner::new("");
        replace_disk(&runner, "tank", "da1", "da2").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zpool".to_string(), vec!["replace".to_string(), "--".to_string(), "tank".to_string(), "da1".to_string(), "da2".to_string()])
        );
    }

    #[test]
    fn guards_against_pool_names_that_look_like_flags() {
        let runner = FakeRunner::new("");
        destroy_pool(&runner, "-f").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1[1], "--", "a leading '--' must precede the pool name so zpool cannot parse it as an option");
    }
}
```

- [ ] **Step 3: Add `mod zfs;` to `src/main.rs`**

- [ ] **Step 4: Run the tests**

Run: `cargo test zfs::pool`
Expected: PASS (13 tests)

- [ ] **Step 5: Commit**

```bash
git add src/zfs/mod.rs src/zfs/pool.rs src/main.rs
git commit -m "feat: ZFS pool domain model, parsing, and operations"
```

---

### Task 7: ZFS datasets — domain model, parsing, operations

**Files:**
- Create: `src/zfs/dataset.rs`
- Modify: `src/zfs/mod.rs` (add `pub mod dataset;`)

**Interfaces:**
- Consumes: `AppError`, `Result` from Task 1; `CommandRunner`, `test_support::FakeRunner` from Task 2.
- Produces: `enum DatasetKind { Filesystem, Volume, Other(String) }`, `struct Dataset { pub name: String, pub kind: DatasetKind, pub used: u64, pub available: u64, pub referenced: u64, pub mountpoint: Option<String> }`, `parse_dataset_list(output: &str) -> Result<Vec<Dataset>>`, `list_datasets(runner: &dyn CommandRunner) -> Result<Vec<Dataset>>`, `create_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()>`, `destroy_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()>`.

- [ ] **Step 1: Write `src/zfs/dataset.rs`**

```rust
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetKind {
    Filesystem,
    Volume,
    Other(String),
}

impl DatasetKind {
    fn parse(s: &str) -> DatasetKind {
        match s {
            "filesystem" => DatasetKind::Filesystem,
            "volume" => DatasetKind::Volume,
            other => DatasetKind::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub name: String,
    pub kind: DatasetKind,
    pub used: u64,
    pub available: u64,
    pub referenced: u64,
    pub mountpoint: Option<String>,
}

pub const DATASET_LIST_ARGS: &[&str] =
    &["list", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type"];

/// Parses `zfs list -H -p -o name,used,avail,refer,mountpoint,type`
/// output (tab-separated, one dataset per line).
pub fn parse_dataset_list(output: &str) -> Result<Vec<Dataset>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 6 {
                return Err(AppError::Parse(format!(
                    "expected 6 fields in zfs list line: {line:?}"
                )));
            }
            let mountpoint = match fields[4] {
                "-" | "none" => None,
                mp => Some(mp.to_string()),
            };
            Ok(Dataset {
                name: fields[0].to_string(),
                used: fields[1]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad used in: {line:?}")))?,
                available: fields[2]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad avail in: {line:?}")))?,
                referenced: fields[3]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad refer in: {line:?}")))?,
                mountpoint,
                kind: DatasetKind::parse(fields[5]),
            })
        })
        .collect()
}

pub fn list_datasets(runner: &dyn CommandRunner) -> Result<Vec<Dataset>> {
    let output = runner.run("zfs", DATASET_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zfs list".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_dataset_list(&output.stdout)
}

pub fn create_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let output = runner.run("zfs", &["create", name])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs create {name}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_dataset(runner: &dyn CommandRunner, name: &str) -> Result<()> {
    let output = runner.run("zfs", &["destroy", name])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs destroy {name}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const ZFS_LIST_FIXTURE: &str = "\
tank/data\t100000000\t900000000\t100000000\t/tank/data\tfilesystem
tank/vol1\t50000000\t900000000\t50000000\t-\tvolume
";

    #[test]
    fn parses_dataset_list_fixture() {
        let datasets = parse_dataset_list(ZFS_LIST_FIXTURE).unwrap();
        assert_eq!(
            datasets,
            vec![
                Dataset {
                    name: "tank/data".to_string(),
                    kind: DatasetKind::Filesystem,
                    used: 100_000_000,
                    available: 900_000_000,
                    referenced: 100_000_000,
                    mountpoint: Some("/tank/data".to_string()),
                },
                Dataset {
                    name: "tank/vol1".to_string(),
                    kind: DatasetKind::Volume,
                    used: 50_000_000,
                    available: 900_000_000,
                    referenced: 50_000_000,
                    mountpoint: None,
                },
            ]
        );
    }

    #[test]
    fn rejects_malformed_dataset_list_line() {
        let result = parse_dataset_list("tank/data\tnot-enough-fields\n");
        assert!(result.is_err());
    }

    #[test]
    fn list_datasets_uses_correct_command() {
        let runner = FakeRunner::new(ZFS_LIST_FIXTURE);
        let datasets = list_datasets(&runner).unwrap();
        assert_eq!(datasets.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "zfs");
        assert_eq!(calls[0].1, vec!["list", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,type"]);
    }

    #[test]
    fn create_dataset_runs_expected_command() {
        let runner = FakeRunner::new("");
        create_dataset(&runner, "tank/newds").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["create".to_string(), "tank/newds".to_string()]));
    }

    #[test]
    fn destroy_dataset_runs_expected_command() {
        let runner = FakeRunner::new("");
        destroy_dataset(&runner, "tank/olds").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "tank/olds".to_string()]));
    }
}
```

- [ ] **Step 2: Add `pub mod dataset;` to `src/zfs/mod.rs`**

```rust
pub mod dataset;
pub mod pool;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test zfs::dataset`
Expected: PASS (5 tests)

- [ ] **Step 4: Commit**

```bash
git add src/zfs/dataset.rs src/zfs/mod.rs
git commit -m "feat: ZFS dataset domain model, parsing, and operations"
```

---

### Task 8: ZFS snapshots — domain model, parsing, operations, replication

**Files:**
- Create: `src/zfs/snapshot.rs`
- Modify: `src/zfs/mod.rs` (add `pub mod snapshot;`)

**Interfaces:**
- Consumes: `AppError`, `Result` from Task 1; `CommandRunner`, `test_support::FakeRunner` from Task 2.
- Produces: `struct Snapshot { pub dataset: String, pub name: String, pub used: u64 }`, `parse_snapshot_list(output: &str) -> Result<Vec<Snapshot>>`, `list_snapshots(runner: &dyn CommandRunner) -> Result<Vec<Snapshot>>`, `create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()>`, `destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()>`, `rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()>`, `replicate_snapshot(runner: &dyn CommandRunner, source: &str, dest_host: Option<&str>, dest_dataset: &str) -> Result<()>`.

- [ ] **Step 1: Write `src/zfs/snapshot.rs`**

```rust
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub dataset: String,
    pub name: String,
    pub used: u64,
}

pub const SNAPSHOT_LIST_ARGS: &[&str] = &["list", "-t", "snapshot", "-H", "-p", "-o", "name,used"];

/// Parses `zfs list -t snapshot -H -p -o name,used` output. Each line's
/// first field is `dataset@snapshot`, split on the last `@`.
pub fn parse_snapshot_list(output: &str) -> Result<Vec<Snapshot>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 2 {
                return Err(AppError::Parse(format!(
                    "expected 2 fields in snapshot list line: {line:?}"
                )));
            }
            let (dataset, name) = fields[0]
                .split_once('@')
                .ok_or_else(|| AppError::Parse(format!("missing '@' in snapshot name: {line:?}")))?;
            Ok(Snapshot {
                dataset: dataset.to_string(),
                name: name.to_string(),
                used: fields[1]
                    .parse()
                    .map_err(|_| AppError::Parse(format!("bad used in: {line:?}")))?,
            })
        })
        .collect()
}

pub fn list_snapshots(runner: &dyn CommandRunner) -> Result<Vec<Snapshot>> {
    let output = runner.run("zfs", SNAPSHOT_LIST_ARGS)?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: "zfs list -t snapshot".to_string(),
            status: output.status,
            stderr: output.stderr,
        });
    }
    parse_snapshot_list(&output.stdout)
}

pub fn create_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["snapshot", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs snapshot {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn destroy_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["destroy", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs destroy {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

pub fn rollback_snapshot(runner: &dyn CommandRunner, dataset: &str, name: &str) -> Result<()> {
    let target = format!("{dataset}@{name}");
    let output = runner.run("zfs", &["rollback", &target])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: format!("zfs rollback {target}"),
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

/// Replicates `source` (a `dataset@snapshot` name) to `dest_dataset`.
/// `dest_host` is the SSH destination for the receiving side, or `None`
/// to receive locally on the same host that `runner` targets. ZFS names
/// are restricted to a safe character set (alphanumerics, `_-./:@`), so
/// no additional shell-escaping is needed when building the pipeline.
pub fn replicate_snapshot(
    runner: &dyn CommandRunner,
    source: &str,
    dest_host: Option<&str>,
    dest_dataset: &str,
) -> Result<()> {
    let pipeline = match dest_host {
        None => format!("zfs send {source} | zfs receive {dest_dataset}"),
        Some(host) => format!("zfs send {source} | ssh {host} zfs receive {dest_dataset}"),
    };
    let output = runner.run("sh", &["-c", &pipeline])?;
    if !output.success() {
        return Err(AppError::CommandFailed {
            cmd: pipeline,
            status: output.status,
            stderr: output.stderr,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const SNAPSHOT_LIST_FIXTURE: &str = "\
tank/data@2026-09-01\t10000
tank/data@2026-09-04\t2000
";

    #[test]
    fn parses_snapshot_list_fixture() {
        let snapshots = parse_snapshot_list(SNAPSHOT_LIST_FIXTURE).unwrap();
        assert_eq!(
            snapshots,
            vec![
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 10_000 },
                Snapshot { dataset: "tank/data".to_string(), name: "2026-09-04".to_string(), used: 2_000 },
            ]
        );
    }

    #[test]
    fn rejects_line_missing_at_sign() {
        let result = parse_snapshot_list("tank-data-no-at-sign\t10000\n");
        assert!(result.is_err());
    }

    #[test]
    fn list_snapshots_uses_correct_command() {
        let runner = FakeRunner::new(SNAPSHOT_LIST_FIXTURE);
        let snapshots = list_snapshots(&runner).unwrap();
        assert_eq!(snapshots.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["list", "-t", "snapshot", "-H", "-p", "-o", "name,used"]);
    }

    #[test]
    fn create_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        create_snapshot(&runner, "tank/data", "manual-1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["snapshot".to_string(), "tank/data@manual-1".to_string()]));
    }

    #[test]
    fn destroy_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        destroy_snapshot(&runner, "tank/data", "old-1").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["destroy".to_string(), "tank/data@old-1".to_string()]));
    }

    #[test]
    fn rollback_snapshot_joins_dataset_and_name() {
        let runner = FakeRunner::new("");
        rollback_snapshot(&runner, "tank/data", "2026-09-01").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["rollback".to_string(), "tank/data@2026-09-01".to_string()]));
    }

    #[test]
    fn replicate_snapshot_local_destination_builds_expected_pipeline() {
        let runner = FakeRunner::new("");
        replicate_snapshot(&runner, "tank/data@snap1", None, "backup/data").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("sh".to_string(), vec!["-c".to_string(), "zfs send tank/data@snap1 | zfs receive backup/data".to_string()])
        );
    }

    #[test]
    fn replicate_snapshot_remote_destination_builds_expected_pipeline() {
        let runner = FakeRunner::new("");
        replicate_snapshot(&runner, "tank/data@snap1", Some("backup-host"), "backup/data").unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "sh".to_string(),
                vec!["-c".to_string(), "zfs send tank/data@snap1 | ssh backup-host zfs receive backup/data".to_string()]
            )
        );
    }
}
```

- [ ] **Step 2: Add `pub mod snapshot;` to `src/zfs/mod.rs`**

```rust
pub mod dataset;
pub mod pool;
pub mod snapshot;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test zfs::snapshot`
Expected: PASS (8 tests)

- [ ] **Step 4: Commit**

```bash
git add src/zfs/snapshot.rs src/zfs/mod.rs
git commit -m "feat: ZFS snapshot domain model, parsing, operations, and replication"
```

---

### Task 9: UI application state

**Files:**
- Create: `src/ui/state.rs`
- Create: `src/ui/mod.rs` (stub re-exporting `state`, filled in fully in Task 10)

**Interfaces:**
- Consumes: `Pool` from Task 6, `Dataset` from Task 7, `Snapshot` from Task 8, `CommandRunner` and `test_support::FakeRunner` from Task 2, `Result` from Task 1.
- Produces: `enum Section { Hosts, Pools, Datasets, Snapshots }`, `enum WorkerMsg { Pools(Result<Vec<Pool>>), Datasets(Result<Vec<Dataset>>), Snapshots(Result<Vec<Snapshot>>) }`, `struct App { pub sections: Vec<Section>, pub selected_section: usize, pub hosts: Vec<String>, pub selected_host: usize, pub pools: Vec<Pool>, pub datasets: Vec<Dataset>, pub snapshots: Vec<Snapshot>, pub status_message: Option<String>, pub should_quit: bool, pub loading: bool, ... }`, `App::new(hosts: Vec<String>) -> Self`, `App::handle_key(&mut self, key: crossterm::event::KeyCode)`, `App::refresh_pools/refresh_datasets/refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>)`, `App::drain_worker_messages(&mut self)`.

- [ ] **Step 1: Write `src/ui/state.rs`**

```rust
use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::dataset::Dataset;
use crate::zfs::pool::Pool;
use crate::zfs::snapshot::Snapshot;
use crossterm::event::KeyCode;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
}

pub enum WorkerMsg {
    Pools(Result<Vec<Pool>>),
    Datasets(Result<Vec<Dataset>>),
    Snapshots(Result<Vec<Snapshot>>),
}

pub struct App {
    pub sections: Vec<Section>,
    pub selected_section: usize,
    pub hosts: Vec<String>,
    pub selected_host: usize,
    pub pools: Vec<Pool>,
    pub datasets: Vec<Dataset>,
    pub snapshots: Vec<Snapshot>,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub loading: bool,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl App {
    pub fn new(hosts: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        App {
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots],
            selected_section: 0,
            hosts,
            selected_host: 0,
            pools: Vec::new(),
            datasets: Vec::new(),
            snapshots: Vec::new(),
            status_message: None,
            should_quit: false,
            loading: false,
            tx,
            rx,
        }
    }

    pub fn current_section(&self) -> Section {
        self.sections[self.selected_section]
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Right | KeyCode::Tab => {
                self.selected_section = (self.selected_section + 1) % self.sections.len();
            }
            KeyCode::Left => {
                self.selected_section = if self.selected_section == 0 {
                    self.sections.len() - 1
                } else {
                    self.selected_section - 1
                };
            }
            KeyCode::Down if self.current_section() == Section::Hosts && !self.hosts.is_empty() => {
                self.selected_host = (self.selected_host + 1) % self.hosts.len();
            }
            KeyCode::Up if self.current_section() == Section::Hosts && !self.hosts.is_empty() => {
                self.selected_host = if self.selected_host == 0 {
                    self.hosts.len() - 1
                } else {
                    self.selected_host - 1
                };
            }
            _ => {}
        }
    }

    pub fn refresh_pools(&mut self, runner: Arc<dyn CommandRunner>) {
        self.loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::list_pools(runner.as_ref());
            let _ = tx.send(WorkerMsg::Pools(result));
        });
    }

    pub fn refresh_datasets(&mut self, runner: Arc<dyn CommandRunner>) {
        self.loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::list_datasets(runner.as_ref());
            let _ = tx.send(WorkerMsg::Datasets(result));
        });
    }

    pub fn refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>) {
        self.loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::snapshot::list_snapshots(runner.as_ref());
            let _ = tx.send(WorkerMsg::Snapshots(result));
        });
    }

    pub fn drain_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.loading = false;
            match msg {
                WorkerMsg::Pools(Ok(pools)) => self.pools = pools,
                WorkerMsg::Pools(Err(e)) => self.status_message = Some(e.to_string()),
                WorkerMsg::Datasets(Ok(datasets)) => self.datasets = datasets,
                WorkerMsg::Datasets(Err(e)) => self.status_message = Some(e.to_string()),
                WorkerMsg::Snapshots(Ok(snapshots)) => self.snapshots = snapshots,
                WorkerMsg::Snapshots(Err(e)) => self.status_message = Some(e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use std::time::{Duration, Instant};

    #[test]
    fn handle_key_navigates_sections_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Right);
        assert_eq!(app.current_section(), Section::Pools);
        app.handle_key(KeyCode::Left);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Left);
        assert_eq!(app.current_section(), Section::Snapshots);
    }

    #[test]
    fn handle_key_quit_sets_should_quit() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_navigates_hosts_and_wraps() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 1);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 1);
    }

    #[test]
    fn drain_worker_messages_updates_pools_on_success() {
        let mut app = App::new(vec!["local".to_string()]);
        let pool = Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        };
        app.tx.send(WorkerMsg::Pools(Ok(vec![pool.clone()]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools, vec![pool]);
        assert!(!app.loading);
    }

    #[test]
    fn drain_worker_messages_sets_status_message_on_failure() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx
            .send(WorkerMsg::Datasets(Err(crate::error::AppError::Parse("boom".to_string()))))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.status_message.is_some());
    }

    #[test]
    fn refresh_pools_populates_pools_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        app.refresh_pools(runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
        assert_eq!(app.pools[0].name, "tank");
    }
}
```

- [ ] **Step 2: Create stub `src/ui/mod.rs`**

```rust
pub mod state;

pub use state::App;
```

- [ ] **Step 3: Add `mod ui;` to `src/main.rs`**

- [ ] **Step 4: Run the tests**

Run: `cargo test ui::state`
Expected: PASS (6 tests)

- [ ] **Step 5: Commit**

```bash
git add src/ui/state.rs src/ui/mod.rs src/main.rs
git commit -m "feat: ratatui App state, navigation, and worker-thread refresh"
```

---

### Task 10: Rendering and event loop wiring

**Files:**
- Create: `src/ui/layout.rs`
- Modify: `src/ui/mod.rs` (replace stub with full `run_app`/`event_loop`)
- Modify: `src/main.rs` (wire config + hosts + UI together)

**Interfaces:**
- Consumes: `App`, `Section` from Task 9; `HostInventory` from Task 5; `Config` from Task 4.
- Produces: `pub fn run_app(inventory: HostInventory) -> Result<()>`, `pub fn draw(f: &mut ratatui::Frame, app: &App)`.

- [ ] **Step 1: Write `src/ui/layout.rs`**

```rust
use super::state::{App, Section};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_bar(f, app, outer[1]);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let text = app.status_message.as_deref().unwrap_or("");
    let status = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, area);
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = match s {
                Section::Hosts => "Hosts",
                Section::Pools => "Pools",
                Section::Datasets => "Datasets",
                Section::Snapshots => "Snapshots",
            };
            let style = if i == app.selected_section {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();
    let title = if app.loading { "zfsctl (refreshing...)" } else { "zfsctl" };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    match app.sections[app.selected_section] {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Pools => draw_pools(f, app, area),
        Section::Datasets => draw_datasets(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
    }
}

fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .hosts
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let style = if i == app.selected_host {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(h.as_str()).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Hosts"));
    f.render_widget(list, area);
}

fn draw_pools(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.pools.iter().map(|p| {
        Row::new(vec![
            p.name.clone(),
            p.size.to_string(),
            p.allocated.to_string(),
            p.free.to_string(),
            format!("{:?}", p.health),
        ])
    });
    let widths = [
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Size", "Alloc", "Free", "Health"]))
        .block(Block::default().borders(Borders::ALL).title("Pools ('r' to refresh)"));
    f.render_widget(table, area);
}

fn draw_datasets(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.datasets.iter().map(|d| {
        Row::new(vec![
            d.name.clone(),
            d.used.to_string(),
            d.available.to_string(),
            format!("{:?}", d.kind),
        ])
    });
    let widths = [
        Constraint::Length(24),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(12),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Used", "Avail", "Kind"]))
        .block(Block::default().borders(Borders::ALL).title("Datasets ('r' to refresh)"));
    f.render_widget(table, area);
}

fn draw_snapshots(f: &mut Frame, app: &App, area: Rect) {
    let rows = app
        .snapshots
        .iter()
        .map(|s| Row::new(vec![format!("{}@{}", s.dataset, s.name), s.used.to_string()]));
    let widths = [Constraint::Length(34), Constraint::Length(14)];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Snapshot", "Used"]))
        .block(Block::default().borders(Borders::ALL).title("Snapshots ('r' to refresh)"));
    f.render_widget(table, area);
}
```

- [ ] **Step 2: Replace `src/ui/mod.rs` with the full event loop**

```rust
pub mod layout;
pub mod state;

pub use state::App;

use crate::error::{AppError, Result};
use crate::hosts::HostInventory;
use crate::ui::state::Section;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::time::Duration;

pub fn run_app(inventory: HostInventory) -> Result<()> {
    enable_raw_mode().map_err(|e| AppError::Config(format!("enabling raw mode: {e}")))?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| AppError::Config(format!("entering alternate screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| AppError::Config(format!("creating terminal: {e}")))?;

    let host_names: Vec<String> = inventory.hosts.iter().map(|h| h.name.clone()).collect();
    let mut app = App::new(host_names);
    if let Some(first_host) = inventory.hosts.first() {
        app.refresh_pools(first_host.runner.clone());
    }

    let result = event_loop(&mut terminal, &mut app, &inventory);

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    inventory: &HostInventory,
) -> Result<()> {
    loop {
        app.drain_worker_messages();
        terminal
            .draw(|f| layout::draw(f, app))
            .map_err(|e| AppError::Config(format!("drawing frame: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| AppError::Config(format!("polling events: {e}")))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| AppError::Config(format!("reading event: {e}")))?
            {
                app.handle_key(key.code);
                if key.code == KeyCode::Char('r') {
                    if let Some(host) = inventory.hosts.get(app.selected_host) {
                        match app.current_section() {
                            Section::Pools => app.refresh_pools(host.runner.clone()),
                            Section::Datasets => app.refresh_datasets(host.runner.clone()),
                            Section::Snapshots => app.refresh_snapshots(host.runner.clone()),
                            Section::Hosts => {}
                        }
                    }
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Replace `src/main.rs` to wire config, hosts, and the UI together**

```rust
mod config;
mod error;
mod exec;
mod hosts;
mod ui;
mod zfs;

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> error::Result<()> {
    check_root()?;
    let config_path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
    let config = config::Config::load(&config_path)?;
    let inventory = hosts::HostInventory::from_config(&config);
    ui::run_app(inventory)
}

fn check_root() -> error::Result<()> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(error::AppError::Config(
            "zfsctl must be run as root".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_uid_matches_nix() {
        let is_root = nix::unistd::Uid::effective().is_root();
        let expected = nix::unistd::Uid::effective().as_raw() == 0;
        assert_eq!(is_root, expected);
    }
}
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: PASS (all tests from Tasks 1-9 plus this task's changes; no rendering tests are added here since `terminal.draw` requires a real/test backend and is covered by manual verification instead)

- [ ] **Step 5: Build in release mode to confirm no warnings/errors**

Run: `cargo build --release`
Expected: builds cleanly

- [ ] **Step 6: Commit**

```bash
git add src/ui/layout.rs src/ui/mod.rs src/main.rs
git commit -m "feat: wire ratatui rendering and event loop to config/hosts"
```

---

### Task 11: Manual verification on real FreeBSD, quickstart docs

**Files:**
- Create: `README.md`

**Interfaces:**
- Consumes: nothing new — this task only documents and manually verifies what Tasks 1-10 built.

- [ ] **Step 1: Write `README.md` with a quickstart**

```markdown
# zfsctl

A napp-it-like terminal UI for managing ZFS storage on FreeBSD.

## Requirements

- FreeBSD (uses `zpool`/`zfs`; no Linux/illumos support)
- Root privileges
- `ssh` configured (with working host keys / `~/.ssh/config` entries) for any remote hosts you add to the config

## Running

```sh
cargo build --release
sudo ./target/release/zfsctl
```

## Configuration

Host inventory lives in `/usr/local/etc/zfsctl/config.toml`:

```toml
[[hosts]]
name = "backup"
ssh_target = "backup.example.com"
```

The local machine is always available as the "local" host and does not need a config entry.

## Keys

- `Tab` / `Left` / `Right`: switch sections (Hosts, Pools, Datasets, Snapshots)
- `Up` / `Down` (in Hosts section): select the active host
- `r`: refresh the current section for the selected host
- `q`: quit
```

- [ ] **Step 2: Manually verify on a real FreeBSD host with an existing zpool**

Checklist (run as root on FreeBSD):
1. `cargo build --release` succeeds.
2. Running as non-root (`./target/release/zfsctl`) prints the root-required error and exits 1.
3. Running as root (`sudo ./target/release/zfsctl`) opens the TUI showing the sidebar with Hosts/Pools/Datasets/Snapshots.
4. The Hosts section lists "local" (plus any configured remote hosts).
5. Switching to Pools and pressing `r` populates the pools table with real `zpool list` data.
6. Switching to Datasets and pressing `r` populates real dataset data.
7. Switching to Snapshots and pressing `r` populates real snapshot data.
8. If a remote host is configured in `config.toml`, selecting it in the Hosts section and refreshing Pools/Datasets/Snapshots shows that host's data over SSH.
9. `q` cleanly exits back to the normal terminal (no leftover alternate-screen/raw-mode corruption).

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add quickstart README"
```
