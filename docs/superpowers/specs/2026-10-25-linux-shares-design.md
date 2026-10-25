# Creating and editing shares on Linux hosts

Date: 2026-10-25. Status: design, not approved yet.

## Why

On a FreeBSD host the Shares section can create, edit and remove NFS and Samba shares (`N` and `M` on a dataset, `Enter` and `d` in Shares). On a Linux host (Ubuntu 24 test VM `ub24`) the section is read-only: it lists shares and checks the services, nothing more. The user wants Linux to work the same way, with the same form, and wants it proven with real clients.

Success: on ub24, a dataset can be shared over NFS and over Samba from the same forms as on FreeBSD, a real client can mount both and read and write, and the shares can be edited and removed again, all without leaving zfsctl.

## Goals

- `N` (NFS) and `M` (Samba) on a dataset, and `Enter` / `d` in the Shares section, work on a Linux host, with the FreeBSD form fields (Networks, Hosts, Read-only, Root for NFS; name, user, password, read-only, guest for Samba).
- The same confirms as on FreeBSD (second keystroke, typed names where FreeBSD asks for them). No weaker path on Linux.
- Failure leaves the host as it was: the Samba file is restored when a write or reload fails, exactly as on FreeBSD.
- `print-sudoers` and `check-host` know the Linux shares commands, so a host can be prepared and checked.
- Proven live on ub24 with a real NFS mount and a real SMB mount, not just `zfs get`.

## Non-goals

- illumos stays read-only. It has its own native `sharesmb` and `sharenfs`; a separate piece of work.
- Installing `nfs-kernel-server` or `samba`, creating Linux users, opening the firewall. The user does these; zfsctl only says what is missing.
- Kerberos and `sec=` for NFS, Samba users beyond one valid user or guest, Samba domain features. Same limits as FreeBSD.
- Sharing through `zfs set sharesmb` on Linux. FreeBSD never used it for Samba (it does not work there); Linux uses the same managed block in the Samba config file.

## Anti-goals

- A share that the `zfs get` output shows but a client cannot mount. This has happened twice on FreeBSD; every claim of "works" here needs a client mount.
- Touching a Samba or exports line zfsctl did not write. Only the managed block and the `sharenfs` property are ever changed.
- Anything that widens sudo beyond the pinned commands below.

## How it works

### NFS

The share is the `sharenfs` property, as on FreeBSD, set with `zfs set` (already in the core sudoers set). OpenZFS on Linux hands the property value to `exportfs`, so the value uses Linux `exports(5)` options, not FreeBSD's.

The form fields keep their meaning:

| Field | FreeBSD value | Linux value (design guess, to verify live) |
|---|---|---|
| nothing set | `on` | `on` |
| Read-only | `-ro` | `ro` |
| Networks | one `-network CIDR` clause each | `rw=@CIDR` / `ro=@CIDR`, several joined with `:` |
| Hosts | bare host names | the same names in the same `rw=` / `ro=` list |
| Root | `-maproot=USER` | `no_root_squash` (Linux cannot map root to another user) |

The exact value for several networks and hosts, and for read-only with a network list, must be found on ub24 with a real mount before the code is written (assemble, then `exportfs -v` and a client mount). The parse side (reading an existing value back into the fields) is the inverse; a value using anything else (`sec=`, `crossmnt`, `async`...) is "not losslessly editable" and gets the same warning FreeBSD already gives.

Root field on Linux: my call, not the user's. Typing `root` means `no_root_squash`; any other name is refused with a message that Linux can only keep or drop root squashing. Empty means squash (the default).

### Samba

Same managed block as FreeBSD (`SmbShare`, `parse_managed`, `render_with_shares`, `validate_*` are reused unchanged), with three differences driven by the OS:

| | FreeBSD | Linux |
|---|---|---|
| config file | `/usr/local/etc/smb4.conf` | `/etc/samba/smb.conf` |
| reload | `service samba_server reload` | `systemctl reload smbd` |
| sudo probe file for `check-host` | `/etc/master.passwd` | `/etc/shadow` |

`smbpasswd -a -s USER` is the same. The Samba user must already exist as a Linux user; zfsctl does not create users (FreeBSD does not either). If `smbpasswd` fails because the user is unknown, the error text from the tool is shown and `smb.conf` is not changed, as on FreeBSD.

The config path and reload command come from the host's OS, passed into `set_share`, `remove_share` and `list_managed` (today they are constants). Everything else in those functions, including `write_or_restore` and `reload_or_restore`, stays as is.

### Gates

- `ui::support::shares_editable` becomes true for FreeBSD and Linux, false for illumos and an unread OS. The last gate in `dispatch_mutation` follows it.
- The Shares title text "read-only on Linux" goes away for Linux.
- Hints and the keys that check `shares_editable` need no change beyond the function.
- When the service is not installed or stopped, the form still opens and saves the share (the property and the file are correct either way), and the existing Shares service line says why a client cannot connect yet. My call: saving is not blocked, because installing the server later should make the already-written shares work.

### Sudoers

- `core` already covers `zfs set sharenfs`.
- Linux gets the `samba` feature as opt-in (`print-sudoers ub24 --with samba`): `cat` and `tee` pinned to `/etc/samba/smb.conf`, `smbpasswd`, and `systemctl reload smbd` pinned to exactly that. `systemctl show` stays the sudo-free read it is today.
- `select_for_os` for Linux must accept `samba` (today it only adds what `works_on(Linux)` allows) and the privileged entries must carry the OS's file path. How the entry table expresses a per-OS path and a privileged `systemctl` next to the unprivileged one is the builder's call.
- `check-host` reports the new entries like any other.

## Verification (the part that decides "done")

On ub24 with `nfs-kernel-server` and `samba` installed by the user, using a throwaway dataset on pool `ztest`:

1. NFS: create from `N` with no restriction, mount from this workstation, write a file, see it in the dataset. Then edit (read-only, a network, root), re-mount, confirm the change bites (write refused when read-only). Remove, confirm the mount is gone (`exportfs -v`, `showmount -e`).
2. Samba: create from `M` with a user and password, mount with `mount.cifs` or `smbclient`, write a file. Edit to read-only, guest, remove. Check `testparm` is happy after every step and that `/etc/samba/smb.conf` outside the managed block is byte-for-byte unchanged.
3. Failure path: put a deliberately bad line outside the block, confirm the reload failure restores the file and the error is shown.
4. FreeBSD hub regression: the existing Shares tests pass and a smoke check shows the FreeBSD forms unchanged.
5. `check-host ub24` with and without the samba sudoers set.

No mutating keys in any scripted smoke test except on the throwaway dataset on ub24, never on the hub's real datasets.

## Left to the builder

- The final Linux `sharenfs` strings (found live, see above) and the matching parser.
- Entry-table shape for the per-OS Samba path and the privileged `systemctl`.
- Test plan, within: pure tests for the Linux assemble and parse functions, a fake-runner test of `set_share` and `remove_share` per OS (path, reload command, restore on failure), support-gate tests, sudoers tests.
- Book chapter and README line for Linux shares; the "read-only on Linux" wording removed.

## Risks

- Ubuntu's stock `smb.conf` may have settings that make a plain share unusable (for example `usershare` or a missing `map to guest`); guest access may need one `[global]` line. If so, zfsctl reports it and does not edit `[global]`; the book says what to add.
- Samba shares also need filesystem permissions for the Samba user on the dataset's mount point. zfsctl does not change them; the book says so.
- A firewall (`ufw`) on the VM can block both services. Not zfsctl's to change.
