# zfsctl: FreeBSD ZFS/storage management TUI — Design

## Summary

A napp-it-like terminal UI for managing ZFS storage and related FreeBSD
system administration tasks (shares, users, disk health, scheduled jobs)
across one or more FreeBSD hosts. Written in Rust with ratatui.
FreeBSD-only — no Linux or illumos support.

## Goals

- Broad feature parity with napp-it's core surface area: pools, datasets,
  snapshots, replication, NFS/Samba shares, users/ACLs, disk health, and
  scheduled jobs.
- Manage multiple FreeBSD hosts from one TUI, not just the local machine.
- Keep the app itself stateless between runs wherever possible — rely on
  existing OS mechanisms (cron, `/etc/exports`, `smb4.conf`) as the
  source of truth for anything that must keep working when the TUI isn't
  open.
- No `unsafe` FFI, no coupling to a specific libzfs/OpenZFS version.

## Non-goals (v1)

- Linux / illumos support.
- A privileged helper daemon or agent process on managed hosts.
- A structured database of job run history (log files are enough).
- End-to-end automated tests against a real ZFS pool (manual
  verification on real hardware instead).

## Architecture

### Crate layout

Single binary crate (a multi-crate workspace is unnecessary until there's
a second binary or a published library to split out):

```
src/
  main.rs          # entry point, root check, arg parsing
  config.rs        # TOML config load/save (hosts, jobs, preferences)
  exec/            # command execution abstraction
    mod.rs         #   CommandRunner trait: run(cmd, args) -> Output
    local.rs        #   LocalRunner: std::process::Command
    ssh.rs          #   SshRunner: shells to system `ssh <host> <cmd>`
  zfs/             # ZFS domain model + operations, built on exec::CommandRunner
    pool.rs         #   Pool, Vdev, parsing `zpool list/status -H -p`
    dataset.rs       #   Dataset/zvol, parsing `zfs list -H -p`
    snapshot.rs      #   Snapshot create/destroy/rollback, `zfs send/receive`
  disks.rs         # camcontrol/smartctl wrapping for disk health
  shares/          # NFS (/etc/exports) and Samba (smb4.conf) management
  users.rs         # system users + `zfs allow` dataset permissions
  jobs.rs          # cron entry management for scheduled snapshot/scrub/replication
  hosts.rs         # host inventory (local + remote), backed by exec::CommandRunner impls
  ui/              # ratatui: App state, sidebar/tabs, per-section views, event loop
```

### ZFS/system integration: CLI shelling, not libzfs FFI

All ZFS operations go through the `zpool`/`zfs` CLI (parsed via `-H -p`
machine-readable flags), not FFI bindings to libzfs. Rationale: existing
Rust libzfs bindings are thin, largely unmaintained, and coupled to
specific OpenZFS versions — a poor fit across FreeBSD 13/14/15, which
ship different OpenZFS versions. CLI shelling avoids `unsafe` code and
version coupling, and is the same approach napp-it itself takes.

The same principle applies to disk health (`camcontrol`, `smartctl -j`),
shares (`/etc/exports`, `smb4.conf` + `service ... reload`), users
(`pw`, `zfs allow`, `getfacl`/`setfacl`), and jobs (`crontab`) — plain
file edits and CLI invocations, no new system-integration mechanism per
feature area.

### Execution abstraction (local vs. remote)

```rust
trait CommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
}

struct CommandOutput { stdout: String, stderr: String, status: ExitStatus }

struct LocalRunner;                 // std::process::Command::new(cmd).args(args)
struct SshRunner { host: String }   // shells to system `ssh`, centralizes arg quoting
```

Every ZFS/disk/share/user/job operation is written once against `&dyn
CommandRunner`. A `Host` is `{ name, runner: Box<dyn CommandRunner> }`;
higher-level code never branches on local-vs-remote. Remote execution
shells out to the system `ssh` binary (reuses the user's
`~/.ssh/config`, `known_hosts`, and agent — no auth code to write)
rather than an in-process SSH library. `SshRunner` centralizes shell
quoting when flattening args into one remote command string, so no call
site has to think about escaping.

This trait boundary is also what makes higher-level logic (job
application, share management, parsing→action flows) unit-testable
against a fake in-memory runner without root or a real system.

### Privileges

The app requires running as root (or via `sudo`/`doas`), matching how
napp-it's own backend operates and how other FreeBSD sysadmin TUIs
(e.g. `bsdconfig`) work. No privilege-separation or helper-daemon
architecture in v1.

### ZFS domain model & parsing

Parsed exclusively from machine-readable CLI output, never
human-formatted text:

- `zpool list -H -p -o name,size,alloc,free,health,...` → `Pool { name,
  size, allocated, free, health: PoolHealth, vdevs: Vec<Vdev> }`
- `zpool status <pool>` has no full tabular form — parsed line-by-line
  for the vdev tree and per-device state/errors.
- `zfs list -H -p -o name,used,avail,refer,mountpoint,type` → `Dataset {
  name, kind: Filesystem|Volume, used, available, referenced,
  mountpoint }`
- `zfs list -t snapshot -H -p` → `Snapshot { dataset, name, created,
  used }`
- Mutations (`zfs create/destroy/snapshot/rollback`, `zpool
  create/destroy/scrub/replace`) check exit status + stderr, then
  re-list to refresh state — no parsing of mutation output itself.

Health/type/kind fields become enums (`PoolHealth`, etc.), not raw
strings, so the UI matches on `PoolHealth::Degraded` rather than
string-comparing.

### Jobs (cron-backed scheduling)

- Job definitions (what to run, target dataset/pool, target host,
  schedule) live in the app's TOML config — the app's own source of
  truth, not crontab.
- Applying a job writes/updates a managed block in the target host's
  crontab (local via `crontab`, remote via SSH), each line tagged with a
  marker comment (e.g. `# zfsctl:job-id=<uuid>`) so the app can find and
  update/remove exactly its own entries without touching unrelated cron
  lines.
- Each cron line invokes a plain one-shot command directly (`zfs
  snapshot -r tank/data@auto-%Y%m%d%H%M`, `zpool scrub tank`, or a
  replication one-liner piping `zfs send | ssh ... zfs receive`) — no
  wrapper binary needed on the target host.
- Run history/status: job commands redirect output and capture exit
  status into a per-job log file (e.g.
  `/var/log/zfsctl/jobs/<job-id>.log`); the TUI's job view reads that
  file for last-run status. No structured run-history database.
- The app is stateless between runs — jobs keep firing via cron even
  when the TUI isn't open.

### Shares (NFS + Samba)

- NFS: manage a marker-delimited block within `/etc/exports`, leaving
  hand-edited entries outside that block untouched. Apply →
  `service mountd reload` (or equivalent).
- Samba: manage `[share]` sections in an included, `zfsctl`-owned conf
  file referenced from `smb4.conf` via `include =`, rather than editing
  the main file directly. Apply → `service samba_server reload`.
- Both reuse the same local/SSH `CommandRunner` — no new abstraction.
- Creating a share from the Datasets view pre-fills the path from the
  dataset's mountpoint.

### Users & ACLs

- System users/groups: `pw useradd/usermod/userdel`,
  `pw groupadd/groupmod`.
- Delegated ZFS permissions: `zfs allow`/`zfs unallow`.
- File-level POSIX/NFSv4 ACLs on dataset mountpoints:
  `getfacl`/`setfacl`.
- Presented as two distinct UI subsections (System Users vs. Dataset
  Permissions) — related but distinct concerns.

### Disk health

- Enumerate physical disks via `camcontrol devlist` / `geom disk list`,
  cross-referenced against pool vdevs so the UI can show which disk
  backs which vdev.
- SMART data via `smartctl -a <device> -j` (JSON), falling back to text
  parsing only if `-j` is unavailable. Parsed into `DiskHealth {
  device, temperature, power_on_hours, reallocated_sectors,
  overall_status, ... }`.
- Read-only view. Destructive disk actions (`zpool replace/offline`)
  live under the Pools view as pool operations, not here.

### App configuration & storage

Single human-editable TOML config file (e.g.
`/usr/local/etc/zfsctl/config.toml`) holding host inventory, job
definitions, and preferences. Job run history and logs are plain flat
files under `/var/log/zfsctl/`. No embedded database — simple to
inspect and back up by hand, no migration story needed.

### UI / event loop

Synchronous ratatui main loop, not a full async runtime:

- Main thread: standard `crossterm`-polling draw loop, renders `App`
  state each tick.
- Potentially slow operations (SSH to remote hosts, SMART scans,
  cron/file edits) dispatch to worker threads (`std::thread::spawn`, or
  a small fixed pool for concurrent multi-host refreshes), returning
  results over an `mpsc::channel` drained each tick.
- `App` state tracks in-flight operations per pane (e.g. a "refreshing"
  indicator) instead of blocking the render thread.
- Command execution itself stays plain blocking `std::process::Command`
  calls, just moved off the render thread — same pattern used by tools
  like `gitui`/`bottom`. Avoids async-ifying every `CommandRunner` call.

### Navigation

Sidebar + tabs, k9s-style: a persistent left sidebar/tree (Hosts → Pools
→ Datasets → Snapshots, plus Shares/Users/Disks/Jobs sections) with a
main panel that updates based on selection. Keyboard-driven, suited to
ZFS's naturally hierarchical pool/dataset/snapshot structure.

### Error handling & testing

- A single app-level `Error` enum (via `thiserror`) covers
  `CommandRunner` failures (nonzero exit, SSH connection failure, parse
  failure), surfaced in the UI as a dismissible status/error line — a
  failed remote host doesn't take down the whole TUI.
- Parsing logic is unit-tested against captured real CLI output
  fixtures (not a live pool).
- Higher-level logic (job application, share management,
  parsing→action flows) is tested against a fake in-memory
  `CommandRunner` — no root or real system required.
- No end-to-end test suite against a real ZFS pool in v1; manual
  verification on real FreeBSD hardware is the integration-level check.

## Feature scope summary (v1, "broad napp-it parity")

- Pools: list, status, create, destroy, scrub, replace/offline disks.
- Datasets/zvols: list, create, destroy, edit properties.
- Snapshots: create, destroy, rollback, `zfs send`/`receive` replication
  (including to remote hosts over SSH).
- Jobs: scheduled snapshot/scrub/replication via managed cron entries,
  log-file-based run status.
- Shares: NFS exports and Samba shares, managed as owned blocks within
  existing config files.
- Users: system users/groups (`pw`), delegated ZFS permissions (`zfs
  allow`), file ACLs (`getfacl`/`setfacl`).
- Disks: inventory + SMART health, cross-referenced to pool vdevs.
- Hosts: multi-host inventory; every feature area above works
  identically against local or remote (SSH) hosts via the
  `CommandRunner` abstraction.
