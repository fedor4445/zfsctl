//! Pipes one child's stdout into another's stdin and counts the bytes: the hub-side half of
//! `zfs send | zfs receive`, with no shell involved. A cancel flag kills both children.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub bytes: u64,
    pub cancelled: bool,
    pub send_ok: bool,
    pub recv_ok: bool,
    pub send_stderr: String,
    pub recv_stderr: String,
}

const BUFFER: usize = 256 * 1024;
const TICK: Duration = Duration::from_millis(200);

fn drain_stderr(child: &mut Child) -> std::thread::JoinHandle<String> {
    let pipe = child.stderr.take();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_string(&mut text);
        }
        text
    })
}

/// Runs `send` and `recv`, copying send's stdout to recv's stdin until it ends. `on_progress` is
/// called with the byte count about every 200 ms. Setting `cancel` kills both children.
pub fn pump(mut send: Command, mut recv: Command, cancel: &AtomicBool, mut on_progress: impl FnMut(u64)) -> std::io::Result<Transfer> {
    send.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    recv.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut sender = send.spawn()?;
    let mut receiver = match recv.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = sender.kill();
            let _ = sender.wait();
            return Err(e);
        }
    };
    let (Some(mut from), Some(mut to)) = (sender.stdout.take(), receiver.stdin.take()) else {
        let _ = sender.kill();
        let _ = receiver.kill();
        let _ = sender.wait();
        let _ = receiver.wait();
        return Err(std::io::Error::other("could not open the pipes"));
    };
    let send_log = drain_stderr(&mut sender);
    let recv_log = drain_stderr(&mut receiver);

    let total_bytes = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&total_bytes);
    let copier = std::thread::spawn(move || {
        let mut buf = vec![0_u8; BUFFER];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.get(..n).is_none_or(|chunk| to.write_all(chunk).is_err()) {
                        break;
                    }
                    counter.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                }
            }
        }
        // Dropping `to` closes the receive's stdin, which is how it learns the stream ended.
    });

    let mut cancelled = false;
    while !copier.is_finished() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = sender.kill();
            let _ = receiver.kill();
            break;
        }
        on_progress(total_bytes.load(Ordering::Relaxed));
        std::thread::sleep(TICK);
    }
    let _ = copier.join();
    on_progress(total_bytes.load(Ordering::Relaxed));

    let recv_status = receiver.wait()?;
    if !recv_status.success() {
        // The receive is gone: whatever the send is still doing is pointless.
        let _ = sender.kill();
    }
    let send_status = sender.wait()?;
    Ok(Transfer {
        bytes: total_bytes.load(Ordering::Relaxed),
        cancelled,
        // A killed send is simply not ok; callers check `cancelled` first, and when the receive
        // failed they report the receive's message first.
        send_ok: send_status.success(),
        recv_ok: recv_status.success(),
        send_stderr: send_log.join().unwrap_or_default(),
        recv_stderr: recv_log.join().unwrap_or_default(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    pub bytes: u64,
    pub cancelled: bool,
    pub send_ok: bool,
    pub send_stderr: String,
    /// A write or fsync error on the destination file (disk full, ...), if there was one.
    pub write_error: Option<String>,
}

/// Runs `send` and writes its stdout to `file` (fsynced at the end), counting the bytes: the hub-side
/// half of a replication to a stream file. `on_progress` is called about every 200 ms. Setting
/// `cancel` kills the child; a failed write stops the copy and kills it too.
pub fn pump_to_file(mut send: Command, mut file: std::fs::File, cancel: &AtomicBool, mut on_progress: impl FnMut(u64)) -> std::io::Result<FileTransfer> {
    send.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut sender = send.spawn()?;
    let Some(mut from) = sender.stdout.take() else {
        let _ = sender.kill();
        let _ = sender.wait();
        return Err(std::io::Error::other("could not open the pipe"));
    };
    let send_log = drain_stderr(&mut sender);

    let total_bytes = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&total_bytes);
    let copier = std::thread::spawn(move || -> Option<String> {
        let mut buf = vec![0_u8; BUFFER];
        loop {
            match from.read(&mut buf) {
                Ok(0) => break,
                Err(e) => return Some(format!("reading the stream failed: {e}")),
                Ok(n) => {
                    let Some(chunk) = buf.get(..n) else { break };
                    if let Err(e) = file.write_all(chunk) {
                        return Some(e.to_string());
                    }
                    counter.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                }
            }
        }
        file.sync_all().err().map(|e| format!("syncing failed: {e}"))
    });

    let mut cancelled = false;
    while !copier.is_finished() {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = sender.kill();
            break;
        }
        on_progress(total_bytes.load(Ordering::Relaxed));
        std::thread::sleep(TICK);
    }
    let write_error = copier.join().unwrap_or_else(|_| Some("the writer thread panicked".to_string()));
    on_progress(total_bytes.load(Ordering::Relaxed));
    if write_error.is_some() {
        // Nothing can be stored any more: whatever the send is still doing is pointless.
        let _ = sender.kill();
    }
    let send_status = sender.wait()?;
    Ok(FileTransfer {
        bytes: total_bytes.load(Ordering::Relaxed),
        cancelled,
        send_ok: send_status.success(),
        send_stderr: send_log.join().unwrap_or_default(),
        write_error,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    pub cancelled: bool,
    /// The exit code; `None` when the child was killed by a signal.
    pub code: Option<i32>,
    pub stderr: String,
}

/// Runs `cmd` and hands each line of its stdout to `on_line` as it arrives. Setting `cancel` kills the
/// child; the call still returns once its output ends.
pub fn run_lines(mut cmd: Command, cancel: &AtomicBool, mut on_line: impl FnMut(&str)) -> std::io::Result<Lines> {
    use std::io::BufRead;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("could not open the pipe"));
    };
    let log = drain_stderr(&mut child);
    let (tx, rx) = channel::<String>();
    let reader = std::thread::spawn(move || {
        // Bytes, not `lines()`: a line that is not UTF-8 (a Latin-1 file name) must not end the read or vanish.
        let mut reader = std::io::BufReader::new(stdout);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&raw);
            let line = text.strip_suffix('\n').unwrap_or(&text);
            if tx.send(line.strip_suffix('\r').unwrap_or(line).to_string()).is_err() {
                break;
            }
        }
    });
    let mut cancelled = false;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(line) => on_line(&line),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !cancelled && cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = child.kill();
        }
    }
    let _ = reader.join();
    let status = child.wait()?;
    Ok(Lines { cancelled, code: status.code(), stderr: log.join().unwrap_or_default() })
}

/// Like `run_lines`, for a command that logs to stderr (rclone's `--use-json-log`): lines of stderr reach
/// `on_line` as they arrive too, and the whole of stderr is still returned.
pub fn run_lines_merged(mut cmd: Command, cancel: &AtomicBool, mut on_line: impl FnMut(&str)) -> std::io::Result<Lines> {
    use std::io::BufRead;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("could not open the pipes"));
    };
    let (tx, rx) = channel::<(bool, String)>();
    let out_tx = tx.clone();
    let out = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            if out_tx.send((false, line)).is_err() {
                break;
            }
        }
    });
    let err = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            if tx.send((true, line)).is_err() {
                break;
            }
        }
    });
    let (mut cancelled, mut log) = (false, String::new());
    loop {
        match rx.recv_timeout(TICK) {
            Ok((from_stderr, line)) => {
                if from_stderr {
                    log.push_str(&line);
                    log.push('\n');
                }
                on_line(&line);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !cancelled && cancel.load(Ordering::SeqCst) {
            cancelled = true;
            let _ = child.kill();
        }
    }
    let _ = (out.join(), err.join());
    let status = child.wait()?;
    Ok(Lines { cancelled, code: status.code(), stderr: log })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_lines_reach_the_callback_from_both_streams_and_stderr_is_returned() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out1; echo err1 >&2; exit 3"]);
        let mut seen = Vec::new();
        let lines = run_lines_merged(cmd, &AtomicBool::new(false), |l| seen.push(l.to_string())).unwrap();
        seen.sort();
        assert_eq!(seen, ["err1", "out1"]);
        assert_eq!((lines.code, lines.stderr.as_str(), lines.cancelled), (Some(3), "err1\n", false));
    }

    #[test]
    fn merged_run_can_be_cancelled() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let lines = run_lines_merged(cmd, &AtomicBool::new(true), |_| {}).unwrap();
        assert!(lines.cancelled);
    }

    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn the_bytes_arrive_intact_and_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let cancel = AtomicBool::new(false);
        let mut last = 0;
        let t = pump(sh("head -c 3000000 /dev/zero; printf END"), sh(&format!("cat > {}", out.display())), &cancel, |b| last = b).unwrap();
        assert!(t.send_ok && t.recv_ok && !t.cancelled, "{t:?}");
        assert_eq!(t.bytes, 3_000_003);
        let written = std::fs::read(&out).unwrap();
        assert_eq!((written.len(), written.ends_with(b"END")), (3_000_003, true));
        assert!(last <= t.bytes);
    }

    #[test]
    fn a_failing_send_is_reported_with_its_stderr() {
        let cancel = AtomicBool::new(false);
        let t = pump(sh("echo boom >&2; exit 3"), sh("cat > /dev/null"), &cancel, |_| {}).unwrap();
        assert!(!t.send_ok && t.recv_ok, "{t:?}");
        assert!(t.send_stderr.contains("boom"), "{t:?}");
    }

    #[test]
    fn a_receive_that_exits_early_fails_the_run_promptly_with_its_message() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let t = pump(sh("yes | head -c 500000000"), sh("echo nope >&2; exit 4"), &cancel, |_| {}).unwrap();
        assert!(!t.recv_ok, "{t:?}");
        assert!(t.recv_stderr.contains("nope"), "{t:?}");
        assert!(started.elapsed() < Duration::from_secs(10), "the send must not be left blocked on a full pipe");
    }

    #[test]
    fn cancel_kills_both_children_and_returns_cancelled() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let t = pump(sh("sleep 60"), sh("cat > /dev/null"), &cancel, |_| {}).unwrap();
        setter.join().unwrap();
        assert!(t.cancelled, "{t:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_command_that_cannot_start_is_an_error_and_leaves_nothing_running() {
        let cancel = AtomicBool::new(false);
        let missing = Command::new("/nonexistent/zfsctl-test-binary");
        assert!(pump(sh("sleep 60"), missing, &cancel, |_| {}).is_err());
    }

    #[test]
    fn pump_to_file_writes_the_stream_counts_the_bytes_and_reports_a_clean_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let file = std::fs::File::create(&path).unwrap();
        let t = pump_to_file(sh("printf 'STREAMDATA'"), file, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((t.bytes, t.cancelled, t.send_ok, t.write_error), (10, false, true, None));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "STREAMDATA");
    }

    #[test]
    fn pump_to_file_reports_a_failing_send_with_its_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("out")).unwrap();
        let t = pump_to_file(sh("echo oops >&2; printf abc; exit 3"), file, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!t.send_ok && !t.cancelled && t.write_error.is_none());
        assert_eq!(t.bytes, 3);
        assert!(t.send_stderr.contains("oops"), "{}", t.send_stderr);
    }

    #[test]
    fn pump_to_file_kills_the_send_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("out")).unwrap();
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let t = pump_to_file(sh("sleep 60"), file, &cancel, |_| {}).unwrap();
        assert!(t.cancelled && !t.send_ok);
        assert!(started.elapsed() < Duration::from_secs(10), "the sleep was killed, not waited for");
    }

    #[test]
    fn pump_to_file_stops_and_reports_a_write_error_such_as_a_full_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        std::fs::write(&path, "").unwrap();
        let read_only = std::fs::File::open(&path).unwrap(); // every write to it fails, like ENOSPC
        let started = Instant::now();
        let t = pump_to_file(sh("head -c 1000000 /dev/zero; sleep 30"), read_only, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(t.write_error.is_some(), "{t:?}");
        assert!(!t.send_ok, "the send is killed once the write fails");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_lines_hands_over_each_stdout_line_and_the_exit_code() {
        let mut seen = Vec::new();
        let r = run_lines(sh("echo one; echo two; echo oops >&2; exit 2"), &AtomicBool::new(false), |l| seen.push(l.to_string())).unwrap();
        assert_eq!(seen, vec!["one", "two"]);
        assert_eq!((r.cancelled, r.code), (false, Some(2)));
        assert!(r.stderr.contains("oops"), "{}", r.stderr);
    }

    #[test]
    fn run_lines_kills_the_child_when_cancelled_and_still_returns() {
        let started = Instant::now();
        let r = run_lines(sh("echo started; sleep 60"), &AtomicBool::new(true), |_| {}).unwrap();
        assert!(r.cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_ne!(r.code, Some(0));
    }

    #[test]
    fn run_lines_reports_a_command_that_cannot_start() {
        let missing = Command::new("/nonexistent/zfsctl-test-binary");
        assert!(run_lines(missing, &AtomicBool::new(false), |_| {}).is_err());
    }
}
