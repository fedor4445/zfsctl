# print-sudoers and check-host — Design

Date: 2026-10-09

## Goal

Make the sudoers entry a managed host needs something zfsctl can generate and verify, instead of a hand-maintained list in the README. Twice a binary was missed (`geom`, `service`) and found only when a user touched the feature; `restic` was added to the README later by hand. The privilege model does not change: zfsctl still connects as an unprivileged user and runs every privileged command through `sudo -n`, authorised by a path-pinned `NOPASSWD` entry.

## Scope

- A **registry** of every command zfsctl runs on a managed host, with the feature that needs it and whether it needs privilege.
- `zfsctl print-sudoers <host>`: resolves the real paths on that host and prints the exact sudoers line, the drop-in file name and mode, and the `visudo` check to run first.
- `zfsctl check-host <host>`: asks the host's sudo what it allows and reports `ok`, `MISSING` or `TOO BROAD` per entry.
- Drift guards so a command cannot be added to the code without being registered.

Non-goals (YAGNI): writing or editing any sudoers file, auto-fixing a host, ad-hoc `user@host` targets (hosts come from `config.toml`), a daemon or any new privileged code, changing how commands are run at runtime, replacing the strings at the call sites with a typed enum.

## Decisions made while brainstorming

1. **Default feature set:** every feature except `restic`. `--with restic` adds it; `--without <feature>` removes one (`--without samba`). No inference from `config.toml` or the saved jobs, so the output depends only on the command line and the host.
2. **Registry shape:** a table in a new `src/exec/privilege.rs`; call sites keep their literal command strings. Rejected: a typed `Cmd` enum at all ~43 call sites (27 files and every fake-runner test touched, high regression risk for little extra safety) and a registry without a scan test (a missing entry would surface only on a real host, the original failure).
3. **No root SSH, no new privilege:** both subcommands connect over the normal unprivileged `ssh_target` and are read-only. The only privileged step stays with the operator: pasting the output into `visudo`.

## The registry

`src/exec/privilege.rs` holds a `const ENTRIES: &[Entry]`:

```
Entry { name: &str, feature: Feature, kind: Kind }
Kind::Privileged { pinned: &[&[&str]] }   // empty = any arguments, else exactly these argument lists
Kind::Unprivileged                        // runs without sudo; never in the sudoers line
```

Features and their privileged entries:

| Feature | Entries |
|---|---|
| `core` | `zpool`, `zfs` |
| `acl` | `getfacl`, `setfacl` |
| `disks` | `geom` |
| `bootenvs` | `bectl` |
| `shares` | `service` |
| `samba` | `smbpasswd`; `smbstatus -S`; `cat <CONF_PATH>`; `tee <CONF_PATH>`; `service` (shared with `shares`) |
| `restic` | `restic` |

`CONF_PATH` is `zfs::smb::CONF_PATH` (`/usr/local/etc/smb4.conf`). A command used by two features appears once in the output. Unprivileged entries (`getent`, `uname`, `date`, `hostname`, `sysctl`, `swapinfo`, `iostat`, `showmount`) are registered with `Kind::Unprivileged` and the feature they serve (`core` for the metrics panel, `shares` for `showmount`, and so on); they matter only to a host whose ssh key is locked to a forced command, so `print-sudoers` lists them in a trailing comment and never in the sudoers line.

`SshRunner` consults the registry before building the remote command. A command whose name is not registered is refused with `AppError::Config("<name> is not a registered remote command")` and nothing is sent. `LocalRunner` is unchanged: the hub runs as root and needs no sudoers entry. The check is on the bare command name; the pinned arguments are enforced by sudo on the host, not by zfsctl.

## `zfsctl print-sudoers <host> [--with F]... [--without F]...`

1. `<host>` must be a configured host with an `ssh_target` of the form `user@address`. The local host prints "the hub runs as root; there is nothing to configure" and exits 0. A target with no `user@` part is an error (the user would be guessed from the hub's own account).
2. The feature set is the default (all but `restic`), plus `--with`, minus `--without`; an unknown feature name is an error listing the valid ones.
3. Over the host's unprivileged ssh path it runs one `sh -c 'PATH="$PATH:/sbin:/usr/sbin:/usr/local/sbin:/usr/local/bin"; for c in "$@"; do command -v "$c"; done' sh <names...>` with the selected privileged command names and parses one absolute path per line. The extended `PATH` matters: a non-interactive ssh session often has only `/usr/bin:/bin`, which would hide every package-installed tool; sudo itself resolves from its own `secure_path`. A name that does not resolve is reported (`geom: not found on fbsd16`) and the command exits 1 without printing a line, unless that feature was not asked for.
4. **Path vetting:** each resolved path must start with `/bin/`, `/sbin/`, `/usr/bin/`, `/usr/sbin/`, `/usr/local/bin/` or `/usr/local/sbin/`. Anything else (a home directory, `/tmp`, a relative path) is an error naming the command and path, so a user-writable binary earlier in `PATH` can never become a sudoers entry.
5. Output, to stdout, nothing written to disk:
   - a comment header (host, date, the features included, a note that `zfs`, `bectl` and `restic` are as powerful as root);
   - `<user> ALL=(root) NOPASSWD: <path>, <path> <arg>, ...`, one logical line, each pinned entry as `<path> <args>` exactly;
   - the install steps as comments: the drop-in name `/usr/local/etc/sudoers.d/zfsctl` (the path FreeBSD's sudo includes), mode `0440`, and `visudo -c -f <file>` to run before trusting it;
   - the unprivileged commands in a comment.

## `zfsctl check-host <host> [--with F]... [--without F]...`

Same host and feature handling as above. For every selected privileged entry, over the unprivileged ssh path:

- `sudo -n -l <resolved path> [pinned args]` must exit 0 → `ok`; otherwise `MISSING`.
- For every pinned entry it also runs `sudo -n -l <resolved path> <a deliberately different argument>` (for `cat`/`tee`: `/etc/master.passwd`; for `smbstatus`: `-v`) and expects a refusal → `ok`; if sudo allows it, the entry is `TOO BROAD`.

One line per entry (`ok   zpool  /sbin/zpool`), then a summary. Exit 0 only if everything is `ok`; 1 if anything is `MISSING` or `TOO BROAD`; 2 if the host could not be reached (an ssh exit of 255 stops the run at once rather than timing out per entry).

## Drift guards

1. **Scan test:** reads every `.rs` file under `src/` (excluding the test modules and the registry itself), extracts each string literal passed as the first argument of `run`, `run_checked`, `run_with_stdin`, `command` or `run_unprivileged`, and fails naming any that the registry does not contain. (A command name built from a variable would escape the scan; none exist today, and the runtime refusal in `SshRunner` covers it.)
2. **README test:** every registered privileged name appears in the README's sudoers section, so the prose cannot lose one. The README keeps a short explanation and points to the two commands for the actual line.
3. **Runtime refusal** in `SshRunner` as above.

## Command line and wiring

`main.rs` gains `Command::PrintSudoers` and `Command::CheckHost` (host, with, without). The usage string becomes `zfsctl [daemon [--state-dir DIR] | print-sudoers HOST [--with F] [--without F] | check-host HOST [--with F] [--without F]]`. Both run after the existing root check and config load (the hub's root key is the identity that connects) and print through plain `println!`, with no TUI. Pure pieces live in `privilege.rs` (registry, feature selection, line rendering, path vetting, `command -v` parsing, `sudo -l` result interpretation); the two commands are thin functions taking a `&dyn CommandRunner`.

## Testing

- Unit: feature selection (default, `--with restic`, `--without samba`, unknown name); line rendering (shared `service` once, pinned entries exact, user and drop-in text); path vetting (each allowed prefix, rejects `/home/x/restic`, `/tmp/zfs`, relative); `command -v` parsing (missing name, blank lines); `sudo -l` interpretation (ok, missing, too broad).
- Fake-runner: `print-sudoers` with every name resolving; with one missing (exit 1, no line); `check-host` all ok, one `MISSING`, one `TOO BROAD`, ssh 255 (exit 2, one call).
- The three drift guards above.
- Live on `fbsd16`, t1, t2 (and the hub as the local-host message): the generated line equals the installed entry's binaries; `check-host` is `ok` everywhere (restic with `--with restic`); a host checked with a feature whose entry is absent shows `MISSING`; the `TOO BROAD` probe is exercised against a throwaway drop-in that allows plain `cat`, which the operator pastes and later removes as root on the VM (no root ssh is used or needed).
