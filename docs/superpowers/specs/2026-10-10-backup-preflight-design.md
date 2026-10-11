# Backup Preflight Check — Design

Date: 2026-10-10

## Goal

Setting up a backup job needs things zfsctl does not create: restic and rclone on the host, an rclone remote in root's config, a password file, an initialised repository. When one is missing the first run fails with restic's raw text (for example `unable to open repository at <repository>: error talking HTTP to rclone`), which does not say what to do. A **preflight check** in the backup job form tells the user, before the first scheduled run, what is missing or wrong, in plain words with a next step.

## Scope

- A key in the backup job form (New and Edit): **F5**. It checks the repository and password file **as the job would use them**, on the form's host, and shows the result inside the form.
- Read-only. It creates and changes nothing, never runs `restic init`, and never reads or shows the password.
- Saving is never blocked by the check, and the check does not run automatically.

Non-goals (YAGNI): `restic init` or any repository creation, creating rclone remotes, ssh keys or password files, any new sudoers entry, an automatic check on save, a "run now" key, checks on the Jobs screen, checks for kinds other than backup.

## Decisions made while brainstorming

1. **Trigger:** an explicit key in the form (the user chose this over an automatic check on save, or both). A slow or unreachable target (a USB disk not plugged in, a hung SFTP host) can then neither freeze the TUI nor stop a job being saved.
2. **One restic call, not many checks.** `restic cat config --repo=<repo> --password-file=<file>` run through the host's existing privileged runner, exactly like the job's own restic calls. As root it exercises, in one go: restic installed and allowed by sudoers, rclone installed, the remote defined in root's rclone config, ssh login and host key for an sftp/rclone-sftp target, the password file existing and readable, the password being right, and the repository existing. Separate checks (`test -f`, `rclone listremotes`, …) would each need a new sudoers allowance; this needs none.
3. **No secrets.** The password file is passed by path to restic; zfsctl never opens it.
4. **Same masking as the daemon.** The repository text is replaced with `<repository>` in any message, as `daemon::backup::redact` does.

## Behaviour

On F5 in the backup form:

1. The form's Host, Repository and Password file fields are validated with the same rules the save uses (`restic::valid_repository`, `restic::valid_password_file`, known host). A bad field is reported as such and nothing is run.
2. The form shows `Checking …` and a worker thread runs the check. The form stays usable (typing, Esc, Tab).
3. The result replaces the status line in the form: a success line `OK: the repository opens`, or one finding as below, in the theme's error style when it is a problem.
4. Pressing F5 again re-runs it. Changing a field clears an old result (it no longer describes what is typed). Closing the form drops a pending result.
5. A fixed deadline (45 s) ends a hung check with `no answer from <host> in 45 s`. A late result for a closed form or a changed field is discarded.

### Findings

`classify` turns restic's exit status and text into one of these. Each message ends with a next step. The table lists the intent; the exact patterns are recorded from real failures on the hub before they are written (below), and anything not matched passes restic's own one-line error through (with the repository masked), never swallowed.

| Situation | Message |
|---|---|
| restic not found / not permitted by sudo | restic is not installed on `<host>`, or its sudoers entry lacks it (`zfsctl print-sudoers`) |
| password file missing or unreadable | no readable password file at `<path>` on `<host>` |
| wrong password | the password file does not unlock this repository |
| repository does not exist | no repository at that address yet: create it with `restic init` |
| rclone remote not defined | rclone remote `<name>` is not in root's rclone config on `<host>` |
| rclone not installed | rclone is not installed on `<host>` |
| connection failure (ssh login, host key, timeout) | restic's own error line, repository masked |
| anything else | restic's error line, repository masked |
| success | `OK: the repository opens` |

## Design

- `src/restic.rs`: `cat_config_args(repo, password_file) -> Vec<String>` beside `snapshots_args`/`backup_args` (same `--repo=`/`--password-file=` style, `--` not needed, no `--retry-lock`: it takes no lock), and `classify_preflight(success, stdout, stderr, repo) -> Preflight` (pure, unit-tested).
- `src/ui/state.rs`: F5 handled in the form key handler next to F2, only when `kind` is `FormKind::BackupJob`. Form state gains the check status (`Idle | Checking | Done(Preflight)`); `WorkerMsg::BackupPreflight(host index, repository, password_file, Preflight)` carries the result, and `drain_worker_messages` applies it only if the form is still open on that host with those two field values (the same stale-identity discard `Vdevs`/`PoolProperties` use). The event loop resolves the host's runner as for `fetch_pool_names`.
- `src/ui/layout.rs` / `modal.rs`: render the status line in the form; `hints.rs`: add `F5 Check` to the backup form's hint line.
- Docs: README backup section, book chapter 15 (a short "Check before you save" part with a screen capture, kept in step by the existing capture and drift tests), and the in-app Help text.

No change to the daemon, `Job`, the config file or the sudoers allowlist.

## Error handling

- Invalid fields: reported in the form, nothing runs.
- Worker failure to start a command (runner error): shown as `could not run restic: <error>`, masked.
- Timeout: reported as above; the worker may keep running until its process ends, which is harmless (read-only).
- A result arriving for a different host/field values or a closed form: discarded.

## Testing

- Unit: `cat_config_args`; `classify_preflight` for every row of the table using **recorded real restic output**, plus an unknown-text case that must pass through masked.
- Form tests with the fake `CommandRunner` (the existing pattern): F5 with bad fields runs nothing; F5 runs exactly one `restic cat config …`; result applied; stale result discarded after a field edit; F5 on non-backup forms does nothing; the hint line shows F5 only on the backup form.
- Live on the hub (`fb`, target t1), recording the real text before the patterns are written, then confirming each message: working repository; missing password file; wrong password; repository path that does not exist; rclone remote name not defined; a remote that cannot connect (wrong host/port, with the timeout path); rclone not found (by running with it hidden from `PATH`, or on a host without it). Live-only findings are noted in the project memory.
- Book drift tests and the README/book consistency tests still pass.

## Open points for the plan

- Exact restic wording for each failure: settled by the live recording, not by guessing.
- Whether `OK` should also show restic's repository version/id: shown if cheap to parse from `cat config` output, otherwise a plain `OK`.
- Whether the 45 s deadline needs to be configurable: assume not.
