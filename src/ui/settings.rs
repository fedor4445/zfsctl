//! The Settings section: the settings of `config.toml` that have no screen of their own (theme, auto-refresh, the
//! Dashboard thresholds and the cloud caps), as rows to show and as validated changes to save.

use crate::config_edit::Change;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    Theme,
    AutoRefresh,
    Memory,
    Swap,
    Quota,
    Cpu,
    Scrub,
    ScrubIgnore,
    Cap(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub group: &'static str,
    pub setting: Setting,
    pub label: String,
    /// What the screen shows: the effective value.
    pub value: String,
    pub is_default: bool,
    /// What the file says for each field of the form; empty where the key is not set.
    pub raw: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsView {
    pub loaded: bool,
    /// Why nothing can be edited: `config.toml` could not be read or parsed.
    pub problem: Option<String>,
    pub rows: Vec<Row>,
}

const DISPLAY: &str = "Display";
const THRESHOLDS: &str = "Dashboard thresholds";
const CAPS: &str = "Cloud caps";

/// The rows for the file at `config_path`. `remotes` are the rclone remotes the Cloud screen knows; any remote that
/// already has a cap in the file is listed too.
pub fn load(config_path: &Path, remotes: &[String], no_color: bool) -> SettingsView {
    use crate::config::{Config, Thresholds, DEFAULT_AUTO_REFRESH_SECS};
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => return SettingsView { loaded: true, problem: Some(e.to_string()), rows: Vec::new() },
    };
    let d = Thresholds::default();
    let t = cfg.dashboard.resolve();
    let dash = &cfg.dashboard;
    let text = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_default();
    let row = |group, setting, label: &str, value: String, is_default: bool, raw: Vec<String>| Row { group, setting, label: label.to_string(), value, is_default, raw };
    let mut rows = vec![
        row(DISPLAY, Setting::Theme, "Theme", cfg.ui.theme.clone().unwrap_or_else(|| if no_color { "mono" } else { "ansi" }.to_string()), cfg.ui.theme.is_none(), vec![cfg.ui.theme.clone().unwrap_or_default()]),
        row(
            DISPLAY,
            Setting::AutoRefresh,
            "Auto-refresh",
            match cfg.ui.auto_refresh_secs.unwrap_or(DEFAULT_AUTO_REFRESH_SECS) {
                0 => "off".to_string(),
                n => format!("{n} s"),
            },
            cfg.ui.auto_refresh_secs.is_none(),
            vec![text(cfg.ui.auto_refresh_secs)],
        ),
        row(THRESHOLDS, Setting::Memory, "Memory warn / crit", format!("{} / {} %", t.memory.0, t.memory.1), t.memory == d.memory, vec![text(dash.memory_warn.map(u64::from)), text(dash.memory_crit.map(u64::from))]),
        row(THRESHOLDS, Setting::Swap, "Swap warn / crit", format!("{} / {} %", t.swap.0, t.swap.1), t.swap == d.swap, vec![text(dash.swap_warn.map(u64::from)), text(dash.swap_crit.map(u64::from))]),
        row(THRESHOLDS, Setting::Quota, "Quota warn / crit", format!("{} / {} %", t.quota.0, t.quota.1), t.quota == d.quota, vec![text(dash.quota_warn.map(u64::from)), text(dash.quota_crit.map(u64::from))]),
        row(THRESHOLDS, Setting::Cpu, "CPU warn", format!("{} %", t.cpu_warn), t.cpu_warn == d.cpu_warn, vec![text(dash.cpu_warn.map(u64::from))]),
        row(THRESHOLDS, Setting::Scrub, "Scrub warn / crit", format!("{} / {} days", t.scrub_days.0, t.scrub_days.1), t.scrub_days == d.scrub_days, vec![text(dash.scrub_warn_days.map(u64::from)), text(dash.scrub_crit_days.map(u64::from))]),
        row(THRESHOLDS, Setting::ScrubIgnore, "Scrub ignore", if t.scrub_ignore.is_empty() { "(none)".to_string() } else { t.scrub_ignore.join(", ") }, t.scrub_ignore.is_empty(), vec![t.scrub_ignore.join(", ")]),
    ];
    let mut names: Vec<&String> = remotes.iter().chain(cfg.cloud.caps.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let cap = cfg.cloud.caps.get(name);
        rows.push(row(CAPS, Setting::Cap(name.clone()), name, cap.cloned().unwrap_or_else(|| "(no cap)".to_string()), cap.is_none(), vec![cap.cloned().unwrap_or_default()]));
    }
    SettingsView { loaded: true, problem: None, rows }
}

/// `(label, current text)` for each field of the form of `row`; the text is empty where the setting is on its default.
pub fn form_fields(row: &Row) -> Vec<(String, String)> {
    let labels: Vec<String> = match row.setting {
        Setting::Theme => vec![format!("Theme ({})", crate::ui::theme::THEME_NAMES.join(", "))],
        Setting::AutoRefresh => vec!["Seconds between refreshes (0 = off)".to_string()],
        Setting::Memory | Setting::Swap | Setting::Quota => vec!["Warn at (%)".to_string(), "Critical at (%)".to_string()],
        Setting::Cpu => vec!["Warn at (%)".to_string()],
        Setting::Scrub => vec!["Warn after (days)".to_string(), "Critical after (days)".to_string()],
        Setting::ScrubIgnore => vec!["Pools to skip (comma-separated)".to_string()],
        Setting::Cap(_) => vec!["Plan size (such as 10G or 5T)".to_string()],
    };
    labels.into_iter().zip(row.raw.iter().cloned().chain(std::iter::repeat(String::new()))).collect()
}

fn dashboard(key: &str) -> [&str; 2] {
    ["dashboard", key]
}

/// Both halves of a pair, or neither; warn above zero and below crit, crit at most `max`. A pair on its defaults is
/// removed from the file rather than pinned.
fn pair_changes(values: &[String], keys: [&str; 2], max: u32, default: (u32, u32)) -> Result<Vec<Change>, String> {
    let get = |i: usize| values.get(i).map_or("", |v| v.trim());
    let (warn, crit) = (get(0), get(1));
    let remove = || keys.iter().map(|k| Change::remove(&dashboard(k))).collect();
    if warn.is_empty() && crit.is_empty() {
        return Ok(remove());
    }
    if warn.is_empty() || crit.is_empty() {
        return Err("enter both numbers or leave both empty".to_string());
    }
    let number = |v: &str| v.parse::<u32>().map_err(|_| format!("'{v}' is not a whole number"));
    let (w, c) = (number(warn)?, number(crit)?);
    if w == 0 || w >= c || c > max {
        return Err(format!("warn must be above 0 and below critical, and critical at most {max}"));
    }
    if (w, c) == default {
        return Ok(remove());
    }
    Ok(vec![Change::set(&dashboard(keys[0]), i64::from(w)), Change::set(&dashboard(keys[1]), i64::from(c))])
}

pub fn build_changes(setting: &Setting, values: &[String]) -> Result<Vec<Change>, String> {
    let first = values.first().map_or("", |v| v.trim());
    let number = |max: u64, min: u64, what: &str| -> Result<u64, String> {
        first.parse::<u64>().ok().filter(|n| (min..=max).contains(n)).ok_or_else(|| format!("{what} must be a whole number from {min} to {max}"))
    };
    Ok(match setting {
        Setting::Theme if first.is_empty() => vec![Change::remove(&["ui", "theme"])],
        Setting::Theme => {
            if !crate::ui::theme::THEME_NAMES.contains(&first) {
                return Err(format!("unknown theme '{first}': valid themes are {}", crate::ui::theme::THEME_NAMES.join(", ")));
            }
            vec![Change::set(&["ui", "theme"], first)]
        }
        Setting::AutoRefresh if first.is_empty() => vec![Change::remove(&["ui", "auto_refresh_secs"])],
        Setting::AutoRefresh => vec![Change::set(&["ui", "auto_refresh_secs"], number(3600, 0, "Auto-refresh")? as i64)],
        Setting::Memory => pair_changes(values, ["memory_warn", "memory_crit"], 100, (90, 97))?,
        Setting::Swap => pair_changes(values, ["swap_warn", "swap_crit"], 100, (50, 90))?,
        Setting::Quota => pair_changes(values, ["quota_warn", "quota_crit"], 100, (90, 98))?,
        Setting::Scrub => pair_changes(values, ["scrub_warn_days", "scrub_crit_days"], u32::MAX, (35, 90))?,
        Setting::Cpu if first.is_empty() => vec![Change::remove(&dashboard("cpu_warn"))],
        Setting::Cpu => {
            let n = number(100, 1, "CPU warn")?;
            if n == 95 { vec![Change::remove(&dashboard("cpu_warn"))] } else { vec![Change::set(&dashboard("cpu_warn"), n as i64)] }
        }
        Setting::ScrubIgnore => {
            let names: Vec<&str> = first.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
            if names.is_empty() {
                vec![Change::remove(&dashboard("scrub_ignore"))]
            } else {
                vec![Change::set(&dashboard("scrub_ignore"), names.into_iter().collect::<toml_edit::Array>())]
            }
        }
        Setting::Cap(name) if first.is_empty() => vec![Change::remove(&["cloud", "caps", name])],
        Setting::Cap(name) => {
            if crate::config::parse_cap(first).is_none() {
                return Err(format!("'{first}' is not a size: use digits and K, M, G or T, such as 10G"));
            }
            vec![Change::set(&["cloud", "caps", name], first)]
        }
    })
}

/// One line for the confirmation: what is about to be saved.
pub fn describe(setting: &Setting, values: &[String]) -> String {
    let v = |i: usize| values.get(i).map_or("", |v| v.trim());
    let (label, unit) = match setting {
        Setting::Theme => ("Theme", ""),
        Setting::AutoRefresh => ("Auto-refresh", " s"),
        Setting::Memory => ("Memory warn / crit", " %"),
        Setting::Swap => ("Swap warn / crit", " %"),
        Setting::Quota => ("Quota warn / crit", " %"),
        Setting::Cpu => ("CPU warn", " %"),
        Setting::Scrub => ("Scrub warn / crit", " days"),
        Setting::ScrubIgnore => ("Scrub ignore", ""),
        Setting::Cap(name) => return format!("Cap of {name}: {}", if v(0).is_empty() { "none" } else { v(0) }),
    };
    if values.iter().all(|x| x.trim().is_empty()) {
        return format!("{label}: back to the default");
    }
    let shown: Vec<&str> = values.iter().map(|x| x.trim()).collect();
    format!("{label}: {}{unit}", shown.join(" / "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str, remotes: &[&str], no_color: bool) -> SettingsView {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        load(&path, &remotes.iter().map(ToString::to_string).collect::<Vec<_>>(), no_color)
    }

    fn values(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    fn row<'a>(v: &'a SettingsView, label: &str) -> &'a Row {
        v.rows.iter().find(|r| r.label == label).unwrap_or_else(|| panic!("no row {label}: {:?}", v.rows))
    }

    #[test]
    fn load_lists_the_groups_in_order_with_defaults_marked() {
        let v = view("", &[], false);
        let labels: Vec<&str> = v.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Theme", "Auto-refresh", "Memory warn / crit", "Swap warn / crit", "Quota warn / crit", "CPU warn", "Scrub warn / crit", "Scrub ignore"]);
        assert!(v.loaded && v.problem.is_none() && v.rows.iter().all(|r| r.is_default), "{v:?}");
        assert_eq!(row(&v, "Theme").value, "ansi");
        assert_eq!(row(&v, "Auto-refresh").value, "10 s");
        assert_eq!(row(&v, "Memory warn / crit").value, "90 / 97 %");
        assert_eq!(row(&v, "Scrub warn / crit").value, "35 / 90 days");
        assert_eq!(view("", &[], true).rows[0].value, "mono");
        assert_eq!(v.rows[0].group, "Display");
        assert_eq!(row(&v, "Memory warn / crit").group, "Dashboard thresholds");
    }

    #[test]
    fn load_shows_saved_values_and_cap_rows() {
        let v = view("[ui]\ntheme = \"mono\"\nauto_refresh_secs = 0\n[dashboard]\nmemory_warn = 80\n[cloud.caps]\nb2 = \"10G\"\n", &["zfsctl-aws", "b2"], false);
        assert_eq!(row(&v, "Theme").value, "mono");
        assert!(!row(&v, "Theme").is_default);
        assert_eq!(row(&v, "Auto-refresh").value, "off");
        let memory = row(&v, "Memory warn / crit");
        assert_eq!((memory.value.as_str(), memory.is_default), ("80 / 97 %", false));
        assert_eq!(memory.raw, ["80", ""]);
        let b2 = row(&v, "b2");
        assert_eq!((b2.value.as_str(), b2.is_default, b2.group), ("10G", false, "Cloud caps"));
        let aws = row(&v, "zfsctl-aws");
        assert_eq!((aws.value.as_str(), aws.is_default), ("(no cap)", true));
        let caps: Vec<&str> = v.rows.iter().filter(|r| r.group == "Cloud caps").map(|r| r.label.as_str()).collect();
        assert_eq!(caps, ["b2", "zfsctl-aws"]);
    }

    #[test]
    fn an_unparsable_file_gives_a_problem_and_no_rows() {
        let v = view("[dashboard\n", &["b2"], false);
        assert!(v.loaded && v.rows.is_empty());
        assert!(v.problem.as_deref().is_some_and(|p| p.contains("config.toml")), "{v:?}");
    }

    #[test]
    fn form_fields_carry_the_saved_text_and_empty_for_defaults() {
        let v = view("[dashboard]\nmemory_warn = 80\n", &[], false);
        let fields = form_fields(row(&v, "Memory warn / crit"));
        assert_eq!(fields.len(), 2);
        assert_eq!((fields[0].1.as_str(), fields[1].1.as_str()), ("80", ""));
        assert_eq!(form_fields(row(&v, "CPU warn")).len(), 1);
    }

    fn sets(changes: &[Change]) -> Vec<String> {
        changes.iter().map(|c| match c {
            Change::Set(p, v) => format!("set {} {}", p.join("."), v.to_string().trim()),
            Change::Remove(p) => format!("remove {}", p.join(".")),
        }).collect()
    }

    #[test]
    fn a_valid_pair_becomes_two_sets_and_defaults_become_removes() {
        assert_eq!(sets(&build_changes(&Setting::Memory, &values(&["85", "95"])).unwrap()), ["set dashboard.memory_warn 85", "set dashboard.memory_crit 95"]);
        let reset = ["remove dashboard.memory_warn", "remove dashboard.memory_crit"];
        assert_eq!(sets(&build_changes(&Setting::Memory, &values(&["90", "97"])).unwrap()), reset);
        assert_eq!(sets(&build_changes(&Setting::Memory, &values(&["", ""])).unwrap()), reset);
        assert_eq!(sets(&build_changes(&Setting::Scrub, &values(&["30", "60"])).unwrap()), ["set dashboard.scrub_warn_days 30", "set dashboard.scrub_crit_days 60"]);
    }

    #[test]
    fn the_quota_thresholds_are_a_pair_like_memory_and_swap() {
        assert_eq!(sets(&build_changes(&Setting::Quota, &values(&["85", "95"])).unwrap()), ["set dashboard.quota_warn 85", "set dashboard.quota_crit 95"]);
        assert_eq!(sets(&build_changes(&Setting::Quota, &values(&["90", "98"])).unwrap()), ["remove dashboard.quota_warn", "remove dashboard.quota_crit"]);
        assert!(build_changes(&Setting::Quota, &values(&["98", "90"])).is_err());
        assert_eq!(describe(&Setting::Quota, &values(&["85", "95"])), "Quota warn / crit: 85 / 95 %");
    }

    #[test]
    fn a_pair_with_warn_not_below_crit_or_half_filled_is_refused() {
        for bad in [["97", "90"], ["0", "50"], ["50", "101"], ["abc", "95"], ["85", ""], ["", "95"]] {
            assert!(build_changes(&Setting::Swap, &values(&bad)).is_err(), "{bad:?}");
        }
        assert!(build_changes(&Setting::Scrub, &values(&["90", "35"])).is_err());
        assert!(build_changes(&Setting::Memory, &values(&["85", ""])).unwrap_err().contains("both"));
    }

    #[test]
    fn cpu_refresh_theme_and_cap_validation() {
        for bad in ["0", "101", "x"] {
            assert!(build_changes(&Setting::Cpu, &values(&[bad])).is_err(), "cpu {bad}");
        }
        assert_eq!(sets(&build_changes(&Setting::Cpu, &values(&["80"])).unwrap()), ["set dashboard.cpu_warn 80"]);
        for ok in ["0", "3600"] {
            assert!(build_changes(&Setting::AutoRefresh, &values(&[ok])).is_ok(), "refresh {ok}");
        }
        for bad in ["3601", "-1", "x"] {
            assert!(build_changes(&Setting::AutoRefresh, &values(&[bad])).is_err(), "refresh {bad}");
        }
        assert!(build_changes(&Setting::Theme, &values(&["nord"])).unwrap_err().contains("solarized-dark"));
        assert_eq!(sets(&build_changes(&Setting::Theme, &values(&["mono"])).unwrap()), ["set ui.theme \"mono\""]);
        assert_eq!(sets(&build_changes(&Setting::Theme, &values(&[""])).unwrap()), ["remove ui.theme"]);
        let cap = Setting::Cap("b2".to_string());
        assert_eq!(sets(&build_changes(&cap, &values(&["10G"])).unwrap()), ["set cloud.caps.b2 \"10G\""]);
        assert_eq!(sets(&build_changes(&cap, &values(&[""])).unwrap()), ["remove cloud.caps.b2"]);
        for bad in ["0", "10GB", "abc"] {
            assert!(build_changes(&cap, &values(&[bad])).is_err(), "cap {bad}");
        }
    }

    #[test]
    fn scrub_ignore_is_split_on_commas_and_trimmed() {
        assert_eq!(sets(&build_changes(&Setting::ScrubIgnore, &values(&[" scratch, tmp ,,"])).unwrap()), ["set dashboard.scrub_ignore [\"scratch\", \"tmp\"]"]);
        assert_eq!(sets(&build_changes(&Setting::ScrubIgnore, &values(&[" , "])).unwrap()), ["remove dashboard.scrub_ignore"]);
    }

    #[test]
    fn describe_names_the_setting_and_the_new_value() {
        assert_eq!(describe(&Setting::Memory, &values(&["85", "95"])), "Memory warn / crit: 85 / 95 %");
        assert_eq!(describe(&Setting::Cap("b2".to_string()), &values(&[""])), "Cap of b2: none");
        assert_eq!(describe(&Setting::Theme, &values(&[""])), "Theme: back to the default");
    }
}
