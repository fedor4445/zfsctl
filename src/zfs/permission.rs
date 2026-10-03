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

/// Extracts the dataset name from a `"---- Permissions on <name> ----"`
/// banner line. ZFS names never contain spaces, so the first space
/// after the fixed prefix is always the boundary before the trailing
/// dash padding.
fn parse_banner_dataset(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("---- Permissions on ")?;
    let end = rest.find(' ')?;
    Some(&rest[..end])
}

/// Parses `zfs allow <dataset>`'s output (confirmed live — no `-H`/`-p`
/// machine-readable mode exists for this command, unlike `zfs get`).
/// With nothing delegated, the command prints nothing at all (confirmed:
/// not even a banner line) — `parse_permissions("", dataset)` returns an
/// all-empty snapshot, not an error. With delegations present, a banner
/// line (`"---- Permissions on <name> ----"`) is followed by one of 5
/// exact section headers (no leading whitespace, ends with `:`), each
/// followed by indented entry lines until the next header or end of
/// output — confirmed live, including that the `zfs-allow(8)` man
/// page's own example for `-c` is wrong (it shows `-c` output under
/// `Permission sets:` with no `@` prefix; the real header is the 5th
/// one, `Create time permissions:`).
///
/// **`zfs allow <dataset>` prints one such block per ancestor dataset
/// that has delegations too, not just `<dataset>`'s own** (confirmed
/// against the OpenZFS source: `dsl_deleg_get` walks up the dataset
/// tree and `print_fs_perms` loops over every result) — `dataset` is
/// the name the caller actually asked about, and only the block whose
/// banner names it is parsed; every other block (an ancestor's own
/// delegations) is skipped entirely, never merged in as if it were
/// `dataset`'s own.
///
/// Lenient: an unrecognized header or a malformed entry line is skipped
/// rather than failing the whole parse, matching
/// `zfs::share::parse_shares`'s established leniency for the same
/// reason.
pub fn parse_permissions(output: &str, dataset: &str) -> PermissionsSnapshot {
    let mut snapshot = PermissionsSnapshot::default();
    let mut current: Option<String> = None;
    let mut in_target_block = false;
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with("----") {
            in_target_block = parse_banner_dataset(line) == Some(dataset);
            current = None;
            continue;
        }
        if !in_target_block {
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
    Ok(parse_permissions(&output.stdout, dataset))
}

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
        assert_eq!(parse_permissions("", "scratch/data"), PermissionsSnapshot::default());
    }

    #[test]
    fn parse_permissions_fixture_covers_all_five_sections() {
        let snapshot = parse_permissions(PERMISSIONS_FIXTURE, "scratch/data");
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
        let snapshot = parse_permissions(output, "scratch/data");
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
        let snapshot = parse_permissions(output, "scratch/data");
        assert_eq!(
            snapshot.grants,
            vec![PermissionGrant { recipient: Recipient::Group("wheel".to_string()), scope: Scope::Local, perms: vec!["create".to_string(), "destroy".to_string()] }],
            "a malformed entry line must be skipped, not panic or corrupt the next valid entry"
        );
    }

    #[test]
    fn parse_permissions_ignores_blocks_for_ancestor_datasets() {
        // `zfs allow <child>` prints one "---- Permissions on X ----"
        // block per ancestor that has delegations, not just the
        // child's own (confirmed via dsl_deleg_get walking up the
        // dataset tree). A parent's grants must never be merged in as
        // if they belonged to the child being viewed.
        let output = "\
---- Permissions on scratch -------------------------------------------
Local+Descendent permissions:
        user fb snapshot
---- Permissions on scratch/data -------------------------------------
Local permissions:
        user fb mount
";
        let snapshot = parse_permissions(output, "scratch/data");
        assert_eq!(
            snapshot.grants,
            vec![PermissionGrant { recipient: Recipient::User("fb".to_string()), scope: Scope::Local, perms: vec!["mount".to_string()] }],
            "the ancestor block (scratch) must not be merged into scratch/data's own permissions"
        );
    }

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
}
