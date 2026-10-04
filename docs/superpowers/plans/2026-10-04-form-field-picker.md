# Form Field Picker (F2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `F2` open a filterable picker popup for form fields whose valid values come from a known vocabulary (`zfs allow`'s `Permissions`, and the ACL form's `Permissions`/`Flags`/`Type`), so a user can browse/select instead of typing exact names from memory.

**Architecture:** A new `src/ui/picker.rs` module owns the pure data (`PickerState`, the four vocabulary constants, and the lookup/pre-seeding logic) — fully unit-testable with no terminal or SSH involved. `App` gains a `picker: Option<PickerState>` field, separate from `modal`, so the picker renders on top of a form that stays open underneath. The event loop routes keys to the picker first whenever it's open, falling through to the existing modal/key handling otherwise.

**Tech Stack:** Rust (2021 edition), ratatui 0.29, crossterm 0.28 — no new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-04-form-field-picker-design.md`.

## Global Constraints

- This crate has **no `[lib]` target** — every test command in this plan is plain `cargo test ...`, never `cargo test --lib ...`.
- **Exactly four fields get a picker in this pass**: `GrantPermission`'s Permissions (index 2), `GrantPermissionSet`'s Permissions (index 1), `GrantCreateTimePermission`'s Permissions (index 0), `RevokePermissions`'s Permissions (index 0) — all four share the `ZFS_ALLOW_PERMISSIONS` vocabulary — and `AddAclEntry`'s Permissions (index 1, `ACL_PERMISSIONS`), Flags (index 2, `ACL_FLAGS`), and Type (index 3, `ACL_TYPES`, single-select). Every other field (`Recipient`, `Scope`, `Name`, `SSH target`, `Set name`, etc.) has no vocabulary — `F2` there is a no-op.
- **Live-fetched values (real usernames) are explicitly out of scope** for this pass — see the spec's Non-goals.
- The vocabulary lookup must key on `(kind, field_index)`, **never on a field's label text** — `GrantPermission`'s and `AddAclEntry`'s fields are both labeled "Permissions" but mean two completely different, non-interchangeable vocabularies (96 ZFS-allow entries vs. 14 NFSv4 ACE permission names).
- Register any new module (`mod picker;`) in the same step that creates its skeleton, not deferred to a later step.

## Review Focus

- `Up`/`Down`/`Enter`/`Space` against a *filtered* (not full) list, including when the filter matches zero entries — must never panic or act on an out-of-bounds index.
- Opening the picker on a field that already has text must pre-seed from it (pre-check matching comma-separated values for multi-select; pre-highlight an exact match for single-select) — not silently discard what's already typed by opening to a blank/default state.
- `F2` on a field with no attached vocabulary must be a true no-op — the form stays open exactly as it was, no picker appears.
- Confirming a multi-select picker (`Enter`) must rebuild the field's value entirely from the checked set, in a stable (sorted) order — not append to or otherwise merge with whatever was typed before opening the picker.
- The picker must intercept every key while open, including ones the underlying form would otherwise handle (`Tab`, `Esc`) — `Esc` while the picker is open must cancel *the picker*, not the form underneath it.

---

## File Structure

- **Create:** `src/ui/picker.rs` — `PickerState`, the four vocabulary constants, `vocabulary_for`, `build_picker_state`.
- **Modify:** `src/ui/mod.rs` — `pub mod picker;` declaration; event-loop dispatch priority (picker keys intercepted before modal keys).
- **Modify:** `src/ui/state.rs` — `App.picker` field; `handle_modal_key`'s `F2` arm; new `handle_picker_key` method.
- **Modify:** `src/ui/layout.rs` — new `draw_picker` function, called after `draw_modal`.
- **Modify:** `README.md` — document `F2`.

## Task 1: `picker` module — data model, vocabularies, pre-seeding

**Files:**
- Create: `src/ui/picker.rs`
- Modify: `src/ui/mod.rs:1-4` (add `pub mod picker;`)

**Interfaces:**
- Produces: `pub struct PickerState { pub values: Vec<String>, pub filter: String, pub selected: usize, pub checked: std::collections::HashSet<String>, pub multi_select: bool }` (`Debug, Clone, PartialEq`); `pub fn vocabulary_for(kind: &crate::ui::modal::FormKind, field_index: usize) -> Option<(&'static [&'static str], bool)>`; `pub fn build_picker_state(kind: &crate::ui::modal::FormKind, field_index: usize, current_value: &str) -> Option<PickerState>`. Consumed by Task 2.

- [ ] **Step 1: Write the module skeleton, register it, and write the vocabulary-lookup tests (failing)**

In `src/ui/mod.rs`, find:

```rust
pub mod layout;
pub mod modal;
pub mod state;
```

and change it to:

```rust
pub mod layout;
pub mod modal;
pub mod picker;
pub mod state;
```

Create `src/ui/picker.rs`:

```rust
use crate::ui::modal::FormKind;

#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub values: Vec<String>,
    pub filter: String,
    pub selected: usize,
    pub checked: std::collections::HashSet<String>,
    pub multi_select: bool,
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ui::picker::`
Expected: compile error — `vocabulary_for` is not defined.

- [ ] **Step 3: Implement the vocabulary constants and `vocabulary_for`**

Add this to `src/ui/picker.rs`, above the `#[cfg(test)]` module:

```rust
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

#[cfg(test)]
mod tests {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ui::picker::`
Expected: PASS (12 tests).

- [ ] **Step 5: Write the pre-seeding (`build_picker_state`) tests (failing)**

Add to `src/ui/picker.rs`'s `#[cfg(test)] mod tests` block:

```rust
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
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test ui::picker::`
Expected: compile error — `build_picker_state` is not defined.

- [ ] **Step 7: Implement `build_picker_state`**

Add this to `src/ui/picker.rs`, after `vocabulary_for`:

```rust
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
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test ui::picker::`
Expected: PASS (18 tests total in this module).

- [ ] **Step 9: Run the full test suite**

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 10: Commit**

```bash
git add src/ui/picker.rs src/ui/mod.rs
git commit -m "feat: picker module -- vocabularies and pre-seeded PickerState for the F2 picker"
```

## Task 2: Wire `F2` and picker-key handling into `App`

**Files:**
- Modify: `src/ui/state.rs`

**Interfaces:**
- Consumes: `crate::ui::picker::{PickerState, build_picker_state}` from Task 1.
- Produces: `App.picker: Option<PickerState>`; `pub fn App::handle_picker_key(&mut self, key: KeyCode)`. Consumed by Task 3's event-loop dispatch.

- [ ] **Step 1: Write the failing tests**

In `src/ui/state.rs`, find:

```rust
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
```

and change it to:

```rust
use crate::ui::modal::{FormField, FormKind, Modal, MutationCall, PendingAction};
use crate::ui::picker::{build_picker_state, PickerState};
```

In the `#[cfg(test)] mod tests` block, find a convenient spot (e.g. right after the `handle_key_n_on_hosts_section_opens_add_host_form` test) and add:

```rust
    fn open_grant_permission_form(app: &mut App) {
        app.handle_key(KeyCode::Tab); // -> Pools
        app.handle_key(KeyCode::Tab); // -> Datasets
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Right); // Right focus, Properties tab
        app.handle_key(KeyCode::Char(']')); // Permissions tab
        app.handle_key(KeyCode::Char('n')); // open Grant Permission form
    }

    #[test]
    fn f2_on_a_vocabulary_field_opens_the_picker_without_touching_the_form() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab); // -> Scope
        app.handle_modal_key(KeyCode::Tab); // -> Permissions (index 2)
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_some());
        match &app.modal {
            Some(Modal::Form { fields, focused, kind, .. }) => {
                assert_eq!(*focused, 2);
                assert_eq!(fields.len(), 3);
                assert_eq!(*kind, FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() });
            }
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn f2_on_a_field_with_no_vocabulary_is_a_no_op() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app); // focused = 0, Recipient
        app.handle_modal_key(KeyCode::F(2));
        assert!(app.picker.is_none());
        assert!(app.modal.is_some());
    }

    #[test]
    fn typing_in_an_open_picker_filters_the_list_case_insensitively_and_resets_selection() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Down); // move off 0 first, to prove filtering resets it
        for c in "SNAP".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.selected, 0);
        assert!(picker.values.iter().filter(|v| v.to_lowercase().contains("snap")).count() >= 2); // snapshot, snapdir, snapdev, snapshot_limit
    }

    #[test]
    fn space_toggles_a_value_in_a_multi_select_picker() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        assert_eq!(app.picker.as_ref().unwrap().checked, std::collections::HashSet::from(["create".to_string()]));
        app.handle_picker_key(KeyCode::Char(' '));
        assert!(app.picker.as_ref().unwrap().checked.is_empty(), "a second space on the same highlighted value must untoggle it");
    }

    #[test]
    fn space_is_a_no_op_in_a_single_select_picker() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']')); // Acl tab
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n')); // open Add ACL Entry form
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab); // -> Type (index 3)
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Char(' '));
        assert!(app.picker.as_ref().unwrap().checked.is_empty());
    }

    #[test]
    fn enter_on_a_multi_select_picker_joins_checked_values_into_the_field_and_closes_it() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        for _ in 0.."create".len() {
            app.handle_picker_key(KeyCode::Backspace);
        }
        for c in "destroy".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[2].value, "create,destroy"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_a_single_select_picker_writes_the_highlighted_value_and_closes_it() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']'));
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Down); // allow -> deny
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[3].value, "deny"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_an_empty_filtered_single_select_picker_is_a_no_op() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
        app.datasets_table.select(Some(0));
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Tab);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Char(']'));
        app.handle_key(KeyCode::Char(']'));
        app.acl_entries_for = Some("/tank/data".to_string());
        app.handle_key(KeyCode::Char('n'));
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        for c in "nonexistent-type".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Enter);
        assert!(app.picker.is_some(), "Enter against an empty filtered list must not close the picker");
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[3].value, "", "the field must be untouched"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_the_picker_without_touching_the_field_or_the_form() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        for c in "existing".chars() {
            app.handle_modal_key(KeyCode::Char(c));
        }
        app.handle_modal_key(KeyCode::F(2));
        for c in "create".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Char(' '));
        app.handle_picker_key(KeyCode::Esc);
        assert!(app.picker.is_none());
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[2].value, "existing", "Esc must leave the field exactly as it was before F2"),
            other => panic!("expected the form to still be open, got {other:?}"),
        }
    }

    #[test]
    fn up_and_down_clamp_within_the_filtered_list_without_panicking() {
        let mut app = App::new(vec!["local".to_string()]);
        open_grant_permission_form(&mut app);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::Tab);
        app.handle_modal_key(KeyCode::F(2));
        app.handle_picker_key(KeyCode::Up); // already at 0, must not underflow
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        for c in "xattr".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        // filtered list now has exactly 1 entry ("xattr" -- unique in
        // ZFS_ALLOW_PERMISSIONS; "read_xattr"/"write_xattr" only exist
        // in the separate ACL_PERMISSIONS vocabulary) -- Down must not
        // move past it
        app.handle_picker_key(KeyCode::Down);
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ui::state::tests::f2_ ui::state::tests::typing_in_an_open_picker ui::state::tests::space_ ui::state::tests::enter_on ui::state::tests::esc_cancels_the_picker ui::state::tests::up_and_down_clamp`
Expected: compile error — `App` has no field `picker` and no method `handle_picker_key`.

- [ ] **Step 3: Add the `App.picker` field and its initializer**

Find:

```rust
    pub modal: Option<Modal>,
```

and change it to:

```rust
    pub modal: Option<Modal>,
    /// A picker popup layered on top of `modal` (which stays open and
    /// untouched underneath) -- separate state, not a `Modal` variant,
    /// so the form being edited and the picker helping edit one of its
    /// fields are independent for as long as the picker is open.
    pub picker: Option<PickerState>,
```

Find:

```rust
            modal: None,
```

and change it to:

```rust
            modal: None,
            picker: None,
```

- [ ] **Step 4: Add the `F2` arm to `handle_modal_key`'s `Modal::Form` match**

Find:

```rust
                _ => {
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
            },
            Modal::Info { .. } => None,
```

and change it to:

```rust
                KeyCode::F(2) => {
                    if let Some(picker) = build_picker_state(&kind, focused, &fields[focused].value) {
                        self.picker = Some(picker);
                    }
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
                _ => {
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
            },
            Modal::Info { .. } => None,
```

- [ ] **Step 5: Add `handle_picker_key`**

Add this method to `impl App`, right after `handle_modal_key`'s closing `}`:

```rust
    /// Handles a key while `self.picker` is open. Never returns a
    /// `PendingAction` -- a picker only ever edits the still-open
    /// form's focused field, it never submits anything itself.
    pub fn handle_picker_key(&mut self, key: KeyCode) {
        let Some(mut picker) = self.picker.take() else { return };
        let filter_lower = picker.filter.to_lowercase();
        let filtered: Vec<String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).cloned().collect();
        match key {
            KeyCode::Esc => {}
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                self.picker = Some(picker);
            }
            KeyCode::Down => {
                if !filtered.is_empty() && picker.selected + 1 < filtered.len() {
                    picker.selected += 1;
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') if picker.multi_select => {
                if let Some(value) = filtered.get(picker.selected) {
                    if !picker.checked.insert(value.clone()) {
                        picker.checked.remove(value);
                    }
                }
                self.picker = Some(picker);
            }
            KeyCode::Char(' ') => {
                self.picker = Some(picker);
            }
            KeyCode::Enter => {
                if picker.multi_select {
                    let mut vals: Vec<String> = picker.checked.iter().cloned().collect();
                    vals.sort();
                    let new_value = vals.join(",");
                    if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                        if let Some(field) = fields.get_mut(*focused) {
                            field.value = new_value;
                        }
                    }
                } else if let Some(value) = filtered.get(picker.selected).cloned() {
                    if let Some(Modal::Form { fields, focused, .. }) = &mut self.modal {
                        if let Some(field) = fields.get_mut(*focused) {
                            field.value = value;
                        }
                    }
                } else {
                    self.picker = Some(picker);
                }
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
                self.picker = Some(picker);
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.selected = 0;
                self.picker = Some(picker);
            }
            _ => {
                self.picker = Some(picker);
            }
        }
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test ui::state::tests::f2_ ui::state::tests::typing_in_an_open_picker ui::state::tests::space_ ui::state::tests::enter_on ui::state::tests::esc_cancels_the_picker ui::state::tests::up_and_down_clamp`
Expected: PASS (10 tests).

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 8: Commit**

```bash
git add src/ui/state.rs
git commit -m "feat: F2 picker key handling -- open, filter, toggle, confirm, cancel"
```

## Task 3: Rendering, event-loop dispatch priority, README

This task has no new unit tests of its own, for the same reason prior plans' event-loop-wiring and rendering tasks didn't: `src/ui/mod.rs`'s event loop has no existing test harness, and no `draw_*` function anywhere in this codebase has a dedicated rendering test. Every piece of logic this task touches (`app.picker`, `handle_picker_key`) was already fully unit tested in Task 2 — this task is rendering plus a one-line dispatch-priority change.

**Files:**
- Modify: `src/ui/mod.rs`
- Modify: `src/ui/layout.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `App.picker`, `App::handle_picker_key` (Task 2).

- [ ] **Step 1: Give the picker dispatch priority over the modal in the event loop**

In `src/ui/mod.rs`, find:

```rust
                if app.modal.is_some() {
                    if let Some(action) = app.handle_modal_key(key.code) {
                        if let Some(host) = inventory.hosts.get(action.host) {
                            app.dispatch_mutation(action, host.runner.clone());
                        }
                    }
                } else {
```

and change it to:

```rust
                if app.picker.is_some() {
                    app.handle_picker_key(key.code);
                } else if app.modal.is_some() {
                    if let Some(action) = app.handle_modal_key(key.code) {
                        if let Some(host) = inventory.hosts.get(action.host) {
                            app.dispatch_mutation(action, host.runner.clone());
                        }
                    }
                } else {
```

- [ ] **Step 2: Render the picker on top of the modal**

In `src/ui/layout.rs`, find:

```rust
    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, area);
    }
}
```

and change it to:

```rust
    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, area);
    }
    if let Some(picker) = app.picker.clone() {
        draw_picker(f, &picker, area);
    }
}
```

Add a new function right after `draw_modal` (after its closing `}`, before `fn wrapped_line_count`):

```rust
/// Renders on top of `draw_modal`'s output whenever `app.picker` is
/// `Some` -- the form underneath stays drawn exactly as `draw_modal`
/// left it; this is a second, smaller popup layered above it.
fn draw_picker(f: &mut Frame, picker: &crate::ui::picker::PickerState, area: Rect) {
    let filter_lower = picker.filter.to_lowercase();
    let filtered: Vec<&String> = picker.values.iter().filter(|v| v.to_lowercase().contains(&filter_lower)).collect();

    const PICKER_WIDTH_PERCENT: u16 = 50;
    let popup_height = (filtered.len() as u16 + 4).clamp(6, area.height.saturating_sub(4).max(6));
    let popup_area = centered_rect(PICKER_WIDTH_PERCENT, popup_height, area);
    f.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let marker = if picker.multi_select {
                if picker.checked.contains(value.as_str()) { "[x] " } else { "[ ] " }
            } else {
                ""
            };
            let style = if i == picker.selected { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
            ListItem::new(format!("{marker}{value}")).style(style)
        })
        .collect();

    let title = if picker.filter.is_empty() {
        "Pick a value (type to filter, Space toggle, Enter confirm, Esc cancel)".to_string()
    } else {
        format!("Pick a value — filter: '{}' (Space toggle, Enter confirm, Esc cancel)", picker.filter)
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, popup_area);
}
```

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: PASS, no new warnings.

- [ ] **Step 4: Update `README.md`**

Find:

```markdown
- `Esc`: dismiss the current status message, back out of a focused properties pane, or (while a modal is open) cancel it
```

and change it to:

```markdown
- `Esc`: dismiss the current status message, back out of a focused properties pane, or (while a modal is open) cancel it
- `F2` (while a form field with a known value set is focused — `zfs allow`'s Permissions, or the ACL form's Permissions/Flags/Type): open a picker. Type to filter, `Up`/`Down` to move, `Space` to toggle a value (Permissions/Flags — multiple can be checked), `Enter` to confirm (joins every checked value with commas for multi-select, or picks the highlighted single value for Type), `Esc` to cancel without changing the field. A field with no known value set (`Recipient`, `Name`, `Scope`, etc.) ignores `F2`.
```

- [ ] **Step 5: Commit**

```bash
git add src/ui/mod.rs src/ui/layout.rs README.md
git commit -m "feat: render the F2 picker and give it dispatch priority over the open form"
```
