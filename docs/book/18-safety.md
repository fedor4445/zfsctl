# Safety

## What you will learn

- What zfsctl does to protect your data, and what it cannot protect you from.
- The few habits that matter most.

## What zfsctl does for you

- **Two keys for anything destructive or hard to undo** (destroy, rollback, new pool, new vdev, replace disk, revoke, shrinking a volume, pool features). Smaller changes, such as setting a property, saving a share or creating a snapshot, apply when you submit the form. A question is answered with `y` or `n` and then `Enter`; `Enter` alone does nothing, and a held key cannot answer twice.
- **Typed names for the worst actions.** Destroying a pool, a dataset, a snapshot or a boot environment, and rolling back, make you type the name.
- **The exact command is shown** before a form that can destroy something runs.
- **Refusals instead of broad actions.** Recursive destroy refuses a pool's root; nothing destroys the running boot environment; a backup job never creates a repository; a replication never touches the source.
- **Narrow privileges.** The programs allowed on a remote host are named in its sudoers entry; `zfsctl check-host` warns about `TOO BROAD`.
- **No secrets stored.** Jobs hold the path of a password file, not the password; the repository string may not contain credentials.
- **Your own files stay yours.** `config.toml` saves change only their own section; the Samba block is the only part of `smb4.conf` zfsctl touches.
- **No reboots.** zfsctl never reboots a host.

## What it cannot do

- It cannot undo a destroy, a rollback or an overwritten disk: ZFS cannot either.
- It cannot save you from a pool with no redundancy.
- It cannot make a copy on the same pool a backup.

## Habits that matter

1. **Redundancy.** Mirror or raidz for anything you care about.
2. **Scrub monthly**, and read the alerts.
3. **Snapshot often**, destroy rarely.
4. **Follow 3-2-1**: three copies, two kinds of storage, one off-site (restic, chapter 15).
5. **Test a restore.** Once.
6. **Check serial numbers** before touching a disk.
7. **Keep pools below 80%.**
8. **Keep the passwords** for your repositories somewhere safe, apart from the machine.

> **Safety:** root on the hub controls every host in the lab: guard the hub's ssh keys, and do not enable ssh login as root on any machine; zfsctl never needs it (chapter 5).

## Try it

Make a snapshot, then try to destroy it and press `Enter` without typing the name: nothing happens. Then try `y` then `n`. You will see that a slip does not destroy anything.

## Where next

- [Troubleshooting](19-troubleshooting.md)
- [Reference: commands](22-reference-commands.md)
