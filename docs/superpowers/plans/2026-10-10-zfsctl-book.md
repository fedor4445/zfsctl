# The zfsctl Book Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Write "The zfsctl Book": 23 chapters in `docs/book/` for ZFS and FreeBSD users, friendly to people coming from Linux and Windows, accurate to the code, with real screen captures and tests that keep it from drifting.

**Architecture:** One Markdown file per chapter plus a cover/TOC. A capture tool drives the real TUI on the test hub and writes text captures that chapters embed unedited. A Rust test module (`src/book_check.rs`) checks the book's structure (TOC, chapter shape, links, callouts, captures) and, for the reference chapters, that keys, config fields and commands match the code.

**Tech Stack:** Markdown; Rust tests; the pexpect+pyte harness (Python venv) for captures; the test hub (`fb@172.16.10.90`, root via sudo) and its VMs.

**Spec:** `docs/superpowers/specs/2026-10-10-zfsctl-book-design.md`

## Global Constraints

- **Accuracy over completeness.** Every statement about behaviour comes from the code, the in-app Help (`help_text()` in `src/ui/state.rs`), the README, or something observed on the hub in this task. If it cannot be verified, it is left out or marked `> **Not built yet:**` (bhyve guests, jail management, a plain rclone-sync job).
- **Voice:** "you" the operator; short paragraphs; task-first headings; a term is explained the first time it appears and goes in the glossary; what a key does to your data is said before the key is shown. Never talk down; no marketing words.
- **Callouts** are blockquotes starting with one of these bold labels and nothing else: `**Coming from Linux:**`, `**Coming from Windows:**`, `**Safety:**`, `**Not built yet:**`, `**Tip:**`.
- **Chapter shape** (chapters 01–19): `# <Title>`, `## What you will learn`, the body, `## Try it`, `## Where next`. Reference chapters (20–23) have `# <Title>` and sections of their own. Every chapter file is named `NN-slug.md` as in the spec's table.
- **Screens** are fenced blocks tagged `screen`, taken from the real TUI at about 110 columns (`--cols 110`) with the capture tool, trimmed to the relevant rows by the tool's `--rows`/`--crop` options, never edited by hand; each is a file in `docs/book/captures/` that the chapter embeds verbatim (the test checks the embedded text equals the file).
- **The running example** is a home lab: a hub `hub`, a second machine `backup`, a pool `tank`. Captures come from the test boxes (`fb`, `fbsd16`, `fbsd16t1`, `fbsd16t2`) and chapter 01 says so once.
- **Nothing destructive on the hub.** Captures and how-tos on the hub are read-only or use throwaway pools on file-backed vdevs (`truncate -s` files) and datasets named `scratch/bookdemo*` that are destroyed afterwards; never touch `tank`/`tpool`/`zroot` data; never send mail except the one deliberate `notify-test` the user already approved is NOT repeated here (use `p` preview and `zfsctl report`).
- **Commits:** each task commits on branch `book` with the Co-Authored-By trailer. The branch is merged only when the user says so.
- Run `cargo test` and `cargo clippy` (0 warnings) before every commit that touches Rust.

## Review Focus

- A key, command, config key or file path named in the book that does not exist or behaves differently (the drift tests catch the common ones; the reviewer reads the rest against the code).
- Advice that could lose data (a recipe that skips a confirmation, an unsafe `zpool` habit, a restic or replication instruction that omits the check) and statements of safety that the code does not back (the "never" claims in chapter 18).
- Claims about Linux/Windows equivalents that are wrong or condescending.
- Features described that are not built, and built features that are missing from the book.
- A capture that is stale (the TUI changed after it was taken) or contains something private (an email address, a hostname the reader cannot use, a password).

---

### Task 1: Scaffolding, capture tool and structure tests

**Files:**
- Create: `docs/book/README.md`, `docs/book/tools/capture.py`, `docs/book/tools/README.md`, `docs/book/captures/.gitkeep`, `src/book_check.rs`
- Modify: `src/main.rs` (`#[cfg(test)] mod book_check;`)

**Interfaces:**
- Produces: the TOC in `docs/book/README.md` (a Markdown list of `[NN Title](NN-slug.md)` links, in order); `capture.py` CLI: `capture.py --name NAME [--cols 110] [--rows 30] [--keys KEYS] [--wait SECS] [--crop FIRST:LAST] [--section SECTION]` writes `docs/book/captures/NAME.txt`; `book_check` helpers `chapters() -> Vec<(String, String)>` (file name, text) and `toc_links() -> Vec<String>`.

- [ ] **Step 1: Write the failing tests** (`src/book_check.rs`; all read files under `CARGO_MANIFEST_DIR/docs/book`)

```rust
use std::path::{Path, PathBuf};

fn book_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs").join("book")
}

fn chapters() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(book_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() > 3 && n.as_bytes()[..2].iter().all(u8::is_ascii_digit) && n.ends_with(".md"))
        .map(|n| {
            let text = std::fs::read_to_string(book_dir().join(&n)).unwrap();
            (n, text)
        })
        .collect();
    v.sort();
    v
}

fn toc_links() -> Vec<String> {
    let readme = std::fs::read_to_string(book_dir().join("README.md")).unwrap();
    readme.lines().filter_map(|l| l.trim_start().strip_prefix("- [")).filter_map(|rest| rest.split_once("](")).filter_map(|(_, r)| r.split_once(')')).map(|(file, _)| file.to_string()).filter(|f| f.ends_with(".md")).collect()
}

#[test]
fn the_toc_lists_every_chapter_file_in_order_and_nothing_else() {
    let files: Vec<String> = chapters().into_iter().map(|(n, _)| n).collect();
    assert_eq!(toc_links(), files, "docs/book/README.md must list exactly the chapter files, in order");
}

#[test]
fn chapters_one_to_nineteen_have_the_standard_shape() {
    for (name, text) in chapters() {
        let number: u32 = name[..2].parse().unwrap();
        if number > 19 {
            continue;
        }
        let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("# ") || l.starts_with("## ")).collect();
        assert!(headings.first().is_some_and(|h| h.starts_with("# ")), "{name}: starts with a title");
        for want in ["## What you will learn", "## Try it", "## Where next"] {
            assert!(headings.contains(&want), "{name}: missing {want}");
        }
        let pos = |h: &str| headings.iter().position(|x| *x == h).unwrap();
        assert!(pos("## What you will learn") == 1 && pos("## Try it") < pos("## Where next"), "{name}: order");
        assert_eq!(*headings.last().unwrap(), "## Where next", "{name}: ends with Where next");
    }
}

#[test]
fn relative_links_resolve_to_a_file_and_an_existing_heading() {
    for (name, text) in chapters().into_iter().chain(std::iter::once(("README.md".to_string(), std::fs::read_to_string(book_dir().join("README.md")).unwrap()))) {
        let mut rest = text.as_str();
        while let Some(i) = rest.find("](") {
            let after = &rest[i + 2..];
            let Some(end) = after.find(')') else { break };
            let target = &after[..end];
            rest = &after[end..];
            if target.starts_with("http") || target.starts_with('#') || target.starts_with("mailto:") {
                continue;
            }
            let (file, anchor) = target.split_once('#').map_or((target, None), |(f, a)| (f, Some(a)));
            let path = book_dir().join(file);
            assert!(path.exists(), "{name}: link to {target} does not resolve");
            if let (Some(anchor), true) = (anchor, file.ends_with(".md")) {
                let body = std::fs::read_to_string(&path).unwrap();
                let slugs: Vec<String> = body.lines().filter(|l| l.starts_with('#')).map(|l| l.trim_start_matches('#').trim().to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect::<String>().replace(' ', "-")).collect();
                assert!(slugs.iter().any(|s| s == anchor), "{name}: {target}: no heading '{anchor}' in {file}");
            }
        }
    }
}

#[test]
fn callouts_use_only_the_known_labels() {
    const LABELS: [&str; 5] = ["Coming from Linux", "Coming from Windows", "Safety", "Not built yet", "Tip"];
    for (name, text) in chapters() {
        for (n, line) in text.lines().enumerate() {
            if let Some(rest) = line.strip_prefix("> **") {
                let label = rest.split_once(":**").map_or(rest, |(l, _)| l);
                assert!(LABELS.contains(&label), "{name}:{}: unknown callout label '{label}'", n + 1);
            }
        }
    }
}

#[test]
fn embedded_captures_are_non_empty_and_equal_their_files() {
    for (name, text) in chapters() {
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            if line.trim() == "```screen" {
                let mut body = Vec::new();
                for l in lines.by_ref() {
                    if l.trim() == "```" {
                        break;
                    }
                    body.push(l);
                }
                let body = body.join("\n");
                assert!(!body.trim().is_empty(), "{name}: an empty screen block");
                let captures = std::fs::read_dir(book_dir().join("captures")).unwrap().filter_map(|e| e.ok()).filter_map(|e| std::fs::read_to_string(e.path()).ok());
                assert!(captures.map(|c| c.trim_end().to_string()).any(|c| body.trim_end().contains(c.as_str())), "{name}: a screen block that matches no file in docs/book/captures (captures are never edited by hand)");
            }
        }
    }
}

#[test]
fn the_book_has_the_title_and_a_how_to_read_section() {
    let readme = std::fs::read_to_string(book_dir().join("README.md")).unwrap();
    assert!(readme.starts_with("# The zfsctl Book"), "{}", readme.lines().next().unwrap_or(""));
    for want in ["## Who this is for", "## How to read it", "## The running example", "## Chapters"] {
        assert!(readme.contains(want), "missing {want}");
    }
}
```

- [ ] **Step 2: Run to verify they fail** — add `#[cfg(test)] mod book_check;` to `src/main.rs`, then `cargo test book_check 2>&1 | tail` → the tests fail (no `docs/book/README.md` yet: a panic on `read_to_string`).

- [ ] **Step 3: Create the cover and the capture tool**
  - `docs/book/README.md`: `# The zfsctl Book`, then `## Who this is for` (ZFS and FreeBSD users first, welcoming to Linux and Windows people; two sentences each on what the book assumes and does not), `## How to read it` (the chapter shape, the callout labels with one example of each, "keys are in backticks", "screens are real captures"), `## The running example` (hub, backup, tank; captures from the test boxes), `## Chapters` (the TOC list; initially empty list is fine at this step but the tests require it equal the chapter files, so it grows with each task), and a closing line linking the project README.
  - `docs/book/tools/capture.py`: drives `ssh -tt <HUB> 'sudo <ZFSCTL_BIN>'` with pexpect, renders with pyte at `--cols`×`--rows`, sends `--keys` (a string where `<Tab>`, `<S-Tab>`, `<Enter>`, `<Esc>`, `<Down>`, `<Up>` are expanded), waits `--wait` seconds, optionally selects `--section NAME` by sending Tab until the sidebar highlight reads NAME, crops lines `FIRST:LAST`, strips trailing spaces and any line that contains an email address or the string `password`, and writes the text to `docs/book/captures/NAME.txt` with a final newline. Environment: `HUB` (default `fb@172.16.10.90`), `ZFSCTL_BIN` (default `./zfsctl/target/release/zfsctl`). `tools/README.md` documents setup (`python3 -m venv`, `pip install pexpect pyte`) and one example command.

- [ ] **Step 4: Run** — the structure tests that do not need chapters pass (`the_book_has_the_title_and_a_how_to_read_section`); the TOC test passes with an empty chapter list; the others pass vacuously. Run `cargo test 2>&1 | grep "test result"` and `cargo clippy`.

- [ ] **Step 5: Commit** — `docs: the book's cover, capture tool and structure tests`.

---

### Task 2: Start here and the primers (chapters 01–03)

**Files:**
- Create: `docs/book/01-what-is-zfsctl.md`, `02-zfs-in-15-minutes.md`, `03-freebsd-for-newcomers.md`; captures `docs/book/captures/01-hosts.txt`
- Modify: `docs/book/README.md` (TOC)

- [ ] **Step 1: Read the sources** — `README.md` (intro, Remote host setup, Daemon/State), `src/main.rs`, `src/ui/layout.rs` (sidebar, section names), `src/hosts.rs`, and observe on the hub: `ssh fb@172.16.10.90 'freebsd-version; zpool list; zfs list -t all -r tpool | head; service zfsctl status'` for the primer facts and the captures.
- [ ] **Step 2: Capture** — `capture.py --name 01-hosts --section Hosts --cols 110 --rows 14 --wait 12`.
- [ ] **Step 3: Write chapter 01** — the problem zfsctl solves; the hub-and-hosts picture (a diagram in a text block); what runs where (TUI as root on the hub, daemon for scheduled jobs, hosts reached over ssh as an unprivileged user with sudoers); what it does (the sections in one line each) and does not do (no bhyve/jails management, no root ssh); the safety promise in four bullets (confirmations, typed confirmations, refuses over-broad actions, never stores passwords); the running example and the note that captures come from test boxes; the capture above explained line by line. Callouts: one Linux and one Windows bridge ("Coming from Windows: think of it as Disk Management plus Task Scheduler plus a backup tool, for ZFS machines"; "Coming from Linux: it manages ZFS the way Cockpit or webmin might, but in your terminal").
- [ ] **Step 4: Write chapter 02** — ZFS in 15 minutes: why ZFS (checksums, copy-on-write, snapshots); pool, vdev (single, mirror, raidz1/2/3, log, cache, spare, special), dataset (filesystem, volume), snapshot, clone, replication (`send`/`receive`), scrub, resilver, properties (compression, quota, recordsize), ARC; a worked picture of tank = mirror of two disks with datasets `tank/home`, `tank/media`; Linux bridges (ext4/xfs+LVM+mdadm → one system; `lvcreate -s` vs snapshots; `rsync` vs `zfs send`), Windows bridges (Storage Spaces, ReFS, Volume Shadow Copy, File History → snapshots, `robocopy` → replication). Every `zpool`/`zfs` command shown is run on the hub (throwaway file-backed pool `zpool create bookdemo /tmp/bookdemo.img`, destroyed after) and its output pasted as a `sh`/text block; no `screen` captures here. Try it: create and destroy a file-backed pool by hand, with the exact commands.
- [ ] **Step 5: Write chapter 03** — FreeBSD for newcomers: base system vs ports/packages (`pkg install`, `/usr/local`), `/etc/rc.conf`, `sysrc`, `service NAME start|status`, rc.d scripts, where logs are, `sudo` vs `doas`, users and groups, device names (`da0`, `ada0`, `nda0` vs `sda`; `geom`), `kldload`, `freebsd-update`, jails in one paragraph (zfsctl does not manage them yet: `> **Not built yet:**`). Every command checked on the hub or a VM (`ssh fbsd@172.16.10.61 '…'` read-only commands only).
- [ ] **Step 6: TOC and tests** — add the three chapters to the TOC; `cargo test book_check` passes (shape, links, callouts, captures).
- [ ] **Step 7: Commit** — `docs: the book, part 1: what zfsctl is and the two primers`.

---

### Task 3: Getting going (chapters 04–05)

**Files:** Create `04-installing.md`, `05-remote-hosts.md`; captures `04-config.txt`, `05-check-host.txt`, `05-print-sudoers.txt`. Modify the TOC.

- [ ] **Step 1: Read** — README (install, config, daemon, sudoers sections), `src/exec/privilege.rs` (`ENTRIES`), `src/exec/sudoers.rs`, `src/main.rs`, the hub's rc script (`/usr/local/etc/rc.d/zfsctl`) and `sysrc zfsctl_enable`.
- [ ] **Step 2: Capture** — `sh`-style captures are plain text files made with `ssh` (not the TUI): `zfsctl check-host fbsd16` and `zfsctl print-sudoers fbsd16t1` output into `docs/book/captures/05-*.txt` (the tool has a `--cmd` mode for this: add it to `capture.py` with a test in `tools/` or a documented one-liner), and the redacted config (`sed` away anything private) into `04-config.txt`.
- [ ] **Step 3: Write chapter 04** — what you need (a FreeBSD box with ZFS, root, Rust or the binary); building and installing (`cargo build --release`, copy to `/usr/local/bin`); first run (`zfsctl` as root, what you see); the config file location and the `[[hosts]]` format; the rc script and the daemon (`sysrc zfsctl_enable=YES`, `service zfsctl start`), the state directory (`/var/db/zfsctl`: `jobs.toml`, `history.jsonl`, `daemon.json`, `notify.json`, `health-events.jsonl`, progress and cancel dirs), what the daemon logs and where; upgrading (the `.prev` copy habit). Callouts: systemd vs rc.d; Task Scheduler vs the daemon. Try it: start the TUI, press `?`, `q`.
- [ ] **Step 4: Write chapter 05** — the model: an unprivileged user per host plus a path-pinned sudoers entry; why never root ssh; setting up a remote host step by step (create user, authorise the hub's key, install the sudoers drop-in, test with `check-host`); `print-sudoers` (what it prints, its flags and exit codes) and `check-host` (ok, MISSING, TOO BROAD); adding the host in the TUI (`n` in Hosts, the optional tag) and by hand in `config.toml`; the table of the 12 privileged commands and what each is for (from `ENTRIES`); what a forced-command key needs; a short "what if it fails" list with real messages. Safety callouts on `restic` and `zfs` being as powerful as root.
- [ ] **Step 5: TOC and tests** — `cargo test book_check`.
- [ ] **Step 6: Commit** — `docs: the book, part 2: installing and remote hosts`.

---

### Task 4: The screen, Hosts and Pools (chapters 06–08)

**Files:** Create `06-the-screen.md`, `07-hosts.md`, `08-pools.md`; captures `06-screen.txt`, `06-help.txt`, `07-hosts-wide.txt`, `07-hosts-filter.txt`, `07-host-panel.txt`, `08-pools.txt`, `08-pool-panel.txt`, `08-vdevs.txt`, `08-create-pool-form.txt`. Modify the TOC.

- [ ] **Step 1: Read** — `src/ui/layout.rs`, `src/ui/hints.rs`, `help_text()`, `src/ui/theme.rs`, `src/zfs/host_health.rs`, `src/ui/metrics.rs`, `src/zfs/pool*.rs`, `src/zfs/pool_info.rs`, README Hosts and Pools parts.
- [ ] **Step 2: Capture** at `--cols 110`: the whole screen (Hosts selected), the Help pop-up (`?`), Hosts with a filter typed (`/t1`), the host panel, Pools with a pool selected, the vdev tree (`Enter`), the Create Pool form opened (`n`) and cancelled (`Esc`, never submitted).
- [ ] **Step 3: Write chapter 06** — the three areas (sidebar, list, bottom panel), moving around (`Tab`, `Shift+Tab`, arrows, `Enter`, `Esc`), the hint bar and why it is honest (drift guard), Help (`?`), About, the shell key `!` (what it opens, as whom), themes and colours (`[ui] theme`, NO_COLOR), auto-refresh (`auto_refresh_secs`), quitting; the two-key confirmation explained once with a capture of a confirmation. Windows bridge: "the sidebar is like the left tree in Disk Management".
- [ ] **Step 3b: Write chapter 07** — the Hosts table column by column (Name, Address, State, Type, Tag, Used, Pools, Scrub, Jobs, Rtt) and how columns drop on a narrow terminal; the health colours and what each State text means (every reason string from `host_health.rs` listed); the `/` filter; adding a host (`n`); the bottom panel and the braille I/O graphs (what the peak means; a sample takes ~1 s); the type words (`metal`, `vm`, `jail`) and how detected; the tag. Keys table for the section.
- [ ] **Step 4: Write chapter 08** — reading the Pools list; the bottom panel (health, space, frag, dedup, errors, scrub, vdev/disk/dataset/snapshot counts and why snapshots are only a count); drilling in (`Enter`: the vdev tree and its states); properties pane (`Right`, `Enter` to edit, which properties accept which values); scrub (`s`, what it does, when); create (`n`, vdev specs `mirror da0 da1`, the typed-name confirmation, what is overwritten), add vdev (`a`), replace disk, offline/online, resilver, destroy (typed name); every key with its confirmation; the safety callouts before each. Try it: on a throwaway file-backed pool created in chapter 02, run a scrub and read its result, then destroy it. Linux and Windows bridges (`zpool status` vs `mdadm --detail`; Storage Spaces health).
- [ ] **Step 5: Verify each how-to** on the hub with the throwaway pool (record the exact keys used in the chapter's Try it).
- [ ] **Step 6: TOC and tests; commit** — `docs: the book, part 3a: the screen, Hosts and Pools`.

---

### Task 5: Datasets, Snapshots, Disks (chapters 09–11)

**Files:** Create `09-datasets.md`, `10-snapshots.md`, `11-disks.md`; captures `09-datasets.txt`, `09-properties.txt`, `09-permissions.txt`, `09-acl.txt`, `10-snapshots.txt`, `10-create-snapshot-form.txt`, `11-disks.txt`, `11-create-pool-from-disks.txt`. Modify the TOC.

- [ ] **Step 1: Read** — `src/zfs/dataset.rs`, `property.rs`, `permission.rs`, `src/acl.rs`, `snapshot.rs`, `disk*.rs`, the matching UI code and README parts.
- [ ] **Step 2: Capture** each screen with a throwaway dataset tree on the file-backed pool (create `bookdemo/home`, `bookdemo/media` by hand before capturing; destroy after).
- [ ] **Step 3: Write chapter 09** — the list (used, available, referenced, mountpoint), create (`n`), properties and editing (compression, quota, recordsize, mountpoint, readonly: what each means and the confirm for risky ones), the Permissions tab (`zfs allow`: delegating create/snapshot to a user), the ACL tab (NFSv4 ACLs, POSIX not supported: say so), destroy (typed name; recursive `D` with the pool-root guard), the bottom panel (space breakdown). Linux bridge: ext4 quotas/`chattr`, `setfacl`; Windows: NTFS permissions, quotas.
- [ ] **Step 4: Write chapter 10** — what a snapshot is, creating (`s` implied dataset, naming), the list, destroy (confirm), rollback (what is lost: later snapshots; the confirm), clone, holds if built (verify in code; otherwise omit), recursive snapshots, how snapshot jobs name theirs (`<prefix>-<timestamp>`) and why that matters for backup jobs; the bottom panel's four figures. Windows bridge: Previous Versions / VSS; Linux: LVM snapshots, Btrfs snapshots.
- [ ] **Step 5: Write chapter 11** — what the Disks list shows (name, size, type, model, serial, status/pool), selecting with Space, building a pool from selected disks (`n`: stripe/mirror/raidz choices, the overwrite warning), adding a vdev (`a`), identifying a disk (serial and model) before replacing it, the disk panel (busy %, queue, errors). Linux bridge: `lsblk`, `/dev/disk/by-id`; Windows: Disk Management.
- [ ] **Step 6: Verify on the hub, TOC and tests; commit** — `docs: the book, part 3b: Datasets, Snapshots and Disks`.

---

### Task 6: Shares, Boot Environments, Jobs (chapters 12–14)

**Files:** Create `12-shares.md`, `13-boot-environments.md`, `14-jobs.md`; captures for each section as in Task 5. Modify the TOC.

- [ ] **Step 1: Read** — `src/zfs/share*.rs`, `smb.rs`, `bootenv.rs`, `src/jobs.rs`, `src/daemon/{engine,scheduler,cron,snapshot,scrub,replicate*}.rs`, `src/ui/jobs.rs`, README Shares, Boot Envs and Scheduled jobs parts.
- [ ] **Step 2: Capture** — the Shares list and the NFS and SMB forms (cancelled), Boot Envs, Jobs with a throwaway scrub job on the demo pool and its panel; create the job in the TUI, capture, then delete it.
- [ ] **Step 3: Write chapter 12** — NFS (`sharenfs`, the networks/hosts/read-only/root mapping form, what is exported and the mountd note: pin the port if you use a firewall), SMB (the managed block in `smb4.conf`, markers, `smbpasswd` and the password prompt, what happens on reload failure), the Samba password handling (never stored), connected clients in the panel. `Not built yet` where applicable. Linux bridge: `/etc/exports`, `smb.conf`; Windows: "map a network drive", Shared Folders.
- [ ] **Step 4: Write chapter 13** — what a boot environment is and why (safe upgrades), create/clone, activate, activate for the next boot only (`T` clears), rename, mount, destroy, the `bectl` requirement; the statuses (NR, N, R, T). Linux bridge: GRUB snapshots/Snapper; Windows: System Restore points (honestly different).
- [ ] **Step 5: Write chapter 14** — the daemon again briefly; job kinds (scrub, snapshot, replication, backup → chapter 15), creating each from the TUI (forms and field meaning, `F2` pickers), schedules (cron syntax, the presets, the next-run preview), catch-up (what it means after the daemon was down), enable/disable, delete, the history and last-run results, cancelling a transfer (`c`), snapshot retention (`keep`) and the prefix rule, replication host to host (initial and incremental, the destination rules, the shared-destination refusal) and to a file (`.zfs` stream files, resume), progress and rates. Windows bridge: Task Scheduler; Linux: cron + `sanoid`/`znapzend` (as comparisons, no endorsement).
- [ ] **Step 6: Verify on the hub, TOC and tests; commit** — `docs: the book, part 3c: Shares, Boot Environments and Jobs`.

---

### Task 7: Backups and Alerts (chapters 15–16)

**Files:** Create `15-backups.md`, `16-alerts.md`; captures `15-backup-form.txt`, `15-jobs-backup.txt`, `16-alerts.txt`, `16-alerts-form.txt`, `16-report.txt` (from `zfsctl report`, with any private address redacted by the tool). Modify the TOC.

- [ ] **Step 1: Read** — `src/daemon/backup.rs`, `src/restic.rs`, README backup and alerts parts, `src/daemon/{monitor,health,health_text,deliver,notify}.rs`, `src/ui/alerts.rs`, `src/config.rs` (`NotifyConfig`), the saved notes about cloud backups and mail methods in the project memory.
- [ ] **Step 2: Capture** the backup form (cancelled), a Jobs list with a backup job (use a throwaway local restic repository on the demo pool and `restic init` it), the Alerts screen, the alert form (cancelled), and `zfsctl report`.
- [ ] **Step 3: Write chapter 15** — how a backup job works (snapshot clone, `restic backup`, destroy clone, retention), prerequisites (restic ≥ 0.16, password file, `restic init`), repositories ("choose your own storage": local path, sftp, rclone remotes → S3, B2, Dropbox, Drive and the rate-limit advice, `rest:`; why restic's direct `s3:`/`b2:` and environment variables are not supported), retention syntax, restoring (the exact `restic restore` command and the tag), stale locks and cancelling, what the Jobs panel shows, safety notes. Windows bridge: File History/Windows Backup; Linux: `borg`/`restic` by cron.
- [ ] **Step 4: Write chapter 16** — what is watched and how often, the alert rules (two probes, escalation, reminders, recovery), the daily report and its contents, the Alerts screen (every key), the save method (only `[notify]`, `.bak`), "choose your own way to send" (msmtp, sendmail, a script, the hook for Telegram/ntfy), `notify-test` and `report`, troubleshooting mail (what `mail: failed: exited with status N` means), the limits (no acknowledge; a dead daemon sends nothing). Windows bridge: Event Viewer alerts / Task Scheduler email; Linux: logwatch/monit.
- [ ] **Step 5: Verify (without sending mail), TOC and tests; commit** — `docs: the book, part 3d: Backups and Alerts`.

---

### Task 8: Recipes, Safety, Troubleshooting (chapters 17–19)

**Files:** Create `17-recipes.md`, `18-safety.md`, `19-troubleshooting.md`. Modify the TOC.

- [ ] **Step 1: Read** — `src/exec/{mod,ssh,local,privilege}.rs`, the confirmation code in `src/ui/state.rs` (`irreversible_prompt`, `job_save_prompt`, typed confirms), `src/ui/modal.rs`, all `AppError` messages (`grep -rn 'AppError::' src`), and the README sections on safety and sudoers.
- [ ] **Step 2: Write chapter 17** — eight recipes, each with numbered steps naming the exact screens and keys and an "if it goes wrong" line: first pool from two disks; nightly snapshots with retention; replicate to a second box; back up to a USB disk with restic; back up to the cloud (rclone remote); restore one file from a snapshot and from restic; get alerted when a disk fails; replace a failed disk. Each recipe is performed once on the hub (throwaway pools/datasets/files) before it is written down; the chapter says which parts were verified live.
- [ ] **Step 3: Write chapter 18** — the safety model as a list of claims, each backed by a code location named in a note for the reviewer (not in the book text): confirmations (what needs `y`+Enter, what needs a typed name, why Enter alone does nothing), what zfsctl refuses (over-broad destroys, the pool-root guard, empty/`.`/`..` names, unknown sudoers commands), what runs where and as whom (a table), what it stores (never passwords; the password file path only), what a job can and cannot destroy (only its own snapshots by prefix), data-safety habits (scrub schedule, keep snapshots, test restores, backups off the machine), and the honest limits.
- [ ] **Step 4: Write chapter 19** — a table of messages and fixes taken from the real error strings (unreachable, ssh login refused, host key not accepted, password required from sudo, `check-host` states, restic repository errors, rclone HOME, lock waits, `mountd` firewall, msmtp failures, config refusals "edit it by hand"), each reproduced where safe; how to read the daemon log; how to ask for help (what to include).
- [ ] **Step 5: TOC and tests; commit** — `docs: the book, part 4: recipes, safety and troubleshooting`.

---

### Task 9: References, glossary and the drift tests (chapters 20–23)

**Files:** Create `20-reference-keys.md`, `21-reference-config.md`, `22-reference-commands.md`, `23-glossary.md`; modify `src/book_check.rs` (new tests), the TOC.

**Interfaces:** the reference chapters use fixed table formats the tests read:
- keys: a table per section under `## <Section>` with rows `| \`key\` | what it does |`;
- config: a table per file under `## config.toml` / `## jobs.toml` with rows `| \`key\` | type | default | meaning |`;
- commands: one `### \`zfsctl …\`` heading per command form.

- [ ] **Step 1: Write the failing drift tests** (append to `src/book_check.rs`)

```rust
fn chapter(name: &str) -> String {
    std::fs::read_to_string(book_dir().join(name)).unwrap()
}

/// Keys named in the first backticked cell of a Markdown table row, by section heading.
fn key_rows(text: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("| `") {
            if let Some((key, _)) = rest.split_once("` |") {
                out.push((section.clone(), key.to_string()));
            }
        }
    }
    out
}

#[test]
fn every_key_in_the_keys_reference_is_in_the_apps_help_and_every_help_key_is_in_the_book() {
    let help = crate::ui::state::help_text_for_book();
    let book = chapter("20-reference-keys.md");
    let rows = key_rows(&book);
    assert!(rows.len() > 40, "the keys reference lists the keys of every section ({} found)", rows.len());
    for (section, key) in &rows {
        let probe = key.split(" / ").next().unwrap();
        assert!(help.contains(probe), "{section}: the book documents key `{key}` that the app's Help does not mention");
    }
    // Every key line of Help (two leading spaces, a key column, two spaces, a description) is in the book.
    for line in help.lines().filter(|l| l.starts_with("  ") && !l.trim().is_empty()) {
        let key = line.trim_start().split("  ").next().unwrap_or("").trim();
        if key.is_empty() || key.len() > 24 {
            continue;
        }
        assert!(rows.iter().any(|(_, k)| k.split(" / ").any(|part| part == key) || k == key), "Help documents `{key}` but the keys reference does not");
    }
}

#[test]
fn every_config_key_in_the_reference_is_a_real_field_and_every_field_is_listed() {
    let text = chapter("21-reference-config.md");
    let documented: Vec<String> = key_rows(&text).into_iter().map(|(_, k)| k).collect();
    let fields = crate::config::documented_config_keys();
    for key in &documented {
        assert!(fields.contains(&key.as_str()), "the book documents config key `{key}` that does not exist");
    }
    for key in fields {
        assert!(documented.iter().any(|d| d == key), "config key `{key}` is not in the reference");
    }
}

#[test]
fn every_command_form_in_the_reference_parses_and_every_form_is_documented() {
    let text = chapter("22-reference-commands.md");
    let headings: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("### `")).filter_map(|r| r.strip_suffix('`')).map(str::to_string).collect();
    for h in &headings {
        let words: Vec<String> = h.split_whitespace().skip(1).map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string()).collect();
        assert!(h.starts_with("zfsctl"), "{h}");
        let args: Vec<String> = words.iter().filter(|w| !w.starts_with("--") && !w.chars().all(|c| c.is_ascii_uppercase())).cloned().collect();
        assert!(crate::parse_args_for_book(&args).is_ok(), "the book documents `{h}` but the program rejects it");
    }
    for want in ["zfsctl", "zfsctl daemon", "zfsctl print-sudoers HOST", "zfsctl check-host HOST", "zfsctl notify-test", "zfsctl report"] {
        assert!(headings.iter().any(|h| h.starts_with(want)), "command `{want}` is missing from the reference");
    }
}

#[test]
fn every_privileged_command_the_program_runs_is_named_in_the_sudoers_part() {
    let text = chapter("22-reference-commands.md");
    for name in crate::exec::privilege::privileged_names() {
        assert!(text.contains(&format!("`{name}`")), "privileged command `{name}` is not named in the commands reference");
    }
}

#[test]
fn every_glossary_term_is_defined_once_and_every_chapter_term_in_bold_is_in_the_glossary() {
    let glossary = chapter("23-glossary.md");
    let terms: Vec<String> = glossary.lines().filter_map(|l| l.strip_prefix("**")).filter_map(|r| r.split_once("**")).map(|(t, _)| t.to_lowercase()).collect();
    let mut sorted = terms.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), terms.len(), "a term is defined twice");
    for (name, text) in chapters() {
        for term in ["pool", "vdev", "dataset", "snapshot", "scrub", "resilver", "replication", "boot environment", "hub"] {
            if text.to_lowercase().contains(term) && !name.starts_with("23") {
                assert!(terms.iter().any(|t| t == term), "{name} uses '{term}' but the glossary does not define it");
            }
        }
    }
}
```
These need small public seams in the code, added in this task: `ui::state::help_text_for_book() -> String` (a `pub(crate)` wrapper around `help_text()`), `config::documented_config_keys() -> Vec<&'static str>` (every key of `Config`, `HostConfig`, `UiConfig`, `DaemonConfig`, `NotifyConfig` and `Job` as written in TOML, derived from one authoritative list next to the structs and itself tested against a serde round-trip of a fully populated value so the list cannot miss a field), `crate::parse_args_for_book(&[String]) -> Result<(), String>` (a `pub(crate)` wrapper around `parse_args`), `exec::privilege::privileged_names() -> Vec<&'static str>`.

- [ ] **Step 2: Run to verify they fail** — the seams do not exist yet (compile errors), then the reference chapters do not exist (panics).
- [ ] **Step 3: Add the seams, write chapters 20–23** from `help_text()`, the structs and `parse_args`: every key by section with what it does and what it asks; every `config.toml`/`jobs.toml` key with type, default and meaning (and the state-directory files); every command form with flags and exit codes plus the sudoers entry layout; the glossary (about 80 terms: ZFS, FreeBSD, zfsctl, and Linux/Windows equivalents noted in a trailing parenthesis).
- [ ] **Step 4: Run** — `cargo test 2>&1 | grep -E "test result|FAILED"; cargo clippy 2>&1 | grep -c warning` → all pass, 0. Mutation checks: remove a key row (the Help-to-book test fails); document a nonexistent config key (the field test fails); add a bogus command heading (the parse test fails).
- [ ] **Step 5: Commit** — `docs: the book, part 5: references, glossary and drift tests`.

---

### Task 10: Accuracy review, README link, finish

- [ ] **Step 1: Build the review package** (`review-package` over the branch) and dispatch one fresh reviewer on the most capable model with the plan's Review Focus verbatim, the spec, the drift-test results, and the instruction to read every chapter against the code (not to rewrite), grade findings Critical/Important/Minor and give the file and line of each.
- [ ] **Step 2: Fix pass** — every Critical and Important finding fixed in one pass (a wrong key or command gets a test that failed first where the drift tests can express it; prose fixes are re-checked against the code); minors go to a deferred list in the final message.
- [ ] **Step 3: Link the book** from the project `README.md` (a "Documentation" line at the top pointing to `docs/book/README.md`) and add `docs/book/README.md`'s line "Built from build <git short hash>"; run `cargo test`.
- [ ] **Step 4: Final live pass** — re-capture any screen changed since it was taken (`capture.py` reruns are idempotent) and confirm `git diff` on the captures is empty or explained.
- [ ] **Step 5: Commit** — `docs: the book: review fixes and the link from the README`. The branch is merged only on the user's word.
