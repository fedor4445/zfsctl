//! Every command zfsctl runs on a managed host, the feature that needs it, and what it needs from sudo.
//! This table is the one source for the generated sudoers line, the host check and the drift guards.

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Core,
    Acl,
    Disks,
    BootEnvs,
    Shares,
    Samba,
    Restic,
    Rclone,
}

impl Feature {
    pub const ALL: [Feature; 8] = [Feature::Core, Feature::Acl, Feature::Disks, Feature::BootEnvs, Feature::Shares, Feature::Samba, Feature::Restic, Feature::Rclone];

    pub fn name(self) -> &'static str {
        match self {
            Feature::Core => "core",
            Feature::Acl => "acl",
            Feature::Disks => "disks",
            Feature::BootEnvs => "bootenvs",
            Feature::Shares => "shares",
            Feature::Samba => "samba",
            Feature::Restic => "restic",
            Feature::Rclone => "rclone",
        }
    }

    pub fn parse(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }
}

#[derive(Debug)]
pub enum Kind {
    /// Runs through `sudo -n`. With `pinned` empty any arguments are allowed; otherwise only exactly these
    /// argument lists, and `probe` is one extra argument that sudo must refuse (`check-host` uses it to
    /// notice an entry that is broader than pinned).
    Privileged { pinned: &'static [&'static [&'static str]], probe: &'static str },
    /// Runs without sudo; never in the sudoers line.
    Unprivileged,
}

#[derive(Debug)]
pub struct Entry {
    pub name: &'static str,
    /// The features that need it. Empty: a command only `print-sudoers` / `check-host` themselves run.
    pub features: &'static [Feature],
    pub kind: Kind,
}

const ANY: Kind = Kind::Privileged { pinned: &[], probe: "" };
const SMB_CONF: Kind = Kind::Privileged { pinned: &[&[crate::zfs::smb::CONF_PATH]], probe: "/etc/master.passwd" };

pub const ENTRIES: &[Entry] = &[
    Entry { name: "zpool", features: &[Feature::Core], kind: ANY },
    Entry { name: "zfs", features: &[Feature::Core], kind: ANY },
    Entry { name: "getfacl", features: &[Feature::Acl], kind: ANY },
    Entry { name: "setfacl", features: &[Feature::Acl], kind: ANY },
    Entry { name: "geom", features: &[Feature::Disks], kind: ANY },
    Entry { name: "bectl", features: &[Feature::BootEnvs], kind: ANY },
    Entry { name: "service", features: &[Feature::Shares, Feature::Samba], kind: ANY },
    Entry { name: "smbpasswd", features: &[Feature::Samba], kind: ANY },
    Entry { name: "smbstatus", features: &[Feature::Samba], kind: Kind::Privileged { pinned: &[&["-S"]], probe: "-v" } },
    Entry { name: "cat", features: &[Feature::Samba], kind: SMB_CONF },
    Entry { name: "tee", features: &[Feature::Samba], kind: SMB_CONF },
    Entry { name: "restic", features: &[Feature::Restic], kind: ANY },
    Entry { name: "rclone", features: &[Feature::Rclone], kind: ANY },
    Entry { name: "readlink", features: &[Feature::Restic], kind: ANY },
    Entry { name: "getent", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "uname", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "date", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "hostname", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "sysctl", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "swapinfo", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "iostat", features: &[Feature::Core], kind: Kind::Unprivileged },
    Entry { name: "showmount", features: &[Feature::Shares], kind: Kind::Unprivileged },
    Entry { name: "stat", features: &[Feature::Restic], kind: Kind::Unprivileged },
    Entry { name: "true", features: &[], kind: Kind::Unprivileged },
    Entry { name: "sh", features: &[], kind: Kind::Unprivileged },
    Entry { name: "sudo", features: &[], kind: Kind::Unprivileged },
];

/// The names of the commands that run through `sudo`, for the book's drift test.
#[cfg(test)]
pub(crate) fn privileged_names() -> Vec<&'static str> {
    ENTRIES.iter().filter(|e| matches!(e.kind, Kind::Privileged { .. })).map(|e| e.name).collect()
}

pub fn lookup(name: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.name == name)
}

pub fn is_registered(name: &str) -> bool {
    lookup(name).is_some()
}

/// Whether `name` is registered for the way it is about to run: through `sudo` (`privileged`) or without.
/// A command registered only as unprivileged would run under `sudo` with no sudoers entry, and the other way
/// round it would go out without sudo although the entry says it needs it.
pub fn is_registered_as(name: &str, privileged: bool) -> bool {
    lookup(name).is_some_and(|e| matches!(e.kind, Kind::Privileged { .. }) == privileged)
}

/// What `SshRunner` calls before it sends anything: a command nobody registered would have no sudoers
/// entry generated for it, so it is refused here instead of failing later on a real host.
pub fn require_registered(name: &str, privileged: bool) -> Result<()> {
    if is_registered_as(name, privileged) {
        Ok(())
    } else if is_registered(name) {
        let (is, run) = if privileged { ("unprivileged", "through sudo") } else { ("privileged", "without sudo") };
        Err(AppError::Config(format!("{name} is registered as {is} but is being run {run} (fix the call or its exec::privilege::ENTRIES kind)")))
    } else {
        Err(AppError::Config(format!("{name} is not a registered remote command (add it to exec::privilege::ENTRIES)")))
    }
}

/// The features a run covers: all but `restic`, plus `with`, minus `without`.
pub fn select(with: &[String], without: &[String]) -> Result<Vec<Feature>> {
    let parse = |name: &String| {
        Feature::parse(name).ok_or_else(|| {
            let valid = Feature::ALL.map(Feature::name).join(", ");
            AppError::Config(format!("unknown feature '{name}'; valid features: {valid}"))
        })
    };
    let added: Vec<Feature> = with.iter().map(parse).collect::<Result<_>>()?;
    let removed: Vec<Feature> = without.iter().map(parse).collect::<Result<_>>()?;
    if let Some(f) = added.iter().find(|f| removed.contains(f)) {
        return Err(AppError::Config(format!("feature '{}' is both added and removed", f.name())));
    }
    Ok(Feature::ALL.into_iter().filter(|f| (!matches!(*f, Feature::Restic | Feature::Rclone) || added.contains(f)) && !removed.contains(f)).collect())
}

fn serving(entry: &Entry, features: &[Feature]) -> bool {
    entry.features.iter().any(|f| features.contains(f))
}

pub fn privileged_for(features: &[Feature]) -> Vec<&'static Entry> {
    ENTRIES.iter().filter(|e| matches!(e.kind, Kind::Privileged { .. }) && serving(e, features)).collect()
}

pub fn unprivileged_for(features: &[Feature]) -> Vec<&'static Entry> {
    ENTRIES.iter().filter(|e| matches!(e.kind, Kind::Unprivileged) && serving(e, features)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(entries: Vec<&'static Entry>) -> Vec<&'static str> {
        entries.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn the_default_selection_is_every_feature_but_restic() {
        let all = select(&[], &[]).unwrap();
        assert_eq!(all.len(), 6);
        assert!(!all.contains(&Feature::Restic));
        assert!(all.contains(&Feature::Samba) && all.contains(&Feature::Core));
    }

    #[test]
    fn with_adds_restic_and_without_removes_a_feature() {
        let f = select(&["restic".to_string()], &["samba".to_string()]).unwrap();
        assert!(f.contains(&Feature::Restic) && !f.contains(&Feature::Samba));
    }

    #[test]
    fn an_unknown_feature_is_an_error_naming_the_valid_ones() {
        let err = select(&["nfs".to_string()], &[]).unwrap_err().to_string();
        assert!(err.contains("unknown feature 'nfs'") && err.contains("core, acl, disks, bootenvs, shares, samba, restic"), "{err}");
        assert!(select(&[], &["nope".to_string()]).is_err());
    }

    #[test]
    fn a_feature_both_added_and_removed_is_an_error() {
        let err = select(&["restic".to_string()], &["restic".to_string()]).unwrap_err().to_string();
        assert!(err.contains("both added and removed"), "{err}");
    }

    #[test]
    fn the_default_privileged_set_is_the_eleven_binaries_in_registry_order() {
        let features = select(&[], &[]).unwrap();
        assert_eq!(
            names(privileged_for(&features)),
            ["zpool", "zfs", "getfacl", "setfacl", "geom", "bectl", "service", "smbpasswd", "smbstatus", "cat", "tee"]
        );
    }

    #[test]
    fn service_is_listed_once_when_both_shares_and_samba_are_selected() {
        let n = names(privileged_for(&[Feature::Shares, Feature::Samba]));
        assert_eq!(n.iter().filter(|x| **x == "service").count(), 1, "{n:?}");
        assert_eq!(names(privileged_for(&[Feature::Shares])), ["service"]);
    }

    #[test]
    fn rclone_is_opt_in_like_restic() {
        let default = select(&[], &[]).unwrap();
        assert!(!default.contains(&Feature::Rclone) && !names(privileged_for(&default)).contains(&"rclone"));
        let with = select(&["rclone".to_string()], &["samba".to_string()]).unwrap();
        assert!(with.contains(&Feature::Rclone) && !with.contains(&Feature::Restic) && !with.contains(&Feature::Samba));
        assert_eq!(names(privileged_for(&[Feature::Rclone])), ["rclone"]);
        assert_eq!(Feature::parse("rclone"), Some(Feature::Rclone));
    }

    #[test]
    fn restic_is_privileged_only_when_selected() {
        assert_eq!(names(privileged_for(&[Feature::Restic])), ["restic", "readlink"]);
        assert!(!names(privileged_for(&select(&[], &[]).unwrap())).contains(&"restic"));
    }

    #[test]
    fn unprivileged_commands_never_appear_among_the_privileged_ones() {
        let all = select(&["restic".to_string()], &[]).unwrap();
        let privileged = names(privileged_for(&all));
        for unprivileged in names(unprivileged_for(&all)) {
            assert!(!privileged.contains(&unprivileged), "{unprivileged}");
        }
        assert!(names(unprivileged_for(&all)).contains(&"getent"));
    }

    #[test]
    fn the_tool_only_commands_are_registered_but_belong_to_no_feature() {
        assert!(is_registered("sh") && is_registered("sudo"));
        let all = select(&["restic".to_string()], &[]).unwrap();
        assert!(!names(unprivileged_for(&all)).contains(&"sh"));
        assert!(!names(unprivileged_for(&all)).contains(&"sudo"));
    }

    #[test]
    fn pinned_entries_carry_a_probe_and_no_pinned_argument_needs_sudoers_escaping() {
        for entry in ENTRIES {
            if let Kind::Privileged { pinned, probe } = &entry.kind {
                assert_eq!(pinned.is_empty(), probe.is_empty(), "{}: a pinned entry needs a probe, an open one has none", entry.name);
                for args in *pinned {
                    for arg in *args {
                        assert!(!arg.contains([' ', ',', '\\', ':', '=', '#']), "{}: {arg:?} would need escaping in sudoers", entry.name);
                    }
                }
            }
        }
    }

    #[test]
    fn require_registered_accepts_known_commands_and_refuses_others() {
        assert!(require_registered("zfs", true).is_ok());
        assert!(require_registered("uname", false).is_ok());
        let err = require_registered("rm", true).unwrap_err().to_string();
        assert!(err.contains("rm is not a registered remote command"), "{err}");
    }

    #[test]
    fn a_command_must_be_registered_for_the_way_it_runs() {
        let err = require_registered("uname", true).unwrap_err().to_string();
        assert!(err.contains("registered as unprivileged") && err.contains("through sudo"), "{err}");
        let err = require_registered("zfs", false).unwrap_err().to_string();
        assert!(err.contains("registered as privileged") && err.contains("without sudo"), "{err}");
    }

    #[test]
    fn no_name_is_registered_twice() {
        for (i, a) in ENTRIES.iter().enumerate() {
            assert!(ENTRIES[i + 1..].iter().all(|b| b.name != a.name), "{} appears twice", a.name);
        }
    }
    /// The first string literal after `(` of a run-style call, skipping `run_checked`'s leading runner argument.
    fn commands_in(src: &str) -> Vec<String> {
        let code = src.split("#[cfg(test)]").next().unwrap_or("");
        let mut found = Vec::new();
        for (marker, skip_runner) in [("run_checked(", true), (".run(", false), (".run_unprivileged(", false), (".run_with_stdin(", false), (".command(", false)] {
            for (at, _) in code.match_indices(marker) {
                let mut rest = &code[at + marker.len()..];
                if skip_runner {
                    match rest.find(',') {
                        Some(comma) => rest = &rest[comma + 1..],
                        None => continue,
                    }
                }
                let rest = rest.trim_start();
                if let Some(literal) = rest.strip_prefix('"') {
                    if let Some(end) = literal.find('"') {
                        let name = &literal[..end];
                        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-')) {
                            found.push(name.to_string());
                        }
                    }
                }
            }
        }
        found
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_command_the_code_runs_is_registered() {
        let mut files = Vec::new();
        rust_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut seen = std::collections::BTreeSet::new();
        for file in &files {
            for name in commands_in(&std::fs::read_to_string(file).unwrap()) {
                assert!(is_registered(&name), "{} runs '{name}', which is not in exec::privilege::ENTRIES", file.display());
                seen.insert(name);
            }
        }
        // If the scan stops finding the known commands, it has silently stopped guarding anything.
        for known in ["zpool", "zfs", "getfacl", "setfacl", "geom", "bectl", "service", "smbpasswd", "smbstatus", "cat", "tee", "restic", "rclone", "getent", "sysctl"] {
            assert!(seen.contains(known), "the scan no longer finds '{known}': {seen:?}");
        }
    }

    /// What a README sudoers section fails to say: each privileged command as `name` or a path ending in `/name`,
    /// and each pinned form as `name arg...` (so dropping the `smb4.conf` pin from the prose is noticed).
    fn readme_gaps(section: &str) -> Vec<String> {
        let mut gaps = Vec::new();
        for entry in ENTRIES {
            let Kind::Privileged { pinned, .. } = &entry.kind else { continue };
            if !section.contains(&format!("`{}`", entry.name)) && !section.contains(&format!("/{}", entry.name)) {
                gaps.push(format!("{} is not named", entry.name));
            }
            for args in *pinned {
                let form = format!("{} {}", entry.name, args.join(" "));
                if !section.contains(&form) {
                    gaps.push(format!("the pinned form '{form}' is missing"));
                }
            }
        }
        gaps
    }

    #[test]
    fn the_readme_gap_check_notices_a_missing_command_and_a_missing_pin() {
        let full = "`zpool` /sbin/zfs `getfacl` `setfacl` `geom` `bectl` `service` `smbpasswd` /usr/local/bin/smbstatus -S /bin/cat /usr/local/etc/smb4.conf /usr/bin/tee /usr/local/etc/smb4.conf `restic` `readlink` `rclone`";
        assert_eq!(readme_gaps(full), Vec::<String>::new());
        assert_eq!(readme_gaps(&full.replace("`geom`", "")), ["geom is not named"]);
        assert_eq!(readme_gaps(&full.replace("tee /usr/local/etc/smb4.conf", "tee")), ["the pinned form 'tee /usr/local/etc/smb4.conf' is missing"]);
        // A word in prose is not a mention of the command.
        assert!(readme_gaps(&full.replace("`getfacl`", "getfacl")).contains(&"getfacl is not named".to_string()));
    }

    #[test]
    fn the_readme_names_every_privileged_command() {
        let readme = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
        let start = readme.find("### Remote host setup").expect("the README has a Remote host setup section");
        let section = &readme[start..];
        let section = &section[..section.find("\n## ").unwrap_or(section.len())];
        assert_eq!(readme_gaps(section), Vec::<String>::new());
        assert!(section.contains("print-sudoers") && section.contains("check-host"), "the README must point to the generated entry");
    }

}
