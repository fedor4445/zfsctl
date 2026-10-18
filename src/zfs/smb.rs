//! Managed block in smb4.conf: pure parsing, rendering, validation.
use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

pub const CONF_PATH: &str = "/usr/local/etc/smb4.conf";
pub const BLOCK_START: &str = "## zfsctl managed shares -- do not edit by hand -- start";
pub const BLOCK_END: &str = "## zfsctl managed shares -- do not edit by hand -- end";
const DATASET_KEY: &str = "# zfsctl-dataset";
// What the project wrote under its earlier name (zfsctl). A conf written then is still read; the next write replaces it.
const LEGACY_BLOCK_START: &str = "## zfsctl managed shares -- do not edit by hand -- start";
const LEGACY_BLOCK_END: &str = "## zfsctl managed shares -- do not edit by hand -- end";
const LEGACY_DATASET_KEY: &str = "# zfsctl-dataset";

/// One managed share. `dataset` is the identity (one share per dataset);
/// `name` is the `[stanza]` label Windows clients see.
#[derive(Debug, Clone, PartialEq)]
pub struct SmbShare {
    pub name: String,
    pub dataset: String,
    pub path: String,
    pub valid_user: Option<String>,
    pub read_only: bool,
    /// Anonymous access (`guest ok = yes`). Explicit opt-in; never combined with a `valid_user`.
    pub guest_ok: bool,
}

fn smb_err(msg: impl Into<String>) -> AppError {
    AppError::Smb(msg.into())
}

/// Byte range `[start, end)` covering the START line through the END
/// line's newline.
struct BlockSpan {
    start: usize,
    end: usize,
}

fn locate_block(conf: &str) -> Result<Option<BlockSpan>> {
    let (mut offset, mut start, mut end) = (0usize, None, None);
    for line in conf.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']).trim_end();
        if text == BLOCK_START || text == LEGACY_BLOCK_START {
            if start.is_some() {
                return Err(smb_err("smb4.conf has more than one zfsctl start marker; fix it by hand"));
            }
            start = Some(offset);
        } else if text == BLOCK_END || text == LEGACY_BLOCK_END {
            if start.is_none() {
                return Err(smb_err("smb4.conf has a zfsctl end marker before any start marker; fix it by hand"));
            }
            if end.is_some() {
                return Err(smb_err("smb4.conf has more than one zfsctl end marker; fix it by hand"));
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }
    match (start, end) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) => Ok(Some(BlockSpan { start, end })),
        _ => Err(smb_err("smb4.conf has a zfsctl start marker without an end marker; fix it by hand")),
    }
}

#[derive(Default)]
struct Partial {
    name: String,
    dataset: Option<String>,
    path: Option<String>,
    valid_user: Option<String>,
    read_only: Option<bool>,
    guest_ok: Option<bool>,
}

fn finish(p: Partial, out: &mut Vec<SmbShare>) -> Result<()> {
    let dataset = p.dataset.ok_or_else(|| smb_err(format!("managed share [{}] has no '# zfsctl-dataset' line", p.name)))?;
    let path = p.path.ok_or_else(|| smb_err(format!("managed share [{}] has no 'path'", p.name)))?;
    if out.iter().any(|s| s.name.eq_ignore_ascii_case(&p.name)) {
        return Err(smb_err(format!("managed share name [{}] appears twice", p.name)));
    }
    if out.iter().any(|s| s.dataset == dataset) {
        return Err(smb_err(format!("dataset '{dataset}' has more than one managed share")));
    }
    out.push(SmbShare { name: p.name, dataset, path, valid_user: p.valid_user, read_only: p.read_only.unwrap_or(true), guest_ok: p.guest_ok.unwrap_or(false) });
    Ok(())
}

/// Parses only what lies strictly between the markers. Strict on
/// purpose: anything unrecognised inside the block is an error, because
/// the next write rewrites the whole block and would silently drop it.
pub fn parse_managed(conf: &str) -> Result<Vec<SmbShare>> {
    let Some(span) = locate_block(conf)? else { return Ok(Vec::new()) };
    let lines: Vec<&str> = conf[span.start..span.end].lines().collect();
    // `locate_block` guarantees the start and end marker lines, so there are always two to strip.
    let Some(body) = lines.split_first().and_then(|(_, rest)| rest.split_last()).map(|(_, body)| body) else {
        return Err(smb_err("managed block has no marker lines"));
    };
    let mut shares = Vec::new();
    let mut current: Option<Partial> = None;
    for raw in body {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(p) = current.take() {
                finish(p, &mut shares)?;
            }
            current = Some(Partial { name: line[1..line.len() - 1].trim().to_string(), ..Default::default() });
            continue;
        }
        let Some(p) = current.as_mut() else {
            return Err(smb_err(format!("unexpected line in managed block before any [share]: '{line}'")));
        };
        if let Some(rest) = line.strip_prefix(DATASET_KEY).or_else(|| line.strip_prefix(LEGACY_DATASET_KEY)) {
            let value = rest.trim_start().strip_prefix('=').map(str::trim).filter(|v| !v.is_empty());
            p.dataset = Some(value.ok_or_else(|| smb_err(format!("bad '# zfsctl-dataset' line: '{line}'")))?.to_string());
        } else if line.starts_with('#') || line.starts_with(';') {
            // ordinary comment
        } else if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim().to_lowercase(), value.trim());
            match key.as_str() {
                "path" => p.path = Some(value.to_string()),
                "valid users" => {
                    if value.contains(|c: char| c == ',' || c.is_whitespace()) {
                        return Err(smb_err(format!("[{}] lists several valid users; only one is supported", p.name)));
                    }
                    p.valid_user = if value.is_empty() { None } else { Some(value.to_string()) };
                }
                "browseable" => {}
                "guest ok" => p.guest_ok = Some(!value.eq_ignore_ascii_case("no")),
                "read only" => p.read_only = Some(!value.eq_ignore_ascii_case("no")),
                _ => return Err(smb_err(format!("unrecognised setting in managed block [{}]: '{line}'", p.name))),
            }
        } else {
            return Err(smb_err(format!("unrecognised line in managed block: '{line}'")));
        }
    }
    if let Some(p) = current.take() {
        finish(p, &mut shares)?;
    }
    Ok(shares)
}

fn render_block(shares: &[SmbShare]) -> String {
    use std::fmt::Write;
    // Writing to a `String` cannot fail, so the `fmt::Result`s are ignored.
    let mut out = format!("{BLOCK_START}\n");
    for s in shares {
        let _ = write!(out, "[{}]\n    {DATASET_KEY} = {}\n    path = {}\n", s.name, s.dataset, s.path);
        if let Some(user) = &s.valid_user {
            let _ = writeln!(out, "    valid users = {user}");
        } else if s.guest_ok {
            out.push_str("    guest ok = yes\n");
        }
        let _ = write!(out, "    browseable = yes\n    read only = {}\n", if s.read_only { "yes" } else { "no" });
    }
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

/// Returns `conf` with the managed block replaced by `shares` (or
/// appended if there is none yet). Bytes outside the markers are never
/// altered. Refuses a malformed marker layout rather than guessing.
pub fn render_with_shares(conf: &str, shares: &[SmbShare]) -> Result<String> {
    let block = render_block(shares);
    if let Some(span) = locate_block(conf)? { Ok(format!("{}{}{}", &conf[..span.start], block, &conf[span.end..])) } else {
        let mut out = conf.to_string();
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&block);
        Ok(out)
    }
}

/// Lowercased `[stanza]` names found outside the managed block (so a new
/// managed share can't collide with `[alice]`, `[global]`, ...).
pub fn outside_block_stanza_names(conf: &str) -> Result<Vec<String>> {
    let outside = match locate_block(conf)? {
        Some(span) => format!("{}{}", &conf[..span.start], &conf[span.end..]),
        None => conf.to_string(),
    };
    Ok(outside
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('[') && l.ends_with(']'))
        .map(|l| l[1..l.len() - 1].trim().to_lowercase())
        .collect())
}

pub fn validate_share_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("Share name is required".to_string());
    }
    if name.chars().count() > 80 {
        return Err("Share name is too long (80 characters max)".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Share name may only contain letters, digits, '-' and '_'".to_string());
    }
    if ["global", "homes", "printers"].iter().any(|r| name.eq_ignore_ascii_case(r)) {
        return Err(format!("'{name}' is a reserved Samba section name"));
    }
    Ok(())
}

/// Conservative Unix-account charset. A leading `-` is rejected so a
/// username can never be parsed as an option by `smbpasswd`.
pub fn validate_user(user: &str) -> std::result::Result<(), String> {
    let mut chars = user.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || user.chars().count() > 32 || !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(format!("'{user}' is not a valid Unix username"));
    }
    Ok(())
}

pub fn read_conf(runner: &dyn CommandRunner) -> Result<String> {
    read_conf_state(runner).map(|(conf, _)| conf)
}

/// Like `read_conf`, but also says whether the file existed (`false`
/// means `cat` reported it missing and the returned text is empty).
fn read_conf_state(runner: &dyn CommandRunner) -> Result<(String, bool)> {
    let out = runner.run("cat", &[CONF_PATH])?;
    if out.success() {
        if out.stdout.contains('\u{FFFD}') {
            return Err(smb_err("smb4.conf is not valid UTF-8; zfsctl will not rewrite it"));
        }
        return Ok((out.stdout, true));
    }
    if out.stderr.contains("No such file or directory") {
        return Ok((String::new(), false));
    }
    Err(AppError::CommandFailed { cmd: format!("cat {CONF_PATH}"), status: out.status, stderr: out.stderr })
}

pub fn list_managed(runner: &dyn CommandRunner) -> Result<Vec<SmbShare>> {
    parse_managed(&read_conf(runner)?)
}

fn write_conf(runner: &dyn CommandRunner, content: &str) -> Result<()> {
    let out = runner.run_with_stdin("tee", &[CONF_PATH], content)?;
    if !out.success() {
        return Err(smb_err(format!("could not write {CONF_PATH}: {}", out.stderr.trim())));
    }
    // `tee` echoes its input exactly; anything else means a short or garbled write.
    if out.stdout != content {
        return Err(smb_err(format!("could not write {CONF_PATH}: tee's output did not match what was sent")));
    }
    Ok(())
}

/// Writes `updated`; if that fails, tries to put `original` back and
/// reports both outcomes. When the file did not exist before there is
/// nothing to restore, so the error says it may be partial instead.
fn write_or_restore(runner: &dyn CommandRunner, updated: &str, original: &str, existed: bool) -> Result<()> {
    let Err(first) = write_conf(runner, updated) else { return Ok(()) };
    if !existed {
        return Err(smb_err(format!(
            "{first}; {CONF_PATH} did not exist before, so it could not be restored and may be partial -- check or remove it by hand"
        )));
    }
    match write_conf(runner, original) {
        Ok(()) => Err(smb_err(format!("{first}; the previous smb4.conf was restored"))),
        Err(restore) => Err(smb_err(format!(
            "{first} AND restoring the previous smb4.conf also failed ({restore}); fix {CONF_PATH} by hand"
        ))),
    }
}

/// Reloads Samba; if that fails, puts `previous` back so the file on
/// disk never stays in a state the running daemon rejected. If the file
/// did not exist before, nothing is restored (and none is claimed).
fn reload_or_restore(runner: &dyn CommandRunner, previous: &str, existed: bool) -> Result<()> {
    let out = runner.run("service", &["samba_server", "reload"])?;
    if out.success() {
        return Ok(());
    }
    let reload_msg = out.stderr.trim().to_string();
    if !existed {
        return Err(smb_err(format!(
            "'service samba_server reload' failed ({reload_msg}); {CONF_PATH} did not exist before and was left as written -- remove it by hand if unwanted"
        )));
    }
    match write_conf(runner, previous) {
        Ok(()) => Err(smb_err(format!(
            "'service samba_server reload' failed ({reload_msg}); the previous smb4.conf has been restored"
        ))),
        Err(restore) => Err(smb_err(format!(
            "'service samba_server reload' failed ({reload_msg}) AND restoring the previous smb4.conf also failed ({restore}); fix {CONF_PATH} by hand"
        ))),
    }
}

pub fn set_share(runner: &dyn CommandRunner, share: &SmbShare, password: Option<&str>) -> Result<()> {
    validate_share_name(&share.name).map_err(smb_err)?;
    if let Some(user) = &share.valid_user {
        validate_user(user).map_err(smb_err)?;
    }
    if share.guest_ok && share.valid_user.is_some() {
        return Err(smb_err("guest access can't be combined with a valid user"));
    }
    if share.dataset.is_empty() || share.dataset.chars().any(char::is_control) {
        return Err(smb_err("dataset is empty or contains control characters"));
    }
    if share.path.is_empty() || share.path.chars().any(char::is_control) {
        return Err(smb_err("share path is empty or contains control characters"));
    }
    if share.path == "/" {
        return Err(smb_err("refusing to share the filesystem root '/'"));
    }
    if password == Some("") {
        return Err(smb_err("password must not be empty"));
    }
    if password.is_some_and(|p| p.contains(['\n', '\r'])) {
        return Err(smb_err("password must not contain a line break"));
    }

    let (original, existed) = read_conf_state(runner)?;
    let mut shares = parse_managed(&original)?;
    if outside_block_stanza_names(&original)?.contains(&share.name.to_lowercase()) {
        return Err(smb_err(format!("a share named '{}' already exists in smb4.conf outside zfsctl's managed block", share.name)));
    }
    if shares.iter().any(|s| s.dataset != share.dataset && s.name.eq_ignore_ascii_case(&share.name)) {
        return Err(smb_err(format!("another managed share is already named '{}'", share.name)));
    }
    match shares.iter_mut().find(|s| s.dataset == share.dataset) {
        Some(slot) => *slot = share.clone(),
        None => shares.push(share.clone()),
    }
    let updated = render_with_shares(&original, &shares)?;

    let mut changed_pw_user: Option<&str> = None;
    if let (Some(user), Some(pw)) = (&share.valid_user, password) {
        let out = runner.run_with_stdin("smbpasswd", &["-a", "-s", user], &format!("{pw}\n{pw}\n"))?;
        if !out.success() {
            return Err(smb_err(format!(
                "could not set the Samba password for '{user}' ({}); smb4.conf was not changed",
                out.stderr.trim()
            )));
        }
        changed_pw_user = Some(user);
    }
    write_or_restore(runner, &updated, &original, existed)
        .and_then(|()| reload_or_restore(runner, &original, existed))
        .map_err(|e| match changed_pw_user {
            Some(user) => smb_err(format!("{e}; NOTE: the Samba password for '{user}' was already changed.")),
            None => e,
        })
}

pub fn remove_share(runner: &dyn CommandRunner, dataset: &str) -> Result<()> {
    let (original, existed) = read_conf_state(runner)?;
    let mut shares = parse_managed(&original)?;
    let before = shares.len();
    shares.retain(|s| s.dataset != dataset);
    if shares.len() == before {
        return Err(smb_err(format!("no managed SMB share for dataset '{dataset}'")));
    }
    write_or_restore(runner, &render_with_shares(&original, &shares)?, &original, existed)?;
    reload_or_restore(runner, &original, existed)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const FB_CONF: &str = "[global]\n    workgroup = WORKGROUP\n    security = user\n    map to guest = Bad User\n\n[alice]\n    path = /data/share/alice\n    valid users = alice\n    browseable = yes\n    read only = no\n\n[carol]\n    path = /data/share/carol\n    valid users = carol\n    browseable = yes\n    read only = no\n";

    pub(crate) fn share(name: &str, dataset: &str, user: Option<&str>, ro: bool) -> SmbShare {
        SmbShare {
            name: name.to_string(),
            dataset: dataset.to_string(),
            path: format!("/{dataset}"),
            valid_user: user.map(str::to_string),
            read_only: ro,
            guest_ok: false,
        }
    }

    #[test]
    fn no_block_means_no_managed_shares() {
        assert_eq!(parse_managed(FB_CONF).unwrap(), vec![]);
        assert_eq!(parse_managed("").unwrap(), vec![]);
    }

    #[test]
    fn render_appends_a_block_to_a_file_without_one_and_preserves_every_original_byte() {
        let out = render_with_shares(FB_CONF, &[share("media", "tank/media", Some("carol"), false)]).unwrap();
        assert!(out.starts_with(FB_CONF), "original bytes must be an untouched prefix");
        assert_eq!(parse_managed(&out).unwrap(), vec![share("media", "tank/media", Some("carol"), false)]);
        assert_eq!(out.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn render_handles_empty_file_and_file_without_trailing_newline() {
        let out = render_with_shares("", &[share("a", "tank/a", None, true)]).unwrap();
        assert!(out.starts_with(BLOCK_START));
        let out = render_with_shares("[global]\n  x = y", &[share("a", "tank/a", None, true)]).unwrap();
        assert!(out.starts_with("[global]\n  x = y\n"));
        assert_eq!(parse_managed(&out).unwrap().len(), 1);
    }

    #[test]
    fn render_replaces_an_existing_block_in_place_leaving_before_and_after_untouched() {
        let before = "[global]\n  a = b\n\n";
        let after = "\n[alice]\n  path = /x\n";
        let first = render_with_shares(&format!("{before}{after}"), &[share("one", "tank/one", None, true)]).unwrap();
        // Hand-place text after the block, as a real file could have.
        let with_tail = format!("{first}[tail]\n  path = /tail\n");
        let second = render_with_shares(
            &with_tail,
            &[share("one", "tank/one", None, true), share("two", "tank/two", Some("bob"), false)],
        )
        .unwrap();
        assert!(second.starts_with(before));
        assert!(second.ends_with("[tail]\n  path = /tail\n"));
        assert_eq!(second.matches(BLOCK_START).count(), 1, "must never append a second block");
        assert_eq!(parse_managed(&second).unwrap().len(), 2);
    }

    #[test]
    fn render_with_no_shares_keeps_an_empty_block_and_is_idempotent() {
        let once = render_with_shares(FB_CONF, &[]).unwrap();
        let twice = render_with_shares(&once, &[]).unwrap();
        assert_eq!(once, twice);
        assert_eq!(parse_managed(&once).unwrap(), vec![]);
    }

    #[test]
    fn parse_ignores_unmanaged_stanzas_entirely() {
        let conf = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let names: Vec<String> = parse_managed(&conf).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["media".to_string()]);
    }

    #[test]
    fn unterminated_duplicate_or_reversed_markers_are_refused() {
        let start_only = format!("{FB_CONF}{BLOCK_START}\n");
        assert!(parse_managed(&start_only).is_err());
        assert!(render_with_shares(&start_only, &[]).is_err(), "must not append a second block");
        let two = format!("{BLOCK_START}\n{BLOCK_END}\n{BLOCK_START}\n{BLOCK_END}\n");
        assert!(parse_managed(&two).is_err());
        let reversed = format!("{BLOCK_END}\n{BLOCK_START}\n");
        assert!(parse_managed(&reversed).is_err());
    }

    #[test]
    fn a_malformed_block_is_an_error_not_a_silent_drop() {
        let wrap = |body: &str| format!("{BLOCK_START}\n{body}{BLOCK_END}\n");
        assert!(parse_managed(&wrap("[a]\n    path = /a\n")).is_err(), "missing dataset comment");
        assert!(parse_managed(&wrap("[a]\n    # zfsctl-dataset = tank/a\n")).is_err(), "missing path");
        assert!(parse_managed(&wrap("[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n    wide links = yes\n")).is_err(), "unknown key");
        assert!(parse_managed(&wrap("path = /a\n")).is_err(), "key before any stanza");
        let dup = "[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n[A]\n    # zfsctl-dataset = tank/b\n    path = /b\n";
        assert!(parse_managed(&wrap(dup)).is_err(), "duplicate share name, case-insensitive");
        let multi = "[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n    valid users = x, y\n";
        assert!(parse_managed(&wrap(multi)).is_err(), "multiple users unsupported");
    }

    #[test]
    fn read_only_round_trips_and_defaults_to_samba_semantics() {
        let conf = render_with_shares("", &[share("a", "tank/a", None, true), share("b", "tank/b", None, false)]).unwrap();
        let parsed = parse_managed(&conf).unwrap();
        assert!(parsed[0].read_only);
        assert!(!parsed[1].read_only);
    }

    #[test]
    fn outside_block_names_are_lowercased_and_exclude_managed_ones() {
        let conf = render_with_shares(FB_CONF, &[share("Media", "tank/media", None, true)]).unwrap();
        let names = outside_block_stanza_names(&conf).unwrap();
        assert_eq!(names, vec!["global".to_string(), "alice".to_string(), "carol".to_string()]);
    }

    #[test]
    fn share_name_validation() {
        for ok in ["media", "My-Share_2"] {
            assert!(validate_share_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "has space", "br[ack]et", "a/b", "global", "HOMES", "printers", "ünï"] {
            assert!(validate_share_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_share_name(&"a".repeat(81)).is_err());
    }

    #[test]
    fn user_validation_rejects_flag_lookalikes_and_odd_characters() {
        for ok in ["carol", "carol.s", "_svc", "a-b", "u1"] {
            assert!(validate_user(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "--x", "a b", "a,b", "a\nb", "a;b", "a/b"] {
            assert!(validate_user(bad).is_err(), "{bad:?}");
        }
    }
    use crate::exec::test_support::FakeRunner;
    fn calls(r: &FakeRunner) -> Vec<String> {
        r.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).collect()
    }
    fn tee_writes(r: &FakeRunner) -> Vec<String> {
        r.stdin_calls.lock().unwrap().iter().filter(|(c, _)| c == "tee").map(|(_, s)| s.clone()).collect()
    }
    fn runner_with(conf: &str) -> FakeRunner {
        FakeRunner::new("").route("cat", CONF_PATH, conf)
    }

    #[test]
    fn read_conf_treats_a_missing_file_as_empty_but_other_failures_as_errors() {
        let missing = FakeRunner::new("").route_failure("cat", CONF_PATH, "cat: /usr/local/etc/smb4.conf: No such file or directory", 1);
        assert_eq!(read_conf(&missing).unwrap(), "");
        let denied = FakeRunner::new("").route_failure("cat", CONF_PATH, "sudo: a password is required", 1);
        assert!(read_conf(&denied).is_err());
    }

    #[test]
    fn set_share_create_runs_cat_smbpasswd_tee_reload_in_that_order() {
        let r = runner_with(FB_CONF);
        set_share(&r, &share("media", "tank/media", Some("carol"), false), Some("p w$1")).unwrap();
        assert_eq!(calls(&r), vec!["cat", "smbpasswd", "tee", "service"]);
        let all = r.calls.lock().unwrap();
        assert_eq!(all[1].1, vec!["-a", "-s", "carol"]);
        assert_eq!(all[2].1, vec![CONF_PATH]);
        assert_eq!(all[3].1, vec!["samba_server", "reload"]);
        let stdin = r.stdin_calls.lock().unwrap();
        assert_eq!(stdin[0], ("smbpasswd".to_string(), "p w$1\np w$1\n".to_string()), "password verbatim, doubled, on stdin only");
        assert!(all.iter().all(|(_, args)| args.iter().all(|a| !a.contains("p w$1"))), "password must never appear in an argument");
        let written = &stdin[1].1;
        assert!(written.starts_with(FB_CONF), "bytes outside the block are preserved");
        assert!(written.contains("valid users = carol"));
    }

    #[test]
    fn set_share_without_a_user_or_without_a_password_skips_smbpasswd() {
        let r = runner_with(FB_CONF);
        set_share(&r, &share("pub", "tank/pub", None, true), Some("ignored")).unwrap();
        assert!(!calls(&r).contains(&"smbpasswd".to_string()));
        let r = runner_with(FB_CONF);
        set_share(&r, &share("media", "tank/media", Some("carol"), true), None).unwrap();
        assert!(!calls(&r).contains(&"smbpasswd".to_string()), "blank password on edit = keep the existing one");
    }

    #[test]
    fn set_share_bad_user_fails_before_the_config_is_touched() {
        let r = runner_with(FB_CONF).route_failure("smbpasswd", "-a", "Failed to add entry for user nosuch.", 1);
        let err = set_share(&r, &share("media", "tank/media", Some("nosuch"), false), Some("pw")).unwrap_err().to_string();
        assert!(err.contains("nosuch") && err.contains("not changed"), "{err}");
        assert!(tee_writes(&r).is_empty());
        assert!(!calls(&r).contains(&"service".to_string()));
    }

    #[test]
    fn set_share_edit_replaces_the_dataset_s_stanza_instead_of_adding_a_second_block() {
        let first = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let r = runner_with(&first);
        set_share(&r, &share("renamed", "tank/media", None, false), None).unwrap();
        let written = tee_writes(&r).remove(0);
        let parsed = parse_managed(&written).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "renamed");
        assert_eq!(written.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn set_share_refuses_names_that_collide_with_unmanaged_or_other_managed_stanzas() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("Alice", "tank/a", None, true), None).is_err());
        assert!(set_share(&r, &share("global", "tank/a", None, true), None).is_err());
        assert!(tee_writes(&r).is_empty());
        let first = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        let r = runner_with(&first);
        assert!(set_share(&r, &share("MEDIA", "tank/other", None, true), None).is_err());
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn set_share_validates_its_inputs_even_if_the_ui_did_not() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("bad name", "tank/a", None, true), None).is_err());
        assert!(set_share(&r, &share("ok", "tank/a", Some("-evil"), true), Some("pw")).is_err());
        assert!(set_share(&r, &share("ok", "tank/a", Some("bob"), true), Some("a\nb")).is_err(), "newline would break the stdin protocol");
        assert!(calls(&r).iter().all(|c| c != "smbpasswd" && c != "tee"));
    }

    #[test]
    fn set_share_works_on_a_host_with_no_smb4_conf_yet() {
        let r = FakeRunner::new("").route_failure("cat", CONF_PATH, "cat: x: No such file or directory", 1);
        set_share(&r, &share("a", "tank/a", None, true), None).unwrap();
        assert!(tee_writes(&r)[0].starts_with(BLOCK_START));
    }

    #[test]
    fn set_share_write_failure_is_reported_and_skips_reload() {
        let r = runner_with(FB_CONF).route_failure("tee", CONF_PATH, "Permission denied", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("smb4.conf") && err.contains("Permission denied"), "{err}");
        assert!(!calls(&r).contains(&"service".to_string()));
    }

    #[test]
    fn set_share_reload_failure_restores_the_previous_file_and_says_both_things() {
        let r = runner_with(FB_CONF).route_failure("service", "samba_server", "smbd: invalid config", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("reload") && err.contains("restored"), "{err}");
        let writes = tee_writes(&r);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[1], FB_CONF, "the original bytes are written back");
    }

    #[test]
    fn remove_share_drops_only_that_dataset_and_never_runs_smbpasswd() {
        let both = render_with_shares(FB_CONF, &[share("a", "tank/a", Some("carol"), true), share("b", "tank/b", None, true)]).unwrap();
        let r = runner_with(&both);
        remove_share(&r, "tank/a").unwrap();
        let written = tee_writes(&r).remove(0);
        let left = parse_managed(&written).unwrap();
        assert_eq!(left.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["b"]);
        assert!(written.starts_with(FB_CONF));
        assert!(!calls(&r).contains(&"smbpasswd".to_string()));
        assert_eq!(calls(&r).last().unwrap(), "service");
    }

    #[test]
    fn remove_share_for_an_unknown_dataset_is_an_error_and_changes_nothing() {
        let r = runner_with(FB_CONF);
        assert!(remove_share(&r, "tank/none").is_err());
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn set_share_rejects_empty_or_control_character_datasets() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("ok", "", None, true), None).is_err());
        assert!(set_share(&r, &share("ok", "tank/a\n[evil]", None, true), None).is_err());
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn a_blank_user_without_guest_renders_no_guest_ok_and_parses_back_false() {
        let out = render_with_shares("", &[share("any", "tank/any", None, true)]).unwrap();
        assert!(!out.contains("guest ok"), "{out}");
        assert!(!parse_managed(&out).unwrap()[0].guest_ok);
    }

    #[test]
    fn guest_ok_parses_yes_no_case_insensitively() {
        let mk = |v: &str| format!("{BLOCK_START}\n[a]\n    # zfsctl-dataset = tank/a\n    path = /tank/a\n    guest ok = {v}\n{BLOCK_END}\n");
        assert!(parse_managed(&mk("Yes")).unwrap()[0].guest_ok);
        assert!(!parse_managed(&mk("NO")).unwrap()[0].guest_ok);
        assert!(parse_managed(&mk("true")).unwrap()[0].guest_ok);
    }

    #[test]
    fn set_share_rejects_guest_with_a_valid_user_and_makes_no_calls() {
        let r = runner_with(FB_CONF);
        let mut s = share("x", "tank/x", Some("bob"), true);
        s.guest_ok = true;
        assert!(set_share(&r, &s, None).is_err());
        assert!(tee_writes(&r).is_empty());
        assert_eq!(calls(&r), Vec::<String>::new());
    }

    #[test]
    fn guest_shares_render_guest_ok_and_round_trip() {
        let mut g = share("pub", "tank/pub", None, true);
        g.guest_ok = true;
        let out = render_with_shares("", &[g, share("priv", "tank/priv", Some("bob"), true)]).unwrap();
        let stanzas: Vec<&str> = out.split("[priv]").collect();
        assert!(stanzas[0].contains("guest ok = yes"), "{out}");
        assert!(!stanzas[1].contains("guest ok"), "{out}");
        let parsed = parse_managed(&out).unwrap();
        assert_eq!(parsed[0].valid_user, None);
        assert!(parsed[0].guest_ok);
        assert!(!parsed[1].guest_ok);
        assert_eq!(parsed[1].valid_user.as_deref(), Some("bob"));
    }

    #[test]
    fn non_utf8_conf_is_refused_and_never_rewritten() {
        let r = runner_with("[global]\n  x = \u{FFFD}\n");
        assert!(read_conf(&r).unwrap_err().to_string().contains("UTF-8"));
        assert!(set_share(&r, &share("a", "tank/a", None, true), None).is_err());
        assert!(remove_share(&r, "tank/a").is_err());
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn write_conf_treats_an_echo_mismatch_as_a_failed_write() {
        let r = runner_with(FB_CONF).route("tee", CONF_PATH, "truncated");
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("could not write"), "{err}");
        assert!(!calls(&r).contains(&"service".to_string()));
    }

    #[test]
    fn first_write_failure_attempts_a_restore_and_reports_both() {
        let r = runner_with(FB_CONF).route_failure("tee", CONF_PATH, "disk full", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("disk full") && err.contains("restoring") && err.contains("by hand"), "{err}");
        assert_eq!(tee_writes(&r).len(), 2);
        assert_eq!(tee_writes(&r)[1], FB_CONF);
        // restore succeeds when only the first write is short
        let r = runner_with(FB_CONF).route("tee", CONF_PATH, "truncated");
        let _ = set_share(&r, &share("a", "tank/a", None, true), None);
        assert_eq!(tee_writes(&r).len(), 2);
    }

    #[test]
    fn write_failure_on_an_absent_file_does_not_claim_a_restore() {
        let r = FakeRunner::new("")
            .route_failure("cat", CONF_PATH, "cat: x: No such file or directory", 1)
            .route_failure("tee", CONF_PATH, "Permission denied", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(err.contains("partial") && !err.contains("was restored"), "{err}");
        assert_eq!(tee_writes(&r).len(), 1, "no restore attempt for an absent original");
    }

    #[test]
    fn reload_failure_on_an_absent_file_does_not_claim_a_restore() {
        let r = FakeRunner::new("")
            .route_failure("cat", CONF_PATH, "cat: x: No such file or directory", 1)
            .route_failure("service", "samba_server", "bad", 1);
        let err = set_share(&r, &share("a", "tank/a", None, true), None).unwrap_err().to_string();
        assert!(!err.contains("restored"), "{err}");
        assert_eq!(tee_writes(&r).len(), 1);
    }

    #[test]
    fn errors_after_smbpasswd_succeeded_mention_the_changed_password() {
        let s = share("a", "tank/a", Some("carol"), true);
        let r = runner_with(FB_CONF).route_failure("tee", CONF_PATH, "Permission denied", 1);
        let err = set_share(&r, &s, Some("pw")).unwrap_err().to_string();
        assert!(err.contains("NOTE: the Samba password for 'carol' was already changed."), "{err}");
        let r = runner_with(FB_CONF).route_failure("service", "samba_server", "bad", 1);
        let err = set_share(&r, &s, Some("pw")).unwrap_err().to_string();
        assert!(err.contains("NOTE: the Samba password for 'carol' was already changed."), "{err}");
        let r = runner_with(FB_CONF).route_failure("tee", CONF_PATH, "Permission denied", 1);
        let err = set_share(&r, &s, None).unwrap_err().to_string();
        assert!(!err.contains("NOTE"), "{err}");
    }

    #[test]
    fn set_share_rejects_an_empty_password_and_root_path() {
        let r = runner_with(FB_CONF);
        assert!(set_share(&r, &share("a", "tank/a", Some("bob"), true), Some("")).is_err());
        let mut s = share("a", "tank/a", None, true);
        s.path = "/".to_string();
        assert!(set_share(&r, &s, None).is_err());
        assert!(calls(&r).is_empty());
    }

    fn managed(body: &str) -> String {
        format!("{BLOCK_START}\n{body}{BLOCK_END}\n")
    }

    #[test]
    fn parse_managed_requires_a_stanza_header_to_have_both_brackets() {
        assert!(parse_managed(&managed("[open\n    # zfsctl-dataset = tank/a\n    path = /a\n")).is_err());
        assert!(parse_managed(&managed("[ok]\n    # zfsctl-dataset = tank/a\n    path = /a\nclose]\n")).is_err());
    }

    #[test]
    fn parse_managed_accepts_both_comment_styles_inside_the_block() {
        let conf = managed("[a]\n    # zfsctl-dataset = tank/a\n    # hash comment\n    ; semicolon comment\n    path = /tank/a\n");
        assert_eq!(parse_managed(&conf).unwrap().len(), 1);
        let conf = managed("[a]\n    ; only a semicolon comment\n    # zfsctl-dataset = tank/a\n    path = /tank/a\n");
        assert_eq!(parse_managed(&conf).unwrap().len(), 1);
    }

    #[test]
    fn render_separates_the_block_from_existing_text_with_exactly_one_blank_line() {
        let s = [share("a", "tank/a", None, true)];
        let block = render_block(&s);
        assert_eq!(render_with_shares("[g]\nx\n", &s).unwrap(), format!("[g]\nx\n\n{block}"));
        assert_eq!(render_with_shares("[g]\nx", &s).unwrap(), format!("[g]\nx\n\n{block}"));
        assert_eq!(render_with_shares("", &s).unwrap(), block);
    }

    #[test]
    fn outside_block_stanza_names_ignores_lines_that_only_start_or_only_end_with_a_bracket() {
        let conf = "[Global]\n  a = [b]\n[unterminated\ntrailing]\n [ spaced ]\n";
        assert_eq!(outside_block_stanza_names(conf).unwrap(), vec!["global", "spaced"]);
    }

    #[test]
    fn validate_share_name_length_limit_is_80() {
        assert!(validate_share_name(&"a".repeat(80)).is_ok());
        assert!(validate_share_name(&"a".repeat(81)).is_err());
    }

    #[test]
    fn validate_user_length_limit_is_32() {
        assert!(validate_user(&"a".repeat(32)).is_ok());
        assert!(validate_user(&"a".repeat(33)).is_err());
        assert!(validate_user("").is_err());
        assert!(validate_user("a.b-c_d").is_ok());
        assert!(validate_user("a b").is_err());
    }

    #[test]
    fn set_share_rejects_an_empty_or_control_character_dataset_and_path() {
        let r = runner_with(FB_CONF);
        let mut s = share("ok", "tank/a", None, true);
        s.dataset = String::new();
        assert!(set_share(&r, &s, None).is_err(), "empty dataset");
        s.dataset = "tank/a\n".to_string();
        assert!(set_share(&r, &s, None).is_err(), "control char in dataset");
        let mut s = share("ok", "tank/a", None, true);
        s.path = String::new();
        assert!(set_share(&r, &s, None).is_err(), "empty path");
        s.path = "/tank/a\n".to_string();
        assert!(set_share(&r, &s, None).is_err(), "control char in path");
        assert!(tee_writes(&r).is_empty());
    }

    #[test]
    fn set_share_name_clash_is_only_with_a_different_datasets_share() {
        let first = render_with_shares(FB_CONF, &[share("media", "tank/media", None, true)]).unwrap();
        // Re-saving the same dataset under the same name is an edit, not a clash.
        let r = runner_with(&first);
        set_share(&r, &share("media", "tank/media", None, false), None).unwrap();
        // A different dataset with a different name is fine too.
        let r = runner_with(&first);
        set_share(&r, &share("other", "tank/other", None, true), None).unwrap();
        assert_eq!(parse_managed(&tee_writes(&r)[0]).unwrap().len(), 2);
    }

    #[test]
    fn a_block_written_under_the_old_name_is_read_and_rewritten_with_the_new_markers() {
        let old = format!("{FB_CONF}## zfsctl managed shares -- do not edit by hand -- start\n[media]\n    # zfsctl-dataset = tank/media\n    path = /tank/media\n## zfsctl managed shares -- do not edit by hand -- end\n");
        let shares = parse_managed(&old).unwrap();
        assert_eq!((shares.len(), shares[0].dataset.as_str(), shares[0].path.as_str()), (1, "tank/media", "/tank/media"));
        // Writing it back replaces the old block in place: no second block, no old markers left.
        let new = render_with_shares(&old, &shares).unwrap();
        assert_eq!(new.matches("managed shares").count(), 2, "{new}");
        assert!(new.contains(BLOCK_START) && new.contains(BLOCK_END) && new.contains("# zfsctl-dataset = tank/media"), "{new}");
        assert!(!new.contains("zfsctl"), "{new}");
        // An old start marker beside a new end marker is one block, and two old starts are still refused.
        assert!(locate_block(&format!("{LEGACY_BLOCK_START}\n{BLOCK_END}\n")).unwrap().is_some());
        assert!(locate_block(&format!("{LEGACY_BLOCK_START}\n{BLOCK_START}\n{BLOCK_END}\n")).is_err());
    }
}
