# The zfsctl Book

A guide to running ZFS with zfsctl: a terminal tool that shows your pools, datasets, snapshots, disks and shares, runs your scheduled snapshots, replication and backups, and tells you when something needs attention.

## How it was made

zfsctl and this book were written by fedor4445 in collaboration with Claude, an AI model by Anthropic (through Claude Code), and directed and tested by the author on real machines.

## Who this is for

You run ZFS on FreeBSD, on one machine or several. The book assumes you can use a terminal and install a package. FreeBSD-specific parts (`rc.conf`, `geom`, `bectl`, `pkg`) are explained where they come up.

## How to read it

Read the first five chapters in order once; after that, jump to the chapter for the part of zfsctl you are using. Every chapter has the same shape: *What you will learn*, the body, *Try it*, and *Where next*.

Boxes mark the things worth noticing:

> **Safety:** said before anything that changes your data.

> **Limit:** something you might expect that zfsctl does not do, so you are never misled.

> **Tip:** a shortcut or a good habit.

Keys are written in backticks, like `n` or `Enter`. Commands you type are in code blocks; the ones that need root say so.

## The running example

The examples follow a small home lab: one machine, the *hub*, runs zfsctl and holds a pool called `tank`; a second machine, `backup`, receives copies. In the screens the machines have other names (`fb`, `fbsd16`, `fbsd16t1`, `fbsd16t2`); chapter 1 says so again where it matters.

## Chapters

- [01 What zfsctl is](01-what-is-zfsctl.md)
- [02 ZFS in 15 minutes](02-zfs-in-15-minutes.md)
- [03 FreeBSD for newcomers](03-freebsd-for-newcomers.md)
- [04 Installing](04-installing.md)
- [05 Remote hosts and sudoers](05-remote-hosts.md)
- [06 The screen](06-the-screen.md)
- [07 Hosts](07-hosts.md)
- [08 Pools](08-pools.md)
- [09 Datasets](09-datasets.md)
- [10 Snapshots](10-snapshots.md)
- [11 Disks](11-disks.md)
- [12 Shares](12-shares.md)
- [13 Boot environments](13-boot-environments.md)
- [14 Scheduled jobs](14-jobs.md)
- [15 Backups](15-backups.md)
- [16 Alerts and the daily report](16-alerts.md)
- [17 Recipes](17-recipes.md)
- [18 Safety](18-safety.md)
- [19 Troubleshooting](19-troubleshooting.md)
- [20 Reference: keys](20-reference-keys.md)
- [21 Reference: configuration](21-reference-config.md)
- [22 Reference: commands](22-reference-commands.md)
- [23 Glossary](23-glossary.md)

For the project itself (building, the full feature list) see the [project README](https://github.com/fedor4445/zfsctl#readme).
