//! The sudoers entry a managed host needs: generated from the registry (`print-sudoers`) and verified against
//! the host (`check-host`). Pure pieces first; the two commands are below.

use super::privilege::{self, Entry, Feature, Kind};

/// Why a run stopped. `Unreachable` (ssh exit 255) maps to exit code 2 so a script can tell "the host is
/// down" from "the host is misconfigured".
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Unreachable(String),
    Problem(String),
}

impl Failure {
    pub fn exit_code(&self) -> i32 {
        match self {
            Failure::Unreachable(_) => 2,
            Failure::Problem(_) => 1,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Failure::Unreachable(m) | Failure::Problem(m) => m,
        }
    }
}

fn problem(message: impl Into<String>) -> Failure {
    Failure::Problem(message.into())
}

#[derive(Debug)]
pub struct Resolved {
    pub entry: &'static Entry,
    pub path: String,
}

/// Directories only root can write to. A resolved path anywhere else could be a binary the connecting user
/// planted earlier in `PATH`, and must never become a sudoers entry.
const SYSTEM_DIRS: [&str; 6] = ["/bin/", "/sbin/", "/usr/bin/", "/usr/sbin/", "/usr/local/bin/", "/usr/local/sbin/"];

/// Run as `sh -c RESOLVE_SCRIPT sh <name>...`: prints `name=path` per command (empty path when missing, which
/// keeps the lines aligned with the names). The extended `PATH` matters because a non-interactive ssh session
/// often has only `/usr/bin:/bin`; sudo itself resolves from its own `secure_path`.
pub const RESOLVE_SCRIPT: &str = r#"PATH="$PATH:/sbin:/usr/sbin:/usr/local/sbin:/usr/local/bin"; for c in "$@"; do printf '%s=%s\n' "$c" "$(command -v "$c")"; done"#;

/// The account zfsctl connects as: the part of `ssh_target` before `@`. An alias with no user part is refused:
/// the sudoers line would have to guess whose account it is for.
pub fn user_of(ssh_target: &str) -> Result<&str, Failure> {
    let ok = |user: &str| !user.is_empty() && !user.starts_with('-') && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    match ssh_target.split_once('@') {
        Some((user, _)) if ok(user) => Ok(user),
        _ => Err(problem(format!("ssh_target '{ssh_target}' needs a plain user@host form (letters, digits, . _ -), so the sudoers line knows which account it is for"))),
    }
}

pub fn vet_path(name: &str, path: &str) -> Result<(), Failure> {
    let plain = path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'));
    if plain && !path.contains("..") && SYSTEM_DIRS.iter().any(|dir| path.starts_with(dir)) {
        Ok(())
    } else {
        Err(problem(format!("{name} resolved to '{path}', which is not under a system directory ({}); refusing to put it in a sudoers entry", SYSTEM_DIRS.join(" "))))
    }
}

pub fn parse_resolution(host: &str, stdout: &str, wanted: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let found: std::collections::HashMap<&str, &str> = stdout.lines().filter_map(|line| line.split_once('=')).collect();
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for entry in wanted {
        match found.get(entry.name).copied().filter(|p| !p.is_empty()) {
            Some(path) => {
                vet_path(entry.name, path)?;
                resolved.push(Resolved { entry, path: path.to_string() });
            }
            None => missing.push(entry.name),
        }
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(problem(format!("not found on {host}: {} (install them, or leave out the feature that needs them with --without)", missing.join(", "))))
    }
}

/// One sudoers command spec per allowed form: the bare path for an open entry, `path args` for each pinned one.
pub fn sudoers_items(resolved: &[Resolved]) -> Vec<String> {
    let mut items = Vec::new();
    for r in resolved {
        match &r.entry.kind {
            Kind::Privileged { pinned, .. } if !pinned.is_empty() => items.extend(pinned.iter().map(|args| format!("{} {}", r.path, args.join(" ")))),
            _ => items.push(r.path.clone()),
        }
    }
    items
}

pub fn render(host: &str, user: &str, features: &[Feature], resolved: &[Resolved], generated: &str) -> String {
    let mut text = format!("# zfsctl sudoers entry for host {host}, generated {generated}\n");
    text.push_str(&format!("# features: {}\n", features.iter().map(|f| f.name()).collect::<Vec<_>>().join(", ")));
    let powerful: Vec<&str> = resolved.iter().map(|r| r.entry.name).filter(|name| matches!(*name, "zfs" | "bectl" | "restic")).collect();
    if !powerful.is_empty() {
        let (verb, pronoun) = if powerful.len() == 1 { ("is", "it") } else { ("are", "them") };
        text.push_str(&format!("# {} {verb} as powerful as root: grant {pronoun} only to hosts you trust zfsctl with.\n", powerful.join(", ")));
    }
    text.push_str("# Install as /usr/local/etc/sudoers.d/zfsctl (owner root, mode 0440) after checking it:\n#   visudo -c -f /usr/local/etc/sudoers.d/zfsctl\n");
    let unprivileged: Vec<&str> = privilege::unprivileged_for(features).iter().map(|e| e.name).collect();
    if !unprivileged.is_empty() {
        text.push_str(&format!("# These run without sudo (needed only if the ssh key is locked to a forced command): {}\n", unprivileged.join(", ")));
    }
    text.push_str(&format!("{user} ALL=(root) NOPASSWD: \\\n    {}\n", sudoers_items(resolved).join(", \\\n    ")));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::privilege::{privileged_for, select};

    fn resolution_for(host: &str, lines: &str, features: &[Feature]) -> Result<Vec<Resolved>, Failure> {
        parse_resolution(host, lines, &privileged_for(features))
    }

    const DEFAULT_LINES: &str = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nsmbpasswd=/usr/local/bin/smbpasswd\nsmbstatus=/usr/local/bin/smbstatus\ncat=/bin/cat\ntee=/usr/bin/tee\n";

    #[test]
    fn the_user_is_the_part_before_the_at_sign() {
        assert_eq!(user_of("fbsd@172.16.10.60"), Ok("fbsd"));
        assert_eq!(user_of("zfs.admin_1@host"), Ok("zfs.admin_1"));
    }

    #[test]
    fn a_target_without_a_user_is_refused_because_the_user_would_be_guessed() {
        for bad in ["fb16", "172.16.10.60", "@host", "-oProxyCommand=x@host", "a b@host", "a,b@host", ""] {
            let err = user_of(bad).unwrap_err();
            assert_eq!(err.exit_code(), 1, "{bad:?}");
        }
        assert!(user_of("fb16").unwrap_err().message().contains("user@"));
    }

    #[test]
    fn system_paths_are_accepted() {
        for p in ["/sbin/zpool", "/bin/cat", "/usr/bin/tee", "/usr/sbin/service", "/usr/local/bin/restic", "/usr/local/sbin/x"] {
            assert!(vet_path("x", p).is_ok(), "{p}");
        }
    }

    #[test]
    fn paths_outside_the_system_directories_are_refused() {
        for p in ["/home/fbsd/restic", "/tmp/zfs", "/var/tmp/zfs", "zfs", "./zfs", "/usr/local/bin/../../tmp/x", "/usr/local/bin/a b", "/usr/local/bin/a,b", "/usr/local/bin/a\\b", ""] {
            assert!(vet_path("restic", p).is_err(), "{p:?} must be refused");
        }
        let err = vet_path("restic", "/home/fbsd/restic").unwrap_err();
        assert!(err.message().contains("restic") && err.message().contains("/home/fbsd/restic"), "{}", err.message());
    }

    #[test]
    fn a_shell_alias_or_builtin_answer_is_not_a_path() {
        let lines = "zpool=alias zpool='rm -rf /'\nzfs=/sbin/zfs\n";
        let wanted: Vec<_> = privileged_for(&[Feature::Core]);
        let err = parse_resolution("h", lines, &wanted).unwrap_err();
        assert!(err.message().contains("zpool"), "{}", err.message());
    }

    #[test]
    fn resolution_parses_name_equals_path_lines_in_registry_order() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("h", DEFAULT_LINES, &features).unwrap();
        assert_eq!(resolved.len(), 11);
        assert_eq!((resolved[0].entry.name, resolved[0].path.as_str()), ("zpool", "/sbin/zpool"));
        assert_eq!((resolved[10].entry.name, resolved[10].path.as_str()), ("tee", "/usr/bin/tee"));
    }

    #[test]
    fn a_missing_command_names_the_host_and_every_missing_command() {
        let lines = DEFAULT_LINES.replace("geom=/sbin/geom\n", "geom=\n").replace("bectl=/sbin/bectl\n", "");
        let err = resolution_for("fbsd16", &lines, &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("fbsd16") && err.message().contains("geom") && err.message().contains("bectl"), "{}", err.message());
    }

    #[test]
    fn the_items_are_paths_with_the_pinned_arguments_written_out() {
        let resolved = resolution_for("h", DEFAULT_LINES, &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(
            sudoers_items(&resolved),
            [
                "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
                "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
            ]
        );
    }

    #[test]
    fn the_rendered_entry_has_the_install_steps_and_one_continued_line() {
        let features = select(&[], &[]).unwrap();
        let resolved = resolution_for("fbsd16", DEFAULT_LINES, &features).unwrap();
        let text = render("fbsd16", "fbsd", &features, &resolved, "2026-10-09");
        assert!(text.starts_with("# zfsctl sudoers entry for host fbsd16, generated 2026-10-09\n"), "{text}");
        assert!(text.contains("# features: core, acl, disks, bootenvs, shares, samba\n"), "{text}");
        assert!(text.contains("visudo -c -f /usr/local/etc/sudoers.d/zfsctl") && text.contains("mode 0440"), "{text}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool, \\\n    /sbin/zfs, \\\n"), "{text}");
        assert!(text.trim_end().ends_with("/usr/bin/tee /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.contains("without sudo") && text.contains("getent"), "unprivileged commands are listed in a comment: {text}");
        assert!(!text.contains("restic"), "restic was not selected: {text}");
        assert!(text.contains("zfs, bectl are as powerful as root"), "{text}");
    }

    #[test]
    fn restic_is_in_the_line_only_when_selected() {
        let features = select(&["restic".to_string()], &["samba".to_string()]).unwrap();
        let lines = "zpool=/sbin/zpool\nzfs=/sbin/zfs\ngetfacl=/bin/getfacl\nsetfacl=/bin/setfacl\ngeom=/sbin/geom\nbectl=/sbin/bectl\nservice=/usr/sbin/service\nrestic=/usr/local/bin/restic\n";
        let resolved = resolution_for("h", lines, &features).unwrap();
        let text = render("h", "fbsd", &features, &resolved, "d");
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
        assert!(!text.contains("smbpasswd") && !text.contains("tee"), "{text}");
    }
}
