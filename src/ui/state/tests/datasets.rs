//! Datasets: lists, properties, quotas and limits, encrypted datasets and their keys.
#![allow(clippy::wildcard_imports)]
use super::*;

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
fn handle_key_n_on_datasets_is_noop_while_right_focused() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none(), "create-snapshot key must be a no-op while the properties pane is focused");
}

#[test]
fn handle_key_navigates_dataset_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
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
            mountpoint: None, quota: 0, refquota: 0,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.datasets_table.selected(), Some(0));
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/other".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
        mountpoint: None, quota: 0, refquota: 0,
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
        Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
        Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
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
    app.datasets = vec![Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
fn selected_pool_name_and_selected_dataset_name_helpers() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    assert_eq!(app.selected_pool_name(), Some("tank".to_string()));
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
        Dataset { name: "tank/aa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
        Dataset { name: "tank/b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
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
                Dataset { name: "tank/aaa".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
                Dataset { name: "tank/different".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 },
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
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
fn shift_n_and_m_with_no_dataset_selected_are_inert() {
    for key in ['N', 'M'] {
        let mut app = datasets_app_with_mount(Some("/tank/media"));
        app.datasets_table.select(None);
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none() && app.status_message.is_none(), "{key}");
    }
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
fn refreshing_datasets_also_reads_which_are_encrypted_and_locked() {
    let mut app = App::new(vec!["local".to_string()]);
    let get = "tank/data\tencryption\taes-256-gcm\ntank/data\tkeyformat\tpassphrase\ntank/data\tkeystatus\tunavailable\ntank/data\tencryptionroot\ttank/data\n";
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data\t100\t200\t100\t/tank/data\tfilesystem\tyes\n").route("zfs", "get", get));
    app.refresh_datasets(runner);
    wait_until(&mut app, |a| !a.datasets.is_empty());
    assert!(app.encryption["tank/data"].locked, "the Encryption message arrives before the list");
}

fn app_with_dataset_detail_for(selected: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")), quota: 0, refquota: 0 };
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
    let same = vec![Dataset { name: "tank/a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/a".to_string()), quota: 0, refquota: 0 }];
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
    let moved = vec![Dataset { name: "tank/z".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some("/tank/z".to_string()), quota: 0, refquota: 0 }];
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

fn two_props() -> Vec<Property> {
    ["x", "y"].iter().map(|n| Property { name: (*n).to_string(), value: "v".to_string(), source: PropertySource::Local }).collect()
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
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{n}")), quota: 0, refquota: 0 };
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
fn a_stuck_busy_marker_expires_so_the_section_can_never_be_locked_for_good() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.bootenv_busy = Some(("spare".to_string(), Instant::now().checked_sub(Duration::from_mins(5)).unwrap()));
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1);
}

#[test]
fn the_acltype_of_another_dataset_is_never_applied() {
    let mut app = acl_app_with_acltype(Some("posix"));
    app.dataset_properties_for = Some("tank/other".to_string());
    assert_eq!(app.acl_unsupported(), None, "stale properties of a different dataset say nothing about this one");
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

/// A feature@ edit form on pool `tank`, which is the selected pool, with the given `bootfs` value and replication jobs.
fn feature_app(bootfs: &str, jobs: Vec<crate::jobs::Job>) -> App {
    let mut app = property_form(FormKind::EditPoolProperty { host: 0, pool: "tank".into(), key: "feature@zstd_compress".into() }, "enabled");
    app.pools = vec![crate::zfs::pool::Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app.pool_properties = vec![Property { name: "bootfs".to_string(), value: bootfs.to_string(), source: PropertySource::Local }];
    app.jobs_view.jobs = jobs;
    app
}

#[test]
fn enabling_a_feature_on_a_boot_pool_needs_the_pool_name_and_names_the_loader() {
    let mut app = feature_app("tank/ROOT/default", vec![]);
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    let Some(Modal::ConfirmTyped { prompt, expected, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(expected, "tank");
    assert!(prompt.contains("zpool set feature@zstd_compress=enabled tank") && prompt.contains("boot pool") && prompt.contains("boot loader"), "{prompt}");
}

#[test]
fn enabling_a_feature_on_a_pool_that_replication_sends_from_names_the_targets_but_keeps_y_n() {
    let jobs = vec![crate::jobs::Job::test_replicate("r1", "tank/data", "zt", "backup1", "pool/copy"), crate::jobs::Job::test_replicate("r2", "other/data", "zt", "backup2", "pool/copy")];
    let mut app = feature_app("-", jobs);
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("backup1") && !prompt.contains("backup2") && prompt.contains("cannot receive it") && prompt.ends_with("(y/n)"), "{prompt}");
}

#[test]
fn a_feature_on_a_plain_pool_keeps_the_ordinary_question() {
    let mut app = feature_app("-", vec![]);
    let prompt = submit_to_confirm(&mut app);
    assert!(!prompt.contains("boot pool") && !prompt.contains("Replication jobs"), "{prompt}");
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
fn enter_on_the_and_more_line_opens_the_list_at_the_first_dataset_without_a_line() {
    let mut app = unprotected_app(&["tank/a", "tank/b", "tank/c", "tank/d", "tank/e"]);
    let items = app.dashboard().attention;
    let more = items.iter().position(|i| i.subject == "protection").expect("the folded line");
    for _ in 0..=more {
        app.handle_key(KeyCode::Down);
    }
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "Enter" && h.label == "Show list"));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Choose { title, rows, selected, .. }) => {
            assert_eq!(title, "Unprotected data");
            assert_eq!(rows.len(), 5);
            assert_eq!(*selected, 3);
            assert!(rows[3].contains("tank/d"), "{rows:?}");
        }
        other => panic!("expected the list, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Esc);
    assert!(app.modal.is_none());
}

#[test]
fn the_list_moves_with_the_arrows_stays_in_range_and_ignores_other_keys() {
    use crate::daemon::protection::Gap;
    let (mut app, _dir) = dash_app();
    app.modal = Some(list_of(vec![unprotected(Some(Gap::Missing), None, "tank/a"), unprotected(Some(Gap::Missing), None, "tank/b")]));
    let selected = |app: &App| match &app.modal {
        Some(Modal::Choose { selected, .. }) => *selected,
        other => panic!("{other:?}"),
    };
    app.handle_modal_key(KeyCode::Up);
    assert_eq!(selected(&app), 0);
    app.handle_modal_key(KeyCode::Down);
    app.handle_modal_key(KeyCode::Down);
    assert_eq!(selected(&app), 1, "not past the last row");
    app.handle_modal_key(KeyCode::Char('x'));
    app.handle_modal_key(KeyCode::Char('q'));
    assert_eq!(selected(&app), 1, "a stray key neither closes it nor acts");
    app.handle_modal_key(KeyCode::Home);
    assert_eq!(selected(&app), 0);
    app.handle_modal_key(KeyCode::End);
    assert_eq!(selected(&app), 1);
}

#[test]
fn a_dataset_that_needs_a_copy_asks_which_kind_and_opens_that_form_filled_in() {
    use crate::daemon::protection::{CopyGap, Gap};
    let copies = Some(CopyGap { have: 0, need: 1, online_have: 0, online_need: 0, domains: None, worst: Gap::Missing, files_ignored: 0 });
    for (row, title, host_label, dataset_label) in [(0, "New replication job", "Source host", "Source dataset"), (1, "New backup job", "Host (where restic runs)", "Dataset"), (2, "New cloud mirror job", "Host (where rclone runs)", "Dataset")] {
        let (mut app, _dir) = dash_app();
        app.modal = Some(list_of(vec![unprotected(None, copies, "tank/photos")]));
        app.handle_modal_key(KeyCode::Enter);
        assert!(matches!(&app.modal, Some(Modal::Choose { title, rows, .. }) if title == "Which copy?" && rows.len() == 3), "{:?}", app.modal);
        for _ in 0..row {
            app.handle_modal_key(KeyCode::Down);
        }
        app.handle_modal_key(KeyCode::Enter);
        assert!(matches!(&app.modal, Some(Modal::Form { title: t, .. }) if t == title), "{:?}", app.modal);
        assert_eq!(field(app.modal.as_ref(), 0), (host_label.to_string(), "fbsd16".to_string()));
        assert_eq!(field(app.modal.as_ref(), 1), (dataset_label.to_string(), "tank/photos".to_string()));
    }
}

#[test]
fn enter_on_a_quota_row_prefills_a_human_size_and_none_for_unset() {
    let g: u64 = 1 << 30;
    let mut app = quota_app(false, 10 * g, 0, 0, 0);
    open_property(&mut app, "quota");
    let Some(crate::ui::modal::Modal::Form { fields, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(fields[0].value, "10G");
    app.modal = None;
    open_property(&mut app, "refquota");
    let Some(crate::ui::modal::Modal::Form { fields, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(fields[0].value, "none");
}

#[test]
fn a_quota_below_use_keeps_the_form_open_with_our_message() {
    let mut app = quota_app(false, 0, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(submit_value(&mut app, "4G").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "the form stays open");
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("already use")), "{:?}", app.status_error);
    assert!(submit_value(&mut app, "ten gigs").is_none() && app.status_error.as_deref().is_some_and(|m| m.contains("not a size")));
}

#[test]
fn every_limit_change_asks_first_and_sends_exact_bytes_and_none_as_none() {
    let g: u64 = 1 << 30;
    let mut app = quota_app(false, 0, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(submit_value(&mut app, "20G").is_none(), "even a roomy quota is confirmed");
    let action = confirm_limit_change(&mut app);
    assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "quota".into(), value: (20 * g).to_string() });
    let mut app = quota_app(false, 20 * g, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(submit_value(&mut app, "none").is_none(), "removing a limit is confirmed too");
    let action = confirm_limit_change(&mut app);
    assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "quota".into(), value: "none".into() });
}

#[test]
fn a_quota_with_under_ten_percent_headroom_asks_y_then_enter() {
    let mut app = quota_app(false, 0, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(submit_value(&mut app, "5G").is_none(), "5G is exactly what is used: writes would fail at once");
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { prompt, action, choice: None }) => {
            assert!(prompt.contains("tank/data") && prompt.contains("quota"), "{prompt}");
            assert!(matches!(&action.call, MutationCall::SetDatasetProperty { key, .. } if key == "quota"));
        }
        other => panic!("expected a Confirm, got {other:?}"),
    }
}

#[test]
fn a_volume_refuses_quota_and_still_takes_the_reservations() {
    let mut app = quota_app(true, 0, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(app.modal.is_none(), "no form for a volume's quota");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("volume")), "{:?}", app.status_message);
    for key in ["refreservation", "reservation"] {
        open_property(&mut app, key);
        assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "{key}");
        app.modal = None;
    }
}

#[test]
fn zero_means_none_in_the_quota_form_and_the_prompt_shows_a_readable_size() {
    let g: u64 = 1 << 30;
    let mut app = quota_app(false, 20 * g, 0, 0, 0);
    open_property(&mut app, "quota");
    assert!(submit_value(&mut app, "0").is_none());
    let action = confirm_limit_change(&mut app);
    assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "quota".into(), value: "none".into() });
    let mut app = quota_app(false, 0, 0, 0, 0);
    open_property(&mut app, "quota");
    app.dataset_properties_for = Some("tank/other".to_string());
    assert!(submit_value(&mut app, "20G").is_none(), "facts of another dataset must not validate this one");
    app.dataset_properties_for = Some("tank/data".to_string());
    assert!(submit_value(&mut app, "5G").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { prompt, .. }) => assert!(prompt.contains("quota=5368709120 (5G)"), "{prompt}"),
        other => panic!("expected a Confirm, got {other:?}"),
    }
}

#[test]
fn enter_on_checksum_and_dedup_open_a_form_and_f2_lists_the_values() {
    let mut app = integrity_app("on", "off");
    open_property(&mut app, "checksum");
    let Some(crate::ui::modal::Modal::Form { fields, kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(fields[0].label.contains("F2"), "{}", fields[0].label);
    assert_eq!(fields[0].value, "on");
    let list = crate::ui::picker::vocabulary_for(kind, 0).unwrap().0;
    assert!(list.contains(&"fletcher4") && list.contains(&"blake3") && list.contains(&"off"));
    app.modal = None;
    open_property(&mut app, "dedup");
    let Some(crate::ui::modal::Modal::Form { kind, .. }) = &app.modal else { panic!() };
    assert!(crate::ui::picker::vocabulary_for(kind, 0).unwrap().0.contains(&"sha256,verify"));
}

#[test]
fn checksum_off_asks_for_the_dataset_name_and_says_what_it_means() {
    let mut app = integrity_app("on", "off");
    open_property(&mut app, "checksum");
    assert!(submit_value(&mut app, "off").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { prompt, expected, action, .. }) => {
            assert_eq!(expected, "tank/data");
            assert!(prompt.contains("silent corruption") && prompt.contains("zfs set checksum=off tank/data") && prompt.contains("Existing data"), "{prompt}");
            assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "checksum".into(), value: "off".into() });
        }
        other => panic!("{other:?}"),
    }
    for c in "tank/dat".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "a partial name does nothing");
    app.handle_modal_key(KeyCode::Char('a'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some(), "the full name sends it");
}

#[test]
fn another_checksum_asks_y_enter_with_the_scrub_reminder_and_sends_the_normalized_value() {
    let mut app = integrity_app("on", "off");
    open_property(&mut app, "checksum");
    assert!(submit_value(&mut app, " SHA256 ").is_none());
    let action = match app.modal.take() {
        Some(crate::ui::modal::Modal::Confirm { prompt, action, .. }) => {
            assert!(prompt.contains("written from now on") && prompt.contains("scrub"), "{prompt}");
            action
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "checksum".into(), value: "sha256".into() });
}

#[test]
fn dedup_on_asks_for_the_name_and_warns_about_ram_and_off_is_a_plain_confirm() {
    let mut app = integrity_app("on", "off");
    open_property(&mut app, "dedup");
    assert!(submit_value(&mut app, "SHA256, Verify").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { prompt, action, .. }) => {
            assert!(prompt.contains("RAM") && prompt.contains("does not undeduplicate") && prompt.contains("RAM is not known"), "{prompt}");
            assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "dedup".into(), value: "sha256,verify".into() });
        }
        other => panic!("{other:?}"),
    }
    let mut app = integrity_app("on", "on");
    open_property(&mut app, "dedup");
    assert!(submit_value(&mut app, "off").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Confirm { .. })), "{:?}", app.modal);
}

#[test]
fn a_value_the_dataset_already_has_is_said_and_sends_nothing() {
    let mut app = integrity_app("sha256", "off");
    open_property(&mut app, "checksum");
    assert!(submit_value(&mut app, "sha256").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })) && app.status_error.as_deref().is_some_and(|m| m.contains("already sha256")), "{:?}", app.status_error);
}

#[test]
fn turning_the_checksum_back_on_says_a_scrub_cannot_verify_what_was_written_while_it_was_off() {
    let mut app = integrity_app("off", "off");
    open_property(&mut app, "checksum");
    assert!(submit_value(&mut app, "on").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { prompt, .. }) => assert!(prompt.contains("has none") && prompt.contains("rewriting"), "{prompt}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn dedup_on_without_a_known_current_value_still_asks_for_the_name() {
    let mut app = integrity_app("on", "off");
    app.dataset_properties.retain(|p| p.name != "dedup");
    app.dataset_properties.push(Property { name: "compression".into(), value: "on".into(), source: PropertySource::Default });
    // open the form on a row that exists, then point it at dedup: the list has no dedup row, so `current` is empty
    open_property(&mut app, "checksum");
    if let Some(crate::ui::modal::Modal::Form { kind, .. }) = &mut app.modal {
        *kind = crate::ui::modal::FormKind::EditDatasetProperty { host: 0, dataset: "tank/data".into(), key: "dedup".into() };
    }
    assert!(submit_value(&mut app, "on").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::ConfirmTyped { .. })), "{:?}", app.modal);
}

const PASS: &str = "correct horse battery";

#[test]
fn big_l_on_a_locked_dataset_opens_a_masked_passphrase_form_for_the_root() {
    let mut app = key_app(true, "passphrase");
    app.datasets_table.select(Some(1)); // the child: the key belongs to tank/s
    app.handle_key(KeyCode::Char('L'));
    let Some(Modal::Form { title, fields, kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(title.contains("tank/s") && !title.contains("kid"), "{title}");
    assert_eq!(fields.len(), 2);
    assert!(kind.masks_field(0) && !kind.masks_field(1));
    assert_eq!(fields[1].value, "y", "mount after loading is the default");
}

#[test]
fn the_key_keys_say_why_they_do_nothing() {
    let says = |app: &mut App, key: char, text: &str| {
        app.status_message = None;
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none(), "{:?}", app.modal);
        let got = app.status_message.clone().unwrap_or_default();
        assert!(got.contains(text), "{key}: wanted '{text}' in '{got}'");
    };
    let mut open = key_app(false, "passphrase");
    says(&mut open, 'L', "already loaded");
    open.datasets_table.select(Some(1));
    says(&mut open, 'C', "change the key on tank/s");
    open.datasets_table.select(Some(2));
    says(&mut open, 'L', "not encrypted");
    says(&mut open, 'U', "not encrypted");
    let mut locked = key_app(true, "passphrase");
    says(&mut locked, 'U', "already locked");
    says(&mut locked, 'C', "load the key first");
    let mut raw = key_app(true, "raw");
    says(&mut raw, 'L', "raw key");
}

#[test]
fn loading_a_key_sends_the_passphrase_to_the_dispatcher_and_nowhere_else() {
    let mut app = key_app(true, "passphrase");
    app.handle_key(KeyCode::Char('L'));
    type_into(&mut app, PASS);
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::Backspace);
    type_into(&mut app, "n");
    let action = app.handle_modal_key(KeyCode::Enter).expect("the form submits");
    assert_eq!(action.section, Section::Datasets);
    let MutationCall::LoadKey { dataset, passphrase, mount } = &action.call else { panic!("{:?}", action.call) };
    assert_eq!((dataset.as_str(), passphrase.0.as_str(), *mount), ("tank/s", PASS, false));
    assert!(app.modal.is_none());
    assert!(!format!("{app_status:?}{action:?}", app_status = app.status_message).contains(PASS));
}

#[test]
fn a_full_load_key_run_keeps_the_passphrase_out_of_arguments_status_and_errors() {
    for (runner, ok) in [(FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n"), true), (FakeRunner::new("").route("zfs", "get", "keylocation\tprompt\nkeyformat\tpassphrase\nencryptionroot\ttank/s\ncanmount\ton\n").route_failure("zfs", "load-key", "Key load error: Incorrect key provided for 'tank/s'.", 255), false)] {
        let mut app = key_app(true, "passphrase");
        app.handle_key(KeyCode::Char('L'));
        type_into(&mut app, PASS);
        app.handle_modal_key(KeyCode::Tab);
        let action = app.handle_modal_key(KeyCode::Enter).expect("submits");
        let runner = Arc::new(runner);
        app.dispatch_mutation(action, runner.clone());
        let deadline = Instant::now() + Duration::from_secs(2);
        let refreshes = loop {
            let r = app.drain_worker_messages();
            if !r.is_empty() || app.status_message.is_some() || Instant::now() > deadline {
                break r;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(!refreshes.is_empty(), ok, "{refreshes:?} {:?}", app.status_message);
        let calls = runner.calls.lock().unwrap();
        assert!(calls.iter().all(|(_, a)| a.iter().all(|x| !x.contains(PASS))), "{calls:?}");
        assert!(calls.iter().any(|(_, a)| a[0] == "load-key"));
        assert_eq!(runner.stdin_calls.lock().unwrap().as_slice(), [("zfs".to_string(), format!("{PASS}\n"))]);
        assert!(!app.status_message.clone().unwrap_or_default().contains(PASS));
    }
}

fn fill_encrypted_form(app: &mut App, name: &str, cipher: &str, pass: &str, repeat: &str) {
    app.handle_key(KeyCode::Char('E'));
    let Some(Modal::Form { fields, .. }) = app.modal.as_mut() else { panic!("{:?}", app.modal) };
    fields[0].value = name.into();
    fields[1].value = cipher.into();
    fields[2].value = pass.into();
    fields[3].value = repeat.into();
    for _ in 0..4 {
        app.handle_modal_key(KeyCode::Enter);
        if !matches!(app.modal, Some(Modal::Form { .. })) {
            break;
        }
    }
}

#[test]
fn an_encrypted_dataset_needs_its_name_typed_with_the_lost_key_warning() {
    let mut app = key_app(false, "passphrase");
    fill_encrypted_form(&mut app, "tank/vault", "aes-256-gcm", PASS, PASS);
    let Some(Modal::ConfirmTyped { prompt, expected, .. }) = &app.modal else { panic!("{:?} / {:?}", app.modal, app.status_message) };
    assert_eq!(expected, "tank/vault");
    assert!(prompt.contains("CANNOT be recovered") && prompt.contains("including zfsctl") && !prompt.contains(PASS), "{prompt}");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "Enter before the name is typed does nothing");
    type_into(&mut app, "tank/vault");
    let action = app.handle_modal_key(KeyCode::Enter).expect("name typed");
    assert!(matches!(&action.call, MutationCall::CreateEncryptedDataset { name, cipher, passphrase } if name == "tank/vault" && cipher == "aes-256-gcm" && passphrase.0 == PASS));
    assert!(!format!("{action:?}").contains(PASS));
}

#[test]
fn a_bad_encrypted_form_keeps_the_form_and_clears_the_passphrases() {
    for (name, cipher, pass, repeat, says) in [
        ("tank/v", "aes-256-gcm", PASS, "something else", "do not match"),
        ("tank/v", "aes-256-gcm", "short", "short", "8 characters"),
        ("tank/v", "rot13", PASS, PASS, "Cipher"),
        ("vault", "aes-256-gcm", PASS, PASS, "full dataset name"),
        ("-tank/vault", "aes-256-gcm", PASS, PASS, "full dataset name"),
        ("tank/my vault", "aes-256-gcm", PASS, PASS, "full dataset name"),
        ("", "aes-256-gcm", PASS, PASS, "full dataset name"),
        ("tank/s/new", "aes-256-gcm", PASS, PASS, "tank/s is encrypted"),
    ] {
        let mut app = key_app(false, "passphrase");
        fill_encrypted_form(&mut app, name, cipher, pass, repeat);
        let Some(Modal::Form { fields, .. }) = &app.modal else { panic!("{name}: {:?}", app.modal) };
        assert!(fields[2].value.is_empty() && fields[3].value.is_empty());
        assert!(app.status_message.as_deref().unwrap_or("").contains(says), "{says}: {:?}", app.status_message);
    }
}

#[test]
fn a_full_encrypted_create_keeps_the_passphrase_out_of_arguments_and_status() {
    let mut app = key_app(false, "passphrase");
    fill_encrypted_form(&mut app, "tank/vault", "aes-256-gcm", PASS, PASS);
    type_into(&mut app, "tank/vault");
    let action = app.handle_modal_key(KeyCode::Enter).expect("confirmed");
    let runner = Arc::new(FakeRunner::new(""));
    app.dispatch_mutation(action, runner.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.drain_worker_messages().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let calls = runner.calls.lock().unwrap();
    assert!(calls.iter().all(|(_, a)| a.iter().all(|x| !x.contains(PASS))));
    assert_eq!(runner.stdin_calls.lock().unwrap().as_slice(), [("zfs".to_string(), format!("{PASS}\n"))]);
    assert!(!app.status_message.clone().unwrap_or_default().contains(PASS));
}

#[test]
fn capital_c_compares_with_the_live_dataset() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('C'));
    assert_eq!(app.pending_changes[0].against, crate::ui::changes::Against::Live);
    assert_eq!(app.changes_view.as_ref().unwrap().against, crate::ui::changes::Against::Live);
}

#[test]
fn c_with_nothing_selected_does_nothing() {
    let mut app = changes_app();
    app.snapshots_table.select(None);
    app.handle_key(KeyCode::Char('c'));
    app.handle_key(KeyCode::Char('C'));
    assert!(app.changes_view.is_none() && app.pending_changes.is_empty());
}

#[test]
fn c_while_a_read_is_running_starts_no_second_one() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    app.handle_key(KeyCode::Char('c'));
    app.handle_key(KeyCode::Char('C'));
    assert_eq!(app.pending_changes.len(), 1);
}

#[test]
fn enter_on_sync_opens_a_form_and_f2_lists_the_three_values() {
    let mut app = sync_app("standard");
    open_property(&mut app, "sync");
    let Some(crate::ui::modal::Modal::Form { fields, kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(fields[0].label.contains("F2"), "{}", fields[0].label);
    assert_eq!(fields[0].value, "standard");
    assert_eq!(crate::ui::picker::vocabulary_for(kind, 0).unwrap().0, ["standard", "always", "disabled"]);
}

#[test]
fn sync_disabled_asks_for_the_dataset_name_and_says_what_is_lost() {
    let mut app = sync_app("standard");
    open_property(&mut app, "sync");
    assert!(submit_value(&mut app, " DISABLED ").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { prompt, expected, action, .. }) => {
            assert_eq!(expected, "tank/data");
            assert!(prompt.contains("zfs set sync=disabled tank/data") && prompt.contains("power loss") && prompt.contains("stable storage") && prompt.contains("SLOG"), "{prompt}");
            assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "sync".into(), value: "disabled".into() });
        }
        other => panic!("{other:?}"),
    }
    for c in "tank/dat".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "a partial name does nothing");
    app.handle_modal_key(KeyCode::Char('a'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some(), "the full name sends it");
}

#[test]
fn sync_always_and_standard_ask_y_enter_and_send_the_normalized_value() {
    for (from, to) in [("standard", " Always"), ("disabled", "standard")] {
        let mut app = sync_app(from);
        open_property(&mut app, "sync");
        assert!(submit_value(&mut app, to).is_none());
        let Some(crate::ui::modal::Modal::Confirm { prompt, action, .. }) = app.modal.take() else { panic!("{:?}", app.modal) };
        assert!(prompt.contains("Set it? (y/n)") && prompt.contains("inherit"), "{prompt}");
        assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "sync".into(), value: to.trim().to_lowercase() });
    }
}

#[test]
fn sync_to_the_value_it_has_or_a_bad_value_is_refused_in_words() {
    let mut app = sync_app("standard");
    open_property(&mut app, "sync");
    assert!(submit_value(&mut app, "standard").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "{:?}", app.modal);
    assert!(submit_value(&mut app, "off").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "{:?}", app.modal);
}

#[test]
fn sync_works_on_a_volume_and_with_an_unknown_current_value() {
    let mut app = sync_app("");
    for p in &mut app.dataset_properties {
        if p.name == "type" {
            p.value = "volume".into();
        }
    }
    open_property(&mut app, "sync");
    assert!(submit_value(&mut app, "standard").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Confirm { .. })), "{:?}", app.modal);
}

#[test]
fn sync_with_the_properties_of_another_dataset_is_refused() {
    let mut app = sync_app("standard");
    open_property(&mut app, "sync");
    app.dataset_properties_for = Some("tank/other".to_string());
    assert!(submit_value(&mut app, "disabled").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "{:?}", app.modal);
}

fn setting(dataset: &str, property: &str, value: &str) -> crate::zfs::local_settings::LocalSetting {
    crate::zfs::local_settings::LocalSetting { dataset: dataset.into(), property: property.into(), value: value.into() }
}

#[test]
fn h_in_datasets_asks_for_the_read_once_and_the_answer_opens_the_list_grouped_by_dataset() {
    let mut app = key_app(false, "passphrase");
    app.handle_key(KeyCode::Char('H'));
    assert_eq!(app.pending_local_audit, [0]);
    assert!(app.local_audit_busy);
    app.handle_key(KeyCode::Char('H'));
    assert_eq!(app.pending_local_audit, [0], "a second press while it is reading asks for nothing more");
    let long = "x".repeat(100);
    app.on_local_audit(0, Ok(vec![setting("tank/s", "atime", "off"), setting("tank/s", "compression", "lz4"), setting("tank/plain", "sync", &long)]));
    assert!(!app.local_audit_busy);
    let Some(crate::ui::modal::Modal::Choose { title, note, rows, then, look, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(title, "Set by hand on local");
    assert!(note.starts_with("3 properties set by hand on local, on 2 datasets"), "{note}");
    assert_eq!(look.heads, [(0, "tank/s".to_string()), (2, "tank/plain".to_string())]);
    assert_eq!(rows[0], "atime = off");
    assert!(rows[2].starts_with("sync = xxx") && rows[2].ends_with('…') && rows[2].chars().count() < 80, "{}", rows[2]);
    assert_eq!(then, &crate::ui::modal::ChooseThen::LocalSettings(vec!["tank/s".into(), "tank/s".into(), "tank/plain".into()]));
}

#[test]
fn properties_at_their_default_are_hidden_counted_and_d_shows_them() {
    let mut app = key_app(false, "passphrase");
    app.on_local_audit(0, Ok(vec![setting("tank", "atime", "on"), setting("tank", "compression", "lz4"), setting("tank/a", "sync", "standard")]));
    let Some(crate::ui::modal::Modal::Choose { note, rows, then, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(rows, &["compression = lz4"]);
    assert!(note.starts_with("1 property set by hand") && note.contains("2 at their default value are hidden"), "{note}");
    assert_eq!(then, &crate::ui::modal::ChooseThen::LocalSettings(vec!["tank".into()]));
    app.handle_modal_key(KeyCode::Char('d'));
    let Some(crate::ui::modal::Modal::Choose { note, rows, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(rows, &["atime = on", "compression = lz4", "sync = standard"]);
    assert!(note.contains("Defaults are shown"), "{note}");
    app.handle_modal_key(KeyCode::Char('d'));
    let Some(crate::ui::modal::Modal::Choose { rows, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(rows.len(), 1);
}

#[test]
fn when_only_defaults_were_set_the_list_shows_them_rather_than_nothing() {
    let mut app = key_app(false, "passphrase");
    app.on_local_audit(0, Ok(vec![setting("tank", "atime", "on")]));
    let Some(crate::ui::modal::Modal::Choose { rows, note, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(rows, &["atime = on"]);
    assert!(!note.contains("hidden"), "{note}");
}

#[test]
fn enter_in_the_set_by_hand_list_selects_that_dataset_and_closes_it() {
    let mut app = key_app(false, "passphrase");
    app.on_local_audit(0, Ok(vec![setting("tank/plain", "atime", "off")]));
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.selected_dataset_name().as_deref(), Some("tank/plain"));
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("press →")), "{:?}", app.status_message);
}

#[test]
fn nothing_set_by_hand_and_a_failed_read_say_so_and_another_host_is_ignored() {
    let mut app = key_app(false, "passphrase");
    app.on_local_audit(0, Ok(vec![]));
    let Some(crate::ui::modal::Modal::Info { body, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(body.contains("Nothing was set by hand"), "{body}");
    app.modal = None;
    app.on_local_audit(0, Err(crate::error::AppError::Parse("boom".into())));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("boom")), "{:?}", app.status_message);
    app.on_local_audit(5, Ok(vec![setting("x", "atime", "off")]));
    assert!(app.modal.is_none(), "a read for a host that is not selected opens nothing");
}

#[test]
fn h_does_nothing_outside_the_datasets_list() {
    let mut app = key_app(false, "passphrase");
    go_to(&mut app, Section::Pools);
    app.handle_key(KeyCode::Char('H'));
    assert!(app.pending_local_audit.is_empty() && !app.local_audit_busy);
}
