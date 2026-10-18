use super::*;
// The form builders and field parsers these tests exercise live in the `forms` module.
#[allow(clippy::wildcard_imports)]
use super::forms::*;

fn smb(dataset: &str) -> crate::zfs::share::Share {
    crate::zfs::share::Share::Smb {
        dataset: dataset.to_string(),
        name: "media".to_string(),
        path: format!("/{dataset}"),
        valid_user: Some("carol".to_string()),
        read_only: false,
        guest_ok: false,
    }
}
use crate::exec::test_support::FakeRunner;
use crate::zfs::property::PropertySource;

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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
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

#[test]
fn bracket_keys_cycle_through_all_three_dataset_detail_tabs() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.handle_key(KeyCode::Right); // enter right focus, Properties tab
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
    app.handle_key(KeyCode::Char(']'));
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
    app.handle_key(KeyCode::Char(']'));
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Acl);
    app.handle_key(KeyCode::Char(']')); // wraps back to Properties
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Properties);
    app.handle_key(KeyCode::Char('[')); // wraps the other way
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Acl);
    app.handle_key(KeyCode::Char('['));
    assert_eq!(app.dataset_detail_tab, DatasetDetailTab::Permissions);
}

#[test]
fn refresh_dataset_acl_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
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
    app.datasets = vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()) }];
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
    app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/old".to_string()) }];
    app.datasets_table.select(Some(0));
    app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
    app.acl_entries_table.select(Some(0));
    app.acl_entries_for = Some("/tank/old".to_string());

    app.tx
        .send(WorkerMsg::Datasets(
            0,
            Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/new".to_string()) }]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert!(app.acl_entries.is_empty(), "stale ACL entries for tank/old must be cleared once the list refresh re-points the selection at tank/new");
    assert_eq!(app.acl_entries_for, None);
}

#[test]
fn handle_key_s_on_pools_opens_scrub_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    match app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.host, 0);
            assert_eq!(action.section, Section::Pools);
            assert_eq!(action.call, crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_s_on_pools_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right); // focus properties pane
    assert_eq!(app.pane_focus, PaneFocus::Right);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none(), "scrub key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_d_on_pools_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none(), "destroy key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_n_on_pools_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "create-pool key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_n_on_datasets_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "create-dataset key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_d_on_datasets_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none(), "destroy-dataset key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_s_on_datasets_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none(), "create-snapshot key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_s_with_no_row_selected_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none());
}

#[test]
fn handle_key_d_on_pools_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "scratch");
            assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyPool { pool: "scratch".to_string() });
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

fn confirm_scrub() -> (App, crate::ui::modal::PendingAction) {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction { host: 0, section: Section::Pools, call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() } };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action: action.clone(), choice: None });
    (app, action)
}

/// `None`: no confirm is open; `Some(None)`: open, nothing selected; `Some(Some(yes))`: the selection.
#[allow(clippy::option_option)]
fn choice_of(app: &App) -> Option<Option<bool>> {
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { choice, .. }) => Some(*choice),
        _ => None,
    }
}

#[test]
fn a_confirm_needs_y_and_then_enter_to_run() {
    let (mut app, action) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Char('y')), None, "y alone runs nothing");
    assert_eq!(choice_of(&app), Some(Some(true)), "y arms yes and keeps the window open");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), Some(action));
    assert!(app.modal.is_none());
}

#[test]
fn enter_on_its_own_does_nothing_at_all() {
    let (mut app, _) = confirm_scrub();
    for _ in 0..5 {
        assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "mashing Enter must never run or dismiss it");
    }
    assert_eq!(choice_of(&app), Some(None));
}

#[test]
fn n_then_enter_cancels_and_n_alone_does_not_close() {
    let (mut app, _) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Char('n')), None);
    assert_eq!(choice_of(&app), Some(Some(false)), "n arms no and keeps the window open");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none(), "Enter after n closes it without running anything");
}

#[test]
fn esc_also_arms_no_and_needs_enter_to_close() {
    let (mut app, _) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Esc), None);
    assert_eq!(choice_of(&app), Some(Some(false)), "Esc selects no, it does not close");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none());
}

#[test]
fn the_last_choice_before_enter_wins() {
    let (mut app, action) = confirm_scrub();
    app.handle_modal_key(KeyCode::Char('y'));
    app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "y then n: cancelled");
    let (mut app, _) = confirm_scrub();
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Esc);
    app.handle_modal_key(KeyCode::Char('y'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), Some(action), "n, Esc, y: runs");
}

#[test]
fn other_keys_never_change_or_dismiss_a_confirm() {
    let (mut app, _) = confirm_scrub();
    app.handle_modal_key(KeyCode::Char('y'));
    for key in [KeyCode::Char('x'), KeyCode::Char(' '), KeyCode::Tab, KeyCode::Backspace, KeyCode::Up, KeyCode::Down] {
        assert_eq!(app.handle_modal_key(key), None);
    }
    assert_eq!(choice_of(&app), Some(Some(true)), "stray keys keep the armed choice and the window");
}

#[test]
fn every_confirm_opened_from_a_key_starts_with_no_choice() {
    let (mut app, _dir) = selected_jobs_app(&[scrub_job("a")]);
    app.handle_key(KeyCode::Char('d'));
    assert_eq!(choice_of(&app), Some(None), "a freshly opened confirm has nothing selected");
}

#[test]
fn handle_modal_key_confirm_typed_requires_exact_match() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tan".to_string(),
        action: action.clone(),
    });
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None, "submitting with a mismatched confirmation must not run the action");
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
        other => panic!("expected the modal to stay open, got {other:?}"),
    }

    // Type the missing character, then submit again.
    let result = app.handle_modal_key(KeyCode::Char('k'));
    assert_eq!(result, None);
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, Some(action));
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_confirm_typed_backspace_edits_input() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tanx".to_string(),
        action,
    });
    app.handle_modal_key(KeyCode::Backspace);
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { input, .. }) => assert_eq!(input, "tan"),
        other => panic!("expected the modal to stay open, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_confirm_typed_esc_cancels() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    app.modal = Some(crate::ui::modal::Modal::ConfirmTyped {
        prompt: "Type the pool name:".to_string(),
        expected: "tank".to_string(),
        input: "tank".to_string(),
        action,
    });
    let result = app.handle_modal_key(KeyCode::Esc);
    assert_eq!(result, None);
    assert!(app.modal.is_none());
}

#[test]
fn dispatch_mutation_success_queues_refresh_for_its_section() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Pools, Section::Disks]);
    assert_eq!(app.status_message, None);
}

#[test]
fn dispatch_mutation_failure_sets_status_message_and_queues_no_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::failing("pool is busy", 1));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if app.status_message.is_some() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(refreshes.is_empty());
    assert!(app.status_message.is_some());
}

#[test]
fn dispatch_mutation_result_for_stale_host_is_discarded() {
    // Simulates: a mutation was dispatched against host 0, the user
    // switched to host 1, and only then did host 0's result arrive.
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    app.selected_host = 1; // navigated away before the result arrived
    let refreshes = app.drain_worker_messages();
    assert!(refreshes.is_empty(), "a mutation result for a host we've navigated away from must not queue a refresh");
    assert_eq!(app.status_message, None);
}

#[test]
fn handle_key_navigates_sections_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Pools);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Dashboard);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Settings);
}

#[test]
fn handle_key_quit_sets_should_quit() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn handle_key_question_mark_opens_help_info_modal_from_any_section() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools, to confirm it's not section-gated
    app.handle_key(KeyCode::Char('?'));
    match &app.modal {
        Some(Modal::Info { title, .. }) => assert_eq!(title, "Help"),
        other => panic!("expected Info modal, got {other:?}"),
    }
}

#[test]
fn handle_key_capital_a_opens_about_info_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('A'));
    match &app.modal {
        Some(Modal::Info { title, body }) => {
            assert_eq!(title, "About");
            assert!(body.contains("zfsctl"));
            assert!(body.contains("Claude"));
        }
        other => panic!("expected Info modal, got {other:?}"),
    }
}

#[test]
fn handle_key_capital_a_does_not_shadow_section_gated_lowercase_a() {
    // The existing section-gated lowercase 'a' bindings (e.g. add
    // vdev) must keep working -- only capital 'A' is the new global
    // About key.
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('A'));
    assert!(app.modal.is_some());
    app.handle_modal_key(KeyCode::Esc);
    assert!(app.modal.is_none());
    app.handle_key(KeyCode::Char('a'));
    assert!(app.modal.is_none(), "lowercase 'a' outside any section-gated context must stay a no-op, not open About");
}

#[test]
fn esc_closes_the_info_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('?'));
    assert!(app.modal.is_some());
    let result = app.handle_modal_key(KeyCode::Esc);
    assert!(result.is_none());
    assert!(app.modal.is_none());
}

#[test]
fn handle_key_navigates_hosts_and_wraps() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    assert_eq!(app.selected_host, 0);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_host, 1);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_host, 0);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.selected_host, 1);
}

#[test]
fn drain_worker_messages_updates_pools_on_success() {
    let mut app = App::new(vec!["local".to_string()]);
    let pool = Pool {
        name: "tank".to_string(),
        size: 100,
        allocated: 10,
        free: 90,
        health: crate::zfs::pool::PoolHealth::Online,
    };
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool.clone()]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools, vec![pool]);
    assert!(!app.is_refreshing(Section::Pools, 0));
}

#[test]
fn drain_worker_messages_sets_status_message_on_failure() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx
        .send(WorkerMsg::Datasets(0, Err(crate::error::AppError::Parse("boom".to_string()))))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.is_some());
}

#[test]
fn esc_clears_existing_status_message() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("something went wrong".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn drain_worker_messages_clears_status_message_on_subsequent_success() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale error".to_string());
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message, None);
}

#[test]
fn refresh_pools_populates_pools_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    app.refresh_pools(runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pools.len(), 1);
    assert_eq!(app.pools[0].name, "tank");
}

#[test]
fn drain_worker_messages_discards_result_for_stale_host() {
    // Simulates: a refresh was in flight for host 0, the user switched
    // to host 1, and only then did host 0's (slow) result arrive.
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.selected_host = 1;
    let pool = Pool {
        name: "tank".to_string(),
        size: 100,
        allocated: 10,
        free: 90,
        health: crate::zfs::pool::PoolHealth::Online,
    };
    app.begin_refresh(Section::Pools, 0);
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool]))).unwrap();
    app.drain_worker_messages();

    assert!(app.pools.is_empty(), "stale result for host 0 must not overwrite the pane while host 1 is selected");
    assert!(!app.is_refreshing(Section::Pools, 0), "the in-flight flag must still clear even though the result was discarded");
    assert_eq!(app.status_message, None, "a discarded stale result is not an error");
}

#[test]
fn switching_selected_host_clears_pane_data() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.pools = vec![Pool {
        name: "tank".to_string(),
        size: 100,
        allocated: 10,
        free: 90,
        health: crate::zfs::pool::PoolHealth::Online,
    }];
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 1,
        available: 2,
        referenced: 1,
        mountpoint: None,
    }];
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "snap1".to_string(), used: 1 }];
    app.pools_table.select(Some(0));
    app.datasets_table.select(Some(0));
    app.snapshots_table.select(Some(0));

    app.handle_key(KeyCode::Down); // still on Hosts section by default; switches host

    assert!(app.pools.is_empty());
    assert!(app.datasets.is_empty());
    assert!(app.snapshots.is_empty());
    assert_eq!(app.pools_table.selected(), None);
    assert_eq!(app.datasets_table.selected(), None);
    assert_eq!(app.snapshots_table.selected(), None);
}

#[test]
fn handle_key_navigates_pool_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![
        Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    assert_eq!(app.current_section(), Section::Pools);
    assert_eq!(app.pools_table.selected(), None);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(1));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0), "must wrap back to the first row");
    app.handle_key(KeyCode::Up);
    assert_eq!(app.pools_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn handle_key_pool_row_navigation_is_noop_on_empty_list() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), None);
}

#[test]
fn drain_worker_messages_clamps_selected_pool_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools_table.select(Some(2));
    app.begin_refresh(Section::Pools, 0);
    app.tx
        .send(WorkerMsg::Pools(0, Ok(vec![Pool {
            name: "only".to_string(),
            size: 0,
            allocated: 0,
            free: 0,
            health: crate::zfs::pool::PoolHealth::Online,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(0));
}

#[test]
fn handle_key_navigates_dataset_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
    ];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // Hosts -> Pools -> Datasets
    assert_eq!(app.current_section(), Section::Datasets);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.datasets_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.datasets_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn drain_worker_messages_clamps_selected_dataset_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets_table.select(Some(2));
    app.begin_refresh(Section::Datasets, 0);
    app.tx
        .send(WorkerMsg::Datasets(0, Ok(vec![Dataset {
            name: "only".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 0,
            available: 0,
            referenced: 0,
            mountpoint: None,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.datasets_table.selected(), Some(0));
}

#[test]
fn handle_key_navigates_snapshot_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![
        Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0 },
        Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0 },
    ];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    assert_eq!(app.current_section(), Section::Snapshots);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.snapshots_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.snapshots_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn drain_worker_messages_clamps_selected_snapshot_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots_table.select(Some(2));
    app.begin_refresh(Section::Snapshots, 0);
    app.tx
        .send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot {
            dataset: "tank/data".to_string(),
            name: "only".to_string(),
            used: 0,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.snapshots_table.selected(), Some(0));
}

#[test]
fn refresh_pools_does_not_spawn_second_thread_while_in_flight() {
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();

    app.refresh_pools(runner.clone());
    assert!(app.is_refreshing(Section::Pools, 0));
    // A second refresh while the first is still in flight must be a
    // no-op — otherwise holding 'r' spawns unbounded threads/processes.
    app.refresh_pools(runner.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        fake.calls.lock().unwrap().len(),
        1,
        "second refresh_pools call while one is in flight must not spawn another thread"
    );
}

#[test]
fn enter_on_selected_pool_drills_into_vdevs() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.drilled_pool, Some("tank".to_string()));
}

#[test]
fn enter_with_no_row_selected_does_not_drill_in() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.drilled_pool, None);
}

#[test]
fn esc_backs_out_of_vdev_drill_down() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.drilled_pool, None);
    assert!(app.vdevs.is_empty());
    assert_eq!(app.vdevs_table.selected(), None);
}

#[test]
fn esc_with_no_drill_down_active_clears_status_message_as_before() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn up_down_while_drilled_in_navigate_vdev_rows_not_pool_rows() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![
        VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online },
        VdevLine { depth: 1, name: "da0".to_string(), health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.vdevs_table.selected(), Some(0));
    assert_eq!(app.pools_table.selected(), Some(0), "pool row selection must be untouched while drilled in");
}

#[test]
fn refresh_vdevs_populates_vdevs_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    // In the real event loop, `refresh_vdevs` is only ever called while
    // drilled into the pool being refreshed — the stale-pool check in
    // `drain_worker_messages` requires this to accept the result.
    app.drilled_pool = Some("tank".to_string());
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
        "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\nerrors: No known data errors\n",
    ));
    app.refresh_vdevs(runner, "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.vdevs.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.vdevs.len(), 1);
    assert_eq!(app.vdevs[0].name, "tank");
}

#[test]
fn refresh_vdevs_populates_pool_scan_status_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tpool".to_string());
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(
        "  pool: tpool\n state: ONLINE\n  scan: resilver in progress since Sat Sep 26 12:09:06 2026\nconfig:\n\n\tNAME   STATE     READ WRITE CKSUM\n\ttpool  ONLINE       0     0     0\n\nerrors: No known data errors\n",
    ));
    app.refresh_vdevs(runner, "tpool".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if app.pool_scan_status.is_some() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        app.pool_scan_status,
        Some(crate::zfs::pool::ScanStatus::InProgress {
            summary: "resilver in progress since Sat Sep 26 12:09:06 2026".to_string(),
            percent_done: None,
            eta: None,
        })
    );
}

#[test]
fn pool_scan_status_is_cleared_on_backing_out_of_drilldown() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tank".to_string());
    app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::InProgress { summary: "resilver in progress".to_string(), percent_done: None, eta: None });
    app.handle_key(KeyCode::Esc);
    assert!(app.pool_scan_status.is_none());
}

#[test]
fn pool_scan_status_is_cleared_on_drilling_into_a_pool() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools_table.select(Some(0));
    app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "stale from a previous drilldown".to_string(), errors: false });
    app.handle_key(KeyCode::Enter);
    assert!(app.pool_scan_status.is_none());
}

#[test]
fn clear_stale_scan_status_clears_completed_status_after_the_display_duration() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "resilvered 501M in 00:00:32 with 0 errors".to_string(), errors: false });
    app.pool_scan_status_shown_at = Some(Instant::now().checked_sub(Duration::from_secs(7)).unwrap());
    app.clear_stale_scan_status();
    assert!(app.pool_scan_status.is_none());
}

#[test]
fn clear_stale_scan_status_leaves_a_recently_shown_completed_status_alone() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::Completed { summary: "resilvered 501M in 00:00:32 with 0 errors".to_string(), errors: false });
    app.pool_scan_status_shown_at = Some(Instant::now());
    app.clear_stale_scan_status();
    assert!(app.pool_scan_status.is_some(), "must not clear before it's been shown for the full display duration");
}

#[test]
fn clear_stale_scan_status_never_clears_an_in_progress_status() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pool_scan_status = Some(crate::zfs::pool::ScanStatus::InProgress { summary: "resilver in progress".to_string(), percent_done: Some(50.0), eta: None });
    app.pool_scan_status_shown_at = Some(Instant::now().checked_sub(Duration::from_secs(999)).unwrap());
    app.clear_stale_scan_status();
    assert!(app.pool_scan_status.is_some(), "an in-progress resilver/scrub must stay visible regardless of how long it's been running");
}

#[test]
fn vdevs_result_for_a_different_pool_than_currently_drilled_is_discarded() {
    // Simulates: the user drilled into "tank" (slow refresh in flight),
    // backed out, drilled into "scratch" instead — "tank"'s result then
    // arrives late and must not be shown under "scratch"'s title.
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("scratch".to_string());
    app.status_message = Some("stale but unrelated".to_string());
    app.vdevs = vec![VdevLine { depth: 0, name: "scratch".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));

    app.tx
        .send(WorkerMsg::Vdevs(
            0,
            "tank".to_string(),
            Ok((vec![VdevLine { depth: 0, name: "tank".to_string(), health: crate::zfs::pool::PoolHealth::Online }], None)),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert_eq!(app.vdevs.len(), 1);
    assert_eq!(app.vdevs[0].name, "scratch", "a vdev result for a pool other than the currently drilled-into one must not overwrite the pane");
    assert_eq!(
        app.status_message,
        Some("stale but unrelated".to_string()),
        "a discarded stale-pool result must not clear an unrelated status message"
    );
}

#[test]
fn refresh_pool_properties_populates_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
    app.refresh_pool_properties(runner, "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pool_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pool_properties.len(), 1);
    assert_eq!(app.pool_properties[0].name, "autoexpand");
}

#[test]
fn pool_properties_result_for_a_different_pool_than_selected_is_discarded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "scratch".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0)); // selected pool is "scratch"
    app.tx
        .send(WorkerMsg::PoolProperties(0, "tank".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.pool_properties.is_empty(), "a result for a pool that's no longer selected must not overwrite the pane");
    assert_eq!(app.status_message, None);
}

#[test]
fn refresh_dataset_properties_populates_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\tcompression\tlz4\tlocal\n"));
    app.refresh_dataset_properties(runner, "tank/data".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.dataset_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.dataset_properties.len(), 1);
    assert_eq!(app.dataset_properties[0].name, "compression");
}

#[test]
fn dataset_properties_result_for_a_different_dataset_than_selected_is_discarded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/other".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.tx
        .send(WorkerMsg::DatasetProperties(0, "tank/data".to_string(), Ok(vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.dataset_properties.is_empty());
}

#[test]
fn refresh_pool_properties_does_not_spawn_second_thread_while_in_flight() {
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(FakeRunner::new("tank\tautoexpand\toff\tdefault\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    app.refresh_pool_properties(runner.clone(), "tank".to_string());
    app.refresh_pool_properties(runner.clone(), "tank".to_string());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pool_properties.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[test]
fn refresh_request_while_in_flight_is_queued_and_redispatched_on_completion() {
    // Simulates: the user presses 'r' while a refresh for the same
    // (section, host) is already in flight (e.g. a slow SSH call, or a
    // post-mutation auto-refresh landing mid-flight). The second
    // request must not be silently dropped — it must run once the
    // first one completes.
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();

    app.refresh_pools(runner.clone());
    assert!(app.is_refreshing(Section::Pools, 0));

    // Second request while the first is still in flight.
    app.refresh_pools(runner.clone());
    assert!(
        app.pending_refresh.contains(&(Section::Pools, 0)),
        "the second request must be remembered rather than dropped"
    );

    // Let the first refresh complete and drain its result.
    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Pools], "the queued request must surface for the event loop to redispatch");
    assert!(
        !app.pending_refresh.contains(&(Section::Pools, 0)),
        "the pending flag must be cleared once it has been surfaced for redispatch"
    );

    // Simulate the event loop redispatching the queued request.
    app.refresh_pools(runner.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if fake.calls.lock().unwrap().len() >= 2 || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 2, "the redispatched refresh must actually run against the runner");
}

#[test]
fn refresh_pools_for_one_host_does_not_block_refresh_for_another_host() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    let fake_a = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    let runner_a: Arc<dyn CommandRunner> = fake_a.clone();

    // Host 0's refresh is "in flight" (never drained in this test, so it
    // stays in flight for the duration).
    app.refresh_pools(runner_a.clone());
    assert!(app.is_refreshing(Section::Pools, 0));

    // Switching to host 1 and refreshing must not be blocked by host 0's
    // still-in-flight refresh.
    app.selected_host = 1;
    let fake_b = Arc::new(FakeRunner::new("scratch\t50\t5\t45\tONLINE\n"));
    let runner_b: Arc<dyn CommandRunner> = fake_b.clone();
    app.refresh_pools(runner_b.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pools.len(), 1);
    assert_eq!(app.pools[0].name, "scratch", "host 1's refresh must have run, not been blocked by host 0's in-flight one");
}

#[test]
fn handle_key_n_on_pools_opens_create_pool_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, focused, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(*focused, 0);
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_n_on_datasets_opens_create_dataset_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // Hosts -> Pools -> Datasets
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::CreateDataset { host: 0 });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_create_dataset_form_submits() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    for c in "tank/newds".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Datasets,
            call: crate::ui::modal::MutationCall::CreateDataset { name: "tank/newds".to_string() },
        })
    );
}

#[test]
fn handle_key_d_on_datasets_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/olds".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/olds");
            assert_eq!(action.call, crate::ui::modal::MutationCall::DestroyDataset { name: "tank/olds".to_string() });
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn handle_key_s_on_datasets_opens_create_snapshot_form_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::CreateSnapshot { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_create_snapshot_form_submits_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None,
    }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    for c in "manual-1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Snapshots,
            call: crate::ui::modal::MutationCall::CreateSnapshot {
                dataset: "tank/data".to_string(),
                name: "manual-1".to_string(),
            },
        })
    );
}

#[test]
fn handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "old-1".to_string(), used: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@old-1");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('b'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@2026-09-01");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_char_edits_focused_field() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t'));
    app.handle_modal_key(KeyCode::Char('a'));
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('k'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "tank"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_tab_moves_focus_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Tab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
        other => panic!("expected Form modal, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Tab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0, "must wrap back to the first field"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_backtab_moves_focus_backward_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n')); // CreatePool form, 2 fields, focused = 0
    app.handle_modal_key(KeyCode::BackTab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1, "BackTab from the first field must wrap to the last"),
        other => panic!("expected Form modal, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::BackTab);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_up_and_down_move_focus_the_same_as_backtab_and_tab() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n')); // CreatePool form, 2 fields, focused = 0
    app.handle_modal_key(KeyCode::Down);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
        other => panic!("expected Form modal, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Up);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 0),
        other => panic!("expected Form modal, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Up);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1, "Up from the first field must wrap to the last, same as BackTab"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn failed_submission_resets_focus_to_the_first_field_so_an_invalid_earlier_field_is_visible_without_tabbing_through_everything() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries_for = Some("/tank/data".to_string());
    app.handle_key(KeyCode::Char('n')); // AddAclEntry form, 4 fields: Recipient, Permissions, Flags, Type
    for c in "fb".chars() {
        app.handle_modal_key(KeyCode::Char(c)); // bad Recipient -- needs "user:fb"
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "read_data".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // -> Flags
    app.handle_modal_key(KeyCode::Tab); // -> Type (focused = 3)
    app.handle_modal_key(KeyCode::Enter); // fails: bad Recipient
    match &app.modal {
        Some(Modal::Form { focused, .. }) => {
            assert_eq!(*focused, 0, "a failed submission should land back on the first field, not stay on the last one that happened to be focused");
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_enter_on_non_last_field_advances_focus() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { focused, .. }) => assert_eq!(*focused, 1),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_form_enter_on_last_field_with_empty_required_value_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t')); // name = "t"
    app.handle_modal_key(KeyCode::Tab); // move to vdev spec field, leave it empty
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None, "an empty required field must block submission");
    assert!(app.modal.is_some(), "the form must stay open so the user can fill in the missing field");
}

#[test]
fn handle_modal_key_form_enter_on_last_field_with_valid_values_submits() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    for c in "tank".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "mirror da0 da1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = submit_and_confirm(&mut app);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Pools,
            call: crate::ui::modal::MutationCall::CreatePool {
                name: "tank".to_string(),
                vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()],
            },
        })
    );
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_form_esc_cancels() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    let result = app.handle_modal_key(KeyCode::Esc);
    assert_eq!(result, None);
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_form_backspace_edits_focused_field() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Char('t'));
    app.handle_modal_key(KeyCode::Char('x'));
    app.handle_modal_key(KeyCode::Backspace);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, .. }) => assert_eq!(fields[0].value, "t"),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_o_on_leaf_disk_opens_offline_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('o'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.section, Section::Vdevs);
            assert_eq!(action.call, crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_o_on_group_vdev_row_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('o'));
    assert!(app.modal.is_none());
    assert!(app.status_message.is_some(), "selecting a group row should hint that a disk must be selected instead");
}

#[test]
fn handle_key_shift_o_on_leaf_disk_opens_online_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Offline }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('O'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, crate::ui::modal::MutationCall::OnlineDisk { pool: "tank".to_string(), disk: "da1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn handle_key_x_on_leaf_disk_opens_replace_disk_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(
                *kind,
                crate::ui::modal::FormKind::ReplaceDisk { host: 0, pool: "tank".to_string(), old_disk: "da1".to_string() }
            );
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_a_in_vdev_drilldown_opens_add_vdev_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('a'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::AddVdev { host: 0, pool: "tank".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_add_vdev_form_submits_with_correct_pool_and_vdev_spec() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('a'));
    for c in "mirror da2 da3".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = submit_and_confirm(&mut app);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: crate::ui::modal::MutationCall::AddVdev {
                pool: "tank".to_string(),
                vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()],
            },
        })
    );
}

#[test]
fn handle_key_x_on_group_vdev_row_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "raidz1-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    assert!(app.modal.is_none());
    assert!(app.status_message.is_some());
}

#[test]
fn handle_modal_key_replace_disk_form_submits_with_correct_pool_and_disks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    for c in "da2".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = submit_and_confirm(&mut app);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: crate::ui::modal::MutationCall::ReplaceDisk {
                pool: "tank".to_string(),
                old_disk: "da1".to_string(),
                new_disk: "da2".to_string(),
            },
        })
    );
}

#[test]
fn dispatch_mutation_on_vdev_section_queues_both_vdevs_and_pools_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Vdevs,
        call: crate::ui::modal::MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da1".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Vdevs, Section::Pools, Section::Disks]);
}

#[test]
fn right_arrow_on_pools_enters_right_focus_and_clears_properties() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    assert!(app.pool_properties.is_empty(), "entering right focus clears stale properties until Task 5's fetch repopulates them");
    assert_eq!(app.pool_properties_table.selected(), None);
}

#[test]
fn right_arrow_on_hosts_is_a_noop() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
}

#[test]
fn left_arrow_returns_focus_to_middle() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    app.handle_key(KeyCode::Left);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
}

#[test]
fn esc_returns_focus_to_middle_when_right_focused_without_clearing_status_message() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Right);
    app.status_message = Some("keep me".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    assert_eq!(app.status_message, Some("keep me".to_string()), "backing out of right-focus takes priority over the generic Esc-clears-status-message arm, same precedence as the vdev drill-down");
}

#[test]
fn esc_still_clears_status_message_when_middle_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn tab_cycles_sections_and_resets_pane_focus() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Right);
    app.handle_key(KeyCode::Tab); // -> Datasets
    assert_eq!(app.current_section(), Section::Datasets);
    assert_eq!(app.pane_focus, PaneFocus::Middle, "switching sections resets focus back to the middle pane");
}

#[test]
fn backtab_cycles_sections_backward() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Dashboard, "Hosts is the second section now");
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Settings, "must wrap backward from the first section to the last");
}

#[test]
fn up_down_on_pools_middle_focus_clears_properties_on_row_change() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![
        Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pools_table.selected(), Some(0));
    assert!(app.pool_properties.is_empty(), "moving to a different pool row must not leave the previous row's properties visible");
    assert_eq!(app.pool_properties_table.selected(), None);
}

#[test]
fn up_down_on_pools_right_focus_moves_properties_table_not_pools_table() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![
        Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local },
        Property { name: "p2".to_string(), value: "v2".to_string(), source: PropertySource::Local },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.pool_properties_table.selected(), Some(0));
    assert_eq!(app.pools_table.selected(), Some(0), "pools_table selection must be untouched while right-focused");
}

#[test]
fn up_down_on_datasets_middle_focus_clears_properties_on_row_change() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
    ];
    app.dataset_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Down);
    assert!(app.dataset_properties.is_empty());
    assert_eq!(app.dataset_properties_table.selected(), None);
}

#[test]
fn up_down_on_datasets_right_focus_moves_properties_table() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "p1".to_string(), value: "v1".to_string(), source: PropertySource::Local }];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.dataset_properties_table.selected(), Some(0));
}

#[test]
fn switching_host_resets_pane_focus_and_clears_properties() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "x".to_string(), value: "y".to_string(), source: PropertySource::Local }];
    app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() }];
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Tab); // -> Datasets, resets pane_focus to Middle first
    app.handle_key(KeyCode::BackTab); // -> Pools again
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert_eq!(app.pane_focus, PaneFocus::Middle);
    assert!(app.pool_properties.is_empty());
    assert_eq!(app.pool_properties_table.selected(), None);
    assert!(app.dataset_properties.is_empty());
    assert_eq!(app.dataset_properties_table.selected(), None);
    assert!(app.disks.is_empty());
    assert_eq!(app.disks_table.selected(), None);
    assert!(app.selected_disks.is_empty());
}

#[test]
fn refresh_disks_populates_disks_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(
        FakeRunner::new("")
            .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
            .route("zpool", "list", "tank\t0\t0\t0\tONLINE\n")
            .route(
                "zpool",
                "status",
                "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME  STATE     READ WRITE CKSUM\n\ttank  ONLINE       0     0     0\n\t  da0 ONLINE       0     0     0\n\nerrors: No known data errors\n",
            ),
    );
    app.refresh_disks(runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.disks.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.disks.len(), 1);
    assert_eq!(
        app.disks[0].membership,
        crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() }
    );
}

#[test]
fn refresh_disks_does_not_spawn_second_thread_while_in_flight() {
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(
        FakeRunner::new("")
            .route("geom", "disk", "Geom name: da0\nProviders:\n1. Name: da0\n   Mediasize: 268435456 (256M)\n")
            .route("zpool", "list", "")
            .route("zpool", "status", ""),
    );
    let runner: Arc<dyn CommandRunner> = fake.clone();
    app.refresh_disks(runner.clone());
    app.refresh_disks(runner.clone());

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.disks.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 2, "geom disk list + zpool list; zpool status is skipped since the empty zpool list fixture has no pools");
}

#[test]
fn disks_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx
        .send(WorkerMsg::Disks(1, Ok(vec![crate::zfs::disk::DiskInfo {
            name: "da0".to_string(),
            size: 0,
            membership: crate::zfs::disk::DiskMembership::Available,
            ..Default::default()
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.disks.is_empty(), "a result for a host that isn't selected must not populate the pane");
}

#[test]
fn stale_disk_selection_is_dropped_when_refresh_shows_it_claimed_or_gone() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_disks.insert("da0".to_string());
    app.selected_disks.insert("da1".to_string());
    app.tx
        .send(WorkerMsg::Disks(
            0,
            Ok(vec![
                crate::zfs::disk::DiskInfo {
                    name: "da0".to_string(),
                    size: 0,
                    membership: crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() },
            ..Default::default()
                },
                // da1 is missing entirely from this refresh (e.g. unplugged)
            ]),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.selected_disks.is_empty(), "da0 became claimed and da1 disappeared — both must be dropped from the stale selection");
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
    app.datasets = vec![Dataset { name: "tank/old".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.dataset_permissions = vec![PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }];
    app.dataset_permissions_table.select(Some(0));
    app.dataset_permissions_for = Some("tank/old".to_string());

    app.tx
        .send(WorkerMsg::Datasets(
            0,
            Ok(vec![Dataset { name: "tank/new".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert!(app.dataset_permissions.is_empty(), "stale permissions for tank/old must be cleared once the list refresh re-points the selection at tank/new");
    assert_eq!(app.dataset_permissions_for, None);
}

#[test]
fn selected_pool_name_and_selected_dataset_name_helpers() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    assert_eq!(app.selected_pool_name(), Some("tank".to_string()));
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    assert_eq!(app.selected_dataset_name(), Some("tank/data".to_string()));
    assert_eq!(App::new(vec!["local".to_string()]).selected_pool_name(), None);
}

#[test]
fn pools_list_replacement_that_re_points_selected_index_at_a_different_pool_clears_stale_properties() {
    // Reviewer's repro: view "tank/b"'s properties (index N), then a
    // list-changing mutation reorders/replaces the list so index N now
    // points at a different pool. The stale properties (and their
    // selection) must be cleared rather than silently kept and shown
    // under the new pool's title.
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![
        Pool { name: "aa".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
    ];
    app.handle_key(KeyCode::Tab); // Hosts -> Pools
    app.pools_table.select(Some(1)); // "b" selected at index 1
    app.handle_key(KeyCode::Right); // enter Right focus
    assert_eq!(app.selected_pool_name(), Some("b".to_string()));

    // Simulate the successful fetch of "b"'s properties landing.
    app.tx
        .send(WorkerMsg::PoolProperties(
            0,
            "b".to_string(),
            Ok(vec![Property { name: "comment".to_string(), value: "b's value".to_string(), source: PropertySource::Local }]),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pool_properties.len(), 1);
    app.pool_properties_table.select(Some(0));

    // Now a list refresh lands whose new list re-sorts "aa" before "b" —
    // index 1 (still selected) now points at a pool named "different",
    // not "b" at all.
    app.tx
        .send(WorkerMsg::Pools(
            0,
            Ok(vec![
                Pool { name: "aaa".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
                Pool { name: "different".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
            ]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert_eq!(app.selected_pool_name(), Some("different".to_string()), "index 1 now points at a different pool");
    assert!(
        app.pool_properties.is_empty(),
        "stale properties for the old pool must be cleared once the list no longer matches what they were fetched for"
    );
    assert_eq!(app.pool_properties_table.selected(), None);
}

#[test]
fn datasets_list_replacement_that_re_points_selected_index_at_a_different_dataset_clears_stale_properties() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![
        Dataset { name: "tank/aa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        Dataset { name: "tank/b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
    ];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets_table.select(Some(1)); // "tank/b"
    app.handle_key(KeyCode::Right);
    assert_eq!(app.selected_dataset_name(), Some("tank/b".to_string()));

    app.tx
        .send(WorkerMsg::DatasetProperties(
            0,
            "tank/b".to_string(),
            Ok(vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }]),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.dataset_properties.len(), 1);
    app.dataset_properties_table.select(Some(0));

    app.tx
        .send(WorkerMsg::Datasets(
            0,
            Ok(vec![
                Dataset { name: "tank/aaa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
                Dataset { name: "tank/different".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
            ]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert_eq!(app.selected_dataset_name(), Some("tank/different".to_string()));
    assert!(
        app.dataset_properties.is_empty(),
        "stale properties for the old dataset must be cleared once the list no longer matches what they were fetched for"
    );
    assert_eq!(app.dataset_properties_table.selected(), None);
}

#[test]
fn pools_list_replacement_that_keeps_same_pool_at_same_index_does_not_clear_properties() {
    // The common, correct case: a refresh lands but the selected pool's
    // identity at the selected index is unchanged — properties must
    // survive.
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.tx
        .send(WorkerMsg::PoolProperties(
            0,
            "tank".to_string(),
            Ok(vec![Property { name: "comment".to_string(), value: "v".to_string(), source: PropertySource::Local }]),
        ))
        .unwrap();
    app.drain_worker_messages();
    app.pool_properties_table.select(Some(0));

    // Refresh returns the same pool (e.g. updated size) at the same index.
    app.tx
        .send(WorkerMsg::Pools(
            0,
            Ok(vec![Pool { name: "tank".to_string(), size: 999, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }]),
        ))
        .unwrap();
    app.drain_worker_messages();

    assert_eq!(app.pool_properties.len(), 1, "properties must survive a refresh that still points at the same pool");
    assert_eq!(app.pool_properties_table.selected(), Some(0));
}

#[test]
fn enter_on_right_focused_pool_property_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "comment".to_string(), value: "old comment".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, description, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].value, "old comment", "the form must be pre-filled with the property's current value");
            assert_eq!(*kind, crate::ui::modal::FormKind::EditPoolProperty { host: 0, pool: "tank".to_string(), key: "comment".to_string() });
            assert_eq!(description.as_deref(), Some("A free-form text comment attached to this pool"));
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn enter_on_right_focused_property_with_no_known_description_leaves_it_none() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "some_future_property".to_string(), value: "x".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { description, .. }) => assert_eq!(*description, None),
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn enter_on_right_focused_read_only_pool_property_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "size".to_string(), value: "4000000000000".to_string(), source: PropertySource::Other("-".to_string()) }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("'size' is a read-only property".to_string()));
}

#[test]
fn enter_on_right_focused_pool_property_with_none_selected_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none());
}

#[test]
fn handle_modal_key_edit_pool_property_form_submits_with_new_value() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.pool_properties = vec![Property { name: "comment".to_string(), value: "old".to_string(), source: PropertySource::Local }];
    app.pool_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    // clear the pre-filled value, then type a new one
    for _ in 0.."old".len() {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "new comment".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::PoolProperties,
            call: crate::ui::modal::MutationCall::SetPoolProperty {
                pool: "tank".to_string(),
                key: "comment".to_string(),
                value: "new comment".to_string(),
            },
        })
    );
}

#[test]
fn enter_on_right_focused_dataset_property_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, description, .. }) => {
            assert_eq!(fields[0].value, "lz4");
            assert_eq!(*kind, crate::ui::modal::FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".to_string(), key: "compression".to_string() });
            assert_eq!(
                description.as_deref(),
                Some("Compression algorithm applied to data written to this dataset (e.g. off, lz4, zstd, gzip)")
            );
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn enter_on_right_focused_read_only_dataset_property_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "used".to_string(), value: "1234567".to_string(), source: PropertySource::Other("-".to_string()) }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("'used' is a read-only property".to_string()));
}

#[test]
fn handle_modal_key_edit_dataset_property_form_submits_with_new_value() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties = vec![Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local }];
    app.dataset_properties_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    for _ in 0.."lz4".len() {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "zstd".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::DatasetProperties,
            call: crate::ui::modal::MutationCall::SetDatasetProperty {
                dataset: "tank/data".to_string(),
                key: "compression".to_string(),
                value: "zstd".to_string(),
            },
        })
    );
}

#[test]
fn space_toggles_selection_on_available_disk() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.handle_key(KeyCode::Tab); // -> Snapshots
    app.handle_key(KeyCode::Tab); // -> Disks
    app.disks = vec![crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() }];
    app.disks_table.select(Some(0));
    app.handle_key(KeyCode::Char(' '));
    assert!(app.selected_disks.contains("da0"));
    app.handle_key(KeyCode::Char(' '));
    assert!(!app.selected_disks.contains("da0"), "pressing Space again deselects it");
}

#[test]
fn space_on_claimed_disk_sets_hint_and_does_not_select() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.disks = vec![crate::zfs::disk::DiskInfo {
        name: "da0".to_string(),
        size: 0,
        membership: crate::zfs::disk::DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() },
            ..Default::default()
    }];
    app.disks_table.select(Some(0));
    app.handle_key(KeyCode::Char(' '));
    assert!(app.selected_disks.is_empty());
    assert_eq!(app.status_message, Some("disk 'da0' already in pool 'tank'".to_string()));
}

#[test]
fn esc_clears_disk_selection_in_disks_section() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Esc);
    assert!(app.selected_disks.is_empty());
}

#[test]
fn up_down_in_disks_section_moves_disks_table_selection() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.disks = vec![
        crate::zfs::disk::DiskInfo { name: "da0".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() },
        crate::zfs::disk::DiskInfo { name: "da1".to_string(), size: 0, membership: crate::zfs::disk::DiskMembership::Available, ..Default::default() },
    ];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.disks_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.disks_table.selected(), Some(1));
}

fn identity_disk(name: &str, model: &str, serial: &str) -> crate::zfs::disk::DiskInfo {
    crate::zfs::disk::DiskInfo {
        name: name.to_string(),
        size: 4_000_787_030_016,
        model: Some(model.to_string()),
        serial: Some(serial.to_string()),
        media: crate::zfs::disk::DiskMedia::Hdd(5400),
        ..Default::default()
    }
}

fn disk_form_description(key: char) -> String {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.disks = vec![identity_disk("da0", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567"), identity_disk("da1", "Samsung SSD 860", "S3Z2NB0K123")];
    app.selected_disks.insert("da1".to_string());
    app.selected_disks.insert("da0".to_string());
    app.selected_disks.insert("gone9".to_string());
    app.handle_key(KeyCode::Char(key));
    match &app.modal {
        Some(Modal::Form { description: Some(d), fields, .. }) => {
            assert_eq!(fields.len(), 2, "fields unchanged");
            d.clone()
        }
        other => panic!("expected Form with description, got {other:?}"),
    }
}

#[test]
fn disk_forms_list_each_selected_disks_identity_in_the_description() {
    for key in ['n', 'a'] {
        let d = disk_form_description(key);
        for want in ["WDC WD40EFRX-68N32N0", "WD-WCC7K1234567", "Samsung SSD 860", "S3Z2NB0K123"] {
            assert!(d.contains(want), "{key}: missing {want:?} in {d:?}");
        }
        let lines: Vec<&str> = d.lines().collect();
        assert_eq!(lines.len(), 3, "one line per selected disk: {d:?}");
        assert!(lines[0].starts_with("da0") && lines[1].starts_with("da1"));
        assert_eq!(lines[2], "gone9", "unknown disk shows just its name");
    }
}

fn app_with_selected_disks(disks: Vec<crate::zfs::disk::DiskInfo>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    for d in &disks {
        app.selected_disks.insert(d.name.clone());
    }
    app.disks = disks;
    app
}

#[test]
fn disk_form_description_caps_at_six_lines_plus_more_selected() {
    let disks: Vec<_> = (0..8).map(|i| identity_disk(&format!("da{i}"), "Model", "SN")).collect();
    let app = app_with_selected_disks(disks);
    let names: Vec<String> = (0..8).map(|i| format!("da{i}")).collect();
    let d = app.selected_disk_identities(&names);
    let lines: Vec<&str> = d.lines().collect();
    assert_eq!(lines.len(), 7, "{d:?}");
    assert!(lines[5].starts_with("da5") && lines[6] == "+2 more selected", "{d:?}");
    let six: Vec<String> = names[..6].to_vec();
    assert_eq!(app.selected_disk_identities(&six).lines().count(), 6, "exactly six: no suffix");
}

#[test]
fn disk_form_description_strips_escapes_newlines_and_bidi_from_model_and_serial() {
    let evil = identity_disk("da0", "Ev\u{1b}[31mil\nModel\u{202e}X", "SN\u{7}\u{9b}1");
    for key in ['n', 'a'] {
        let mut app = app_with_selected_disks(vec![evil.clone()]);
        app.handle_key(KeyCode::Char(key));
        let Some(Modal::Form { description: Some(d), .. }) = &app.modal else { panic!("no form") };
        assert_eq!(d.lines().count(), 1, "{d:?}");
        for bad in ['\u{1b}', '\n', '\u{202e}', '\u{7}', '\u{9b}'] {
            assert!(!d.contains(bad), "{bad:?} leaked into {d:?}");
        }
        assert!(d.contains("Model") && d.contains("SN"));
    }
}

#[test]
fn handle_key_n_in_disks_with_selection_opens_create_pool_from_disks_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da1".to_string());
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(
                *kind,
                FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] },
                "disks must be sorted for a deterministic snapshot"
            );
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_n_in_disks_without_selection_sets_hint() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
}

#[test]
fn handle_key_a_in_disks_with_selection_opens_add_vdev_from_disks_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('a'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 2);
            assert_eq!(*kind, FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_a_in_disks_without_selection_sets_hint() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.handle_key(KeyCode::Char('a'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("select at least one disk with Space first".to_string()));
}

#[test]
fn build_pending_action_create_pool_from_disks_blank_redundancy_stripes_each_disk() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Pools,
            call: MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["da0".to_string(), "da1".to_string()] },
        }
    );
}

#[test]
fn build_pending_action_create_pool_from_disks_with_redundancy_groups_all_disks() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string(), "da1".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::CreatePool { name: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da0".to_string(), "da1".to_string()] }
    );
}

#[test]
fn build_pending_action_create_pool_from_disks_rejects_empty_name() {
    let kind = FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".to_string()] };
    let fields = vec![
        FormField { label: "Name".to_string(), value: "  ".to_string() },
        FormField { label: "Redundancy".to_string(), value: String::new() },
    ];
    assert_eq!(build_pending_action(&kind, &fields), None);
}

#[test]
fn build_pending_action_add_vdev_from_disks_rejects_empty_pool_name() {
    let kind = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".to_string()] };
    let fields = vec![
        FormField { label: "Target pool name".to_string(), value: "   ".to_string() },
        FormField { label: "Redundancy".to_string(), value: String::new() },
    ];
    assert_eq!(build_pending_action(&kind, &fields), None);
}

#[test]
fn build_pending_action_add_vdev_from_disks() {
    let kind = FormKind::AddVdevFromDisks { host: 0, disks: vec!["da2".to_string(), "da3".to_string()] };
    let fields = vec![
        FormField { label: "Target pool name".to_string(), value: "tank".to_string() },
        FormField { label: "Redundancy".to_string(), value: "mirror".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Vdevs,
            call: MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["mirror".to_string(), "da2".to_string(), "da3".to_string()] },
        }
    );
}

#[test]
fn selected_disks_survives_a_failed_create_pool_from_disks_submission() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..4 {
        app.handle_key(KeyCode::Tab);
    } // -> Disks
    app.selected_disks.insert("da0".to_string());
    app.handle_key(KeyCode::Char('n'));
    for c in "tank".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // move to the Redundancy field
    let result = submit_and_confirm(&mut app);
    assert!(result.is_some(), "the form still submits and returns a PendingAction");
    assert!(
        app.selected_disks.contains("da0"),
        "the selection must survive the form submit itself — it's only cleared once the mutation actually succeeds"
    );
}

#[test]
fn selected_disks_cleared_once_the_resulting_mutation_succeeds() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_disks.insert("da0".to_string());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.selected_disks.is_empty());
}

#[test]
fn handle_key_shift_r_on_pools_opens_resilver_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab); // -> Pools
    app.pools_table.select(Some(0));
    app.handle_key(KeyCode::Char('R'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, MutationCall::ResilverPool { pool: "tank".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn mutation_done_for_pools_section_also_queues_disks_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    let refreshes = app.drain_worker_messages();
    assert!(refreshes.contains(&Section::Pools));
    assert!(refreshes.contains(&Section::Disks), "a Pools-section mutation (create/destroy pool) changes disk membership too");
}

#[test]
fn refresh_shares_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(
        FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
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
    assert!(app.shares_loaded);
}

#[test]
fn shares_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx
        .send(WorkerMsg::Shares(
            1,
            Ok(crate::zfs::share::SharesSnapshot {
                shares: vec![smb("tank/data")],
                nfs_available: true,
                samba_available: true,
                smb_error: None,
            }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.shares.is_empty(), "a result for a host that isn't selected must not populate the pane");
}

#[test]
fn switching_host_clears_shares_state() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.nfs_available = Some(true);
    app.samba_available = Some(false);
    app.shares_loaded = true;
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert!(app.shares.is_empty());
    assert_eq!(app.shares_table.selected(), None);
    assert_eq!(app.nfs_available, None);
    assert_eq!(app.samba_available, None);
    assert!(!app.shares_loaded);
}

#[test]
fn enter_on_nfs_share_row_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Nfs {
        dataset: "tank/data".to_string(),
        raw: "-maproot=nobody -network 172.16.10.0/24".to_string(),
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

fn datasets_app_with_mount(mountpoint: Option<&str>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/media".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string) }];
    app.datasets_table.select(Some(0));
    app.shares_loaded = true;
    app.samba_available = Some(true);
    app
}

#[test]
fn shift_m_on_datasets_opens_a_blank_smb_form_for_an_unshared_dataset() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('M'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/media".into(), path: "/tank/media".into(), existing_user: None });
            assert_eq!(fields.len(), 5);
            assert_eq!(fields[4].value, "n");
            assert_eq!(fields[0].value, "media", "suggests the dataset's last path component");
            assert_eq!(fields[1].value, "");
        }
        other => panic!("expected Form, got {other:?}"),
    }
}

#[test]
fn shift_m_prefills_from_the_existing_managed_share() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")];
    app.handle_key(KeyCode::Char('M'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields[0].value, "media");
            assert_eq!(fields[1].value, "carol");
            assert_eq!(fields[2].value, "", "the password is never pre-filled");
            assert_eq!(fields[3].value, "n");
            assert!(matches!(kind, FormKind::ShareSmb { existing_user: Some(u), .. } if u == "carol"));
        }
        other => panic!("expected Form, got {other:?}"),
    }
}

#[test]
fn shift_m_is_refused_without_a_mountpoint_before_shares_load_or_when_smbd_is_down() {
    let mut app = datasets_app_with_mount(None);
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("'tank/media' has no mounted mountpoint to share"));
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares_loaded = false;
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.samba_available = Some(false);
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("Samba (smbd) is not running on this host"));
}

#[test]
fn shift_n_and_m_with_no_dataset_selected_are_inert() {
    for key in ['N', 'M'] {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.datasets_table.select(None);
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none() && app.status_message.is_none(), "{key}");
    }
}

/// Feeds each value then Enter through `handle_modal_key` (Enter
/// advances; the 4th submits); returns what the final Enter produced.
fn type_into_smb_form(app: &mut App, values: [&str; 5]) -> Option<PendingAction> {
    let mut last = None;
    for v in values {
        for c in v.chars() {
            // '\u{8}' stands for Backspace (the read-only and guest fields are pre-filled with "n")
            app.handle_modal_key(if c == '\u{8}' { KeyCode::Backspace } else { KeyCode::Char(c) });
        }
        last = app.handle_modal_key(KeyCode::Enter);
    }
    last
}

fn open_blank_smb_form() -> App {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('M'));
    // clear the suggested name so each test types its own
    for _ in 0..5 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    app
}

#[test]
fn submitting_a_valid_smb_form_builds_a_set_smb_share_action() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["media", "carol", " pw 1 ", "\u{8}y", "n"]).expect("submitted");
    assert_eq!(
        action.call,
        MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare { name: "media".into(), dataset: "tank/media".into(), path: "/tank/media".into(), valid_user: Some("carol".into()), read_only: true, guest_ok: false },
            password: Some(crate::ui::modal::Secret(" pw 1 ".to_string())),
        },
        "the password is kept verbatim, not trimmed"
    );
    assert_eq!(action.section, Section::Shares);
}

#[test]
fn smb_form_validation_errors_keep_the_form_open_with_a_specific_message() {
    let cases: [([&str; 5], &str); 6] = [
        (["", "", "", "n", "n"], "required"),
        (["bad name", "", "", "n", "n"], "letters, digits"),
        (["media", "-evil", "pw", "n", "n"], "valid Unix username"),
        (["media", "carol", "", "n", "n"], "Samba password is required"),
        (["alice", "", "", "n", "n"], "already"),
        (["media", "carol", "pw", "n", "\u{8}y"], "Guest access can't be combined"),
    ];
    for (values, expect) in cases {
        let mut app = open_blank_smb_form();
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/other".into(), name: "alice".into(), path: "/x".into(), valid_user: None, read_only: true, guest_ok: false }];
        assert!(type_into_smb_form(&mut app, values).is_none());
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?} must keep the form open");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains(expect), "{values:?}: '{msg}' should contain '{expect}'");
    }
}

#[test]
fn editing_with_a_blank_password_is_allowed_only_when_the_user_is_unchanged() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")]; // existing user: carol
    app.handle_key(KeyCode::Char('M'));
    for _ in 0..4 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    let action = app.handle_modal_key(KeyCode::Enter).expect("unchanged user + blank password is fine");
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, .. }));

    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")];
    app.handle_key(KeyCode::Char('M'));
    app.handle_modal_key(KeyCode::Enter); // to Valid user
    for _ in 0..5 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "bob".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    for _ in 0..4 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "changing the user requires a password");
}

#[test]
fn a_blank_user_ignores_any_typed_password() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "n"]).unwrap();
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.valid_user.is_none()));
}

#[test]
fn a_default_blank_form_is_not_guest_and_builds_no_guest_ok() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["any", "", "", "n", "n"]).unwrap();
    let MutationCall::SetSmbShare { ref share, password: None } = action.call else { panic!("{:?}", action.call) };
    assert!(!share.guest_ok);
    assert!(!crate::zfs::smb::render_with_shares("", std::slice::from_ref(share)).unwrap().contains("guest ok"));
}

#[test]
fn guest_y_with_a_blank_user_builds_a_guest_share_ignoring_the_password() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "\u{8}y"]).unwrap();
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.guest_ok && share.valid_user.is_none()));
}

#[test]
fn editing_an_existing_guest_share_prefills_guest_y() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/media".into(), name: "pub".into(), path: "/tank/media".into(), valid_user: None, read_only: true, guest_ok: true }];
    app.handle_key(KeyCode::Char('M'));
    let Some(Modal::Form { fields, .. }) = &app.modal else { panic!("expected form") };
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[4].value, "y");
}

#[test]
fn smb_form_warns_that_a_password_replaces_the_users_samba_password() {
    let modal = smb_edit_modal(None, "tank/data", "/tank/data", 0);
    let Modal::Form { description: Some(d), .. } = modal else { panic!("expected form with description") };
    assert!(d.contains("replaces"), "{d}");
}

#[test]
fn enter_on_an_smb_share_row_opens_its_prefilled_form_using_the_stanza_path() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::ShareSmb { path, .. }, .. }) if path == "/tank/data"));
}

#[test]
fn handle_key_d_on_smb_share_row_opens_a_remove_confirm_for_the_managed_share() {
    let mut app = App::new(vec!["local".to_string()]);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    }
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => assert_eq!(action.call, MutationCall::RemoveSmbShare { dataset: "tank/data".to_string() }),
        other => panic!("expected Confirm, got {other:?}"),
    }
}

#[test]
fn an_smb_error_in_the_shares_snapshot_sets_an_error_status_but_keeps_the_nfs_shares() {
    let mut app = App::new(vec!["local".to_string()]);
    let nfs = crate::zfs::share::Share::Nfs { dataset: "tank/n".into(), raw: "on".into(), networks: vec![], hosts: vec![], read_only: false, root: None };
    app.tx
        .send(WorkerMsg::Shares(
            0,
            Ok(crate::zfs::share::SharesSnapshot { shares: vec![nfs], nfs_available: true, samba_available: true, smb_error: Some("boom".into()) }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.shares.len(), 1, "NFS shares still populated");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("boom")));
    assert!(app.status_is_error());
}

#[test]
fn handle_key_shift_n_on_datasets_opens_blank_share_nfs_form_when_not_yet_shared() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(true);
    app.shares_loaded = true;
    app.handle_key(KeyCode::Char('N'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            // read_only renders as "n" (not blank) for a false placeholder,
            // consistent with `share_edit_modal`'s handling of real shares
            // (see `enter_on_nfs_share_row_opens_prefilled_edit_form`) —
            // it still assembles to the same "on" value downstream.
            assert_eq!(fields[0].value, "");
            assert_eq!(fields[1].value, "");
            assert_eq!(fields[2].value, "n");
            assert_eq!(fields[3].value, "");
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
    app.shares_loaded = true;
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("NFS server not available on this host (nfsd is not running)".to_string()));
}

#[test]
fn handle_key_shift_n_with_unloaded_shares_names_the_fix() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
}

#[test]
fn handle_key_shift_n_on_datasets_sets_hint_when_shares_not_yet_loaded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(true); // availability known, but the share list itself was never refreshed
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none(), "must not pre-fill from a self.shares that was never populated — could silently widen an existing restricted share");
    assert!(app.status_message.is_some());
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
fn build_pending_action_add_acl_entry_assembles_named_user_with_flags_and_deny() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 3 };
    let fields = vec![
        FormField { label: "Recipient".to_string(), value: "user:nobody".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data, write_data".to_string() },
        FormField { label: "Flags".to_string(), value: "file_inherit,dir_inherit".to_string() },
        FormField { label: "Type".to_string(), value: "deny".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::DatasetAcl,
            call: MutationCall::AddAclEntry {
                path: "/tank/data".to_string(),
                position: 3,
                entry: crate::acl::AclEntry {
                    tag: crate::acl::AclTag::User("nobody".to_string()),
                    permissions: vec!["read_data".to_string(), "write_data".to_string()],
                    flags: vec!["file_inherit".to_string(), "dir_inherit".to_string()],
                    entry_type: crate::acl::AclEntryType::Deny,
                },
            },
        }
    );
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
fn handle_key_d_on_selected_grant_row_revokes_only_that_rows_own_permissions() {
    // Reviewer's repro: fb has "snapshot" as Local+Descendent and
    // "mount" as Local only, so two separate rows show. Revoking the
    // "Local / mount" row must not also delete fb's Local half of
    // "snapshot" -- it must only ever touch exactly the permissions
    // that row displays, not everything for that recipient+scope.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None }];
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

#[test]
fn handle_key_n_on_hosts_section_opens_add_host_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(Modal::Form { title, fields, kind, .. }) => {
            assert_eq!(title, "Add Host");
            assert_eq!(fields.len(), 3);
            assert_eq!(*kind, FormKind::AddHost);
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

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
    open_grant_permission_form(&mut app); // focused = 0, Recipient (live account picker)
    app.handle_modal_key(KeyCode::Tab); // -> Scope, no vocabulary
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
    assert!(app.picker.as_ref().unwrap().filter.is_empty(), "Space must not leak into the type-to-filter text");
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

#[test]
fn submitting_add_host_form_with_blank_name_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Tab); // move to SSH target field
    for c in "example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "blank Name must keep the form open");
}

#[test]
fn submitting_add_host_form_with_blank_target_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "newhost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "blank SSH target must keep the form open");
}

#[test]
fn submitting_add_host_form_with_whitespace_only_name_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "   ".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "a whitespace-only name must be treated as blank");
}

#[test]
fn submitting_add_host_form_with_case_insensitive_duplicate_name_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string(), "fb16".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "FB16".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "a name differing only by case from an existing host must be rejected as a duplicate");
    assert!(app.status_message.is_some(), "a duplicate rejection must say why, per the spec's own testing section -- a silent re-show looks like nothing happened");
}

#[test]
fn submitting_add_host_form_with_duplicate_of_local_keeps_form_open() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "Local".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "\"local\" (case-insensitively) must always be rejected as a duplicate");
    assert!(app.status_message.is_some());
}

#[test]
fn submitting_valid_add_host_form_closes_modal_and_sets_connecting_status() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "newhost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "newhost.example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_none(), "a valid submission must close the form immediately, not wait for the async probe");
    assert_eq!(app.status_message, Some("Connecting to 'newhost.example.com'...".to_string()));
}

#[test]
fn host_probe_success_queues_the_host_for_the_event_loop_to_apply() {
    // Queued, not pushed directly onto `self.hosts` here -- the
    // event loop is the one place that pushes into `app.hosts` and
    // `inventory.hosts` together (see `apply_pending_new_hosts` in
    // `ui/mod.rs`), so the two index-aligned lists can never drift
    // apart. A final review caught that an earlier single-`Option`
    // version of this signal silently dropped a second host whose
    // probe resolved in the same tick as the first's.
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::HostProbe("newhost".to_string(), "newhost.example.com".to_string(), Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(!app.hosts.contains(&"newhost".to_string()), "pushing onto app.hosts happens in the event loop, not here");
    assert_eq!(app.pending_new_hosts, vec![("newhost".to_string(), "newhost.example.com".to_string())]);
}

#[test]
fn host_probe_success_queues_every_host_even_when_several_resolve_before_one_drain() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::HostProbe("hosta".to_string(), "hosta.example.com".to_string(), Ok(()))).unwrap();
    app.tx.send(WorkerMsg::HostProbe("hostb".to_string(), "hostb.example.com".to_string(), Ok(()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(
        app.pending_new_hosts,
        vec![("hosta".to_string(), "hosta.example.com".to_string()), ("hostb".to_string(), "hostb.example.com".to_string())]
    );
}

#[test]
fn host_probe_failure_sets_status_message_and_touches_nothing_else() {
    let mut app = App::new(vec!["local".to_string()]);
    let before = app.hosts.clone();
    app.tx
        .send(WorkerMsg::HostProbe(
            "newhost".to_string(),
            "newhost.example.com".to_string(),
            Err(crate::error::AppError::CommandFailed {
                cmd: "ssh newhost.example.com".to_string(),
                status: 255,
                stderr: "Could not resolve hostname newhost.example.com".to_string(),
            }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.hosts, before, "a failed probe must not add anything to the host list");
    assert!(app.pending_new_hosts.is_empty());
    assert!(app.status_message.as_ref().unwrap().contains("Could not resolve hostname"));
}

#[test]
fn host_jobs_counts_total_enabled_jobs_and_failed_latest_runs_per_host() {
    let mut a = crate::jobs::Job::test_backup("j1", "tank/x", "p", "/r", "/pw");
    a.host = "fbsd16".into();
    let mut b = crate::jobs::Job::test_backup("j2", "tank/y", "p", "/r", "/pw");
    b.host = "fbsd16".into();
    let mut c = crate::jobs::Job::test_backup("j3", "tank/z", "p", "/r", "/pw");
    c.host = "fbsd16".into();
    c.enabled = false;
    let (mut app, dir) = app_with_jobs(&[a, b, c]);
    app.hosts = vec!["local".into(), "fbsd16".into()];
    let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
    h.append(&crate::daemon::history::Event { job_id: "j1".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Finished, at: chrono::Utc::now(), scheduled_for: None, result: Some(crate::daemon::history::RunResult::Failed), message: String::new(), catch_up: false, summary: None }).unwrap();
    app.refresh_jobs();
    assert_eq!(app.host_jobs_counts("fbsd16"), (1, 2), "the disabled job counts nowhere");
    assert_eq!(app.host_jobs_counts("local"), (0, 0));
}

fn filter_app() -> App {
    let mut app = in_section(Section::Hosts);
    app.hosts = vec!["local".into(), "fbsd16".into(), "t1".into(), "nas".into()];
    app.set_host_addresses(vec!["local".into(), "10.0.0.60".into(), "10.0.0.61".into(), "10.0.0.9".into()]);
    app.set_host_tags(vec![String::new(), String::new(), String::new(), "Backup".into()]);
    app.host_probes.insert("t1".into(), HostProbeState { kind: Some(crate::zfs::host_health::HostKind::Vm("bhyve".into())), ..HostProbeState::default() });
    app
}

fn type_filter(app: &mut App, text: &str) {
    app.handle_key(KeyCode::Char('/'));
    for c in text.chars() {
        app.handle_key(KeyCode::Char(c));
    }
}

#[test]
fn slash_filters_live_by_name_address_tag_and_type_ignoring_case() {
    let mut app = filter_app();
    type_filter(&mut app, "back");
    assert_eq!(app.visible_hosts(), vec![3], "tag, case-insensitive");
    for _ in 0..4 {
        app.handle_key(KeyCode::Backspace);
    }
    for c in "10.0.0.6".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert_eq!(app.visible_hosts(), vec![1, 2], "address");
    for _ in 0..8 {
        app.handle_key(KeyCode::Backspace);
    }
    assert_eq!(app.visible_hosts().len(), 4, "an empty filter shows everything");
    for c in "vm".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert_eq!(app.visible_hosts(), vec![2], "type word");
    for _ in 0..2 {
        app.handle_key(KeyCode::Backspace);
    }
    for c in "FBSD".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert_eq!(app.visible_hosts(), vec![1], "name");
}

#[test]
fn typing_in_the_filter_does_not_trigger_other_keys() {
    let mut app = filter_app();
    type_filter(&mut app, "qn");
    assert!(!app.should_quit && app.modal.is_none(), "q and n are text while the filter is open");
}

#[test]
fn the_selection_moves_to_a_visible_host_when_the_filter_hides_it() {
    let mut app = filter_app();
    app.selected_host = 0;
    type_filter(&mut app, "nas");
    assert_eq!(app.selected_host, 3);
}

#[test]
fn up_and_down_move_among_visible_hosts_and_wrap() {
    let mut app = filter_app();
    type_filter(&mut app, "10.0.0.");
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.visible_hosts(), vec![1, 2, 3]);
    assert_eq!(app.selected_host, 1);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_host, 2);
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_host, 1, "wraps within the visible hosts");
    app.handle_key(KeyCode::Up);
    assert_eq!(app.selected_host, 3);
}

#[test]
fn enter_with_no_match_keeps_the_input_open_and_esc_clears_everything() {
    let mut app = filter_app();
    app.selected_host = 2;
    type_filter(&mut app, "zzz");
    assert!(app.visible_hosts().is_empty());
    assert_eq!(app.selected_host, 2, "no match: the selection stays where it was, on a host that exists");
    app.handle_key(KeyCode::Enter);
    assert!(app.host_filter_editing, "cannot commit a filter that matches nothing");
    app.handle_key(KeyCode::Esc);
    assert!(app.host_filter.is_empty() && !app.host_filter_editing);
    assert_eq!(app.visible_hosts().len(), 4);
}

#[test]
fn esc_after_a_committed_filter_clears_it() {
    let mut app = filter_app();
    type_filter(&mut app, "nas");
    app.handle_key(KeyCode::Enter);
    assert!(!app.host_filter_editing && app.host_filter == "nas");
    app.handle_key(KeyCode::Esc);
    assert!(app.host_filter.is_empty());
}

#[test]
fn slash_outside_hosts_is_not_a_filter_and_add_host_clears_one() {
    let mut app = in_section(Section::Pools);
    app.handle_key(KeyCode::Char('/'));
    assert!(!app.host_filter_editing);
    let mut app = filter_app();
    type_filter(&mut app, "nas");
    app.handle_key(KeyCode::Enter);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.host_filter.is_empty(), "opening Add Host clears the filter so the new host will be visible");
}

#[test]
fn esc_while_typing_a_filter_puts_the_selection_back_where_it_was() {
    let mut app = filter_app();
    app.selected_host = 0;
    type_filter(&mut app, "nas");
    assert_eq!(app.selected_host, 3, "the selection follows the filter while typing");
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.selected_host, 0, "cancelling restores the host that was active");
    // A committed filter's Esc only clears the text: the user navigated on purpose.
    type_filter(&mut app, "nas");
    app.handle_key(KeyCode::Enter);
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.selected_host, 3);
}

fn alerts_app(config: &str) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.config_path = dir.path().join("config.toml");
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
    app.refresh_alerts();
    (app, dir)
}

#[test]
fn alerts_comes_after_jobs_and_cloud_and_settings_is_last() {
    let app = App::new(vec!["local".to_string()]);
    assert_eq!(&app.sections[..2], [Section::Dashboard, Section::Hosts], "the Dashboard comes first, then Hosts");
    assert_eq!(app.sections.last(), Some(&Section::Settings));
    assert_eq!(app.sections[app.sections.len() - 2], Section::Alerts);
    assert_eq!(app.sections[app.sections.len() - 3], Section::Cloud);
    assert_eq!(app.sections[app.sections.len() - 4], Section::Jobs);
}

#[test]
fn up_and_down_scroll_the_alert_events_within_bounds() {
    let (mut app, dir) = alerts_app("");
    let log = crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl"));
    for i in 0..5 {
        log.append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "a".into(), kind: "alert".into(), text: format!("n{i}") }).unwrap();
    }
    app.refresh_alerts();
    assert_eq!(app.alerts_view.events.len(), 5);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.alerts_view.scroll, 0, "not above the first");
    for _ in 0..9 {
        app.handle_key(KeyCode::Down);
    }
    assert_eq!(app.alerts_view.scroll, 4, "not past the last");
    app.handle_key(KeyCode::Up);
    assert_eq!(app.alerts_view.scroll, 3);
    app.refresh_alerts();
    assert_eq!(app.alerts_view.scroll, 3, "a reload keeps the scroll position");
}

#[test]
fn new_events_do_not_move_the_line_the_operator_is_reading() {
    let (mut app, dir) = alerts_app("");
    let log = crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl"));
    let add = |text: &str| log.append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "a".into(), kind: "alert".into(), text: text.into() }).unwrap();
    for i in 0..6 {
        add(&format!("old{i}"));
    }
    app.refresh_alerts();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Down);
    let reading = app.alerts_view.events[app.alerts_view.scroll].text.clone();
    add("new1");
    add("new2");
    app.refresh_alerts();
    assert_eq!(app.alerts_view.events[app.alerts_view.scroll].text, reading, "the same event stays at the top of the view");
    let mut at_top = alerts_app("").0;
    at_top.refresh_alerts();
    assert_eq!(at_top.alerts_view.scroll, 0);
}

#[test]
fn the_alerts_data_is_reloaded_every_five_seconds_only_while_the_section_is_open() {
    let (mut app, dir) = alerts_app("");
    let t0 = Instant::now();
    app.last_alerts_refresh = Some(t0);
    std::fs::write(dir.path().join("config.toml"), "[notify]\nmail_to = \"a@x.org\"\n").unwrap();
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(2));
    assert!(!app.alerts_view.monitor_on(), "too soon");
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(6));
    assert!(app.alerts_view.monitor_on());
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    std::fs::write(dir.path().join("config.toml"), "").unwrap();
    app.refresh_alerts_if_stale(t0 + Duration::from_secs(60));
    assert!(app.alerts_view.monitor_on(), "another section is open: no reload");
}

fn open_alerts_form(app: &mut App) -> Vec<String> {
    app.handle_key(KeyCode::Char('e'));
    form_values(app)
}

fn fill(app: &mut App, values: [&str; 6]) {
    if let Some(Modal::Form { fields, .. }) = &mut app.modal {
        for (f, v) in fields.iter_mut().zip(values) {
            f.value = v.to_string();
        }
    }
}

fn submit_alerts_form(app: &mut App) -> Option<PendingAction> {
    let mut last = None;
    for _ in 0..6 {
        last = app.handle_modal_key(KeyCode::Enter);
    }
    last
}

#[test]
fn e_opens_the_form_prefilled_with_the_saved_values_and_blank_for_unset() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 10\nreport_time = \"off\"\nremind_hours = 0\n");
    assert_eq!(open_alerts_form(&mut app), ["me@x.org", "", "", "10", "off", "0"]);
}

#[test]
fn e_is_refused_while_config_toml_cannot_be_read() {
    let (mut app, _dir) = alerts_app("[notify\nbroken");
    app.handle_key(KeyCode::Char('e'));
    assert!(app.modal.is_none());
    assert!(app.status_message.clone().unwrap_or_default().contains("could not be read"), "{:?}", app.status_message);
}

#[test]
fn a_bad_value_is_refused_with_the_daemons_message_and_the_form_stays_open() {
    for (values, want) in [
        (["not an address", "", "", "", "", ""], "mail_to"),
        (["a@x.org", "", "", "abc", "", ""], "whole number"),
        (["a@x.org", "", "", "0", "", ""], "check_interval_minutes"),
        (["a@x.org", "", "", "", "7am", ""], "report_time"),
        (["a@x.org", "", "", "", "", "-1"], "whole number"),
        (["a@x.org", "", "", "", "", "99999"], "remind_hours"),
    ] {
        let (mut app, _dir) = alerts_app("");
        open_alerts_form(&mut app);
        fill(&mut app, values);
        assert!(submit_alerts_form(&mut app).is_none());
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?}: the form stays open");
        assert!(app.status_message.clone().unwrap_or_default().contains(want), "{values:?}: {:?}", app.status_message);
    }
}

#[test]
fn a_good_form_asks_for_the_two_key_confirmation_and_saves_only_the_notify_section() {
    let (mut app, dir) = alerts_app("# keep\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    open_alerts_form(&mut app);
    fill(&mut app, ["me@x.org", "", "", "10", "", ""]);
    assert!(submit_alerts_form(&mut app).is_none(), "no save before the confirmation");
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("me@x.org") && prompt.contains("10 min") && prompt.contains("07:00"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter alone does nothing");
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter applies");
    let MutationCall::SaveNotify { config_path, section } = &action.call else { panic!("{:?}", action.call) };
    assert_eq!(section, "[notify]\nmail_to = \"me@x.org\"\ncheck_interval_minutes = 10\n");
    crate::config::save_notify_section(config_path, section).unwrap();
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(saved.starts_with("# keep\n[[hosts]]") && saved.contains("mail_to = \"me@x.org\""), "{saved}");
}

#[test]
fn declining_the_save_returns_the_form_with_what_was_typed_and_writes_nothing() {
    let (mut app, dir) = alerts_app("");
    open_alerts_form(&mut app);
    fill(&mut app, ["me@x.org", "", "", "10", "", ""]);
    submit_alerts_form(&mut app);
    app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert_eq!(form_values(&app)[0], "me@x.org");
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), "");
}

#[test]
fn clearing_the_address_is_allowed_and_says_email_off_in_the_prompt() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\n");
    open_alerts_form(&mut app);
    fill(&mut app, ["", "", "", "", "", ""]);
    submit_alerts_form(&mut app);
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("Email off") && prompt.contains("remind every 6 hours"), "{prompt}");
}

#[test]
fn a_saved_note_replaces_the_blank_status_after_a_successful_save_and_is_dropped_on_failure() {
    let (mut app, _dir) = alerts_app("");
    app.done_note = Some("Alert settings saved; the daemon applies them within seconds".to_string());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message.as_deref(), Some("Alert settings saved; the daemon applies them within seconds"));
    app.done_note = Some("x".into());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Err(crate::error::AppError::Config("boom".into())))).unwrap();
    app.drain_worker_messages();
    assert!(app.done_note.is_none());
}

#[test]
fn dispatching_a_save_remembers_its_note_and_other_calls_leave_it_alone() {
    let (mut app, dir) = alerts_app("");
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::exec::test_support::FakeRunner::new(""));
    let save = PendingAction { host: 0, section: Section::Alerts, call: MutationCall::SaveNotify { config_path: dir.path().join("config.toml"), section: "[notify]\nmail_to = \"a@x.org\"\n".into() } };
    app.dispatch_mutation(save, Arc::clone(&runner));
    assert!(app.done_note.as_deref().is_some_and(|n| n.contains("saved")));
    let other = PendingAction { host: 0, section: Section::Jobs, call: MutationCall::DeleteJob { state_dir: dir.path().to_path_buf(), id: "none".into() } };
    app.dispatch_mutation(other, runner);
    assert!(app.done_note.is_some(), "another call neither clears nor takes the Alerts note");
}

#[test]
fn t_with_nothing_configured_says_so_and_asks_nothing() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none());
    assert!(app.status_message.clone().unwrap_or_default().contains("set an email address"), "{:?}", app.status_message);
}

#[test]
fn t_asks_first_naming_the_recipient_and_the_command_and_sends_only_after_y_then_enter() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\nmail_command = \"msmtp -a default -t\"\n");
    app.handle_key(KeyCode::Char('t'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("me@x.org") && prompt.contains("msmtp -a default -t") && prompt.contains("real message"), "{prompt}");
    assert!(matches!(action.call, MutationCall::SendTestMessage { .. }));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter alone does nothing");
    app.handle_modal_key(KeyCode::Char('y'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some());
}

#[test]
fn t_declined_sends_nothing() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\n");
    app.handle_key(KeyCode::Char('t'));
    app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none());
}

#[test]
fn t_names_the_hook_when_only_a_hook_is_set() {
    let (mut app, _dir) = alerts_app("[daemon]\nnotify_command = \"curl -s x\"\n");
    app.handle_key(KeyCode::Char('t'));
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("notify hook") && !prompt.contains("To:"), "{prompt}");
}

#[test]
fn t_with_an_invalid_saved_section_refuses_with_the_reason() {
    let (mut app, _dir) = alerts_app("[notify]\nmail_to = \"nope\"\n");
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none() && app.status_message.clone().unwrap_or_default().contains("mail_to"), "{:?}", app.status_message);
}

#[test]
fn p_runs_the_preview_in_the_background_and_shows_it_in_a_popup_and_not_twice_at_once() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('p'));
    assert!(app.alerts_preview_busy);
    assert!(app.status_message.clone().unwrap_or_default().contains("Preparing"));
    let note = app.status_message.clone();
    app.handle_key(KeyCode::Char('p'));
    assert_eq!(app.status_message, note, "a second press while it runs does nothing");
    assert_eq!(app.alerts_previews_started, 1, "and starts no second probe");
    // The real worker finishes on its own; wait for its result.
    let start = Instant::now();
    while app.modal.is_none() && start.elapsed() < Duration::from_secs(90) {
        app.drain_worker_messages();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!app.alerts_preview_busy);
    let Some(Modal::Info { title, body }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(title.contains("nothing sent") && body.contains("Subject: [zfsctl] daily report"), "{title}\n{body}");
}

#[test]
fn a_preview_result_is_shown_even_when_it_arrives_after_leaving_the_screen() {
    let (mut app, _dir) = alerts_app("");
    app.alerts_preview_busy = true;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    app.tx.send(WorkerMsg::AlertsPreview("Subject: x\n\nbody".to_string())).unwrap();
    app.drain_worker_messages();
    assert!(!app.alerts_preview_busy);
    assert!(matches!(&app.modal, Some(Modal::Info { body, .. }) if body.contains("Subject: x")));
}

#[test]
fn only_an_alerts_result_takes_the_alerts_note_and_one_for_another_section_leaves_it() {
    let (mut app, _dir) = alerts_app("");
    app.done_note = Some("Test message sent: the mail command accepted it".to_string());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Jobs, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.as_deref() != Some("Test message sent: the mail command accepted it"), "a Jobs result must not announce a test message");
    assert!(app.done_note.is_some());
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message.as_deref(), Some("Test message sent: the mail command accepted it"));
}

#[test]
fn an_alerts_result_is_shown_even_when_another_host_is_selected_now() {
    let (mut app, _dir) = alerts_app("");
    app.hosts.push("other".to_string());
    app.selected_host = 1;
    app.tx.send(WorkerMsg::MutationDone(0, Section::Alerts, Err(crate::error::AppError::Config("mail: failed: exited with status 78".into())))).unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.clone().unwrap_or_default().contains("status 78"), "{:?}", app.status_message);
}

#[test]
fn while_a_test_message_is_being_sent_the_screen_says_so_and_t_does_not_send_a_second() {
    let (mut app, dir) = alerts_app("[notify]\nmail_to = \"me@x.org\"\nmail_command = \"true\"\n");
    let runner: Arc<dyn CommandRunner> = Arc::new(crate::exec::test_support::FakeRunner::new(""));
    let settings = app.alerts_view.settings.clone().unwrap();
    app.dispatch_mutation(PendingAction { host: 0, section: Section::Alerts, call: MutationCall::SendTestMessage { settings: Box::new(settings), hook: None } }, runner);
    assert!(app.alerts_test_busy);
    assert!(app.status_message.clone().unwrap_or_default().contains("Sending"), "{:?}", app.status_message);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none(), "no second confirmation while one is in flight");
    assert!(app.status_message.clone().unwrap_or_default().contains("already"), "{:?}", app.status_message);
    let start = Instant::now();
    while app.alerts_test_busy && start.elapsed() < Duration::from_secs(20) {
        app.drain_worker_messages();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!app.alerts_test_busy, "cleared when the result arrives");
    let _ = dir;
}

#[test]
fn a_preview_that_arrives_while_a_form_is_open_is_parked_and_the_form_is_left_alone() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('e'));
    app.alerts_preview_busy = true;
    app.tx.send(WorkerMsg::AlertsPreview("Subject: x\n\nbody".to_string())).unwrap();
    app.drain_worker_messages();
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "the operator's form is untouched");
    assert!(app.status_message.clone().unwrap_or_default().contains("preview is ready"), "{:?}", app.status_message);
    assert!(!app.alerts_preview_busy);
    app.handle_modal_key(KeyCode::Esc);
    app.handle_key(KeyCode::Char('p'));
    assert!(matches!(&app.modal, Some(Modal::Info { body, .. }) if body.contains("Subject: x")), "p shows the parked preview");
    assert_eq!(app.alerts_previews_started, 0, "without probing again");
}

#[test]
fn the_save_confirmation_shows_the_mail_command_that_will_run_as_root() {
    let (mut app, _dir) = alerts_app("");
    app.handle_key(KeyCode::Char('e'));
    fill(&mut app, ["me@x.org", "msmtp -a work -t", "", "", "", ""]);
    submit_alerts_form(&mut app);
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("msmtp -a work -t"), "{prompt}");
}

#[test]
fn help_documents_the_alerts_section_and_its_keys() {
    let text = help_text();
    for want in ["Alerts", "edit the alert settings", "test message", "preview"] {
        assert!(text.contains(want), "missing {want:?}");
    }
}

#[test]
fn help_documents_the_hosts_filter_key() {
    let text = help_text();
    assert!(text.contains("filter"), "{text}");
}

#[test]
fn the_type_is_cached_once_seen_and_a_failed_probe_does_not_erase_it() {
    use crate::zfs::host_health::{HostKind, HostProbe};
    let mut app = App::new(vec!["local".into(), "a".into()]);
    let reached = |kind| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind, os: None };
    app.tx.send(WorkerMsg::HostHealth("a".into(), reached(Some(HostKind::Vm("bhyve".into()))))).unwrap();
    app.drain_worker_messages();
    app.tx.send(WorkerMsg::HostHealth("a".into(), reached(None))).unwrap();
    app.tx.send(WorkerMsg::HostHealth("a".into(), HostProbe::Disconnected)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_probes["a"].kind, Some(HostKind::Vm("bhyve".into())));
}

#[test]
fn an_unknown_type_is_cached_so_it_is_not_asked_again() {
    use crate::zfs::host_health::{HostKind, HostProbe};
    let mut app = App::new(vec!["local".into(), "a".into()]);
    app.tx.send(WorkerMsg::HostHealth("a".into(), HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Unknown), os: None })).unwrap();
    app.drain_worker_messages();
    assert!(app.host_probes["a"].kind.is_some());
}

#[test]
fn host_probe_failure_clears_the_in_flight_marker_so_the_name_can_be_retried() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "newhost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "newhost.example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    app.handle_modal_key(KeyCode::Enter); // starts probing "newhost"
    app.tx
        .send(WorkerMsg::HostProbe(
            "newhost".to_string(),
            "newhost.example.com".to_string(),
            Err(crate::error::AppError::CommandFailed { cmd: "ssh newhost.example.com".to_string(), status: 255, stderr: "unreachable".to_string() }),
        ))
        .unwrap();
    app.drain_worker_messages();

    // Retrying the same name after the failed probe cleared it
    // must be accepted, not rejected as still-in-flight.
    app.handle_key(KeyCode::Char('n'));
    for c in "newhost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "newhost.example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none(), "a retry after the first probe failed must be accepted");
}

#[test]
fn submitting_add_host_form_while_same_name_probe_already_in_flight_is_rejected_with_a_message() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Char('n'));
    for c in "newhost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "newhost.example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    app.handle_modal_key(KeyCode::Enter); // first submission: starts probing "newhost", self.hosts doesn't have it yet
    assert!(app.modal.is_none());

    app.handle_key(KeyCode::Char('n'));
    for c in "NewHost".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in "different.example.com".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // past the optional Tag field
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "a name already being probed (case-insensitively) must be rejected, not started a second time");
    assert!(app.status_message.as_ref().unwrap().to_lowercase().contains("newhost"), "the rejection must say which name collided");
}

fn enter_datasets_acl_tab(app: &mut App) {
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.handle_key(KeyCode::Right); // -> Right focus, Properties tab
    app.handle_key(KeyCode::Char(']')); // -> Permissions tab
    app.handle_key(KeyCode::Char(']')); // -> Acl tab
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
    app.datasets = vec![Dataset { name: "tank/vol".to_string(), kind: crate::zfs::dataset::DatasetKind::Volume, used: 0, available: 0, referenced: 0, mountpoint: None }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none(), "a zvol (no mountpoint) must not open an ACL form");
}

#[test]
fn handle_key_n_on_acl_tab_opens_add_acl_entry_form_appending_at_the_current_count() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
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
fn submitting_any_form_with_invalid_input_sets_a_status_message_explaining_why() {
    // A live session hit this directly: typing a bare username
    // ("fb") into the ACL Recipient field instead of "user:fb"
    // made Enter silently do nothing, with no visible explanation
    // -- indistinguishable from the UI being frozen. This is the
    // shared fallback every form's failed submission goes through
    // (not ACL-specific), so the fix and its test live here.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries_for = Some("/tank/data".to_string());
    app.handle_key(KeyCode::Char('n'));
    for c in "fb".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // -> Permissions
    for c in "read_data".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab); // -> Flags
    app.handle_modal_key(KeyCode::Tab); // -> Type
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(result, None);
    assert!(app.modal.is_some(), "invalid input must keep the form open");
    assert!(app.status_message.is_some(), "a failed submission must say so, not silently do nothing");
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none());
}

#[test]
fn handle_key_d_on_selected_acl_row_opens_remove_confirm_with_its_position() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
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
        MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "-maproot=nobody -network 172.16.10.0/24".to_string() }
    );
}

#[test]
fn set_error_marks_the_current_status_message_as_an_error() {
    let mut app = App::new(vec!["local".to_string()]);
    assert!(!app.status_is_error());
    app.set_error("zpool destroy failed".to_string());
    assert_eq!(app.status_message.as_deref(), Some("zpool destroy failed"));
    assert!(app.status_is_error());
}

#[test]
fn a_later_ordinary_message_is_not_styled_as_an_error() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("boom".to_string());
    app.status_message = Some("select a disk with Space first".to_string());
    assert!(!app.status_is_error(), "stale error flag must not recolor an unrelated message");
}

#[test]
fn clearing_the_message_clears_the_error_state() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("boom".to_string());
    app.handle_key(crossterm::event::KeyCode::Esc);
    assert_eq!(app.status_message, None);
    assert!(!app.status_is_error());
}

fn accounts_fixture() -> crate::zfs::accounts::HostAccounts {
    use crate::zfs::accounts::{Account, HostAccounts};
    HostAccounts {
        users: vec![Account { name: "carol".into(), id: 1003 }, Account { name: "root".into(), id: 0 }],
        groups: vec![Account { name: "staff".into(), id: 1100 }],
        group_error: None,
    }
}

/// An App with an SMB share form open on its "Valid user" field (index 1) for `host`.
fn smb_form_app(host: usize) -> App {
    let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
    app.modal = Some(Modal::Form {
        title: "SMB share".to_string(),
        fields: vec![
            FormField { label: "Share name".into(), value: "media".into() },
            FormField { label: "Valid user".into(), value: "root".into() },
            FormField { label: "Password".into(), value: String::new() },
            FormField { label: "Read-only".into(), value: "n".into() },
            FormField { label: "Guest".into(), value: "n".into() },
        ],
        focused: 1,
        description: None,
        kind: FormKind::ShareSmb { host, dataset: "tank/a".into(), path: "/tank/a".into(), existing_user: None },
    });
    app
}

fn press_f2(app: &mut App) {
    app.handle_modal_key(KeyCode::F(2));
}

#[test]
fn f2_on_an_account_field_with_no_cache_opens_a_loading_picker_and_queues_one_fetch() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    let p = app.picker.as_ref().expect("picker opens");
    assert_eq!(p.load, crate::ui::picker::PickerLoad::Loading);
    assert_eq!(app.pending_account_fetch, vec![1]);
    assert!(app.account_fetch_in_flight.contains_key(&1));
}

#[test]
fn a_second_f2_while_the_fetch_is_in_flight_queues_nothing_more() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    app.picker = None;
    press_f2(&mut app);
    assert_eq!(app.pending_account_fetch, vec![1]);
    assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
}

#[test]
fn a_stuck_in_flight_marker_older_than_30s_lets_the_next_f2_queue_a_fresh_fetch() {
    let mut app = smb_form_app(1);
    app.account_fetch_in_flight.insert(1, Instant::now().checked_sub(Duration::from_secs(31)).unwrap());
    press_f2(&mut app);
    assert_eq!(app.pending_account_fetch, vec![1]);
}

#[test]
fn a_fresh_cache_opens_ready_immediately_and_queues_nothing() {
    let mut app = smb_form_app(1);
    app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
    press_f2(&mut app);
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
    assert_eq!(p.values, vec!["carol", "root"]);
    assert_eq!(p.selected, 1, "field text 'root' is preselected");
    assert!(app.pending_account_fetch.is_empty());
}

#[test]
fn a_cache_older_than_60s_is_treated_as_missing() {
    let mut app = smb_form_app(1);
    app.account_cache.insert(1, (Instant::now().checked_sub(Duration::from_secs(61)).unwrap(), accounts_fixture()));
    press_f2(&mut app);
    assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
    assert_eq!(app.pending_account_fetch, vec![1]);
}

#[test]
fn an_ok_result_fills_the_open_loading_picker_caches_and_clears_in_flight() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
    app.drain_worker_messages();
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
    assert_eq!(p.values, vec!["carol", "root"]);
    assert!(app.account_cache.contains_key(&1));
    assert!(!app.account_fetch_in_flight.contains_key(&1));
}

#[test]
fn an_err_result_marks_the_picker_failed_and_is_not_cached() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    let err = crate::error::AppError::Parse("ssh: connect timed out".to_string());
    app.tx.send(WorkerMsg::Accounts(1, Err(err))).unwrap();
    app.drain_worker_messages();
    assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("timed out")));
    assert!(app.account_cache.is_empty());
    assert!(app.account_fetch_in_flight.is_empty(), "a retry after Esc must be able to queue again");
}

#[test]
fn a_result_for_another_host_never_touches_the_open_picker_but_is_cached() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    app.tx.send(WorkerMsg::Accounts(0, Ok(accounts_fixture()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
    assert!(app.account_cache.contains_key(&0));
}

#[test]
fn a_late_result_with_no_picker_open_is_cached_and_does_not_open_one() {
    let mut app = smb_form_app(1);
    app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
    app.drain_worker_messages();
    assert!(app.picker.is_none());
    assert!(app.account_cache.contains_key(&1));
}

#[test]
fn loading_and_failed_pickers_accept_only_esc() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    for key in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Enter, KeyCode::Backspace] {
        app.handle_picker_key(key);
        assert!(app.picker.is_some(), "{key:?} must not close or alter a Loading picker");
        assert!(app.picker.as_ref().unwrap().filter.is_empty());
    }
    app.handle_picker_key(KeyCode::Esc);
    assert!(app.picker.is_none());
    match &app.modal {
        Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "root"),
        other => panic!("form must stay open: {other:?}"),
    }
}

#[test]
fn a_failed_picker_accepts_only_esc() {
    let mut app = smb_form_app(1);
    press_f2(&mut app);
    app.picker.as_mut().unwrap().load = crate::ui::picker::PickerLoad::Failed("x".into());
    for key in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Enter, KeyCode::Backspace] {
        app.handle_picker_key(key);
        assert!(app.picker.is_some(), "{key:?} must not close a Failed picker");
        assert!(app.picker.as_ref().unwrap().filter.is_empty());
    }
    match &app.modal {
        Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "root"),
        other => panic!("form must stay open: {other:?}"),
    }
    app.handle_picker_key(KeyCode::Esc);
    assert!(app.picker.is_none());
}

#[test]
fn a_cache_aged_59s_is_still_fresh() {
    let mut app = smb_form_app(1);
    let aged = Instant::now().checked_sub(Duration::from_secs(59)).expect("monotonic clock far enough along");
    app.account_cache.insert(1, (aged, accounts_fixture()));
    press_f2(&mut app);
    assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Ready);
    assert!(app.pending_account_fetch.is_empty());
}

#[test]
fn an_in_flight_marker_aged_29s_still_blocks_a_requeue() {
    let mut app = smb_form_app(1);
    let aged = Instant::now().checked_sub(Duration::from_secs(29)).expect("monotonic clock far enough along");
    app.account_fetch_in_flight.insert(1, aged);
    press_f2(&mut app);
    assert!(app.pending_account_fetch.is_empty());
    assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
}

#[test]
fn selecting_a_live_value_replaces_the_field_text() {
    let mut app = smb_form_app(1);
    app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
    press_f2(&mut app);
    app.handle_picker_key(KeyCode::Up); // carol
    app.handle_picker_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "carol"),
        other => panic!("{other:?}"),
    }
    assert!(app.picker.is_none());
}

#[test]
fn five_thousand_accounts_fill_and_filter_correctly() {
    use crate::zfs::accounts::{Account, HostAccounts};
    let users = (0..5000).map(|i| Account { name: format!("user{i:04}"), id: 2000 + i }).collect();
    let mut app = smb_form_app(1);
    app.account_cache.insert(1, (Instant::now(), HostAccounts { users, groups: vec![], group_error: None }));
    press_f2(&mut app);
    assert_eq!(app.picker.as_ref().unwrap().values.len(), 5000);
    for c in "4999".chars() {
        app.handle_picker_key(KeyCode::Char(c));
    }
    app.handle_picker_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "user4999"),
        other => panic!("{other:?}"),
    }
}

// ---- mutation-testing gap fillers ------------------------------------

fn wait_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if done(app) || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn is_loading_ignores_the_vdevs_auto_refresh_but_counts_everything_else() {
    let mut app = App::new(vec!["local".to_string()]);
    assert!(!app.is_loading());
    app.in_flight.insert((Section::Vdevs, 0));
    assert!(!app.is_loading(), "the background vdev poll must not show the refreshing indicator");
    app.in_flight.insert((Section::Pools, 0));
    assert!(app.is_loading());
}

#[test]
fn active_host_name_is_the_selected_host_or_a_question_mark() {
    let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
    assert_eq!(app.active_host_name(), "local");
    app.selected_host = 1;
    assert_eq!(app.active_host_name(), "other");
    app.selected_host = 7;
    assert_eq!(app.active_host_name(), "?");
}

#[test]
fn focused_form_value_reads_the_focused_field_and_is_empty_without_a_form() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.focused_form_value(), "");
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![
            FormField { label: "a".to_string(), value: "first".to_string() },
            FormField { label: "b".to_string(), value: "second".to_string() },
        ],
        focused: 1,
        description: None,
        kind: FormKind::AddHost,
    });
    assert_eq!(app.focused_form_value(), "second");
    if let Some(Modal::Form { focused, .. }) = app.modal.as_mut() {
        *focused = 9;
    }
    assert_eq!(app.focused_form_value(), "", "out-of-range focus reads as empty");
}

#[test]
fn a_cache_entry_exactly_ttl_old_is_stale() {
    let mut app = smb_form_app(1);
    let now = Instant::now();
    app.account_cache.insert(1, (now.checked_sub(ACCOUNT_CACHE_TTL).unwrap(), accounts_fixture()));
    let mut picker = crate::ui::picker::PickerState {
        values: vec![],
        filter: String::new(),
        selected: 0,
        checked: std::collections::HashSet::new(),
        multi_select: false,
        load: crate::ui::picker::PickerLoad::Loading,
        note: None,
        account: None,
        pool_host: None,
    };
    app.open_account_picker(&mut picker, 1, "", now);
    assert_eq!(picker.load, crate::ui::picker::PickerLoad::Loading, "age == TTL is not fresh");
    assert_eq!(app.pending_account_fetch, vec![1]);
}

#[test]
fn refresh_datasets_populates_datasets_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\t100\t200\t100\t/tank/data\tfilesystem\tyes\n"));
    app.refresh_datasets(runner);
    wait_until(&mut app, |a| !a.datasets.is_empty());
    assert_eq!(app.datasets.len(), 1);
    assert_eq!(app.datasets[0].name, "tank/data");
    assert!(!app.is_loading());
}

#[test]
fn refresh_snapshots_populates_snapshots_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data@s1\t10\n"));
    app.refresh_snapshots(runner);
    wait_until(&mut app, |a| !a.snapshots.is_empty());
    assert_eq!(app.snapshots.len(), 1);
    assert_eq!(app.snapshots[0].name, "s1");
    assert!(!app.is_loading());
}

#[test]
fn a_disk_refresh_drops_selected_disks_that_are_missing_or_no_longer_available() {
    let mut app = App::new(vec!["local".to_string()]);
    for n in ["da0", "da1", "da2"] {
        app.selected_disks.insert(n.to_string());
    }
    let disk = |name: &str, membership| DiskInfo { name: name.to_string(), size: 1, membership, ..Default::default() };
    let in_use = DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() };
    app.tx.send(WorkerMsg::Disks(0, Ok(vec![disk("da0", DiskMembership::Available), disk("da1", in_use)]))).unwrap();
    app.drain_worker_messages();
    let mut kept: Vec<_> = app.selected_disks.iter().cloned().collect();
    kept.sort();
    assert_eq!(kept, vec!["da0"], "da1 is in use, da2 vanished");
}

fn app_with_dataset_detail_for(selected: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")) };
    app.datasets = vec![ds("tank/a"), ds("tank/b")];
    app.datasets_table.select(Some(usize::from(selected != "tank/a")));
    app.dataset_properties = vec![Property { name: "atime".to_string(), value: "on".to_string(), source: PropertySource::Local }];
    app.dataset_properties_for = Some("tank/a".to_string());
    app.dataset_permissions = vec![PermissionRow::CreateTime { perms: vec!["mount".to_string()] }];
    app.dataset_permissions_for = Some("tank/a".to_string());
    app.acl_entries_for = Some("/tank/a".to_string());
    app.acl_entries = vec![crate::acl::AclEntry {
        tag: crate::acl::AclTag::Owner,
        permissions: vec!["read_data".to_string()],
        flags: vec![],
        entry_type: crate::acl::AclEntryType::Allow,
    }];
    app
}

#[test]
fn a_dataset_list_refresh_keeps_right_pane_data_that_still_belongs_to_the_selection() {
    let mut app = app_with_dataset_detail_for("tank/a");
    let same = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()) }];
    app.tx.send(WorkerMsg::Datasets(0, Ok(same))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.dataset_properties.len(), 1);
    assert_eq!(app.dataset_permissions.len(), 1);
    assert_eq!(app.acl_entries.len(), 1);
    assert!(app.dataset_properties_for.is_some() && app.dataset_permissions_for.is_some() && app.acl_entries_for.is_some());
}

#[test]
fn a_dataset_list_refresh_that_moves_the_selection_clears_each_right_pane_cache() {
    let mut app = app_with_dataset_detail_for("tank/a");
    let moved = vec![Dataset { name: "tank/z".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/z".to_string()) }];
    app.tx.send(WorkerMsg::Datasets(0, Ok(moved))).unwrap();
    app.drain_worker_messages();
    assert!(app.dataset_properties.is_empty() && app.dataset_properties_for.is_none());
    assert!(app.dataset_permissions.is_empty() && app.dataset_permissions_for.is_none());
    assert!(app.acl_entries.is_empty() && app.acl_entries_for.is_none());
}

#[test]
fn parse_recipient_field_needs_a_name_after_user_and_group_prefixes() {
    use crate::zfs::permission::Recipient;
    assert_eq!(parse_recipient_field("user:bob"), Some(Recipient::User("bob".to_string())));
    assert_eq!(parse_recipient_field("group:wheel"), Some(Recipient::Group("wheel".to_string())));
    assert_eq!(parse_recipient_field("user:"), None);
    assert_eq!(parse_recipient_field("group:"), None);
    assert_eq!(parse_recipient_field("everyone"), Some(Recipient::Everyone));
    assert_eq!(parse_recipient_field("bob"), None);
}

#[test]
fn move_table_selection_wraps_and_steps_by_one_in_both_directions() {
    let mut t = TableState::default();
    move_table_selection(&mut t, 0, true);
    assert_eq!(t.selected(), None);
    move_table_selection(&mut t, 5, true);
    assert_eq!(t.selected(), Some(0));
    move_table_selection(&mut t, 5, true);
    assert_eq!(t.selected(), Some(1));
    t.select(Some(4));
    move_table_selection(&mut t, 5, true);
    assert_eq!(t.selected(), Some(0), "forward wraps");
    move_table_selection(&mut t, 5, false);
    assert_eq!(t.selected(), Some(4), "backward from the first row wraps");
    move_table_selection(&mut t, 5, false);
    assert_eq!(t.selected(), Some(3));
    t.select(None);
    move_table_selection(&mut t, 5, false);
    assert_eq!(t.selected(), Some(4), "backward from no selection lands on the last row");
}

#[test]
fn bang_requests_a_shell_on_the_selected_host() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.handle_key(KeyCode::Char('!'));
    assert!(app.pending_shell);
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("fbsd16")), "{:?}", app.status_message);
}

#[test]
fn bang_works_inside_a_drilled_pool_too() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tank".to_string());
    app.handle_key(KeyCode::Char('!'));
    assert!(app.pending_shell);
}

#[test]
fn bang_is_inert_with_a_modal_open_or_no_hosts() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Info { title: "Help".to_string(), body: String::new() });
    app.handle_key(KeyCode::Char('!'));
    assert!(!app.pending_shell, "a modal owns the keyboard");
    let mut empty = App::new(Vec::new());
    empty.handle_key(KeyCode::Char('!'));
    assert!(!empty.pending_shell, "nothing to open a shell on");
}

#[test]
fn help_text_documents_the_shell_key() {
    assert!(help_text().contains("  !  "), "{}", help_text());
}

#[test]
fn refresh_visible_reloads_the_list_you_are_looking_at() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    app.refresh_visible(runner);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pools.len(), 1);
}

#[test]
fn refresh_visible_in_the_hosts_section_does_nothing() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.refresh_visible(runner);
    assert!(app.pools.is_empty() && !app.is_loading());
}

fn app_with_jobs(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
    (app, dir)
}

fn scrub_job(id: &str) -> crate::jobs::Job {
    crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
}

#[test]
fn tab_cycles_through_jobs_cloud_alerts_and_settings_and_wraps_to_the_dashboard() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.sections.last(), Some(&Section::Settings));
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Jobs);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Cloud);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Alerts);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Settings);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Dashboard, "wraps to the first section, the Dashboard");
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Settings);
}

#[test]
fn refresh_jobs_reads_the_state_dir_and_keeps_the_selection_on_the_same_job() {
    let (mut app, dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
    assert!(!app.jobs_view.loaded);
    app.refresh_jobs();
    assert_eq!(app.jobs_view.jobs.len(), 2);
    app.jobs_table.select(Some(1));
    // A job is added before "b": "b" moves to row 2 and must stay selected.
    crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("new"), scrub_job("b")]).unwrap();
    app.refresh_jobs();
    assert_eq!(app.jobs_table.selected(), Some(2));
    // The selected job disappears: the selection clamps instead of dangling.
    crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
    app.refresh_jobs();
    assert_eq!(app.jobs_table.selected(), Some(0));
    crate::jobs::save(&dir.path().join("jobs.toml"), &[]).unwrap();
    app.refresh_jobs();
    assert_eq!(app.jobs_table.selected(), None);
}

#[test]
fn up_and_down_move_the_jobs_selection_and_wrap() {
    let (mut app, _dir) = app_with_jobs(&[scrub_job("a"), scrub_job("b")]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    assert_eq!(app.jobs_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.jobs_table.selected(), Some(1));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.jobs_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.jobs_table.selected(), Some(1));
}

#[test]
fn jobs_refresh_happens_at_once_then_every_two_seconds_in_the_section_and_ten_elsewhere() {
    let (mut app, dir) = app_with_jobs(&[scrub_job("a")]);
    let t0 = Instant::now();
    app.refresh_jobs_if_stale(t0);
    assert!(app.jobs_view.loaded, "the first call loads immediately");
    crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a"), scrub_job("b")]).unwrap();
    app.refresh_jobs_if_stale(t0 + Duration::from_secs(1));
    assert_eq!(app.jobs_view.jobs.len(), 1, "not yet: two seconds apply while the section is on screen");
    app.refresh_jobs_if_stale(t0 + Duration::from_secs(2));
    assert_eq!(app.jobs_view.jobs.len(), 2);
    crate::jobs::save(&dir.path().join("jobs.toml"), &[scrub_job("a")]).unwrap();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    app.refresh_jobs_if_stale(t0 + Duration::from_secs(5));
    assert_eq!(app.jobs_view.jobs.len(), 2, "ten seconds apply from another section");
    app.refresh_jobs_if_stale(t0 + Duration::from_secs(12));
    assert_eq!(app.jobs_view.jobs.len(), 1);
}

#[test]
fn refresh_visible_in_the_jobs_section_reloads_without_a_runner() {
    let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
    app.refresh_visible(Arc::new(FakeRunner::new("")));
    assert_eq!(app.jobs_view.jobs.len(), 1);
}

fn pools_app_with_one_pool() -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app
}

fn form_values(app: &App) -> Vec<String> {
    match &app.modal {
        Some(Modal::Form { fields, .. }) => fields.iter().map(|f| f.value.clone()).collect(),
        other => panic!("expected a form, got {other:?}"),
    }
}

fn set_form_values(app: &mut App, values: [&str; 4]) {
    let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
    for (field, value) in fields.iter_mut().zip(values) {
        field.value = value.to_string();
    }
}

fn at_utc(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
}

#[test]
fn j_on_a_pool_opens_the_job_form_with_the_host_and_pool_filled_in() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    assert_eq!(form_values(&app), vec!["fbsd16", "tank", "0 3 * * 0", "y"]);
    let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
    assert!(title.contains("New scrub job"), "{title}");
    assert!(matches!(kind, FormKind::Job { existing: None, .. }));
}

#[test]
fn j_with_no_pool_selected_opens_nothing_and_says_why() {
    let mut app = pools_app_with_one_pool();
    app.pools_table.select(None);
    app.handle_key(KeyCode::Char('J'));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Select a pool")), "{:?}", app.status_message);
}

#[test]
fn n_in_the_jobs_section_opens_a_blank_pool_form_for_the_selected_host() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(form_values(&app), vec!["local", "", "0 3 * * 0", "y"]);
}

#[test]
fn enter_on_a_job_opens_the_form_with_its_values_and_nothing_without_a_selection() {
    let mut job = scrub_job("scrub-local-tank");
    job.schedule = "*/10 * * * *".into();
    job.catch_up = false;
    let (mut app, _dir) = app_with_jobs(&[job]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none(), "no row is selected yet");
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), vec!["local", "tank", "*/10 * * * *", "n"]);
    let Some(Modal::Form { title, kind, .. }) = &app.modal else { panic!() };
    assert!(title.contains("scrub-local-tank"), "{title}");
    assert!(matches!(kind, FormKind::Job { existing: Some(_), .. }));
}

fn job_kind(existing: Option<crate::jobs::Job>, taken: &[&str]) -> FormKind {
    FormKind::Job { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), taken_ids: taken.iter().map(|s| (*s).to_string()).collect() }
}

fn fields(values: [&str; 4]) -> Vec<FormField> {
    ["Host", "Pool", "Schedule", "Catch up (y/n)"].iter().zip(values).map(|(l, v)| FormField { label: (*l).to_string(), value: v.to_string() }).collect()
}

#[test]
fn a_new_job_gets_an_id_a_creation_time_and_is_enabled() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let action = build_job_action(&job_kind(None, &[]), &fields(["fbsd16", " tank ", " 0 2 * * * ", "y"]), std::path::Path::new("/state"), now).unwrap();
    assert_eq!(action.section, Section::Jobs);
    let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
    assert_eq!(state_dir, std::path::PathBuf::from("/state"));
    assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str()), ("scrub-fbsd16-tank", "fbsd16", Some("tank"), "0 2 * * *"));
    assert!(job.catch_up && job.enabled && job.kind == crate::jobs::KIND_SCRUB);
    assert_eq!(job.created_at, Some(now));
}

#[test]
fn a_colliding_id_gets_a_numeric_suffix() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let action = build_job_action(&job_kind(None, &["scrub-local-tank"]), &fields(["local", "tank", "0 3 * * 0", "y"]), std::path::Path::new("/s"), now).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!(job.id, "scrub-local-tank-2");
}

#[test]
fn an_edit_changes_the_fields_but_keeps_id_creation_time_and_enabled() {
    let mut existing = scrub_job("my-job");
    existing.enabled = false;
    existing.created_at = Some(at_utc("2026-09-01T00:00:00Z"));
    let action = build_job_action(&job_kind(Some(existing), &["my-job"]), &fields(["fbsd16", "other", "0 4 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str(), job.catch_up), ("my-job", "fbsd16", Some("other"), "0 4 * * *", false));
    assert!(!job.enabled, "an edit does not re-enable a disabled job");
    assert_eq!(job.created_at, Some(at_utc("2026-09-01T00:00:00Z")));
}

#[test]
fn a_bad_form_is_refused_with_the_reason() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let try_it = |values: [&str; 4]| build_job_action(&job_kind(None, &[]), &fields(values), std::path::Path::new("/s"), now).unwrap_err();
    assert!(try_it(["nowhere", "tank", "0 3 * * 0", "y"]).contains("Unknown host 'nowhere'"));
    assert!(try_it(["local", "  ", "0 3 * * 0", "y"]).contains("Pool name is required"));
    assert!(try_it(["local", "tank", "61 * * * *", "y"]).contains("outside 0-59"));
    assert!(try_it(["local", "tank", "0 3 * * 0", "maybe"]).contains("y or n"));
}

#[test]
fn submitting_a_good_form_returns_the_save_action_and_a_bad_one_stays_open_on_the_first_field() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    set_form_values(&mut app, ["fbsd16", "tank", "61 * * * *", "y"]);
    for _ in 0..3 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "the schedule is bad");
    assert!(app.modal.is_some(), "the form stays open");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("outside 0-59")), "{:?}", app.status_message);
    assert!(app.status_is_error());
    let Some(Modal::Form { focused, .. }) = &app.modal else { panic!() };
    assert_eq!(*focused, 0, "focus returns to the first field, as the SMB form does");
    set_form_values(&mut app, ["fbsd16", "tank", "0 3 * * 0", "y"]);
    for _ in 0..3 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    let action = app.handle_modal_key(KeyCode::Enter).expect("the form submits");
    assert!(matches!(action.call, MutationCall::SaveJob { .. }));
    assert!(app.modal.is_none());
}

#[test]
fn f2_on_host_lists_the_configured_hosts_and_on_schedule_the_presets_inserting_only_the_cron_part() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    app.handle_modal_key(KeyCode::F(2));
    let picker = app.picker.clone().expect("a host picker");
    assert_eq!(picker.values, vec!["local", "fbsd16"]);
    assert!(!picker.multi_select);
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[0], "fbsd16".to_string());
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    let presets = app.picker.clone().expect("a schedule picker");
    assert_eq!(presets.values, crate::ui::jobs::preset_values());
    assert_eq!(presets.selected, 2, "the picker opens on the field's current preset");
    app.handle_picker_key(KeyCode::Up);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[2], "0 2 * * *", "no label text lands in the field");
}

#[test]
fn f2_on_catch_up_does_nothing() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.is_none());
}

#[test]
fn f2_on_the_pool_field_lists_the_pool_names_of_the_host_in_the_host_field() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    let picker = app.picker.as_ref().expect("a pool picker");
    assert_eq!(picker.load, crate::ui::picker::PickerLoad::Loading);
    assert_eq!(app.pending_pool_fetch, vec![(1, crate::ui::picker::LiveNames::Pools)]);
    app.tx.send(WorkerMsg::PoolNames(1, Ok(vec!["scratch".to_string(), "zroot".to_string()]))).unwrap();
    app.drain_worker_messages();
    let picker = app.picker.as_ref().unwrap();
    assert_eq!(picker.load, crate::ui::picker::PickerLoad::Ready);
    assert_eq!(picker.values, vec!["scratch", "zroot"]);
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[1], "zroot");
}

#[test]
fn a_failed_pool_lookup_says_so_and_an_unknown_host_is_refused_without_a_fetch() {
    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    app.tx.send(WorkerMsg::PoolNames(1, Err(crate::error::AppError::Parse("ssh down".to_string())))).unwrap();
    app.drain_worker_messages();
    assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("ssh down")));

    let mut app = pools_app_with_one_pool();
    app.handle_key(KeyCode::Char('J'));
    for c in "x".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("unknown host")));
    assert!(app.pending_pool_fetch.is_empty());
}

fn selected_jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let (mut app, dir) = app_with_jobs(jobs_list);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    (app, dir)
}

#[test]
fn space_on_a_job_queues_a_disable_then_an_enable_and_says_what_it_is_doing() {
    let (mut app, _dir) = selected_jobs_app(&[scrub_job("a")]);
    app.handle_key(KeyCode::Char(' '));
    let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
    assert_eq!(action.section, Section::Jobs);
    assert!(matches!(&action.call, MutationCall::SetJobEnabled { id, enabled: false, .. } if id == "a"), "{:?}", action.call);
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Disabling job 'a'")), "{:?}", app.status_message);
    app.pending_mutations.clear();
    app.jobs_view.jobs[0].enabled = false;
    app.handle_key(KeyCode::Char(' '));
    assert!(matches!(&app.pending_mutations[0].call, MutationCall::SetJobEnabled { enabled: true, .. }));
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Enabling job 'a'")), "{:?}", app.status_message);
}

#[test]
fn space_with_no_job_selected_does_nothing() {
    let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Char(' '));
    assert!(app.pending_mutations.is_empty());
}

#[test]
fn d_on_a_job_asks_for_a_warning_confirmation_and_deletes_only_that_job() {
    let (mut app, _dir) = selected_jobs_app(&[scrub_job("a"), scrub_job("b")]);
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("Delete job 'a'") && prompt.contains("history"), "{prompt}");
    assert_eq!(action.call.tone(), crate::ui::modal::Tone::Warn);
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("confirmed with y then Enter");
    assert_eq!(action.section, Section::Jobs);
    assert!(matches!(&action.call, MutationCall::DeleteJob { id, .. } if id == "a"), "{:?}", action.call);
}

#[test]
fn d_with_no_job_selected_opens_nothing_and_n_cancels_a_delete() {
    let (mut app, _dir) = app_with_jobs(&[scrub_job("a")]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Char('d'));
    assert!(app.modal.is_none());
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char('d'));
    assert!(app.handle_modal_key(KeyCode::Char('n')).is_none());
    assert!(app.modal.is_some(), "n only selects no");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(app.modal.is_none());
}

#[test]
fn help_text_documents_the_jobs_keys() {
    let text = help_text();
    let jobs = &text[text.find("\nJobs\n").expect("a Jobs block")..];
    for want in ["Enter", "Space", "  d ", "  n ", "history"] {
        assert!(jobs.contains(want), "missing {want:?} in the Jobs block:\n{jobs}");
    }
}

#[test]
fn help_text_documents_the_global_keys() {
    let text = help_text();
    assert!(text.starts_with("Global"));
    assert!(text.contains("Tab / Shift+Tab"));
}

#[test]
fn back_tab_steps_one_section_back_and_wraps_to_the_last() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = 2;
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.selected_section, 1);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.selected_section, 0);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.selected_section, app.sections.len() - 1);
}

#[test]
fn tab_and_back_tab_do_nothing_while_drilled_into_a_pool() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.drilled_pool = Some("tank".to_string());
    let before = app.selected_section;
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.selected_section, before);
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.selected_section, before);
}

#[test]
fn esc_only_clears_selected_disks_in_the_disks_section_and_only_when_some_are_selected() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_disks.insert("da0".to_string());
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.status_message = Some("hello".to_string());
    app.handle_key(KeyCode::Esc);
    assert!(app.status_message.is_none(), "Esc outside Disks dismisses the status message");
    assert_eq!(app.selected_disks.len(), 1, "and leaves the disk selection alone");
    app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
    app.selected_disks.clear();
    app.status_message = Some("hello".to_string());
    app.handle_key(KeyCode::Esc);
    assert!(app.status_message.is_none(), "with no disks selected Esc still dismisses the message");
}

fn in_section(section: Section) -> App {
    let mut app = App::new(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
    app.pane_focus = PaneFocus::Middle;
    app
}

#[test]
fn add_host_has_an_optional_third_tag_field_and_submits_on_it() {
    let mut app = in_section(Section::Hosts);
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(form_values(&app).len(), 3);
    for c in "box".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    for c in "u@box".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_some(), "Enter on the second field moves on, it does not submit");
    for c in " nas ".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.new_host_tags.get("box").map(String::as_str), Some("nas"));
}

#[test]
fn a_blank_tag_is_not_remembered() {
    let mut app = in_section(Section::Hosts);
    app.handle_key(KeyCode::Char('n'));
    for c in "box".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    for c in "u@box".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none() && app.new_host_tags.is_empty());
}

#[test]
fn host_tags_default_to_empty_and_follow_set_host_tags() {
    let mut app = App::new(vec!["local".into(), "a".into()]);
    assert_eq!((app.host_tag(0), app.host_tag(1), app.host_tag(9)), ("", "", ""));
    app.set_host_tags(vec![String::new(), "backup".into()]);
    assert_eq!(app.host_tag(1), "backup");
}

fn two_props() -> Vec<Property> {
    ["x", "y"].iter().map(|n| Property { name: (*n).to_string(), value: "v".to_string(), source: PropertySource::Local }).collect()
}

fn pool_fixture(name: &str) -> crate::zfs::pool::Pool {
    crate::zfs::pool::Pool { name: name.to_string(), size: 1, allocated: 0, free: 1, health: crate::zfs::pool::PoolHealth::Online }
}

/// Presses Up with the table at row 1 and checks it moved to row 0, while
/// every other table in `others` was left alone.
fn up_moves(app: &mut App, table: fn(&mut App) -> &mut TableState, others: &[fn(&mut App) -> &mut TableState]) {
    table(app).select(Some(1));
    for o in others {
        o(app).select(Some(1));
    }
    app.handle_key(KeyCode::Up);
    assert_eq!(table(app).selected(), Some(0));
    for o in others {
        assert_eq!(o(app).selected(), Some(1), "Up must only move the table the user is looking at");
    }
}

fn down_moves(app: &mut App, table: fn(&mut App) -> &mut TableState) {
    table(app).select(Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(table(app).selected(), Some(1));
}

#[test]
fn up_steps_back_through_hosts_and_wraps() {
    let mut app = in_section(Section::Hosts);
    app.selected_host = 2;
    app.handle_key(KeyCode::Up);
    assert_eq!(app.selected_host, 1);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.selected_host, 0);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.selected_host, 2);
}

#[test]
fn up_and_down_move_the_vdev_rows_when_drilled_and_the_pool_rows_when_not() {
    let vdev = |n: &str| crate::zfs::pool::VdevLine { depth: 0, name: n.to_string(), health: crate::zfs::pool::PoolHealth::Online };
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("tank"), pool_fixture("scratch")];
    app.vdevs = vec![vdev("tank"), vdev("da0")];
    up_moves(&mut app, |a| &mut a.pools_table, &[|a| &mut a.vdevs_table]);
    down_moves(&mut app, |a| &mut a.pools_table);
    app.drilled_pool = Some("tank".to_string());
    up_moves(&mut app, |a| &mut a.vdevs_table, &[|a| &mut a.pools_table]);
    down_moves(&mut app, |a| &mut a.vdevs_table);
}

#[test]
fn up_and_down_move_the_pool_property_rows_in_the_right_pane() {
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("tank"), pool_fixture("scratch")];
    app.pool_properties = two_props();
    app.pane_focus = PaneFocus::Right;
    up_moves(&mut app, |a| &mut a.pool_properties_table, &[|a| &mut a.pools_table]);
    down_moves(&mut app, |a| &mut a.pool_properties_table);
}

#[test]
fn up_and_down_move_the_row_list_of_the_active_dataset_detail_tab() {
    let mut app = in_section(Section::Datasets);
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")) };
    app.datasets = vec![ds("tank/a"), ds("tank/b")];
    app.dataset_properties = two_props();
    app.dataset_permissions = vec![
        PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
        PermissionRow::CreateTime { perms: vec!["create".to_string()] },
    ];
    let entry = |tag| crate::acl::AclEntry { tag, permissions: vec![], flags: vec![], entry_type: crate::acl::AclEntryType::Allow };
    app.acl_entries = vec![entry(crate::acl::AclTag::Owner), entry(crate::acl::AclTag::Everyone)];
    up_moves(&mut app, |a| &mut a.datasets_table, &[]); // a row change legitimately resets the detail pane
    down_moves(&mut app, |a| &mut a.datasets_table);
    // Moving the dataset row cleared the detail lists; refill them for the right-pane checks.
    app.dataset_properties = two_props();
    app.dataset_permissions = vec![
        PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
        PermissionRow::CreateTime { perms: vec!["create".to_string()] },
    ];
    app.acl_entries = vec![entry(crate::acl::AclTag::Owner), entry(crate::acl::AclTag::Everyone)];
    app.pane_focus = PaneFocus::Right;
    app.dataset_detail_tab = DatasetDetailTab::Properties;
    up_moves(&mut app, |a| &mut a.dataset_properties_table, &[|a| &mut a.datasets_table, |a| &mut a.dataset_permissions_table, |a| &mut a.acl_entries_table]);
    down_moves(&mut app, |a| &mut a.dataset_properties_table);
    app.dataset_detail_tab = DatasetDetailTab::Permissions;
    up_moves(&mut app, |a| &mut a.dataset_permissions_table, &[|a| &mut a.dataset_properties_table, |a| &mut a.acl_entries_table]);
    down_moves(&mut app, |a| &mut a.dataset_permissions_table);
    app.dataset_detail_tab = DatasetDetailTab::Acl;
    up_moves(&mut app, |a| &mut a.acl_entries_table, &[|a| &mut a.dataset_properties_table, |a| &mut a.dataset_permissions_table]);
    down_moves(&mut app, |a| &mut a.acl_entries_table);
}

#[test]
fn up_and_down_move_the_snapshot_disk_and_share_rows() {
    let mut app = in_section(Section::Snapshots);
    let snap = |n: &str| crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: n.to_string(), used: 1 };
    app.snapshots = vec![snap("s1"), snap("s2")];
    up_moves(&mut app, |a| &mut a.snapshots_table, &[|a| &mut a.disks_table, |a| &mut a.shares_table]);
    down_moves(&mut app, |a| &mut a.snapshots_table);

    let mut app = in_section(Section::Disks);
    app.disks = vec![
        DiskInfo { name: "da0".to_string(), size: 1, ..Default::default() },
        DiskInfo { name: "da1".to_string(), size: 1, ..Default::default() },
    ];
    up_moves(&mut app, |a| &mut a.disks_table, &[|a| &mut a.snapshots_table, |a| &mut a.shares_table]);
    down_moves(&mut app, |a| &mut a.disks_table);

    let mut app = in_section(Section::Shares);
    app.shares = vec![smb("tank/a"), smb("tank/b")];
    up_moves(&mut app, |a| &mut a.shares_table, &[|a| &mut a.snapshots_table, |a| &mut a.disks_table]);
    down_moves(&mut app, |a| &mut a.shares_table);
}

#[test]
fn n_and_a_only_ask_for_a_disk_selection_in_the_disks_section() {
    for section in [Section::Snapshots, Section::Shares] {
        let mut app = in_section(section);
        app.handle_key(KeyCode::Char('n'));
        app.handle_key(KeyCode::Char('a'));
        assert!(app.status_message.is_none(), "{section:?}: {:?}", app.status_message);
        assert!(app.modal.is_none());
    }
    let mut app = in_section(Section::Disks);
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(app.status_message.as_deref(), Some("select at least one disk with Space first"));
    app.status_message = None;
    app.handle_key(KeyCode::Char('a'));
    assert_eq!(app.status_message.as_deref(), Some("select at least one disk with Space first"));
}

#[test]
fn removing_an_acl_entry_needs_the_selection_to_be_inside_the_list() {
    let mut app = in_section(Section::Datasets);
    app.datasets = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()) }];
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
fn up_and_down_on_an_empty_hosts_list_do_not_fall_through_to_another_sections_rows() {
    let mut app = App::new(vec![]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    app.shares = vec![smb("tank/a"), smb("tank/b")];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.shares_table.selected(), Some(0));
    app.shares_table.select(Some(1));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.shares_table.selected(), Some(1));
}

// ---- auto refresh -----------------------------------------------------

const TEN: Duration = Duration::from_secs(10);

fn runner(out: &str) -> Arc<dyn CommandRunner> {
    Arc::new(FakeRunner::new(out))
}

#[test]
fn auto_refresh_targets_only_the_visible_list_sections() {
    for (section, expected) in [
        (Section::Hosts, None),
        (Section::Pools, Some(Section::Pools)),
        (Section::Datasets, Some(Section::Datasets)),
        (Section::Snapshots, Some(Section::Snapshots)),
        (Section::Disks, Some(Section::Disks)),
        (Section::Shares, Some(Section::Shares)),
        (Section::BootEnvs, Some(Section::BootEnvs)),
    ] {
        assert_eq!(in_section(section).auto_refresh_section(), expected, "{section:?}");
    }
}

#[test]
fn auto_refresh_never_runs_under_a_modal_a_picker_or_a_pool_drill_down() {
    let mut app = in_section(Section::Pools);
    app.drilled_pool = Some("tank".to_string());
    assert_eq!(app.auto_refresh_section(), None, "the vdev view has its own faster poll");
    app.drilled_pool = None;
    app.modal = Some(Modal::Info { title: "t".to_string(), body: "b".to_string() });
    assert_eq!(app.auto_refresh_section(), None);
    app.modal = None;
    app.picker = Some(crate::ui::picker::PickerState {
        values: vec![],
        filter: String::new(),
        selected: 0,
        checked: HashSet::new(),
        multi_select: false,
        load: crate::ui::picker::PickerLoad::Ready,
        note: None,
        account: None,
        pool_host: None,
    });
    assert_eq!(app.auto_refresh_section(), None);
}

#[test]
fn auto_refresh_fires_immediately_then_waits_for_the_interval() {
    let mut app = in_section(Section::Pools);
    let t0 = Instant::now();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.pools.len(), 1, "first call refreshes straight away");

    app.pools.clear();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + Duration::from_secs(9), TEN);
    std::thread::sleep(Duration::from_millis(100));
    app.drain_worker_messages();
    assert!(app.pools.is_empty(), "9s < 10s: not due yet");

    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + TEN, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.pools.len(), 1, "due at exactly the interval");
}

#[test]
fn auto_refresh_tracks_each_section_and_host_separately() {
    let mut app = in_section(Section::Pools);
    let t0 = Instant::now();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    // Switching to Snapshots right after must not be held back by Pools' timer.
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.auto_refresh(runner("tank/a@s1\t10\n"), t0 + Duration::from_secs(1), TEN);
    wait_until(&mut app, |a| !a.snapshots.is_empty());
    assert_eq!(app.snapshots.len(), 1);
}

#[test]
fn an_auto_refresh_is_quiet_but_a_manual_one_shows_the_loading_indicator() {
    let mut app = in_section(Section::Pools);
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    assert!(!app.is_loading(), "background polling must not flash the refreshing indicator");
    wait_until(&mut app, |a| !a.pools.is_empty());
    app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
    assert!(app.is_loading());
    wait_until(&mut app, |a| !a.is_loading());
    assert!(!app.is_loading());
}

#[test]
fn a_quiet_refresh_keeps_an_informational_status_but_a_manual_one_clears_it() {
    let mut app = in_section(Section::Pools);
    app.status_message = Some("scrub started on 'tank'".to_string());
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.status_message.as_deref(), Some("scrub started on 'tank'"));

    app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
    wait_until(&mut app, |a| a.status_message.is_none());
    assert!(app.status_message.is_none());
}

#[test]
fn a_quiet_refresh_clears_a_stale_error_once_the_host_answers() {
    let mut app = in_section(Section::Pools);
    app.set_error("connection refused".to_string());
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert!(app.status_message.is_none());
}

#[test]
fn a_refresh_keeps_the_selection_on_the_same_item_when_the_list_reorders() {
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
    app.pools_table.select(Some(1)); // bravo
    let reordered = vec![pool_fixture("new"), pool_fixture("alpha"), pool_fixture("charlie"), pool_fixture("bravo")];
    app.tx.send(WorkerMsg::Pools(0, Ok(reordered))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(3));
    assert_eq!(app.selected_pool_name().as_deref(), Some("bravo"), "a keypress must still act on bravo");
}

#[test]
fn a_refresh_falls_back_to_clamping_when_the_selected_item_is_gone() {
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
    app.pools_table.select(Some(2));
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool_fixture("alpha")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(0));
}

#[test]
fn selection_is_pinned_by_identity_in_datasets_snapshots_and_disks_too() {
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: None };
    let mut app = in_section(Section::Datasets);
    app.datasets = vec![ds("tank/a"), ds("tank/b")];
    app.datasets_table.select(Some(1));
    app.tx.send(WorkerMsg::Datasets(0, Ok(vec![ds("tank/0"), ds("tank/a"), ds("tank/b")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.selected_dataset_name().as_deref(), Some("tank/b"));

    let snap = |d: &str, n: &str| crate::zfs::snapshot::Snapshot { dataset: d.to_string(), name: n.to_string(), used: 1 };
    app.snapshots = vec![snap("tank/a", "s1"), snap("tank/b", "s1")];
    app.snapshots_table.select(Some(1)); // tank/b@s1: same snapshot name as another dataset's
    app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![snap("tank/b", "s1"), snap("tank/a", "s1")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.snapshots_table.selected(), Some(0), "identity is dataset@name, not just the name");

    let disk = |n: &str| DiskInfo { name: n.to_string(), size: 1, ..Default::default() };
    app.disks = vec![disk("da0"), disk("da1")];
    app.disks_table.select(Some(1));
    app.tx.send(WorkerMsg::Disks(0, Ok(vec![disk("da9"), disk("da0"), disk("da1")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.disks_table.selected(), Some(2));
}

#[test]
fn auto_refresh_starts_the_refresh_that_belongs_to_the_visible_section() {
    for section in [Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs] {
        let mut app = in_section(section);
        app.auto_refresh(runner(""), Instant::now(), TEN);
        assert!(app.in_flight.contains(&(section, 0)), "{section:?} refresh not started");
        // Datasets also loads the snapshot list the first time (the destroy prompts count them).
        let expected = if section == Section::Datasets { 2 } else { 1 };
        assert_eq!(app.in_flight.len(), expected, "only the visible section is refreshed ({section:?})");
        assert!(app.quiet_refresh.contains(&(section, 0)), "{section:?} should be quiet");
    }
}

#[test]
fn a_share_refresh_keeps_the_selection_on_the_same_share_and_protocol() {
    let nfs = |d: &str| crate::zfs::share::Share::Nfs { dataset: d.to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None };
    let snapshot = |shares| crate::zfs::share::SharesSnapshot { shares, nfs_available: true, samba_available: true, smb_error: None };
    let mut app = in_section(Section::Shares);
    app.shares = vec![nfs("tank/a"), smb("tank/b")];
    app.shares_table.select(Some(1)); // the SMB share on tank/b
    app.tx.send(WorkerMsg::Shares(0, Ok(snapshot(vec![nfs("tank/0"), nfs("tank/b"), nfs("tank/a"), smb("tank/b")])))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.shares_table.selected(), Some(3), "tank/b's SMB share, not its NFS share");
}

#[test]
fn a_quiet_shares_refresh_keeps_an_informational_status() {
    let mut app = in_section(Section::Shares);
    app.status_message = Some("share created".to_string());
    app.auto_refresh(runner(""), Instant::now(), TEN);
    wait_until(&mut app, |a| a.shares_loaded);
    assert!(app.shares_loaded);
    assert_eq!(app.status_message.as_deref(), Some("share created"));
}

#[test]
fn n_in_jobs_opens_a_blank_snapshot_form_with_sensible_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('N'));
    assert_eq!(form_values(&app), ["local", "", "n", "auto", "24", "0 * * * *", "y"]);
}

#[test]
fn j_on_a_dataset_prefills_the_snapshot_form() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('J'));
    assert_eq!(form_values(&app)[1], "tank/media");
}

#[test]
fn enter_on_a_snapshot_job_edits_it_in_the_snapshot_form() {
    let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 7)]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["local", "tank/data", "n", "hourly", "7", "0 * * * *", "y"]);
}

fn snapshot_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::SnapshotJob { hosts: vec!["local".to_string()], existing: existing.map(Box::new), jobs }
}

fn snap_fields(values: [&str; 7]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
}

fn snap_err(values: [&str; 7], jobs: Vec<crate::jobs::Job>) -> String {
    build_snapshot_job_action(&snapshot_kind(None, jobs), &snap_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
}

#[test]
fn a_valid_snapshot_form_builds_a_save_of_a_new_job_with_a_fresh_id_and_creation_time() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", " tank/data ", "y", "hourly", "24", "0 * * * *", "y"]), std::path::Path::new("/state"), now).unwrap();
    assert_eq!(action.section, Section::Jobs);
    let MutationCall::SaveJob { state_dir, job } = action.call else { panic!("{:?}", action.call) };
    assert_eq!(state_dir, std::path::PathBuf::from("/state"));
    assert_eq!((job.id.as_str(), job.kind.as_str(), job.dataset.as_deref(), job.recursive, job.prefix.as_deref(), job.keep), ("snap-local-tank-data-hourly", crate::jobs::KIND_SNAPSHOT, Some("tank/data"), true, Some("hourly"), Some(24)));
    assert!(job.pool.is_none() && job.catch_up && job.enabled);
    assert_eq!(job.created_at, Some(now));
}

#[test]
fn blank_prefix_and_blank_yes_no_fields_take_their_defaults() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let action = build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(["local", "tank", "", "", "3", "0 * * * *", ""]), std::path::Path::new("/s"), now).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.recursive, job.prefix.as_deref(), job.catch_up), (false, Some("auto"), true));
}

#[test]
fn an_edited_snapshot_job_keeps_its_id_creation_time_and_enabled_flag() {
    let mut existing = crate::jobs::Job::test_snapshot("keep-me", "tank/data", "hourly", 3);
    existing.enabled = false;
    existing.created_at = Some(at_utc("2026-01-01T00:00:00Z"));
    let kind = snapshot_kind(Some(existing.clone()), vec![existing]);
    let action = build_snapshot_job_action(&kind, &snap_fields(["local", "tank/other", "n", "daily", "9", "0 2 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.id.as_str(), job.enabled, job.created_at), ("keep-me", false, Some(at_utc("2026-01-01T00:00:00Z"))));
    assert_eq!((job.dataset.as_deref(), job.prefix.as_deref(), job.keep, job.catch_up), (Some("tank/other"), Some("daily"), Some(9), false));
}

#[test]
fn the_snapshot_form_refuses_what_the_daemon_would_refuse() {
    let good = ["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"];
    assert!(build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(good), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
    assert!(snap_err(["nope", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Unknown host"));
    assert!(snap_err(["local", "", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
    assert!(snap_err(["local", "-rf", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
    assert!(snap_err(["local", "tank/data", "maybe", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Recursive"));
    assert!(snap_err(["local", "tank/data", "n", "a b", "24", "0 * * * *", "y"], vec![]).contains("Prefix"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "0", "0 * * * *", "y"], vec![]).contains("Keep"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "x", "0 * * * *", "y"], vec![]).contains("Keep"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "bogus", "y"], vec![]).contains("Schedule"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "q"], vec![]).contains("Catch up"));
}

#[test]
fn the_snapshot_form_refuses_a_prefix_another_job_already_uses_on_that_dataset() {
    let other = crate::jobs::Job::test_snapshot("other", "tank/data", "hourly", 5);
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![other.clone()]).contains("other"));
    // Editing that same job is fine, as is a different prefix.
    let kind = snapshot_kind(Some(other.clone()), vec![other]);
    assert!(build_snapshot_job_action(&kind, &snap_fields(["local", "tank/data", "n", "hourly", "9", "0 * * * *", "y"]), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
}

#[test]
fn f2_on_the_snapshot_forms_dataset_field_queues_a_dataset_fetch_for_the_chosen_host() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('N'));
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)]);
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["tank".to_string(), "tank/data".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[1], "tank/data");
}

#[test]
fn f2_on_the_snapshot_forms_host_and_schedule_fields_opens_their_pickers_and_the_rest_do_nothing() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('N'));
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.picker.as_ref().unwrap().values, vec!["local"]);
    app.picker = None;
    for _ in 0..5 {
        app.handle_modal_key(KeyCode::Tab);
    }
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.picker.as_ref().unwrap().values, crate::ui::jobs::preset_values());
    app.picker = None;
    app.handle_modal_key(KeyCode::Tab); // Catch up
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.is_none());
}

#[test]
fn space_refuses_to_enable_a_snapshot_job_that_clashes_with_an_enabled_one() {
    let mut a = crate::jobs::Job::test_snapshot("a", "tank/data", "hourly", 3);
    a.enabled = false;
    let b = crate::jobs::Job::test_snapshot("b", "tank/data", "hourly", 48);
    let (mut app, _dir) = app_with_jobs(&[a, b]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char(' '));
    assert!(app.pending_mutations.is_empty(), "{:?}", app.pending_mutations);
    let msg = app.status_message.clone().unwrap_or_default();
    assert!(msg.contains("'b'") && msg.contains("prune each other"), "{msg}");
}

#[test]
fn space_still_enables_a_job_with_no_clash() {
    let mut a = crate::jobs::Job::test_snapshot("a", "tank/data", "hourly", 3);
    a.enabled = false;
    let (mut app, _dir) = app_with_jobs(&[a]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char(' '));
    assert_eq!(app.pending_mutations.len(), 1);
}

fn repl_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::ReplicateJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn repl_fields(values: [&str; 8]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
}

fn repl_err(values: [&str; 8], jobs: Vec<crate::jobs::Job>) -> String {
    build_replicate_job_action(&repl_kind(None, jobs), &repl_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
}

const GOOD: [&str; 8] = ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"];

#[test]
fn big_r_in_jobs_opens_a_blank_replication_form_with_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('R'));
    assert_eq!(form_values(&app), ["local", "", "auto", "", "", "n", "30 * * * *", "y", ""]);
}

#[test]
fn a_valid_replication_form_builds_a_save_with_a_fresh_id_and_creation_time() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let action = build_replicate_job_action(&repl_kind(None, vec![]), &repl_fields(GOOD), std::path::Path::new("/state"), now).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.kind.as_str(), job.host.as_str(), job.dataset.as_deref(), job.prefix.as_deref(), job.dest_host.as_deref(), job.dest_dataset.as_deref(), job.force), (crate::jobs::KIND_REPLICATE, "local", Some("tank/data"), Some("hourly"), Some("fbsd16"), Some("backup/data"), false));
    assert_eq!(job.id, "repl-local-tank-data-to-fbsd16-backup-data");
    assert_eq!(job.created_at, Some(now));
    assert!(job.enabled && job.catch_up && job.pool.is_none() && job.keep.is_none());
}

#[test]
fn the_replication_form_refuses_what_the_daemon_would_refuse() {
    let with = |i: usize, v: &'static str| {
        let mut vals = GOOD;
        vals[i] = v;
        vals
    };
    assert!(repl_err(with(0, "nope"), vec![]).contains("host"));
    assert!(repl_err(with(1, "-rf"), vec![]).contains("source dataset"));
    assert!(repl_err(with(2, "a b"), vec![]).contains("prefix"));
    assert!(repl_err(with(3, ""), vec![]).contains("destination host"));
    assert!(repl_err(with(3, "nope"), vec![]).contains("host"));
    assert!(repl_err(with(4, "tank/a@b"), vec![]).contains("destination dataset"));
    assert!(repl_err(with(5, "maybe"), vec![]).contains("Force"));
    assert!(repl_err(with(6, "bogus"), vec![]).contains("Schedule"));
    assert!(repl_err(with(7, "q"), vec![]).contains("Catch up"));
    assert!(repl_err(["fbsd16", "backup/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"], vec![]).contains("same dataset"));
    let mut to_file = GOOD;
    to_file[3] = "file";
    to_file[4] = "/mnt/usb";
    assert!(build_replicate_job_action(&repl_kind(None, vec![]), &repl_fields(to_file), std::path::Path::new("/x"), chrono::Utc::now()).is_ok(), "a file destination with an absolute directory is accepted");
    to_file[4] = "relative/dir";
    assert!(repl_err(to_file, vec![]).contains("dest_dir"));
}

#[test]
fn the_replication_form_refuses_a_destination_another_job_already_uses() {
    let other = crate::jobs::Job::test_replicate("other", "tank/elsewhere", "hourly", "fbsd16", "backup/data");
    assert!(repl_err(GOOD, vec![other.clone()]).contains("other"));
    // Editing that same job is fine.
    let kind = repl_kind(Some(other.clone()), vec![other]);
    assert!(build_replicate_job_action(&kind, &repl_fields(GOOD), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
}

#[test]
fn enter_on_a_replicate_job_edits_it_in_the_replication_form() {
    let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")]);
    app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y", ""]);
}

#[test]
fn f2_on_the_replication_forms_fields_opens_the_right_pickers() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
    app.handle_key(KeyCode::Char('R'));
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16"], "source host");
    app.picker = None;
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)], "source dataset: the source host's datasets");
    app.picker = None;
    app.pending_pool_fetch.clear();
    app.handle_modal_key(KeyCode::Tab); // prefix: free text
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.is_none());
    app.handle_modal_key(KeyCode::Tab); // destination host
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.picker.as_ref().unwrap().values, vec!["local", "fbsd16", "file"], "destination host also offers file");
    app.picker = None;
    // Type the destination host, then F2 on the destination dataset lists *that* host's datasets.
    for c in "fbsd16".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.pending_pool_fetch, vec![(1, crate::ui::picker::LiveNames::Datasets)]);
}

#[test]
fn f2_on_the_destination_dataset_of_a_file_destination_says_to_type_a_path() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('R'));
    for _ in 0..3 {
        app.handle_modal_key(KeyCode::Tab);
    }
    for c in "file".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("directory")));
    assert!(app.pending_pool_fetch.is_empty());
}

#[test]
fn space_refuses_to_enable_a_replicate_job_that_clashes_on_its_destination() {
    let mut a = crate::jobs::Job::test_replicate("a", "tank/one", "hourly", "fbsd16", "backup/data");
    a.enabled = false;
    let b = crate::jobs::Job::test_replicate("b", "tank/two", "hourly", "fbsd16", "backup/data");
    let (mut app, _dir) = app_with_jobs(&[a, b]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char(' '));
    assert!(app.pending_mutations.is_empty());
    assert!(app.status_message.clone().unwrap_or_default().contains("same destination"));
}

#[test]
fn c_cancels_a_running_replication_and_is_inert_otherwise() {
    let repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data");
    let (mut app, dir) = app_with_jobs(&[repl]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char('c'));
    assert!(app.status_message.is_none(), "not running: nothing happens");
    assert!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")).is_empty());
    let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
    h.append(&crate::daemon::history::Event { job_id: "r".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Started, at: chrono::Utc::now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }).unwrap();
    app.refresh_jobs();
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")), ["r"]);
    assert!(app.status_message.clone().unwrap_or_default().contains("Cancel requested"));
}

fn be(name: &str, now: bool, reboot: bool, temp: bool, mount: Option<&str>) -> crate::zfs::bootenv::BootEnv {
    crate::zfs::bootenv::BootEnv {
        name: name.to_string(),
        flags: crate::zfs::bootenv::ActiveFlags { now, reboot, temp },
        mountpoint: mount.map(str::to_string),
        space: "1M".to_string(),
        created: "2026-10-07 19:26".to_string(),
    }
}

#[test]
fn boot_envs_sits_between_shares_and_jobs() {
    let app = App::new(vec!["local".to_string()]);
    let at = |s| app.sections.iter().position(|x| *x == s).unwrap();
    assert_eq!(at(Section::BootEnvs), at(Section::Shares) + 1);
    assert_eq!(at(Section::Jobs), at(Section::BootEnvs) + 1);
    assert_eq!(app.sections.last(), Some(&Section::Settings));
}

#[test]
fn refresh_bootenvs_populates_app_state() {
    let mut app = App::new(vec!["local".to_string()]);
    app.refresh_bootenvs(runner("default\tNR\t/\t5.18G\t2026-07-24 18:48\nspare\t-\t-\t8K\t2026-10-07 19:26\n"));
    wait_until(&mut app, |a| !a.bootenvs.is_empty());
    assert_eq!(app.bootenvs.len(), 2);
    assert!(app.bootenvs[0].flags.now && app.bootenvs[0].flags.reboot);
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn a_failing_bectl_marks_the_section_unavailable_and_the_next_good_refresh_recovers() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("stale", false, false, false, None)];
    app.bootenvs_table.select(Some(0));
    app.tx.send(WorkerMsg::BootEnvs(0, Err(crate::error::AppError::CommandFailed { cmd: "bectl list".into(), status: 127, stderr: "bectl: not found".into() }))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty(), "a stale list must not look current");
    assert_eq!(app.bootenvs_table.selected(), None);
    assert!(app.bootenvs_unavailable.as_deref().is_some_and(|m| m.contains("bectl: not found")));
    // Not gated on the earlier failure: the periodic refresh still runs and recovers.
    app.auto_refresh(runner("default\tNR\t/\t1G\td\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.bootenvs.is_empty());
    assert_eq!(app.bootenvs_unavailable, None);
    assert_eq!(app.bootenvs.len(), 1);
}

#[test]
fn a_host_with_zero_boot_environments_is_empty_not_unavailable() {
    let mut app = in_section(Section::BootEnvs);
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty());
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn bootenvs_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx.send(WorkerMsg::BootEnvs(1, Ok(vec![be("x", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty());
}

#[test]
fn switching_host_clears_boot_environment_state() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.bootenvs = vec![be("x", false, false, false, None)];
    app.bootenvs_table.select(Some(0));
    app.bootenvs_unavailable = Some("boom".to_string());
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert!(app.bootenvs.is_empty());
    assert_eq!(app.bootenvs_table.selected(), None);
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn a_refresh_keeps_the_selection_on_the_same_boot_environment_and_clamps_when_it_vanishes() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
    app.bootenvs_table.select(Some(1));
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("0", false, false, false, None), be("a", false, false, false, None), be("b", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), Some(2), "still on b");
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("a", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), Some(0), "b vanished: clamped into range");
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), None);
}

#[test]
fn up_and_down_move_the_boot_environment_selection() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.bootenvs_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("b"));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.bootenvs_table.selected(), Some(0));
}

fn bootenv_app(rows: Vec<crate::zfs::bootenv::BootEnv>, select: usize) -> App {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = rows;
    app.bootenvs_table.select(Some(select));
    app
}

fn running() -> crate::zfs::bootenv::BootEnv {
    be("default", true, true, false, Some("/"))
}

fn spare() -> crate::zfs::bootenv::BootEnv {
    be("spare", false, false, false, None)
}

fn only_pending(app: &App) -> &MutationCall {
    let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
    assert_eq!(action.section, Section::BootEnvs);
    &action.call
}

#[test]
fn n_opens_the_create_form_listing_existing_names_for_the_picker() {
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(form_values(&app), ["", ""]);
    let Some(Modal::Form { kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(*kind, FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string(), "spare".to_string()] });
}

#[test]
fn n_works_with_zero_boot_environments_but_not_when_unavailable() {
    let mut app = in_section(Section::BootEnvs);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_some());
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs_unavailable = Some("no bectl".to_string());
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none());
}

/// Opens the create form, types both fields and presses Enter on the last one.
fn submit(app: &mut App, name: &str, from: &str) -> Option<PendingAction> {
    app.handle_key(KeyCode::Char('n'));
    for c in name.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in from.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter)
}

#[test]
fn the_create_form_returns_the_create_call_with_or_without_a_clone_source() {
    let mut app = bootenv_app(vec![running(), spare()], 0);
    let action = submit(&mut app, "pre-upgrade", "spare").expect("submit returns the action");
    assert_eq!((action.host, action.section), (0, Section::BootEnvs));
    assert_eq!(action.call, MutationCall::CreateBootEnv { name: "pre-upgrade".to_string(), from: Some("spare".to_string()) });
    let mut app = bootenv_app(vec![running(), spare()], 0);
    let action = submit(&mut app, "pre-upgrade", "").expect("submit returns the action");
    assert_eq!(action.call, MutationCall::CreateBootEnv { name: "pre-upgrade".to_string(), from: None });
    assert!(app.modal.is_none());
}

#[test]
fn the_create_form_refuses_bad_names_duplicates_and_unknown_sources_with_a_reason() {
    for (name, from, why) in [
        ("", "", "cannot be empty"),
        ("-F", "", "cannot start with '-'"),
        ("a b", "", "not allowed"),
        ("default", "", "already exists"),
        ("ok", "nope", "no boot environment named 'nope'"),
        ("ok", "-e", "no boot environment named '-e'"),
    ] {
        let mut app = bootenv_app(vec![running(), spare()], 0);
        assert!(submit(&mut app, name, from).is_none(), "{name:?}/{from:?}: nothing is submitted");
        assert!(app.modal.is_some(), "{name:?}/{from:?}: the form stays open");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains(why)), "{name:?}/{from:?}: {:?}", app.status_message);
    }
}

#[test]
fn build_bootenv_action_makes_the_expected_calls() {
    let create = FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string()] };
    let field = |v: &str| FormField { label: "l".to_string(), value: v.to_string() };
    let a = build_bootenv_action(&create, &[field("new"), field("default")]).unwrap();
    assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: Some("default".to_string()) });
    assert_eq!(a.section, Section::BootEnvs);
    let a = build_bootenv_action(&create, &[field("new"), field("")]).unwrap();
    assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: None });
    let rename = FormKind::RenameBootEnv { host: 2, from: "old".to_string(), existing: vec!["old".to_string(), "taken".to_string()] };
    let a = build_bootenv_action(&rename, &[field("fresh")]).unwrap();
    assert_eq!((a.host, a.call), (2, MutationCall::RenameBootEnv { from: "old".to_string(), to: "fresh".to_string() }));
    assert!(build_bootenv_action(&rename, &[field("taken")]).is_err());
    assert!(build_bootenv_action(&rename, &[field("old")]).is_err(), "renaming to itself");
    assert!(build_bootenv_action(&rename, &[field("-x")]).is_err());
}

#[test]
fn a_asks_to_confirm_then_activates_permanently_and_only_when_it_would_change_something() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('a'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("'spare'"), "{prompt}");
    assert_eq!(action.call, MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: false });
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('a'));
    assert!(app.modal.is_none(), "already the BE used on reboot");
}

#[test]
fn big_t_clears_a_temporary_activation_without_a_confirm() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('T'));
    assert!(app.pending_mutations.is_empty(), "spare has no temporary activation to clear");
    let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
    app.handle_key(KeyCode::Char('T'));
    assert_eq!(only_pending(&app), &MutationCall::ClearTemporaryBootEnv { name: "trial".to_string() });
    let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.pending_mutations.is_empty(), "already temporary");
    let mut app = bootenv_app(vec![running()], 0);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.pending_mutations.is_empty(), "the running BE");
}

#[test]
fn m_mounts_an_unmounted_be_and_u_unmounts_a_mounted_one() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(only_pending(&app), &MutationCall::MountBootEnv { name: "spare".to_string() });
    app.handle_key(KeyCode::Char('u'));
    assert_eq!(app.pending_mutations.len(), 1, "an unmounted BE cannot be unmounted");
    let mut app = bootenv_app(vec![running(), be("m", false, false, false, Some("/tmp/be_mount.X"))], 1);
    app.handle_key(KeyCode::Char('u'));
    assert_eq!(only_pending(&app), &MutationCall::UnmountBootEnv { name: "m".to_string() });
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1, "already mounted");
    let mut app = bootenv_app(vec![running()], 0);
    app.handle_key(KeyCode::Char('m'));
    app.handle_key(KeyCode::Char('u'));
    assert!(app.pending_mutations.is_empty(), "the running BE is mounted at / and cannot be remounted or unmounted");
}

#[test]
fn big_r_opens_the_rename_form() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('R'));
    assert_eq!(form_values(&app), [""]);
    let Some(Modal::Form { kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(*kind, FormKind::RenameBootEnv { host: 0, from: "spare".to_string(), existing: vec!["default".to_string(), "spare".to_string()] });
}

#[test]
fn d_and_big_d_ask_for_the_name_and_the_running_be_is_refused_with_a_reason() {
    for (key, origin) in [('d', false), ('D', true)] {
        let mut app = bootenv_app(vec![running(), spare()], 1);
        app.handle_key(KeyCode::Char(key));
        let Some(Modal::ConfirmTyped { expected, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
        assert_eq!(expected, "spare");
        assert_eq!(action.call, MutationCall::DestroyBootEnv { name: "spare".to_string(), origin });
    }
    for blocked in [running(), be("next", false, true, false, None), be("trial", false, false, true, None)] {
        let mut app = bootenv_app(vec![blocked.clone()], 0);
        app.handle_key(KeyCode::Char('d'));
        assert!(app.modal.is_none());
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains(&blocked.name) && m.contains("cannot be destroyed")), "{:?}", app.status_message);
    }
}

#[test]
fn an_empty_list_is_loaded_but_a_failure_or_a_host_switch_makes_it_unloaded() {
    let mut app = in_section(Section::BootEnvs);
    assert!(!app.bootenvs_loaded);
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs_loaded, "zero boot environments is a real answer");
    app.tx.send(WorkerMsg::BootEnvs(0, Err(crate::error::AppError::Parse("x".into())))).unwrap();
    app.drain_worker_messages();
    assert!(!app.bootenvs_loaded);
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    for _ in 0..6 {
        app.handle_key(KeyCode::BackTab);
    }
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::Down); // switch host
    assert!(!app.bootenvs_loaded);
}

#[test]
fn a_successful_boot_environment_mutation_queues_a_re_list() {
    let mut app = in_section(Section::BootEnvs);
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
    assert_eq!(app.drain_worker_messages(), vec![Section::BootEnvs]);
}

#[test]
fn help_documents_the_boot_environment_keys() {
    let text = help_text();
    for want in ["Boot Envs", "next boot only", "rename", "mount", "destroy"] {
        assert!(text.contains(want), "missing {want:?}");
    }
}

const BACKUP_GOOD: [&str; 10] = ["local", "tank/data", "", "hourly", "", "/mnt/usb/restic", "/root/pw", "7d 4w", "30 3 * * *", "y"];

fn backup_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::BackupJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn backup_fields(values: [&str; 10]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: "x".to_string(), value: (*v).to_string() }).collect()
}

fn backup_err(values: [&str; 10]) -> String {
    build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
}

#[test]
fn big_b_in_jobs_opens_the_backup_form_with_sensible_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('B'));
    assert_eq!(form_values(&app), ["local", "", "", "new", "", "", "", "", "30 3 * * *", "y"]);
    assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::BackupJob { .. }, .. })));
}

fn saved_backup(values: [&str; 10]) -> crate::jobs::Job {
    let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    *job
}

#[test]
fn the_source_field_saves_new_a_pinned_snapshot_or_a_prefix_and_the_path_is_optional() {
    let mut values = BACKUP_GOOD;
    values[3] = "new";
    values[4] = "5";
    values[2] = "photos/2026";
    let job = saved_backup(values);
    assert_eq!((job.source.as_deref(), job.snapshot.as_deref(), job.prefix.as_deref(), job.keep, job.path.as_deref()), (Some("new"), None, None, Some(5), Some("photos/2026")));
    assert_eq!(crate::jobs::backup_source(&job), crate::jobs::BackupSource::Fresh);
    values[4] = "";
    assert_eq!(saved_backup(values).keep, None, "blank means the default of 3");
    values[3] = "@snap4";
    values[4] = "5";
    values[2] = "";
    let job = saved_backup(values);
    assert_eq!((job.source.as_deref(), job.snapshot.as_deref(), job.prefix.as_deref(), job.keep, job.path.as_deref()), (Some("snapshot"), Some("snap4"), None, None, None));
    let job = saved_backup(BACKUP_GOOD);
    assert_eq!((job.source.as_deref(), job.prefix.as_deref(), job.keep), (None, Some("hourly"), None));
}

#[test]
fn editing_a_new_source_job_shows_new_and_its_keep_and_a_pinned_one_shows_at_name() {
    let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    job.source = Some("new".into());
    job.keep = Some(4);
    job.path = Some("docs".into());
    let (mut app, _dir) = app_with_jobs(&[job.clone()]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[2..5], ["docs", "new", "4"]);
    job.source = Some("snapshot".into());
    job.snapshot = Some("snap4".into());
    job.keep = None;
    let (mut app, _dir) = app_with_jobs(&[job]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[3..5], ["@snap4", ""]);
}

#[test]
fn f2_on_the_source_field_lists_the_snapshots_of_the_forms_dataset() {
    let mut app = backup_form_app(BACKUP_GOOD);
    let Some(Modal::Form { focused, .. }) = &mut app.modal else { panic!() };
    *focused = 3;
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.as_ref().is_some_and(|p| p.pool_host == Some(0)));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Snapshots("tank/data".to_string()))]);
}

#[test]
fn f2_on_the_source_field_without_a_dataset_says_to_enter_one_and_fetches_nothing() {
    let mut values = BACKUP_GOOD;
    values[1] = "";
    let mut app = backup_form_app(values);
    let Some(Modal::Form { focused, .. }) = &mut app.modal else { panic!() };
    *focused = 3;
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.pending_pool_fetch.is_empty());
    assert!(matches!(&app.picker, Some(p) if matches!(&p.load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("dataset"))));
}

#[test]
fn a_backup_job_that_takes_snapshots_asks_before_it_starts_destroying_its_own() {
    let fresh = |keep: Option<u32>| crate::jobs::Job { source: Some("new".into()), keep, ..backup_job(None) };
    let prompt = job_save_prompt(None, &fresh(Some(3)), None).expect("a new snapshot-taking job asks");
    assert!(prompt.contains("takes a snapshot of tank/data") && prompt.contains("newest 3") && prompt.contains("zit-"), "{prompt}");
    assert_eq!(job_save_prompt(Some(&fresh(Some(3))), &fresh(Some(3)), None), None, "unchanged");
    assert_eq!(job_save_prompt(Some(&fresh(Some(3))), &fresh(Some(5)), None), None, "keeping more is safe");
    assert!(job_save_prompt(Some(&fresh(Some(5))), &fresh(Some(3)), None).is_some(), "keeping fewer destroys");
    assert!(job_save_prompt(Some(&backup_job(None)), &fresh(None), None).is_some(), "switching to new asks");
}

#[test]
fn big_b_outside_jobs_does_nothing() {
    let mut app = in_section(Section::Pools);
    app.handle_key(KeyCode::Char('B'));
    assert!(app.modal.is_none());
}

#[test]
fn a_good_backup_form_saves_a_backup_job_with_a_generated_id() {
    let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(BACKUP_GOOD), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!("{:?}", action.call) };
    assert_eq!((job.id.as_str(), job.kind.as_str(), job.host.as_str(), job.dataset.as_deref(), job.prefix.as_deref()), ("backup-local-tank-data", "backup", "local", Some("tank/data"), Some("hourly")));
    assert_eq!((job.repository.as_deref(), job.password_file.as_deref(), job.retention.as_deref()), (Some("/mnt/usb/restic"), Some("/root/pw"), Some("7d 4w")));
    assert_eq!((job.schedule.as_str(), job.catch_up, job.enabled), ("30 3 * * *", true, true));
    assert_eq!(action.section, Section::Jobs);
}

#[test]
fn a_blank_keep_field_saves_no_retention_and_a_blank_prefix_means_the_default() {
    let mut values = BACKUP_GOOD;
    values[7] = "  ";
    values[3] = "";
    let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.retention, job.prefix.as_deref()), (None, Some(crate::jobs::DEFAULT_PREFIX)));
}

#[test]
fn the_backup_form_refuses_what_the_daemon_would_refuse() {
    let with = |i: usize, v: &'static str| {
        let mut vals = BACKUP_GOOD;
        vals[i] = v;
        vals
    };
    assert!(backup_err(with(0, "nope")).contains("host"));
    assert!(backup_err(with(1, "-rf")).contains("dataset"));
    assert!(backup_err(with(3, "a b")).contains("Source"));
    assert!(backup_err(with(5, "")).contains("repository"));
    assert!(backup_err(with(5, "-r")).contains("repository"));
    assert!(backup_err(with(6, "relative")).contains("password_file"));
    assert!(backup_err(with(7, "7x")).contains("retention"));
    assert!(backup_err(with(8, "bogus")).contains("Schedule"));
    assert!(backup_err(with(9, "q")).contains("Catch up"));
    assert!(backup_err(with(2, "../x")).contains("path"));
    assert!(backup_err(with(2, "/etc")).contains("path"));
    assert!(backup_err(with(3, "@a b")).contains("Source"));
    let mut fresh = BACKUP_GOOD;
    fresh[3] = "new";
    fresh[4] = "0";
    assert!(backup_err(fresh).contains("Snapshots to keep"));
    fresh[4] = "many";
    assert!(backup_err(fresh).contains("Snapshots to keep"));
}

#[test]
fn enter_on_a_backup_job_edits_it_in_the_backup_form_and_keeps_its_id_and_state() {
    let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    job.retention = Some("7d".to_string());
    job.enabled = false;
    let (mut app, _dir) = app_with_jobs(&[job.clone()]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["local", "tank/data", "", "hourly", "", "/mnt/usb/restic", "/root/pw", "7d", "0 3 * * *", "y"]);
    let kind = backup_kind(Some(job.clone()), vec![job]);
    let mut values = BACKUP_GOOD;
    values[5] = "/mnt/other";
    let action = build_backup_job_action(&kind, &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.id.as_str(), job.enabled, job.repository.as_deref()), ("backup-x", false, Some("/mnt/other")));
}

fn edit_backup_err_or_job(host: &str, dataset: &str) -> std::result::Result<crate::jobs::Job, String> {
    let job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    let mut values = BACKUP_GOOD;
    values[0] = host;
    values[1] = dataset;
    build_backup_job_action(&backup_kind(Some(job.clone()), vec![job]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).map(|a| match a.call {
        MutationCall::SaveJob { job, .. } => *job,
        other => panic!("{other:?}"),
    })
}

#[test]
fn editing_a_backup_job_refuses_a_new_host_or_pool_but_allows_another_dataset_in_the_same_pool() {
    let e = edit_backup_err_or_job("fbsd16", "tank/data").unwrap_err();
    assert!(e.contains("host or pool") && e.contains("create a new one"), "{e}");
    assert!(edit_backup_err_or_job("local", "other/data").unwrap_err().contains("host or pool"));
    assert_eq!(edit_backup_err_or_job("local", "tank/other").unwrap().dataset.as_deref(), Some("tank/other"));
}

#[test]
fn c_cancels_a_running_backup_and_is_inert_otherwise() {
    let job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    let (mut app, dir) = app_with_jobs(&[job]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Char('c'));
    assert!(app.status_message.is_none(), "not running: nothing happens");
    assert!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")).is_empty());
    let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
    h.append(&crate::daemon::history::Event { job_id: "backup-x".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Started, at: chrono::Utc::now(), scheduled_for: None, result: None, message: String::new(), catch_up: false, summary: None }).unwrap();
    app.refresh_jobs();
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(crate::daemon::cancel::take_requests(&dir.path().join("cancel")), ["backup-x"]);
    assert!(app.status_message.clone().unwrap_or_default().contains("Cancel requested"));
}

#[test]
fn help_documents_the_backup_form_check_key() {
    assert!(help_text().contains("F5"));
}

#[test]
fn help_documents_the_backup_key() {
    let text = help_text();
    assert!(text.contains("new backup job"), "{text}");
}

#[test]
fn help_explains_the_two_step_confirmation() {
    let text = help_text();
    assert!(text.contains("takes two keys") && text.contains("Enter alone does nothing"), "{text}");
}


fn destroy_app() -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    let ds = |name: &str| crate::zfs::dataset::Dataset { name: name.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None };
    app.datasets = vec![ds("tank/data"), ds("tank/data/a"), ds("tank/data/b"), ds("tank/data2"), ds("tank/other")];
    app.datasets_table.select(Some(0));
    let snap = |d: &str, n: &str| crate::zfs::snapshot::Snapshot { dataset: d.to_string(), name: n.to_string(), used: 0 };
    app.snapshots = vec![snap("tank/data", "s1"), snap("tank/data", "s2"), snap("tank/data/a", "s1"), snap("tank/data2", "x"), snap("tank/other", "x")];
    app.snapshots_loaded = true;
    app
}

#[test]
fn big_d_on_a_dataset_asks_for_its_name_and_says_what_goes_with_it() {
    let mut app = destroy_app();
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, expected, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(expected, "tank/data");
    assert_eq!(action.call, MutationCall::DestroyDatasetRecursive { name: "tank/data".to_string() });
    assert!(prompt.contains("2 child datasets") && prompt.contains("3 snapshots"), "siblings like tank/data2 are not counted: {prompt}");
    assert!(prompt.contains("last refresh"), "{prompt}");
    for c in "tank/data".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let run = app.handle_modal_key(KeyCode::Enter).expect("typed the name, then Enter");
    assert_eq!(run.call, MutationCall::DestroyDatasetRecursive { name: "tank/data".to_string() });
    assert_eq!(run.section, Section::Datasets);
}

#[test]
fn the_prompt_counts_singular_correctly_and_admits_when_snapshots_are_unknown() {
    let mut app = destroy_app();
    app.datasets.retain(|d| d.name != "tank/data/b");
    app.snapshots.retain(|s| s.dataset == "tank/data" && s.name == "s1");
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("1 child dataset ") && prompt.contains("1 snapshot"), "{prompt}");
    let mut app = destroy_app();
    app.snapshots_loaded = false;
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("snapshots: unknown") && !prompt.contains("3 snapshots"), "{prompt}");
}

#[test]
fn plain_d_warns_that_zfs_will_refuse_and_points_at_big_d_only_when_there_is_something_under_it() {
    let mut app = destroy_app();
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::ConfirmTyped { prompt, action, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("zfs will refuse") && prompt.contains("press Esc, then D"), "{prompt}");
    assert_eq!(action.call, MutationCall::DestroyDataset { name: "tank/data".to_string() }, "d stays non-recursive");
    let mut app = destroy_app();
    app.datasets_table.select(Some(4)); // tank/other: one snapshot, no children
    app.snapshots.retain(|s| s.dataset != "tank/other");
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(!prompt.contains("zfs will refuse"), "nothing under it: {prompt}");
}

#[test]
fn big_d_is_inert_without_a_selection_or_with_the_right_pane_focused() {
    let mut app = destroy_app();
    app.datasets_table.select(None);
    app.handle_key(KeyCode::Char('D'));
    assert!(app.modal.is_none());
    let mut app = destroy_app();
    app.pane_focus = PaneFocus::Right;
    app.handle_key(KeyCode::Char('D'));
    assert!(app.modal.is_none());
}

#[test]
fn snapshots_count_as_loaded_after_a_refresh_and_not_after_a_host_switch() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    assert!(!app.snapshots_loaded);
    app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert!(app.snapshots_loaded, "an empty list is still a loaded list");
    app.handle_key(KeyCode::Down); // switch host (Hosts section)
    assert!(!app.snapshots_loaded);
}

#[test]
fn help_documents_recursive_destroy() {
    assert!(help_text().contains("D                destroy a dataset AND its child datasets and snapshots"), "{}", help_text());
}


fn rename_to(app: &mut App, new_name: &str) -> PendingAction {
    app.handle_key(KeyCode::Char('R'));
    for c in new_name.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter).expect("the rename form submits")
}

fn names(app: &App) -> Vec<&str> {
    app.bootenvs.iter().map(|b| b.name.as_str()).collect()
}

#[test]
fn the_selection_follows_a_renamed_boot_environment_instead_of_sliding_to_its_neighbour() {
    let rows = vec![running(), spare(), be("zeta", false, false, false, None)];
    let mut app = bootenv_app(rows, 1); // spare
    rename_to(&mut app, "zz");
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![running(), be("zeta", false, false, false, None), be("zz", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(names(&app), ["default", "zeta", "zz"]);
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("zz"), "index 1 is now zeta: the key must not act on it");
}

#[test]
fn a_refresh_that_lands_before_the_rename_does_not_use_up_the_pin() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    rename_to(&mut app, "zz");
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![running(), spare()]))).unwrap(); // an auto-refresh from before the rename
    app.drain_worker_messages();
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("spare"));
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![running(), be("zz", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("zz"), "the later list still honours the pin");
}

#[test]
fn a_failed_rename_drops_the_pin() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    rename_to(&mut app, "zz");
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Err(crate::error::AppError::Parse("no".into())))).unwrap();
    app.drain_worker_messages();
    // A later, unrelated list with a boot environment of that name must not steal the selection.
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![running(), spare(), be("zz", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("spare"));
}

#[test]
fn no_confirm_keys_cannot_fire_twice_before_the_first_change_has_finished() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1);
    for key in ['m', 'u', 't', 'T', 'a', 'd', 'D', 'R', 'n'] {
        app.handle_key(KeyCode::Char(key));
    }
    assert_eq!(app.pending_mutations.len(), 1, "nothing else was queued");
    assert!(app.modal.is_none(), "no window opened for a change while one is running");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Still working on 'spare'")), "{:?}", app.status_message);
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
    app.drain_worker_messages();
    app.pending_mutations.clear();
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1, "free again once the change finished");
}

#[test]
fn a_change_started_from_a_confirmation_marks_the_section_busy_until_it_finishes() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('a'));
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("confirmed");
    app.dispatch_mutation(action, std::sync::Arc::new(FakeRunner::new("")));
    app.handle_key(KeyCode::Char('t'));
    assert!(app.pending_mutations.is_empty() && app.status_message.as_deref().is_some_and(|m| m.contains("Still working")), "{:?}", app.status_message);
    wait_until(&mut app, |a| a.bootenv_busy.is_none());
    assert!(app.bootenv_busy.is_none());
}

#[test]
fn a_stuck_busy_marker_expires_so_the_section_can_never_be_locked_for_good() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.bootenv_busy = Some(("spare".to_string(), Instant::now().checked_sub(Duration::from_mins(5)).unwrap()));
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1);
}

#[test]
fn switching_host_clears_the_busy_marker_and_the_pin() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.bootenv_busy = Some(("x".to_string(), Instant::now()));
    app.bootenv_pin = Some("y".to_string());
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::BackTab);
    app.handle_key(KeyCode::Down);
    assert!(app.bootenv_busy.is_none() && app.bootenv_pin.is_none());
}


#[test]
fn recipient_keywords_are_case_insensitive_and_whitespace_tolerant_but_names_keep_their_case() {
    use crate::zfs::permission::Recipient;
    assert_eq!(parse_recipient_field("User:Carol"), Some(Recipient::User("Carol".into())));
    assert_eq!(parse_recipient_field("  GROUP : wheel  "), Some(Recipient::Group("wheel".into())));
    assert_eq!(parse_recipient_field(" user:  bob "), Some(Recipient::User("bob".into())));
    assert_eq!(parse_recipient_field("EVERYONE"), Some(Recipient::Everyone));
    for bad in ["user:", "user:   ", "group", "users:bob", "", "bob"] {
        assert_eq!(parse_recipient_field(bad), None, "{bad:?}");
    }
    use crate::acl::AclTag;
    assert_eq!(parse_acl_recipient_field(" Owner "), Some(AclTag::Owner));
    assert_eq!(parse_acl_recipient_field("GROUP"), Some(AclTag::OwningGroup));
    assert_eq!(parse_acl_recipient_field("Group: Wheel"), Some(AclTag::NamedGroup("Wheel".into())));
    assert_eq!(parse_acl_recipient_field("USER :carol"), Some(AclTag::User("carol".into())));
    assert_eq!(parse_acl_recipient_field("user:  "), None);
}

fn open_form(app: &mut App, kind: FormKind, values: &[&str]) {
    let fields = values.iter().map(|v| FormField { label: "f".to_string(), value: (*v).to_string() }).collect::<Vec<_>>();
    app.modal = Some(Modal::Form { title: "t".to_string(), focused: fields.len() - 1, fields, description: None, kind });
}

fn submit_problem(kind: FormKind, values: &[&str]) -> String {
    let mut app = App::new(vec!["local".to_string()]);
    open_form(&mut app, kind, values);
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "an invalid form submits nothing");
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "and stays open so the typing is not lost");
    app.status_message.unwrap_or_default()
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
fn each_form_problem_says_which_field_is_wrong() {
    let grant = || FormKind::GrantPermission { host: 0, dataset: "tank/data".to_string() };
    assert!(submit_problem(grant(), &["bob", "both", "snapshot"]).contains("Recipient"));
    assert!(submit_problem(grant(), &["user:carol", "sideways", "snapshot"]).contains("Scope"));
    assert!(submit_problem(grant(), &["user:carol", "both", " , "]).contains("Permissions"));
    let set = || FormKind::GrantPermissionSet { host: 0, dataset: "tank/data".to_string() };
    assert!(submit_problem(set(), &["@ro", "snapshot"]).contains("without the leading '@'"));
    assert!(submit_problem(set(), &["", "snapshot"]).contains("Set name"));
    let acl = || FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 0 };
    assert!(submit_problem(acl(), &["nobody", "read_data", "", "allow"]).contains("Recipient"));
    assert!(submit_problem(acl(), &["owner", "read_data", "", "maybe"]).contains("Type"));
    // A form with no specific rule still says something useful.
    assert!(submit_problem(FormKind::CreatePool { host: 0 }, &["", ""]).contains("Could not submit"));
}

fn acl_app_with_acltype(value: Option<&str>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()) }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
    app.acl_entries_table.select(Some(0));
    app.acl_entries_for = Some("/tank/data".to_string());
    if let Some(v) = value {
        app.dataset_properties_for = Some("tank/data".to_string());
        app.dataset_properties = vec![Property { name: "acltype".to_string(), value: v.to_string(), source: PropertySource::Local }];
    }
    app
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
fn the_acltype_of_another_dataset_is_never_applied() {
    let mut app = acl_app_with_acltype(Some("posix"));
    app.dataset_properties_for = Some("tank/other".to_string());
    assert_eq!(app.acl_unsupported(), None, "stale properties of a different dataset say nothing about this one");
}


#[test]
fn the_datasets_view_refreshes_the_snapshot_list_quietly_with_the_datasets_so_destroy_prompts_can_count_them() {
    let mut app = in_section(Section::Datasets);
    app.status_message = Some("scrub started".to_string());
    app.auto_refresh(runner(""), Instant::now(), TEN);
    assert!(app.in_flight.contains(&(Section::Datasets, 0)) && app.in_flight.contains(&(Section::Snapshots, 0)));
    assert!(app.quiet_refresh.contains(&(Section::Snapshots, 0)), "quiet: its arrival must not wipe an informational status");
    wait_until(&mut app, |a| a.snapshots_loaded);
    assert!(app.snapshots_loaded);
    assert_eq!(app.status_message.as_deref(), Some("scrub started"));
    app.in_flight.clear();
    app.last_auto_refresh.clear();
    app.auto_refresh(runner(""), Instant::now(), TEN);
    assert!(app.in_flight.contains(&(Section::Snapshots, 0)), "every Datasets refresh also refreshes the snapshots, so the counts are as fresh as the list");
}


#[test]
fn uppercase_y_and_n_also_only_select_so_caps_lock_cannot_leave_the_user_stuck() {
    let (mut app, action) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Char('Y')), None);
    assert_eq!(choice_of(&app), Some(Some(true)));
    assert_eq!(app.handle_modal_key(KeyCode::Char('N')), None);
    assert_eq!(choice_of(&app), Some(Some(false)));
    app.handle_modal_key(KeyCode::Char('Y'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), Some(action), "still two keys: select, then Enter");
}

fn ds_named(name: &str, mountpoint: Option<&str>) -> crate::zfs::dataset::Dataset {
    crate::zfs::dataset::Dataset { name: name.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string) }
}

fn datasets_app_selecting(datasets: Vec<crate::zfs::dataset::Dataset>, selected: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    let index = datasets.iter().position(|d| d.name == selected).unwrap();
    app.datasets = datasets;
    app.datasets_table.select(Some(index));
    app.snapshots_loaded = true;
    app
}

#[test]
fn a_pool_root_cannot_be_destroyed_with_d_or_big_d_because_zfs_destroy_r_would_wipe_the_pool() {
    for key in ['d', 'D'] {
        let mut app = datasets_app_selecting(vec![ds_named("tank", Some("/tank")), ds_named("tank/data", Some("/tank/data"))], "tank");
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none(), "{key}: no window may open for a pool root");
        let status = app.status_message.clone().unwrap_or_default();
        assert!(status.contains("pool") && status.contains("Pools"), "{key}: {status}");
    }
}

#[test]
fn recursive_destroy_is_refused_on_boot_environment_datasets_and_on_anything_holding_the_running_root() {
    let all = || vec![
        ds_named("zroot", Some("/zroot")),
        ds_named("zroot/ROOT", None),
        ds_named("zroot/ROOT/default", Some("/")),
        ds_named("zroot/ROOT/other", None),
        ds_named("zroot/rootfs", None),
        ds_named("zroot/rootfs/inner", Some("/")),
        ds_named("zroot/home", Some("/home")),
    ];
    for refused in ["zroot/ROOT", "zroot/ROOT/default", "zroot/ROOT/other", "zroot/rootfs", "zroot/rootfs/inner"] {
        let mut app = datasets_app_selecting(all(), refused);
        app.handle_key(KeyCode::Char('D'));
        assert!(app.modal.is_none(), "{refused} must be refused");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Boot Envs") || m.contains("running")), "{refused}: {:?}", app.status_message);
    }
    let mut ok = datasets_app_selecting(all(), "zroot/home");
    ok.handle_key(KeyCode::Char('D'));
    assert!(matches!(ok.modal, Some(Modal::ConfirmTyped { .. })), "an ordinary dataset still works");
}

#[test]
fn the_recursive_prompt_admits_partial_destruction_and_describes_clones_precisely() {
    let mut app = destroy_app();
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("stops part-way") && prompt.contains("stays destroyed"), "{prompt}");
    assert!(prompt.contains("clones inside it go with it") && prompt.contains("outside"), "{prompt}");
}

#[test]
fn help_does_not_claim_esc_cancels_every_modal() {
    let text = help_text();
    assert!(text.contains("in a y/n confirmation Esc selects No"), "{text}");
}

fn probes_app() -> App {
    App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string()])
}

#[test]
fn address_of_strips_the_user_and_names_local_for_the_hub() {
    assert_eq!(address_of(None), "local");
    assert_eq!(address_of(Some("fbsd@172.16.10.61")), "172.16.10.61");
    assert_eq!(address_of(Some("172.16.10.62")), "172.16.10.62");
    assert_eq!(address_of(Some("root@nas.example.com")), "nas.example.com");
    assert_eq!(address_of(Some("")), "-");
}

#[test]
fn addresses_default_to_local_and_a_dash_and_follow_set_host_addresses() {
    let mut app = probes_app();
    assert_eq!((app.host_address(0), app.host_address(1)), ("local", "-"));
    app.set_host_addresses(vec!["local".into(), "172.16.10.60".into(), "172.16.10.61".into()]);
    assert_eq!(app.host_address(1), "172.16.10.60");
    assert_eq!(app.host_address(9), "-", "an index with no host");
}

#[test]
fn nothing_is_due_before_the_stagger_and_every_host_is_due_once_it_has_passed() {
    let app = probes_app();
    let t0 = app.probe_epoch;
    assert_eq!(app.due_host_probes(t0), vec![0], "host 0 immediately, the others one second apart");
    assert_eq!(app.due_host_probes(t0 + Duration::from_secs(1)), vec![0, 1]);
    assert_eq!(app.due_host_probes(t0 + Duration::from_secs(5)), vec![0, 1, 2]);
}

#[test]
fn a_host_is_not_probed_again_while_a_probe_runs_or_before_the_interval() {
    let mut app = probes_app();
    let t0 = app.probe_epoch;
    app.start_host_probe(0, std::sync::Arc::new(FakeRunner::new("")));
    assert!(app.host_probes["local"].in_flight);
    assert!(!app.due_host_probes(t0 + Duration::from_secs(600)).contains(&0), "one probe in flight per host");
    wait_until(&mut app, |a| !a.host_probes["local"].in_flight);
    let done = app.host_probes["local"].finished_at.unwrap();
    assert!(!app.due_host_probes(done + Duration::from_secs(29)).contains(&0), "not before the 30 s interval");
    assert!(app.due_host_probes(done + Duration::from_secs(30)).contains(&0), "due again after it");
}

#[test]
fn a_probe_result_is_stored_for_its_host_and_assessed() {
    let mut app = probes_app();
    app.tx.send(WorkerMsg::HostHealth("fbsd16".to_string(), crate::zfs::host_health::HostProbe::Disconnected)).unwrap();
    app.drain_worker_messages();
    let a = app.host_assessment(1);
    assert_eq!((a.level, a.reason.as_str()), (crate::zfs::host_health::Level::Disconnected, "unreachable"));
    assert_eq!(app.host_assessment(2).level, crate::zfs::host_health::Level::Probing, "never probed");
}

#[test]
fn a_result_for_a_host_that_no_longer_exists_is_discarded() {
    let mut app = probes_app();
    app.tx.send(WorkerMsg::HostHealth("ghost".to_string(), crate::zfs::host_health::HostProbe::Disconnected)).unwrap();
    app.drain_worker_messages();
    assert!(!app.host_probes.contains_key("ghost"));
}

#[test]
fn a_failed_job_of_that_host_warns_only_for_an_enabled_job_whose_latest_run_failed() {
    use crate::daemon::history::{Event, EventKind, History, RunResult};
    let dir = tempfile::tempdir().unwrap();
    let mut a = scrub_job("a");
    a.host = "fbsd16".to_string();
    let mut off = scrub_job("off");
    off.host = "fbsd16t1".to_string();
    off.enabled = false;
    let mut other = scrub_job("other");
    other.host = "local".to_string();
    crate::jobs::save(&dir.path().join("jobs.toml"), &[a, off, other]).unwrap();
    let h = History::new(dir.path().join("history.jsonl"));
    let ev = |id: &str, run: &str, at: &str, result| Event { job_id: id.into(), run_id: run.into(), event: EventKind::Finished, at: chrono::DateTime::parse_from_rfc3339(at).unwrap().with_timezone(&chrono::Utc), scheduled_for: None, result: Some(result), message: String::new(), catch_up: false, summary: None };
    h.append(&ev("a", "a1", "2026-10-09T01:00:00Z", RunResult::Failed)).unwrap();
    h.append(&ev("off", "o1", "2026-10-09T01:00:00Z", RunResult::Failed)).unwrap();
    h.append(&ev("other", "x1", "2026-10-09T01:00:00Z", RunResult::Ok)).unwrap();
    let mut app = probes_app();
    app.state_dir = dir.path().to_path_buf();
    app.refresh_jobs();
    let healthy = || crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os: None };
    for name in ["local", "fbsd16", "fbsd16t1"] {
        app.tx.send(WorkerMsg::HostHealth(name.to_string(), healthy())).unwrap();
    }
    app.drain_worker_messages();
    assert_eq!(app.host_assessment(1).reason, "job failed", "fbsd16 has an enabled job whose latest run failed");
    assert_eq!(app.host_assessment(2).reason, "ok", "its only failing job is disabled");
    assert_eq!(app.host_assessment(0).reason, "ok", "another host's failure is not this host's");
    h.append(&ev("a", "a2", "2026-10-09T02:00:00Z", RunResult::Ok)).unwrap();
    app.refresh_jobs();
    assert_eq!(app.host_assessment(1).reason, "ok", "a later success clears it");
}

#[test]
fn a_probe_that_never_answers_is_flagged_instead_of_showing_its_last_good_result() {
    let mut app = probes_app();
    let healthy = crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os: None };
    let long_ago = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();
    app.host_probes.insert("fbsd16".to_string(), HostProbeState { last: Some(healthy.clone()), in_flight: true, finished_at: Some(long_ago), started_at: Some(long_ago), kind: None, resources: vec![], utc_offset_secs: None });
    let a = app.host_assessment(1);
    assert_eq!(a.level, crate::zfs::host_health::Level::Warning);
    assert!(a.reason.starts_with("no answer for "), "{}", a.reason);
    app.host_probes.insert("fbsd16t1".to_string(), HostProbeState { last: Some(healthy), in_flight: true, finished_at: Some(long_ago), started_at: Some(Instant::now()), kind: None, resources: vec![], utc_offset_secs: None });
    assert_eq!(app.host_assessment(2).reason, "ok", "a probe that has only just started is normal");
}

/// Enter on a finished irreversible form, then the confirmation's `y` and Enter.
fn submit_and_confirm(app: &mut App) -> Option<PendingAction> {
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    app.handle_modal_key(KeyCode::Char('y'));
    app.handle_modal_key(KeyCode::Enter)
}

/// Presses Enter on a finished irreversible form and returns the prompt of the confirmation it opens,
/// asserting nothing ran yet.
fn submit_to_confirm(app: &mut App) -> String {
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "an irreversible form must not run on Enter");
    match &app.modal {
        Some(Modal::Confirm { prompt, choice: None, .. }) => prompt.clone(),
        other => panic!("expected an unanswered confirmation, got {other:?}"),
    }
}

fn pool_for_forms(app: &mut App) {
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
}

#[test]
fn add_vdev_shows_the_exact_command_and_runs_only_after_y_then_enter() {
    let mut app = App::new(vec!["local".to_string()]);
    pool_for_forms(&mut app);
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('a'));
    for c in "mirror da2 da3".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool add tank mirror da2 da3"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter with nothing selected does nothing");
    assert_eq!(app.handle_modal_key(KeyCode::Char('y')), None);
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter runs it");
    assert_eq!(action.call, MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["mirror".into(), "da2".into(), "da3".into()] });
}

#[test]
fn declining_an_irreversible_form_takes_two_steps_and_runs_nothing() {
    let mut app = App::new(vec!["local".to_string()]);
    pool_for_forms(&mut app);
    app.vdevs = vec![VdevLine { depth: 1, name: "da1".to_string(), health: crate::zfs::pool::PoolHealth::Faulted }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('x'));
    for c in "da2".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool replace tank da1 da2"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Char('n')), None);
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })), "n only selects");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert_eq!(open_form_values(&app), Some((vec!["da2".to_string()], 0)), "declining returns to the form with what was typed");
}

#[test]
fn create_pool_forms_confirm_with_the_command_and_the_warning_about_the_disks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    for c in "newpool".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter);
    for c in "mirror da0 da1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool create newpool mirror da0 da1") && prompt.contains("overwrit"), "{prompt}");
}

#[test]
fn the_from_disks_forms_confirm_too() {
    for kind in [FormKind::CreatePoolFromDisks { host: 0, disks: vec!["da0".into(), "da1".into()] }, FormKind::AddVdevFromDisks { host: 0, disks: vec!["da0".into(), "da1".into()] }] {
        let mut app = App::new(vec!["local".to_string()]);
        let (a, b) = if matches!(kind, FormKind::CreatePoolFromDisks { .. }) { ("newpool", "mirror") } else { ("tank", "mirror") };
        app.modal = Some(Modal::Form {
            title: "t".to_string(),
            fields: vec![FormField { label: "a".into(), value: a.into() }, FormField { label: "b".into(), value: b.into() }],
            focused: 1,
            description: None,
            kind,
        });
        let prompt = submit_to_confirm(&mut app);
        assert!(prompt.contains("da0 da1"), "{prompt}");
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
fn routine_forms_still_submit_on_enter() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![FormField { label: "name".into(), value: "tank/new".into() }],
        focused: 0,
        description: None,
        kind: FormKind::CreateDataset { host: 0 },
    });
    assert!(app.handle_modal_key(KeyCode::Enter).is_some());
}

#[test]
fn boot_once_asks_first_and_only_y_then_enter_queues_it() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('t'));
    match &app.modal {
        Some(Modal::Confirm { prompt, choice: None, .. }) => assert!(prompt.contains("'spare'") && prompt.contains("NEXT boot"), "{prompt}"),
        other => panic!("{other:?}"),
    }
    assert!(app.pending_mutations.is_empty(), "nothing is queued before the answer");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("runs after y then Enter");
    assert_eq!(action.call, MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: true });
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none(), "the running boot environment has nothing to boot once");
}

fn property_form(kind: FormKind, value: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form { title: "t".to_string(), fields: vec![FormField { label: "value".into(), value: value.into() }], focused: 0, description: None, kind });
    app
}

#[test]
fn shrinking_a_volume_and_enabling_a_pool_feature_confirm_but_ordinary_properties_do_not() {
    let mut app = property_form(FormKind::EditDatasetProperty { host: 0, dataset: "tank/vol".into(), key: "volsize".into() }, "1G");
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zfs set volsize=1G tank/vol") && prompt.contains("data past the new size"), "{prompt}");
    let mut app = property_form(FormKind::EditPoolProperty { host: 0, pool: "tank".into(), key: "feature@async_destroy".into() }, "enabled");
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool set feature@async_destroy=enabled tank") && prompt.contains("cannot be undone"), "{prompt}");
    let mut app = property_form(FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".into(), key: "compression".into() }, "lz4");
    assert!(app.handle_modal_key(KeyCode::Enter).is_some(), "an ordinary property still applies on Enter");
}

#[test]
fn the_confirmation_names_the_host_puts_the_command_on_its_own_line_and_names_whose_permissions() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![FormField { label: "pool".into(), value: "tank".into() }, FormField { label: "b".into(), value: "mirror".into() }],
        focused: 1,
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

// ---- job-save confirms ----

fn snap_job(keep: Option<u32>) -> crate::jobs::Job {
    crate::jobs::Job {
        id: "snap-local-tank-data".into(), host: "local".into(), kind: crate::jobs::KIND_SNAPSHOT.into(), pool: None, schedule: "0 * * * *".into(), catch_up: true, enabled: true,
        created_at: None, dataset: Some("tank/data".into()), recursive: false, prefix: Some("hourly".into()), keep, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false,
        repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
    }
}

fn repl_job(force: bool) -> crate::jobs::Job {
    crate::jobs::Job { kind: crate::jobs::KIND_REPLICATE.into(), keep: None, dest_host: Some("fbsd16".into()), dest_dataset: Some("backup/data".into()), force, ..snap_job(None) }
}

fn backup_job(retention: Option<&str>) -> crate::jobs::Job {
    crate::jobs::Job {
        kind: crate::jobs::KIND_BACKUP.into(), keep: None, repository: Some("/mnt/usb/restic".into()), password_file: Some("/root/pw".into()), retention: retention.map(Into::into), ..snap_job(None)
    }
}

fn snaps(dataset: &str, names: &[&str]) -> Vec<crate::zfs::snapshot::Snapshot> {
    names.iter().map(|n| crate::zfs::snapshot::Snapshot { dataset: dataset.into(), name: (*n).into(), used: 0 }).collect()
}

#[test]
fn turning_force_on_for_a_replication_asks_and_names_the_destination() {
    let prompt = job_save_prompt(Some(&repl_job(false)), &repl_job(true), None).expect("force newly on");
    assert!(prompt.contains("zfs receive -F") && prompt.contains("fbsd16:backup/data") && prompt.ends_with("(y/n)"), "{prompt}");
    assert!(job_save_prompt(None, &repl_job(true), None).is_some(), "a new forced job asks too");
}

#[test]
fn a_replication_save_that_does_not_arm_force_does_not_ask() {
    assert_eq!(job_save_prompt(None, &repl_job(false), None), None);
    assert_eq!(job_save_prompt(Some(&repl_job(true)), &repl_job(true), None), None, "force was already on and nothing changed");
    assert_eq!(job_save_prompt(Some(&repl_job(true)), &repl_job(false), None), None, "turning force off is safe");
    let mut moved = repl_job(true);
    moved.dest_dataset = Some("other/data".into());
    assert!(job_save_prompt(Some(&repl_job(true)), &moved, None).is_some(), "forcing into a different destination is new");
}

#[test]
fn a_snapshot_job_asks_when_keep_would_destroy_snapshots_and_says_how_many() {
    let list = snaps("tank/data", &["hourly-20261009T010000Z", "hourly-20261009T020000Z", "hourly-20261009T030000Z", "hourly-20261009T040000Z"]);
    let prompt = job_save_prompt(Some(&snap_job(Some(24))), &snap_job(Some(2)), Some(&list)).expect("4 exist, 1 more is taken, keep 2");
    assert!(prompt.contains("would destroy 3") && prompt.contains("hourly-<timestamp>") && prompt.contains("keep 2"), "{prompt}");
    assert!(job_save_prompt(None, &snap_job(Some(2)), Some(&list)).is_some(), "a new job on a dataset that has them asks too");
}

#[test]
fn a_snapshot_job_does_not_ask_when_nothing_would_be_destroyed_or_keep_is_not_lowered() {
    let list = snaps("tank/data", &["hourly-20261009T010000Z", "manual-1", "daily-20261009T010000Z"]);
    assert_eq!(job_save_prompt(None, &snap_job(Some(5)), Some(&list)), None, "within keep");
    assert_eq!(job_save_prompt(None, &snap_job(Some(1)), Some(&snaps("tank/data", &[])) ), None, "a clean dataset");
    assert_eq!(job_save_prompt(None, &snap_job(Some(1)), Some(&snaps("tank/other", &["hourly-20261009T010000Z", "hourly-20261009T020000Z"]))), None, "another dataset's snapshots");
    let many = snaps("tank/data", &["hourly-20261009T010000Z", "hourly-20261009T020000Z", "hourly-20261009T030000Z"]);
    assert_eq!(job_save_prompt(Some(&snap_job(Some(2))), &snap_job(Some(2)), Some(&many)), None, "unchanged keep");
    assert_eq!(job_save_prompt(Some(&snap_job(Some(2))), &snap_job(Some(10)), Some(&many)), None, "raising keep");
    assert_eq!(job_save_prompt(None, &snap_job(None), Some(&many)), None, "no keep: nothing is pruned");
}

#[test]
fn a_snapshot_job_asks_when_it_cannot_count_and_counts_a_recursive_tree() {
    let prompt = job_save_prompt(None, &snap_job(Some(3)), None).expect("unknown count");
    assert!(prompt.contains("unknown here"), "{prompt}");
    let mut tree = snaps("tank/data", &["hourly-20261009T010000Z", "hourly-20261009T020000Z"]);
    tree.extend(snaps("tank/data/sub", &["hourly-20261009T010000Z", "hourly-20261009T020000Z"]));
    let mut recursive = snap_job(Some(1));
    recursive.recursive = true;
    assert!(job_save_prompt(None, &recursive, Some(&tree)).unwrap().contains("destroy 4"), "2 per dataset, +1 each, keep 1");
    assert!(job_save_prompt(None, &snap_job(Some(1)), Some(&tree)).unwrap().contains("destroy 2"), "not recursive: only the dataset itself");
    let mut widened = snap_job(Some(1));
    widened.recursive = true;
    assert!(job_save_prompt(Some(&snap_job(Some(1))), &widened, Some(&tree)).is_some(), "turning recursion on widens what is pruned");
}

#[test]
fn a_backup_job_asks_when_retention_is_set_or_changed_and_names_the_repository() {
    let prompt = job_save_prompt(None, &backup_job(Some("7d 4w")), None).expect("new retention");
    assert!(prompt.contains("7d 4w") && prompt.contains("/mnt/usb/restic") && prompt.contains("restic forget --prune"), "{prompt}");
    assert!(job_save_prompt(Some(&backup_job(None)), &backup_job(Some("3l")), None).is_some());
    assert!(job_save_prompt(Some(&backup_job(Some("7d"))), &backup_job(Some("3d")), None).is_some());
}

#[test]
fn a_backup_job_does_not_ask_without_retention_or_when_it_is_unchanged() {
    assert_eq!(job_save_prompt(None, &backup_job(None), None), None);
    assert_eq!(job_save_prompt(Some(&backup_job(Some("7d"))), &backup_job(Some("7d")), None), None);
    assert_eq!(job_save_prompt(Some(&backup_job(Some("7d"))), &backup_job(None), None), None, "removing retention is safe");
}

#[test]
fn a_scrub_job_never_asks() {
    let scrub = crate::jobs::Job { kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), dataset: None, prefix: None, ..snap_job(None) };
    assert_eq!(job_save_prompt(None, &scrub, None), None);
}


fn submit_backup_form(values: [&str; 10]) -> (App, Option<PendingAction>) {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: backup_fields(values), focused: 9, description: None, kind: backup_kind(None, vec![]) });
    let out = app.handle_modal_key(KeyCode::Enter);
    (app, out)
}

#[test]
fn saving_a_backup_form_with_retention_asks_first_and_saves_only_after_y_then_enter() {
    let (mut app, out) = submit_backup_form(BACKUP_GOOD);
    assert_eq!(out, None, "Enter must not save a retention that deletes backups");
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("expected a confirmation, got {:?}", app.modal) };
    assert!(prompt.contains("7d 4w") && prompt.contains("/mnt/usb/restic"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter alone does nothing");
    assert_eq!(app.handle_modal_key(KeyCode::Char('y')), None);
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter saves");
    assert!(matches!(action.call, MutationCall::SaveJob { .. }), "{:?}", action.call);
}

#[test]
fn declining_the_job_save_confirm_saves_nothing() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    assert_eq!(app.handle_modal_key(KeyCode::Char('n')), None);
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "declined: nothing runs");
    let (values, focused) = open_form_values(&app).expect("declining returns to the form");
    assert_eq!(values, BACKUP_GOOD.map(String::from).to_vec());
    assert_eq!(focused, 9, "on the last field, so Enter asks again and Esc discards");
}

#[test]
fn esc_selects_no_and_enter_then_returns_to_the_form() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    assert_eq!(app.handle_modal_key(KeyCode::Esc), None);
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })), "Esc only selects");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(open_form_values(&app).is_some());
}

#[test]
fn the_returned_form_asks_again_on_enter_and_is_discarded_by_esc() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })), "Enter on the last field asks again");
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    assert_eq!(app.handle_modal_key(KeyCode::Esc), None);
    assert!(app.modal.is_none(), "Esc on the form discards it");
    assert!(app.confirm_restore.is_none());
}

#[test]
fn accepting_the_confirm_still_saves_and_leaves_nothing_to_restore() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter saves");
    assert!(matches!(action.call, MutationCall::SaveJob { .. }));
    assert!(app.modal.is_none() && app.confirm_restore.is_none());
}

#[test]
fn a_leftover_form_is_never_restored_for_a_different_confirm() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    let form = app.confirm_restore.take().expect("the submit kept the form").1;
    app.confirm_restore = Some(("a prompt from some earlier confirm".to_string(), form));
    app.handle_modal_key(KeyCode::Char('n'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none(), "the tag did not match this confirm");
}

#[test]
fn declining_a_confirm_that_did_not_come_from_a_form_just_closes_it() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = PendingAction { host: 0, section: Section::Jobs, call: MutationCall::DeleteJob { state_dir: std::path::PathBuf::from("/x"), id: "j".to_string() } };
    app.modal = Some(Modal::Confirm { choice: Some(false), prompt: "Delete?".to_string(), action });
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none());
}

/// The values and focused index of the open form, if one is open.
fn open_form_values(app: &App) -> Option<(Vec<String>, usize)> {
    match &app.modal {
        Some(Modal::Form { fields, focused, .. }) => Some((fields.iter().map(|f| f.value.clone()).collect(), *focused)),
        _ => None,
    }
}

#[test]
fn saving_a_backup_form_without_retention_saves_at_once() {
    let mut values = BACKUP_GOOD;
    values[7] = "";
    let (app, out) = submit_backup_form(values);
    assert!(matches!(out.map(|a| a.call), Some(MutationCall::SaveJob { .. })));
    assert!(app.modal.is_none());
}


#[test]
fn the_add_host_probe_runs_true_without_sudo() {
    let runner = crate::exec::test_support::FakeRunner::new("");
    probe_reachable(&runner, "fbsd@h").expect("a reachable host passes");
    assert_eq!(*runner.unprivileged_calls.lock().unwrap(), vec![("true".to_string(), Vec::<String>::new())], "sudo -n true is refused by the documented sudoers entry");
}

#[test]
fn the_add_host_probe_reports_a_rejected_connection_with_its_message() {
    let runner = crate::exec::test_support::FakeRunner::failing("Permission denied (publickey).", 255);
    let err = probe_reachable(&runner, "fbsd@h").unwrap_err().to_string();
    assert!(err.contains("ssh fbsd@h") && err.contains("Permission denied"), "{err}");
}

fn backup_form_app(values: [&str; 10]) -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: backup_fields(values), focused: 5, description: None, kind: backup_kind(None, vec![]) });
    app
}

#[test]
fn f5_in_the_backup_form_queues_one_check_for_the_forms_host_and_marks_it_checking() {
    let mut app = backup_form_app(BACKUP_GOOD);
    assert_eq!(app.handle_modal_key(KeyCode::F(5)), None);
    assert_eq!(app.pending_preflight, vec![PendingPreflight { host: 0, host_name: "local".into(), repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into() }]);
    assert_eq!(app.backup_preflight, Some(PreflightCheck { host: 0, repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into(), result: None, file_note: None }));
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "the form stays open");
}

#[test]
fn f5_twice_while_checking_queues_only_one_check() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    app.handle_modal_key(KeyCode::F(5));
    assert_eq!(app.pending_preflight.len(), 1);
}

#[test]
fn f5_after_a_result_checks_again() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    app.pending_preflight.clear();
    app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok, None)).unwrap();
    app.drain_worker_messages();
    app.handle_modal_key(KeyCode::F(5));
    assert_eq!(app.pending_preflight.len(), 1);
    assert_eq!(app.backup_preflight.as_ref().unwrap().result, None, "a new check clears the old answer");
}

#[test]
fn f5_runs_nothing_for_bad_fields_and_says_why() {
    for (values, want) in [
        (["nohost", "tank/data", "", "hourly", "", "/mnt/usb/restic", "/root/pw", "", "30 3 * * *", "y"], "Unknown host 'nohost'"),
        (["local", "tank/data", "", "hourly", "", "", "/root/pw", "", "30 3 * * *", "y"], "repository"),
        (["local", "tank/data", "", "hourly", "", "/mnt/usb/restic", "pw", "", "30 3 * * *", "y"], "password file"),
        (["local", "tank/data", "", "hourly", "", "rest:https://user:secret@host/r", "/root/pw", "", "30 3 * * *", "y"], "repository"),
    ] {
        let mut app = backup_form_app(values);
        app.handle_modal_key(KeyCode::F(5));
        assert!(app.pending_preflight.is_empty(), "{values:?}");
        assert!(app.backup_preflight.is_none(), "{values:?}");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains(want)), "{values:?}: {:?}", app.status_message);
    }
}

#[test]
fn f5_does_nothing_in_other_forms() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: vec![FormField { label: "x".into(), value: "/r".into() }], focused: 0, description: None, kind: FormKind::CreateDataset { host: 0 } });
    app.handle_modal_key(KeyCode::F(5));
    assert!(app.pending_preflight.is_empty() && app.backup_preflight.is_none());
}

#[test]
fn a_result_for_other_values_is_discarded_and_a_matching_one_is_kept() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    let stale = crate::restic::Preflight::Problem("old".into());
    app.tx.send(WorkerMsg::Preflight(0, "/some/other/repo".into(), "/root/pw".into(), stale, None)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.backup_preflight.as_ref().unwrap().result, None);
    app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok, None)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.backup_preflight.as_ref().unwrap().result, Some(crate::restic::Preflight::Ok));
}

#[test]
fn a_check_result_carries_its_password_file_note_and_a_new_check_clears_it() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    app.pending_preflight.clear();
    let note = "the password file can be read or changed by other users (mode 644): run `chmod 600 /root/pw`".to_string();
    app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok, Some(note.clone()))).unwrap();
    app.drain_worker_messages();
    let check = app.backup_preflight.as_ref().unwrap();
    assert_eq!((check.result.clone(), check.file_note.clone()), (Some(crate::restic::Preflight::Ok), Some(note)));
    app.handle_modal_key(KeyCode::F(5));
    let check = app.backup_preflight.as_ref().unwrap();
    assert_eq!((check.result.clone(), check.file_note.clone()), (None, None), "a new check starts without the old advice");
}

#[test]
fn opening_a_backup_form_forgets_an_earlier_check() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.backup_preflight = Some(PreflightCheck { host: 0, repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into(), result: Some(crate::restic::Preflight::Ok), file_note: None });
    app.handle_key(KeyCode::Char('B'));
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert!(app.backup_preflight.is_none());
}

fn keep_form_app(keep: &str) -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    let mut values: Vec<&str> = GOOD.to_vec();
    values.push(keep);
    let fields = values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect();
    app.modal = Some(Modal::Form { title: "x".into(), fields, focused: 8, description: None, kind: repl_kind(None, vec![]) });
    app
}

fn target_snaps(n: u64) -> Vec<crate::zfs::replication::SnapInfo> {
    (1..=n).map(|i| crate::zfs::replication::SnapInfo { name: format!("hourly-20261006T{i:02}0000Z"), guid: 100 + i, createtxg: i }).collect()
}

fn answer_prune_check(app: &mut App, listing: crate::error::Result<Vec<crate::zfs::replication::SnapInfo>>) {
    app.tx.send(WorkerMsg::PrunePreview(1, "backup/data".into(), listing)).unwrap();
    app.drain_worker_messages();
}

#[test]
fn keep_on_target_must_be_a_whole_number_of_at_least_one() {
    for bad in ["0", "x", "-1", "2.5"] {
        let mut fields = repl_fields(GOOD).into_iter().collect::<Vec<_>>();
        fields.push(FormField { label: String::new(), value: bad.into() });
        let err = build_replicate_job_action(&repl_kind(None, vec![]), &fields, std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err();
        assert!(err.contains("Keep on target"), "{bad}: {err}");
    }
}

#[test]
fn keep_on_target_on_a_file_destination_is_refused() {
    let mut app = keep_form_app("3");
    let Some(Modal::Form { fields, .. }) = app.modal.as_mut() else { panic!() };
    fields[3].value = "file".into();
    fields[4].value = "/mnt/usb/streams".into();
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.pending_prune_check.is_empty());
    assert!(app.status_message.clone().unwrap_or_default().contains("Keep on target"));
}

#[test]
fn a_blank_keep_on_target_saves_at_once_with_no_check() {
    let mut app = keep_form_app("");
    let action = app.handle_modal_key(KeyCode::Enter).expect("saved at once, as before");
    assert!(app.pending_prune_check.is_empty() && app.prune_check.is_none());
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!(job.dest_keep, None);
}

#[test]
fn a_keep_on_target_holds_the_save_and_queues_a_listing_of_the_destination() {
    let mut app = keep_form_app("3");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert_eq!(app.pending_prune_check, vec![PendingPruneCheck { host: 1, host_name: "fbsd16".into(), dest_dataset: "backup/data".into() }]);
    assert!(app.modal.is_none() && app.prune_check.is_some());
    assert!(app.status_message.clone().unwrap_or_default().contains("fbsd16"));
}

#[test]
fn nothing_to_destroy_goes_on_to_the_usual_save_path() {
    let mut app = keep_form_app("4");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Ok(target_snaps(3)));
    assert!(app.modal.is_none(), "no destroy confirmation: {:?}", app.modal);
    assert_eq!(app.pending_mutations.len(), 1, "the save goes through");
}

#[test]
fn a_selection_opens_a_confirm_naming_count_oldest_newest_and_kept_and_declining_returns_the_form() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Ok(target_snaps(6)));
    let Some(Modal::Confirm { prompt, choice, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    assert_eq!(choice, None, "a second keystroke is needed");
    assert!(prompt.contains("destroy 4 snapshots on fbsd16 backup/data"), "{prompt}");
    assert!(prompt.contains("oldest hourly-20261006T010000Z") && prompt.contains("newest destroyed hourly-20261006T040000Z") && prompt.contains("3 kept"), "{prompt}");
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    let Some(Modal::Form { fields, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(fields[8].value, "3", "the form comes back with its values");
    assert!(app.pending_mutations.is_empty());
}

#[test]
fn confirming_the_selection_saves_the_job_with_its_dest_keep() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Ok(target_snaps(6)));
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("saved");
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!(job.dest_keep, Some(3));
}

#[test]
fn a_failed_listing_refuses_the_save_with_the_reason_and_keeps_the_form() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Err(crate::error::AppError::Parse("boom".into())));
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert!(app.pending_mutations.is_empty());
    assert!(app.status_message.clone().unwrap_or_default().contains("boom"));
}

#[test]
fn an_answer_for_another_dataset_is_dropped() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    app.tx.send(WorkerMsg::PrunePreview(1, "other/data".into(), Ok(target_snaps(6)))).unwrap();
    app.drain_worker_messages();
    assert!(app.prune_check.is_some() && app.modal.is_none());
}

#[test]
fn a_target_that_is_exactly_full_still_asks_because_the_next_run_adds_one_and_destroys_the_oldest() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Ok(target_snaps(3)));
    let Some(Modal::Confirm { prompt, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("destroy 1 snapshot on fbsd16") && prompt.contains("oldest hourly-20261006T010000Z"), "{prompt}");
}

#[test]
fn a_forced_replication_with_a_selection_shows_both_warnings_and_one_confirm_saves() {
    let mut app = keep_form_app("3");
    let Some(Modal::Form { fields, .. }) = app.modal.as_mut() else { panic!() };
    fields[5].value = "y".into();
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Ok(target_snaps(6)));
    let Some(Modal::Confirm { prompt, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("destroy 4 snapshots") && prompt.contains("zfs receive -F"), "{prompt}");
    app.handle_modal_key(KeyCode::Char('y'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some());
}

const MIRROR_GOOD: [&str; 9] = ["local", "tank/data", "", "hourly", "", "aws:bkt/photos", "", "30 3 * * *", "y"];

fn mirror_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::MirrorJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn mirror_fields(values: [&str; 9]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: "x".to_string(), value: (*v).to_string() }).collect()
}

fn mirror_build(values: [&str; 9], jobs: Vec<crate::jobs::Job>) -> std::result::Result<PendingAction, String> {
    build_mirror_job_action(&mirror_kind(None, jobs), &mirror_fields(values), std::path::Path::new("/state"), chrono::Utc::now())
}

#[test]
fn big_c_in_jobs_opens_a_blank_cloud_mirror_form_with_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('C'));
    assert_eq!(form_values(&app), ["local", "", "", "new", "", "", "", "30 3 * * *", "y"]);
}

#[test]
fn a_valid_mirror_form_builds_a_save_with_the_remote_and_the_speed_limit() {
    let mut values = MIRROR_GOOD;
    values[6] = "5M";
    let action = mirror_build(values, vec![]).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.kind.as_str(), job.remote.as_deref(), job.bwlimit.as_deref(), job.dataset.as_deref()), (crate::jobs::KIND_MIRROR, Some("aws:bkt/photos"), Some("5M"), Some("tank/data")));
    assert_eq!(job.id, "mirror-local-tank-data");
    let blank = mirror_build(MIRROR_GOOD, vec![]).unwrap();
    let MutationCall::SaveJob { job, .. } = blank.call else { panic!() };
    assert_eq!(job.bwlimit, None, "a blank limit saves none");
}

#[test]
fn a_bad_remote_speed_limit_or_clash_is_refused_with_a_message_naming_the_field() {
    for evil in ["", "nocolon", ":s3,access_key_id=X:b/p", "-x:b"] {
        let mut values = MIRROR_GOOD;
        values[5] = evil;
        assert!(mirror_build(values, vec![]).unwrap_err().starts_with("Remote must look like"), "{evil:?}");
    }
    let mut values = MIRROR_GOOD;
    values[6] = "5MB";
    assert!(mirror_build(values, vec![]).unwrap_err().starts_with("Speed limit"));
    let other = crate::jobs::Job::test_mirror("other", "tank/other", "hourly", "aws:bkt");
    let err = mirror_build(MIRROR_GOOD, vec![other]).unwrap_err();
    assert!(err.contains("'other'") && err.contains("remote overlaps"), "{err}");
}

#[test]
fn enter_on_a_mirror_job_edits_it_in_the_mirror_form() {
    let mut m = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos");
    m.bwlimit = Some("5M".into());
    let (mut app, _dir) = app_with_jobs(&[m]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["local", "tank/data", "", "hourly", "", "aws:bkt/photos", "5M", "30 3 * * *", "y"]);
}

#[test]
fn saving_a_mirror_job_asks_first_naming_the_remote_and_saying_nothing_in_old_is_deleted() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: mirror_fields(MIRROR_GOOD), focused: 8, description: None, kind: mirror_kind(None, vec![]) });
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    let Some(Modal::Confirm { prompt, choice, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    assert_eq!(choice, None);
    assert!(prompt.contains("aws:bkt/photos/current") && prompt.contains("old/") && prompt.contains("never deletes"), "{prompt}");
}

#[test]
fn changing_what_a_mirror_reads_asks_again_and_says_old_files_move_to_old() {
    let before = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos");
    let mut same = before.clone();
    assert_eq!(job_save_prompt(Some(&before), &same, None), None, "nothing relevant changed");
    same.bwlimit = Some("5M".into());
    assert_eq!(job_save_prompt(Some(&before), &same, None), None, "a speed limit is harmless");
    for change in [&(|j: &mut crate::jobs::Job| j.path = Some("docs".into())) as &dyn Fn(&mut crate::jobs::Job), &|j| j.source = Some("@old".into())] {
        let mut after = before.clone();
        change(&mut after);
        let prompt = job_save_prompt(Some(&before), &after, None).expect("a different path or source asks");
        assert!(prompt.contains("moved to old/"), "{prompt}");
    }
    let fresh = job_save_prompt(None, &before, None).unwrap();
    assert!(!fresh.contains("previous one"), "a new job has no previous source: {fresh}");
}

#[test]
fn opening_a_mirror_form_forgets_an_earlier_check() {
    let old = Some(PreflightCheck { host: 0, repository: "aws:bkt/photos".into(), password_file: String::new(), result: None, file_note: None });
    let m = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos");
    let (mut app, _dir) = app_with_jobs(&[m]);
    app.refresh_jobs();
    app.backup_preflight = old.clone();
    app.handle_key(KeyCode::Char('C'));
    assert_eq!(app.backup_preflight, None, "C");
    app.modal = None;
    app.backup_preflight = old;
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert_eq!(app.backup_preflight, None, "Enter on a mirror job");
}

#[test]
fn f2_on_the_mirror_forms_source_lists_the_datasets_snapshots() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: mirror_fields(MIRROR_GOOD), focused: 3, description: None, kind: mirror_kind(None, vec![]) });
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.is_some(), "the Source field has a picker");
}

fn mirror_form_app(values: [&str; 9]) -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: mirror_fields(values), focused: 5, description: None, kind: mirror_kind(None, vec![]) });
    app
}

#[test]
fn f5_in_the_mirror_form_queues_one_check_of_the_remote_on_the_forms_host() {
    let mut app = mirror_form_app(MIRROR_GOOD);
    assert_eq!(app.handle_modal_key(KeyCode::F(5)), None);
    assert_eq!(app.pending_preflight, vec![PendingPreflight { host: 0, host_name: "local".into(), repository: "aws:bkt/photos".into(), password_file: String::new() }]);
    assert_eq!(app.backup_preflight, Some(PreflightCheck { host: 0, repository: "aws:bkt/photos".into(), password_file: String::new(), result: None, file_note: None }));
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "the form stays open");
    app.handle_modal_key(KeyCode::F(5));
    assert_eq!(app.pending_preflight.len(), 1, "a second press while checking queues nothing");
}

#[test]
fn f5_in_the_mirror_form_runs_nothing_for_a_bad_remote_or_host_and_says_why() {
    for (values, want) in [
        (["nohost", "tank/data", "", "hourly", "", "aws:bkt", "", "30 3 * * *", "y"], "Unknown host 'nohost'"),
        (["local", "tank/data", "", "hourly", "", "", "", "30 3 * * *", "y"], "enter the remote"),
        (["local", "tank/data", "", "hourly", "", ":s3,access_key_id=X:b", "", "30 3 * * *", "y"], "enter the remote"),
    ] {
        let mut app = mirror_form_app(values);
        app.handle_modal_key(KeyCode::F(5));
        assert!(app.pending_preflight.is_empty() && app.backup_preflight.is_none(), "{values:?}");
        assert!(app.status_message.clone().unwrap_or_default().contains(want), "{:?}", app.status_message);
    }
}

// ---- Cloud ----

fn cloud_mirror(id: &str, host: &str, remote: &str) -> crate::jobs::Job {
    let mut j = crate::jobs::Job::test_mirror(id, "tank/data", "hourly", remote);
    j.host = host.into();
    j
}

fn cloud_app(jobs: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let (mut app, dir) = app_with_jobs(jobs);
    app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Cloud).unwrap();
    (app, dir)
}

#[test]
fn refreshing_cloud_asks_the_hub_and_each_host_with_a_cloud_job_once() {
    let (mut app, _dir) = cloud_app(&[cloud_mirror("m1", "fbsd16", "aws:bkt/p"), cloud_mirror("m2", "fbsd16", "aws:bkt/q"), scrub_job("s")]);
    app.refresh_cloud();
    let asked: Vec<(usize, String)> = app.pending_cloud.iter().map(|p| (p.host, p.host_name.clone())).collect();
    assert_eq!(asked, [(0, "local".to_string()), (1, "fbsd16".to_string())]);
    assert!(app.pending_cloud.iter().all(|p| p.work == CloudWork::Remotes));
    app.refresh_cloud();
    assert_eq!(app.pending_cloud.len(), 2, "an answer still awaited is not asked for again");
}

#[test]
fn the_answer_for_a_host_fills_the_table_and_a_failure_is_kept_as_its_reason() {
    let (mut app, _dir) = cloud_app(&[cloud_mirror("m1", "local", "b2:zfsctl/photos")]);
    app.refresh_cloud();
    app.tx.send(WorkerMsg::CloudRemotes("local".into(), Ok(vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]))).unwrap();
    app.drain_worker_messages();
    let rows = app.cloud_rows();
    assert_eq!(rows.iter().map(|r| (r.name.as_str(), r.job_ids.len())).collect::<Vec<_>>(), [("b2", 1), ("t1", 0)]);
    app.refresh_cloud();
    assert_eq!(app.pending_cloud.len(), 2, "the first ask was answered, so the host can be asked again");
    app.tx.send(WorkerMsg::CloudRemotes("local".into(), Err("rclone is not installed on local".into()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.cloud_errors.get("local").map(String::as_str), Some("rclone is not installed on local"));
    assert_eq!(app.cloud_rows().len(), 2, "the last good list stays on screen");
}

#[test]
fn arrow_keys_move_through_the_remotes_and_s_measures_only_mirror_jobs_of_the_selected_one() {
    let (mut app, _dir) = cloud_app(&[cloud_mirror("m1", "local", "b2:zfsctl/photos"), crate::jobs::Job::test_backup("b1", "tank/data", "hourly", "rclone:zfsctl-aws:bkt/x", "/pw")]);
    app.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into()), ("zfsctl-aws".into(), "s3".into())]);
    app.refresh_jobs();
    app.pending_cloud.clear();
    app.handle_key(KeyCode::Down);
    assert_eq!(app.cloud_table.selected(), Some(0));
    app.handle_key(KeyCode::Char('s'));
    assert_eq!(app.pending_cloud, vec![PendingCloud { host: 0, host_name: "local".into(), work: CloudWork::Size { job_id: "m1".into(), remote: "b2:zfsctl/photos".into() } }]);
    app.handle_key(KeyCode::Char('s'));
    assert_eq!(app.pending_cloud.len(), 1, "a measurement still running is not started twice");
    app.handle_key(KeyCode::Down);
    app.pending_cloud.clear();
    app.handle_key(KeyCode::Char('s'));
    assert!(app.pending_cloud.is_empty(), "a restic repository is not measured");
    assert!(app.status_message.as_deref().unwrap_or("").contains("Nothing to measure"), "{:?}", app.status_message);
    app.tx.send(WorkerMsg::CloudSize("m1".into(), crate::ui::cloud::Size { current: Some((4, 2000)), old: None, at: chrono::Utc::now(), problem: None })).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.cloud_sizes.get("m1").and_then(|s| s.current), Some((4, 2000)));
}

#[test]
fn f5_in_cloud_checks_the_remote_of_the_selected_remotes_mirror_job() {
    let (mut app, _dir) = cloud_app(&[cloud_mirror("m1", "local", "b2:zfsctl/photos")]);
    app.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::F(5));
    assert_eq!(app.pending_preflight, vec![PendingPreflight { host: 0, host_name: "local".into(), repository: "b2:zfsctl/photos".into(), password_file: String::new() }]);
    app.pending_preflight.clear();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::F(5));
    assert!(app.pending_preflight.is_empty() && app.status_message.as_deref().unwrap_or("").contains("Nothing to check"), "{:?}", app.status_message);
}

#[test]
fn big_c_in_cloud_opens_the_mirror_form_like_jobs_does() {
    let (mut app, _dir) = cloud_app(&[]);
    app.handle_key(KeyCode::Char('C'));
    assert!(matches!(app.modal, Some(Modal::Form { kind: FormKind::MirrorJob { .. }, .. })), "{:?}", app.modal);
}

#[test]
fn the_cloud_section_asks_for_its_remotes_when_first_opened_and_then_once_a_minute() {
    let (mut app, _dir) = cloud_app(&[]);
    let t0 = Instant::now();
    app.refresh_cloud_if_stale(t0);
    assert_eq!(app.pending_cloud.len(), 1, "opened for the first time: the hub is asked");
    app.pending_cloud.clear();
    app.tx.send(WorkerMsg::CloudRemotes("local".into(), Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(30));
    assert!(app.pending_cloud.is_empty(), "not again within a minute");
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(61));
    assert_eq!(app.pending_cloud.len(), 1);
    app.pending_cloud.clear();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(200));
    assert!(app.pending_cloud.is_empty(), "other sections do not ask");
}

#[test]
fn the_plan_caps_are_read_from_config_toml_each_time_the_remotes_are_asked() {
    let (mut app, dir) = cloud_app(&[]);
    app.config_path = dir.path().join("config.toml");
    app.refresh_cloud();
    assert!(app.cloud_caps.is_empty(), "no config file: no caps");
    std::fs::write(&app.config_path, "[cloud.caps]\nb2 = \"10G\"\nzfsctl-aws = \"ten gigs\"\n").unwrap();
    app.refresh_cloud();
    assert_eq!(app.cloud_caps.get("b2"), Some(&Ok(10_000_000_000)));
    assert_eq!(app.cloud_caps.get("zfsctl-aws"), Some(&Err("ten gigs".to_string())), "a bad size is kept as text, to be reported");
    std::fs::write(&app.config_path, "this is not toml [").unwrap();
    app.refresh_cloud();
    assert!(app.cloud_caps.is_empty(), "an unreadable config shows no caps rather than stale ones");
}

// ---- host resources (Dashboard) ----

fn reading(free_gib: u64) -> crate::zfs::host_resources::Resources {
    crate::zfs::host_resources::Resources { cores: 4, physical: 20 << 30, free: free_gib << 30, inactive: 0, arc: 0, load: None, cpu_ticks: None, swap: None }
}

#[test]
fn the_last_two_readings_of_a_host_are_kept_newest_first_for_the_cpu_difference() {
    let mut app = probes_app();
    assert!(app.host_resources("fbsd16").is_none());
    for free in [1, 2, 3] {
        app.tx.send(WorkerMsg::HostResources("fbsd16".into(), Some(reading(free)), None)).unwrap();
    }
    app.drain_worker_messages();
    let (now, before) = app.host_resources("fbsd16").expect("readings");
    assert_eq!((now.free >> 30, before.map(|b| b.free >> 30)), (3, Some(2)), "only the last two are kept");
    assert_eq!(app.host_probes["fbsd16"].resources.len(), 2);
}

#[test]
fn the_utc_offset_is_kept_once_asked_and_a_reading_without_it_does_not_erase_it() {
    let mut app = probes_app();
    app.tx.send(WorkerMsg::HostResources("local".into(), Some(reading(1)), Some(7200))).unwrap();
    app.tx.send(WorkerMsg::HostResources("local".into(), Some(reading(2)), None)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_probes["local"].utc_offset_secs, Some(7200));
}

#[test]
fn a_probe_that_did_not_reach_the_host_clears_its_readings_so_stale_numbers_do_not_stay() {
    let mut app = probes_app();
    app.tx.send(WorkerMsg::HostResources("fbsd16".into(), Some(reading(1)), None)).unwrap();
    app.drain_worker_messages();
    assert!(app.host_resources("fbsd16").is_some());
    app.tx.send(WorkerMsg::HostHealth("fbsd16".into(), crate::zfs::host_health::HostProbe::Disconnected)).unwrap();
    app.drain_worker_messages();
    assert!(app.host_resources("fbsd16").is_none(), "an unreachable host has no current readings");
    app.tx.send(WorkerMsg::HostResources("fbsd16".into(), Some(reading(1)), None)).unwrap();
    app.tx.send(WorkerMsg::HostResources("fbsd16".into(), None, None)).unwrap();
    app.drain_worker_messages();
    assert!(app.host_resources("fbsd16").is_none(), "a failed reading clears too");
    app.tx.send(WorkerMsg::HostResources("ghost".into(), Some(reading(1)), Some(0))).unwrap();
    app.drain_worker_messages();
    assert!(!app.host_probes.contains_key("ghost"), "a host that no longer exists is ignored");
}

#[test]
fn a_probe_reads_the_resources_and_asks_for_the_utc_offset_only_until_it_is_known() {
    const STATUS: &str = "  pool: scratch\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tscratch  ONLINE  0  0  0\n";
    const SYSCTL: &str = "4\n21474836480\n4096\n524288\n786432\n10737418240\n{ 0.05 0.12 0.08 }\n1000 0 200 10 8790\n";
    let runner = std::sync::Arc::new(
        FakeRunner::new("").route("zpool", "list", "scratch\t100\t10\t90\t1\tONLINE\n").route("zpool", "status", STATUS).route("sysctl", "-n", SYSCTL).route("date", "+%z", "+0200\n"),
    );
    let mut app = probes_app();
    for round in 1..=2 {
        app.start_host_probe(0, runner.clone());
        wait_until(&mut app, |a| a.host_probes["local"].resources.len() == round);
    }
    assert_eq!(app.host_probes["local"].resources.len(), 2);
    assert_eq!(app.host_probes["local"].utc_offset_secs, Some(7200));
    let calls = runner.unprivileged_calls.lock().unwrap().clone();
    let count = |cmd: &str| calls.iter().filter(|(c, _)| c == cmd).count();
    assert_eq!(count("date"), 1, "the offset is read once per host, not every probe");
    assert_eq!(count("swapinfo"), 2, "memory, CPU and swap are read on every probe");
    assert_eq!(calls.iter().filter(|(c, a)| c == "sysctl" && a.contains(&"hw.ncpu".to_string())).count(), 2);
}

#[test]
fn the_daemons_probe_makes_no_resource_calls() {
    let runner = FakeRunner::new("").route("zpool", "list", "scratch\t100\t10\t90\t1\tONLINE\n");
    let _ = crate::zfs::host_health::probe_host_guarded(&runner, false);
    let calls = runner.unprivileged_calls.lock().unwrap().clone();
    assert!(!calls.iter().any(|(c, _)| c == "swapinfo" || c == "date"), "{calls:?}");
    assert!(!calls.iter().any(|(_, a)| a.contains(&"hw.ncpu".to_string())), "{calls:?}");
}

// ---- Dashboard ----

fn dash_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.config_path = dir.path().join("config.toml");
    app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
    (app, dir)
}

fn reached_probe(capacity: u8) -> crate::zfs::host_health::HostProbe {
    let pool = crate::zfs::host_health::PoolProbe { name: "tank".into(), health: crate::zfs::pool::PoolHealth::Online, capacity_percent: capacity, frag_percent: None, errors: Some(0), scan: Some(crate::zfs::host_health::ScanSummary::Never), last_scrub: None };
    crate::zfs::host_health::HostProbe::Reached { rtt_ms: 2, pools: vec![pool], kind: None, os: None }
}

#[test]
fn the_program_opens_on_the_dashboard_which_is_the_first_sidebar_section() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.sections.first(), Some(&Section::Dashboard));
    assert_eq!(app.current_section(), Section::Hosts, "a bare App starts on Hosts; the launch calls open_dashboard");
    app.open_dashboard();
    assert_eq!(app.current_section(), Section::Dashboard);
}

#[test]
fn the_dashboard_is_built_from_the_probes_the_readings_and_the_thresholds_in_config() {
    let (mut app, dir) = dash_app();
    app.host_probes.insert("local".into(), HostProbeState { last: Some(reached_probe(85)), resources: vec![reading(10), reading(10)], utc_offset_secs: Some(0), ..HostProbeState::default() });
    app.refresh_dashboard();
    let d = app.dashboard();
    assert_eq!(d.hosts.len(), 2, "every configured host has a row");
    assert_eq!(d.hosts[0].space.level, crate::ui::dashboard::Level::Watch);
    assert_eq!(d.hosts[1].state.level, crate::ui::dashboard::Level::Unknown, "fbsd16 has not answered yet");
    assert_eq!(d.verdict.text, "checking... 1 issue, 2 to watch", "known problems show while a host is still unanswered");
    assert_eq!(d.hosts[0].memory.level, crate::ui::dashboard::Level::Fine, "10 of 20 GiB free is fine with the defaults");
    std::fs::write(dir.path().join("config.toml"), "[dashboard]\nmemory_warn = 1\nmemory_crit = 2\n").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard().hosts[0].memory.level, crate::ui::dashboard::Level::Issue, "the [dashboard] thresholds are read when the Dashboard refreshes");
    std::fs::write(dir.path().join("config.toml"), "not toml [").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard().hosts[0].memory.level, crate::ui::dashboard::Level::Fine, "an unreadable config means the defaults");
}

#[test]
fn up_down_and_enter_on_the_dashboard_only_navigate() {
    let (mut app, _dir) = dash_app();
    app.refresh_jobs();
    let items = app.dashboard().attention;
    assert!(items.iter().any(|i| i.subject == "daemon"), "no heartbeat: the daemon is the one problem: {items:?}");
    app.handle_key(KeyCode::Down);
    assert_eq!(app.dashboard_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Jobs, "the daemon line belongs to Jobs");
    assert!(app.pending_mutations.is_empty() && app.modal.is_none() && app.picker.is_none(), "Enter changed nothing but the section");
}

#[test]
fn enter_on_a_host_line_selects_that_host_in_hosts() {
    let (mut app, _dir) = dash_app();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(95)), ..HostProbeState::default() });
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.subject == "fbsd16").expect("fbsd16 has a line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Hosts);
    assert_eq!(app.selected_host, 1, "the host the line is about, not the one that was selected before");
}

#[test]
fn a_probe_that_gave_up_leaves_no_old_readings_on_the_dashboard() {
    let (mut app, _dir) = dash_app();
    app.host_probes.insert("local".into(), HostProbeState { last: Some(reached_probe(85)), resources: vec![reading(10), reading(10)], utc_offset_secs: Some(0), in_flight: true, started_at: Some(std::time::Instant::now() - Duration::from_secs(120)), ..HostProbeState::default() });
    let d = app.dashboard();
    assert_eq!(d.hosts[0].space.level, crate::ui::dashboard::Level::Unknown);
    assert_eq!(d.hosts[0].memory.level, crate::ui::dashboard::Level::Unknown);
    assert!(d.hosts[0].state.text.starts_with("no answer"), "{:?}", d.hosts[0].state);
}

#[test]
fn enter_with_nothing_selected_and_keys_that_change_things_do_nothing_on_the_dashboard() {
    let (mut app, _dir) = dash_app();
    app.handle_key(KeyCode::Enter);
    for key in ['d', 'n', 'N', 'B', 's', ' '] {
        app.handle_key(KeyCode::Char(key));
    }
    assert_eq!(app.current_section(), Section::Dashboard);
    assert!(app.pending_mutations.is_empty() && app.modal.is_none(), "{:?}", app.modal);
}

#[test]
fn r_on_the_dashboard_reloads_the_jobs_and_asks_the_hub_for_its_remotes() {
    let (mut app, _dir) = dash_app();
    app.refresh_visible(Arc::new(FakeRunner::new("")));
    assert!(app.jobs_view.loaded, "the jobs are reloaded");
    assert_eq!(app.pending_cloud.len(), 1, "the hub is asked which rclone remotes it has");
}

#[test]
fn while_the_dashboard_is_open_the_jobs_alerts_and_cloud_list_keep_fresh() {
    let (mut app, _dir) = dash_app();
    let t0 = Instant::now();
    app.refresh_jobs_if_stale(t0);
    assert!(app.jobs_view.loaded, "jobs load on the first call");
    app.refresh_alerts_if_stale(t0);
    assert!(app.alerts_view.loaded, "alerts load too, they feed the Alerts line");
    app.refresh_cloud_if_stale(t0);
    assert_eq!(app.pending_cloud.len(), 1);
    app.pending_cloud.clear();
    app.tx.send(WorkerMsg::CloudRemotes("local".into(), Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(30));
    assert!(app.pending_cloud.is_empty(), "not again within a minute");
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(61));
    assert_eq!(app.pending_cloud.len(), 1);
}

fn settings_app(config: &str) -> (App, tempfile::TempDir) {
    let (mut app, dir) = alerts_app(config);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
    app.refresh_settings();
    (app, dir)
}

fn select_setting(app: &mut App, label: &str) {
    let at = app.settings_view.rows.iter().position(|r| r.label == label).unwrap_or_else(|| panic!("no row {label}"));
    app.settings_table.select(Some(at));
}

/// Enter through every field of the open form, then answer the confirmation with `y` and Enter: the save to run.
fn submit_setting(app: &mut App) -> Option<PendingAction> {
    let fields = match &app.modal {
        Some(Modal::Form { fields, .. }) => fields.len(),
        other => panic!("expected a form, got {other:?}"),
    };
    for _ in 0..fields {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    if !matches!(app.modal, Some(Modal::Confirm { .. })) {
        return None;
    }
    app.handle_modal_key(KeyCode::Char('y'));
    app.handle_modal_key(KeyCode::Enter)
}

fn set_setting_fields(app: &mut App, values: &[&str]) {
    let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
    for (f, v) in fields.iter_mut().zip(values) {
        f.value = (*v).to_string();
    }
}

fn run_save(action: &PendingAction) {
    action.call.run(&crate::exec::test_support::FakeRunner::new("")).unwrap();
}

#[test]
fn the_settings_section_loads_its_rows_from_the_file() {
    let (app, _dir) = settings_app("[dashboard]\ncpu_warn = 80\n");
    assert_eq!(app.current_section(), Section::Settings);
    assert!(app.settings_view.loaded);
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "CPU warn").unwrap().value, "80 %");
}

#[test]
fn enter_on_a_setting_opens_its_form_with_the_saved_values() {
    let (mut app, _dir) = settings_app("[dashboard]\nmemory_warn = 80\n");
    select_setting(&mut app, "Memory warn / crit");
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["80", ""]);
}

#[test]
fn a_valid_setting_is_confirmed_then_saved_keeping_the_rest_of_the_file() {
    let (mut app, dir) = settings_app("# keep\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n");
    select_setting(&mut app, "Memory warn / crit");
    app.handle_key(KeyCode::Enter);
    set_setting_fields(&mut app, &["85", "95"]);
    for _ in 0..2 {
        app.handle_modal_key(KeyCode::Enter);
    }
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("Memory warn / crit: 85 / 95 %"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter alone does nothing");
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter applies");
    assert_eq!(action.section, Section::Settings);
    run_save(&action);
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(saved.starts_with("# keep\n[[hosts]]") && saved.contains("memory_warn = 85"), "{saved}");
    assert!(dir.path().join("config.toml.bak").exists());
    app.refresh_settings();
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "Memory warn / crit").unwrap().value, "85 / 95 %");
}

#[test]
fn an_invalid_setting_keeps_the_form_open_and_writes_nothing() {
    let (mut app, dir) = settings_app("");
    select_setting(&mut app, "Swap warn / crit");
    app.handle_key(KeyCode::Enter);
    set_setting_fields(&mut app, &["95", "60"]);
    assert!(submit_setting(&mut app).is_none());
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "the form stays open");
    assert!(app.status_message.clone().unwrap_or_default().contains("below critical"), "{:?}", app.status_message);
    assert_eq!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap(), "");
}

#[test]
fn a_hand_edit_made_while_the_form_is_open_is_not_overwritten() {
    let (mut app, dir) = settings_app("");
    select_setting(&mut app, "CPU warn");
    app.handle_key(KeyCode::Enter);
    set_setting_fields(&mut app, &["80"]);
    std::fs::write(dir.path().join("config.toml"), "[ui]\ntheme = \"mono\"\n").unwrap();
    let action = submit_setting(&mut app).expect("a save");
    run_save(&action);
    let saved = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(saved.contains("theme = \"mono\"") && saved.contains("cpu_warn = 80"), "{saved}");
}

#[test]
fn an_unparsable_config_shows_the_problem_and_enter_edits_nothing() {
    let (mut app, _dir) = settings_app("[dashboard\n");
    assert!(app.settings_view.problem.is_some() && app.settings_view.rows.is_empty());
    app.handle_key(KeyCode::Enter);
    assert!(app.modal.is_none());
}

#[test]
fn refreshing_reloads_the_settings_from_the_file() {
    let (mut app, dir) = settings_app("");
    std::fs::write(dir.path().join("config.toml"), "[dashboard]\ncpu_warn = 70\n").unwrap();
    app.refresh_visible(std::sync::Arc::new(crate::exec::test_support::FakeRunner::new("")));
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "CPU warn").unwrap().value, "70 %");
}

#[test]
fn a_cap_is_set_and_cleared_from_its_row() {
    let (mut app, dir) = settings_app("[cloud.caps]\nb2 = \"10G\"\n");
    select_setting(&mut app, "b2");
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["10G"]);
    set_setting_fields(&mut app, &["20G"]);
    run_save(&submit_setting(&mut app).unwrap());
    assert!(std::fs::read_to_string(dir.path().join("config.toml")).unwrap().contains("b2 = \"20G\""));
    app.refresh_settings();
    select_setting(&mut app, "b2");
    app.handle_key(KeyCode::Enter);
    set_setting_fields(&mut app, &[""]);
    run_save(&submit_setting(&mut app).unwrap());
    assert!(!std::fs::read_to_string(dir.path().join("config.toml")).unwrap().contains("cloud"));
}

fn saved_settings(app: &mut App, dir: &tempfile::TempDir, config: &str) {
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    app.tx.send(WorkerMsg::MutationDone(0, Section::Settings, Ok(()))).unwrap();
    app.drain_worker_messages();
}

#[test]
fn saving_the_theme_changes_the_apps_theme_at_once() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[ui]\ntheme = \"mono\"\n");
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::Mono);
    saved_settings(&mut app, &dir, "[ui]\ntheme = \"nord\"\n");
    assert!(app.status_error.clone().unwrap_or_default().contains("nord"), "{:?}", app.status_error);
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::Mono, "an unknown theme changes nothing");
}

#[test]
fn a_solarized_theme_without_truecolor_says_so_as_at_startup() {
    let (mut app, _dir) = settings_app("[ui]\ntheme = \"solarized-dark\"\n");
    app.apply_saved_settings_with(&crate::ui::theme::ThemeEnv { no_color: None, colorterm: None });
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::SolarizedDark);
    assert!(app.status_message.is_some());
    app.status_message = None;
    app.apply_saved_settings_with(&crate::ui::theme::ThemeEnv { no_color: None, colorterm: Some("truecolor".into()) });
    assert_eq!(app.status_message, None);
}

#[test]
fn saving_the_auto_refresh_changes_the_interval_and_zero_turns_it_off() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[ui]\nauto_refresh_secs = 5\n");
    assert_eq!(app.auto_refresh_interval, Some(Duration::from_secs(5)));
    saved_settings(&mut app, &dir, "[ui]\nauto_refresh_secs = 0\n");
    assert_eq!(app.auto_refresh_interval, None);
    saved_settings(&mut app, &dir, "");
    assert_eq!(app.auto_refresh_interval, Some(Duration::from_secs(crate::config::DEFAULT_AUTO_REFRESH_SECS)));
}

#[test]
fn saving_thresholds_and_caps_reaches_the_dashboard_and_cloud_at_once() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[dashboard]\nmemory_warn = 85\nmemory_crit = 95\n[cloud.caps]\nb2 = \"10G\"\n");
    assert_eq!(app.dashboard_thresholds.memory, (85, 95));
    assert_eq!(app.cloud_caps.get("b2"), Some(&Ok(10_000_000_000)));
    app.refresh_settings();
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "b2").unwrap().value, "10G");
}

#[test]
fn the_saved_note_is_not_wiped_by_applying_the_settings() {
    let (mut app, dir) = settings_app("");
    app.done_note = Some("Saved to config.toml".to_string());
    saved_settings(&mut app, &dir, "[dashboard]\ncpu_warn = 80\n");
    assert_eq!(app.status_message.as_deref(), Some("Saved to config.toml"));
}

#[test]
fn help_has_a_settings_section() {
    let text = help_text();
    assert!(text.contains("\nSettings\n") && text.contains("edit the setting in a form"), "{text}");
}

#[test]
fn the_settings_load_on_entering_the_section_and_again_after_leaving_it() {
    let (mut app, dir) = alerts_app("");
    let go = |app: &mut App, s: Section| app.selected_section = app.sections.iter().position(|x| *x == s).unwrap();
    go(&mut app, Section::Settings);
    assert!(!app.settings_view.loaded);
    app.ensure_settings_loaded();
    assert!(app.settings_view.loaded);
    std::fs::write(dir.path().join("config.toml"), "[dashboard]\ncpu_warn = 70\n").unwrap();
    app.ensure_settings_loaded();
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "CPU warn").unwrap().value, "95 %", "no reload while it stays open");
    go(&mut app, Section::Alerts);
    app.ensure_settings_loaded();
    go(&mut app, Section::Settings);
    app.ensure_settings_loaded();
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "CPU warn").unwrap().value, "70 %");
}

#[test]
fn the_theme_note_is_repeated_only_when_the_theme_changes() {
    let (mut app, _dir) = settings_app("[ui]\ntheme = \"solarized-dark\"\n");
    let no_truecolor = crate::ui::theme::ThemeEnv { no_color: None, colorterm: None };
    app.apply_saved_settings_with(&no_truecolor);
    assert!(app.status_message.is_some(), "the theme changed from the default: say what the terminal lacks");
    app.status_message = Some("Saved to config.toml".to_string());
    app.apply_saved_settings_with(&no_truecolor);
    assert_eq!(app.status_message.as_deref(), Some("Saved to config.toml"), "the same theme again: keep the saved note");
}

#[test]
fn enter_on_a_resilver_line_selects_that_host_in_pools() {
    let (mut app, _dir) = dash_app();
    let mut probe = reached_probe(10);
    if let crate::zfs::host_health::HostProbe::Reached { pools, .. } = &mut probe {
        pools[0].scan = Some(crate::zfs::host_health::ScanSummary::Running { percent: Some(42), resilver: true });
    }
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(probe), ..HostProbeState::default() });
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.what.contains("resilvering 42%")).expect("a resilver line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Pools);
    assert_eq!(app.selected_host, 1);
}

#[test]
fn enter_on_a_share_line_selects_that_host_in_shares_and_the_marker_reaches_the_dashboard() {
    let (mut app, dir) = dash_app();
    let mut file = crate::daemon::shares_check::SharesFile::default();
    file.hosts.insert("fbsd16".into(), crate::daemon::shares_check::SharesResult { checked_at: chrono::Utc::now(), nfs_running: false, samba_running: true, nfs_shares: 2, smb_shares: 0, smb_error: None, error: None });
    file.save(&dir.path().join("shares.json")).unwrap();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(10)), ..HostProbeState::default() });
    app.refresh_dashboard();
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.what.contains("nfsd is not running")).expect("a share line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Shares);
    assert_eq!(app.selected_host, 1);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
    assert_eq!(app.dashboard().running, None);
    let _guard = crate::daemon::checks::RunGuard::start(dir.path(), "daemon", None);
    app.refresh_checks_marker();
    assert!(app.dashboard().running.is_some(), "a fresh marker shows the notice");
}

#[test]
fn the_dashboard_reads_smart_json_and_enter_on_its_line_selects_that_host_in_disks() {
    let (mut app, dir) = dash_app();
    let mut file = crate::daemon::smart_check::SmartFile::default();
    file.hosts.insert(
        "fbsd16".into(),
        crate::daemon::smart_check::HostResult {
            checked_at: chrono::Utc::now() - chrono::Duration::hours(2),
            disks: vec![crate::smart::Disk { dev: "da1".into(), problems: vec![crate::smart::Problem { severity: crate::smart::Severity::Watch, text: "reallocated sectors 8".into() }] }],
            error: None,
        },
    );
    file.save(&dir.path().join("smart.json")).unwrap();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(10)), ..HostProbeState::default() });
    app.refresh_dashboard();
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.what.contains("da1 reallocated sectors 8")).expect("a disk line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Disks);
    assert_eq!(app.selected_host, 1);
}

fn healthy_runner() -> std::sync::Arc<crate::exec::test_support::FakeRunner> {
    std::sync::Arc::new(
        crate::exec::test_support::FakeRunner::new("")
            .route("smartctl", "--scan", include_str!("../../smart_fixtures/scan.json"))
            .route("smartctl", "-j", include_str!("../../smart_fixtures/hdd_ok.json"))
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("cat", crate::zfs::smb::CONF_PATH, "[global]\n"),
    )
}

fn hosts_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    app.selected_host = 1;
    (app, dir)
}

fn wait_for_host_check(app: &mut App) {
    for _ in 0..200 {
        app.drain_worker_messages();
        if !app.host_check_running {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the check never finished");
}

#[test]
fn c_in_hosts_checks_the_selected_host_and_says_what_it_found() {
    let (mut app, dir) = hosts_app();
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(app.pending_host_checks, ["fbsd16"]);
    assert_eq!(app.status_message.as_deref(), Some("Checking fbsd16..."));
    app.start_host_check("fbsd16".into(), healthy_runner());
    wait_for_host_check(&mut app);
    assert_eq!(app.status_message.as_deref(), Some("fbsd16: disks fine, shares fine"));
    assert!(crate::daemon::smart_check::SmartFile::load(&dir.path().join("smart.json")).hosts.contains_key("fbsd16"));
    assert!(crate::daemon::shares_check::SharesFile::load(&dir.path().join("shares.json")).hosts.contains_key("fbsd16"));
    assert!(crate::daemon::checks::read_marker(dir.path()).is_none(), "the marker is gone when the check is done");
}

#[test]
fn c_is_refused_while_a_fresh_marker_exists_and_works_again_when_it_is_stale() {
    let (mut app, dir) = hosts_app();
    {
        let _daemon = crate::daemon::checks::RunGuard::start(dir.path(), "daemon", None);
        app.handle_key(KeyCode::Char('c'));
        assert!(app.pending_host_checks.is_empty());
        let said = app.status_error.clone().unwrap_or_default();
        assert!(said.contains("already running") && said.contains("daemon"), "{said}");
    }
    // a run that died: its marker has no heartbeat for over three minutes
    let old = chrono::Utc::now() - chrono::Duration::minutes(10);
    std::fs::write(dir.path().join(crate::daemon::checks::MARKER_FILE), serde_json::to_string(&crate::daemon::checks::Marker { by: "daemon".into(), since: old, beat: old, host: None }).unwrap()).unwrap();
    app.status_error = None;
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(app.pending_host_checks, ["fbsd16"]);
}

#[test]
fn c_does_nothing_while_a_manual_check_runs_and_only_in_hosts() {
    let (mut app, _dir) = hosts_app();
    app.handle_key(KeyCode::Char('c'));
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(app.pending_host_checks.len(), 1, "a second press while one runs is refused");
    let (mut other, _d) = hosts_app();
    other.selected_section = other.sections.iter().position(|s| *s == Section::Pools).unwrap();
    other.handle_key(KeyCode::Char('c'));
    assert!(other.pending_host_checks.is_empty());
}

#[test]
fn a_check_of_an_unreachable_host_keeps_the_old_entry_and_says_it_could_not_be_reached() {
    let (mut app, dir) = hosts_app();
    let mut before = crate::daemon::smart_check::SmartFile::default();
    before.hosts.insert("fbsd16".into(), crate::daemon::smart_check::HostResult { checked_at: chrono::Utc::now() - chrono::Duration::hours(5), disks: vec![crate::smart::Disk { dev: "da1".into(), problems: vec![crate::smart::Problem { severity: crate::smart::Severity::Issue, text: "SMART health FAILED".into() }] }], error: None });
    before.save(&dir.path().join("smart.json")).unwrap();
    app.handle_key(KeyCode::Char('c'));
    app.start_host_check("fbsd16".into(), std::sync::Arc::new(crate::exec::test_support::FakeRunner::failing("ssh: connect to host h port 22: Connection timed out", 255)));
    wait_for_host_check(&mut app);
    assert_eq!(app.status_message.as_deref(), Some("fbsd16: disks not reachable, shares not reachable"));
    let after = crate::daemon::smart_check::SmartFile::load(&dir.path().join("smart.json"));
    assert!(after.hosts["fbsd16"].error.is_some() && crate::daemon::smart_check::problems(&after).len() == 1, "the red line stays, with the error noted");
}

#[test]
fn a_hung_manual_check_gives_up_at_its_deadline_and_frees_c_and_the_marker() {
    struct Hung;
    impl crate::exec::CommandRunner for Hung {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            std::thread::sleep(Duration::from_secs(3));
            Err(crate::error::AppError::Config("too late".into()))
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
    }
    let (mut app, dir) = hosts_app();
    app.handle_key(KeyCode::Char('c'));
    let started = Instant::now();
    app.start_host_check_with("fbsd16".into(), std::sync::Arc::new(Hung), Duration::from_millis(400));
    wait_for_host_check(&mut app);
    assert!(started.elapsed() < Duration::from_secs(2), "waited for the hung host");
    assert!(app.status_message.clone().unwrap_or_default().contains("no answer"), "{:?}", app.status_message);
    assert!(crate::daemon::checks::read_marker(dir.path()).is_none(), "the marker is released at the deadline");
    app.pending_host_checks.clear();
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(app.pending_host_checks, ["fbsd16"], "c works again");
}

#[test]
fn the_apps_host_os_follows_the_last_probe_that_detected_it() {
    use crate::zfs::host_health::{HostOs, HostProbe};
    let mut app = App::new(vec!["local".to_string(), "ub24".to_string()]);
    assert_eq!(app.host_os(1), None, "not probed yet");
    let reached = |os| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os };
    app.tx.send(WorkerMsg::HostHealth("ub24".into(), reached(Some(HostOs::Linux)))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_os(1), Some(HostOs::Linux));
    app.tx.send(WorkerMsg::HostHealth("ub24".into(), reached(None))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_os(1), Some(HostOs::Linux), "a later probe that did not ask does not make it forget");
    assert_eq!(app.host_os(0), None);
    assert_eq!(app.host_os(9), None, "no such host");
}
