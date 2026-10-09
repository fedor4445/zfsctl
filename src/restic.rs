//! The pure parts of restic backups: the retention text, validation of what a job may hand to
//! restic, the argument lists, and reading restic's `--json` output.
//!
//! Every option is passed as `--flag=value`, so a value that starts with `-` can never be read as
//! an option. restic's tag filter is `--tag a,b` = both tags and `--tag a --tag b` = either.

use serde_json::Value;

/// How many backups of each kind to keep; every field `None` keeps all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Retention {
    pub last: Option<u32>,
    pub daily: Option<u32>,
    pub weekly: Option<u32>,
    pub monthly: Option<u32>,
    pub yearly: Option<u32>,
}

impl Retention {
    pub fn is_empty(&self) -> bool {
        *self == Retention::default()
    }

    /// The `--keep-*` flags, in a fixed order.
    pub fn args(&self) -> Vec<String> {
        [("last", self.last), ("daily", self.daily), ("weekly", self.weekly), ("monthly", self.monthly), ("yearly", self.yearly)]
            .into_iter()
            .filter_map(|(name, n)| n.map(|n| format!("--keep-{name}={n}")))
            .collect()
    }
}

/// Parses `7d 4w 12m 3y 5l` (daily, weekly, monthly, yearly, last). Blank means keep everything.
pub fn parse_retention(text: &str) -> Result<Retention, String> {
    let mut r = Retention::default();
    for token in text.split_whitespace() {
        let shape = || format!("'{token}' is not like 7d, 4w, 12m, 3y or 5l");
        let Some(unit) = token.chars().last() else { continue };
        let digits = token.strip_suffix(unit).unwrap_or("");
        let n: u32 = digits.parse().map_err(|_| shape())?;
        if !(1..=9999).contains(&n) {
            return Err(format!("'{token}': the count must be 1 to 9999"));
        }
        let slot = match unit {
            'l' => &mut r.last,
            'd' => &mut r.daily,
            'w' => &mut r.weekly,
            'm' => &mut r.monthly,
            'y' => &mut r.yearly,
            _ => return Err(shape()),
        };
        if slot.is_some() {
            return Err(format!("'{token}': that unit is given twice"));
        }
        *slot = Some(n);
    }
    Ok(r)
}

fn plain(s: &str) -> bool {
    !s.chars().any(|c| matches!(c, '\0' | '\n' | '\r'))
}

/// A restic repository string: non-empty, no surrounding whitespace, no control newlines, and not
/// starting with `-`. It may hold a host or path but must not hold credentials (documented).
pub fn valid_repository(s: &str) -> bool {
    !s.is_empty() && s.trim() == s && !s.starts_with('-') && plain(s) && !has_url_password(s)
}

/// `rest:https://user:secret@host/...`: a password inside the URL would be stored in `jobs.toml` and shown in
/// the Jobs table. A user name alone (`user@host`) and a port (`host:8000`) are fine.
fn has_url_password(s: &str) -> bool {
    let Some((_, rest)) = s.split_once("://") else { return false };
    let authority = rest.split('/').next().unwrap_or("");
    authority.split_once('@').is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

/// An absolute path on the job's host with no empty, `.` or `..` component.
pub fn valid_password_file(s: &str) -> bool {
    s.len() > 1 && s.starts_with('/') && plain(s) && s.split('/').skip(1).all(|c| !c.is_empty() && c != "." && c != "..")
}

fn repo_flags(repo: &str, password_file: &str) -> [String; 2] {
    [format!("--repo={repo}"), format!("--password-file={password_file}")]
}

/// Jobs that share a repository take turns: wait for another run's lock instead of failing at once.
const RETRY_LOCK: &str = "--retry-lock=10m";

/// `restic snapshots` of the backups made from `snapshot` by this job on this machine (both tags must match).
pub fn snapshots_args(repo: &str, password_file: &str, host: &str, job_id: &str, snapshot: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    vec!["snapshots".to_string(), r, p, RETRY_LOCK.to_string(), format!("--host={host}"), "--json".to_string(), format!("--tag=zfsctl:{job_id},zfsctl-snapshot:{snapshot}")]
}

/// `restic backup` of `path`, tagged with the job and the ZFS snapshot it was made from.
pub fn backup_args(repo: &str, password_file: &str, host: &str, job_id: &str, snapshot: &str, time: Option<&str>, path: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    let mut args = vec!["backup".to_string(), r, p, RETRY_LOCK.to_string(), format!("--host={host}"), format!("--tag=zfsctl:{job_id}"), format!("--tag=zfsctl-snapshot:{snapshot}")];
    if let Some(time) = time {
        args.push(format!("--time={time}"));
    }
    args.extend(["--json".to_string(), "--".to_string(), path.to_string()]);
    args
}

/// `restic forget --prune` limited to this job's backups from this host.
pub fn forget_args(repo: &str, password_file: &str, host: &str, job_id: &str, retention: &Retention) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    let mut args = vec!["forget".to_string(), r, p, RETRY_LOCK.to_string(), "--prune".to_string(), format!("--tag=zfsctl:{job_id}"), format!("--host={host}")];
    args.extend(retention.args());
    args
}

/// `restic unlock`: removes only stale locks (restic's own test: a dead process on the same host, or
/// a lock nobody has refreshed for 30 minutes). Never `--remove-all`, which would remove a live one.
pub fn unlock_args(repo: &str, password_file: &str) -> Vec<String> {
    let [r, p] = repo_flags(repo, password_file);
    vec!["unlock".to_string(), r, p]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub new: u64,
    pub changed: u64,
    pub unmodified: u64,
    pub added: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Status { bytes_done: u64, total_bytes: u64 },
    Summary(Summary),
    /// restic's fatal error (`exit_error`).
    Error(String),
    Other,
}

/// Classifies one line of `restic backup --json` output.
pub fn parse_line(line: &str) -> Line {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Line::Other };
    let num = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    match v.get("message_type").and_then(Value::as_str) {
        Some("status") => Line::Status { bytes_done: num("bytes_done"), total_bytes: num("total_bytes") },
        Some("summary") => Line::Summary(Summary { new: num("files_new"), changed: num("files_changed"), unmodified: num("files_unmodified"), added: num("data_added") }),
        Some("exit_error") => Line::Error(v.get("message").and_then(Value::as_str).unwrap_or("restic failed").to_string()),
        _ => Line::Other,
    }
}

/// How many snapshots `restic snapshots --json` listed.
pub fn snapshot_count(json: &str) -> Result<usize, String> {
    serde_json::from_str::<Vec<Value>>(json.trim()).map(|v| v.len()).map_err(|e| format!("unexpected restic snapshots output ({e})"))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A failed restic command's message: an `exit_error` of `--json` output if there is one (stdout or
/// stderr), else stderr, on one line.
pub fn error_text(stdout: &str, stderr: &str) -> String {
    failure_text(exit_error_in(stdout).as_deref(), stderr)
}

/// The message of the first `exit_error` line in `text`, if any.
fn exit_error_in(text: &str) -> Option<String> {
    text.lines().find_map(|l| match parse_line(l) {
        Line::Error(m) => Some(m),
        _ => None,
    })
}

/// "N files could not be read, first: ..." from restic's per-file `error` lines, if there are any.
fn file_errors_summary(text: &str) -> Option<String> {
    let messages: Vec<String> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("message_type").and_then(Value::as_str) == Some("error"))
        .map(|v| {
            let error = v.get("error");
            error.and_then(|e| e.get("message").and_then(Value::as_str).or_else(|| e.as_str())).unwrap_or("unreadable").to_string()
        })
        .collect();
    let first = messages.first()?;
    Some(format!("{} file{} could not be read, first: {first}", messages.len(), if messages.len() == 1 { "" } else { "s" }))
}

/// A message is one line of at most this many characters (it goes into history and the notify hook).
const MAX_TEXT: usize = 400;

fn capped(text: String) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text;
    }
    let mut short: String = text.chars().take(MAX_TEXT - 1).collect();
    short.push('…');
    short
}

/// `fatal` (restic's own `exit_error` message) if there is one, else the `exit_error` in `stderr`
/// (restic prints its JSON errors there), else a summary of its per-file errors, else `stderr` itself:
/// on one line, at most 400 characters.
pub fn failure_text(fatal: Option<&str>, stderr: &str) -> String {
    let chosen = fatal.map(str::to_string).or_else(|| exit_error_in(stderr)).or_else(|| file_errors_summary(stderr));
    let text = one_line(chosen.as_deref().unwrap_or(stderr));
    if text.is_empty() {
        "restic failed".to_string()
    } else {
        capped(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_parses_each_unit_and_builds_the_keep_flags_in_a_fixed_order() {
        let r = parse_retention("7d 4w 12m 3y 5l").unwrap();
        assert_eq!(r, Retention { last: Some(5), daily: Some(7), weekly: Some(4), monthly: Some(12), yearly: Some(3) });
        assert_eq!(r.args(), vec!["--keep-last=5", "--keep-daily=7", "--keep-weekly=4", "--keep-monthly=12", "--keep-yearly=3"]);
        assert_eq!(parse_retention("  30d  ").unwrap().args(), vec!["--keep-daily=30"]);
    }

    #[test]
    fn blank_retention_is_empty_and_keeps_everything() {
        assert!(parse_retention("").unwrap().is_empty());
        assert!(parse_retention("   ").unwrap().is_empty());
        assert!(Retention::default().args().is_empty());
    }

    #[test]
    fn bad_retention_is_refused_with_the_token_named() {
        for bad in ["0d", "7d 7d", "7x", "d", "7", "-3d", "99999d", "1.5d", "7 d", "7dd", "all"] {
            let err = parse_retention(bad).unwrap_err();
            assert!(!err.is_empty(), "{bad}");
        }
        assert!(parse_retention("7x").unwrap_err().contains("7x"));
        assert!(parse_retention("7d 7d").unwrap_err().contains("twice"));
    }

    #[test]
    fn repository_and_password_file_validation() {
        for good in ["/mnt/usb/restic", "sftp:backup@host:/srv/restic", "rclone:b2:bucket/path", "/mnt/my disk/repo"] {
            assert!(valid_repository(good), "{good}");
        }
        for bad in ["", " ", "-r", "--help", "/a\nb", "/a\0b", " /lead", "trail ", "a\rb"] {
            assert!(!valid_repository(bad), "{bad:?}");
        }
        for good in ["/root/.restic-pw", "/etc/zfsctl/pw"] {
            assert!(valid_password_file(good), "{good}");
        }
        for bad in ["", "/", "rel/pw", "-x", "/a/../b", "/a//b", "/a/./b", "/a/", "/a\nb", "/a\0b"] {
            assert!(!valid_password_file(bad), "{bad:?}");
        }
    }

    #[test]
    fn snapshots_args_match_both_tags_with_one_comma_separated_flag() {
        let a = snapshots_args("/repo", "/pw", "fb", "backup-local-tank-data", "hourly-20261008T100000Z");
        assert_eq!(a, vec!["snapshots", "--repo=/repo", "--password-file=/pw", "--retry-lock=10m", "--host=fb", "--json", "--tag=zfsctl:backup-local-tank-data,zfsctl-snapshot:hourly-20261008T100000Z"]);
    }

    #[test]
    fn backup_args_tag_the_job_and_snapshot_and_put_the_path_after_a_double_dash() {
        let a = backup_args("/repo", "/pw", "local", "job1", "snapA", Some("2026-10-08 20:00:00"), "/var/run/zfsctl/job1");
        assert_eq!(a, vec!["backup", "--repo=/repo", "--password-file=/pw", "--retry-lock=10m", "--host=local", "--tag=zfsctl:job1", "--tag=zfsctl-snapshot:snapA", "--time=2026-10-08 20:00:00", "--json", "--", "/var/run/zfsctl/job1"]);
        let no_time = backup_args("-weird", "/pw", "local", "job1", "snapA", None, "/m");
        assert!(!no_time.iter().any(|x| x.starts_with("--time")));
        assert_eq!(no_time.get(1).map(String::as_str), Some("--repo=-weird"), "a leading dash stays inside the flag value");
    }

    #[test]
    fn forget_args_are_limited_to_this_jobs_tag_and_host() {
        let r = parse_retention("7d 4w").unwrap();
        let a = forget_args("/repo", "/pw", "local", "job1", &r);
        assert_eq!(a, vec!["forget", "--repo=/repo", "--password-file=/pw", "--retry-lock=10m", "--prune", "--tag=zfsctl:job1", "--host=local", "--keep-daily=7", "--keep-weekly=4"]);
    }

    #[test]
    fn json_lines_are_classified() {
        assert_eq!(parse_line(r#"{"message_type":"status","percent_done":0.5,"total_bytes":1000,"bytes_done":500}"#), Line::Status { bytes_done: 500, total_bytes: 1000 });
        assert_eq!(
            parse_line(r#"{"message_type":"summary","files_new":3,"files_changed":2,"files_unmodified":40,"data_added":4096,"snapshot_id":"abc"}"#),
            Line::Summary(Summary { new: 3, changed: 2, unmodified: 40, added: 4096 })
        );
        assert_eq!(parse_line(r#"{"message_type":"exit_error","code":10,"message":"Fatal: wrong password or no key found"}"#), Line::Error("Fatal: wrong password or no key found".to_string()));
        assert_eq!(parse_line(r#"{"message_type":"error","error":{"message":"x"}}"#), Line::Other);
        assert_eq!(parse_line("not json at all"), Line::Other);
        assert_eq!(parse_line(""), Line::Other);
    }

    #[test]
    fn snapshot_count_reads_the_json_array() {
        assert_eq!(snapshot_count("[]\n").unwrap(), 0);
        assert_eq!(snapshot_count("[{\"id\":\"a\"},{\"id\":\"b\"}]").unwrap(), 2);
        assert!(snapshot_count("Fatal: nope").is_err());
    }

    #[test]
    fn error_text_prefers_the_json_exit_error_then_stderr() {
        let out = "{\"message_type\":\"error\",\"error\":{}}\n{\"message_type\":\"exit_error\",\"code\":12,\"message\":\"Fatal: wrong password or no key found\"}\n";
        assert_eq!(error_text(out, "ignored"), "Fatal: wrong password or no key found");
        assert_eq!(error_text("", "  Fatal: unable to open repo\nIs there one?\n"), "Fatal: unable to open repo Is there one?");
        assert_eq!(error_text("", ""), "restic failed");
        let multi = "{\"message_type\":\"exit_error\",\"message\":\"Fatal: repository does not exist\\nIs there a repository?\"}";
        assert_eq!(error_text(multi, ""), "Fatal: repository does not exist Is there a repository?");
    }

    #[test]
    fn a_json_exit_error_on_stderr_is_unwrapped_too() {
        let err = "{\"message_type\":\"exit_error\",\"code\":12,\"message\":\"Fatal: wrong password or no key found\"}\n";
        assert_eq!(error_text("", err), "Fatal: wrong password or no key found");
        assert_eq!(failure_text(None, err), "Fatal: wrong password or no key found");
        assert_eq!(failure_text(None, "plain text\n"), "plain text");
        assert_eq!(failure_text(Some("from stdout"), err), "from stdout", "an explicit fatal message wins");
    }

    #[test]
    fn unlock_args_remove_only_stale_locks() {
        let a = unlock_args("/repo", "/pw");
        assert_eq!(a, vec!["unlock", "--repo=/repo", "--password-file=/pw"]);
        assert!(!a.iter().any(|x| x.contains("remove-all")), "never --remove-all: that would remove a live lock");
    }

    #[test]
    fn many_per_file_errors_are_summarised_not_dumped() {
        use std::fmt::Write as _;
        let mut err = String::new();
        for i in 0..50 {
            let _ = writeln!(err, "{{\"message_type\":\"error\",\"error\":{{\"message\":\"open /x/f{i}: permission denied\"}},\"during\":\"archival\",\"item\":\"/x/f{i}\"}}");
        }
        let text = failure_text(None, &err);
        assert!(text.starts_with("50 files could not be read"), "{text}");
        assert!(text.contains("permission denied"), "{text}");
        assert!(text.len() < 300, "{} bytes", text.len());
    }

    #[test]
    fn any_failure_text_is_capped() {
        let long = "x ".repeat(2000);
        let text = failure_text(None, &long);
        assert!(text.chars().count() <= 400 && text.ends_with('…'), "{}", text.chars().count());
        assert_eq!(failure_text(None, "short"), "short");
    }


    #[test]
    fn a_repository_with_a_password_in_its_url_is_refused() {
        for bad in ["rest:https://user:secret@host:8000/repo", "rest:http://u:p@host/", "https://a:b@h"] {
            assert!(!valid_repository(bad), "{bad}");
        }
        for ok in ["rest:https://user@host:8000/repo", "rest:http://host:8000/", "sftp:backup@host:/srv/restic", "s3:https://s3.amazonaws.com/bucket/path", "rclone:b2:bucket", "/mnt/usb/restic"] {
            assert!(valid_repository(ok), "{ok}");
        }
    }
}
