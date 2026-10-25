//! The one-line key-hint bar. `hints()` is a pure function of `&App`
//! that mirrors what `App::handle_key` / `handle_modal_key` /
//! `handle_picker_key` actually do — the tests in this file drive the
//! real handlers to keep the two from drifting apart.

use crate::ui::modal::{Modal, Tone};
use crate::ui::picker::{build_picker_state, PickerState};
use crate::ui::state::{App, DatasetDetailTab, PaneFocus, Section};
use crate::ui::theme::Theme;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

/// One `key label` pair. Lower `priority` = more important; `fit` drops
/// the highest number first (rightmost among ties). `pinned` hints are
/// never dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: u8,
    pub pinned: bool,
    pub tone: Tone,
}

fn h(key: &'static str, label: &'static str, priority: u8) -> Hint {
    Hint { key, label, priority, pinned: false, tone: Tone::Neutral }
}

impl Hint {
    fn pinned(mut self) -> Hint {
        self.pinned = true;
        self
    }

    fn tone(mut self, tone: Tone) -> Hint {
        self.tone = tone;
        self
    }
}

/// Precedence: picker, then modal, then (drilled pool | section → pane →
/// detail tab). `? Help` and `q Quit` are pinned on every non-modal
/// screen; a modal or picker shows only its own keys.
pub fn hints(app: &App) -> Vec<Hint> {
    if let Some(picker) = &app.picker {
        return picker_hints(picker);
    }
    if let Some(modal) = &app.modal {
        return modal_hints(modal);
    }
    if app.host_filter_editing {
        // Every key but these is text while a filter is typed.
        return vec![h("Enter", "Keep filter", 1), h("Esc", "Clear", 1)];
    }
    let mut v = context_hints(app);
    if !app.hosts.is_empty() {
        v.push(h("!", "Shell", 8));
    }
    v.push(h("A", "About", 9));
    v.push(h("?", "Help", 0).pinned());
    v.push(h("q", "Quit", 0).pinned());
    v
}

fn picker_hints(picker: &PickerState) -> Vec<Hint> {
    if picker.load != crate::ui::picker::PickerLoad::Ready {
        return vec![h("Esc", "Close", 0)];
    }
    let mut v = vec![h("↑↓", "Move", 2), h("type", "Filter", 3)];
    if picker.multi_select {
        v.push(h("Space", "Toggle", 1));
    }
    v.push(h("Enter", "OK", 0));
    v.push(h("Esc", "Cancel", 0));
    v
}

fn modal_hints(modal: &Modal) -> Vec<Hint> {
    match modal {
        Modal::Info { .. } => vec![h("↑/↓", "Scroll", 0), h("any key", "Close", 0)],
        Modal::Choose { then, .. } => {
            let what = match then {
                crate::ui::modal::ChooseThen::Unprotected(_) => "Fix",
                crate::ui::modal::ChooseThen::Copy { .. } => "Open form",
                crate::ui::modal::ChooseThen::Provider => "Show steps",
            };
            let mut v = vec![h("↑/↓", "Move", 0), h("Enter", what, 0)];
            if matches!(then, crate::ui::modal::ChooseThen::Unprotected(_)) {
                v.extend([h("Space", "Mark", 1), h("a", "Mark host", 2), h("s", "Snapshot jobs", 1), h("i", "Ignore", 2)]);
            }
            v.push(h("Esc", "Close", 0));
            v
        }
        // Two steps for both outcomes: y / n / Esc select, Enter applies. Enter alone does nothing, so
        // it is only advertised once there is a selection for it to apply.
        Modal::Confirm { action, choice, .. } => match choice {
            None => vec![h("y", "Yes", 0), h("n/Esc", "No", 0)],
            Some(true) => vec![h("Enter", "Run", 0).tone(action.call.tone()), h("n/Esc", "No", 1)],
            Some(false) => vec![h("Enter", "Cancel", 0), h("y", "Yes", 1)],
        },
        Modal::ConfirmTyped { action, .. } => vec![
            h("type", "name to confirm", 2),
            h("Enter", "Confirm", 0).tone(action.call.tone()),
            h("Esc", "Cancel", 0),
        ],
        Modal::Form { fields, focused, kind, .. } => {
            let enter = if focused + 1 < fields.len() { "Next" } else { "Submit" };
            let mut v = vec![h("Enter", enter, 0), h("Tab", "Next field", 2), h("Shift+Tab", "Prev field", 3)];
            let value = fields.get(*focused).map_or("", |f| f.value.as_str());
            if build_picker_state(kind, *focused, value).is_some() {
                v.push(h("F2", "Pick", 1));
            }
            if matches!(kind, crate::ui::modal::FormKind::BackupJob { .. } | crate::ui::modal::FormKind::MirrorJob { .. }) {
                v.push(h("F5", "Check", 1));
            }
            v.push(h("Esc", "Cancel", 0));
            v
        }
    }
}

fn context_hints(app: &App) -> Vec<Hint> {
    let mut v = Vec::new();

    if app.drilled_pool.is_some() {
        if !app.vdevs.is_empty() {
            v.push(h("↑↓", "Move", 1));
        }
        // Only a leaf disk can be offlined/onlined/replaced; on a mirror/raidz
        // group row these keys just set a status message.
        let leaf_selected = app
            .vdevs_table
            .selected()
            .and_then(|i| app.vdevs.get(i))
            .is_some_and(|v| crate::zfs::pool::is_leaf_disk(&v.name));
        if leaf_selected {
            v.push(h("o", "Offline", 2));
            v.push(h("O", "Online", 2));
            v.push(h("x", "Replace", 2));
        }
        if app.vdevs_table.selected().is_some_and(|i| crate::zfs::pool::removable_device(&app.vdevs, i).is_some()) {
            v.push(h("X", "Remove", 2));
        }
        v.push(h("a", "Add vdev", 3));
        v.push(h("r", "Refresh", 4));
        v.push(h("Esc", "Back", 1));
        return v;
    }

    let section = app.current_section();
    let right = app.pane_focus == PaneFocus::Right;
    match (section, right) {
        // A section this host's OS has no equivalent for is dimmed: only the global hints below apply.
        (s, _) if !app.section_supported(s) => {}
        (Section::Hosts, _) => {
            if !app.hosts.is_empty() {
                v.push(h("↑↓", "Host", 1));
            }
            if !app.hosts.is_empty() {
                v.push(h("c", "Check disks and shares", 2));
            }
            if !app.hosts.is_empty() {
                v.push(h("Space", "Mark", 2));
                v.push(h("e", "Edit tag and site", 2));
            }
            if !app.marked_hosts.is_empty() {
                v.push(h("Esc", "Unmark", 2));
            }
            v.push(h("n", "Add host", 2));
            v.push(h("/", "Filter", 3));
            if !app.host_filter.is_empty() {
                v.push(h("Esc", "Clear filter", 3));
            }
        }
        (Section::Pools, false) => {
            let selected = app.pools_table.selected().is_some();
            if !app.pools.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if selected {
                v.push(h("Enter", "Vdevs", 1));
            }
            v.push(h("→", "Properties", 2));
            if selected {
                v.push(h("s", "Scrub", 2));
                v.push(h("J", "Schedule", 3));
                v.push(h("R", "Resilver", 3));
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
            }
            v.push(h("n", "New pool", 2));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Pools, true) => {
            if !app.pool_properties.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.pool_properties_table.selected().is_some() {
                v.push(h("Enter", "Edit", 1));
            }
            v.push(h("←", "Back", 1));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Datasets, false) => {
            let selected = app.datasets_table.selected().is_some();
            if !app.datasets.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            v.push(h("→", "Details", 2));
            v.push(h("n", "New", 2));
            v.push(h("E", "New encrypted", 3));
            if selected {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("D", "Destroy recursively", 3).tone(Tone::Danger));
                v.push(h("s", "Snapshot", 2));
                // Always advertised: when a share can't be created the key
                // explains why (status message) instead of silently vanishing.
                if app.shares_editable() {
                    v.push(h("N", "NFS share", 3));
                    v.push(h("M", "SMB share", 3));
                }
                v.push(h("J", "Snapshot job", 3));
                // The key keys: on an encrypted row, the one that applies to its state.
                if app.key_applies('L') {
                    v.push(h("L", "Load key", 2));
                }
                if app.key_applies('U') {
                    v.push(h("U", "Unload key", 3).tone(Tone::Warn));
                }
                if app.key_applies('C') {
                    v.push(h("C", "Change key", 3).tone(Tone::Warn));
                }
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Datasets, true) => {
            match app.dataset_detail_tab {
                DatasetDetailTab::Properties => {
                    if !app.dataset_properties.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.dataset_properties_table.selected().is_some() {
                        v.push(h("Enter", "Edit", 1));
                    }
                }
                DatasetDetailTab::Permissions => {
                    if !app.dataset_permissions.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.selected_dataset_name().is_some() {
                        v.push(h("n", "Grant", 2));
                        v.push(h("s", "Grant set", 3));
                        v.push(h("c", "Create-time", 3));
                        if app.dataset_permissions_table.selected().is_some() {
                            v.push(h("d", "Revoke all", 2).tone(Tone::Warn));
                            v.push(h("x", "Revoke some", 3));
                        }
                    }
                }
                DatasetDetailTab::Acl => {
                    if !app.acl_entries.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.selected_dataset_mountpoint().is_some() && app.acl_unsupported().is_none() {
                        v.push(h("n", "Add entry", 2));
                        if app.acl_entries_table.selected().is_some_and(|i| i < app.acl_entries.len()) {
                            v.push(h("d", "Remove", 2).tone(Tone::Warn));
                        }
                    }
                }
            }
            v.push(h("[ ]", "Tabs", 3));
            v.push(h("←", "Back", 1));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Snapshots, _) if app.browse_view_active() => {
            if let Some(view) = app.browse_view.as_ref() {
                if view.input.is_some() {
                    v.push(h("Enter", "Keep", 1));
                    v.push(h("Esc", "Cancel", 1));
                } else if matches!(view.phase, crate::ui::browse::Phase::Ready) {
                    if !view.rows.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if view.rows.get(view.selected).is_some_and(|r| r.entry.kind == crate::zfs::snapshot_browse::EntryKind::Directory && !r.entry.ambiguous && crate::zfs::snapshot_browse::valid_component(&r.entry.name)) {
                        v.push(h("Enter", "Open", 1));
                    }
                    if !view.path.is_empty() {
                        v.push(h("←", "Up", 1));
                    }
                    v.push(h("/", "Name", 2));
                    v.push(h("m", "Modified", 3));
                    v.push(h("s", "Size", 3));
                    v.push(h("t", "Type", 3));
                    if view.can_restore() {
                        v.push(h("R", "Restore", 2));
                    }
                    if !view.filter.is_empty() {
                        v.push(h("x", "Clear", 2));
                    }
                    v.push(h("Esc", "Close", 1));
                } else {
                    if !view.path.is_empty() && !matches!(view.phase, crate::ui::browse::Phase::Reading) {
                        v.push(h("←", "Up", 1));
                    }
                    v.push(h("Esc", "Close", 1));
                }
            }
        }
        (Section::Snapshots, _) if app.changes_view_active() => {
            let ready = app.changes_view.as_ref().is_some_and(|view| matches!(view.phase, crate::ui::changes::Phase::Ready { .. }));
            let typing = app.changes_view.as_ref().is_some_and(|view| view.name_input.is_some());
            let in_folder = app.changes_view.as_ref().is_some_and(|view| view.open_group.is_some());
            if ready && typing {
                v.push(h("Enter", "Keep", 1));
                v.push(h("Esc", "Cancel", 1));
            } else if ready {
                v.push(h("↑↓", "Move", 1));
                if !in_folder {
                    v.push(h("Enter", "Open", 2));
                }
                v.push(h("[ ]", "Depth", 2));
                if in_folder {
                    v.push(h("/", "Filter", 2));
                }
                v.push(h("H", "Hold older", 2));
                v.push(h("Esc", "Back", 1));
            } else {
                v.push(h("Esc", "Close", 1));
            }
        }
        (Section::Snapshots, _) => {
            if !app.snapshots.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.snapshots_table.selected().is_some() {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("b", "Rollback", 2).tone(Tone::Danger));
                v.push(h("H", "Hold", 2));
                v.push(h("c", "Changes", 2));
                v.push(h("C", "Since", 3));
                v.push(h("f", "Browse", 2));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Disks, _) => {
            if !app.disks.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.disks_table.selected().is_some() {
                v.push(h("Space", "Select", 1));
            }
            if !app.selected_disks.is_empty() {
                v.push(h("n", "New pool", 2));
                v.push(h("a", "Add vdev", 2));
                v.push(h("Esc", "Clear", 3));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Shares, _) => {
            if !app.shares.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.shares_table.selected().is_some() && app.shares_editable() {
                v.push(h("Enter", "Edit", 1));
                v.push(h("d", "Remove", 2).tone(Tone::Warn));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::BootEnvs, _) => {
            if !app.bootenvs.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if let Some(be) = app.selected_bootenv() {
                if be.can_activate() {
                    v.push(h("a", "Activate", 2).tone(Tone::Warn));
                }
                if be.can_activate_temporarily() {
                    v.push(h("t", "Next boot only", 2));
                }
                if be.can_clear_temporary() {
                    v.push(h("T", "Clear next-boot", 2));
                }
                v.push(h("R", "Rename", 3));
                if be.can_mount() {
                    v.push(h("m", "Mount", 3));
                }
                if be.can_unmount() {
                    v.push(h("u", "Unmount", 3));
                }
                if be.can_destroy() {
                    v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                    v.push(h("D", "Destroy + origin", 3).tone(Tone::Danger));
                }
            }
            if app.bootenvs_unavailable.is_none() {
                v.push(h("n", "New", 2));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Jobs, _) => {
            if !app.jobs_view.jobs.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if let Some(job) = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i)) {
                v.push(h("Enter", "Edit", 1));
                v.push(h("Space", if job.enabled { "Disable" } else { "Enable" }, 2));
                v.push(h("d", "Delete", 2).tone(Tone::Warn));
            }
            v.push(h("n", "New scrub job", 2));
            v.push(h("N", "New snapshot job", 3));
            v.push(h("R", "New replication job", 3));
            v.push(h("B", "New backup job", 3));
            v.push(h("C", "New cloud mirror", 3));
            if let Some(job) = app.jobs_table.selected().and_then(|i| app.jobs_view.jobs.get(i)) {
                if crate::jobs::is_cancellable(job) && app.jobs_view.is_running(&job.id) {
                    v.push(h("c", "Cancel transfer", 1).tone(Tone::Warn));
                }
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Dashboard, _) => {
            let dashboard = app.dashboard();
            let has_lines = dashboard.selectable() > 0;
            if has_lines {
                v.push(h("↑↓", "Move", 1));
            }
            let selected = app.dashboard_table.selected().filter(|_| has_lines);
            if let Some(i) = selected {
                if dashboard.is_safety(i) {
                    v.push(h("Enter", "Show graph", 1));
                } else if dashboard.group_at(i).is_some() {
                    v.push(h("Enter", "Open group", 1));
                } else if let Some(item) = dashboard.attention.get(i) {
                    v.push(h("Enter", if dashboard.protection_fix(item).is_some() { "Fix" } else if dashboard.protection_popup(item).is_some() { "Show list" } else { "Go to section" }, 1));
                }
            }
            if dashboard.groups.is_some() {
                v.push(h("e", if app.dashboard_expanded { "Problem hosts only" } else { "All hosts" }, 3));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Cloud, _) => {
            let row = app.cloud_table.selected().and_then(|i| app.cloud_rows().into_iter().nth(i));
            if !app.cloud_rows().is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if row.as_ref().is_some_and(|r| r.job_ids.iter().any(|id| app.jobs_view.jobs.iter().any(|j| &j.id == id && j.kind == crate::jobs::KIND_MIRROR))) {
                v.push(h("s", "Bucket size", 1));
            }
            if row.as_ref().is_some_and(|r| crate::ui::cloud::check_target(r, &app.jobs_view.jobs).is_some()) {
                v.push(h("F5", "Check", 2));
            }
            v.push(h("n", "Set up a provider", 3));
            v.push(h("C", "New cloud mirror", 3));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Settings, _) => {
            if !app.settings_view.rows.is_empty() {
                v.push(h("↑↓", "Move", 1));
                v.push(h("Enter", "Edit", 1));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Alerts, _) => {
            v.push(h("e", "Edit settings", 1));
            v.push(h("t", "Send test", 2));
            v.push(h("p", "Preview report", 2));
            if !app.alerts_view.events.is_empty() {
                v.push(h("↑↓", "Scroll", 3));
            }
            v.push(h("r", "Refresh", 4));
        }
        // Never in `App::sections`; listed so a future section forces a decision here.
        (Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl, _) => {}
    }
    v.push(h("Tab", "Section", 5));
    v
}

fn total_width(hints: &[Hint]) -> usize {
    if hints.is_empty() {
        return 0;
    }
    hints.iter().map(|x| x.key.chars().count() + 1 + x.label.chars().count()).sum::<usize>() + 2 * (hints.len() - 1)
}

/// Drops hints until the rest fit in `width` columns: highest `priority`
/// number first, rightmost among ties, never a pinned one. Display order
/// of the survivors is unchanged.
pub fn fit(hints: &[Hint], width: usize) -> Vec<Hint> {
    let mut kept: Vec<Hint> = hints.to_vec();
    while total_width(&kept) > width {
        let victim = kept
            .iter()
            .enumerate()
            .filter(|(_, hint)| !hint.pinned)
            .max_by_key(|(i, hint)| (hint.priority, *i))
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                kept.remove(i);
            }
            None => break,
        }
    }
    kept
}

pub fn hint_line(hints: &[Hint], width: usize, theme: &Theme) -> Line<'static> {
    let kept = fit(hints, width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, hint) in kept.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let key_style = match hint.tone {
            Tone::Neutral => theme.accent.add_modifier(Modifier::BOLD),
            Tone::Warn => theme.warn.add_modifier(Modifier::BOLD),
            Tone::Danger => theme.danger,
        };
        spans.push(Span::styled(hint.key, key_style));
        spans.push(Span::styled(format!(" {}", hint.label), theme.muted));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests;
