//! When a backup job last checked its restic repository. `restic check` verifies the structure of the repository (the index,
//! the snapshots and the trees they point to), which is what tells you that a backup can still be restored from; it does not
//! read every stored byte. A backup job runs it after a successful backup once its last check is older than the job's
//! interval, or failed, and `restic-checks.json` next to `replication.json` remembers the outcome.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

/// The outcome of a job's latest check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    pub at: DateTime<Utc>,
    pub ok: bool,
}

/// Two backup jobs can finish at the same moment; their read-modify-write of the state file must not interleave.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// The recorded state; a missing or unreadable file counts as nothing recorded (every job is then due once).
pub fn read_state(path: &Path) -> BTreeMap<String, Checked> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn record(path: &Path, job_id: &str, checked: Checked) -> std::io::Result<()> {
    let _guard = STATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut state = read_state(path);
    state.insert(job_id.to_string(), checked);
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Whether a check is due: never when the interval is 0; otherwise when nothing is recorded, the last check failed (so a
/// problem keeps the job red until it is fixed) or the last one is `days` old.
pub fn due(last: Option<&Checked>, days: u32, now: DateTime<Utc>) -> bool {
    days > 0 && last.is_none_or(|c| !c.ok || now - c.at >= Duration::days(i64::from(days)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-{day:02}T03:00:00Z")).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_check_is_due_when_never_done_failed_or_old_and_never_when_the_interval_is_zero() {
        let ok = Checked { at: at(1), ok: true };
        assert!(due(None, 30, at(2)), "never checked");
        assert!(!due(Some(&ok), 30, at(30)), "29 days old");
        assert!(due(Some(&ok), 30, at(31)), "30 days old");
        assert!(due(Some(&Checked { at: at(1), ok: false }), 30, at(2)), "a failed check is repeated at the next run");
        assert!(!due(None, 0, at(2)) && !due(Some(&Checked { at: at(1), ok: false }), 0, at(30)), "0 means never");
        assert!(due(Some(&ok), 1, at(2)));
    }

    #[test]
    fn the_state_is_written_per_job_and_a_damaged_file_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restic-checks.json");
        assert!(read_state(&path).is_empty());
        record(&path, "a", Checked { at: at(1), ok: true }).unwrap();
        record(&path, "b", Checked { at: at(2), ok: false }).unwrap();
        record(&path, "a", Checked { at: at(3), ok: true }).unwrap();
        let state = read_state(&path);
        assert_eq!((state["a"].at, state["a"].ok, state["b"].ok), (at(3), true, false));
        std::fs::write(&path, "not json").unwrap();
        assert!(read_state(&path).is_empty());
    }
}
