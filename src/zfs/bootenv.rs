//! Boot environments via FreeBSD's `bectl`. Only `bectl list -H` is parsed (one
//! tab-separated row per BE: name, active flags, mountpoint, space, created); the
//! multi-line `list -a` form is never used. Every call is argv-only.

use crate::error::{AppError, Result};
use crate::exec::CommandRunner;
use crate::zfs::host_health::HostOs;

/// What `bectl list` reports in its flags column: `N` active now, `R` active on
/// reboot, `T` activated for the next boot only, `-` none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActiveFlags {
    pub now: bool,
    pub reboot: bool,
    pub temp: bool,
}

impl ActiveFlags {
    /// `Now+Reboot`, `Now`, `Reboot`, `Temp` (joined with `+` when several), or empty.
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.now {
            parts.push("Now");
        }
        if self.reboot {
            parts.push("Reboot");
        }
        if self.temp {
            parts.push("Temp");
        }
        parts.join("+")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEnv {
    pub name: String,
    pub flags: ActiveFlags,
    /// `None` when not mounted (`bectl` prints `-`).
    pub mountpoint: Option<String>,
    /// As bectl prints it (`5.18G`, `8K`, `0`).
    pub space: String,
    pub created: String,
}

impl BootEnv {
    /// Permanent activation is pointless when it is already the one used on reboot.
    pub fn can_activate(&self) -> bool {
        !self.flags.reboot
    }

    /// A next-boot-only activation of the running or already-temporary BE does nothing.
    pub fn can_activate_temporarily(&self) -> bool {
        !self.flags.now && !self.flags.temp
    }

    pub fn can_clear_temporary(&self) -> bool {
        self.flags.temp
    }

    /// Not the running BE, not the one booted next, and not one with a pending next-boot
    /// activation (destroying that would leave the next-boot pointer dangling; `T` clears it first).
    pub fn can_destroy(&self) -> bool {
        !self.flags.now && !self.flags.reboot && !self.flags.temp
    }

    pub fn can_mount(&self) -> bool {
        self.mountpoint.is_none()
    }

    /// Mounted somewhere other than `/` (the running BE is mounted at `/` and cannot be unmounted).
    pub fn can_unmount(&self) -> bool {
        self.mountpoint.as_deref().is_some_and(|m| m != "/")
    }
}

fn parse_flags(flags: &str, line: &str) -> Result<ActiveFlags> {
    if flags.is_empty() {
        return Err(AppError::Parse(format!("empty active flags in bectl list line: {line:?}")));
    }
    if flags == "-" {
        return Ok(ActiveFlags::default());
    }
    let mut parsed = ActiveFlags::default();
    for c in flags.chars() {
        match c {
            'N' => parsed.now = true,
            'R' => parsed.reboot = true,
            'T' => parsed.temp = true,
            _ => return Err(AppError::Parse(format!("unknown active flag {c:?} in bectl list line: {line:?}"))),
        }
    }
    Ok(parsed)
}

/// Parses `bectl list -H`. A malformed row fails the whole parse: silently skipping one
/// would hide a boot environment the user thinks they have.
pub fn parse_bootenvs(output: &str) -> Result<Vec<BootEnv>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[name, flags, mountpoint, space, created] = fields.as_slice() else {
                return Err(AppError::Parse(format!("expected 5 fields in bectl list line: {line:?}")));
            };
            if name.is_empty() {
                return Err(AppError::Parse(format!("empty name in bectl list line: {line:?}")));
            }
            Ok(BootEnv {
                name: name.to_string(),
                flags: parse_flags(flags, line)?,
                mountpoint: (mountpoint != "-").then(|| mountpoint.to_string()),
                space: space.to_string(),
                created: created.to_string(),
            })
        })
        .collect()
}

/// A name must be non-empty, must not start with `-` (it would be read as an option) and
/// may contain only ASCII letters, digits and `_ - . :` (ZFS dataset-name characters).
pub fn validate_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("The boot environment name cannot be empty".to_string());
    }
    if name.starts_with('-') {
        return Err("A boot environment name cannot start with '-'".to_string());
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))) {
        return Err(format!("{c:?} is not allowed in a boot environment name (letters, digits and _ - . : only)"));
    }
    Ok(())
}

fn checked(name: &str) -> Result<&str> {
    validate_name(name).map_err(AppError::Parse)?;
    Ok(name)
}

/// The boot environments of a host, from the tool its OS has: `bectl list -H` on FreeBSD (and for a host whose OS is not
/// known), `beadm list -H` on illumos. `beadm list` needs no sudo; changing boot environments there is not built yet.
pub fn list_for(runner: &dyn CommandRunner, os: Option<crate::zfs::host_health::HostOs>) -> Result<Vec<BootEnv>> {
    if os != Some(crate::zfs::host_health::HostOs::Illumos) {
        return list(runner);
    }
    // Through `sh` so that listing needs no sudo (`beadm` itself is registered for the changes, which do): a host without the
    // sudoers entry still shows its boot environments.
    let out = runner.run_unprivileged("sh", &["-c", "beadm list -H"])?;
    if !out.success() {
        return Err(AppError::CommandFailed { cmd: "beadm list".to_string(), status: out.status, stderr: out.stderr });
    }
    parse_beadm_list(&out.stdout)
}

/// `beadm list -H`: one `;`-separated row per boot environment: name, uuid, flags (`N` now, `R` reboot, `T` temporary),
/// mountpoint, space in bytes, policy, created as seconds since the epoch. Numbers that cannot be read are shown as `0` and
/// `-` instead of failing the whole list.
pub fn parse_beadm_list(output: &str) -> Result<Vec<BootEnv>> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split(';').collect();
            if f.len() < 7 || f[0].is_empty() {
                return Err(AppError::Parse(format!("unexpected beadm list line: {line}")));
            }
            Ok(BootEnv {
                name: f[0].to_string(),
                flags: ActiveFlags { now: f[2].contains('N'), reboot: f[2].contains('R'), temp: f[2].contains('T') },
                mountpoint: Some(f[3]).filter(|m| !m.is_empty() && *m != "-").map(str::to_string),
                space: f[4].parse::<u64>().map_or_else(|_| "0".to_string(), crate::zfs::disk::human_size),
                created: f[6].parse::<i64>().ok().and_then(|secs| chrono::DateTime::from_timestamp(secs, 0)).map_or_else(|| "-".to_string(), |t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()),
            })
        })
        .collect()
}

pub fn list(runner: &dyn CommandRunner) -> Result<Vec<BootEnv>> {
    let output = super::run_checked(runner, "bectl", &["list", "-H"], "bectl list")?;
    parse_bootenvs(&output.stdout)
}

/// The boot environment tool of a host's OS: `beadm` on illumos, `bectl` on FreeBSD (and for a host whose OS is not known).
fn tool(os: Option<HostOs>) -> &'static str {
    if os == Some(HostOs::Illumos) { "beadm" } else { "bectl" }
}

/// What `beadm` has no equivalent for (mounting at a path of its own choosing, destroying with the origin snapshot): refused
/// with a message instead of guessed at.
fn not_on_illumos(what: &str) -> AppError {
    AppError::Config(format!("{what} is not available on illumos hosts yet"))
}

/// `create [-e <from>] <name>` (the same shape for both tools); without `from` the running BE is cloned.
pub fn create(runner: &dyn CommandRunner, name: &str, from: Option<&str>, os: Option<HostOs>) -> Result<()> {
    let name = checked(name)?;
    let mut args = vec!["create"];
    if let Some(from) = from {
        args.extend(["-e", checked(from)?]);
    }
    args.push(name);
    super::run_checked(runner, tool(os), &args, &format!("{} create {name}", tool(os)))?;
    Ok(())
}

/// `activate [-t] <name>`: permanent, or for the next boot only with `temporary`.
pub fn activate(runner: &dyn CommandRunner, name: &str, temporary: bool, os: Option<HostOs>) -> Result<()> {
    let name = checked(name)?;
    let mut args = vec!["activate"];
    if temporary {
        args.push("-t");
    }
    args.push(name);
    super::run_checked(runner, tool(os), &args, &format!("{} activate {name}", tool(os)))?;
    Ok(())
}

/// `activate -T <name>`: removes a temporary activation.
pub fn clear_temporary(runner: &dyn CommandRunner, name: &str, os: Option<HostOs>) -> Result<()> {
    let name = checked(name)?;
    super::run_checked(runner, tool(os), &["activate", "-T", name], &format!("{} activate -T {name}", tool(os)))?;
    Ok(())
}

pub fn rename(runner: &dyn CommandRunner, from: &str, to: &str, os: Option<HostOs>) -> Result<()> {
    let (from, to) = (checked(from)?, checked(to)?);
    super::run_checked(runner, tool(os), &["rename", from, to], &format!("{} rename {from} {to}", tool(os)))?;
    Ok(())
}

/// `bectl mount <name>`; bectl picks a `/tmp/be_mount.XXXX` path, which the next list shows. `beadm mount` needs a path.
pub fn mount(runner: &dyn CommandRunner, name: &str, os: Option<HostOs>) -> Result<()> {
    if os == Some(HostOs::Illumos) {
        return Err(not_on_illumos("Mounting a boot environment"));
    }
    let name = checked(name)?;
    super::run_checked(runner, "bectl", &["mount", name], &format!("bectl mount {name}"))?;
    Ok(())
}

pub fn unmount(runner: &dyn CommandRunner, name: &str, os: Option<HostOs>) -> Result<()> {
    if os == Some(HostOs::Illumos) {
        return Err(not_on_illumos("Unmounting a boot environment"));
    }
    let name = checked(name)?;
    super::run_checked(runner, "bectl", &["umount", name], &format!("bectl umount {name}"))?;
    Ok(())
}

/// `bectl destroy [-o] <name>` (`-o` also removes the origin snapshot); `beadm destroy -F <name>` (`-F`: no prompt, the
/// confirmation was asked already; there is no origin option).
pub fn destroy(runner: &dyn CommandRunner, name: &str, origin: bool, os: Option<HostOs>) -> Result<()> {
    if os == Some(HostOs::Illumos) {
        if origin {
            return Err(not_on_illumos("Destroying a boot environment together with its origin"));
        }
        let name = checked(name)?;
        super::run_checked(runner, "beadm", &["destroy", "-F", name], &format!("beadm destroy {name}"))?;
        return Ok(());
    }
    let name = checked(name)?;
    let mut args = vec!["destroy"];
    if origin {
        args.push("-o");
    }
    args.push(name);
    super::run_checked(runner, "bectl", &args, &format!("bectl destroy {name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    /// Real `bectl list -H` rows captured on FreeBSD 15.1 (tab separated), plus a
    /// mounted BE and a temporarily-activated one as seen in the 2026-10-07 probe.
    const LIST: &str = "default\tNR\t/\t5.18G\t2026-07-24 18:48\nzit-probe1\t-\t-\t8K\t2026-10-07 19:26\nzit-probe2\tT\t/tmp/be_mount.US6e\t0\t2026-10-07 19:26\n";

    fn be(flags: &str) -> BootEnv {
        parse_bootenvs(&format!("x\t{flags}\t-\t1M\td\n")).unwrap().remove(0)
    }

    fn args(runner: &FakeRunner) -> Vec<String> {
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one command: {calls:?}");
        assert_eq!(calls[0].0, "bectl");
        calls[0].1.clone()
    }

    const BEADM_LIST: &str = "omnios-r151054bj;be860a1e-238d-49a6-a18e-289e31cf644a;;;98881536;static;1784923897\n\
omnios-r151058s-backup-1;cfc19085-badb-4aa1-8caa-f6fc2648a1c0;;;187392;static;1789832373\n\
omnios-r151058w;c26a8ba2-fd92-4570-9b3c-4a31bee47709;NR;/;4240252928;static;1791728263\n";

    fn calls(r: &FakeRunner) -> Vec<String> {
        r.calls.lock().unwrap().iter().map(|(c, a)| format!("{c} {}", a.join(" "))).collect()
    }

    #[test]
    fn on_illumos_the_changes_are_beadm_commands_and_on_freebsd_they_are_still_bectl() {
        use crate::zfs::host_health::HostOs::Illumos;
        let r = FakeRunner::new("");
        create(&r, "zit-new", Some("omnios-r151058s"), Some(Illumos)).unwrap();
        create(&r, "zit-clone", None, Some(Illumos)).unwrap();
        activate(&r, "zit-new", false, Some(Illumos)).unwrap();
        activate(&r, "zit-new", true, Some(Illumos)).unwrap();
        clear_temporary(&r, "zit-new", Some(Illumos)).unwrap();
        rename(&r, "zit-new", "zit-renamed", Some(Illumos)).unwrap();
        destroy(&r, "zit-renamed", false, Some(Illumos)).unwrap();
        assert_eq!(calls(&r), ["beadm create -e omnios-r151058s zit-new", "beadm create zit-clone", "beadm activate zit-new", "beadm activate -t zit-new", "beadm activate -T zit-new", "beadm rename zit-new zit-renamed", "beadm destroy -F zit-renamed"]);
        let f = FakeRunner::new("");
        create(&f, "n", Some("d"), None).unwrap();
        destroy(&f, "n", true, Some(crate::zfs::host_health::HostOs::FreeBsd)).unwrap();
        assert_eq!(calls(&f), ["bectl create -e d n", "bectl destroy -o n"]);
    }

    #[test]
    fn what_beadm_cannot_do_the_bectl_way_is_refused_not_guessed() {
        use crate::zfs::host_health::HostOs::Illumos;
        let r = FakeRunner::new("");
        assert!(mount(&r, "x", Some(Illumos)).is_err() && unmount(&r, "x", Some(Illumos)).is_err());
        assert!(destroy(&r, "x", true, Some(Illumos)).is_err(), "destroy + origin has no beadm equivalent");
        assert!(calls(&r).is_empty(), "nothing was run: {:?}", calls(&r));
        assert!(create(&r, "-oops", None, Some(Illumos)).is_err(), "names are checked as before");
    }

    #[test]
    fn beadm_list_h_is_read_into_boot_environments() {
        let bes = parse_beadm_list(BEADM_LIST).unwrap();
        assert_eq!(bes.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["omnios-r151054bj", "omnios-r151058s-backup-1", "omnios-r151058w"]);
        assert_eq!(bes[0].flags, ActiveFlags::default());
        assert_eq!((bes[0].mountpoint.as_deref(), bes[0].space.as_str()), (None, "94.3M"));
        let current = &bes[2];
        assert_eq!((current.flags.now, current.flags.reboot, current.flags.temp), (true, true, false));
        assert_eq!((current.mountpoint.as_deref(), current.space.as_str()), (Some("/"), "3.9G"));
        assert_eq!(current.created.len(), "2026-10-11 10:17".len(), "{}", current.created);
        assert!(current.created.starts_with("2026-10-"), "{}", current.created);
    }

    #[test]
    fn beadm_list_h_survives_blank_lines_garbage_and_temporary_activation() {
        assert!(parse_beadm_list("").unwrap().is_empty() && parse_beadm_list("\n\n").unwrap().is_empty());
        assert!(parse_beadm_list("not a beadm line\n").is_err() || parse_beadm_list("not a beadm line\n").unwrap().is_empty());
        let t = parse_beadm_list("x;u;T;;1024;static;1791728263\n").unwrap();
        assert_eq!((t[0].flags.temp, t[0].flags.now), (true, false));
        let odd = parse_beadm_list("x;u;;;notanumber;static;alsonot\n").unwrap();
        assert_eq!((odd[0].space.as_str(), odd[0].created.as_str()), ("0", "-"), "unreadable numbers are shown, not fatal");
    }

    #[test]
    fn the_listing_tool_follows_the_os_and_beadm_needs_no_sudo() {
        use crate::zfs::host_health::HostOs;
        let omnios = crate::exec::test_support::FakeRunner::new("").route("sh", "-c", BEADM_LIST);
        assert_eq!(list_for(&omnios, Some(HostOs::Illumos)).unwrap().len(), 3);
        assert_eq!(omnios.unprivileged_calls.lock().unwrap().len(), 1, "run without sudo");
        let freebsd = crate::exec::test_support::FakeRunner::new("").route("bectl", "list", "default\tNR\t/\t3.11G\t2026-09-27 17:25\n");
        assert_eq!(list_for(&freebsd, None).unwrap().len(), 1, "an unknown OS asks bectl, as before");
        assert_eq!(list_for(&freebsd, Some(HostOs::FreeBsd)).unwrap().len(), 1);
    }

    #[test]
    fn parses_the_real_list_output() {
        let envs = parse_bootenvs(LIST).unwrap();
        assert_eq!(envs.len(), 3);
        assert_eq!(envs[0].name, "default");
        assert_eq!(envs[0].flags, ActiveFlags { now: true, reboot: true, temp: false });
        assert_eq!(envs[0].mountpoint.as_deref(), Some("/"));
        assert_eq!(envs[0].space, "5.18G");
        assert_eq!(envs[0].created, "2026-07-24 18:48");
        assert_eq!(envs[1].flags, ActiveFlags::default());
        assert_eq!(envs[1].mountpoint, None);
        assert_eq!(envs[1].space, "8K");
        assert_eq!(envs[2].flags, ActiveFlags { now: false, reboot: false, temp: true });
        assert_eq!(envs[2].mountpoint.as_deref(), Some("/tmp/be_mount.US6e"));
        assert_eq!(envs[2].space, "0");
    }

    #[test]
    fn empty_output_is_no_boot_environments_not_an_error() {
        assert_eq!(parse_bootenvs("").unwrap(), vec![]);
        assert_eq!(parse_bootenvs("\n\n").unwrap(), vec![]);
    }

    #[test]
    fn a_malformed_row_fails_the_whole_parse() {
        assert!(parse_bootenvs("default\tNR\t/\t5.18G\n").is_err(), "four fields");
        assert!(parse_bootenvs("default\tNR\t/\t5.18G\td\textra\n").is_err(), "six fields");
        assert!(parse_bootenvs("default\tX\t/\t5.18G\td\n").is_err(), "unknown flag");
        assert!(parse_bootenvs("default\t\t/\t5.18G\td\n").is_err(), "empty flags");
        assert!(parse_bootenvs("\tNR\t/\t5.18G\td\n").is_err(), "empty name");
        assert!(parse_bootenvs("good\tNR\t/\t5G\td\nbad row\n").is_err(), "one bad row poisons the fetch");
    }

    #[test]
    fn flags_decode_to_labels() {
        assert_eq!(be("NR").flags.label(), "Now+Reboot");
        assert_eq!(be("N").flags.label(), "Now");
        assert_eq!(be("R").flags.label(), "Reboot");
        assert_eq!(be("T").flags.label(), "Temp");
        assert_eq!(be("-").flags.label(), "");
    }

    #[test]
    fn predicates_follow_the_flags_and_mount_state() {
        let running = parse_bootenvs("default\tNR\t/\t5G\td\n").unwrap().remove(0);
        assert!(!running.can_activate() && !running.can_destroy() && !running.can_mount() && !running.can_unmount());
        assert!(!running.can_activate_temporarily() && !running.can_clear_temporary());

        let pending = be("R"); // activated for the next boot, not running
        assert!(!pending.can_activate() && !pending.can_destroy());
        assert!(pending.can_activate_temporarily());

        let temp = be("T");
        assert!(temp.can_activate() && temp.can_clear_temporary());
        assert!(!temp.can_destroy(), "its next-boot pointer would dangle: clear it first");
        assert!(!temp.can_activate_temporarily());

        let spare = be("-");
        assert!(spare.can_activate() && spare.can_activate_temporarily() && spare.can_destroy());
        assert!(spare.can_mount() && !spare.can_unmount() && !spare.can_clear_temporary());

        let mounted = parse_bootenvs("m\t-\t/tmp/be_mount.X\t1M\td\n").unwrap().remove(0);
        assert!(mounted.can_unmount() && !mounted.can_mount());
    }

    #[test]
    fn names_are_validated() {
        for ok in ["default", "pre-upgrade_15.1", "be:1", "A9"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "--force", "a b", "a/b", "a@b", "a\tb", "naïve"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn list_runs_bectl_list_h() {
        let runner = FakeRunner::new(LIST);
        assert_eq!(list(&runner).unwrap().len(), 3);
        assert_eq!(args(&runner), vec!["list", "-H"]);
    }

    #[test]
    fn list_surfaces_a_failing_bectl() {
        assert!(list(&FakeRunner::failing("bectl: not found", 127)).is_err());
    }

    #[test]
    fn create_runs_with_and_without_a_clone_source() {
        let runner = FakeRunner::new("");
        create(&runner, "new", None, None).unwrap();
        assert_eq!(args(&runner), vec!["create", "new"]);
        let runner = FakeRunner::new("");
        create(&runner, "new", Some("old"), None).unwrap();
        assert_eq!(args(&runner), vec!["create", "-e", "old", "new"]);
    }

    #[test]
    fn activate_destroy_rename_mount_unmount_build_the_probed_argv() {
        let check = |f: &dyn Fn(&FakeRunner), want: &[&str]| {
            let runner = FakeRunner::new("");
            f(&runner);
            assert_eq!(args(&runner), want);
        };
        check(&|r| activate(r, "a", false, None).unwrap(), &["activate", "a"]);
        check(&|r| activate(r, "a", true, None).unwrap(), &["activate", "-t", "a"]);
        check(&|r| clear_temporary(r, "a", None).unwrap(), &["activate", "-T", "a"]);
        check(&|r| rename(r, "a", "b", None).unwrap(), &["rename", "a", "b"]);
        check(&|r| mount(r, "a", None).unwrap(), &["mount", "a"]);
        check(&|r| unmount(r, "a", None).unwrap(), &["umount", "a"]);
        check(&|r| destroy(r, "a", false, None).unwrap(), &["destroy", "a"]);
        check(&|r| destroy(r, "a", true, None).unwrap(), &["destroy", "-o", "a"]);
    }

    #[test]
    fn a_name_that_looks_like_an_option_never_reaches_bectl() {
        let runner = FakeRunner::new("");
        assert!(create(&runner, "-F", None, None).is_err());
        assert!(create(&runner, "ok", Some("-e"), None).is_err());
        assert!(activate(&runner, "-t", false, None).is_err());
        assert!(clear_temporary(&runner, "-t", None).is_err());
        assert!(rename(&runner, "ok", "-x", None).is_err());
        assert!(rename(&runner, "-x", "ok", None).is_err());
        assert!(mount(&runner, "-x", None).is_err());
        assert!(unmount(&runner, "-x", None).is_err());
        assert!(destroy(&runner, "-F", false, None).is_err());
        assert!(runner.calls.lock().unwrap().is_empty(), "nothing was run");
    }

    #[test]
    fn bectls_own_refusal_text_is_surfaced() {
        let runner = FakeRunner::failing("cannot destroy active boot environment", 5);
        let err = destroy(&runner, "default", false, None).unwrap_err().to_string();
        assert!(err.contains("cannot destroy active boot environment"), "{err}");
    }
}
