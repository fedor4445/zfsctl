use crate::error::{AppError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: String,
    pub value: String,
    pub source: PropertySource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertySource {
    Local,
    Default,
    /// The ancestor dataset this property is inherited from (only
    /// meaningful for `zfs` properties — `zpool` properties never
    /// report this source).
    Inherited(String),
    /// Any other source string `zpool`/`zfs get` reports verbatim —
    /// most commonly `-` (read-only/computed properties like `used`
    /// or `creation`), but also `temporary` and `received`.
    Other(String),
}

impl std::fmt::Display for PropertySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropertySource::Local => write!(f, "local"),
            PropertySource::Default => write!(f, "default"),
            PropertySource::Inherited(dataset) => write!(f, "inherited from {dataset}"),
            PropertySource::Other(s) => write!(f, "{s}"),
        }
    }
}

impl PropertySource {
    fn parse(s: &str) -> PropertySource {
        if let Some(dataset) = s.strip_prefix("inherited from ") {
            return PropertySource::Inherited(dataset.to_string());
        }
        match s {
            "local" => PropertySource::Local,
            "default" => PropertySource::Default,
            other => PropertySource::Other(other.to_string()),
        }
    }
}

/// Parses `zpool get all -H -p <pool>` or `zfs get all -H -p <dataset>`
/// output — both emit the identical tab-separated
/// `name  property  value  source` shape, one property per line. The
/// leading `name` column is ignored (both call sites already know which
/// pool/dataset they asked about).
pub fn parse_property_list(output: &str) -> Result<Vec<Property>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 4 {
                return Err(AppError::Parse(format!(
                    "expected 4 fields in get-all property line: {line:?}"
                )));
            }
            Ok(Property {
                name: fields[1].to_string(),
                value: fields[2].to_string(),
                source: PropertySource::parse(fields[3]),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZPOOL_GET_FIXTURE: &str = "\
tank\tsize\t4000000000000\t-
tank\tcapacity\t23\t-
tank\tautoexpand\toff\tdefault
tank\tcomment\t-\tdefault
tank\tautoreplace\ton\tlocal
";

    const ZFS_GET_FIXTURE: &str = "\
tank/data\ttype\tfilesystem\t-
tank/data\tused\t1234567\t-
tank/data\tcompression\tlz4\tlocal
tank/data\tquota\t0\tdefault
tank/data\treadonly\toff\tinherited from tank
";

    #[test]
    fn parses_zpool_get_fixture() {
        let props = parse_property_list(ZPOOL_GET_FIXTURE).unwrap();
        assert_eq!(
            props,
            vec![
                Property { name: "size".to_string(), value: "4000000000000".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "capacity".to_string(), value: "23".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "autoexpand".to_string(), value: "off".to_string(), source: PropertySource::Default },
                Property { name: "comment".to_string(), value: "-".to_string(), source: PropertySource::Default },
                Property { name: "autoreplace".to_string(), value: "on".to_string(), source: PropertySource::Local },
            ]
        );
    }

    #[test]
    fn parses_zfs_get_fixture_including_inherited_source() {
        let props = parse_property_list(ZFS_GET_FIXTURE).unwrap();
        assert_eq!(
            props,
            vec![
                Property { name: "type".to_string(), value: "filesystem".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "used".to_string(), value: "1234567".to_string(), source: PropertySource::Other("-".to_string()) },
                Property { name: "compression".to_string(), value: "lz4".to_string(), source: PropertySource::Local },
                Property { name: "quota".to_string(), value: "0".to_string(), source: PropertySource::Default },
                Property { name: "readonly".to_string(), value: "off".to_string(), source: PropertySource::Inherited("tank".to_string()) },
            ]
        );
    }

    #[test]
    fn rejects_malformed_property_line() {
        let result = parse_property_list("tank\tonly-two-fields\n");
        assert!(result.is_err());
    }

    #[test]
    fn property_source_display_renders_clean_text_not_debug_syntax() {
        assert_eq!(PropertySource::Local.to_string(), "local");
        assert_eq!(PropertySource::Default.to_string(), "default");
        assert_eq!(PropertySource::Inherited("tank".to_string()).to_string(), "inherited from tank");
        assert_eq!(PropertySource::Other("-".to_string()).to_string(), "-");
    }
}
