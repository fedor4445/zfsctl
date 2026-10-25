//! The snapshot changes view and the file browser: their keys and the reads that feed them.
#![allow(clippy::wildcard_imports)]
use super::*;

impl App {
    /// See `refresh_pools` for why this queues rather than drops a
    /// request while already in flight.
    /// The changes view, when it belongs to the host and section on screen.
    pub fn changes_view_active(&self) -> bool {
        self.current_section() == Section::Snapshots && self.changes_view.as_ref().is_some_and(|v| v.host == self.selected_host)
    }

    /// Keys while the changes view is open: they move in it, open a folder, set the depth, hold the older snapshot
    /// or close it; the snapshot list's own destructive keys do nothing behind it. Everything else (quit, Help, the
    /// shell, Tab) works as everywhere. Returns whether the key was handled.
    pub(super) fn key_changes_view(&mut self, key: KeyCode) -> bool {
        if self.changes_view.is_none() || self.modal.is_some() || self.picker.is_some() || self.current_section() != Section::Snapshots {
            return false;
        }
        if !self.changes_view_active() {
            if let Some(view) = self.changes_view.take() {
                view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            return false;
        }
        let older = self.changes_view.as_ref().and_then(crate::ui::changes::ChangesView::older_snapshot);
        let Some(view) = self.changes_view.as_mut() else { return false };
        // While a name filter is being typed every key is text (Esc and Enter end it).
        if view.filter_key(key) {
            return true;
        }
        match key {
            KeyCode::Char('/') => {
                view.start_filter();
            }
            KeyCode::Esc | KeyCode::Backspace => {
                if !view.back() {
                    view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    self.changes_view = None;
                }
            }
            KeyCode::Down => view.move_selection(true),
            KeyCode::Up => view.move_selection(false),
            KeyCode::Enter => view.open_selected(),
            KeyCode::Char('[') => view.set_depth(view.depth.saturating_sub(1)),
            KeyCode::Char(']') => view.set_depth(view.depth + 1),
            KeyCode::Char('H') => {
                if let Some((dataset, name)) = older {
                    let prompt = hold_prompt(&format!("{dataset}@{name}"));
                    let call = MutationCall::HoldSnapshot { dataset, name };
                    self.modal = Some(Modal::Confirm { choice: None, prompt, action: PendingAction { host: self.selected_host, section: Section::Snapshots, call } });
                }
            }
            KeyCode::Char('c' | 'C' | 'd' | 'b' | 'f' | 'R') => {}
            _ => return false,
        }
        true
    }

    /// The browse view, when it belongs to the host and section on screen.
    pub fn browse_view_active(&self) -> bool {
        self.current_section() == Section::Snapshots && self.browse_view.as_ref().is_some_and(|v| v.host == self.selected_host)
    }

    /// Keys while the browser is open: they move in it, open a folder, go up, set the filters or close it; the snapshot
    /// list's own destructive keys do nothing behind it. While a filter prompt is open every key is text. Everything else
    /// (quit, Help, the shell, Tab) works as everywhere. Returns whether the key was handled.
    pub(super) fn key_browse_view(&mut self, key: KeyCode) -> bool {
        use crate::ui::browse::InputKind;
        if self.browse_view.is_none() || self.modal.is_some() || self.picker.is_some() || self.current_section() != Section::Snapshots {
            return false;
        }
        if !self.browse_view_active() {
            if let Some(view) = self.browse_view.take() {
                view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            return false;
        }
        let request = self.browse_request + 1;
        let Some(view) = self.browse_view.as_mut() else { return false };
        if view.input_key(key) {
            return true;
        }
        let mut next = None;
        match key {
            KeyCode::Esc => {
                view.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                self.browse_view = None;
            }
            KeyCode::Down => view.move_selection(true),
            KeyCode::Up => view.move_selection(false),
            KeyCode::Enter | KeyCode::Right => next = view.enter_selected(request).unwrap_or(None),
            KeyCode::Left | KeyCode::Backspace => next = view.go_up(request),
            KeyCode::Char('/') => view.start_input(InputKind::Name),
            KeyCode::Char('m') => view.start_input(InputKind::Modified),
            KeyCode::Char('s') => view.start_input(InputKind::Size),
            KeyCode::Char('t') => view.cycle_kind(),
            KeyCode::Char('x') => view.clear_filters(),
            KeyCode::Char('R') => {
                if let Some(source) = view.restore_source() {
                    let host = view.host;
                    let value = crate::zfs::snapshot_restore::default_destination_name(&source.entry.name, &source.snapshot);
                    self.modal = Some(Modal::Form {
                        title: "Restore as a copy".to_string(),
                        fields: vec![FormField { label: "Restore as".to_string(), value }],
                        focused: 0,
                        description: Some("A new file next to the original; nothing is replaced.".to_string()),
                        kind: FormKind::RestoreFile { host, source: Box::new(source) },
                    });
                }
            }
            KeyCode::Char('c' | 'C' | 'd' | 'b' | 'H' | 'f') => {}
            _ => return false,
        }
        if let Some(pending) = next {
            self.browse_request = request;
            self.pending_browse.push(pending);
        }
        true
    }

    /// Ends a queued browse read that cannot run (its host is gone) as a failed one.
    pub fn fail_browse(&mut self, pending: &crate::ui::browse::PendingBrowse, why: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::Browse(pending.host, pending.request, Err(why)));
    }

    /// Lists one snapshot folder on a worker thread and sends the outcome back (an error as an error).
    pub fn start_browse(&mut self, runner: Arc<dyn CommandRunner>, pending: crate::ui::browse::PendingBrowse) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::ui::browse::read_pending(runner.as_ref(), &pending);
            let _ = tx.send(WorkerMsg::Browse(pending.host, pending.request, result));
        });
    }

    /// Ends a queued read that cannot run (its host is gone) as a failed one.
    pub fn fail_changes(&mut self, pending: &crate::ui::changes::PendingChanges, why: crate::error::AppError) {
        let _ = self.tx.send(WorkerMsg::Changes(pending.host, pending.request, Err(why)));
    }

    /// Reads one changes view's diff on a worker thread and sends the outcome back (an error as an error).
    pub fn start_changes(&mut self, runner: Arc<dyn CommandRunner>, pending: crate::ui::changes::PendingChanges) {
        use crate::ui::changes::{Against, ChangesOutcome};
        use crate::zfs::snapshot_changes::{dataset_mountpoint, previous_snapshot, read_changes, Limits};
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<ChangesOutcome> {
                let this = format!("{}@{}", pending.dataset, pending.name);
                let (older, newer) = match pending.against {
                    Against::Previous => match previous_snapshot(runner.as_ref(), &pending.dataset, &pending.name)? {
                        Some(previous) => (previous, this),
                        None => return Ok(ChangesOutcome::NoPrevious),
                    },
                    Against::Live => (this, pending.dataset.clone()),
                };
                let mount = dataset_mountpoint(runner.as_ref(), &pending.dataset)?;
                let summary = read_changes(runner.as_ref(), &older, &newer, mount, &Limits::default(), &pending.cancel)?;
                Ok(ChangesOutcome::Done { older, newer, summary })
            })();
            let _ = tx.send(WorkerMsg::Changes(pending.host, pending.request, result));
        });
    }
}
