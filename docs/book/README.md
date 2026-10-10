# The zfsctl Book

A friendly guide to running ZFS on FreeBSD with zfsctl: a terminal tool that shows your pools, datasets, snapshots, disks and shares, runs your scheduled snapshots, replication and backups, and tells you when something needs attention.

## Who this is for

You run (or want to run) ZFS on FreeBSD, on one machine or several. You may be a long-time FreeBSD or ZFS user who wants one place to see and manage everything, or you may come from Linux or Windows and be meeting ZFS and FreeBSD for the first time. The book assumes you can use a terminal and install a package. It does not assume you know what a vdev is, what `rc.conf` does, or how FreeBSD names a disk: those are explained where you need them, and short notes point out what the familiar Linux or Windows thing is called here.

## How to read it

Read the first five chapters in order once; after that, jump to the chapter for the part of zfsctl you are using. Every chapter has the same shape: *What you will learn*, the body, *Try it*, and *Where next*.

Boxes mark the things worth noticing:

> **Coming from Linux:** a note on what the Linux tool or habit you know is called here.

> **Coming from Windows:** the same for Windows.

> **Safety:** said before anything that changes your data.

> **Not built yet:** something you might expect that zfsctl does not do (yet), so you are never misled.

> **Tip:** a shortcut or a good habit.

Keys are written in backticks, like `n` or `Enter`. Screens in the book are real captures from a running zfsctl, not drawings. Commands you type are in code blocks; the ones that need root say so.

## The running example

The examples follow a small home lab: one machine, the *hub*, runs zfsctl and holds a pool called `tank`; a second machine, `backup`, receives copies. The screens come from the author's test machines, which have other names (`fb`, `fbsd16`, `fbsd16t1`, `fbsd16t2`); chapter 1 says so again where it matters.

## Chapters

For the project itself (building, the full feature list) see the [project README](../../README.md).
