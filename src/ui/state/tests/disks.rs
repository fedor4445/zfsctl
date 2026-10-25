//! Disks: selection, and the forms that build a pool or add a vdev from them.
#![allow(clippy::wildcard_imports)]
use super::*;

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
            assert_eq!(fields.len(), 3, "the Add vdev form has a Role field and the Create Pool form an Options field");
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
            assert_eq!(fields.len(), 3, "name, redundancy and options");
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
            assert_eq!(fields.len(), 3, "pool, role, redundancy");
            assert_eq!(fields[1].value, "data", "the role defaults to data, as before");
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
        FormField { label: "Role".to_string(), value: "data".to_string() },
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
    let mut as_log = fields.clone();
    as_log[1].value = "log".to_string();
    assert_eq!(build_pending_action(&kind, &as_log).unwrap().call, MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["log".into(), "mirror".into(), "da2".into(), "da3".into()] });
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
    app.handle_modal_key(KeyCode::Tab); // and on to the Options field, the last one
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
fn mutation_done_for_pools_section_also_queues_disks_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx.send(WorkerMsg::MutationDone(0, Section::Pools, Ok(()))).unwrap();
    let refreshes = app.drain_worker_messages();
    assert!(refreshes.contains(&Section::Pools));
    assert!(refreshes.contains(&Section::Disks), "a Pools-section mutation (create/destroy pool) changes disk membership too");
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
        let fields = if matches!(kind, FormKind::CreatePoolFromDisks { .. }) {
            vec![FormField { label: "a".into(), value: "newpool".into() }, FormField { label: "b".into(), value: "mirror".into() }]
        } else {
            vec![FormField { label: "a".into(), value: "tank".into() }, FormField { label: "role".into(), value: "data".into() }, FormField { label: "b".into(), value: "mirror".into() }]
        };
        let last = fields.len() - 1;
        app.modal = Some(Modal::Form {
            title: "t".to_string(),
            fields,
            focused: last,
            description: None,
            kind,
        });
        let prompt = submit_to_confirm(&mut app);
        assert!(prompt.contains("da0 da1"), "{prompt}");
    }
}

#[test]
fn a_single_disk_special_is_refused_before_anything_runs_and_the_form_stays_open() {
    let mut app = add_from_disks(&["da4"], "special", "");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("whole pool")), "{:?}", app.status_error);
    assert!(app.pending_add_checks.is_empty());
}

#[test]
fn the_raw_add_form_refuses_a_lone_special_disk_and_queues_the_dry_run_otherwise() {
    let mut app = App::new(vec!["local".to_string()]);
    pool_for_forms(&mut app);
    app.vdevs = vec![VdevLine { depth: 1, name: "mirror-0".to_string(), health: crate::zfs::pool::PoolHealth::Online }];
    app.vdevs_table.select(Some(0));
    app.handle_key(KeyCode::Char('a'));
    for c in "special da6".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(matches!(app.modal, Some(Modal::Form { .. })) && app.status_error.as_deref().is_some_and(|m| m.contains("whole pool")), "{:?}", app.status_error);
    assert!(app.pending_add_checks.is_empty());
    for _ in 0..20 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "special mirror da6 da7".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool add tank special mirror da6 da7"), "{prompt}");
    assert_eq!(app.pending_add_checks[0].role, crate::zfs::vdev_role::Role::Special);
}
