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

    let counted = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&counted);
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
        on_progress(counted.load(Ordering::Relaxed));
        std::thread::sleep(TICK);
    }
    let _ = copier.join();
    on_progress(counted.load(Ordering::Relaxed));

    let recv_status = receiver.wait()?;
    if !recv_status.success() {
        // The receive is gone: whatever the send is still doing is pointless.
        let _ = sender.kill();
    }
    let send_status = sender.wait()?;
    Ok(Transfer {
        bytes: counted.load(Ordering::Relaxed),
        cancelled,
        // A killed send is simply not ok; callers check `cancelled` first, and when the receive
        // failed they report the receive's message first.
        send_ok: send_status.success(),
        recv_ok: recv_status.success(),
        send_stderr: send_log.join().unwrap_or_default(),
        recv_stderr: recv_log.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
