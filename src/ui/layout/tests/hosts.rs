//! The Hosts screen and the host panel.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn the_hosts_section_shows_a_whole_host_summary() {
    use crate::zfs::host::{ArcStats, HostSummary};
    use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.pools = vec![pool("tank", 10, 100, PoolHealth::Online), pool("old", 50, 100, PoolHealth::Degraded)];
    app.metrics.retarget(Some(MetricsTarget::Host));
    let host = HostSummary {
        os: Some("FreeBSD 15.1-RELEASE".to_string()),
        uptime_secs: Some(90_000),
        load: Some([0.5, 0.25, 0.1]),
        arc: Some(ArcStats { size: 1 << 30, max: 4 << 30, hits: 99, misses: 1 }),
        pools: app.pools.clone(),
        status: Some("all pools are healthy".to_string()),
        ..HostSummary::default()
    };
    app.metrics.apply(
        &MetricsTarget::Host,
        Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts: PoolFacts::default() }, space: None, host: Some(host), snapshot: None, disk: None, share: None, contents: None }),
        std::time::Instant::now(),
    );
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("Metrics — host local (all pools)"), "{text}");
    for want in ["FreeBSD 15.1-RELEASE", "1d 1h", "1 not ONLINE", "hit 99.0%", "0.50 0.25 0.10", "all pools are healthy"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_sidebar_dims_unsupported_entries_for_the_selected_host() {
    use ratatui::style::Modifier;
    let mut app = linux_host_app();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let style_of = |label: &str| {
        let (x, y) = (0..30u16).find_map(|y| (0..40u16).find_map(|x| ((0..label.len() as u16).all(|i| buf[(x + i, y)].symbol() == &label[i as usize..=i as usize])).then_some((x, y)))).unwrap_or_else(|| panic!("no {label} on screen"));
        (buf[(x, y)].fg, buf[(x, y)].modifier)
    };
    let dim = app.theme.dim();
    let dimmed = |label: &str| style_of(label).0 == dim.fg.unwrap_or(ratatui::style::Color::Reset) && style_of(label).1.contains(dim.add_modifier | Modifier::empty());
    assert!(dimmed("Boot Envs"), "unsupported entries are dimmed");
    assert!(dimmed("Disks") && dimmed("Shares") && !dimmed("Pools") && !dimmed("Jobs"), "the ones that need FreeBSD tools are dimmed too, the rest is not");
    app.selected_host = 0;
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let buf2 = terminal.backend().buffer().clone();
    let boot = (0..30u16).find(|y| (0..40u16).any(|x| buf2[(x, *y)].symbol() == "B" && buf2[(x + 1, *y)].symbol() == "o" && buf2[(x + 5, *y)].symbol() == "E")).unwrap();
    let x = (0..40u16).find(|x| buf2[(*x, boot)].symbol() == "B").unwrap();
    assert_ne!(buf2[(x, boot)].fg, dim.fg.unwrap_or(ratatui::style::Color::Reset), "back on the FreeBSD host it is restored");
}

#[test]
fn the_host_columns_drop_in_the_documented_order_and_the_required_three_stay() {
    // Array order is the drop order: the OS name first, Type last.
    let order = |c: &HostColumns| [c.os, c.rtt, c.jobs, c.scrub, c.pools, c.tag, c.used, c.type_];
    let wide = fit_host_columns(200, 10, 8);
    assert!(order(&wide).iter().all(|b| *b) && wide.addr, "everything fits at 200");
    for w in 30..200 {
        let on = order(&fit_host_columns(w, 10, 8));
        // a column that survives implies every column that drops after it survives too
        assert!(on.windows(2).all(|p| !(p[0] && !p[1])), "width {w}: {on:?}");
        // widening never removes a column (except when Address appears, which takes priority over them)
        let (now, next) = (fit_host_columns(w, 10, 8), fit_host_columns(w + 1, 10, 8));
        if now.addr == next.addr {
            assert!(on.iter().zip(order(&next)).all(|(a, b)| !*a || b), "width {w}");
        }
    }
    assert!(!order(&fit_host_columns(30, 10, 8)).iter().any(|b| *b), "at 30 columns only the required ones remain");
    assert!(!order(&fit_host_columns(200, 10, 0))[5], "no tags configured: no Tag column");
}

#[test]
fn the_hosts_table_shows_the_short_os_name_when_it_fits() {
    let mut app = linux_host_app();
    app.host_probes.get_mut("ub24").unwrap().os_name = Some("Linux 6.8 (Ubuntu 24)".into());
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    let wide = render(&mut app, 200, 14).join("\n");
    assert!(wide.contains("OS") && wide.contains("Linux 6.8 (Ubuntu 24)"), "{wide}");
    let narrow = render(&mut app, 80, 14).join("\n");
    assert!(!narrow.contains("Ubuntu 24"), "the OS name is the first column to go:\n{narrow}");
}

#[test]
fn a_one_or_two_character_tag_is_fitted_as_wide_as_it_is_drawn() {
    // The drawn Tag column is at least 3 wide (its header), so the fitting must count it that way.
    for inner in 0..200 {
        assert_eq!(fit_host_columns(inner, 8, 1), fit_host_columns(inner, 8, 3), "inner {inner}");
        assert_eq!(fit_host_columns(inner, 8, 2), fit_host_columns(inner, 8, 3), "inner {inner}");
    }
}

#[test]
fn the_hosts_table_shows_the_new_columns_and_cells() {
    let mut app = hosts_app();
    app.selected_host = 0;
    let text = render(&mut app, 170, 30).join("\n");
    for want in ["Type", "Tag", "Used", "Pools", "Scrub", "Jobs", "Rtt", "vm", "metal", "jail", "backup", "85%", "20%"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn a_narrow_hosts_table_still_shows_name_address_and_state() {
    let mut app = hosts_app();
    let text = render(&mut app, 100, 30).join("\n");
    for want in ["Name", "Address", "State", "172.16.10.60"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_hosts_title_shows_the_filter_and_only_matching_rows_are_drawn() {
    let mut app = hosts_app();
    app.host_filter = "dead".into();
    app.selected_host = 3;
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("deadbox") && !text.contains("fbsd16t1"), "{text}");
    assert!(text.contains("filter: dead") && text.contains("1/4"), "{text}");
    app.host_filter_editing = true;
    assert!(render(&mut app, 120, 30).join("\n").contains("filter: dead_"), "a cursor while typing");
    app.host_filter = "zzz".into();
    assert!(render(&mut app, 120, 30).join("\n").contains("no host matches"));
}

#[test]
fn the_hosts_table_scrolls_to_keep_the_selected_host_visible() {
    let names: Vec<String> = (0..40).map(|i| format!("host{i:02}")).collect();
    let mut app = App::new(names);
    app.selected_host = 39;
    let text = render(&mut app, 120, 24).join("\n");
    assert!(text.contains("host39"), "the selected host must be on screen:\n{text}");
    assert!(!text.contains("host01"), "the top rows scrolled away:\n{text}");
    app.selected_host = 0;
    assert!(render(&mut app, 120, 24).join("\n").contains("host00"));
}

#[test]
fn the_hosts_table_shows_name_address_state_and_rtt() {
    let mut app = hosts_app();
    app.selected_host = 0;
    let text = render(&mut app, 150, 30).join("\n");
    for want in ["Name", "Address", "State", "Rtt", "fbsd16", "172.16.10.60", "tank DEGRADED", "tank 85% full", "unreachable", "12ms", "ok"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert!(text.contains("local"), "the hub's address column says local");
}

#[test]
fn a_host_that_has_not_been_probed_is_dim_and_says_probing() {
    let mut app = App::new(vec!["local".to_string(), "new".to_string()]);
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("probing"), "{text}");
}

#[test]
fn narrow_terminals_drop_rtt_then_address_but_keep_name_and_state() {
    let mut app = hosts_app();
    let wide = render(&mut app, 140, 30).join("\n");
    assert!(wide.contains("Rtt") && wide.contains("Address"));
    let mid = render(&mut app, 90, 30).join("\n");
    assert!(!mid.contains("Rtt") && mid.contains("Address") && mid.contains("State"), "{mid}");
    let narrow = render(&mut app, 50, 30).join("\n");
    assert!(!narrow.contains("Address") && narrow.contains("Name") && narrow.contains("State") && narrow.contains("unreachable"), "{narrow}");
}

#[test]
fn the_single_host_hint_and_the_selection_are_kept() {
    let mut app = App::new(vec!["local".to_string()]);
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("Only this host is configured"), "{text}");
    let mut many = hosts_app();
    many.selected_host = 1;
    let buf = buffer_of(&mut many, 120, 30);
    let rows = render(&mut many, 120, 30);
    let y = rows.iter().position(|r| r.contains("172.16.10.60")).unwrap();
    let x = rows[y].find("172.16.10.60").unwrap();
    let selected = buf[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].modifier;
    let other = {
        let y2 = rows.iter().position(|r| r.contains("172.16.10.61")).unwrap();
        buf[(u16::try_from(x).unwrap(), u16::try_from(y2).unwrap())].modifier
    };
    assert_ne!(selected, other, "the selected host's row looks selected");
}

#[test]
fn a_long_cpu_model_is_cut_before_it_reaches_the_zfs_column() {
    let mut app = host_panel_app(());
    let target = MetricsTarget::Host;
    let mut host = crate::ui::metrics::tests_support::rich_host();
    host.cpu.as_mut().unwrap().model = "Intel Xeon E312xx (Sandy Bridge, IBRS update) with a long tail".to_string();
    app.metrics.apply(&target, Ok(crate::zfs::metrics::TargetSample { pool: crate::zfs::metrics::PoolSample::default(), space: None, host: Some(host), snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
    let rows = render(&mut app, 170, 48);
    let model_row = rows.iter().find(|r| r.contains("Intel Xeon E312xx")).expect("the model line is shown");
    assert!(model_row.contains('…'), "{model_row}");
    let status_row = rows.iter().find(|r| r.contains("Status")).unwrap();
    let at = status_row.find("Status").unwrap();
    assert!(status_row[..at].ends_with("  "), "the ZFS column keeps a gutter: {status_row}");
}

#[test]
fn the_site_column_follows_tag_only_when_some_host_has_a_site_and_goes_before_tag_when_narrow() {
    assert!(!fit_host_columns_with_site(200, 10, 8, 0).site, "no site configured: no Site column");
    assert!(fit_host_columns_with_site(200, 10, 8, 6).site);
    for w in 30..200 {
        let c = fit_host_columns_with_site(w, 10, 8, 6);
        assert!(!c.site || c.tag, "width {w}: a Site column never outlives the Tag column");
    }
    assert!((30..200).any(|w| {
        let c = fit_host_columns_with_site(w, 10, 8, 6);
        c.tag && !c.site
    }), "there is a width with Tag but no Site");
    assert_eq!(fit_host_columns(120, 10, 8), fit_host_columns_with_site(120, 10, 8, 0));
}

#[test]
fn the_hosts_table_shows_each_hosts_site_and_the_filter_finds_it() {
    let mut app = hosts_app();
    app.host_sites = vec![String::new(), "rack1".to_string(), "rack1".to_string()];
    let text = render(&mut app, 190, 30).join("\n");
    assert!(text.contains("Site") && text.contains("rack1"), "{text}");
    assert!(app.host_matches(1, "RACK") && !app.host_matches(0, "rack"));
    app.host_sites.clear();
    assert!(!render(&mut app, 190, 30).join("\n").contains("Site"), "no site on any host: no column");
}

fn host_screen_with_net(w: u16, h: u16) -> String {
    use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
    use crate::zfs::net::NetIface;
    let mut app = App::new(vec!["local".to_string()]);
    app.metrics.retarget(Some(MetricsTarget::Host));
    let net = vec![
        NetIface { name: "vtnet0".into(), up: true, speed: Some("10Gbase-T".into()), in_bps: 1_258_291, out_bps: 1_153_434 },
        NetIface { name: "vtnet1".into(), up: false, speed: Some("10Gbase-T".into()), in_bps: 0, out_bps: 0 },
        NetIface { name: "tailscale0".into(), up: true, speed: None, in_bps: 2048, out_bps: 0 },
    ];
    for _ in 0..3 {
        let host = crate::ui::metrics::tests_support::rich_host();
        let sample = TargetSample { pool: PoolSample { io: PoolIo::default(), facts: PoolFacts::default() }, space: None, host: Some(crate::zfs::host::HostSummary { net: net.clone(), ..host }), snapshot: None, disk: None, share: None, contents: None };
        app.metrics.apply(&MetricsTarget::Host, Ok(sample), std::time::Instant::now());
    }
    render(&mut app, w, h).join("\n")
}

#[test]
fn the_host_panel_shows_the_network_block_below_the_io_graphs_when_there_is_room() {
    let text = host_screen_with_net(130, 46);
    let at = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle:?} in\n{text}"));
    assert!(at("Network") > at("Read "), "the network block comes after the I/O graphs");
    for want in ["vtnet0", "10Gbase-T", "1.2M/s", "○ down", "tailscale0", "ZFS     2.4.2-1"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert!(at("vtnet0") > at("Network") && at("tailscale0") > at("vtnet1"), "one line per interface, in order");
}

#[test]
fn a_short_terminal_keeps_the_host_panel_whole_and_drops_the_network_block() {
    let text = host_screen_with_net(130, 24);
    assert!(!text.contains("Network"), "no room for it: the I/O graphs and pool rows keep their rows\n{text}");
    assert!(text.contains("Read "), "{text}");
}
