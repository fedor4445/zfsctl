# Native encryption: design

Date: 2026-10-18. Status: draft for approval. Slice 4 of 4, in three stages that each ship on their own.

## Why

OpenZFS native encryption (`encryption`, `keyformat`, `keylocation`, `keystatus`, `encryptionroot`) lets a dataset be
encrypted per dataset. It can be set **only when the dataset is created**; after that the key can be loaded, unloaded and
changed. zfsctl lists the properties but cannot create an encrypted dataset, cannot load a key, and says nothing when a
dataset is locked. The user wants it as an option for admins. The risk is data loss through a lost key and through
replication that sends data in the clear, so this slice is deliberately staged and cautious.

## Stage A: visibility (read-only)

- The Datasets list gets a **Lock** column: blank for unencrypted, `open` (key loaded) or `LOCKED` (key unavailable). The
  panel shows `Encryption aes-256-gcm`, `Key passphrase`, `Root tank/secure` when the dataset is encrypted.
- Dashboard: a yellow line per host `tank/secure is locked` for an encrypted dataset whose key is not loaded **and that a
  job or share uses** (a job source/target or a shared dataset), because that is a thing that will fail; a plain locked
  dataset nobody uses is not a problem and gets no line. Source: `zfs get -H -p -t filesystem,volume
  keystatus,encryptionroot` in the daily-check pass, stored in the existing state file.

## Stage B: key handling

- On an encrypted dataset: `load-key` (a passphrase form, or the key from its `keylocation` when that is a file, with the
  file's existence checked first), `unload-key`, `change-key`. Locked datasets can then be mounted with the existing mount
  action. The passphrase is typed into a masked field and reaches `zfs load-key` through **stdin** (the runner already
  supports stdin); it is never put in an argument, a log, the status line or a state file, and the field is cleared on
  close. `unload-key` of a dataset in use is refused by ZFS; the message is shown.
- `change-key` and `unload-key` get the two-key confirm. `change-key` says that the old passphrase stops working for new
  data only after the wrapping key is rewritten, and warns to keep the new one safe.
- All via `zfs load-key/unload-key/change-key`; core sudoers; the three OSes (illumos: OmniOS has native encryption;
  the form hides what the host's `zfs` does not know).

## Stage C: create encrypted

- The New dataset form (`n`) gets: `Encryption` (off, `aes-256-gcm`, `aes-128-gcm`, `aes-256-ccm`, ... default
  `aes-256-gcm` when on), `Key format` (passphrase, raw, hex), and for a passphrase a masked "Passphrase" and "Repeat"
  (minimum 8 characters, both must match), for raw/hex a key file path that must already exist, mode 0400 and not be inside
  the dataset's own mountpoint. Children inherit the encryption root; a child of an encrypted dataset can only be created
  encrypted-by-inheritance, said in the form.
- A typed confirm (type the dataset name) states in plain words: **if you lose the passphrase or key file, the data cannot
  be recovered by anyone**, including zfsctl. Nothing is stored by zfsctl. `zfs create` receives the passphrase on stdin
  (twice, as `keyformat=passphrase` prompts).
- Properties that cannot change after creation are marked as such in the property tab.

## Jobs and replication (a separate guard, built with stage A)

Sending an encrypted dataset without `-w` (raw) delivers it decrypted (or fails). Until jobs have an explicit raw option,
the replication, backup and mirror job forms **refuse an encrypted source** with a clear message, and the daemon refuses at
run time the same way (a job whose source became encrypted since it was saved fails with that reason, red on the Dashboard,
never silently sends plaintext). An explicit "raw send (stays encrypted)" option for the host-to-host replication job is a
follow-up slice, not part of this one.

## Non-goals

- Encrypting an existing dataset in place (ZFS cannot; the way is create + replicate/rsync, which zfsctl does not automate).
- Key escrow, key files management, auto-load at boot, hardware tokens.
- Encrypted pool creation (`zpool create -O encryption=…`) in Create pool.
- Raw-send replication option (follow-up).

## Constraints

- Secrets: never in argv, logs, status, state files or panics; wiped from memory on close where the language allows
  (the existing `Secret` type is the pattern). Tests assert no secret appears in recorded calls' arguments.
- The two-key confirm and typed names throughout; errors from ZFS shown verbatim.
- Stages ship separately: A with the job guard first, then B, then C.

## Left to the builder

Column widths, the `Secret` reuse, how the host's supported cipher list is learned (version table), form layouts, wording,
tests (fake runner asserting stdin carries the passphrase and argv does not), the book (09 Datasets, 14 Jobs, 18 Safety),
README.

## Done means

Per stage: spec/plan, tests, whole-branch review, a live check on the hub and a VM (create an encrypted scratch dataset,
lock and unlock it, see the Dashboard line for a locked dataset used by a job, see the job guard refuse), merge, install.
