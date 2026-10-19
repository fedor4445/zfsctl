# What zfsctl is

## What you will learn

- What problem zfsctl solves and what it looks like when it runs.
- How its pieces fit together: the *hub*, the *hosts* and the *daemon*.
- What it can do for you, what it deliberately does not do, and the promises it makes about your data.

## The problem

ZFS is wonderful and its tools are plain: `zpool`, `zfs`, and a handful of other commands that you type, one machine at a time. That is fine for one pool. It gets tiring with three machines, a schedule of snapshots, copies to a second box, backups to somewhere else, and a wish to be told when a disk misbehaves. You end up with a pile of cron jobs and shell scripts, and a mental map of what runs where.

zfsctl is one terminal program that puts that map on a screen. It shows every pool, dataset, snapshot, disk and share on the machines you give it, lets you create and change them with confirmations in the right places, runs your scheduled snapshots, replication and backups, and sends you an email when something needs attention. It changes your data only when you ask, and it asks twice for anything you could not undo.

## The hub and its hosts

```text
   you ──ssh──►  hub  ──ssh (as an ordinary user + sudo)──►  backup
                  │                                          other hosts…
                  ├─ zfsctl (the screen you use)
                  └─ zfsctl daemon (runs your jobs, watches your hosts)
```

- The **hub** is one FreeBSD machine where you run zfsctl, as root. It also manages itself (it appears in the list as `local`).
- A **host** is any other machine you want to manage. The hub reaches it over ssh as an ordinary, unprivileged user, and that user may run exactly the ZFS commands zfsctl needs through `sudo`, nothing more. zfsctl never logs in as root over ssh, and chapter 5 shows how to set a host up.
- The **daemon** is a second copy of the program (`zfsctl daemon`) that the hub starts at boot. It has no screen: it runs your scheduled jobs and watches the hosts while you are away. The screen and the daemon share a few small files in `/var/db/zfsctl`.

You can start with one machine (just the hub) and add hosts later.

## What you see

When you start zfsctl you land on the Hosts section. This is a real capture from the author's test machines (the hub is called `fb` and the other hosts are small virtual machines; the book's examples use the names `hub`, `backup` and `tank` in the text, but every screen is a genuine one):

```screen
┌zfsctl────────────────┐┌Hosts───────────────────────────────────────────────────────────────────────────────┐
│Hosts                 ││Name       Address            State                               Type   Used  Pools│
│Pools                 ││● local    local              ok                                  vm     42%   3    │
│Datasets              ││● fbsd16   172.16.10.60       ok                                  vm     55%   1    │
│Snapshots             ││● fbsd16t1 172.16.10.61       ok                                  vm     72%   2    │
│Disks                 ││● fbsd16t2 172.16.10.62       ok                                  vm     72%   2    │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Alerts                ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — host local (all pools)────────────────────────────────────────────────────┐
│                      ││I/O     read 0B/s  write 0B/s              ZFS     2.4.2-1-FreeBSD_g6330a45b0       │
│                      ││OS      FreeBSD 15.1-RELEASE-p4            Pools   3 (all ONLINE)                   │
│                      ││Up      3d 11h   rtt 2ms                   Space   30%  18.7G / 62.2G               │
│                      ││Load    0.60 0.40 0.38                     ARC     4.4G / 18.9G  hit 99.3%          │
│                      ││CPU     36 cores  0%                       Status  all pools are healthy            │
│                      ││        Intel(R) Xeon(R) CPU E5-2660 0 @…                                           │
│                      ││Memory  19.9G  free 9.5G  wired 6.7G                                                │
│                      ││Swap    0B / 2.0G                                                                   │
│                      ││scratch       42% frag 40% ONLINE   scrub Oct  8 19:48:07 2026                      │
│                      ││tpool         41% frag  2% ONLINE   scrub Sep 26 13:15:14 2026                      │
│                      ││zroot         24% frag 34% ONLINE   scrub Sep  5 20:18:51 2026                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Host  n Add host  / Filter  Tab Section  ! Shell  A About  ? Help  q Quit
```

On the left is the **sidebar** with the sections: Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs and Alerts. The big area at the top right is the **list** for the section you are in. The area below it is the **panel**: live details and graphs for whatever is selected. The line at the very bottom is the **hint bar**: it always lists the keys that work right now. Chapter 6 walks through all of this.

## What it does

- **Shows** pools, vdevs and disks, datasets and their properties, snapshots, shares (NFS and SMB), boot environments, jobs and their history, on every host.
- **Changes** things with forms and confirmations: create and destroy pools, datasets and snapshots, add and replace disks, set properties, share datasets, manage boot environments.
- **Schedules** scrubs, snapshots (with retention), replication from one host to another or to a file, and backups with restic (to a disk, another machine or cloud storage).
- **Watches** every host and emails you when a pool degrades, fills up, or a host stops answering, and sends a daily report.

## What it does not do

- It does not manage virtual machines (bhyve) or jails yet.

> **Not built yet:** bhyve virtual machines and jails. They are planned; nothing in this book relies on them.

- It does not log in as root over ssh, ever, and it does not store your passwords. A restic repository password stays in a root-only file on the host; a Samba password is passed straight to Samba and not kept.
- It is not a replacement for knowing what ZFS is. Chapter 2 is a short introduction, and the glossary at the end explains every term.

## The safety promise

- **Two keys for anything that cannot be undone.** A yes/no question needs `y` (or `n`) to choose and then `Enter` to apply, so a stray key or a held-down key can never do it. The most dangerous actions also need you to type the name of the thing you are destroying.
- **A form that you decline comes back.** If you say no at the final question, you return to the form with what you typed, not to an empty screen.
- **It refuses what is too broad.** For example a recursive destroy refuses a pool's root dataset (that would wipe the whole pool), and its jobs only ever delete the snapshots they made themselves.
- **No surprises on the other machines.** Every command zfsctl runs on a host is on a short list that chapter 5 prints, and the sudoers entry on each host allows exactly that list.

Chapter 18 gathers these promises, and the honest limits of them, in one place.

## A note about the screens

Every screen and command output in this book was captured from a running zfsctl on test machines, not drawn. Where a screen would show something private (an email address), it is replaced with `you@example.com`.

## Built on

zfsctl is a thin layer over other people's work. None of it is bundled; all of it deserves credit and a visit:

- [OpenZFS](https://openzfs.org/): the file system, volume manager and the `zpool`/`zfs` tools that zfsctl drives.
- [FreeBSD](https://www.freebsd.org/): the operating system of the hub, and of most of the test machines.
- [illumos](https://illumos.org/) and [OmniOS](https://omnios.org/): the descendant of OpenSolaris, where ZFS was born at Sun; OmniOS is the distribution used for testing.
- [Linux](https://www.kernel.org/) (tested on [Ubuntu](https://ubuntu.com/)) with [OpenZFS on Linux](https://openzfs.github.io/openzfs-docs/).
- [Rust](https://www.rust-lang.org/): the language, with [ratatui](https://ratatui.rs/) for the screen and [crossterm](https://github.com/crossterm-rs/crossterm) for the terminal.
- [restic](https://restic.net/) and [rclone](https://rclone.org/): backups and cloud mirrors, when you use those job kinds.

## Try it

If zfsctl is already installed on your hub, run it as root (`zfsctl`), look at the Hosts list, press `?` to open the Help screen, press any key to close it, and press `q` to quit. If it is not installed yet, chapter 4 gets you there; chapters 2 and 3 are good reading first.

## Where next

- [ZFS in 15 minutes](02-zfs-in-15-minutes.md) if ZFS is new to you.
- [FreeBSD for newcomers](03-freebsd-for-newcomers.md) if FreeBSD is new to you.
- [Installing](04-installing.md) if you know both and want to start.
