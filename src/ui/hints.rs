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
    let mut v = context_hints(app);
    v.push(h("A", "About", 9));
    v.push(h("?", "Help", 0).pinned());
    v.push(h("q", "Quit", 0).pinned());
    v
}

fn picker_hints(picker: &PickerState) -> Vec<Hint> {
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
        Modal::Info { .. } => vec![h("any key", "Close", 0)],
        Modal::Confirm { action, .. } => {
            vec![h("y/Enter", "Confirm", 0).tone(action.call.tone()), h("n/Esc", "Cancel", 0)]
        }
        Modal::ConfirmTyped { action, .. } => vec![
            h("type", "name to confirm", 2),
            h("Enter", "Confirm", 0).tone(action.call.tone()),
            h("Esc", "Cancel", 0),
        ],
        Modal::Form { fields, focused, kind, .. } => {
            let enter = if focused + 1 < fields.len() { "Next" } else { "Submit" };
            let mut v = vec![h("Enter", enter, 0), h("Tab", "Next field", 2), h("Shift+Tab", "Prev field", 3)];
            let value = fields.get(*focused).map(|f| f.value.as_str()).unwrap_or("");
            if build_picker_state(kind, *focused, value).is_some() {
                v.push(h("F2", "Pick", 1));
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
        v.push(h("a", "Add vdev", 3));
        v.push(h("r", "Refresh", 4));
        v.push(h("Esc", "Back", 1));
        return v;
    }

    let section = app.current_section();
    let right = app.pane_focus == PaneFocus::Right;
    match (section, right) {
        (Section::Hosts, _) => {
            if !app.hosts.is_empty() {
                v.push(h("↑↓", "Host", 1));
            }
            v.push(h("n", "Add host", 2));
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
            if selected {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("s", "Snapshot", 2));
                if app.shares_loaded && app.nfs_available != Some(false) {
                    v.push(h("N", "NFS share", 3));
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
                    if app.selected_dataset_mountpoint().is_some() {
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
        (Section::Snapshots, _) => {
            if !app.snapshots.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.snapshots_table.selected().is_some() {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("b", "Rollback", 2).tone(Tone::Danger));
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
            if app.shares_table.selected().is_some() {
                v.push(h("Enter", "Edit", 1));
                v.push(h("d", "Remove", 2).tone(Tone::Warn));
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
        Dataset { name: name.to_string(), kind: DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{name}")) }
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
        go(&mut a, Section::Datasets);
        a.datasets = vec![dataset("tank/a"), dataset("tank/b")];
        a.datasets_table.select(Some(0));
        a.shares_loaded = shares_loaded;
        a.nfs_available = Some(true);
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
            Snapshot { dataset: "tank/a".to_string(), name: "s1".to_string(), used: 1 },
            Snapshot { dataset: "tank/a".to_string(), name: "s2".to_string(), used: 1 },
        ];
        a.snapshots_table.select(Some(0));
        a
    }

    fn disks(selected: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Disks);
        a.disks = vec![
            DiskInfo { name: "da0".to_string(), size: 1, membership: DiskMembership::Available },
            DiskInfo { name: "da1".to_string(), size: 1, membership: DiskMembership::Available },
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
            ("datasets right: properties", datasets_right(DatasetDetailTab::Properties)),
            ("datasets right: permissions", datasets_right(DatasetDetailTab::Permissions)),
            ("datasets right: acl", datasets_right(DatasetDetailTab::Acl)),
            ("snapshots", snapshots()),
            ("disks, none selected", disks(false)),
            ("disks, one selected", disks(true)),
            ("shares", shares()),
        ]
    }

    // ---- fingerprint ----------------------------------------------

    /// Everything a key press can change, EXCEPT `status_message`. A key
    /// that only sets a status message ("select a disk first…") is, for
    /// the hint bar's purposes, a no-op: it must not be advertised.
    #[derive(Debug, PartialEq)]
    struct Fingerprint {
        modal_open: bool,
        picker_open: bool,
        section: usize,
        focus: PaneFocus,
        tab: DatasetDetailTab,
        drilled: Option<String>,
        host: usize,
        quit: bool,
        selected_disks: Vec<String>,
        selections: Vec<Option<usize>>,
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
            host: a.selected_host,
            quit: a.should_quit,
            selected_disks: disks,
            selections: vec![
                a.pools_table.selected(),
                a.datasets_table.selected(),
                a.snapshots_table.selected(),
                a.disks_table.selected(),
                a.shares_table.selected(),
                a.vdevs_table.selected(),
                a.pool_properties_table.selected(),
                a.dataset_properties_table.selected(),
                a.dataset_permissions_table.selected(),
                a.acl_entries_table.selected(),
            ],
        }
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
                let before = fingerprint(&probe);
                probe.handle_key(code);
                assert_ne!(
                    fingerprint(&probe),
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
        let probes = ['s', 'R', 'd', 'n', 'N', 'b', 'a', 'x', 'o', 'O', 'c', ' ', '[', ']'];
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
                let before = fingerprint(&probe);
                probe.handle_key(code);
                assert_eq!(fingerprint(&probe), before, "scenario '{name}' does NOT advertise {ch:?} yet pressing it changes state");
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

    fn keys(app: &App) -> Vec<&'static str> {
        hints(app).iter().map(|h| h.key).collect()
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
    fn hints_for_the_hosts_screen_do_not_include_refresh() {
        // `r` is a no-op in the Hosts section (see event_loop's refresh match).
        assert!(!keys(&hosts()).contains(&"r"));
    }

    #[test]
    fn a_modal_shows_only_its_own_keys() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Info { title: "Help".to_string(), body: String::new() });
        let hs = hints(&app);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].key, "any key");
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
    fn plain_confirm_shows_y_and_n() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Confirm {
            prompt: "Scrub pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
        });
        let hs = hints(&app);
        assert!(hs.iter().any(|h| h.key == "y/Enter"));
        assert!(hs.iter().any(|h| h.key == "n/Esc"));
        assert_eq!(hs.iter().find(|h| h.key == "y/Enter").unwrap().tone, Tone::Neutral);
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
            focused: 0,
            description: None,
            kind: FormKind::GrantPermission { host: 0, dataset: "tank/a".to_string() },
        });
        assert!(!hints(&app).iter().any(|h| h.key == "F2"), "field 0 has no vocabulary");
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
            checked: Default::default(),
            multi_select: true,
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
}
