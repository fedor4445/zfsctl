# Delegated ZFS Permissions (zfs allow/unallow) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a **Permissions** sub-tab to the Datasets right pane (the first sub-tab the app has ever had — previously Properties-only) exposing `zfs allow`'s delegated administration permissions: view every permission set, create-time permission, and user/group/everyone grant on the selected dataset; grant/add to any of the three (`'n'`/`'s'`/`'c'`); revoke a whole target or just specific permissions from it (`'d'`/`'x'`).

**Architecture:** A new `zfs::permission` module hand-parses `zfs allow <dataset>`'s human-formatted output (confirmed live — no machine-readable mode exists for this command) into a `PermissionsSnapshot`, and builds the exact flag combination `zfs allow`/`unallow` need for each of three target kinds (a user/group/everyone grant, a named permission set, or a create-time permission) crossed with three delegation scopes. A new `DatasetDetailTab` enum (`Properties` | `Permissions`) lets `'['`/`']'` cycle which view the Datasets right pane shows, following the exact same fetch-on-demand/stale-identity-discard pattern the existing Properties pane already uses. Every mutation (grant, revoke-all, revoke-specific) is a `MutationCall::AllowPermission`/`UnallowPermission` call built from four new `FormKind`s — no sub-tab plumbing or mutation machinery is added to the Pools right pane, which stays single-view (pools can't have delegated permissions — confirmed via `zfs-allow(8)`'s synopsis, which only ever says `filesystem|volume`).

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-02-zfs-permissions-design.md`.

## Global Constraints

- FreeBSD only, CLI shelling via machine-readable flags where they exist (none exist for `zfs allow` — hand-parsing its human-formatted output is unavoidable and confirmed-correct, see the spec's Architecture section) — unchanged from prior plans.
- `CommandRunner` is the sole execution abstraction; `allow`/`unallow` delegate to `runner.run`/`super::run_checked` like every other mutation, never a raw shell string.
- **`zfs allow`/`zfs unallow` do NOT honor `--` as an end-of-options marker** — confirmed live (`zfs allow -u fb -- snapshot scratch/data` fails with `"wrong number of parameters"`), a third exception alongside `zfs set`/`zpool set`. Do not add `--` anywhere in `permission.rs`.
- Every caller-supplied token (recipient name, permission-set name, individual permission name) passed to `allow`/`unallow` must pass `is_safe_permission_arg` (rejects anything starting with `-`) before it's used to build a command — this is a different, simpler check than `zfs::snapshot`'s `is_safe_zfs_target`/`is_safe_host` (a full character-allowlist used because `replicate_snapshot` hand-builds a raw shell string); do not conflate the two or reuse `is_safe_zfs_target` itself.
- Always pass explicit `-u`/`-g` for user/group grants (never the bare-name heuristic `zfs-allow(8)` also supports) — confirmed in the spec's Architecture section as the safer, unambiguous form.
- `allow`/`unallow` are purely additive/subtractive, never a replace — there is no "edit a grant to this exact new list" operation anywhere in this plan. `'n'`/`'s'`/`'c'` always add; `'x'` removes specific permissions; `'d'` removes everything for a target.
- The Permissions sub-tab only ever appears for the Datasets right pane. The Pools right pane is untouched by this plan — no sub-tab, no new keys, no new state fields on it.
- `Permissions` in every new form is a **required** field (blank keeps the form open) — unlike Shares' all-optional fields, there is no sensible default for "which permissions". `Scope` is the one field with a default for blank (`Both`), but is otherwise strict: only `local`/`descendent`/`both` (case-insensitive) are accepted, anything else keeps the form open (a typo must never silently fall back to the widest scope).

## Review Focus

- Pressing `'n'`/`'s'`/`'c'` (new grant/set/create-time) with no dataset selected in the Datasets middle pane must not panic or open a form — every existing creation key in this app already guards this with `if let Some(i) = ...table.selected()`; this plan's new keys must match that, not skip it because "a dataset is probably selected."
- Pressing `'d'`/`'x'` with no row selected in the Permissions sub-tab (e.g. a dataset with zero delegated permissions, so the table is empty) must not panic or open a modal.
- A malformed or unrecognized line in `zfs allow`'s output (e.g. a line under a header string the parser doesn't recognize, or an indented line that doesn't match any expected shape) must be silently skipped, not panic or corrupt other already-parsed entries — mirrors `zfs::share::parse_shares`'s established leniency for exactly this reason (one odd line for one dataset shouldn't take down the whole view).
- Re-entering the Permissions sub-tab via `Right` (not `'['`/`']'`) after it was the active sub-tab during a previous visit must reset back to `Properties` — a stale sub-tab preference must never silently persist across a fresh Right-focus-entry, matching how the Properties pane itself already always starts fresh (clears and refetches) on re-entry.
- A `Recipient` field value that's syntactically close-but-wrong (a bare name with no `user:`/`group:` prefix, or wrong-case like `User:fb`) must keep the grant form open rather than silently defaulting to some recipient — this is the stricter-than-usual validation the spec specifically calls for to avoid a permissions-widening footgun.

---

## File Structure

- **Create:** `src/zfs/permission.rs` — `Recipient`, `Scope`, `PermissionTarget`, `PermissionGrant`, `PermissionSet`, `PermissionsSnapshot`, the output parser, `is_safe_permission_arg`, `list_permissions`, `allow`, `unallow`.
- **Modify:** `src/zfs/mod.rs` — register the new module.
- **Modify:** `src/ui/modal.rs` — `FormKind::GrantPermission`/`GrantPermissionSet`/`GrantCreateTimePermission`/`RevokePermissions`; `MutationCall::AllowPermission`/`UnallowPermission`.
- **Modify:** `src/ui/state.rs` — `DatasetDetailTab`; `PermissionRow`; new `Section::DatasetPermissions` variant; `App` fields (`dataset_detail_tab`, `dataset_permissions`, `dataset_permissions_table`, `dataset_permissions_for`); `WorkerMsg::DatasetPermissions`; `refresh_dataset_permissions`; `handle_key` arms (`'['`/`']'`, tab-aware Up/Down in Right focus, `'n'`/`'s'`/`'c'`/`'d'`/`'x'`); `build_pending_action` arms.
- **Modify:** `src/ui/layout.rs` — `draw_dataset_properties` dispatches to a new `draw_dataset_permissions` based on the active sub-tab; sidebar label match; `draw_main`'s final exhaustive match.
- **Modify:** `src/ui/mod.rs` — refresh dispatch for the new section/sub-tab.
- **Modify:** `README.md` — keybindings (folded into Task 3, not a separate task — a prior plan in this project skipped its README task entirely by declaring "all tasks complete" too early; folding it into the last task's own steps means there's no separate task to forget).

## Task 1: `zfs::permission` module

**Files:**
- Create: `src/zfs/permission.rs`
- Modify: `src/zfs/mod.rs:1-6` (add `pub mod permission;` alongside the existing `pub mod` lines)

**Interfaces:**
- Produces: `Recipient { User(String), Group(String), Everyone }`, `Scope { Local, Descendent, Both }` (both `Copy` where applicable), `PermissionTarget { Grant { recipient: Recipient, scope: Scope }, Set(String), CreateTime }`, `PermissionGrant { recipient: Recipient, scope: Scope, perms: Vec<String> }`, `PermissionSet { name: String, perms: Vec<String> }`, `PermissionsSnapshot { sets: Vec<PermissionSet>, create_time: Vec<String>, grants: Vec<PermissionGrant> }` (all `pub`, all `Debug, Clone, PartialEq`), `pub fn list_permissions(runner: &dyn CommandRunner, dataset: &str) -> Result<PermissionsSnapshot>`, `pub fn allow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: &[String]) -> Result<()>`, `pub fn unallow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: Option<&[String]>) -> Result<()>`. These exact names and signatures are consumed by Tasks 2 and 3 — do not rename or reorder fields.

- [ ] **Step 1: Write the data-model and parser tests (failing)**

Create `src/zfs/permission.rs` with just the imports, type definitions, and this test module (the parser/guard functions referenced don't exist yet, so this will fail to compile — that's expected for this step):

```rust
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    User(String),
    Group(String),
    Everyone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Local,
    Descendent,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionTarget {
    Grant { recipient: Recipient, scope: Scope },
    Set(String),
    CreateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionGrant {
    pub recipient: Recipient,
    pub scope: Scope,
    pub perms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet {
    pub name: String,
    pub perms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionsSnapshot {
    pub sets: Vec<PermissionSet>,
    pub create_time: Vec<String>,
    pub grants: Vec<PermissionGrant>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const PERMISSIONS_FIXTURE: &str = "\
---- Permissions on scratch/data -------------------------------------
Permission sets:
        @admin clone,rollback,snapshot
Create time permissions:
        destroy
Local permissions:
        group wheel create,destroy
Descendent permissions:
        everyone mount
Local+Descendent permissions:
        user fb @admin,snapshot
";

    #[test]
    fn parse_permissions_empty_output_is_all_empty() {
        assert_eq!(parse_permissions(""), PermissionsSnapshot::default());
    }

    #[test]
    fn parse_permissions_fixture_covers_all_five_sections() {
        let snapshot = parse_permissions(PERMISSIONS_FIXTURE);
        assert_eq!(
            snapshot,
            PermissionsSnapshot {
                sets: vec![PermissionSet {
                    name: "admin".to_string(),
                    perms: vec!["clone".to_string(), "rollback".to_string(), "snapshot".to_string()],
                }],
                create_time: vec!["destroy".to_string()],
                grants: vec![
                    PermissionGrant {
                        recipient: Recipient::Group("wheel".to_string()),
                        scope: Scope::Local,
                        perms: vec!["create".to_string(), "destroy".to_string()],
                    },
                    PermissionGrant {
                        recipient: Recipient::Everyone,
                        scope: Scope::Descendent,
                        perms: vec!["mount".to_string()],
                    },
                    PermissionGrant {
                        recipient: Recipient::User("fb".to_string()),
                        scope: Scope::Both,
                        perms: vec!["@admin".to_string(), "snapshot".to_string()],
                    },
                ],
            },
            "every section header must be recognized and every entry parsed into the right bucket"
        );
    }

    #[test]
    fn parse_permissions_ignores_a_line_under_an_unrecognized_header() {
        let output = "\
---- Permissions on scratch/data -------------------------------------
Some Future Section:
        whatever this is
Local permissions:
        group wheel create,destroy
";
        let snapshot = parse_permissions(output);
        assert_eq!(
            snapshot.grants,
            vec![PermissionGrant { recipient: Recipient::Group("wheel".to_string()), scope: Scope::Local, perms: vec!["create".to_string(), "destroy".to_string()] }],
            "an unrecognized section must not panic or drop already-parsed entries from other sections"
        );
    }

    #[test]
    fn parse_permissions_ignores_a_malformed_entry_line() {
        let output = "\
---- Permissions on scratch/data -------------------------------------
Local permissions:
        this line has no recognizable shape at all
        group wheel create,destroy
";
        let snapshot = parse_permissions(output);
        assert_eq!(
            snapshot.grants,
            vec![PermissionGrant { recipient: Recipient::Group("wheel".to_string()), scope: Scope::Local, perms: vec!["create".to_string(), "destroy".to_string()] }],
            "a malformed entry line must be skipped, not panic or corrupt the next valid entry"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib zfs::permission`
Expected: compile error — `parse_permissions` is not defined.

- [ ] **Step 3: Implement the parser**

Add this above the `#[cfg(test)]` module in `src/zfs/permission.rs`:

```rust
fn split_perms(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Parses one indented entry line under `Local permissions:`/
/// `Descendent permissions:`/`Local+Descendent permissions:` into its
/// recipient and raw (unsplit) permission list — `user <name> <perms>`,
/// `group <name> <perms>`, or `everyone <perms>` (no name token).
/// Returns `None` for anything else, so the caller can skip a line it
/// doesn't recognize instead of panicking.
fn parse_recipient_line(line: &str) -> Option<(Recipient, Vec<String>)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("user"), Some(name), Some(perms)) => Some((Recipient::User(name.to_string()), split_perms(perms))),
        (Some("group"), Some(name), Some(perms)) => Some((Recipient::Group(name.to_string()), split_perms(perms))),
        (Some("everyone"), Some(perms), None) => Some((Recipient::Everyone, split_perms(perms))),
        _ => None,
    }
}

/// Parses `zfs allow <dataset>`'s output (confirmed live — no `-H`/`-p`
/// machine-readable mode exists for this command, unlike `zfs get`).
/// With nothing delegated, the command prints nothing at all (confirmed:
/// not even a banner line) — `parse_permissions("")` returns an
/// all-empty snapshot, not an error. With delegations present, a banner
/// line is followed by one of 5 exact section headers (no leading
/// whitespace, ends with `:`), each followed by indented entry lines
/// until the next header or end of output — confirmed live, including
/// that the `zfs-allow(8)` man page's own example for `-c` is wrong (it
/// shows `-c` output under `Permission sets:` with no `@` prefix; the
/// real header is the 5th one, `Create time permissions:`). Lenient:
/// an unrecognized header or a malformed entry line is skipped rather
/// than failing the whole parse, matching `zfs::share::parse_shares`'s
/// established leniency for the same reason.
pub fn parse_permissions(output: &str) -> PermissionsSnapshot {
    let mut snapshot = PermissionsSnapshot::default();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("----") {
            continue;
        }
        let is_header = !line.starts_with(char::is_whitespace) && line.trim_end().ends_with(':');
        if is_header {
            current = Some(line.trim().to_string());
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match current.as_deref() {
            Some("Permission sets:") => {
                if let Some((name, perms)) = trimmed.split_once(' ') {
                    if let Some(name) = name.strip_prefix('@') {
                        snapshot.sets.push(PermissionSet { name: name.to_string(), perms: split_perms(perms) });
                    }
                }
            }
            Some("Create time permissions:") => {
                snapshot.create_time = split_perms(trimmed);
            }
            Some("Local permissions:") => {
                if let Some((recipient, perms)) = parse_recipient_line(trimmed) {
                    snapshot.grants.push(PermissionGrant { recipient, scope: Scope::Local, perms });
                }
            }
            Some("Descendent permissions:") => {
                if let Some((recipient, perms)) = parse_recipient_line(trimmed) {
                    snapshot.grants.push(PermissionGrant { recipient, scope: Scope::Descendent, perms });
                }
            }
            Some("Local+Descendent permissions:") => {
                if let Some((recipient, perms)) = parse_recipient_line(trimmed) {
                    snapshot.grants.push(PermissionGrant { recipient, scope: Scope::Both, perms });
                }
            }
            _ => {}
        }
    }
    snapshot
}

/// Runs `zfs allow <dataset>` (the listing form — no dataset-type
/// filter needed or applicable, unlike `zfs::share::list_shares`: this
/// command only ever targets one already-known filesystem/volume, never
/// lists across the whole host).
pub fn list_permissions(runner: &dyn CommandRunner, dataset: &str) -> Result<PermissionsSnapshot> {
    let output = super::run_checked(runner, "zfs", &["allow", dataset], &format!("zfs allow {dataset}"))?;
    Ok(parse_permissions(&output.stdout))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib zfs::permission`
Expected: PASS (4 tests).

- [ ] **Step 5: Write the guard and command-building tests (failing)**

Add to the `#[cfg(test)] mod tests` block in `src/zfs/permission.rs`:

```rust
    #[test]
    fn allow_grant_user_local_builds_expected_args() {
        let runner = FakeRunner::new("");
        allow(
            &runner,
            "scratch/data",
            &PermissionTarget::Grant { recipient: Recipient::User("fb".to_string()), scope: Scope::Local },
            &["snapshot".to_string()],
        )
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "zfs".to_string(),
                vec!["allow".to_string(), "-l".to_string(), "-u".to_string(), "fb".to_string(), "snapshot".to_string(), "scratch/data".to_string()]
            )
        );
    }

    #[test]
    fn allow_grant_group_descendent_builds_expected_args() {
        let runner = FakeRunner::new("");
        allow(
            &runner,
            "scratch/data",
            &PermissionTarget::Grant { recipient: Recipient::Group("wheel".to_string()), scope: Scope::Descendent },
            &["create".to_string(), "destroy".to_string()],
        )
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "zfs".to_string(),
                vec!["allow".to_string(), "-d".to_string(), "-g".to_string(), "wheel".to_string(), "create,destroy".to_string(), "scratch/data".to_string()]
            )
        );
    }

    #[test]
    fn allow_grant_everyone_both_has_no_scope_flag_and_no_u_or_g() {
        let runner = FakeRunner::new("");
        allow(
            &runner,
            "scratch/data",
            &PermissionTarget::Grant { recipient: Recipient::Everyone, scope: Scope::Both },
            &["mount".to_string()],
        )
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["allow".to_string(), "everyone".to_string(), "mount".to_string(), "scratch/data".to_string()])
        );
    }

    #[test]
    fn allow_set_builds_expected_args() {
        let runner = FakeRunner::new("");
        allow(&runner, "scratch/data", &PermissionTarget::Set("admin".to_string()), &["snapshot".to_string(), "rollback".to_string()]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["allow".to_string(), "-s".to_string(), "@admin".to_string(), "snapshot,rollback".to_string(), "scratch/data".to_string()])
        );
    }

    #[test]
    fn allow_create_time_builds_expected_args() {
        let runner = FakeRunner::new("");
        allow(&runner, "scratch/data", &PermissionTarget::CreateTime, &["destroy".to_string()]).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["allow".to_string(), "-c".to_string(), "destroy".to_string(), "scratch/data".to_string()]));
    }

    #[test]
    fn unallow_full_revoke_omits_the_perms_argument() {
        let runner = FakeRunner::new("");
        unallow(&runner, "scratch/data", &PermissionTarget::Set("admin".to_string()), None).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("zfs".to_string(), vec!["unallow".to_string(), "-s".to_string(), "@admin".to_string(), "scratch/data".to_string()]));
    }

    #[test]
    fn unallow_specific_perms_includes_the_perms_argument() {
        let runner = FakeRunner::new("");
        unallow(
            &runner,
            "scratch/data",
            &PermissionTarget::Grant { recipient: Recipient::User("fb".to_string()), scope: Scope::Both },
            Some(&["snapshot".to_string()]),
        )
        .unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("zfs".to_string(), vec!["unallow".to_string(), "-u".to_string(), "fb".to_string(), "snapshot".to_string(), "scratch/data".to_string()])
        );
    }

    #[test]
    fn allow_rejects_permission_token_starting_with_dash() {
        let runner = FakeRunner::new("");
        let result = allow(&runner, "scratch/data", &PermissionTarget::CreateTime, &["-weird".to_string()]);
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "an invalid token must never reach the command");
    }

    #[test]
    fn allow_rejects_user_name_starting_with_dash() {
        let runner = FakeRunner::new("");
        let result = allow(
            &runner,
            "scratch/data",
            &PermissionTarget::Grant { recipient: Recipient::User("-weird".to_string()), scope: Scope::Both },
            &["snapshot".to_string()],
        );
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn unallow_rejects_set_name_starting_with_dash() {
        let runner = FakeRunner::new("");
        let result = unallow(&runner, "scratch/data", &PermissionTarget::Set("-weird".to_string()), None);
        assert!(result.is_err());
        assert!(runner.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn list_permissions_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_permissions(&runner, "scratch/data").is_err());
    }
```

(`FakeRunner::new`/`FakeRunner::failing`/`.calls` match the existing style used throughout `src/zfs/*.rs` — see e.g. `src/zfs/pool.rs`'s tests for the same pattern.)

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test --lib zfs::permission`
Expected: compile error — `allow`/`unallow`/`is_safe_permission_arg` are not defined.

- [ ] **Step 7: Implement the guard and command builders**

Add above the `#[cfg(test)]` module in `src/zfs/permission.rs` (after `list_permissions`):

```rust
/// Returns true if `s` is safe to use as one caller-supplied token (a
/// recipient name, permission-set name, or individual permission name)
/// in a `zfs allow`/`unallow` command. Confirmed live that `zfs allow`/
/// `unallow` do NOT honor `--` as an end-of-options marker — a third
/// confirmed exception alongside `zfs set`/`zpool set` (`zfs allow -u fb
/// -- snapshot scratch/data` fails with `"wrong number of parameters"`).
/// Rejecting a leading `-` before the token ever reaches the command is
/// the only guard that structurally eliminates flag misinterpretation
/// without needing to know exactly how this command's own non-standard
/// argument parser would react to one. This is a different, simpler
/// check than `zfs::snapshot`'s `is_safe_zfs_target`/`is_safe_host` (a
/// full character-allowlist) — those exist because `replicate_snapshot`
/// hand-builds a raw shell pipeline string, bypassing the normal safe
/// argv-array exec path every other mutation (including this one) uses.
fn is_safe_permission_arg(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-')
}

/// Builds the flag/value arguments for `target`, shared by `allow` and
/// `unallow` since both take the identical target-selection flags.
/// Validates every caller-supplied name along the way.
fn target_flag_args(target: &PermissionTarget) -> Result<Vec<String>> {
    let mut args = Vec::new();
    match target {
        PermissionTarget::Grant { recipient, scope } => {
            match scope {
                Scope::Local => args.push("-l".to_string()),
                Scope::Descendent => args.push("-d".to_string()),
                Scope::Both => {}
            }
            match recipient {
                Recipient::User(name) => {
                    if !is_safe_permission_arg(name) {
                        return Err(AppError::Parse(format!("invalid user name: {name:?}")));
                    }
                    args.push("-u".to_string());
                    args.push(name.clone());
                }
                Recipient::Group(name) => {
                    if !is_safe_permission_arg(name) {
                        return Err(AppError::Parse(format!("invalid group name: {name:?}")));
                    }
                    args.push("-g".to_string());
                    args.push(name.clone());
                }
                Recipient::Everyone => args.push("everyone".to_string()),
            }
        }
        PermissionTarget::Set(name) => {
            if !is_safe_permission_arg(name) {
                return Err(AppError::Parse(format!("invalid permission set name: {name:?}")));
            }
            args.push("-s".to_string());
            args.push(format!("@{name}"));
        }
        PermissionTarget::CreateTime => args.push("-c".to_string()),
    }
    Ok(args)
}

fn validate_perms(perms: &[String]) -> Result<()> {
    for p in perms {
        if !is_safe_permission_arg(p) {
            return Err(AppError::Parse(format!("invalid permission token: {p:?}")));
        }
    }
    Ok(())
}

/// Grants (or adds to an existing grant/set/create-time permission —
/// `zfs allow` is purely additive, confirmed live: see the spec) the
/// given `perms` for `target` on `dataset`.
pub fn allow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: &[String]) -> Result<()> {
    validate_perms(perms)?;
    let mut args = target_flag_args(target)?;
    args.push(perms.join(","));
    args.push(dataset.to_string());
    let mut full_args = vec!["allow".to_string()];
    full_args.extend(args);
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    super::run_checked(runner, "zfs", &arg_refs, &format!("zfs allow {dataset}"))?;
    Ok(())
}

/// Revokes `perms` for `target` on `dataset` — `None` revokes
/// everything for that target (confirmed live: this is what deletes a
/// whole permission set or removes a grant entirely), `Some(perms)`
/// revokes just those specific permissions, leaving the rest intact.
pub fn unallow(runner: &dyn CommandRunner, dataset: &str, target: &PermissionTarget, perms: Option<&[String]>) -> Result<()> {
    if let Some(perms) = perms {
        validate_perms(perms)?;
    }
    let mut args = target_flag_args(target)?;
    if let Some(perms) = perms {
        args.push(perms.join(","));
    }
    args.push(dataset.to_string());
    let mut full_args = vec!["unallow".to_string()];
    full_args.extend(args);
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
    super::run_checked(runner, "zfs", &arg_refs, &format!("zfs unallow {dataset}"))?;
    Ok(())
}
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --lib zfs::permission`
Expected: PASS (all 15 tests in this file).

- [ ] **Step 9: Register the module**

In `src/zfs/mod.rs`, change:

```rust
pub mod dataset;
pub mod disk;
pub mod pool;
pub mod property;
pub mod share;
pub mod snapshot;
```

to:

```rust
pub mod dataset;
pub mod disk;
pub mod permission;
pub mod pool;
pub mod property;
pub mod share;
pub mod snapshot;
```

- [ ] **Step 10: Run the full test suite**

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 11: Commit**

```bash
git add src/zfs/permission.rs src/zfs/mod.rs
git commit -m "feat: zfs::permission module — zfs allow/unallow parsing and command building"
```

## Task 2: `DatasetDetailTab` — state, refresh, and rendering

This task does **not** add any grant/revoke interactivity — it wires up the sub-tab itself (`'['`/`']'` to switch, `Up`/`Down` to navigate whichever table is showing, `'r'` to refresh) so the Permissions sub-tab is viewable end-to-end before Task 3 adds mutations. Mirrors `zfs::share`'s Task 2 in the Shares plan, which established the same "view first, interactivity later" split.

**Files:**
- Modify: `src/ui/state.rs` (new enum, new `Section` variant, new `App` fields, new `WorkerMsg` variant, new `refresh_dataset_permissions` method, `handle_key` arms, `drain_worker_messages` arm, `clear_host_data`)
- Modify: `src/ui/layout.rs` (sidebar label match, `draw_main`'s final match, `draw_dataset_properties` dispatch + new `draw_dataset_permissions`)
- Modify: `src/ui/mod.rs` (refresh dispatch)

**Interfaces:**
- Consumes: `crate::zfs::permission::{PermissionsSnapshot, PermissionGrant, PermissionSet, Recipient, Scope, list_permissions}` from Task 1.
- Produces: `pub enum DatasetDetailTab { Properties, Permissions }` (`Debug, Clone, Copy, PartialEq, Eq`, default `Properties`); `pub enum PermissionRow { Set { name: String, perms: Vec<String> }, CreateTime { perms: Vec<String> }, Grant { recipient: Recipient, scope: Scope, perms: Vec<String> } }` (`Debug, Clone, PartialEq`); new `App` fields `pub dataset_detail_tab: DatasetDetailTab`, `pub dataset_permissions: Vec<PermissionRow>`, `pub dataset_permissions_table: TableState`, `pub dataset_permissions_for: Option<String>`; `pub fn refresh_dataset_permissions(&mut self, runner: Arc<dyn CommandRunner>, dataset: String)`. Task 3's `handle_key` arms and `build_pending_action` read `dataset_permissions`/`dataset_permissions_table` and match on `PermissionRow` variants — do not rename these.

- [ ] **Step 1: Write the flatten-snapshot and refresh tests (failing)**

In `src/ui/state.rs`'s `#[cfg(test)] mod tests` block, add:

```rust
    #[test]
    fn flatten_permissions_snapshot_produces_one_row_per_entry_in_a_stable_order() {
        let snapshot = crate::zfs::permission::PermissionsSnapshot {
            sets: vec![crate::zfs::permission::PermissionSet { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }],
            create_time: vec!["destroy".to_string()],
            grants: vec![crate::zfs::permission::PermissionGrant {
                recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                scope: crate::zfs::permission::Scope::Both,
                perms: vec!["mount".to_string()],
            }],
        };
        let rows = flatten_permissions_snapshot(&snapshot);
        assert_eq!(
            rows,
            vec![
                PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] },
                PermissionRow::CreateTime { perms: vec!["destroy".to_string()] },
                PermissionRow::Grant {
                    recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                    scope: crate::zfs::permission::Scope::Both,
                    perms: vec!["mount".to_string()],
                },
            ]
        );
    }

    #[test]
    fn refresh_dataset_permissions_populates_app_state_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
            "---- Permissions on tank/data -------------------------------------\nLocal+Descendent permissions:\n        user fb snapshot\n",
        ));
        app.refresh_dataset_permissions(runner, "tank/data".to_string());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.dataset_permissions.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            app.dataset_permissions,
            vec![PermissionRow::Grant { recipient: crate::zfs::permission::Recipient::User("fb".to_string()), scope: crate::zfs::permission::Scope::Both, perms: vec!["snapshot".to_string()] }]
        );
        assert_eq!(app.dataset_permissions_for, Some("tank/data".to_string()));
    }

    #[test]
    fn dataset_permissions_result_for_a_stale_dataset_is_discarded() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx
            .send(WorkerMsg::DatasetPermissions(0, "tank/old".to_string(), Ok(crate::zfs::permission::PermissionsSnapshot::default())))
            .unwrap();
        app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.drain_worker_messages();
        assert_eq!(app.dataset_permissions_for, None, "a result for a dataset that's no longer selected must not populate the pane");
    }

    #[test]
    fn entering_right_focus_on_datasets_resets_to_properties_tab() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.dataset_detail_tab = DatasetDetailTab::Permissions;
        app.handle_key(KeyCode::Right);
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties, "re-entering the right pane must never silently keep a stale sub-tab");
    }

    #[test]
    fn bracket_keys_toggle_dataset_detail_tab_only_in_datasets_right_focus() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Char('[')); // Pools right pane has no sub-tab -- no-op
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);

        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // enter right focus
        app.handle_key(KeyCode::Char(']'));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
        app.handle_key(KeyCode::Char('['));
        assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ui::state`
Expected: compile error — `flatten_permissions_snapshot`, `PermissionRow`, `DatasetDetailTab`, `WorkerMsg::DatasetPermissions`, `refresh_dataset_permissions`, `dataset_permissions`/`dataset_permissions_for`/`dataset_detail_tab` are not defined.

- [ ] **Step 3: Add the `Section` variant, enums, and `App` fields**

In `src/ui/state.rs`, change the `Section` enum:

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

to:

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
    DatasetPermissions,
}
```

Add these new types near the top of the file, right after the `PaneFocus` enum:

```rust
/// Which view the Datasets right pane shows. Meaningful only while
/// `current_section() == Section::Datasets && pane_focus ==
/// PaneFocus::Right` — the Pools right pane has exactly one view and
/// never gains this concept. `'['`/`']'` cycle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DatasetDetailTab {
    #[default]
    Properties,
    Permissions,
}

/// One flattened, display/selection-ready row for the Permissions
/// sub-tab's table — `zfs::permission::PermissionsSnapshot`'s three
/// buckets (sets, create-time, grants) collapsed into a single
/// selectable list the moment a refresh result arrives, so the table
/// and `'d'`/`'x'` never need to re-derive it from the snapshot shape.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionRow {
    Set { name: String, perms: Vec<String> },
    CreateTime { perms: Vec<String> },
    Grant { recipient: crate::zfs::permission::Recipient, scope: crate::zfs::permission::Scope, perms: Vec<String> },
}

/// Flattens a `PermissionsSnapshot` into one `Vec<PermissionRow>` in a
/// stable order (sets, then create-time, then grants) for display and
/// selection.
fn flatten_permissions_snapshot(snapshot: &crate::zfs::permission::PermissionsSnapshot) -> Vec<PermissionRow> {
    let mut rows = Vec::new();
    for set in &snapshot.sets {
        rows.push(PermissionRow::Set { name: set.name.clone(), perms: set.perms.clone() });
    }
    if !snapshot.create_time.is_empty() {
        rows.push(PermissionRow::CreateTime { perms: snapshot.create_time.clone() });
    }
    for grant in &snapshot.grants {
        rows.push(PermissionRow::Grant { recipient: grant.recipient.clone(), scope: grant.scope, perms: grant.perms.clone() });
    }
    rows
}
```

Add a new `WorkerMsg` variant — change:

```rust
    /// The `String` is the dataset name this fetch was for.
    DatasetProperties(usize, String, Result<Vec<Property>>),
    MutationDone(usize, Section, Result<()>),
```

to:

```rust
    /// The `String` is the dataset name this fetch was for.
    DatasetProperties(usize, String, Result<Vec<Property>>),
    /// The `String` is the dataset name this fetch was for — same
    /// stale-identity discard rationale as `DatasetProperties`.
    DatasetPermissions(usize, String, Result<crate::zfs::permission::PermissionsSnapshot>),
    MutationDone(usize, Section, Result<()>),
```

Add new `App` fields — change:

```rust
    pub dataset_properties: Vec<Property>,
    pub dataset_properties_table: TableState,
```

to:

```rust
    pub dataset_properties: Vec<Property>,
    pub dataset_properties_table: TableState,
    pub dataset_detail_tab: DatasetDetailTab,
    pub dataset_permissions: Vec<PermissionRow>,
    pub dataset_permissions_table: TableState,
```

(leave the line right after it, `pub dataset_properties_for: Option<String>,`, where it is, then add `pub dataset_permissions_for: Option<String>,` immediately after that line).

In `App::new`'s initializer, find:

```rust
            dataset_properties: Vec::new(),
            dataset_properties_table: TableState::default(),
            dataset_properties_for: None,
```

and change it to:

```rust
            dataset_properties: Vec::new(),
            dataset_properties_table: TableState::default(),
            dataset_detail_tab: DatasetDetailTab::default(),
            dataset_permissions: Vec::new(),
            dataset_permissions_table: TableState::default(),
            dataset_permissions_for: None,
            dataset_properties_for: None,
```

In `clear_host_data`, find:

```rust
        self.dataset_properties.clear();
        self.dataset_properties_table.select(None);
        self.dataset_properties_for = None;
    }
```

and change it to:

```rust
        self.dataset_properties.clear();
        self.dataset_properties_table.select(None);
        self.dataset_properties_for = None;
        self.dataset_detail_tab = DatasetDetailTab::default();
        self.dataset_permissions.clear();
        self.dataset_permissions_table.select(None);
        self.dataset_permissions_for = None;
    }
```

- [ ] **Step 4: Add `refresh_dataset_permissions`**

Right after `refresh_dataset_properties` in `src/ui/state.rs`:

```rust
    /// See `refresh_dataset_properties`.
    pub fn refresh_dataset_permissions(&mut self, runner: Arc<dyn CommandRunner>, dataset: String) {
        let host = self.selected_host;
        if self.is_refreshing(Section::DatasetPermissions, host) {
            self.pending_refresh.insert((Section::DatasetPermissions, host));
            return;
        }
        self.begin_refresh(Section::DatasetPermissions, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::permission::list_permissions(runner.as_ref(), &dataset);
            let _ = tx.send(WorkerMsg::DatasetPermissions(host, dataset, result));
        });
    }
```

- [ ] **Step 5: Add the `drain_worker_messages` arm**

Right after the `WorkerMsg::DatasetProperties(...)` arm in `drain_worker_messages`, add:

```rust
                WorkerMsg::DatasetPermissions(host, dataset, result) => {
                    self.end_refresh(Section::DatasetPermissions, host);
                    let had_pending = self.pending_refresh.remove(&(Section::DatasetPermissions, host));
                    if host != self.selected_host {
                        continue;
                    }
                    if had_pending {
                        refreshes_needed.push(Section::DatasetPermissions);
                    }
                    if Some(&dataset) != self.selected_dataset_name().as_ref() {
                        continue;
                    }
                    match result {
                        Ok(snapshot) => {
                            self.dataset_permissions = flatten_permissions_snapshot(&snapshot);
                            clamp_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len());
                            self.dataset_permissions_for = Some(dataset);
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
```

- [ ] **Step 6: Add the sub-tab-cycle and tab-aware navigation `handle_key` arms**

Find the Datasets Right-focus entry inside the `KeyCode::Right` arm:

```rust
                    Section::Datasets => {
                        self.dataset_properties.clear();
                        self.dataset_properties_table.select(None);
                        self.dataset_properties_for = None;
                    }
```

and change it to:

```rust
                    Section::Datasets => {
                        self.dataset_properties.clear();
                        self.dataset_properties_table.select(None);
                        self.dataset_properties_for = None;
                        self.dataset_detail_tab = DatasetDetailTab::default();
                        self.dataset_permissions.clear();
                        self.dataset_permissions_table.select(None);
                        self.dataset_permissions_for = None;
                    }
```

Find the two Datasets Middle-focus selection-change arms:

```rust
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
            }
```

and change them to:

```rust
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
                self.dataset_permissions.clear();
                self.dataset_permissions_table.select(None);
                self.dataset_permissions_for = None;
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
                self.dataset_properties.clear();
                self.dataset_properties_table.select(None);
                self.dataset_properties_for = None;
                self.dataset_permissions.clear();
                self.dataset_permissions_table.select(None);
                self.dataset_permissions_for = None;
            }
```

Find the tab-unaware Datasets Right-focus Up/Down arms:

```rust
            KeyCode::Down if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), false);
            }
```

and change them to be tab-aware:

```rust
            KeyCode::Down if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => match self.dataset_detail_tab {
                DatasetDetailTab::Properties => move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), true),
                DatasetDetailTab::Permissions => move_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len(), true),
            },
            KeyCode::Up if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => match self.dataset_detail_tab {
                DatasetDetailTab::Properties => move_table_selection(&mut self.dataset_properties_table, self.dataset_properties.len(), false),
                DatasetDetailTab::Permissions => move_table_selection(&mut self.dataset_permissions_table, self.dataset_permissions.len(), false),
            },
```

Add the sub-tab-cycle keys — since there are only 2 tabs today, both keys toggle identically; this is still correctly extensible if a 3rd tab is added later. Add right after those two arms:

```rust
            KeyCode::Char('[') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                self.dataset_detail_tab = match self.dataset_detail_tab {
                    DatasetDetailTab::Properties => DatasetDetailTab::Permissions,
                    DatasetDetailTab::Permissions => DatasetDetailTab::Properties,
                };
            }
            KeyCode::Char(']') if self.current_section() == Section::Datasets && self.pane_focus == PaneFocus::Right => {
                self.dataset_detail_tab = match self.dataset_detail_tab {
                    DatasetDetailTab::Properties => DatasetDetailTab::Permissions,
                    DatasetDetailTab::Permissions => DatasetDetailTab::Properties,
                };
            }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --lib ui::state`
Expected: PASS (5 new tests, plus all pre-existing tests still passing — in particular re-check `enter_on_nfs_share_row_opens_prefilled_edit_form`-style Shares tests and any `BackTab`-wrap tests are unaffected, since `DatasetPermissions` is a drill-down-only tag never added to `App::sections`, exactly like `DatasetProperties`/`PoolProperties`/`Vdevs` — no `BackTab`-wrap mechanical fixes are needed this time).

- [ ] **Step 8: Wire the refresh dispatch in `src/ui/mod.rs`**

Find the queued-refresh dispatch match:

```rust
                    Section::DatasetProperties => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                        }
                    }
                    Section::Hosts => {}
```

and change it to:

```rust
                    Section::DatasetProperties => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                        }
                    }
                    Section::DatasetPermissions => {
                        if let Some(dataset) = app.selected_dataset_name() {
                            app.refresh_dataset_permissions(host.runner.clone(), dataset);
                        }
                    }
                    Section::Hosts => {}
```

Find the `'r'`-key dispatch's Right-focus match:

```rust
                                match app.current_section() {
                                    Section::Pools => {
                                        if let Some(pool) = app.selected_pool_name() {
                                            app.refresh_pool_properties(host.runner.clone(), pool);
                                        }
                                    }
                                    Section::Datasets => {
                                        if let Some(dataset) = app.selected_dataset_name() {
                                            app.refresh_dataset_properties(host.runner.clone(), dataset);
                                        }
                                    }
                                    _ => {}
                                }
```

and change the `Section::Datasets` arm to be tab-aware:

```rust
                                match app.current_section() {
                                    Section::Pools => {
                                        if let Some(pool) = app.selected_pool_name() {
                                            app.refresh_pool_properties(host.runner.clone(), pool);
                                        }
                                    }
                                    Section::Datasets => match app.dataset_detail_tab {
                                        DatasetDetailTab::Properties => {
                                            if let Some(dataset) = app.selected_dataset_name() {
                                                app.refresh_dataset_properties(host.runner.clone(), dataset);
                                            }
                                        }
                                        DatasetDetailTab::Permissions => {
                                            if let Some(dataset) = app.selected_dataset_name() {
                                                app.refresh_dataset_permissions(host.runner.clone(), dataset);
                                            }
                                        }
                                    },
                                    _ => {}
                                }
```

Find the `'r'`-key dispatch's non-Right-focus wildcard group:

```rust
                                    Section::Hosts
                                    | Section::Vdevs
                                    | Section::PoolProperties
                                    | Section::DatasetProperties => {}
```

and change it to:

```rust
                                    Section::Hosts
                                    | Section::Vdevs
                                    | Section::PoolProperties
                                    | Section::DatasetProperties
                                    | Section::DatasetPermissions => {}
```

Find the `KeyCode::Right`-just-pressed dispatch block:

```rust
                    if key.code == KeyCode::Right && app.pane_focus == PaneFocus::Right {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            match app.current_section() {
                                Section::Pools => {
                                    if let Some(pool) = app.selected_pool_name() {
                                        app.refresh_pool_properties(host.runner.clone(), pool);
                                    }
                                }
                                Section::Datasets => {
                                    if let Some(dataset) = app.selected_dataset_name() {
                                        app.refresh_dataset_properties(host.runner.clone(), dataset);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
```

This one is unaffected — `handle_key` always resets `dataset_detail_tab` to `Properties` on Right-focus entry (Step 6), so this unconditional `refresh_dataset_properties` call is still exactly correct. Leave it as-is. Immediately after this block, add a new one for the sub-tab-cycle keys:

```rust
                    if (key.code == KeyCode::Char('[') || key.code == KeyCode::Char(']'))
                        && app.pane_focus == PaneFocus::Right
                        && app.current_section() == Section::Datasets
                        && app.dataset_detail_tab == DatasetDetailTab::Permissions
                    {
                        if let Some(host) = inventory.hosts.get(app.selected_host) {
                            if let Some(dataset) = app.selected_dataset_name() {
                                app.refresh_dataset_permissions(host.runner.clone(), dataset);
                            }
                        }
                    }
```

(Switching back to `Properties` needs no refetch — that data is still valid, it didn't go stale just from flipping tabs.)

Add the import this file needs — find the `use` block at the top of `src/ui/mod.rs` that imports `Section`/`PaneFocus` from `crate::ui::state` (or wherever `Section`/`PaneFocus` are currently imported in this file) and add `DatasetDetailTab` alongside them.

- [ ] **Step 9: Add the rendering in `src/ui/layout.rs`**

In the sidebar label match, find:

```rust
                Section::DatasetProperties => "Dataset Properties",
```

and change it to add a new arm right after:

```rust
                Section::DatasetProperties => "Dataset Properties",
                Section::DatasetPermissions => "Dataset Permissions",
```

In `draw_main`'s final exhaustive match, find:

```rust
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties => {
            unreachable!("never in App::sections")
        }
```

and change it to:

```rust
        Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions => {
            unreachable!("never in App::sections")
        }
```

Change `draw_dataset_properties` to dispatch on the active sub-tab — find:

```rust
fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.dataset_properties.iter().map(|p| {
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_properties_table);
}
```

and change it to:

```rust
fn draw_dataset_properties(f: &mut Frame, app: &mut App, area: Rect) {
    if app.dataset_detail_tab == DatasetDetailTab::Permissions {
        draw_dataset_permissions(f, app, area);
        return;
    }
    let rows = app.dataset_properties.iter().map(|p| {
        Row::new(vec![p.name.clone(), p.value.clone(), p.source.to_string()]).style(read_only_property_style(p))
    });
    let widths = [Constraint::Length(16), Constraint::Length(16), Constraint::Min(10)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Properties — {dataset} ('r' refresh, Enter to edit, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Property", "Value", "Source"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_properties_table);
}

fn draw_dataset_permissions(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.dataset_permissions.iter().map(|row| match row {
        PermissionRow::Set { name, perms } => Row::new(vec![format!("Set @{name}"), "—".to_string(), perms.join(",")]),
        PermissionRow::CreateTime { perms } => Row::new(vec!["Create-time".to_string(), "—".to_string(), perms.join(",")]),
        PermissionRow::Grant { recipient, scope, perms } => {
            let target = match recipient {
                crate::zfs::permission::Recipient::User(name) => format!("user {name}"),
                crate::zfs::permission::Recipient::Group(name) => format!("group {name}"),
                crate::zfs::permission::Recipient::Everyone => "everyone".to_string(),
            };
            let scope_label = match scope {
                crate::zfs::permission::Scope::Local => "Local",
                crate::zfs::permission::Scope::Descendent => "Descendent",
                crate::zfs::permission::Scope::Both => "Local+Descendent",
            };
            Row::new(vec![target, scope_label.to_string(), perms.join(",")])
        }
    });
    let widths = [Constraint::Length(20), Constraint::Length(18), Constraint::Min(16)];
    let dataset = app.selected_dataset_name().unwrap_or_else(|| "?".to_string());
    let title = format!("Permissions — {dataset} ('r' refresh, n/s/c grant, d/x revoke, '['/']' tabs, ← back)");
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Target", "Scope", "Permissions"]))
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.dataset_permissions_table);
}
```

Add `PermissionRow`/`DatasetDetailTab` to this file's imports from `crate::ui::state` (wherever `Section`/`App` etc. are currently imported).

- [ ] **Step 10: Run the full test suite**

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 11: Commit**

```bash
git add src/ui/state.rs src/ui/layout.rs src/ui/mod.rs
git commit -m "feat: DatasetDetailTab — Permissions sub-tab state, refresh, and rendering"
```

## Task 3: Grant/revoke interactivity, `FormKind`/`MutationCall`, and README

**Files:**
- Modify: `src/ui/modal.rs` (`MutationCall` variants + `run` arms, `FormKind` variants)
- Modify: `src/ui/state.rs` (`build_pending_action` arms, `handle_key` arms for `'n'`/`'s'`/`'c'`/`'d'`/`'x'` in the Permissions sub-tab)
- Modify: `README.md` (keybindings — folded into this task's own steps, see Step 10 below; there is no separate README task in this plan)

**Interfaces:**
- Consumes: `crate::zfs::permission::{PermissionTarget, Recipient, Scope}` from Task 1; `PermissionRow`, `DatasetDetailTab` from Task 2.
- Produces: `MutationCall::AllowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Vec<String> }`, `MutationCall::UnallowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Option<Vec<String>> }`; `FormKind::GrantPermission { host: usize, dataset: String }` (fields `[Recipient, Scope, Permissions]`), `FormKind::GrantPermissionSet { host: usize, dataset: String }` (fields `[Set name, Permissions]`), `FormKind::GrantCreateTimePermission { host: usize, dataset: String }` (fields `[Permissions]`), `FormKind::RevokePermissions { host: usize, dataset: String, target: crate::zfs::permission::PermissionTarget }` (fields `[Permissions to remove]`).

- [ ] **Step 1: Write the `build_pending_action` tests (failing)**

In `src/ui/state.rs`'s test module, add:

```rust
    #[test]
    fn build_pending_action_grant_permission_requires_recipient_and_permissions() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let blank = vec![
            FormField { label: "Recipient".to_string(), value: String::new() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: String::new() },
        ];
        assert!(build_pending_action(&kind, &blank).is_none(), "blank Permissions must keep the form open");

        let no_recipient = vec![
            FormField { label: "Recipient".to_string(), value: "fb".to_string() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: "snapshot".to_string() },
        ];
        assert!(build_pending_action(&kind, &no_recipient).is_none(), "a bare name with no user:/group: prefix must keep the form open");
    }

    #[test]
    fn build_pending_action_grant_permission_assembles_user_local() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "user:fb".to_string() },
            FormField { label: "Scope".to_string(), value: "local".to_string() },
            FormField { label: "Permissions".to_string(), value: "snapshot, create".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Grant {
                        recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                        scope: crate::zfs::permission::Scope::Local,
                    },
                    perms: vec!["snapshot".to_string(), "create".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_grant_permission_blank_scope_defaults_to_both() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
            FormField { label: "Scope".to_string(), value: String::new() },
            FormField { label: "Permissions".to_string(), value: "mount".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action.call,
            MutationCall::AllowPermission {
                dataset: "tank/data".to_string(),
                target: crate::zfs::permission::PermissionTarget::Grant { recipient: crate::zfs::permission::Recipient::Everyone, scope: crate::zfs::permission::Scope::Both },
                perms: vec!["mount".to_string()],
            }
        );
    }

    #[test]
    fn build_pending_action_grant_permission_garbage_scope_keeps_form_open() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
            FormField { label: "Scope".to_string(), value: "lcoal".to_string() },
            FormField { label: "Permissions".to_string(), value: "mount".to_string() },
        ];
        assert!(build_pending_action(&kind, &fields).is_none(), "a scope typo must not silently fall back to Both");
    }

    #[test]
    fn build_pending_action_grant_permission_set_assembles() {
        let kind = FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![
            FormField { label: "Set name".to_string(), value: "admin".to_string() },
            FormField { label: "Permissions".to_string(), value: "snapshot,rollback".to_string() },
        ];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
                    perms: vec!["snapshot".to_string(), "rollback".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_grant_create_time_permission_assembles() {
        let kind = FormKind::GrantCreateTimePermission { host: 0, dataset: "tank/data".to_string() };
        let fields = vec![FormField { label: "Permissions".to_string(), value: "destroy".to_string() }];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::CreateTime,
                    perms: vec!["destroy".to_string()],
                },
            }
        );
    }

    #[test]
    fn build_pending_action_revoke_permissions_assembles() {
        let kind = FormKind::RevokePermissions {
            host: 0,
            dataset: "tank/data".to_string(),
            target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
        };
        let fields = vec![FormField { label: "Permissions to remove".to_string(), value: "rollback".to_string() }];
        let action = build_pending_action(&kind, &fields).unwrap();
        assert_eq!(
            action,
            PendingAction {
                host: 0,
                section: Section::DatasetPermissions,
                call: MutationCall::UnallowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()),
                    perms: Some(vec!["rollback".to_string()]),
                },
            }
        );
    }

    #[test]
    fn build_pending_action_revoke_permissions_blank_keeps_form_open() {
        let kind = FormKind::RevokePermissions { host: 0, dataset: "tank/data".to_string(), target: crate::zfs::permission::PermissionTarget::CreateTime };
        let fields = vec![FormField { label: "Permissions to remove".to_string(), value: String::new() }];
        assert!(build_pending_action(&kind, &fields).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ui::state`
Expected: compile error — `FormKind::GrantPermission` etc. and `MutationCall::AllowPermission`/`UnallowPermission` are not defined.

- [ ] **Step 3: Add the `MutationCall` and `FormKind` variants**

In `src/ui/modal.rs`, change the `MutationCall` enum — find:

```rust
    CreateSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
}
```

and change it to:

```rust
    CreateSnapshot { dataset: String, name: String },
    DestroySnapshot { dataset: String, name: String },
    RollbackSnapshot { dataset: String, name: String },
    AllowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Vec<String> },
    UnallowPermission { dataset: String, target: crate::zfs::permission::PermissionTarget, perms: Option<Vec<String>> },
}
```

In `MutationCall::run`'s match, find:

```rust
            MutationCall::RollbackSnapshot { dataset, name } => crate::zfs::snapshot::rollback_snapshot(runner, dataset, name),
        }
    }
}
```

and change it to:

```rust
            MutationCall::RollbackSnapshot { dataset, name } => crate::zfs::snapshot::rollback_snapshot(runner, dataset, name),
            MutationCall::AllowPermission { dataset, target, perms } => crate::zfs::permission::allow(runner, dataset, target, perms),
            MutationCall::UnallowPermission { dataset, target, perms } => {
                crate::zfs::permission::unallow(runner, dataset, target, perms.as_deref())
            }
        }
    }
}
```

In the `FormKind` enum, find the last variant:

```rust
    /// Fields: `[networks, hosts, read_only, root]` — `networks`/
    /// `hosts` are comma-separated lists, `read_only` is `"y"`/`"n"`
    /// text, `root` is a username; every field may be blank.
    ShareNfs { host: usize, dataset: String },
}
```

and change it to:

```rust
    /// Fields: `[networks, hosts, read_only, root]` — `networks`/
    /// `hosts` are comma-separated lists, `read_only` is `"y"`/`"n"`
    /// text, `root` is a username; every field may be blank.
    ShareNfs { host: usize, dataset: String },
    /// Fields: `[recipient, scope, permissions]` — `recipient` is
    /// `"everyone"` / `"user:<name>"` / `"group:<name>"`, `scope` is
    /// `"local"`/`"descendent"`/`"both"` (blank defaults to `"both"`),
    /// `permissions` is a required comma-separated list.
    GrantPermission { host: usize, dataset: String },
    /// Fields: `[set_name, permissions]` — both required; `set_name` is
    /// the bare name without the leading `@`.
    GrantPermissionSet { host: usize, dataset: String },
    /// Fields: `[permissions]` — required.
    GrantCreateTimePermission { host: usize, dataset: String },
    /// Fields: `[permissions]` — required; `target` is fixed at
    /// modal-open time from the selected Permissions-sub-tab row, not
    /// user-entered.
    RevokePermissions { host: usize, dataset: String, target: crate::zfs::permission::PermissionTarget },
}
```

- [ ] **Step 4: Run the tests to verify they still fail with the right error**

Run: `cargo test --lib ui::state`
Expected: compile error — `build_pending_action` has no match arm for the new `FormKind` variants yet.

- [ ] **Step 5: Implement the `build_pending_action` arms and their field-parsing helpers**

Add these parsing helpers near `build_pending_action` in `src/ui/state.rs` (private, same visibility as the function itself):

```rust
/// Parses the `Recipient` field's mini-language: `everyone`,
/// `user:<name>`, or `group:<name>` — no bare-name fallback, since this
/// app always passes explicit `-u`/`-g` to `zfs allow` (see the spec).
fn parse_recipient_field(s: &str) -> Option<crate::zfs::permission::Recipient> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("everyone") {
        return Some(crate::zfs::permission::Recipient::Everyone);
    }
    if let Some(name) = s.strip_prefix("user:") {
        if !name.is_empty() {
            return Some(crate::zfs::permission::Recipient::User(name.to_string()));
        }
        return None;
    }
    if let Some(name) = s.strip_prefix("group:") {
        if !name.is_empty() {
            return Some(crate::zfs::permission::Recipient::Group(name.to_string()));
        }
        return None;
    }
    None
}

/// Parses the `Scope` field — blank defaults to `Both`, anything else
/// must be exactly `local`/`descendent`/`both` (case-insensitive) or
/// the caller must treat it as invalid. Deliberately strict (unlike
/// most optional fields in this app): silently falling back to the
/// *widest* scope on a typo would be a real footgun for a permissions
/// feature specifically.
fn parse_scope_field(s: &str) -> Option<crate::zfs::permission::Scope> {
    let s = s.trim();
    if s.is_empty() {
        return Some(crate::zfs::permission::Scope::Both);
    }
    if s.eq_ignore_ascii_case("local") {
        return Some(crate::zfs::permission::Scope::Local);
    }
    if s.eq_ignore_ascii_case("descendent") {
        return Some(crate::zfs::permission::Scope::Descendent);
    }
    if s.eq_ignore_ascii_case("both") {
        return Some(crate::zfs::permission::Scope::Both);
    }
    None
}

/// Parses a required comma-separated `Permissions` field — splits,
/// trims, drops empty tokens; `None` if that leaves nothing (blank, or
/// just commas/whitespace), since there's no sensible "blank means X"
/// default for a permission list (unlike Shares' optional fields).
fn parse_required_perms_field(s: &str) -> Option<Vec<String>> {
    let perms: Vec<String> = s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    if perms.is_empty() {
        None
    } else {
        Some(perms)
    }
}
```

Add the four new match arms to `build_pending_action` — find the end of the function:

```rust
        FormKind::ShareNfs { host, dataset } => {
            let networks: Vec<String> = fields[0].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let hosts: Vec<String> = fields[1].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let read_only = fields[2].value.trim().to_lowercase().starts_with('y');
            let root = fields[3].value.trim();
            let root = if root.is_empty() { None } else { Some(root) };
            let value = crate::zfs::share::assemble_sharenfs(&networks, &hosts, read_only, root);
            Some(PendingAction { host: *host, section: Section::Shares, call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value } })
        }
    }
}
```

and change it to add the new arms before the closing `}`:

```rust
        FormKind::ShareNfs { host, dataset } => {
            let networks: Vec<String> = fields[0].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let hosts: Vec<String> = fields[1].value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let read_only = fields[2].value.trim().to_lowercase().starts_with('y');
            let root = fields[3].value.trim();
            let root = if root.is_empty() { None } else { Some(root) };
            let value = crate::zfs::share::assemble_sharenfs(&networks, &hosts, read_only, root);
            Some(PendingAction { host: *host, section: Section::Shares, call: MutationCall::SetDatasetProperty { dataset: dataset.clone(), key: "sharenfs".to_string(), value } })
        }
        FormKind::GrantPermission { host, dataset } => {
            let recipient = parse_recipient_field(&fields[0].value)?;
            let scope = parse_scope_field(&fields[1].value)?;
            let perms = parse_required_perms_field(&fields[2].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: dataset.clone(),
                    target: crate::zfs::permission::PermissionTarget::Grant { recipient, scope },
                    perms,
                },
            })
        }
        FormKind::GrantPermissionSet { host, dataset } => {
            let name = fields[0].value.trim();
            if name.is_empty() {
                return None;
            }
            let perms = parse_required_perms_field(&fields[1].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission {
                    dataset: dataset.clone(),
                    target: crate::zfs::permission::PermissionTarget::Set(name.to_string()),
                    perms,
                },
            })
        }
        FormKind::GrantCreateTimePermission { host, dataset } => {
            let perms = parse_required_perms_field(&fields[0].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::AllowPermission { dataset: dataset.clone(), target: crate::zfs::permission::PermissionTarget::CreateTime, perms },
            })
        }
        FormKind::RevokePermissions { host, dataset, target } => {
            let perms = parse_required_perms_field(&fields[0].value)?;
            Some(PendingAction {
                host: *host,
                section: Section::DatasetPermissions,
                call: MutationCall::UnallowPermission { dataset: dataset.clone(), target: target.clone(), perms: Some(perms) },
            })
        }
    }
}
```

Note: `build_pending_action` returns `Option<PendingAction>` and the existing arms use `return None;` inside an `if` — the new helpers return `Option<T>` so `?` works directly inside this function (it already returns `Option<PendingAction>`, confirmed from the existing arms' shape).

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib ui::state`
Expected: PASS (9 new tests from Step 1).

- [ ] **Step 7: Write the `handle_key` tests for `'n'`/`'s'`/`'c'`/`'d'`/`'x'` (failing)**

Add to `src/ui/state.rs`'s test module:

```rust
    fn enter_datasets_permissions_tab(app: &mut App) {
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.handle_key(KeyCode::Right); // -> Right focus, Properties tab
        app.handle_key(KeyCode::Char(']')); // -> Permissions tab
    }

    #[test]
    fn handle_key_n_on_permissions_tab_with_no_dataset_selected_does_not_panic_or_open_form() {
        let mut app = App::new(vec!["local".to_string()]);
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_n_on_permissions_tab_opens_grant_permission_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('n'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 3);
                assert_eq!(*kind, FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_s_on_permissions_tab_opens_grant_permission_set_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('s'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(*kind, FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_c_on_permissions_tab_opens_create_time_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('c'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(*kind, FormKind::GrantCreateTimePermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_d_and_x_on_permissions_tab_with_no_row_selected_do_not_panic() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
        app.handle_key(KeyCode::Char('x'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn handle_key_d_on_selected_permission_row_opens_full_revoke_confirm() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.dataset_permissions = vec![PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }];
        app.dataset_permissions_table.select(Some(0));
        app.handle_key(KeyCode::Char('d'));
        match &app.modal {
            Some(Modal::Confirm { action, .. }) => {
                assert_eq!(
                    action.call,
                    MutationCall::UnallowPermission { dataset: "tank/data".to_string(), target: crate::zfs::permission::PermissionTarget::Set("admin".to_string()), perms: None }
                );
            }
            other => panic!("expected Confirm modal, got {other:?}"),
        }
    }

    #[test]
    fn handle_key_x_on_selected_permission_row_opens_revoke_specific_form() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        enter_datasets_permissions_tab(&mut app);
        app.dataset_permissions = vec![PermissionRow::Grant {
            recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
            scope: crate::zfs::permission::Scope::Both,
            perms: vec!["snapshot".to_string(), "mount".to_string()],
        }];
        app.dataset_permissions_table.select(Some(0));
        app.handle_key(KeyCode::Char('x'));
        match &app.modal {
            Some(Modal::Form { fields, kind, .. }) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(
                    *kind,
                    FormKind::RevokePermissions {
                        host: 0,
                        dataset: "tank/data".to_string(),
                        target: crate::zfs::permission::PermissionTarget::Grant { recipient: crate::zfs::permission::Recipient::User("fb".to_string()), scope: crate::zfs::permission::Scope::Both },
                    }
                );
            }
            other => panic!("expected Form modal, got {other:?}"),
        }
    }
```

- [ ] **Step 8: Run the tests to verify they fail**

Run: `cargo test --lib ui::state`
Expected: compile error or assertion failure — the `'n'`/`'s'`/`'c'`/`'d'`/`'x'` keys don't have Permissions-sub-tab arms yet.

- [ ] **Step 9: Implement the `handle_key` arms**

Add these arms to `handle_key` in `src/ui/state.rs`, right after the sub-tab-cycle (`'['`/`']'`) arms added in Task 2:

```rust
            KeyCode::Char('n')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant permission on '{dataset}'"),
                        fields: vec![
                            FormField { label: "Recipient (everyone / user:<name> / group:<name>)".to_string(), value: String::new() },
                            FormField { label: "Scope (local/descendent/both, blank=both)".to_string(), value: String::new() },
                            FormField { label: "Permissions".to_string(), value: String::new() },
                        ],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantPermission { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('s')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant/add-to permission set on '{dataset}'"),
                        fields: vec![
                            FormField { label: "Set name (without @)".to_string(), value: String::new() },
                            FormField { label: "Permissions".to_string(), value: String::new() },
                        ],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantPermissionSet { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('c')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let Some(dataset) = self.selected_dataset_name() {
                    self.modal = Some(Modal::Form {
                        title: format!("Grant create-time permission on '{dataset}'"),
                        fields: vec![FormField { label: "Permissions".to_string(), value: String::new() }],
                        focused: 0,
                        description: None,
                        kind: FormKind::GrantCreateTimePermission { host: self.selected_host, dataset },
                    });
                }
            }
            KeyCode::Char('d')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_permissions_table.selected()) {
                    if let Some(row) = self.dataset_permissions.get(i) {
                        let target = permission_target_for_row(row);
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Revoke all permissions for this target on '{dataset}'? (y/n)"),
                            action: PendingAction {
                                host: self.selected_host,
                                section: Section::DatasetPermissions,
                                call: MutationCall::UnallowPermission { dataset, target, perms: None },
                            },
                        });
                    }
                }
            }
            KeyCode::Char('x')
                if self.current_section() == Section::Datasets
                    && self.pane_focus == PaneFocus::Right
                    && self.dataset_detail_tab == DatasetDetailTab::Permissions =>
            {
                if let (Some(dataset), Some(i)) = (self.selected_dataset_name(), self.dataset_permissions_table.selected()) {
                    if let Some(row) = self.dataset_permissions.get(i) {
                        let target = permission_target_for_row(row);
                        self.modal = Some(Modal::Form {
                            title: format!("Revoke specific permissions on '{dataset}'"),
                            fields: vec![FormField { label: "Permissions to remove".to_string(), value: String::new() }],
                            focused: 0,
                            description: None,
                            kind: FormKind::RevokePermissions { host: self.selected_host, dataset, target },
                        });
                    }
                }
            }
```

Add the helper function `permission_target_for_row` near `flatten_permissions_snapshot` (Task 2):

```rust
/// Reconstructs the `PermissionTarget` a `PermissionRow` was flattened
/// from — used by `'d'`/`'x'` to revoke exactly the target the selected
/// row represents.
fn permission_target_for_row(row: &PermissionRow) -> crate::zfs::permission::PermissionTarget {
    match row {
        PermissionRow::Set { name, .. } => crate::zfs::permission::PermissionTarget::Set(name.clone()),
        PermissionRow::CreateTime { .. } => crate::zfs::permission::PermissionTarget::CreateTime,
        PermissionRow::Grant { recipient, scope, .. } => {
            crate::zfs::permission::PermissionTarget::Grant { recipient: recipient.clone(), scope: *scope }
        }
    }
}
```

- [ ] **Step 10: Run the tests to verify they pass, then run the full suite**

Run: `cargo test --lib ui::state`
Expected: PASS (7 new tests from Step 7).

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 11: Update `README.md`**

In the `## Keys` section, find:

```markdown
- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
```

and change it to:

```markdown
- `Tab` / `Shift+Tab`: switch sections (Hosts, Pools, Datasets, Snapshots, Disks, Shares) — this is now the *only* way to cycle sections; `Left`/`Right` have a different meaning (see below).
- `'['` / `']'` (Datasets right pane only, once focused with `Right`): cycle between the Properties and Permissions sub-tabs.
```

Find the `### Pool/Dataset properties pane` section:

```markdown
### Pool/Dataset properties pane (right pane, `Right` to focus)

- `Up` / `Down`: select a property
- `Enter`: edit the selected property's value (prompts pre-filled with the current value; submitting runs `zpool set`/`zfs set`)
- `r`: re-fetch properties for the currently-selected pool/dataset
- `Esc` / `Left`: back out to the middle pane
```

and change it to:

```markdown
### Pool/Dataset properties pane (right pane, `Right` to focus)

- `Up` / `Down`: select a property (or, on the Datasets Permissions sub-tab, a permission row — see below)
- `Enter`: edit the selected property's value (prompts pre-filled with the current value; submitting runs `zpool set`/`zfs set`)
- `r`: re-fetch properties (or permissions, on that sub-tab) for the currently-selected pool/dataset
- `Esc` / `Left`: back out to the middle pane

#### Datasets' Permissions sub-tab (`'['`/`']'` to switch to it)

Delegated ZFS administration permissions (`zfs allow`/`unallow`) for the
selected dataset — who can run which ZFS subcommands/properties on it.
Not available for Pools (`zfs allow` only targets a filesystem or
volume).

- `n`: grant a permission to a user/group/everyone (prompts for
  Recipient — `everyone` / `user:<name>` / `group:<name>` — Scope —
  `local`/`descendent`/`both`, blank means `both` — and Permissions, a
  required comma-separated list)
- `s`: define or add to a named permission set (prompts for the set
  name without `@`, and Permissions)
- `c`: add a create-time permission — auto-granted to whoever creates a
  new child dataset under this one (prompts for Permissions only)
- `d`: revoke everything for the selected row's target (y/n confirm)
- `x`: revoke just specific permissions from the selected row, leaving
  the rest intact (prompts for which permissions to remove)

Granting is always additive (never a replace) — this mirrors how `zfs
allow` itself works: granting a user two different permissions in two
separate calls merges into one combined grant, it never overwrites.
```

- [ ] **Step 12: Commit**

```bash
git add src/ui/modal.rs src/ui/state.rs README.md
git commit -m "feat: Permissions sub-tab interactivity — grant, revoke, and revoke-specific"
```
