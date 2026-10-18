# Raw-send replication: design

Date: 2026-10-18. Follow-up to the encryption slice, which made replication refuse an encrypted source.

## Why

A plain `zfs send` of an encrypted dataset delivers it decrypted. `zfs send -w` (raw) sends the encrypted blocks as they
are: the destination holds an encrypted copy it cannot read without the key. That is the safe way to replicate encrypted
data to a host or file the owner does not fully trust. Today zfsctl refuses such jobs; this slice allows them with an
explicit option.

## Decisions (user, 2026-10-18)

- **Scope:** host-to-host replication **and** replication to stream files. The cloud mirror (rclone) and restic backups
  are unchanged (mirror still refuses an encrypted source; restic re-encrypts with its own key).
- **Destination:** a raw replica is **locked** and is expected to stay so. It is not a problem anywhere in zfsctl.
- **Mixed trees:** the question does not arise: a replication job sends **one dataset** (`recursive` belongs to snapshot
  jobs), so there is no tree to split. A raw send of a plain dataset works (`-w` there equals `-Lec`) but is a different stream type; see the review notes below.

## Facts verified 2026-10-18 on the hub (FreeBSD)

- `zfs send -w ds@s | zfs receive -s -u dst` makes `dst` encrypted, `keystatus=unavailable`, `keylocation=prompt`, its own
  encryption root; loadable with the source's passphrase. Works for a child sent alone (it becomes its own root).
- `-w` on a plain dataset is accepted; the received dataset has `encryption=off`.
- Incremental raw (`-w -i @s1 ds@s2`) works. After `change-key` on the source, the next incremental carries the new key:
  the destination then loads with the **new** passphrase (the old one is refused).
- Resumable receive (`-s`, `send -t token`) keeps working; a resume token carries the raw flag.

## Behaviour

- The replication job gets a `raw` setting (`Raw (y/n)` field, default `n`; stored in `jobs.toml` as `raw = true`, absent
  when false). It applies to host and file targets.
- **Run time** (`guard`): an encrypted source is allowed only when the job is raw. Without raw it fails as today, and the
  message now says to switch on Raw. Raw jobs skip the encrypted check entirely. Locked source: a raw send of a locked
  dataset works in ZFS (no key needed), so it is allowed.
- **Save time** (`submit_job_form`): an encrypted source with Raw `n` is refused with the same message; with Raw `y` it
  saves. The mirror form keeps refusing.
- **send arguments:** `-w` goes before `-i` and before the `--`: `zfs send -w [-nvP] [-i @base] -- ds@new`; the dry run
  gets `-w` too. `send -t token` is unchanged.
- **Dashboard/report:** a locked dataset that only a raw job **writes** to is not reported (it is meant to stay locked).
  Reads and shares still count as today.
- **Docs:** book 14 (Jobs), 09 (encrypted datasets), 18 (Safety): what raw means, that the replica needs the source's
  passphrase to open, that after `C` on the source the replica's passphrase follows at the next run.

## Non-goals

Raw for the rclone mirror; sending a whole tree (`-R`); converting plain to encrypted in transit; key escrow for replicas.

## Left to the builder

Field position/wording in the form, message text.

## Review notes (2026-10-18)

- A raw job refuses to resume a resume token that a plain send made when the source is encrypted (`rawok` is decoded from
  `zfs send -nv -t`); the message names `zfs receive -A`.
- Raw and plain streams do not chain: the stream-file state records `raw` and starts a full stream when it differs; a host
  job gets a confirm when Raw changes on a saved job.
- Verified on FreeBSD only; Linux and illumos behaviour of raw receives and of key changes is not tested.
