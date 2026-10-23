use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;
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
            DiskMedia::Hdd(0) => parts.push("HDD".to_string()),
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
#[allow(clippy::cast_precision_loss)] // display rounding; sub-byte precision is irrelevant
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
        let letter = UNITS.get(unit).copied().unwrap_or('E');
        return if rounded >= 100.0 { format!("{rounded:.0}{letter}") } else { format!("{rounded:.1}{letter}") };
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
                d.size = rest.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0);
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

/// The disks of the host, from the tool its OS has: `geom disk list` on FreeBSD (and for a host whose OS is not known),
/// `lsblk` on Linux, `iostat -En` on illumos. The last two need no sudo.
pub fn list_disks(runner: &dyn CommandRunner, os: Option<HostOs>) -> Result<Vec<DiskInfo>> {
    match os {
        Some(HostOs::Linux) => {
            let args = ["-b", "-d", "-n", "-P", "-o", "NAME,SIZE,TYPE,MODEL,SERIAL,ROTA"];
            let out = runner.run_unprivileged("lsblk", &args)?;
            if !out.success() {
                return Err(crate::error::AppError::CommandFailed { cmd: "lsblk".to_string(), status: out.status, stderr: out.stderr });
            }
            Ok(parse_lsblk(&out.stdout))
        }
        Some(HostOs::Illumos) => {
            let out = runner.run_unprivileged("iostat", &["-En"])?;
            if !out.success() {
                return Err(crate::error::AppError::CommandFailed { cmd: "iostat -En".to_string(), status: out.status, stderr: out.stderr });
            }
            Ok(parse_iostat_en(&out.stdout))
        }
        _ => {
            let output = super::run_checked(runner, "geom", &["disk", "list"], "geom disk list")?;
            Ok(parse_disk_list(&output.stdout))
        }
    }
}

/// `lsblk -P` lines (`KEY="value" KEY="value" ...`; a value may hold spaces): only `TYPE="disk"` entries, so loop devices,
/// optical drives and partitions are left out. `ROTA` 1 is a rotating disk (its speed is not known: `Hdd(0)`), 0 an SSD.
/// `lsblk -P` writes unusual characters as `\xNN` (a space in a model is `\x20`).
fn unescape_lsblk(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x')).then(|| text.get(i + 2..i + 4)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(b) => {
                out.push(b);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_lsblk(output: &str) -> Vec<DiskInfo> {
    output
        .lines()
        .filter_map(|line| {
            let mut pairs: Vec<(&str, &str)> = Vec::new();
            let mut rest = line.trim();
            while let Some(eq) = rest.find("=\"") {
                let after = &rest[eq + 2..];
                let Some(end) = after.find('"') else { break };
                pairs.push((rest[..eq].trim(), &after[..end]));
                rest = &after[end + 1..];
            }
            let get = |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
            let text = |key: &str| get(key).map(unescape_lsblk);
            (get("TYPE")? == "disk").then(|| DiskInfo {
                name: get("NAME").unwrap_or_default().to_string(),
                size: get("SIZE").and_then(|s| s.parse().ok()).unwrap_or(0),
                membership: DiskMembership::Available,
                model: text("MODEL").and_then(|m| clean_identity(&m)),
                serial: text("SERIAL").and_then(|s| clean_identity(&s)),
                media: match get("ROTA") {
                    Some("1") => DiskMedia::Hdd(0),
                    Some("0") => DiskMedia::Ssd,
                    _ => DiskMedia::Unknown,
                },
            })
        })
        .filter(|d| !d.name.is_empty() && !is_virtual_block_device(&d.name))
        .collect()
}

/// `lsblk` calls a zvol (`zd0`), a compressed-RAM disk (`zram0`) and a network block device (`nbd0`) a disk too. None is a
/// disk to build a pool on: a pool on a zvol of a pool is a deadlock waiting to happen.
fn is_virtual_block_device(name: &str) -> bool {
    name.strip_prefix("zd").is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())) || name.starts_with("zram") || name.starts_with("nbd")
}

/// `iostat -En` blocks: `c1t0d0  Soft Errors: ...`, then `Vendor: .. Product: .. Revision: .. Serial No: ..` and
/// `Size: 21.47GB <21474836480 bytes>`. A block without a size (an empty optical drive) is not a disk. The tool says nothing
/// about rotation, so the media stays unknown.
fn parse_iostat_en(output: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    let mut current: Option<DiskInfo> = None;
    for line in output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 3 && tokens[1] == "Soft" && tokens[2] == "Errors:" {
            disks.extend(current.take().filter(|d| d.size > 0));
            current = Some(DiskInfo { name: tokens[0].to_string(), ..Default::default() });
            continue;
        }
        let Some(d) = current.as_mut() else { continue };
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Vendor:") {
            let (vendor, rest) = rest.split_once("Product:").unwrap_or((rest, ""));
            let (product, rest) = rest.split_once("Revision:").unwrap_or((rest, ""));
            d.model = clean_identity(&format!("{} {}", vendor.trim(), product.trim()));
            d.serial = rest.split_once("Serial No:").and_then(|(_, serial)| clean_identity(serial));
        } else if let Some(rest) = line.strip_prefix("Size:") {
            d.size = rest.split_once('<').and_then(|(_, bytes)| bytes.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    }
    disks.extend(current.filter(|d| d.size > 0));
    disks
}

/// Walks `vdevs` (the same depth-tagged shape `get_pool_vdevs` produces)
/// and, for each leaf disk matching a `DiskInfo` in `disks`, sets its
/// membership to the owning pool and the nearest enclosing group's
/// name (or the disk's own name if it's a bare top-level vdev with no
/// group, e.g. a stripe member). Depth 0 is the pool's own name line
/// (not a vdev); depth 1 is either a group node (`mirror-0`,
/// `raidz1-0`, ...) or a bare top-level disk; depth 2+ are a group's
/// children.
#[cfg(test)]
pub fn annotate_membership(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine]) {
    annotate_with(disks, pool, vdevs, &disk_matches_vdev);
}

/// Whether the pool device `vdev_name` is the disk `disk_name`, by the name rule of the host's OS.
pub(crate) fn disk_matches_vdev_for(os: Option<HostOs>, disk_name: &str, vdev_name: &str) -> bool {
    if os == Some(HostOs::Linux) { linux_disk_matches_vdev(disk_name, vdev_name) } else { disk_matches_vdev(disk_name, vdev_name) }
}

/// `annotate_membership` with the name rule of the host's OS: Linux names partitions `sdb1` and `nvme0n1p1`, FreeBSD and
/// illumos use the rule above (`ada0p4`, `da0s1a`, `c1t0d0s0`).
fn annotate_membership_for(os: Option<HostOs>, disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine]) {
    if os == Some(HostOs::Linux) {
        annotate_with(disks, pool, vdevs, &linux_disk_matches_vdev);
    } else {
        annotate_with(disks, pool, vdevs, &disk_matches_vdev);
    }
}

fn annotate_with(disks: &mut [DiskInfo], pool: &str, vdevs: &[VdevLine], matches: &dyn Fn(&str, &str) -> bool) {
    let mut current_group: Option<String> = None;
    // The first depth-0 line is the pool; a later one (`logs`, `cache`, `special`...) opens a class, which a disk in it carries.
    let mut class: Option<&str> = None;
    let mut seen_pool = false;
    for vdev in vdevs {
        if vdev.depth == 0 {
            if seen_pool {
                class = match vdev.name.as_str() {
                    "logs" => Some("log"),
                    "cache" => Some("cache"),
                    "spares" => Some("spare"),
                    "special" => Some("special"),
                    "dedup" => Some("dedup"),
                    _ => None,
                };
            }
            seen_pool = true;
            continue;
        }
        if vdev.depth == 1 {
            current_group = Some(vdev.name.clone());
        }
        if !is_leaf_disk(&vdev.name) {
            continue;
        }
        let group = if vdev.depth == 1 {
            vdev.name.clone()
        } else {
            current_group.clone().unwrap_or_else(|| vdev.name.clone())
        };
        // A lone log, cache or spare disk is named by its class alone (`cache`); a group keeps its name (`special mirror-1`).
        let vdev_name = match class {
            Some(class @ ("log" | "cache" | "spare")) if vdev.depth == 1 => class.to_string(),
            Some(class) => format!("{class} {group}"),
            None => group,
        };
        if let Some(disk) = disks.iter_mut().find(|d| matches(&d.name, &vdev.name)) {
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
pub fn list_disks_with_membership(runner: &dyn CommandRunner, os: Option<HostOs>) -> Result<Vec<DiskInfo>> {
    let mut disks = list_disks(runner, os)?;
    let pools = pool::list_pools(runner)?;
    for p in &pools {
        // On Linux pools are often built from `/dev/disk/by-id` names (`ata-WDC_...`), which match no disk name; `-L` makes
        // `zpool status` print the real devices (`sdb1`) instead.
        let (vdevs, _scan_status) = pool::get_pool_vdevs_with(runner, &p.name, os == Some(HostOs::Linux))?;
        annotate_membership_for(os, &mut disks, &p.name, &vdevs);
    }
    Ok(disks)
}

/// Linux: `vdev_name` is the disk, or a partition of it (`sdb1`; `nvme0n1p1` when the disk's name ends in a digit). A disk
/// named by id or a file-backed vdev matches nothing.
fn linux_disk_matches_vdev(disk_name: &str, vdev_name: &str) -> bool {
    if disk_name == vdev_name {
        return true;
    }
    let Some(suffix) = vdev_name.strip_prefix(disk_name) else { return false };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if disk_name.ends_with(|c: char| c.is_ascii_digit()) {
        suffix.strip_prefix('p').is_some_and(digits)
    } else {
        digits(suffix)
    }
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

    const LSBLK_UBUNTU: &str = include_str!("disk_fixtures/lsblk_ubuntu.txt");
    const IOSTAT_EN_OMNIOS: &str = include_str!("disk_fixtures/iostat_en_omnios.txt");

    #[test]
    fn lsblk_hex_escapes_in_the_model_are_decoded() {
        let disks = parse_lsblk("NAME=\"sda\" SIZE=\"4000787030016\" TYPE=\"disk\" MODEL=\"WDC\\x20WD40EFRX\\x2d68N32N0\" SERIAL=\"WD-WCC7K1234567\" ROTA=\"1\"\n");
        assert_eq!(disks[0].model.as_deref(), Some("WDC WD40EFRX-68N32N0"));
        assert_eq!(unescape_lsblk("a\\x2"), "a\\x2", "a truncated escape is left alone");
    }

    #[test]
    fn lsblk_skips_zvols_zram_and_nbd_devices_that_are_not_real_disks() {
        let text = "NAME=\"zd0\" SIZE=\"1073741824\" TYPE=\"disk\" MODEL=\"\" SERIAL=\"\" ROTA=\"0\"\n\
                    NAME=\"zram0\" SIZE=\"4294967296\" TYPE=\"disk\" MODEL=\"\" SERIAL=\"\" ROTA=\"0\"\n\
                    NAME=\"nbd3\" SIZE=\"1073741824\" TYPE=\"disk\" MODEL=\"\" SERIAL=\"\" ROTA=\"1\"\n\
                    NAME=\"sda\" SIZE=\"1000204886016\" TYPE=\"disk\" MODEL=\"WDC\" SERIAL=\"X\" ROTA=\"1\"\n\
                    NAME=\"nvme0n1\" SIZE=\"512110190592\" TYPE=\"disk\" MODEL=\"S\" SERIAL=\"Y\" ROTA=\"0\"\n";
        assert_eq!(parse_lsblk(text).iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["sda", "nvme0n1"]);
    }

    #[test]
    fn on_linux_the_vdev_names_come_from_zpool_status_with_real_paths_so_by_id_names_match_their_disks() {
        use crate::zfs::host_health::HostOs;
        // `zpool status -L` prints the resolved devices (`sdb1`), where plain `zpool status` prints `ata-WDC_...`.
        let resolved = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  mirror-0  ONLINE       0     0     0\n\t    sdb1    ONLINE       0     0     0\n\t    sdc1    ONLINE       0     0     0\n\nerrors: No known data errors\n";
        let by_id = resolved.replace("sdb1", "ata-WDC_WD40EFRX_1").replace("sdc1", "ata-WDC_WD40EFRX_2");
        let lsblk = "NAME=\"sdb\" SIZE=\"4000787030016\" TYPE=\"disk\" MODEL=\"WDC WD40EFRX\" SERIAL=\"1\" ROTA=\"1\"\nNAME=\"sdc\" SIZE=\"4000787030016\" TYPE=\"disk\" MODEL=\"WDC WD40EFRX\" SERIAL=\"2\" ROTA=\"1\"\nNAME=\"sdd\" SIZE=\"4000787030016\" TYPE=\"disk\" MODEL=\"WDC WD40EFRX\" SERIAL=\"3\" ROTA=\"1\"\n";
        // The status call must carry -L on Linux: FakeRunner routes by first argument, so the by-id answer is for plain `status`.
        let runner = FakeRunner::new("").route("lsblk", "-b", lsblk).route("zpool", "list", "tank\t100\t10\t90\tONLINE\n").route("zpool", "status", &by_id).route("zpool", "-L", "");
        let _ = &resolved;
        let calls_status_with_l = {
            let r = runner;
            let disks = list_disks_with_membership(&r, Some(HostOs::Linux)).unwrap();
            let calls = r.calls.lock().unwrap().clone();
            (disks, calls)
        };
        let (_disks, calls) = calls_status_with_l;
        assert!(calls.iter().any(|(cmd, args)| cmd == "zpool" && args.first().map(String::as_str) == Some("status") && args.iter().any(|a| a == "-L")), "{calls:?}");
        let ok = FakeRunner::new("").route("lsblk", "-b", lsblk).route("zpool", "list", "tank\t100\t10\t90\tONLINE\n").route("zpool", "status", &resolved);
        let disks = list_disks_with_membership(&ok, Some(HostOs::Linux)).unwrap();
        assert_eq!(disks.iter().filter(|d| matches!(d.membership, DiskMembership::InPool { .. })).count(), 2, "{disks:?}");
        assert_eq!(disks[2].membership, DiskMembership::Available);
    }

    #[test]
    fn lsblk_lists_only_disks_with_size_model_serial_and_media() {
        let disks = parse_lsblk(LSBLK_UBUNTU);
        assert_eq!(disks.len(), 1, "the loop devices are not disks: {disks:?}");
        assert_eq!(disks[0], DiskInfo { name: "vda".into(), size: 42_949_672_960, membership: DiskMembership::Available, model: None, serial: Some("vp7vE2LV".into()), media: DiskMedia::Hdd(0) });
    }

    #[test]
    fn lsblk_reads_quoted_models_with_spaces_ssd_and_survives_garbage() {
        let text = "NAME=\"sda\" SIZE=\"1000204886016\" TYPE=\"disk\" MODEL=\"Samsung SSD 860 EVO\" SERIAL=\"S3Z9NB0K\" ROTA=\"0\"\n\
                    NAME=\"sr0\" SIZE=\"1073741312\" TYPE=\"rom\" MODEL=\"QEMU DVD-ROM\" SERIAL=\"\" ROTA=\"1\"\n\
                    garbage line without pairs\n\
                    NAME=\"nvme0n1\" SIZE=\"not a number\" TYPE=\"disk\" MODEL=\"\" SERIAL=\"\" ROTA=\"\"\n";
        let disks = parse_lsblk(text);
        assert_eq!(disks[0].model.as_deref(), Some("Samsung SSD 860 EVO"));
        assert_eq!((disks[0].media.clone(), disks[0].size), (DiskMedia::Ssd, 1_000_204_886_016));
        assert_eq!(disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["sda", "nvme0n1"], "the DVD drive and the garbage line are skipped");
        assert_eq!((disks[1].size, disks[1].media.clone(), disks[1].model.clone()), (0, DiskMedia::Unknown, None));
        assert!(parse_lsblk("").is_empty() && parse_lsblk("   \n\n").is_empty());
    }

    #[test]
    fn iostat_en_reads_name_model_serial_and_size_per_disk() {
        let disks = parse_iostat_en(IOSTAT_EN_OMNIOS);
        assert_eq!(disks.len(), 2, "{disks:?}");
        assert_eq!(disks[0], DiskInfo { name: "c2t0d0".into(), size: 16_106_127_360, membership: DiskMembership::Available, model: Some("Virtio Block Device".into()), serial: Some("f4DOej3R".into()), media: DiskMedia::Unknown });
        assert_eq!((disks[1].name.as_str(), disks[1].model.as_deref(), disks[1].serial.as_deref(), disks[1].size), ("c1t0d0", Some("ATA QEMU HARDDISK"), Some("LRdb61VJ"), 21_474_836_480));
    }

    #[test]
    fn iostat_en_skips_entries_without_a_size_and_survives_garbage() {
        let text = "sd1              Soft Errors: 0 Hard Errors: 0 Transport Errors: 0 \nVendor: NECVMWar Product: VMware IDE CDR10 Revision: 1.00 Serial No: \nSize: 0.00GB <0 bytes>\n\
                    c0t0d0           Soft Errors: 0 Hard Errors: 0 Transport Errors: 0 \nVendor: ATA Product: X Revision: 1 Serial No:  \n";
        assert!(parse_iostat_en(text).is_empty(), "no usable size, no disk");
        assert!(parse_iostat_en("").is_empty() && parse_iostat_en("lots of\nunrelated text\n").is_empty());
    }

    #[test]
    fn linux_membership_matches_whole_disks_and_numbered_or_p_partitions_only() {
        assert!(linux_disk_matches_vdev("sdb", "sdb") && linux_disk_matches_vdev("sdb", "sdb1") && linux_disk_matches_vdev("sdb", "sdb10"));
        assert!(linux_disk_matches_vdev("nvme0n1", "nvme0n1p1") && linux_disk_matches_vdev("nvme0n1", "nvme0n1"));
        assert!(!linux_disk_matches_vdev("sdb", "sdc1") && !linux_disk_matches_vdev("sdb", "sdbx") && !linux_disk_matches_vdev("sdb", "sdb1x"));
        assert!(!linux_disk_matches_vdev("sdb", "sdb1p"), "a p marker needs a number");
        assert!(!linux_disk_matches_vdev("sda", "/var/tmp/zt1.img"), "a file-backed vdev matches nothing");
        assert!(!linux_disk_matches_vdev("nvme0n1", "nvme0n11"), "a bare digit after a digit is another disk, as on FreeBSD");
    }

    #[test]
    fn illumos_membership_matches_whole_disks_and_slices() {
        assert!(disk_matches_vdev("c1t0d0", "c1t0d0") && disk_matches_vdev("c1t0d0", "c1t0d0s0") && disk_matches_vdev("c1t0d0", "c1t0d0s2"));
        assert!(!disk_matches_vdev("c1t0d0", "c1t1d0") && !disk_matches_vdev("c1t0d0", "c1t0d0s"));
    }

    #[test]
    fn the_backend_follows_the_os_and_membership_comes_from_zpool_status() {
        use crate::zfs::host_health::HostOs;
        let status = "  pool: tank\n state: ONLINE\nconfig:\n\n\tNAME        STATE     READ WRITE CKSUM\n\ttank        ONLINE       0     0     0\n\t  c1t0d0s0  ONLINE       0     0     0\n\nerrors: No known data errors\n";
        let omnios = FakeRunner::new("").route("iostat", "-En", IOSTAT_EN_OMNIOS).route("zpool", "list", "tank\t100\t10\t90\tONLINE\n").route("zpool", "status", status);
        let disks = list_disks_with_membership(&omnios, Some(HostOs::Illumos)).unwrap();
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[1].membership, DiskMembership::InPool { pool: "tank".into(), vdev: "c1t0d0s0".into() }, "{disks:?}");
        assert_eq!(disks[0].membership, DiskMembership::Available);
        let ubuntu = FakeRunner::new("").route("lsblk", "-b", LSBLK_UBUNTU).route("zpool", "list", "").route("zpool", "status", "");
        assert_eq!(list_disks_with_membership(&ubuntu, Some(HostOs::Linux)).unwrap().len(), 1);
        let freebsd = FakeRunner::new("").route("geom", "disk", GEOM_DISK_LIST_FIXTURE).route("zpool", "list", "").route("zpool", "status", "");
        assert!(!list_disks_with_membership(&freebsd, None).unwrap().is_empty(), "an unknown OS asks geom, as before");
        assert!(!list_disks_with_membership(&freebsd, Some(HostOs::Other)).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_rpm_hdd_is_called_hdd_without_a_number() {
        let d = DiskInfo { name: "vda".into(), media: DiskMedia::Hdd(0), ..Default::default() };
        assert!(d.identity_line().contains("HDD") && !d.identity_line().contains("rpm"), "{}", d.identity_line());
        let fast = DiskInfo { name: "da0".into(), media: DiskMedia::Hdd(7200), ..Default::default() };
        assert!(fast.identity_line().contains("HDD 7200 rpm"));
    }

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
        let disks = list_disks(&runner, None).unwrap();
        assert_eq!(disks.len(), 3);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0], ("geom".to_string(), vec!["disk".to_string(), "list".to_string()]));
    }

    #[test]
    fn list_disks_surfaces_command_failure() {
        let runner = FakeRunner::failing("geom: no such class", 1);
        let result = list_disks(&runner, None);
        assert!(result.is_err());
    }

    #[test]
    fn a_disk_in_a_log_cache_special_or_spare_class_says_so_in_its_membership() {
        let mut disks: Vec<DiskInfo> = ["da0", "da1", "da2", "da3", "da4", "da5", "da6"].iter().map(|n| DiskInfo { name: (*n).to_string(), ..Default::default() }).collect();
        let line = |depth: usize, name: &str| VdevLine { depth, name: name.to_string(), health: PoolHealth::Online };
        let vdevs = vec![
            line(0, "tank"),
            line(1, "da0"),
            line(0, "special"),
            line(1, "mirror-1"),
            line(2, "da1"),
            line(2, "da2"),
            line(0, "logs"),
            line(1, "da3"),
            line(0, "cache"),
            line(1, "da4"),
            line(0, "spares"),
            line(1, "da5"),
        ];
        annotate_membership(&mut disks, "tank", &vdevs);
        let vdev = |i: usize| match &disks[i].membership {
            DiskMembership::InPool { vdev, .. } => vdev.clone(),
            DiskMembership::Available => "available".to_string(),
        };
        assert_eq!([vdev(0), vdev(1), vdev(2), vdev(3), vdev(4), vdev(5), vdev(6)], ["da0", "special mirror-1", "special mirror-1", "log", "cache", "spare", "available"]);
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
        let disks = list_disks_with_membership(&runner, None).unwrap();
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
        let result = list_disks_with_membership(&runner, None);
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
        assert_eq!(human_size(102_359), "100K");
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

    #[test]
    fn human_size_switches_from_bytes_to_kib_exactly_at_1024() {
        assert_eq!(human_size(1023), "1023B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(0), "0B");
    }
}
