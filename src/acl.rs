use crate::error::Result;
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclTag {
    Owner,
    OwningGroup,
    Everyone,
    User(String),
    NamedGroup(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclEntryType {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AclEntry {
    pub tag: AclTag,
    pub permissions: Vec<String>,
    pub flags: Vec<String>,
    pub entry_type: AclEntryType,
}

fn split_slash_list(s: &str) -> Vec<String> {
    s.split('/').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

fn parse_acl_tag(tag: &str, qualifier: Option<&str>) -> Option<AclTag> {
    match (tag, qualifier) {
        ("owner@", None) => Some(AclTag::Owner),
        ("group@", None) => Some(AclTag::OwningGroup),
        ("everyone@", None) => Some(AclTag::Everyone),
        ("user", Some(name)) if !name.is_empty() => Some(AclTag::User(name.to_string())),
        ("group", Some(name)) if !name.is_empty() => Some(AclTag::NamedGroup(name.to_string())),
        _ => None,
    }
}

/// Parses one `getfacl -v` entry line (not a `#`-comment header line) —
/// confirmed live, 4 colon-separated fields for `owner@`/`group@`/
/// `everyone@` (no qualifier) or 5 fields for `user`/`group` (with a
/// qualifier): tag[:qualifier]:permissions:flags:type. Permissions and
/// flags are `/`-joined long-form names (confirmed live via `-v`); an
/// empty flags field is two adjacent colons. Returns `None` for
/// anything unrecognized, so the caller can skip a line it doesn't
/// understand instead of panicking.
fn parse_acl_line(line: &str) -> Option<AclEntry> {
    let fields: Vec<&str> = line.trim().split(':').collect();
    let (tag, perms_field, flags_field, type_field) = match fields.as_slice() {
        [tag, perms, flags, ty] => (parse_acl_tag(tag, None)?, *perms, *flags, *ty),
        [tag, qualifier, perms, flags, ty] => (parse_acl_tag(tag, Some(qualifier))?, *perms, *flags, *ty),
        _ => return None,
    };
    let entry_type = match type_field {
        "allow" => AclEntryType::Allow,
        "deny" => AclEntryType::Deny,
        _ => return None,
    };
    Some(AclEntry { tag, permissions: split_slash_list(perms_field), flags: split_slash_list(flags_field), entry_type })
}

/// Runs `getfacl -v <path>` (the `-v` flag is essential — confirmed
/// live it renders permissions/flags as named, `/`-separated long-form
/// tokens rather than the non-verbose form's undocumented fixed-width
/// positional letter codes) and parses every non-header, non-blank
/// line. Unlike `zfs::share::parse_shares`/`zfs::permission::parse_permissions`,
/// an unparseable *entry* line fails the whole list rather than being
/// skipped: the returned `Vec`'s index doubles as the `setfacl`
/// position for add/remove, so silently dropping one entry would make
/// every later entry's index point at the wrong position.
pub fn list_acl(runner: &dyn CommandRunner, path: &str) -> Result<Vec<AclEntry>> {
    let output = crate::zfs::run_checked(runner, "getfacl", &["-v", path], &format!("getfacl -v {path}"))?;
    output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| parse_acl_line(line).ok_or_else(|| crate::error::AppError::Parse(format!("unrecognized getfacl entry line: {line:?}"))))
        .collect()
}

fn render_acl_entry(entry: &AclEntry) -> String {
    let tag_str = match &entry.tag {
        AclTag::Owner => "owner@".to_string(),
        AclTag::OwningGroup => "group@".to_string(),
        AclTag::Everyone => "everyone@".to_string(),
        AclTag::User(name) => format!("user:{name}"),
        AclTag::NamedGroup(name) => format!("group:{name}"),
    };
    let type_str = match entry.entry_type {
        AclEntryType::Allow => "allow",
        AclEntryType::Deny => "deny",
    };
    format!("{tag_str}:{}:{}:{type_str}", entry.permissions.join("/"), entry.flags.join("/"))
}

/// Inserts `entry` at `position` (confirmed live: `setfacl -a
/// <position> "<entry>" <path>`, then `getfacl -v` read back the exact
/// same entry string unchanged, including flags and `deny`).
pub fn add_acl_entry(runner: &dyn CommandRunner, path: &str, position: usize, entry: &AclEntry) -> Result<()> {
    let position_str = position.to_string();
    let entry_str = render_acl_entry(entry);
    crate::zfs::run_checked(runner, "setfacl", &["-a", &position_str, &entry_str, path], &format!("setfacl -a {position} ... {path}"))?;
    Ok(())
}

/// Removes the entry at `position` (confirmed live: `setfacl -x
/// <position> <path>` removed exactly that entry, leaving every other
/// entry's position and content untouched).
pub fn remove_acl_entry(runner: &dyn CommandRunner, path: &str, position: usize) -> Result<()> {
    let position_str = position.to_string();
    crate::zfs::run_checked(runner, "setfacl", &["-x", &position_str, path], &format!("setfacl -x {position} {path}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const ACL_FIXTURE: &str = "\
# file: /scratch/data
# owner: root
# group: wheel
            owner@:read_data/write_data/execute::allow
            group@:read_data/execute::allow
         everyone@:read_data/execute::allow
";

    #[test]
    fn parse_acl_line_owner_entry_with_no_flags() {
        assert_eq!(
            parse_acl_line("            owner@:read_data/write_data/execute::allow"),
            Some(AclEntry {
                tag: AclTag::Owner,
                permissions: vec!["read_data".to_string(), "write_data".to_string(), "execute".to_string()],
                flags: vec![],
                entry_type: AclEntryType::Allow,
            })
        );
    }

    #[test]
    fn parse_acl_line_named_user_entry_with_flags_and_deny() {
        // Confirmed live: setfacl -a 0 "user:nobody:read_data/write_data:file_inherit/dir_inherit:deny" /scratch/data
        // then getfacl -v round-tripped this exact string back.
        assert_eq!(
            parse_acl_line("       user:nobody:read_data/write_data:file_inherit/dir_inherit:deny"),
            Some(AclEntry {
                tag: AclTag::User("nobody".to_string()),
                permissions: vec!["read_data".to_string(), "write_data".to_string()],
                flags: vec!["file_inherit".to_string(), "dir_inherit".to_string()],
                entry_type: AclEntryType::Deny,
            })
        );
    }

    #[test]
    fn parse_acl_line_named_group_entry() {
        assert_eq!(
            parse_acl_line("       group:admins:read_data:inherited:allow"),
            Some(AclEntry {
                tag: AclTag::NamedGroup("admins".to_string()),
                permissions: vec!["read_data".to_string()],
                flags: vec!["inherited".to_string()],
                entry_type: AclEntryType::Allow,
            })
        );
    }

    #[test]
    fn parse_acl_line_ignores_malformed_line() {
        assert_eq!(parse_acl_line("this line has no recognizable shape at all"), None);
    }

    #[test]
    fn list_acl_fixture_parses_all_entries_skipping_header_lines() {
        let entries = list_acl(&FakeRunner::new(ACL_FIXTURE), "/scratch/data").unwrap();
        assert_eq!(
            entries,
            vec![
                AclEntry { tag: AclTag::Owner, permissions: vec!["read_data".to_string(), "write_data".to_string(), "execute".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
                AclEntry { tag: AclTag::OwningGroup, permissions: vec!["read_data".to_string(), "execute".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
                AclEntry { tag: AclTag::Everyone, permissions: vec!["read_data".to_string(), "execute".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
            ]
        );
    }

    #[test]
    fn list_acl_surfaces_command_failure() {
        let runner = FakeRunner::failing("permission denied", 1);
        assert!(list_acl(&runner, "/scratch/data").is_err());
    }

    #[test]
    fn list_acl_fails_rather_than_silently_shifting_positions_past_an_unparseable_entry_line() {
        // Caught by a final review: silently skipping an unparseable
        // *entry* line (as opposed to a `#`-comment header line) means
        // every later entry's Vec index no longer matches its real
        // `setfacl` position -- `d` on a later row would then remove
        // the wrong entry. Position correctness matters more than
        // leniency here, unlike zfs::share/zfs::permission's parsers.
        let fixture = "\
# file: /scratch/data
            owner@:read_data::allow
       this-is-not-a-recognizable-acl-entry-line
            group@:read_data::allow
";
        assert!(list_acl(&FakeRunner::new(fixture), "/scratch/data").is_err());
    }

    #[test]
    fn add_acl_entry_builds_expected_args() {
        let runner = FakeRunner::new("");
        let entry = AclEntry {
            tag: AclTag::User("nobody".to_string()),
            permissions: vec!["read_data".to_string(), "write_data".to_string()],
            flags: vec!["file_inherit".to_string(), "dir_inherit".to_string()],
            entry_type: AclEntryType::Deny,
        };
        add_acl_entry(&runner, "/scratch/data", 0, &entry).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                "setfacl".to_string(),
                vec![
                    "-a".to_string(),
                    "0".to_string(),
                    "user:nobody:read_data/write_data:file_inherit/dir_inherit:deny".to_string(),
                    "/scratch/data".to_string(),
                ]
            )
        );
    }

    #[test]
    fn add_acl_entry_with_no_qualifier_and_no_flags() {
        let runner = FakeRunner::new("");
        let entry = AclEntry { tag: AclTag::Everyone, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow };
        add_acl_entry(&runner, "/scratch/data", 3, &entry).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            ("setfacl".to_string(), vec!["-a".to_string(), "3".to_string(), "everyone@:read_data::allow".to_string(), "/scratch/data".to_string()])
        );
    }

    #[test]
    fn remove_acl_entry_builds_expected_args() {
        let runner = FakeRunner::new("");
        remove_acl_entry(&runner, "/scratch/data", 2).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("setfacl".to_string(), vec!["-x".to_string(), "2".to_string(), "/scratch/data".to_string()]));
    }

    #[test]
    fn parse_acl_line_rejects_a_named_user_or_group_with_an_empty_qualifier() {
        assert_eq!(parse_acl_line("user::read_data::allow"), None, "4 fields with tag `user` has no qualifier");
        assert_eq!(parse_acl_line("user::read_data::allow::"), None);
        assert_eq!(parse_acl_tag("user", Some("")), None);
        assert_eq!(parse_acl_tag("group", Some("")), None);
        assert_eq!(parse_acl_tag("user", Some("bob")), Some(AclTag::User("bob".to_string())));
        assert_eq!(parse_acl_tag("group", Some("wheel")), Some(AclTag::NamedGroup("wheel".to_string())));
    }
}
