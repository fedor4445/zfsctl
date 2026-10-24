//! Pools and vdevs: lists, drill-down, scrub and resilver status, properties and disk actions.
#![allow(clippy::wildcard_imports)]
use super::*;

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
fn is_loading_ignores_the_vdevs_auto_refresh_but_counts_everything_else() {
    let mut app = App::new(vec!["local".to_string()]);
    assert!(!app.is_loading());
    app.in_flight.insert((Section::Vdevs, 0));
    assert!(!app.is_loading(), "the background vdev poll must not show the refreshing indicator");
    app.in_flight.insert((Section::Pools, 0));
    assert!(app.is_loading());
}

#[test]
fn bang_works_inside_a_drilled_pool_too() {
    let mut app = App::new(vec!["local".to_string()]);
    app.drilled_pool = Some("tank".to_string());
    app.handle_key(KeyCode::Char('!'));
    assert!(app.pending_shell);
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
    // the host shows the layout first; only then can the question be answered
    let check = app.pending_add_checks.remove(0);
    app.on_add_checked(check, Ok("would update 'tank' to the following configuration:\n\n\ttank\n\t  mirror-0\n\t  mirror\n".to_string()));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "Enter with nothing selected does nothing");
    assert_eq!(app.handle_modal_key(KeyCode::Char('y')), None);
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter runs it");
    assert_eq!(action.call, MutationCall::AddVdev { pool: "tank".to_string(), vdev_spec: vec!["mirror".into(), "da2".into(), "da3".into()] });
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
