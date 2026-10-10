//! The sudoers entry a managed host needs: generated from the registry (`print-sudoers`) and verified against
//! the host (`check-host`). Pure pieces first; the two commands are below.

use super::privilege::{self, Entry, Feature, Kind};
use super::CommandRunner;

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

fn unreachable(host: &str, detail: impl std::fmt::Display) -> Failure {
    Failure::Unreachable(format!("cannot reach {host}: {detail}"))
}

/// Asks the host where each command lives, over the unprivileged ssh path (no sudo, so it works before
/// any sudoers entry exists), and vets the answers.
pub fn resolve(runner: &dyn CommandRunner, host: &str, entries: &[&'static Entry]) -> Result<Vec<Resolved>, Failure> {
    let mut args: Vec<&str> = vec!["-c", RESOLVE_SCRIPT, "sh"];
    args.extend(entries.iter().map(|e| e.name));
    let out = runner.run_unprivileged("sh", &args).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    if !out.success() {
        return Err(problem(format!("looking up paths on {host} failed: {}", out.stderr.trim())));
    }
    parse_resolution(host, &out.stdout, entries)
}

pub fn print_sudoers(runner: &dyn CommandRunner, host: &str, ssh_target: &str, features: &[Feature], generated: &str) -> Result<String, Failure> {
    let user = user_of(ssh_target)?;
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to put in a sudoers entry"));
    }
    let resolved = resolve(runner, host, &entries)?;
    Ok(render(host, user, features, &resolved, generated))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Missing,
    TooBroad,
}

#[derive(Debug)]
pub struct Line {
    pub name: &'static str,
    /// The path and, for a pinned entry, its arguments: what sudoers has to allow.
    pub detail: String,
    pub status: Status,
}

#[derive(Debug)]
pub struct Report {
    pub lines: Vec<Line>,
}

impl Report {
    pub fn exit_code(&self) -> i32 {
        i32::from(self.lines.iter().any(|l| l.status != Status::Ok))
    }

    pub fn render(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            let label = match line.status {
                Status::Ok => "ok",
                Status::Missing => "MISSING",
                Status::TooBroad => "TOO BROAD",
            };
            text.push_str(&format!("{label:<10}{:<11}{}\n", line.name, line.detail));
        }
        let count = |s: Status| self.lines.iter().filter(|l| l.status == s).count();
        text.push_str(&format!("{} ok, {} missing, {} too broad\n", count(Status::Ok), count(Status::Missing), count(Status::TooBroad)));
        text
    }
}

/// Whether the host's sudo would run `path args` without a password (`sudo -n -l <command>` exits 0).
fn sudo_allows(runner: &dyn CommandRunner, host: &str, path: &str, args: &[&str]) -> Result<bool, Failure> {
    let mut full = vec!["-n", "-l", path];
    full.extend_from_slice(args);
    let out = runner.run_unprivileged("sudo", &full).map_err(|e| unreachable(host, e))?;
    if out.status == 255 {
        return Err(unreachable(host, out.stderr.trim()));
    }
    Ok(out.success())
}

pub fn check_host(runner: &dyn CommandRunner, host: &str, features: &[Feature]) -> Result<Report, Failure> {
    let entries = privilege::privileged_for(features);
    if entries.is_empty() {
        return Err(problem("the selected features need no privileged commands, so there is nothing to check"));
    }
    let resolved = resolve(runner, host, &entries)?;
    let mut lines = Vec::new();
    for r in &resolved {
        let forms: Vec<(&[&str], &str)> = match &r.entry.kind {
            Kind::Privileged { pinned, probe } if !pinned.is_empty() => pinned.iter().map(|args| (*args, *probe)).collect(),
            _ => vec![(&[][..], "")],
        };
        for (args, probe) in forms {
            // A pinned entry must refuse a different argument too, or the pin is not doing its job.
            let status = if !sudo_allows(runner, host, &r.path, args)? {
                Status::Missing
            } else if !probe.is_empty() && sudo_allows(runner, host, &r.path, &[probe])? {
                Status::TooBroad
            } else {
                Status::Ok
            };
            let detail = if args.is_empty() { r.path.clone() } else { format!("{} {}", r.path, args.join(" ")) };
            lines.push(Line { name: r.entry.name, detail, status });
        }
    }
    Ok(Report { lines })
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
    use crate::error::Result as AppResult;
    use crate::exec::{CommandOutput, CommandRunner};
    use std::sync::Mutex;

    /// Answers every call through `answer(cmd, args)` and records the call.
    struct Scripted {
        calls: Mutex<Vec<String>>,
        answer: Box<dyn Fn(&str, &[&str]) -> CommandOutput + Send + Sync>,
    }

    impl Scripted {
        fn new(answer: impl Fn(&str, &[&str]) -> CommandOutput + Send + Sync + 'static) -> Self {
            Scripted { calls: Mutex::new(Vec::new()), answer: Box::new(answer) }
        }
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl CommandRunner for Scripted {
        fn run(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.calls.lock().unwrap().push(format!("{cmd} {}", args.join(" ")));
            Ok((self.answer)(cmd, args))
        }
        fn run_with_stdin(&self, cmd: &str, args: &[&str], _stdin: &str) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> AppResult<CommandOutput> {
            self.run(cmd, args)
        }
    }

    fn out(stdout: &str, status: i32) -> CommandOutput {
        CommandOutput { stdout: stdout.to_string(), stderr: String::new(), status }
    }

    /// `sh -c SCRIPT sh name...` answered from a table of where each command lives.
    fn host_with(paths: &'static [(&'static str, &'static str)]) -> Scripted {
        Scripted::new(move |cmd, args| {
            assert_eq!(cmd, "sh");
            assert_eq!(&args[..3], ["-c", RESOLVE_SCRIPT, "sh"]);
            let lines: String = args[3..].iter().map(|n| format!("{n}={}\n", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p))).collect();
            out(&lines, 0)
        })
    }

    const ALL_PATHS: &[(&str, &str)] = &[
        ("zpool", "/sbin/zpool"), ("zfs", "/sbin/zfs"), ("getfacl", "/bin/getfacl"), ("setfacl", "/bin/setfacl"), ("geom", "/sbin/geom"),
        ("bectl", "/sbin/bectl"), ("service", "/usr/sbin/service"), ("smbpasswd", "/usr/local/bin/smbpasswd"), ("smbstatus", "/usr/local/bin/smbstatus"),
        ("cat", "/bin/cat"), ("tee", "/usr/bin/tee"), ("restic", "/usr/local/bin/restic"),
    ];

    #[test]
    fn print_sudoers_asks_the_host_once_for_exactly_the_selected_commands() {
        let runner = host_with(ALL_PATHS);
        let features = select(&[], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@172.16.10.60", &features, "2026-10-09").unwrap();
        assert_eq!(runner.count(), 1);
        let call = runner.calls.lock().unwrap()[0].clone();
        assert!(call.ends_with("sh zpool zfs getfacl setfacl geom bectl service smbpasswd smbstatus cat tee"), "{call}");
        assert!(text.contains("fbsd ALL=(root) NOPASSWD: \\\n    /sbin/zpool"), "{text}");
        assert!(!text.contains("restic"), "{text}");
    }

    #[test]
    fn with_restic_the_host_is_asked_for_restic_too() {
        let runner = host_with(ALL_PATHS);
        let features = select(&["restic".to_string()], &[]).unwrap();
        let text = print_sudoers(&runner, "fbsd16", "fbsd@h", &features, "d").unwrap();
        assert!(runner.calls.lock().unwrap()[0].ends_with("tee restic"));
        assert!(text.contains("/usr/local/bin/restic"), "{text}");
    }

    #[test]
    fn an_alias_target_is_refused_before_any_ssh_call() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "fb16", "fb16", &select(&[], &[]).unwrap(), "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0, "no line may be produced and nothing may be asked of the host");
    }

    #[test]
    fn a_user_writable_resolution_is_refused_and_no_line_is_printed() {
        let runner = host_with(&[("zpool", "/home/fbsd/bin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert!(err.message().contains("/home/fbsd/bin/zpool"), "{}", err.message());
    }

    #[test]
    fn a_missing_command_stops_the_run_with_exit_1() {
        let runner = host_with(&[("zpool", "/sbin/zpool")]);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.message().contains("zfs"), "{}", err.message());
    }

    #[test]
    fn an_unreachable_host_is_exit_2() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: connect to host h port 22: Operation timed out".to_string(), status: 255 });
        let err = print_sudoers(&runner, "h", "fbsd@h", &[Feature::Core], "d").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.message().contains("timed out"), "{}", err.message());
    }

    #[test]
    fn selecting_no_privileged_commands_is_a_problem_not_an_empty_line() {
        let runner = host_with(ALL_PATHS);
        let err = print_sudoers(&runner, "h", "fbsd@h", &[], "d").unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 0);
    }

    /// A host whose sudoers allows exactly `allowed` command lines (path plus arguments, space-joined).
    fn sudo_host(allowed: &'static [&'static str], paths: &'static [(&'static str, &'static str)]) -> Scripted {
        Scripted::new(move |cmd, args| match cmd {
            "sh" => {
                let lines: String = args[3..].iter().map(|n| format!("{n}={}\n", paths.iter().find(|(name, _)| name == n).map_or("", |(_, p)| *p))).collect();
                out(&lines, 0)
            }
            "sudo" => {
                assert_eq!(&args[..2], ["-n", "-l"]);
                let asked = args[2..].join(" ");
                out(&asked, if allowed.contains(&asked.as_str()) { 0 } else { 1 })
            }
            other => panic!("unexpected {other}"),
        })
    }

    const EXACT: &[&str] = &[
        "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
        "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
    ];

    fn statuses(report: &Report) -> Vec<(&'static str, Status)> {
        report.lines.iter().map(|l| (l.name, l.status)).collect()
    }

    #[test]
    fn a_host_with_exactly_the_right_entries_is_all_ok() {
        let runner = sudo_host(EXACT, ALL_PATHS);
        let report = check_host(&runner, "fbsd16", &select(&[], &[]).unwrap()).unwrap();
        assert_eq!(report.lines.len(), 11);
        assert!(report.lines.iter().all(|l| l.status == Status::Ok), "{}", report.render());
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_missing_entry_is_reported_by_name_and_fails_the_run() {
        const WITHOUT_GEOM: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(WITHOUT_GEOM, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("geom", Status::Missing)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("MISSING") && report.render().contains("1 missing"), "{}", report.render());
    }

    #[test]
    fn a_plain_cat_entry_is_too_broad() {
        const BROAD_CAT: &[&str] = &[
            "/sbin/zpool", "/sbin/zfs", "/bin/getfacl", "/bin/setfacl", "/sbin/geom", "/sbin/bectl", "/usr/sbin/service",
            "/usr/local/bin/smbpasswd", "/usr/local/bin/smbstatus -S", "/bin/cat /usr/local/etc/smb4.conf", "/bin/cat /etc/master.passwd", "/usr/bin/tee /usr/local/etc/smb4.conf",
        ];
        let report = check_host(&sudo_host(BROAD_CAT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        assert!(statuses(&report).contains(&("cat", Status::TooBroad)), "{}", report.render());
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("TOO BROAD"), "{}", report.render());
    }

    #[test]
    fn restic_is_checked_only_when_selected() {
        let runner = sudo_host(&["/usr/local/bin/restic"], ALL_PATHS);
        let report = check_host(&runner, "h", &select(&["restic".to_string()], &["core", "acl", "disks", "bootenvs", "shares", "samba"].map(String::from)).unwrap()).unwrap();
        assert_eq!(statuses(&report), [("restic", Status::Ok)]);
    }

    #[test]
    fn an_unreachable_host_is_exit_2_after_a_single_ssh_call() {
        let runner = Scripted::new(|_, _| CommandOutput { stdout: String::new(), stderr: "ssh: Could not resolve hostname".to_string(), status: 255 });
        let err = check_host(&runner, "h", &select(&[], &[]).unwrap()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 1, "one timeout, not one per entry");
    }

    #[test]
    fn ssh_dropping_mid_run_is_exit_2_and_stops_asking() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let runner = Scripted::new(move |cmd, args| {
            if cmd == "sh" {
                let lines: String = args[3..].iter().map(|n| format!("{n}=/sbin/{n}\n")).collect();
                return out(&lines, 0);
            }
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { out("", 0) } else { CommandOutput { stdout: String::new(), stderr: "gone".to_string(), status: 255 } }
        });
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert_eq!(runner.count(), 3, "lookup, one answered check, the one that failed");
    }

    #[test]
    fn a_missing_binary_on_the_host_is_a_problem_before_any_sudo_check() {
        let runner = sudo_host(EXACT, &[("zpool", "/sbin/zpool")]);
        let err = check_host(&runner, "h", &[Feature::Core]).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert_eq!(runner.count(), 1);
    }

    #[test]
    fn the_report_lists_one_line_per_entry_with_its_pinned_arguments() {
        let report = check_host(&sudo_host(EXACT, ALL_PATHS), "h", &select(&[], &[]).unwrap()).unwrap();
        let text = report.render();
        assert!(text.contains("ok") && text.contains("/bin/cat /usr/local/etc/smb4.conf"), "{text}");
        assert!(text.trim_end().ends_with("11 ok, 0 missing, 0 too broad"), "{text}");
    }

}
