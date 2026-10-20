# Snapshot recovery: hold, timeline, browse, restore as a copy (design)

Status: draft for review, 2026-10-20. Nothing built.

## Why

On 2026-10-20 the user's phone cleaned out its camera folder and Syncthing spread about 2,500 deletions to every other
device. The only complete copy of the photos was in July's ZFS snapshots, on a host that was nearly full. At the moment
that mattered (the app stopped, the damage assessed) the user wanted to know **which snapshot still held which files**
and to **keep that snapshot safe**. It took a long time: the snapshot directory (`.zfs`) was not visible through the
zone's mount, reading it needed root, and nothing in zfsctl showed what had changed between two snapshots.

zfsctl already lists, creates, rolls back and destroys snapshots. It cannot protect one, show what changed between two,
or give a file back. This design adds those, in four slices that are each useful alone.

The user remembers vaguely, never file names ("I'm not Claude"). So the design answers "when did it change and how
much", then "where", and only then "which file".

## Goals

1. **Protect a snapshot** so that nothing, not a person, a retention job or a recursive destroy, can remove it by
   accident, until it is released on purpose.
2. **Show what changed** between a snapshot and the one before it: counts of files added, removed, modified, renamed,
   grouped by folder, with file type and change time, so the moment of a mass deletion is obvious.
3. **Let the user look inside** a snapshot's folders with the facts a vague memory can use: type, size, created,
   modified, changed, mode and owner; filter by date, size and type.
4. **Give a file back as a copy** at a new path, never over anything.

## Non-goals

- A general file manager. No editing, moving, deleting, uploading or opening of files.
- Showing file *contents*. Slices 1 to 3 never read the bytes of a file. Slice 4 copies bytes on the host but does not
  display them.
- Restoring a whole dataset or rolling back (rollback exists already and is a different, destructive operation).
- Searching by file name across snapshots (the user does not remember names).
- Anything on a host that has not been given the matching sudoers feature.

## Anti-goals (a failure even if it "works")

- Any step that deletes, overwrites, moves or rewrites user data, or makes a snapshot easier to lose.
- A hold released by a job, by a timer, or by one keystroke.
- A restore that replaces an existing file.
- Widening what a host's unprivileged zfsctl user may do beyond what a slice needs (see Security).
- A browse or diff that can hang the TUI or fill the hub's memory on a dataset with millions of files.

## Constraints

- **Data retention and security come first** (user's words). When a choice trades convenience for safety, safety wins.
- Works on every host type zfsctl supports: FreeBSD, Linux, illumos (OmniOS). Every command line is checked against the
  real tool on each OS before it is written (see Verified facts).
- Read-only by default; every change goes through the existing two-key confirm (`y`/`n`, then Enter) except taking a
  hold, which is harmless and reversible (see 1).
- Rust only, no new dependencies unless unavoidable. The command registry (`src/exec/privilege.rs`) stays the single
  list of what may run; `print-sudoers` and `check-host` follow it.
- Same look as the rest of the program (ratatui, the existing hint bar, modal and confirm helpers).

## Verified facts (checked on real hosts on 2026-10-20)

- `zfs` is already allowed in full for the `core` feature (`Entry { name: "zfs", features: [Core], kind: ANY }`), so
  `zfs hold`, `zfs holds`, `zfs release` and `zfs diff` need **no new sudoers entry**.
- `zfs get -r -H -p -t snapshot -o name,value userrefs <dataset>` lists the number of holds of every snapshot of a
  dataset in one call (FreeBSD hub and OmniOS verified). `... written` gives the bytes written since the previous
  snapshot, in one call. **On illumos the options must come before the property name** (`-r` after `userrefs` fails with
  "missing '@' delimiter"); build every argument list options-first.
- `zfs holds -H <snapshot>` prints nothing when there are no holds, and exits 0 (FreeBSD and OmniOS).
- `zfs diff -FHt <older snapshot> <newer snapshot or dataset>` prints one line per change:
  `<ctime seconds>.<nanoseconds>\t<change>\t<type>\t<path>`, where change is `+` `-` `M` or `R`, type is `F` file, `/`
  directory, `@` symlink, `=` socket, `>` door, `|` pipe, `B` block, `C` character, `P` event port (FreeBSD, OmniOS
  verified; Linux OpenZFS 2.2.2 documents the same flags but a Linux host with a snapshot has not been tried). A rename
  line has a fifth field, the new path.
- **Paths from `zfs diff` are given as the host's global view**, not as the user knows them. On OmniOS, for a dataset
  delegated to a zone, a path reads `/rpool/zones/syncthing/root/omni30/syncthing/state/...` while the dataset's
  `mountpoint` is `/omni30/syncthing`. Show paths relative to the dataset's own mount: strip up to and including the
  dataset's mountpoint when it appears in the path, otherwise strip the longest common prefix of all paths and say so.
- On the same OmniOS dataset the `.zfs` snapshot directory does not exist (`ls /tank/syncthing/.zfs` failed), so
  browsing cannot assume `.zfs/snapshot/<name>/`.
- FreeBSD `stat -f` and Linux `stat -c` differ, and illumos `ls -E`/`stat` differ again: a portable metadata read is
  not one command line (see slice 3).

## The slices

### Slice 1: hold (protect) a snapshot

**What the user sees.** In the Snapshots section a new column **Hold** shows `H` (zfsctl's hold) or `h` (someone
else's) or nothing, and the panel names the tags. A key (proposed: `H`) on the highlighted snapshot toggles zfsctl's hold:

- **Take:** one confirm line ("Protect `tank/data@x` from deletion?"), `y` then Enter. Taking a hold only makes a
  snapshot harder to lose, so it does not need the "type the name" confirm.
- **Release:** the stricter confirm: two keys and the snapshot's name typed, because it makes a snapshot destroyable
  again. The text says what will become possible.
- **Held snapshots in every destroy path.** `d` (destroy) and `D` (recursive destroy) check the hold count first and
  refuse with a message that names the tags ("protected: 1 hold, tag `zfsctl`; release it first"). ZFS itself refuses
  too; zfsctl must never use `zfs destroy -d` (deferred destroy), which would destroy the snapshot the moment the hold
  is released.
- **Jobs.** Snapshot retention, backup and mirror "new" snapshot pruning and `dest_keep` skip a held snapshot and log
  "kept (held)" in the run's message. A held snapshot is not an error and does not fail a run.

**Commands (all `zfs`, already allowed).**
`zfs hold -- zfsctl <snapshot>` · `zfs release -- zfsctl <snapshot>` · `zfs holds -H -- <snapshot>` ·
`zfs get -r -H -p -t snapshot -o name,value userrefs <dataset>` (one call fills the column; refresh with the list).
Tag: the literal `zfsctl`. zfsctl only ever releases its own tag; other tags are shown and left alone.

**Why `zfs hold`.** It is ZFS's own mechanism, enforced by the kernel, so it protects even if zfsctl is not running,
and it blocks `zfs destroy` on the snapshot, a recursive destroy of its dataset, and a rollback past it.

### Slice 2: timeline of changes

**What the user sees.** The Snapshots list gains **Written** (bytes since the previous snapshot, from the `written`
property: instant and cheap) so a big drop or spike stands out. `c` ("changes") on a snapshot runs a diff from the
**previous** snapshot of the same dataset to the highlighted one and opens a read-only view:

- A header: `tank/data@b vs @a: 2,505 removed, 12 added, 31 modified, 0 renamed` and the change time range.
- Groups by folder at a depth the user can change with `[`/`]` (default 2 levels below the dataset root), each with its
  counts and the types involved: `Camera/ -2,498 +0 ~3 (files)`.
- Enter on a group lists its entries: change, type (in words: file, directory, symlink, ...), change time, path.
  `zfs diff` does not give size or creation time; slice 3 does.
- `C` runs the same view against the **live dataset** instead of the previous snapshot ("what has changed since this snapshot"): `zfs diff <snapshot> <dataset>` (my call).
- `Esc` backs out. A key on the view holds the *older* snapshot (the one that still has the files), the moment the
  user described.

**Limits.** A diff can be huge. It runs on a worker thread, streams lines, keeps counts per group and only a bounded
number of entries (default 5,000 shown per group, 2,000,000 lines read in total); at the limit it stops, says "stopped
after N changes", and still shows what it has. It can be cancelled with `Esc` at any time and has a time limit
(default 120 seconds). The result is held in memory only. Nothing is written to disk.

**Commands.** `zfs diff -FHt <older> <newer>` (privileged, already allowed). Output decoding is strict: a line that
does not match the shape is counted as "unreadable" and shown in the header, never guessed at. Octal escapes in paths
(`\040`) are decoded as ZFS prints them; a path is only ever shown, never executed or passed to a shell.

**Verification before it is built.** Run the exact command on FreeBSD (hub scratch pool), OmniOS (verified read-only)
and Linux. Linux needs a throwaway snapshot on the Ubuntu test VM: ask the user before creating one.

### Slice 3: browse a snapshot's folders

**What the user sees.** From the Snapshots section, `f` (files) on a snapshot opens a folder browser of that snapshot. (`b` is Rollback there already.) Each
entry shows: name, **type** (file, directory, symlink, ...), **size**, **created** (when the OS records it), **modified**,
**changed**, mode and owner. `/` filters by name; separate keys filter by date range, size range and type. Folders show
their entry count, not a recursive size (a recursive size means walking the tree).

**The mechanism is a decision (see Open questions).** Metadata for a snapshot's files needs a root read on the host,
and there is no portable single command. The proposed default:

- A new sudoers feature **`browse`** (off by default; `print-sudoers --with browse` writes it), granting **metadata
  only**: a single pinned listing helper per OS, not a general `ls` or `cat`.
- The listing reads `<dataset mountpoint>/.zfs/snapshot/<snapshot>/<relative path>` when that exists. When it does not
  (the OmniOS zone case), the feature reports "this snapshot cannot be browsed here" and says why; it does not fall
  back to cloning, because that changes the pool.
- Every path is validated in zfsctl **before** it is sent: it must be relative, must have no `..`, no empty or `.`
  component, no NUL or newline, and is joined onto a prefix zfsctl builds. Names that come back from the host are
  displayed, never executed or interpolated.
- Symlinks are shown as symlinks and never followed.

### Slice 4: restore as a copy

**What the user sees.** `R` on a file in the browser (or in a slice 2 entry list) offers **restore as a copy**. The
destination defaults to the file's own folder with a suffix (`photo.jpg` becomes `photo.restored-<snapshot>.jpg`) and
can be edited. The confirm is two keys and shows the **exact command**, the source snapshot path and the destination.

- **Never overwrites.** If the destination exists, the restore is refused, not offered a "replace".
- **Never restores a folder tree in the first version.** One file at a time; a folder is a later decision.
- After the copy, zfsctl checks the destination's size and mode against the source and says so, or says it could not.
- A new sudoers feature **`restore`** (off by default) with a single pinned copy command per OS that preserves
  times and mode and refuses to overwrite.

## Security

- Slices 1 and 2 add **no privilege** to any host (`zfs` is already allowed in full). They add *capability* inside what
  is already allowed. A hold only makes data safer.
- Slices 3 and 4 add privilege and are therefore **separate sudoers features, default off**, each documented in the
  book's sudoers chapter and listed by `print-sudoers` and `check-host` (which already follow the registry).
- Path hardening as above; no shell interpolation (commands are built as argument lists and quoted by the existing
  `shell_quote`); a snapshot name or dataset name from the host is validated against ZFS's name rules before use.
- Nothing displayed is trusted: names from a host may contain control characters or escape sequences; every string is
  sanitised for the terminal before drawing.
- A root-capable zfsctl hub is the trust boundary, as today. Holds make a compromised hub *less* able to destroy data
  unnoticed, not more.

## Where it lives

- **Code:** `src/zfs/snapshot.rs` and a new `src/zfs/snapshot_recovery.rs` for parsing (`userrefs`, `holds`, `diff`),
  a pure grouping module for the timeline (no I/O), views in `src/ui/` next to the Snapshots section, hints in
  `src/ui/hints.rs`. Hold checks go in the one place all destroy paths share, so no path can forget them.
- **Jobs:** the pruning code in `src/daemon/snapshot.rs`, `backup.rs`, `mirror.rs`, `replicate.rs` asks the same check.
- **Book:** chapter 10 (Snapshots) gains "Protecting a snapshot" and "What changed"; the keys reference, glossary
  ("hold") and the sudoers chapters follow; the book's tests stay green.
- **Order of delivery:** slice 1, then 2, then 3 and 4 only after the open questions are answered.

## Testing

- Unit tests with the existing `FakeRunner` for every command line (options first, quoting, the exact tag), for each
  parser (including garbage and hostile lines), the grouping, and the limits.
- Property-style tests for path validation (the hostile list: `..`, absolute, NUL, newline, `.`, empty, very long).
- A mutation pass on the destroy guards: removing the hold check from any destroy path must fail a test.
- Live checks on the hub (FreeBSD, real scratch pool, throwaway snapshots), OmniOS (read-only), Ubuntu (ask first).
- Book captures taken with the capture tool, as for every other screen.

## Open questions (for the user)

1. **Slice 3 mechanism.** Is a root-read metadata listing helper behind an off-by-default `browse` feature acceptable,
   given it can list names of files anywhere on the host? Alternative: skip browsing and rely on slice 2's entries,
   then restore by path from those. My default is to build the gated version, after slices 1 and 2.
2. **Folder restore** (slice 4) is out of the first version; confirm that is right.
3. **Tag name.** `zfsctl` is proposed. A different name changes nothing else.

## Left to the builder

Key letters (the ones above were checked against the Snapshots hint bar, which uses `d`, `b`, `r`; re-check against the final bar), column widths, wording of messages, default
grouping depth and the exact numeric limits (the figures above are starting points), the names of the new modules, and
how the timeline view is drawn within the existing layout.
