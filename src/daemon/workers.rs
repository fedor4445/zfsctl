//! Runs a list of small tasks on a few worker threads, so a big fleet is asked a few hosts at a time instead of all at once.
//!
//! The workers are detached: a task that outlives its caller's deadline is left to finish by itself, as one thread per host
//! always was, and the workers go on draining the queue. A task that panics is dropped and the worker carries on with the next.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A unit of work: it reports through whatever channel it captured.
pub type Task = Box<dyn FnOnce() + Send + 'static>;

/// Most probes (and storage reads) that run at the same time, in the daemon and in the screen; the rest of a big fleet
/// waits its turn.
pub const PARALLEL: usize = 16;

/// Most host checks (disks and shares) at the same time: they run once a day and take longer, so more of them may overlap.
pub const PARALLEL_CHECKS: usize = 32;

/// Starts `min(limit, tasks)` workers that run `tasks` in order, `limit` at a time.
pub fn run_limited(limit: usize, tasks: Vec<Task>) {
    let workers = limit.max(1).min(tasks.len());
    let queue: Arc<Mutex<VecDeque<Task>>> = Arc::new(Mutex::new(tasks.into()));
    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        std::thread::spawn(move || loop {
            let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front();
            let Some(task) = next else { break };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn at_most_the_limit_run_at_once_and_every_task_runs() {
        let (now, most, done) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (tx, rx) = mpsc::channel();
        let tasks: Vec<Task> = (0..40)
            .map(|_| {
                let (now, most, done, tx) = (Arc::clone(&now), Arc::clone(&most), Arc::clone(&done), tx.clone());
                Box::new(move || {
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    now.fetch_sub(1, Ordering::SeqCst);
                    done.fetch_add(1, Ordering::SeqCst);
                    let _ = tx.send(());
                }) as Task
            })
            .collect();
        drop(tx);
        run_limited(5, tasks);
        for _ in 0..40 {
            rx.recv_timeout(Duration::from_secs(10)).expect("a task did not finish");
        }
        assert_eq!(done.load(Ordering::SeqCst), 40);
        let most = most.load(Ordering::SeqCst);
        assert!(most <= 5, "{most} at once, the limit is 5");
        assert!(most >= 2, "but side by side ({most})");
    }

    #[test]
    fn a_task_that_panics_does_not_stop_the_ones_behind_it() {
        let (tx, rx) = mpsc::channel();
        let mut tasks: Vec<Task> = vec![Box::new(|| panic!("a probe went wrong"))];
        for i in 0..3 {
            let tx = tx.clone();
            tasks.push(Box::new(move || {
                let _ = tx.send(i);
            }));
        }
        drop(tx);
        run_limited(1, tasks);
        let got: Vec<i32> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("the worker died with the panic")).collect();
        assert_eq!(got, [0, 1, 2], "one worker, in order, after the panic");
    }

    #[test]
    fn no_tasks_start_no_threads_and_a_zero_limit_still_runs_them() {
        run_limited(4, Vec::new());
        let (tx, rx) = mpsc::channel();
        run_limited(0, vec![Box::new(move || {
            let _ = tx.send(1);
        }) as Task]);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    }
}
