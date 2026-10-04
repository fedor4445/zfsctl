use crate::ui::modal::FormKind;

#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub values: Vec<String>,
    pub filter: String,
    pub selected: usize,
    pub checked: std::collections::HashSet<String>,
    pub multi_select: bool,
}

/// Confirmed live via `zfs allow` (no arguments -- OpenZFS prints its
/// full permission/property usage text) on the real FreeBSD test box,
/// 2026-10-04. 96 entries: 19 subcommands, 13 quota/user-property
/// "other" permissions, 64 delegable properties.
const ZFS_ALLOW_PERMISSIONS: &[&str] = &[
    "allow", "clone", "create", "destroy", "diff", "hold", "mount", "promote", "receive", "release", "rename",
    "rollback", "send", "send:raw", "share", "snapshot", "bookmark", "load-key", "change-key",
    "groupquota", "groupused", "userprop", "userquota", "userused", "userobjquota", "userobjused",
    "groupobjquota", "groupobjused", "projectused", "projectquota", "projectobjused", "projectobjquota",
    "aclinherit", "aclmode", "acltype", "atime", "canmount", "casesensitivity", "checksum", "compression",
    "context", "copies", "dedup", "defaultgroupobjquota", "defaultgroupquota", "defaultprojectobjquota",
    "defaultprojectquota", "defaultuserobjquota", "defaultuserquota", "defcontext", "devices", "direct",
    "dnodesize", "encryption", "exec", "filesystem_limit", "fscontext", "jailed", "keyformat", "keylocation",
    "logbias", "longname", "mlslabel", "mountpoint", "nbmand", "normalization", "overlay", "pbkdf2iters",
    "prefetch", "primarycache", "quota", "readonly", "recordsize", "redundant_metadata", "refquota",
    "refreservation", "relatime", "reservation", "rootcontext", "secondarycache", "setuid", "sharenfs",
    "sharesmb", "snapdev", "snapdir", "snapshot_limit", "special_small_blocks", "sync", "utf8only", "version",
    "volblocksize", "volmode", "volsize", "volthreading", "vscan", "xattr",
];

/// The 14 NFSv4 ACE permission names -- matches the vocabulary
/// confirmed live against real `getfacl -v` output during the File
/// ACLs feature (`docs/superpowers/specs/2026-10-03-file-acls-design.md`).
const ACL_PERMISSIONS: &[&str] = &[
    "read_data", "write_data", "append_data", "read_xattr", "write_xattr", "execute", "delete_child",
    "read_attributes", "write_attributes", "delete", "read_acl", "write_acl", "write_owner", "synchronize",
];

/// `inherited` is deliberately excluded -- it's a system-applied
/// marker for an entry inherited *from* a parent, not something
/// meaningful to manually set when creating a new entry.
const ACL_FLAGS: &[&str] = &["file_inherit", "dir_inherit", "inherit_only", "no_propagate"];

const ACL_TYPES: &[&str] = &["allow", "deny"];

/// Maps a form field (identified by its `FormKind` and positional
/// index) to the picker vocabulary attached to it, if any. Keys on
/// `(kind, field_index)`, never on a field's label text -- several
/// fields across different forms share the label "Permissions" but
/// mean different, non-interchangeable vocabularies.
pub fn vocabulary_for(kind: &FormKind, field_index: usize) -> Option<(&'static [&'static str], bool)> {
    match (kind, field_index) {
        (FormKind::GrantPermission { .. }, 2)
        | (FormKind::GrantPermissionSet { .. }, 1)
        | (FormKind::GrantCreateTimePermission { .. }, 0)
        | (FormKind::RevokePermissions { .. }, 0) => Some((ZFS_ALLOW_PERMISSIONS, true)),
        (FormKind::AddAclEntry { .. }, 1) => Some((ACL_PERMISSIONS, true)),
        (FormKind::AddAclEntry { .. }, 2) => Some((ACL_FLAGS, true)),
        (FormKind::AddAclEntry { .. }, 3) => Some((ACL_TYPES, false)),
        _ => None,
    }
}

/// Builds the picker state for opening `F2` on `field_index` of a form
/// of kind `kind`, pre-seeded from `current_value` (whatever's already
/// typed into that field) -- so `F2` on a half-typed field doesn't
/// discard what's there. Returns `None` if the field has no attached
/// vocabulary (the caller treats this as `F2` being a no-op).
pub fn build_picker_state(kind: &FormKind, field_index: usize, current_value: &str) -> Option<PickerState> {
    let (values, multi_select) = vocabulary_for(kind, field_index)?;
    let values: Vec<String> = values.iter().map(|s| s.to_string()).collect();
    let mut checked = std::collections::HashSet::new();
    let mut selected = 0usize;
    if multi_select {
        for piece in current_value.split(',').map(|p| p.trim()) {
            if values.iter().any(|v| v == piece) {
                checked.insert(piece.to_string());
            }
        }
    } else if let Some(idx) = values.iter().position(|v| v == current_value.trim()) {
        selected = idx;
    }
    Some(PickerState { values, filter: String::new(), selected, checked, multi_select })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_for_grant_permission_permissions_field_is_zfs_allow_multi_select() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let (values, multi_select) = vocabulary_for(&kind, 2).unwrap();
        assert!(multi_select);
        assert!(values.contains(&"create"));
        assert!(values.contains(&"quota"));
        assert_eq!(values.len(), 96);
    }

    #[test]
    fn vocabulary_for_grant_permission_recipient_field_is_none() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        assert_eq!(vocabulary_for(&kind, 0), None);
    }

    #[test]
    fn vocabulary_for_grant_permission_scope_field_is_none() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        assert_eq!(vocabulary_for(&kind, 1), None);
    }

    #[test]
    fn vocabulary_for_grant_permission_set_permissions_field_is_zfs_allow_multi_select() {
        let kind = FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() };
        let (values, multi_select) = vocabulary_for(&kind, 1).unwrap();
        assert!(multi_select);
        assert!(values.contains(&"snapshot"));
    }

    #[test]
    fn vocabulary_for_grant_create_time_permission_field_is_zfs_allow_multi_select() {
        let kind = FormKind::GrantCreateTimePermission { host: 0, dataset: "tank/data".to_string() };
        let (values, multi_select) = vocabulary_for(&kind, 0).unwrap();
        assert!(multi_select);
        assert!(values.contains(&"mount"));
    }

    #[test]
    fn vocabulary_for_revoke_permissions_field_is_zfs_allow_multi_select() {
        let kind = FormKind::RevokePermissions {
            host: 0,
            dataset: "tank/data".to_string(),
            target: crate::zfs::permission::PermissionTarget::CreateTime,
        };
        let (values, multi_select) = vocabulary_for(&kind, 0).unwrap();
        assert!(multi_select);
        assert!(values.contains(&"rollback"));
    }

    #[test]
    fn vocabulary_for_add_acl_entry_recipient_field_is_none() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        assert_eq!(vocabulary_for(&kind, 0), None);
    }

    #[test]
    fn vocabulary_for_add_acl_entry_permissions_field_is_acl_permissions_multi_select() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let (values, multi_select) = vocabulary_for(&kind, 1).unwrap();
        assert!(multi_select);
        assert_eq!(values.len(), 14);
        assert!(values.contains(&"read_data"));
        assert!(values.contains(&"synchronize"));
    }

    #[test]
    fn vocabulary_for_add_acl_entry_flags_field_is_acl_flags_multi_select() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let (values, multi_select) = vocabulary_for(&kind, 2).unwrap();
        assert!(multi_select);
        assert_eq!(values, &["file_inherit", "dir_inherit", "inherit_only", "no_propagate"]);
    }

    #[test]
    fn vocabulary_for_add_acl_entry_type_field_is_acl_types_single_select() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let (values, multi_select) = vocabulary_for(&kind, 3).unwrap();
        assert!(!multi_select);
        assert_eq!(values, &["allow", "deny"]);
    }

    #[test]
    fn vocabulary_for_add_acl_entry_out_of_range_field_is_none() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        assert_eq!(vocabulary_for(&kind, 4), None);
    }

    #[test]
    fn vocabulary_for_add_host_is_none_for_every_field() {
        let kind = FormKind::AddHost;
        assert_eq!(vocabulary_for(&kind, 0), None);
        assert_eq!(vocabulary_for(&kind, 1), None);
    }

    #[test]
    fn build_picker_state_returns_none_for_a_field_with_no_vocabulary() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        assert_eq!(build_picker_state(&kind, 0, ""), None);
    }

    #[test]
    fn build_picker_state_for_a_blank_multi_select_field_starts_with_nothing_checked() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let picker = build_picker_state(&kind, 2, "").unwrap();
        assert!(picker.multi_select);
        assert!(picker.checked.is_empty());
        assert_eq!(picker.filter, "");
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn build_picker_state_for_a_prefilled_multi_select_field_precheck_matching_values() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let picker = build_picker_state(&kind, 2, "create, destroy").unwrap();
        assert_eq!(picker.checked, std::collections::HashSet::from(["create".to_string(), "destroy".to_string()]));
    }

    #[test]
    fn build_picker_state_for_a_prefilled_multi_select_field_ignores_unrecognized_pieces() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
        let picker = build_picker_state(&kind, 2, "create, not-a-real-permission").unwrap();
        assert_eq!(picker.checked, std::collections::HashSet::from(["create".to_string()]));
    }

    #[test]
    fn build_picker_state_for_a_blank_single_select_field_selects_index_zero() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let picker = build_picker_state(&kind, 3, "").unwrap();
        assert!(!picker.multi_select);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn build_picker_state_for_a_prefilled_single_select_field_preselects_the_match() {
        let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
        let picker = build_picker_state(&kind, 3, "deny").unwrap();
        assert_eq!(picker.selected, 1); // ACL_TYPES = ["allow", "deny"]
    }
}
