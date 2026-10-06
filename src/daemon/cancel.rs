//! Cancel requests: the TUI drops a file in `<state>/cancel/` and the daemon's one-second loop
//! picks it up. There is no socket; the state directory is the whole interface.

use std::path::Path;

/// The job id made safe for a file name: anything outside `[A-Za-z0-9_.-]` becomes `-`.
pub fn file_name(job_id: &str) -> String {
    job_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect()
}

pub fn request(dir: &Path, job_id: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join(file_name(job_id))).map(|_| ())
}

/// The names of the pending requests; every file found is deleted (a request for a job that is
/// not running is simply discarded by the caller).
pub fn take_requests(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names = Vec::new();
    for e in entries.filter_map(Result::ok) {
        if let Some(name) = e.file_name().to_str() {
            names.push(name.to_string());
        }
        let _ = std::fs::remove_file(e.path());
    }
    names
}

pub fn clear_all(dir: &Path) {
    let _ = take_requests(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_name_is_the_id_made_safe() {
        assert_eq!(file_name("repl-tank-data"), "repl-tank-data");
        assert_eq!(file_name("a/b c"), "a-b-c");
        assert_eq!(file_name("../x"), "..-x");
    }

    #[test]
    fn requests_are_collected_once_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("cancel");
        assert!(take_requests(&sub).is_empty(), "no directory is no requests");
        request(&sub, "job-a").unwrap();
        request(&sub, "job b").unwrap();
        let mut got = take_requests(&sub);
        got.sort();
        assert_eq!(got, ["job-a", "job-b"]);
        assert!(take_requests(&sub).is_empty());
    }
}
