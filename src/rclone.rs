//! The pure parts of the cloud mirror job: validating a remote and a speed limit, building the `rclone`
//! argument lists, reading its output, and the overlap rule. Running it is `daemon::mirror`'s job.

use chrono::{DateTime, Utc};

fn plain(s: &str) -> bool {
    !s.chars().any(|c| matches!(c, '\0' | '\n' | '\r'))
}

/// An rclone remote path `name:path`: the name is letters, digits, `_` and `-` (not starting with `-`), so a
/// connection string such as `:s3,access_key_id=X:bucket` (credentials in `jobs.toml`) or an option-looking
/// value is never accepted. The path may be empty (`aws:`), may hold spaces, and must not contain a newline.
pub fn valid_remote(s: &str) -> bool {
    let Some((name, path)) = s.split_once(':') else { return false };
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        && s.trim() == s
        && plain(path)
}

/// `--bwlimit`: bytes per second as digits with an optional single `K`, `M` or `G` suffix.
pub fn valid_bwlimit(s: &str) -> bool {
    let digits = s.strip_suffix(['K', 'M', 'G']).unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `remote` with `tail` under it: `aws:bkt/photos` + `current` is `aws:bkt/photos/current`, and a bare
/// `aws:` gives `aws:current`.
pub fn join(remote: &str, tail: &str) -> String {
    let remote = remote.trim_end_matches('/');
    if remote.ends_with(':') {
        format!("{remote}{tail}")
    } else {
        format!("{remote}/{tail}")
    }
}

/// `YYYYMMDDTHHMMSSZ` in UTC: names the `old/` folder of one run.
pub fn run_stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// `rclone sync SOURCE REMOTE/current --backup-dir REMOTE/old/STAMP`: what the sync would replace or delete is
/// moved to the backup directory, a sibling of `current`, never discarded. `--s3-no-check-bucket` stops rclone
/// trying to create the bucket first, which the one-bucket IAM policy of the guide does not allow (it is ignored
/// by other backends, and a missing bucket still fails the run).
pub fn sync_args(source: &str, remote: &str, stamp: &str, bwlimit: Option<&str>) -> Vec<String> {
    let mut args = vec!["sync".to_string(), source.to_string(), join(remote, "current"), "--backup-dir".to_string(), join(remote, &format!("old/{stamp}"))];
    if let Some(limit) = bwlimit {
        args.extend(["--bwlimit".to_string(), limit.to_string()]);
    }
    for a in ["--s3-no-check-bucket", "--use-json-log", "--stats", "1s", "--stats-log-level", "NOTICE", "--log-level", "NOTICE"] {
        args.push(a.to_string());
    }
    args
}

/// `rclone size --json PATH`: counts the regular files a sync would copy (symlinks and empty directories are not files).
pub fn size_args(path: &str) -> Vec<String> {
    vec!["size".to_string(), "--json".to_string(), path.to_string()]
}

pub fn parse_size_count(json: &str) -> Result<u64, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| format!("cannot read rclone size output: {e}"))?;
    value.get("count").and_then(serde_json::Value::as_u64).ok_or_else(|| "rclone size output has no file count".to_string())
}

/// A read-only listing of the remote's top level, with short timeouts, for the form's check.
pub fn lsf_args(remote: &str) -> Vec<String> {
    ["lsf", remote, "--max-depth", "1", "--contimeout", "15s", "--timeout", "30s", "--retries", "1", "--low-level-retries", "1"].map(String::from).to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Stats { bytes: u64, total: u64 },
    Error(String),
    Other,
}

/// One line of `--use-json-log` output: a periodic stats line, an error, or anything else.
pub fn parse_line(line: &str) -> Line {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else { return Line::Other };
    if let Some(stats) = value.get("stats") {
        let n = |k: &str| stats.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
        return Line::Stats { bytes: n("bytes"), total: n("totalBytes") };
    }
    match (value.get("level").and_then(serde_json::Value::as_str), value.get("msg").and_then(serde_json::Value::as_str)) {
        (Some("error" | "critical"), Some(msg)) => Line::Error(msg.to_string()),
        _ => Line::Other,
    }
}

/// rclone quotes the remote in its messages; a message shown to the user never carries it.
pub fn redact(text: &str, remote: &str) -> String {
    if remote.is_empty() {
        text.to_string()
    } else {
        text.replace(remote, "<remote>")
    }
}

/// One line, at most 400 characters, with the remote masked.
pub fn failure_text(fatal: Option<&str>, stderr: &str, remote: &str) -> String {
    let tail: String = stderr.lines().rev().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('{')).unwrap_or("").to_string();
    let text = fatal.map(str::to_string).unwrap_or(tail);
    let text: String = redact(&text.split_whitespace().collect::<Vec<_>>().join(" "), remote).chars().take(400).collect();
    if text.is_empty() {
        "rclone failed".to_string()
    } else {
        text
    }
}

/// `rclone config file`: prints where root's rclone configuration (the cloud keys) lives.
pub fn config_file_args() -> Vec<String> {
    vec!["config".to_string(), "file".to_string()]
}

/// The path out of `rclone config file`'s output ("Configuration file is stored at:" and then the path).
pub fn parse_config_file(out: &str) -> Option<String> {
    let path = out.lines().map(str::trim).rev().find(|l| !l.is_empty())?;
    (path.starts_with('/') && !path.contains(' ')).then(|| path.to_string())
}

/// Advice about the rclone configuration file's mode and owner: it holds the cloud keys. `None` when only root has access.
pub fn config_permission_note(path: &str, mode: u32, owner: &str) -> Option<String> {
    let mut problems = Vec::new();
    if mode & 0o077 != 0 {
        problems.push(format!("the rclone configuration holds your cloud keys and can be read or changed by other users (mode {:03o}): run `chmod 600 {path}`", mode & 0o777));
    }
    if owner != "root" {
        problems.push(format!("the rclone configuration is owned by {owner}, not root: run `chown root {path}`"));
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Said when the remote opens but the configuration file's permissions could not be read from here.
pub const CONFIG_UNCHECKED: &str = "the rclone configuration's permissions could not be checked from here: make sure only root can read it";

/// A warning when the remote path ends in one of the folders a mirror creates itself.
pub fn own_folder_warning(remote: &str) -> Option<String> {
    let last = remote.split(':').nth(1)?.trim_end_matches('/').rsplit('/').next()?;
    matches!(last, "current" | "old").then(|| format!("this path ends in '{last}', which a mirror creates itself below the path you give: use the parent as the remote"))
}

/// What the form's remote check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Ok,
    /// One plain sentence saying what is wrong and what to do.
    Problem(String),
}

/// Turns the outcome of `lsf_args` into a finding. Anything not recognised passes rclone's own line through,
/// with the remote masked: a wrong guess here must never hide a problem.
pub fn classify_check(status: i32, stdout: &str, stderr: &str, remote: &str, host: &str) -> Check {
    if status == 0 {
        return Check::Ok;
    }
    let lower = format!("{stderr}\n{stdout}").to_lowercase();
    let has = |s: &str| lower.contains(s);
    let name = remote.split(':').next().unwrap_or("");
    let problem = if has("sudo:") && (has("password is required") || has("not allowed") || has("command not found")) {
        format!("rclone is not installed on {host}, or its sudoers entry does not allow it (run `zfsctl print-sudoers {host} --with rclone`)")
    } else if has("executable file not found") || has("rclone: not found") || has("rclone: command not found") {
        format!("rclone is not installed on {host}")
    } else if has("didn't find section in config file") {
        format!("rclone remote '{name}' is not in root's rclone config on {host} (create it as root with `rclone config`)")
    } else if has("you must use bucket") {
        "this key is limited to other bucket(s): use the bucket the key was made for (the name in the remote after the colon)".to_string()
    } else if has("accessdenied") || has("access denied") || has("forbidden") || has("invalidaccesskey") || has("signaturedoesnotmatch") || has("unauthorized") || has("403") || has("401") {
        "the cloud refused the keys (access denied): check the key's permissions for this bucket".to_string()
    } else if has("nosuchbucket") || has("directory not found") || has("bucket not found") || has("container not found") || has("not found") {
        "the bucket or folder was not found: a folder is created by the first run, but a bucket must exist already".to_string()
    } else {
        failure_text(None, stderr, remote)
    };
    Check::Problem(problem)
}

fn split(remote: &str) -> (&str, Vec<&str>) {
    let (name, path) = remote.split_once(':').unwrap_or((remote, ""));
    (name, path.split('/').filter(|s| !s.is_empty()).collect())
}

/// Whether two remote paths are the same place or one holds the other. The same remote name is required:
/// different names are treated as different places (two names for one bucket cannot be told apart).
pub fn overlaps(a: &str, b: &str) -> bool {
    let ((na, pa), (nb, pb)) = (split(a), split(b));
    na == nb && (pa.starts_with(&pb) || pb.starts_with(&pa))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE_JSON: &str = "{\"count\":2,\"bytes\":12,\"sizeless\":0}\n";
    const STATS_LINE: &str = r#"{"time":"2026-10-16T22:51:27-04:00","level":"notice","msg":"\nTransferred: ...","stats":{"bytes":2125824,"checks":0,"errors":0,"speed":1077061.7,"totalBytes":4194304,"totalChecks":0},"source":"accounting/stats.go:549"}"#;
    const SYMLINK_LINE: &str = r#"{"time":"2026-10-16T22:51:15-04:00","level":"notice","msg":"Can't follow symlink without -L/--copy-links","object":"link","objectType":"*local.Object","source":"local/local.go:1354"}"#;
    const ERROR_LINE: &str = r#"{"time":"2026-10-16T22:51:15-04:00","level":"error","msg":"big: Failed to copy: permission denied","object":"big","source":"x"}"#;
    const UNKNOWN_REMOTE: &str = "2026/10/16 22:51:16 CRITICAL: Failed to create file system for \"nosuchremote:bkt\": didn't find section in config file (\"nosuchremote\")\n";
    const MISSING_DIR: &str = "2026/10/16 22:51:16 ERROR : error listing: directory not found\n2026/10/16 22:51:16 NOTICE: Failed to lsf with 2 errors: last error was: error in ListJSON: directory not found\n";

    #[test]
    fn remotes_are_name_colon_path_without_options_or_connection_strings() {
        for good in ["aws:bkt/p", "b2:bkt", "aws:", "my-remote_1:a/b c", "aws:bkt/photos 2026"] {
            assert!(valid_remote(good), "{good}");
        }
        for bad in [":s3,access_key_id=X:b/p", "-x:b", "a,b:c", "nocolon", "aws:b\nx", "", ":", "a b:c", " aws:b", "aws:b "] {
            assert!(!valid_remote(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_speed_limit_is_digits_with_one_optional_unit() {
        for good in ["5M", "500K", "1G", "100"] {
            assert!(valid_bwlimit(good), "{good}");
        }
        for bad in ["5MB", "M", "-1", "1.5M", "0x10", "", " 5M", "5m"] {
            assert!(!valid_bwlimit(bad), "{bad:?}");
        }
    }

    #[test]
    fn join_puts_a_folder_under_the_remote() {
        assert_eq!(join("aws:bkt/photos", "current"), "aws:bkt/photos/current");
        assert_eq!(join("aws:bkt/photos/", "current"), "aws:bkt/photos/current");
        assert_eq!(join("aws:", "current"), "aws:current");
    }

    #[test]
    fn the_stamp_is_utc_to_the_second() {
        let at = DateTime::parse_from_rfc3339("2026-10-17T03:05:09Z").unwrap().with_timezone(&Utc);
        assert_eq!(run_stamp(at), "20261017T030509Z");
    }

    #[test]
    fn sync_moves_what_it_replaces_into_a_sibling_old_folder_and_keeps_each_value_one_argument() {
        let args = sync_args("/var/run/zfsctl/j/my photos", "aws:my bkt/p", "20261017T030509Z", None);
        assert_eq!(args[..5], ["sync", "/var/run/zfsctl/j/my photos", "aws:my bkt/p/current", "--backup-dir", "aws:my bkt/p/old/20261017T030509Z"]);
        assert!(!args.contains(&"--bwlimit".to_string()));
        assert!(args.contains(&"--use-json-log".to_string()));
        assert!(args.contains(&"--s3-no-check-bucket".to_string()));
        let limited = sync_args("/s", "b2:bkt", "S", Some("5M"));
        let i = limited.iter().position(|a| a == "--bwlimit").unwrap();
        assert_eq!(limited[i + 1], "5M");
        for forbidden in ["--delete-excluded", "--links", "-L", "--copy-links"] {
            assert!(!limited.contains(&forbidden.to_string()), "{forbidden}");
        }
    }

    #[test]
    fn size_counts_files_and_a_symlink_only_tree_counts_zero() {
        assert_eq!(size_args("/x"), ["size", "--json", "/x"]);
        assert_eq!(parse_size_count(SIZE_JSON), Ok(2));
        assert_eq!(parse_size_count("{\"count\":0,\"bytes\":0,\"sizeless\":0}"), Ok(0));
        assert!(parse_size_count("not json").is_err() && parse_size_count("{}").is_err());
    }

    #[test]
    fn lines_are_stats_errors_or_other() {
        assert_eq!(parse_line(STATS_LINE), Line::Stats { bytes: 2_125_824, total: 4_194_304 });
        assert_eq!(parse_line(ERROR_LINE), Line::Error("big: Failed to copy: permission denied".into()));
        assert_eq!(parse_line(SYMLINK_LINE), Line::Other, "a skipped symlink is only a notice");
        assert_eq!(parse_line("2026/10/16 NOTICE: plain text"), Line::Other);
    }

    #[test]
    fn the_remote_is_masked_in_messages() {
        assert_eq!(failure_text(Some("error listing aws:bkt/p: boom"), "", "aws:bkt/p"), "error listing <remote>: boom");
        assert_eq!(failure_text(None, "{\"level\":\"notice\"}\nfinal line aws:bkt/p failed\n", "aws:bkt/p"), "final line <remote> failed");
        assert_eq!(failure_text(None, "", "aws:b"), "rclone failed");
    }

    #[test]
    fn a_check_says_in_plain_words_what_is_wrong() {
        let problem = |s, o, e| match classify_check(s, o, e, "nosuchremote:bkt", "fbsd16") {
            Check::Problem(p) => p,
            Check::Ok => panic!("expected a problem"),
        };
        assert_eq!(classify_check(0, "current/\nold/\n", "", "aws:b", "h"), Check::Ok);
        assert!(problem(1, "", UNKNOWN_REMOTE).contains("remote 'nosuchremote' is not in root's rclone config on fbsd16"));
        assert!(problem(3, "", MISSING_DIR).contains("bucket or folder was not found"));
        assert!(problem(1, "", "403 Forbidden: AccessDenied").contains("refused the keys"));
        assert!(problem(1, "", "you must use bucket(s) [{\"7ef2\" \"zfsctl\"}] with this application key").contains("limited to other bucket"));
        assert!(problem(1, "", "sudo: a password is required").contains("print-sudoers fbsd16 --with rclone"));
        assert!(problem(1, "", "exec: \"rclone\": executable file not found in $PATH").contains("not installed"));
        assert_eq!(problem(1, "", "weird nosuchremote:bkt failure"), "weird <remote> failure");
    }

    #[test]
    fn the_config_file_path_and_its_permissions_are_read() {
        assert_eq!(parse_config_file("Configuration file is stored at:\n/root/.config/rclone/rclone.conf\n"), Some("/root/.config/rclone/rclone.conf".to_string()));
        assert_eq!(parse_config_file("Configuration file doesn't exist, but rclone will use this path:\n/root/x y/rclone.conf\n"), None);
        assert_eq!(parse_config_file(""), None);
        assert_eq!(config_permission_note("/root/rc.conf", 0o600, "root"), None);
        let note = config_permission_note("/root/rc.conf", 0o644, "fb").unwrap();
        assert!(note.contains("chmod 600 /root/rc.conf") && note.contains("chown root"), "{note}");
    }

    #[test]
    fn a_remote_ending_in_current_or_old_is_warned_about() {
        assert!(own_folder_warning("aws:bkt/photos/current").unwrap().contains("'current'"));
        assert!(own_folder_warning("aws:bkt/old/").unwrap().contains("'old'"));
        assert_eq!(own_folder_warning("aws:bkt/photos"), None);
        assert_eq!(own_folder_warning("aws:"), None);
    }

    #[test]
    fn overlap_is_same_remote_name_and_one_path_holding_the_other() {
        assert!(overlaps("aws:b/p", "aws:b/p"));
        assert!(overlaps("aws:b/p", "aws:b/p/q"));
        assert!(overlaps("aws:b/p/q", "aws:b/p"));
        assert!(overlaps("aws:b/p/", "aws:b/p"));
        assert!(overlaps("aws:", "aws:b"));
        assert!(!overlaps("aws:b/p", "aws:b/pq"));
        assert!(!overlaps("aws:b/p", "b2:b/p"));
    }
}
