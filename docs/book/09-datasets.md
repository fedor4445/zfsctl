# Datasets

## What you will learn

- How to read the Datasets list and its panel.
- How to create, change and destroy datasets, and what each property does.
- How to delegate permissions with `zfs allow` and read file ACLs.

## The list

A *dataset* is a filesystem (or a volume) inside a pool. Go to Datasets and select one with `Down`:

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌Properties — scratch/data ('r' refresh, Enter to ┐
│Dashboard             ││Name                                  Used     Avail    Lock   Kind      ││Property         Value            Source         │
│Hosts                 ││scratch                               103M     17.3M           Filesystem││                                                 │
│Pools                 ││scratch/data                          100M     17.3M           Filesystem││                                                 │
│Datasets              ││scratch/vault                         408K     17.3M    LOCKED Filesystem││                                                 │
│Snapshots             ││scratch/vault/docs                    200K     17.3M    LOCKED Filesystem││                                                 │
│Disks                 ││tpool                                 3.9G     5.2G            Filesystem││                                                 │
│Shares                ││zroot                                 10.7G    30.5G           Filesystem││                                                 │
│Boot Envs             ││zroot/ROOT                            5.3G     30.5G           Filesystem││                                                 │
│Jobs                  ││zroot/ROOT/default                    5.3G     30.5G           Filesystem││                                                 │
│Cloud                 ││zroot/home                            595M     30.5G           Filesystem││                                                 │
│Alerts                │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│Settings              │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│                      ││Read  4.0K/s  peak 4.0K/s             Write 0B/s  peak 0B/s                Used    85%  100M of 118M                        │
│                      ││0 ops  lat 1.6ms                      0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││⡇                                                                          Data    100M   Snaps 256K                        │
│                      ││⡇                                                                          Kids    0B   Refres 0B                           │
│                      ││⡇                                                                          Compr   on 1.00x   Rec 128K                      │
│                      ││⡇                                                                          Quota   -   Ref -                                │
│                      ││⡇                                                                          Pool    ONLINE  errs 0                           │
│                      ││⡇                                                                                                                           │
│                      ││⡇                                                                                                                           │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  → Details  n New  E New encrypted  d Destroy  D Destroy recursively  s Snapshot  N NFS share  M SMB share  J Snapshot job  ? Help  q Quit
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
│Dashboard             ││Name                                Used     Avail    Lock   Kind      ││Property         Value                   Source    │
│Hosts                 ││scratch                             103M     17.3M           Filesystem││type             filesystem              -         │
│Pools                 ││scratch/data                        100M     17.3M           Filesystem││creation         1788654263              -         │
│Datasets              ││scratch/vault                       408K     17.3M    LOCKED Filesystem││used             105308160               -         │
│Snapshots             ││scratch/vault/docs                  200K     17.3M    LOCKED Filesystem││available        18178048                -         │
│Disks                 ││tpool                               3.9G     5.2G            Filesystem││referenced       105046016               -         │
│Shares                ││zroot                               10.7G    30.5G           Filesystem││compressratio    1.00                    -         │
│Boot Envs             ││zroot/ROOT                          5.3G     30.5G           Filesystem││mounted          yes                     -         │
│Jobs                  ││zroot/ROOT/default                  5.3G     30.5G           Filesystem││quota            0                       default   │
│Cloud                 │└───────────────────────────────────────────────────────────────────────┘└───────────────────────────────────────────────────┘
│Alerts                │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│Settings              ││Read  4.0K/s  peak 4.0K/s             Write 0B/s  peak 0B/s                Used    85%  100M of 118M                        │
│                      ││0 ops  lat 1.6ms                      0 ops  lat -                         Refer   100M   Logical 100M                      │
│                      ││⡇                                                                          Data    100M   Snaps 256K                        │
│                      ││⡇                                                                          Kids    0B   Refres 0B                           │
│                      ││⡇                                                                          Compr   on 1.00x   Rec 128K                      │
│                      ││⡇                                                                          Quota   -   Ref -                                │
│                      ││⡇                                                                          Pool    ONLINE  errs 0                           │
│                      ││⡇                                                                                                                           │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row is a property with its **value** and its **source**: `local` (set on this dataset), `inherited from tank` (a parent's setting), or `default`. Properties with a `-` source (`used`, `available`, `referenced`) are read-only. `Enter` on an editable one opens a form with the current value. The ones you will meet most:

| Property | What it does |
|---|---|
| `compression` | `on` means lz4, which is the default for new pools and datasets on OpenZFS 2.2 and later: it saves space and usually costs no speed. `zstd` (levels 1 to 19; plain `zstd` is `zstd-3`) compresses harder for more CPU, and needs a pool feature (`zstd_compress`, chapter 8). Only data written afterwards is affected |
| `quota` / `refquota` | the most this dataset (with / without its children and snapshots) may use |
| `reservation` / `refreservation` | space guaranteed to it |
| `recordsize` | the largest block ZFS uses for a file; the default (128K) suits general use. It applies only to files written afterwards, and OpenZFS calls changing it on general-purpose filesystems "strongly discouraged"; the classic reason to is a database that reads and writes fixed pages (match them, for example 16K) |
| `mountpoint` | where it is mounted |
| `readonly` | refuse writes |
| `sharenfs` | NFS export options (chapter 12 does this from a form) |

> **Safety:** shrinking a volume's `volsize` can destroy the data past the new size, so zfsctl shows the exact command and asks the two-key question first. Changing `mountpoint` on a dataset that is in use can disconnect programs using it.

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
- **Turning `dedup` on** asks you to **type the dataset's name** too: deduplication keeps a table with one entry for every unique block and looks in it on every write, so the table wants to live in memory. Its size depends on how many unique blocks the pool holds, not on terabytes: many small blocks make a big table (the Handbook's rule of thumb is 5 to 6 GB of RAM per TB, and the real figure can be well above or below that). Estimate before you switch it on: `zdb -S tank` simulates deduplication on the data already there and prints the table it would build (it reads the whole pool, so run it when the machine is quiet; [zdb(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zdb.8.html)). OpenZFS 2.3's *fast dedup* (a log for table updates, the `dedup_table_quota` pool property that caps the table, and `zpool ddtprune` to drop old unique entries) is, in the release notes' words, "a major performance upgrade", but the rule is the same: try it on your own data first. The question shows how much RAM the host has and warns when that is under 16 GiB, and switching it off later does not undeduplicate what is already stored. Turning it off, or changing the algorithm, asks `y` then `Enter`.

The panel under the list shows `Checksum` and `Dedup` only when they are not the defaults. Needs attention lists a host that has a dataset with its checksum off (`tank/scratch checksum is off`, up to three names) and a pool that has dedup on but saves less than `dedup_min_ratio` (default 1.10x) once it holds more than 10 GiB and has had dedup on for a week (`tank dedup is on but saves only 1.02x`): paying memory for nothing. The wait matters: the ratio only counts data written after dedup was switched on, so a pool full of old data reads 1.00x at first.

## Encrypted datasets

ZFS can encrypt a dataset (`encryption=aes-256-gcm` and the like), but only when the dataset is created; the key is then loaded and unloaded. zfsctl shows which datasets are locked and loads, unloads and changes passphrase keys; it also creates them (`E`, below):

- When any dataset on the host is encrypted, the Datasets list gets a **Lock** column: blank for a plain dataset, `open` when the key is loaded and **`LOCKED`** (red) when it is not. A locked dataset cannot be read or mounted. The panel adds the `Encrypt` line described above.
- A locked dataset that a restic backup reads, that a replication writes into, or that is shared over NFS (a Samba share is not seen) is a yellow line in Needs attention (`tank/secure is locked (a job uses it)`) and in the daily report: it is something that will fail. A locked dataset nobody uses gets no line.
- **What encryption does not hide.** ZFS encrypts file and volume data, file attributes, ACLs, permission bits and directory listings. It does **not** encrypt the structure of the pool: dataset and snapshot names, the dataset hierarchy, properties, file sizes and holes, and the deduplication tables are readable by anyone who has the disks ([zfs-load-key(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zfs-load-key.8.html)). Deduplication inside an encrypted dataset reveals which blocks are equal, and data is compressed before it is encrypted, so an application that lets an attacker influence the data and watch its size can leak through compression, as in the CRIME attack. Changing the passphrase does not re-encrypt the data, and the old wrapped key may still be recoverable from the disk.
- **Encrypting data you already have.** A dataset cannot be encrypted in place. Create an encrypted dataset and copy the data into it: with `rsync`, or by receiving a plain `zfs send` stream into it with the encryption options (a full stream received that way becomes an encryption root; `keylocation=prompt`, which zfsctl's key keys expect, is then set with `zfs set`, see [zfs-receive(8)](https://openzfs.github.io/openzfs-docs/man/master/8/zfs-receive.8.html)).
- **Jobs and encryption:** a cloud mirror **refuses an encrypted source**, and a replication (to a host or to stream files) refuses one unless its **Raw** field is `y`: a plain send delivers the data decrypted, a raw one (`zfs send -w`) keeps it encrypted, and the copy on the destination is locked and stays locked. The form refuses to save a job that would send in the clear when it already knows the dataset is encrypted, and a job whose source became encrypted later fails with the same reason (red on the Dashboard). A locked raw replica is not reported as a problem. A **restic backup is allowed**: restic encrypts the files with its own key, so an open encrypted dataset is backed up as usual, while a locked one fails with `tank/secure is locked (key not loaded): load the key first`.

### Create an encrypted dataset: `E`

`E` opens a form for a new **encrypted** dataset: its full name (with the pool), the cipher (`aes-256-gcm` unless you change it; `aes-128/192/256-gcm` and `-ccm` also exist) and a passphrase typed twice (8 characters or more, shown as `*`). Then you **type the dataset's name** to confirm, under a warning you should take literally: **if you lose the passphrase the data cannot be recovered by anyone**, zfsctl included, because zfsctl stores no key anywhere. Encryption can only be chosen now; it cannot be added to or removed from a dataset later. The key is a passphrase asked at the prompt (`keylocation=prompt`), which is what `L` and `C` work with.

Everything created under an encrypted dataset with the plain `n` is encrypted with the same key; `E` refuses a name under one, because it would start a second key. The Properties tab marks the properties that can only be set at creation with a `*` (`encryption`, `casesensitivity`, `normalization`, `utf8only`, `volblocksize`). An older `zfs` without native encryption answers with its own error.

### Keys: `L`, `U`, `C`

Three keys act on the selected encrypted dataset (a child that shares its parent's key acts on the **encryption root**, the dataset whose key it is; the form names it). The row's state decides which one is offered.

- `L` **loads the key** of a locked dataset. A form asks for the passphrase (shown as `*`) and whether to mount the dataset afterwards (`y` by default). The passphrase is passed to `zfs load-key` on its input, never in a command line, and zfsctl does not keep it. A wrong passphrase is ZFS's `Incorrect key provided`, and the form is not re-filled.
- `U` **unloads the key**. After it the data cannot be read until you load the key again, so you press `y` then `Enter`. ZFS refuses while the dataset, or one under it, is still mounted; unmount it first at the shell (zfsctl shows ZFS's message and says so).
- `C` **changes the passphrase** of an open encryption root: the new one twice (8 characters or more), then `y` and `Enter` on a question that says the old passphrase stops working, that the data is not re-encrypted (copies sent elsewhere earlier, such as raw replicas, still need the old passphrase) and that nothing else keeps the new one. On a child that uses its parent's key the key says so and does nothing, because a change there would give the child a key of its own.

Only keys typed at a prompt are handled (`keylocation=prompt`, the default, with `keyformat=passphrase`). zfsctl asks the host again right before it loads or changes a key, and refuses a dataset whose key is read from a file (`keylocation=file://...`, or a raw or hex key): zfs would read the file and ignore what you typed, so use `zfs load-key` and `zfs change-key` at the shell for those. Loading mounts only the dataset itself, and only when it is a filesystem with `canmount=on`; a container with `canmount=off` or a volume is just unlocked, and the datasets under it are mounted by ZFS's own `zfs mount`.

## Create a dataset

Press `n` and give the new dataset's name, including its pool: `tank/media`. Press `Enter`. It is created and mounted under its parent's mountpoint at once, and it inherits its parent's properties.

Make one dataset for each thing you will treat differently (home directories, media, databases, backups): you can then snapshot, limit and replicate them separately.

## Destroy a dataset

- `d` destroys the selected dataset. You **type its full name** to confirm. zfsctl itself refuses a pool's root dataset (use `d` on the Pools list to destroy a pool), and ZFS refuses if it still has snapshots or child datasets, and zfsctl shows you ZFS's message.
- `D` destroys the dataset **and everything under it**: its child datasets and all their snapshots. You type the name, and the question says how many datasets and snapshots will go. It is a `zfs destroy -r`, never `-R`, so clones elsewhere are not removed.

> **Safety:** `D` refuses a pool's root dataset, because that would wipe the whole pool, and it refuses anything that holds the running system or the boot environments (use the Boot Envs section for those). Everything else it will do after you type the name, so read the counts it shows.

## Permissions: `zfs allow`

The Permissions tab shows **delegated permissions**: ZFS lets you allow an ordinary user to snapshot, mount or create datasets without making them root. On FreeBSD, mounting also needs the system to let ordinary users mount at all (`sysctl vfs.usermount=1`, which the [Handbook](https://docs.freebsd.org/en/books/handbook/zfs/)'s delegation example sets) and the user must own the mount point's directory; `zfs allow … mount` alone is not enough.

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌Permissions — scratch/data ('r' refresh, n/s/c gr┐
│Dashboard             ││Name                                  Used     Avail    Lock   Kind      ││Target           Scope           Permissions     │
│Hosts                 ││scratch                               103M     17.3M           Filesystem││                                                 │
│Pools                 ││scratch/data                          100M     17.3M           Filesystem││                                                 │
│Datasets              ││scratch/vault                         408K     17.3M    LOCKED Filesystem││                                                 │
│Snapshots             ││scratch/vault/docs                    200K     17.3M    LOCKED Filesystem││                                                 │
│Disks                 ││tpool                                 3.9G     5.2G            Filesystem││                                                 │
│Shares                │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│Boot Envs             │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│Jobs                  ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 118M                        │
│Cloud                 ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│Alerts                ││                                                                           Data    100M   Snaps 256K                        │
│Settings              ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
n Grant  s Grant set  c Create-time  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

This dataset has none. The rows, when present, show the **target** (a user, a group, everyone, a named set, or create-time), the **scope** (the dataset itself, its descendants, or both) and the **permissions**. Keys: `n` grants to a recipient (`user:alice`, `group:backup`, `everyone`), `s` grants as a named permission set, `c` grants at create-time (what the creator of a new child receives), `d` revokes everything of the selected row and `x` revokes specific permissions. Revoking asks the two-key question first. In the forms, `F2` lists the host's real users and groups.

## ACLs

The ACL tab lists the **file access control list** of the dataset's mount point: who may read, write and execute.

```screen
┌zfsctl────────────────┐┌Datasets — local ('r' refresh, → for properties)─────────────────────────┐┌ACL — scratch/data ('r' refresh, 'n' add, 'd' rem┐
│Dashboard             ││Name                                  Used     Avail    Lock   Kind      ││#   Tar Permissions          Flags         Type  │
│Hosts                 ││scratch                               103M     17.3M           Filesystem││0   own read_data/write_data —             Allow │
│Pools                 ││scratch/data                          100M     17.3M           Filesystem││1   gro read_data/execute/re —             Allow │
│Datasets              ││scratch/vault                         408K     17.3M    LOCKED Filesystem││2   eve read_data/execute/re —             Allow │
│Snapshots             ││scratch/vault/docs                    200K     17.3M    LOCKED Filesystem││                                                 │
│Disks                 ││tpool                                 3.9G     5.2G            Filesystem││                                                 │
│Shares                │└─────────────────────────────────────────────────────────────────────────┘└─────────────────────────────────────────────────┘
│Boot Envs             │┌Metrics — scratch/data (I/O of pool scratch)────────────────────────────────────────────────────────────────────────────────┐
│Jobs                  ││Read  0B/s  peak 0B/s                 Write 0B/s  peak 0B/s                Used    85%  100M of 118M                        │
│Cloud                 ││0 ops  lat -                          0 ops  lat -                         Refer   100M   Logical 100M                      │
│Alerts                ││                                                                           Data    100M   Snaps 256K                        │
│Settings              ││                                                                           Kids    0B   Refres 0B                           │
│                      ││                                                                           Compr   on 1.00x   Rec 128K                      │
└──────────────────────┘└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
↑↓ Move  n Add entry  [ ] Tabs  ← Back  r Refresh  Tab Section  ! Shell  A About  ? Help  q Quit
```

Each row has a position (`#`), a target (the owner, the group, everyone, or a named user or group), the permissions, any flags and the type (`Allow` or `Deny`). `n` adds an entry (always at the end) and `d` removes the selected one by position. This tab edits **NFSv4 ACLs**, the kind FreeBSD uses on ZFS with `acltype=nfsv4`; if the dataset has `acltype=posix`, the title says so and the tab does not edit it.

## Try it

Make a dataset for practice on the practice pool from chapter 2: Datasets, `n`, `bookdemo/notes`, `Enter`. Press `Right` and look at its properties. Set `quota` to `10M` with `Enter`. Put a file in it from the shell, look at the panel, then destroy it with `d`, typing `bookdemo/notes`.

## Try encryption

Seven steps on the practice pool (`bookdemo`), from a new encrypted dataset to a changed passphrase. Nothing here touches real data. Keep the passphrases somewhere you can read: zfsctl stores none.

1. **Create.** Press `E`. Name `bookdemo/vault`, cipher `aes-256-gcm` (the default), passphrase `correct horse battery` twice. Press `Enter` on the last field, then type `bookdemo/vault` and press `Enter`. The list shows `bookdemo/vault` with `open` in the Lock column. Press `Right`: the Properties tab shows `encryption*` as `aes-256-gcm` (the `*` marks what can never change).
2. **Put something in it.** Press `n` and create `bookdemo/vault/docs`. It uses the same key: its Lock column also says `open`. Write a file into `/bookdemo/vault` from the shell if you like.
3. **Lock it.** Select `bookdemo/vault`, press `U`, then `y`, then `Enter`. ZFS refuses with `is busy`: the dataset is mounted, and zfsctl never unmounts for you. At the shell run `zfs unmount bookdemo/vault/docs` (it may already be unmounted) and then `zfs unmount bookdemo/vault`. Press `U`, `y`, `Enter` again. Both rows turn red **LOCKED**, and the files cannot be read.
4. **Try the guards while it is locked.**
   - Press `L` on the child `bookdemo/vault/docs`. The form is titled `Load key: 'bookdemo/vault'`: the key belongs to the top dataset, so that is what is loaded. Press `Esc`.
   - Press `U` or `C` on a locked row: the status line says it is already locked, or to load the key first.
   - Press `E` and try the name `bookdemo/vault/other`: refused, because it is under an encrypted dataset (use `n` there).
   - Try to save a replication or cloud-mirror job whose source is `bookdemo/vault` (Jobs, see [Jobs](14-jobs.md)): refused with `is encrypted`.
5. **Unlock it.** Press `L` on `bookdemo/vault`. Type a wrong passphrase and press `Enter` twice (once to leave the field, once to submit): ZFS answers `Incorrect key provided`. Press `L` again with the right one and leave `Mount it after loading` at `y`. Both rows go back to `open` and `bookdemo/vault` is mounted again.
6. **Change the passphrase.** Select `bookdemo/vault`, press `C`, type `a brand new pass` twice and press `Enter`. Read the question (the old passphrase stops working; nothing is re-encrypted; keep the new one safe), press `y`, then `Enter`. Lock it again as in step 3 and press `L`: the old passphrase is now refused and the new one works.
7. **Clean up.** Select `bookdemo/vault/docs` and press `d`, typing its name; then do the same for `bookdemo/vault`. (Or `D` on `bookdemo/vault` to remove both at once.)

If a restic backup job reads a dataset while it is locked, the Dashboard shows `bookdemo/vault is locked (a job uses it)` after the next storage check (`c` in Hosts runs it at once).

## Where next

- [Snapshots](10-snapshots.md): protecting what is in a dataset.
- [Shares](12-shares.md): letting other machines use it.
