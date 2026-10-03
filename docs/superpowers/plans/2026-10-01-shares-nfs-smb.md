# Shares (NFS/SMB) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Section::Shares` exposing ZFS's native `sharenfs`/`sharesmb` dataset properties as structured, friendly forms (networks, hosts, read-only vs read-write, root-user mapping for NFS; a share name for SMB), detecting whether `nfsd`/Samba are actually available on the host.

**Architecture:** A new `zfs::share` module parses `zfs get -H -p sharenfs,sharesmb` (no dataset argument — lists every dataset on the host in one call) into a flat `Vec<Share>`, one entry per active share (a dataset shared via both protocols produces two entries), plus two read-only availability checks. `Section::Shares` follows the exact `Pools`/`Datasets`/`Disks` refresh pattern. Every mutation (create, edit, remove) is just a `MutationCall::SetDatasetProperty` call — no new mutation plumbing — assembled from two new `FormKind`s that build a `sharenfs`/`sharesmb` value from friendly fields. Share creation is triggered by new keys in the **Datasets** section (`'N'`/`'M'`), paralleling how `'s'` already creates a snapshot from there; the Shares section itself is for viewing, editing, and removing.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-01-shares-nfs-smb-design.md`.

> **Post-implementation amendment (2026-10-02):** This plan was executed
> as written (Tasks 1-3 below; Task 4 was executed later, folded into
> the fix wave described next), then a whole-branch review found that
> **SMB share creation/editing does not actually work on FreeBSD** —
> confirmed live, as root, against the real test box: `zfs set
> sharesmb=on <dataset>` prints `"No SMB support in FreeBSD yet."` and
> fails, though the property is stored anyway. Per explicit product
> decision, every `'M'` key, `FormKind::ShareSmb`, and SMB-form code
> this plan describes below was **built and then removed** in a
> subsequent fix wave — SMB shares can still be **viewed and removed**,
> never created or edited. The task bodies below are kept verbatim as
> the historical record of what was actually built in Tasks 1-3; they
> no longer describe the shipped end state. **The spec
> (`docs/superpowers/specs/2026-10-01-shares-nfs-smb-design.md`) is kept
> current and is the authoritative description of the shipped
> feature** — read it, not this plan, for how Shares actually behaves
> today. The same whole-branch review also found and fixed: snapshots/
> zvols appearing as fake shares (missing `-t filesystem` filter), a
> stale-pre-fill bug where `'N'` could silently widen an already-
> restricted share before the Shares section had ever been refreshed
> (fixed via a new `shares_loaded` guard), and the Shares list never
> showing a share's raw value (fixed, plus a lossy-edit warning for
> values the structured form can't fully represent).

## Global Constraints

- FreeBSD only, CLI shelling via machine-readable flags where available — `zfs get -H -p sharenfs,sharesmb` (no dataset argument) is exactly as machine-readable as every other `get` call in this app, just grouped by multiple datasets instead of one.
- **No new `MutationCall` variants** — every share mutation (create, edit, remove) is `MutationCall::SetDatasetProperty { dataset, key, value }`, reusing the existing function and its `--` guard entirely unchanged.
- **`sharenfs` syntax, confirmed live against the real test box** (not raw dash-prefixed `exports(5)` flags — OpenZFS's own simpler cross-platform mini-language): comma-separated clauses, no dashes. `rw=<list>`/`ro=<list>` restrict access (`@` prefixes a network/CIDR, e.g. `@172.16.10.0/24`; a bare name/IP is a specific host; entries are colon-joined, e.g. `rw=@172.16.10.0/24:somehost`); a bare `rw`/`ro` with no `=list` applies to everyone; `root=<user>` maps root's remote credential. Confirmed exact round-trip: `zfs set sharenfs='rw=@172.16.10.0/24,root=nobody' scratch/data` → `zfs get sharenfs scratch/data` returns the identical string, and `/etc/zfs/exports` gained `/scratch/data rw=@172.16.10.0/24 root=nobody` automatically — the app only ever runs `zfs set`, never touches that file or runs `zfs share`/`exportfs` itself. There is no `mapall=`-equivalent keyword documented anywhere for this syntax (unlike raw `exports(5)`) — the structured form has exactly one root-mapping field, not two.
- **Samba availability check, confirmed live**: `service samba_server onestatus`'s exit code reflects the combined status of `smbd`, `nmbd`, *and* `winbindd` (the AD/domain-integration daemon) — confirmed on the real test box that a host with `winbindd` stopped but `smbd` serving shares fine reports a **nonzero** exit code from `onestatus` despite SMB sharing being fully functional. Availability must be determined by checking the **output text** for `"smbd is running"`, never the exit code. `nfsd`'s check (`service nfsd onestatus`) has no such issue — exit code alone is reliable.
- **No field in either share form is required** — every field may legitimately be blank (blank Networks/Hosts/Read-only means "share to everyone, read-write"; blank SMB name means "use the default name"), so neither `build_pending_action` arm needs an empty-field rejection check, unlike every other creation form in this app.
- Adding `Section::Shares` to the `Section` enum and appending it to `App::sections` makes several pre-existing exhaustive `match`es over `Section` fail to compile until patched, and shifts which section `BackTab` wraps to from `Hosts` (`Disks` → `Shares`) — Task 2 updates `state.rs`, `layout.rs`, and `ui/mod.rs` together for the compile-order reason, and includes the mechanical fixes to 5 pre-existing tests that hardcoded the old wrap target (identified by grepping every `BackTab` use in `state.rs` before writing this plan — see Task 2 Step 8 for the exact list).
- No ratatui rendering/terminal tests — all logic unit-tested against `App` via `handle_key`/`handle_modal_key`/`FakeRunner`, consistent with every prior plan.

## Review Focus

- A `sharenfs` value containing clauses the parser doesn't recognize (e.g. `sec=sys,rw,crossmnt,no_subtree_check`) must not drop the share or fail — it still displays (via the kept `raw` field) with whatever recognized clauses (`rw`/`ro`/`root=`) were found. Pinned in Task 1.
- Samba reporting a nonzero `onestatus` exit code while `smbd` itself is running must still be read as "available" — the `winbindd`-down false negative found live. Pinned in Task 1.
- A dataset shared via both NFS and SMB must produce two independent rows, not one merged row or one silently dropped. Pinned in Task 1 (`parse_shares`) and Task 2 (end-to-end).
- Submitting either share form with every field left blank must assemble to `"on"` (NFS) or `"on"` (SMB), never an empty string or a malformed `zfs set` call. Pinned in Task 3.
- Switching hosts while viewing Shares must not leak the previous host's shares list or availability flags onto the newly-selected host. Pinned in Task 2 (`clear_host_data`).

---

## File Structure

```
src/
  exec/
    mod.rs             # Modify: FakeRunner gains with_status (test-only)
  zfs/
    share.rs            # Create: Share, SharesSnapshot, parse_shares, parse_sharenfs_fields, assemble_sharenfs, list_shares, nfs_server_available, samba_available, list_shares_with_availability
    mod.rs                # Modify: pub mod share;
  ui/
    modal.rs              # Modify: FormKind::ShareNfs/ShareSmb (Task 3)
    state.rs                # Modify: Section::Shares; App fields; WorkerMsg::Shares; refresh_shares; clear_host_data; App::sections; mechanical BackTab-wrap test fixes (Task 2) — then handle_key arms, build_pending_action arms (Task 3)
    layout.rs                # Modify: sidebar label, draw_main dispatch, draw_shares (Task 2)
    mod.rs                    # Modify: 'r'-key and queued-refresh dispatch arms for Section::Shares (Task 2)
README.md                     # Modify: keybindings (Task 4)
```

---

### Task 1: `zfs::share` module

**Files:**
- Modify: `src/exec/mod.rs` (test-only `FakeRunner::with_status`)
- Create: `src/zfs/share.rs`
- Modify: `src/zfs/mod.rs`

**Interfaces:**
- Consumes: `Result` from `src/error.rs`; `CommandRunner` from `src/exec/mod.rs`; `run_checked` from `src/zfs/mod.rs`.
- Produces: `pub enum Share { Nfs { dataset: String, raw: String, networks: Vec<String>, hosts: Vec<String>, read_only: bool, root: Option<String> }, Smb { dataset: String, name: Option<String> } }` (Debug, Clone, PartialEq); `pub struct SharesSnapshot { pub shares: Vec<Share>, pub nfs_available: bool, pub samba_available: bool }`; `pub fn list_shares(runner: &dyn CommandRunner) -> Result<Vec<Share>>`; `pub fn nfs_server_available(runner: &dyn CommandRunner) -> Result<bool>`; `pub fn samba_available(runner: &dyn CommandRunner) -> Result<bool>`; `pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot>`; `pub fn assemble_sharenfs(networks: &[String], hosts: &[String], read_only: bool, root: Option<&str>) -> String` (needed by Task 3's `build_pending_action`, so must be `pub`).

- [ ] **Step 1: Write the failing test for `FakeRunner::with_status`**

Add to `src/exec/mod.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn fake_runner_with_status_keeps_custom_stdout_with_a_nonzero_status() {
        let runner = FakeRunner::new("smbd is running as pid 1.\nwinbindd is not running.\n").with_status(1);
        let output = runner.run("service", &["samba_server", "onestatus"]).unwrap();
        assert_eq!(output.stdout, "smbd is running as pid 1.\nwinbindd is not running.\n");
        assert_eq!(output.status, 1);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test fake_runner_with_status`
Expected: FAIL (`with_status` doesn't exist yet — neither `new`, which always sets status 0, nor `failing`, which always sets stdout empty, can express "custom stdout with a custom nonzero status" together)

- [ ] **Step 3: Add `FakeRunner::with_status`**

Add to `FakeRunner`'s `impl` block in `src/exec/mod.rs`, after `route`:

```rust
        /// Overrides the default response's exit status while keeping
        /// its stdout — needed for tests simulating "a nonzero exit
        /// with meaningful stdout" (e.g. a status check whose text
        /// still reports useful state despite a nonzero overall exit),
        /// which neither `new` (always status 0) nor `failing` (always
        /// empty stdout) can express together. Builder-style, like
        /// `route`.
        pub fn with_status(mut self, status: i32) -> Self {
            self.response.status = status;
            self
        }
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test fake_runner_with_status`
Expected: PASS

- [ ] **Step 5: Write `src/zfs/share.rs` with its full test suite**

```rust
use crate::error::Result;
use crate::exec::CommandRunner;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Share {
    Nfs {
        dataset: String,
        raw: String,
        networks: Vec<String>,
        hosts: Vec<String>,
        read_only: bool,
        root: Option<String>,
    },
    Smb {
        dataset: String,
        name: Option<String>,
    },
}

/// The result of one Shares refresh: every active share on the host,
/// plus whether NFS/Samba are actually available to create new ones.
pub struct SharesSnapshot {
    pub shares: Vec<Share>,
    pub nfs_available: bool,
    pub samba_available: bool,
}

/// Splits a `rw=`/`ro=` clause's list into networks (`@`-prefixed) and
/// hosts (everything else), colon-separated.
fn parse_host_list(list: &str, networks: &mut Vec<String>, hosts: &mut Vec<String>) {
    for item in list.split(':') {
        if let Some(net) = item.strip_prefix('@') {
            networks.push(net.to_string());
        } else if !item.is_empty() {
            hosts.push(item.to_string());
        }
    }
}

/// Parses a `sharenfs` value's comma-separated clauses into
/// `(networks, hosts, read_only, root)`. Unrecognized clauses (e.g.
/// `sec=sys`, `crossmnt`) are silently ignored for these structured
/// fields — the caller always keeps the original string too (`raw`),
/// so nothing is lost for display, and the existing generic
/// property-edit form remains available as a raw-text escape hatch for
/// values using clauses this structured form doesn't support.
fn parse_sharenfs_fields(raw: &str) -> (Vec<String>, Vec<String>, bool, Option<String>) {
    let mut networks = Vec::new();
    let mut hosts = Vec::new();
    let mut read_only = false;
    let mut root = None;
    for clause in raw.split(',') {
        let clause = clause.trim();
        if clause == "rw" {
            read_only = false;
        } else if clause == "ro" {
            read_only = true;
        } else if let Some(list) = clause.strip_prefix("rw=") {
            read_only = false;
            parse_host_list(list, &mut networks, &mut hosts);
        } else if let Some(list) = clause.strip_prefix("ro=") {
            read_only = true;
            parse_host_list(list, &mut networks, &mut hosts);
        } else if let Some(user) = clause.strip_prefix("root=") {
            root = Some(user.to_string());
        }
        // else: unrecognized (e.g. "on", "sec=sys", "crossmnt") — ignored for structured fields.
    }
    (networks, hosts, read_only, root)
}

/// Assembles a `sharenfs` value from structured fields — the inverse
/// of `parse_sharenfs_fields`. Every argument empty/false/`None`
/// assembles to plain `"on"` (share to everyone, read-write, no
/// root-mapping restriction).
pub fn assemble_sharenfs(networks: &[String], hosts: &[String], read_only: bool, root: Option<&str>) -> String {
    if networks.is_empty() && hosts.is_empty() && root.is_none() && !read_only {
        return "on".to_string();
    }
    let verb = if read_only { "ro" } else { "rw" };
    let mut clauses = Vec::new();
    if networks.is_empty() && hosts.is_empty() {
        clauses.push(verb.to_string());
    } else {
        let mut list: Vec<String> = networks.iter().map(|n| format!("@{n}")).collect();
        list.extend(hosts.iter().cloned());
        clauses.push(format!("{verb}={}", list.join(":")));
    }
    if let Some(user) = root {
        clauses.push(format!("root={user}"));
    }
    clauses.join(",")
}

/// Groups `zfs get -H -p sharenfs,sharesmb` output (every dataset on
/// the host, no dataset argument) by dataset name and builds one
/// `Share` per protocol actually in use — a dataset shared via both
/// NFS and SMB produces two independent entries. Datasets where both
/// properties are `off` are dropped entirely. Lenient: a malformed
/// line is skipped rather than failing the whole list, since one odd
/// line for one dataset shouldn't take down the host's entire share
/// list — diverges deliberately from the stricter single-entity
/// parsers elsewhere in this crate (`parse_property_list` etc.), which
/// only ever describe one already-known-good pool/dataset at a time.
fn parse_shares(output: &str) -> Vec<Share> {
    let mut by_dataset: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            continue;
        }
        let (dataset, property, value) = (fields[0], fields[1], fields[2]);
        let entry = by_dataset.entry(dataset.to_string()).or_insert((None, None));
        match property {
            "sharenfs" => entry.0 = Some(value.to_string()),
            "sharesmb" => entry.1 = Some(value.to_string()),
            _ => {}
        }
    }
    let mut shares = Vec::new();
    for (dataset, (sharenfs, sharesmb)) in by_dataset {
        if let Some(raw) = sharenfs.as_deref() {
            if raw != "off" {
                let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
                shares.push(Share::Nfs { dataset: dataset.clone(), raw: raw.to_string(), networks, hosts, read_only, root });
            }
        }
        if let Some(name) = sharesmb.as_deref() {
            if name != "off" {
                let name = if name == "on" { None } else { Some(name.to_string()) };
                shares.push(Share::Smb { dataset: dataset.clone(), name });
            }
        }
    }
    shares
}

pub fn list_shares(runner: &dyn CommandRunner) -> Result<Vec<Share>> {
    let output = super::run_checked(runner, "zfs", &["get", "-H", "-p", "sharenfs,sharesmb"], "zfs get sharenfs,sharesmb")?;
    Ok(parse_shares(&output.stdout))
}

/// Runs `service nfsd onestatus` directly (not through `run_checked` —
/// a nonzero exit here is an expected, meaningful "not running"
/// result, not a command failure to propagate as an error). Exit code
/// alone is reliable, confirmed live.
pub fn nfs_server_available(runner: &dyn CommandRunner) -> Result<bool> {
    let output = runner.run("service", &["nfsd", "onestatus"])?;
    Ok(output.success())
}

/// Runs `service samba_server onestatus` directly, same rationale as
/// `nfs_server_available`. Checks the **output text** for `"smbd is
/// running"` rather than the exit code — confirmed live that this
/// rc.d meta-service's exit code reflects the combined status of
/// `smbd`, `nmbd`, and `winbindd` (the AD/domain-integration daemon,
/// irrelevant to basic file sharing), so a host with `winbindd`
/// stopped but `smbd` serving shares fine reports a nonzero exit code
/// from `onestatus` despite SMB sharing being fully functional.
pub fn samba_available(runner: &dyn CommandRunner) -> Result<bool> {
    let output = runner.run("service", &["samba_server", "onestatus"])?;
    Ok(output.stdout.contains("smbd is running"))
}

/// Composes all three calls into one snapshot — avoids extra
/// round-trips, mirrors `zfs::disk::list_disks_with_membership`.
pub fn list_shares_with_availability(runner: &dyn CommandRunner) -> Result<SharesSnapshot> {
    let shares = list_shares(runner)?;
    let nfs_available = nfs_server_available(runner)?;
    let samba_available = samba_available(runner)?;
    Ok(SharesSnapshot { shares, nfs_available, samba_available })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn parse_sharenfs_fields_bare_on_means_no_restriction() {
        assert_eq!(parse_sharenfs_fields("on"), (vec![], vec![], false, None));
    }

    #[test]
    fn parse_sharenfs_fields_rw_with_network_and_root() {
        assert_eq!(
            parse_sharenfs_fields("rw=@172.16.10.0/24,root=nobody"),
            (vec!["172.16.10.0/24".to_string()], vec![], false, Some("nobody".to_string()))
        );
    }

    #[test]
    fn parse_sharenfs_fields_ro_with_host() {
        assert_eq!(parse_sharenfs_fields("ro=somehost"), (vec![], vec!["somehost".to_string()], true, None));
    }

    #[test]
    fn parse_sharenfs_fields_ignores_unrecognized_clauses_but_keeps_recognized_ones() {
        assert_eq!(parse_sharenfs_fields("sec=sys,rw,crossmnt,no_subtree_check"), (vec![], vec![], false, None));
    }

    #[test]
    fn assemble_sharenfs_all_blank_produces_on() {
        assert_eq!(assemble_sharenfs(&[], &[], false, None), "on");
    }

    #[test]
    fn assemble_sharenfs_read_only_with_no_restriction_produces_bare_ro() {
        assert_eq!(assemble_sharenfs(&[], &[], true, None), "ro");
    }

    #[test]
    fn assemble_sharenfs_networks_hosts_and_root_combine_correctly() {
        assert_eq!(
            assemble_sharenfs(&["172.16.10.0/24".to_string()], &["somehost".to_string()], false, Some("nobody")),
            "rw=@172.16.10.0/24:somehost,root=nobody"
        );
    }

    #[test]
    fn parse_then_assemble_sharenfs_round_trips_a_canonical_value() {
        let raw = "rw=@172.16.10.0/24,root=nobody";
        let (networks, hosts, read_only, root) = parse_sharenfs_fields(raw);
        assert_eq!(assemble_sharenfs(&networks, &hosts, read_only, root.as_deref()), raw);
    }

    const SHARES_FIXTURE: &str = "\
tank/both\tsharenfs\ton\tlocal
tank/both\tsharesmb\ton\tlocal
tank/data\tsharenfs\toff\tdefault
tank/data\tsharesmb\toff\tdefault
tank/nfsonly\tsharenfs\trw=@172.16.10.0/24,root=nobody\tlocal
tank/nfsonly\tsharesmb\toff\tdefault
tank/smbonly\tsharenfs\toff\tdefault
tank/smbonly\tsharesmb\tmyshare\tlocal
tank/weird\tsharenfs\tsec=sys,rw,crossmnt,no_subtree_check\tlocal
tank/weird\tsharesmb\toff\tdefault
";

    #[test]
    fn parse_shares_groups_by_dataset_and_drops_fully_unshared_datasets() {
        let shares = parse_shares(SHARES_FIXTURE);
        assert_eq!(
            shares,
            vec![
                Share::Nfs { dataset: "tank/both".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None },
                Share::Smb { dataset: "tank/both".to_string(), name: None },
                Share::Nfs {
                    dataset: "tank/nfsonly".to_string(),
                    raw: "rw=@172.16.10.0/24,root=nobody".to_string(),
                    networks: vec!["172.16.10.0/24".to_string()],
                    hosts: vec![],
                    read_only: false,
                    root: Some("nobody".to_string()),
                },
                Share::Smb { dataset: "tank/smbonly".to_string(), name: Some("myshare".to_string()) },
                Share::Nfs {
                    dataset: "tank/weird".to_string(),
                    raw: "sec=sys,rw,crossmnt,no_subtree_check".to_string(),
                    networks: vec![],
                    hosts: vec![],
                    read_only: false,
                    root: None,
                },
            ],
            "tank/data (both off) must not appear; tank/weird's unrecognized clauses must not drop the share, raw must be preserved"
        );
    }

    #[test]
    fn list_shares_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new("tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n");
        let shares = list_shares(&runner).unwrap();
        assert_eq!(shares, vec![Share::Nfs { dataset: "tank/data".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None }]);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["get".to_string(), "-H".to_string(), "-p".to_string(), "sharenfs,sharesmb".to_string()]));
    }

    #[test]
    fn list_shares_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares(&runner).is_err());
    }

    #[test]
    fn nfs_server_available_true_when_exit_zero() {
        let runner = FakeRunner::new("nfsd is running as pid 1234.\n");
        assert_eq!(nfs_server_available(&runner).unwrap(), true);
    }

    #[test]
    fn nfs_server_available_false_when_exit_nonzero() {
        let runner = FakeRunner::new("nfsd is not running.\n").with_status(1);
        assert_eq!(nfs_server_available(&runner).unwrap(), false);
    }

    #[test]
    fn samba_available_true_when_smbd_running_even_if_exit_nonzero() {
        // The real, confirmed case: winbindd down drags onestatus's
        // exit code to nonzero even though smbd itself is fine.
        let runner = FakeRunner::new("nmbd is running as pid 1.\nsmbd is running as pid 2.\nwinbindd is not running.\n").with_status(1);
        assert_eq!(samba_available(&runner).unwrap(), true);
    }

    #[test]
    fn samba_available_false_when_smbd_itself_is_not_running() {
        let runner = FakeRunner::new("nmbd is not running.\nsmbd is not running.\nwinbindd is not running.\n").with_status(1);
        assert_eq!(samba_available(&runner).unwrap(), false);
    }

    #[test]
    fn list_shares_with_availability_composes_all_three_calls() {
        let runner = FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n");
        let snapshot = list_shares_with_availability(&runner).unwrap();
        assert_eq!(snapshot.shares.len(), 1);
        assert_eq!(snapshot.nfs_available, true);
        assert_eq!(snapshot.samba_available, true);
    }

    #[test]
    fn list_shares_with_availability_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_shares_with_availability(&runner).is_err());
    }
}
```

- [ ] **Step 6: Declare the module in `src/zfs/mod.rs`**

```rust
pub mod dataset;
pub mod disk;
pub mod pool;
pub mod property;
pub mod share;
pub mod snapshot;
```

- [ ] **Step 7: Run the tests**

Run: `cargo test zfs::share`
Expected: PASS (17 tests)

- [ ] **Step 8: Run the full suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, all existing tests plus the 18 new ones from this task (17 in `zfs::share` + 1 `FakeRunner::with_status` test); release build clean aside from a new `dead_code` warning on every `zfs::share` item (expected — nothing consumes this module yet, Task 2 wires it up)

- [ ] **Step 9: Commit**

```bash
git add src/exec/mod.rs src/zfs/share.rs src/zfs/mod.rs
git commit -m "feat: zfs::share module — sharenfs/sharesmb parsing and availability detection"
```

---

### Task 2: `Section::Shares` — state, refresh, and rendering

**Files:**
- Modify: `src/ui/state.rs`, `src/ui/layout.rs`, `src/ui/mod.rs`

**Interfaces:**
- Consumes: `Share`, `SharesSnapshot`, `list_shares_with_availability` from `src/zfs/share.rs` (Task 1).
- Produces: `Section::Shares` variant. `App` fields `pub shares: Vec<Share>`, `pub shares_table: TableState`, `pub nfs_available: Option<bool>`, `pub samba_available: Option<bool>`. `WorkerMsg::Shares(usize, Result<SharesSnapshot>)`. `App::refresh_shares(&mut self, runner: Arc<dyn CommandRunner>)`. `draw_shares` (private to `layout.rs`).

This task does **not** add any interactivity beyond Up/Down navigation and `'r'` refresh — `Enter`/`'d'` in Shares and `'N'`/`'M'` in Datasets are Task 3, deliberately deferred alongside the `FormKind` variants they depend on.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn refresh_shares_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(
        FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\ntank/data\tsharesmb\toff\tdefault\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is not running.\n"),
    );
    app.refresh_shares(runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.shares.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.shares.len(), 1);
    assert_eq!(app.nfs_available, Some(true));
    assert_eq!(app.samba_available, Some(false));
}

#[test]
fn shares_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx
        .send(WorkerMsg::Shares(
            1,
            Ok(crate::zfs::share::SharesSnapshot {
                shares: vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }],
                nfs_available: true,
                samba_available: true,
            }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.shares.is_empty(), "a result for a host that isn't selected must not populate the pane");
}

#[test]
fn switching_host_clears_shares_state() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }];
    app.shares_table.select(Some(0));
    app.nfs_available = Some(true);
    app.samba_available = Some(false);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert!(app.shares.is_empty());
    assert_eq!(app.shares_table.selected(), None);
    assert_eq!(app.nfs_available, None);
    assert_eq!(app.samba_available, None);
}
```

Also add `use crate::zfs::share::Share;` is not required at the top level yet (the tests above fully-qualify it) — but Step 3 below does need `SharesSnapshot` referenced from `WorkerMsg`, so add this import to `state.rs`'s top-level `use` block:

```rust
use crate::zfs::share::SharesSnapshot;
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test refresh_shares_populates_app_state_from_fake_runner`
Expected: FAIL (`Section::Shares`, `App::shares`, `App::refresh_shares` don't exist yet)

- [ ] **Step 3: Add `Section::Shares`, the new `App` fields, `WorkerMsg::Shares`, and update `App::new`**

Change the `Section` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
    Disks,
    Shares,
    Vdevs,
    PoolProperties,
    DatasetProperties,
}
```

Add a variant to `WorkerMsg`, after `Disks`:

```rust
    Shares(usize, Result<SharesSnapshot>),
```

Add four fields to the `App` struct, after `selected_disks: HashSet<String>,`:

```rust
    pub shares: Vec<crate::zfs::share::Share>,
    pub shares_table: TableState,
    /// `None` means "not yet checked" — kept distinct from `Some(false)`
    /// so the Shares title doesn't flash a false "unavailable" hint
    /// before the first refresh, consistent with every other section
    /// staying quiet (an empty list) rather than asserting a default
    /// before it's actually fetched anything.
    pub nfs_available: Option<bool>,
    pub samba_available: Option<bool>,
```

Add `Section::Shares` to `App::new`'s `sections` field:

```rust
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares],
```

Initialize the four new fields in `App::new`, after `selected_disks: HashSet::new(),`:

```rust
            shares: Vec::new(),
            shares_table: TableState::default(),
            nfs_available: None,
            samba_available: None,
```

- [ ] **Step 4: Update `clear_host_data`**

Add four lines, after `self.selected_disks.clear();`:

```rust
        self.shares.clear();
        self.shares_table.select(None);
        self.nfs_available = None;
        self.samba_available = None;
```

- [ ] **Step 5: Add `refresh_shares`**

Add after `refresh_disks`:

```rust
    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    pub fn refresh_shares(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Shares, host) {
            self.pending_refresh.insert((Section::Shares, host));
            return;
        }
        self.begin_refresh(Section::Shares, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::share::list_shares_with_availability(runner.as_ref());
            let _ = tx.send(WorkerMsg::Shares(host, result));
        });
    }
```

- [ ] **Step 6: Add the `WorkerMsg::Shares` arm to `drain_worker_messages`**

Add after the `WorkerMsg::Disks` arm:

```rust
                WorkerMsg::Shares(host, result) => {
                    self.end_refresh(Section::Shares, host);
                    let had_pending = self.pending_refresh.remove(&(Section::Shares, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::Shares);
                    }
                    match result {
                        Ok(snapshot) => {
                            self.shares = snapshot.shares;
                            clamp_table_selection(&mut self.shares_table, self.shares.len());
                            self.nfs_available = Some(snapshot.nfs_available);
                            self.samba_available = Some(snapshot.samba_available);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

- [ ] **Step 7: Add Up/Down navigation for the Shares section**

Add to `handle_key`'s match, anywhere before the final `_ => {}` (e.g. right after the Disks section's Up/Down arms):

```rust
            KeyCode::Down if self.current_section() == Section::Shares => {
                move_table_selection(&mut self.shares_table, self.shares.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Shares => {
                move_table_selection(&mut self.shares_table, self.shares.len(), false);
            }
```

- [ ] **Step 8: Apply the mechanical test fixes for the new last section**

`Section::Shares` is now the last section, so `BackTab` from `Hosts` wraps to `Shares`, not `Disks`. Five pre-existing tests hardcode the old wrap target (found by grepping every `BackTab` use in `state.rs` before writing this plan):

In `handle_key_navigates_sections_and_wraps`, change the final assertion from `Section::Disks` to `Section::Shares`.

In `backtab_cycles_sections_backward`, change the assertion from `Section::Disks` to `Section::Shares` (keep the existing message string).

In each of `handle_key_navigates_snapshot_rows_and_wraps`, `handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal`, and `handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal`, these reach `Snapshots` via two `BackTab`s (`// Hosts -> Disks (wraps backward)` then `// Disks -> Snapshots`) — that now takes a third `BackTab`. Change:

```rust
        app.handle_key(KeyCode::BackTab); // Hosts -> Disks (wraps backward)
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
```

to:

```rust
        app.handle_key(KeyCode::BackTab); // Hosts -> Shares (wraps backward)
        app.handle_key(KeyCode::BackTab); // Shares -> Disks
        app.handle_key(KeyCode::BackTab); // Disks -> Snapshots
```

in all three places. Apply this identically in each — the three occurrences are otherwise unrelated tests (snapshot row navigation, destroy-confirm, rollback-confirm), only this comment-and-BackTab-count prefix is shared.

- [ ] **Step 9: Update `layout.rs`**

Add `use crate::zfs::share::Share;` to the top-level imports.

In `draw_sidebar`'s label `match`, add after `Section::Disks => "Disks",`:

```rust
                Section::Shares => "Shares",
```

In `draw_main`'s final `match section { ... }` block, add a new arm:

```rust
    match section {
        Section::Hosts => draw_hosts(f, app, area),
        Section::Snapshots => draw_snapshots(f, app, area),
        Section::Disks => draw_disks(f, app, area),
        Section::Shares => draw_shares(f, app, area),
        Section::Pools | Section::Datasets => unreachable!("handled above"),
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties => {
            unreachable!("never in App::sections")
        }
    }
```

Add a new function, after `draw_disks`:

```rust
fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.shares.iter().map(|s| match s {
        Share::Nfs { dataset, networks, hosts, read_only, root, .. } => {
            let mut details = vec![if *read_only { "ro".to_string() } else { "rw".to_string() }];
            if !networks.is_empty() {
                details.push(format!("networks={}", networks.join(",")));
            }
            if !hosts.is_empty() {
                details.push(format!("hosts={}", hosts.join(",")));
            }
            if let Some(user) = root {
                details.push(format!("root={user}"));
            }
            Row::new(vec![dataset.clone(), "NFS".to_string(), details.join(", ")])
        }
        Share::Smb { dataset, name } => {
            Row::new(vec![dataset.clone(), "SMB".to_string(), name.clone().unwrap_or_else(|| "(default name)".to_string())])
        }
    });
    let widths = [Constraint::Length(24), Constraint::Length(8), Constraint::Min(20)];
    let mut hints = Vec::new();
    if app.nfs_available == Some(false) {
        hints.push("NFS unavailable: nfsd not running");
    }
    if app.samba_available == Some(false) {
        hints.push("SMB unavailable: Samba not running");
    }
    let hint_suffix = if hints.is_empty() { String::new() } else { format!(" [{}]", hints.join("; ")) };
    let title = format!("Shares — {} ('r' refresh, Enter edit, 'd' remove){hint_suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Dataset", "Protocol", "Details"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.shares_table);
}
```

- [ ] **Step 10: Update `ui/mod.rs`**

In the queued-refresh dispatch `match section { ... }` (near the top of `event_loop`), add after `Section::Disks => app.refresh_disks(host.runner.clone()),`:

```rust
                    Section::Shares => app.refresh_shares(host.runner.clone()),
```

In the `'r'`-key handling's `else` branch (`match app.current_section() { ... }`), add after `Section::Disks => app.refresh_disks(host.runner.clone()),` — the trailing `Section::Hosts | Section::Vdevs | ...` catch-all needs no change, since `Section::Shares` is now named explicitly:

```rust
                                    Section::Shares => app.refresh_shares(host.runner.clone()),
```

- [ ] **Step 11: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, all existing tests plus the 3 new ones from Step 1 (the mechanical fixes in Step 8 don't add new tests, they fix existing ones); release build clean aside from pre-existing dead-code warnings unrelated to this task

- [ ] **Step 12: Commit**

```bash
git add src/ui/state.rs src/ui/layout.rs src/ui/mod.rs
git commit -m "feat: Section::Shares — state, refresh, and rendering"
```

---

### Task 3: Shares interactivity — create, edit, remove

**Files:**
- Modify: `src/ui/modal.rs`, `src/ui/state.rs`

**Interfaces:**
- Consumes: `shares`, `shares_table`, `nfs_available`, `samba_available` from `src/ui/state.rs` (Task 2); `Share`, `assemble_sharenfs` from `src/zfs/share.rs` (Task 1); `MutationCall::SetDatasetProperty` (pre-existing).
- Produces: `FormKind::ShareNfs { host: usize, dataset: String }` (fields: `[Networks, Hosts, Read-only (y/n), Root user mapping]`), `FormKind::ShareSmb { host: usize, dataset: String }` (fields: `[Share name]`). `handle_key` arms for `Enter`/`'d'` in Shares, `'N'`/`'M'` in Datasets. `build_pending_action` arms for both new `FormKind`s.

- [ ] **Step 1: Write failing tests**

Add to `src/ui/state.rs`'s test module:

```rust
#[test]
fn enter_on_nfs_share_row_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Nfs {
        dataset: "tank/data".to_string(),
        raw: "rw=@172.16.10.0/24,root=nobody".to_string(),
        networks: vec!["172.16.10.0/24".to_string()],
        hosts: vec![],
        read_only: false,
        root: Some("nobody".to_string()),
    }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 4);
            assert_eq!(fields[0].value, "172.16.10.0/24");
            assert_eq!(fields[1].value, "");
            assert_eq!(fields[2].value, "n");
            assert_eq!(fields[3].value, "nobody");
            assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn enter_on_smb_share_row_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: Some("myshare".to_string()) }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields[0].value, "myshare");
            assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_d_on_nfs_share_row_opens_remove_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Nfs {
        dataset: "tank/data".to_string(),
        raw: "on".to_string(),
        networks: vec![],
        hosts: vec![],
        read_only: false,
        root: None,
    }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(
                action.call,
                MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "off".to_string() }
            );
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_d_on_smb_share_row_opens_remove_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/data".to_string(), name: None }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(
                action.call,
                MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharesmb".to_string(), value: "off".to_string() }
            );
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_shift_n_on_datasets_opens_blank_share_nfs_form_when_not_yet_shared() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(true);
    app.handle_key(KeyCode::Char('N'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert!(fields.iter().all(|f| f.value.is_empty()));
            assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_shift_n_on_datasets_sets_hint_when_nfs_unavailable() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(false);
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("NFS server not available on this host".to_string()));
}

#[test]
fn handle_key_shift_m_on_datasets_opens_blank_share_smb_form_when_not_yet_shared() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.samba_available = Some(true);
    app.handle_key(KeyCode::Char('M'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields[0].value, "");
            assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_shift_m_on_datasets_sets_hint_when_samba_unavailable() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.samba_available = Some(false);
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("Samba not available on this host".to_string()));
}

#[test]
fn build_pending_action_share_nfs_blank_fields_assembles_to_on() {
    let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![
        FormField { label: "Networks".to_string(), value: String::new() },
        FormField { label: "Hosts".to_string(), value: String::new() },
        FormField { label: "Read-only (y/n)".to_string(), value: String::new() },
        FormField { label: "Root user mapping".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Shares,
            call: MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "on".to_string() },
        }
    );
}

#[test]
fn build_pending_action_share_nfs_assembles_filled_fields() {
    let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![
        FormField { label: "Networks".to_string(), value: "172.16.10.0/24".to_string() },
        FormField { label: "Hosts".to_string(), value: String::new() },
        FormField { label: "Read-only (y/n)".to_string(), value: "n".to_string() },
        FormField { label: "Root user mapping".to_string(), value: "nobody".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "rw=@172.16.10.0/24,root=nobody".to_string() }
    );
}

#[test]
fn build_pending_action_share_smb_blank_name_assembles_to_on() {
    let kind = FormKind::ShareSmb { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![FormField { label: "Share name".to_string(), value: String::new() }];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Shares,
            call: MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharesmb".to_string(), value: "on".to_string() },
        }
    );
}

#[test]
fn build_pending_action_share_smb_assembles_custom_name() {
    let kind = FormKind::ShareSmb { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![FormField { label: "Share name".to_string(), value: "myshare".to_string() }];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharesmb".to_string(), value: "myshare".to_string() }
    );
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test enter_on_nfs_share_row_opens_prefilled_edit_form`
Expected: FAIL (`FormKind::ShareNfs` doesn't exist yet)

- [ ] **Step 3: Add the two new `FormKind` variants**

In `src/ui/modal.rs`'s `FormKind` enum, add after `CreateSnapshot { host: usize, dataset: String },`:

```rust
    /// Fields: `[networks, hosts, read_only, root]` — `networks`/
    /// `hosts` are comma-separated lists, `read_only` is `"y"`/`"n"`
    /// text, `root` is a username; every field may be blank.
    ShareNfs { host: usize, dataset: String },
    /// Fields: `[name]` — blank means the default share name.
    ShareSmb { host: usize, dataset: String },
```

- [ ] **Step 4: Add the `handle_key` arms**

Add the Shares section's `Enter`/`'d'` arms to `src/ui/state.rs`'s `handle_key`, directly after the Shares Up/Down arms added in Task 2:

```rust
            KeyCode::Enter if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        self.modal = Some(share_edit_modal(share, self.selected_host));
                    }
                }
            }
            KeyCode::Char('d') if self.current_section() == Section::Shares => {
                if let Some(i) = self.shares_table.selected() {
                    if let Some(share) = self.shares.get(i) {
                        let (dataset, key, protocol) = match share {
                            crate::zfs::share::Share::Nfs { dataset, .. } => (dataset.clone(), "sharenfs", "NFS"),
                            crate::zfs::share::Share::Smb { dataset, .. } => (dataset.clone(), "sharesmb", "SMB"),
                        };
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Remove {protocol} share for '{dataset}'? (y/n)"),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::Shares,
                                call: MutationCall::SetDatasetProperty { dataset, key: key.to_string(), value: "off".to_string() },
                            },
                        });
                    }
                }
            }
```

Add the Datasets section's `'N'`/`'M'` arms, directly after the existing `'s'` (create snapshot) arm:

```rust
            KeyCode::Char('N') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if self.nfs_available == Some(false) {
                    self.status_message = Some("NFS server not available on this host".to_string());
                } else if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        let existing = self.shares.iter().find(|s| matches!(s, crate::zfs::share::Share::Nfs { dataset, .. } if dataset == &ds.name));
                        let placeholder = crate::zfs::share::Share::Nfs {
                            dataset: ds.name.clone(),
                            raw: "off".to_string(),
                            networks: vec![],
                            hosts: vec![],
                            read_only: false,
                            root: None,
                        };
                        self.modal = Some(share_edit_modal(existing.unwrap_or(&placeholder), self.selected_host));
                    }
                }
            }
            KeyCode::Char('M') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Middle => {
                if self.samba_available == Some(false) {
                    self.status_message = Some("Samba not available on this host".to_string());
                } else if let Some(i) = self.datasets_table.selected() {
                    if let Some(ds) = self.datasets.get(i) {
                        let existing = self.shares.iter().find(|s| matches!(s, crate::zfs::share::Share::Smb { dataset, .. } if dataset == &ds.name));
                        let placeholder = crate::zfs::share::Share::Smb { dataset: ds.name.clone(), name: None };
                        self.modal = Some(share_edit_modal(existing.unwrap_or(&placeholder), self.selected_host));
                    }
                }
            }
```

This reuses one helper (`share_edit_modal`) for all four entry points (Shares' `Enter`, both Datasets keys' already-shared and not-yet-shared cases) — one form-building function, no duplicated field-construction logic. Add the helper as a free function, near `build_pending_action`:

```rust
/// Builds the `Modal::Form` for editing (or, for a not-yet-shared
/// placeholder `Share` with `off`/`None` fields, creating) the given
/// share — used by the Shares section's `Enter` and the Datasets
/// section's `'N'`/`'M'` keys alike, so there's exactly one place that
/// builds either form. `host` is the index of the currently-selected
/// host (`self.selected_host` at every call site), threaded through
/// into the resulting `FormKind` so the eventual mutation runs against
/// the right host.
fn share_edit_modal(share: &crate::zfs::share::Share, host: usize) -> Modal {
    match share {
        crate::zfs::share::Share::Nfs { dataset, networks, hosts, read_only, root, .. } => Modal::Form {
            title: format!("NFS share: '{dataset}'"),
            fields: vec![
                FormField { label: "Networks".to_string(), value: networks.join(",") },
                FormField { label: "Hosts".to_string(), value: hosts.join(",") },
                FormField { label: "Read-only (y/n)".to_string(), value: if *read_only { "y".to_string() } else { "n".to_string() } },
                FormField { label: "Root user mapping".to_string(), value: root.clone().unwrap_or_default() },
            ],
            focused: 0,
            description: None,
            kind: FormKind::ShareNfs { host, dataset: dataset.clone() },
        },
        crate::zfs::share::Share::Smb { dataset, name } => Modal::Form {
            title: format!("SMB share: '{dataset}'"),
            fields: vec![FormField { label: "Share name".to_string(), value: name.clone().unwrap_or_default() }],
            focused: 0,
            description: None,
            kind: FormKind::ShareSmb { host, dataset: dataset.clone() },
        },
    }
}
```

- [ ] **Step 5: Add the `build_pending_action` arms**

Add to `build_pending_action`'s match, after the `FormKind::CreateSnapshot` arm:

```rust
        FormKind::ShareNfs { host, dataset } => {
            let networks: Vec<String> = fields[0].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let hosts: Vec<String> = fields[1].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let read_only = fields[2].value.trim().to_lowercase().starts_with('y');
            let root = fields[3].value.trim();
            let root = if root.is_empty() { None } else { Some(root) };
            let value = crate::zfs::share::assemble_sharenfs(&networks, &hosts, read_only, root);
            Some(PendingAction {
                host: *host,
                section: Section::Shares,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value },
            })
        }
        FormKind::ShareSmb { host, dataset } => {
            let name = fields[0].value.trim();
            let value = if name.is_empty() { "on".to_string() } else { name.to_string() };
            Some(PendingAction {
                host: *host,
                section: Section::Shares,
                call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharesmb".to_string(), value },
            })
        }
```

- [ ] **Step 6: Run the full test suite and build**

Run: `cargo test && cargo build --release`
Expected: PASS, all existing tests plus the 13 new ones from Step 1

- [ ] **Step 7: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs
git commit -m "feat: Shares interactivity — create, edit, and remove NFS/SMB shares"
```

---

### Task 4: README keybindings

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the keybindings**

In the "## Keys" intro bullet, change the section list to include Shares:

```
- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
```

In the "### Datasets" section, add two bullets after the existing `s` (snapshot) bullet:

```
- `N`: share the selected dataset via NFS (prompts for networks, hosts, read-only, and root user mapping — all optional; blank means share to everyone, read-write) — no-op with a hint if NFS isn't available on this host
- `M`: share the selected dataset via SMB (prompts for an optional share name) — no-op with a hint if Samba isn't available on this host
```

Add a new "### Shares" section, after "### Disks" and before "## Status":

```
### Shares

- `Up` / `Down`: select a share
- `Enter`: edit the selected share (prompts pre-filled with its current networks/hosts/permissions or share name)
- `d`: remove the selected share (y/n confirm)
- `r`: refresh the share list
```

In the "## Status" paragraph, add a sentence at the end:

```
A Shares section lists every active NFS/SMB share (ZFS's own `sharenfs`/`sharesmb` dataset properties) with structured create/edit forms — full networks/hosts/permissions control for NFS, on/off plus a share name for SMB — and detects whether `nfsd`/Samba are actually available on the host.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document the Shares section and NFS/SMB share keys"
```
