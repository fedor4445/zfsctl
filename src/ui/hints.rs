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
                    v.push(h("Enter", "Apply", 1));
                    v.push(h("Esc", "Cancel", 1));
                } else if matches!(view.phase, crate::ui::browse::Phase::Ready) {
                    if !view.rows.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if view.rows.get(view.selected).is_some_and(|r| r.entry.kind == crate::zfs::snapshot_browse::EntryKind::Directory) {
                        v.push(h("Enter", "Open", 1));
                    }
                    if !view.path.is_empty() {
                        v.push(h("←", "Up", 1));
                    }
                    v.push(h("/", "Name", 2));
                    v.push(h("m", "Modified", 3));
                    v.push(h("s", "Size", 3));
                    v.push(h("t", "Type", 3));
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
            if ready {
                v.push(h("↑↓", "Move", 1));
                v.push(h("Enter", "Open", 2));
                v.push(h("[ ]", "Depth", 2));
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
                if be.can_mount() && app.bootenv_key_offered('m') {
                    v.push(h("m", "Mount", 3));
                }
                if be.can_unmount() && app.bootenv_key_offered('u') {
                    v.push(h("u", "Unmount", 3));
                }
                if be.can_destroy() {
                    v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                    if app.bootenv_key_offered('D') {
                        v.push(h("D", "Destroy + origin", 3).tone(Tone::Danger));
                    }
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
            let has_lines = !app.dashboard().attention.is_empty();
            if has_lines {
                v.push(h("↑↓", "Move", 1));
            }
            if has_lines && app.dashboard_table.selected().is_some() {
                v.push(h("Enter", "Go to section", 1));
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
                v.push(h("F5", "Check", 2));
            }
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
mod tests {
    use super::*;

    fn smb(dataset: &str) -> crate::zfs::share::Share {
        crate::zfs::share::Share::Smb {
            dataset: dataset.to_string(),
            name: "media".to_string(),
            path: format!("/{dataset}"),
            valid_user: Some("carol".to_string()),
            read_only: false,
            guest_ok: false,
        }
    }
    use crate::acl::{AclEntry, AclEntryType, AclTag};
    use crate::ui::modal::{FormField, FormKind, MutationCall, PendingAction};
    use crate::ui::state::PermissionRow;
    use crate::zfs::dataset::{Dataset, DatasetKind};
    use crate::zfs::disk::{DiskInfo, DiskMembership};
    use crate::zfs::pool::{Pool, PoolHealth, VdevLine};
    use crate::zfs::property::{Property, PropertySource};
    use crate::zfs::share::Share;
    use crate::zfs::snapshot::Snapshot;
    use crossterm::event::KeyCode;
    use ratatui::style::Style;

    // ---- fixtures -------------------------------------------------

    fn base() -> App {
        App::new(vec!["local".to_string(), "other".to_string()])
    }

    fn go(app: &mut App, section: Section) {
        app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
        app.pane_focus = PaneFocus::Middle;
    }

    fn pool(name: &str) -> Pool {
        Pool { name: name.to_string(), size: 100, allocated: 10, free: 90, health: PoolHealth::Online }
    }

    fn dataset(name: &str) -> Dataset {
        Dataset { name: name.to_string(), kind: DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{name}")), quota: 0, refquota: 0 }
    }

    fn prop(name: &str) -> Property {
        Property { name: name.to_string(), value: "off".to_string(), source: PropertySource::Local }
    }

    fn pools_middle() -> App {
        let mut a = base();
        go(&mut a, Section::Pools);
        a.pools = vec![pool("tank"), pool("scratch")];
        a.pools_table.select(Some(0));
        a
    }

    fn pools_middle_empty() -> App {
        let mut a = base();
        go(&mut a, Section::Pools);
        a
    }

    fn pools_right() -> App {
        let mut a = pools_middle();
        a.pane_focus = PaneFocus::Right;
        a.pool_properties = vec![prop("comment"), prop("autoexpand")];
        a.pool_properties_table.select(Some(0));
        a
    }

    fn drilled(selected: bool) -> App {
        let mut a = pools_middle();
        a.drilled_pool = Some("tank".to_string());
        a.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        a.vdevs_table.select(if selected { Some(1) } else { None });
        a
    }

    /// A drilled pool whose selected row is a mirror GROUP, not a disk:
    /// o/O/x only set a status message there.
    fn drilled_group_selected() -> App {
        let mut a = drilled(false);
        a.vdevs.insert(1, VdevLine { depth: 1, name: "mirror-0".to_string(), health: PoolHealth::Online });
        a.vdevs_table.select(Some(1));
        a
    }

    fn datasets_middle(shares_loaded: bool) -> App {
        let mut a = base();
        let host = a.hosts[0].clone();
        a.set_os_for_test(&host, crate::zfs::host_health::HostOs::FreeBsd);
        go(&mut a, Section::Datasets);
        a.datasets = vec![dataset("tank/a"), dataset("tank/b")];
        a.datasets_table.select(Some(0));
        a.shares_loaded = shares_loaded;
        a.nfs_available = Some(true);
        a
    }

    fn datasets_none_selected() -> App {
        let mut a = datasets_middle(true);
        a.datasets_table.select(None);
        a
    }

    fn datasets_right(tab: DatasetDetailTab) -> App {
        let mut a = datasets_middle(false);
        a.pane_focus = PaneFocus::Right;
        a.dataset_detail_tab = tab;
        a.dataset_properties = vec![prop("compression"), prop("atime")];
        a.dataset_properties_table.select(Some(0));
        a.dataset_permissions = vec![
            PermissionRow::Set { name: "s1".to_string(), perms: vec!["create".to_string()] },
            PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
        ];
        a.dataset_permissions_table.select(Some(0));
        a.acl_entries_for = Some("/tank/a".to_string());
        a.acl_entries = vec![
            AclEntry { tag: AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
            AclEntry { tag: AclTag::Everyone, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
        ];
        a.acl_entries_table.select(Some(0));
        a
    }

    fn snapshots() -> App {
        let mut a = base();
        go(&mut a, Section::Snapshots);
        a.snapshots = vec![
            Snapshot { dataset: "tank/a".to_string(), name: "s1".to_string(), used: 1, holds: 0, written: 0 },
            Snapshot { dataset: "tank/a".to_string(), name: "s2".to_string(), used: 1, holds: 0, written: 0 },
        ];
        a.snapshots_table.select(Some(0));
        a
    }

    fn disks(selected: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Disks);
        a.disks = vec![
            DiskInfo { name: "da0".to_string(), size: 1, membership: DiskMembership::Available, ..Default::default() },
            DiskInfo { name: "da1".to_string(), size: 1, membership: DiskMembership::Available, ..Default::default() },
        ];
        a.disks_table.select(Some(0));
        if selected {
            a.selected_disks.insert("da0".to_string());
        }
        a
    }

    fn shares() -> App {
        let mut a = base();
        go(&mut a, Section::Shares);
        a.shares = vec![
            Share::Nfs { dataset: "tank/a".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None },
            smb("tank/b"),
        ];
        a.shares_table.select(Some(0));
        a
    }

    fn jobs(with_selection: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Jobs);
        let job = |id: &str, enabled: bool| crate::jobs::Job { id: id.to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None };
        a.jobs_view.jobs = vec![job("a", true), job("b", false)];
        a.jobs_table.select(if with_selection { Some(0) } else { None });
        a
    }

    fn cloud(with_mirror: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Cloud);
        let mut job = crate::jobs::Job::test_mirror("m", "tank/data", "hourly", "b2:bkt/p");
        job.host = "local".into();
        a.jobs_view.jobs = if with_mirror { vec![job] } else { vec![] };
        a.cloud_known.insert("local".into(), vec![("b2".into(), "b2".into()), ("t1".into(), "sftp".into())]);
        a.cloud_table.select(Some(0));
        a
    }

    fn dashboard(selected: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Dashboard);
        // Three lines of Needs attention: no heartbeat (the daemon is down), and a pool 85% full that was never scrubbed.
        let pool = crate::zfs::host_health::PoolProbe { name: "tank".into(), health: crate::zfs::pool::PoolHealth::Online, capacity_percent: 85, frag_percent: None, errors: Some(0), scan: None, last_scrub: None };
        let name = a.hosts[0].clone();
        a.host_probes.insert(name, crate::ui::state::HostProbeState { last: Some(crate::zfs::host_health::HostProbe::Reached { rtt_ms: 1, pools: vec![pool], kind: None, os: None, os_name: None }), ..Default::default() });
        a.dashboard_table.select(selected.then_some(0));
        a
    }

    fn alerts() -> App {
        let mut a = base();
        go(&mut a, Section::Alerts);
        let e = |n: usize| crate::daemon::health_text::HealthEvent { at: chrono::Utc::now(), host: "a".into(), kind: "alert".into(), text: format!("n{n}") };
        a.alerts_view.events = (0..4).map(e).collect();
        a
    }

    fn settings(rows: usize) -> App {
        let mut a = base();
        go(&mut a, Section::Settings);
        let row = |n: usize| crate::ui::settings::Row { group: "Display", setting: crate::ui::settings::Setting::Cpu, label: format!("row{n}"), value: "1".into(), is_default: true, raw: vec![String::new()] };
        a.settings_view = crate::ui::settings::SettingsView { loaded: true, problem: None, rows: (0..rows).map(row).collect() };
        a.settings_table.select((rows > 0).then_some(0));
        a
    }

    #[test]
    fn the_settings_section_offers_move_and_edit_only_when_there_are_rows() {
        let labels = |a: &App| hints(a).iter().map(|h| (h.key, h.label)).collect::<Vec<_>>();
        let with = labels(&settings(2));
        for want in [("↑↓", "Move"), ("Enter", "Edit"), ("r", "Refresh")] {
            assert!(with.contains(&want), "{with:?}");
        }
        let without = labels(&settings(0));
        assert!(without == [("r", "Refresh")] || without.iter().all(|(k, _)| *k != "Enter" && *k != "↑↓"), "{without:?}");
    }

    #[test]
    fn a_dimmed_section_offers_only_the_global_hints() {
        use crate::zfs::host_health::HostOs;
        let mut a = base();
        a.hosts = vec!["local".to_string(), "ub24".to_string()];
        a.host_probes.insert("ub24".to_string(), crate::ui::state::HostProbeState { os: Some(HostOs::Linux), ..Default::default() });
        a.selected_host = 1;
        go(&mut a, Section::BootEnvs);
        let keys: Vec<&str> = hints(&a).iter().map(|h| h.key).collect();
        assert!(!keys.contains(&"n") && !keys.contains(&"d") && !keys.contains(&"a") && !keys.contains(&"Enter"), "{keys:?}");
        assert!(keys.contains(&"Tab") && keys.contains(&"q"), "the global hints stay: {keys:?}");
    }

    #[test]
    fn the_hosts_section_offers_c_for_a_host_and_other_sections_do_not() {
        let mut a = base();
        go(&mut a, Section::Hosts);
        assert!(hints(&a).iter().any(|h| h.key == "c" && h.label == "Check disks and shares"));
        a.hosts.clear();
        assert!(!hints(&a).iter().any(|h| h.key == "c"), "no host, nothing to check");
        go(&mut a, Section::Pools);
        assert!(!hints(&a).iter().any(|h| h.key == "c" && h.label.starts_with("Check")));
    }

    #[test]
    fn the_dashboard_offers_move_always_go_only_with_a_selected_line_and_nothing_that_changes_anything() {
        let labels = |a: &App| hints(a).iter().map(|h| (h.key, h.label)).collect::<Vec<_>>();
        let none = labels(&dashboard(false));
        assert!(none.contains(&("↑↓", "Move")) && none.contains(&("r", "Refresh")) && !none.iter().any(|(k, _)| *k == "Enter"), "{none:?}");
        let some = labels(&dashboard(true));
        assert!(some.contains(&("Enter", "Go to section")), "{some:?}");
        for (key, label) in &some {
            assert!(!matches!(*key, "d" | "n" | "N" | "B" | "C" | "R" | "s" | "Space"), "the Dashboard must not advertise {key} {label}");
        }
        let mut quiet = dashboard(false);
        quiet.host_probes.clear();
        quiet.jobs_view.heartbeat = Some(crate::daemon::heartbeat::Heartbeat { pid: 1, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() });
        let keys: Vec<&str> = hints(&quiet).iter().map(|h| h.key).collect();
        assert!(!keys.contains(&"↑↓") && !keys.contains(&"Enter"), "nothing to move through when all is quiet: {keys:?}");
    }

    #[test]
    fn the_cloud_section_offers_size_and_check_only_for_a_remote_that_has_a_mirror_job() {
        let with = cloud(true);
        let labels: Vec<(&str, &str)> = hints(&with).iter().map(|h| (h.key, h.label)).collect();
        for want in [("↑↓", "Move"), ("s", "Bucket size"), ("F5", "Check"), ("C", "New cloud mirror"), ("r", "Refresh")] {
            assert!(labels.contains(&want), "{labels:?}");
        }
        let without = cloud(false);
        let keys: Vec<&str> = hints(&without).iter().map(|h| h.key).collect();
        assert!(!keys.contains(&"s") && !keys.contains(&"F5") && keys.contains(&"C"), "{keys:?}");
    }

    #[test]
    fn the_alerts_section_advertises_scrolling_only_when_there_are_events() {
        let with = alerts();
        assert!(hints(&with).iter().any(|h| h.key == "↑↓" && h.label == "Scroll"));
        let mut without = alerts();
        without.alerts_view.events.clear();
        assert!(!hints(&without).iter().any(|h| h.key == "↑↓"));
    }

    #[test]
    fn the_jobs_section_advertises_edit_enable_or_disable_delete_and_new_for_the_selected_job() {
        let selected = jobs(true);
        let labels: Vec<(&str, &str)> = hints(&selected).iter().map(|h| (h.key, h.label)).collect();
        for want in [("Enter", "Edit"), ("Space", "Disable"), ("d", "Delete"), ("n", "New scrub job"), ("N", "New snapshot job"), ("R", "New replication job")] {
            assert!(labels.contains(&want), "{labels:?}");
        }
        let mut second = jobs(true);
        second.jobs_table.select(Some(1));
        assert!(hints(&second).iter().any(|h| (h.key, h.label) == ("Space", "Enable")), "a disabled job offers Enable");
        let none = jobs(false);
        let keys: Vec<&str> = hints(&none).iter().map(|h| h.key).collect();
        assert!(keys.contains(&"n") && !keys.contains(&"Space") && !keys.contains(&"d") && !keys.contains(&"Enter"), "{keys:?}");
    }

    fn hosts() -> App {
        let mut a = base();
        go(&mut a, Section::Hosts);
        a
    }

    /// Every non-modal scenario the drift guard exercises.
    fn scenarios() -> Vec<(&'static str, App)> {
        vec![
            ("hosts", hosts()),
            ("pools middle", pools_middle()),
            ("pools middle, empty", pools_middle_empty()),
            ("pools right", pools_right()),
            ("vdevs, row selected", drilled(true)),
            ("vdevs, nothing selected", drilled(false)),
            ("vdevs, mirror group selected", drilled_group_selected()),
            ("datasets middle", datasets_middle(false)),
            ("datasets middle, shares loaded", datasets_middle(true)),
            ("datasets middle, nothing selected", datasets_none_selected()),
            ("datasets right: properties", datasets_right(DatasetDetailTab::Properties)),
            ("datasets right: permissions", datasets_right(DatasetDetailTab::Permissions)),
            ("datasets right: acl", datasets_right(DatasetDetailTab::Acl)),
            ("snapshots", snapshots()),
            ("disks, none selected", disks(false)),
            ("disks, one selected", disks(true)),
            ("shares", shares()),
            ("boot envs, running selected", boot_envs(0)),
            ("boot envs, spare selected", boot_envs(1)),
            ("boot envs, temporary selected", boot_envs(2)),
            ("boot envs, mounted selected", boot_envs(3)),
            ("boot envs, next-boot selected", boot_envs(4)),
            ("boot envs, none selected", boot_envs_none()),
            ("boot envs, unavailable", boot_envs_unavailable()),
            ("jobs, one selected", jobs(true)),
            ("jobs, none selected", jobs(false)),
            ("dashboard, line selected", dashboard(true)),
            ("dashboard, none selected", dashboard(false)),
            ("cloud, with a mirror job", cloud(true)),
            ("cloud, no job", cloud(false)),
            ("alerts", alerts()),
            ("settings, two rows", settings(2)),
        ]
    }

    // ---- fingerprint ----------------------------------------------

    /// Everything a key press can change. `status` is included only by
    /// `fingerprint_with_status` (used for the "advertised keys do something"
    /// guard: a key that explains why it can't act, e.g. "NFS server not
    /// available", counts as doing something). The "unadvertised keys are
    /// no-ops" guard uses plain `fingerprint`, which leaves `status` out, as
    /// several unadvertised keys already set informational messages.
    #[derive(Debug, PartialEq)]
    struct Fingerprint {
        modal_open: bool,
        picker_open: bool,
        section: usize,
        focus: PaneFocus,
        tab: DatasetDetailTab,
        drilled: Option<String>,
        changes_view: Option<(bool, usize, usize)>,
        browse_view: Option<(bool, usize, usize, bool)>,
        host: usize,
        host_filter: (String, bool),
        alerts_scroll: usize,
        quit: bool,
        selected_disks: Vec<String>,
        selections: Vec<Option<usize>>,
        status: Option<String>,
    }

    fn fingerprint(a: &App) -> Fingerprint {
        let mut disks: Vec<String> = a.selected_disks.iter().cloned().collect();
        disks.sort();
        Fingerprint {
            modal_open: a.modal.is_some(),
            picker_open: a.picker.is_some(),
            section: a.selected_section,
            focus: a.pane_focus,
            tab: a.dataset_detail_tab,
            drilled: a.drilled_pool.clone(),
            changes_view: a.changes_view.as_ref().map(|v| (matches!(v.phase, crate::ui::changes::Phase::Ready { .. }), v.depth, v.selected)),
            browse_view: a.browse_view.as_ref().map(|v| (matches!(v.phase, crate::ui::browse::Phase::Ready), v.path.len(), v.selected, v.input.is_some())),
            host: a.selected_host,
            host_filter: (a.host_filter.clone(), a.host_filter_editing),
            alerts_scroll: a.alerts_view.scroll,
            quit: a.should_quit,
            status: None,
            selected_disks: disks,
            selections: vec![
                a.pools_table.selected(),
                a.datasets_table.selected(),
                a.snapshots_table.selected(),
                a.disks_table.selected(),
                a.shares_table.selected(),
                a.bootenvs_table.selected(),
                a.jobs_table.selected(),
                a.cloud_table.selected(),
                a.dashboard_table.selected(),
                a.settings_table.selected(),
                a.vdevs_table.selected(),
                a.pool_properties_table.selected(),
                a.dataset_properties_table.selected(),
                a.dataset_permissions_table.selected(),
                a.acl_entries_table.selected(),
            ],
        }
    }

    fn fingerprint_with_status(a: &App) -> Fingerprint {
        Fingerprint { status: a.status_message.clone(), ..fingerprint(a) }
    }

    /// Maps a hint's displayed key to the real key it stands for, or
    /// `None` for keys this test cannot or should not press.
    fn key_code(key: &str) -> Option<KeyCode> {
        match key {
            "Enter" => Some(KeyCode::Enter),
            "Esc" => Some(KeyCode::Esc),
            "Tab" => Some(KeyCode::Tab),
            "↑↓" => Some(KeyCode::Down),
            "←" => Some(KeyCode::Left),
            "→" => Some(KeyCode::Right),
            "Space" => Some(KeyCode::Char(' ')),
            "[ ]" => Some(KeyCode::Char(']')),
            // `r` is handled in the event loop (ui/mod.rs), not in
            // `handle_key`, so it cannot be driven from here.
            "r" => None,
            k if k.chars().count() == 1 => k.chars().next().map(KeyCode::Char),
            _ => None,
        }
    }

    // ---- the drift guard ------------------------------------------

    #[test]
    fn every_advertised_key_actually_does_something() {
        for (name, app) in scenarios() {
            for hint in hints(&app) {
                let Some(code) = key_code(hint.key) else { continue };
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                let before = fingerprint_with_status(&probe);
                probe.handle_key(code);
                assert_ne!(
                    fingerprint_with_status(&probe),
                    before,
                    "scenario '{name}' advertises '{} {}' but pressing {code:?} changes nothing",
                    hint.key,
                    hint.label
                );
            }
        }
    }

    #[test]
    fn keys_that_are_not_advertised_are_no_ops() {
        let probes = ['s', 'R', 'd', 'n', 'N', 'M', 'b', 'a', 'x', 'o', 'O', 'c', ' ', '[', ']', 't', 'T', 'm', 'u', 'D', 'B', 'H'];
        for (name, app) in scenarios() {
            let advertised: Vec<KeyCode> = hints(&app).iter().filter_map(|h| key_code(h.key)).collect();
            for ch in probes {
                let code = KeyCode::Char(ch);
                // `[ ]` is advertised as a pair; treat both brackets as advertised together.
                let bracket_pair_advertised = (ch == '[' || ch == ']') && advertised.contains(&KeyCode::Char(']'));
                if advertised.contains(&code) || bracket_pair_advertised {
                    continue;
                }
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                // N/M explain refusals via status messages, so for them a status change counts too.
                let fp = if ch == 'N' || ch == 'M' { fingerprint_with_status } else { fingerprint };
                let before = fp(&probe);
                probe.handle_key(code);
                assert_eq!(fp(&probe), before, "scenario '{name}' does NOT advertise {ch:?} yet pressing it changes state");
            }
            // Enter, when not advertised, must also be inert.
            if !advertised.contains(&KeyCode::Enter) {
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                let before = fingerprint(&probe);
                probe.handle_key(KeyCode::Enter);
                assert_eq!(fingerprint(&probe), before, "scenario '{name}' does NOT advertise Enter yet it changes state");
            }
        }
    }

    // ---- content ---------------------------------------------------

    fn picker_with(load: crate::ui::picker::PickerLoad) -> PickerState {
        PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load,
            note: None,
            account: None,
            pool_host: None,
        }
    }

    #[test]
    fn a_loading_or_failed_picker_advertises_only_esc() {
        for load in [crate::ui::picker::PickerLoad::Loading, crate::ui::picker::PickerLoad::Failed("x".into())] {
            let mut a = base();
            a.picker = Some(picker_with(load));
            assert_eq!(keys(&a), vec!["Esc"]);
        }
    }

    #[test]
    fn f2_is_advertised_on_the_four_live_account_fields_and_actually_opens_a_picker() {
        let forms: Vec<(FormKind, usize)> = vec![
            (FormKind::ShareSmb { host: 0, dataset: "d".into(), path: "/d".into(), existing_user: None }, 1),
            (FormKind::ShareNfs { host: 0, dataset: "d".into() }, 3),
            (FormKind::GrantPermission { host: 0, dataset: "d".into() }, 0),
            (FormKind::AddAclEntry { host: 0, path: "/d".into(), position: 0 }, 0),
        ];
        for (kind, idx) in forms {
            let n = match &kind {
                FormKind::ShareSmb { .. } => 5,
                FormKind::GrantPermission { .. } => 3,
                _ => 4, // ShareNfs and the four-field ACL form
            };
            let fields = (0..n).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect();
            let mut a = base();
            a.modal = Some(Modal::Form { title: "t".into(), fields, focused: idx, description: None, kind: kind.clone() });
            assert!(keys(&a).contains(&"F2"), "{kind:?} field {idx}");
            a.handle_modal_key(KeyCode::F(2));
            assert!(a.picker.is_some(), "{kind:?} field {idx}: F2 must open a picker");
        }
    }

    fn keys(app: &App) -> Vec<&'static str> {
        hints(app).iter().map(|h| h.key).collect()
    }


    #[test]
    fn the_backup_form_advertises_f5_and_no_other_form_does() {
        let mut a = base();
        a.modal = Some(Modal::Form {
            title: "t".into(),
            fields: (0..8).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect(),
            focused: 3,
            description: None,
            kind: FormKind::BackupJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
        });
        assert!(keys(&a).contains(&"F5"));
        a.modal = Some(Modal::Form {
            title: "t".into(),
            fields: (0..7).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect(),
            focused: 3,
            description: None,
            kind: FormKind::SnapshotJob { hosts: vec!["local".into()], existing: None, jobs: vec![] },
        });
        assert!(!keys(&a).contains(&"F5"));
    }

    #[test]
    fn datasets_middle_always_advertises_nfs_and_smb_share_when_a_dataset_is_selected() {
        for loaded in [false, true] {
            for nfs in [None, Some(true), Some(false)] {
                for smb in [None, Some(true), Some(false)] {
                    for mp in [None, Some("/tank/a".to_string())] {
                        let mut a = datasets_middle(loaded);
                        a.nfs_available = nfs;
                        a.samba_available = smb;
                        a.datasets[0].mountpoint = mp.clone();
                        let ks = keys(&a);
                        assert!(ks.contains(&"N") && ks.contains(&"M"), "loaded={loaded} nfs={nfs:?} smb={smb:?} mp={mp:?}: {ks:?}");
                    }
                }
            }
        }
        let ks = keys(&datasets_none_selected());
        assert!(!ks.contains(&"N") && !ks.contains(&"M"), "{ks:?}");
    }

    #[test]
    fn pools_middle_advertises_the_pool_actions_and_marks_destroy_dangerous() {
        let app = pools_middle();
        let hs = hints(&app);
        for k in ["Enter", "s", "R", "d", "n", "→"] {
            assert!(hs.iter().any(|h| h.key == k), "missing {k}: {hs:?}");
        }
        let destroy = hs.iter().find(|h| h.key == "d").unwrap();
        assert_eq!(destroy.tone, Tone::Danger);
    }

    #[test]
    fn pool_actions_are_hidden_while_the_right_pane_has_focus() {
        let ks = keys(&pools_right());
        for k in ["s", "R", "d", "n"] {
            assert!(!ks.contains(&k), "{k} must be hidden: {ks:?}");
        }
    }

    #[test]
    fn pool_actions_are_hidden_when_no_pool_is_selected() {
        let ks = keys(&pools_middle_empty());
        for k in ["s", "R", "d", "Enter", "↑↓"] {
            assert!(!ks.contains(&k), "{k} must be hidden: {ks:?}");
        }
        assert!(ks.contains(&"n"));
    }

    #[test]
    fn global_hints_are_always_present_and_pinned() {
        for (name, app) in scenarios() {
            let hs = hints(&app);
            for k in ["?", "q"] {
                let h = hs.iter().find(|h| h.key == k).unwrap_or_else(|| panic!("{name}: missing {k}"));
                assert!(h.pinned, "{name}: {k} must be pinned");
            }
        }
    }

    #[test]
    fn the_shell_key_is_advertised_only_when_there_is_a_host_to_open_one_on() {
        assert!(keys(&pools_middle()).contains(&"!"));
        let no_hosts = App::new(Vec::new());
        assert!(!hints(&no_hosts).iter().any(|h| h.key == "!"));
    }

    #[test]
    fn the_hosts_hints_advertise_the_filter_and_a_set_filter_advertises_esc() {
        let mut app = hosts();
        assert!(keys(&app).contains(&"/"));
        assert!(!keys(&app).contains(&"Esc"));
        app.host_filter = "x".into();
        assert!(hints(&app).iter().any(|h| h.key == "Esc" && h.label == "Clear filter"));
    }

    #[test]
    fn while_a_filter_is_typed_the_hints_are_the_filter_keys_only() {
        let mut app = hosts();
        app.host_filter_editing = true;
        let hs = hints(&app);
        assert_eq!(hs.iter().map(|h| (h.key, h.label)).collect::<Vec<_>>().len() >= 2, true);
        let ks = keys(&app);
        assert!(ks.contains(&"Enter") && ks.contains(&"Esc"));
        assert!(!ks.contains(&"↑↓") && !ks.contains(&"n") && !ks.contains(&"q"), "{ks:?}");
    }

    #[test]
    fn hints_offer_the_hold_key_when_a_snapshot_is_selected() {
        let a = snapshots();
        let hold = hints(&a).into_iter().find(|h| h.key == "H").expect("H is offered");
        assert_eq!(hold.label, "Hold");
        assert_eq!(hold.tone, Tone::Neutral);
        let mut none = snapshots();
        none.snapshots_table.select(None);
        assert!(!keys(&none).contains(&"H"));
    }

    #[test]
    fn hints_offer_the_changes_keys_when_a_snapshot_is_selected() {
        let a = snapshots();
        let ks = keys(&a);
        assert!(ks.contains(&"c") && ks.contains(&"C"), "{ks:?}");
        let mut none = snapshots();
        none.snapshots_table.select(None);
        let ks = keys(&none);
        assert!(!ks.contains(&"c") && !ks.contains(&"C"), "{ks:?}");
    }

    #[test]
    fn hints_in_the_changes_view_offer_what_works_in_each_phase() {
        use crate::ui::changes::{Against, ChangesView, PendingChanges, Phase};
        use crate::zfs::snapshot_changes::{ChangeSummary, Limits};
        let pending = PendingChanges { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s".to_string(), against: Against::Previous, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
        let mut a = snapshots();
        a.changes_view = Some(ChangesView::new(&pending));
        let reading = keys(&a);
        assert!(reading.contains(&"Esc") && !reading.contains(&"Enter") && !reading.contains(&"d"), "{reading:?}");
        let mut view = ChangesView::new(&pending);
        view.phase = Phase::Ready { older: "tank/a@o".to_string(), newer: "tank/a@s".to_string(), summary: ChangeSummary::new(&Limits::default(), None) };
        a.changes_view = Some(view);
        let ready = keys(&a);
        for want in ["↑↓", "Enter", "[ ]", "H", "Esc"] {
            assert!(ready.contains(&want), "{want} in {ready:?}");
        }
        assert!(!ready.contains(&"d"), "the snapshot list's own keys are not offered inside the view: {ready:?}");
    }

    fn browsing(rows: &[(&str, crate::zfs::snapshot_browse::EntryKind)], path: &[&str]) -> App {
        use crate::ui::browse::{BrowseOutcome, BrowseView, PendingBrowse};
        use crate::zfs::snapshot_browse::{ListEntry, Listing, Row};
        let pending = PendingBrowse { host: 0, request: 1, dataset: "tank/a".to_string(), name: "s1".to_string(), path: path.iter().map(|s| (*s).to_string()).collect(), mountpoint: None, zone: None, os: crate::zfs::host_health::HostOs::FreeBsd, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
        let mut view = BrowseView::new(&pending, 0);
        let entries = rows
            .iter()
            .map(|(n, kind)| Row { entry: ListEntry { name: (*n).to_string(), kind: *kind, mode: "drwxr-xr-x".to_string(), owner: "o".to_string(), group: "g".to_string(), size: 5, time: Some(1), link_target: None }, changed: None, created: None })
            .collect();
        view.apply("h", Ok(BrowseOutcome { listing: Listing { entries, truncated: false, unreadable: 0, has_created: true, times_incomplete: false }, mountpoint: "/m".to_string(), zone: None }));
        let mut a = snapshots();
        a.browse_view = Some(view);
        a
    }

    #[test]
    fn hints_offer_browse_when_a_snapshot_is_selected() {
        assert!(keys(&snapshots()).contains(&"f"));
        let mut none = snapshots();
        none.snapshots_table.select(None);
        assert!(!keys(&none).contains(&"f"));
    }

    #[test]
    fn hints_in_the_browser_offer_what_works_in_each_state() {
        use crate::zfs::snapshot_browse::EntryKind::{Directory, File};
        let keys = |a: &App| -> Vec<&'static str> { keys(a).into_iter().filter(|k| !["Tab", "!", "A", "?", "q"].contains(k)).collect() };
        let mut reading = snapshots();
        let pending = crate::ui::browse::PendingBrowse { host: 0, request: 1, dataset: "d".to_string(), name: "s".to_string(), path: vec![], mountpoint: None, zone: None, os: crate::zfs::host_health::HostOs::Other, cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) };
        reading.browse_view = Some(crate::ui::browse::BrowseView::new(&pending, 0));
        assert_eq!(keys(&reading), ["Esc"]);
        let top = browsing(&[("pics", Directory), ("a", File)], &[]);
        let ks = keys(&top);
        for want in ["↑↓", "Enter", "/", "m", "s", "t", "Esc"] {
            assert!(ks.contains(&want), "{want} in {ks:?}");
        }
        assert!(!ks.contains(&"←") && !ks.contains(&"x") && !ks.contains(&"d"), "no Up at the top, no Clear without a filter, none of the list's keys: {ks:?}");
        let mut file_selected = browsing(&[("a", File)], &["pics"]);
        let ks = keys(&file_selected);
        assert!(ks.contains(&"←") && !ks.contains(&"Enter"), "Up inside a folder; Enter only on a folder: {ks:?}");
        file_selected.browse_view.as_mut().unwrap().cycle_kind();
        assert!(keys(&file_selected).contains(&"x"));
        let mut typing = browsing(&[("a", File)], &[]);
        typing.browse_view.as_mut().unwrap().start_input(crate::ui::browse::InputKind::Name);
        assert_eq!(keys(&typing), ["Enter", "Esc"]);
    }

    #[test]
    fn every_key_the_browser_advertises_does_something_and_unadvertised_ones_do_nothing() {
        type Scenario = (&'static str, Box<dyn Fn() -> App>);
        use crate::zfs::snapshot_browse::EntryKind::{Directory, File};
        use crossterm::event::KeyCode;
        let code = |k: &str| match k {
            "↑↓" => KeyCode::Down,
            "←" => KeyCode::Left,
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            other => KeyCode::Char(other.chars().next().unwrap()),
        };
        let fp = |a: &App| a.browse_view.as_ref().map(|v| (v.selected, v.path.clone(), v.rows.len(), v.input.is_some(), v.filter.clone(), matches!(v.phase, crate::ui::browse::Phase::Reading)));
        let scenarios: Vec<Scenario> = vec![
            ("top", Box::new(|| browsing(&[("pics", Directory), ("a", File), ("b", File)], &[]))),
            ("inside", Box::new(|| browsing(&[("pics", Directory), ("a", File)], &["x"]))),
            ("file selected", Box::new(|| {
                let mut a = browsing(&[("pics", Directory), ("a", File)], &[]);
                a.browse_view.as_mut().unwrap().selected = 1;
                a
            })),
        ];
        for (name, make) in &scenarios {
            let advertised: Vec<&'static str> = hints(&make()).iter().map(|h| h.key).filter(|k| !["Tab", "!", "A", "?", "q"].contains(k)).collect();
            for key in &advertised {
                let mut a = make();
                let before = fp(&a);
                a.handle_key(code(key));
                assert!(*key == "↑↓" && name.contains("file") || fp(&a) != before || a.browse_view.is_none(), "scenario '{name}' advertises {key} but it changes nothing");
            }
            for key in ["Enter", "←", "x", "f", "d", "b", "H", "c", "C"] {
                if advertised.contains(&key) {
                    continue;
                }
                let mut a = make();
                let before = fp(&a);
                a.handle_key(code(key));
                assert_eq!(fp(&a), before, "scenario '{name}' does not advertise {key} yet it changes state");
                assert!(a.modal.is_none(), "{key} opened a confirm behind the browser");
            }
        }
    }

    #[test]
    fn hints_for_the_hosts_screen_do_not_include_refresh() {
        // `r` is a no-op in the Hosts section (see event_loop's refresh match).
        assert!(!keys(&hosts()).contains(&"r"));
    }

    #[test]
    fn a_modal_shows_only_its_own_keys() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Info { title: "Help".to_string(), body: String::new() });
        let hs = hints(&app);
        assert_eq!(hs.iter().map(|h| h.key).collect::<Vec<_>>(), ["↑/↓", "any key"]);
        assert!(!hs.iter().any(|h| h.pinned));
    }

    #[test]
    fn confirm_hint_inherits_the_actions_tone() {
        let mut app = pools_middle();
        app.modal = Some(Modal::ConfirmTyped {
            prompt: "Type the pool name to destroy 'tank':".to_string(),
            expected: "tank".to_string(),
            input: String::new(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::DestroyPool { pool: "tank".to_string() } },
        });
        let hs = hints(&app);
        let confirm = hs.iter().find(|h| h.label == "Confirm").unwrap();
        assert_eq!(confirm.tone, Tone::Danger);
        assert!(hs.iter().any(|h| h.key == "type"));
        assert!(hs.iter().any(|h| h.key == "Esc"));
    }

    #[test]
    fn a_confirm_shows_only_the_keys_that_make_sense_for_its_state() {
        let confirm = |choice: Option<bool>, call: MutationCall| {
            let mut app = pools_middle();
            app.modal = Some(Modal::Confirm { prompt: "Scrub pool 'tank'? (y/n)".to_string(), action: PendingAction { host: 0, section: Section::Pools, call }, choice });
            hints(&app)
        };
        let scrub = || MutationCall::ScrubPool { pool: "tank".to_string() };
        let keys = |hs: &[Hint]| hs.iter().map(|h| (h.key, h.label)).collect::<Vec<_>>();
        assert_eq!(keys(&confirm(None, scrub())), vec![("y", "Yes"), ("n/Esc", "No")], "nothing selected: no Enter hint, Enter does nothing");
        let yes = confirm(Some(true), scrub());
        assert_eq!(keys(&yes), vec![("Enter", "Run"), ("n/Esc", "No")]);
        assert_eq!(confirm(Some(false), scrub()).iter().map(|h| (h.key, h.label)).collect::<Vec<_>>(), vec![("Enter", "Cancel"), ("y", "Yes")]);
        let danger = confirm(Some(true), MutationCall::DestroyPool { pool: "tank".to_string() });
        assert_eq!(danger.iter().find(|h| h.key == "Enter").unwrap().tone, Tone::Danger, "Run carries the action's tone");
    }

    #[test]
    fn form_enter_label_is_next_until_the_last_field_then_submit() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Form {
            title: "Create Pool".to_string(),
            fields: vec![
                FormField { label: "Name".to_string(), value: String::new() },
                FormField { label: "Vdev spec".to_string(), value: String::new() },
            ],
            focused: 0,
            description: None,
            kind: FormKind::CreatePool { host: 0 },
        });
        assert_eq!(hints(&app).iter().find(|h| h.key == "Enter").unwrap().label, "Next");
        if let Some(Modal::Form { focused, .. }) = &mut app.modal {
            *focused = 1;
        }
        assert_eq!(hints(&app).iter().find(|h| h.key == "Enter").unwrap().label, "Submit");
    }

    #[test]
    fn form_shows_f2_pick_only_on_a_field_with_a_picker() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Form {
            title: "Grant".to_string(),
            fields: vec![
                FormField { label: "Recipient".to_string(), value: String::new() },
                FormField { label: "Scope".to_string(), value: String::new() },
                FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() },
            ],
            focused: 1,
            description: None,
            kind: FormKind::GrantPermission { host: 0, dataset: "tank/a".to_string() },
        });
        assert!(!hints(&app).iter().any(|h| h.key == "F2"), "field 1 (Scope) has no vocabulary");
        if let Some(Modal::Form { focused, .. }) = &mut app.modal {
            *focused = 2;
        }
        assert!(hints(&app).iter().any(|h| h.key == "F2"), "field 2 has a vocabulary");
    }

    #[test]
    fn an_open_picker_wins_over_the_form_underneath() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Info { title: "x".to_string(), body: String::new() });
        app.picker = Some(crate::ui::picker::PickerState {
            values: vec!["a".to_string()],
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: true,
            load: crate::ui::picker::PickerLoad::Ready,
            note: None,
            account: None,
            pool_host: None,
        });
        let ks = keys(&app);
        assert!(ks.contains(&"Space"), "{ks:?}");
        assert!(!ks.contains(&"any key"), "{ks:?}");
    }

    // ---- fit / render ----------------------------------------------

    fn sample() -> Vec<Hint> {
        vec![
            Hint { key: "s", label: "Scrub", priority: 2, pinned: false, tone: Tone::Neutral },
            Hint { key: "d", label: "Destroy", priority: 2, pinned: false, tone: Tone::Danger },
            Hint { key: "Tab", label: "Section", priority: 5, pinned: false, tone: Tone::Neutral },
            Hint { key: "?", label: "Help", priority: 0, pinned: true, tone: Tone::Neutral },
            Hint { key: "q", label: "Quit", priority: 0, pinned: true, tone: Tone::Neutral },
        ]
    }

    #[test]
    fn fit_keeps_everything_when_it_fits() {
        assert_eq!(fit(&sample(), 200), sample());
    }

    #[test]
    fn fit_drops_the_lowest_priority_hint_first_and_never_a_pinned_one() {
        // Full width: "s Scrub  d Destroy  Tab Section  ? Help  q Quit" = 47.
        let kept = fit(&sample(), 40);
        assert!(!kept.iter().any(|h| h.key == "Tab"), "{kept:?}"); // priority 5 goes first
        assert!(kept.iter().any(|h| h.key == "?") && kept.iter().any(|h| h.key == "q"));
    }

    #[test]
    fn fit_among_equal_priorities_drops_the_rightmost() {
        let kept = fit(&sample(), 31); // forces Tab, then one of s/d (priority 2) out
        assert!(kept.iter().any(|h| h.key == "s"), "{kept:?}");
        assert!(!kept.iter().any(|h| h.key == "d"), "{kept:?}");
    }

    #[test]
    fn fit_at_a_hopeless_width_keeps_only_the_pinned_hints() {
        let kept = fit(&sample(), 3);
        assert_eq!(kept.iter().map(|h| h.key).collect::<Vec<_>>(), vec!["?", "q"]);
    }

    #[test]
    fn hint_line_styles_danger_keys_with_the_danger_style_and_labels_muted() {
        let theme = crate::ui::theme::Theme::ansi();
        let line = hint_line(&sample(), 200, &theme);
        let d = line.spans.iter().find(|s| s.content == "d").unwrap();
        assert_eq!(d.style, theme.danger);
        let label = line.spans.iter().find(|s| s.content == " Destroy").unwrap();
        assert_eq!(label.style, theme.muted);
        let plain = line.spans.iter().find(|s| s.content == "s").unwrap();
        assert_ne!(plain.style, Style::new());
    }

    fn empty_every_list(a: &mut App) {
        a.hosts.clear();
        a.vdevs.clear();
        a.pools.clear();
        a.pool_properties.clear();
        a.datasets.clear();
        a.dataset_properties.clear();
        a.dataset_permissions.clear();
        a.acl_entries.clear();
        a.snapshots.clear();
        a.disks.clear();
        a.shares.clear();
        a.bootenvs.clear();
        a.jobs_view.jobs.clear();
        // A running daemon: its absence is a line of the Dashboard's Needs attention list, and this clears every list.
        a.jobs_view.heartbeat = Some(crate::daemon::heartbeat::Heartbeat { pid: 1, started_at: chrono::Utc::now(), last_tick: chrono::Utc::now() });
        a.cloud_known.clear();
        a.alerts_view.events.clear();
        a.settings_view.rows.clear();
    }

    #[test]
    fn the_move_hint_is_shown_only_while_the_list_has_rows() {
        for (name, mut app) in scenarios() {
            let before = keys(&app).contains(&"↑↓");
            if name != "pools middle, empty" && name != "boot envs, unavailable" {
                assert!(before, "{name} should advertise ↑↓ with rows present");
            }
            empty_every_list(&mut app);
            assert!(!keys(&app).contains(&"↑↓"), "{name} must not advertise ↑↓ over an empty list");
        }
    }

    #[test]
    fn acl_remove_is_advertised_only_when_the_selection_is_a_real_entry() {
        let mut a = datasets_right(DatasetDetailTab::Acl);
        assert!(keys(&a).contains(&"d"));
        a.acl_entries_table.select(Some(a.acl_entries.len()));
        assert!(!keys(&a).contains(&"d"), "selection one past the last entry is not an entry");
        a.acl_entries_table.select(None);
        assert!(!keys(&a).contains(&"d"));
    }

    #[test]
    fn fit_keeps_hints_that_exactly_fill_the_width_and_drops_one_when_a_column_short() {
        assert_eq!(fit(&sample(), 47), sample());
        assert_eq!(fit(&sample(), 46).len(), 4);
    }

    #[test]
    fn hint_line_separates_hints_with_two_spaces_and_never_leads_with_one() {
        let theme = crate::ui::theme::Theme::ansi();
        let text: String = hint_line(&sample(), 200, &theme).spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "s Scrub  d Destroy  Tab Section  ? Help  q Quit");
    }

    // ---- guards hold when every list is populated -----------------------

    /// Fills every list and selects row 0 in every table without moving the
    /// section, pane, tab or drill-down, so a handler that fires outside its
    /// own section has real data to act on.
    fn populate_everything(a: &mut App, with_selected_disks: bool) {
        a.pools = vec![pool("tank"), pool("scratch")];
        a.datasets = vec![dataset("tank/a"), dataset("tank/b")];
        a.snapshots = vec![
            Snapshot { dataset: "tank/a".to_string(), name: "s1".to_string(), used: 1, holds: 0, written: 0 },
            Snapshot { dataset: "tank/a".to_string(), name: "s2".to_string(), used: 1, holds: 0, written: 0 },
        ];
        a.disks = vec![
            DiskInfo { name: "da0".to_string(), size: 1, membership: DiskMembership::Available, ..Default::default() },
            DiskInfo { name: "da1".to_string(), size: 1, membership: DiskMembership::Available, ..Default::default() },
        ];
        a.shares = vec![smb("tank/b"), smb("tank/c")];
        a.jobs_view.jobs = vec![
            crate::jobs::Job { id: "a".to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None },
            crate::jobs::Job { id: "b".to_string(), host: "local".to_string(), kind: crate::jobs::KIND_SCRUB.to_string(), pool: Some("tank".to_string()), schedule: "0 3 * * 0".to_string(), catch_up: true, enabled: false, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None },
        ];
        a.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        a.pool_properties = vec![prop("autoexpand"), prop("comment")];
        a.dataset_properties = vec![prop("compression"), prop("atime")];
        a.dataset_permissions = vec![
            PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
            PermissionRow::Set { name: "s1".to_string(), perms: vec!["create".to_string()] },
        ];
        a.acl_entries_for = Some("/tank/a".to_string());
        a.acl_entries = vec![
            AclEntry { tag: AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
            AclEntry { tag: AclTag::Everyone, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
        ];
        for table in [
            &mut a.pools_table,
            &mut a.datasets_table,
            &mut a.snapshots_table,
            &mut a.disks_table,
            &mut a.shares_table,
            &mut a.jobs_table,
            &mut a.vdevs_table,
            &mut a.pool_properties_table,
            &mut a.dataset_properties_table,
            &mut a.dataset_permissions_table,
            &mut a.acl_entries_table,
        ] {
            table.select(Some(0));
        }
        if with_selected_disks {
            a.selected_disks.insert("da0".to_string());
        }
    }

    fn every_probe_key() -> Vec<KeyCode> {
        let mut keys: Vec<KeyCode> = (b' '..=b'~').map(|b| KeyCode::Char(char::from(b))).collect();
        keys.extend([KeyCode::Enter, KeyCode::Esc, KeyCode::Tab, KeyCode::BackTab, KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right, KeyCode::Backspace]);
        keys
    }

    fn populated_scenarios() -> Vec<(String, App)> {
        let mut out = Vec::new();
        for with_disks in [false, true] {
            for (name, mut app) in scenarios() {
                populate_everything(&mut app, with_disks);
                out.push((format!("{name}{}", if with_disks { " +selected disks" } else { "" }), app));
            }
        }
        out
    }

    fn rebuild_populated(name: &str) -> App {
        populated_scenarios().into_iter().find(|(n, _)| n == name).unwrap().1
    }

    #[test]
    fn unadvertised_keys_stay_inert_even_when_every_list_is_populated() {
        for (name, app) in populated_scenarios() {
            let advertised: Vec<KeyCode> = hints(&app).iter().filter_map(|h| key_code(h.key)).collect();
            let brackets = advertised.contains(&KeyCode::Char(']'));
            for code in every_probe_key() {
                let bracket = matches!(code, KeyCode::Char('[' | ']'));
                // `r` lives in the event loop; Down/Up/Tab/BackTab/Left/Right navigate in many panes
                // without being listed individually, and `?`/`A`/`q` are global.
                if advertised.contains(&code) || (bracket && brackets) {
                    continue;
                }
                if matches!(code, KeyCode::Down | KeyCode::Up | KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('?' | 'A' | 'q')) {
                    continue;
                }
                let mut probe = rebuild_populated(&name);
                // Backing out of a focused pane or a drill-down is real, unlisted navigation.
                if matches!(code, KeyCode::Esc | KeyCode::Backspace) && (probe.pane_focus == PaneFocus::Right || probe.drilled_pool.is_some()) {
                    continue;
                }
                let before = fingerprint(&probe);
                probe.handle_key(code);
                // Esc may legitimately clear a status message; it is excluded from `fingerprint`.
                assert_eq!(fingerprint(&probe), before, "scenario '{name}' does not advertise {code:?} yet pressing it changes state");
            }
        }
    }

    #[test]
    fn every_advertised_key_does_something_even_when_every_list_is_populated() {
        for (name, app) in populated_scenarios() {
            for hint in hints(&app) {
                let Some(code) = key_code(hint.key) else { continue };
                let mut probe = rebuild_populated(&name);
                let before = fingerprint_with_status(&probe);
                probe.handle_key(code);
                assert_ne!(fingerprint_with_status(&probe), before, "scenario '{name}' advertises '{} {}' but {code:?} changes nothing", hint.key, hint.label);
            }
        }
    }

    #[test]
    fn cancel_is_advertised_only_for_a_running_replication() {
        let mut a = jobs(true);
        a.jobs_view.jobs = vec![crate::jobs::Job::test_replicate("r", "tank/data", "hourly", "fbsd16", "backup/data")];
        a.jobs_table.select(Some(0));
        assert!(!keys(&a).contains(&"c"), "not running");
        a.jobs_view.runs = vec![crate::daemon::history::Run { job_id: "r".into(), run_id: "r1".into(), started_at: chrono::Utc::now(), scheduled_for: None, finished_at: None, result: None, message: String::new(), catch_up: false, summary: None }];
        assert!(keys(&a).contains(&"c"));
        a.jobs_view.jobs = vec![crate::jobs::Job::test_snapshot("r", "tank/data", "hourly", 3)];
        assert!(!keys(&a).contains(&"c"), "only replications can be cancelled");
    }

    fn boot_envs(selected: usize) -> App {
        use crate::zfs::bootenv::{ActiveFlags, BootEnv};
        let row = |name: &str, now, reboot, temp, mount: Option<&str>| BootEnv {
            name: name.to_string(),
            flags: ActiveFlags { now, reboot, temp },
            mountpoint: mount.map(str::to_string),
            space: "1M".to_string(),
            created: "2026-10-07 19:26".to_string(),
        };
        let mut a = base();
        go(&mut a, Section::BootEnvs);
        a.bootenvs = vec![
            row("default", true, true, false, Some("/")),
            row("spare", false, false, false, None),
            row("trial", false, false, true, None),
            row("mounted", false, false, false, Some("/tmp/be_mount.X")),
            row("next", false, true, false, None),
        ];
        a.bootenvs_table.select(Some(selected));
        a
    }

    fn boot_envs_none() -> App {
        let mut a = boot_envs(0);
        a.bootenvs_table.select(None);
        a
    }

    fn boot_envs_unavailable() -> App {
        let mut a = base();
        go(&mut a, Section::BootEnvs);
        a.bootenvs_unavailable = Some("bectl: not found".to_string());
        a
    }

    #[test]
    fn boot_envs_advertise_exactly_what_the_selected_row_allows() {
        // `context_hints` is the section's own hints, without the always-present `! A ? q`.
        let k = |a: &App| context_hints(a).iter().map(|h| h.key).filter(|k| k.chars().count() == 1 && *k != "r").collect::<Vec<_>>();
        assert_eq!(k(&boot_envs(0)), vec!["R", "n"], "running: rename and new only");
        assert_eq!(k(&boot_envs(1)), vec!["a", "t", "R", "m", "d", "D", "n"]);
        assert_eq!(k(&boot_envs(2)), vec!["a", "T", "R", "m", "n"], "temporary: clear it before destroying");
        assert_eq!(k(&boot_envs(3)), vec!["a", "t", "R", "u", "d", "D", "n"]);
        assert_eq!(k(&boot_envs(4)), vec!["t", "R", "m", "n"]);
        assert_eq!(k(&boot_envs_none()), vec!["n"]);
        assert!(k(&boot_envs_unavailable()).is_empty(), "nothing can be done to a section that cannot list");
    }

    #[test]
    fn the_jobs_section_advertises_b_for_a_backup_job_and_c_only_for_a_running_cancellable_one() {
        let a = jobs(true);
        let labels: Vec<(&str, &str)> = hints(&a).iter().map(|h| (h.key, h.label)).collect();
        assert!(labels.contains(&("B", "New backup job")), "{labels:?}");
        assert!(labels.contains(&("C", "New cloud mirror")), "{labels:?}");
        assert!(!labels.iter().any(|(k, _)| *k == "c"), "nothing is running");
    }


    #[test]
    fn datasets_advertise_recursive_destroy_only_with_a_dataset_selected() {
        let with = keys(&datasets_middle(false));
        assert!(with.contains(&"D"), "{with:?}");
        let h = hints(&datasets_middle(false));
        let d = h.iter().find(|x| x.key == "D").unwrap();
        assert_eq!((d.label, d.tone), ("Destroy recursively", Tone::Danger));
        assert!(!keys(&datasets_none_selected()).contains(&"D"));
    }


    #[test]
    fn the_key_keys_are_advertised_only_on_an_encrypted_row_and_only_the_one_that_applies() {
        let enc = |locked| crate::zfs::encryption::Enc { cipher: "aes-256-gcm".into(), key_format: "passphrase".into(), root: "tank/a".into(), locked };
        assert!(!keys(&datasets_middle(false)).iter().any(|k| ["L", "U", "C"].contains(k)), "a plain dataset has no key keys");
        let mut open = datasets_middle(false);
        open.encryption.insert("tank/a".into(), enc(false));
        let k = keys(&open);
        assert!(k.contains(&"U") && k.contains(&"C") && !k.contains(&"L"), "{k:?}");
        let mut locked = datasets_middle(false);
        locked.encryption.insert("tank/a".into(), enc(true));
        let k = keys(&locked);
        assert!(k.contains(&"L") && !k.contains(&"U") && !k.contains(&"C"), "{k:?}");
        // A key kept in a file, and a child that uses its parent's key, only refuse: not advertised.
        let mut raw = datasets_middle(false);
        raw.encryption.insert("tank/a".into(), crate::zfs::encryption::Enc { key_format: "raw".into(), ..enc(true) });
        assert!(!keys(&raw).iter().any(|k| ["L", "U", "C"].contains(k)), "{:?}", keys(&raw));
        let mut child = datasets_middle(false);
        child.encryption.insert("tank/a".into(), crate::zfs::encryption::Enc { root: "tank".into(), ..enc(false) });
        let k = keys(&child);
        assert!(k.contains(&"U") && !k.contains(&"C"), "a child can be unloaded through its root but not changed: {k:?}");
    }

    #[test]
    fn the_acl_tab_hides_add_and_remove_on_a_posix_acltype_dataset() {
        let mut a = datasets_right(DatasetDetailTab::Acl);
        assert!(keys(&a).contains(&"n") && keys(&a).contains(&"d"));
        a.dataset_properties_for = Some("tank/a".to_string());
        a.dataset_properties = vec![Property { name: "acltype".to_string(), value: "posix".to_string(), source: PropertySource::Local }];
        let k = keys(&a);
        assert!(!k.contains(&"n") && !k.contains(&"d"), "{k:?}");
    }
}
