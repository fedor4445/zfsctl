# VM storage: design

Date: 2026-10-19; narrowed 2026-10-25 (zfsctl is FreeBSD-only; the Linux and illumos parts are gone for good).

## Why

Many FreeBSD ZFS hosts also run virtual machines under bhyve, and a VM's disks are ZFS zvols (or files on a dataset).
Everything zfsctl does to a dataset already works on those (snapshots, jobs, retention, replication, backup, encryption,
space), but zfsctl does not know that `tank/vm/web/disk0` is the disk of a VM, nor that the VM is running. Two accidents
follow: destroying or rolling back storage a running VM is using, and handing a physical disk that belongs to a pool to a
guest.

## Decisions (user)

- **Identity (2026-10-19):** zfsctl stays a ZFS/NAS utility for a vanilla FreeBSD. It does **not** create, start, stop or
  install VMs, and does not become an appliance. The VM manager (`vm-bhyve`) stays the user's.
- **The lane (2026-10-25):** the FreeBSD storage operations that affect the storage behind VMs, whatever the guest runs
  (Linux, FreeBSD, Windows): which storage belongs to which VM, snapshots that mean something, refusals that protect a
  running guest, provisioning of VM disks. Nothing is guest-specific, and nothing is for another host OS.
- **Manager:** `vm-bhyve` only. libvirt, OmniOS bhyve zones, Proxmox and LXD are not in scope.
- **Slice 1 (zvol for a VM):** zfsctl creates the zvol and **shows the one config line to add**; it does not attach it and
  does not edit the manager's files.
- **Slice 2 (physical disk for a VM), guarded:** a form that checks a physical disk and **refuses** (never overrides; a
  forced override is the sysadmin's job in a shell) if it is unsafe to hand to a guest. At the end it **runs the
  manager's own attach command** after the two-key confirm, which shows the exact command. zfsctl never edits a manager's
  files by hand.

## Out of scope (anti-goals)

- Create, start, stop, console, install, delete of VMs; any hand-edit of a manager's configuration; PCI/GPU
  passthrough; a VM installer screen. zfsctl never wipes, partitions or formats a disk.
- Anything that would let a user override a refusal from inside zfsctl.
- Detaching or removing a disk from a VM.
- Anything that depends on the guest OS (file systems inside the guest, guest agents, quiescing a guest).

## Parts

### A. Discovery (read-only, `vm-bhyve`)

A `VMs` view lists each VM on the selected host with: name, running state, and the zvols/datasets/disks behind it.
Sources (my calls; **every command and file below is to be verified on a real install before it is coded**): `vm list`
for names and state; each VM's `<name>.conf` (`diskN_type`, `diskN_name`, `diskN_dev`) under the datastore.

- The result maps a **dataset name -> VM name + running?**. Without `vm-bhyve` installed the section says so and the rest
  of zfsctl is unchanged. **Fail closed:** if zfsctl cannot ask the manager (command missing while its datastore exists,
  sudo refuses, ssh drops, output not understood), it does not assume "no VM uses this"; the refusals in B then say
  "could not check whether a VM uses it" and refuse the destructive action.
- New privileged read commands go into the sudoers registry (`print-sudoers`, `check-host`), narrowly (read-only
  subcommands only).

### B. Refusals and labels on existing screens

- Destroy, `D` recursive destroy, rollback, and a shrink of a zvol that a **running** VM uses are refused, naming the VM.
  A **stopped** VM's zvol still gets the normal confirms, with one added line naming the VM.
- Dataset list and detail show the VM name next to a zvol it belongs to.
- Snapshot of a zvol used by a VM: the confirm says whether the VM is running ("crash-consistent only") or stopped.
- A job that snapshots all of a VM's disks does so in one command so the set is consistent. Pre/post hooks for snapshot
  jobs use the existing hook mechanism (`ZFSCTL_*` env); no new subsystem.

### C. Slice 1: create a zvol for a VM

A form (key `n` on the VMs view): VM name, disk number, size, pool/parent.

- Name: `<pool>/vm/<name>/disk<N>` (my call; matches the `vm-bhyve` layout; configurable parent). Existing names are
  refused.
- Options shown with defaults and the exact command: `volblocksize` (default 16K, my call), sparse or reserved (default
  reserved, my call: sparse is opt-in because it overcommits), `compression=on` (inherited unless changed), no dedup,
  optional encryption using the existing passphrase handling (passphrase on stdin only).
- **Thin-provisioning check:** when sparse, if the sum of the pool's sparse zvol sizes exceeds the pool's free space, the
  confirm states the overcommit and by how much. It warns; it does not refuse (my call).
- The confirm is the usual two-key one and shows `zfs create -V <size> ...`. After success the result screen shows the
  `vm-bhyve` `diskN_*` stanza as text to copy. Nothing else is changed.
- Growing a zvol after creation reuses the existing `volsize` edit and adds a note that the guest must be told to grow.

### D. Slice 2: guarded physical-disk form

A form: VM (from discovery), then a disk, picked with F2. The picker shows every disk of the host with a verdict; only
`free` ones can be chosen. Checks, all of which must pass:

1. not a member of any **imported** pool (from `zpool status` data zfsctl already has);
2. carries no ZFS label (so it is not an exported or old pool member); read with a read-only `zdb -l` and treated as
   **unsafe if the check cannot run**;
3. no mounted filesystem, no swap, not the boot or system disk, no partitions in use (existing Disks-screen data);
4. not already used by any VM (running or stopped) according to discovery A;
5. the target VM is stopped.

Refusal list shows the reason per disk, for example `member of pool tank (mirror-1)`, `ZFS label from pool "old"`,
`used by VM web (disk2)`. There is no override. A disk that passes ends in the usual confirm showing the exact command,
then zfsctl runs the manager's attach command for the stopped VM, persistent form only (`vm add` for `vm-bhyve`, form to be
verified). The checks are re-run immediately before the command. It never writes to the disk itself. The result screen
shows the command's output; a failure is shown whole and nothing is retried. The attach command is privileged:
`print-sudoers` gets one narrow entry. Removing a disk from a VM stays with the manager.

### E. The VMs section (UI)

A sidebar section **VMs**, between Boot Envs and Jobs. The left list shows each VM of the selected host: name, running
state. The right pane has two tabs, switched with `[` / `]` like the Datasets detail pane: **Disks** (the VM's zvols,
datasets and physical disks, with size, used and encryption state; `n` creates a zvol for it, `a` assigns a physical disk)
and **Snapshots** (the VM's snapshot sets across all its disks, each marked running or stopped). Section and tab names,
widths and key hints are the builder's to settle.

### F. Hosts section: which hypervisor a host runs on

The Hosts section's Type column names the hypervisor a host is itself a guest of (`kvm`, `bhyve`, and so on) instead of the
generic `vm`. The health probe already reads `kern.vm_guest` (`host_health.rs`: "kept for later"). Unknown shows the
generic `vm` or nothing, never a guess. Information only; changes no behaviour.

## What the book must say (conceptual accuracy)

- A snapshot of a running VM's zvol is **crash-consistent**, like pulling the plug: the guest's file systems recover, but
  a database inside the guest may need its own dump or a stopped guest for an application-consistent copy.
- Replication is the copy for zvols. restic reads files, so it applies to file-backed disk images, not to a zvol.
- A VM's disk grown on the host needs the guest told to grow too.
- Every ZFS claim cites a man page; vm-bhyve claims cite its documentation or are marked as verified on the test box.

## Constraints

- Rust-only; two-key confirms; typed names for irreversible actions; secrets only on stdin; no root SSH, sudoers only.
- Tests: every new check has a fake-runner test for each refusal and for the fail-closed path; a mutation run on the new
  files (the project standard).

## Testing and hardware (what needs metal)

Nothing here needs bare metal except one fact: what `vm list` prints for a **running** guest. The test machines are KVM
guests where bhyve cannot start a VM and nested virtualization is not wanted. So:

- Everything that reads or changes ZFS (zvol creation, refusals, snapshots, the disk checks, the sudoers entries) is built
  and live-tested as usual, on a scratch pool.
- `vm-bhyve` can be installed on a test VM and a VM created (not started): this verifies the datastore layout, the
  `.conf` format and `vm list` for a stopped guest, and gives real fixtures.
- The Running state is parsed from the manager's documented output and marked **untested on real hardware** in the README
  status until a bare-metal box runs a guest. Unknown output fails closed: a state that is not recognised counts as
  running for the refusals.
- The refusal itself is unit-tested with a faked "running" VM, and cannot be shown live without one; say so.

## Left to the builder

Module layout (a new `src/vm/` plus a VMs view in `ui/`), the exact names of the new key and section, picker wording,
fixture formats, and how the dataset-to-VM map is cached and refreshed (not on every redraw).

## Open for the user

- The public release hold (decided 2026-10-19: held until this is built) is to be reconsidered; see the notes.
- The branch `vm-storage` (9 commits) predates the FreeBSD-only cut; its Linux and illumos code must not be merged.
