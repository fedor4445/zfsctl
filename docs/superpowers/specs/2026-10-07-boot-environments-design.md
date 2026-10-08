# Boot Environments (bectl) — Design

Date: 2026-10-07

## Goal

Manage FreeBSD boot environments from the TUI on any managed host: see them,
create one before an upgrade, activate one (permanently or for the next boot
only), rename, mount/unmount and destroy. Same SSH + `sudo -n` model as the
rest of the app.

Out of scope (YAGNI): `bectl jail`, `import`/`export`, `-r <beroot>` for
multiple BE roots, a diff view, performing a reboot.

## Verified bectl behavior (FreeBSD 15.1-RELEASE-p4, probed 2026-10-07)

- `bectl list -H`: one tab-separated row per BE, five columns: name, active
  flags, mountpoint, space, created (`default  NR  /  5.18G  2026-07-24 18:48`).
  Unmounted BEs show `-` as mountpoint; space can be `8K` or `0`.
- Active flags: `N` (active now), `R` (active on reboot), `NR`, `T`
  (temporary, next boot only), `-`.
- `bectl list -a -H` prints a multi-line per-dataset form; it is NOT used.
- `activate <be>` is permanent; `activate -t <be>` sets the temporary flag;
  `activate -T <be>` clears it ("Temporary activation removed").
- `create <be>`; `create -e <existing> <be>` clones from a BE. A duplicate
  name exits 2 ("boot environment name already taken"); a name with spaces
  exits 1.
- `rename <old> <new>`; `mount <be>` prints a random `/tmp/be_mount.XXXX`
  path; `umount <be>`; `destroy [-o] <be>` (`-o` also removes the origin
  snapshot). Destroying the active BE exits 5 ("cannot destroy active boot
  environment").
- bectl is NOT in the managed VMs' sudoers (`sudo -n bectl` → "a password is
  required"), so a sudoers entry is part of this feature.

## Approach

A dedicated "Boot Envs" section that parses `bectl list -H` and uses bectl for
every mutation. bectl stays the authority for the rules that matter
(activation, active-BE protection). Rejected: deriving BEs from
`zfs list */ROOT` (would re-implement `bootfs`/`canmount` activation logic),
and a Datasets sub-tab (BEs are a host-level concept).

## UI

- New `Section::BootEnvs` ("Boot Envs"), after Shares in the sidebar.
- Table columns: Name, Active (decoded: Now, Reboot, Now+Reboot, Temp),
  Mountpoint, Space, Created. Columns drop on narrow terminals like Disks.
- Keys: `n` create (name field; F2 picker to optionally clone from an existing
  BE), `a` activate permanently (confirm), `t` activate for next boot only (no
  confirm), `T` clear temporary flag, `R` rename (`r` is the global refresh),
  `m` mount (the path appears in the Mountpoint column after the re-list), `u`
  unmount, `d` destroy and `D` destroy + origin snapshot (`-o`), both with a
  type-the-name confirm.
- Permanent activate and destroy always confirm. Temporary activation does not
  (it reverts by itself).
- Destroying a BE that has `N` or `R` set is refused up front in the UI with
  the reason; bectl is the backstop.
- `t` is not offered (and does nothing) on a BE that is already running or already marked `T`.
- Destroying a mounted BE is left to bectl; its own error is shown verbatim.

## Module and data flow

New `src/zfs/bootenv.rs`, pure parsing separated from I/O (as in `smb.rs`):

- `BootEnv { name, flags: ActiveFlags { now, reboot, temp }, mountpoint:
  Option<String>, space: String, created: String }`.
- `parse_bootenvs(&str) -> Result<Vec<BootEnv>>`. A malformed row fails the
  whole fetch (a silently skipped row is the ACL-position bug class).
- I/O via `CommandRunner` with argv only, never `sh -c` (must work under
  `sudo -n`): `list`, `create(name, from)`, `activate(name, Permanent |
  Temporary)`, `clear_temporary`, `rename`, `mount` (returns path),
  `unmount`, `destroy(name, origin)`.
- Availability: `bectl list -H` failing (non-zero or command not found) gives
  an Unavailable state showing bectl's message in the pane (like
  `samba_available`). Success with zero rows shows "no boot environments".
- State: `bootenvs`, `bootenvs_table`, wired into the same refresh paths as
  Shares (including the 10 s periodic refresh). The periodic refresh must NOT
  be gated on already-detected state (chicken-and-egg: a BE-less first load
  would never recover).
- Name validation client-side (non-empty, no spaces, no `/`, not already
  taken); bectl's error text is still surfaced verbatim as the final
  authority. Every mutation is followed by a re-list; no partial-state
  assumptions.

## Sudoers, docs, errors

- Add `bectl` to the sudoers allowlist (not argument-pinned, like `zfs` and
  `zpool`; it is as broad as they are: `bectl jail`/`mount`/`activate` amount to root on the host). Derive the full
  allowlist from the codebase, not memory, then update the README and the
  three managed VMs.
- README gets a Boot Environments section and the sudoers line.
- Mutation failures use the existing error modal with bectl's stderr; no
  retries.

## Testing

- Unit: parsing (real 15.1 `-H` rows; flag combos `NR`, `T`, `-`, `N`, `R`;
  `-` mountpoint; `0`/`8K` space; malformed rows; empty output), name
  validation, argv per operation via `FakeRunner`, UI state tests for key
  handling, the active-BE destroy refusal and the confirm flows.
- Live (hub, driven through the real TUI with pexpect+pyte): create, clone
  from `default`, rename, `t`, `T`, mount/unmount, destroy with `-o`; confirm
  each with `bectl list` and that `zfs list` shows the datasets and origin
  snapshots really appear/disappear. Verify the sudoers path on a VM, and the
  Unavailable state on a host where bectl fails.
- Not verifiable by us: an actual reboot into a permanent or temporary
  activation. Only the `bootfs`/flags are checked; a real reboot test is left
  to the user, ideally on a VM.

## Live results on the VMs (2026-10-08)

After `/sbin/bectl` was added to each VM's sudoers (`/usr/local/etc/sudoers.d/zfsctl-bectl`, installed by the user as root), the hub's TUI drove `fbsd16t1` (FreeBSD 15.1) and `fbsd16` (16.0-CURRENT) over SSH + `sudo -n`, checking every step against `bectl list -H` and `zpool get bootfs zroot` on the VM:

- create, clone from another BE with F2 (origin snapshot confirmed with `zfs get origin`), rename, `t` (flag `T`) and `T`, mount (`/tmp/be_mount.*` shown) and unmount, destroy with `D` (no snapshots left under `zroot/ROOT`).
- **Permanent activation** (never tested before): the confirm prompt appeared; after `y`, `default` read `N`, the new BE `R`, and `bootfs` pointed at it; activating `default` again restored `NR` and the original `bootfs`. No reboot was performed, so booting into an activated BE is still untested.
- 15.1 and 16.0-CURRENT behaved identically. One empty listing seen right after an activation on 16.0-CURRENT came from the test script's own SSH hop: a direct `bectl list -H` immediately and 5 seconds after activating listed both BEs.
- Both VMs were left with only `default` (`NR`) and the original `bootfs`.

### Reboot into an activated BE (2026-10-08)

On `fbsd16t2` (15.1) the user created a BE named `new` in the TUI, activated it (permanent `a`), and rebooted. The VM came up in it: the TUI showed `new` as `Now+Reboot` with `default` inactive (space 808K, no mountpoint), and on the VM `kenv` reported `currdev`, `vfs.root.mountfrom` and `zfs_be_active` all `zfs:zroot/ROOT/new`, `/` mounted from `zroot/ROOT/new` and `zpool get bootfs zroot` was `zroot/ROOT/new`.

### Temporary activation across reboots (2026-10-08)

On `fbsd16t2` with `new` permanent (`bootfs = zroot/ROOT/new`), the user pressed `t` on `default` (flag `T`, `bootfs` untouched) and rebooted twice:

1. First reboot: the VM came up in `default` through boot-once (`kenv zfs-bootonce = zfs:zroot/ROOT/default:`, `/` mounted from `zroot/ROOT/default`). The TUI showed `default` as `Now` and `new` still as `Reboot`; `bootfs` was still `zroot/ROOT/new` and the `T` flag was gone.
2. Second reboot: the VM returned to `new` by itself (`Now+Reboot`, `zfs-bootonce` empty, `/` from `zroot/ROOT/new`).

So `t` is one-shot, as designed: it does not promote the BE (the VM's `zfs_bootonce_activate` is the default `NO`). Together with the permanent activation above, every activation path of this section has now been verified by actually booting. `fbsd16t2` was left running `new`; `default` is inactive there.
