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

- **The sidebar** (left) lists the sections: Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs and Alerts. The highlighted one is where you are.
- **The list** (top right) shows the items of that section: hosts, pools, datasets and so on. In Pools and Datasets a second pane on the right shows the selected item's properties.
- **The panel** (bottom right) shows live details of whatever is selected: space, health, counts, and two graphs of read and write activity. It disappears on a very short terminal so that the list keeps room.
- **The hint bar** is the last line of the screen. It lists exactly the keys that work right now, and it changes as you move: a key that would do nothing is not shown. When there is a message for you (a result, an error), it appears on the line just above.

> **Coming from Windows:** the sidebar is like the tree on the left of Disk Management or an MMC console; the list is the main window; the panel is the status area.

> **Coming from Linux:** the movement keys are the ones you know from tools like `htop` or `k9s`, minus the vi bindings: arrows and `Tab`.

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
