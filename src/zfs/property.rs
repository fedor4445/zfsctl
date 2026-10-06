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

    /// False only for `-` — the source `zpool`/`zfs get` reports for
    /// computed, read-only properties (`size`, `used`, `guid`,
    /// `creation`, `health`, ...), which have no settable concept at
    /// all and always fail `set` with "is readonly". Every other
    /// source (`local`, `default`, `inherited from <dataset>`, and the
    /// other `Other` values like `temporary`/`received`) names a
    /// property that genuinely can be set — `inherited` in particular
    /// just means "not overridden here yet," not "can't be."
    pub fn is_editable(&self) -> bool {
        !matches!(self, PropertySource::Other(s) if s == "-")
    }
}

/// A one-line synopsis of a `zpool`/`zfs` property, for display in the
/// edit popup — purely a UX nicety, not derived from `get`/`set`
/// output (neither exposes descriptions). Covers the properties most
/// commonly seen in `zpool get all`/`zfs get all` output; returns
/// `None` for anything not in this table rather than guessing.
pub fn describe(name: &str) -> Option<&'static str> {
    match name {
        // zfs dataset/volume properties
        "available" => Some("Space available to this dataset and its children, not including reservations"),
        "atime" => Some("Whether access time is updated on read"),
        "relatime" => Some("Whether access time updates are relaxed, only updated periodically instead of on every read"),
        "canmount" => Some("Whether this dataset can be mounted, and whether 'zfs mount -a' mounts it automatically"),
        "casesensitivity" => Some("Whether file/directory name lookups are case-sensitive (fixed at creation)"),
        "checksum" => Some("Checksum algorithm used to verify data integrity"),
        "compression" => Some("Compression algorithm applied to data written to this dataset (e.g. off, lz4, zstd, gzip)"),
        "compressratio" => Some("Compression ratio achieved for this dataset's data"),
        "copies" => Some("Number of copies of user data to store per block, for extra redundancy without a redundant vdev"),
        "creation" => Some("Time this dataset was created"),
        "dedup" => Some("Whether deduplication is enabled for blocks written to this dataset"),
        "devices" => Some("Whether device nodes can be opened on this dataset"),
        "encryption" => Some("Encryption algorithm used for this dataset, if any"),
        "exec" => Some("Whether processes can be executed from files in this dataset"),
        "filesystem_count" => Some("Number of filesystems/volumes currently under this dataset (with a filesystem_limit set)"),
        "filesystem_limit" => Some("Maximum number of filesystems/volumes that can exist under this dataset"),
        "guid" => Some("Unique identifier for this dataset/pool"),
        "logbias" => Some("Hint for how to handle synchronous writes: 'latency' (default) or 'throughput'"),
        "mounted" => Some("Whether this dataset is currently mounted"),
        "mountpoint" => Some("Filesystem path where this dataset is mounted"),
        "nbmand" => Some("Whether to use non-blocking mandatory locks (relevant for CIFS/SMB clients)"),
        "normalization" => Some("Unicode normalization form applied to file/directory names (fixed at creation)"),
        "origin" => Some("The snapshot this dataset was cloned from, if any"),
        "overlay" => Some("Whether a non-empty directory can be used as a mountpoint"),
        "primarycache" => Some("What kind of data ('all', 'none', or 'metadata') is cached in the ARC (RAM cache)"),
        "quota" => Some("Maximum amount of space this dataset and its descendants can consume"),
        "readonly" => Some("Whether this dataset (or pool) can be written to"),
        "recordsize" => Some("Suggested block size for files in this dataset"),
        "redundant_metadata" => Some("How much extra redundant metadata is stored: 'all' or 'most'"),
        "refquota" => Some("Maximum amount of space this dataset itself can consume, not counting descendants"),
        "refreservation" => Some("Minimum amount of space guaranteed to this dataset itself, not counting descendants"),
        "reservation" => Some("Minimum amount of space guaranteed to this dataset and its descendants"),
        "secondarycache" => Some("What kind of data ('all', 'none', or 'metadata') is cached on an L2ARC device"),
        "setuid" => Some("Whether the setuid/setgid bits are honored on this dataset"),
        "sharenfs" => Some("Whether and how this dataset is shared via NFS"),
        "sharesmb" => Some("Whether and how this dataset is shared via SMB"),
        "snapdir" => Some("Whether the hidden .zfs/snapshot directory is visible"),
        "snapshot_count" => Some("Number of snapshots currently under this dataset (with a snapshot_limit set)"),
        "snapshot_limit" => Some("Maximum number of snapshots that can exist under this dataset"),
        "special_small_blocks" => Some("Size below which blocks are written to a 'special' allocation-class vdev instead"),
        "sync" => Some("Behavior for synchronous requests: 'standard', 'always', or 'disabled'"),
        "type" => Some("Whether this dataset is a filesystem, volume, or snapshot"),
        "used" => Some("Space consumed by this dataset and all its descendants"),
        "utf8only" => Some("Whether only valid UTF-8 file/directory names are accepted (fixed at creation)"),
        "version" => Some("On-disk format version of this dataset/pool"),
        "volblocksize" => Some("Block size for a volume (zvol), fixed at creation"),
        "volsize" => Some("Logical size of a volume (zvol)"),
        "xattr" => Some("How extended attributes are stored: 'on', 'off', or 'sa' (in a hidden directory)"),
        "zoned" => Some("Whether this dataset is managed from a non-global jail/zone"),
        // zpool properties
        "allocated" => Some("Space currently allocated in the pool"),
        "altroot" => Some("Alternate root directory the pool is mounted under, if any"),
        "ashift" => Some("Alignment shift (block size, as a power of two) used for this pool's vdevs"),
        "autoexpand" => Some("Whether the pool automatically expands to use the full size of enlarged/replaced devices"),
        "autoreplace" => Some("Whether ZFS automatically replaces a failed device with an available spare or matching device"),
        "autotrim" => Some("Whether ZFS automatically issues TRIM/UNMAP for freed space on devices that support it"),
        "bootfs" => Some("Dataset used as the default boot filesystem for this pool"),
        "cachefile" => Some("Location of the cache file used to import this pool automatically at boot"),
        "capacity" => Some("Percentage of pool space currently used"),
        "comment" => Some("A free-form text comment attached to this pool"),
        "dedupratio" => Some("Deduplication ratio achieved across the whole pool"),
        "delegation" => Some("Whether delegated administration ('zfs allow') is enabled for this pool"),
        "expandsize" => Some("Amount of space available for the pool to expand into"),
        "failmode" => Some("Pool behavior when all redundancy for a device is lost: 'wait', 'continue', or 'panic'"),
        "fragmentation" => Some("Estimated fragmentation of the pool's free space"),
        "free" => Some("Unallocated space in the pool"),
        "freeing" => Some("Space still being reclaimed from recently destroyed datasets"),
        "health" => Some("Current health state of the pool"),
        "leaked" => Some("Space leaked due to a bug, since fixed, that hasn't been reclaimed"),
        "listsnapshots" => Some("Whether 'zfs list' shows snapshots by default for this pool"),
        "multihost" => Some("Whether multihost protection (MMP) is enabled, to detect concurrent import by another host"),
        "size" => Some("Total raw capacity of the pool"),
        _ => None,
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
            let &[_, name, value, source] = fields.as_slice() else {
                return Err(AppError::Parse(format!(
                    "expected 4 fields in get-all property line: {line:?}"
                )));
            };
            Ok(Property {
                name: name.to_string(),
                value: value.to_string(),
                source: PropertySource::parse(source),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_known_dataset_and_pool_properties() {
        assert_eq!(describe("compression"), Some("Compression algorithm applied to data written to this dataset (e.g. off, lz4, zstd, gzip)"));
        assert_eq!(describe("autoexpand"), Some("Whether the pool automatically expands to use the full size of enlarged/replaced devices"));
    }

    #[test]
    fn describe_unknown_property_returns_none() {
        assert_eq!(describe("some_future_property_not_in_the_table"), None);
    }

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
    fn is_editable_false_only_for_dash_source() {
        assert!(!PropertySource::Other("-".to_string()).is_editable());
        assert!(PropertySource::Local.is_editable());
        assert!(PropertySource::Default.is_editable());
        assert!(PropertySource::Inherited("tank".to_string()).is_editable());
        assert!(PropertySource::Other("temporary".to_string()).is_editable());
        assert!(PropertySource::Other("received".to_string()).is_editable());
    }

    #[test]
    fn property_source_display_renders_clean_text_not_debug_syntax() {
        assert_eq!(PropertySource::Local.to_string(), "local");
        assert_eq!(PropertySource::Default.to_string(), "default");
        assert_eq!(PropertySource::Inherited("tank".to_string()).to_string(), "inherited from tank");
        assert_eq!(PropertySource::Other("-".to_string()).to_string(), "-");
    }
}
