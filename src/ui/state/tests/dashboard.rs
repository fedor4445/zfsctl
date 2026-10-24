//! The Dashboard: its lines, the protection list and the fixes it opens.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn build_pending_action_add_acl_entry_assembles_named_user_with_flags_and_deny() {
    let kind = FormKind::AddAclEntry { host: 0, path: "/tank/data".to_string(), position: 3 };
    let fields = vec![
        FormField { label: "Recipient".to_string(), value: "user:nobody".to_string() },
        FormField { label: "Permissions".to_string(), value: "read_data, write_data".to_string() },
        FormField { label: "Flags".to_string(), value: "file_inherit,dir_inherit".to_string() },
        FormField { label: "Type".to_string(), value: "deny".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::DatasetAcl,
            call: MutationCall::AddAclEntry {
                path: "/tank/data".to_string(),
                position: 3,
                entry: crate::acl::AclEntry {
                    tag: crate::acl::AclTag::User("nobody".to_string()),
                    permissions: vec!["read_data".to_string(), "write_data".to_string()],
                    flags: vec!["file_inherit".to_string(), "dir_inherit".to_string()],
                    entry_type: crate::acl::AclEntryType::Deny,
                },
            },
        }
    );
}

#[test]
fn tab_cycles_through_jobs_cloud_alerts_and_settings_and_wraps_to_the_dashboard() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.sections.last(), Some(&Section::Settings));
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Jobs);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Cloud);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Alerts);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Settings);
    app.handle_key(KeyCode::Tab);
    assert_eq!(app.current_section(), Section::Dashboard, "wraps to the first section, the Dashboard");
    app.handle_key(KeyCode::BackTab);
    assert_eq!(app.current_section(), Section::Settings);
}

#[test]
fn the_program_opens_on_the_dashboard_which_is_the_first_sidebar_section() {
    let mut app = App::new(vec!["local".to_string()]);
    assert_eq!(app.sections.first(), Some(&Section::Dashboard));
    assert_eq!(app.current_section(), Section::Hosts, "a bare App starts on Hosts; the launch calls open_dashboard");
    app.open_dashboard();
    assert_eq!(app.current_section(), Section::Dashboard);
}

#[test]
fn the_dashboard_is_built_from_the_probes_the_readings_and_the_thresholds_in_config() {
    let (mut app, dir) = dash_app();
    app.host_probes.insert("local".into(), HostProbeState { last: Some(reached_probe(85)), resources: vec![reading(10), reading(10)], utc_offset_secs: Some(0), ..HostProbeState::default() });
    app.refresh_dashboard();
    let d = app.dashboard();
    assert_eq!(d.hosts.len(), 2, "every configured host has a row");
    assert_eq!(d.hosts[0].space.level, crate::ui::dashboard::Level::Watch);
    assert_eq!(d.hosts[1].state.level, crate::ui::dashboard::Level::Unknown, "fbsd16 has not answered yet");
    assert_eq!(d.verdict.text, "checking... 1 issue, 2 to watch", "known problems show while a host is still unanswered");
    assert_eq!(d.hosts[0].memory.level, crate::ui::dashboard::Level::Fine, "10 of 20 GiB free is fine with the defaults");
    std::fs::write(dir.path().join("config.toml"), "[dashboard]\nmemory_warn = 1\nmemory_crit = 2\n").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard().hosts[0].memory.level, crate::ui::dashboard::Level::Issue, "the [dashboard] thresholds are read when the Dashboard refreshes");
    std::fs::write(dir.path().join("config.toml"), "not toml [").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard().hosts[0].memory.level, crate::ui::dashboard::Level::Fine, "an unreadable config means the defaults");
}

#[test]
fn up_down_and_enter_on_the_dashboard_only_navigate() {
    let (mut app, _dir) = dash_app();
    app.refresh_jobs();
    let items = app.dashboard().attention;
    assert!(items.iter().any(|i| i.subject == "daemon"), "no heartbeat: the daemon is the one problem: {items:?}");
    app.handle_key(KeyCode::Down);
    assert_eq!(app.dashboard_table.selected(), Some(0));
    app.handle_key(KeyCode::Up);
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Jobs, "the daemon line belongs to Jobs");
    assert!(app.pending_mutations.is_empty() && app.modal.is_none() && app.picker.is_none(), "Enter changed nothing but the section");
}

#[test]
fn enter_on_a_named_protection_line_goes_straight_to_its_fix() {
    let mut app = unprotected_app(&["tank/docs", "tank/photos"]);
    let items = app.dashboard().attention;
    assert_eq!(items.iter().filter(|i| i.what.contains("no snapshot job")).count(), 2, "{items:?}");
    app.handle_key(KeyCode::Down);
    app.handle_key(KeyCode::Down);
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "Enter" && h.label == "Fix"), "the hint says what Enter does here");
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Dashboard);
    assert!(matches!(&app.modal, Some(Modal::Form { title, .. }) if title == "New snapshot job"), "no list in between: {:?}", app.modal);
    assert_eq!(field(app.modal.as_ref(), 1), ("Dataset".to_string(), "tank/photos".to_string()), "the line that was selected");
    assert_eq!(field(app.modal.as_ref(), 0).1, "local");
}

#[test]
fn a_probe_that_gave_up_leaves_no_old_readings_on_the_dashboard() {
    let (mut app, _dir) = dash_app();
    app.host_probes.insert("local".into(), HostProbeState { last: Some(reached_probe(85)), resources: vec![reading(10), reading(10)], utc_offset_secs: Some(0), in_flight: true, started_at: Some(std::time::Instant::now() - Duration::from_secs(120)), ..HostProbeState::default() });
    let d = app.dashboard();
    assert_eq!(d.hosts[0].space.level, crate::ui::dashboard::Level::Unknown);
    assert_eq!(d.hosts[0].memory.level, crate::ui::dashboard::Level::Unknown);
    assert!(d.hosts[0].state.text.starts_with("no answer"), "{:?}", d.hosts[0].state);
}

#[test]
fn enter_with_nothing_selected_and_keys_that_change_things_do_nothing_on_the_dashboard() {
    let (mut app, _dir) = dash_app();
    app.handle_key(KeyCode::Enter);
    for key in ['d', 'n', 'N', 'B', 's', ' '] {
        app.handle_key(KeyCode::Char(key));
    }
    assert_eq!(app.current_section(), Section::Dashboard);
    assert!(app.pending_mutations.is_empty() && app.modal.is_none(), "{:?}", app.modal);
}

#[test]
fn r_on_the_dashboard_reloads_the_jobs_and_asks_the_hub_for_its_remotes() {
    let (mut app, _dir) = dash_app();
    app.refresh_visible(Arc::new(FakeRunner::new("")));
    assert!(app.jobs_view.loaded, "the jobs are reloaded");
    assert_eq!(app.pending_cloud.len(), 1, "the hub is asked which rclone remotes it has");
}

#[test]
fn while_the_dashboard_is_open_the_jobs_alerts_and_cloud_list_keep_fresh() {
    let (mut app, _dir) = dash_app();
    let t0 = Instant::now();
    app.refresh_jobs_if_stale(t0);
    assert!(app.jobs_view.loaded, "jobs load on the first call");
    app.refresh_alerts_if_stale(t0);
    assert!(app.alerts_view.loaded, "alerts load too, they feed the Alerts line");
    app.refresh_cloud_if_stale(t0);
    assert_eq!(app.pending_cloud.len(), 1);
    app.pending_cloud.clear();
    app.tx.send(WorkerMsg::CloudRemotes("local".into(), Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(30));
    assert!(app.pending_cloud.is_empty(), "not again within a minute");
    app.refresh_cloud_if_stale(t0 + Duration::from_secs(61));
    assert_eq!(app.pending_cloud.len(), 1);
}

#[test]
fn a_wrong_protection_value_reaches_the_dashboard_as_a_watch_line() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[protection]\nlevel = \"Careful\"\n");
    assert_eq!(app.dashboard_protection_problem.as_deref(), Some("[protection] level must be basic, careful or custom"));
    assert_eq!(app.dashboard_protection, crate::config::ProtectionSettings::default());
    std::fs::write(dir.path().join("config.toml"), "[protection]\nemail = \"of\"\n").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard_protection_problem.as_deref(), Some("[protection] email must be immediate, report or off"));
    assert!(app.dashboard().attention.iter().any(|i| i.subject == "config.toml" && i.what.ends_with("judging at Basic")));
    std::fs::write(dir.path().join("config.toml"), "[protection]\nemail = \"off\"\n").unwrap();
    app.refresh_dashboard();
    assert!(app.dashboard_protection_problem.is_none());
    assert!(app.dashboard().attention.iter().all(|i| i.subject != "config.toml"));
}

#[test]
fn saving_thresholds_and_caps_reaches_the_dashboard_and_cloud_at_once() {
    let (mut app, dir) = settings_app("");
    saved_settings(&mut app, &dir, "[dashboard]\nmemory_warn = 85\nmemory_crit = 95\n[cloud.caps]\nb2 = \"10G\"\n");
    assert_eq!(app.dashboard_thresholds.memory, (85, 95));
    assert_eq!(app.cloud_caps.get("b2"), Some(&Ok(10_000_000_000)));
    app.refresh_settings();
    assert_eq!(app.settings_view.rows.iter().find(|r| r.label == "b2").unwrap().value, "10G");
}

#[test]
fn enter_on_a_share_line_selects_that_host_in_shares_and_the_marker_reaches_the_dashboard() {
    let (mut app, dir) = dash_app();
    let mut file = crate::daemon::shares_check::SharesFile::default();
    file.hosts.insert("fbsd16".into(), crate::daemon::shares_check::SharesResult { checked_at: chrono::Utc::now(), nfs_running: false, samba_running: true, nfs_shares: 2, smb_shares: 0, smb_error: None, error: None, nfs_name: None, smb_name: None });
    file.save(&dir.path().join("shares.json")).unwrap();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(10)), ..HostProbeState::default() });
    app.refresh_dashboard();
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.what.contains("nfsd is not running")).expect("a share line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Shares);
    assert_eq!(app.selected_host, 1);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
    assert_eq!(app.dashboard().running, None);
    let _guard = crate::daemon::checks::RunGuard::start(dir.path(), "daemon", None);
    app.refresh_checks_marker();
    assert!(app.dashboard().running.is_some(), "a fresh marker shows the notice");
}

#[test]
fn the_dashboard_reads_smart_json_and_enter_on_its_line_selects_that_host_in_disks() {
    let (mut app, dir) = dash_app();
    let mut file = crate::daemon::smart_check::SmartFile::default();
    file.hosts.insert(
        "fbsd16".into(),
        crate::daemon::smart_check::HostResult {
            checked_at: chrono::Utc::now() - chrono::Duration::hours(2),
            disks: vec![crate::smart::Disk { dev: "da1".into(), problems: vec![crate::smart::Problem { severity: crate::smart::Severity::Watch, text: "reallocated sectors 8".into() }] }],
            error: None,
        },
    );
    file.save(&dir.path().join("smart.json")).unwrap();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(10)), ..HostProbeState::default() });
    app.refresh_dashboard();
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.what.contains("da1 reallocated sectors 8")).expect("a disk line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Disks);
    assert_eq!(app.selected_host, 1);
}

#[test]
fn choosing_a_host_from_the_dashboard_clears_the_previous_hosts_data() {
    let (mut app, _dir) = dash_app();
    app.host_probes.insert("fbsd16".into(), HostProbeState { last: Some(reached_probe(95)), ..HostProbeState::default() });
    app.shares_loaded = true;
    app.nfs_available = Some(true);
    let items = app.dashboard().attention;
    let index = items.iter().position(|i| i.subject == "fbsd16").expect("fbsd16 has a line");
    for _ in 0..=index {
        app.handle_key(KeyCode::Down);
    }
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.selected_host, 1);
    assert!(!app.shares_loaded && app.nfs_available.is_none(), "what the hub's Shares screen loaded is not the new host's");
}

#[test]
fn a_reservation_above_the_quota_is_refused_with_the_quota_named() {
    let g: u64 = 1 << 30;
    let mut app = quota_app(false, 10 * g, 0, 0, 0);
    open_property(&mut app, "reservation");
    assert!(submit_value(&mut app, "12G").is_none());
    assert!(app.status_error.as_deref().is_some_and(|m| m.contains("larger than the quota")), "{:?}", app.status_error);
}

fn calm_probe() -> crate::zfs::host_health::HostProbe {
    use crate::zfs::host_health::{HostProbe, PoolProbe, ScanSummary};
    let when = chrono::Utc::now().format("%a %b %e %H:%M:%S %Y").to_string();
    let pool = PoolProbe { name: "tank".into(), health: crate::zfs::pool::PoolHealth::Online, capacity_percent: 10, frag_percent: None, errors: Some(0), scan: Some(ScanSummary::Finished { when: when.clone(), errors: 0 }), last_scrub: Some(when) };
    HostProbe::Reached { rtt_ms: 2, pools: vec![pool], kind: None, os: None, os_name: None }
}

/// Two hosts, `fbsd16` tagged `web` and `local` untagged, both healthy.
fn grouped_app() -> App {
    let (mut app, dir) = dash_app();
    std::mem::forget(dir);
    app.set_host_tags(vec![String::new(), "web".to_string()]);
    for name in ["local", "fbsd16"] {
        app.host_probes.insert(name.into(), HostProbeState { last: Some(calm_probe()), utc_offset_secs: Some(0), ..HostProbeState::default() });
    }
    app
}

#[test]
fn down_goes_through_the_attention_lines_and_then_the_groups_and_enter_on_a_group_opens_its_hosts() {
    let mut app = grouped_app();
    let d = app.dashboard();
    let n = d.attention.len();
    assert_eq!(d.selectable(), n + 2);
    for _ in 0..=n {
        app.handle_key(KeyCode::Down);
    }
    assert_eq!(app.dashboard_table.selected(), Some(n), "the first group row (web)");
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "Enter" && h.label == "Open group"));
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Hosts);
    assert_eq!(app.host_filter, "web", "the Hosts list is narrowed to the group");
    assert!(!app.host_filter_editing);
    assert_eq!(app.selected_host, 1, "the first host that matches");
    assert_eq!(app.visible_hosts(), vec![1]);
}

#[test]
fn enter_on_the_group_of_hosts_without_a_tag_opens_hosts_unfiltered_and_says_why() {
    let mut app = grouped_app();
    let n = app.dashboard().attention.len();
    for _ in 0..(n + 2) {
        app.handle_key(KeyCode::Down);
    }
    assert_eq!(app.dashboard_table.selected(), Some(n + 1), "the (no tag) group");
    app.handle_key(KeyCode::Enter);
    assert_eq!(app.current_section(), Section::Hosts);
    assert!(app.host_filter.is_empty(), "there is no tag to filter by");
    assert!(app.status_message.as_deref().unwrap_or("").contains("no tag"), "{:?}", app.status_message);
}

#[test]
fn e_shows_every_host_under_its_group_and_e_again_folds_them() {
    let mut app = grouped_app();
    assert!(app.dashboard().groups.unwrap().iter().all(|g| g.members.is_empty()));
    app.handle_key(KeyCode::Char('e'));
    assert!(app.dashboard_expanded);
    let groups = app.dashboard().groups.unwrap();
    assert_eq!(groups.iter().map(|g| g.members.len()).sum::<usize>(), 2, "both hosts listed");
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "e" && h.label == "Problem hosts only"));
    app.handle_key(KeyCode::Char('e'));
    assert!(!app.dashboard_expanded);
}

#[test]
fn e_does_nothing_without_tags_and_is_not_offered() {
    let (mut app, _dir) = dash_app();
    app.handle_key(KeyCode::Char('e'));
    assert!(!app.dashboard_expanded);
    assert!(!crate::ui::hints::hints(&app).iter().any(|h| h.key == "e"));
}

#[test]
fn the_dashboard_judges_each_group_by_its_own_level_and_names_a_wrong_group() {
    let (mut app, dir) = dash_app();
    std::fs::write(dir.path().join("config.toml"), "[[hosts]]\nname = \"fbsd16\"\nssh_target = \"u@x\"\ntag = \"db\"\n\n[groups.db.protection]\nlevel = \"careful\"\n").unwrap();
    app.refresh_dashboard();
    assert_eq!((app.dashboard_protection.for_host("fbsd16").copies, app.dashboard_protection.for_host("local").copies), (3, 1));
    assert!(app.dashboard_protection_problem.is_none());
    std::fs::write(dir.path().join("config.toml"), "[[hosts]]\nname = \"fbsd16\"\nssh_target = \"u@x\"\ntag = \"db\"\n\n[groups.db.protection]\nlevel = \"paranoid\"\n").unwrap();
    app.refresh_dashboard();
    assert_eq!(app.dashboard_protection.for_host("fbsd16").copies, 1, "a wrong group is judged by the global settings");
    assert_eq!(app.dashboard_protection_problem.as_deref(), Some("[groups.db.protection] level must be basic, careful or custom"));
    assert!(app.dashboard().attention.iter().any(|i| i.subject == "config.toml" && i.what.ends_with("those hosts use the global settings")), "{:?}", app.dashboard().attention);
}

#[test]
fn enter_on_the_data_safety_line_opens_the_graph_of_where_the_data_lives() {
    let (mut app, dir) = dash_app();
    std::mem::forget(dir);
    let mut job = crate::jobs::Job::test_replicate("r", "tank/data", "rep", "fbsd16", "tank/copy");
    job.host = "local".into();
    app.jobs_view.jobs = vec![job];
    app.jobs_view.loaded = true;
    let d = app.dashboard();
    assert!(d.safety.is_some());
    for _ in 0..d.selectable() {
        app.handle_key(KeyCode::Down);
    }
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "Enter" && h.label == "Show graph"));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Info { title, body }) => {
            assert_eq!(title, "Data safety");
            assert!(body.contains("local:tank/data") && body.contains("fbsd16:tank/copy") && body.contains("never ran"), "{body}");
        }
        other => panic!("expected the graph, got {other:?}"),
    }
    assert_eq!(app.current_section(), Section::Dashboard, "it opens over the Dashboard");
}
