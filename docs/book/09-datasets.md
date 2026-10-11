# Datasets

## What you will learn

- How to read the Datasets list and its panel.
- How to create, change and destroy datasets, and what each property does.
- How to delegate permissions with `zfs allow` and read file ACLs.

## The list

A *dataset* is a filesystem (or a volume) inside a pool. Go to Datasets and select one with `Down`:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌Properties — scratch/data ('r' refresh, Enter to ┐
│Hosts                 ││Name                           Used           Avail          Kind        ││Property         Value            Source         │
│Pools                 ││scratch                        106639360      19189760       Filesystem  ││                                                 │
│Datasets              ││scratch/data                   105250816      19189760       Filesystem  ││                                                 │
│Snapshots             ││tpool                          4196505216     5628389760     Filesystem  ││                                                 │
│Disks                 ││zroot                          11325595648    32899702784    Filesystem  ││                                                 │
│Shares                ││zroot/ROOT                     5673803776     32899702784    Filesystem  ││                                                 │
│Boot Envs             ││zroot/ROOT/default             5673705472     32899702784    Filesystem  ││                                                 │
│Jobs                  ││zroot/home                     578416640      32899702784    Filesystem  ││                                                 │
│Alerts                ││zroot/home/alice               147456         32899702784    Filesystem  ││                                                 │
│                      ││zroot/home/fb                  578031616      32899702784    Filesystem  ││                                                 │
│                      │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│                      │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 119M                        │
│                      ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││                                                                           Data    100M   Snaps 200K                        │
│                      ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
│                      ││                                                                           Quota   -   Ref -                                │
│                      ││                                                                           Pool    ONLINE  errs 0                           │
│                      ││                                                                                                                            │
│                      ││                                                                                                                            │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  → Details  n New  d Destroy  D Destroy recursively  s Snapshot  N NFS share  M SMB share  J Snapshot job  r Refresh  ? Help  q Quit
```

The columns are the dataset's **Name**, the space it **Used** and the space **Avail**able to it, and its **Kind** (`Filesystem` or `Volume`). The sizes are shown in bytes; on a narrow terminal the columns are cut, so use a wide one (this capture is 150 columns) when you read the numbers. The panel below gives the same figures in human units.

On the right is a pane with the selected dataset's details, which you reach with `Right`. Its title says which tab you are in: **Properties**, **Permissions** or **ACL**; `[` and `]` cycle through them.

## The panel

With a dataset selected, the panel under the list shows where its space goes:

| Line | Meaning |
|---|---|
| **Used** | space used, as a percentage and in bytes, of what it could use |
| **Refer** and **Logical** | space this dataset refers to (excluding snapshots and children), and the size before compression |
| **Data**, **Snaps** | space held by the data itself, and by its snapshots |
| **Kids**, **Refres** | space used by child datasets, and by a reservation |
| **Compr** and **Rec** | compression and its ratio, and the record size |
| **Quota**, **Ref** | the quota and the refquota, if any |
| **Pool** | the pool's health |

## Properties

`Right` moves into the pane, which opens on **Properties**:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)───────────────────────┐┌Properties — scratch/data ('r' refresh, Enter to ed┐
│Hosts                 ││Name                         Used           Avail          Kind        ││Property         Value                   Source    │
│Pools                 ││scratch                      106639360      19189760       Filesystem  ││type             filesystem              -         │
│Datasets              ││scratch/data                 105250816      19189760       Filesystem  ││creation         1788654263              -         │
│Snapshots             ││tpool                        4196505216     5628389760     Filesystem  ││used             105250816               -         │
│Disks                 ││zroot                        11325603840    32899694592    Filesystem  ││available        19189760                -         │
│Shares                ││zroot/ROOT                   5673803776     32899694592    Filesystem  ││referenced       105046016               -         │
│Boot Envs             ││zroot/ROOT/default           5673705472     32899694592    Filesystem  ││compressratio    1.00                    -         │
│Jobs                  ││zroot/home                   578416640      32899694592    Filesystem  ││mounted          yes                     -         │
│Alerts                ││zroot/home/alice             147456         32899694592    Filesystem  ││quota            0                       default   │
│                      │└───────────────────────────────────────────────────────────────────────┘└───────────────────────────────────────────────────┘
│                      │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│                      ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 119M                        │
│                      ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││                                                                           Data    100M   Snaps 200K                        │
│                      ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
│                      ││                                                                           Quota   -   Ref -                                │
│                      ││                                                                           Pool    ONLINE  errs 0                           │
│                      ││                                                                                                                            │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row is a property with its **value** and its **source**: `local` (set on this dataset), `inherited from tank` (a parent's setting), or `default`. Properties with a `-` source (`used`, `available`, `referenced`) are read-only. `Enter` on an editable one opens a form with the current value. The ones you will meet most:

| Property | What it does |
|---|---|
| `compression` | `lz4` is a good default: it saves space and usually speeds things up |
| `quota` / `refquota` | the most this dataset (with / without its children and snapshots) may use |
| `reservation` / `refreservation` | space guaranteed to it |
| `recordsize` | the block size ZFS uses for files; 128K suits general use, smaller (16K) suits databases |
| `mountpoint` | where it is mounted |
| `readonly` | refuse writes |
| `sharenfs` | NFS export options (chapter 12 does this from a form) |

> **Safety:** shrinking a volume's `volsize` can destroy the data past the new size, so zfsctl shows the exact command and asks the two-key question first. Changing `mountpoint` on a dataset that is in use can disconnect programs using it.

> **Coming from Linux:** `quota` is like a per-directory quota without a quota daemon, and `compression=lz4` replaces any thought of compressing files yourself. **Coming from Windows:** `quota` is NTFS disk quotas for a folder, but per filesystem.

## Create a dataset

Press `n` and give the new dataset's name, including its pool: `tank/media`. Press `Enter`. It is created and mounted under its parent's mountpoint at once, and it inherits its parent's properties.

Make one dataset for each thing you will treat differently (home directories, media, databases, backups): you can then snapshot, limit and replicate them separately.

## Destroy a dataset

- `d` destroys the selected dataset. You **type its full name** to confirm. zfsctl itself refuses a pool's root dataset (use `d` on the Pools list to destroy a pool), and ZFS refuses if it still has snapshots or child datasets, and zfsctl shows you ZFS's message.
- `D` destroys the dataset **and everything under it**: its child datasets and all their snapshots. You type the name, and the question says how many datasets and snapshots will go. It is a `zfs destroy -r`, never `-R`, so clones elsewhere are not removed.

> **Safety:** `D` refuses a pool's root dataset, because that would wipe the whole pool, and it refuses anything that holds the running system or the boot environments (use the Boot Envs section for those). Everything else it will do after you type the name, so read the counts it shows.

## Permissions: `zfs allow`

The Permissions tab shows **delegated permissions**: ZFS lets you allow an ordinary user to snapshot, mount or create datasets without making them root.

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌Permissions — scratch/data ('r' refresh, n/s/c gr┐
│Hosts                 ││Name                           Used           Avail          Kind        ││Target           Scope           Permissions     │
│Pools                 ││scratch                        106639360      19189760       Filesystem  ││                                                 │
│Datasets              ││scratch/data                   105250816      19189760       Filesystem  ││                                                 │
│Snapshots             ││tpool                          4196505216     5628389760     Filesystem  ││                                                 │
│Disks                 ││zroot                          11325603840    32899694592    Filesystem  ││                                                 │
│Shares                ││zroot/ROOT                     5673803776     32899694592    Filesystem  ││                                                 │
│Boot Envs             │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│Jobs                  │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│Alerts                ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 119M                        │
│                      ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││                                                                           Data    100M   Snaps 200K                        │
│                      ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
n Grant  s Grant set  c Create-time  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

This dataset has none. The rows, when present, show the **target** (a user, a group, everyone, a named set, or create-time), the **scope** (the dataset itself, its descendants, or both) and the **permissions**. Keys: `n` grants to a recipient (`user:alice`, `group:backup`, `everyone`), `s` grants as a named permission set, `c` grants at create-time (what the creator of a new child receives), `d` revokes everything of the selected row and `x` revokes specific permissions. Revoking asks the two-key question first. In the forms, `F2` lists the host's real users and groups.

## ACLs

The ACL tab lists the **file access control list** of the dataset's mount point: who may read, write and execute.

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌ACL — scratch/data ('r' refresh, 'n' add, 'd' rem┐
│Hosts                 ││Name                           Used           Avail          Kind        ││#   Tar Permissions          Flags         Type  │
│Pools                 ││scratch                        106639360      19189760       Filesystem  ││0   own read_data/write_data —             Allow │
│Datasets              ││scratch/data                   105250816      19189760       Filesystem  ││1   gro read_data/execute/re —             Allow │
│Snapshots             ││tpool                          4196505216     5628389760     Filesystem  ││2   eve read_data/execute/re —             Allow │
│Disks                 ││zroot                          11325579264    32899719168    Filesystem  ││                                                 │
│Shares                ││zroot/ROOT                     5673803776     32899719168    Filesystem  ││                                                 │
│Boot Envs             │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│Jobs                  │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│Alerts                ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 119M                        │
│                      ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││                                                                           Data    100M   Snaps 200K                        │
│                      ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  n Add entry  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row has a position (`#`), a target (the owner, the group, everyone, or a named user or group), the permissions, any flags and the type (`Allow` or `Deny`). `n` adds an entry (always at the end) and `d` removes the selected one by position. This tab edits **NFSv4 ACLs**, the kind FreeBSD uses on ZFS with `acltype=nfsv4`; if the dataset has `acltype=posix`, the title says so and the tab does not edit it.

> **Coming from Linux:** NFSv4 ACLs are richer than POSIX ACLs (`setfacl`); the permissions have names like `read_data`. **Coming from Windows:** these are very close to NTFS permissions, which is why Samba maps onto them so well.

## Try it

Make a dataset for practice on the practice pool from chapter 2: Datasets, `n`, `bookdemo/notes`, `Enter`. Press `Right` and look at its properties. Set `quota` to `10M` with `Enter`. Put a file in it from the shell, look at the panel, then destroy it with `d`, typing `bookdemo/notes`.

## Where next

- [Snapshots](10-snapshots.md): protecting what is in a dataset.
- [Shares](12-shares.md): letting other machines use it.
