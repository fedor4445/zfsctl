//! Pop-ups: confirmations, pickers, lists and the About box.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn other_targets_do_not_get_the_pool_info() {
    use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
    let mut app = pools_app_with_info(None);
    let target = MetricsTarget::Dataset("tank/a".to_string());
    app.metrics.retarget(Some(target.clone()));
    app.metrics.apply(&target, Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts: PoolFacts { size: 1000, allocated: 100, ..PoolFacts::default() } }, space: None, host: None, snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
    let text = render(&mut app, 170, 40).join("\n");
    assert!(!text.contains("Trim ") && !text.contains("Vdevs      "), "{text}");
}

fn style_of_symbol(buf: &ratatui::buffer::Buffer, symbol: &str) -> Option<Style> {
    buf.content().iter().find(|c| c.symbol() == symbol).map(ratatui::buffer::Cell::style)
}

fn destroy_pool_modal() -> Modal {
    Modal::ConfirmTyped {
        prompt: "Type the pool name to destroy 'tank':".to_string(),
        expected: "tank".to_string(),
        input: "ta".to_string(),
        action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::DestroyPool { pool: "tank".to_string() } },
    }
}

#[test]
fn a_destroy_modal_is_titled_with_a_glyph_and_the_action_and_drawn_in_red() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.modal = Some(destroy_pool_modal());
    let rows = render(&mut app, 100, 30).join("\n");
    assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
    let buf = buffer_of(&mut app, 100, 30);
    assert_eq!(style_of_symbol(&buf, "✖").unwrap().fg, Some(Color::Red));
}

#[test]
fn a_destroy_modal_still_shows_the_typed_input_line() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.modal = Some(destroy_pool_modal());
    let rows = render(&mut app, 100, 30).join("\n");
    assert!(rows.contains("> ta"), "{rows}");
}

#[test]
fn a_warn_confirm_is_titled_with_a_triangle_and_drawn_in_yellow() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.modal = Some(Modal::Confirm { choice: None,
        prompt: "Offline disk 'da0' in pool 'tank'? (y/n)".to_string(),
        action: PendingAction { host: 0, section: Section::Vdevs, call: MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da0".to_string() } },
    });
    let rows = render(&mut app, 100, 30).join("\n");
    assert!(rows.contains("▲ OFFLINE DISK"), "{rows}");
    let buf = buffer_of(&mut app, 100, 30);
    assert_eq!(style_of_symbol(&buf, "▲").unwrap().fg, Some(Color::Yellow));
}

#[test]
fn a_routine_confirm_keeps_its_plain_title() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.modal = Some(Modal::Confirm { choice: None,
        prompt: "Scrub pool 'tank'? (y/n)".to_string(),
        action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
    });
    let rows = render(&mut app, 100, 30).join("\n");
    assert!(rows.contains("Confirm (y/n, then Enter)"), "{rows}");
    assert!(!rows.contains('✖'));
}

#[test]
fn a_confirm_says_what_is_selected_and_what_enter_will_do() {
    let none = render(&mut confirm_with(None, "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
    assert!(none.contains("Press y for yes or n for no, then Enter"), "{none}");
    let yes = render(&mut confirm_with(Some(true), "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
    assert!(yes.contains("YES selected") && yes.contains("Enter to run"), "{yes}");
    let no = render(&mut confirm_with(Some(false), "Scrub pool 'tank'? (y/n)"), 100, 30).join("\n");
    assert!(no.contains("NO selected") && no.contains("Enter to cancel"), "{no}");
}

#[test]
fn the_danger_modal_is_distinguishable_without_color_in_the_mono_theme() {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.theme = Theme::mono();
    app.modal = Some(destroy_pool_modal());
    let rows = render(&mut app, 100, 30).join("\n");
    assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
}

#[test]
fn a_long_failed_picker_message_keeps_esc_hint_and_the_error_tail_visible() {
    let mut app = app_in_pools(vec![]);
    let msg = format!("ssh: connect to host 172.16.10.90 port 22: {} Connection timed out ENDMARK", "y".repeat(80));
    app.picker = Some(crate::ui::picker::PickerState {
        values: vec![],
        filter: String::new(),
        selected: 0,
        checked: std::collections::HashSet::new(),
        multi_select: false,
        load: crate::ui::picker::PickerLoad::Failed(msg),
        note: None,
        account: None,
        pool_host: None,
    });
    let rows = render(&mut app, 80, 24).join("\n");
    assert!(rows.contains("Esc to close"), "{rows}");
    assert!(rows.contains("ENDMARK"), "{rows}");
}
#[test]
fn picker_title_mentions_space_only_for_multi_select() {
    assert_eq!(super::super::picker_title("", true), "Pick a value (type filter, Space toggle, Enter OK, Esc cancel)");
    assert_eq!(super::super::picker_title("", false), "Pick a value (type filter, Enter OK, Esc cancel)");
    assert_eq!(super::super::picker_title("wh", true), "Pick — filter 'wh' (Space toggle, Enter OK, Esc cancel)");
    assert_eq!(super::super::picker_title("wh", false), "Pick — filter 'wh' (Enter OK, Esc cancel)");
}

#[test]
fn a_choose_list_scrolls_to_keep_the_chosen_row_on_screen_and_marks_it() {
    let mut app = App::new(vec!["local".to_string()]);
    let rows: Vec<String> = (0..40).map(|i| format!("dataset-{i:02} lacks something")).collect();
    app.modal = Some(Modal::Choose { title: "Unprotected data".into(), note: "40 datasets\nsecond line".into(), rows, selected: 35, then: crate::ui::modal::ChooseThen::Copy { host: "h".into(), dataset: "d".into() }, look: crate::ui::modal::ChooseLook::default() });
    let buf = render_buffer(&mut app, 100, 20);
    let text: String = (0..20).map(|y| (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect();
    assert!(text.contains("dataset-35") && !text.contains("dataset-00"), "the chosen row is shown, the top is scrolled off:\n{text}");
    let find = |word: &str| {
        (0..20u16).find_map(|y| {
            let row: String = (0..100u16).map(|x| buf[(x, y)].symbol().to_string()).collect();
            row.find(word).map(|at| (u16::try_from(row[..at].chars().count()).unwrap(), y))
        })
        .unwrap()
    };
    let (cx, cy) = find("dataset-35");
    let (ox, oy) = find("dataset-34");
    assert!(buf[(cx, cy)].style() != buf[(ox, oy)].style(), "the chosen row is drawn differently from its neighbour");
}

#[test]
fn an_info_pop_up_other_than_help_keeps_its_plain_lines() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Info { title: "Report preview (nothing sent)".to_string(), body: "NEEDS ATTENTION\n  nothing".to_string() });
    let buf = render_buffer(&mut app, 100, 30);
    let (fg, bold) = style_of(&buf, 100, 30, "NEEDS ATTENTION");
    assert!(fg != app.theme.header().fg.unwrap() && !bold, "only Help styles its non-indented lines");
}
