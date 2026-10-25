//! Headings and column titles are drawn in the header style.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn settings_group_titles_use_the_header_style_and_their_rows_do_not() {
    let (mut app, _dir) = settings_render_app("");
    let buf = render_buffer(&mut app, 120, 30);
    let header = app.theme.header();
    assert_eq!(style_of(&buf, 120, 30, "Dashboard thresholds"), (header.fg.unwrap(), true), "group title");
    let (row_fg, row_bold) = style_of(&buf, 120, 30, "Memory warn");
    assert!(row_fg != header.fg.unwrap() && !row_bold, "a row stays body-coloured");
}

#[test]
fn the_help_screens_section_titles_use_the_header_style_and_its_keys_do_not() {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Info { title: "Help".to_string(), body: crate::ui::state::help_text_for_book() });
    let buf = render_buffer(&mut app, 140, 50);
    assert_eq!(style_of(&buf, 140, 50, "Global"), (app.theme.header().fg.unwrap(), true), "section title");
    let (fg, bold) = style_of(&buf, 140, 50, "switch sections");
    assert!(fg != app.theme.header().fg.unwrap() && !bold, "a key line stays body-coloured");
}

#[test]
fn a_choose_list_draws_host_headings_in_the_header_style_and_rows_in_their_level_colour() {
    use crate::ui::dashboard::Level;
    let mut app = App::new(vec!["local".to_string()]);
    let look = crate::ui::modal::ChooseLook { heads: vec![(0, "alphahost  (2 datasets)".into()), (2, "betahost  (1 dataset)".into())], levels: vec![Level::Issue, Level::Watch, Level::Issue], marked: vec![] };
    let rows = vec!["issue  tank/one  no snapshots".to_string(), "watch  zroot/var  no copy".to_string(), "issue  tank/two  no snapshots".to_string()];
    app.modal = Some(Modal::Choose { title: "Unprotected data".into(), note: "3 datasets".into(), rows, selected: 2, then: crate::ui::modal::ChooseThen::Copy { host: "h".into(), dataset: "d".into() }, look });
    let buf = render_buffer(&mut app, 100, 24);
    let header = app.theme.header();
    assert_eq!(style_of(&buf, 100, 24, "alphahost"), (header.fg.unwrap(), true));
    assert_eq!(style_of(&buf, 100, 24, "betahost"), (header.fg.unwrap(), true));
    assert_eq!(style_of(&buf, 100, 24, "tank/one").0, app.theme.err.fg.unwrap(), "a red row");
    assert_eq!(style_of(&buf, 100, 24, "zroot/var").0, app.theme.warn.fg.unwrap(), "an amber row");
    let lines: Vec<String> = (0..24u16).map(|y| (0..100u16).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect();
    let at = |w: &str| lines.iter().position(|l| l.contains(w)).unwrap();
    assert!(at("betahost") > at("zroot/var") + 1, "a blank line before the next host: {lines:#?}");
}

#[test]
fn a_table_screens_column_titles_use_the_header_style() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    let buf = render_buffer(&mut app, 120, 30);
    assert_eq!(style_of(&buf, 120, 30, "Written"), (app.theme.header().fg.unwrap(), true));
}
