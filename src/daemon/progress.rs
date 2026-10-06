//! Live progress of a running transfer: a small JSON file per run in `<state>/progress/`, written
//! by the daemon once a second and read by the TUI. Files are written atomically, so a reader
//! never sees half of one.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub job_id: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Bytes per second over the last sample.
    pub rate: Option<u64>,
}

fn file(dir: &Path, run_id: &str) -> PathBuf {
    let safe: String = run_id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '-' }).collect();
    dir.join(format!("{safe}.json"))
}

pub fn write(dir: &Path, run_id: &str, p: &Progress) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let path = file(dir, run_id);
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(serde_json::to_string(p).map_err(std::io::Error::other)?.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

/// Every readable progress file; a file that does not parse is ignored.
pub fn read_all(dir: &Path) -> Vec<Progress> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect()
}

pub fn remove(dir: &Path, run_id: &str) {
    let _ = std::fs::remove_file(file(dir, run_id));
}

/// Removes every progress file: the daemon does this at startup, since none of them can belong
/// to a transfer that is still running.
pub fn clear_all(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.filter_map(Result::ok) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `512 B`, `2.0 KiB`, `12.3 GiB`.
#[allow(clippy::cast_precision_loss)]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match UNITS.get(unit) {
        Some(name) if unit > 0 => format!("{value:.1} {name}"),
        _ => format!("{n} B"),
    }
}

/// `sent 12.3 GiB of 40.0 GiB (30%)  85.0 MiB/s  eta 5m 00s`, with whatever is known.
pub fn progress_line(p: &Progress) -> String {
    let mut line = format!("sent {}", human_bytes(p.bytes));
    if let Some(total) = p.total.filter(|t| *t > 0) {
        let _ = write!(line, " of {} ({}%)", human_bytes(total), p.bytes.min(total) * 100 / total);
    }
    if let Some(rate) = p.rate.filter(|r| *r > 0) {
        let _ = write!(line, "  {}/s", human_bytes(rate));
        if let Some(total) = p.total {
            let secs = total.saturating_sub(p.bytes) / rate;
            let _ = write!(line, "  eta {}", crate::ui::jobs::duration_text(i64::try_from(secs).unwrap_or(i64::MAX)));
        }
    }
    line
}

/// Rewrites a run's progress file at most once a second.
pub struct ProgressWriter {
    dir: PathBuf,
    run_id: String,
    job_id: String,
    total: Option<u64>,
    started_at: DateTime<Utc>,
    last: Option<(DateTime<Utc>, u64)>,
}

impl ProgressWriter {
    pub fn new(dir: PathBuf, run_id: String, job_id: String, total: Option<u64>, started_at: DateTime<Utc>) -> Self {
        ProgressWriter { dir, run_id, job_id, total, started_at, last: None }
    }

    pub fn update(&mut self, bytes: u64, now: DateTime<Utc>) {
        let rate = match self.last {
            Some((then, _)) if (now - then).num_milliseconds() < 1000 => return,
            Some((then, before)) => {
                let ms = u64::try_from((now - then).num_milliseconds()).unwrap_or(1).max(1);
                Some(bytes.saturating_sub(before).saturating_mul(1000) / ms)
            }
            None => None,
        };
        self.last = Some((now, bytes));
        let p = Progress { job_id: self.job_id.clone(), bytes, total: self.total, started_at: self.started_at, updated_at: now, rate };
        let _ = write(&self.dir, &self.run_id, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample(bytes: u64, total: Option<u64>, rate: Option<u64>) -> Progress {
        Progress { job_id: "j".into(), bytes, total, started_at: at("2026-10-06T10:00:00Z"), updated_at: at("2026-10-06T10:00:05Z"), rate }
    }

    #[test]
    fn human_bytes_picks_a_sensible_unit() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(13_194_139_533), "12.3 GiB");
    }

    #[test]
    fn the_line_shows_bytes_total_percent_rate_and_eta_when_known() {
        let gib = 1024 * 1024 * 1024;
        let line = progress_line(&sample(12 * gib, Some(40 * gib), Some(100 * 1024 * 1024)));
        assert_eq!(line, "sent 12.0 GiB of 40.0 GiB (30%)  100.0 MiB/s  eta 4m 46s");
        assert_eq!(progress_line(&sample(5, None, None)), "sent 5 B");
        assert!(progress_line(&sample(10, Some(5), Some(1))).contains("(100%)"), "never over 100%");
    }

    #[test]
    fn files_round_trip_atomically_and_bad_ones_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("progress");
        write(&sub, "r1", &sample(1, Some(2), None)).unwrap();
        std::fs::write(sub.join("junk.json"), "not json").unwrap();
        std::fs::write(sub.join("note.txt"), "x").unwrap();
        assert_eq!(read_all(&sub), vec![sample(1, Some(2), None)]);
        assert!(read_all(&dir.path().join("missing")).is_empty());
        remove(&sub, "r1");
        assert!(read_all(&sub).is_empty());
        write(&sub, "r2", &sample(1, None, None)).unwrap();
        clear_all(&sub);
        assert!(std::fs::read_dir(&sub).map_or(true, |mut d| d.next().is_none()));
    }

    #[test]
    fn the_writer_writes_at_most_once_a_second_and_works_out_the_rate() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ProgressWriter::new(dir.path().to_path_buf(), "r1".into(), "j".into(), Some(1000), at("2026-10-06T10:00:00Z"));
        w.update(100, at("2026-10-06T10:00:00Z"));
        assert_eq!(read_all(dir.path())[0].bytes, 100, "the first sample is written at once");
        w.update(150, at("2026-10-06T10:00:00.400Z"));
        assert_eq!(read_all(dir.path())[0].bytes, 100, "within a second nothing is rewritten");
        w.update(1100, at("2026-10-06T10:00:02Z"));
        let p = &read_all(dir.path())[0];
        assert_eq!((p.bytes, p.rate), (1100, Some(500)), "1000 bytes over 2 s");
    }
}
