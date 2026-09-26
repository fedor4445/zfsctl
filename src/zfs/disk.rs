use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::pool::{self, is_leaf_disk, VdevLine};

#[derive(Debug, Clone, PartialEq)]
pub struct DiskInfo {
    pub name: String,
    pub size: u64,
    pub membership: DiskMembership,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiskMembership {
    Available,
    InPool { pool: String, vdev: String },
}

/// Parses `geom disk list`'s block format — tracks the current disk
/// from each `Geom name: <x>` line, and reads the first `Mediasize:
/// <bytes> (...)` line inside that block for size. Not a clean `-H -p`
/// tabular format, so parsed leniently line-by-line, the same approach
/// `parse_pool_status_vdevs` already uses for `zpool status`. A block
/// with no `Mediasize:` line at all (some virtual/optical geoms omit
/// it) still produces a `DiskInfo` with `size: 0`, rather than being
/// silently dropped.
fn parse_disk_list(output: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_size: Option<u64> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("Geom name:") {
            if let Some(name) = current_name.take() {
                disks.push(DiskInfo { name, size: current_size.take().unwrap_or(0), membership: DiskMembership::Available });
            }
            current_name = Some(name.trim().to_string());
        } else if current_size.is_none() {
            if let Some(rest) = trimmed.strip_prefix("Mediasize:") {
                current_size = rest.trim().split_whitespace().next().and_then(|s| s.parse().ok());
            }
        }
    }
    if let Some(name) = current_name {
        disks.push(DiskInfo { name, size: current_size.unwrap_or(0), membership: DiskMembership::Available });
    }
    disks
}

pub fn list_disks(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>> {
    let output = super::run_checked(runner, "geom", &["disk", "list"], "geom disk list")?;
    Ok(parse_disk_list(&output.stdout))
}

/// Walks `vdevs` (the same depth-tagged shape `get_pool_vdevs` produces)
/// and, for each leaf disk matching a `DiskInfo` in `disks`, sets its
/// membership to the owning pool and the nearest enclosing group's
/// name (or the disk's own name if it's a bare top-level vdev with no
/// group, e.g. a stripe member). Depth 0 is the pool's own name line
/// (not a vdev); depth 1 is either a group node (`mirror-0`,
/// `raidz1-0`, ...) or a bare top-level disk; depth 2+ are a group's
/// children.
pub fn annotate_membership(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine]) {
    let mut current_group: Option<String> = None;
    for vdev in vdevs {
        if vdev.depth == 0 {
            continue;
        }
        if vdev.depth == 1 {
            current_group = Some(vdev.name.clone());
        }
        if !is_leaf_disk(&vdev.name) {
            continue;
        }
        let vdev_name = if vdev.depth == 1 {
            vdev.name.clone()
        } else {
            current_group.clone().unwrap_or_else(|| vdev.name.clone())
        };
        if let Some(disk) = disks.iter_mut().find(|d| disk_matches_vdev(&d.name, &vdev.name)) {
            disk.membership = DiskMembership::InPool { pool: pool.to_string(), vdev: vdev_name };
        }
    }
}

/// True if `vdev_name` refers to `disk_name` itself, or to a GPT
/// partition (`ada0p4`) or BSD slice+partition (`da0s1a`) on it —
/// `zpool status` reports the actual device `zfs`/`zpool` opened, which
/// on any partitioned disk (the default `bsdinstall` layout) is never
/// the bare whole-disk name `geom disk list` uses. Does not resolve
/// GEOM labels (`gpt/zfs0`, `diskid/...`) back to a physical disk —
/// those don't share a name prefix with any whole-disk name at all, so
/// there's nothing to pattern-match against; out of scope for this fix.
fn disk_matches_vdev(disk_name: &str, vdev_name: &str) -> bool {
    if disk_name == vdev_name {
        return true;
    }
    let Some(suffix) = vdev_name.strip_prefix(disk_name) else { return false };
    if let Some(rest) = suffix.strip_prefix('p') {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = suffix.strip_prefix('s') {
        // Split at the first non-digit by index — not `take_while` on a
        // shared iterator, which would silently consume and discard the
        // first non-digit char (letting `da0s1-` / `da0s1ab` match).
        let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits_end == 0 {
            return false;
        }
        let remainder = &rest[digits_end..];
        return remainder.is_empty() || (remainder.len() == 1 && remainder.chars().all(|c| c.is_ascii_alphabetic()));
    }
    false
}

/// Fetches every disk on the host and annotates each with its pool/vdev
/// membership by cross-referencing every pool currently on the host.
/// Self-contained — correct even if the caller has never separately
/// fetched the pool list this session.
pub fn list_disks_with_membership(runner: &dyn CommandRunner) -> Result<Vec<DiskInfo>> {
    let mut disks = list_disks(runner)?;
    let pools = pool::list_pools(runner)?;
    for p in &pools {
        let (vdevs, _scan_status) = pool::get_pool_vdevs(runner, &p.name)?;
        annotate_membership(&mut disks, &p.name, &vdevs);
    }
    Ok(disks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use crate::zfs::pool::PoolHealth;

    const GEOM_DISK_LIST_FIXTURE: &str = "\
Geom name: ada0
Providers:
1. Name: ada0
   Mediasize: 48318382080 (45G)
   Sectorsize: 512
   Mode: r3w3e6
   descr: QEMU HARDDISK
   ident: o3q4Qrvy
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
Geom name: da0
Providers:
1. Name: da0
   Mediasize: 268435456 (256M)
   Sectorsize: 512
   Mode: r1w1e1
   descr: Virtio Block Device
   ident: (null)
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
Geom name: da1
Providers:
1. Name: da1
   Mediasize: 268435456 (256M)
   Sectorsize: 512
   Mode: r1w1e1
   descr: Virtio Block Device
   ident: (null)
   rotationrate: unknown
   fwsectors: 63
   fwheads: 16
";

    const GEOM_DISK_LIST_MISSING_MEDIASIZE_FIXTURE: &str = "\
Geom name: cd0
Providers:
1. Name: cd0
   Sectorsize: 2048
   Mode: r0w0e0
   descr: (unknown device)
";

    #[test]
    fn parses_multi_disk_geom_disk_list_fixture() {
        let disks = parse_disk_list(GEOM_DISK_LIST_FIXTURE);
        assert_eq!(
            disks,
            vec![
                DiskInfo { name: "ada0".to_string(), size: 48_318_382_080, membership: DiskMembership::Available },
                DiskInfo { name: "da0".to_string(), size: 268_435_456, membership: DiskMembership::Available },
                DiskInfo { name: "da1".to_string(), size: 268_435_456, membership: DiskMembership::Available },
            ]
        );
    }

    #[test]
    fn parses_disk_block_with_no_mediasize_line_as_zero_size_instead_of_dropping_it() {
        let disks = parse_disk_list(GEOM_DISK_LIST_MISSING_MEDIASIZE_FIXTURE);
        assert_eq!(disks, vec![DiskInfo { name: "cd0".to_string(), size: 0, membership: DiskMembership::Available }]);
    }

    #[test]
    fn list_disks_uses_correct_command_and_parses_result() {
        let runner = FakeRunner::new(GEOM_DISK_LIST_FIXTURE);
        let disks = list_disks(&runner).unwrap();
        assert_eq!(disks.len(), 3);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("geom".to_string(), vec!["disk".to_string(), "list".to_string()]));
    }

    #[test]
    fn list_disks_surfaces_command_failure() {
        let runner = FakeRunner::failing("geom: no such class", 1);
        let result = list_disks(&runner);
        assert!(result.is_err());
    }

    #[test]
    fn annotate_membership_tags_leaf_disks_nested_under_a_mirror_group() {
        let mut disks = vec![
            DiskInfo { name: "ada0".to_string(), size: 0, membership: DiskMembership::Available },
            DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available },
            DiskInfo { name: "da1".to_string(), size: 0, membership: DiskMembership::Available },
        ];
        let vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "mirror-0".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 2, name: "da0".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 2, name: "da1".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        assert_eq!(disks[0].membership, DiskMembership::Available, "ada0 isn't in this pool's vdev tree, must stay Available");
        assert_eq!(disks[1].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() });
        assert_eq!(disks[2].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() });
    }

    #[test]
    fn annotate_membership_tags_bare_top_level_disk_with_its_own_name() {
        let mut disks = vec![DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available }];
        let vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        assert_eq!(disks[0].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() });
    }

    #[test]
    fn annotate_membership_resolves_partition_vdev_to_its_parent_disk() {
        let mut disks = vec![DiskInfo { name: "ada0".to_string(), size: 0, membership: DiskMembership::Available }];
        let vdevs = vec![
            VdevLine { depth: 0, name: "zroot".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "ada0p4".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "zroot", &vdevs);
        assert_eq!(
            disks[0].membership,
            DiskMembership::InPool { pool: "zroot".to_string(), vdev: "ada0p4".to_string() },
            "a disk whose only vdev entry is a GPT partition on it (the default bsdinstall layout) must still resolve to that disk, not stay Available"
        );
    }

    #[test]
    fn disk_matches_vdev_recognizes_partitions_and_slices() {
        assert!(disk_matches_vdev("ada0", "ada0"));
        assert!(disk_matches_vdev("ada0", "ada0p4"));
        assert!(disk_matches_vdev("ada0", "ada0p12"));
        assert!(disk_matches_vdev("da0", "da0s1a"));
        assert!(disk_matches_vdev("da0", "da0s1"));
        assert!(!disk_matches_vdev("ada0", "da0p4"), "must not match a different disk's partition");
        assert!(!disk_matches_vdev("ada0", "ada0xyz"), "must not match garbage suffixes");
        assert!(!disk_matches_vdev("ada0", "ada01"), "a bare trailing digit with no p/s marker is a different whole disk, not a partition of ada0");
        assert!(!disk_matches_vdev("da0", "da0s1ab"), "a slice may carry at most one partition letter");
        assert!(!disk_matches_vdev("da0", "da0s1-"), "a non-letter after the slice number must not match");
        assert!(!disk_matches_vdev("da0", "da0s"), "a slice marker needs a slice number");
        assert!(!disk_matches_vdev("ada0", "ada0p"), "a partition marker needs a partition number");
    }

    const ZPOOL_LIST_TANK_FIXTURE: &str = "tank\t4000000000000\t1200000000000\t2800000000000\tONLINE\n";
    const ZPOOL_STATUS_TANK_FIXTURE: &str = "\
  pool: tank
 state: ONLINE
config:

\tNAME        STATE     READ WRITE CKSUM
\ttank        ONLINE       0     0     0
\t  mirror-0  ONLINE       0     0     0
\t    da0     ONLINE       0     0     0
\t    da1     ONLINE       0     0     0

errors: No known data errors
";

    #[test]
    fn list_disks_with_membership_cross_references_the_hosts_pools() {
        let runner = FakeRunner::new("")
            .route("geom", "disk", GEOM_DISK_LIST_FIXTURE)
            .route("zpool", "list", ZPOOL_LIST_TANK_FIXTURE)
            .route("zpool", "status", ZPOOL_STATUS_TANK_FIXTURE);
        let disks = list_disks_with_membership(&runner).unwrap();
        assert_eq!(disks.len(), 3);
        assert_eq!(disks.iter().find(|d| d.name == "ada0").unwrap().membership, DiskMembership::Available);
        assert_eq!(
            disks.iter().find(|d| d.name == "da0").unwrap().membership,
            DiskMembership::InPool { pool: "tank".to_string(), vdev: "mirror-0".to_string() }
        );
    }

    #[test]
    fn list_disks_with_membership_surfaces_command_failure() {
        let runner = FakeRunner::failing("geom: no such class", 1);
        let result = list_disks_with_membership(&runner);
        assert!(result.is_err());
    }
}
