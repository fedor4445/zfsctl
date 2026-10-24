//! Moving around: sections, focus, the keys that open help, about and a shell.
#![allow(clippy::wildcard_imports)]
use super::*;

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
fn esc_clears_existing_status_message() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("something went wrong".to_string());
    app.handle_key(KeyCode::Esc);
    assert_eq!(app.status_message, None);
}

#[test]
fn failed_submission_resets_focus_to_the_first_field_so_an_invalid_earlier_field_is_visible_without_tabbing_through_everything() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
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
fn help_text_documents_the_shell_key() {
    assert!(help_text().contains("  !  "), "{}", help_text());
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
fn help_documents_recursive_destroy() {
    assert!(help_text().contains("D                destroy a dataset AND its child datasets and snapshots"), "{}", help_text());
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
fn a_dimmed_section_runs_no_command_when_refreshed_and_shows_no_stale_rows() {
    let mut app = two_os_app(crate::zfs::host_health::HostOs::Linux);
    app.selected_host = 1;
    app.bootenvs = vec![be("left-from-the-hub", true, true, false, None)];
    let runner: std::sync::Arc<dyn crate::exec::CommandRunner> = std::sync::Arc::new(NoCalls);
    app.refresh_bootenvs(runner.clone());
    app.refresh_dataset_acl(runner, "/tank/x".to_string());
    std::thread::sleep(Duration::from_millis(100));
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty(), "the other host's rows are gone");
    assert!(app.acl_unsupported().is_none() || app.acl_unsupported().is_some_and(|t| t.contains("Linux")));
}

#[test]
fn help_has_a_blank_line_between_key_lines() {
    let text = help_text();
    let lines: Vec<&str> = text.lines().collect();
    for pair in lines.windows(2) {
        assert!(!(pair[0].starts_with("  ") && pair[1].starts_with("  ")), "no blank line between {:?} and {:?}", pair[0], pair[1]);
    }
    assert!(text.contains("switch sections\n\n  Up / Down"), "{text}");
    assert!(!text.contains("Global\n\n"), "a heading is followed directly by its first key");
}

#[test]
fn an_info_popup_scrolls_with_the_navigation_keys_and_any_other_key_closes_it() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Info { title: "Help".to_string(), body: "a\nb\nc".to_string() });
    app.handle_modal_key(KeyCode::Down);
    assert_eq!(app.info_scroll, 1);
    assert!(app.modal.is_some());
    app.handle_modal_key(KeyCode::PageDown);
    assert_eq!(app.info_scroll, 2, "never past the last line");
    app.handle_modal_key(KeyCode::Up);
    assert_eq!(app.info_scroll, 1);
    app.handle_modal_key(KeyCode::Home);
    assert_eq!(app.info_scroll, 0);
    app.handle_modal_key(KeyCode::End);
    assert_eq!(app.info_scroll, 2);
    app.handle_modal_key(KeyCode::Char('x'));
    assert!(app.modal.is_none());
    assert_eq!(app.info_scroll, 0, "the next pop-up starts at the top");
}

#[test]
fn esc_in_an_open_group_returns_to_the_groups_and_then_closes_the_view() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Enter);
    assert!(app.changes_view.as_ref().unwrap().open_group.is_some());
    app.handle_key(KeyCode::Esc);
    assert!(app.changes_view.as_ref().is_some_and(|v| v.open_group.is_none()));
    app.handle_key(KeyCode::Esc);
    assert!(app.changes_view.is_none());
}

#[test]
fn up_down_select_and_the_brackets_change_the_depth_inside_the_limits() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Down);
    assert_eq!(app.changes_view.as_ref().unwrap().selected, 1);
    app.handle_key(KeyCode::Up);
    assert_eq!(app.changes_view.as_ref().unwrap().selected, 0);
    app.handle_key(KeyCode::Char(']'));
    assert_eq!(app.changes_view.as_ref().unwrap().depth, 3);
    for _ in 0..5 {
        app.handle_key(KeyCode::Char(']'));
    }
    assert_eq!(app.changes_view.as_ref().unwrap().depth, 4);
    for _ in 0..9 {
        app.handle_key(KeyCode::Char('['));
    }
    assert_eq!(app.changes_view.as_ref().unwrap().depth, 1);
}

#[test]
fn help_and_quit_keep_working_inside_the_view() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Char('?'));
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Info { .. })));
    app.modal = None;
    app.handle_key(KeyCode::Char('q'));
    assert!(app.should_quit);
}
