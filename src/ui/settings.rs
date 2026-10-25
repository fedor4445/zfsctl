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
    DedupRatio,
    Cpu,
    Scrub,
    ScrubIgnore,
    Cap(String),
    ProtLevel,
    ProtCopies,
    ProtOnline,
    ProtDomains,
    ProtIgnore,
    ProtRecent,
    ProtEmail,
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
    /// Why `[protection]` is wrong (`resolve`'s words), shown under its group; the rows stay editable.
    pub protection_problem: Option<String>,
}

const DISPLAY: &str = "Display";
const THRESHOLDS: &str = "Dashboard thresholds";
const CAPS: &str = "Cloud caps";
const PROTECTION: &str = "Data protection";
const PROT_COPIES: &str = "Copies on other machines (1 to 3)";
const PROT_ONLINE: &str = "Online copies (0 to 2)";
const PROT_DOMAINS: &str = "Failure domains (1 to 3)";
const PROT_RECENT: &str = "How recent (strict, normal, relaxed)";
const PROT_EMAIL: &str = "Email (immediate, report, off)";

/// The rows for the file at `config_path`. `remotes` are the rclone remotes the Cloud screen knows; any remote that
/// already has a cap in the file is listed too.
pub fn load(config_path: &Path, remotes: &[String], no_color: bool) -> SettingsView {
    use crate::config::{Config, Thresholds, DEFAULT_AUTO_REFRESH_SECS};
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => return SettingsView { loaded: true, problem: Some(e.to_string()), rows: Vec::new(), protection_problem: None },
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
        row(THRESHOLDS, Setting::DedupRatio, "Dedup warn below", format!("{:.2}x", f64::from(t.dedup_ratio) / 100.0), t.dedup_ratio == d.dedup_ratio, vec![dash.dedup_min_ratio.map_or_else(String::new, |r| format!("{r:.2}"))]),
        row(THRESHOLDS, Setting::Cpu, "CPU warn", format!("{} %", t.cpu_warn), t.cpu_warn == d.cpu_warn, vec![text(dash.cpu_warn.map(u64::from))]),
        row(THRESHOLDS, Setting::Scrub, "Scrub warn / crit", format!("{} / {} days", t.scrub_days.0, t.scrub_days.1), t.scrub_days == d.scrub_days, vec![text(dash.scrub_warn_days.map(u64::from)), text(dash.scrub_crit_days.map(u64::from))]),
        row(THRESHOLDS, Setting::ScrubIgnore, "Scrub ignore", if t.scrub_ignore.is_empty() { "(none)".to_string() } else { t.scrub_ignore.join(", ") }, t.scrub_ignore.is_empty(), vec![t.scrub_ignore.join(", ")]),
    ];
    let p = &cfg.protection;
    // What the file says (a wrong type as its TOML text), or the default; `protection_problem` says when it is wrong.
    let raw = |v: &Option<toml::Value>| crate::config::protection_text(v.as_ref());
    let shown = |v: &Option<toml::Value>, default: &str| raw(v).unwrap_or_else(|| default.to_string());
    let level = shown(&p.level, "basic");
    rows.push(row(PROTECTION, Setting::ProtLevel, "Level", level.clone(), p.level.is_none(), vec![raw(&p.level).unwrap_or_default()]));
    if level.trim().eq_ignore_ascii_case("custom") {
        rows.push(row(PROTECTION, Setting::ProtCopies, PROT_COPIES, shown(&p.copies, "1"), p.copies.is_none(), vec![raw(&p.copies).unwrap_or_default()]));
        rows.push(row(PROTECTION, Setting::ProtOnline, PROT_ONLINE, shown(&p.online, "0"), p.online.is_none(), vec![raw(&p.online).unwrap_or_default()]));
    }
    rows.push(row(PROTECTION, Setting::ProtDomains, PROT_DOMAINS, shown(&p.domains, "1"), p.domains.is_none(), vec![raw(&p.domains).unwrap_or_default()]));
    rows.push(row(PROTECTION, Setting::ProtRecent, PROT_RECENT, shown(&p.recent, "normal"), p.recent.is_none(), vec![raw(&p.recent).unwrap_or_default()]));
    let (ignored, _) = crate::config::parse_ignore(p.ignore.as_ref());
    let ignored: Vec<String> = ignored.into_iter().map(|(h, d)| format!("{h}:{d}")).collect();
    rows.push(row(PROTECTION, Setting::ProtIgnore, "Ignored datasets", if ignored.is_empty() { "(none)".to_string() } else { ignored.join(", ") }, ignored.is_empty(), vec![ignored.join(", ")]));
    rows.push(row(PROTECTION, Setting::ProtEmail, PROT_EMAIL, shown(&p.email, "immediate"), p.email.is_none(), vec![raw(&p.email).unwrap_or_default()]));
    let mut names: Vec<&String> = remotes.iter().chain(cfg.cloud.caps.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let cap = cfg.cloud.caps.get(name);
        rows.push(row(CAPS, Setting::Cap(name.clone()), name, cap.cloned().unwrap_or_else(|| "(no cap)".to_string()), cap.is_none(), vec![cap.cloned().unwrap_or_default()]));
    }
    SettingsView { loaded: true, problem: None, rows, protection_problem: cfg.protection.resolve().err() }
}

/// `(label, current text)` for each field of the form of `row`; the text is empty where the setting is on its default.
pub fn form_fields(row: &Row) -> Vec<(String, String)> {
    let labels: Vec<String> = match row.setting {
        Setting::Theme => vec![format!("Theme ({})", crate::ui::theme::THEME_NAMES.join(", "))],
        Setting::AutoRefresh => vec!["Seconds between refreshes (0 = off)".to_string()],
        Setting::Memory | Setting::Swap | Setting::Quota => vec!["Warn at (%)".to_string(), "Critical at (%)".to_string()],
        Setting::Cpu => vec!["Warn at (%)".to_string()],
        Setting::DedupRatio => vec!["Warn when dedup saves less than (x, 1.00 to 10.00)".to_string()],
        Setting::Scrub => vec!["Warn after (days)".to_string(), "Critical after (days)".to_string()],
        Setting::ScrubIgnore => vec!["Pools to skip (comma-separated)".to_string()],
        Setting::Cap(_) => vec!["Plan size (such as 10G or 5T)".to_string()],
        Setting::ProtLevel => vec!["Level (basic, careful, custom)".to_string()],
        Setting::ProtCopies => vec![PROT_COPIES.to_string()],
        Setting::ProtOnline => vec![PROT_ONLINE.to_string()],
        Setting::ProtDomains => vec![PROT_DOMAINS.to_string()],
        Setting::ProtIgnore => vec!["Leave out (host:dataset, comma-separated; each covers what is below it)".to_string()],
        Setting::ProtRecent => vec![PROT_RECENT.to_string()],
        Setting::ProtEmail => vec![PROT_EMAIL.to_string()],
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

/// One `[protection]` key: empty or the default removes it. The result is checked by `ProtectionConfig::resolve` on the
/// file's current `[protection]` keys plus this edit, so the screen and the config agree and no combination that
/// would make the section invalid can be saved.
fn protection_change(setting: &Setting, value: &str, current: &crate::config::ProtectionConfig) -> Result<Vec<Change>, String> {
    let (key, default) = match setting {
        Setting::ProtLevel => ("level", "basic"),
        Setting::ProtCopies => ("copies", "1"),
        Setting::ProtOnline => ("online", "0"),
        Setting::ProtDomains => ("domains", "1"),
        Setting::ProtRecent => ("recent", "normal"),
        _ => ("email", "immediate"),
    };
    let path = ["protection", key];
    let remove = value.is_empty() || value == default;
    let count = if !remove && matches!(setting, Setting::ProtCopies | Setting::ProtOnline | Setting::ProtDomains) { Some(value.parse::<u8>().map_err(|_| format!("'{value}' is not a whole number"))?) } else { None };
    let text = (!remove).then(|| toml::Value::from(value));
    let number = count.map(toml::Value::from);
    let mut cfg = current.clone();
    match setting {
        Setting::ProtLevel => cfg.level = text,
        Setting::ProtCopies => cfg.copies = number,
        Setting::ProtOnline => cfg.online = number,
        Setting::ProtDomains => cfg.domains = number,
        Setting::ProtRecent => cfg.recent = text,
        _ => cfg.email = text,
    }
    if matches!(setting, Setting::ProtCopies | Setting::ProtOnline) {
        // These keys only count under `custom`, so check them as the config will once they do.
        cfg.level = Some("custom".into());
    }
    cfg.resolve()?;
    Ok(vec![match (remove, count) {
        (true, _) => Change::remove(&path),
        (_, Some(n)) => Change::set(&path, i64::from(n)),
        _ => Change::set(&path, value),
    }])
}

/// `build_changes` against the `[protection]` keys the file at `config_path` has now.
pub fn build_changes_for(config_path: &Path, setting: &Setting, values: &[String]) -> Result<Vec<Change>, String> {
    let current = crate::config::Config::load(config_path).map(|c| c.protection).unwrap_or_default();
    build_changes_with(setting, values, &current)
}

#[cfg(test)]
fn build_changes(setting: &Setting, values: &[String]) -> Result<Vec<Change>, String> {
    build_changes_with(setting, values, &crate::config::ProtectionConfig::default())
}

fn build_changes_with(setting: &Setting, values: &[String], current: &crate::config::ProtectionConfig) -> Result<Vec<Change>, String> {
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
        Setting::DedupRatio if first.is_empty() => vec![Change::remove(&dashboard("dedup_min_ratio"))],
        Setting::DedupRatio => {
            let ratio = first.trim_end_matches('x').parse::<f64>().ok().filter(|r| (1.0..=10.0).contains(r)).ok_or("Dedup warn must be a number from 1.00 to 10.00, such as 1.10")?;
            if (ratio - 1.10).abs() < 1e-9 { vec![Change::remove(&dashboard("dedup_min_ratio"))] } else { vec![Change::set(&dashboard("dedup_min_ratio"), (ratio * 100.0).round() / 100.0)] }
        }
        Setting::Cpu if first.is_empty() => vec![Change::remove(&dashboard("cpu_warn"))],
        Setting::Cpu => {
            let n = number(100, 1, "CPU warn")?;
            if n == 95 { vec![Change::remove(&dashboard("cpu_warn"))] } else { vec![Change::set(&dashboard("cpu_warn"), n as i64)] }
        }
        Setting::ProtIgnore => {
            let items: Vec<&str> = first.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
            let array: toml::Value = toml::Value::Array(items.iter().map(|s| toml::Value::String((*s).to_string())).collect());
            let (entries, problems) = crate::config::parse_ignore(Some(&array));
            if let Some(problem) = problems.into_iter().next() {
                return Err(problem.trim_start_matches("[protection] ignore: ").to_string());
            }
            if entries.is_empty() {
                vec![Change::remove(&["protection", "ignore"])]
            } else {
                vec![Change::set(&["protection", "ignore"], entries.iter().map(|(h, d)| format!("{h}:{d}")).collect::<toml_edit::Array>())]
            }
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
        Setting::ProtLevel | Setting::ProtCopies | Setting::ProtOnline | Setting::ProtDomains | Setting::ProtRecent | Setting::ProtEmail => protection_change(setting, first, current)?,
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
        Setting::DedupRatio => ("Dedup warn below", "x"),
        Setting::Cpu => ("CPU warn", " %"),
        Setting::Scrub => ("Scrub warn / crit", " days"),
        Setting::ScrubIgnore => ("Scrub ignore", ""),
        Setting::ProtLevel => ("Level", ""),
        Setting::ProtCopies => ("Copies on other machines", ""),
        Setting::ProtOnline => ("Online copies", ""),
        Setting::ProtDomains => ("Failure domains", ""),
        Setting::ProtIgnore => ("Ignored datasets", ""),
        Setting::ProtRecent => ("How recent", ""),
        Setting::ProtEmail => ("Email", ""),
        Setting::Cap(name) => return format!("Cap of {name}: {}", if v(0).is_empty() { "none" } else { v(0) }),
    };
    // `1.25x` is accepted when typed; the unit is added once.
    let values: Vec<String> = if matches!(setting, Setting::DedupRatio) { values.iter().map(|v| v.trim().trim_end_matches('x').to_string()).collect() } else { values.to_vec() };
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
        assert_eq!(labels, ["Theme", "Auto-refresh", "Memory warn / crit", "Swap warn / crit", "Quota warn / crit", "Dedup warn below", "CPU warn", "Scrub warn / crit", "Scrub ignore", "Level", "Failure domains (1 to 3)", "How recent (strict, normal, relaxed)", "Ignored datasets", "Email (immediate, report, off)"]);
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
    fn the_dedup_threshold_is_one_number_from_1_to_10_and_the_default_removes_the_key() {
        assert_eq!(sets(&build_changes(&Setting::DedupRatio, &values(&["1.25"])).unwrap()), ["set dashboard.dedup_min_ratio 1.25"]);
        assert_eq!(sets(&build_changes(&Setting::DedupRatio, &values(&["1.10"])).unwrap()), ["remove dashboard.dedup_min_ratio"]);
        assert_eq!(sets(&build_changes(&Setting::DedupRatio, &values(&[""])).unwrap()), ["remove dashboard.dedup_min_ratio"]);
        for bad in ["0.9", "11", "abc", "-1"] {
            assert!(build_changes(&Setting::DedupRatio, &values(&[bad])).is_err(), "{bad}");
        }
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
    fn the_protection_rows_show_the_effective_values() {
        let v = view("", &[], false);
        let prot: Vec<(&str, &str, bool)> = v.rows.iter().filter(|r| r.group == "Data protection").map(|r| (r.label.as_str(), r.value.as_str(), r.is_default)).collect();
        assert_eq!(prot, [("Level", "basic", true), ("Failure domains (1 to 3)", "1", true), ("How recent (strict, normal, relaxed)", "normal", true), ("Ignored datasets", "(none)", true), ("Email (immediate, report, off)", "immediate", true)]);
        assert!(v.rows.iter().all(|r| !r.label.starts_with("Copies") && !r.label.starts_with("Online")));
        let v = view("[protection]\nlevel = \"careful\"\nrecent = \"strict\"\n", &[], false);
        assert_eq!(row(&v, "Level").value, "careful");
        assert!(!row(&v, "How recent (strict, normal, relaxed)").is_default);
    }

    #[test]
    fn a_wrong_protection_value_is_shown_as_written_with_the_problem() {
        let v = view("[protection]\nlevel = \"Careful\"\n", &[], false);
        assert_eq!(v.protection_problem.as_deref(), Some("[protection] level must be basic, careful or custom"));
        assert!(v.problem.is_none(), "the other settings stay editable");
        let level = row(&v, "Level");
        assert_eq!((level.value.as_str(), level.is_default), ("Careful", false));
        let v = view("[protection]\nemail = \"of\"\n", &[], false);
        assert_eq!(v.protection_problem.as_deref(), Some("[protection] email must be immediate, report or off"));
        assert_eq!(row(&v, "Email (immediate, report, off)").value, "of");
        let v = view("[protection]\nlevel = \"custom\"\ncopies = \"2\"\n", &[], false);
        assert_eq!(v.protection_problem.as_deref(), Some("[protection] copies must be 1, 2 or 3"));
        assert_eq!(row(&v, "Copies on other machines (1 to 3)").value, "2", "a string shown as its text");
        assert!(view("[protection]\nlevel = \"careful\"\n", &[], false).protection_problem.is_none());
        // A wrong-case Custom still shows the two rows it is about.
        assert!(view("[protection]\nlevel = \"Custom\"\n", &[], false).rows.iter().any(|r| r.label.starts_with("Copies")));
    }

    #[test]
    fn custom_adds_its_two_rows() {
        let v = view("[protection]\nlevel = \"custom\"\ncopies = 2\n", &[], false);
        let labels: Vec<&str> = v.rows.iter().filter(|r| r.group == "Data protection").map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Level", "Copies on other machines (1 to 3)", "Online copies (0 to 2)", "Failure domains (1 to 3)", "How recent (strict, normal, relaxed)", "Ignored datasets", "Email (immediate, report, off)"]);
        let copies = row(&v, "Copies on other machines (1 to 3)");
        assert_eq!((copies.value.as_str(), copies.is_default, copies.raw.as_slice()), ("2", false, &["2".to_string()][..]));
        let online = row(&v, "Online copies (0 to 2)");
        assert_eq!((online.value.as_str(), online.is_default), ("0", true));
    }

    #[test]
    fn saving_a_level_writes_protection_level_and_a_default_removes_it() {
        assert_eq!(sets(&build_changes(&Setting::ProtLevel, &values(&["careful"])).unwrap()), ["set protection.level \"careful\""]);
        assert_eq!(sets(&build_changes(&Setting::ProtLevel, &values(&["basic"])).unwrap()), ["remove protection.level"]);
        assert_eq!(sets(&build_changes(&Setting::ProtLevel, &values(&[""])).unwrap()), ["remove protection.level"]);
        assert_eq!(sets(&build_changes(&Setting::ProtCopies, &values(&["2"])).unwrap()), ["set protection.copies 2"]);
        assert_eq!(sets(&build_changes(&Setting::ProtCopies, &values(&["1"])).unwrap()), ["remove protection.copies"]);
        let with = |copies: Option<u8>, setting: &Setting, v: &str| build_changes_with(setting, &values(&[v]), &crate::config::ProtectionConfig { level: Some("custom".into()), copies: copies.map(Into::into), ..Default::default() });
        assert_eq!(sets(&with(Some(3), &Setting::ProtOnline, "2").unwrap()), ["set protection.online 2"]);
        assert_eq!(sets(&build_changes(&Setting::ProtRecent, &values(&["strict"])).unwrap()), ["set protection.recent \"strict\""]);
        assert_eq!(sets(&build_changes(&Setting::ProtRecent, &values(&["normal"])).unwrap()), ["remove protection.recent"]);
        assert_eq!(sets(&build_changes(&Setting::ProtEmail, &values(&["report"])).unwrap()), ["set protection.email \"report\""]);
        assert_eq!(sets(&build_changes(&Setting::ProtEmail, &values(&["immediate"])).unwrap()), ["remove protection.email"]);
    }

    #[test]
    fn every_data_protection_row_saves_into_a_real_config_file() {
        // The changes a row produces must survive the config writer's own checks (editable tables, parse, "says what was asked").
        let file = "[ui]\ntheme = \"mono\"\n";
        let current = crate::config::ProtectionConfig { level: Some("custom".into()), copies: Some(3.into()), ..Default::default() };
        let rows = [(Setting::ProtLevel, "custom"), (Setting::ProtCopies, "2"), (Setting::ProtOnline, "1"), (Setting::ProtRecent, "strict"), (Setting::ProtEmail, "report")];
        for (setting, value) in rows {
            let changes = build_changes_with(&setting, &values(&[value]), &current).unwrap();
            let written = crate::config_edit::apply(file, &changes).unwrap_or_else(|e| panic!("{setting:?} = {value}: {e}"));
            assert!(written.contains("[protection]"), "{setting:?}: {written}");
        }
    }

    #[test]
    fn online_and_copies_are_checked_against_each_other_and_the_file() {
        let cur = |copies: Option<u8>, online: Option<u8>| crate::config::ProtectionConfig { level: Some("custom".into()), copies: copies.map(Into::into), online: online.map(Into::into), ..Default::default() };
        let run = |setting: &Setting, v: &str, c: &crate::config::ProtectionConfig| build_changes_with(setting, &values(&[v]), c);
        assert!(run(&Setting::ProtOnline, "2", &cur(None, None)).unwrap_err().contains("online must not be more than copies"));
        assert!(run(&Setting::ProtOnline, "1", &cur(None, None)).is_ok(), "one online of one copy is fine");
        assert!(run(&Setting::ProtCopies, "1", &cur(Some(2), Some(2))).is_err());
        assert!(run(&Setting::ProtCopies, "", &cur(Some(2), Some(2))).is_err());
        assert_eq!(sets(&run(&Setting::ProtOnline, "2", &cur(Some(3), None)).unwrap()), ["set protection.online 2"]);
        assert_eq!(sets(&run(&Setting::ProtCopies, "3", &cur(Some(2), Some(2))).unwrap()), ["set protection.copies 3"]);
        // Level other than custom ignores copies and online, so they cannot break it.
        let basic = crate::config::ProtectionConfig { copies: Some(1.into()), online: Some(2.into()), ..Default::default() };
        assert!(run(&Setting::ProtEmail, "report", &basic).is_ok());
    }

    #[test]
    fn a_wrong_value_is_refused_with_the_configs_words() {
        assert_eq!(build_changes(&Setting::ProtLevel, &values(&["paranoid"])).unwrap_err(), "[protection] level must be basic, careful or custom");
        assert_eq!(build_changes(&Setting::ProtRecent, &values(&["often"])).unwrap_err(), "[protection] recent must be strict, normal or relaxed");
        assert_eq!(build_changes(&Setting::ProtEmail, &values(&["daily"])).unwrap_err(), "[protection] email must be immediate, report or off");
        assert_eq!(build_changes(&Setting::ProtCopies, &values(&["4"])).unwrap_err(), "[protection] copies must be 1, 2 or 3");
        assert_eq!(build_changes(&Setting::ProtOnline, &values(&["3"])).unwrap_err(), "[protection] online must be 0, 1 or 2");
        assert!(build_changes(&Setting::ProtCopies, &values(&["many"])).is_err());
    }

    #[test]
    fn describe_names_the_setting_and_the_new_value() {
        assert_eq!(describe(&Setting::Memory, &values(&["85", "95"])), "Memory warn / crit: 85 / 95 %");
        assert_eq!(describe(&Setting::Cap("b2".to_string()), &values(&[""])), "Cap of b2: none");
        assert_eq!(describe(&Setting::Theme, &values(&[""])), "Theme: back to the default");
        assert_eq!(describe(&Setting::DedupRatio, &values(&["1.25x"])), "Dedup warn below: 1.25x", "the unit is not doubled");
    }

    #[test]
    fn domains_is_a_row_for_every_level_and_is_one_to_three() {
        let v = view("", &[], false);
        let row = v.rows.iter().find(|r| r.setting == Setting::ProtDomains).expect("a Domains row without custom level");
        assert_eq!((row.value.as_str(), row.is_default), ("1", true));
        assert_eq!(sets(&build_changes(&Setting::ProtDomains, &values(&["2"])).unwrap()), ["set protection.domains 2"]);
        assert_eq!(sets(&build_changes(&Setting::ProtDomains, &values(&["1"])).unwrap()), ["remove protection.domains"], "the default is removed, not written");
        assert_eq!(build_changes(&Setting::ProtDomains, &values(&["4"])).unwrap_err(), "[protection] domains must be 1, 2 or 3");
        assert!(build_changes(&Setting::ProtDomains, &values(&["x"])).is_err());
        assert_eq!(describe(&Setting::ProtDomains, &values(&["2"])), "Failure domains: 2");
    }

    #[test]
    fn ignored_datasets_are_a_comma_list_of_host_and_dataset_and_empty_removes_the_key() {
        assert_eq!(sets(&build_changes(&Setting::ProtIgnore, &values(&[" local:zroot/tmp , fbsd16:scratch ,,"])).unwrap()), ["set protection.ignore [\"local:zroot/tmp\", \"fbsd16:scratch\"]"]);
        assert_eq!(sets(&build_changes(&Setting::ProtIgnore, &values(&[" , "])).unwrap()), ["remove protection.ignore"]);
        assert!(build_changes(&Setting::ProtIgnore, &values(&["tank/tmp"])).unwrap_err().contains("'tank/tmp' is not host:dataset"));
        let v = view("[protection]\nignore = [\"local:a\", \"b:c/d\"]\n", &[], false);
        let row = v.rows.iter().find(|r| r.setting == Setting::ProtIgnore).unwrap();
        assert_eq!((row.value.as_str(), row.is_default, row.raw.as_slice()), ("local:a, b:c/d", false, &["local:a, b:c/d".to_string()][..]));
    }
}
