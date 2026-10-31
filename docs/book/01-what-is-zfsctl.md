# What zfsctl is

## What you will learn

- What problem zfsctl solves and what it looks like when it runs.
- How its pieces fit together: the *hub*, the *hosts* and the *daemon*.
- What it can do for you, what it deliberately does not do, and the promises it makes about your data.

## Before you begin

zfsctl is provided as is, without warranty (see the LICENSE file): use it at your own risk. It runs commands as root that can destroy data, such as destroying a dataset, rolling back a snapshot, changing a pool or activating a boot environment. It asks for confirmation before anything you could not undo, but that is not a backup. Keep backups of anything you care about, and try zfsctl first on a scratch pool or a test machine. The [Safety](18-safety.md) chapter lists what it does to protect you and the habits that matter most.

## What zfsctl is not

zfsctl is not a storage appliance, a ready-made "NAS in a box". It has no operating system of its own, no web page and no installer that takes over a machine. It is an ordinary program that you install on a plain FreeBSD you already run, in the same way as any other tool, and remove the same way. Your machines stay what they were: normal servers with normal ZFS, which you can still manage by hand with `zpool` and `zfs` at any moment. zfsctl adds a screen and a scheduler on top and leaves nothing behind that you cannot see.

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

## How it fits together

The picture below shows the program itself, in layers (the one above showed machines). Each layer talks only to the one below it, and only the bottom layers touch your data.

```text
 ┌───────────────────────────────────────────────────────────────────────┐
 │  YOU                                                                  │
 │  keyboard, terminal (ssh to the hub)       email, hook script (alerts)│
 └──────────────┬─────────────────────────────────────────▲──────────────┘
                │ keys                                    │ messages
 ┌──────────────▼───────────────┐         ┌───────────────┴──────────────┐
 │  SCREEN         zfsctl       │         │  DAEMON      zfsctl daemon   │
 │  sections, lists, panels,    │         │  scheduler: scrubs,          │
 │  forms, confirmations        │         │  snapshots, replication,     │
 │  (asks twice before          │         │  backups, cloud mirrors      │
 │  anything irreversible)      │         │  watcher: health, space,     │
 │                              │         │  hosts that stop answering   │
 └──────────────┬───────────────┘         └───────────────┬──────────────┘
                │          ┌──────────────────────┐       │
                ├─────────►│ STATE   /var/db/     │◄──────┤  jobs, history,
                │          │         zfsctl       │       │  events, config
                │          └──────────────────────┘       │
 ┌──────────────▼─────────────────────────────────────────▼──────────────┐
 │  SAFETY GATE                                                          │
 │  one short list of allowed commands; refuses what is too broad        │
 │  (a pool's root dataset, snapshots a job did not make)                │
 └──────────────────────────────────┬────────────────────────────────────┘
                                    │ one command at a time
 ┌──────────────────────────────────▼────────────────────────────────────┐
 │  TRANSPORT                                                            │
 │  this machine: run the command directly (hub is root)                 │
 │  other hosts:  ssh as an ordinary user, then sudo for that one command│
 └───────────┬─────────────────────────────────────────────┬─────────────┘
             │                                             │
 ┌───────────▼─────────────────────┐        ┌──────────────▼─────────────┐
 │  EACH HOST                      │        │  EACH HOST                 │
 │  zpool  zfs  bectl  smartctl    │  ...   │  the same, on another      │
 │  Samba  NFS  restic  rclone     │        │  operating system          │
 │  (FreeBSD)                      │        │                            │
 └───────────┬─────────────────────┘        └──────────────┬─────────────┘
             │                                             │
 ┌───────────▼─────────────────────────────────────────────▼─────────────┐
 │  YOUR DATA   pools, datasets, snapshots, disks                        │
 │  and, for backups, other disks, other machines or cloud storage       │
 └───────────────────────────────────────────────────────────────────────┘
```

Read it from the top:

- **Screen and daemon** are two copies of one program. They never talk to each other directly; they leave notes in the **state** directory (jobs, history, events), so the daemon keeps working when nobody is logged in and the screen can show what happened while you were away.
- **The safety gate** is where the promises of this chapter live. Both the screen and the daemon go through it, so a job cannot do what a form would refuse.
- **Transport** is the only part that knows whether a host is the hub itself or somewhere on the network. Everything above it sees the same thing either way.
- **The tools** at the bottom are the ones you already know. zfsctl does not reimplement ZFS: it runs `zpool` and `zfs` (and, for the extras, `bectl`, `smartctl`, restic and rclone) and reads what they print.

The same layers apply whether you manage one machine or twenty; only the number of boxes at the bottom changes.

## What you see

When you start zfsctl you land on the Hosts section. In the screens the hub is called `fb` and the other hosts are small virtual machines; the text uses the names `hub`, `backup` and `tank`:

```screen
┌zfsctl────────────────┐┌Hosts───────────────────────────────────────────────────────────────────────────────┐
│Dashboard             ││Name       Address            State                     Type   Tag     Site    Used │
│Hosts                 ││● local    local              ok                        vm                     45%  │
│Pools                 ││● fbsd16   172.16.10.60       ok                        vm     freebsd lab-box 62%  │
│Datasets              ││● fbsd16t1 172.16.10.61       ok                        vm     freebsd lab-box 53%  │
│Snapshots             ││● fbsd16t2 172.16.10.62       ok                        vm     freebsd lab-box 79%  │
│Disks                 ││                                                                                    │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Cloud                 ││                                                                                    │
│Alerts                ││                                                                                    │
│Settings              ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — host local (all pools)────────────────────────────────────────────────────┐
│                      ││I/O     read 0B/s  write 0B/s              ZFS     2.4.2-1-FreeBSD_g6330a45b0       │
│                      ││OS      FreeBSD 15.1-RELEASE-p4            Pools   3 (all ONLINE)                   │
│                      ││Up      14d 1h   rtt 3ms                   Space   31%  19.3G / 62.2G               │
│                      ││Load    2.25 0.74 0.32                     ARC     5.2G / 18.9G  hit 100.0%         │
│                      ││CPU     36 cores  0%                       Status  all pools are healthy            │
│                      ││        Intel(R) Xeon(R) CPU E5-2660 0 @…                                           │
│                      ││Memory  19.9G  free 8.3G  wired 7.9G                                                │
│                      ││Swap    0B / 2.0G                                                                   │
│                      ││scratch       45% frag 50% ONLINE   scrub Oct  8 19:48:07 2026                      │
│                      ││tpool         41% frag  2% ONLINE   scrub Oct 25 03:00:03 2026                      │
│                      ││zroot         26% frag 41% ONLINE   scrub Oct 25 03:00:56 2026                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Host  c Check disks and shares  Space Mark  e Edit tag and site  n Add host  / Filter  ? Help  q Quit
```

On the left is the **sidebar** with the sections: Hosts, Pools, Datasets, Snapshots, Disks, Shares, Boot Envs, Jobs and Alerts. The big area at the top right is the **list** for the section you are in. The area below it is the **panel**: live details and graphs for whatever is selected. The line at the very bottom is the **hint bar**: it always lists the keys that work right now. Chapter 6 walks through all of this.

## What it does

- **Shows** pools, vdevs and disks, datasets and their properties, snapshots, shares (NFS and SMB), boot environments, jobs and their history, on every host.
- **Changes** things with forms and confirmations: create and destroy pools, datasets and snapshots, add and replace disks, set properties, share datasets, manage boot environments.
- **Schedules** scrubs, snapshots (with retention), replication from one host to another or to a file, and backups with restic (to a disk, another machine or cloud storage).
- **Watches** every host and emails you when a pool degrades, fills up, or a host stops answering, and sends a daily report.

## What it does not do

- It does not log in as root over ssh, ever, and it does not store your passwords. A restic repository password stays in a root-only file on the host; a Samba password is passed straight to Samba and not kept.
- It is not a replacement for knowing what ZFS is. Chapter 2 is a short introduction, and the glossary at the end explains every term.

## The safety promise

- **Two keys for anything that cannot be undone.** A yes/no question needs `y` (or `n`) to choose and then `Enter` to apply, so a stray key or a held-down key can never do it. The most dangerous actions also need you to type the name of the thing you are destroying.
- **A form that you decline comes back.** If you say no at the final question, you return to the form with what you typed, not to an empty screen.
- **It refuses what is too broad.** For example a recursive destroy refuses a pool's root dataset (that would wipe the whole pool), and its jobs only ever delete the snapshots they made themselves.
- **No surprises on the other machines.** Every command zfsctl runs on a host is on a short list that chapter 5 prints, and the sudoers entry on each host allows exactly that list.

Chapter 18 gathers these promises, and the honest limits of them, in one place.

## A note about the screens

Where a screen would show something private (an email address), it is replaced with `you@example.com`.

## Built on

zfsctl is a thin layer over other people's work. None of it is bundled; all of it deserves credit and a visit:

- [OpenZFS](https://openzfs.org/): the file system, volume manager and the `zpool`/`zfs` tools that zfsctl drives.
- [FreeBSD](https://www.freebsd.org/): the operating system of the hub, and of most of the test machines.
- [Rust](https://www.rust-lang.org/): the language, with [ratatui](https://ratatui.rs/) for the screen and [crossterm](https://github.com/crossterm-rs/crossterm) for the terminal.
- [restic](https://restic.net/) and [rclone](https://rclone.org/): backups and cloud mirrors, when you use those job kinds.

## Try it

If zfsctl is already installed on your hub, run it as root (`zfsctl`), look at the Hosts list, press `?` to open the Help screen, press any key to close it, and press `q` to quit. If it is not installed yet, chapter 4 gets you there; chapters 2 and 3 are good reading first.

## Where next

- [ZFS in 15 minutes](02-zfs-in-15-minutes.md) if ZFS is new to you.
- [FreeBSD for newcomers](03-freebsd-for-newcomers.md) if FreeBSD is new to you.
- [Installing](04-installing.md) if you know both and want to start.
