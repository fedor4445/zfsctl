//! The frame around every screen: split widths, borders, sidebar, hint bar and status line.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn failed_popup_height_fits_body_within_bounds() {
    assert_eq!(super::super::failed_popup_height("Loading accounts…\n\nEsc to close", 40, 24), 6);
    let body = format!("Could not load accounts: {}\n\nEsc to close", "x".repeat(130));
    let h = super::super::failed_popup_height(&body, 40, 24);
    assert!(h >= super::super::wrapped_line_count(&body, 38) + 2 && h > 5, "{h}");
    assert_eq!(super::super::failed_popup_height(&body, 40, 6), 6);
}

#[test]
fn split_widths_keeps_sixty_forty_when_both_sides_fit() {
    assert_eq!(split_widths(100, 50, 30), (60, 40));
}

#[test]
fn split_widths_gives_a_truncating_list_the_detail_panes_spare_width() {
    // right needs 36 (its minimum) so it can donate 40-36=4
    assert_eq!(split_widths(100, 70, 20), (64, 36));
    // asks for less than the spare: only what is needed moves
    assert_eq!(split_widths(100, 62, 20), (62, 38));
}

#[test]
fn split_widths_gives_a_truncating_detail_pane_the_lists_spare_width() {
    assert_eq!(split_widths(100, 35, 55), (45, 55));
    // list never drops below its own need or below 30
    assert_eq!(split_widths(100, 50, 70), (50, 50));
    assert_eq!(split_widths(100, 10, 90), (30, 70));
}

#[test]
fn split_widths_keeps_the_default_when_both_sides_truncate_or_the_terminal_is_tiny() {
    assert_eq!(split_widths(100, 80, 60), (60, 40));
    assert_eq!(split_widths(60, 80, 10), (36, 24));
}

#[test]
fn split_widths_always_sums_to_total() {
    for total in 0..200u16 {
        for lw in (0..200).step_by(7) {
            for rw in (0..200).step_by(11) {
                let (l, r) = split_widths(total, lw, rw);
                assert_eq!(l + r, total);
            }
        }
    }
}

#[test]
fn pane_border_style_highlights_only_the_current_pane() {
    let theme = Theme::ansi();
    assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Middle), Style::default().fg(Color::Cyan));
    assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Right), Style::default());
    assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Right), Style::default().fg(Color::Cyan));
    assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Middle), Style::default());
}

#[test]
fn short_names_keep_the_default_split() {
    let mut app = app_in_datasets(&["tank/a"]);
    let rows = render(&mut app, 130, 24);
    // The detail pane's left border sits in the same column on every row where it starts.
    let col = rows[0].find("Properties").map(|i| rows[0][..i].chars().count());
    assert!(col.is_some_and(|c| c >= 22 + 60), "{:?}", rows[0]);
}

#[test]
fn a_dimmed_section_says_why_in_its_body() {
    let mut app = linux_host_app();
    for (section, text) in [(Section::BootEnvs, "Boot Envs is not supported on Linux hosts yet.")] {
        app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
        let screen = render(&mut app, 120, 20).join("\n");
        assert!(screen.contains(text), "{screen}");
    }
}

#[test]
fn tiny_terminals_have_no_metrics_panel_and_an_empty_list_says_nothing_is_selected() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    assert!(!render(&mut app, 100, 12).join("\n").contains("Metrics"), "too short for a panel");
    app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
    let text = render(&mut app, 100, 30).join("\n");
    assert!(text.contains("nothing selected"), "{text}");
}

#[test]
fn hint_bar_is_the_last_row_and_shows_the_pinned_keys() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 80, 24);
    let last = &rows[23];
    assert!(last.contains("? Help"), "{last:?}");
    assert!(last.contains("q Quit"), "{last:?}");
    assert!(last.contains("s Scrub"), "{last:?}");
}

#[test]
fn the_hint_bar_follows_focus() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    assert!(render(&mut app, 100, 24)[23].contains("s Scrub"));
    app.pane_focus = PaneFocus::Right;
    let last = render(&mut app, 100, 24)[23].clone();
    assert!(!last.contains("s Scrub"), "{last:?}");
    assert!(last.contains("← Back"), "{last:?}");
}

#[test]
fn status_message_sits_directly_above_the_hint_bar() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.status_message = Some("hello status".to_string());
    let rows = render(&mut app, 80, 24);
    assert!(rows[22].contains("hello status"), "{:?}", rows[22]);
    assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
}

#[test]
fn with_no_message_or_scan_the_status_line_takes_no_row() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 80, 24);
    // The panes' bottom border is on row 22, directly above the hint bar.
    assert!(rows[22].contains('└'), "{:?}", rows[22]);
    assert!(rows[23].contains("Quit"));
}

#[test]
fn a_very_long_status_message_is_capped_and_cannot_push_the_hint_bar_off_screen() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.status_message = Some((0..30).map(|i| format!("failure line {i}")).collect::<Vec<_>>().join("\n"));
    let rows = render(&mut app, 80, 24);
    assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
    assert!(rows[20].contains("failure line 0"), "{:?}", rows[20]);
    assert!(rows[22].contains("failure line 2"), "{:?}", rows[22]);
    assert!(!rows.join("\n").contains("failure line 3"), "capped at STATUS_MAX_ROWS");
    assert!(rows[0].contains("zfsctl"), "the sidebar must still render");
}

#[test]
fn the_sidebar_no_longer_has_its_own_help_footer() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 100, 24);
    // "? Help" now appears only once, in the bottom bar.
    let occurrences = rows.iter().filter(|r| r.contains("? Help")).count();
    assert_eq!(occurrences, 1, "{rows:#?}");
}

#[test]
fn drawing_never_panics_on_a_tiny_terminal() {
    for (w, h) in [(20, 4), (30, 6), (10, 3), (1, 1), (80, 2)] {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.status_message = Some("a message that is far wider than this terminal".to_string());
        let _ = render(&mut app, w, h);
    }
}

#[test]
fn tiny_terminals_still_show_the_pinned_hints() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let rows = render(&mut app, 30, 8);
    assert!(rows[7].contains("? Help"), "{:?}", rows[7]);
    assert!(rows[7].contains("q Quit"), "{:?}", rows[7]);
}

#[test]
fn failure_messages_use_the_err_style_and_ordinary_ones_do_not() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let theme = app.theme.clone();
    let style_of_row22 = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer()[(0, 22)].style()
    };
    app.set_error("zpool destroy failed".to_string());
    assert_eq!(style_of_row22(&mut app).fg, theme.err.fg);
    app.status_message = Some("select a disk with Space first".to_string());
    assert_eq!(style_of_row22(&mut app).fg, Some(Color::Reset)); // an unstyled buffer cell reports Reset, not None
}

#[test]
fn the_selection_line_is_not_clipped_under_a_long_prompt_on_a_small_terminal() {
    let long = "Remove SMB share 'media' for 'tank/media'? Connected clients lose access. They will have to reconnect and any open files may be lost. (y/n)";
    for choice in [None, Some(true), Some(false)] {
        let rows = render(&mut confirm_with(choice, long), 64, 20).join("\n");
        let want = match choice {
            None => "Press y",
            Some(true) => "YES selected",
            Some(false) => "NO selected",
        };
        assert!(rows.contains(want), "{choice:?} clipped:\n{rows}");
    }
}

#[test]
fn failed_popup_height_reserves_a_word_wrap_margin() {
    let body = format!("Could not load accounts: {}\n\nEsc to close", "word ".repeat(26));
    let old = super::super::wrapped_line_count(&body, 38) + 2;
    let h = super::super::failed_popup_height(&body, 40, 24);
    assert!(h > old, "{h} vs old {old}");
    assert_eq!(super::super::failed_popup_height(&body, 40, 3), 3);
    assert_eq!(super::super::failed_popup_height(&body, 40, 0), 0);
}

#[test]
fn a_realistic_multi_word_ssh_failure_keeps_esc_hint_and_tail_visible() {
    let mut app = app_in_pools(vec![]);
    let msg = "Could not load accounts: command 'getent passwd' failed with status 255: fb@172.16.10.90: Permission denied (publickey,password). Warning: Permanently added '172.16.10.90' (ED25519) to the list of known hosts. ENDMARK";
    app.picker = Some(crate::ui::picker::PickerState {
        values: vec![],
        filter: String::new(),
        selected: 0,
        checked: std::collections::HashSet::new(),
        multi_select: false,
        load: crate::ui::picker::PickerLoad::Failed(msg.to_string()),
        note: None,
        account: None,
        pool_host: None,
    });
    let rows = render(&mut app, 80, 24).join("\n");
    assert!(rows.contains("Esc to close"), "{rows}");
    assert!(rows.contains("ENDMARK"), "{rows}");
}

#[test]
fn the_about_screen_draws_the_wordmark_in_the_warn_colour_and_leaves_it_out_when_it_cannot_fit() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(crossterm::event::KeyCode::Char('A'));
    let rows = render(&mut app, 120, 40);
    let logo = crate::ui::logo::wordmark();
    assert!(logo.iter().all(|line| rows.iter().any(|r| r.contains(line.as_str()))), "{rows:?}");
    assert_eq!(fg_of(&mut app, "▀▀▀▀▀", 120, 40), Some(ratatui::style::Color::Yellow), "the theme's warn colour");
    assert!(rows.iter().any(|r| r.contains("zfsctl v")), "the version stays");
    // a pop-up too narrow for the drawing shows the text without it (a wrapped wordmark would be garbage)
    let narrow = render(&mut app, 60, 40);
    assert!(logo.iter().all(|line| narrow.iter().all(|r| !r.contains(line.as_str()))), "the big wordmark is left out: {narrow:?}");
    assert!(narrow.iter().any(|r| r.contains("zfsctl v")), "{narrow:?}");
}

#[test]
fn the_about_wordmark_and_version_are_centred_in_the_pop_up() {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(crossterm::event::KeyCode::Char('A'));
    let rows: Vec<Vec<char>> = render(&mut app, 120, 40).iter().map(|r| r.chars().collect()).collect();
    let find = |needle: &str| -> (usize, usize) {
        let n: Vec<char> = needle.chars().collect();
        rows.iter().enumerate().find_map(|(y, r)| r.windows(n.len()).position(|w| w == n.as_slice()).map(|x| (y, x))).unwrap_or_else(|| panic!("{needle} not on screen"))
    };
    let (top, left) = find("┌About");
    let right = left + rows[top][left..].iter().position(|c| *c == '┐').expect("the pop-up's corner");
    let gaps = |x: usize, width: usize| ((x - (left + 1)) as i64, ((right - 1) - (x + width - 1)) as i64);
    let last_line = crate::ui::logo::wordmark().pop().unwrap();
    let (_, x) = find(&last_line);
    let (l, r) = gaps(x, crate::ui::logo::WIDTH);
    assert!((l - r).abs() <= 1 && l > 0, "the wordmark has {l} columns on its left and {r} on its right");
    let version = format!("zfsctl v{}", env!("CARGO_PKG_VERSION"));
    let (_, x) = find(&version);
    let (l, r) = gaps(x, version.chars().count());
    assert!((l - r).abs() <= 1 && l > 0, "the version line has {l} on its left and {r} on its right");
    let (_, x) = find("A terminal UI for managing ZFS");
    assert_eq!(x, left + 1, "the description stays flush left");
}

#[test]
fn a_selection_below_the_visible_rows_scrolls_into_view() {
    let (mut app, _dir) = settings_render_app("");
    app.settings_table.select(Some(app.settings_view.rows.len() - 1));
    let text = render(&mut app, 100, 12).join("\n");
    assert!(text.contains("Scrub ignore"), "{text}");
}

#[test]
fn a_narrow_terminal_cuts_long_values_with_an_ellipsis_and_never_panics() {
    let (mut app, _dir) = alerts_render_app("[notify]\nmail_to = \"a@x.org\"\nmail_command = \"msmtp -a default -t --some --very --long --set --of --options\"\n");
    for (w, h) in [(60, 20), (80, 24), (30, 10)] {
        let text = render(&mut app, w, h).join("\n");
        assert!(!text.is_empty());
    }
    assert!(render(&mut app, 80, 24).join("\n").contains('…'));
}
