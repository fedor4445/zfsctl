//! Hosts: the list, the add-host form, probes and their results, the name filter, tags and the OS.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn a_mutation_the_hosts_os_cannot_serve_is_refused_before_anything_is_sent() {
    use crate::zfs::host_health::HostOs;
    use crate::ui::modal::{MutationCall, PendingAction};
    let cases = [
        (Section::Shares, MutationCall::RemoveSmbShare { dataset: "tank/x".into() }, None),
        (Section::Shares, MutationCall::RemoveSmbShare { dataset: "tank/x".into() }, Some(HostOs::Linux)),
        (Section::BootEnvs, MutationCall::CreateBootEnv { name: "be".into(), from: None }, Some(HostOs::Linux)),
    ];
    for (section, call, os) in cases {
        let mut app = App::new(vec!["local".to_string()]);
        if let Some(os) = os {
            app.set_os_for_test("local", os);
        }
        let runner = Arc::new(FakeRunner::new(""));
        app.dispatch_mutation(PendingAction { host: 0, section, call }, runner.clone());
        std::thread::sleep(Duration::from_millis(100));
        assert!(runner.calls.lock().unwrap().is_empty() && runner.unprivileged_calls.lock().unwrap().is_empty(), "{section:?} {os:?} must send nothing");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("not")), "{:?}", app.status_message);
    }
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
        mountpoint: None, quota: 0, refquota: 0,
    }];
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "snap1".to_string(), used: 1, holds: 0, written: 0 }];
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
fn right_arrow_on_hosts_is_a_noop() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.current_section(), Section::Hosts);
    app.handle_key(KeyCode::Right);
    assert_eq!(app.pane_focus, PaneFocus::Middle);
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

#[test]
fn help_documents_the_hosts_filter_key() {
    let text = help_text();
    assert!(text.contains("filter"), "{text}");
}

#[test]
fn the_type_is_cached_once_seen_and_a_failed_probe_does_not_erase_it() {
    use crate::zfs::host_health::{HostKind, HostProbe};
    let mut app = App::new(vec!["local".into(), "a".into()]);
    let reached = |kind| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind, os: None, os_name: None };
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
    app.tx.send(WorkerMsg::HostHealth("a".into(), HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: Some(HostKind::Unknown), os: None, os_name: None })).unwrap();
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
fn bang_requests_a_shell_on_the_selected_host() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.handle_key(KeyCode::Char('!'));
    assert!(app.pending_shell);
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("fbsd16")), "{:?}", app.status_message);
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
fn refresh_visible_in_the_hosts_section_does_nothing() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new(""));
    app.refresh_visible(runner);
    assert!(app.pools.is_empty() && !app.is_loading());
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

#[test]
fn auto_refresh_tracks_each_section_and_host_separately() {
    let mut app = in_section(Section::Pools);
    let t0 = Instant::now();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    // Switching to Snapshots right after must not be held back by Pools' timer.
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.auto_refresh(runner("tank/a@s1\t10\t0\t0\n"), t0 + Duration::from_secs(1), TEN);
    wait_until(&mut app, |a| !a.snapshots.is_empty());
    assert_eq!(app.snapshots.len(), 1);
}

#[test]
fn a_quiet_refresh_clears_a_stale_error_once_the_host_answers() {
    let mut app = in_section(Section::Pools);
    app.set_error("connection refused".to_string());
    app.status_error_at = Instant::now().checked_sub(Duration::from_secs(9));
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert!(app.status_message.is_none());
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
    assert_eq!(app.due_host_probes(t0), vec![0], "host 0 immediately, the others a fraction of a second apart");
    assert_eq!(app.due_host_probes(t0 + Duration::from_millis(250)), vec![0, 1]);
    assert_eq!(app.due_host_probes(t0 + Duration::from_secs(5)), vec![0, 1, 2]);
}

#[test]
fn at_most_the_limit_of_probes_run_at_once_however_many_hosts_are_due() {
    let limit = crate::daemon::workers::PARALLEL;
    let mut app = App::new((0..limit * 3).map(|i| format!("h{i}")).collect());
    let late = app.probe_epoch + Duration::from_hours(1);
    assert_eq!(app.due_host_probes(late).len(), limit, "a big fleet is asked a few hosts at a time");
    for name in app.hosts.clone().iter().take(limit - 1) {
        app.host_probes.entry(name.clone()).or_default().in_flight = true;
    }
    assert_eq!(app.due_host_probes(late).len(), 1, "one place free: one more host");
    app.host_probes.entry(app.hosts[limit].clone()).or_default().in_flight = true;
    assert!(app.due_host_probes(late).is_empty(), "no place free: none");
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
fn a_probe_that_never_answers_is_flagged_instead_of_showing_its_last_good_result() {
    let mut app = probes_app();
    let healthy = crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os: None, os_name: None };
    let long_ago = Instant::now().checked_sub(Duration::from_secs(120)).unwrap();
    app.host_probes.insert("fbsd16".to_string(), HostProbeState { last: Some(healthy.clone()), in_flight: true, finished_at: Some(long_ago), started_at: Some(long_ago), os: None, os_name: None, kind: None, resources: vec![], utc_offset_secs: None });
    let a = app.host_assessment(1);
    assert_eq!(a.level, crate::zfs::host_health::Level::Warning);
    assert!(a.reason.starts_with("no answer for "), "{}", a.reason);
    app.host_probes.insert("fbsd16t1".to_string(), HostProbeState { last: Some(healthy), in_flight: true, finished_at: Some(long_ago), started_at: Some(Instant::now()), os: None, os_name: None, kind: None, resources: vec![], utc_offset_secs: None });
    assert_eq!(app.host_assessment(2).reason, "ok", "a probe that has only just started is normal");
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
    const SYSCTL: &str = "4\n21474836480\n4096\n524288\n786432\n10737418240\n{ 0.05 0.12 0.08 }\n1000 0 200 10 8790\n9990\n10\n0\n0\n0\n";
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

fn healthy_runner() -> std::sync::Arc<crate::exec::test_support::FakeRunner> {
    std::sync::Arc::new(
        crate::exec::test_support::FakeRunner::new("")
            .route("smartctl", "--scan", include_str!("../../../smart_fixtures/scan.json"))
            .route("smartctl", "-j", include_str!("../../../smart_fixtures/hdd_ok.json"))
            .route("service", "nfsd", "nfsd is running as pid 1.\n")
            .route("service", "samba_server", "smbd is running as pid 2.\n")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("cat", crate::zfs::smb::CONF_PATH, "[global]\n"),
    )
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
    assert!(crate::daemon::storage_check::StorageFile::load(&dir.path().join("storage.json")).hosts.contains_key("fbsd16"), "`c` also refreshes the quota reading");
    assert!(crate::daemon::checks::read_marker(dir.path()).is_none(), "the marker is gone when the check is done");
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
fn the_apps_host_os_follows_the_last_probe_that_detected_it() {
    use crate::zfs::host_health::{HostOs, HostProbe};
    let mut app = App::new(vec!["local".to_string(), "ub24".to_string()]);
    assert_eq!(app.host_os(1), None, "not probed yet");
    let reached = |os| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os, os_name: None };
    app.tx.send(WorkerMsg::HostHealth("ub24".into(), reached(Some(HostOs::Linux)))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_os(1), Some(HostOs::Linux));
    app.tx.send(WorkerMsg::HostHealth("ub24".into(), reached(None))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.host_os(1), Some(HostOs::Linux), "a later probe that did not ask does not make it forget");
    assert_eq!(app.host_os(0), None);
    assert_eq!(app.host_os(9), None, "no such host");
}

#[test]
fn a_log_add_confirms_plainly_and_asks_the_host_for_the_layout() {
    let mut app = add_from_disks(&["da4"], "log", "");
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("zpool add tank log da4") && prompt.contains("Checking the resulting layout"), "{prompt}");
    assert!(prompt.contains("can be removed again") && !prompt.contains("cannot be removed"), "a log is removable: {prompt}");
    assert_eq!(app.pending_add_checks.len(), 1);
    assert_eq!((app.pending_add_checks[0].pool.as_str(), app.pending_add_checks[0].spec.clone(), app.pending_add_checks[0].role), ("tank", vec!["log".to_string(), "da4".to_string()], crate::zfs::vdev_role::Role::Log));
}

#[test]
fn a_host_that_refuses_the_dry_run_gives_the_form_back_with_its_own_message() {
    let mut app = add_from_disks(&["da4"], "log", "");
    submit_to_confirm(&mut app);
    let check = app.pending_add_checks.remove(0);
    app.on_add_checked(check, Err("command 'zpool add -n tank log da4' failed with status 1: /dev/da4 is part of active pool 'other'".to_string()));
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("part of active pool")), "{:?}", app.status_error);
}

#[test]
fn y_then_enter_waits_until_the_host_has_shown_the_layout() {
    let mut app = add_from_disks(&["da4"], "log", "");
    submit_to_confirm(&mut app);
    app.handle_modal_key(KeyCode::Char('y'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "no layout yet: the add does not go");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Waiting for the host")), "{:?}", app.status_message);
    assert!(matches!(app.modal, Some(Modal::Confirm { choice: Some(true), .. })), "the y stays selected");
    let check = app.pending_add_checks.remove(0);
    app.on_add_checked(check, Ok("would update 'tank'".to_string()));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some(), "once the layout is on screen, Enter sends it");
}

#[test]
fn a_host_that_never_answers_the_layout_check_gives_the_form_back_after_the_deadline() {
    struct Hang;
    impl CommandRunner for Hang {
        fn run(&self, _: &str, _: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            std::thread::sleep(Duration::from_secs(3));
            Ok(crate::exec::CommandOutput { stdout: String::new(), stderr: String::new(), status: 0 })
        }
        fn run_with_stdin(&self, c: &str, a: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
        fn run_unprivileged(&self, c: &str, a: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
            self.run(c, a)
        }
    }
    let mut app = add_from_disks(&["da4"], "log", "");
    submit_to_confirm(&mut app);
    let check = app.pending_add_checks.remove(0);
    app.start_add_check_with(check, Arc::new(Hang), Duration::from_millis(100));
    let end = Instant::now() + Duration::from_secs(2);
    while matches!(app.modal, Some(Modal::Confirm { .. })) && Instant::now() < end {
        app.drain_worker_messages();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("did not answer")), "{:?}", app.status_error);
}

#[test]
fn a_refusal_with_several_lines_is_one_status_line_and_a_repeated_ask_is_told_apart_by_its_id() {
    let mut app = add_from_disks(&["da4"], "log", "");
    submit_to_confirm(&mut app);
    let first = app.pending_add_checks.remove(0);
    // decline and submit the same thing again: a new ask with a new id
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    app.handle_modal_key(KeyCode::Enter);
    let second = app.pending_add_checks.remove(0);
    assert_ne!(first.id, second.id);
    app.on_add_checked(first, Err("stale".to_string()));
    assert!(matches!(app.modal, Some(Modal::Confirm { .. })), "the first ask's answer must not touch the second confirm");
    app.on_add_checked(second, Err("invalid vdev specification\nuse '-f' to override the following errors:\nmismatched replication level".to_string()));
    assert_eq!(app.status_error.as_deref(), Some("invalid vdev specification | use '-f' to override the following errors: | mismatched replication level"));
}

fn version_of(app: &mut App, version: &str) {
    app.storage_check.hosts.insert("local".into(), crate::daemon::storage_check::StorageResult { checked_at: chrono::Utc::now(), datasets: vec![], seen: vec![], pools: vec![], zfs_version: Some(version.into()), locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None });
}

#[test]
fn an_unknown_value_keeps_the_form_open_with_the_list_and_an_old_host_refuses_blake3() {
    let mut app = integrity_app("on", "off");
    open_property(&mut app, "checksum");
    assert!(submit_value(&mut app, "sha3").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })) && app.status_error.as_deref().is_some_and(|m| m.contains("fletcher4")), "{:?}", app.status_error);
    version_of(&mut app, "2.1.14-1");
    assert!(submit_value(&mut app, "blake3").is_none());
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("2.2") && m.contains("2.1")), "{:?}", app.status_error);
    version_of(&mut app, "2.4.2-1-FreeBSD_gabc");
    assert!(submit_value(&mut app, "blake3").is_none());
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Confirm { .. })), "{:?}", app.modal);
}

#[test]
fn capital_h_message_cleans_foreign_tags_and_drops_ones_that_are_empty_after_cleaning() {
    let mut app = hold_app(2, Some(&["a\u{1b}[31mb\u{202e}", "\u{1b}\u{7}", "keep"]));
    app.handle_key(KeyCode::Char('H'));
    assert_eq!(app.status_message.as_deref(), Some("held by another tag: a [31mb, keep; zfsctl will not release it"));
}

#[test]
fn the_release_prompt_says_what_becomes_possible() {
    let mut app = hold_app(1, Some(&["zfsctl"]));
    app.handle_key(KeyCode::Char('H'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { prompt, expected, .. }) => {
            assert!(prompt.contains("'tank/data@s1' — it can then be destroyed and pruned again"), "{prompt}");
            assert_eq!(expected, "tank/data@s1");
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

fn wait_for_snapshots(app: &mut App) {
    for _ in 0..300 {
        app.drain_worker_messages();
        if app.snapshots_loaded && !app.is_refreshing(Section::Snapshots, 0) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.snapshots_loaded);
}

#[test]
fn a_refresh_while_a_tag_fetch_runs_starts_no_second_fetch() {
    let runner = Arc::new(FakeRunner::new("").route("zfs", "list", "tank/data@s1\t10\t1\t0\n").route("zfs", "holds", "tank/data@s1\tzfsctl\tx\n"));
    let dynrunner: Arc<dyn CommandRunner> = runner.clone();
    let mut app = App::new(vec!["local".to_string()]);
    app.tags_running.lock().unwrap().insert(0);
    app.refresh_snapshots(dynrunner);
    wait_for_snapshots(&mut app);
    std::thread::sleep(std::time::Duration::from_millis(100));
    app.drain_worker_messages();
    assert_eq!(runner.calls.lock().unwrap().iter().filter(|c| c.1[0] == "holds").count(), 0);
    assert!(app.snapshot_tags.is_empty());
    assert!(app.tags_running.lock().unwrap().contains(&0), "the running fetch's mark is not cleared by the skipped one");
}

#[test]
fn the_tag_fetch_mark_is_cleared_when_the_tags_arrive() {
    let runner = Arc::new(FakeRunner::new("").route("zfs", "list", "tank/data@s1\t10\t1\t0\n").route("zfs", "holds", "tank/data@s1\tzfsctl\tx\n"));
    let dynrunner: Arc<dyn CommandRunner> = runner.clone();
    let mut app = App::new(vec!["local".to_string()]);
    app.refresh_snapshots(dynrunner);
    for _ in 0..300 {
        app.drain_worker_messages();
        if !app.snapshot_tags.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.snapshot_tags.is_empty());
    assert!(app.tags_running.lock().unwrap().is_empty());
}

#[test]
fn tags_from_another_host_are_ignored() {
    let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
    app.tx.send(WorkerMsg::SnapshotTags(1, 0, std::collections::HashMap::from([("tank/data@s1".to_string(), vec!["zfsctl".to_string()])]))).unwrap();
    app.drain_worker_messages();
    assert!(app.snapshot_tags.is_empty());
}

#[test]
fn a_panicking_tag_fetch_still_clears_the_per_host_mark() {
    let mut app = App::new(vec!["local".to_string()]);
    app.refresh_snapshots(Arc::new(PanickingList));
    for _ in 0..300 {
        app.drain_worker_messages();
        if app.snapshots_loaded && app.tags_running.lock().unwrap().is_empty() && !app.is_refreshing(Section::Snapshots, 0) {
            std::thread::sleep(std::time::Duration::from_millis(50));
            app.drain_worker_messages();
            if app.tags_running.lock().unwrap().is_empty() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(app.snapshots_loaded);
    assert!(app.tags_running.lock().unwrap().is_empty());
    assert!(app.snapshot_tags.is_empty());
}

#[test]
fn a_foreign_tag_with_control_characters_is_shown_readably_in_the_status() {
    let mut app = hold_app(1, Some(&["a\u{1b}[31mb\u{202e}c"]));
    app.handle_key(KeyCode::Char('H'));
    let msg = app.status_message.clone().unwrap();
    assert!(!msg.chars().any(|c| c.is_control() || c == '\u{202e}'), "{msg:?}");
    assert_eq!(msg, "held by another tag: a [31mb c; zfsctl will not release it");
}

#[test]
fn a_very_long_foreign_tag_is_truncated_in_the_status() {
    let long = "x".repeat(200);
    let mut app = hold_app(1, Some(&[long.as_str()]));
    app.handle_key(KeyCode::Char('H'));
    let msg = app.status_message.clone().unwrap();
    assert!(msg.contains(&format!("{}…", "x".repeat(39))), "{msg:?}");
    assert!(msg.chars().count() < 100, "{msg:?}");
}

#[test]
fn a_result_for_an_older_request_or_another_host_is_ignored() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    let request = app.changes_view.as_ref().unwrap().request;
    deliver(&mut app, 0, request + 99, Ok(crate::ui::changes::ChangesOutcome::NoPrevious));
    assert!(matches!(app.changes_view.as_ref().unwrap().phase, crate::ui::changes::Phase::Reading));
    deliver(&mut app, 5, request, Ok(crate::ui::changes::ChangesOutcome::NoPrevious));
    assert!(matches!(app.changes_view.as_ref().unwrap().phase, crate::ui::changes::Phase::Reading));
    deliver(&mut app, 0, request, Ok(crate::ui::changes::ChangesOutcome::NoPrevious));
    assert!(matches!(app.changes_view.as_ref().unwrap().phase, crate::ui::changes::Phase::NoPrevious));
}

#[test]
fn a_view_of_another_host_is_dropped_on_the_next_key() {
    let mut app = ready_app();
    app.selected_host = 3;
    app.handle_key(KeyCode::Down);
    assert!(app.changes_view.is_none());
}

#[test]
fn typing_in_the_filter_prompt_is_not_taken_for_commands() {
    let mut app = browsing_app();
    app.handle_key(KeyCode::Char('/'));
    for c in "qd?f".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert!(!app.should_quit && app.modal.is_none(), "letters typed into the prompt are text");
    assert_eq!(app.browse_view.as_ref().unwrap().input.as_ref().map(|i| i.text.as_str()), Some("qd?f"));
}

#[test]
fn f_before_the_host_has_been_probed_asks_to_wait_instead_of_guessing_the_operating_system() {
    let mut app = changes_app();
    app.host_probes.clear();
    app.handle_key(KeyCode::Char('f'));
    assert!(app.browse_view.is_none() && app.pending_browse.is_empty(), "nothing is sent to a host of unknown type");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("not been probed")), "{:?}", app.status_message);
}
