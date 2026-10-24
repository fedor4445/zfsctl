//! Backup and cloud mirror jobs: their forms, the remote check and the prune question.
#![allow(clippy::wildcard_imports)]
use super::*;

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

fn backup_job(retention: Option<&str>) -> crate::jobs::Job {
    crate::jobs::Job {
        kind: crate::jobs::KIND_BACKUP.into(), keep: None, repository: Some("/mnt/usb/restic".into()), password_file: Some("/root/pw".into()), retention: retention.map(Into::into), ..snap_job(None)
    }
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
fn saving_a_backup_form_without_retention_saves_at_once() {
    let mut values = BACKUP_GOOD;
    values[7] = "";
    let (app, out) = submit_backup_form(values);
    assert!(matches!(out.map(|a| a.call), Some(MutationCall::SaveJob { .. })));
    assert!(app.modal.is_none());
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
fn opening_a_backup_form_forgets_an_earlier_check() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.backup_preflight = Some(PreflightCheck { host: 0, repository: "/mnt/usb/restic".into(), password_file: "/root/pw".into(), result: Some(crate::restic::Preflight::Ok), file_note: None });
    app.handle_key(KeyCode::Char('B'));
    assert!(matches!(app.modal, Some(Modal::Form { .. })));
    assert!(app.backup_preflight.is_none());
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

#[test]
fn a_backup_or_snapshot_job_on_an_encrypted_dataset_is_not_refused_by_the_replication_guard() {
    // The guard is for replication and mirror jobs only: restic re-encrypts, and a snapshot job never sends.
    let mut app = App::new(vec!["local".to_string()]);
    encrypted(&mut app, "tank/data", 0);
    app.hosts = vec!["local".to_string()];
    for kind in [crate::jobs::KIND_BACKUP, crate::jobs::KIND_SNAPSHOT] {
        let mut job = crate::jobs::Job::test_replicate("j", "tank/data", "p", "hub", "x/y");
        job.kind = kind.to_string();
        let action = PendingAction { host: 0, section: Section::Jobs, call: MutationCall::SaveJob { state_dir: std::path::PathBuf::new(), job: Box::new(job) } };
        assert_eq!(app.encrypted_source(&action), None, "{kind}");
    }
}

#[test]
fn the_last_bucket_measurement_survives_a_restart_and_a_missing_or_broken_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(crate::ui::cloud::load_sizes(dir.path()).is_empty());
    std::fs::write(dir.path().join("cloud-sizes.json"), "not json").unwrap();
    assert!(crate::ui::cloud::load_sizes(dir.path()).is_empty());

    let mut app = App::new(vec!["local".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    let size = crate::ui::cloud::Size { current: Some((3, 1_992_294)), old: None, at: chrono::Utc::now(), problem: None };
    app.tx.send(WorkerMsg::CloudSize("mirror-a".into(), size.clone())).unwrap();
    app.drain_worker_messages();
    assert!(dir.path().join("cloud-sizes.json").is_file());

    let mut next = App::new(vec!["local".to_string()]);
    next.state_dir = dir.path().to_path_buf();
    assert!(next.cloud_sizes.is_empty());
    next.refresh_jobs();
    assert_eq!(next.cloud_sizes.get("mirror-a"), Some(&size));
    // A reading taken this session is not replaced by the saved one.
    let newer = crate::ui::cloud::Size { current: Some((9, 9)), ..size };
    next.cloud_sizes.insert("mirror-a".into(), newer.clone());
    next.cloud_sizes_loaded = false;
    next.refresh_jobs();
    assert_eq!(next.cloud_sizes.get("mirror-a"), Some(&newer));
}

#[test]
fn a_failed_measurement_keeps_the_last_good_numbers_and_says_why() {
    let mut app = App::new(vec!["local".to_string()]);
    let good = crate::ui::cloud::Size { current: Some((3, 1_992_294)), old: None, at: chrono::Utc::now() - chrono::Duration::hours(5), problem: None };
    app.cloud_sizes.insert("m".into(), good.clone());
    let failed = crate::ui::cloud::Size { current: None, old: None, at: chrono::Utc::now(), problem: Some("rclone: timeout".into()) };
    app.tx.send(WorkerMsg::CloudSize("m".into(), failed)).unwrap();
    app.drain_worker_messages();
    let kept = &app.cloud_sizes["m"];
    assert_eq!((kept.current, kept.at, kept.problem.as_deref()), (good.current, good.at, Some("rclone: timeout")));
    // With nothing measured before, the failure is all there is.
    let mut fresh = App::new(vec!["local".to_string()]);
    let failed = crate::ui::cloud::Size { current: None, old: None, at: chrono::Utc::now(), problem: Some("x".into()) };
    fresh.tx.send(WorkerMsg::CloudSize("m".into(), failed.clone())).unwrap();
    fresh.drain_worker_messages();
    assert_eq!(fresh.cloud_sizes["m"], failed);
}
