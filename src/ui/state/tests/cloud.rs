//! The Cloud section: remotes, the provider guide, sizes and checks.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn f2_on_the_mirrors_remote_field_and_the_backups_repository_field_list_the_hosts_rclone_remotes() {
    let mut app = mirror_form_app(MIRROR_GOOD);
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.as_ref().is_some_and(|p| p.pool_host == Some(0)));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Remotes { prefix: String::new() })]);
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Remotes { prefix: "rclone:".to_string() })], "restic reaches an rclone remote as rclone:name:path");
}

#[test]
fn picking_a_remote_replaces_the_name_and_keeps_the_path_that_was_typed() {
    let mut app = mirror_form_app(MIRROR_GOOD);
    app.handle_modal_key(KeyCode::F(2));
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["aws:  (Amazon S3 (etc.))".to_string(), "b2:  (Backblaze B2)".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[5], "b2:bkt/photos", "the bucket and folder stay, the remote changes");
    let mut blank = MIRROR_GOOD;
    blank[5] = "";
    let mut app = mirror_form_app(blank);
    app.handle_modal_key(KeyCode::F(2));
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["b2:  (Backblaze B2)".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[5], "b2:", "nothing typed: just the remote, for the bucket to follow");
    let mut backup = BACKUP_GOOD;
    backup[5] = "rclone:aws:bkt/restic";
    let mut app = backup_form_app(backup);
    app.handle_modal_key(KeyCode::F(2));
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["rclone:aws:  (Amazon S3 (etc.))".to_string(), "rclone:b2:  (Backblaze B2)".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[5], "rclone:b2:bkt/restic");
    let mut other = BACKUP_GOOD;
    other[5] = "sftp:nas:/backups";
    let mut app = backup_form_app(other);
    app.handle_modal_key(KeyCode::F(2));
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["rclone:b2:  (Backblaze B2)".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[5], "rclone:b2:", "a repository of another kind is replaced, not merged");
}

#[test]
fn big_c_in_jobs_opens_a_blank_cloud_mirror_form_with_defaults() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('C'));
    assert_eq!(form_values(&app), ["local", "", "", "new", "", "", "", "30 3 * * *", "y"]);
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
fn saving_a_mirror_job_asks_first_naming_the_remote_and_saying_nothing_in_old_is_deleted() {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: mirror_fields(MIRROR_GOOD), focused: 8, description: None, kind: mirror_kind(None, vec![]) });
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    let Some(Modal::Confirm { prompt, choice, .. }) = app.modal.clone() else { panic!("{:?}", app.modal) };
    assert_eq!(choice, None);
    assert!(prompt.contains("aws:bkt/photos/current") && prompt.contains("old/") && prompt.contains("never deletes"), "{prompt}");
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
    assert_eq!(app.pending_preflight, vec![PendingPreflight { host: 0, host_name: "local".into(), repository: "t1:".into(), password_file: String::new() }], "a remote no job uses yet is checked at its top level, which only lists it");
}

#[test]
fn n_in_cloud_lists_the_providers_and_enter_shows_the_steps_for_the_one_picked() {
    let (mut app, _dir) = cloud_app(&[]);
    app.handle_key(KeyCode::Char('n'));
    match &app.modal {
        Some(Modal::Choose { title, rows, then, .. }) => {
            assert_eq!(title, "Set up a cloud provider");
            assert_eq!(rows.len(), crate::ui::cloud_guide::PROVIDERS.len());
            assert!(rows[0].contains("Backblaze B2") && rows.iter().any(|r| r.contains("Google Drive")) && rows.iter().any(|r| r.contains("OneDrive")));
            assert_eq!(*then, crate::ui::modal::ChooseThen::Provider);
        }
        other => panic!("expected the provider list, got {other:?}"),
    }
    app.handle_modal_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Info { title, body }) => {
            assert!(title.contains("Backblaze B2"), "{title}");
            assert!(body.contains("sudo rclone config") && body.contains("b2:mybucket/photos"), "{body}");
        }
        other => panic!("expected the steps, got {other:?}"),
    }
    assert!(app.pending_mutations.is_empty() && app.pending_cloud.is_empty() && app.pending_preflight.is_empty(), "the guide runs nothing");
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
