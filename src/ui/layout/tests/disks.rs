//! The Disks screen and the forms that build from disks.
#![allow(clippy::wildcard_imports)]
use super::*;

fn app_in_disks() -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
    app.disks = vec![identity_disk("ada1", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567")];
    app.disks_table.select(Some(0));
    app
}

#[test]
fn disks_table_shows_type_model_serial_and_human_size_when_wide() {
    let mut app = app_in_disks();
    let text = render(&mut app, 160, 24).join("\n");
    for want in ["Type", "Model", "Serial", "HDD 5400", "WDC WD40EFRX-68N32N0", "WD-WCC7K1234567", "3.6T", "available"] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("4000787030016"), "raw bytes must be gone");
}

#[test]
fn disks_table_drops_serial_and_model_but_keeps_name_size_status_when_narrow() {
    let mut app = app_in_disks();
    let text = render(&mut app, 80, 24).join("\n");
    for want in ["Name", "ada1", "Size", "3.6T", "Status", "available"] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(!text.contains("Serial") && !text.contains("WD-WCC7K1234567"), "serial must drop first:\n{text}");
    assert!(!text.contains("Model") && !text.contains("WDC WD40"), "model must drop next:\n{text}");
}

#[test]
fn disks_table_columns_drop_in_order_serial_model_type_at_every_width() {
    let mut app = app_in_disks();
    app.disks[0].model = Some("Samsung SSD 970 EVO Plus 1TB".to_string());
    // (terminal width, type, model, serial); the sidebar takes ~22 columns
    for (w, ty, model, serial) in [(60u16, false, false, false), (80, false, false, false), (100, true, false, false), (120, true, true, false), (160, true, true, true)] {
        let text = render(&mut app, w, 24).join("\n");
        // At 60 columns the sidebar leaves the table so little room
        // that Name is squeezed; the headers must still all be there.
        let wants: &[&str] = if w < 80 { &["Name", "Size", "3.6T", "Status"] } else { &["Name", "ada1", "Size", "3.6T", "Status", "available"] };
        for want in wants {
            assert!(text.contains(want), "{w}: missing {want:?} in:\n{text}");
        }
        assert_eq!(text.contains("Type"), ty, "{w} type:\n{text}");
        assert_eq!(text.contains("Samsung SSD 970 EVO Plus 1TB"), model, "{w} full model:\n{text}");
        assert_eq!(text.contains("WD-WCC7K1234567"), serial, "{w} serial:\n{text}");
    }
}

#[test]
fn create_pool_form_with_eight_disks_shows_overflow_line_and_last_field() {
    let text = app_with_form_and_disks(8, 80, 24).join("\n");
    assert!(text.contains("+2 more selected") && text.contains("Redundancy"), "{text}");
}

#[test]
fn disks_table_strips_escapes_newlines_and_bidi_from_model_and_serial() {
    let mut app = app_in_disks();
    app.disks[0].model = Some("Ev\u{1b}[31mil\nModel\u{202e}X".to_string());
    app.disks[0].serial = Some("SN\u{7}\u{9b}1".to_string());
    let rows = render(&mut app, 160, 24);
    let text = rows.join("\n");
    for bad in ['\u{1b}', '\u{202e}', '\u{7}', '\u{9b}'] {
        assert!(!text.contains(bad), "{bad:?} leaked:\n{text}");
    }
    assert!(text.contains("Model") && text.contains("SN"), "{text}");
}

#[test]
fn the_disks_section_shows_a_disks_activity_and_pool_health() {
    use crate::zfs::disk::{DiskInfo, DiskMedia, DiskMembership};
    use crate::zfs::disk_detail::DiskLive;
    use crate::zfs::metrics::{ErrorCounts, MetricsTarget, PoolIo, PoolSample, TargetSample};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
    app.disks = vec![DiskInfo {
        name: "ada1".to_string(),
        size: 5 << 30,
        membership: DiskMembership::InPool { pool: "tpool".to_string(), vdev: "raidz2-0".to_string() },
        model: Some("QEMU HARDDISK".to_string()),
        serial: Some("SN123".to_string()),
        media: DiskMedia::Hdd(7200),
    }];
    app.disks_table.select(Some(0));
    let target = MetricsTarget::Disk { name: "ada1".to_string(), pool: Some("tpool".to_string()) };
    assert_eq!(app.metrics_target(), Some(target.clone()));
    app.metrics.retarget(Some(target.clone()));
    let io = PoolIo { read_bps: 3584, write_ops: 4, write_latency_ns: Some(3_000_000), ..PoolIo::default() };
    let live = DiskLive { state: Some("DEGRADED".to_string()), errors: Some(ErrorCounts { read: 1, write: 0, cksum: 2 }), busy_percent: Some(12.0), queue: Some(2) };
    app.metrics.apply(
        &target,
        Ok(TargetSample { pool: PoolSample { io, ..PoolSample::default() }, space: None, host: None, snapshot: None, disk: Some(live), share: None, contents: None }),
        std::time::Instant::now(),
    );
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["Metrics — disk ada1", "QEMU HARDDISK", "SN123", "HDD 7200 rpm", "tpool / raidz2-0", "DEGRADED   R1 W0 C2", "12%", "queue 2", "lat 3.0ms"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}
