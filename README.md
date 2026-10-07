# zfsctl

A terminal UI for managing ZFS storage on FreeBSD.

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

To manage several hosts from one login (metal or VMs), pick one as your hub and add the others to that host's `config.toml` — the Hosts section shows a hint if it detects only "local" is configured.

### Themes and the key-hint bar

The bottom row of every screen lists the keys that work right now for whatever has focus (it changes as you move between sections, panes, detail tabs, modals and pickers; keys that would do nothing are hidden). A status message, when there is one, appears on the line above it.

Color carries meaning, never decoration: pool and vdev health are `● ONLINE` (green), `▲ DEGRADED` (yellow), `✖ FAULTED`/`UNAVAIL`/`REMOVED` (red), `○ OFFLINE` (gray); pool capacity bars turn yellow at 70% and red above 85% (`!` suffix); resilver/scrub progress is cyan; destructive confirmations (destroy, rollback) are red with a `✖` title and disruptive ones (offline, replace, revoke, remove) are yellow with `▲`. Every state also has a glyph and a word, so nothing depends on color alone.

Pick a theme in `config.toml` (optional):

```toml
[ui]
theme = "ansi"   # ansi (default) | mono | solarized-dark | solarized-light
```

- `ansi` uses only the 16 terminal colors, so your terminal's own theme applies.
- `mono` uses no color at all (bold/dim/reverse and the glyphs). It is also chosen automatically when `NO_COLOR` is set to a non-empty value.
- `solarized-dark` / `solarized-light` use the Solarized palette (a tribute to Sun, who gave us ZFS). They want a truecolor terminal (`COLORTERM=truecolor`); otherwise Solarized's 256-color approximations are used and a note says so at startup.
- An unrecognized value is a startup error that lists the valid names.

### Auto-refresh

While you sit on Pools, Datasets, Snapshots, Disks or Shares, that list refreshes itself in the background (every 10 seconds by default, and straight away the first time you open it). Hosts, open modals/pickers and a pool's vdev view (which polls every 2 seconds on its own) are not touched. Your selection follows the item you had selected by name, so a refresh never changes which row a keypress acts on, and background refreshes don't flash the "refreshing" indicator or wipe a status message you're reading (a stale error does clear once the host answers). `r` still refreshes on demand.

```toml
[ui]
auto_refresh_secs = 10   # seconds between refreshes; 0 turns it off
```

### Remote host setup: unprivileged SSH + sudo

`ssh_target` should be an **unprivileged** user, not `root` — `zfsctl` connects over SSH as whatever user `ssh_target` names, then runs every remote command through `sudo -n` (the exceptions are read-only lookups that need no privilege and run over SSH without sudo: `getent passwd` / `getent group` for the F2 picker, and `uname`, `date`, `sysctl`, `iostat` and `showmount` for the metrics panel; if the SSH key is locked to a forced command, that command must allow them too) (non-interactive: it fails immediately with a clear error if the sudoers entry below is missing or wrong, instead of hanging on a password prompt that can never arrive). This keeps a stolen SSH key scoped to exactly the commands `zfsctl` ever runs, not full root.
The `!` shell is deliberately the plain unprivileged login: it adds no sudoers requirement and no path to root.

`zfsctl` shells out to eleven privileged binaries in total (confirmed by grepping every `run`/`run_checked`/`run_with_stdin` call site, not just the obvious `zpool`/`zfs` ones — `getfacl`/`setfacl` back the ACL tab, `geom` backs the Disks section, `bectl` backs the Boot Envs section, `service` backs the Shares section's NFS/SMB availability check and the Samba reload, `smbpasswd` sets a managed SMB share's Samba password, `cat`/`tee` read and write `smb4.conf`, and `smbstatus -S` lists the clients connected to an SMB share for the metrics panel; all eleven need the sudoers entry, or whichever feature uses the missing one fails with "a password is required" the first time you touch it). Replication needs no `sh` entry either: the hub runs the two `zfs` commands itself and pipes them together. On each remote host, as root, find the exact paths (typically `/sbin/zpool`, `/sbin/zfs`, `/bin/getfacl`, `/bin/setfacl`, `/sbin/geom`, `/sbin/bectl`, `/usr/sbin/service`, `/usr/local/bin/smbpasswd`, `/usr/local/bin/smbstatus`, `/bin/cat`, `/usr/bin/tee` on FreeBSD, but confirm on your actual install rather than assume):

```sh
which zpool zfs getfacl setfacl geom bectl service smbpasswd smbstatus cat tee
```

Then add a `NOPASSWD` entry scoped to exactly those paths for the connecting user (replace `fbsd` and the paths with what you found):

```sh
visudo
# add a line like:
fbsd ALL=(root) NOPASSWD: /sbin/zpool, /sbin/zfs, /bin/getfacl, /bin/setfacl, /sbin/geom, /sbin/bectl, /usr/sbin/service, /usr/local/bin/smbpasswd, /usr/local/bin/smbstatus -S, /bin/cat /usr/local/etc/smb4.conf, /usr/bin/tee /usr/local/etc/smb4.conf
```

Full paths (not bare command names) in the sudoers entry matter: `sudo`'s own `secure_path` (FreeBSD's default) already protects the *command being resolved*, but the sudoers entry itself should still pin the exact binaries it authorizes, as a second, independent layer — this is the actual place the hardening belongs, not the client code, which only ever sends `sudo -n zpool ...` (bare names; the unprivileged `getent` lookups are the only commands sent without `sudo -n`) and relies on `secure_path` for safe resolution.

`smbstatus` is pinned to `-S` (the share-connection listing); it only works as root, and the broader `-b`/`-p` forms are not needed. Without this one entry everything else keeps working: the Shares metrics panel just shows the SMB client line as `unavailable` instead of a client list.

`cat` and `tee` are additionally pinned to the `smb4.conf` argument. A bare `sudo cat` would let the connecting user read any file as root (e.g. `/etc/master.passwd`), and a bare `sudo tee` would let them overwrite any file, so pinning prevents arbitrary-file read/overwrite through those two commands. It does not make the entry as a whole root-equivalent-proof: `smbpasswd`, `zfs` and `setfacl` are already broad (for example `zfs` can mount over or change permissions on arbitrary paths, and `setfacl` can alter ACLs anywhere). A `sudo sh` entry would be unrestricted root outright, which is why `zfsctl` uses `cat`/`tee` with a fixed path rather than `sh -c`. If your Samba config lives somewhere other than `/usr/local/etc/smb4.conf`, `zfsctl` does not currently support it.

## Keys

- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
- `'['` / `']'` (Datasets right pane only, once focused with `Right`): cycle between the Properties, Permissions, and ACL sub-tabs.
- `Up` / `Down` (in Hosts section): select the active host
- `Up` / `Down` (in Pools/Datasets/Snapshots, or a properties pane): select a row
- `r`: refresh whichever pane currently has focus (the middle list, or — for Pools/Datasets — the properties pane if it's focused)
- `?`: show the Help screen (full keybinding reference)
- `A`: show the About screen
- `!`: open a shell on the selected host and return to zfsctl when you exit it. On a remote host this is an interactive `ssh` login as the same unprivileged user zfsctl connects as, never a root shell (run the allowed `sudo zpool ...` yourself from there). Locally it is `$SHELL`. When the shell exits, the list on screen is refreshed.
- `q`: quit
- `Esc`: dismiss the current status message, back out of a focused properties pane, or (while a modal is open) cancel it
- `F2` (while a form field with a known value set is focused — `zfs allow`'s Permissions, the ACL form's Permissions/Flags/Type, an edited pool property that takes fixed values such as `failmode` or `autoexpand`, or the Redundancy field of the create-pool and add-vdev from-disks forms): open a picker. Type to filter, `Up`/`Down` to move, `Space` to toggle a value (Permissions/Flags — multiple can be checked), `Enter` to confirm (joins every checked value with commas for multi-select, or picks the highlighted single value for Type), `Esc` to cancel without changing the field. A field with no known value set (`Name`, `Scope`, etc.) ignores `F2`.

On the four account fields (SMB valid user, NFS "Root user mapping" (users only), `zfs allow` recipient, ACL entry subject) `F2` lists the target host's real users and groups, real accounts first. They are fetched on first use and cached for 60 seconds, over SSH without sudo, so no sudoers change is needed. If the lookup fails the popup says so and typing in the field still works.

### Forms (any modal with text fields to fill in)

- `Tab` / `Down`: move to the next field (wraps from the last field back to the first)
- `Shift+Tab` / `Up`: move to the previous field (wraps from the first field back to the last)
- `Enter` on the last field: submit. An invalid value anywhere in the form is rejected with a status message explaining that submission failed — the form stays open, focus resets to the first field so you can `Tab`/`Down` through to find and fix the problem without needing to `Esc` and start over
- `Esc`: cancel, discarding whatever was typed

### Hosts

- `Up` / `Down`: select the active host
- `n`: add a new host (prompts for Name and an SSH target, e.g. `user@host`). Validates the name isn't blank or a case-insensitive duplicate of an existing host (including `"local"`), then probes SSH reachability in the background (up to 10 seconds) before accepting it — on success the host is usable immediately, no restart, and is written to `config.toml` so it survives one; on failure (bad name, unreachable host, auth rejected) the attempt is abandoned and the specific SSH error is shown — press `n` again to retry.

### Pools

- `Enter` on a selected pool: drill into its vdev/disk tree
- `Right`: move focus to the properties pane on the right (fetches the selected pool's `zpool` properties)
- `Left` (while the properties pane is focused): move focus back to the pool list
- `n`: create a new pool (prompts for name and vdev spec, e.g. `mirror da0 da1`)
- `d`: destroy the selected pool (type the pool's name to confirm)
- `s`: scrub the selected pool (y/n confirm)
- `R`: resilver the selected pool (y/n confirm)
- `J`: schedule a recurring scrub of the selected pool (opens the job form with host and pool filled in; see **Scheduled jobs** below)

### Pool/Dataset properties pane (right pane, `Right` to focus)

- `Up` / `Down`: select a property (or, on the Datasets Permissions sub-tab, a permission row — see below)
- `Enter`: edit the selected property's value (prompts pre-filled with the current value; submitting runs `zpool set`/`zfs set`)
- `r`: re-fetch properties (or permissions, on that sub-tab) for the currently-selected pool/dataset
- `Esc` / `Left`: back out to the middle pane

#### Datasets' Permissions sub-tab (`'['`/`']'` to switch to it)

Delegated ZFS administration permissions (`zfs allow`/`unallow`) for the
selected dataset — who can run which ZFS subcommands/properties on it.
Not available for Pools (`zfs allow` only targets a filesystem or
volume).

- `n`: grant a permission to a user/group/everyone (prompts for
  Recipient — `everyone` / `user:<name>` / `group:<name>` — Scope —
  `local`/`descendent`/`both`, blank means `both` — and Permissions, a
  required comma-separated list)
- `s`: define or add to a named permission set (prompts for the set
  name without `@`, and Permissions)
- `c`: add a create-time permission — auto-granted to whoever creates a
  new child dataset under this one (prompts for Permissions only)
- `d`: revoke everything for the selected row's target (y/n confirm)
- `x`: revoke just specific permissions from the selected row, leaving
  the rest intact (prompts for which permissions to remove)

Granting is always additive (never a replace) — this mirrors how `zfs
allow` itself works: granting a user two different permissions in two
separate calls merges into one combined grant, it never overwrites.

#### Datasets' ACL sub-tab (`'['`/`']'` to switch to it)

FreeBSD native NFSv4 access control lists (`getfacl`/`setfacl`) on the
selected dataset's mountpoint — not available for a zvol or an
unmounted filesystem (no mountpoint to point `getfacl` at).

- `n`: add a new ACL entry, always appended at the end (prompts for
  Recipient — `owner` / `group` / `everyone` / `user:<name>` /
  `group:<name>` — Permissions, a required comma-separated list of
  long-form names like `read_data`, Flags — optional, e.g.
  `file_inherit` — and Type, `allow`/`deny`, blank means `allow`)
- `d`: remove the selected entry by its position (y/n confirm)

There's no in-place edit, same as the Permissions sub-tab — to change
an entry, remove it and re-add it. Entries are evaluated in the order
shown, and that order is also the position number `d`/`n` operate on.

### Vdevs (inside a drilled-into pool)

- `Esc` / `Backspace`: back out to the Pools list
- `o`: offline the selected disk (y/n confirm) — no-op on a mirror/raidz group row, not a physical disk
- `O`: online the selected disk (y/n confirm) — same restriction as `o`
- `x`: replace the selected disk (prompts for the new device name) — same restriction as `o`
- `a`: add a new vdev/mirror/raidz group to this pool (prompts for a vdev spec, e.g. `mirror da2 da3`)

### Datasets

- `Right` / `Left`: move focus to/from the properties pane (same as Pools, above)
- `n`: create a new dataset (prompts for name)
- `d`: destroy the selected dataset (type the dataset's name to confirm)
- `s`: snapshot the selected dataset (prompts for the snapshot's name only)
- `J`: schedule recurring snapshots of the selected dataset, keeping the newest N (opens the snapshot-job form with the dataset filled in; see **Scheduled jobs** below)
- `M`: create/edit a managed SMB share for the selected (mounted) dataset (see Shares → SMB shares). Always shown; if smbd isn't running, the dataset isn't mounted, or shares aren't loaded yet, it says so in the status line instead
- `N`: create/edit an NFS share for the selected dataset (always shown; if nfsd isn't running it says so in the status line instead; prompts for networks, hosts, read-only, and root user mapping — all optional; press `r` in the Shares section at least once first, so zfsctl knows the dataset's current share state before offering to change it)

### Snapshots

- `d`: destroy the selected snapshot (type `dataset@name` to confirm)
- `b`: roll back to the selected snapshot (type `dataset@name` to confirm) — discards any writes made since

### Disks

- `Up` / `Down`: select a disk (the table shows each disk's human-readable size, SSD/HDD type and rpm, model and serial so you can tell identical-looking drives apart — Serial, then Model, then Type are dropped on narrow terminals — and the create-pool / add-vdev forms list each selected disk's identity)
- `Space`: toggle-select the highlighted disk (only if it's not already claimed by a pool)
- `n`: create a new pool from the selected disk(s) (prompts for name and redundancy — blank for a stripe, or `mirror`/`raidz1`/`raidz2`/`raidz3`)
- `a`: add a vdev to an existing pool from the selected disk(s) (prompts for the target pool name and redundancy, same as above)
- `Esc`: clear the current selection
- `r`: refresh the disk list

### Shares

- `Up` / `Down`: select a share
- `Enter`: edit the selected share — an NFS share's networks/hosts/read-only/root mapping, or a managed SMB share (same form as `M` in Datasets)
- `d`: remove the selected share (NFS or SMB), y/n confirm
- `r`: refresh the share list

#### SMB shares

FreeBSD's `sharesmb` ZFS property doesn't work (`zfs set sharesmb=on` prints "No SMB support in FreeBSD yet."), so `zfsctl` manages Samba itself. In Datasets, `M` creates or edits an SMB share for the selected (mounted) dataset: share name, valid user, a Samba password, read-only, and guest access.

- **Managed block.** Shares live in one block in `/usr/local/etc/smb4.conf`, delimited by these exact marker lines:
  ```
  ## zfsctl managed shares -- do not edit by hand -- start
  ## zfsctl managed shares -- do not edit by hand -- end
  ```
  Never edit inside it; everything outside it is never touched (hand-written shares such as your own `[homes]` stay byte-identical). The block is appended if absent. Each stanza carries a `# zfsctl-dataset = <dataset>` comment, which is how a share is tied to its dataset. Anything unrecognised inside the block is reported as an error rather than silently dropped. After every change `zfsctl` reloads Samba (`service samba_server reload`). Note that `smbd` accepts the reload even for a config it dislikes while it is running, so the previous `smb4.conf` is only restored if the reload command itself fails (and the same restore is attempted if writing the file fails). If `smb4.conf` did not exist beforehand, nothing is restored and the error says the file may be partial. A file that is not valid UTF-8 is never rewritten.
- **One user per share**, one SMB share per dataset.
- **Guest access is an explicit opt-in** (the `Guest access (y/n)` field, default `n`). A blank Valid user with guest `n` is a share any authenticated Samba user can reach (Samba's default). Guest `y` renders `guest ok = yes`: anonymous access with no password, which is read/write if Read-only is `n`, so set Read-only to `y` for guest shares unless you really want anonymous writes. Guest cannot be combined with a Valid user, and any password typed is ignored for a guest share.
- **The Valid user must already be a Unix account.** `zfsctl` gives it a Samba password with `smbpasswd -a -s`. A password is required on create; on edit, a blank password keeps the existing one (only if the user is unchanged). Setting a password **replaces that user's Samba password for every share they use**; there is no way to avoid this when creating a share, so enter the user's current Samba password if you want to keep it.
- **Removing a share** (`d` in Shares) deletes its stanza but leaves the user's Samba password in place, since the same user may also back a hand-written share.
- SMB shares are only listed while `smbd` is running; a config or parse error shows in the Shares status line while NFS rows still render.

### Boot Envs

Lists the host's boot environments (`bectl list`): Name, Active (`Now`, `Reboot`, `Now+Reboot`, `Temp`), Space, Mountpoint and Created; Mountpoint then Created drop on a narrow terminal. If the host has no `bectl` (or is not laid out for boot environments) the section title says `[unavailable: ...]` with the reason, and recovers on its own once it works. Keys act only on what is possible for the selected row, and the hint bar shows exactly those:

- `n`: new boot environment (name; `F2` on the second field picks one to clone, blank clones the running one).
- `a`: make it the boot environment for every future boot (asks first). `t`: use it for the next boot only; `T`: clear that. Neither the running nor the next-boot one is destroyable.
- `R`: rename. `m` / `u`: mount / unmount; the Mountpoint column shows the `/tmp/be_mount.XXXX` path.
- `d`: destroy (type its name). `D`: destroy and remove its origin snapshot (`bectl destroy -o`).

`zfsctl` never reboots the host. bectl's own error text is shown verbatim when it refuses something.

### Jobs

The Jobs section lists the scheduled jobs of every host (it is global, not per host). Its title says whether the daemon is running; the status line shows `N jobs failed` from any section while an enabled job's latest run failed.

- `Up` / `Down`: select a job; the bottom panel shows its last 8 runs, its schedule and its next three run times
- `n`: new scrub job (host, pool, schedule, catch up; `F2` lists the configured hosts, the pools of the chosen host (fetched live) and schedule presets, and the form previews the next three run times as you type)
- `R`: new replication job (source host and dataset, snapshot prefix, destination host and dataset, force, schedule, catch up; `F2` lists the configured hosts, the datasets of the chosen host (fetched live) and schedule presets)
- `c`: cancel the selected replication while it is running (it stops within a second or two; the next run resumes it)
- `N`: new snapshot job (host, dataset, recursive, prefix, keep, schedule, catch up; `F2` lists the configured hosts, the datasets of the chosen host (fetched live) and schedule presets)
- `Enter`: edit the selected job (its id, creation time and enabled flag are kept)
- `Space`: enable or disable the selected job
- `d`: delete the selected job (y/n; its past runs stay in the history until pruned)

## Scheduled jobs (`zfsctl daemon`)

`zfsctl daemon` runs scheduled jobs on the hub, unattended. It must run as root, like the TUI, and reaches other hosts over the same SSH + sudo path, so it needs no new sudoers entries. There are three job kinds. A **scrub** job runs `zpool scrub` on a pool of any configured host on a cron schedule, watches it to completion and records the result. A **snapshot** job takes a snapshot of one dataset on a cron schedule and keeps only the newest `keep` of the snapshots it made. A **replicate** job copies the newest snapshot with a given prefix from a dataset on one configured host to a dataset on another. Manage jobs from the **Jobs** section of the TUI (see its keys above, or `J` on a pool in Pools), or edit `jobs.toml` by hand: the daemon notices either within a second. The TUI only reads the state directory, so it must run on the hub (as root) to see the daemon's jobs.

**Install the service.** Copy the binary and the rc script, then enable it:

```sh
install -m 755 target/release/zfsctl /usr/local/bin/zfsctl
install -m 755 contrib/rc.d/zfsctl /usr/local/etc/rc.d/zfsctl
sysrc zfsctl_enable=YES
service zfsctl start
```

`daemon(8)` detaches it, writes `/var/run/zfsctl.pid`, sends output to `/var/log/zfsctl.log`, and restarts it ten seconds after a crash. `zfsctl_binary`, `zfsctl_state_dir` and `zfsctl_log` in `rc.conf` override the binary path, the state directory and the log.

**State** lives in `/var/db/zfsctl/` (root only): `jobs.toml` (the jobs), `history.jsonl` (one JSON line per run event) and `daemon.json` (a heartbeat rewritten every second; older than 10 seconds means the daemon is not running). The daemon log is stamped in the hub's local time (with its offset), the same zone the schedules use; `history.jsonl` stays in UTC. The daemon notices an edited `jobs.toml` within a second and keeps the previous schedule if the new file does not parse. It also re-reads `config.toml` within a second, so a host added in the TUI (or a changed `notify_command`) takes effect without a restart; a `config.toml` that does not parse is logged and the previous hosts are kept.

**A job** is one `[[jobs]]` table:

```toml
[[jobs]]
id = "scrub-tank-monthly"
host = "local"          # a host name from config.toml; "local" is the hub
kind = "scrub"
pool = "tank"
schedule = "0 3 1 * *"  # five-field cron, in the hub's local time zone
catch_up = true         # run once on startup if a scheduled time was missed (default true)
enabled = true          # default true
created_at = "2026-10-06T12:00:00Z"  # optional; a catch-up never reaches back before this
```

**A snapshot job** takes `zfs snapshot` of one dataset (`recursive = true` adds `-r`, so a whole tree is snapshotted atomically and pruned together) and names it `<prefix>-<UTC timestamp>`, for example `hourly-20261006T193000Z`. After a successful snapshot it destroys its own snapshots beyond the newest `keep`, oldest first. It only ever destroys names that match exactly `<prefix>-<YYYYMMDDTHHMMSSZ>`, so manual snapshots, other jobs' snapshots and look-alikes such as `hourly-keep-forever` are never touched. If a snapshot cannot be destroyed (a hold, a clone) the run is recorded as failed with the reason, and the rest are still pruned. Hourly, daily and weekly tiers are separate jobs with separate prefixes. Two enabled snapshot jobs cannot share a host and prefix on the same dataset, or on a dataset inside a recursive job's tree (they would prune each other); the later one is reported as unable to run. The lock is per dataset and prefix, so an hourly and a daily job on one dataset that fall due together both run. `keep` counts the snapshot just taken, which is never pruned. Changing a job's prefix leaves the old prefix's snapshots in place.

```toml
[[jobs]]
id = "snap-tank-data-hourly"
host = "local"
kind = "snapshot"
dataset = "tank/data"
recursive = true        # default false
prefix = "hourly"       # default "auto"; letters, digits, _ - and .
keep = 24               # at least 1
schedule = "0 * * * *"
```

In the TUI, `N` in Jobs opens the form and `J` on a dataset in Datasets opens it with the dataset filled in.

Schedules are `minute hour day-of-month month day-of-week` with `*`, numbers, lists, ranges and steps (`*/15`). Day of week is `0`-`6`, `0` being Sunday. If both day fields are restricted a day matches when either does. A local time that does not exist (the spring DST jump) is skipped; one that happens twice (autumn) runs once.

**A replication job** sends the newest snapshot named `<prefix>-<UTC timestamp>` (for example the ones a snapshot job with the same prefix makes) from a dataset on one host to a dataset on another. The hub runs `zfs send` through the source host's SSH+sudo path and `zfs receive` through the destination's, as two processes, and copies the bytes between them itself; the source and destination may be any two configured hosts (the hub is `local`), including the same host with a different dataset.

```toml
[[jobs]]
id = "repl-tank-data-to-vm"
host = "local"             # the source host
kind = "replicate"
dataset = "tank/data"      # the source dataset
prefix = "hourly"          # which snapshots to send; default "auto"
dest_host = "fbsd16"       # a configured host
dest_dataset = "backup/tank-data"
force = false              # default false; true adds `zfs receive -F` to incremental receives
schedule = "30 * * * *"
```

- **Incremental.** Each run sends the newest matching snapshot incrementally from the newest snapshot the two sides have in common (matched by guid, so a source snapshot pruned since the last run only makes the increment longer). The first run, or one with nothing in common, is a full send. If the destination already has the newest snapshot the run is `skipped: up to date`.
- **Nothing is destroyed on the source, and the destination is only rolled back with `force`.** A replication never creates, destroys or rolls back a snapshot on the source, and the destination is received unmounted (`zfs receive -s -u`). A destination that was written to since its newest snapshot makes the receive fail with zfs's own message ("destination ... has been modified since most recent snapshot"); `force = true` adds `-F` to an incremental (or resumed) receive, which rolls the destination back to the base: it destroys destination snapshots and changes newer than the base. A full send is never forced, so it can never overwrite an existing dataset. A full send into an existing dataset that shares nothing with the source fails with "destination ... exists / must specify -F".
- **Resumable.** Because the receive is `-s`, an interrupted transfer (a network drop, a cancel, a daemon restart) leaves a resume token on the destination, and the next run continues with `zfs send -t` instead of starting over (`resumed an interrupted transfer`). If a token can no longer be continued (its source snapshot is gone) the run fails and says so; discard it by hand with `zfs receive -A <dataset>` on the destination host.
- **Progress and cancel.** The job panel shows `sent 12.3 GiB of 40.0 GiB (30%)  85.0 MiB/s  eta 5m 00s` for a running replication, taken from files the daemon writes once a second in `<state>/progress/`. `c` writes a request file in `<state>/cancel/`; the daemon notices it within a second, stops both processes and records the run as `cancelled`. A cancelled run is not counted as failed and does not notify. A daemon restart during a transfer ends it, closes the run as lost, and the next run resumes.
- **Rules.** Two jobs may send from the same dataset to different destinations; two enabled jobs cannot write to the same destination (their incrementals would corrupt each other), and a slot that falls due while the job's previous transfer is still running is skipped; the later one is reported as unable to run, and `Space` refuses to enable a job that would clash. Only `/sbin/zfs` is used on the hosts, so no new sudoers entries are needed. `dest_host = "file"` (stream files in a hub directory) is not supported yet.

**Notifications.** To be told when a job fails or a scrub finds errors, set a command in `config.toml`:

```toml
[daemon]
notify_command = "curl -s -d \"$ZFSCTL_JOB on $ZFSCTL_HOST: $ZFSCTL_MESSAGE\" ntfy.sh/my-zfs-alerts"
```

It runs through `sh -c` with `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT` and `ZFSCTL_MESSAGE` set, and is killed after 30 seconds. Job data only ever reaches it through those variables, never the command line, so a pool or message can never be executed as code. A failing hook is logged and never affects the job.

## Status

v1 covers browsing and 11 of the 12 ZFS operations implemented at the library layer: create/destroy/scrub a pool, offline/online/replace a disk, create/destroy a dataset, and create/destroy/rollback a snapshot — all from the TUI, locally or over SSH. Scheduled `zfs send`/`receive` replication between hosts is described under Scheduled jobs. Pool and dataset properties (`zpool`/`zfs get`/`set`) can now be viewed and edited from a properties pane next to the Pools/Datasets lists (`Right` to focus it). Delegated ZFS administration permissions (`zfs allow`/`unallow`) can be viewed, granted, and revoked from a Permissions sub-tab in the Datasets right pane (`'['`/`']'` to switch to it, once focused with `Right`) — Pools has no equivalent, since `zfs allow` only targets a filesystem or volume. File ACLs (`getfacl`/`setfacl`, NFSv4 only — POSIX ACLs are deferred) can be viewed, added, and removed from an ACL sub-tab in the Datasets right pane, scoped to the dataset's mountpoint; bhyve zvol association is still not implemented — planned as follow-up work. A Disks section lists every block device on the host with its size and pool/vdev membership, and lets you build a new pool or add a vdev to an existing one from a multi-disk selection instead of typing device names; `zpool resilver` is also available from the Pools section. A Shares section lists every dataset's active NFS/SMB share and lets you create or edit an NFS share's networks/hosts/read-only/root mapping from a structured form; SMB shares are created, edited and removed through a managed block in `smb4.conf` (see Shares → SMB shares), since FreeBSD's `sharesmb` ZFS property doesn't actually serve anything (confirmed empirically).
