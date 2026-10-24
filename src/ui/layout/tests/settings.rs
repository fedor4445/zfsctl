//! The Settings and Alerts screens.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn the_alerts_screen_shows_settings_status_and_events() {
    let (mut app, dir) = alerts_render_app("[daemon]\nnotify_command = \"curl -s x\"\n\n[notify]\nmail_to = \"me@x.org\"\n");
    std::fs::write(dir.path().join("notify.json"), "{\"last_report\":\"2026-10-10\"}").unwrap();
    crate::daemon::health_text::EventLog::new(dir.path().join("health-events.jsonl")).append(&crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "fbsd16".into(), kind: "alert".into(), text: "tank DEGRADED".into() }).unwrap();
    app.refresh_alerts();
    let text = render(&mut app, 140, 40).join("\n");
    for want in ["Alerts", "Email to", "me@x.org", "Mail command", "msmtp -t", "(default)", "Hook command", "curl -s x", "edit in config.toml", "Monitor on", "Last report sent 2026-10-10", "fbsd16", "alert", "tank DEGRADED"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_settings_screen_shows_groups_rows_and_defaults() {
    let (mut app, _dir) = settings_render_app("[dashboard]\nmemory_warn = 80\n[cloud.caps]\nb2 = \"10G\"\n");
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["Settings", "Display", "Theme", "Dashboard thresholds", "Memory warn / crit", "80 / 97 %", "CPU warn", "95 %", "(default)", "Cloud caps", "b2", "10G"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_settings_screen_without_remotes_says_how_to_list_them() {
    let (mut app, _dir) = settings_render_app("");
    assert!(render(&mut app, 120, 30).join("\n").contains("open Cloud once to list your remotes"));
}

#[test]
fn the_settings_screen_shows_the_problem_when_the_file_is_unreadable() {
    let (mut app, _dir) = settings_render_app("[dashboard\n");
    let text = render(&mut app, 120, 30).join("\n");
    assert!(text.contains("config.toml") && text.contains("by hand"), "{text}");
    assert!(!text.contains("Memory warn"), "{text}");
}

#[test]
fn a_wrong_protection_value_is_shown_in_red_under_its_group() {
    let (mut app, _dir) = settings_render_app("[protection]\nemail = \"of\"\n");
    let text = render(&mut app, 140, 40).join("\n");
    assert!(text.contains("[protection]: email must be immediate, report or off; judging at Basic"), "{text}");
    assert!(text.contains("Memory warn"), "the rest is still shown: {text}");
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in {text}"));
    assert!(at("Data protection") < at("judging at Basic") && at("judging at Basic") < at("Level"), "{text}");
}

#[test]
fn before_the_first_load_the_settings_screen_says_so() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
    assert!(render(&mut app, 100, 30).join("\n").contains("Loading"));
}

#[test]
fn before_the_first_load_the_alerts_screen_says_so_instead_of_showing_empty_settings() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
    assert!(render(&mut app, 100, 30).join("\n").contains("Loading"));
}

#[test]
fn nothing_configured_says_the_monitor_is_off_and_how_to_turn_it_on() {
    let (mut app, _dir) = alerts_render_app("");
    let text = render(&mut app, 140, 40).join("\n");
    assert!(text.contains("Monitor off") && text.contains("(not set)") && text.contains("Last report: never") && text.contains("(none yet)"), "{text}");
}

#[test]
fn an_unreadable_config_is_reported_on_the_screen() {
    let (mut app, _dir) = alerts_render_app("[notify\nbroken");
    let text = render(&mut app, 140, 40).join("\n");
    assert!(text.contains("could not be read"), "{text}");
}
