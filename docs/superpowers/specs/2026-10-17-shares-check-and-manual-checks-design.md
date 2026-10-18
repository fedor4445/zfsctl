# Shares check on the Dashboard, and checks on demand: design

Date: 2026-10-17. Status: draft for approval. Builds on `2026-10-17-disk-health-design.md`.

## Why

The Dashboard shows nothing about file sharing: a stopped NFS server or Samba service, with shares that need it, goes
unnoticed until a client complains. The user wants that on the Dashboard, checked once a day together with the SMART
disk check (decided), plus a way to run the checks on demand, per host ("no need to bother good running servers"),
and a red message on the Dashboard while the automatic check is running, so a manual check does not collide with it.

## What

1. **The daily check also checks shares.** The daemon's daily pass (today: `smartctl` on every host) also asks every
   host about its shares, with the code the Shares section already uses (`zfs::share::list_shares_with_availability`:
   is the NFS server up, is Samba up, which shares exist). The answer goes to `<state_dir>/shares.json`, one entry per
   host, with its own `checked_at`, written atomically, a failed check keeping the last good result with an `error`
   (same rules as `smart.json`).
2. **What is a problem:** a host that has NFS shares while the NFS server is not running, or SMB shares while Samba is
   not running: red (issue), `fbsd16  NFS: 2 shares but nfsd is not running   (checked 3h ago)`; Samba reads
   `Samba: 1 share but smbd is not running`. A share error the Shares section already reports (`smb_error`, the managed
   block of `smb4.conf` unreadable) is yellow. A host with no shares, or a service that is off with no shares needing
   it, shows nothing. A host where the check cannot run (no `shares`/`samba` sudoers feature, sudo denied) is silent,
   like a host without `smartctl`. A host that cannot be reached shows nothing here (the Hosts row says so).
   Enter on the line goes to the Shares section of that host.
3. **Checks on demand, per host:** in the **Hosts** section, `c` runs both checks (disks and shares) on the selected
   host, in the background, from the TUI itself with the same code as the daemon (it needs no daemon). The result is
   merged into `smart.json` and `shares.json` (only that host's entries; the rest is kept), so the Dashboard, the
   report and the daemon all see it. The status line says `Checking fbsd16...` and then what was found
   (`fbsd16 checked: 2 disks fine, 1 share problem`). One manual check at a time; `c` while one runs, or while the
   daemon's pass runs, does nothing but say why. It never starts a self-test and only reads.
4. **A red message while a check runs:** the Dashboard shows a red line under its header while any check is running,
   by the daemon (`daily disk and shares check running since 03:30`) or by the TUI
   (`checking fbsd16: disks and shares since 14:02`). It is a notice, not a problem: it does not count in the
   verdict. It comes from a marker file `<state_dir>/checks-running.json` (`{by, since, host?}`) that whoever starts a
   check writes first and removes when done; a marker older than 10 minutes is ignored (a crashed run must not
   leave the Dashboard red).
5. **The daily report** lists share problems under needs attention, like the disk lines. No push alerts.
6. **The daily time setting:** the key `[daemon] smart_check` (introduced today, not yet in anyone's config) becomes
   `daily_check`, because it now covers both checks; `smart_check` is still read as an alias. Same values (`"HH:MM"`,
   `"off"`, default `03:30`). The book says so.

## Rules

- Read-only everywhere: the existing Shares code only reads (`service ... onestatus`, `zfs get`, reading `smb4.conf`);
  `smartctl` as before. Nothing new is needed in sudoers: hosts that already have the `shares`/`samba` features work,
  others stay silent.
- A hung or failing host never delays the others or the daemon: per-host deadline and budget as for SMART; the shares
  check has its own short deadline inside the same per-host budget.
- Both files and the marker are written atomically. Missing, torn or old-shaped files show nothing and never crash.
- The manual check and the daemon's run write the same files; each write is a read-modify-write of whole host entries
  followed by an atomic rename, so the worst case is that the later write wins, never a torn file. The red notice and the
  refusal while the other side runs keep that rare.
- The Dashboard's SMART staleness line ("disk check is N days old") now covers both files' newest check.

## Decisions made after the builder check (my calls, flagged for the user)

- **Stopped is not denied.** The Shares section's `nfs_server_available` / `samba_available` turn every non-zero exit
  (a refusal by sudo, an ssh failure) into "not running". The new check does not reuse them: it runs the same
  `service ... onestatus` itself and tells three outcomes apart: exit 255 = host unreachable (failed, keeps the old
  entry), a non-zero exit whose stderr starts with `sudo:` (or says the command is not found) = not set up (silent), any
  other non-zero exit = really stopped. The share listing (`zfs get sharenfs`, `smb4.conf`) is reused; a refusal by
  sudo there is "not set up" too. The Shares section is not changed.
- **Success replaces, failure keeps, manual runs too.** A check that answers (even "no shares, nothing wrong")
  replaces the host's entry, so removing the shares and pressing `c` clears the line. A check that is unreachable,
  denied or has nothing to report keeps the previous entry with an `error` note, exactly as the daily run does. A half
  that worked is stored even if the other half failed (disks and shares are stored separately).
- **The running marker has a heartbeat.** `checks-running.json` is `{by, since, beat}`; whoever runs refreshes `beat`
  every 20 seconds, and a marker whose `beat` is older than 3 minutes is stale and ignored. A long daily pass therefore
  stays visible, and a crashed one disappears within 3 minutes. Removal is owner-checked (only the writer of that
  `by`+`since` removes it). The daemon does not start its pass while a fresh TUI marker exists (it tries again at its
  next wake, five minutes later); the TUI refuses `c` while any fresh marker exists.
- **Same user.** The daemon and the TUI both run as root on the hub (the book requires it), so the files have one owner.
- **Staleness line:** "disk and shares check is N days old" uses the newest `checked_at` over both files; a missing file
  is "no data", not "never checked", and no line shows when neither file has any data.
- **Manual check order:** disks, then shares, one host, in sequence in one background thread; each half is written as
  soon as it is done. The status line names what each half found (`fbsd16: disks fine, shares: NFS stopped with 2
  shares`, `fbsd16: disks fine, shares not reachable`).

## Not in scope

- Linux/illumos commands (`systemctl`, `svcs`): the Shares code is FreeBSD-only today and moves with the multi-OS work.
- Checking that a share is actually reachable from a client (a mount test), share contents, SMART self-tests, history,
  push alerts, a hub-wide "check all hosts now" key.

## Left to the builder

File shapes, where the running marker is taken (a guard that always removes it, also on panic), key hint wording, how
the TUI's background check reports back (follow how the Alerts test message or Cloud bucket sizes already do it),
tests in the repo's style, the book (chapter 6, 12, 21) and README.

## Done means

Spec, plan, implementation with tests (fake runners for each outcome: running, stopped with shares, stopped without,
denied, unreachable; marker stale and fresh; manual check refusals), whole-branch review, live check on the hub and the
VMs (stop `nfsd` on a VM with an NFS share and see the red line after a manual `c`; start it again, `c` clears it),
book updated, merged and installed.
