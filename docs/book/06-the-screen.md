# The screen

## What you will learn

- The three areas of the screen and how to move between them.
- How the hint bar, the Help screen and the confirmations work.
- How colours, themes and refreshing behave.

## The three areas

Here is the Pools section with nothing selected yet (a real capture; the pools belong to the author's test hub):

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — ? ('r' refresh, Ente┐
│Hosts                 ││Name                  Health       Used          ││Property    Value      Source    │
│Pools                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Datasets              ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Snapshots             ││zroot                 ● ONLINE     ██░░░░░░ 24%  ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                  ││                                                 ││                                 │
│Alerts                ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      ││                                                 ││                                 │
│                      │└─────────────────────────────────────────────────┘└─────────────────────────────────┘
│                      │┌Metrics─────────────────────────────────────────────────────────────────────────────┐
│                      ││nothing selected                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  → Properties  n New pool  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **The sidebar** (left) lists the sections: Dashboard, Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs, Cloud, Alerts and Settings. The highlighted one is where you are. zfsctl opens on the Dashboard.
- **The list** (top right) shows the items of that section: hosts, pools, datasets and so on. In Pools and Datasets a second pane on the right shows the selected item's properties.
- **The panel** (bottom right) shows live details of whatever is selected: space, health, counts, and two graphs of read and write activity. It disappears on a very short terminal so that the list keeps room.
- **The hint bar** is the last line of the screen. It lists exactly the keys that work right now, and it changes as you move: a key that would do nothing is not shown. When there is a message for you (a result, an error), it appears on the line just above.

> **Coming from Windows:** the sidebar is like the tree on the left of Disk Management or an MMC console; the list is the main window; the panel is the status area.

> **Coming from Linux:** the movement keys are the ones you know from tools like `htop` or `k9s`, minus the vi bindings: arrows and `Tab`.

## The Dashboard

zfsctl opens on the **Dashboard**, the one screen that answers "is anything wrong, or about to be?". It has no boxes: just headings, aligned columns and colour.

```screen
┌zfsctl────────────────┐ Dashboard  ·  Sat 17 Oct 14:08                                                                                                 ▲ 4 to watch
│Dashboard             │
│Hosts                 │ NEEDS ATTENTION ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│Pools                 │   ▲ watch fbsd16    zroot never scrubbed
│Datasets              │   ▲ watch fbsd16t1  zroot never scrubbed
│Snapshots             │   ▲ watch fbsd16t2  zroot never scrubbed
│Disks                 │   ▲ watch local     zroot last scrub 41 d ago
│Shares                │
│Boot Envs             │ HOSTS ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│Jobs                  │  Host      State                        Pools  Capacity             Memory             CPU              Load  Scrub     Errors
│Cloud                 │  local     ▲ zroot last scrub 41 d ago  3      ━━━━╌╌╌╌╌╌ 42%       ━╌╌╌╌╌╌╌╌╌ 14%     ╌╌╌╌╌╌╌╌╌╌ 0%    0.24  41 d ago  0
│Alerts                │  fbsd16    ▲ zroot never scrubbed       1      ━━━━━━╌╌╌╌ 61%       ━━╌╌╌╌╌╌╌╌ 18%     ╌╌╌╌╌╌╌╌╌╌ 1%    0.24  never     0
│                      │  fbsd16t1  ▲ zroot never scrubbed       2      ━━━━━╌╌╌╌╌ 52%       ━━━━╌╌╌╌╌╌ 44%     ╌╌╌╌╌╌╌╌╌╌ 2%    0.55  never     0
│                      │  fbsd16t2  ▲ zroot never scrubbed       2      ━━━━━━━╌╌╌ 73%       ━━━━━╌╌╌╌╌ 45%     ╌╌╌╌╌╌╌╌╌╌ 2%    0.30  never     0
│                      │
│                      │ DAEMON AND JOBS ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● daemon  running (pid 16127), heartbeat 1s ago
│                      │   ● jobs    scrub 1/1 ok  cloud 2/2 ok
│                      │
│                      │ CLOUD ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● b2 (b2)  1 job  last never
│                      │   ○ t1 (sftp)  no jobs
│                      │   ● zfsctl-aws (s3)  1 job  last never
│                      │
│                      │ ALERTS ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
│                      │   ● monitor on · last alert fbsd16 zroot 86% full (6d ago) · report 2026-10-17
└──────────────────────┘
↑↓ Move  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

- **The header** names the screen and gives the verdict on the right: `all quiet`, or how many things need attention (`1 issue, 1 to watch`). Until every host has answered its first probe it says `checking...`, because a screen that does not know yet must not say all is quiet.
- **Needs attention** lists the problems, worst first. `✖ issue` (red) is wrong now; `▲ watch` (yellow) is an early warning from today's numbers, such as a pool at 85% or a memory reading that is too high. When all is well it says `nothing needs attention`. It shows six lines and counts the rest (`+3 more`).
- **Hosts** has a row per host. State is the same verdict as in the Hosts section, raised (never lowered) by the readings that only the Dashboard has. **Capacity** is how full the fullest pool. **Memory** is memory in use *not counting* free pages, inactive pages and the ZFS cache: on FreeBSD the cache fills most of the RAM by design and gives it back when asked, so counting it would make the screen alarm all the time. **CPU** is the busy share between the last two probes (about 30 seconds). **Load** is the one-minute average and is never coloured. **Scrub** is how long since the last finished scrub (a resilver does not count). **Errors** is the read, write and checksum errors ZFS has counted.
- **Daemon and jobs, Cloud, Alerts** are one line each: whether the daemon runs and how fresh its heartbeat is, how many jobs of each kind are fine (`backup 0/1 FAILED`), each cloud remote with its last result (a bucket's size appears only if you measured it in Cloud), and whether the alert monitor can send mail.
- **Unknown is never green.** A host that has not answered, or a reading that cannot be taken yet (the first CPU figure needs two probes), is shown as `?` in dim text. Every status has a glyph and a word as well as a colour, so the `mono` theme reads the same.

| Reading | Yellow (watch) | Red (issue) |
|---|---|---|
| Capacity | 80% | 90% |
| Memory in use | 90% | 97% |
| Swap in use | 50% | 90% |
| CPU, last two probes | 95% | never |
| Last scrub | 35 days, or never | 90 days |
| Host does not answer | | unreachable |

These defaults can be changed under `[dashboard]` in `config.toml` (see the configuration reference). To stop a pool nobody scrubs, such as a scratch pool, from keeping the screen yellow, list it in `scrub_ignore = ["scratch"]`.

> **Safety:** the Dashboard only displays. `Up` / `Down` select a line of Needs attention and `Enter` takes you to the section that owns it; that is all. To fix something, go there: scrub a pool in Pools, look at a failed job in Jobs.

> **Tip:** gauges (`━━━━╌╌╌╌╌╌`) appear when the terminal is wide enough. On a narrower one they go first, then the least useful columns; the host's name and state always stay. It stays readable at 80 columns.

> **Not built yet:** NFS and Samba availability, disk wear (SMART) and predictions such as "this pool fills in 40 days". The first needs a privileged check per host on every probe, the second needs smartmontools on each host, the third needs a history of samples that zfsctl does not keep.

## Settings

The last section of the sidebar edits the parts of `config.toml` that no other screen has: the **theme** and the **auto-refresh** interval (`[ui]`), the **Dashboard thresholds** (`[dashboard]`: memory, swap, CPU, scrub age, and the pools the Dashboard does not judge by scrub age) and the **cloud caps** (`[cloud.caps]`, one row per rclone remote). Up and Down select a row and `Enter` opens a small form for it; a blank field goes back to the default, and a dimmed `(default)` marks every setting you have not set. A pair such as *Memory warn / crit* needs both numbers (or both empty), with warn above 0 and below crit. A save asks for the usual two-key confirmation, then writes only that key into `config.toml`: comments, hosts and every other section stay as they were, and the old file is kept as `config.toml.bak`. The change shows at once, with no restart: the theme repaints, the Dashboard uses the new thresholds and the Cloud screen the new caps. The email and report settings stay on the Alerts screen, and the hook command is edited in the file.

## Moving around

| Key | What it does |
|---|---|
| `Tab` / `Shift+Tab` | next / previous section (the sidebar wraps around) |
| `Up` / `Down` | select the previous / next row |
| `Right` / `Left` | move into / out of the properties pane (Pools, Datasets) |
| `Enter` | open the selected item (for example drill into a pool) or edit it |
| `Esc` | go back one step: close a message, leave a pane, cancel a form |
| `r` | refresh what you are looking at |
| `?` | the Help screen |
| `A` | About |
| `!` | open a shell on the selected host |
| `q` | quit |

The full list, section by section, is in [the keys reference](20-reference-keys.md).

## Help

Press `?` at any time. The Help screen lists every key for every section:

```screen
┌zfsctl───────────────┌Help────────────────────────────────────────────────────────────┐─────────────────────┐
│Hosts                │Global                                                          │   Type   Used  Pools│
│Pools                │  Tab / Shift+Tab  switch sections                              │   vm     42%   3    │
│Datasets             │  Up / Down        select a row                                 │   vm     55%   1    │
│Snapshots            │(host/pool/dataset/snapshot/disk/share, or a                    │   vm     72%   2    │
│Disks                │property/permission/ACL row)                                    │   vm     72%   2    │
│Shares               │  r                refresh the focused pane                     │                     │
│Boot Envs            │  F2               in a form, on a field with known values      │                     │
│Jobs                 │(Permissions, Flags, Type, Recipient, Valid user, Root user     │                     │
│Alerts               │mapping, a pool property with fixed values such as failmode or  │                     │
│                     │autoexpand, a from-disks Redundancy, a job's Host, Pool or      │                     │
│                     │Dataset, a replication's Destination host, and Schedule): open a│                     │
│                     │picker (type filter, Enter OK, Esc cancel; Space toggles in     │                     │
│                     │multi-select ones)                                              │                     │
│                     │  Esc              dismiss a status message, back out of a      │                     │
│                     │focused pane, or cancel a modal (in a y/n confirmation Esc      │                     │
│                     │selects No, then Enter)                                         │                     │
│                     │  y / n, Enter     a y/n confirmation takes two keys: y (yes) or│                     │
│                     │n / Esc (no) selects, then Enter applies it; Enter alone does   │                     │
│                     │nothing. Forms that cannot be undone (create pool, add vdev,    │                     │
│                     │replace disk, revoke, shrinking a volume, pool features) end in │                     │
│                     │one, showing the exact command                                  │                     │
│                     │  ?                this Help screen                             │                     │
│                     │  !                open a shell on the selected host (remote    │                     │
│                     │hosts: your unprivileged ssh login); exit returns to zfsctl     │                     │
│                     │  A                About                                        │─────────────────────┘
│                     │  q                quit                                         │─────────────────────┐
│                     │                                                                │BSD_g6330a45b0       │
│                     │Hosts                                                           │E)                   │
│                     │  Up / Down        select the active host                       │ 62.2G               │
│                     │  /                filter the host list by name, address, tag or│  hit 99.3%          │
│                     │type (Enter keeps it, Esc clears it)                            │e healthy            │
│                     │  n                add a new host (prompts for Name, SSH target │                     │
│                     │and an optional Tag)                                            │                     │
│                     │                                                                │                     │
│                     │Pools                                                           │                     │
│                     │  Enter            drill into vdev/disk tree                    │                     │
│                     │  Right / Left     focus/unfocus the properties pane            │                     │
│                     │  n / d            create / destroy a pool                      │                     │
│                     │  s / R            scrub / resilver (y/n confirm)               │                     │
│                     │  J                schedule a recurring scrub of the selected   │                     │
│                     │pool (opens the job form)                                       │                     │
│                     │  J                (in Datasets) schedule recurring snapshots of│                     │
│                     │the selected dataset                                            │                     │
│                     │                                                                │                     │
│                     │Vdevs (inside a drilled-into pool)                              │                     │
│                     │  o / O            offline / online the selected disk           │                     │
│                     │  x                replace the selected disk                    │                     │
│                     │  a                add a vdev/mirror/raidz group                │                     │
│                     │                                                                │                     │
└─────────────────────│Pool/Dataset properties pane                                    │─────────────────────┘
any key Close         └────────────────────────────────────────────────────────────────┘
```

It is part of the program, so it is always right for the version you are running; any key closes it. (On a small terminal the window may cut off the end of the list; the keys reference in this book has everything.)

## Confirmations

The actions that destroy or overwrite data ask first, and ask in a way that a slip cannot answer. (Ordinary edits, such as setting a property like `quota` or `readonly`, creating a dataset or a snapshot, or saving a share, apply when you submit the form: the form is your confirmation.) A yes/no question looks like this:

```screen
┌zfsctl────────────────┐┌Pools — local ('r' refresh, → for properties)────┐┌Properties — scratch ('r' refresh┐
│Hosts                 ││Name                  Health       Used          ││Property    Value      Source    │
│Pools                 ││scratch               ● ONLINE     ███░░░░░ 42%  ││                                 │
│Datasets              ││tpool                 ● ONLINE     ███░░░░░ 41%  ││                                 │
│Snapshots             ││zroot                 ● ONLINE     ██░░░░░░ 24%  ││                                 │
│Disks                 ││                                                 ││                                 │
│Shares                ││                                                 ││                                 │
│Boot Envs             ││                                                 ││                                 │
│Jobs                  ││                                                 ││                                 │
│Alerts               ┌Confirm (y/n, then Enter)───────────────────────────────────────┐                     │
│                     │Scrub pool 'scratch'? (y/n)                                     │                     │
│                     │                                                                │─────────────────────┘
│                     │Press y for yes or n for no, then Enter.                        │─────────────────────┐
│                     │                                                                │ONLINE               │
│                     └────────────────────────────────────────────────────────────────┘%  102M / 240M       │
│                      ││                                                     Vdevs      1  (1 mirror)       │
│                      ││                                                     Disks      2  (2 data)         │
│                      ││                                                     Datasets   2 filesystems       │
│                      ││                                                     Snapshots  3                   │
│                      ││                                                     Compress   on 1.00x   Trim on  │
│                      ││                                                     Frag    40%   Dedup 1.00x      │
│                      ││                                                     +2 more lines: enlarge the ter…│
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
y Yes  n/Esc No
```

Press `y` to choose yes or `n` (or `Esc`) to choose no, and then `Enter` to apply your choice. `Enter` on its own does nothing, and a held-down key cannot answer twice. The hint bar says `y Yes  n/Esc No` while the question is open.

Some actions go further and ask you to **type a name**: destroying a pool or dataset, destroying or rolling back to a snapshot, destroying a boot environment. You type the exact name and then `Enter`; a wrong name keeps the prompt open and only `Esc` cancels. If you say no to a confirmation that came from a form, you return to the form with what you typed.

> **Safety:** destroying, rolling back, creating a pool, adding a vdev, replacing a disk, revoking permissions, shrinking a volume and enabling a pool feature all need two keys (or a typed name) on a question that says what it will do. Smaller changes do not ask, so read a form before you press `Enter` on its last field. If you are not sure, `Esc`.

## Forms

A form is a small window of fields. `Tab` or `Down` moves to the next field, `Shift+Tab` or `Up` to the previous one, and `Enter` on the last field submits it. `Esc` cancels. On some fields `F2` opens a list of valid choices to pick from (hosts, pools, schedules, users and so on). A form that can destroy something (create a pool, add a vdev, replace a disk) shows you the exact command and asks the two-key question before it runs.

## The shell key

`!` opens a shell and returns to zfsctl when you exit it. On the hub it is your normal shell, running as the user zfsctl runs as (root). On another host it is a normal interactive `ssh` login as that host's unprivileged account, never a root shell, and it needs nothing extra in sudoers. It is handy for a quick `zpool status` or `ls` without leaving the screen.

## Colours and themes

Colour is never the only signal: pool health is shown as `● ONLINE` (green), `▲ DEGRADED` (yellow), `✖ FAULTED`, `UNAVAIL` or `REMOVED` (red) and `○ OFFLINE` (grey), so it still reads on a monochrome terminal. Capacity bars turn yellow at 70% full and red above 85%.

Choose a theme in `config.toml`: `ansi` (the default: it uses only your terminal's own 16 colours, so your terminal theme applies), `mono` (no colour; chosen automatically when `NO_COLOR` is set), `solarized-dark` or `solarized-light` (best on a truecolor terminal). An unknown name is a startup error that lists the valid ones.

## Refreshing

The lists in Pools, Datasets, Snapshots, Disks, Shares and Boot Envs refresh themselves every ten seconds while you look at them (change it with `auto_refresh_secs` in `config.toml`; `0` turns it off). Your selection follows the item you had selected, by name, so a refresh never moves your cursor onto a different row. Press `r` to refresh now.

## Try it

Start zfsctl, press `Tab` to visit each section, press `?` and read the Help, close it, go to Pools, press `Down` to select a pool and `s`. You get the scrub question from the capture above. Press `Enter` alone and nothing happens, because the question is still open: that is the two-key rule. Press `n`, then `Enter`, to cancel.

## Where next

- [Hosts](07-hosts.md): reading the Hosts list.
- [Pools](08-pools.md): the heart of ZFS.
