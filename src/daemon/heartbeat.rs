//! `daemon.json`: proof the daemon is alive, rewritten every tick. The TUI
//! reads it to show whether jobs will fire, and a second daemon reads it to
//! refuse to start.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// A heartbeat older than this means the daemon is not running.
pub const STALE_AFTER: Duration = Duration::seconds(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub last_tick: DateTime<Utc>,
}

impl Heartbeat {
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        now - self.last_tick < STALE_AFTER
    }
}

/// Writes the heartbeat atomically, so a reader never sees half of it.
pub fn write(path: &Path, heartbeat: &Heartbeat) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string(heartbeat).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

/// `None` when the file is missing or unreadable.
pub fn read(path: &Path) -> Option<Heartbeat> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The pid of another daemon that is alive: a fresh heartbeat from a process
/// that still exists and is not us. A crashed daemon's heartbeat can still be
/// fresh for a few seconds, so the process check is what lets a supervisor
/// restart it immediately.
pub fn live_other_instance(heartbeat: Option<&Heartbeat>, now: DateTime<Utc>, my_pid: u32, pid_alive: impl Fn(u32) -> bool) -> Option<u32> {
    heartbeat.filter(|h| h.pid != my_pid && h.is_fresh(now) && pid_alive(h.pid)).map(|h| h.pid)
}

/// Whether a process with this pid exists. Signal 0 only checks; `EPERM` means it exists but is not ours.
pub fn process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    let Ok(raw) = i32::try_from(pid) else { return false };
    matches!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None), Ok(()) | Err(Errno::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn beat(pid: u32, last: &str) -> Heartbeat {
        Heartbeat { pid, started_at: at("2026-10-07T00:00:00Z"), last_tick: at(last) }
    }

    #[test]
    fn a_heartbeat_round_trips_through_its_file_and_a_missing_or_corrupt_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        assert!(read(&path).is_none());
        let b = beat(42, "2026-10-07T00:00:05Z");
        write(&path, &b).unwrap();
        assert_eq!(read(&path), Some(b));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn a_heartbeat_goes_stale_after_ten_seconds() {
        let b = beat(1, "2026-10-07T00:00:00Z");
        assert!(b.is_fresh(at("2026-10-07T00:00:09Z")));
        assert!(!b.is_fresh(at("2026-10-07T00:00:10Z")));
    }

    #[test]
    fn another_instance_needs_a_fresh_heartbeat_from_a_live_process_that_is_not_us() {
        let now = at("2026-10-07T00:00:05Z");
        let fresh = beat(7, "2026-10-07T00:00:03Z");
        assert_eq!(live_other_instance(Some(&fresh), now, 99, |_| true), Some(7));
        assert_eq!(live_other_instance(Some(&fresh), now, 7, |_| true), None, "our own pid");
        assert_eq!(live_other_instance(Some(&fresh), now, 99, |_| false), None, "crashed: a supervisor may restart at once");
        assert_eq!(live_other_instance(Some(&beat(7, "2026-10-07T00:00:00Z")), at("2026-10-07T00:01:00Z"), 99, |_| true), None, "stale");
        assert_eq!(live_other_instance(None, now, 99, |_| true), None);
    }

    #[test]
    fn process_alive_is_true_for_this_process_and_false_for_one_that_has_exited() {
        assert!(process_alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }
}
