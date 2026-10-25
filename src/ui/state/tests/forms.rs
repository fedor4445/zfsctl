//! Forms, confirmations and pickers: how a modal takes keys and what it hands back.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn handle_key_s_with_no_row_selected_does_not_open_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('s'));
    assert!(app.modal.is_none());
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
            assert!(body.contains("ZFS storage on FreeBSD"), "{body}");
            assert!(crate::ui::logo::wordmark().iter().all(|line| body.contains(line.as_str())), "the wordmark is on the About screen: {body}");
        }
        other => panic!("expected Info modal, got {other:?}"),
    }
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
fn a_default_blank_form_is_not_guest_and_builds_no_guest_ok() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["any", "", "", "n", "n"]).unwrap();
    let MutationCall::SetSmbShare { ref share, password: None } = action.call else { panic!("{:?}", action.call) };
    assert!(!share.guest_ok);
    assert!(!crate::zfs::smb::render_with_shares("", std::slice::from_ref(share)).unwrap().contains("guest ok"));
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
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
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
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
fn submitting_any_form_with_invalid_input_sets_a_status_message_explaining_why() {
    // A live session hit this directly: typing a bare username
    // ("fb") into the ACL Recipient field instead of "user:fb"
    // made Enter silently do nothing, with no visible explanation
    // -- indistinguishable from the UI being frozen. This is the
    // shared fallback every form's failed submission goes through
    // (not ACL-specific), so the fix and its test live here.
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
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
fn a_bad_form_is_refused_with_the_reason() {
    let now = at_utc("2026-10-06T12:00:00Z");
    let try_it = |values: [&str; 4]| build_job_action(&job_kind(None, &[]), &fields(values), std::path::Path::new("/s"), now).unwrap_err();
    assert!(try_it(["nowhere", "tank", "0 3 * * 0", "y"]).contains("Unknown host 'nowhere'"));
    assert!(try_it(["local", "  ", "0 3 * * 0", "y"]).contains("Pool name is required"));
    assert!(try_it(["local", "tank", "61 * * * *", "y"]).contains("outside 0-59"));
    assert!(try_it(["local", "tank", "0 3 * * 0", "maybe"]).contains("y or n"));
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
fn n_opens_the_create_form_listing_existing_names_for_the_picker() {
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('n'));
    assert_eq!(form_values(&app), ["", ""]);
    let Some(Modal::Form { kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(*kind, FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string(), "spare".to_string()] });
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
fn help_explains_the_two_step_confirmation() {
    let text = help_text();
    assert!(text.contains("takes two keys") && text.contains("Enter alone does nothing"), "{text}");
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

#[test]
fn f5_does_nothing_in_other_forms() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: vec![FormField { label: "x".into(), value: "/r".into() }], focused: 0, description: None, kind: FormKind::CreateDataset { host: 0 } });
    app.handle_modal_key(KeyCode::F(5));
    assert!(app.pending_preflight.is_empty() && app.backup_preflight.is_none());
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
fn the_dry_run_answer_puts_the_layout_into_the_confirm_and_a_special_stays_irreversible() {
    let mut app = add_from_disks(&["da4", "da5"], "special", "");
    let prompt = submit_to_confirm(&mut app);
    assert!(prompt.contains("cannot be removed again"), "{prompt}");
    let check = app.pending_add_checks.remove(0);
    app.on_add_checked(check, Ok("would update 'tank' to the following configuration:\n\n\ttank  ONLINE\n\t  special\n\t    mirror".to_string()));
    match &app.modal {
        Some(Modal::Confirm { prompt, choice: None, .. }) => assert!(prompt.contains("would update 'tank'") && prompt.contains("special") && !prompt.contains("Checking the resulting"), "{prompt}"),
        other => panic!("{other:?}"),
    }
    // declining still returns to the form
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "declining after the layout arrived still gives the form back: {:?}", app.modal);
}

#[test]
fn a_dry_run_answer_for_a_confirm_that_is_gone_is_ignored() {
    let mut app = add_from_disks(&["da4"], "log", "");
    submit_to_confirm(&mut app);
    let check = app.pending_add_checks.remove(0);
    app.modal = None;
    app.on_add_checked(check.clone(), Err("late".to_string()));
    assert!(app.modal.is_none() && app.status_error.is_none(), "a late refusal must not reopen a form the user left");
    // another confirm for a different add is not touched either
    let mut other = add_from_disks(&["da9"], "cache", "");
    submit_to_confirm(&mut other);
    other.on_add_checked(check, Ok("layout".to_string()));
    match &other.modal {
        Some(Modal::Confirm { prompt, .. }) => assert!(prompt.contains("Checking the resulting layout"), "{prompt}"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn capital_x_on_a_log_cache_or_spare_row_asks_y_n_with_the_command_and_removes_it() {
    let mut app = App::new(vec!["local".to_string()]);
    pool_for_forms(&mut app);
    let line = |depth, name: &str| VdevLine { depth, name: name.to_string(), health: crate::zfs::pool::PoolHealth::Online };
    app.vdevs = vec![line(0, "tank"), line(1, "da0"), line(0, "logs"), line(1, "da4"), line(0, "cache"), line(1, "da5")];
    app.vdevs_table.select(Some(3));
    app.handle_key(KeyCode::Char('X'));
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("zpool remove tank da4") && prompt.contains("log device"), "{prompt}");
    assert_eq!(app.handle_modal_key(KeyCode::Char('y')), None);
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter sends it");
    assert_eq!(action.call, MutationCall::RemoveVdev { pool: "tank".into(), device: "da4".into() });
}

#[test]
fn capital_x_on_a_data_device_says_it_is_not_offered_and_the_hint_follows_the_row() {
    let mut app = App::new(vec!["local".to_string()]);
    pool_for_forms(&mut app);
    let line = |depth, name: &str| VdevLine { depth, name: name.to_string(), health: crate::zfs::pool::PoolHealth::Online };
    app.vdevs = vec![line(0, "tank"), line(1, "da0"), line(0, "logs"), line(1, "da4")];
    app.vdevs_table.select(Some(1));
    let keys = |a: &App| crate::ui::hints::hints(a).iter().map(|h| h.key.to_string()).collect::<Vec<_>>();
    assert!(!keys(&app).contains(&"X".to_string()));
    app.handle_key(KeyCode::Char('X'));
    assert!(app.modal.is_none() && app.status_message.as_deref().is_some_and(|m| m.contains("log, cache or spare")), "{:?}", app.status_message);
    app.vdevs_table.select(Some(3));
    assert!(keys(&app).contains(&"X".to_string()));
}

#[test]
fn special_small_blocks_is_a_guided_form_that_warns_when_it_reaches_the_recordsize() {
    let mut app = quota_app(false, 0, 0, 0, 0);
    app.dataset_properties.push(Property { name: "special_small_blocks".into(), value: "0".into(), source: PropertySource::Default });
    app.dataset_properties.push(Property { name: "recordsize".into(), value: "131072".into(), source: PropertySource::Default });
    open_property(&mut app, "special_small_blocks");
    let Some(crate::ui::modal::Modal::Form { fields, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(fields[0].value, "0");
    assert!(submit_value(&mut app, "3K").is_none() && app.status_error.as_deref().is_some_and(|m| m.contains("power of two")), "{:?}", app.status_error);
    assert!(submit_value(&mut app, "32K").is_none());
    let action = confirm_limit_change(&mut app);
    assert_eq!(action.call, MutationCall::SetDatasetProperty { dataset: "tank/data".into(), key: "special_small_blocks".into(), value: "32768".into() });
    open_property(&mut app, "special_small_blocks");
    assert!(submit_value(&mut app, "128K").is_none());
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { prompt, .. }) => assert!(prompt.contains("ALL of this dataset's data"), "{prompt}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_bad_load_form_stays_open_and_a_cancelled_one_is_gone() {
    let mut app = key_app(true, "passphrase");
    app.handle_key(KeyCode::Char('L'));
    app.handle_modal_key(KeyCode::Enter); // to the last field
    app.handle_modal_key(KeyCode::Enter); // submit
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "an empty passphrase keeps the form: {:?}", app.modal);
    assert!(app.status_is_error() && app.status_message.as_deref().unwrap_or("").contains("passphrase"), "{:?}", app.status_message);
    app.handle_modal_key(KeyCode::Esc);
    assert!(app.modal.is_none());
}

#[test]
fn unloading_a_key_needs_y_then_enter_and_says_what_it_means() {
    let mut app = key_app(false, "passphrase");
    app.handle_key(KeyCode::Char('U'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("zfs unload-key tank/s") && prompt.contains("cannot be read") && prompt.contains("mounted"), "{prompt}");
    assert_eq!(action.call, MutationCall::UnloadKey { dataset: "tank/s".into() });
    assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "Enter alone does nothing");
    app.handle_modal_key(KeyCode::Char('y'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_some());
}

#[test]
fn changing_a_key_checks_the_repeat_then_asks_with_the_warnings() {
    let mut app = key_app(false, "passphrase");
    app.handle_key(KeyCode::Char('C'));
    type_into(&mut app, "new passphrase");
    app.handle_modal_key(KeyCode::Tab);
    type_into(&mut app, "new passphrasX");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
    assert!(app.status_message.as_deref().unwrap_or("").contains("do not match"), "{:?}", app.status_message);
    let Some(Modal::Form { fields, .. }) = &app.modal else { panic!() };
    assert!(fields.iter().all(|f| f.value.is_empty()), "a mismatch clears both blind fields");
    type_into(&mut app, "new passphrase");
    app.handle_modal_key(KeyCode::Tab);
    type_into(&mut app, "new passphrase");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    let Some(Modal::Confirm { prompt, action, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    for want in ["zfs change-key tank/s", "old passphrase stops", "not re-encrypted", "sent elsewhere", "Keep the new"] {
        assert!(prompt.contains(want), "'{want}' missing: {prompt}");
    }
    assert!(!prompt.contains("new passphrase"), "the confirm must not show the passphrase");
    assert!(matches!(&action.call, MutationCall::ChangeKey { passphrase, .. } if passphrase.0 == "new passphrase"));
    // Declining hands the form back, so nothing typed is lost.
    app.handle_modal_key(KeyCode::Char('n'));
    app.handle_modal_key(KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
}

#[test]
fn a_new_key_must_be_eight_characters() {
    let mut app = key_app(false, "passphrase");
    app.handle_key(KeyCode::Char('C'));
    type_into(&mut app, "short");
    app.handle_modal_key(KeyCode::Tab);
    type_into(&mut app, "short");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(app.status_message.as_deref().unwrap_or("").contains("8 characters"), "{:?}", app.status_message);
}

#[test]
fn r_on_a_regular_file_opens_the_destination_form_with_the_default_name() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { title, fields, kind, .. }) => {
            assert_eq!(title, "Restore as a copy");
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].label, "Restore as");
            assert_eq!(fields[0].value, "a.restored-s2.txt");
            assert!(matches!(kind, crate::ui::modal::FormKind::RestoreFile { .. }));
        }
        other => panic!("expected the form, got {other:?}"),
    }
}

#[test]
fn r_on_a_folder_a_symlink_an_ambiguous_name_or_while_reading_does_nothing() {
    use crate::zfs::snapshot_browse::EntryKind;
    let mut folder = browsing_app();
    folder.handle_key(KeyCode::Char('R'));
    assert!(folder.modal.is_none() && folder.status_message.is_none(), "folder");
    let mut link = browsing_on_file();
    link.browse_view.as_mut().unwrap().rows[1].entry.kind = EntryKind::Symlink;
    link.handle_key(KeyCode::Char('R'));
    assert!(link.modal.is_none() && link.status_message.is_none(), "symlink");
    let mut ambiguous = browsing_on_file();
    ambiguous.browse_view.as_mut().unwrap().rows[1].entry.ambiguous = true;
    ambiguous.handle_key(KeyCode::Char('R'));
    assert!(ambiguous.modal.is_none() && ambiguous.status_message.is_none(), "ambiguous");
    let mut odd = browsing_on_file();
    odd.browse_view.as_mut().unwrap().rows[1].entry.name = "a\nb".to_string();
    odd.handle_key(KeyCode::Char('R'));
    assert!(odd.modal.is_none() && odd.status_message.is_none(), "unusual name");
    let mut reading = changes_app();
    reading.handle_key(KeyCode::Char('f'));
    reading.handle_key(KeyCode::Char('R'));
    assert!(reading.modal.is_none() && reading.status_message.is_none(), "still reading");
}

#[test]
fn a_name_the_engine_refuses_shows_its_sentence_and_opens_no_confirm() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    type_into_form(&mut app, "a.txt");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert_eq!(app.status_message.as_deref(), Some("choose a different name: the restore never replaces the original"));
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })), "the form stays open with what was typed");
    type_into_form(&mut app, "x/y");
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.starts_with("that name cannot be used")), "{:?}", app.status_message);
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })));
}

#[test]
fn submitting_the_form_opens_a_confirm_that_shows_source_destination_and_command() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    let prompt = confirm_prompt(&app);
    assert!(prompt.starts_with("Restore as a copy\n"), "{prompt}");
    assert!(prompt.contains("From:  /tank/data/.zfs/snapshot/s2/a.txt"), "{prompt}");
    assert!(prompt.contains("To:    /tank/data/a.restored-s2.txt"), "{prompt}");
    assert!(prompt.contains("Command: cp -p -n -- /tank/data/.zfs/snapshot/s2/a.txt /tank/data/a.restored-s2.txt"), "{prompt}");
    assert!(!prompt.contains("Note:"), "{prompt}");
    assert!(prompt.contains("Size: 5   Mode: drwxr-xr-x"), "{prompt}");
    assert!(prompt.contains("This creates one new file next to the original and never replaces anything."), "{prompt}");
    assert!(prompt.contains("Existing files are never overwritten; if the name is taken nothing is copied."), "{prompt}");
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!((action.host, action.section), (0, Section::Snapshots));
            assert!(matches!(&action.call, crate::ui::modal::MutationCall::RestoreFile { host, .. } if host == &app.hosts[0]));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_name_with_spaces_is_kept_as_typed() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    type_into_form(&mut app, " my copy ");
    app.handle_modal_key(KeyCode::Enter);
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { action: PendingAction { call: crate::ui::modal::MutationCall::RestoreFile { plan, .. }, .. }, .. }) => assert_eq!(plan.dest_name, " my copy "),
        other => panic!("{other:?}"),
    }
}
