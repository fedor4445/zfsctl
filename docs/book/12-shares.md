# Shares

## What you will learn

- How to share a dataset with other machines over NFS and SMB.
- Why zfsctl manages SMB itself on FreeBSD.
- How to edit and remove shares.

## The list

The Shares section lists what the host is sharing, from both protocols:

```screen
┌zfsctl────────────────┐┌Shares — local ('r' refresh, Enter edit, 'd' remove)────────────────────────────────────────────────────┐
│Dashboard             ││Dataset                  Protocol Details                                                               │
│Hosts                 ││scratch/data             NFS      -network 172.16.10.0/24                                               │
│Pools                 ││tpool                    NFS      -network 172.16.10.0/24;-network 172.16.20.0/24                       │
│Datasets              ││zroot/home/carol         NFS      -network 172.16.10.0/24;-network 172.16.20.0/24                       │
│Snapshots             ││scratch/data             SMB      \\data  guest  rw                                                     │
│Disks                 ││                                                                                                        │
│Shares                ││                                                                                                        │
│Boot Envs             ││                                                                                                        │
│Jobs                  ││                                                                                                        │
│Cloud                 ││                                                                                                        │
│Alerts                ││                                                                                                        │
│Settings              ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      │└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
│                      │┌Metrics─────────────────────────────────────────────────────────────────────────────────────────────────┐
│                      ││nothing selected                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
│                      ││                                                                                                        │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row is a dataset, the protocol (`NFS` or `SMB`) and the details: for NFS the export options (here, the networks allowed), for SMB the share name and who may use it. `Enter` edits the selected share, `d` removes it (after a yes/no question), `r` refreshes.

## NFS shares

In Datasets, select a dataset and press `N`. (Visit the Shares section once first, so zfsctl has the current state of the host's shares; it tells you if you have not.)

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─┐┌Properties — scratch/data ('r' re┐
│Dashboard             ││Name                     Used    Avail    Kind   ││Property    Value      Source    │
│Hosts                 ││scratch                  103M    17.3M    Filesys││                                 │
│Pools                 ││scratch/data             100M    17.3M    Filesys││                                 │
│Datasets              ││scratch/vault            408K    17.3M    Filesys││                                 │
│Snapshots             ││scratch/vault/docs       200K    17.3M    Filesys││                                 │
│Disks                 ││tpool                    3.9G    5.2G     Filesys││                                 │
│Shares                ││zroot                    10.7G   30.5G    Filesys││                                 │
│Boot Envs             ││zroot/ROOT               5.3G    30.5G    Filesys││                                 │
│Jobs                  ││zroot/ROOT/default       5.3G    30.5G    Filesys││                                 │
│Cloud                 ││zroot/home               595M    30.5G    Filesys││                                 │
│Alerts               ┌NFS share: 'scratch/data'───────────────────────────────────────┐                     │
│Settings             │> Networks: 172.16.10.0/24                                      │                     │
│                     │  Hosts:                                                        │─────────────────────┘
│                     │  Read-only (y/n): n                                            │─────────────────────┐
│                     │  Root user mapping:                                            │%  100M of 118M      │
│                     └────────────────────────────────────────────────────────────────┘0M   Logical 100M    │
│                      ││                                                     Data    100M   Snaps 256K      │
│                      ││                                                     Kids    0B   Refres 0B         │
│                      ││                                                     Compr   on 1.00x   Rec 128K    │
│                      ││                                                     Quota   -   Ref -              │
│                      ││                                                     Pool    ONLINE  errs 0         │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  Esc Cancel
```

- **Networks**: the networks allowed to connect, such as `172.16.10.0/24`.
- **Hosts**: individual hosts, if you prefer them to networks.
- **Read-only**: `y` to let clients read but not write.
- **Root user mapping**: which local user the clients' `root` becomes (blank: nobody special).

zfsctl writes the dataset's `sharenfs` property in the syntax FreeBSD's `mountd` wants and reloads it. The NFS server must be running on the host (`service nfsd start`); if it is not, zfsctl tells you instead of creating a share nobody can reach.

> **Safety:** an NFS share with no network or host restriction is open to everyone who can reach the host. Always give a network.

## SMB shares (Samba)

FreeBSD's ZFS has no working `sharesmb` property (`zfs set sharesmb=on` just prints "No SMB support in FreeBSD yet."). So zfsctl manages Samba for you. In Datasets, select a *mounted* dataset and press `M`:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─┐┌Properties — scratch/data ('r' re┐
│Dashboard             ││Name                     Used    Avail    Kind   ││Property    Value      Source    │
│Hosts                 ││scratch                  103M    17.3M    Filesys││                                 │
│Pools                 ││scratch/data             100M    17.3M    Filesys││                                 │
│Datasets              ││scratch/vault            408K    17.3M    Filesys││                                 │
│Snapshots             ││scratch/vault/docs       200K    17.3M    Filesys││                                 │
│Disks                 ││tpool                    3.9G    5.2G     Filesys││                                 │
│Shares                ││zroot                    10.7G   30.5G    Filesys││                                 │
│Boot Envs            ┌SMB share: 'scratch/data'───────────────────────────────────────┐                     │
│Jobs                 │Shares /scratch/data as \\<host>\<name>. The Valid user must    │                     │
│Settings             │for every share they use. Blank user + guest n: any Samba user  │                     │
│                     │blank user).                                                    │                     │
│                     │                                                                │─────────────────────┘
│                     │> Share name: data                                              │─────────────────────┐
│                     │  Valid user (blank = any Samba user):                          │%  100M of 118M      │
│                     │  Password (blank on edit = unchanged):                         │0M   Logical 100M    │
│                     │  Read-only (y/n): n                                            │0M   Snaps 256K      │
│                     │  Guest access (y/n): y                                         │   Refres 0B         │
│                     │                                                                │ 1.00x   Rec 128K    │
│                     │                                                                │  Ref -              │
│                     └────────────────────────────────────────────────────────────────┘LINE  errs 0         │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
│                      ││                                                                                    │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────┘
Enter Next  Tab Next field  Shift+Tab Prev field  Esc Cancel
```

- **Share name**: what clients see (`\\host\data`).
- **Valid user**: the one Unix account allowed in; it must already exist on the host. Leave it blank for "any Samba user".
- **Password**: required whenever a Valid user is set or changed (on edit, blank keeps the existing one if the user is unchanged); ignored for a share with no user. Samba keeps its own passwords, separate from the Unix ones.
- **Read-only**, and **Guest access** (`n` by default). Guest access cannot be combined with a Valid user.

What zfsctl does: it keeps all its shares in one marked block inside `/usr/local/etc/smb4.conf`:

```
## zfsctl managed shares -- do not edit by hand -- start
...
## zfsctl managed shares -- do not edit by hand -- end
```

Everything outside the block, such as your own `[homes]` share, stays exactly as you wrote it. After every change zfsctl reloads Samba. Samba itself (`pkg install samba4xx`) and its service must be installed on the host first.

> **Safety:** setting a user's Samba password here **replaces** that user's Samba password for all the shares they use. **Guest access** means anyone can connect with no password; if you enable it, also set Read-only to `y` unless you really want anonymous writes. Removing a share leaves the user's Samba password in place.

> **Tip:** zfsctl does not insist on its way. Shares you wrote by hand stay yours, and you can ignore this section and manage NFS and Samba with your own tools; zfsctl only manages what is inside its block.

## On the Dashboard

You do not have to open Shares on every host to find out that NFS or Samba has stopped. Once a day the daemon asks each host whether the NFS server and Samba run where the host has shares for them, and the Dashboard shows a red line for a host where they do not (chapter 6). `c` in the Hosts section checks one host right now.

## Try it

Share the practice dataset over NFS to your home network: Shares (once), Datasets, select it, `N`, enter your network. From another machine run `showmount -e <host>` and see it listed. Then remove it from Shares with `d`.

## Where next

- [Boot environments](13-boot-environments.md): undo a bad upgrade.
- [Scheduled jobs](14-jobs.md): automatic snapshots and scrubs.
