//! What a pool is made of and holds, for the Pools panel: its vdevs and disks (read from the `zpool status`
//! the sampler already fetches), and the number of datasets and snapshots (one slower `zfs list`, counted and
//! never kept per item).

use super::run_checked;
use crate::error::Result;
use crate::exec::CommandRunner;

/// The pool's vdevs and disks by kind and class.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PoolLayout {
    /// Top-level vdevs in the data, special, dedup and log classes.
    pub vdevs: usize,
    /// The vdevs by kind (`mirror`, `raidz1`, `draid2`, `disk` for a single disk), in order of first appearance.
    pub kinds: Vec<(String, usize)>,
    /// Every physical disk, including cache and spare devices.
    pub disks: usize,
    /// The disks by class (`data`, `log`, `cache`, `spare`, `special`, `dedup`), in the order ZFS lists them.
    pub classes: Vec<(&'static str, usize)>,
}

fn bump<K: PartialEq>(list: &mut Vec<(K, usize)>, key: K) {
    match list.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => *n += 1,
        None => list.push((key, 1)),
    }
}

/// `mirror-0`, `raidz2-1`, `draid2:4d:8c:1s-0` -> `mirror`, `raidz2`, `draid2`; `None` for anything else.
fn group_kind(name: &str) -> Option<String> {
    let (head, tail) = name.rsplit_once('-')?;
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let kind = head.split(':').next().unwrap_or(head);
    (kind == "mirror" || kind.starts_with("raidz") || kind.starts_with("draid")).then(|| kind.to_string())
}

/// Reads the config tree of `zpool status <pool>`.
pub fn parse_pool_layout(status: &str) -> PoolLayout {
    let lines = super::pool::parse_pool_status_vdevs(status);
    let mut layout = PoolLayout::default();
    let mut class: &'static str = "data";
    for (i, line) in lines.iter().enumerate() {
        match (line.depth, line.name.as_str()) {
            (0, "logs") => class = "log",
            (0, "cache") => class = "cache",
            (0, "spares") => class = "spare",
            (0, "special") => class = "special",
            (0, "dedup") => class = "dedup",
            // The pool's own line.
            (0, _) => {}
            (depth, name) => {
                let is_leaf = lines.get(i + 1).is_none_or(|next| next.depth <= depth);
                if depth == 1 && class != "cache" && class != "spare" {
                    layout.vdevs += 1;
                    bump(&mut layout.kinds, group_kind(name).unwrap_or_else(|| "disk".to_string()));
                }
                if is_leaf {
                    layout.disks += 1;
                    bump(&mut layout.classes, class);
                }
            }
        }
    }
    layout
}

/// How many datasets and snapshots a pool holds, and two cheap settings of its root dataset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PoolContents {
    pub filesystems: usize,
    pub volumes: usize,
    pub snapshots: usize,
    pub compression: Option<String>,
    pub compress_ratio: Option<f64>,
    pub autotrim: Option<bool>,
}

/// Counts the lines of `zfs list -H -t filesystem,volume,snapshot -o type`.
pub fn parse_pool_counts(output: &str) -> (usize, usize, usize) {
    let (mut filesystems, mut volumes, mut snapshots) = (0, 0, 0);
    for line in output.lines() {
        match line.trim() {
            "filesystem" => filesystems += 1,
            "volume" => volumes += 1,
            "snapshot" => snapshots += 1,
            _ => {}
        }
    }
    (filesystems, volumes, snapshots)
}

/// Counts the pool's datasets and snapshots (one `zfs list` that asks only for the type column, so a pool with
/// many snapshots costs time, not memory) and reads the root's compression and the pool's `autotrim`. The two
/// settings are best effort: a failure leaves them unset.
pub fn fetch_pool_contents(runner: &dyn CommandRunner, pool: &str) -> Result<PoolContents> {
    let listed = run_checked(runner, "zfs", &["list", "-H", "-t", "filesystem,volume,snapshot", "-o", "type", "-r", pool], &format!("zfs list {pool}"))?;
    let (filesystems, volumes, snapshots) = parse_pool_counts(&listed.stdout);
    let mut contents = PoolContents { filesystems, volumes, snapshots, ..PoolContents::default() };
    if let Ok(out) = runner.run("zfs", &["get", "-Hp", "-o", "property,value", "compression,compressratio", pool]) {
        if out.success() {
            for line in out.stdout.lines() {
                match line.split_once('\t') {
                    Some(("compression", v)) if v != "-" => contents.compression = Some(v.trim().to_string()),
                    Some(("compressratio", v)) => contents.compress_ratio = v.trim().trim_end_matches('x').parse().ok(),
                    _ => {}
                }
            }
        }
    }
    if let Ok(out) = runner.run("zpool", &["get", "-Hp", "-o", "value", "autotrim", pool]) {
        if out.success() {
            contents.autotrim = match out.stdout.trim() {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            };
        }
    }
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const STATUS: &str = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME          STATE     READ WRITE CKSUM\n\ttank          ONLINE       0     0     0\n\t  mirror-0    ONLINE       0     0     0\n\t    da0       ONLINE       0     0     0\n\t    da1       ONLINE       0     0     0\n\t  raidz1-1    ONLINE       0     0     0\n\t    da2       ONLINE       0     0     0\n\t    da3       ONLINE       0     0     0\n\t    da4       ONLINE       0     0     0\n\t  da5         ONLINE       0     0     0\n\tlogs\n\t  mirror-2    ONLINE       0     0     0\n\t    da6       ONLINE       0     0     0\n\t    da7       ONLINE       0     0     0\n\tcache\n\t  da8         ONLINE       0     0     0\n\tspares\n\t  da9         AVAIL\n\nerrors: No known data errors\n";

    #[test]
    fn vdevs_and_disks_are_counted_by_kind_and_class() {
        let l = parse_pool_layout(STATUS);
        assert_eq!(l.vdevs, 4, "mirror, raidz1, single disk, log mirror; cache and spare are not vdevs");
        assert_eq!(l.kinds, [("mirror".to_string(), 2), ("raidz1".to_string(), 1), ("disk".to_string(), 1)]);
        assert_eq!(l.disks, 10);
        assert_eq!(l.classes, [("data", 6), ("log", 2), ("cache", 1), ("spare", 1)]);
    }

    #[test]
    fn a_single_disk_pool_and_an_empty_status_are_handled() {
        let one = "  pool: t\n state: ONLINE\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tt     ONLINE  0 0 0\n\t  da0 ONLINE  0 0 0\n\nerrors: No known data errors\n";
        let l = parse_pool_layout(one);
        assert_eq!((l.vdevs, l.disks), (1, 1));
        assert_eq!(l.kinds, [("disk".to_string(), 1)]);
        assert_eq!(parse_pool_layout(""), PoolLayout::default());
    }

    #[test]
    fn special_and_dedup_vdevs_and_a_replacing_node_are_counted() {
        let s = "  pool: t\n state: DEGRADED\nconfig:\n\n\tNAME  STATE  READ WRITE CKSUM\n\tt     DEGRADED  0 0 0\n\t  mirror-0  DEGRADED  0 0 0\n\t    replacing-0  DEGRADED  0 0 0\n\t      da0  ONLINE  0 0 0\n\t      da9  ONLINE  0 0 0\n\t    da1  ONLINE  0 0 0\n\tspecial\n\t  mirror-1  ONLINE  0 0 0\n\t    da2  ONLINE  0 0 0\n\t    da3  ONLINE  0 0 0\n\tdedup\n\t  mirror-2  ONLINE  0 0 0\n\t    da4  ONLINE  0 0 0\n\t    da5  ONLINE  0 0 0\n";
        let l = parse_pool_layout(s);
        assert_eq!(l.vdevs, 3);
        assert_eq!(l.disks, 7, "da0, da9 (mid-replace), da1, da2..da5");
        assert_eq!(l.classes, [("data", 3), ("special", 2), ("dedup", 2)]);
    }

    #[test]
    fn group_names_are_recognised() {
        assert_eq!(group_kind("mirror-0").as_deref(), Some("mirror"));
        assert_eq!(group_kind("raidz2-3").as_deref(), Some("raidz2"));
        assert_eq!(group_kind("draid2:4d:8c:1s-0").as_deref(), Some("draid2"));
        assert_eq!(group_kind("da0"), None);
        assert_eq!(group_kind("gpt-1"), None, "a disk label that ends in a number is not a group");
        assert_eq!(group_kind("mirror-"), None);
    }

    #[test]
    fn the_listing_is_only_counted() {
        assert_eq!(parse_pool_counts("filesystem\nfilesystem\nvolume\nsnapshot\nsnapshot\nsnapshot\n\nbookmark\n"), (2, 1, 3));
        assert_eq!(parse_pool_counts(""), (0, 0, 0));
    }

    #[test]
    fn fetching_asks_only_for_the_type_column_and_reads_the_settings() {
        let runner = FakeRunner::new("")
            .route("zfs", "list", "filesystem\nfilesystem\nsnapshot\nsnapshot\nsnapshot\n")
            .route("zfs", "get", "compression\tlz4\ncompressratio\t1.43x\n")
            .route("zpool", "get", "on\n");
        let c = fetch_pool_contents(&runner, "tank").unwrap();
        assert_eq!((c.filesystems, c.volumes, c.snapshots), (2, 0, 3));
        assert_eq!((c.compression.as_deref(), c.compress_ratio, c.autotrim), (Some("lz4"), Some(1.43), Some(true)));
        let calls = runner.calls.lock().unwrap().clone();
        assert_eq!(calls[0].1, ["list", "-H", "-t", "filesystem,volume,snapshot", "-o", "type", "-r", "tank"]);
    }

    #[test]
    fn a_failing_listing_is_an_error_but_failing_settings_are_not() {
        let runner = FakeRunner::new("").route_failure("zfs", "list", "no such pool", 1);
        assert!(fetch_pool_contents(&runner, "tank").is_err());
        let runner = FakeRunner::new("").route("zfs", "list", "filesystem\n").route_failure("zfs", "get", "bad", 1).route_failure("zpool", "get", "bad", 1);
        let c = fetch_pool_contents(&runner, "tank").unwrap();
        assert_eq!((c.filesystems, c.compression, c.compress_ratio, c.autotrim), (1, None, None, None));
    }
}
