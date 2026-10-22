# zfsctl

A terminal UI and daemon for managing ZFS storage: a FreeBSD hub that manages FreeBSD, Linux and illumos hosts over ssh.

> **Built with Claude.** zfsctl was written by fedor4445 in collaboration with Claude, an AI model by Anthropic (through Claude Code): the design, code, tests and documentation were produced in conversation with it, and directed and tested by the author on real machines.

**Documentation: [The zfsctl Book](docs/book/README.md)**, with real screens and a chapter for each part of the tool. This README is the reference for the details.

## Requirements

- A FreeBSD machine as the hub, where zfsctl runs (it uses `zpool`/`zfs`). The hosts it manages over ssh may run FreeBSD, Linux (tested on Ubuntu 24 with OpenZFS) or illumos (tested on OmniOS)
- Root privileges
- `ssh` configured (with working host keys / `~/.ssh/config` entries) for any remote hosts you add to the config

## Running

```sh
cargo build --release
sudo ./target/release/zfsctl
```

`zfsctl --help` (or `-h`) lists every command, the feature names for `print-sudoers` and `check-host`, the files it uses and the exit codes. Like every other command it must run as root.

## Configuration

Host inventory lives in `/usr/local/etc/zfsctl/config.toml`:

```toml
[[hosts]]
name = "backup"
ssh_target = "backup.example.com"
tag = "offsite"      # optional label shown in the Hosts table and matched by its / filter
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

`ssh_target` should be an **unprivileged** user, not `root` — `zfsctl` connects over SSH as whatever user `ssh_target` names, then runs every remote command through `sudo -n` (the exceptions are read-only lookups that need no privilege and run over SSH without sudo: `getent passwd` / `getent group` for the F2 picker, and `uname`, `date`, `hostname`, `sysctl`, `swapinfo`, `iostat` and `showmount` for the metrics panel, and `zoneadm` / `zonecfg` on illumos to find the zone that holds a dataset for the snapshot browser, plus `sh`, `lsblk`, `systemctl`, `svcs`, `pgrep` and `stat` where the host's OS or a feature needs them; if the SSH key is locked to a forced command, that command must allow them too) (non-interactive: it fails immediately with a clear error if the sudoers entry below is missing or wrong, instead of hanging on a password prompt that can never arrive). This keeps a stolen SSH key scoped to exactly the commands `zfsctl` ever runs, not full root. A port or other ssh option does not go in `ssh_target` (it is passed to ssh as one argument); put it in `~/.ssh/config` under the same host name.
The `!` shell is deliberately the plain unprivileged login: it adds no sudoers requirement and no path to root.

**Let zfsctl write and check the entry.** On the hub, as root:

```sh
zfsctl print-sudoers fbsd16              # the exact sudoers line for that host, paths resolved on the host
zfsctl print-sudoers fbsd16 --with restic --with rclone --without samba
zfsctl check-host fbsd16                 # ok / MISSING / TOO BROAD per entry; exit 0 only if all ok (2 = unreachable)
```

Both connect with the host's normal unprivileged `ssh_target` and only read: they never write a sudoers file. `print-sudoers` resolves each binary's real path on the host, refuses a path outside the system directories, and prints the line plus the `visudo -c -f` check to run before installing it as `/usr/local/etc/sudoers.d/zfsctl`. The default covers every feature except `restic` (`--with restic` adds it; `--without acl` and so on remove one). `check-host` asks the host's sudo what it would allow and also tries a wrong argument on the pinned `cat`/`tee`/`smbstatus` entries, so a plain `cat` entry (or a wildcard such as `smb4.conf*`) is reported as TOO BROAD. It asks sudo about the bare command name, as zfsctl runs it, and reports a MISSING entry when sudo would run a different binary than the one found. If the account also has a sudo rule that asks for a password (a `%wheel` rule, say), it says so in a NOTE and exits 1, because sudo may satisfy a command through that rule and the `ok` lines then prove nothing. The manual list below is what these commands generate, kept for reference.

`zfsctl` shells out to eleven privileged binaries in total (confirmed by grepping every `run`/`run_checked`/`run_with_stdin` call site, not just the obvious `zpool`/`zfs` ones — `getfacl`/`setfacl` back the ACL tab, `geom` backs the Disks section, `bectl` backs the Boot Envs section, `service` backs the Shares section's NFS/SMB availability check and the Samba reload, `smbpasswd` sets a managed SMB share's Samba password, `cat`/`tee` read and write `smb4.conf`, and `smbstatus -S` lists the clients connected to an SMB share for the metrics panel; all eleven need the sudoers entry, or whichever feature uses the missing one fails with "a password is required" the first time you touch it). Replication needs no `sh` entry either: the hub runs the two `zfs` commands itself and pipes them together. On each remote host, as root, find the exact paths (typically `/sbin/zpool`, `/sbin/zfs`, `/bin/getfacl`, `/bin/setfacl`, `/sbin/geom`, `/sbin/bectl`, `/usr/sbin/service`, `/usr/local/bin/smbpasswd`, `/usr/local/bin/smbstatus`, `/bin/cat`, `/usr/bin/tee` on FreeBSD, but confirm on your actual install rather than assume):

A twelfth binary, `/usr/local/bin/restic`, is needed only on a host that runs backup jobs (jobs on the hub itself run as root and need nothing): add it to that host's sudoers entry. It is as broad as `zfs` and `bectl`, since restic as root can read any file and write anywhere. A backup job with a *Path inside the dataset* also runs `readlink` (a read-only look at whether a part of the path is a symlink, so the job never follows a link out of the snapshot); `--with restic` adds `/usr/bin/readlink` to the line too, and a host set up before that needs `zfsctl print-sudoers HOST --with restic` installed again.

A thirteenth binary, `/usr/local/bin/rclone`, is needed only on a host that runs cloud mirror jobs (jobs on the hub itself run as root and need nothing): add it with `zfsctl print-sudoers HOST --with rclone`. It is as broad as `restic`: rclone as root can read any file and write to any remote its `rclone.conf` names.

On an illumos host the boot environment tool is `/usr/sbin/beadm`, allowed with any arguments (`zfsctl print-sudoers HOST` includes it by default there; listing needs no sudo). A fourteenth binary, `/usr/local/sbin/smartctl` (from `pkg install smartmontools`), is needed only for the daily disk health check (`zfsctl print-sudoers HOST --with smart`). Its entry is pinned to two forms, `smartctl --scan -j` and `smartctl -j -H -A -i /dev/*`; sudo's `*` also matches spaces, so the second cannot stop an extra flag, but the account already holds `zfs` and `zpool`, which are as powerful as root, and zfsctl only ever sends the read-only flags.

A fifteenth binary, `ls`, is needed only to look inside snapshots (`zfsctl print-sudoers HOST --with browse`; off by default on every OS). Its entry is pinned to forms that all end in `-- *` (print-sudoers writes the three for the host's own OS, two on illumos; all eight are: (sudo matches the words as one string, so this does not keep an extra option or path out, but `ls` has no option that writes or runs anything; the worst is a recursive listing): `ls -lAb -D %s -- *`, `ls -lAbc -D %s -- *`, `ls -lAbU -D %s -- *` (FreeBSD), `ls -lAQ --time-style +%s -- *`, `ls -lAQ --time-style +%s --time ctime -- *`, `ls -lAQ --time-style +%s --time birth -- *` (Linux), `ls -lAbE -- *` and `ls -lAbEc -- *` (illumos); the `b` and `Q` make `ls` print a newline or an escape character in a name as an escape sequence, so one line is always one entry). It lists names and metadata, never file contents. Sudo's `*` also matches spaces and `/`, so the path cannot be confined to `.zfs/snapshot`, but the account already holds `zfs` and `zpool`, which can read every dataset. A host set up before the escaped listings (the forms were `-lA...` before they became `-lAb`, `-lAQ` and `-lAbE`) needs the browse entry installed again: run `zfsctl print-sudoers HOST --with browse` and install the new file. For a dataset given to an illumos zone it reads through `<zone path>/root`, which that zone's own root user controls (a hostile zone could make the path lead elsewhere on the host), so that listing is a hint only; it finds the zone with `zoneadm list -cp` and `zonecfg -z NAME info dataset`, which need no sudo. The browser treats a snapshot's file names as untrusted text: listings are escaped (`ls -b`, `ls -Q`), so one line is always one entry and a crafted name cannot forge extra entries; a name with a control character, a byte that is not UTF-8 or, on illumos, a backslash is shown but never opened.

A sixteenth binary, `cp`, is needed only to copy a file out of a snapshot (`zfsctl print-sudoers HOST --with restore`; off by default on every OS). Its entry is pinned to one form per OS: `cp -p -n -T -- * *` on Linux and `cp -p -n -- * *` on FreeBSD and illumos. `-p` keeps times and modes (run as root on FreeBSD it also keeps file flags and ACLs), `-n` never overwrites an existing file, and GNU's `-T` (Linux only; FreeBSD and illumos have no such option) makes an existing directory at the destination an error instead of a copy into it. Sudo matches the words as one string, so it cannot confine the paths: with this entry the account can copy any file the host can read to any new path, which `zfs` and `zpool` already allow.

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

`cat` and `tee` are additionally pinned to the `smb4.conf` argument. A bare `sudo cat` would let the connecting user read any file as root (e.g. `/etc/master.passwd`), and a bare `sudo tee` would let them overwrite any file, so pinning prevents arbitrary-file read/overwrite through those two commands. It does not make the entry as a whole root-equivalent-proof: `smbpasswd`, `zfs`, `setfacl` and `bectl` are already broad (for example `zfs` can mount over or change permissions on arbitrary paths, `setfacl` can alter ACLs anywhere, and `bectl` can open a shell in a boot environment, mount one anywhere, or activate one for the next boot, which amounts to root on the host). A `sudo sh` entry would be unrestricted root outright, which is why `zfsctl` uses `cat`/`tee` with a fixed path rather than `sh -c`. If your Samba config lives somewhere other than `/usr/local/etc/smb4.conf`, `zfsctl` does not currently support it.

## Keys

- `Tab` / `Shift+Tab`: switch sections (Dashboard, Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs, Cloud, Alerts, Settings) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
- `'['` / `']'` (Datasets right pane only, once focused with `Right`): cycle between the Properties, Permissions, and ACL sub-tabs.
- `Up` / `Down` (in Hosts section): select the active host
- `Up` / `Down` (in Pools/Datasets/Snapshots, or a properties pane): select a row
- `r`: refresh whichever pane currently has focus (the middle list, or — for Pools/Datasets — the properties pane if it's focused)
- **Confirmations take two keys.** A y/n window needs `y` (yes) or `n` / `Esc` (no) to *select*, then `Enter` to apply it; `Enter` on its own does nothing, so a stray key or a held-down Enter can never run, or even dismiss, a confirmation. The window shows what is selected and what Enter will do. Forms that cannot be taken back (creating a pool, adding a vdev, replacing a disk, revoking permissions) end in the same confirmation, which shows the exact command first; Enter on the last field never runs them by itself; shrinking a volume (`volsize`) and setting a pool `feature@…` property get the same confirmation. An Enter that follows another within a quarter of a second is ignored, which swallows most of a held key's repeats; what really protects you from a held key is that an unanswered confirmation ignores Enter. Typed-name confirmations (destroying a pool, dataset or snapshot) need the exact name and then `Enter`, and `Esc` cancels them at once. A confirmation that came from submitting a form (irreversible forms, volume size and pool features, saved jobs that would delete data) gives the form back when you decline it, with everything you typed and the last field focused: Enter asks again, Esc discards the form.
- `?`: show the Help screen (full keybinding reference)
- `A`: show the About screen (the wordmark and the version)
- `!`: open a shell on the selected host and return to zfsctl when you exit it. On a remote host this is an interactive `ssh` login as the same unprivileged user zfsctl connects as, never a root shell (run the allowed `sudo zpool ...` yourself from there). Locally it is `$SHELL`. When the shell exits, the list on screen is refreshed.
- `q`: quit
- `Esc`: dismiss the current status message, back out of a focused properties pane, or (while a modal is open) cancel it
- `F2` (while a form field with a known value set is focused — `zfs allow`'s Permissions, the ACL form's Permissions/Flags/Type, an edited pool property that takes fixed values such as `failmode` or `autoexpand`, or the Redundancy field of the create-pool and add-vdev from-disks forms): open a picker. Type to filter, `Up`/`Down` to move, `Space` to toggle a value (Permissions/Flags — multiple can be checked), `Enter` to confirm (joins every checked value with commas for multi-select, or picks the highlighted single value for Type), `Esc` to cancel without changing the field. A field with no known value set (`Name`, `Scope`, etc.) ignores `F2`.

On the four account fields (SMB valid user, NFS "Root user mapping" (users only), `zfs allow` recipient, ACL entry subject) `F2` lists the target host's real users and groups, real accounts first. They are fetched on first use and cached for 60 seconds, over SSH without sudo, so no sudoers change is needed. If the lookup fails the popup says so and typing in the field still works.

### Forms (any modal with text fields to fill in)

- `Tab` / `Down`: move to the next field (wraps from the last field back to the first)
- `Shift+Tab` / `Up`: move to the previous field (wraps from the first field back to the last)
- `Enter` on the last field: submit. An invalid value anywhere in the form is rejected with a status message that names the field and what is wrong with it (for the permission and ACL forms: a permission that starts with `-`, a set name typed with its leading `@`, an unknown recipient or type) — the form stays open with everything you typed, focus resets to the first field so you can `Tab`/`Down` through to find and fix the problem without needing to `Esc` and start over
- `Esc`: cancel, discarding whatever was typed

### Dashboard

zfsctl opens on the Dashboard: one borderless screen with the health of every component, the problems first. A header says whether all is quiet or how many things need attention ("checking..." until every host has answered its first probe, so it never claims quiet before it knows). **Needs attention** lists every issue (red, `✖`) and every thing to watch (yellow, `▲`) with the host or job it is about. **Hosts** has one row per host: state, pools, space, memory (free, inactive and ZFS cache memory are not counted as in use, so a cache-filled RAM is not an alarm), CPU (the busy share between two probes, about 30 s), load, how long since the last scrub, and disk errors, each coloured by how worried to be; gauges appear when the terminal is wide enough and columns drop as it narrows. Below: the daemon (running, heartbeat age), jobs by kind with failed ones flagged, each cloud remote with the provider's name first and its last result (bucket size only if you measured it in Cloud), and the alerts monitor. Anything not known yet or not answering is shown as unknown (`○`, `?`), never as fine. A pool that is resilvering gets a yellow line (`tank resilvering 42%`). The daemon also checks every host's disks with `smartctl` once a day (`[daemon] daily_check = "03:30"`, `"off"` to stop, was `smart_check`; it needs the `smart` sudoers feature and `smartmontools` on the host) and writes `smart.json` in its state directory; a host with a SMART problem (a drive's own health check failed, reallocated, pending or uncorrectable sectors, an SSD at 80% wear, a low spare, 60 degrees and up) gets one Needs attention line with the age of the check, healthy hosts and hosts without `smartctl` get none, and the daily report lists the same disks. The same daily pass checks file sharing: a host with NFS shares while `nfsd` is stopped, or Samba shares while `smbd` is stopped, gets a red Needs attention line (`shares.json` in the state directory). A red notice under the header shows while a check runs. `c` in the Hosts section runs both checks on the selected host on demand (read-only, merged into the same files). It never starts a self-test.

It is display only. `Up` / `Down` select a line of Needs attention and `Enter` jumps to the section that owns it (that is all it does); fixing happens there. It reuses the probes that already run every 30 s and adds two read-only unprivileged commands per host per probe (`sysctl`, `swapinfo`) and one `date +%z` per host per session; no sudoers entry changes, and the daemon's own health probes (and so the alert mail) are unchanged. Defaults: space 80% / 90% (yellow / red); memory 90% / 97%; swap 50% / 90%; CPU 95% (yellow only, never red); last scrub 35 days (yellow, also when never scrubbed) / 90 days (red). They can be changed under `[dashboard]` in `config.toml` (`memory_warn`, `memory_crit`, `swap_warn`, `swap_crit`, `cpu_warn`, `scrub_warn_days`, `scrub_crit_days`), and `scrub_ignore = ["scratch"]` lists pools it should not judge by scrub age. A resilver does not count as a scrub. Not on the Dashboard in this version: NFS and Samba availability (it would cost a privileged check per host per probe), disk wear (needs smartmontools) and predictions such as "this pool fills in 40 days" (needs a history of samples).

### Hosts

- `Up` / `Down`: select the active host
- `/`: filter the list by name, address, tag or type (case-insensitive, live as you type). `Enter` keeps the filter, `Esc` clears it; `Up`/`Down` skip hidden hosts and the selection always stays on a visible one. A filter that matches nothing cannot be kept.
- `n`: add a new host (prompts for Name, an SSH target such as `user@host`, and an optional Tag). Validates the name isn't blank or a case-insensitive duplicate of an existing host (including `"local"`), then probes SSH reachability in the background (up to 10 seconds) before accepting it — on success the host is usable immediately, no restart, and is written to `config.toml` so it survives one (only a new `[[hosts]]` block is added after the last existing host: your comments, the order and the other sections stay as you wrote them; a `config.toml` that writes `hosts` as an inline array or dotted keys is refused with a note to edit it by hand, and the host works for the session anyway); on failure (bad name, unreachable host, auth rejected) the attempt is abandoned and the specific SSH error is shown — press `n` again to retry.

The Hosts list is a table: `● Name | Address | State | Type | Tag | Used | Pools | Scrub | Jobs | Rtt`. Every host is probed in the background about every 30 seconds (staggered, one probe at a time per host; an unreachable host costs a single 10-second SSH timeout and never blocks the UI). The marker, the name and the State text take the host's health colour: normal when fine, yellow for a warning, red for critical or unreachable, dim while the first probe is still running. Address is the host part of the configured `ssh_target` as written (no lookup; `local` for the hub). The State column names the first reason of the worst level:

- red: `unreachable` (the SSH connection failed); a pool that is not ONLINE (`tank DEGRADED`); a pool 90% full or more (`tank 91% full`)
- yellow: read/write/checksum errors on the pool or on any device inside it, or permanent data errors (`tank 3 errors`); the last scrub or resilver finished with errors (`scrub found errors on tank`); a pool 80% full or more; the latest run of an enabled job on that host failed (`job failed`); the host answers but cannot be inspected, such as sudo refused (`cannot inspect: <reason>`); a probe that has not finished after 45 seconds (`no answer for 60s`)

State also tells a network failure (`unreachable`) from an ssh refusal (`ssh login refused`, `host key not accepted`). The other columns:

- **Type**: `metal`, `vm` or `jail`, read once per host without sudo: FreeBSD `sysctl -n kern.vm_guest security.jail.jailed`, Linux `systemd-detect-virt` (or the DMI vendor in `/sys/class/dmi/id/sys_vendor`), illumos `zonename` and `smbios` (a container or a non-global zone shows as `jail`; a host where nothing can be read shows `-`). `vm` is coloured cyan and `jail` magenta, apart from the health colours.
- **Tag**: a free label per host, `tag = "backup"` under its `[[hosts]]` entry in `config.toml` or the optional third field of the Add Host form (change it later by editing `config.toml` and restarting). Shown only when some host has one.
- **Used**: the fullest pool's capacity, yellow from 80%, red from 90%. **Pools**: how many. **Scrub**: `running 34%` or `paused` if any pool is, else `never` if any pool never scrubbed, else the date of the oldest last scrub.
- **Jobs**: `failed/total` of the host's enabled jobs (a job counts as failed when its latest run failed).
- **Rtt**: the round trip of the last probe.

Name, Address and State always show (Address drops only on a very narrow pane); on a narrower terminal the other columns go in this order: Rtt, Jobs, Scrub, Pools, Tag, Used, Type. The table scrolls with the selection, and a paused scrub is shown as `paused` rather than finished.

The bottom panel of the selected host adds, on top of OS, uptime, load, ZFS version, pools, space and ARC: the CPU (cores and live usage over a one-second window, with the model below), memory (physical, free, wired), swap, the L2ARC size and hit rate when a cache device exists, and one row per pool with its used percentage, fragmentation, health and last scrub (the date `zpool status` prints, without the weekday, because that time has no time zone). Every figure is best-effort: a host that lacks one shows `-`. When the last sample failed, the panel keeps the previous figures, dimmed, with `(stale)` in its title; on a narrow or short panel the lines are stacked in one column, most important first (pools and status, then pool rows, space, ARC, CPU and so on), the graphs give way to a one-line I/O rate, and whatever still does not fit is counted in a `+N more lines: enlarge the terminal` line instead of being cut silently. The Status line (the first line of `zpool status -x`) is drawn in the color of the host's State in the table, and when zfsctl itself raised the level (a pool 80% or more full, a failed job) the reason is shown on a line under it.

### Pools

The bottom panel of the selected pool shows its Read and Write graphs (braille area graphs, with the peak of what is drawn in the header) beside its health, space, fragmentation, dedup, error counters and last scrub, and a second column of what the pool is made of and holds: **Vdevs** (the top-level data, special, dedup and log vdevs, by kind: `2 mirror, 1 raidz1`, a single disk counting as `disk`), **Disks** (every physical disk, by class: `data`, `log`, `cache`, `spare`, `special`, `dedup`), **Datasets** (`12 filesystems, 3 volumes`), **Snapshots** (only the count, never the list), and the root dataset's compression and the pool's `autotrim`. The vdev and disk numbers come from the `zpool status` the panel already reads. Datasets and snapshots are counted with one extra `zfs list` that asks only for the type column, and it runs when you select a pool and then at most every 30 seconds (a pool with tens of thousands of snapshots can take a few seconds to count), so the counts show `…` until the first one arrives and `-` if counting failed. On a narrow terminal the two columns become one, health and space first, then these counts, with a `+N more lines` note when the rest does not fit.

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
unmounted filesystem (no mountpoint to point `getfacl` at). On a dataset whose `acltype` property is `posix` the tab says so in its title, hides `n` and `d`, and explains in the status line if you press them: it edits NFSv4 ACLs only. In the Recipient fields (here and in the Permissions sub-tab) `user:` and `group:` may be written in any case with spaces around the colon (`User: carol`); the name keeps its case. `d` asks for confirmation naming the entry (`allow owner@: read_data/write_data`), not just its number.

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
- `d`: destroy the selected dataset (type the dataset's name to confirm). zfs refuses a dataset that still has snapshots or child datasets; the prompt says so and how many
- `D`: destroy the selected dataset **and** its child datasets and all their snapshots (`zfs destroy -r`). You type the dataset's name, and the prompt says exactly how many child datasets and snapshots go with it: counted at the last refresh, which the Datasets view repeats about every ten seconds along with the dataset list (a snapshot taken a moment ago may not be counted yet). If something under it is busy, zfs stops part-way, and what it had already destroyed stays destroyed. Snapshot clones inside the dataset go with it; clones outside it are never touched (`-R` is not used), so zfs refuses if any depend on it. `D` is refused on a pool's root dataset (it would wipe the pool: use the Pools section), on boot-environment datasets (use Boot Envs, which refuses the running and next-boot ones), and on anything holding the running system. `d` on a pool root says the same
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
- `a`: make it the boot environment for every future boot (asks first). `t`: use it for the next boot only (asks first); `T`: clear that. Neither the running nor the next-boot one is destroyable.
- `R`: rename. `m` / `u`: mount / unmount; the Mountpoint column shows the `/tmp/be_mount.XXXX` path.
- `d`: destroy (type its name). `D`: destroy and remove its origin snapshot (`bectl destroy -o`).

While a change to a boot environment is running (usually a second or two) the keys that change things wait and the status line says `Still working on '<name>'`, so a second `m` or `t` cannot queue a duplicate. After a rename the selection stays on the renamed boot environment. When the selected one is mounted, its mount path leads the pane title, so it is visible even when the Mountpoint column has dropped off a narrow terminal.

`zfsctl` never reboots the host. bectl's own error text is shown verbatim when it refuses something; for example, destroying a mounted boot environment fails with `cannot destroy mounted boot env unless forced`, so unmount it (`u`) first.

### Jobs

The Jobs section lists the scheduled jobs of every host (it is global, not per host). Its title says whether the daemon is running; the status line shows `N jobs failed` from any section while an enabled job's latest run failed.

- `Up` / `Down`: select a job; the bottom panel shows its last 8 runs, its schedule and its next three run times
- `n`: new scrub job (host, pool, schedule, catch up; `F2` lists the configured hosts, the pools of the chosen host (fetched live) and schedule presets, and the form previews the next three run times as you type)
- `R`: new replication job (source host and dataset, snapshot prefix, destination host and dataset, force, schedule, catch up, keep on target; `F2` lists the configured hosts, the datasets of the chosen host (fetched live) and schedule presets)
- `B`: new backup job (host, dataset, path, source, snapshots to keep, restic repository, password file, keep, schedule, catch up; `F2` lists the configured hosts, the datasets of the chosen host (fetched live), the dataset's snapshots and prefixes for the source, and schedule presets; `F5` checks the repository; see "A backup job" below)
- `C`: new cloud mirror job (host, dataset, path, source, snapshots to keep, rclone remote, speed limit, schedule, catch up; `F2` as in the backup form; `F5` checks the remote). See "A cloud mirror job" below
- `c`: cancel the selected replication or backup while it is running (it stops within a second or two; a replication resumes on the next run, a backup starts again). If the backup has finished and its retention prune is running, `c` stops the prune instead: the run is recorded as cancelled with the backup kept, and the retention is applied after the next successful backup (a run that finds nothing new is `skipped: up to date` and does not prune). On a remote host this stops the local `ssh` client; verified on FreeBSD 16 (restic 0.19.1): the remote restic and the clone were gone within seconds. The cancelled restic leaves a stale lock, which the next run's retention clears first (see "Cancel and locks"). A prune that is cancelled while it waits for another run's lock (observed on FreeBSD 15.1 with a live lock held for four minutes) is recorded as cancelled and the remote `restic forget` stays in the process list until that lock is released; it then exited without forgetting anything (the dead ssh connection ends it at its first write), but until it exits it is still there
- The Target column shows where a job's data goes: `tank/data → fbsd16:backup/data`, `tank/data → file:/dir`, or `tank/data → <repository>` for a backup (it widens to fit, as far as the terminal allows)
- `N`: new snapshot job (host, dataset, recursive, prefix, keep, schedule, catch up; `F2` lists the configured hosts, the datasets of the chosen host (fetched live) and schedule presets)
- `Enter`: edit the selected job (its id, creation time and enabled flag are kept)
- `Space`: enable or disable the selected job
- `d`: delete the selected job (y/n; its past runs stay in the history until pruned)

### Settings

The last sidebar section edits the rest of `config.toml` in place: the theme and auto-refresh (`[ui]`), the Dashboard thresholds (`[dashboard]`) and the cloud caps (`[cloud.caps]`). `Enter` on a row opens a form (a blank field means the default; a warn/crit pair needs both numbers, warn above 0 and below crit), then the usual two-key confirmation. Only that key is written, through the same safe write as the Alerts screen (backup `config.toml.bak`, private temporary file, same owner and mode, rename), so comments, hosts and other sections stay as they are; the change applies at once, with no restart. Alerts keep the notify settings and the hook command stays in the file.

### Linux and illumos hosts

Managed hosts may run Linux (Ubuntu 24 tested) or illumos (OmniOS tested); the hub stays FreeBSD. The first probe of a host also reads its OS (`uname -s`, in the same round trip as the host type). Pools, Datasets, Snapshots and the Hosts and Dashboard rows work as they are, since `zpool` and `zfs` print the same columns everywhere. Disks uses `lsblk` (Linux) or `iostat -En` (illumos). Boot Envs on illumos lists the boot environments with `beadm list -H` (no sudo) and changes them with `beadm` (create, activate, next-boot-only, rename, destroy; the `beadm` sudoers entry is part of the default `print-sudoers` there; mount and destroy-with-origin are not offered); Boot Envs on Linux and the dataset ACL tab on both are dimmed in the sidebar for such a host and say they are not supported yet; their keys do nothing, and no FreeBSD-only command is run on those hosts. Shares is listed read-only there (the datasets with `sharenfs`, illumos's native `sharesmb`, and the NFS and SMB service state from `systemctl` or `svcs`), and the daily shares check uses the same states, so a stopped `nfs-server` or `nfs/server` with shares is a red Dashboard line. `print-sudoers` and `check-host` default to the `core` entry (`zpool`, `zfs`) on those hosts, refuse features that run FreeBSD tools, and print `/etc/sudoers.d/zfsctl` as the install path. The Dashboard's Memory, CPU, load and swap come from `/proc` (Linux) or `kstat`, `swap -l` and `uptime` (illumos), in one unprivileged call per probe; the bottom panel of Hosts reads uptime, CPU, memory, swap, load and the ZFS cache the same way (the ZFS version stays `-` on illumos). An unknown or unread OS behaves as FreeBSD.

### Cloud

The Cloud section lists the rclone remotes, each with the provider behind it (`Backblaze B2`, `Amazon S3 (etc.)`, `Google Drive`, `SFTP`...; rclone reports only the type, so an `s3` remote may be any S3-compatible service) (`rclone listremotes --long`, names and types only, never keys) of the hub and of any host that has a cloud job, with the jobs that write to each (a cloud mirror, or a restic backup whose repository is `rclone:`), the latest result of any of them and, after `s`, the size of the bucket. It only reads. A remote whose latest run failed is red; a remote no job uses is dim. The bottom panel shows the selected remote's jobs and the failure reason, if any.

- `Up` / `Down`: select a remote
- `s`: measure the bucket: the size and file count of `current/` and `old/` for each of the remote's cloud mirror jobs. With `[cloud.caps]` set in `config.toml` (`b2 = "10G"`, the room your plan gives you, by remote name) the table and panel also show how much of the cap is used (yellow from 80%, red from 95%) (`rclone size`; it lists the bucket, which a cloud account may count as requests, so it only runs when asked and shows when it was measured)
- `F5`: the form's read-only remote check, for the remote of its first cloud mirror job
- `C`: new cloud mirror job
- `r`: reload the jobs and ask the hosts again (otherwise the list is asked when the section opens and then once a minute while it is on screen)

## Scheduled jobs (`zfsctl daemon`)

`zfsctl daemon` runs scheduled jobs on the hub, unattended. It must run as root, like the TUI, and reaches other hosts over the same SSH + sudo path, so it needs no new sudoers entries. There are five job kinds (the cloud mirror and the backup are described below). A **scrub** job runs `zpool scrub` on a pool of any configured host on a cron schedule, watches it to completion and records the result. A **snapshot** job takes a snapshot of one dataset on a cron schedule and keeps only the newest `keep` of the snapshots it made. A **replicate** job copies the newest snapshot with a given prefix from a dataset on one configured host to a dataset on another. Manage jobs from the **Jobs** section of the TUI (see its keys above, or `J` on a pool in Pools), or edit `jobs.toml` by hand: the daemon notices either within a second. The TUI only reads the state directory, so it must run on the hub (as root) to see the daemon's jobs.

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

**A snapshot job** takes `zfs snapshot` of one dataset (`recursive = true` adds `-r`, so a whole tree is snapshotted atomically and pruned together) and names it `<prefix>-<UTC timestamp>`, for example `hourly-20261006T193000Z`. After a successful snapshot it destroys its own snapshots beyond the newest `keep`, oldest first. It only ever destroys names that match exactly `<prefix>-<YYYYMMDDTHHMMSSZ>`, so manual snapshots, other jobs' snapshots and look-alikes such as `hourly-keep-forever` are never touched. A snapshot with a hold is kept and the run says `kept N (held)`; it is not an error. If a snapshot cannot be destroyed for another reason (a clone) the run is recorded as failed with the reason, and the rest are still pruned. Hourly, daily and weekly tiers are separate jobs with separate prefixes. Two enabled snapshot jobs cannot share a host and prefix on the same dataset, or on a dataset inside a recursive job's tree (they would prune each other); the later one is reported as unable to run. The lock is per dataset and prefix, so an hourly and a daily job on one dataset that fall due together both run. `keep` counts the snapshot just taken, which is never pruned. Changing a job's prefix leaves the old prefix's snapshots in place.

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
- **Keep on target prunes the destination (`dest_keep`).** For a replication to a host, `dest_keep = N` keeps only the newest N of the job's own snapshots on the destination dataset and destroys the older ones at the end of each run, whether it sent something or found the destination up to date. Only names that are exactly `<prefix>-<UTC timestamp>` are ever considered (hand-made snapshots and look-alikes are never touched), the order is by creation (`createtxg`), the newest is always kept, and it looks at that one dataset only (no `-r`, no child datasets). A snapshot with a hold or a clone is left alone and is not an error; any other destroy failure fails the run, though the transfer itself succeeded. Nothing is pruned after a failed, cancelled or resumed transfer, or if the destination's listing cannot be read. Absent keeps everything (the default); it is rejected for file destinations, where stream files form chains. When you save a job in the TUI, the form first lists the destination's snapshots and, if the setting would destroy some now, shows how many and which and asks for a second keystroke; the daemon decides again at run time.
- **Resumable.** Because the receive is `-s`, an interrupted transfer (a network drop, a cancel, a daemon restart) leaves a resume token on the destination, and the next run continues with `zfs send -t` instead of starting over (`resumed an interrupted transfer`). If a token can no longer be continued (its source snapshot is gone) the run fails and says so; discard it by hand with `zfs receive -A <dataset>` on the destination host.
- **Progress and cancel.** The job panel shows `sent 12.3 GiB of 40.0 GiB (30%)  85.0 MiB/s  eta 5m 00s` for a running replication, taken from files the daemon writes once a second in `<state>/progress/`. `c` writes a request file in `<state>/cancel/`; the daemon notices it within a second, stops both processes and records the run as `cancelled`. A cancelled run is not counted as failed and does not notify. A daemon restart during a transfer ends it, closes the run as lost, and the next run resumes.
- **Rules.** Two jobs may send from the same dataset to different destinations; two enabled jobs cannot write to the same destination (their incrementals would corrupt each other), and a slot that falls due while the job's previous transfer is still running is skipped; the later one is reported as unable to run, and `Space` refuses to enable a job that would clash. Only `/sbin/zfs` is used on the hosts, so no new sudoers entries are needed.

**A file target** writes the stream to files in a directory on the hub instead of receiving it anywhere:

```toml
[[jobs]]
id = "repl-tank-data-to-files"
host = "local"
kind = "replicate"
dataset = "tank/data"
prefix = "hourly"
dest_host = "file"
dest_dir = "/mnt/backup/zfs-streams"   # absolute, on the hub
schedule = "30 3 * * *"
```

- Each run writes `tank-data@<snap>.zfs` (a full stream) or `tank-data@<from>..<to>.zfs` (an increment), the dataset name with `/` replaced by `-`. The first run, a run after the recorded snapshot was pruned or recreated on the source, one after `dest_dir` was changed, one after `replication.json` was lost, and one when the directory no longer holds the stream that ends at the recorded snapshot (a swapped or unmounted disk, files you pruned) are full streams; otherwise it is an increment from the snapshot the previous run wrote. `<state>/replication.json` records what each job last wrote (snapshot, guid and directory); it changes only after a file is complete.
- A file is written as `<name>.partial`, fsynced and renamed when complete, so a file without `.partial` is a complete stream. A failed or cancelled run removes its `.partial` (the daemon also removes leftovers at startup, only `*.zfs.partial` in the jobs' directories). File targets cannot resume: the next run starts again. The directory is created private (mode 0700) and new files are 0600 (modes mean nothing on FAT-style USB disks, which also cap a file at 4 GiB). An existing `dest_dir` is used as it is, but a directory that group or others can write to is refused (the daemon writes as root), `dest_dir` must be in canonical form (no `//`, `.`, `..` or trailing `/`), and the `.partial` file is always created fresh. Nothing is created, destroyed or rolled back on the source, and no remote host needs a new sudoers entry.
- **Editing.** Enter on a backup job reopens the form. The host and the dataset's pool cannot be changed: a run that crashed leaves its clone `<pool>/zit-restic-<job id>` behind, and a job pointed elsewhere would never clean it up. Delete the job and create a new one instead (the other fields, and the dataset within the same pool, can be edited).
- **Restoring.** The files are plain `zfs send` streams. Receive the full file, then each increment in order, into a dataset that does not exist yet: `zfs receive pool/restored < tank-data@hourly-A.zfs`, then `zfs receive pool/restored < tank-data@hourly-A..hourly-B.zfs`, and so on. There is no UI for restoring, and `zfsctl` never deletes or overwrites a complete stream: if a file with the new name already exists (a snapshot destroyed and recreated under the same name) the earlier one is kept as `<name>.zfs.old-1`, `.old-2`, and so on. Prune old files yourself, keeping every increment back to a full stream you still want to restore from.
- **Things to know.** The file name has the source dataset (`/` becomes `-`) but not the host, and `tank/a-b` and `tank-a/b` flatten to the same name, so give every file job its own directory (two enabled jobs naming the same directory are refused, but a symlink to it is not detected). Deleting a job does not delete its entry in `<state>/replication.json`; re-adding a job with the same id and directory continues from it, provided the directory still holds the stream the entry names, otherwise it sends a full stream.

**A backup job** runs `restic backup` on a snapshot of a dataset, on the job's host (the hub is `local`). The form's **Source** field says which snapshot: `new` takes a fresh snapshot at the start of every run and keeps the last few of its own (**Snapshots to keep**, default 3); `@name` always backs up that one snapshot (F2 lists the dataset's snapshots, whatever they are called); anything else is a prefix, and the job backs up the newest `<prefix>-<timestamp>` snapshot without taking any, so pair it with a snapshot job that uses the same prefix (F2 lists the prefixes in use). **Path** optionally limits the backup to a directory inside the dataset, such as `photos/2026`. Only the `new` source creates or destroys snapshots, and only its own `zit-<job id>-<timestamp>` ones; saving such a job asks you to confirm first.

```toml
[[jobs]]
id = "backup-local-tank-data"
host = "local"
kind = "backup"
dataset = "tank/data"
prefix = "hourly"                         # source: the newest hourly-<timestamp> snapshot (the default)
# source = "new"                          # or: take a fresh snapshot every run (keep = how many of them; default 3)
# source = "snapshot"                     # or: always one snapshot, named in:
# snapshot = "snap4"
# path = "photos/2026"                    # optional: only this directory inside the dataset
repository = "/mnt/usb/restic-tank"      # restic's -r: a path, sftp:user@host:/path, rclone:remote:path ...
password_file = "/root/.restic-password"  # absolute path on the host, mode 600
retention = "7d 4w 12m"                   # optional: daily/weekly/monthly/yearly/last; blank keeps everything
schedule = "30 3 * * *"
```

- **One-time setup.** `pkg install restic` on the host (0.16 or newer: jobs that share a repository wait up to ten minutes for each other's locks with `--retry-lock`); put the repository password in a root-only file (`install -m 600 /dev/null /root/.restic-password`, then write it); create the repository once with `restic init --repo <repository> --password-file <file>`. A backup job never creates a repository (a mistyped path would silently make a new one); an uninitialised repository or wrong password fails the run with restic's own message.
- **Check before you save.** `F5` in the backup form opens the repository with the password file on the job's host (`restic cat config`, read-only) and says what is missing: the rclone remote, the password file, a wrong password, a repository not yet created with `restic init`, or a restic that sudoers does not allow. It never creates anything, never blocks saving, and gives up after 45 seconds. It also adds `Note:` lines about the password file: a path under `/home`, `/usr/home`, `/tmp` or `/var/tmp`, a file that group or others can read or change, or one not owned by root (the mode and owner come from a plain `stat`, run without sudo; when the ssh user cannot see the file it says the permissions could not be checked). Notes are advice only and never block saving.
- **How a run works.** It picks the snapshot (taking a new one first for `source = "new"`; a pinned `snapshot` that no longer exists fails the run, a prefix with no snapshot skips it), skips (`up to date`) if the repository already holds a backup tagged `zfsctl-snapshot:<name>` from this job, then clones that snapshot read-only to `/var/run/zfsctl/<job id>`, backs up that path, and destroys the clone, whether the backup succeeded, failed or was cancelled. The path is the same every run on purpose: restic finds its parent backup by path, so unchanged files are skipped instead of read again. While the clone exists its snapshot cannot be destroyed: a snapshot job's prune leaves it for the next slot (`kept 1 in use by a clone`). A clone left by a crash is destroyed by the next run.
- **Retention.** With `retention` set, a successful backup is followed by `restic forget --prune` limited to this job's own backups from this machine (matched by tag and the machine's real hostname, so two hubs that share a repository never skip or forget each other's backups); without it nothing is ever forgotten. Backups are dated by their snapshot's creation time, so `7d` means seven days of data, not seven runs. `prune` takes the repository lock and can be slow or costly on a big remote repository.
- **Secrets.** The job holds no password or key, only the path of the password file on the host. The repository string is stored and shown in the Jobs table and details, so it must not hold credentials: a URL with a password in it (`rest:https://user:pass@host/`) is refused when the job is saved; use an rclone remote, whose config holds the keys. Direct S3/Azure/B2 environment variables are not supported, because an environment cannot pass through `sudo -n` without allowing `env`, which would make the sudoers entry root-equivalent.
- **sftp and rclone repositories.** Both were verified end to end (backup, retention prune, `restic check`, restore) with the job on the hub. The job host's root does the connecting, since restic runs through `sudo`: for `sftp:user@host:/path` root's ssh key must be authorised on the target and the target's host key accepted once (`ssh user@host` as root); for `rclone:remote:path` the remote must be defined in root's rclone config (`/root/.config/rclone/rclone.conf`), with `rclone` installed on the host. The daemon is started by rc with `HOME=/`; zfsctl gives the programs it runs the account's real home so rclone finds that config.
- **Cancel and locks.** `c` stops a running backup; restic then leaves a stale repository lock. A backup does not mind, but `forget --prune` does: so a run with `retention` runs `restic unlock` just before the forget (it removes only stale locks, never a live one, and never uses `--remove-all`; without this the forget would wait out its ten-minute `--retry-lock` for a lock nobody holds). A lock that is still held after that is a live one and the forget waits for it as usual; you can also run `restic unlock` yourself. A failed backup does not forget; a run cancelled while it was forgetting may already have forgotten some snapshots (restic's prune is safe to interrupt). restic exit code 3 (snapshot saved but some files unreadable) is reported as a failure that says so.
- **Restoring.** Use restic: `restic restore latest --repo <repository> --password-file <file> --tag zfsctl:<job id> --target /restore`. The backed-up path is `/var/run/zfsctl/<job id>`.
- **Remote hosts.** restic runs on the job's host over SSH + `sudo -n`, so it must be installed there and `/usr/local/bin/restic` added to that host's sudoers entry. That entry is as broad as `zfs` or `bectl`: restic as root reads any file and writes anywhere, so whoever holds the SSH key effectively has root on that host. The hub (`local`) needs no change.

**A cloud mirror job** (`kind = "rclone"`) copies a dataset to a cloud bucket as **plain files you can browse and download one by one**, with `rclone sync`. It reads from a temporary read-only clone of a snapshot, exactly like the backup job (same `source`, `snapshot`, `prefix`, `keep` and `path` fields), and writes `<remote>/current` so that it always equals the dataset. Whatever a run would replace or delete is first **moved** to `<remote>/old/<UTC date and time>/` (`rclone --backup-dir`); zfsctl never expires or deletes anything in `old/` (you do, by hand). `remote` is an rclone remote path such as `aws:mybucket/photos` or `b2:mybucket/photos` (a remote name from `rclone config`, a colon and a path; credentials stay in root's `rclone.conf` on the job's host, never in `jobs.toml`; a connection string like `:s3,access_key_id=...:` is refused). `bwlimit` is an optional speed limit in bytes per second (`5M`, `500K`).

```toml
[[jobs]]
id = "mirror-local-tank-photos"
host = "local"
kind = "rclone"
dataset = "tank/photos"
source = "new"
remote = "aws:mybucket/photos"
bwlimit = "5M"
schedule = "30 3 * * *"
```

- **What is copied.** Regular files. Symlinks, sockets, devices and empty directories are not copied, and owners and permissions are not kept on a bucket; a run that skips only such items is still OK and says nothing about them.
- **Safety.** zfsctl runs only `rclone sync` (with `--backup-dir`) and read-only listings: never `delete`, `purge`, `--delete-excluded` or `--links`. A run whose source has no files (an empty dataset, a wrong `path`, only symlinks) is refused before anything is touched, and so is a run whose clone could not be made. Two mirror jobs on one host cannot have overlapping remotes, and a mirror cannot contain or sit inside a restic job's `rclone:` repository.
- **Setup.** `pkg install rclone` and `rclone config` as root on the job's host, then `zfsctl print-sudoers HOST --with rclone` for a host other than the hub (the hub's own jobs run as root). The cloud key needs list, read, write and delete rights on the bucket: moving a file into `old/` is a copy followed by a delete. Use a key limited to that one bucket. Backblaze B2 keeps every file version by default, so set the bucket's lifecycle to "keep only the last version" unless you want a second, hidden history. Real AWS S3 and Backblaze B2 have not been tested with zfsctl yet; the job was verified end to end with a local-directory remote.
- **Check before you save.** `F5` in the form lists the remote read-only and says in plain words what is wrong (remote name not configured, bucket or folder not found, keys refused), and adds a note when root's `rclone.conf` can be read by other users.
- **Restoring.** Browse the bucket, or `rclone copy aws:mybucket/photos/current /restore`; an older version of a file is under `old/<date and time>/`.
- **Cancel.** `c` stops a running mirror and destroys the clone; a cancelled run is not a failure and the next run starts again.

**Notifications.** To be told when a job fails or a scrub finds errors, set a command in `config.toml`:

```toml
[daemon]
notify_command = "curl -s -d \"$ZFSCTL_JOB on $ZFSCTL_HOST: $ZFSCTL_MESSAGE\" ntfy.sh/my-zfs-alerts"
```

It runs through `sh -c` with `ZFSCTL_JOB`, `ZFSCTL_HOST`, `ZFSCTL_KIND`, `ZFSCTL_RESULT` and `ZFSCTL_MESSAGE` set, and is killed after 30 seconds. Job data only ever reaches it through those variables, never the command line, so a pool or message can never be executed as code. A failing hook is logged and never affects the job.

**Health alerts and the daily report.** The daemon can also watch every host and email you. Add to `config.toml` (every key but `mail_to` is optional):

```toml
[notify]
mail_to = "you@example.com"          # one address or several, comma separated; unset = no email
mail_command = "msmtp -a default -t" # default "msmtp -t"; zfsctl writes From/To/Subject/Date and the body on stdin
mail_from = "zfsctl@hub"             # default zfsctl@<the hub's hostname>
check_interval_minutes = 5           # default 5, at least 1
report_time = "07:00"                # the hub's local time; default 07:00; "off" disables the report
remind_hours = 6                     # repeat an unresolved alert this often; default 6, 0 = never
```

- **What is watched.** Every host is probed every `check_interval_minutes` with the same logic as the Hosts table (unreachable, ssh refused, a pool that is not ONLINE, 80% / 90% full, device or scrub errors, a failed job): nothing new runs on any host and no sudoers entry changes. The monitor runs only when `mail_to` or `notify_command` is set.
- **Alerts** go out when a host has been bad on **two probes in a row** (so a blip never pages you), when it gets worse (warning to critical), as a reminder every `remind_hours` while it stays bad, and when it has been good on two probes in a row again (`recovered after 1h 30m`). Subjects look like `[zfsctl] fbsd16: tank DEGRADED`. The alert state is not kept across a daemon restart: a host that is still bad alerts once more two probes after the start.
- **The daily report** (all hosts) goes out once per local day from `report_time`, also when the daemon was down at that time and comes back later the same day. It has a summary line, a table (state, type, tag, used, pools, last scrub, jobs failed/total), what needs attention and since when, and the events since the last report: job runs per kind and every failed run, and the alerts, recoveries and daemon starts. An all-green day still sends a short report, so silence means the sender is broken, not your hosts.
- **Job failures** are mailed too (`[zfsctl] job backup-x failed on fbsd16`), besides running `notify_command`.
- **Delivery.** A message the mail command could not take is retried on the next ticks, at most 6 times, then dropped (and logged); the hook runs independently of the mail command. The commands run with the package directories on `PATH` and root's real home, so `msmtp` and `~/.msmtprc` are found although rc starts the daemon with a minimal environment.
- **Pick your own way to send.** zfsctl has no mail client of its own and does not care which one you use: `mail_command` is any shell command that reads one complete email (headers, a blank line, the body) on its standard input and sends it. The default is `msmtp -t` only because it is small and easy to set up. Anything that reads a message and takes the recipients from its headers (`-t`) works:
  - `mail_command = "msmtp -a default -t"`: msmtp with an account from `~/.msmtprc` (the daemon runs the command with root's home, so root's `~/.msmtprc` is the one read).
  - `mail_command = "sendmail -t"`: the system's own mail setup, whatever it is (Postfix, Exim, OpenSMTPD, dma, FreeBSD's `mailwrapper`). Use this when the hub already delivers its system mail.
  - `mail_command = "/usr/local/bin/my-mailer"`: your own script or tool (a mail API client, a relay you wrap): it only has to read the message from stdin and exit 0 once it has taken it. A non-zero exit, or no exit within 30 seconds, counts as a failed delivery (the message is retried on the next checks, at most 6 times).
  - No mail at all: leave `mail_to` unset and use `notify_command` below, or use both.

  Credentials and TLS belong to the tool you choose (`~/.msmtprc`, your MTA's config); zfsctl never sees them. `zfsctl notify-test` shows whether your command accepts a message, and the Alerts screen's `t` does the same from the TUI.
- **Telegram, ntfy and the like** work through `notify_command`, which now also receives the health messages: `ZFSCTL_KIND` is `health-alert`, `health-reminder`, `health-recovered`, `health-report` (job failures keep their job kind), with `ZFSCTL_HOST`, `ZFSCTL_RESULT` (`ok`, `warning`, `critical`), `ZFSCTL_MESSAGE` (the subject), `ZFSCTL_BODY` (the full text, cut at 32 KiB) and the whole body on stdin. For example, with the bot token in a root-only file: `notify_command = "curl -s --data-urlencode \"text=$ZFSCTL_MESSAGE\" -d chat_id=123456 \"https://api.telegram.org/bot$(cat /root/.telegram-token)/sendMessage\""`.
- **Try it.** `zfsctl notify-test` sends a short test message on every configured channel and says which worked (it sends real mail). `zfsctl report` prints the report as it would be sent now, subject first, and sends nothing (`zfsctl report --state-dir DIR` reads the job history and event log of a daemon started with `--state-dir DIR`).
- **The Alerts screen.** The last section of the sidebar shows the saved `[notify]` settings (a dimmed `(default)` marks every key you have not set, and the hook command is shown read-only), whether the monitor is on, whether the daemon is running, the date of the last daily report and the recent alerts, recoveries and daemon starts (Up/Down scroll). `e` edits the settings in a form (a blank field means the default; clear *Email to* to turn email off): it refuses what the daemon would refuse, with the same message, and asks for the usual two-key confirmation before saving. `t` sends a real test message through the **saved** settings after asking (it names the recipient and the command). `p` builds the report as it would be sent now, in the background, and shows it in a pop-up without sending or recording anything.
- **How the screen saves.** Only the `[notify]` section of `config.toml` is replaced (or appended); every other byte stays, so your comments, hosts and other sections are untouched. The previous file is copied to `config.toml.bak` first, the new text goes through `config.toml.tmp` and a rename, and the result is parsed and checked before anything is written. A `[notify]` written as dotted keys (`notify.mail_to = ...`), an inline table or a sub-table is refused with a note to edit by hand. The daemon applies a saved change within seconds, keeping its alert state.
- **Not done:** acknowledging or silencing an alert, per-host thresholds, HTML mail, and monitoring the daemon itself (a dead daemon sends nothing: the missing daily report is that signal, and the TUI shows the heartbeat).

## Status

Version 0.2, in development. It manages pools, datasets and their properties, snapshots, disks, NFS and SMB shares, boot environments, delegated permissions and ACLs, native encryption keys, and scheduled snapshot, replication, restic backup and rclone mirror jobs, with alerts and a daily report. The book (`docs/book`) documents every screen.

### What is tested, and what is not

Tested:
- **Unit tests:** over two thousand, running against fake command runners (no real ZFS needed). Most behaviour that can go wrong, such as argument order, refusals and what a failing command reports, is pinned there.
- **Mutation testing:** `cargo mutants` was run on the newer, safety-critical code (encryption, the job guards, replication, the Dashboard), and the mutants that survived were pinned with tests. The memory wipes on secrets cannot be tested from safe Rust and are not covered.
- **Independent review:** each major change was reviewed by a separate AI reviewer before it was merged, and its findings were fixed.
- **Live on real machines:** a FreeBSD 15 hub and three FreeBSD VMs (one on 16.0), with real pools, snapshots, replication between hosts, restic backups and rclone mirrors to Backblaze B2 and S3, boot environments including real reboots, and encryption keys loaded, changed and unloaded through the TUI.

Tested less, or not live:
- **Linux and illumos hosts:** checked on Ubuntu 24 and OmniOS, but far less than FreeBSD. Sections a host's OS cannot support are dimmed rather than half-working. Expect rough edges there.
- **Raw replication of encrypted datasets:** the `zfs send -w` flow was verified by hand on FreeBSD, and the job logic is covered by tests with fake hosts. It has not run as a long-lived daemon job.
- **Restic backups of remote hosts:** local hub backups were verified live; remote hosts were not.
- **Not security-audited.** zfsctl runs commands as root through sudoers rules you install yourself. Read what `zfsctl print-sudoers` generates before installing it.

How it protects data: every change needs `y` then `Enter`; what cannot be undone makes you type the name; recursive destroys refuse a pool's root and anything holding the running system; a replication or mirror of an encrypted dataset is refused unless it is explicitly raw; passphrases reach `zfs` only on standard input, never in an argument, a log or a file.

Please try it on a test pool first, and keep backups of anything you care about.

## Built on

zfsctl is a thin layer over other people's work. None of it is bundled; all of it deserves credit and a visit:

- [OpenZFS](https://openzfs.org/): the file system, volume manager and the `zpool`/`zfs` tools that zfsctl drives.
- [FreeBSD](https://www.freebsd.org/): the operating system of the hub, and of most of the test machines.
- [illumos](https://illumos.org/) and [OmniOS](https://omnios.org/): the descendant of OpenSolaris, where ZFS was born at Sun; OmniOS is the distribution used for testing.
- [Linux](https://www.kernel.org/) (tested on [Ubuntu](https://ubuntu.com/)) with [OpenZFS on Linux](https://openzfs.github.io/openzfs-docs/).
- [Rust](https://www.rust-lang.org/): the language, with [ratatui](https://ratatui.rs/) for the screen and [crossterm](https://github.com/crossterm-rs/crossterm) for the terminal.
- [restic](https://restic.net/) and [rclone](https://rclone.org/): backups and cloud mirrors, when you use those job kinds.

## Licence

BSD 2-Clause: see `LICENSE`. Copyright (c) 2026, fedor4445.
