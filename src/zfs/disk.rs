use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::pool::{self, is_leaf_disk, VdevLine};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiskInfo {
    pub name: String,
    pub size: u64,
    pub membership: DiskMembership,
    /// `descr:` verbatim (manufacturer is usually inside it).
    pub model: Option<String>,
    /// `ident:` — the drive's serial number.
    pub serial: Option<String>,
    pub media: DiskMedia,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum DiskMembership {
    #[default]
    Available,
    InPool { pool: String, vdev: String },
}

/// What `rotationrate:` says: `0` is non-rotating, a positive number is
/// the rpm, anything else (`unknown` on virtual disks) is `Unknown`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DiskMedia {
    Ssd,
    Hdd(u32),
    #[default]
    Unknown,
}

impl DiskMedia {
    fn parse(raw: &str) -> Self {
        match raw.trim().parse::<u32>() {
            Ok(0) => DiskMedia::Ssd,
            Ok(rpm) => DiskMedia::Hdd(rpm),
            Err(_) => DiskMedia::Unknown,
        }
    }
}

impl DiskInfo {
    /// One-line human description for the pool forms, e.g.
    /// `da0  WDC WD40EFRX-68N32N0  S/N WD-WCC7K1234567  HDD 5400 rpm  3.6T`.
    /// Missing parts are omitted; the name is always present.
    pub fn identity_line(&self) -> String {
        let mut parts = vec![self.name.clone()];
        if let Some(m) = &self.model {
            parts.push(m.clone());
        }
        if let Some(s) = &self.serial {
            parts.push(format!("S/N {s}"));
        }
        match self.media {
            DiskMedia::Ssd => parts.push("SSD".to_string()),
            DiskMedia::Hdd(rpm) => parts.push(format!("HDD {rpm} rpm")),
            DiskMedia::Unknown => {}
        }
        if self.size > 0 {
            parts.push(human_size(self.size));
        }
        parts.join("  ")
    }
}

/// 1024-based size in geom's style: `256M`, `3.6T`, `5.0G` — one decimal
/// below 100, none from 100 up. Rounding happens before the unit is
/// chosen, so values that round up to 1024 promote to the next unit
/// (`1.0M`, not `1024K`) and 99.96 prints as `100`, not `100.0`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [char; 6] = ['K', 'M', 'G', 'T', 'P', 'E'];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    loop {
        let rounded = if value >= 99.95 { value.round() } else { (value * 10.0).round() / 10.0 };
        if rounded >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
            continue;
        }
        return if rounded >= 100.0 {
            format!("{rounded:.0}{}", UNITS[unit])
        } else {
            format!("{rounded:.1}{}", UNITS[unit])
        };
    }
}

/// `(null)`, `(unknown device)` and blank values carry no information.
fn clean_identity(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() || v == "(null)" || v == "(unknown device)" {
        None
    } else {
        Some(v.to_string())
    }
}

/// Parses `geom disk list`'s block format — tracks the current disk
/// from each `Geom name: <x>` line, and reads the first `Mediasize:
/// <bytes> (...)`, `descr:`, `ident:` and `rotationrate:` lines inside
/// that block. Not a clean `-H -p`
/// tabular format, so parsed leniently line-by-line, the same approach
/// `parse_pool_status_vdevs` already uses for `zpool status`. A block
/// with no `Mediasize:` line at all (some virtual/optical geoms omit
/// it) still produces a `DiskInfo` with `size: 0`, rather than being
/// silently dropped.
fn parse_disk_list(output: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    let mut current: Option<DiskInfo> = None;
    // First-occurrence tracking per block (a block may repeat fields
    // for secondary providers; the first one wins).
    let (mut seen_size, mut seen_descr, mut seen_ident, mut seen_rot) = (false, false, false, false);
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("Geom name:") {
            disks.extend(current.take());
            current = Some(DiskInfo { name: name.trim().to_string(), ..Default::default() });
            (seen_size, seen_descr, seen_ident, seen_rot) = (false, false, false, false);
            continue;
        }
        let Some(d) = current.as_mut() else { continue };
        if !seen_size {
            if let Some(rest) = trimmed.strip_prefix("Mediasize:") {
                seen_size = true;
                d.size = rest.trim().split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
                continue;
            }
        }
        if !seen_descr {
            if let Some(rest) = trimmed.strip_prefix("descr:") {
                seen_descr = true;
                d.model = clean_identity(rest);
                continue;
            }
        }
        if !seen_ident {
            if let Some(rest) = trimmed.strip_prefix("ident:") {
                seen_ident = true;
                d.serial = clean_identity(rest);
                continue;
            }
        }
        if !seen_rot {
            if let Some(rest) = trimmed.strip_prefix("rotationrate:") {
                seen_rot = true;
                d.media = DiskMedia::parse(rest);
            }
        }
    }
    disks.extend(current);
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
                DiskInfo {
                    name: "ada0".to_string(),
                    size: 48_318_382_080,
                    model: Some("QEMU HARDDISK".to_string()),
                    serial: Some("o3q4Qrvy".to_string()),
                    ..Default::default()
                },
                DiskInfo { name: "da0".to_string(), size: 268_435_456, model: Some("Virtio Block Device".to_string()), ..Default::default() },
                DiskInfo { name: "da1".to_string(), size: 268_435_456, model: Some("Virtio Block Device".to_string()), ..Default::default() },
            ]
        );
    }

    #[test]
    fn parses_disk_block_with_no_mediasize_line_as_zero_size_instead_of_dropping_it() {
        let disks = parse_disk_list(GEOM_DISK_LIST_MISSING_MEDIASIZE_FIXTURE);
        assert_eq!(disks, vec![DiskInfo { name: "cd0".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() }]);
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
            DiskInfo { name: "ada0".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() },
            DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() },
            DiskInfo { name: "da1".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() },
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
        let mut disks = vec![DiskInfo { name: "da0".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() }];
        let vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        assert_eq!(disks[0].membership, DiskMembership::InPool { pool: "tank".to_string(), vdev: "da0".to_string() });
    }

    #[test]
    fn annotate_membership_resolves_partition_vdev_to_its_parent_disk() {
        let mut disks = vec![DiskInfo { name: "ada0".to_string(), size: 0, membership: DiskMembership::Available, ..Default::default() }];
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

    const GEOM_IDENTITY_FIXTURE: &str = "\
Geom name: ada1
Providers:
1. Name: ada1
   Mediasize: 4000787030016 (3.6T)
   Sectorsize: 512
   descr: WDC WD40EFRX-68N32N0
   ident: WD-WCC7K1234567
   rotationrate: 5400
Geom name: ada2
Providers:
1. Name: ada2
   Mediasize: 500107862016 (466G)
   descr: Samsung SSD 860 EVO 500GB
   ident: S3Z2NB0K123456A
   rotationrate: 0
Geom name: nvd0
Providers:
1. Name: nvd0
   Mediasize: 1000204886016 (932G)
   descr: Samsung SSD 970 EVO Plus 1TB
   ident: S4EWNX0M123456B
   rotationrate: 0
   lunid: 0025385b21234567
Geom name: da5
Providers:
1. Name: da5
   Mediasize: 268435456 (256M)
   descr: Virtio Block Device
   ident: (null)
   rotationrate: unknown
Geom name: cd0
Providers:
1. Name: cd0
   descr: (unknown device)
   ident:
   rotationrate: 7200rpm
";

    #[test]
    fn parses_model_serial_and_media_for_an_hdd() {
        let d = &parse_disk_list(GEOM_IDENTITY_FIXTURE)[0];
        assert_eq!(d.model.as_deref(), Some("WDC WD40EFRX-68N32N0"));
        assert_eq!(d.serial.as_deref(), Some("WD-WCC7K1234567"));
        assert_eq!(d.media, DiskMedia::Hdd(5400));
        assert_eq!(d.size, 4_000_787_030_016);
    }

    #[test]
    fn parses_rotationrate_zero_as_ssd_including_nvme_style_block() {
        let disks = parse_disk_list(GEOM_IDENTITY_FIXTURE);
        assert_eq!(disks[1].media, DiskMedia::Ssd);
        assert_eq!(disks[2].name, "nvd0");
        assert_eq!(disks[2].media, DiskMedia::Ssd);
        assert_eq!(disks[2].serial.as_deref(), Some("S4EWNX0M123456B"));
    }

    #[test]
    fn null_unknown_and_empty_identity_values_become_none() {
        let disks = parse_disk_list(GEOM_IDENTITY_FIXTURE);
        assert_eq!(disks[3].model.as_deref(), Some("Virtio Block Device"));
        assert_eq!(disks[3].serial, None, "(null) ident");
        assert_eq!(disks[3].media, DiskMedia::Unknown, "rotationrate: unknown");
        assert_eq!(disks[4].model, None, "(unknown device) descr");
        assert_eq!(disks[4].serial, None, "empty ident");
        assert_eq!(disks[4].media, DiskMedia::Unknown, "non-numeric rotationrate");
    }

    #[test]
    fn identity_fields_do_not_leak_between_blocks() {
        let out = "Geom name: a0\n   descr: One\n   ident: S1\nGeom name: a1\n   Mediasize: 5 (5B)\n";
        let disks = parse_disk_list(out);
        assert_eq!(disks[1].model, None);
        assert_eq!(disks[1].serial, None);
    }

    #[test]
    fn first_occurrence_of_descr_wins() {
        let out = "Geom name: a0\n   descr: First\n   descr: Second\n";
        assert_eq!(parse_disk_list(out)[0].model.as_deref(), Some("First"));
    }

    #[test]
    fn identity_line_includes_every_known_part() {
        let d = DiskInfo {
            name: "da0".into(),
            size: 4_000_787_030_016,
            model: Some("WDC WD40EFRX-68N32N0".into()),
            serial: Some("WD-WCC7K1234567".into()),
            media: DiskMedia::Hdd(5400),
            ..Default::default()
        };
        assert_eq!(d.identity_line(), "da0  WDC WD40EFRX-68N32N0  S/N WD-WCC7K1234567  HDD 5400 rpm  3.6T");
    }

    #[test]
    fn identity_line_omits_missing_parts_and_labels_ssd() {
        let d = DiskInfo { name: "da1".into(), size: 268_435_456, media: DiskMedia::Ssd, ..Default::default() };
        assert_eq!(d.identity_line(), "da1  SSD  256M");
        let d = DiskInfo { name: "da2".into(), size: 0, ..Default::default() };
        assert_eq!(d.identity_line(), "da2");
    }

    #[test]
    fn human_size_uses_1024_units_with_geom_style_precision() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(268_435_456), "256M");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0G");
        assert_eq!(human_size(4_000_787_030_016), "3.6T");
        assert_eq!(human_size(48_318_382_080), "45.0G");
        assert_eq!(human_size(u64::MAX), "16.0E");
    }

    #[test]
    fn human_size_rounds_before_choosing_the_unit() {
        assert_eq!(human_size(1023), "1023B");
        assert_eq!(human_size(1024 * 1024 - 1), "1.0M");
        assert_eq!(human_size(1024u64.pow(3) - 1), "1.0G");
        assert_eq!(human_size((99.96 * 1024.0) as u64), "100K");
        assert_eq!(human_size(256 * 1024 * 1024), "256M");
        let _ = human_size(u64::MAX);
    }

    #[test]
    fn ident_and_rotationrate_first_occurrence_win() {
        let out = "Geom name: a0\n1. Name: a0\n   ident: FIRST\n   rotationrate: 7200\n2. Name: a0p1\n   ident: SECOND\n   rotationrate: 0\n";
        let d = &parse_disk_list(out)[0];
        assert_eq!(d.serial.as_deref(), Some("FIRST"));
        assert_eq!(d.media, DiskMedia::Hdd(7200));
    }

    #[test]
    fn rotationrate_zero_after_nonzero_keeps_the_first() {
        let out = "Geom name: a0\n   rotationrate: 5400\n   rotationrate: 0\n";
        assert_eq!(parse_disk_list(out)[0].media, DiskMedia::Hdd(5400));
    }

    #[test]
    fn fields_in_a_different_order_still_parse() {
        let out = "Geom name: a0\n   rotationrate: 0\n   ident: SN9\n   descr: Some Model\n   Mediasize: 1024 (1.0K)\n";
        let d = &parse_disk_list(out)[0];
        assert_eq!((d.size, d.model.as_deref(), d.serial.as_deref(), d.media.clone()), (1024, Some("Some Model"), Some("SN9"), DiskMedia::Ssd));
    }

    #[test]
    fn crlf_line_endings_parse() {
        let out = "Geom name: a0\r\n   Mediasize: 2048 (2.0K)\r\n   descr: M X\r\n   ident: S1\r\n   rotationrate: 7200\r\n";
        let d = &parse_disk_list(out)[0];
        assert_eq!(d.name, "a0");
        assert_eq!((d.size, d.model.as_deref(), d.serial.as_deref(), d.media.clone()), (2048, Some("M X"), Some("S1"), DiskMedia::Hdd(7200)));
    }

    #[test]
    fn bare_rotationrate_number_is_hdd_and_ident_keeps_inner_spaces() {
        let out = "Geom name: a0\n   rotationrate: 7200\n   ident: AB  CD EF\n";
        let d = &parse_disk_list(out)[0];
        assert_eq!(d.media, DiskMedia::Hdd(7200));
        assert_eq!(d.serial.as_deref(), Some("AB  CD EF"));
    }
}
