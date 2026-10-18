# Datasets

## What you will learn

- How to read the Datasets list and its panel.
- How to create, change and destroy datasets, and what each property does.
- How to delegate permissions with `zfs allow` and read file ACLs.

## The list

A *dataset* is a filesystem (or a volume) inside a pool. Go to Datasets and select one with `Down`:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌Properties — scratch/data ('r' refresh, Enter to ┐
│Hosts                 ││Name                                         Used     Avail    Kind      ││Property         Value            Source         │
│Pools                 ││scratch                                      102M     18.3M    Filesystem││                                                 │
│Datasets              ││scratch/data                                 100M     18.3M    Filesystem││                                                 │
│Snapshots             ││tpool                                        3.9G     5.2G     Filesystem││                                                 │
│Disks                 ││zroot                                        10.6G    30.6G    Filesystem││                                                 │
│Shares                ││zroot/ROOT                                   5.3G     30.6G    Filesystem││                                                 │
│Boot Envs             ││zroot/ROOT/default                           5.3G     30.6G    Filesystem││                                                 │
│Jobs                  ││zroot/home                                   554M     30.6G    Filesystem││                                                 │
│Alerts                ││zroot/home/alice                             144K     30.6G    Filesystem││                                                 │
│                      ││zroot/home/fb                                554M     30.6G    Filesystem││                                                 │
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

The columns are the dataset's **Name**, the space it **Used** and the space **Avail**able to it, and its **Kind** (`Filesystem` or `Volume`). The sizes are in human units (`100M`, `30.6G`). On a narrow terminal the Kind column is cut short (this capture is 150 columns wide so that it shows in full). The panel below gives more detail.

On the right is a pane with the selected dataset's details, which you reach with `Right`. Its title says which tab you are in: **Properties**, **Permissions** or **ACL**; `[` and `]` cycle through them.

## The panel

With a dataset selected, the panel under the list shows where its space goes:

| Line | Meaning |
|---|---|
| **Used** | space used, as a percentage and as sizes, of what it could use |
| **Refer** and **Logical** | space this dataset refers to (excluding snapshots and children), and the size before compression |
| **Data**, **Snaps** | space held by the data itself, and by its snapshots |
| **Kids**, **Refres** | space used by child datasets, and by a reservation |
| **Compr** and **Rec** | compression and its ratio, and the record size |
| **Quota**, **Ref** | the quota and the refquota, if any, each with how much of it is used (`10.0G 96%`) |
| **Encrypt** | only for an encrypted dataset: the cipher, the key format and the encryption root, and `LOCKED` when its key is not loaded |
| **Pool** | the pool's health |

## Properties

`Right` moves into the pane, which opens on **Properties**:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)───────────────────────┐┌Properties — scratch/data ('r' refresh, Enter to ed┐
│Hosts                 ││Name                                       Used     Avail    Kind      ││Property         Value                   Source    │
│Pools                 ││scratch                                    102M     18.3M    Filesystem││type             filesystem              -         │
│Datasets              ││scratch/data                               100M     18.3M    Filesystem││creation         1788654263              -         │
│Snapshots             ││tpool                                      3.9G     5.2G     Filesystem││used             105250816               -         │
│Disks                 ││zroot                                      10.6G    30.6G    Filesystem││available        19189760                -         │
│Shares                ││zroot/ROOT                                 5.3G     30.6G    Filesystem││referenced       105046016               -         │
│Boot Envs             ││zroot/ROOT/default                         5.3G     30.6G    Filesystem││compressratio    1.00                    -         │
│Jobs                  ││zroot/home                                 554M     30.6G    Filesystem││mounted          yes                     -         │
│Alerts                ││zroot/home/alice                           144K     30.6G    Filesystem││quota            0                       default   │
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

## Quotas and reservations

Four properties cap or protect a dataset's space: `quota` and `refquota` are ceilings, `reservation` and `refreservation` are space kept back for it. The `quota` and `reservation` count the dataset, its children and its snapshots; the `ref` versions count only the dataset's own data.

`Enter` on one of these rows (Properties tab) opens a size form. Type `10G`, `500M`, `1.5T` (powers of 1024; `none` removes the limit). zfsctl checks before it sends anything:

- a quota below what the dataset already uses is refused (`quota` is compared with *used*, `refquota` with *referenced*, which leaves out snapshots);
- a reservation larger than the quota, or a quota below the reservation, is refused;
- a reservation the pool cannot back (more than the space still available) is refused;
- a volume has no quota: `quota` and `refquota` say they do not apply, while `reservation` and `refreservation` work.

Every change to one of these four asks for `y` then `Enter` first, showing the exact `zfs set` command; one that leaves less than 10% room over what is used now, or raises a reservation by more than a tenth of the free space, says so in the question.

When any dataset on the host has a limit, the Datasets list gets a **Quota** column (`96% of 10G`, yellow from 90%, red from 98%); it is the first column to go on a narrow terminal. The Dashboard lists, per host, the dataset nearest to its limit in Needs attention, and the daily report repeats it; the percentages are `quota_warn` and `quota_crit` under `[dashboard]` (or Settings). The daemon re-reads quotas every ten minutes, and `c` in Hosts does it on demand.

## Checksum and deduplication

`checksum` is how ZFS notices damaged data, and `dedup` stores identical blocks once. `Enter` on either row opens a form; `F2` lists the choices (checksum: `on`, `off`, `fletcher2`, `fletcher4`, `sha256`, `sha512`, `skein`, `edonr`, `blake3`; dedup: `off`, `on`, `verify` and the algorithm names, with or without `,verify`). The value is checked before anything is sent: an unknown word is refused with the list, and `blake3` is refused on a host that is known to run OpenZFS older than 2.2 (a host whose version is not known is left to answer for itself).

Both properties only affect data written afterwards. zfsctl says so, and asks:

- **`checksum=off`** asks you to **type the dataset's name**: with the checksum off, silent corruption in that dataset is not detected and a scrub cannot repair it. Any other checksum change asks for `y` then `Enter` and reminds you to run a scrub afterwards.
- **Turning `dedup` on** asks you to **type the dataset's name** too: deduplication keeps a table of every block in memory (a rule of thumb is 1 to 5 GB of RAM per TB of deduplicated data), the question shows how much RAM the host has and warns when that is under 16 GiB, and switching it off later does not undeduplicate what is already stored. Turning it off, or changing the algorithm, asks `y` then `Enter`.

The panel under the list shows `Checksum` and `Dedup` only when they are not the defaults. Needs attention lists a host that has a dataset with its checksum off (`tank/scratch checksum is off`, up to three names) and a pool that has dedup on but saves less than `dedup_min_ratio` (default 1.10x) once it holds more than 10 GiB and has had dedup on for a week (`tank dedup is on but saves only 1.02x`): paying memory for nothing. The wait matters: the ratio only counts data written after dedup was switched on, so a pool full of old data reads 1.00x at first. The illumos dump device (`rpool/dump`) has its checksum off by design and is not listed.

## Encrypted datasets

ZFS can encrypt a dataset (`encryption=aes-256-gcm` and the like), but only when the dataset is created; the key is then loaded and unloaded. zfsctl shows this much today and does not yet create or unlock encrypted datasets:

- When any dataset on the host is encrypted, the Datasets list gets a **Lock** column: blank for a plain dataset, `open` when the key is loaded and **`LOCKED`** (red) when it is not. A locked dataset cannot be read or mounted. The panel adds the `Encrypt` line described above.
- A locked dataset that a restic backup reads, that a replication writes into, or that is shared over NFS (or over SMB on illumos; a FreeBSD Samba share is not seen) is a yellow line in Needs attention (`tank/secure is locked (a job uses it)`) and in the daily report: it is something that will fail. A locked dataset nobody uses gets no line.
- **Jobs and encryption:** a replication (to a host or to stream files) and a cloud mirror **refuse an encrypted source**. Without ZFS's raw option a send delivers the data decrypted, and zfsctl does not send data in the clear without being told to. The form refuses to save such a job when it already knows the dataset is encrypted, and a job whose source became encrypted later fails with the same reason (red on the Dashboard). A **restic backup is allowed**: restic encrypts the files with its own key, so an open encrypted dataset is backed up as usual, while a locked one fails with `tank/secure is locked (key not loaded): load the key first`.

Load the key with `zfs load-key tank/secure` on the host (zfsctl will do it for you in a later version).

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
│Hosts                 ││Name                                         Used     Avail    Kind      ││Target           Scope           Permissions     │
│Pools                 ││scratch                                      102M     18.3M    Filesystem││                                                 │
│Datasets              ││scratch/data                                 100M     18.3M    Filesystem││                                                 │
│Snapshots             ││tpool                                        3.9G     5.2G     Filesystem││                                                 │
│Disks                 ││zroot                                        10.6G    30.6G    Filesystem││                                                 │
│Shares                ││zroot/ROOT                                   5.3G     30.6G    Filesystem││                                                 │
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
│Hosts                 ││Name                                         Used     Avail    Kind      ││#   Tar Permissions          Flags         Type  │
│Pools                 ││scratch                                      102M     18.3M    Filesystem││0   own read_data/write_data —             Allow │
│Datasets              ││scratch/data                                 100M     18.3M    Filesystem││1   gro read_data/execute/re —             Allow │
│Snapshots             ││tpool                                        3.9G     5.2G     Filesystem││2   eve read_data/execute/re —             Allow │
│Disks                 ││zroot                                        10.6G    30.6G    Filesystem││                                                 │
│Shares                ││zroot/ROOT                                   5.3G     30.6G    Filesystem││                                                 │
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
