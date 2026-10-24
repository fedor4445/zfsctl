//! The Datasets and Snapshots screens.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn the_hold_column_shows_a_capital_for_ours_and_a_small_h_for_others() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    let snap = |name: &str, holds| crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: name.to_string(), used: 1, holds, written: 0 };
    app.snapshots = vec![snap("free", 0), snap("ours", 1), snap("theirs", 1)];
    app.snapshot_tags.insert("tank/a@ours".to_string(), vec!["zfsctl".to_string(), "x".to_string()]);
    app.snapshot_tags.insert("tank/a@theirs".to_string(), vec!["x".to_string()]);
    app.snapshots_table.select(Some(0));
    let rows = render(&mut app, 100, 12);
    let row = |name: &str| rows.iter().find(|r| r.contains(name)).unwrap_or_else(|| panic!("{rows:?}")).clone();
    assert!(rows.iter().any(|r| r.contains("Hold")), "{rows:?}");
    let hold_of = |name: &str| {
        let r = row(name);
        let after = &r[r.find(name).unwrap() + name.len()..];
        after.split_whitespace().nth(2).filter(|t| *t != "│").map(str::to_string)
    };
    assert_eq!(hold_of("tank/a@free"), None, "{rows:?}");
    assert_eq!(hold_of("tank/a@ours").as_deref(), Some("H"), "{rows:?}");
    assert_eq!(hold_of("tank/a@theirs").as_deref(), Some("h"), "{rows:?}");
}

#[test]
fn the_snapshot_list_has_a_written_column() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1024, holds: 0, written: 2048 }];
    app.snapshots_table.select(Some(0));
    let rows = render(&mut app, 100, 12);
    let header = rows.iter().find(|r| r.contains("Snapshot") && r.contains("Used")).unwrap_or_else(|| panic!("{rows:?}")).clone();
    assert!(header.find("Used").unwrap() < header.find("Written").unwrap(), "Written comes after Used: {header:?}");
    let row = rows.iter().find(|r| r.contains("tank/a@s")).unwrap_or_else(|| panic!("{rows:?}")).clone();
    let after: Vec<&str> = row[row.find("tank/a@s").unwrap()..].split_whitespace().collect();
    assert_eq!(&after[1..3], ["1.0K", "2.0K"], "{row:?}");
}

#[test]
fn the_snapshots_section_shows_the_changes_view_in_place_of_the_list_and_only_for_its_host() {
    use crate::ui::changes::{Against, ChangesView, PendingChanges};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1, holds: 0, written: 0 }];
    let pending = PendingChanges { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s".to_string(), against: Against::Previous, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
    app.changes_view = Some(ChangesView::new(&pending));
    let shown = render(&mut app, 100, 12).join("\n");
    assert!(shown.contains("Changes —") && shown.contains("reading changes"), "{shown}");
    assert!(!shown.contains("Snapshot  "), "the list is not drawn under the view: {shown}");
    app.selected_host = 3;
    let other = render(&mut app, 100, 12).join("\n");
    assert!(!other.contains("Changes —"), "a view of another host is not shown: {other}");
}

#[test]
fn the_snapshots_section_shows_the_browser_in_place_of_the_list_and_only_for_its_host() {
    use crate::ui::browse::{BrowseView, PendingBrowse};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/a".to_string(), name: "s".to_string(), used: 1, holds: 0, written: 0 }];
    let pending = PendingBrowse { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s".to_string(), path: vec![], mountpoint: None, zone: None, os: crate::zfs::host_health::HostOs::FreeBsd, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
    app.browse_view = Some(BrowseView::new(&pending, Some(0)));
    for (w, h) in [(100, 12), (70, 8)] {
        let shown = render(&mut app, w, h).join("\n");
        assert!(shown.contains("Browse —") && shown.contains("reading the folder"), "{w}x{h}: {shown}");
        assert!(!shown.contains("Snapshot  "), "the list is not drawn under the view: {shown}");
    }
    app.selected_host = 3;
    let other = render(&mut app, 100, 12).join("\n");
    assert!(!other.contains("Browse —"), "a view of another host is not shown: {other}");
}

#[test]
fn a_long_dataset_name_widens_the_list_at_the_detail_panes_expense() {
    let long = "tank/backups/hosts/webserver-01/home/alice";
    let mut app = app_in_datasets(&[long]);
    assert!(!render(&mut app, 120, 24).join("\n").contains(long), "premise: a fixed 60/40 split clips this name");
    let mut app = app_in_datasets(&[long]);
    app.dataset_properties = vec![crate::zfs::property::Property { name: "compression".to_string(), value: "lz4".to_string(), source: crate::zfs::property::PropertySource::Default }];
    // 150 cols - 22 sidebar = 128: 60% = 76 can't hold the 87 the name table needs; the detail pane can lend 14.
    let text = render(&mut app, 150, 24).join("\n");
    assert!(text.contains(long), "{text}");
}

#[test]
fn datasets_list_shows_human_sizes_not_raw_bytes_at_110_columns() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
    app.datasets = vec![crate::zfs::dataset::Dataset {
        name: "tank/data".to_string(),
        kind: crate::zfs::dataset::DatasetKind::Filesystem,
        used: 105_250_816,
        available: 32_899_702_784,
        referenced: 0,
        mountpoint: None, quota: 0, refquota: 0,
    }];
    app.datasets_table.select(Some(0));
    let text = render(&mut app, 110, 30).join("\n");
    for want in ["100M", "30.6G", "Filesys"] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("105250816") && !text.contains("32899702784"), "raw bytes must be gone:\n{text}");
}

#[test]
fn snapshots_list_shows_human_sizes_not_raw_bytes() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 2_096_734_848, holds: 0, written: 0 }];
    app.snapshots_table.select(Some(0));
    let text = render(&mut app, 110, 30).join("\n");
    assert!(text.contains("tank/data@b") && text.contains("2.0G"), "{text}");
    assert!(!text.contains("2096734848"), "raw bytes must be gone:\n{text}");
}

#[test]
fn the_snapshots_section_shows_what_destroying_the_selection_would_free() {
    use crate::zfs::metrics::{MetricsTarget, PoolSample, TargetSample};
    use crate::zfs::snapshot_detail::{SnapshotDetail, SnapshotEntry};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots = vec![crate::zfs::snapshot::Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 4096, holds: 0, written: 0 }];
    app.snapshots_table.select(Some(0));
    let target = MetricsTarget::Snapshot("tank/data@b".to_string());
    assert_eq!(app.metrics_target(), Some(target.clone()));
    app.metrics.retarget(Some(target.clone()));
    let entry = |n: &str, created, used| SnapshotEntry { name: format!("tank/data@{n}"), created, used };
    let detail = SnapshotDetail {
        full_name: "tank/data@b".to_string(),
        used: 4096,
        holds: 1,
        reclaim: Some(2048),
        siblings: vec![entry("a", 1_000_000_000, 100), entry("b", 1_500_000_000, 4096)],
        dataset_used: 1000,
        dataset_snapshots_used: 250,
        ..SnapshotDetail::default()
    };
    app.metrics.apply(
        &target,
        Ok(TargetSample { pool: PoolSample::default(), space: None, host: None, snapshot: Some(detail), disk: None, share: None, contents: None }),
        std::time::Instant::now(),
    );
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["Metrics — tank/data@b", "Snapshot  b", "Reclaim   2.0K", "Holds     1", "(blocks destroy)", "2 snaps", "25%"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

fn limited_datasets_app() -> App {
    use crate::zfs::dataset::{Dataset, DatasetKind};
    let g: u64 = 1 << 30;
    let ds = |name: &str, used: u64, referenced: u64, quota: u64, refquota: u64| Dataset { name: name.into(), kind: DatasetKind::Filesystem, used, available: 50 * g, referenced, mountpoint: None, quota, refquota };
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
    app.datasets = vec![ds("tank/home", 96 * g / 10, g, 10 * g, 0), ds("tank/ref", 9 * g, 3 * g, 0, 4 * g), ds("tank/free", g, g, 0, 0)];
    app.datasets_table.select(Some(0));
    app
}

#[test]
fn the_datasets_table_shows_how_full_a_quota_is_and_nothing_when_no_dataset_has_one() {
    let mut app = limited_datasets_app();
    let wide = render(&mut app, 160, 12).join("\n");
    assert!(wide.contains("Quota") && wide.contains("96% of 10G") && wide.contains("75% of 4G ref"), "{wide}");
    for d in &mut app.datasets {
        (d.quota, d.refquota) = (0, 0);
    }
    let none = render(&mut app, 160, 12).join("\n");
    assert!(!none.contains("Quota"), "no column when nothing has a limit:\n{none}");
}

#[test]
fn the_datasets_table_marks_encrypted_datasets_open_or_locked() {
    use crate::zfs::encryption::Enc;
    let mut app = limited_datasets_app();
    let wide = render(&mut app, 160, 12).join("\n");
    assert!(!wide.contains("Lock"), "no column when nothing is encrypted:\n{wide}");
    let enc = |locked| Enc { cipher: "aes-256-gcm".into(), key_format: "passphrase".into(), root: "tank/home".into(), locked };
    app.encryption.insert("tank/home".into(), enc(false));
    app.encryption.insert("tank/ref".into(), enc(true));
    let wide = render(&mut app, 160, 12).join("\n");
    assert!(wide.contains("Lock") && wide.contains("open") && wide.contains("LOCKED"), "{wide}");
    let mut dropped_while_kind_stays = false;
    for width in 50..200 {
        let screen = render(&mut app, width, 12).join("\n");
        if screen.contains("Lock") {
            assert!(screen.contains("Avail") && screen.contains("Kind"), "Lock only shows with the others, width {width}");
        } else if screen.contains("Kind") {
            dropped_while_kind_stays = true;
        }
    }
    assert!(dropped_while_kind_stays);
}

#[test]
fn the_quota_column_is_the_first_to_go_on_a_narrow_pane() {
    let mut app = limited_datasets_app();
    let mut dropped_while_kind_stays = false;
    for width in 60..200 {
        let screen = render(&mut app, width, 12).join("\n");
        if screen.contains("Quota") {
            assert!(screen.contains("Avail") && screen.contains("Kind"), "Quota only shows with the others, width {width}");
        } else if screen.contains("Kind") {
            dropped_while_kind_stays = true;
        }
    }
    assert!(dropped_while_kind_stays, "somewhere the Quota column is gone while Kind still fits");
}


#[test]
fn the_acl_pane_title_says_when_the_dataset_is_posix() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
    app.datasets = vec![crate::zfs::dataset::Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.pane_focus = crate::ui::state::PaneFocus::Right;
    app.dataset_detail_tab = crate::ui::state::DatasetDetailTab::Acl;
    app.dataset_properties_for = Some("tank/data".to_string());
    app.dataset_properties = vec![crate::zfs::property::Property { name: "acltype".to_string(), value: "posix".to_string(), source: crate::zfs::property::PropertySource::Local }];
    let text = render(&mut app, 160, 30).join("\n");
    assert!(text.contains("acltype=posix") && text.contains("NFSv4"), "{text}");
}
