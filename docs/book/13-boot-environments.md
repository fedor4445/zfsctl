# Boot environments

## What you will learn

- What a boot environment is and why it makes upgrades safe.
- How to create, activate, mount and remove one.
- How to try a boot environment once without committing to it.

## What a boot environment is

A *boot environment* is a bootable copy of the operating system, kept as a ZFS clone. Before you upgrade FreeBSD or change something risky, you make one; if the result is bad, you boot the old one and the system files are back as they were. It costs almost no space until the two diverge. What it brings back is limited, though; read *What a boot environment covers* below.

## The list

```screen
┌zfsctl────────────────┐┌Boot Environments — local ('r' to refresh)──────────────────────────────────────────┐
│Dashboard             ││Name                                  Active       Space    Mountpoint              │
│Hosts                 ││default                               Now+Reboot   5.32G    /                       │
│Pools                 ││                                                                                    │
│Datasets              ││                                                                                    │
│Snapshots             ││                                                                                    │
│Disks                 ││                                                                                    │
│Shares                ││                                                                                    │
│Boot Envs             ││                                                                                    │
│Jobs                  ││                                                                                    │
│Cloud                 ││                                                                                    │
│Alerts                ││                                                                                    │
│Settings              ││                                                                                    │
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
↑↓ Move  n New  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

| Column | Meaning |
|---|---|
| **Name** | the boot environment |
| **Active** | `Now` is what you are running, `Reboot` is what the next boot uses, `Now+Reboot` both, and `Temp` is used for the next boot only |
| **Space** | the space it holds |
| **Mountpoint** | where it is mounted right now, if it is (dropped first on a narrow terminal, then Created) |
| **Created** | when it was made (dropped on a narrow terminal) |

If the host has no `bectl`, or is not laid out for boot environments (its root is not on ZFS), the section title says `[unavailable: ...]` with the reason, and recovers by itself once the problem is fixed.

## What you can do

The hint bar shows only the keys that make sense for the selected row.

| Key | What it does |
|---|---|
| `n` | create a new one: a name, and optionally which to clone (`F2` picks one; blank clones the running one) |
| `a` | make it the boot environment for **every** future boot (asks first) |
| `t` / `T` | use it for the **next boot only** (asks first) / clear that (at once) |
| `R` | rename |
| `m` / `u` | mount / unmount; the mount path shows in the Mountpoint column |
| `d` | destroy (you type its name) |
| `D` | destroy and also remove the snapshot it was cloned from |

The running one and the one chosen for the next boot cannot be destroyed. While a change is being made, other change keys wait and the status line says `Still working on '<name>'`, so a double key press cannot queue the same job twice.

> **Safety:** zfsctl **never reboots** your host. After `a` or `t` you reboot when you choose to. `t` is the safest way to try an upgrade: if the new system does not come up, a plain reboot returns to the old one.

## What a boot environment covers

A boot environment is the root dataset (`zroot/ROOT/NAME`) and, on a layout where parts of the system are datasets below it, those too. Datasets that are mounted separately are **not** part of it: the [bectl(8)](https://man.freebsd.org/cgi/man.cgi?query=bectl&sektion=8&manpath=FreeBSD+15.0-RELEASE) page itself says of a separately mounted `/usr/src` that its files "are not in the boot environment". On a standard FreeBSD installation `/home`, `/tmp`, `/usr/ports`, `/usr/src` and parts of `/var` (`/var/log`, `/var/mail`, ...) are separate datasets (`zfs list` shows them), so every boot environment shares them and booting an old one does not roll them back, neither your files nor the logs. The rest of the system, including `/usr/local` and the package database, is in the root dataset and returns to its old state.

A boot environment is also not a copy of the pool. Enabling pool features (`zpool upgrade`, chapter 8) changes the pool for every boot environment at once, and once a new feature is in use an older system, with an older kernel or boot loader, may not be able to import or boot the pool. So do the pool upgrade last, only after you have decided to keep the new system, and not as part of the upgrade you might want to undo.

## An upgrade, safely

1. Press `n`, name it for what you are about to do (`before-15.1-p5`), and `Enter`.
2. Upgrade the system as you normally would (`freebsd-update`, `pkg upgrade`; a system installed with the optional packaged base system, *pkgbase*, updates its base with `pkg` instead, see the [FreeBSD 15.0 release notes](https://www.freebsd.org/releases/15.0R/relnotes/)).
3. Reboot. If it works, nothing more to do. If not, choose the earlier boot environment at the loader menu (or press `a` on it from a rescue shell) and reboot.

Destroying a mounted boot environment fails with bectl's own message (`cannot destroy mounted boot env unless forced`); unmount it with `u` first.

## Try it

Create a boot environment called `practice` with `n`, select it, mount it with `m` and look at the Mountpoint column, unmount with `u`, and destroy it with `d`, typing the name. Nothing about your running system changes.

## Where next

- [Scheduled jobs](14-jobs.md)
- [Recipes](17-recipes.md): "undo a bad upgrade".
