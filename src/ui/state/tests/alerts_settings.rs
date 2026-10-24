//! Alerts and Settings: the forms, the saved note, the test message and the preview.
#![allow(clippy::wildcard_imports)]
use super::*;

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

fn set_form_values(app: &mut App, values: [&str; 4]) {
    let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
    for (field, value) in fields.iter_mut().zip(values) {
        field.value = value.to_string();
    }
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
fn a_replication_save_that_does_not_arm_force_does_not_ask() {
    assert_eq!(job_save_prompt(None, &repl_job(false), None), None);
    assert_eq!(job_save_prompt(Some(&repl_job(true)), &repl_job(true), None), None, "force was already on and nothing changed");
    assert_eq!(job_save_prompt(Some(&repl_job(true)), &repl_job(false), None), None, "turning force off is safe");
    let mut moved = repl_job(true);
    moved.dest_dataset = Some("other/data".into());
    assert!(job_save_prompt(Some(&repl_job(true)), &moved, None).is_some(), "forcing into a different destination is new");
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
fn a_failed_listing_refuses_the_save_with_the_reason_and_keeps_the_form() {
    let mut app = keep_form_app("3");
    app.handle_modal_key(KeyCode::Enter);
    answer_prune_check(&mut app, Err(crate::error::AppError::Parse("boom".into())));
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert!(app.pending_mutations.is_empty());
    assert!(app.status_message.clone().unwrap_or_default().contains("boom"));
}

#[test]
fn a_replication_or_mirror_of_an_encrypted_dataset_is_not_saved() {
    let mut app = keep_form_app("");
    encrypted(&mut app, "tank/data", 0);
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "the form stays open: {:?}", app.modal);
    assert!(app.status_is_error() && app.status_message.as_deref().unwrap_or("").contains("tank/data is encrypted"), "{:?}", app.status_message);

    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: mirror_fields(MIRROR_GOOD).into_iter().collect(), focused: 0, description: None, kind: mirror_kind(None, vec![]) });
    encrypted(&mut app, "tank/data", 0);
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "{:?}", app.modal);
}

#[test]
fn a_disabled_job_on_an_encrypted_dataset_can_still_be_saved() {
    let mut app = keep_form_app("");
    encrypted(&mut app, "tank/data", 0);
    let mut existing = repl_job(false);
    existing.enabled = false;
    existing.host = "local".into();
    existing.dataset = Some("tank/data".into());
    let Some(Modal::Form { kind, .. }) = app.modal.as_mut() else { panic!() };
    *kind = repl_kind(Some(existing), vec![]);
    app.handle_modal_key(KeyCode::Enter);
    assert!(!matches!(app.modal, Some(Modal::Form { .. })), "{:?} {:?}", app.modal, app.status_message);
}

#[test]
fn the_save_time_guard_only_trusts_a_list_read_for_the_jobs_own_host_and_never_blocks_without_one() {
    let mut app = keep_form_app("");
    encrypted(&mut app, "tank/data", 1);
    app.handle_modal_key(KeyCode::Enter);
    assert!(!matches!(app.modal, Some(Modal::Form { .. })), "another host's list says nothing about this job: {:?}", app.modal);
    let mut app = keep_form_app("");
    app.handle_modal_key(KeyCode::Enter);
    assert!(!matches!(app.modal, Some(Modal::Form { .. })), "no list loaded: the run-time guard decides");
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
fn e_opens_the_encrypted_dataset_form_with_the_default_cipher_and_two_masked_fields() {
    let mut app = key_app(false, "passphrase");
    app.handle_key(KeyCode::Char('E'));
    let Some(Modal::Form { fields, kind, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert_eq!(fields[1].value, "aes-256-gcm");
    assert!((0..4).map(|i| kind.masks_field(i)).eq([false, false, true, true]));
}
