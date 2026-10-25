# Hosts

## What you will learn

- What each column of the Hosts table means and what its colours say.
- What the bottom panel tells you about the selected host.
- How to add a host, tag it, and filter a long list.

## The list

The Hosts section is where zfsctl opens. Every host in your configuration is probed in the background about every 30 seconds (an unreachable host costs a single 10-second timeout and never freezes the screen; the screen and the daemon ask at most 16 hosts at a time, so a fleet of hundreds does not open hundreds of ssh connections at once, and the hosts queue for their turn), so the table is a live health overview:

```screen
┌zfsctl────────────────┐┌Hosts───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│Dashboard             ││Name       Address            State                                    Type   Tag     Used  Pools Scrub         Jobs  Rtt   │
│Hosts                 ││● local    local              ok                                       vm             45%   3     Oct 8 2026    0/4   19ms  │
│Pools                 ││● fbsd16   172.16.10.60       ok                                       vm             62%   1     Oct 17 2026   0/3   72ms  │
│Datasets              ││● fbsd16t1 172.16.10.61       ok                                       vm             53%   2     Oct 17 2026   -     68ms  │
│Snapshots             ││● fbsd16t2 172.16.10.62       ok                                       vm             79%   2     Oct 17 2026   -     59ms  │
│Disks                 ││● ub24     172.16.10.52       ok                                       vm     linux   0%    1     Oct 17 2026   -     73ms  │
│Shares                ││● omnios   172.16.10.80       ok                                       vm     illumos 52%   3     Oct 17 2026   0/2   128ms │
│Boot Envs             ││                                                                                                                            │
│Jobs                  ││                                                                                                                            │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Host  c Check disks and shares  Space Mark  e Edit tag and site  n Add host  / Filter  Tab Section  ! Shell  A About  ? Help  q Quit
```

(This capture is 150 columns wide so that every column fits; on a narrower terminal the columns on the right drop away, as described below.)

| Column | Meaning |
|---|---|
| `●` and **Name** | the host's label. The dot and the name take the colour of the host's health |
| **Address** | the host part of its `ssh_target` as you wrote it (no lookup); `local` for the hub |
| **State** | the first reason at the worst level, or `ok` (see below) |
| **OS** | the short OS name, read once with the first probe: `FreeBSD 16.0`, `Linux 6.8 (Ubuntu 24)`, `illumos omnios-r151058`; `-` when it cannot be read. It is the first column to go on a narrow terminal |
| **Type** | `metal`, `vm` or `jail`, read once from the host: on FreeBSD with `sysctl kern.vm_guest` and `security.jail.jailed`, on Linux with `systemd-detect-virt` (or the DMI vendor when there is no systemd), on illumos with `zonename` and `smbios`; a container or a non-global zone shows as `jail`; `-` when it cannot be read |
| **Tag** | your own label from `config.toml` or the Add Host form; shown only if some host has one |
| **Site** | what the host shares a failure with, from `config.toml` or `e`; shown only if some host has one, after Tag, and the first of the two to drop on a narrow terminal |
| **Used** | the fullest pool on the host (yellow from 80%, red from 90%) |
| **Pools** | how many pools |
| **Scrub** | what is happening or the oldest last scrub: `running 34%`, `paused`, `never`, or the date |
| **Jobs** | failed over total enabled jobs of that host (`1/4`; red when any failed); `-` if it has none |
| **Rtt** | the round trip of the last probe |

Name and State always show, and Address on all but a very narrow terminal. As the terminal gets narrower, columns drop from the right in this order: OS, Rtt, Jobs, Scrub, Pools, Tag, Used, Type. At 110 columns you see the first screen of this chapter's other captures.

## What State says

The colour is the worst level that applies; the text is the first reason at that level.

| Colour | State text | What it means |
|---|---|---|
| green (normal) | `ok` | all pools online, below 80% full, no errors, no failed jobs |
| dim | `probing` | the first probe has not finished |
| yellow | `<pool> <n>% full` | a pool is 80% full or more |
| yellow | `<pool> <n> errors` | read, write or checksum errors on the pool or on a device inside it, or permanent data errors |
| yellow | `scrub found errors on <pool>` | the last scrub or resilver finished with errors |
| yellow | `job failed` | an enabled scheduled job of this host failed on its latest run |
| yellow | `cannot inspect: …` | the host answers but zfsctl cannot read it (sudo refused, no `zpool`) |
| yellow | `no answer for 45s` (the number keeps growing) | a probe has been running far longer than a healthy one |
| red | `<pool> DEGRADED` (or `FAULTED`, `UNAVAIL`…) | a pool is not ONLINE |
| red | `<pool> <n>% full` | a pool is 90% full or more |
| red | `unreachable` | ssh could not connect |
| red | `ssh login refused` | the host answered but refused your key |
| red | `host key not accepted` | the host's key is new or has changed |

> **Safety:** a pool above about 80% full is a warning for a reason: ZFS slows down and fragments badly as it fills. Act on yellow before it turns red.

## The filter

The tag is also how the Dashboard groups a long fleet: hosts with the same tag roll up into one row there (chapter 6), so tag by what you would want to look at together (`web`, `db`, `lab`).

With a long list, press `/` and type part of a name, address, tag, site or type (`t1`, `172.16.10`, `backup`, `vm`): the list narrows as you type, and the title shows how many match.

```screen
┌zfsctl────────────────┐┌Hosts (filter: t1_ — 1/6)───────────────────────────────────────────────────────────┐
│Dashboard             ││Name       Address            State                       Type   Tag     Used  Pools│
│Hosts                 ││● fbsd16t1 172.16.10.61       ok                          vm             53%   2    │
│Pools                 ││                                                                                    │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares                ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Keep filter  Esc Clear
```

`Enter` keeps the filter and returns you to the list; `Esc` clears it. While you type, every key is text, so `q` does not quit. Arrow keys skip hidden hosts, and the selected host is always one you can see, so another section never acts on a host you cannot see. If nothing matches, `Enter` will not keep the filter.

## The panel

Select a host with `Up` and `Down` and the panel shows it. The first sample of a remote host takes a few seconds, so you may see `sampling…` first.

```screen
┌zfsctl────────────────┐┌Hosts───────────────────────────────────────────────────────────────────────────────┐
│Dashboard             ││Name       Address            State                       Type   Tag     Used  Pools│
│Hosts                 ││● local    local              ok                          vm             45%   3    │
│Pools                 ││● fbsd16   172.16.10.60       ok                          vm             62%   1    │
│Datasets              ││● fbsd16t1 172.16.10.61       ok                          vm             53%   2    │
│Snapshots             ││● fbsd16t2 172.16.10.62       ok                          vm             79%   2    │
│Disks                 ││● ub24     172.16.10.52       ok                          vm     linux   0%    1    │
│Shares                ││● omnios   172.16.10.80       ok                          vm     illumos 52%   3    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Cloud                 ││                                                                                    │
│Alerts                ││                                                                                    │
│Settings              ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      │└────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics — host local (all pools)────────────────────────────────────────────────────┐
│                      ││I/O     read 0B/s  write 0B/s              ZFS     2.4.2-1-FreeBSD_g6330a45b0       │
│                      ││OS      FreeBSD 15.1-RELEASE-p4            Pools   3 (all ONLINE)                   │
│                      ││Up      13d 12h   rtt 2ms                  Space   31%  19.2G / 62.2G               │
│                      ││Load    0.06 0.34 0.28                     ARC     5.1G / 18.9G  hit 100.0%         │
│                      ││CPU     36 cores  0%                       Status  all pools are healthy            │
│                      ││        Intel(R) Xeon(R) CPU E5-2660 0 @…                                           │
│                      ││Memory  19.9G  free 8.4G  wired 7.8G                                                │
│                      ││Swap    0B / 2.0G                                                                   │
│                      ││scratch       45% frag 48% ONLINE   scrub Oct  8 19:48:07 2026                      │
│                      ││tpool         41% frag  2% ONLINE   scrub Oct 18 03:00:30 2026                      │
│                      ││zroot         26% frag 41% ONLINE   scrub Oct 18 03:00:22 2026                      │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Host  c Check disks and shares  Space Mark  e Edit tag and site  n Add host  / Filter  ? Help  q Quit
```

On the left is the system: operating system, uptime and round trip, load averages, CPU (cores, current use and the processor model), memory (physical, free, wired) and swap. On the right is ZFS: version, how many pools and whether all are healthy, total space used, the ARC (ZFS's memory cache: its size, its ceiling and its hit rate), and a status line from `zpool status -x`. That line takes the color of the host's State in the table above (yellow for a warning, red for a critical state or an unreachable host), and when zfsctl found the problem rather than ZFS (a pool 86% full, a failed job) the reason is written under it: ZFS can say "all pools are healthy" about a pool that is nearly full. Below are one row per pool with its fullness, fragmentation, health and last scrub.

When the panel is wide enough (about 90 columns) it also shows two graphs, **Read** and **Write**, with the peak of what is drawn: they plot the whole host's disk activity over the most recent samples (roughly one every second or two). On a narrower terminal, as in the capture above, the panel keeps the figures and shows the rates as one line.

## Checking one host now

`c` in Hosts checks the selected host's disks (`smartctl`) and its file sharing services (NFS, Samba) right now, in the background, and says what it found (`fbsd16: disks fine, shares fine`). Use it after you replace a disk or restart a service, instead of waiting for the daily check (03:30). The answer is written where the daily check writes it, so the Dashboard shows it too, and a problem that is gone clears. If the host cannot be reached the old answer stays, with a note. Only reads: no self-test is started and nothing is changed on the host. While another check is running (the daemon's pass, or one of yours) `c` waits and says who is running it.

## Adding a host

Press `n`, fill in the **Name** (any label), the **SSH target** (`user@address`, the unprivileged account) and, if you like, a **Tag**, and press `Enter` on the last field. zfsctl first checks that it can reach the host (it runs a harmless `true` over ssh) and, if that works, adds it at once and saves it to `config.toml`: a new `[[hosts]]` block is added next to the others and nothing else in the file changes. If the check fails you see ssh's own message and can try again. Setting the host up on its side is [chapter 5](05-remote-hosts.md).

## Editing tags and sites

`e` in Hosts edits the **tag** and the **site** of the selected host; it asks for nothing else and changes nothing else in `config.toml` (only those two lines of that host's `[[hosts]]` block, with a backup copy first, `config.toml.bak`). The **site** names what hosts share a failure with: one physical machine, one rack, one office. Hosts with the same site count as one place when zfsctl counts how many separate failures your data survives (chapter 6, "Data safety"; the `domains` setting in chapter 21).

To give several hosts the same tag or site, press `Space` on each (a marked host shows `+` instead of `●`) and then `e`. For one host the form shows its current values and an empty field clears the key. For several hosts the fields start empty: an empty field leaves that key alone and a `-` clears it. Before anything is written you see what will be saved and confirm it with `y` and `Enter`. The local host, the hub, has no `[[hosts]]` block, so it cannot be edited here. The number of separate failure domains the copies must span is a row in Settings, "Failure domains".

## Try it

Add the second machine of your lab with `n`, give it the tag `backup`, and check that it appears with `ok`. Then press `/`, type `backup` and see the list narrow to it; press `Esc` to clear.

## Where next

- [Pools](08-pools.md): what the pool rows are made of.
- [Alerts](16-alerts.md): getting told when a State turns yellow or red.
