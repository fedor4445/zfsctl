//! The Pools screen and the vdev tree.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn pools_get_a_metrics_panel_across_the_bottom_half() {
    use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.metrics.retarget(Some(MetricsTarget::Pool("tank".to_string())));
    let io = PoolIo { read_bps: 88_080_384, read_ops: 840, ..PoolIo::default() };
    app.metrics.apply(
        &MetricsTarget::Pool("tank".to_string()),
        Ok(TargetSample { pool: PoolSample { io, facts: PoolFacts { size: 1000, allocated: 100, ..PoolFacts::default() } }, space: None, host: None, snapshot: None, disk: None, share: None, contents: None }),
        std::time::Instant::now(),
    );
    let rows = render(&mut app, 100, 30);
    let panel = rows.iter().position(|r| r.contains("Metrics — tank")).expect("metrics panel title");
    assert!((13..=16).contains(&panel), "panel starts around the middle of the body, got row {panel}");
    let text = rows.join("\n");
    assert!(text.contains("Read  84.0M/s"), "{text}");
    assert!(text.contains("840 ops"), "{text}");
    assert!(text.contains("ONLINE"), "{text}");
    assert!(rows[..panel].iter().any(|r| r.contains("Pools —")), "the lists stay above the panel");
    assert!(rows[panel].starts_with('│'), "the sidebar keeps its full height beside the panel: {:?}", rows[panel]);
}

#[test]
fn the_pool_panel_shows_what_the_pool_is_made_of_and_holds() {
    use crate::zfs::pool_info::PoolContents;
    let contents = PoolContents { filesystems: 12, volumes: 3, snapshots: 482, compression: Some("lz4".into()), compress_ratio: Some(1.43), autotrim: Some(false) };
    let mut app = pools_app_with_info(Some(Ok(contents.clone())));
    let text = render(&mut app, 170, 40).join("\n");
    for want in ["Vdevs", "4  (2 mirror, 1 raidz1, 1 disk)", "Disks", "10  (6 data, 2 log, 1 cache, 1 spare)", "Datasets", "12 filesystems, 3 volumes", "Snapshots", "482", "Scrub", "Oct  8 19:48:07 2026", "lz4 1.43x", "Trim off"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    // On a narrow terminal the text is one column, cut but never missing the four counts the panel exists for.
    let mut narrow = pools_app_with_info(Some(Ok(contents)));
    let text = render(&mut narrow, 100, 30).join("\n");
    for want in ["Health", "Vdevs", "Disks", "Datasets", "Snapshots  482"] {
        assert!(text.contains(want), "narrow: missing {want:?} in\n{text}");
    }
    // Too short for every line: the counts outrank the lesser pool lines, and the cut is announced.
    let mut short = pools_app_with_info(Some(Ok(crate::zfs::pool_info::PoolContents { snapshots: 482, ..Default::default() })));
    let text = render(&mut short, 100, 24).join("\n");
    assert!(text.contains("Snapshots  482") && text.contains("more lines"), "{text}");
    assert!(!text.contains("Scrub   "), "{text}");
}

#[test]
fn one_filesystem_and_one_volume_are_not_pluralised() {
    use crate::zfs::pool_info::PoolContents;
    let mut app = pools_app_with_info(Some(Ok(PoolContents { filesystems: 1, volumes: 1, snapshots: 0, ..Default::default() })));
    let text = render(&mut app, 170, 40).join("\n");
    assert!(text.contains("1 filesystem, 1 volume") && !text.contains("1 filesystems"), "{text}");
    assert!(text.contains("Snapshots  0"), "an empty count is 0, not a dash:\n{text}");
}

#[test]
fn the_counts_show_an_ellipsis_while_pending_and_a_dash_when_the_listing_failed() {
    let mut pending = pools_app_with_info(None);
    let text = render(&mut pending, 170, 40).join("\n");
    assert!(text.contains("Datasets   …") && text.contains("Snapshots  …"), "{text}");
    assert!(text.contains("Vdevs") && text.contains("10  (6 data"), "the layout does not wait for the counts:\n{text}");
    let mut failed = pools_app_with_info(Some(Err("boom".into())));
    let text = render(&mut failed, 170, 40).join("\n");
    assert!(text.contains("Datasets   -") && text.contains("Snapshots  -"), "{text}");
}

#[test]
fn pools_table_shows_health_glyphs_and_capacity_bars() {
    let mut app = app_in_pools(vec![
        pool("tank", 31, 100, PoolHealth::Online),
        pool("scratch", 90, 100, PoolHealth::Degraded),
    ]);
    let rows = render(&mut app, 200, 30).join("\n");
    assert!(rows.contains("● ONLINE"), "{rows}");
    assert!(rows.contains("▲ DEGRADED"), "{rows}");
    assert!(rows.contains("31%"), "{rows}");
    assert!(rows.contains("90%!"), "{rows}");
}

#[test]
fn pools_table_survives_zero_size_and_overfull_pools() {
    let mut app = app_in_pools(vec![
        pool("empty", 0, 0, PoolHealth::Offline),
        pool("over", 150, 100, PoolHealth::Faulted),
    ]);
    let rows = render(&mut app, 200, 30).join("\n");
    assert!(rows.contains("○ OFFLINE"), "{rows}");
    assert!(rows.contains("✖ FAULTED"), "{rows}");
    assert!(rows.contains("100%!"), "{rows}");
}

#[test]
fn unrecognized_pool_health_is_rendered_as_its_raw_text() {
    let mut app = app_in_pools(vec![pool("odd", 1, 100, PoolHealth::Other("SUSPENDED".to_string()))]);
    let rows = render(&mut app, 200, 30).join("\n");
    assert!(rows.contains("SUSPENDED"), "{rows}");
    assert!(!rows.contains("● SUSPENDED"), "{rows}");
}

#[test]
fn health_is_visible_at_80_columns() {
    let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 80, 24).join("\n");
    assert!(rows.contains("ONLINE"), "Health must survive a narrow terminal:\n{rows}");
}

#[test]
fn vdev_tree_shows_health_glyphs() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.drilled_pool = Some("tank".to_string());
    app.vdevs = vec![
        VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Degraded },
        VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Faulted },
    ];
    let rows = render(&mut app, 100, 20).join("\n");
    assert!(rows.contains("▲ DEGRADED"), "{rows}");
    assert!(rows.contains("✖ FAULTED"), "{rows}");
}

#[test]
fn capacity_is_visible_at_80_columns() {
    let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 80, 24).join("\n");
    assert!(rows.contains("ONLINE"), "{rows}");
    assert!(rows.contains("31%"), "capacity must survive 80 columns:\n{rows}");
}

#[test]
fn pool_names_keep_a_readable_width_as_columns_are_added() {
    // 16-character name: the Name column must not be squeezed below
    // this just because a low-priority column (Size/Alloc/Free) fits.
    for width in [90u16, 110, 120, 140, 160, 200] {
        let mut app = app_in_pools(vec![pool("sixteen-char-nm!", 31, 100, PoolHealth::Online)]);
        let rows = render(&mut app, width, 24).join("\n");
        assert!(rows.contains("sixteen-char-nm!"), "name clipped at {width} columns:\n{rows}");
        assert!(rows.contains("ONLINE"), "health missing at {width} columns:\n{rows}");
    }
}

#[test]
fn the_full_capacity_bar_is_used_when_there_is_room() {
    let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 200, 24).join("\n");
    assert!(rows.contains("███░░░░░ 31%") || rows.contains("██░░░░░░ 31%"), "{rows}");
}

#[test]
fn the_type_words_have_their_own_colors_apart_from_the_health_colors() {
    let mut app = hosts_app();
    let vm = fg_of(&mut app, "vm", 170, 30);
    let jail = fg_of(&mut app, "jail", 170, 30);
    let ok = fg_of(&mut app, "ok", 170, 30);
    assert!(vm.is_some() && vm != ok && jail != ok && vm != jail, "{vm:?} {jail:?} {ok:?}");
}

#[test]
fn health_colors_the_marker_and_name_and_the_unreachable_host_is_red() {
    let mut app = hosts_app();
    app.selected_host = 0;
    let theme = app.theme.clone();
    assert_eq!(fg_of(&mut app, "fbsd16 ", 120, 30), theme.err.fg, "a degraded pool is red");
    assert_eq!(fg_of(&mut app, "fbsd16t1", 120, 30), theme.warn.fg, "85% full is yellow");
    assert_eq!(fg_of(&mut app, "deadbox", 120, 30), theme.err.fg, "unreachable is red");
}

#[test]
fn the_host_panel_shows_two_columns_when_wide_and_one_when_narrow_and_says_when_it_cuts_pool_rows() {
    let mut wide = host_panel_app(());
    let text = render(&mut wide, 190, 40).join("\n");
    for want in ["Metrics — host local", "CPU", "36 cores", "Memory", "Swap", "L2ARC", "scratch", "tank", "zroot"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    let mut narrow = host_panel_app(());
    let text = render(&mut narrow, 90, 40).join("\n");
    assert!(text.contains("CPU") && text.contains("Swap"), "one column still shows everything:\n{text}");
    let mut short = host_panel_app(());
    let text = render(&mut short, 190, 24).join("\n");
    assert!(text.contains("more pool row") || text.contains("scratch"), "never silently clipped:\n{text}");
}

#[test]
fn a_short_host_panel_keeps_the_pools_and_the_unhealthy_rows_and_says_how_much_it_hides() {
    for (w, h) in [(80, 24), (100, 30)] {
        let mut app = host_panel_app(());
        let text = render(&mut app, w, h).join("\n");
        assert!(text.contains("DEGRADED"), "{w}x{h}: the unhealthy pool row must survive:\n{text}");
        assert!(text.contains("Status"), "{w}x{h}: the status line must survive:\n{text}");
    }
    let mut tiny = host_panel_app(());
    let text = render(&mut tiny, 80, 24).join("\n");
    assert!(text.contains("more lines: enlarge the terminal"), "{text}");
}
