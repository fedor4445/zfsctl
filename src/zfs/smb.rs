//! Managed block in smb4.conf: pure parsing, rendering, validation.
use crate::error::{AppError, Result};

pub const CONF_PATH: &str = "/usr/local/etc/smb4.conf";
pub const BLOCK_START: &str = "## zfsctl managed shares -- do not edit by hand -- start";
pub const BLOCK_END: &str = "## zfsctl managed shares -- do not edit by hand -- end";
const DATASET_KEY: &str = "# zfsctl-dataset";

/// One managed share. `dataset` is the identity (one share per dataset);
/// `name` is the `[stanza]` label Windows clients see.
#[derive(Debug, Clone, PartialEq)]
pub struct SmbShare {
    pub name: String,
    pub dataset: String,
    pub path: String,
    pub valid_user: Option<String>,
    pub read_only: bool,
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
        if text == BLOCK_START {
            if start.is_some() {
                return Err(smb_err("smb4.conf has more than one zfsctl start marker; fix it by hand"));
            }
            start = Some(offset);
        } else if text == BLOCK_END {
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
    out.push(SmbShare { name: p.name, dataset, path, valid_user: p.valid_user, read_only: p.read_only.unwrap_or(true) });
    Ok(())
}

/// Parses only what lies strictly between the markers. Strict on
/// purpose: anything unrecognised inside the block is an error, because
/// the next write rewrites the whole block and would silently drop it.
pub fn parse_managed(conf: &str) -> Result<Vec<SmbShare>> {
    let Some(span) = locate_block(conf)? else { return Ok(Vec::new()) };
    let lines: Vec<&str> = conf[span.start..span.end].lines().collect();
    let body = &lines[1..lines.len() - 1];
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
        if let Some(rest) = line.strip_prefix(DATASET_KEY) {
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
    let mut out = format!("{BLOCK_START}\n");
    for s in shares {
        out.push_str(&format!("[{}]\n    {DATASET_KEY} = {}\n    path = {}\n", s.name, s.dataset, s.path));
        if let Some(user) = &s.valid_user {
            out.push_str(&format!("    valid users = {user}\n"));
        }
        out.push_str(&format!("    browseable = yes\n    read only = {}\n", if s.read_only { "yes" } else { "no" }));
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
    match locate_block(conf)? {
        Some(span) => Ok(format!("{}{}{}", &conf[..span.start], block, &conf[span.end..])),
        None => {
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
        assert!(parse_managed(&wrap("[a]\n    # zfsctl-dataset = tank/a\n    path = /a\n    guest ok = yes\n")).is_err(), "unknown key");
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
}
