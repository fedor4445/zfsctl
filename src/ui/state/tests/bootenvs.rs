//! Boot environments.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn boot_envs_sits_between_shares_and_jobs() {
    let app = App::new(vec!["local".to_string()]);
    let at = |s| app.sections.iter().position(|x| *x == s).unwrap();
    assert_eq!(at(Section::BootEnvs), at(Section::Shares) + 1);
    assert_eq!(at(Section::Jobs), at(Section::BootEnvs) + 1);
    assert_eq!(app.sections.last(), Some(&Section::Settings));
}

#[test]
fn refresh_bootenvs_populates_app_state() {
    let mut app = App::new(vec!["local".to_string()]);
    app.refresh_bootenvs(runner("default\tNR\t/\t5.18G\t2026-07-24 18:48\nspare\t-\t-\t8K\t2026-10-07 19:26\n"));
    wait_until(&mut app, |a| !a.bootenvs.is_empty());
    assert_eq!(app.bootenvs.len(), 2);
    assert!(app.bootenvs[0].flags.now && app.bootenvs[0].flags.reboot);
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn a_failing_bectl_marks_the_section_unavailable_and_the_next_good_refresh_recovers() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("stale", false, false, false, None)];
    app.bootenvs_table.select(Some(0));
    app.tx.send(WorkerMsg::BootEnvs(0, Err(crate::error::AppError::CommandFailed { cmd: "bectl list".into(), status: 127, stderr: "bectl: not found".into() }))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty(), "a stale list must not look current");
    assert_eq!(app.bootenvs_table.selected(), None);
    assert!(app.bootenvs_unavailable.as_deref().is_some_and(|m| m.contains("bectl: not found")));
    // Not gated on the earlier failure: the periodic refresh still runs and recovers.
    app.auto_refresh(runner("default\tNR\t/\t1G\td\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.bootenvs.is_empty());
    assert_eq!(app.bootenvs_unavailable, None);
    assert_eq!(app.bootenvs.len(), 1);
}

#[test]
fn a_host_with_zero_boot_environments_is_empty_not_unavailable() {
    let mut app = in_section(Section::BootEnvs);
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty());
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn bootenvs_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx.send(WorkerMsg::BootEnvs(1, Ok(vec![be("x", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert!(app.bootenvs.is_empty());
}

#[test]
fn switching_host_clears_boot_environment_state() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.bootenvs = vec![be("x", false, false, false, None)];
    app.bootenvs_table.select(Some(0));
    app.bootenvs_unavailable = Some("boom".to_string());
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert!(app.bootenvs.is_empty());
    assert_eq!(app.bootenvs_table.selected(), None);
    assert_eq!(app.bootenvs_unavailable, None);
}

#[test]
fn a_refresh_keeps_the_selection_on_the_same_boot_environment_and_clamps_when_it_vanishes() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
    app.bootenvs_table.select(Some(1));
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("0", false, false, false, None), be("a", false, false, false, None), be("b", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), Some(2), "still on b");
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![be("a", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), Some(0), "b vanished: clamped into range");
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.bootenvs_table.selected(), None);
}

#[test]
fn up_and_down_move_the_boot_environment_selection() {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = vec![be("a", false, false, false, None), be("b", false, false, false, None)];
    app.handle_key(KeyCode::Down);
    assert_eq!(app.bootenvs_table.selected(), Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("b"));
    app.handle_key(KeyCode::Up);
    assert_eq!(app.bootenvs_table.selected(), Some(0));
}

fn only_pending(app: &App) -> &MutationCall {
    let [action] = app.pending_mutations.as_slice() else { panic!("{:?}", app.pending_mutations) };
    assert_eq!(action.section, Section::BootEnvs);
    &action.call
}

#[test]
fn n_works_with_zero_boot_environments_but_not_when_unavailable() {
    let mut app = in_section(Section::BootEnvs);
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_some());
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs_unavailable = Some("no bectl".to_string());
    app.handle_key(KeyCode::Char('n'));
    assert!(app.modal.is_none());
}

#[test]
fn build_bootenv_action_makes_the_expected_calls() {
    let create = FormKind::CreateBootEnv { host: 0, existing: vec!["default".to_string()] };
    let field = |v: &str| FormField { label: "l".to_string(), value: v.to_string() };
    let a = build_bootenv_action(&create, &[field("new"), field("default")]).unwrap();
    assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: Some("default".to_string()) });
    assert_eq!(a.section, Section::BootEnvs);
    let a = build_bootenv_action(&create, &[field("new"), field("")]).unwrap();
    assert_eq!(a.call, MutationCall::CreateBootEnv { name: "new".to_string(), from: None });
    let rename = FormKind::RenameBootEnv { host: 2, from: "old".to_string(), existing: vec!["old".to_string(), "taken".to_string()] };
    let a = build_bootenv_action(&rename, &[field("fresh")]).unwrap();
    assert_eq!((a.host, a.call), (2, MutationCall::RenameBootEnv { from: "old".to_string(), to: "fresh".to_string() }));
    assert!(build_bootenv_action(&rename, &[field("taken")]).is_err());
    assert!(build_bootenv_action(&rename, &[field("old")]).is_err(), "renaming to itself");
    assert!(build_bootenv_action(&rename, &[field("-x")]).is_err());
}

#[test]
fn a_asks_to_confirm_then_activates_permanently_and_only_when_it_would_change_something() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('a'));
    let Some(Modal::Confirm { prompt, action, .. }) = &app.modal else { panic!("{:?}", app.modal) };
    assert!(prompt.contains("'spare'"), "{prompt}");
    assert_eq!(action.call, MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: false });
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('a'));
    assert!(app.modal.is_none(), "already the BE used on reboot");
}

#[test]
fn big_t_clears_a_temporary_activation_without_a_confirm() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('T'));
    assert!(app.pending_mutations.is_empty(), "spare has no temporary activation to clear");
    let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
    app.handle_key(KeyCode::Char('T'));
    assert_eq!(only_pending(&app), &MutationCall::ClearTemporaryBootEnv { name: "trial".to_string() });
    let mut app = bootenv_app(vec![running(), be("trial", false, false, true, None)], 1);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.pending_mutations.is_empty(), "already temporary");
    let mut app = bootenv_app(vec![running()], 0);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.pending_mutations.is_empty(), "the running BE");
}

#[test]
fn m_mounts_an_unmounted_be_and_u_unmounts_a_mounted_one() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(only_pending(&app), &MutationCall::MountBootEnv { name: "spare".to_string() });
    app.handle_key(KeyCode::Char('u'));
    assert_eq!(app.pending_mutations.len(), 1, "an unmounted BE cannot be unmounted");
    let mut app = bootenv_app(vec![running(), be("m", false, false, false, Some("/tmp/be_mount.X"))], 1);
    app.handle_key(KeyCode::Char('u'));
    assert_eq!(only_pending(&app), &MutationCall::UnmountBootEnv { name: "m".to_string() });
    app.handle_key(KeyCode::Char('m'));
    assert_eq!(app.pending_mutations.len(), 1, "already mounted");
    let mut app = bootenv_app(vec![running()], 0);
    app.handle_key(KeyCode::Char('m'));
    app.handle_key(KeyCode::Char('u'));
    assert!(app.pending_mutations.is_empty(), "the running BE is mounted at / and cannot be remounted or unmounted");
}

#[test]
fn a_successful_boot_environment_mutation_queues_a_re_list() {
    let mut app = in_section(Section::BootEnvs);
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
    assert_eq!(app.drain_worker_messages(), vec![Section::BootEnvs]);
}

#[test]
fn help_documents_the_boot_environment_keys() {
    let text = help_text();
    for want in ["Boot Envs", "next boot only", "rename", "mount", "destroy"] {
        assert!(text.contains(want), "missing {want:?}");
    }
}

#[test]
fn the_selection_follows_a_renamed_boot_environment_instead_of_sliding_to_its_neighbour() {
    let rows = vec![running(), spare(), be("zeta", false, false, false, None)];
    let mut app = bootenv_app(rows, 1); // spare
    rename_to(&mut app, "zz");
    app.tx.send(WorkerMsg::MutationDone(0, Section::BootEnvs, Ok(()))).unwrap();
    app.tx.send(WorkerMsg::BootEnvs(0, Ok(vec![running(), be("zeta", false, false, false, None), be("zz", false, false, false, None)]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(names(&app), ["default", "zeta", "zz"]);
    assert_eq!(app.selected_bootenv().map(|b| b.name.as_str()), Some("zz"), "index 1 is now zeta: the key must not act on it");
}

#[test]
fn recursive_destroy_is_refused_on_boot_environment_datasets_and_on_anything_holding_the_running_root() {
    let all = || vec![
        ds_named("zroot", Some("/zroot")),
        ds_named("zroot/ROOT", None),
        ds_named("zroot/ROOT/default", Some("/")),
        ds_named("zroot/ROOT/other", None),
        ds_named("zroot/rootfs", None),
        ds_named("zroot/rootfs/inner", Some("/")),
        ds_named("zroot/home", Some("/home")),
    ];
    for refused in ["zroot/ROOT", "zroot/ROOT/default", "zroot/ROOT/other", "zroot/rootfs", "zroot/rootfs/inner"] {
        let mut app = datasets_app_selecting(all(), refused);
        app.handle_key(KeyCode::Char('D'));
        assert!(app.modal.is_none(), "{refused} must be refused");
        assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Boot Envs") || m.contains("running")), "{refused}: {:?}", app.status_message);
    }
    let mut ok = datasets_app_selecting(all(), "zroot/home");
    ok.handle_key(KeyCode::Char('D'));
    assert!(matches!(ok.modal, Some(Modal::ConfirmTyped { .. })), "an ordinary dataset still works");
}

#[test]
fn the_recursive_prompt_admits_partial_destruction_and_describes_clones_precisely() {
    let mut app = destroy_app();
    app.handle_key(KeyCode::Char('D'));
    let Some(Modal::ConfirmTyped { prompt, .. }) = &app.modal else { panic!() };
    assert!(prompt.contains("stops part-way") && prompt.contains("stays destroyed"), "{prompt}");
    assert!(prompt.contains("clones inside it go with it") && prompt.contains("outside"), "{prompt}");
}

#[test]
fn boot_once_asks_first_and_only_y_then_enter_queues_it() {
    let mut app = bootenv_app(vec![running(), spare()], 1);
    app.handle_key(KeyCode::Char('t'));
    match &app.modal {
        Some(Modal::Confirm { prompt, choice: None, .. }) => assert!(prompt.contains("'spare'") && prompt.contains("NEXT boot"), "{prompt}"),
        other => panic!("{other:?}"),
    }
    assert!(app.pending_mutations.is_empty(), "nothing is queued before the answer");
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    app.handle_modal_key(KeyCode::Char('y'));
    let action = app.handle_modal_key(KeyCode::Enter).expect("runs after y then Enter");
    assert_eq!(action.call, MutationCall::ActivateBootEnv { name: "spare".to_string(), temporary: true });
    let mut app = bootenv_app(vec![running(), spare()], 0);
    app.handle_key(KeyCode::Char('t'));
    assert!(app.modal.is_none(), "the running boot environment has nothing to boot once");
}

#[test]
fn selecting_a_host_that_is_not_freebsd_dims_boot_envs_and_a_freebsd_host_restores_it() {
    let mut app = two_os_app(crate::zfs::host_health::HostOs::Other);
    app.selected_host = 0;
    assert!(app.section_supported(Section::BootEnvs));
    app.selected_host = 1;
    assert!(!app.section_supported(Section::BootEnvs) && !app.section_supported(Section::DatasetAcl));
    assert!(!app.section_supported(Section::Disks) && !app.section_supported(Section::Shares) && app.section_supported(Section::Pools));
    go_to(&mut app, Section::BootEnvs);
    assert!(!app.current_section_supported());
    app.selected_host = 0;
    assert!(app.current_section_supported(), "back on the FreeBSD host everything is offered again");
}
