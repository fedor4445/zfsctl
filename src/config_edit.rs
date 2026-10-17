//! Edits to the `[ui]`, `[dashboard]` and `[cloud]` tables of `config.toml` that keep everything else as it is.
//! The Settings screen is the only caller.

use std::path::Path;

/// One change to one key. The path is the key's place in the file, such as `["dashboard", "memory_warn"]`.
#[derive(Debug, Clone)]
pub enum Change {
    Set(Vec<String>, toml_edit::Value),
    Remove(Vec<String>),
}

impl Change {
    pub fn set(path: &[&str], value: impl Into<toml_edit::Value>) -> Change {
        Change::Set(path.iter().map(ToString::to_string).collect(), value.into())
    }

    pub fn remove(path: &[&str]) -> Change {
        Change::Remove(path.iter().map(ToString::to_string).collect())
    }
}

/// Tables of `config.toml` the Settings screen may change; every other part of the file must come out as it went in.
const EDITABLE: [&str; 3] = ["ui", "dashboard", "cloud"];

/// The text of the file with `changes` applied, or why that was refused. Comments and layout survive; a table left
/// empty by a removal goes away. The result is checked: it must parse as a `Config`, hold what was set, and differ
/// from the old text only inside the editable tables.
pub fn apply(text: &str, changes: &[Change]) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, Table, TableLike};
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
    let trailing = doc.trailing().as_str().unwrap_or("").to_string();
    for change in changes {
        let (Change::Set(path, _) | Change::Remove(path)) = change;
        if path.len() < 2 || !path.first().is_some_and(|t| EDITABLE.contains(&t.as_str())) {
            return Err(format!("{} is not a setting this screen may change", path.join(".")));
        }
        match change {
            Change::Set(path, value) => {
                let (key, parents) = path.split_last().expect("checked above");
                let mut table: &mut dyn TableLike = doc.as_table_mut();
                for (depth, name) in parents.iter().enumerate() {
                    let last = depth + 1 == parents.len();
                    if table.get(name).is_none() {
                        let mut new = Table::new();
                        new.set_implicit(!last);
                        table.insert(name, Item::Table(new));
                    }
                    table = table.get_mut(name).and_then(Item::as_table_like_mut).ok_or_else(|| format!("{name} in config.toml is not a table"))?;
                }
                let mut value = value.clone();
                if let Some(Item::Value(old)) = table.get_mut(key) {
                    *value.decor_mut() = old.decor().clone();
                    *old = value;
                } else {
                    table.insert(key, Item::Value(value));
                }
            }
            Change::Remove(path) => {
                remove_path(doc.as_table_mut(), path);
            }
        }
    }
    let mut result = doc.to_string();
    // A table added after the last one is written before the comments that end the file, which would leave them
    // under the new header (an uncommented key would then land in the wrong table): put them back above it.
    if !trailing.is_empty() {
        let (before, after) = (&text[..text.len() - trailing.len().min(text.len())], &result[..result.len().saturating_sub(trailing.len())]);
        if text.ends_with(&trailing) && result.ends_with(&trailing) && after.starts_with(before) && after.len() > before.len() {
            result = format!("{before}{trailing}{}", &after[before.len()..]);
        }
    }
    let new: toml::Table = result.parse().map_err(|e| format!("config.toml would no longer parse: {e}"))?;
    toml::from_str::<crate::config::Config>(&result).map_err(|e| format!("config.toml would no longer parse: {e}"))?;
    let old: toml::Table = text.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
    let rest = |mut t: toml::Table| {
        for name in EDITABLE {
            t.remove(name);
        }
        t
    };
    if rest(old) != rest(new.clone()) {
        return Err("config.toml would change outside the settings: edit it by hand".to_string());
    }
    for change in changes {
        if let Change::Set(path, value) = change {
            let asked: toml::Table = format!("v = {}", value.to_string().trim()).parse().map_err(|e| format!("not a value: {e}"))?;
            let root = toml::Value::Table(new.clone());
            let got = path.iter().try_fold(&root, |v, name| v.get(name));
            if got != asked.get("v") {
                return Err("config.toml would not say what was asked: edit it by hand".to_string());
            }
        }
    }
    Ok(result)
}

/// Removes the key at `path` and any table that this leaves empty. Returns whether the table is empty afterwards.
fn remove_path(table: &mut dyn toml_edit::TableLike, path: &[String]) -> bool {
    match path {
        [] => {}
        [key] => {
            table.remove(key);
        }
        [name, rest @ ..] => {
            let emptied = table.get_mut(name).and_then(toml_edit::Item::as_table_like_mut).is_some_and(|child| remove_path(child, rest));
            if emptied {
                table.remove(name);
            }
        }
    }
    table.is_empty()
}

pub fn save(path: &Path, changes: &[Change]) -> crate::error::Result<()> {
    crate::config::rewrite_config(path, &|text| apply(text, changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const FILE: &str = "# my hub\n[[hosts]]\nname = \"a\"\nssh_target = \"u@a\"\n\n[[hosts]]\nname = \"b\"\nssh_target = \"u@b\"\n\n[notify]\nmail_to = \"me@x.org\" # mine\n";

    fn parsed(text: &str) -> Config {
        toml::from_str(text).unwrap_or_else(|e| panic!("does not parse: {e}\n{text}"))
    }

    #[test]
    fn set_adds_a_missing_table_at_the_end_and_keeps_everything_else() {
        let new = apply(FILE, &[Change::set(&["dashboard", "cpu_warn"], 80)]).unwrap();
        assert!(new.starts_with(FILE), "{new}");
        assert_eq!(parsed(&new).dashboard.cpu_warn, Some(80));
    }

    #[test]
    fn set_replaces_a_key_in_place_and_keeps_its_comment() {
        let old = format!("{FILE}\n[dashboard]\ncpu_warn = 90 # mine\n");
        let new = apply(&old, &[Change::set(&["dashboard", "cpu_warn"], 80)]).unwrap();
        assert!(new.contains("cpu_warn = 80 # mine"), "{new}");
        assert_eq!(parsed(&new).dashboard.cpu_warn, Some(80));
    }

    #[test]
    fn remove_deletes_the_key_and_an_emptied_table_with_it() {
        let old = format!("{FILE}\n[dashboard]\ncpu_warn = 90\n");
        let new = apply(&old, &[Change::remove(&["dashboard", "cpu_warn"])]).unwrap();
        assert!(!new.contains("dashboard"), "{new}");
        assert!(new.contains("mail_to = \"me@x.org\" # mine"), "{new}");
    }

    #[test]
    fn remove_of_a_missing_key_is_a_no_op() {
        assert_eq!(apply(FILE, &[Change::remove(&["dashboard", "cpu_warn"])]).unwrap(), FILE);
    }

    #[test]
    fn a_cap_in_an_inline_table_is_edited() {
        let old = format!("{FILE}\n[cloud]\ncaps = {{ b2 = \"10G\" }}\n");
        let new = apply(&old, &[Change::set(&["cloud", "caps", "s3"], "5G")]).unwrap();
        let caps = parsed(&new).cloud.caps;
        assert_eq!(caps.get("b2").map(String::as_str), Some("10G"));
        assert_eq!(caps.get("s3").map(String::as_str), Some("5G"));
    }

    #[test]
    fn removing_the_last_cap_removes_the_cloud_table_too() {
        let old = format!("{FILE}\n[cloud.caps]\nb2 = \"10G\"\n");
        let new = apply(&old, &[Change::remove(&["cloud", "caps", "b2"])]).unwrap();
        assert!(!new.contains("cloud"), "{new}");
        let added = apply(FILE, &[Change::set(&["cloud", "caps", "b2"], "10G")]).unwrap();
        assert!(added.contains("[cloud.caps]"), "{added}");
    }

    #[test]
    fn a_change_that_would_alter_another_table_is_refused() {
        assert!(apply(FILE, &[Change::set(&["notify", "mail_to"], "x@y.org")]).is_err());
        assert!(apply(FILE, &[Change::set(&["hosts"], "x")]).is_err());
    }

    #[test]
    fn a_file_that_does_not_parse_is_refused_with_the_reason() {
        let err = apply("[dashboard\ncpu_warn = 1", &[Change::set(&["dashboard", "cpu_warn"], 80)]).unwrap_err();
        assert!(err.contains("parse"), "{err}");
    }

    #[test]
    fn a_comment_at_the_end_of_the_file_stays_above_a_table_added_after_it() {
        let old = "[daemon]\nnotify_command = \"true\"\n# notify_command = \"/usr/local/bin/hook\"\n";
        let new = apply(old, &[Change::set(&["cloud", "caps", "b2"], "10G")]).unwrap();
        let (comment, header) = (new.find("# notify_command").unwrap(), new.find("[cloud.caps]").unwrap());
        assert!(comment < header, "{new}");
        assert!(new.starts_with(old), "{new}");
        assert_eq!(parsed(&new).cloud.caps.get("b2").map(String::as_str), Some("10G"));
    }

    #[test]
    fn save_keeps_a_bak_copy_and_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, FILE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        save(&path, &[Change::set(&["ui", "auto_refresh_secs"], 5)]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(), FILE);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(parsed(&std::fs::read_to_string(&path).unwrap()).ui.auto_refresh_secs, Some(5));
    }
}
