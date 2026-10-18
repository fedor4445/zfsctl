use crate::ui::modal::FormKind;
use crate::zfs::accounts::HostAccounts;

#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub values: Vec<String>,
    pub filter: String,
    pub selected: usize,
    pub checked: std::collections::HashSet<String>,
    pub multi_select: bool,
    pub load: PickerLoad,
    pub note: Option<String>,
    pub account: Option<(usize, AccountField)>,
    /// Set on the job form's Pool picker: the host whose pool names it lists, fetched live.
    pub pool_host: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountField {
    User,
    ZfsAllowRecipient,
    AclRecipient,
}

/// What a live-fetched name picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveNames {
    Pools,
    Datasets,
    /// The backup form's Source field: `new`, the prefixes in use and every snapshot of this dataset.
    Snapshots(String),
}

/// What the backup form's Source picker offers for a dataset whose snapshots are `names` (the parts after
/// `@`): `new`, then each prefix that snapshots named `<prefix>-<timestamp>` use (newest first), then every
/// snapshot as `@name`, newest first.
pub fn source_choices(names: &[String]) -> Vec<String> {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let mut out = vec![crate::jobs::SOURCE_NEW.to_string()];
    for name in &sorted {
        if let Some((prefix, _)) = name.rsplit_once('-') {
            if crate::daemon::snapshot::is_own(prefix, name) && !out.iter().any(|v| v == prefix) {
                out.push(prefix.to_string());
            }
        }
    }
    out.extend(sorted.iter().map(|n| format!("@{n}")));
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerLoad {
    Ready,
    Loading,
    Failed(String),
}

/// Which live account list (and for which host) a form field's picker is
/// sourced from, if any. Keyed on `(kind, field_index)` like `vocabulary_for`.
pub fn account_field_for(kind: &FormKind, field_index: usize) -> Option<(usize, AccountField)> {
    match (kind, field_index) {
        (FormKind::ShareSmb { host, .. }, 1) | (FormKind::ShareNfs { host, .. }, 3) => Some((*host, AccountField::User)),
        (FormKind::GrantPermission { host, .. }, 0) => Some((*host, AccountField::ZfsAllowRecipient)),
        (FormKind::AddAclEntry { host, .. }, 0) => Some((*host, AccountField::AclRecipient)),
        _ => None,
    }
}

/// The displayed list for an account picker. Names that
/// `smb::validate_user` rejects (spaces, `\`, `$`, a leading `-`, ...) are
/// never offered: they could be selected and then refused by the form's
/// own parser.
pub fn build_values(field: AccountField, accounts: &HostAccounts) -> Vec<String> {
    let safe = |a: &&crate::zfs::accounts::Account| crate::zfs::smb::validate_user(&a.name).is_ok();
    let users = accounts.users.iter().filter(safe).map(|a| a.name.clone());
    let groups = accounts.groups.iter().filter(safe).map(|a| a.name.clone());
    match field {
        AccountField::User => users.collect(),
        AccountField::ZfsAllowRecipient => std::iter::once("everyone".to_string())
            .chain(users.map(|n| format!("user:{n}")))
            .chain(groups.map(|n| format!("group:{n}")))
            .collect(),
        AccountField::AclRecipient => ["owner", "group", "everyone"]
            .into_iter()
            .map(str::to_string)
            .chain(users.map(|n| format!("user:{n}")))
            .chain(groups.map(|n| format!("group:{n}")))
            .collect(),
    }
}

pub fn fill_account_picker(picker: &mut PickerState, accounts: &HostAccounts, current_value: &str) {
    let Some((_, field)) = picker.account else { return };
    picker.values = build_values(field, accounts);
    picker.selected = picker.values.iter().position(|v| v == current_value.trim()).unwrap_or(0);
    let mut notes: Vec<String> = accounts.group_error.iter().map(|e| format!("groups unavailable: {e}")).collect();
    if picker.values.is_empty() {
        notes.push("no selectable accounts".to_string());
    }
    picker.note = if notes.is_empty() { None } else { Some(notes.join("; ")) };
    picker.load = PickerLoad::Ready;
}

/// Confirmed live via `zfs allow` (no arguments -- `OpenZFS` prints its
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

/// The 14 `NFSv4` ACE permission names -- matches the vocabulary
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

const ON_OFF: &[&str] = &["on", "off"];
const FAILMODES: &[&str] = &["wait", "continue", "panic"];

/// The redundancy field of the two "from selected disks" forms. Blank means a plain stripe,
/// which is not a pickable value, so it stays a matter of leaving the field empty.
const REDUNDANCY: &[&str] = &["mirror", "raidz1", "raidz2", "raidz3"];
/// What a disk added to a pool can be (`zfs::vdev_role::Role`).
const ROLES: &[&str] = &["data", "log", "cache", "special", "dedup", "spare"];

/// The pool properties whose value is one of a fixed few, so F2 can offer them. Anything
/// else (`comment`, `bootfs`, `cachefile`, ...) is free text and gets no picker.
fn pool_property_values(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "autoexpand" | "autoreplace" | "autotrim" | "delegation" | "listsnapshots" | "multihost" => Some(ON_OFF),
        "failmode" => Some(FAILMODES),
        _ => None,
    }
}

/// Maps a form field (identified by its `FormKind` and positional
/// index) to the picker vocabulary attached to it, if any. Keys on
/// `(kind, field_index)`, never on a field's label text -- several
/// fields across different forms share the label "Permissions" but
/// mean different, non-interchangeable vocabularies.
pub fn vocabulary_for(kind: &FormKind, field_index: usize) -> Option<(&'static [&'static str], bool)> {
    match (kind, field_index) {
        (FormKind::GrantPermission { .. }, 2) |
(FormKind::GrantPermissionSet { .. }, 1) |
(FormKind::GrantCreateTimePermission { .. } | FormKind::RevokePermissions { ..
}, 0) => Some((ZFS_ALLOW_PERMISSIONS, true)),
        (FormKind::AddAclEntry { .. }, 1) => Some((ACL_PERMISSIONS, true)),
        (FormKind::AddAclEntry { .. }, 2) => Some((ACL_FLAGS, true)),
        (FormKind::AddAclEntry { .. }, 3) => Some((ACL_TYPES, false)),
        (FormKind::EditPoolProperty { key, .. }, 0) => pool_property_values(key).map(|v| (v, false)),
        (FormKind::CreatePoolFromDisks { .. } | FormKind::AddVdevFromDisks { .. }, 1) if matches!(kind, FormKind::CreatePoolFromDisks { .. }) => Some((REDUNDANCY, false)),
        (FormKind::AddVdevFromDisks { .. }, 1) => Some((ROLES, false)),
        (FormKind::AddVdevFromDisks { .. }, 2) => Some((REDUNDANCY, false)),
        _ => None,
    }
}

/// Which fields of a job form take what: configured hosts, a live-fetched dataset or pool name
/// (the host being whatever the form's host field says at F2 time), and a schedule preset.
struct JobPickers<'a> {
    hosts: &'a [String],
    host_fields: &'static [usize],
    live_fields: &'static [usize],
    schedule_field: usize,
}

/// Builds the picker state for opening `F2` on `field_index` of a form
/// of kind `kind`, pre-seeded from `current_value` (whatever's already
/// typed into that field) -- so `F2` on a half-typed field doesn't
/// discard what's there. Returns `None` if the field has no attached
/// vocabulary (the caller treats this as `F2` being a no-op).
pub fn build_picker_state(kind: &FormKind, field_index: usize, current_value: &str) -> Option<PickerState> {
    if let Some((host, field)) = account_field_for(kind, field_index) {
        return Some(PickerState {
            values: Vec::new(),
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: PickerLoad::Loading,
            note: None,
            account: Some((host, field)),
            pool_host: None,
        });
    }
    let job_form = match kind {
        FormKind::Job { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0], live_fields: &[1], schedule_field: 2 }),
        FormKind::SnapshotJob { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0], live_fields: &[1], schedule_field: 5 }),
        FormKind::ReplicateJob { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0, 3], live_fields: &[1, 4], schedule_field: 6 }),
        FormKind::BackupJob { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0], live_fields: &[1, 3], schedule_field: 8 }),
        FormKind::MirrorJob { hosts, .. } => Some(JobPickers { hosts, host_fields: &[0], live_fields: &[1, 3], schedule_field: 7 }),
        _ => None,
    };
    if let Some(spec) = job_form {
        if spec.live_fields.contains(&field_index) {
            // `open_pool_picker` points this at the right host and queues the fetch.
            return Some(PickerState { values: Vec::new(), filter: String::new(), selected: 0, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Loading, note: None, account: None, pool_host: Some(0) });
        }
        let (values, selected) = if spec.host_fields.contains(&field_index) {
            let mut values = spec.hosts.to_vec();
            if matches!(kind, FormKind::ReplicateJob { .. }) && field_index == 3 {
                values.push(crate::jobs::DEST_FILE.to_string());
            }
            let selected = values.iter().position(|h| h == current_value.trim()).unwrap_or(0);
            (values, selected)
        } else if field_index == spec.schedule_field {
            let presets = crate::ui::jobs::preset_values();
            let selected = presets.iter().position(|p| crate::ui::jobs::insert_text(p) == current_value.trim()).unwrap_or(0);
            (presets, selected)
        } else {
            return None;
        };
        return Some(PickerState { values, filter: String::new(), selected, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Ready, note: None, account: None, pool_host: None });
    }
    if let FormKind::CreateBootEnv { existing, .. } = kind {
        if field_index != 1 {
            return None;
        }
        let selected = existing.iter().position(|e| e == current_value.trim()).unwrap_or(0);
        return Some(PickerState { values: existing.clone(), filter: String::new(), selected, checked: std::collections::HashSet::new(), multi_select: false, load: PickerLoad::Ready, note: None, account: None, pool_host: None });
    }
    let (values, multi_select) = vocabulary_for(kind, field_index)?;
    let values: Vec<String> = values.iter().map(std::string::ToString::to_string).collect();
    let mut checked = std::collections::HashSet::new();
    let mut selected = 0usize;
    if multi_select {
        for piece in current_value.split(',').map(str::trim) {
            if values.iter().any(|v| v == piece) {
                checked.insert(piece.to_string());
            }
        }
    } else if let Some(idx) = values.iter().position(|v| v == current_value.trim()) {
        selected = idx;
    }
    Some(PickerState { values, filter: String::new(), selected, checked, multi_select, load: PickerLoad::Ready, note: None, account: None, pool_host: None })
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
    fn pool_properties_with_a_fixed_set_of_values_offer_them_and_free_text_ones_do_not() {
        let edit = |key: &str| FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: key.to_string() };
        assert_eq!(vocabulary_for(&edit("autoexpand"), 0), Some((&["on", "off"][..], false)));
        assert_eq!(vocabulary_for(&edit("failmode"), 0), Some((&["wait", "continue", "panic"][..], false)));
        assert_eq!(vocabulary_for(&edit("comment"), 0), None);
        assert_eq!(vocabulary_for(&edit("bootfs"), 0), None);
    }

    #[test]
    fn the_redundancy_field_of_both_from_disks_forms_offers_the_layouts_but_the_name_field_does_not() {
        let redundancy = Some((&["mirror", "raidz1", "raidz2", "raidz3"][..], false));
        let create = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string()] };
        let add = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] };
        assert_eq!(vocabulary_for(&create, 1), redundancy);
        // the Add vdev form has a Role field before the redundancy
        assert_eq!(vocabulary_for(&add, 1), Some((&["data", "log", "cache", "special", "dedup", "spare"][..], false)));
        assert_eq!(vocabulary_for(&add, 2), redundancy);
        assert_eq!(vocabulary_for(&create, 0), None);
        assert_eq!(vocabulary_for(&add, 0), None);
    }

    #[test]
    fn f2_on_a_pool_property_preselects_its_current_value() {
        let kind = FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: "failmode".to_string() };
        let picker = build_picker_state(&kind, 0, "continue").unwrap();
        assert_eq!(picker.values[picker.selected], "continue");
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
        assert_eq!(build_picker_state(&kind, 1, ""), None);
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

    use crate::zfs::accounts::Account;

    fn acct(name: &str, id: u32) -> Account {
        Account { name: name.to_string(), id }
    }
    fn sample() -> HostAccounts {
        HostAccounts { users: vec![acct("carol", 1003), acct("root", 0), acct("we ird", 1500)], groups: vec![acct("staff", 1100)], group_error: None }
    }

    #[test]
    fn account_field_for_maps_exactly_the_four_live_fields_and_carries_the_host() {
        let smb = FormKind::ShareSmb { host: 2, dataset: "d".into(), path: "/d".into(), existing_user: None };
        let nfs = FormKind::ShareNfs { host: 1, dataset: "d".into() };
        let grant = FormKind::GrantPermission { host: 3, dataset: "d".into() };
        let acl = FormKind::AddAclEntry { host: 4, path: "/d".into(), position: 0 };
        assert_eq!(account_field_for(&smb, 1), Some((2, AccountField::User)));
        assert_eq!(account_field_for(&nfs, 3), Some((1, AccountField::User)));
        assert_eq!(account_field_for(&grant, 0), Some((3, AccountField::ZfsAllowRecipient)));
        assert_eq!(account_field_for(&acl, 0), Some((4, AccountField::AclRecipient)));
        assert_eq!(account_field_for(&smb, 0), None);
        assert_eq!(account_field_for(&smb, 2), None, "password field");
        assert_eq!(account_field_for(&nfs, 0), None);
        assert_eq!(account_field_for(&grant, 1), None);
        assert_eq!(account_field_for(&acl, 1), None);
    }

    #[test]
    fn build_picker_state_for_an_account_field_is_a_loading_single_select_skeleton() {
        let kind = FormKind::ShareSmb { host: 2, dataset: "d".into(), path: "/d".into(), existing_user: None };
        let p = build_picker_state(&kind, 1, "").unwrap();
        assert_eq!(p.load, PickerLoad::Loading);
        assert!(p.values.is_empty() && !p.multi_select);
        assert_eq!(p.account, Some((2, AccountField::User)));
    }

    #[test]
    fn static_pickers_are_ready_with_no_account() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "d".into() };
        let p = build_picker_state(&kind, 2, "").unwrap();
        assert_eq!((p.load, p.account, p.note), (PickerLoad::Ready, None, None));
    }

    #[test]
    fn build_values_orders_and_prefixes_per_field_and_drops_unsafe_names() {
        let s = sample();
        assert_eq!(build_values(AccountField::User, &s), vec!["carol", "root"]);
        assert_eq!(
            build_values(AccountField::ZfsAllowRecipient, &s),
            vec!["everyone", "user:carol", "user:root", "group:staff"]
        );
        assert_eq!(
            build_values(AccountField::AclRecipient, &s),
            vec!["owner", "group", "everyone", "user:carol", "user:root", "group:staff"]
        );
    }

    #[test]
    fn fill_account_picker_sets_values_ready_preselects_the_current_value_and_notes_group_errors() {
        let mut p = build_picker_state(&FormKind::ShareNfs { host: 0, dataset: "d".into() }, 3, "").unwrap();
        let mut accounts = sample();
        accounts.group_error = Some("boom".to_string());
        fill_account_picker(&mut p, &accounts, " root ");
        assert_eq!(p.load, PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert_eq!(p.selected, 1, "current text matches 'root'");
        assert_eq!(p.note.as_deref(), Some("groups unavailable: boom"));
    }

    #[test]
    fn fill_account_picker_notes_when_nothing_is_selectable() {
        let mut p = build_picker_state(&FormKind::ShareNfs { host: 0, dataset: "d".into() }, 3, "").unwrap();
        let mut accounts = sample();
        accounts.users.clear();
        fill_account_picker(&mut p, &accounts, "");
        assert!(p.values.is_empty());
        assert_eq!(p.note.as_deref(), Some("no selectable accounts"));

        accounts.group_error = Some("boom".to_string());
        fill_account_picker(&mut p, &accounts, "");
        assert_eq!(p.note.as_deref(), Some("groups unavailable: boom; no selectable accounts"));
    }

    #[test]
    fn the_clone_from_field_offers_the_existing_boot_environments_and_the_name_field_does_not() {
        let kind = FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string(), "spare".to_string()] };
        assert!(build_picker_state(&kind, 0, "").is_none());
        let picker = build_picker_state(&kind, 1, "spare").unwrap();
        assert_eq!(picker.values, vec!["default", "spare"]);
        assert_eq!(picker.selected, 1, "pre-seeded from what is typed");
        assert!(!picker.multi_select);
        let rename = FormKind::RenameBootEnv { host: 0, from: "a".to_string(), existing: vec![] };
        assert!(build_picker_state(&rename, 0, "").is_none());
    }

    #[test]
    fn the_backup_form_offers_hosts_live_datasets_live_sources_and_schedule_presets() {
        let kind = FormKind::BackupJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: None, jobs: vec![] };
        assert_eq!(build_picker_state(&kind, 0, "fbsd16").unwrap().values, vec!["local", "fbsd16"]);
        assert!(build_picker_state(&kind, 1, "").unwrap().pool_host.is_some(), "the dataset field fetches live names");
        assert!(build_picker_state(&kind, 3, "").unwrap().pool_host.is_some(), "the source field fetches the dataset's snapshots");
        assert!(!build_picker_state(&kind, 8, "").unwrap().values.is_empty(), "schedule presets");
        for text_field in [2, 4, 5, 6, 7, 9] {
            assert!(build_picker_state(&kind, text_field, "").is_none(), "field {text_field} has no vocabulary");
        }
    }

    #[test]
    fn the_source_choices_are_new_then_the_prefixes_in_use_then_every_snapshot_newest_first() {
        let names: Vec<String> = ["snap4", "hourly-20261008T100000Z", "auto-20261001T000000Z", "hourly-20261008T110000Z", "manual-backup"].iter().map(ToString::to_string).collect();
        assert_eq!(
            source_choices(&names),
            ["new", "snap4x", "hourly", "auto", "@snap4", "@manual-backup", "@hourly-20261008T110000Z", "@hourly-20261008T100000Z", "@auto-20261001T000000Z"].iter().filter(|v| **v != "snap4x").map(ToString::to_string).collect::<Vec<_>>()
        );
        assert_eq!(source_choices(&[]), ["new"]);
    }
}
