# Reference: keys

## What you will learn

- Every key of every section, in one place.

This list is checked against the program's own Help screen (`?`) by the book's tests, so it cannot drift from it. The hint bar at the bottom of the screen always shows the keys that work right now; where a key behaves differently in two places, the heading says where.

> **Tip:** `Esc` always means "back out", and every question that can change data takes two keys (`y`/`n`, then `Enter`). See [the screen](06-the-screen.md).

## Global

| Key | What it does |
|---|---|
| `Tab / Shift+Tab` | switch sections |
| `Up / Down` | select a row (host/pool/dataset/snapshot/disk/share, or a property/permission/ACL row) |
| `r` | refresh the focused pane |
| `F2` | in a form, on a field with known values (Permissions, Flags, Type, Recipient, Valid user, Root user mapping, a pool property with fixed values such as failmode or autoexpand, a from-disks Redundancy, a job's Host, Pool or Dataset, a replication's Destination host, a backup's Source (`new`, the prefixes the dataset uses, and each snapshot as `@name`), and Schedule): open a picker (type filter, Enter OK, Esc cancel; Space toggles in multi-select ones) |
| `F5` | in the backup job form: check the repository and password file on the job's host; in the cloud mirror form: check the remote (both read-only; never block saving), with `Note:` lines when the password file is in a home or temp directory or other users can read it |
| `Esc` | dismiss a status message, back out of a focused pane, or cancel a modal (in a y/n confirmation Esc selects No, then Enter) |
| `y / n, Enter` | a y/n confirmation takes two keys: y (yes) or n / Esc (no) selects, then Enter applies it; Enter alone does nothing. Forms that cannot be undone (create pool, add vdev, replace disk, revoke, shrinking a volume, pool features) end in one, showing the exact command |
| `?` | this Help screen |
| `!` | open a shell on the selected host (remote hosts: your unprivileged ssh login); exit returns to zfsctl |
| `A` | About: the zfsctl wordmark, the version and what the tool is for (include the version when you report a bug) |
| `q` | quit |

## Hosts

| Key | What it does |
|---|---|
| `Up / Down` | select the active host |
| `/` | filter the host list by name, address, tag or type (Enter keeps it, Esc clears it) |
| `c` | check the selected host's disks (`smartctl`) and shares (NFS, Samba) now; the result joins the Dashboard (read-only) |
| `n` | add a new host (prompts for Name, SSH target and an optional Tag) |

## Pools

| Key | What it does |
|---|---|
| `Enter` | drill into vdev/disk tree |
| `Right / Left` | focus/unfocus the properties pane |
| `n / d` | create / destroy a pool |
| `s / R` | scrub / resilver (y/n confirm) |
| `J` | schedule a recurring scrub of the selected pool (opens the job form) |
| `J` | (in Datasets) schedule recurring snapshots of the selected dataset |

## Vdevs (inside a drilled-into pool)

| Key | What it does |
|---|---|
| `o / O` | offline / online the selected disk |
| `x` | replace the selected disk |
| `a` | add a vdev/mirror/raidz group, or a log, cache, special, dedup or spare device |
| `X` | remove the selected log, cache or spare device (a plain question first) |

## Pool/Dataset properties pane

| Key | What it does |
|---|---|
| `Enter` | edit the selected property |
| `r` | re-fetch for the current pool/dataset |

## Datasets

| Key | What it does |
|---|---|
| `Right / Left` | focus/unfocus the detail pane |
| `n / d` | create / destroy a dataset (type its name; zfs refuses if it has snapshots or children) |
| `D` | destroy a dataset AND its child datasets and snapshots (type its name; says how many; never -R) |
| `s` | snapshot the selected dataset |
| `E` | create an encrypted dataset (name, cipher, passphrase twice, then type the name) |
| `L / U / C` | load / unload / change the key of the selected encrypted dataset (its encryption root) |
| `N` | create/edit an NFS share |
| `M` | create/edit an SMB share |
| `'[' / ']'` | cycle Properties / Permissions / ACL sub-tabs (once focused) |

## Datasets: Permissions sub-tab

| Key | What it does |
|---|---|
| `n / s / c` | grant: to a recipient / as a named set / as create-time |
| `d / x` | revoke everything / revoke specific permissions |

## Datasets: ACL sub-tab

| Key | What it does |
|---|---|
| `n` | add an ACL entry (always appended at the end) |
| `d` | remove the selected entry by position |

## Snapshots

| Key | What it does |
|---|---|
| `d` | destroy (type dataset@name to confirm) |
| `b` | roll back (type dataset@name to confirm) |
| `H` | hold the selected snapshot, or release zfsctl's hold on it (a hold stops it being destroyed or pruned; other tags are never released) |
| `c` | show what changed since the previous snapshot, by folder (read-only): `Enter` opens a folder, `/` inside a folder filters its entries by name as you type (`a|b` for either, `*` `?` wildcards), `[` and `]` set how deep the folders go, `H` holds the older snapshot, `Esc` goes back and closes |
| `C` | the same, but compared with the live dataset: what has changed since this snapshot |
| `f` | look inside the selected snapshot (read-only, names and metadata only; other hosts need the `browse` sudoers feature): `Enter` opens a folder, `←` goes up, `/` filters by name as you type (`a|b` for either, `*` `?` wildcards), `m` `s` `t` by modified date, size and type, `x` clears the filters, `Esc` closes |
| `R` | in the snapshot browser, on a regular file: copy it back next to the original under a new name (asks for the name, then shows the exact command; `y` then `Enter` runs it; it never replaces a file; other hosts need the `restore` sudoers feature) |

## Disks

| Key | What it does |
|---|---|
| `Space` | toggle-select a disk |
| `n / a` | create a pool / add a vdev from the selected disk(s) |
| `Esc` | clear selection |

## Shares

| Key | What it does |
|---|---|
| `Enter` | edit the selected share |
| `d` | remove the selected share |

## Boot Envs

| Key | What it does |
|---|---|
| `n` | new boot environment (F2 on the second field picks one to clone) |
| `a` | make the selected one the boot environment for every future boot (confirms) |
| `t / T` | use it for the next boot only (confirms) / clear that |
| `R` | rename |
| `m / u` | mount / unmount (the Mountpoint column shows where) |
| `d / D` | destroy (type its name) / destroy and remove its origin snapshot |

## Jobs

| Key | What it does |
|---|---|
| `Up / Down` | select a scheduled job |
| `Enter` | edit the job (host, pool, schedule, catch up) |
| `n` | new scrub job (or press J on a pool in Pools) |
| `N` | new snapshot job (or press J on a dataset in Datasets) |
| `R` | new replication job |
| `B` | new backup job (restic: back up a snapshot through a temporary clone) |
| `C` | new cloud mirror job (rclone: copy a snapshot clone to a cloud bucket as plain files) |
| `c` | cancel the selected replication or backup while it is running |
| `Space` | enable / disable the job |
| `d` | delete the job (y/n; its past runs stay in the history) |

## Dashboard

| Key | What it does |
|---|---|
| `Up / Down` | select a line of Needs attention, then (with tags) a group row, then the data-safety line |
| `Enter` | go to the section that owns that line; on an unprotected-data line, open the form that makes the missing job, or (on "and N more") the list of the unprotected datasets ; on a group row, open the Hosts list narrowed to that tag; on the data-safety line, show where each dataset's data lives (the Dashboard itself changes nothing) |
| `e` | with tags: list every host under its group, or only the ones that need a look |
| `r` | reload the jobs, alerts, cloud list and `[dashboard]` settings |

## Cloud

| Key | What it does |
|---|---|
| `Up / Down` | select an rclone remote (the hub's, and those of hosts that have a cloud job) |
| `s` | measure the bucket: the size of `current/` and `old/` of the remote's cloud mirror jobs (reads the file list, which a cloud account may count as requests) |
| `F5` | check the selected remote (read-only): the remote of its cloud mirror job, or, when no job uses it yet, its top level |
| `n` | set up a provider: a list (Backblaze B2, Amazon S3, Google Drive, OneDrive, Dropbox, SFTP...) and the steps for the one you pick; nothing is created |
| `C` | new cloud mirror job |
| `r` | refresh the list of remotes and jobs |

## Alerts

| Key | What it does |
|---|---|
| `e` | edit the alert settings (saved into the [notify] section of config.toml only; y/n to confirm) |
| `t` | send a test message through the saved settings (a real message; asks first) |
| `p` | preview the daily report as it would be sent now (sends nothing) |
| `Up / Down` | scroll the recent events |

## Settings

| Key | What it does |
|---|---|
| `Up / Down` | select a setting |
| `Enter` | edit the setting in a form (blank = default; saved into `config.toml` after a y/n confirmation) |
| `r` | reload the settings from `config.toml` |

## Try it

Press `?` in zfsctl and compare it with this list; they are the same words.

## Where next

- [Configuration reference](21-reference-config.md)
- [Commands reference](22-reference-commands.md)
