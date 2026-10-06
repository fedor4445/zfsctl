# Shell-Out to a Host Shell Design

## Summary

A key (`!`) suspends the TUI and drops the operator into an interactive shell on the currently selected host, then restores the TUI when the shell exits. It is the first of four independent sub-projects planned together; the other three are specced separately.

## Context: the four planned sub-projects

1. **Shell-out terminal** (this spec).
2. **Scheduler daemon plus scrub scheduling.** A `zfsctl daemon` subcommand, hub-only, running as root, sharing a state directory with the TUI (no socket) and reaching other hosts over the existing SSH+sudo path.
3. **Snapshot schedules and ZFS replication**, with retention, run by the daemon. The two ends of a `zfs send | zfs receive` are two separate SSH processes piped together on the hub, so no `sh` entry is needed in any host's sudoers.
4. **File-level backups with restic** to local paths (external drives), S3, sftp and similar, reading from a ZFS snapshot for point-in-time consistency.

Decisions already made for 2-4 are recorded here only so this spec is read in context; they are not designed here. This spec depends on none of them and none of them depend on it.

## Goals

- One keypress from any list section or drilled view to a shell on the selected host and back, without losing the TUI's state.
- Remote shells run as the same **unprivileged** login user zfsctl already connects as. zfsctl gains no new path to root: a stolen SSH key stays scoped to what the host's pinned sudoers entry allows.
- Whatever the operator did in the shell (created a pool, destroyed a dataset) is reflected on return.

## Non-goals

- A root shell on remote hosts (`sudo -i`). It would need a broad `NOPASSWD: ALL` entry, undoing the pinned allowlist the README documents.
- An embedded terminal pane inside the TUI.
- Running a specific command from the TUI. This is an interactive shell only.
- Persisting shell history or session logging.

## Behavior

- **Key:** `!`, borrowed from vi and less ("shell escape"). It is unused in `handle_key` today.
- **Available:** in any list section and in a drilled pool, on the selected host. **Not available** while a modal or picker is open (those own the keyboard).
- **Local host:** runs `$SHELL`, falling back to `/bin/sh` when unset or empty. It inherits the environment of the user running zfsctl, which on the hub is root.
- **Remote host:** runs `ssh -t -- <ssh_target>`, an interactive login as the user named in `ssh_target`. No `BatchMode`, so an interactive password or passphrase prompt works. `-t` forces a tty even though no remote command is given. The `--` stops a host value starting with `-` being read as an option, matching `SshRunner`.
- **Banner:** before the shell starts, one line is printed: `zfsctl: shell on <host> — exit to return`.
- **On return:** the TUI redraws from a cleared screen and queues a refresh of the visible list section, because the shell may have changed ZFS state.

## Design

### Pending-flag pattern

`handle_key` only sets `app.pending_shell = true` for `!`, mirroring `pending_new_hosts` (`src/ui/state.rs`). Only the event loop (`src/ui/mod.rs`) owns the terminal, so only it can suspend and resume it. The key is ignored while `app.modal` or `app.picker` is `Some`, which the event loop's dispatch order already guarantees.

### Suspend and resume

In the event loop, after key handling, if `pending_shell` is set it is taken (reset to false) and the loop:

1. disables raw mode and leaves the alternate screen;
2. prints the banner;
3. builds the command with `shell_command(host)`, spawns it with inherited stdin/stdout/stderr, and waits for it;
4. re-enables raw mode, re-enters the alternate screen, and calls `terminal.clear()` so shell output never bleeds into the next frame;
5. queues a refresh of the visible list section (the same call the `r` key makes).

Steps 1 and 4 are done even if the spawn fails, so a failed spawn can never leave the operator's terminal in raw mode. The existing panic hook already restores the terminal and is unchanged.

### Ctrl-C while the child runs

With raw mode off, Ctrl-C is delivered to the whole foreground process group, which includes zfsctl, so it would kill the TUI along with the shell's foreground command. The crate has `unsafe_code = "forbid"`, and ignoring a signal by hand needs `unsafe`. The design adds the small `signal-hook` crate, whose safe API wraps that part, and ignores SIGINT and SIGQUIT only for the duration of the wait, restoring the previous behavior afterwards. This is the **one new dependency**. (With `ssh -t`, ssh puts the local tty in raw mode itself, but the guard is applied to both cases for simplicity.)

### Host model

`Host` (`src/hosts.rs`) holds a name and a runner and does not remember its `ssh_target`, which the shell needs. It gains `ssh_target: Option<String>`; `None` means the local host. Both construction sites fill it in: `HostInventory::from_config` from `HostConfig.ssh_target`, and `apply_new_host` (hosts added at runtime) from its `target` argument.

`shell_command(host: &Host) -> Command` is a pure function that builds the `Command` for local or remote. It is separate so it can be unit-tested without a terminal.

## Errors

- The shell cannot be spawned (`$SHELL` missing, `ssh` not installed): the TUI returns and shows the error in the status line.
- `ssh` exits with status **255** (connection or authentication failure): the TUI returns and shows `could not open a shell on <host>`.
- Any other exit, including a non-zero status left over from the shell's last command, is **not** an error and shows nothing.

## Testing

Unit tests, no terminal needed:

- `shell_command` for the local host uses `$SHELL`, and `/bin/sh` when it is unset or empty.
- `shell_command` for a remote host is `ssh -t -- <target>`, with no `BatchMode`, and a target beginning with `-` stays after the `--`.
- `!` sets `pending_shell` in a list section and in a drilled pool, and does nothing with a modal or a picker open.
- Both `Host` construction sites populate `ssh_target`.
- The hint bar advertises `! Shell` where the key works, using the existing hint-versus-keys drift test.

The suspend/resume itself cannot be unit-tested. It is verified live on the hub with the pexpect + pyte setup: press `!`, run a command, run `exit`, and confirm the TUI is back and redrawn; then the same on a remote host, and Ctrl-C inside the shell leaves the TUI running.

## Files touched

- `src/hosts.rs`: `ssh_target` on `Host`.
- `src/ui/state.rs`: `pending_shell`, the `!` key.
- `src/ui/mod.rs`: suspend/resume in the event loop, `shell_command`.
- `src/ui/hints.rs`: the `! Shell` hint.
- `Cargo.toml`: `signal-hook`.
- `README.md`: the key, and a note that the remote shell is unprivileged by design.
