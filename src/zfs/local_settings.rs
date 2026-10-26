//! What was set by hand: every property whose source is `local`, on every dataset of a host (`zfs get -r -s local all`).

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSetting {
    pub dataset: String,
    pub property: String,
    pub value: String,
}

/// One read-only `zfs get` for the whole host, sorted by dataset and then property.
pub fn list_local(runner: &dyn CommandRunner) -> Result<Vec<LocalSetting>> {
    let output = super::run_checked(runner, "zfs", &["get", "-r", "-s", "local", "-H", "-p", "all"], "zfs get -r -s local all")?;
    parse_local(&output.stdout)
}

/// `name  property  value  source` lines. A line that is not four tab-separated fields is an error, as in `parse_property_list`:
/// a half-read list must not pass for "nothing was set by hand".
pub fn parse_local(output: &str) -> Result<Vec<LocalSetting>> {
    let mut settings = output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[dataset, property, value, _source] = fields.as_slice() else {
                return Err(AppError::Parse(format!("expected 4 fields in get property line: {line:?}")));
            };
            Ok(LocalSetting { dataset: dataset.to_string(), property: property.to_string(), value: value.to_string() })
        })
        .collect::<Result<Vec<_>>>()?;
    settings.sort_by(|a, b| (&a.dataset, &a.property).cmp(&(&b.dataset, &b.property)));
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    #[test]
    fn lines_are_read_and_sorted_by_dataset_then_property_and_a_value_may_hold_spaces() {
        let out = "tank/b\tatime\toff\tlocal\ntank\tmountpoint\t/my tank\tlocal\ntank/b\tcompression\tlz4\tlocal\n\n";
        let got = parse_local(out).unwrap();
        let names: Vec<(&str, &str, &str)> = got.iter().map(|s| (s.dataset.as_str(), s.property.as_str(), s.value.as_str())).collect();
        assert_eq!(names, [("tank", "mountpoint", "/my tank"), ("tank/b", "atime", "off"), ("tank/b", "compression", "lz4")]);
    }

    #[test]
    fn nothing_set_by_hand_is_an_empty_list_and_a_broken_line_is_an_error() {
        assert_eq!(parse_local("").unwrap(), []);
        assert!(parse_local("tank\tatime\toff\n").is_err());
    }

    #[test]
    fn the_read_asks_for_local_sources_only() {
        let runner = FakeRunner::new("tank\tatime\toff\tlocal\n");
        assert_eq!(list_local(&runner).unwrap().len(), 1);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, ["get", "-r", "-s", "local", "-H", "-p", "all"].map(String::from));
    }
}
