//! Refreshing: manual and automatic, in-flight work, stale results and keeping the selection.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn dispatch_mutation_failure_sets_status_message_and_queues_no_refresh() {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction {
        host: 0,
        section: Section::Pools,
        call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() },
    };
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::failing("pool is busy", 1));
    app.dispatch_mutation(action, runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if app.status_message.is_some() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(refreshes.is_empty());
    assert!(app.status_message.is_some());
}

#[test]
fn drain_worker_messages_sets_status_message_on_failure() {
    let mut app = App::new(vec!["local".to_string()]);
    app.tx
        .send(WorkerMsg::Datasets(0, Err(crate::error::AppError::Parse("boom".to_string()))))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.status_message.is_some());
}

#[test]
fn drain_worker_messages_clears_status_message_on_subsequent_success() {
    let mut app = App::new(vec!["local".to_string()]);
    app.status_message = Some("stale error".to_string());
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.status_message, None);
}

#[test]
fn refresh_request_while_in_flight_is_queued_and_redispatched_on_completion() {
    // Simulates: the user presses 'r' while a refresh for the same
    // (section, host) is already in flight (e.g. a slow SSH call, or a
    // post-mutation auto-refresh landing mid-flight). The second
    // request must not be silently dropped — it must run once the
    // first one completes.
    let mut app = App::new(vec!["local".to_string()]);
    let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    let runner: Arc<dyn CommandRunner> = fake.clone();

    app.refresh_pools(runner.clone());
    assert!(app.is_refreshing(Section::Pools, 0));

    // Second request while the first is still in flight.
    app.refresh_pools(runner.clone());
    assert!(
        app.pending_refresh.contains(&(Section::Pools, 0)),
        "the second request must be remembered rather than dropped"
    );

    // Let the first refresh complete and drain its result.
    let deadline = Instant::now() + Duration::from_secs(2);
    let refreshes = loop {
        let r = app.drain_worker_messages();
        if !r.is_empty() || Instant::now() > deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(refreshes, vec![Section::Pools], "the queued request must surface for the event loop to redispatch");
    assert!(
        !app.pending_refresh.contains(&(Section::Pools, 0)),
        "the pending flag must be cleared once it has been surfaced for redispatch"
    );

    // Simulate the event loop redispatching the queued request.
    app.refresh_pools(runner.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if fake.calls.lock().unwrap().len() >= 2 || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.calls.lock().unwrap().len(), 2, "the redispatched refresh must actually run against the runner");
}

#[test]
fn refresh_visible_reloads_the_list_you_are_looking_at() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
    app.refresh_visible(runner);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.pools.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pools.len(), 1);
}

#[test]
fn auto_refresh_fires_immediately_then_waits_for_the_interval() {
    let mut app = in_section(Section::Pools);
    let t0 = Instant::now();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.pools.len(), 1, "first call refreshes straight away");

    app.pools.clear();
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + Duration::from_secs(9), TEN);
    std::thread::sleep(Duration::from_millis(100));
    app.drain_worker_messages();
    assert!(app.pools.is_empty(), "9s < 10s: not due yet");

    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), t0 + TEN, TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.pools.len(), 1, "due at exactly the interval");
}

#[test]
fn an_auto_refresh_is_quiet_but_a_manual_one_shows_the_loading_indicator() {
    let mut app = in_section(Section::Pools);
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    assert!(!app.is_loading(), "background polling must not flash the refreshing indicator");
    wait_until(&mut app, |a| !a.pools.is_empty());
    app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
    assert!(app.is_loading());
    wait_until(&mut app, |a| !a.is_loading());
    assert!(!app.is_loading());
}

#[test]
fn a_quiet_refresh_keeps_an_informational_status_but_a_manual_one_clears_it() {
    let mut app = in_section(Section::Pools);
    app.status_message = Some("scrub started on 'tank'".to_string());
    app.auto_refresh(runner("tank\t100\t10\t90\tONLINE\n"), Instant::now(), TEN);
    wait_until(&mut app, |a| !a.pools.is_empty());
    assert_eq!(app.status_message.as_deref(), Some("scrub started on 'tank'"));

    app.refresh_pools(runner("tank\t100\t10\t90\tONLINE\n"));
    wait_until(&mut app, |a| a.status_message.is_none());
    assert!(app.status_message.is_none());
}

#[test]
fn a_refresh_keeps_the_selection_on_the_same_item_when_the_list_reorders() {
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
    app.pools_table.select(Some(1)); // bravo
    let reordered = vec![pool_fixture("new"), pool_fixture("alpha"), pool_fixture("charlie"), pool_fixture("bravo")];
    app.tx.send(WorkerMsg::Pools(0, Ok(reordered))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(3));
    assert_eq!(app.selected_pool_name().as_deref(), Some("bravo"), "a keypress must still act on bravo");
}

#[test]
fn a_refresh_falls_back_to_clamping_when_the_selected_item_is_gone() {
    let mut app = in_section(Section::Pools);
    app.pools = vec![pool_fixture("alpha"), pool_fixture("bravo"), pool_fixture("charlie")];
    app.pools_table.select(Some(2));
    app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool_fixture("alpha")]))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.pools_table.selected(), Some(0));
}

#[test]
fn a_result_for_other_values_is_discarded_and_a_matching_one_is_kept() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    let stale = crate::restic::Preflight::Problem("old".into());
    app.tx.send(WorkerMsg::Preflight(0, "/some/other/repo".into(), "/root/pw".into(), stale, None)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.backup_preflight.as_ref().unwrap().result, None);
    app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok, None)).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.backup_preflight.as_ref().unwrap().result, Some(crate::restic::Preflight::Ok));
}

#[test]
fn a_check_result_carries_its_password_file_note_and_a_new_check_clears_it() {
    let mut app = backup_form_app(BACKUP_GOOD);
    app.handle_modal_key(KeyCode::F(5));
    app.pending_preflight.clear();
    let note = "the password file can be read or changed by other users (mode 644): run `chmod 600 /root/pw`".to_string();
    app.tx.send(WorkerMsg::Preflight(0, "/mnt/usb/restic".into(), "/root/pw".into(), crate::restic::Preflight::Ok, Some(note.clone()))).unwrap();
    app.drain_worker_messages();
    let check = app.backup_preflight.as_ref().unwrap();
    assert_eq!((check.result.clone(), check.file_note.clone()), (Some(crate::restic::Preflight::Ok), Some(note)));
    app.handle_modal_key(KeyCode::F(5));
    let check = app.backup_preflight.as_ref().unwrap();
    assert_eq!((check.result.clone(), check.file_note.clone()), (None, None), "a new check starts without the old advice");
}

#[test]
fn c_is_refused_while_a_fresh_marker_exists_and_works_again_when_it_is_stale() {
    let (mut app, dir) = hosts_app();
    {
        let _daemon = crate::daemon::checks::RunGuard::start(dir.path(), "daemon", None);
        app.handle_key(KeyCode::Char('c'));
        assert!(app.pending_host_checks.is_empty());
        let said = app.status_error.clone().unwrap_or_default();
        assert!(said.contains("already running") && said.contains("daemon"), "{said}");
    }
    // a run that died: its marker has no heartbeat for over three minutes
    let old = chrono::Utc::now() - chrono::Duration::minutes(10);
    std::fs::write(dir.path().join(crate::daemon::checks::MARKER_FILE), serde_json::to_string(&crate::daemon::checks::Marker { by: "daemon".into(), since: old, beat: old, host: None }).unwrap()).unwrap();
    app.status_error = None;
    app.handle_key(KeyCode::Char('c'));
    assert_eq!(app.pending_host_checks, ["fbsd16"]);
}

#[test]
fn a_load_that_left_the_key_loaded_but_failed_to_mount_refreshes_the_list() {
    let mut app = key_app(true, "passphrase");
    app.tx.send(WorkerMsg::MutationDone(0, Section::Datasets, Err(crate::error::AppError::CommandFailed { cmd: "zfs mount x".into(), status: 1, stderr: "busy\nThe key IS loaded; only the mount failed.".into() }))).unwrap();
    assert_eq!(app.drain_worker_messages(), vec![Section::Datasets]);
    assert!(app.status_is_error());
}

#[test]
fn an_error_status_survives_a_quiet_refresh_for_eight_seconds_only() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("protected: x".to_string());
    app.clear_status_after_refresh(true);
    assert_eq!(app.status_message.as_deref(), Some("protected: x"));
    app.status_error_at = Instant::now().checked_sub(Duration::from_secs(9));
    app.clear_status_after_refresh(true);
    assert!(app.status_message.is_none());
}

#[test]
fn a_non_quiet_refresh_clears_a_young_error_at_once() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("protected: x".to_string());
    app.clear_status_after_refresh(false);
    assert!(app.status_message.is_none());
}
