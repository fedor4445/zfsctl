//! Jobs: the Jobs section and the scrub, snapshot and replication job forms.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn n_then_enter_cancels_and_n_alone_does_not_close() {
    let (mut app, _) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Char('n')), None);
    assert_eq!(choice_of(&app), Some(Some(false)), "n arms no and keeps the window open");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    assert!(app.modal.is_none(), "Enter after n closes it without running anything");
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
fn handle_modal_key_form_esc_cancels() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Char('n'));
    let result = app.handle_modal_key(KeyCode::Esc);
    assert_eq!(result, None);
    assert!(app.modal.is_none());
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
fn n_in_jobs_opens_a_blank_snapshot_form_with_sensible_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('N'));
    assert_eq!(form_values(&app), ["local", "", "n", "auto", "24", "0 * * * *", "y"]);
}

#[test]
fn enter_on_a_snapshot_job_edits_it_in_the_snapshot_form() {
    let (mut app, _dir) = app_with_jobs(&[crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 7)]);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(form_values(&app), ["local", "tank/data", "n", "hourly", "7", "0 * * * *", "y"]);
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
fn the_snapshot_form_refuses_a_prefix_another_job_already_uses_on_that_dataset() {
    let other = crate::jobs::Job::test_snapshot("other", "tank/data", "hourly", 5);
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![other.clone()]).contains("other"));
    // Editing that same job is fine, as is a different prefix.
    let kind = snapshot_kind(Some(other.clone()), vec![other]);
    assert!(build_snapshot_job_action(&kind, &snap_fields(["local", "tank/data", "n", "hourly", "9", "0 * * * *", "y"]), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
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

fn repl_err(values: [&str; 8], jobs: Vec<crate::jobs::Job>) -> String {
    build_replicate_job_action(&repl_kind(None, jobs), &repl_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
}

#[test]
fn big_r_in_jobs_opens_a_blank_replication_form_with_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('R'));
    assert_eq!(form_values(&app), ["local", "", "auto", "", "", "n", "30 * * * *", "y", "", "n"]);
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
    assert_eq!(form_values(&app), ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y", "", "n"]);
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
fn big_b_outside_jobs_does_nothing() {
    let mut app = in_section(Section::Pools);
    app.handle_key(KeyCode::Char('B'));
    assert!(app.modal.is_none());
}

#[test]
fn help_does_not_claim_esc_cancels_every_modal() {
    let text = help_text();
    assert!(text.contains("in a y/n confirmation Esc selects No"), "{text}");
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
    let healthy = || crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os: None, os_name: None };
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

fn snaps(dataset: &str, names: &[&str]) -> Vec<crate::zfs::snapshot::Snapshot> {
    names.iter().map(|n| crate::zfs::snapshot::Snapshot { dataset: dataset.into(), name: (*n).into(), used: 0, holds: 0, written: 0 }).collect()
}

#[test]
fn turning_force_on_for_a_replication_asks_and_names_the_destination() {
    let prompt = job_save_prompt(Some(&repl_job(false)), &repl_job(true), None).expect("force newly on");
    assert!(prompt.contains("zfs receive -F") && prompt.contains("fbsd16:backup/data") && prompt.ends_with("(y/n)"), "{prompt}");
    assert!(job_save_prompt(None, &repl_job(true), None).is_some(), "a new forced job asks too");
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
fn a_scrub_job_never_asks() {
    let scrub = crate::jobs::Job { kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), dataset: None, prefix: None, ..snap_job(None) };
    assert_eq!(job_save_prompt(None, &scrub, None), None);
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

#[test]
fn a_replication_of_an_encrypted_dataset_says_to_turn_raw_on_and_saves_once_it_is_on() {
    let mut app = keep_form_app("");
    encrypted(&mut app, "tank/data", 0);
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert!(app.status_message.as_deref().unwrap_or("").contains("Turn Raw on"), "{:?}", app.status_message);
    // The Raw field is the tenth one.
    let Some(Modal::Form { fields, focused, .. }) = app.modal.as_mut() else { panic!() };
    fields.resize_with(10, || FormField { label: String::new(), value: String::new() });
    fields[9].value = "y".into();
    *focused = 9;
    app.status_message = None;
    let action = app.handle_modal_key(KeyCode::Enter);
    assert!(action.is_some() || !matches!(app.modal, Some(Modal::Form { .. })), "a raw job saves: {:?} {:?}", app.modal, app.status_message);
    assert!(!app.status_is_error(), "{:?}", app.status_message);
}

#[test]
fn enter_on_a_dataset_with_no_snapshots_opens_a_new_snapshot_job_for_it_on_its_host() {
    use crate::daemon::protection::Gap;
    let (mut app, _dir) = dash_app();
    app.modal = Some(list_of(vec![unprotected(Some(Gap::Missing), None, "tank/photos")]));
    app.handle_modal_key(KeyCode::Enter);
    assert!(matches!(&app.modal, Some(Modal::Form { title, .. }) if title == "New snapshot job"), "{:?}", app.modal);
    assert_eq!(field(app.modal.as_ref(), 0), ("Host".to_string(), "fbsd16".to_string()));
    assert_eq!(field(app.modal.as_ref(), 1), ("Dataset".to_string(), "tank/photos".to_string()));
}

#[test]
fn a_dataset_whose_job_exists_goes_to_that_job_in_jobs() {
    use crate::daemon::protection::Gap;
    let (mut app, _dir) = dash_app();
    let mut job = crate::jobs::Job::test_snapshot("snap-photos", "tank/photos", "auto", 24);
    job.host = "fbsd16".into();
    app.jobs_view.jobs = vec![crate::jobs::Job::test_snapshot("other", "tank/x", "auto", 24), job];
    app.modal = Some(list_of(vec![unprotected(Some(Gap::Failing), None, "tank/photos")]));
    app.handle_modal_key(KeyCode::Enter);
    assert!(app.modal.is_none());
    assert_eq!(app.current_section(), Section::Jobs);
    assert_eq!(app.jobs_table.selected(), Some(1), "the job that covers it, not the first one");
}

#[test]
fn escape_closes_the_list_without_doing_anything() {
    use crate::daemon::protection::Gap;
    let (mut app, _dir) = dash_app();
    app.modal = Some(list_of(vec![unprotected(Some(Gap::Missing), None, "tank/photos")]));
    app.handle_modal_key(KeyCode::Esc);
    assert!(app.modal.is_none() && app.pending_mutations.is_empty());
    assert_eq!(app.current_section(), Section::Dashboard);
}

#[test]
fn turning_raw_on_or_off_for_a_job_that_has_run_needs_a_confirm_that_says_streams_do_not_chain() {
    let mut app = keep_form_app("");
    let mut existing = repl_job(false);
    existing.host = "local".into();
    existing.dataset = Some("tank/data".into());
    let Some(Modal::Form { kind, fields, focused, .. }) = app.modal.as_mut() else { panic!() };
    *kind = repl_kind(Some(existing), vec![]);
    fields.resize_with(10, || FormField { label: String::new(), value: String::new() });
    fields[9].value = "y".into();
    *focused = 9;
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    let Some(Modal::Confirm { prompt, .. }) = &app.modal else { panic!("{:?} {:?}", app.modal, app.status_message) };
    assert!(prompt.contains("Raw is being turned ON") && prompt.contains("do not chain"), "{prompt}");
}

#[test]
fn esc_while_reading_sets_the_cancel_flag_and_closes_the_view() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    let cancel = std::sync::Arc::clone(&app.pending_changes[0].cancel);
    assert!(!cancel.load(std::sync::atomic::Ordering::SeqCst));
    app.handle_key(KeyCode::Esc);
    assert!(app.changes_view.is_none());
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn esc_cancels_a_running_read_and_closes_the_view() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('f'));
    let cancel = std::sync::Arc::clone(&app.pending_browse[0].cancel);
    app.handle_key(KeyCode::Esc);
    assert!(app.browse_view.is_none());
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
}
