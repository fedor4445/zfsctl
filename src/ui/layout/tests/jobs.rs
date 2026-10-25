//! The Jobs screen, its panel and the job forms' preview and check lines.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn create_pool_form_with_six_long_identity_lines_still_shows_last_field_at_80x24() {
    let text = app_with_form_and_disks(6, 80, 24).join("\n");
    assert!(text.contains("Redundancy"), "last input field clipped:\n{text}");
    assert!(text.contains("Name:"), "{text}");
}

fn jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
    app.refresh_jobs();
    (app, dir)
}

fn a_job(id: &str, schedule: &str) -> crate::jobs::Job {
    crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: schedule.into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, check_days: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
}

#[test]
fn the_jobs_table_shows_each_job_with_its_schedule_next_run_and_last_result() {
    let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0"), a_job("custom", "*/10 * * * *")]);
    let h = crate::daemon::history::History::new(dir.path().join("history.jsonl"));
    let when = chrono::Utc::now() - chrono::Duration::hours(2);
    h.append(&crate::daemon::history::Event { job_id: "scrub-local-tank".into(), run_id: "r1".into(), event: crate::daemon::history::EventKind::Finished, at: when, scheduled_for: Some(when), result: Some(crate::daemon::history::RunResult::Ok), message: String::new(), catch_up: false, summary: None }).unwrap();
    app.refresh_jobs();
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["Job", "Host", "Target", "Schedule", "Next run", "Last", "scrub-local-tank", "Weekly, Sunday 03:00", "*/10 * * * *", "ok 2h ago", "never"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_next_run_column_is_wide_enough_for_the_whole_time() {
    let (mut app, _dir) = jobs_app(&[a_job("a", "* * * * *")]);
    let before = crate::ui::jobs::next_run_text("* * * * *", &chrono::Local::now());
    let text = render(&mut app, 120, 30).join("\n");
    let after = crate::ui::jobs::next_run_text("* * * * *", &chrono::Local::now());
    assert!(text.contains(&before) || text.contains(&after), "the next-run time is cut off (wanted {before:?} or {after:?}):\n{text}");
}

#[test]
fn a_disabled_job_says_so_and_an_unsupported_one_shows_why() {
    let mut off = a_job("off", "0 3 * * 0");
    off.enabled = false;
    let mut bad = a_job("bad", "0 3 * * 0");
    bad.host = "gone".into();
    let (mut app, _dir) = jobs_app(&[off, bad]);
    let text = render(&mut app, 150, 30).join("\n");
    assert!(text.contains("disabled"), "{text}");
    assert!(text.contains("'gone' is not in config.toml"), "{text}");
}

#[test]
fn the_jobs_title_says_whether_the_daemon_is_running() {
    let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("daemon NOT running"), "{text}");
    crate::daemon::heartbeat::write(&dir.path().join("daemon.json"), &crate::daemon::heartbeat::Heartbeat { pid: 4242, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() }).unwrap();
    app.refresh_jobs();
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("daemon running (pid 4242)"), "{text}");
}

#[test]
fn an_empty_jobs_list_explains_how_to_add_one_and_an_unreadable_file_is_flagged() {
    let (mut app, dir) = jobs_app(&[]);
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("No scheduled jobs") && text.contains("'n'"), "{text}");
    std::fs::write(dir.path().join("jobs.toml"), "not [[[ toml").unwrap();
    app.refresh_jobs();
    let text = render(&mut app, 140, 30).join("\n");
    assert!(text.contains("jobs.toml unreadable"), "{text}");
}

fn job_form_app() -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.pools = vec![pool("tank", 10, 100, PoolHealth::Online)];
    app.pools_table.select(Some(0));
    app.handle_key(crossterm::event::KeyCode::Char('J'));
    app
}

#[test]
fn the_job_form_previews_what_the_schedule_means_and_when_it_next_runs() {
    let mut app = job_form_app();
    let text = render(&mut app, 100, 30).join("\n");
    for want in ["New scrub job", "Host: local", "Pool: tank", "Schedule: 0 3 * * 0", "Weekly, Sunday 03:00", "Next: Sun "] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert_eq!(text.matches("03:00").count(), 4, "the label and the three listed run times:\n{text}");
}

#[test]
fn the_job_forms_preview_follows_what_is_typed_and_shows_a_parse_error() {
    let mut app = job_form_app();
    let Some(crate::ui::modal::Modal::Form { fields, .. }) = &mut app.modal else { panic!() };
    fields[2].value = "61 * * * *".to_string();
    let text = render(&mut app, 100, 30).join("\n");
    assert!(text.contains("Schedule error:") && text.contains("outside 0-59"), "{text}");
    assert!(!text.contains("Next:"));
}

fn history_event(job_id: &str, run_id: &str, kind: crate::daemon::history::EventKind, ago_mins: i64, result: Option<crate::daemon::history::RunResult>, message: &str) -> crate::daemon::history::Event {
    let at = chrono::Utc::now() - chrono::Duration::minutes(ago_mins);
    crate::daemon::history::Event { job_id: job_id.into(), run_id: run_id.into(), event: kind, at, scheduled_for: Some(at), result, message: message.into(), catch_up: false, summary: None }
}

#[test]
fn the_jobs_panel_shows_the_selected_jobs_runs_schedule_next_times_and_flags() {
    use crate::daemon::history::{EventKind, History, RunResult};
    let (mut app, dir) = jobs_app(&[a_job("scrub-local-tank", "0 3 * * 0")]);
    let h = History::new(dir.path().join("history.jsonl"));
    h.append(&history_event("scrub-local-tank", "r1", EventKind::Started, 300, None, "")).unwrap();
    h.append(&history_event("scrub-local-tank", "r1", EventKind::Finished, 290, Some(RunResult::Failed), "scrub repaired 12K with 3 errors")).unwrap();
    h.append(&history_event("scrub-local-tank", "r2", EventKind::Skipped, 10, Some(RunResult::Skipped), "skipped: already scanning")).unwrap();
    app.refresh_jobs();
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 130, 34).join("\n");
    for want in ["Job — scrub-local-tank", "Last runs", "failed", "10m 00s", "scrub repaired 12K", "skipped", "skipped: already scanning", "Weekly, Sunday 03:00", "0 3 * * 0", "Next:", "enabled", "catch up"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_jobs_panel_says_when_nothing_is_selected_and_shows_an_unsupported_reason_in_full() {
    let mut bad = a_job("bad", "0 3 * * 0");
    bad.host = "somewhere-else".into();
    let (mut app, _dir) = jobs_app(&[bad]);
    let text = render(&mut app, 120, 34).join("\n");
    assert!(text.contains("nothing selected"), "{text}");
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 150, 34).join("\n");
    assert!(text.contains("host 'somewhere-else' is not in config.toml"), "{text}");
    assert!(text.contains("No runs yet") || text.contains("never"), "{text}");
}

#[test]
fn the_jobs_panel_is_dropped_on_a_short_terminal_so_the_table_keeps_its_height() {
    let (mut app, _dir) = jobs_app(&[a_job("a", "0 3 * * 0")]);
    let text = render(&mut app, 120, 12).join("\n");
    assert!(!text.contains("Last runs"), "{text}");
}

#[test]
fn failed_jobs_are_flagged_in_the_status_line_from_any_section() {
    use crate::daemon::history::{EventKind, History, RunResult};
    let (mut app, dir) = jobs_app(&[a_job("a", "0 3 * * 0"), a_job("b", "0 3 * * 0")]);
    let h = History::new(dir.path().join("history.jsonl"));
    h.append(&history_event("a", "r1", EventKind::Finished, 60, Some(RunResult::Failed), "boom")).unwrap();
    app.refresh_jobs();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("1 job failed"), "{text}");
    h.append(&history_event("b", "r2", EventKind::Finished, 30, Some(RunResult::Failed), "boom")).unwrap();
    app.refresh_jobs();
    assert!(render(&mut app, 120, 30).join("\n").contains("2 jobs failed"));
    h.append(&history_event("a", "r3", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
    h.append(&history_event("b", "r4", EventKind::Finished, 5, Some(RunResult::Ok), "")).unwrap();
    app.refresh_jobs();
    assert!(!render(&mut app, 120, 30).join("\n").contains("failed —"), "recovered jobs clear the indicator");
}

#[test]
fn a_snapshot_job_shows_its_dataset_as_the_target_and_its_keep_prefix_and_recursion_in_the_panel() {
    let mut snap = crate::jobs::Job::test_snapshot("s", "tank/data", "hourly", 24);
    snap.recursive = true;
    let (mut app, _dir) = jobs_app(&[snap]);
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 130, 34).join("\n");
    assert!(text.contains("Target"), "{text}");
    assert!(text.contains("tank/data"), "{text}");
    assert!(text.contains("keep 24") && text.contains("hourly") && text.contains("recursive"), "{text}");
}

#[test]
fn the_snapshot_forms_preview_follows_its_schedule_field() {
    let (mut app, _dir) = jobs_app(&[]);
    app.handle_key(crossterm::event::KeyCode::Char('N'));
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("New snapshot job") && text.contains("Hourly") && text.contains("Next:"), "the default 0 * * * * is the Hourly preset:\n{text}");
}

#[test]
fn the_mirror_forms_check_result_reads_remote_not_repository() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form {
        title: "x".into(),
        fields: ["local", "tank/data", "", "hourly", "", "aws:bkt", "", "30 3 * * *", "y"].iter().map(|v| crate::ui::modal::FormField { label: "x".into(), value: (*v).into() }).collect(),
        focused: 5,
        description: None,
        kind: crate::ui::modal::FormKind::MirrorJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
    });
    app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "aws:bkt".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: Some("chmod it".into()) });
    let text = render(&mut app, 120, 40).join("\n");
    assert!(text.contains("Check: OK, the remote opens.") && text.contains("Note: chmod it"), "{text}");
    app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "aws:other".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
    assert!(!render(&mut app, 120, 40).join("\n").contains("Check:"), "an edited remote hides the old answer");
}

#[test]
fn a_mirror_job_shows_its_remote_as_the_target_and_the_limit_only_when_set() {
    let mut m = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos");
    m.bwlimit = Some("5M".into());
    let (mut app, _dir) = jobs_app(&[m]);
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 130, 34).join("\n");
    assert!(text.contains("tank/data") && text.contains("aws:bkt/photos") && text.contains("limit 5M/s"), "{text}");
    let (mut app, _dir) = jobs_app(&[crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "aws:bkt/photos")]);
    app.handle_key(crossterm::event::KeyCode::Down);
    assert!(!render(&mut app, 130, 34).join("\n").contains("limit "));
}

#[test]
fn a_replicate_job_with_dest_keep_says_how_many_it_keeps_on_the_target() {
    let mut repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
    repl.dest_keep = Some(7);
    let (mut app, _dir) = jobs_app(&[repl]);
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 130, 34).join("\n");
    assert!(text.contains("keep 7 on target"), "{text}");
    let plain = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
    let (mut app, _dir) = jobs_app(&[plain]);
    app.handle_key(crossterm::event::KeyCode::Down);
    assert!(!render(&mut app, 130, 34).join("\n").contains("on target"));
}

#[test]
fn a_replicate_job_shows_its_source_as_the_target_and_destination_prefix_and_progress_in_the_panel() {
    let repl = crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "local", "backup/data");
    let (mut app, dir) = jobs_app(&[repl]);
    let p = crate::daemon::progress::Progress { job_id: "r".into(), bytes: 5, total: Some(10), started_at: chrono::Utc::now(), updated_at: chrono::Utc::now(), rate: None };
    crate::daemon::progress::write(&dir.path().join("progress"), "run1", &p).unwrap();
    app.refresh_jobs();
    app.handle_key(crossterm::event::KeyCode::Down);
    let text = render(&mut app, 130, 34).join("\n");
    for want in ["tank/data", "to local:backup/data", "prefix hourly", "sent 5 B of 10 B (50%)"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_replication_forms_preview_follows_its_schedule_field() {
    let (mut app, _dir) = jobs_app(&[]);
    app.handle_key(crossterm::event::KeyCode::Char('R'));
    let text = render(&mut app, 120, 34).join("\n");
    assert!(text.contains("New replication job") && text.contains("Next:"), "{text}");
}

#[test]
fn a_backup_job_shows_its_repository_retention_and_progress_in_the_details() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
    let mut job = crate::jobs::Job::test_backup("backup-x", "tank/data", "hourly", "/mnt/usb/restic", "/root/pw");
    job.retention = Some("7d 4w".to_string());
    app.jobs_view.jobs = vec![job];
    app.jobs_table.select(Some(0));
    let text = render(&mut app, 140, 30).join("\n");
    for want in ["tank/data", "to /mnt/usb/restic", "keep 7d 4w", "prefix hourly"] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("password"), "the password file path is not shown");
}


#[test]
fn the_jobs_table_shows_where_each_job_sends_its_data_and_clamps_on_a_narrow_terminal() {
    let repl = crate::jobs::Job::test_replicate("repl", "tank/data", "hourly", "local", "backup/data");
    let backup = crate::jobs::Job::test_backup("bak", "tank/media", "hourly", "/mnt/usb/restic", "/root/pw");
    let (mut app, _dir) = jobs_app(&[repl, backup]);
    app.refresh_jobs();
    let wide = render(&mut app, 170, 30).join("\n");
    for want in ["tank/data → local:backup/data", "tank/media → /mnt/usb/restic"] {
        assert!(wide.contains(want), "missing {want:?} in\n{wide}");
    }
    let narrow = render(&mut app, 100, 30).join("\n");
    assert!(narrow.contains("tank/data") && narrow.contains("Schedule") && narrow.contains("Last"), "other columns survive:\n{narrow}");
}

fn backup_form_fields(host: &str, repo: &str, pw: &str) -> Vec<crate::ui::modal::FormField> {
    let values = [host, "tank/data", "", "auto", "", repo, pw, "", "30 3 * * *", "y"];
    values.iter().map(|v| crate::ui::modal::FormField { label: "x".into(), value: (*v).to_string() }).collect()
}

fn backup_kind_for_status() -> crate::ui::modal::FormKind {
    crate::ui::modal::FormKind::BackupJob { hosts: vec!["local".into(), "fbsd16".into()], existing: None, jobs: vec![] }
}

#[test]
fn the_check_line_shows_checking_ok_and_a_problem_for_the_values_on_screen() {
    let fields = backup_form_fields("fbsd16", "/r", "/pw");
    let kind = backup_kind_for_status();
    assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", None))), Some(("Check: checking the repository...".to_string(), false)));
    assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok)))), Some(("Check: OK, the repository opens.".to_string(), false)));
    let problem = crate::restic::Preflight::Problem("no readable password file at /pw on fbsd16".to_string());
    assert_eq!(preflight_status(&kind, &fields, Some(&check(1, "/r", "/pw", Some(problem)))), Some(("Check: no readable password file at /pw on fbsd16".to_string(), true)));
}

#[test]
fn the_check_line_disappears_when_a_field_it_depends_on_changes() {
    let kind = backup_kind_for_status();
    let ok = check(1, "/r", "/pw", Some(crate::restic::Preflight::Ok));
    assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r2", "/pw"), Some(&ok)).is_none(), "repository edited");
    assert!(preflight_status(&kind, &backup_form_fields("fbsd16", "/r", "/pw2"), Some(&ok)).is_none(), "password file edited");
    assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none(), "host edited");
    assert!(preflight_status(&kind, &backup_form_fields("fbsd16", " /r ", "/pw"), Some(&ok)).is_some(), "surrounding spaces are trimmed like the check does");
}

#[test]
fn the_notes_wait_for_the_answer_name_the_location_and_the_permissions_and_follow_the_values_on_screen() {
    let kind = backup_kind_for_status();
    let fields = backup_form_fields("fbsd16", "/r", "/home/fb/pw");
    let mut c = check(1, "/r", "/home/fb/pw", None);
    assert!(preflight_notes(&kind, &fields, Some(&c)).is_empty(), "nothing before the check has answered");
    c.result = Some(crate::restic::Preflight::Ok);
    let notes = preflight_notes(&kind, &fields, Some(&c));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("Note: ") && notes[0].contains("home or temporary directory"), "{notes:?}");
    c.file_note = Some("the password file can be read or changed by other users (mode 644): run `chmod 600 /home/fb/pw`".to_string());
    let notes = preflight_notes(&kind, &fields, Some(&c));
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes[1].contains("mode 644"), "{notes:?}");
    // Editing the password file or the host hides them, like the check line.
    assert!(preflight_notes(&kind, &backup_form_fields("fbsd16", "/r", "/root/pw"), Some(&c)).is_empty());
    assert!(preflight_notes(&kind, &backup_form_fields("local", "/r", "/home/fb/pw"), Some(&c)).is_empty());
    // A good location with nothing else to say has no notes.
    let fine = backup_form_fields("fbsd16", "/r", "/root/pw");
    let mut ok = check(1, "/r", "/root/pw", Some(crate::restic::Preflight::Ok));
    assert!(preflight_notes(&kind, &fine, Some(&ok)).is_empty());
    ok.file_note = Some(crate::restic::PASSWORD_FILE_UNCHECKED.to_string());
    assert!(preflight_notes(&kind, &fine, Some(&ok))[0].contains("could not be checked"));
}

#[test]
fn only_the_backup_form_shows_a_check_line() {
    let ok = check(0, "/r", "/pw", Some(crate::restic::Preflight::Ok));
    let kind = crate::ui::modal::FormKind::CreateDataset { host: 0 };
    assert!(preflight_status(&kind, &backup_form_fields("local", "/r", "/pw"), Some(&ok)).is_none());
    assert!(preflight_status(&backup_kind_for_status(), &backup_form_fields("local", "/r", "/pw"), None).is_none());
}
