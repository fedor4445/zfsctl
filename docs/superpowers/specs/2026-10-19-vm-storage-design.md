# VM storage: design

Date: 2026-10-19. Follow-up to the multi-OS work; the long-parked "bhyve" item, reshaped.

## Why

Many ZFS hosts also run virtual machines, and a VM's disks are ZFS zvols or datasets. Everything zfsctl does to a dataset
already works on those (snapshots, jobs, retention, replication, backup, encryption, space), but zfsctl does not know that
`tank/vm/web/disk0` is the disk of a VM, nor that the VM is running. Two accidents follow: destroying or rolling back
storage a running VM is using, and handing a physical disk that belongs to a pool to a guest.

## Decisions (user, 2026-10-19)

- **Identity:** zfsctl stays a ZFS/NAS utility for a vanilla OS. It does **not** create, start, stop or install VMs,
  and does not become an appliance. A user who would have to shell out for the installer is better served by their VM
  manager (`vm-bhyve`, libvirt, virt-manager, Proxmox). The VM manager stays the user's.
- **The lane:** the ZFS side of VMs: which storage belongs to which VM, snapshots that mean something, refusals that
  protect a running guest, and provisioning of VM disks.
- **Slice 1 (zvol for a VM):** create a zvol for a VM. zfsctl creates the zvol and **shows the one config line to add**;
  it does not attach it and does not edit the manager's files.
- **Slice 2 (physical disk for a VM), guarded:** a form that checks a physical disk and **refuses** (never overrides; a
  forced override is the sysadmin's job in a shell) if it is unsafe to hand to a guest. User decision 2026-10-19: at the
  end it **runs the manager's own attach command** after the two-key confirm, which shows the exact command (slice 1
  still only shows the line). zfsctl never edits a manager's files by hand.
- **Both Linux and illumos hosts matter** ("we should think of KVM and bhyve too"): discovery must not be bhyve-only. User
  decision 2026-10-19: **illumos (OmniOS bhyve zones) discovery is in the first slice**, beside `vm-bhyve` and libvirt.
- Proxmox and LXD (own zvol naming) are not in this slice; each would be another small reader later.

## Out of scope (anti-goals)

- Create, start, stop, console, install, delete of VMs; any hand-edit of a manager's configuration; PCI/GPU
  passthrough; a VM installer screen. zfsctl never wipes, partitions or formats a disk.
- Anything that would let a user override a refusal from inside zfsctl.
- Detaching or removing a disk from a VM.

## Parts

### A. Discovery (read-only)

A `VMs` view lists each VM on the selected host with: name, manager, running state, and the zvols/datasets/disks behind
it. Sources (my calls; **every command and file below is to be verified on a real install before it is coded**):

| Host | Manager | How zfsctl reads it |
|------|---------|---------------------|
| FreeBSD | `vm-bhyve` | `vm list` for names and state; each VM's `<name>.conf` (`diskN_type`, `diskN_name`, `diskN_dev`) under the datastore |
| Linux | libvirt (KVM) | `virsh list --all`, `virsh domblklist <vm> --details` |
| illumos | bhyve zones (OmniOS) | the zone's configuration (`zonecfg -z <zone> info`, or `zadm show` where installed) and `zoneadm list -cv` for state |

- The result maps a **dataset name -> VM name + running?**. Without a manager installed the section says so and the rest of
  zfsctl is unchanged. **Fail closed:** if zfsctl cannot ask the manager (command missing, sudo refuses, ssh drops), it
  does not assume "no VM uses this"; the refusals in B then say "could not check whether a VM uses it" and refuse the
  destructive action. Same principle as the encryption work.
- New privileged read commands must be added to `print-sudoers` for the hosts that use them, narrowly (read-only
  subcommands only).

### B. Refusals and labels on existing screens

- Destroy, `D` recursive destroy, rollback, and a shrink of a zvol that a **running** VM uses are refused, naming the VM.
  A **stopped** VM's zvol still gets the normal confirms, with one added line naming the VM.
- Dataset list and detail show the VM name next to a zvol it belongs to.
- Snapshot of a zvol used by a VM: the confirm says whether the VM is running ("crash-consistent only") or stopped.
- Optional pre/post hooks for snapshot jobs of a VM's disks use the existing hook mechanism (`ZFSCTL_*` env); defined in
  the plan, not a new subsystem. A job that snapshots all of a VM's disks does so in one command so the set is consistent.

### C. Slice 1: create a zvol for a VM

A form (key `n` on the VMs view, or on a pool's datasets): VM name, disk number, size, pool/parent.

- Name: `<pool>/vm/<name>/disk<N>` (my call; matches the `vm-bhyve` layout; configurable parent). Existing names are
  refused.
- Options shown with defaults and the exact command: `volblocksize` (default 16K, my call), sparse or reserved (default
  reserved, my call: sparse is opt-in because it overcommits), `compression=on` (inherited unless changed), no dedup,
  optional encryption using the existing passphrase handling (`create_encrypted`-style, passphrase on stdin only).
- **Thin-provisioning check:** when sparse, if the sum of the pool's sparse zvol sizes exceeds the pool's free space, the
  confirm states the overcommit and by how much. It warns; it does not refuse (my call).
- The confirm is the usual two-key one and shows `zfs create -V <size> ...`. After success the result screen shows the
  config line for the host's manager (e.g. a `vm-bhyve` `diskN_*` stanza, or the libvirt `virsh attach-disk` command), as
  text to copy. Nothing else is changed.
- Growing a zvol after creation reuses the existing `volsize` edit and adds a note that the guest must be told to grow.

### D. Slice 2: guarded physical-disk form

A form: VM (from discovery), then a disk, picked with F2. The picker shows every disk of the host with a verdict; only
`free` ones can be chosen. Checks, all of which must pass:

1. not a member of any **imported** pool (from `zpool status` data zfsctl already has);
2. carries no ZFS label (so it is not an exported or old pool member); read with a read-only label read on the device
   (`zdb -l`/equivalent; to be verified per OS) and treated as **unsafe if the check cannot run**;
3. no mounted filesystem, no swap, not the boot or system disk, no partitions in use (existing Disks-screen data);
4. not already used by any VM (running or stopped) according to discovery A;
5. the target VM is stopped.

Refusal list shows the reason per disk, for example `member of pool tank (mirror-1)`, `ZFS label from pool "old"`,
`used by VM web (disk2)`. There is no override. A disk that passes ends in the usual confirm showing the exact command,
then zfsctl runs the manager's attach command for the stopped VM, persistent form only (per manager, to be verified:
`vm add` for `vm-bhyve`, `virsh attach-disk --config` for libvirt, `zonecfg` for a bhyve zone). The checks are re-run
immediately before the command (the disk may have changed since the picker). It never writes to the disk itself. The
result screen shows the command's output; a failure is shown whole and nothing is retried.
- The attach commands are privileged: `print-sudoers` gets one narrow entry per manager, and only for hosts that use it.
- Removing a disk from a VM stays with the manager.

### E. The VMs section (UI)

User decision 2026-10-19: a sidebar section **VMs**, between Boot Envs and Jobs. The left list shows each VM of the
selected host: name, manager (`bhyve`, `kvm`, `bhyve zone`), running state, and CPU/memory where the manager reports them.
The right pane has two tabs, switched with `[` / `]` like the Datasets detail pane: **Disks** (the VM's zvols, datasets
and physical disks, with size, used and encryption state; `n` creates a zvol for it, `a` assigns a physical disk) and
**Snapshots** (the VM's snapshot sets across all its disks, each marked running or stopped). The Datasets list and
detail show "used by VM <name>" on a zvol. Section and tab names, widths and key hints are the builder's to settle.

### F. Hosts section: which hypervisor a host runs on

User decision 2026-10-19: the Hosts section's Type column marks a host that is itself a virtual machine and says which
hypervisor it runs on (`kvm`, `bhyve`, and so on), "even if users know it from the OS". Today the health probe already reads
`kern.vm_guest` on FreeBSD and the table shows only a generic `vm` (`host_health.rs`: "kept for later"). Show the name
instead. Linux reads `systemd-detect-virt` where present; illumos has its own source (both to verify; unknown shows the
generic `vm` or nothing, never a guess). A VM listed in the VMs section (part A) is labelled with its manager's kind the
same way. The mark is information only and changes no behaviour.

## Constraints

- Rust-only; two-key confirms; typed names for irreversible actions; secrets only on stdin; no root SSH, sudoers only.
- Illumos strict getopt order for any new `zfs`/`zpool` call (a lint test already enforces it).
- Tests: every new check has a fake-runner test for each refusal and for the fail-closed path; a mutation run on the new
  files (the project standard).

## Testing and hardware

All current machines are KVM guests: **bhyve cannot start a VM on any of them** and the user does not want to fight nested
virtualization. So:

- Everything that reads ZFS (zvol creation, refusals, snapshots, the disk checks) is built and live-tested as usual.
- illumos: reading a zone's configuration does not need a running zone, so a bhyve-brand zone can be configured (not
  booted) on OmniOS and read; to verify before coding. Whether OmniOS's bhyve zones are driven by `zonecfg` alone or
  also by `zadm` is to verify.
- libvirt discovery can be built and checked on the Ubuntu VM with `virsh` read-only against a defined (not started)
  domain, if `libvirt` is installable there (to verify).
- `vm-bhyve` discovery is written against its documented files and command output, with fixtures, and marked **untested
  on real hardware** in the README's status section until it runs on bare metal. Slice 1's create and the whole of D are
  ZFS-side and are not blocked by this.

## Left to the builder

Module layout (a new `src/vm.rs` plus a `vm` view in `ui/`), the exact names of the new key and section, picker wording,
fixture formats, and how the dataset-to-VM map is cached and refreshed (not on every redraw).

## Open for the user

- (Decided 2026-10-19: the public release is held until this is built.)
