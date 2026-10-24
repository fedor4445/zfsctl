//! Snapshots: the list, create and destroy, rollback, holds, browsing and restoring files.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn flatten_permissions_snapshot_produces_one_row_per_entry_in_a_stable_order() {
    let snapshot = crate::zfs::permission::PermissionsSnapshot {
        sets: vec![crate::zfs::permission::PermissionSet { name: "admin".to_string(), perms: vec!["snapshot".to_string()] }],
        create_time: vec!["destroy".to_string()],
        grants: vec![crate::zfs::permission::PermissionGrant {
            recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
            scope: crate::zfs::permission::Scope::Both,
            perms: vec!["mount".to_string()],
        }],
    };
    let rows = flatten_permissions_snapshot(&snapshot);
    assert_eq!(
        rows,
        vec![
            PermissionRow::Set { name: "admin".to_string(), perms: vec!["snapshot".to_string()] },
            PermissionRow::CreateTime { perms: vec!["destroy".to_string()] },
            PermissionRow::Grant {
                recipient: crate::zfs::permission::Recipient::User("fb".to_string()),
                scope: crate::zfs::permission::Scope::Both,
                perms: vec!["mount".to_string()],
            },
        ]
    );
}

#[test]
fn handle_key_navigates_snapshot_rows_and_wraps() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![
        Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0, holds: 0, written: 0 },
        Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0, holds: 0, written: 0 },
    ];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    assert_eq!(app.current_section(), Section::Snapshots);
    app.handle_key(KeyCode::Down);
    assert_eq!(app.snapshots_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.snapshots_table.selected(), Some(1), "must wrap back to the last row");
}

#[test]
fn drain_worker_messages_clamps_selected_snapshot_row_when_list_shrinks() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots_table.select(Some(2));
    app.begin_refresh(Section::Snapshots, 0);
    app.tx
        .send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot {
            dataset: "tank/data".to_string(),
            name: "only".to_string(),
            used: 0,
            holds: 0,
            written: 0,
        }])))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.snapshots_table.selected(), Some(0));
}

#[test]
fn handle_key_s_on_datasets_opens_create_snapshot_form_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None, quota: 0, refquota: 0,
    }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 1);
            assert_eq!(*kind, crate::ui::modal::FormKind::CreateSnapshot { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_modal_key_create_snapshot_form_submits_with_implied_dataset() {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 0,
        available: 0,
        referenced: 0,
        mountpoint: None, quota: 0, refquota: 0,
    }];
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('s'));
    for c in "manual-1".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    let result = app.handle_modal_key(KeyCode::Enter);
    assert_eq!(
        result,
        Some(crate::ui::modal::PendingAction {
            host: 0,
            section: Section::Snapshots,
            call: crate::ui::modal::MutationCall::CreateSnapshot {
                dataset: "tank/data".to_string(),
                name: "manual-1".to_string(),
            },
        })
    );
}

#[test]
fn handle_key_d_on_snapshots_opens_destroy_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "old-1".to_string(), used: 0, holds: 0, written: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@old-1");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::DestroySnapshot { dataset: "tank/data".to_string(), name: "old-1".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn handle_key_b_on_snapshots_opens_rollback_confirm_typed_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string(), used: 0, holds: 0, written: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('b'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@2026-09-01");
            assert_eq!(
                action.call,
                crate::ui::modal::MutationCall::RollbackSnapshot { dataset: "tank/data".to_string(), name: "2026-09-01".to_string() }
            );
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn an_smb_error_in_the_shares_snapshot_sets_an_error_status_but_keeps_the_nfs_shares() {
    let mut app = App::new(vec!["local".to_string()]);
    let nfs = crate::zfs::share::Share::Nfs { dataset: "tank/n".into(), raw: "on".into(), networks: vec![], hosts: vec![], read_only: false, root: None };
    app.tx
        .send(WorkerMsg::Shares(
            0,
            Ok(crate::zfs::share::SharesSnapshot { shares: vec![nfs], nfs_available: true, samba_available: true, smb_error: Some("boom".into()), nfs_unknown: false, samba_unknown: false }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert_eq!(app.shares.len(), 1, "NFS shares still populated");
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("boom")));
    assert!(app.status_is_error());
}

#[test]
fn refresh_snapshots_populates_snapshots_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank/data@s1\t10\t0\t0\n"));
    app.refresh_snapshots(runner);
    wait_until(&mut app, |a| !a.snapshots.is_empty());
    assert_eq!(app.snapshots.len(), 1);
    assert_eq!(app.snapshots[0].name, "s1");
    assert!(!app.is_loading());
}

#[test]
fn an_edit_changes_the_fields_but_keeps_id_creation_time_and_enabled() {
    let mut existing = scrub_job("my-job");
    existing.enabled = false;
    existing.created_at = Some(at_utc("2026-09-01T00:00:00Z"));
    let action = build_job_action(&job_kind(Some(existing), &["my-job"]), &fields(["fbsd16", "other", "0 4 * * *", "n"]), std::path::Path::new("/s"), at_utc("2026-10-06T12:00:00Z")).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    assert_eq!((job.id.as_str(), job.host.as_str(), job.pool.as_deref(), job.schedule.as_str(), job.catch_up), ("my-job", "fbsd16", Some("other"), "0 4 * * *", false));
    assert!(!job.enabled, "an edit does not re-enable a disabled job");
    assert_eq!(job.created_at, Some(at_utc("2026-09-01T00:00:00Z")));
}

#[test]
fn up_and_down_move_the_snapshot_disk_and_share_rows() {
    let mut app = in_section(Section::Snapshots);
    let snap = |n: &str| crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: n.to_string(), used: 1, holds: 0, written: 0 };
    app.snapshots = vec![snap("s1"), snap("s2")];
    up_moves(&mut app, |a| &mut a.snapshots_table, &[|a| &mut a.disks_table, |a| &mut a.shares_table]);
    down_moves(&mut app, |a| &mut a.snapshots_table);

    let mut app = in_section(Section::Disks);
    app.disks = vec![
        DiskInfo { name: "da0".to_string(), size: 1, ..Default::default() },
        DiskInfo { name: "da1".to_string(), size: 1, ..Default::default() },
    ];
    up_moves(&mut app, |a| &mut a.disks_table, &[|a| &mut a.snapshots_table, |a| &mut a.shares_table]);
    down_moves(&mut app, |a| &mut a.disks_table);

    let mut app = in_section(Section::Shares);
    app.shares = vec![smb("tank/a"), smb("tank/b")];
    up_moves(&mut app, |a| &mut a.shares_table, &[|a| &mut a.snapshots_table, |a| &mut a.disks_table]);
    down_moves(&mut app, |a| &mut a.shares_table);
}

#[test]
fn selection_is_pinned_by_identity_in_datasets_snapshots_and_disks_too() {
    let ds = |n: &str| Dataset { name: n.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: None, quota: 0, refquota: 0 };
    let mut app = in_section(Section::Datasets);
    app.datasets = vec![ds("tank/a"), ds("tank/b")];
    app.datasets_table.select(Some(1));
    app.tx.send(WorkerMsg::Datasets(0, Ok(vec![ds("tank/0"), ds("tank/a"), ds("tank/b")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.selected_dataset_name().as_deref(), Some("tank/b"));

    let snap = |d: &str, n: &str| crate::zfs::snapshot::Snapshot { dataset: d.to_string(), name: n.to_string(), used: 1, holds: 0, written: 0 };
    app.snapshots = vec![snap("tank/a", "s1"), snap("tank/b", "s1")];
    app.snapshots_table.select(Some(1)); // tank/b@s1: same snapshot name as another dataset's
    app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![snap("tank/b", "s1"), snap("tank/a", "s1")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.snapshots_table.selected(), Some(0), "identity is dataset@name, not just the name");

    let disk = |n: &str| DiskInfo { name: n.to_string(), size: 1, ..Default::default() };
    app.disks = vec![disk("da0"), disk("da1")];
    app.disks_table.select(Some(1));
    app.tx.send(WorkerMsg::Disks(0, Ok(vec![disk("da9"), disk("da0"), disk("da1")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.disks_table.selected(), Some(2));
}

#[test]
fn j_on_a_dataset_prefills_the_snapshot_form() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('J'));
    assert_eq!(form_values(&app)[1], "tank/media");
}

#[test]
fn the_snapshot_form_refuses_what_the_daemon_would_refuse() {
    let good = ["local", "tank/data", "n", "hourly", "24", "0 * * * *", "y"];
    assert!(build_snapshot_job_action(&snapshot_kind(None, vec![]), &snap_fields(good), std::path::Path::new("/x"), chrono::Utc::now()).is_ok());
    assert!(snap_err(["nope", "tank/data", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Unknown host"));
    assert!(snap_err(["local", "", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
    assert!(snap_err(["local", "-rf", "n", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Dataset"));
    assert!(snap_err(["local", "tank/data", "maybe", "hourly", "24", "0 * * * *", "y"], vec![]).contains("Recursive"));
    assert!(snap_err(["local", "tank/data", "n", "a b", "24", "0 * * * *", "y"], vec![]).contains("Prefix"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "0", "0 * * * *", "y"], vec![]).contains("Keep"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "x", "0 * * * *", "y"], vec![]).contains("Keep"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "bogus", "y"], vec![]).contains("Schedule"));
    assert!(snap_err(["local", "tank/data", "n", "hourly", "24", "0 * * * *", "q"], vec![]).contains("Catch up"));
}

#[test]
fn f2_on_the_snapshot_forms_dataset_field_queues_a_dataset_fetch_for_the_chosen_host() {
    let (mut app, _dir) = app_with_jobs(&[]);
    app.handle_key(KeyCode::Char('N'));
    app.handle_modal_key(KeyCode::Tab);
    app.handle_modal_key(KeyCode::F(2));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Datasets)]);
    app.tx.send(WorkerMsg::PoolNames(0, Ok(vec!["tank".to_string(), "tank/data".to_string()]))).unwrap();
    app.drain_worker_messages();
    app.handle_picker_key(KeyCode::Down);
    app.handle_picker_key(KeyCode::Enter);
    assert_eq!(form_values(&app)[1], "tank/data");
}

fn saved_backup(values: [&str; 10]) -> crate::jobs::Job {
    let action = build_backup_job_action(&backup_kind(None, vec![]), &backup_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap();
    let MutationCall::SaveJob { job, .. } = action.call else { panic!() };
    *job
}

#[test]
fn the_source_field_saves_new_a_pinned_snapshot_or_a_prefix_and_the_path_is_optional() {
    let mut values = BACKUP_GOOD;
    values[3] = "new";
    values[4] = "5";
    values[2] = "photos/2026";
    let job = saved_backup(values);
    assert_eq!((job.source.as_deref(), job.snapshot.as_deref(), job.prefix.as_deref(), job.keep, job.path.as_deref()), (Some("new"), None, None, Some(5), Some("photos/2026")));
    assert_eq!(crate::jobs::backup_source(&job), crate::jobs::BackupSource::Fresh);
    values[4] = "";
    assert_eq!(saved_backup(values).keep, None, "blank means the default of 3");
    values[3] = "@snap4";
    values[4] = "5";
    values[2] = "";
    let job = saved_backup(values);
    assert_eq!((job.source.as_deref(), job.snapshot.as_deref(), job.prefix.as_deref(), job.keep, job.path.as_deref()), (Some("snapshot"), Some("snap4"), None, None, None));
    let job = saved_backup(BACKUP_GOOD);
    assert_eq!((job.source.as_deref(), job.prefix.as_deref(), job.keep), (None, Some("hourly"), None));
}

#[test]
fn f2_on_the_source_field_lists_the_snapshots_of_the_forms_dataset() {
    let mut app = backup_form_app(BACKUP_GOOD);
    let Some(Modal::Form { focused, .. }) = &mut app.modal else { panic!() };
    *focused = 3;
    app.handle_modal_key(KeyCode::F(2));
    assert!(app.picker.as_ref().is_some_and(|p| p.pool_host == Some(0)));
    assert_eq!(app.pending_pool_fetch, vec![(0, crate::ui::picker::LiveNames::Snapshots("tank/data".to_string()))]);
}

#[test]
fn the_prompt_counts_singular_correctly_and_admits_when_snapshots_are_unknown() {
    let mut app = destroy_app();
    app.datasets.retain(|d| d.name != "tank/data/b");
    app.snapshots.retain(|s| s.dataset == "tank/data" && s.name == "s1");
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("1 child dataset ") && prompt.contains("1 snapshot"), "{prompt}");
    let mut app = destroy_app();
    app.snapshots_loaded = false;
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("snapshots: unknown") && !prompt.contains("3 snapshots"), "{prompt}");
}

#[test]
fn plain_d_warns_that_zfs_will_refuse_and_points_at_big_d_only_when_there_is_something_under_it() {
    let mut app = destroy_app();
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::ConfirmTyped { prompt, action, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("zfs will refuse") && prompt.contains("press Esc, then D"), "{prompt}");
    assert_eq!(action.call, MutationCall::DestroyDataset { name: "tank/data".to_string() }, "d stays non-recursive");
    let mut app = destroy_app();
    app.datasets_table.select(Some(4)); // tank/other: one snapshot, no children
    app.snapshots.retain(|s| s.dataset != "tank/other");
    app.handle_key(KeyCode::Char('d'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(!prompt.contains("zfs will refuse"), "nothing under it: {prompt}");
}

#[test]
fn big_d_is_inert_without_a_selection_or_with_the_right_pane_focused() {
    let mut app = destroy_app();
    app.datasets_table.select(None);
    app.handle_key(KeyCode::Char('D'));
    assert!(app.modal.is_none());
    let mut app = destroy_app();
    app.pane_focus = PaneFocus::Right;
    app.handle_key(KeyCode::Char('D'));
    assert!(app.modal.is_none());
}

#[test]
fn snapshots_count_as_loaded_after_a_refresh_and_not_after_a_host_switch() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    assert!(!app.snapshots_loaded);
    app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert!(app.snapshots_loaded, "an empty list is still a loaded list");
    app.handle_key(KeyCode::Down); // switch host (Hosts section)
    assert!(!app.snapshots_loaded);
}


#[test]
fn the_datasets_view_refreshes_the_snapshot_list_quietly_with_the_datasets_so_destroy_prompts_can_count_them() {
    let mut app = in_section(Section::Datasets);
    app.status_message = Some("scrub started".to_string());
    app.auto_refresh(runner(""), Instant::now(), TEN);
    assert!(app.in_flight.contains(&(Section::Datasets, 0)) && app.in_flight.contains(&(Section::Snapshots, 0)));
    assert!(app.quiet_refresh.contains(&(Section::Snapshots, 0)), "quiet: its arrival must not wipe an informational status");
    wait_until(&mut app, |a| a.snapshots_loaded);
    assert!(app.snapshots_loaded);
    assert_eq!(app.status_message.as_deref(), Some("scrub started"));
    app.in_flight.clear();
    app.last_auto_refresh.clear();
    app.auto_refresh(runner(""), Instant::now(), TEN);
    assert!(app.in_flight.contains(&(Section::Snapshots, 0)), "every Datasets refresh also refreshes the snapshots, so the counts are as fresh as the list");
}


#[test]
fn uppercase_y_and_n_also_only_select_so_caps_lock_cannot_leave_the_user_stuck() {
    let (mut app, action) = confirm_scrub();
    assert_eq!(app.handle_modal_key(KeyCode::Char('Y')), None);
    assert_eq!(choice_of(&app), Some(Some(true)));
    assert_eq!(app.handle_modal_key(KeyCode::Char('N')), None);
    assert_eq!(choice_of(&app), Some(Some(false)));
    app.handle_modal_key(KeyCode::Char('Y'));
    assert_eq!(app.handle_modal_key(KeyCode::Enter), Some(action), "still two keys: select, then Enter");
}

#[test]
fn accepting_the_confirm_still_saves_and_leaves_nothing_to_restore() {
    let (mut app, _) = submit_backup_form(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter saves");
    assert!(matches!(action.call, MutationCall::SaveJob { .. }));
    assert!(app.modal.is_none() && app.confirm_restore.is_none());
}

#[test]
fn saving_the_theme_changes_the_apps_theme_at_once() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[ui]\ntheme = \"mono\"\n");
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::Mono);
    saved_settings(&mut app, &dir, "[ui]\ntheme = \"nord\"\n");
    assert!(app.status_error.clone().unwrap_or_default().contains("nord"), "{:?}", app.status_error);
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::Mono, "an unknown theme changes nothing");
}

#[test]
fn a_solarized_theme_without_truecolor_says_so_as_at_startup() {
    let (mut app, _dir) = settings_app("[ui]\ntheme = \"solarized-dark\"\n");
    app.apply_saved_settings_with(&crate::ui::theme::ThemeEnv { no_color: None, colorterm: None });
    assert_eq!(app.theme.name, crate::ui::theme::ThemeName::SolarizedDark);
    assert!(app.status_message.is_some());
    app.status_message = None;
    app.apply_saved_settings_with(&crate::ui::theme::ThemeEnv { no_color: None, colorterm: Some("truecolor".into()) });
    assert_eq!(app.status_message, None);
}

#[test]
fn saving_the_auto_refresh_changes_the_interval_and_zero_turns_it_off() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[ui]\nauto_refresh_secs = 5\n");
    assert_eq!(app.auto_refresh_interval, Some(Duration::from_secs(5)));
    saved_settings(&mut app, &dir, "[ui]\nauto_refresh_secs = 0\n");
    assert_eq!(app.auto_refresh_interval, None);
    saved_settings(&mut app, &dir, "");
    assert_eq!(app.auto_refresh_interval, Some(Duration::from_secs(crate::config::DEFAULT_AUTO_REFRESH_SECS)));
}

#[test]
fn the_theme_note_is_repeated_only_when_the_theme_changes() {
    let (mut app, _dir) = settings_app("[ui]\ntheme = \"solarized-dark\"\n");
    let no_truecolor = crate::ui::theme::ThemeEnv { no_color: None, colorterm: None };
    app.apply_saved_settings_with(&no_truecolor);
    assert!(app.status_message.is_some(), "the theme changed from the default: say what the terminal lacks");
    app.status_message = Some("Saved to config.toml".to_string());
    app.apply_saved_settings_with(&no_truecolor);
    assert_eq!(app.status_message.as_deref(), Some("Saved to config.toml"), "the same theme again: keep the saved note");
}

#[test]
fn the_layout_excerpt_shows_what_the_add_changes_not_the_whole_tree() {
    use crate::zfs::vdev_role::Role;
    let mut text = String::from("would update 'tank' to the following configuration:\n\n\ttank\n");
    for i in 0..4 {
        text.push_str(&format!("\t  raidz2-{i}\n"));
        for d in 0..6 {
            text.push_str(&format!("\t    da{}\n", i * 6 + d));
        }
    }
    text.push_str("\tspecial\n\t  mirror\n\t    da30\n\t    da31\n\tcache\n\t  da40\n");
    let special = super::super::forms::layout_excerpt(&text, Role::Special);
    assert!(special.contains("would update 'tank'") && special.contains("special") && special.contains("da31"), "{special}");
    assert!(!special.contains("raidz2-0") && !special.contains("da40"), "only the special section: {special}");
    let cache = super::super::forms::layout_excerpt(&text, Role::Cache);
    assert!(cache.contains("cache") && cache.contains("da40") && !cache.contains("da30"), "{cache}");
    let data = super::super::forms::layout_excerpt(&text, Role::Data);
    assert!(data.contains("raidz2-3") && data.contains("...") && !data.contains("special"), "the tail of the data vdevs: {data}");
    assert_eq!(super::super::forms::layout_excerpt("no tabs here", Role::Log), "no tabs here");
}

#[test]
fn capital_h_on_an_unheld_snapshot_opens_a_y_n_confirm_for_a_hold() {
    let mut app = hold_app(0, None);
    app.handle_key(KeyCode::Char('H'));
    match app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, crate::ui::modal::MutationCall::HoldSnapshot { dataset: "tank/data".to_string(), name: "s1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn capital_h_on_a_held_snapshot_opens_a_typed_confirm_for_a_release() {
    let mut app = hold_app(2, Some(&["keepme", "zfsctl"]));
    app.handle_key(KeyCode::Char('H'));
    match app.modal {
        Some(crate::ui::modal::Modal::ConfirmTyped { expected, action, .. }) => {
            assert_eq!(expected, "tank/data@s1");
            assert_eq!(action.call, crate::ui::modal::MutationCall::ReleaseSnapshot { dataset: "tank/data".to_string(), name: "s1".to_string() });
        }
        other => panic!("expected ConfirmTyped modal, got {other:?}"),
    }
}

#[test]
fn capital_h_with_nothing_selected_does_nothing() {
    let mut app = hold_app(0, None);
    app.snapshots_table.select(None);
    app.handle_key(KeyCode::Char('H'));
    assert!(app.modal.is_none());
    assert!(app.status_message.is_none());
}

#[test]
fn capital_h_on_a_snapshot_held_only_by_another_tag_opens_no_modal_and_says_why() {
    let mut app = hold_app(1, Some(&["backup-keep"]));
    app.handle_key(KeyCode::Char('H'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("held by another tag: backup-keep; zfsctl will not release it"));
}

#[test]
fn capital_h_on_a_held_snapshot_with_unknown_tags_opens_no_modal_and_says_to_refresh() {
    let mut app = hold_app(1, None);
    app.handle_key(KeyCode::Char('H'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("hold tags not loaded yet; press r"));
}

#[test]
fn a_refresh_with_51_held_snapshots_asks_for_tags_in_two_calls() {
    let list = (0..51).map(|i| format!("tank/data@s{i}\t10\t1\t0\n")).collect::<Vec<_>>().concat();
    let runner = Arc::new(FakeRunner::new("").route("zfs", "list", &list).route("zfs", "holds", "tank/data@s0\tzfsctl\tx\n"));
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
    let calls = runner.calls.lock().unwrap();
    let holds: Vec<_> = calls.iter().filter(|c| c.1[0] == "holds").collect();
    assert_eq!(holds.len(), 2);
    assert_eq!(&holds[0].1[..3], ["holds", "-H", "--"]);
    assert_eq!(holds[0].1.len(), 3 + 50);
    assert_eq!(holds[1].1.len(), 3 + 1);
}

#[test]
fn tags_of_an_older_snapshot_list_are_ignored_and_ask_for_a_new_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshot_gen.insert(0, 2);
    app.tags_running.lock().unwrap().insert(0);
    let tags = std::collections::HashMap::from([("tank/data@s1".to_string(), vec!["zfsctl".to_string()])]);
    app.tx.send(WorkerMsg::SnapshotTags(0, 1, tags.clone())).unwrap();
    let needed = app.drain_worker_messages();
    assert!(app.snapshot_tags.is_empty());
    assert!(needed.contains(&Section::Snapshots));
    assert!(app.tags_running.lock().unwrap().is_empty());
    app.tx.send(WorkerMsg::SnapshotTags(0, 2, tags.clone())).unwrap();
    let needed = app.drain_worker_messages();
    assert_eq!(app.snapshot_tags, tags);
    assert!(!needed.contains(&Section::Snapshots));
}

#[test]
fn a_snapshot_refresh_fetches_tags_for_the_held_snapshots_only() {
    let runner = Arc::new(
        FakeRunner::new("").route("zfs", "list", "tank/data@s1\t10\t0\t0\ntank/data@s2\t10\t2\t0\n").route("zfs", "holds", "tank/data@s2\tzfsctl\tThu Oct 1 00:00 2026\ntank/data@s2\tother\tThu Oct 1 00:00 2026\n"),
    );
    let dynrunner: Arc<dyn CommandRunner> = runner.clone();
    let mut app = App::new(vec!["local".to_string()]);
    app.refresh_snapshots(dynrunner);
    for _ in 0..200 {
        app.drain_worker_messages();
        if !app.snapshot_tags.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.snapshot_tags.get("tank/data@s2"), Some(&vec!["zfsctl".to_string(), "other".to_string()]));
    assert!(!app.snapshot_tags.contains_key("tank/data@s1"));
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| c.1[0] == "holds").count(), 1);
}

#[test]
fn a_snapshot_refresh_clears_the_old_tags() {
    let mut app = hold_app(1, Some(&["zfsctl"]));
    app.begin_refresh(Section::Snapshots, 0);
    app.tx.send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot { dataset: "tank/data".to_string(), name: "s1".to_string(), used: 0, holds: 0, written: 0 }]))).unwrap();
    app.drain_worker_messages();
    assert!(app.snapshot_tags.is_empty());
}

#[test]
fn stale_tags_while_a_snapshot_refresh_is_in_flight_or_pending_push_no_refresh() {
    let tags = std::collections::HashMap::from([("tank/data@s1".to_string(), vec!["zfsctl".to_string()])]);
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshot_gen.insert(0, 2);
    app.begin_refresh(Section::Snapshots, 0);
    app.tx.send(WorkerMsg::SnapshotTags(0, 1, tags.clone())).unwrap();
    assert!(!app.drain_worker_messages().contains(&Section::Snapshots), "an in-flight refresh fetches its own tags");
    app.end_refresh(Section::Snapshots, 0);
    app.pending_refresh.insert((Section::Snapshots, 0));
    app.tx.send(WorkerMsg::SnapshotTags(0, 1, tags)).unwrap();
    assert!(!app.drain_worker_messages().contains(&Section::Snapshots));
}

#[test]
fn capital_h_with_only_unprintable_foreign_tags_still_says_it_is_held() {
    let mut app = hold_app(1, Some(&["\u{1b}\u{7}"]));
    app.handle_key(KeyCode::Char('H'));
    assert_eq!(app.status_message.as_deref(), Some("held by another tag (name not printable); zfsctl will not release it"));
}

#[test]
fn d_on_a_held_snapshot_shows_the_refusal_and_runs_no_destroy() {
    let mut app = hold_app(1, Some(&["zfsctl"]));
    app.handle_key(KeyCode::Char('d'));
    for c in "tank/data@s1".chars() {
        assert_eq!(app.handle_modal_key(KeyCode::Char(c)), None);
    }
    let action = app.handle_modal_key(KeyCode::Enter).expect("the typed name confirms");
    let fake = Arc::new(FakeRunner::new("").route("zfs", "get", "tank/data@s1\t1\n").route("zfs", "holds", "tank/data@s1\tzfsctl\tThu Oct 1 00:00 2026\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();
    app.dispatch_mutation(action, runner);
    for _ in 0..200 {
        app.drain_worker_messages();
        if app.status_message.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let status = app.status_message.clone().unwrap_or_default();
    assert!(status.starts_with("protected:"), "{status}");
    assert!(app.status_is_error());
    assert!(!fake.calls.lock().unwrap().iter().any(|c| c.1[0] == "destroy"));
}

#[test]
fn help_lists_the_hold_key() {
    assert!(help_text().contains("  H                hold the selected snapshot"), "{}", help_text());
}

#[test]
fn c_on_a_snapshot_opens_a_reading_view_and_queues_one_read() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    let view = app.changes_view.as_ref().expect("a view");
    assert!(matches!(view.phase, crate::ui::changes::Phase::Reading));
    assert_eq!(app.pending_changes.len(), 1);
    let p = &app.pending_changes[0];
    assert_eq!((p.host, p.dataset.as_str(), p.name.as_str(), p.against), (0, "tank/data", "s2", crate::ui::changes::Against::Previous));
    assert_eq!(p.request, view.request);
}

#[test]
fn capital_h_in_the_view_holds_the_older_snapshot_with_a_y_n_confirm() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Char('H'));
    match app.modal {
        Some(crate::ui::modal::Modal::Confirm { action, .. }) => {
            assert_eq!(action.call, crate::ui::modal::MutationCall::HoldSnapshot { dataset: "tank/data".to_string(), name: "s1".to_string() });
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn capital_h_in_the_view_does_nothing_without_an_older_snapshot() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    app.handle_key(KeyCode::Char('H'));
    assert!(app.modal.is_none(), "still reading");
    let request = app.changes_view.as_ref().unwrap().request;
    deliver(&mut app, 0, request, Ok(crate::ui::changes::ChangesOutcome::NoPrevious));
    app.handle_key(KeyCode::Char('H'));
    assert!(app.modal.is_none(), "the oldest snapshot has no older one");
}

#[test]
fn the_help_lists_the_changes_keys() {
    let text = help_text_for_book();
    assert!(text.contains("  c ") && text.contains("  C "), "{text}");
}

#[test]
fn a_read_whose_runner_cannot_run_the_diff_ends_failed_not_stuck() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    let pending = app.pending_changes.remove(0);
    let request = pending.request;
    app.start_changes(std::sync::Arc::new(FakeRunner::new("")), pending);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while matches!(app.changes_view.as_ref().map(|v| &v.phase), Some(crate::ui::changes::Phase::Reading)) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.drain_worker_messages();
    }
    let view = app.changes_view.as_ref().unwrap();
    assert_eq!(view.request, request);
    assert!(matches!(view.phase, crate::ui::changes::Phase::Failed(_)), "{:?}", view.phase);
}

#[test]
fn a_changes_view_does_not_survive_a_host_switch_and_its_read_is_cancelled() {
    let mut app = App::new(vec!["a".to_string(), "b".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "s1".to_string(), used: 0, holds: 0, written: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('c'));
    let (request, cancel) = (app.pending_changes[0].request, std::sync::Arc::clone(&app.pending_changes[0].cancel));
    app.selected_host = 1;
    app.clear_host_data();
    assert!(app.changes_view.is_none(), "the view belonged to the other host");
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst), "and its diff is cancelled, not left running");
    // The answer arrives late, and the user comes back to the first host: no view is stuck on "reading".
    app.selected_host = 0;
    deliver(&mut app, 0, request, Ok(crate::ui::changes::ChangesOutcome::NoPrevious));
    assert!(app.changes_view.is_none());
}

#[test]
fn f_starts_a_read_of_the_snapshot_top_and_shows_the_view() {
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('f'));
    let view = app.browse_view.as_ref().expect("f opens the browser");
    assert!(matches!(view.phase, crate::ui::browse::Phase::Reading));
    assert_eq!(app.pending_browse.len(), 1);
    let p = &app.pending_browse[0];
    assert_eq!((p.dataset.as_str(), p.name.as_str(), p.path.len(), p.mountpoint.clone()), ("tank/data", "s2", 0, None));
    assert!(app.browse_view_active());
}

#[test]
fn keys_that_act_on_the_hidden_list_do_nothing_inside_the_browser() {
    let mut app = browsing_app();
    for key in ['d', 'b', 'c', 'C', 'H', 'f'] {
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none(), "{key} must not open a confirm behind the browser");
    }
    assert_eq!(app.pending_browse.len(), 1, "f inside the browser starts nothing");
    assert!(app.changes_view.is_none() && app.pending_changes.is_empty(), "c does not start a diff either");
}

#[test]
fn the_changes_view_swallows_d_b_and_f_too() {
    let mut app = ready_app();
    for key in ['d', 'b', 'f'] {
        app.handle_key(KeyCode::Char(key));
        assert!(app.modal.is_none() && app.browse_view.is_none(), "{key}");
    }
}

#[test]
fn enter_opens_a_folder_and_left_goes_back() {
    use crate::zfs::snapshot_browse::EntryKind;
    let mut app = browsing_app();
    app.pending_browse.clear();
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.pending_browse.len(), 1);
    assert_eq!(app.pending_browse[0].path, ["pics"]);
    assert_eq!(app.pending_browse[0].mountpoint.as_deref(), Some("/tank/data"));
    let request = app.browse_view.as_ref().unwrap().request;
    deliver_browse(&mut app, 0, request, Ok(browse_outcome(&[("x", EntryKind::File)])));
    app.pending_browse.clear();
    app.handle_key(KeyCode::Left);
    assert_eq!(app.pending_browse.len(), 1);
    assert!(app.pending_browse[0].path.is_empty());
}

#[test]
fn a_late_answer_for_another_request_is_dropped() {
    use crate::zfs::snapshot_browse::EntryKind;
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('f'));
    let request = app.browse_view.as_ref().unwrap().request;
    deliver_browse(&mut app, 0, request + 7, Ok(browse_outcome(&[("x", EntryKind::File)])));
    assert!(matches!(app.browse_view.as_ref().unwrap().phase, crate::ui::browse::Phase::Reading));
    deliver_browse(&mut app, 3, request, Ok(browse_outcome(&[("x", EntryKind::File)])));
    assert!(matches!(app.browse_view.as_ref().unwrap().phase, crate::ui::browse::Phase::Reading), "another host's answer is dropped");
}

#[test]
fn a_browse_view_does_not_survive_a_host_switch_and_its_read_is_cancelled() {
    let mut app = App::new(vec!["a".to_string(), "b".to_string()]);
    app.host_probes.insert("a".to_string(), HostProbeState { os: Some(crate::zfs::host_health::HostOs::FreeBsd), ..HostProbeState::default() });
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "s1".to_string(), used: 0, holds: 0, written: 0 }];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app.handle_key(KeyCode::Char('f'));
    let (request, cancel) = (app.pending_browse[0].request, std::sync::Arc::clone(&app.pending_browse[0].cancel));
    app.selected_host = 1;
    app.clear_host_data();
    assert!(app.browse_view.is_none());
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst), "the read is cancelled, not left running");
    app.selected_host = 0;
    deliver_browse(&mut app, 0, request, Ok(browse_outcome(&[])));
    assert!(app.browse_view.is_none());
}

#[test]
fn leaving_the_snapshots_section_hides_the_browser_and_an_answer_that_arrives_meanwhile_is_there_on_return() {
    use crate::zfs::snapshot_browse::EntryKind;
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('f'));
    let request = app.browse_view.as_ref().unwrap().request;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    assert!(!app.browse_view_active(), "the browser is not shown on another section");
    deliver_browse(&mut app, 0, request, Ok(browse_outcome(&[("pics", EntryKind::Directory)])));
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    assert!(app.browse_view_active());
    assert!(matches!(app.browse_view.as_ref().unwrap().phase, crate::ui::browse::Phase::Ready), "the answer was kept for when the user comes back");
}

#[test]
fn slash_inside_a_changes_folder_starts_a_filter_and_letters_typed_are_text() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Enter);
    assert!(app.changes_view.as_ref().unwrap().open_group.is_some());
    app.handle_key(KeyCode::Char('/'));
    assert!(app.changes_view.as_ref().unwrap().name_input.is_some());
    for c in "qd?f1".chars() {
        app.handle_key(KeyCode::Char(c));
    }
    assert!(!app.should_quit && app.modal.is_none(), "letters typed into the filter are text");
    let view = app.changes_view.as_ref().unwrap();
    assert_eq!(view.name_input.as_deref(), Some("qd?f1"));
    app.handle_key(KeyCode::Esc);
    assert!(app.changes_view.as_ref().unwrap().name_input.is_none(), "Esc closes the prompt, not the view");
    assert!(app.changes_view.as_ref().unwrap().open_group.is_some());
}

#[test]
fn slash_at_the_folder_list_of_the_changes_view_does_nothing() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Char('/'));
    assert!(app.changes_view.as_ref().unwrap().name_input.is_none());
    assert!(app.host_filter.is_empty() && !app.host_filter_editing, "and it is not taken for the Hosts filter");
}

#[test]
fn the_restore_prompt_shows_host_text_without_control_characters() {
    // The engine refuses a path with a control character, so the prompt's own cleaning is tried on a hand-built plan.
    let plan = crate::zfs::snapshot_restore::RestorePlan {
        source: "/t/\u{1b}[31ma".to_string(),
        destination: "/t/b\u{7}".to_string(),
        dest_folder: "/t".to_string(),
        dest_name: "b".to_string(),
        command: vec!["-p".to_string(), "/t/\u{1b}[31ma".to_string()],
        size: 5,
        mode: "-rw\u{1b}".to_string(),
    };
    let prompt = super::super::restore_prompt(&plan);
    assert!(!prompt.chars().any(|c| c.is_control() && c != '\n'), "{prompt:?}");
    assert!(prompt.contains("From:") && prompt.contains("Command: cp -p"), "{prompt}");
    // And a mountpoint with a control character is refused by the engine before any prompt exists.
    let mut app = browsing_on_file();
    app.browse_view.as_mut().unwrap().mountpoint = Some("/tank/\u{1b}[31mdata".to_string());
    app.handle_key(KeyCode::Char('R'));
    app.handle_modal_key(KeyCode::Enter);
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })));
    assert_eq!(app.status_message.as_deref(), Some("the dataset's mountpoint is not a plain path, so it cannot be browsed"));
}

#[test]
fn the_restore_confirm_needs_two_keys() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert!(app.handle_modal_key(KeyCode::Enter).is_none(), "Enter alone does nothing");
    assert!(app.modal.is_some());
    assert!(app.handle_modal_key(KeyCode::Char('y')).is_none(), "y only selects");
    let action = app.handle_modal_key(KeyCode::Enter).expect("y then Enter runs it");
    assert!(matches!(action.call, crate::ui::modal::MutationCall::RestoreFile { .. }));
    assert!(app.modal.is_none());
}

#[test]
fn declining_the_restore_confirm_hands_the_form_back() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    app.handle_modal_key(KeyCode::Enter);
    app.handle_modal_key(KeyCode::Char('n'));
    assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    assert_eq!(restore_form_text(&app), "a.restored-s2.txt");
}

#[test]
fn the_changes_view_has_no_r() {
    let mut app = ready_app();
    app.handle_key(KeyCode::Char('R'));
    assert!(app.modal.is_none() && app.browse_view.is_none() && app.status_message.is_none());
}

#[test]
fn after_a_restore_the_status_names_the_new_file_and_no_browse_read_is_queued_or_disturbed() {
    let mut app = browsing_app();
    app.handle_key(KeyCode::Enter);
    let (request, cancel) = (app.browse_view.as_ref().unwrap().request, std::sync::Arc::clone(&app.browse_view.as_ref().unwrap().cancel));
    app.pending_browse.clear();
    app.tx.send(WorkerMsg::RestoreDone(0, Ok("Restored /tank/data/pics/x.restored-s2.txt (5 bytes, -rw-r--r--, owner root)".to_string()), Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.pending_browse.is_empty(), "the snapshot folder cannot show the live file, so nothing is re-read");
    assert_eq!(app.browse_view.as_ref().unwrap().request, request);
    assert!(!cancel.load(std::sync::atomic::Ordering::SeqCst), "a read in flight is not cancelled");
    assert_eq!(app.status_message.as_deref(), Some("Restored /tank/data/pics/x.restored-s2.txt (5 bytes, -rw-r--r--, owner root)"));
}

#[test]
fn a_failed_restore_shows_the_engines_sentence_without_the_error_prefix_and_no_restored_note() {
    let mut app = browsing_on_file();
    app.pending_browse.clear();
    let sentence = "that name already exists in the folder: nothing was copied";
    app.tx.send(WorkerMsg::RestoreDone(0, Err(sentence.to_string()), Err(crate::error::AppError::Config(sentence.into())))).unwrap();
    app.drain_worker_messages();
    assert!(app.pending_browse.is_empty());
    assert_eq!(app.status_message.as_deref(), Some(sentence));
}

#[test]
fn a_restore_note_for_another_host_does_not_show_up_on_a_later_hold() {
    let mut app = App::new(vec!["a".to_string(), "b".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.selected_host = 1;
    app.tx.send(WorkerMsg::RestoreDone(0, Ok("Restored /x (1 bytes, m, owner o)".to_string()), Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.as_deref().is_none_or(|m| !m.contains("Restored")), "a restore on another host says nothing here");
    app.tx.send(WorkerMsg::MutationDone(1, Section::Snapshots, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.as_deref().is_none_or(|m| !m.contains("Restored")), "{:?}", app.status_message);
}

#[test]
fn a_finished_snapshot_mutation_without_a_browser_queues_no_read() {
    let mut app = changes_app();
    app.tx.send(WorkerMsg::MutationDone(0, Section::Snapshots, Ok(()))).unwrap();
    app.drain_worker_messages();
    assert!(app.pending_browse.is_empty());
}

fn prompt_for_name(name: &str) -> String {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    type_into_form(&mut app, name);
    app.handle_modal_key(KeyCode::Enter);
    confirm_prompt(&app)
}

#[test]
fn the_restore_command_and_destination_are_shown_quoted_so_spaces_are_visible() {
    let p = prompt_for_name(" my copy ");
    assert!(p.contains("To:    '/tank/data/ my copy '\n"), "{p}");
    assert!(p.contains("Command: cp -p -n -- /tank/data/.zfs/snapshot/s2/a.txt '/tank/data/ my copy '\n"), "{p}");
    let p = prompt_for_name("x -rf passwd");
    assert!(p.contains("To:    '/tank/data/x -rf passwd'\n"), "{p}");
    assert!(p.contains(" '/tank/data/x -rf passwd'\n"), "{p}");
    let p = prompt_for_name("it's");
    assert!(p.contains(r"'/tank/data/it'\''s'"), "{p}");
}

#[test]
fn a_name_with_a_real_slash_is_refused_so_it_never_reaches_the_prompt() {
    let mut app = browsing_on_file();
    app.handle_key(KeyCode::Char('R'));
    type_into_form(&mut app, "x /etc/passwd");
    app.handle_modal_key(KeyCode::Enter);
    assert!(matches!(app.modal, Some(crate::ui::modal::Modal::Form { .. })));
}

#[test]
fn a_name_with_a_lookalike_slash_or_odd_whitespace_gets_a_check_it_note() {
    let note = "Note: this name contains unusual characters; check it letter by letter.";
    for name in ["a\u{2215}b", "a\u{2044}b", "a\u{29F8}b", "a\u{FF0F}b", "a\u{2571}b", "a\u{00A0}b", "a\u{200B}b"] {
        assert!(prompt_for_name(name).contains(note), "{name:?}");
    }
    assert!(!prompt_for_name("plain name.txt").contains("Note:"));
}

#[test]
fn a_restore_note_belongs_to_its_own_result_and_never_to_a_hold_that_finishes_first() {
    let mut app = browsing_app();
    // The hold finishes while the restore is still running: it must not show or take the restore's note.
    app.tx.send(WorkerMsg::MutationDone(0, Section::Snapshots, Ok(()))).unwrap();
    assert!(app.drain_worker_messages().contains(&Section::Snapshots));
    assert!(app.status_message.as_deref().is_none_or(|m| !m.contains("Restored")), "{:?}", app.status_message);
    app.tx.send(WorkerMsg::RestoreDone(0, Ok("Restored /tank/data/x (5 bytes, m, owner o)".to_string()), Ok(()))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message.as_deref(), Some("Restored /tank/data/x (5 bytes, m, owner o)"));
}

#[test]
fn a_direction_mark_in_the_source_name_gets_the_check_it_note_too() {
    let plan = |source: &str| crate::zfs::snapshot_restore::RestorePlan {
        source: source.into(),
        destination: "/tank/data/copy.txt".into(),
        dest_folder: "/tank/data".into(),
        dest_name: "copy.txt".into(),
        command: vec!["-p".into(), "-n".into(), "--".into(), source.into(), "/tank/data/copy.txt".into()],
        size: 5,
        mode: "-rw-r--r--".into(),
    };
    assert!(!restore_prompt(&plan("/tank/data/.zfs/snapshot/s2/gpfdp.txt")).contains("Note:"));
    assert!(restore_prompt(&plan("/tank/data/.zfs/snapshot/s2/gp\u{202E}fdp.txt")).contains("Note: this name contains unusual characters"));
}

#[test]
fn a_successful_restore_does_not_refresh_the_snapshot_list_so_its_message_is_not_wiped() {
    // Found on the hub: the refresh that follows a mutation is a manual one and clears the status line, so the
    // "Restored ..." message vanished at once. A restore changes no snapshot, so nothing needs refreshing.
    let mut app = browsing_app();
    app.tx.send(WorkerMsg::RestoreDone(0, Ok("Restored /tank/data/x.restored-s2.txt (5 bytes, -rw-r--r--, owner root)".to_string()), Ok(()))).unwrap();
    let refreshes = app.drain_worker_messages();
    assert!(!refreshes.contains(&Section::Snapshots), "{refreshes:?}");
    assert!(app.status_message.as_deref().is_some_and(|m| m.starts_with("Restored ")));
    // Any other Snapshots mutation (a hold, say) still refreshes the list.
    app.tx.send(WorkerMsg::MutationDone(0, Section::Snapshots, Ok(()))).unwrap();
    assert!(app.drain_worker_messages().contains(&Section::Snapshots));
}
