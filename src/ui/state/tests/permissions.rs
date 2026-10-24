//! Datasets: the Permissions and ACL tabs and their forms.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn refresh_dataset_permissions_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
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
    app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.drain_worker_messages();
    assert_eq!(app.dataset_permissions_for, None, "a result for a dataset that's no longer selected must not populate the pane");
}

#[test]
fn refresh_dataset_acl_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
        "# file: /tank/data\n# owner: root\n# group: wheel\n            owner@:read_data/write_data/execute::allow\n",
    ));
    app.refresh_dataset_acl(runner, "/tank/data".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.acl_entries.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        app.acl_entries,
        vec![crate::acl::AclEntry {
            tag: crate::acl::AclTag::Owner,
            permissions: vec!["read_data".to_string(), "write_data".to_string(), "execute".to_string()],
            flags: vec![],
            entry_type: crate::acl::AclEntryType::Allow,
        }]
    );
    assert_eq!(app.acl_entries_for, Some("/tank/data".to_string()));
}

#[test]
fn acl_result_for_a_stale_mountpoint_is_discarded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::DatasetAcl(0, "/tank/old".to_string(), Ok(vec![]))).unwrap();
    app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.acl_entries_for = Some("placeholder".to_string());
    app.drain_worker_messages();
    assert_eq!(app.acl_entries_for, Some("placeholder".to_string()), "a result for a mountpoint that's no longer selected must not overwrite the current state");
}

#[test]
fn datasets_list_refresh_that_repoints_selection_clears_stale_acl_entries() {
    // Same class of bug Permissions' final review caught for
    // dataset_permissions_for -- built in from the start here.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/old".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
    app.acl_entries_table.select(Some(0));
    app.acl_entries_for = Some("/tank/old".to_string());

    app.tx
        .send(WorkerMsg::Datasets(
            0,
            Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()), quota: 0, refquota: 0 }]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert!(app.acl_entries.is_empty(), "stale ACL entries for tank/old must be cleared once the list refresh re-points the selection at tank/new");
    assert_eq!(app.acl_entries_for, None);
}

#[test]
fn datasets_list_refresh_that_repoints_selection_clears_stale_permissions() {
    // Same class of bug the Properties pane already guards against
    // (see pools_list_replacement_that_re_points_selected_index_at_a_different_pool_clears_stale_properties):
    // view "tank/old"'s permissions, then a list-changing refresh
    // re-points the selected index at a different dataset. The
    // stale permissions (and their selection) must be cleared
    // rather than silently kept and shown under the new dataset's
    // title — or worse, acted on by 'd'/'x' against the wrong
    // dataset.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.dataset_permissions = vec![PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }];
    app.dataset_permissions_table.select(Some(0));
    app.dataset_permissions_for = Some("tank/old".to_string());

    app.tx
        .send(WorkerMsg::Datasets(
            0,
            Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert!(app.dataset_permissions.is_empty(), "stale permissions for tank/old must be cleared once the list refresh re-points the selection at tank/new");
    assert_eq!(app.dataset_permissions_for, None);
}

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

#[test]
fn build_pending_action_add_acl_entry_requires_recipient_and_permissions() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 3 };
    let blank = vec![
        FormField { label: "Recipient".to_string(), value: String::new() },
        FormField { label: "Permissions".to_string(), value: String::new() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: String::new() },
    ];
    assert!(build_pending_action(&kind, &blank).is_none(), "blank Permissions must keep the form open");

    let bad_recipient = vec![
        FormField { label: "Recipient".to_string(), value: "nobody".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: String::new() },
    ];
    assert!(build_pending_action(&kind, &bad_recipient).is_none(), "a bare name with no user:/group: prefix (and not owner/group/everyone) must keep the form open");
}

#[test]
fn build_pending_action_add_acl_entry_blank_flags_and_type_default_correctly() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
    let fields = vec![
        FormField { label: "Recipient".to_string(), value: "everyone".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::AddAclEntry {
            path: "/tank/data".to_string(),
            position: 0,
            entry: crate::acl::AclEntry {
                tag: crate::acl::AclTag::Everyone,
                permissions: vec!["read_data".to_string()],
                flags: vec![],
                entry_type: crate::acl::AclEntryType::Allow,
            },
        }
    );
}

#[test]
fn build_pending_action_add_acl_entry_garbage_type_keeps_form_open() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
    let fields = vec![
        FormField { label: "Recipient".to_string(), value: "owner".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: "dny".to_string() },
    ];
    assert!(build_pending_action(&kind, &fields).is_none(), "a Type typo must not silently default to allow");
}

#[test]
fn build_pending_action_add_acl_entry_owner_and_group_recipients() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
    let owner_fields = vec![
        FormField { label: "Recipient".to_string(), value: "owner".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &owner_fields).unwrap();
    match action.call {
        MutationCall::AddAclEntry { entry, .. } => assert_eq!(entry.tag, crate::acl::AclTag::Owner),
        other => panic!("expected AddAclEntry, got {other:?}"),
    }

    let group_fields = vec![
        FormField { label: "Recipient".to_string(), value: "group:admins".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data".to_string() },
        FormField { label: "Flags".to_string(), value: String::new() },
        FormField { label: "Type".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &group_fields).unwrap();
    match action.call {
        MutationCall::AddAclEntry { entry, .. } => assert_eq!(entry.tag, crate::acl::AclTag::NamedGroup("admins".to_string())),
        other => panic!("expected AddAclEntry, got {other:?}"),
    }
}

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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
fn handle_key_d_on_selected_grant_row_revokes_only_that_rows_own_permissions() {
    // Reviewer's repro: fb has "snapshot" as Local+Descendent and
    // "mount" as Local only, so two separate rows show. Revoking the
    // "Local / mount" row must not also delete fb's Local half of
    // "snapshot" -- it must only ever touch exactly the permissions
    // that row displays, not everything for that recipient+scope.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_permissions_tab(&mut app);
    app.dataset_permissions = vec![PermissionRow::Grant {
        recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
        scope: crate::zfs::permission::Scope::Local,
        perms: vec!["mount".to_string()],
    }];
    app.dataset_permissions_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(
                action.call,
                MutationCall::UnallowPermission {
                    dataset: "tank/data".to_string(),
                    target: crate::zfs::permission::PermissionTarget::Grant {
                        recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                        scope: crate::zfs::permission::Scope::Local,
                    },
                    perms: Some(vec!["mount".to_string()]),
                },
                "must revoke only this row's own permissions, not perms: None (which would also wipe the Local half of any other grant for the same recipient)"
            );
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_x_on_selected_permission_row_opens_revoke_specific_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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

#[test]
fn handle_key_n_on_acl_tab_with_no_dataset_selected_does_not_panic_or_open_form() {
    let mut app = App::new(vec!["local".to_string()]);
    enter_datasets_acl_tab(&mut app);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none());
}

#[test]
fn handle_key_n_on_acl_tab_with_no_mountpoint_does_not_panic_or_open_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/vol".to_string(), kind: crate::zfs::dataset::DatasetKind::Volume, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "a zvol (no mountpoint) must not open an ACL form");
}

#[test]
fn handle_key_n_on_acl_tab_opens_add_acl_entry_form_appending_at_the_current_count() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries = vec![
        crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
        crate::acl::AclEntry { tag: crate::acl::AclTag::OwningGroup, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
    ];
    app.acl_entries_for = Some("/tank/data".to_string());
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 4);
            assert_eq!(*kind, FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 2 });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_n_on_acl_tab_before_acl_has_loaded_shows_status_and_does_not_open_form() {
    // Catches a real bug a final review found: pressing 'n' while
    // acl_entries is still empty (fetch in flight, fetch failed, or
    // just-cleared by a Datasets refresh) computed position as 0
    // and silently prepended instead of appending -- `setfacl -a 0`
    // puts the new entry FIRST, ahead of every existing entry,
    // confirmed live. Must refuse until acl_entries_for matches.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    // acl_entries is empty and acl_entries_for is None -- never refreshed.
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "must not open a form that would compute the wrong (prepend) position");
    assert!(app.status_message.is_some());
}

#[test]
fn handle_key_d_on_acl_tab_with_no_row_selected_does_not_panic() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none());
}

#[test]
fn handle_key_d_on_selected_acl_row_opens_remove_confirm_with_its_position() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries = vec![
        crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
        crate::acl::AclEntry { tag: crate::acl::AclTag::OwningGroup, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow },
    ];
    app.acl_entries_table.select(Some(1));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, MutationCall::RemoveAclEntry { path: "/tank/data".to_string(), position: 1 });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn removing_an_acl_entry_needs_the_selection_to_be_inside_the_list() {
    let mut app = in_section(Section::Datasets);
    app.datasets = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.pane_focus = PaneFocus::Right;
    app.dataset_detail_tab = DatasetDetailTab::Acl;
    app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec![], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
    app.acl_entries_table.select(Some(1)); // one past the last entry
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none());
    app.acl_entries_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
}

#[test]
fn a_dash_prefixed_permission_keeps_the_form_open_and_names_the_token() {
    let grant = || FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
    let msg = submit_problem(grant(), &["user:carol", "both", "snapshot, -x"]);
    assert!(msg.contains("Permissions") && msg.contains("-x") && msg.contains("'-'"), "{msg}");
    let revoke = FormKind::RevokePermissions { host: 0, dataset: "tank/data".to_string(), target: crate::zfs::permission::PermissionTarget::CreateTime };
    assert!(submit_problem(revoke, &["--help"]).contains("--help"));
    let acl = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
    assert!(submit_problem(acl, &["owner", "read_data,-r", "", "allow"]).contains("-r"));
}

#[test]
fn the_acl_remove_confirm_shows_the_entry_it_would_remove() {
    let mut app = acl_app_with_acltype(Some("nfsv4"));
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("#0") && prompt.contains("allow owner@: read_data") && prompt.contains("/tank/data"), "{prompt}");
}

#[test]
fn a_posix_acltype_dataset_explains_itself_and_the_acl_keys_stay_inert() {
    let mut app = acl_app_with_acltype(Some("posix"));
    assert!(app.acl_unsupported().is_some_and(|r| r.contains("posix") && r.contains("NFSv4")));
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none() && app.status_message.as_deref().is_some_and(|m| m.contains("posix")), "{:?}", app.status_message);
    app.status_message = None;
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none() && app.status_message.as_deref().is_some_and(|m| m.contains("posix")));
    for other in [Some("nfsv4"), Some("off"), None] {
        assert_eq!(acl_app_with_acltype(other).acl_unsupported(), None, "{other:?}");
    }
}

#[test]
fn revoking_permissions_confirms_and_names_the_permissions() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![FormField { label: "perms".into(), value: "snapshot,mount".into() }],
        focused: 0,
        description: None,
        kind: FormKind::RevokePermissions { host: 0, dataset: "tank/data".into(), target: crate::zfs::permission::PermissionTarget::CreateTime },
    });
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("snapshot,mount") && prompt.contains("tank/data"), "{prompt}");
}

#[test]
fn the_confirmation_names_the_host_puts_the_command_on_its_own_line_and_names_whose_permissions() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![FormField { label: "pool".into(), value: "tank".into() }, FormField { label: "role".into(), value: "data".into() }, FormField { label: "b".into(), value: "mirror".into() }],
        focused: 2,
        description: None,
        kind: FormKind::AddVdevFromDisks { host: 1, disks: vec!["da0".into(), "da1".into()] },
    });
    let prompt = submit_to_confirm(&mut app);
    let lines: Vec<&str> = prompt.lines().collect();
    assert!(lines[0].contains("fbsd16"), "{prompt}");
    assert!(lines.iter().any(|l| l.trim_start().starts_with("zpool add tank")), "the command is a line of its own: {prompt}");
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![FormField { label: "perms".into(), value: "mount".into() }],
        focused: 0,
        description: None,
        kind: FormKind::RevokePermissions { host: 0, dataset: "tank/data".into(), target: crate::zfs::permission::PermissionTarget::Grant { recipient: crate::zfs::permission::Recipient::User("alice".into()), scope: crate::zfs::permission::Scope::Both } },
    });
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("alice"), "{prompt}");
}

#[test]
fn the_acl_tab_says_why_on_linux_and_keys_of_a_dimmed_section_are_inert() {
    let mut app = two_os_app(crate::zfs::host_health::HostOs::Linux);
    app.selected_host = 1;
    go_to(&mut app, Section::Datasets);
    app.datasets = vec![ds_named("ztest/data", Some("/ztest/data"))];
    app.datasets_table.select(Some(0));
    app.dataset_properties_for = Some("ztest/data".into());
    assert!(app.acl_unsupported().is_some_and(|t| t.contains("Linux")), "{:?}", app.acl_unsupported());
    go_to(&mut app, Section::BootEnvs);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "n in Boot Envs needs no selection, so only the guard stops it");
    app.handle_key(KeyCode::Tab);
    assert_ne!(app.current_section(), Section::BootEnvs, "navigation still works");
}
