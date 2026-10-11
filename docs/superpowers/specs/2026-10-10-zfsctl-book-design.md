# The zfsctl Book — Design

Date: 2026-10-10

## Goal

A friendly, exact book that takes someone from "what is this?" to running zfsctl confidently on their own machines. Written for ZFS and FreeBSD users first, and openly welcoming to people coming from Linux or Windows ("they could be the next users"). It replaces the idea of a one-file "user manual"; the README stays the short reference and links to the book.

## Decisions made while brainstorming

1. **A folder of chapters** (`docs/book/`), one Markdown file per chapter, not one huge file. Plain Markdown: readable on GitHub or in a terminal; mdBook (or similar) could build a website from it later with no changes. No PDF or HTML is produced now.
2. **Two short primers inside the book** (about five pages each): ZFS in 15 minutes, and FreeBSD for newcomers. The FreeBSD Handbook is linked for depth, not relied on.
3. **Voice: "you", the operator running zfsctl**, with one small running example (a home lab: one hub, a couple of other machines) and plain advice for a first small setup.

## Audience and voice

- Assume nothing about FreeBSD or ZFS vocabulary; explain a term the first time it appears and collect all of them in a glossary.
- Friendly and precise: short paragraphs, task-first headings ("How do I…"), every chapter opens with what you will get out of it, and ends with where to go next.
- Never talk down. Never hide a risk: what a key does to your data is said before the key is shown.
- Where a Linux or Windows reader would trip, a short boxed note says what the familiar thing is called here (see Conventions).
- The freedom to choose your own tools ("pick your own poison": the mail command, the backup repository, the notification hook) is presented as a feature, with a few worked examples and no preaching.

## Structure

`docs/book/README.md` is the cover and table of contents ("The zfsctl Book"), with how to read it. Chapters, in order:

| File | Chapter | Main source of truth |
|---|---|---|
| `01-what-is-zfsctl.md` | What zfsctl is: the hub and its hosts, what it does and does not do, the safety promise | README, code layout |
| `02-zfs-in-15-minutes.md` | Pools, vdevs, datasets, snapshots, replication, scrub; Linux and Windows comparisons | OpenZFS docs knowledge, checked against what zfsctl shows |
| `03-freebsd-for-newcomers.md` | `pkg`, `/usr/local`, `rc.conf` and `sysrc`, `service`, device names, users, sudo, jails in one paragraph | FreeBSD knowledge, checked on the test boxes |
| `04-installing.md` | Getting zfsctl, first run, `config.toml`, the daemon and its rc script, state directory | README, `rc` script, live hub |
| `05-remote-hosts.md` | Adding a host, the unprivileged account, the sudoers entry, `print-sudoers` and `check-host`, why never root ssh | README, `exec/privilege.rs`, `exec/sudoers.rs` |
| `06-the-screen.md` | Sections, panes, the hint bar, themes and colours, Help, shell, quitting | `ui/layout.rs`, `ui/hints.rs`, Help text |
| `07-hosts.md` | The Hosts table, health and its colours, type, tag, the `/` filter, the host panel and graphs | `host_health.rs`, `ui/layout.rs`, `ui/metrics.rs` |
| `08-pools.md` | Pools: health, space, vdevs and disks, scrub, create, add vdev, replace, destroy, properties, the pool panel | `zfs/pool*.rs`, UI |
| `09-datasets.md` | Datasets: create, properties, permissions (`zfs allow`), ACLs, quotas | `zfs/dataset.rs`, `permission.rs`, `acl.rs` |
| `10-snapshots.md` | Snapshots: create, destroy, rollback, clone, holds, recursive | `zfs/snapshot.rs` |
| `11-disks.md` | Disks: what is listed, building a pool from selected disks, identifying a disk | `zfs/disk*.rs` |
| `12-shares.md` | Shares: NFS and SMB, the managed `smb4.conf` block, Samba passwords | `zfs/share*.rs`, `smb.rs` |
| `13-boot-environments.md` | Boot environments: why, create, activate, next-boot-only, rollback | `zfs/bootenv.rs` |
| `14-jobs.md` | Scheduled jobs: scrub, snapshot, replication (host to host, to a file), schedules and catch-up, history | `daemon/*`, `jobs.rs`, UI |
| `15-backups.md` | Backup jobs with restic: repositories (local, sftp, rclone, cloud), retention, restoring, "choose your own storage" | `daemon/backup.rs`, README |
| `16-alerts.md` | Alerts and the daily report: what is watched, the Alerts screen, mail command and hook, "choose your own way to send" | `daemon/monitor.rs`, `health*.rs`, `ui/alerts.rs`, README |
| `17-recipes.md` | Short how-tos that combine chapters (first pool, nightly snapshots, replicate to a second box, USB backup, cloud backup, restore a file, get alerted, replace a failed disk) | all |
| `18-safety.md` | What zfsctl will never do, two-key and typed confirmations, what runs where and as whom, data-safety habits | `exec/*`, UI confirm code |
| `19-troubleshooting.md` | Messages and what they mean, with the fix | error strings in code, live reproductions |
| `20-reference-keys.md` | Every key by section | Help text (`help_text()`), `ui/hints.rs` |
| `21-reference-config.md` | Every `config.toml` and `jobs.toml` key, the state directory files | `config.rs`, `jobs.rs` |
| `22-reference-commands.md` | Command line (`zfsctl`, `daemon`, `print-sudoers`, `check-host`, `notify-test`, `report`), the sudoers entry, exit codes | `main.rs`, `exec/sudoers.rs` |
| `23-glossary.md` | Every term, one or two lines each, with where it is explained | the book |

What is not built yet (bhyve guests, jails management, a plain rclone-sync job) is marked plainly where it comes up, never described as if it worked.

## Conventions

- **Callout boxes** are Markdown blockquotes starting with a bold label, so they work everywhere:
  - `> **Coming from Linux:** …` and `> **Coming from Windows:** …` bridges,
  - `> **Safety:** …` before anything that changes data,
  - `> **Not built yet:** …`,
  - `> **Tip:** …`.
- **Screens** are real text captures from the test hub in fenced blocks, taken at about 110 columns with the harness already used for testing, trimmed to the relevant rows and never edited by hand. Each capture says which build it came from in the index (one line per chapter).
- **Keys** are written in backticks (`n`, `Enter`, `Ctrl`…); a key that does something destructive is always followed by what it asks and what it changes.
- **Commands the reader types** are in `sh` blocks; commands that need root say so.
- **The running example** is a home lab: a hub called `hub`, a second machine `backup`, a pool `tank`; the captures come from the test boxes (`fb`, `fbsd16`…) and the text says so once, in chapter 1.
- Every chapter has the same shape: What you will learn, the body, Try it, Where next.

## Accuracy

1. Each chapter is written from the code, the in-app Help and the README, not from memory; the table above names the source for each.
2. **Drift tests** (new, `cargo test`): every key in `20-reference-keys.md` exists in `help_text()` and every key line of `help_text()` appears there; every key named in backticks in a chapter's "Keys" tables is a key `help_text()` or the hint bar knows; every `config.toml` key in `21-reference-config.md` is a field of `Config`/`NotifyConfig`/`HostConfig`/`Job` and every field is listed; every command in `22-reference-commands.md` is parsed by `parse_args`; every `crate::exec::privilege` entry is named in the sudoers part; every relative link between chapters resolves; every fenced capture is non-empty.
3. A live pass on the hub verifies each how-to that can be tried safely (read-only or with throwaway pools and files, never the real data), and the capture comes from that run.
4. A fresh reviewer reads the finished chapters against the code for accuracy before the book is merged.

## Non-goals (YAGNI)

PDF or HTML output, translations, screenshots as images, a video, documenting FreeBSD or ZFS beyond what a zfsctl user needs (links instead), marketing language, anything not built.

## Process

Chapters are written in groups (start-here and primers; getting going; the tour; doing things and safety; references and glossary), each group a commit on branch `book`, with the drift tests added alongside the reference chapters. The book is merged only on the user's word.
