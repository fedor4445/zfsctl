//! The Cloud screen.
#![allow(clippy::wildcard_imports)]
use super::*;

fn cloud_app() -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Cloud).unwrap();
    app.jobs_view = crate::ui::jobs::JobsView { jobs: vec![crate::jobs::Job::test_mirror("mirror-local-tank-photos", "tank/photos", "hourly", "b2:zfsctl/photos")], loaded: true, ..Default::default() };
    app.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into()), ("zfsctl-aws".into(), "s3".into())]);
    app
}

#[test]
fn the_cloud_section_lists_every_remote_with_its_jobs_and_the_selected_ones_detail() {
    let mut app = cloud_app();
    app.cloud_table.select(Some(0));
    app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: Some((3, 24_576)), at: chrono::Utc::now(), problem: None });
    let text = render(&mut app, 130, 40).join("\n");
    for want in ["Cloud — 3 remotes, 1 job", "Remote", "In bucket", "b2:", "zfsctl-aws:", "t1:", "SFTP", "1.9 MiB", "Remote — b2:", "mirror-local-tank-photos", "current/ 1.9 MiB (412 files)", "old/ 24.0 KiB (3 files)"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    app.cloud_table.select(Some(1));
    let idle = render(&mut app, 130, 40).join("\n");
    assert!(idle.contains("Remote — t1:") && idle.contains("No job uses this remote"), "{idle}");
}

#[test]
fn the_cloud_table_starts_with_the_providers_name() {
    let mut app = cloud_app();
    app.cloud_table.select(Some(0));
    let lines = render(&mut app, 130, 40);
    let header = lines.iter().find(|l| l.contains("Provider")).expect("a Provider header");
    assert!(header.find("Provider").unwrap() < header.find("Remote").unwrap(), "{header}");
    let text = lines.join("\n");
    for want in ["Backblaze B2", "SFTP", "Amazon S3 (etc.)"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_cloud_section_shows_how_much_of_the_plan_cap_is_used_and_warns_near_the_limit() {
    let mut app = cloud_app();
    app.cloud_table.select(Some(0));
    app.cloud_caps.insert("b2".into(), Ok(10_000_000_000));
    app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: Some((3, 24_576)), at: chrono::Utc::now(), problem: None });
    let text = render(&mut app, 130, 40).join("\n");
    assert!(text.contains("1.9 MiB of 10 GB (0.02%)") && text.contains("Plan cap 10 GB: using 1.9 MiB (0.02%)"), "{text}");
    app.cloud_caps.insert("b2".into(), Err("ten gigs".into()));
    assert!(render(&mut app, 130, 40).join("\n").contains("Plan cap 'ten gigs'"));
    app.cloud_caps.insert("b2".into(), Ok(2_500_000));
    let warn = render(&mut app, 130, 40).join("\n");
    assert!(warn.contains("of 2.5 MB (81%)") && warn.contains("Plan cap 2.5 MB: using 1.9 MiB (81%)"), "{warn}");
}

#[test]
fn the_plan_percentage_fits_a_110_column_terminal() {
    let mut app = cloud_app();
    app.cloud_table.select(Some(0));
    app.cloud_caps.insert("b2".into(), Ok(10_000_000_000));
    app.cloud_sizes.insert("mirror-local-tank-photos".into(), crate::ui::cloud::Size { current: Some((412, 1_992_294)), old: None, at: chrono::Utc::now(), problem: None });
    let text = render(&mut app, 110, 30).join("\n");
    assert!(text.contains("1.9 MiB of 10 GB (0.02%)"), "{text}");
}

#[test]
fn the_cloud_section_says_why_a_host_could_not_be_asked_and_what_to_do_when_there_are_no_remotes() {
    let mut app = cloud_app();
    app.cloud_errors.insert("local".into(), "rclone is not installed on local".into());
    let text = render(&mut app, 130, 40).join("\n");
    assert!(text.contains("local: rclone is not installed on local"), "{text}");
    let mut empty = App::new(vec!["local".to_string()]);
    empty.selected_section = empty.sections.iter().position(|s| *s == Section::Cloud).unwrap();
    assert!(render(&mut empty, 130, 40).join("\n").contains("Looking for rclone remotes"));
    empty.cloud_known.insert("local".into(), vec![]);
    assert!(render(&mut empty, 130, 40).join("\n").contains("No rclone remotes found"));
}

#[test]
fn the_cloud_section_shows_the_f5_check_for_the_selected_remote_and_survives_a_small_screen() {
    let mut app = cloud_app();
    app.cloud_table.select(Some(0));
    app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "b2:zfsctl/photos".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
    assert!(render(&mut app, 130, 40).join("\n").contains("Check: OK, the remote opens."));
    app.backup_preflight = Some(crate::ui::state::PreflightCheck { host: 0, repository: "other:x".into(), password_file: String::new(), result: Some(crate::restic::Preflight::Ok), file_note: None });
    assert!(!render(&mut app, 130, 40).join("\n").contains("Check:"), "a check of another remote is not shown here");
    let tiny = render(&mut app, 60, 12).join("\n");
    assert!(tiny.contains("Cloud"), "{tiny}");
}
