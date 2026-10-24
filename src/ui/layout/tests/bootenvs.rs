//! The Boot Envs screen.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn the_boot_envs_table_decodes_activity_and_drops_columns_when_narrow() {
    use crate::zfs::bootenv::{ActiveFlags, BootEnv};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.bootenvs = vec![
        BootEnv { name: "default".into(), flags: ActiveFlags { now: true, reboot: true, temp: false }, mountpoint: Some("/".into()), space: "5.18G".into(), created: "2026-07-24 18:48".into() },
        BootEnv { name: "spare".into(), flags: ActiveFlags { now: false, reboot: false, temp: true }, mountpoint: None, space: "8K".into(), created: "2026-10-07 19:26".into() },
    ];
    let wide = render(&mut app, 120, 24).join("\n");
    for want in ["Boot Environments", "Name", "Active", "Space", "Mountpoint", "Created", "default", "Now+Reboot", "Temp", "5.18G", "2026-07-24 18:48"] {
        assert!(wide.contains(want), "missing {want:?} in:\n{wide}");
    }
    let narrow = render(&mut app, 80, 24).join("\n");
    for want in ["Name", "Active", "Space", "default"] {
        assert!(narrow.contains(want), "missing {want:?} in:\n{narrow}");
    }
    assert!(!narrow.contains("Mountpoint") && !narrow.contains("Created"), "optional columns must drop:\n{narrow}");
}

#[test]
fn an_unavailable_boot_envs_section_says_why_in_its_title() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.bootenvs_unavailable = Some("command 'bectl list' failed with status 127: bectl: not found".to_string());
    let text = render(&mut app, 140, 24).join("\n");
    assert!(text.contains("[unavailable: command 'bectl list' failed"), "{text}");
}

#[test]
fn a_long_boot_environment_name_is_shown_in_full_even_on_a_narrow_terminal() {
    use crate::zfs::bootenv::{ActiveFlags, BootEnv};
    let long = "15.1-RELEASE-p3_2026-10-07_184800";
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.bootenvs = vec![BootEnv { name: long.into(), flags: ActiveFlags::default(), mountpoint: None, space: "8K".into(), created: "2026-10-07 18:48".into() }];
    for width in [80, 120] {
        let text = render(&mut app, width, 24).join("\n");
        assert!(text.contains(long), "{width} cols: name cut off in:\n{text}");
    }
}

#[test]
fn a_loaded_empty_list_says_there_are_no_boot_environments_but_an_unloaded_one_does_not() {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    assert!(!render(&mut app, 120, 24).join("\n").contains("no boot environments"));
    app.bootenvs_loaded = true;
    assert!(render(&mut app, 120, 24).join("\n").contains("no boot environments"));
}


#[test]
fn a_mounted_boot_environments_path_is_in_the_title_even_when_the_column_is_hidden() {
    use crate::zfs::bootenv::{ActiveFlags, BootEnv};
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::BootEnvs).unwrap();
    app.bootenvs = vec![BootEnv { name: "zit-a".into(), flags: ActiveFlags::default(), mountpoint: Some("/tmp/be_mount.Xy12".into()), space: "8K".into(), created: "2026-10-08 18:48".into() }];
    app.bootenvs_table.select(Some(0));
    let narrow = render(&mut app, 80, 24).join("\n");
    assert!(!narrow.contains("Mountpoint"), "the column is hidden at this width:\n{narrow}");
    assert!(narrow.contains("'zit-a' mounted at /tmp/be_mount.Xy12"), "{narrow}");
    app.bootenvs[0].mountpoint = None;
    assert!(!render(&mut app, 80, 24).join("\n").contains("mounted at"));
    app.bootenvs[0].mountpoint = Some("/".into());
    assert!(!render(&mut app, 80, 24).join("\n").contains("mounted at"), "the running BE at / is not news");
}
