//! What a pool's special, dedup, log and cache devices look like, read from `zpool status`: the facts the Dashboard needs to
//! warn about a special or dedup vdev with no redundancy (losing it loses the pool) and about a faulted log or cache device.

use super::pool::{parse_pool_status_vdevs, PoolHealth};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolClasses {
    pub name: String,
    /// `special` and/or `dedup` when that class has a top-level vdev that is a lone disk (not a mirror or raidz).
    #[serde(default)]
    pub unprotected: Vec<String>,
    /// A log device is FAULTED or UNAVAIL (the pool keeps working, but synchronous writes lose their fast path).
    #[serde(default)]
    pub log_bad: bool,
    /// A cache device is FAULTED, UNAVAIL or REMOVED.
    #[serde(default)]
    pub cache_bad: bool,
    /// The disks that are not plain data (special, dedup, log, cache, spare), so a disk's own problem can say what it is for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<DeviceRole>,
}

/// A leaf disk of a non-data class, as `zpool status` names it (`da5`, `ada4p1`, `sdb1`, `gpt/slog0`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceRole {
    pub role: String,
    pub device: String,
}

impl PoolClasses {
    /// Nothing to say about this pool.
    pub fn is_quiet(&self) -> bool {
        self.unprotected.is_empty() && !self.log_bad && !self.cache_bad
    }
}

/// Reads every pool of `zpool status` output (split at its `pool:` lines).
pub fn parse_classes(status_all: &str) -> Vec<PoolClasses> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in status_all.lines() {
        if let Some(name) = line.trim_start().strip_prefix("pool:") {
            blocks.push((name.trim().to_string(), vec![line]));
        } else if let Some((_, lines)) = blocks.last_mut() {
            lines.push(line);
        }
    }
    blocks.into_iter().map(|(name, lines)| classes_of(name, &lines.join("\n"))).collect()
}

fn classes_of(name: String, block: &str) -> PoolClasses {
    let lines = parse_pool_status_vdevs(block);
    let mut out = PoolClasses { name, ..PoolClasses::default() };
    let mut class = "data";
    let pool_line = lines.iter().position(|l| l.depth == 0);
    for (i, line) in lines.iter().enumerate() {
        // The first depth-0 line is the pool itself: a pool may be called `special` or `cache`.
        if line.depth == 0 && Some(i) == pool_line {
            continue;
        }
        if line.depth == 0 {
            class = match line.name.as_str() {
                "logs" => "log",
                "cache" => "cache",
                "spares" => "spare",
                "special" => "special",
                "dedup" => "dedup",
                _ => "data",
            };
            continue;
        }
        let down = |h: &PoolHealth, bad: &[PoolHealth]| bad.contains(h);
        let is_leaf = !lines.get(i + 1).is_some_and(|next| next.depth > line.depth);
        if class != "data" && is_leaf {
            out.roles.push(DeviceRole { role: class.to_string(), device: line.name.clone() });
        }
        match class {
            "special" | "dedup" if line.depth == 1 => {
                let has_children = lines.get(i + 1).is_some_and(|next| next.depth > line.depth);
                if !has_children && !out.unprotected.iter().any(|c| c == class) {
                    out.unprotected.push(class.to_string());
                }
            }
            "log" if down(&line.health, &[PoolHealth::Faulted, PoolHealth::Unavail]) => out.log_bad = true,
            "cache" if down(&line.health, &[PoolHealth::Faulted, PoolHealth::Unavail, PoolHealth::Removed]) => out.cache_bad = true,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_POOLS: &str = "  pool: hybrid\n state: ONLINE\nconfig:\n\n\tNAME          STATE     READ WRITE CKSUM\n\thybrid        ONLINE       0     0     0\n\t  raidz1-0    ONLINE       0     0     0\n\t    da0       ONLINE       0     0     0\n\t    da1       ONLINE       0     0     0\n\t    da2       ONLINE       0     0     0\n\tspecial\n\t  mirror-1    ONLINE       0     0     0\n\t    da3       ONLINE       0     0     0\n\t    da4       ONLINE       0     0     0\n\tlogs\n\t  da5         FAULTED      0     0     0  too many errors\n\tcache\n\t  da6         ONLINE       0     0     0\n\nerrors: No known data errors\n\n  pool: fragile\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\tfragile     ONLINE       0     0     0\n\t  da7       ONLINE       0     0     0\n\tdedup\n\t  da8       ONLINE       0     0     0\n\tspecial\n\t  da9       ONLINE       0     0     0\n\tcache\n\t  da10      UNAVAIL      0     0     0\n\nerrors: No known data errors\n";

    #[test]
    fn a_mirrored_special_is_protected_and_a_faulted_log_is_noticed() {
        let pools = parse_classes(TWO_POOLS);
        assert_eq!(pools.len(), 2);
        let hybrid = &pools[0];
        assert_eq!((hybrid.name.as_str(), hybrid.unprotected.clone(), hybrid.log_bad, hybrid.cache_bad), ("hybrid", vec![], true, false));
        assert!(!hybrid.is_quiet());
    }

    #[test]
    fn a_lone_special_or_dedup_disk_and_an_unavailable_cache_are_noticed() {
        let fragile = &parse_classes(TWO_POOLS)[1];
        assert_eq!(fragile.unprotected, ["dedup", "special"]);
        assert!(fragile.cache_bad && !fragile.log_bad);
    }

    #[test]
    fn the_leaf_disks_of_every_non_data_class_are_listed_with_their_role() {
        let pools = parse_classes(TWO_POOLS);
        let roles = |pool: usize| pools[pool].roles.iter().map(|r| (r.role.as_str(), r.device.as_str())).collect::<Vec<_>>();
        assert_eq!(roles(0), [("special", "da3"), ("special", "da4"), ("log", "da5"), ("cache", "da6")], "data disks and group names are not listed");
        assert_eq!(roles(1), [("dedup", "da8"), ("special", "da9"), ("cache", "da10")]);
        assert!(parse_classes("  pool: t\n state: ONLINE\nconfig:\n\n\tNAME   STATE     READ WRITE CKSUM\n\tt      ONLINE       0     0     0\n\t  da0  ONLINE       0     0     0\n")[0].roles.is_empty());
    }

    #[test]
    fn a_pool_named_special_or_cache_is_not_a_class() {
        for name in ["special", "dedup", "cache", "logs"] {
            let status = format!("  pool: {name}\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\t{name}     ONLINE       0     0     0\n\t  da0       ONLINE       0     0     0\n\nerrors: No known data errors\n");
            assert!(parse_classes(&status)[0].is_quiet(), "{name}");
        }
    }

    #[test]
    fn a_pool_without_classes_is_quiet_and_garbage_gives_nothing() {
        let plain = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  mirror-0  ONLINE       0     0     0\n\t    da0     ONLINE       0     0     0\n\t    da1     ONLINE       0     0     0\n\nerrors: No known data errors\n";
        assert!(parse_classes(plain)[0].is_quiet());
        assert!(parse_classes("").is_empty() && parse_classes("zpool: not found").is_empty());
    }

    #[test]
    fn a_special_mirror_being_replaced_is_not_a_lone_disk() {
        let status = "  pool: t\n state: DEGRADED\nconfig:\n\n\tNAME             STATE     READ WRITE CKSUM\n\tt                DEGRADED     0     0     0\n\t  da0            ONLINE       0     0     0\n\tspecial\n\t  replacing-0    DEGRADED     0     0     0\n\t    da1          FAULTED      0     0     0\n\t    da2          ONLINE       0     0     0\n\nerrors: No known data errors\n";
        assert!(parse_classes(status)[0].unprotected.is_empty());
    }

    #[test]
    fn a_disk_called_mirror_something_under_special_is_still_a_lone_disk() {
        let status = "  pool: t\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\tt           ONLINE       0     0     0\n\t  da0       ONLINE       0     0     0\n\tspecial\n\t  mirrorish ONLINE       0     0     0\n\nerrors: No known data errors\n";
        assert_eq!(parse_classes(status)[0].unprotected, ["special"]);
    }
}
