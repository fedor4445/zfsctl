//! The sector size a pool is built for (`ashift`) against what the disks report: a pool made with an `ashift` smaller than a
//! disk's physical sector turns every small write into a read-modify-write inside the drive, and `ashift` cannot be changed
//! afterwards.

use super::disk::DiskInfo;

/// The `ashift` typed among the leading `-o` options of a `zpool create` spec, if any (`-o ashift=9`).
pub fn typed_ashift(spec: &[String]) -> Option<u32> {
    let mut words = spec.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "-o" | "-O" => {
                let value = words.next()?;
                if word == "-o" {
                    if let Some(n) = value.strip_prefix("ashift=") {
                        return n.parse().ok();
                    }
                }
            }
            _ => return None,
        }
    }
    None
}

/// Words of a vdev spec that are layout keywords, not disks.
const KEYWORDS: [&str; 9] = ["mirror", "raidz", "raidz1", "raidz2", "raidz3", "log", "cache", "spare", "special"];

/// The disks named in the vdev part of `spec` (after the leading options) that the host's Disks list knows, by name or `/dev/name`.
pub fn named_disks<'a>(spec: &[String], disks: &'a [DiskInfo]) -> Vec<&'a DiskInfo> {
    let mut words = spec.iter().map(String::as_str).peekable();
    while let Some(word) = words.peek() {
        if matches!(*word, "-o" | "-O") {
            words.next();
            words.next();
        } else {
            break;
        }
    }
    words
        .filter(|w| !KEYWORDS.contains(w) && *w != "dedup")
        .filter_map(|w| {
            let name = w.strip_prefix("/dev/").unwrap_or(w);
            disks.iter().find(|d| d.name == name)
        })
        .collect()
}

/// The largest physical sector among `disks` that a vdev built with `ashift` could not hold without read-modify-write, with the
/// disk that reports it.
pub fn too_small<'a>(ashift: u32, disks: &[&'a DiskInfo]) -> Option<(&'a DiskInfo, u64)> {
    let block = 1u64.checked_shl(ashift)?;
    disks.iter().map(|d| (*d, d.physical_sector())).filter(|(_, physical)| *physical > block).max_by_key(|(_, physical)| *physical)
}

/// What to say before creating a pool whose `-o ashift` is below a disk's physical sector.
pub fn warning(ashift: u32, disk: &DiskInfo, physical: u64) -> String {
    format!(
        "ashift={ashift} builds the pool for {}-byte blocks, but {} reports {physical}-byte physical sectors. Every small write would become a read-modify-write inside the drive, and ashift cannot be changed later: fixing it means a new pool and copying everything over. Most 4K drives need ashift=12. (A drive can misreport its sector size; check its data sheet if the numbers look wrong.)",
        1u64 << ashift,
        disk.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(name: &str, sector: u64, stripe: u64) -> DiskInfo {
        DiskInfo { name: name.into(), sector_size: sector, stripe_size: stripe, ..Default::default() }
    }
    fn spec(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn the_typed_ashift_is_read_from_the_leading_options_only() {
        assert_eq!(typed_ashift(&spec(&["-o", "ashift=9", "mirror", "da0", "da1"])), Some(9));
        assert_eq!(typed_ashift(&spec(&["-O", "compression=zstd", "-o", "ashift=12", "da0"])), Some(12));
        assert_eq!(typed_ashift(&spec(&["mirror", "da0", "da1"])), None);
        assert_eq!(typed_ashift(&spec(&["-o", "ashift=lots", "da0"])), None);
        assert_eq!(typed_ashift(&spec(&["-O", "ashift=9", "da0"])), None, "-O sets a dataset property, not the ashift");
    }

    #[test]
    fn disks_are_found_by_name_or_dev_path_and_keywords_and_strangers_are_skipped() {
        let disks = vec![disk("da0", 512, 4096), disk("da1", 512, 0)];
        let got = named_disks(&spec(&["-o", "ashift=9", "mirror", "/dev/da0", "da1", "log", "nvd9"]), &disks);
        assert_eq!(got.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["da0", "da1"]);
    }

    #[test]
    fn an_ashift_below_the_physical_sector_is_reported_with_the_worst_disk() {
        let (a, b, c) = (disk("da0", 512, 4096), disk("da1", 512, 0), disk("da2", 4096, 8192));
        let all = [&a, &b, &c];
        let (worst, physical) = too_small(9, &all).unwrap();
        assert_eq!((worst.name.as_str(), physical), ("da2", 8192));
        assert_eq!(too_small(12, &[&a, &b]), None, "4096 holds a 4096-byte sector");
        assert_eq!(too_small(9, &[&b]), None, "512 holds a 512-byte sector");
        assert_eq!(too_small(13, &all), None);
        assert_eq!(too_small(99, &all), None, "an absurd shift does not overflow");
    }

    #[test]
    fn the_warning_names_the_numbers_and_says_it_cannot_be_undone() {
        let d = disk("da0", 512, 4096);
        let text = warning(9, &d, 4096);
        assert!(text.contains("ashift=9") && text.contains("512-byte blocks") && text.contains("da0") && text.contains("4096-byte physical") && text.contains("cannot be changed later"), "{text}");
    }
}
