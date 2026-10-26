//! The state machine's tests, by topic. What several topics use (the builders of apps, forms and fakes) lives here;
//! each topic file has the tests of one part and the helpers only it uses.

use super::*;
// The form builders and field parsers these tests exercise live in the `forms` module.
#[allow(clippy::wildcard_imports)]
use super::forms::*;

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
use crate::exec::test_support::FakeRunner;
use crate::zfs::property::PropertySource;

fn confirm_scrub() -> (App, crate::ui::modal::PendingAction) {
    let mut app = App::new(vec!["local".to_string()]);
    let action = crate::ui::modal::PendingAction { host: 0, section: Section::Pools, call: crate::ui::modal::MutationCall::ScrubPool { pool: "tank".to_string() } };
    app.modal = Some(crate::ui::modal::Modal::Confirm { prompt: "Scrub tank? (y/n)".to_string(), action: action.clone(), choice: None });
    (app, action)
}

/// `None`: no confirm is open; `Some(None)`: open, nothing selected; `Some(Some(yes))`: the selection.
#[allow(clippy::option_option)]
fn choice_of(app: &App) -> Option<Option<bool>> {
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { choice, .. }) => Some(*choice),
        _ => None,
    }
}

fn datasets_app_with_mount(mountpoint: Option<&str>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd); // shares are edited only on a host known to be FreeBSD
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/media".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.shares_loaded = true;
    app.samba_available = Some(true);
    app
}

/// Feeds each value then Enter through `handle_modal_key` (Enter
/// advances; the 4th submits); returns what the final Enter produced.
fn type_into_smb_form(app: &mut App, values: [&str; 5]) -> Option<PendingAction> {
    let mut last = None;
    for v in values {
        for c in v.chars() {
            // '\u{8}' stands for Backspace (the read-only and guest fields are pre-filled with "n")
            app.handle_modal_key(if c == '\u{8}' { KeyCode::Backspace } else { KeyCode::Char(c) });
        }
        last = app.handle_modal_key(KeyCode::Enter);
    }
    last
}

fn open_blank_smb_form() -> App {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('M'));
    // clear the suggested name so each test types its own
    for _ in 0..5 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    app
}

fn open_grant_permission_form(app: &mut App) {
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right); // Right focus, Properties tab
    app.handle_key(KeyCode::Char(']')); // Permissions tab
    app.handle_key(KeyCode::Char('n')); // open Grant Permission form
}

fn filter_app() -> App {
    let mut app = in_section(Section::Hosts);
    app.hosts = vec!["local".into(), "fbsd16".into(), "t1".into(), "nas".into()];
    app.set_host_addresses(vec!["local".into(), "10.0.0.60".into(), "10.0.0.61".into(), "10.0.0.9".into()]);
    app.set_host_tags(vec![String::new(), String::new(), String::new(), "Backup".into()]);
    app.host_probes.insert("t1".into(), HostProbeState { kind: Some(crate::zfs::host_health::HostKind::Vm("bhyve".into())), ..HostProbeState::default() });
    app
}

fn type_filter(app: &mut App, text: &str) {
    app.handle_key(KeyCode::Char('/'));
    for c in text.chars() {
        app.handle_key(KeyCode::Char(c));
    }
}

fn alerts_app(config: &str) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.config_path = dir.path().join("config.toml");
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
    app.refresh_alerts();
    (app, dir)
}

fn open_alerts_form(app: &mut App) -> Vec<String> {
    app.handle_key(KeyCode::Char('e'));
    form_values(app)
}

fn fill(app: &mut App, values: [&str; 6]) {
    if let Some(Modal::Form { fields, .. }) = &mut app.modal {
        for (f, v) in fields.iter_mut().zip(values) {
            f.value = v.to_string();
        }
    }
}

fn submit_alerts_form(app: &mut App) -> Option<PendingAction> {
    let mut last = None;
    for _ in 0..6 {
        last = app.handle_modal_key(KeyCode::Enter);
    }
    last
}

fn enter_datasets_acl_tab(app: &mut App) {
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.handle_key(KeyCode::Right); // -> Right focus, Properties tab
    app.handle_key(KeyCode::Char(']')); // -> Permissions tab
    app.handle_key(KeyCode::Char(']')); // -> Acl tab
}

fn accounts_fixture() -> crate::zfs::accounts::HostAccounts {
    use crate::zfs::accounts::{Account, HostAccounts};
    HostAccounts {
        users: vec![Account { name: "carol".into(), id: 1003 }, Account { name: "root".into(), id: 0 }],
        groups: vec![Account { name: "staff".into(), id: 1100 }],
        group_error: None,
    }
}

/// An App with an SMB share form open on its "Valid user" field (index 1) for `host`.
fn smb_form_app(host: usize) -> App {
    let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
    app.modal = Some(Modal::Form {
        title: "SMB share".to_string(),
        fields: vec![
            FormField { label: "Share name".into(), value: "media".into() },
            FormField { label: "Valid user".into(), value: "root".into() },
            FormField { label: "Password".into(), value: String::new() },
            FormField { label: "Read-only".into(), value: "n".into() },
            FormField { label: "Guest".into(), value: "n".into() },
        ],
        focused: 1,
        description: None,
        kind: FormKind::ShareSmb { host, dataset: "tank/a".into(), path: "/tank/a".into(), existing_user: None },
    });
    app
}

fn press_f2(app: &mut App) {
    app.handle_modal_key(KeyCode::F(2));
}

// ---- mutation-testing gap fillers ------------------------------------

fn wait_until(app: &mut App, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if done(app) || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn app_with_jobs(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    crate::jobs::save(&dir.path().join("jobs.toml"), jobs_list).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Jobs).unwrap();
    (app, dir)
}

fn scrub_job(id: &str) -> crate::jobs::Job {
    crate::jobs::Job { id: id.into(), host: "local".into(), kind: crate::jobs::KIND_SCRUB.into(), pool: Some("tank".into()), schedule: "0 3 * * 0".into(), catch_up: true, enabled: true, created_at: None, dataset: None, recursive: false, prefix: None, keep: None, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, check_days: None, remote: None, bwlimit: None, force: false, raw: false, repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None }
}

fn pools_app_with_one_pool() -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.selected_host = 1;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.pools_table.select(Some(0));
    app
}

fn form_values(app: &App) -> Vec<String> {
    match &app.modal {
        Some(Modal::Form { fields, .. }) => fields.iter().map(|f| f.value.clone()).collect(),
        other => panic!("expected a form, got {other:?}"),
    }
}

fn at_utc(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
}

fn job_kind(existing: Option<crate::jobs::Job>, taken: &[&str]) -> FormKind {
    FormKind::Job { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), taken_ids: taken.iter().map(|s| (*s).to_string()).collect() }
}

fn fields(values: [&str; 4]) -> Vec<FormField> {
    ["Host", "Pool", "Schedule", "Catch up (y/n)"].iter().zip(values).map(|(l, v)| FormField { label: (*l).to_string(), value: v.to_string() }).collect()
}

fn selected_jobs_app(jobs_list: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let (mut app, dir) = app_with_jobs(jobs_list);
    app.refresh_jobs();
    app.handle_key(KeyCode::Down);
    (app, dir)
}

fn in_section(section: Section) -> App {
    let mut app = App::new(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
    app.pane_focus = PaneFocus::Middle;
    app
}

fn pool_fixture(name: &str) -> crate::zfs::pool::Pool {
    crate::zfs::pool::Pool { name: name.to_string(), size: 1, allocated: 0, free: 1, health: crate::zfs::pool::PoolHealth::Online }
}

/// Presses Up with the table at row 1 and checks it moved to row 0, while
/// every other table in `others` was left alone.
fn up_moves(app: &mut App, table: fn(&mut App) -> &mut TableState, others: &[fn(&mut App) -> &mut TableState]) {
    table(app).select(Some(1));
    for o in others {
        o(app).select(Some(1));
    }
    app.handle_key(KeyCode::Up);
    assert_eq!(table(app).selected(), Some(0));
    for o in others {
        assert_eq!(o(app).selected(), Some(1), "Up must only move the table the user is looking at");
    }
}

fn down_moves(app: &mut App, table: fn(&mut App) -> &mut TableState) {
    table(app).select(Some(0));
    app.handle_key(KeyCode::Down);
    assert_eq!(table(app).selected(), Some(1));
}

// ---- auto refresh -----------------------------------------------------

const TEN: Duration = Duration::from_secs(10);

fn runner(out: &str) -> Arc<dyn CommandRunner> {
    Arc::new(FakeRunner::new(out))
}

fn snapshot_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::SnapshotJob { hosts: vec!["local".to_string()], existing: existing.map(Box::new), jobs }
}

fn snap_fields(values: [&str; 7]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
}

fn snap_err(values: [&str; 7], jobs: Vec<crate::jobs::Job>) -> String {
    build_snapshot_job_action(&snapshot_kind(None, jobs), &snap_fields(values), std::path::Path::new("/x"), chrono::Utc::now()).unwrap_err()
}

fn repl_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::ReplicateJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn repl_fields(values: [&str; 8]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect()
}

const GOOD: [&str; 8] = ["local", "tank/data", "hourly", "fbsd16", "backup/data", "n", "30 * * * *", "y"];

fn be(name: &str, now: bool, reboot: bool, temp: bool, mount: Option<&str>) -> crate::zfs::bootenv::BootEnv {
    crate::zfs::bootenv::BootEnv {
        name: name.to_string(),
        flags: crate::zfs::bootenv::ActiveFlags { now, reboot, temp },
        mountpoint: mount.map(str::to_string),
        space: "1M".to_string(),
        created: "2026-10-07 19:26".to_string(),
    }
}

fn bootenv_app(rows: Vec<crate::zfs::bootenv::BootEnv>, select: usize) -> App {
    let mut app = in_section(Section::BootEnvs);
    app.bootenvs = rows;
    app.bootenvs_table.select(Some(select));
    app
}

fn running() -> crate::zfs::bootenv::BootEnv {
    be("default", true, true, false, Some("/"))
}

fn spare() -> crate::zfs::bootenv::BootEnv {
    be("spare", false, false, false, None)
}

/// Opens the create form, types both fields and presses Enter on the last one.
fn submit(app: &mut App, name: &str, from: &str) -> Option<PendingAction> {
    app.handle_key(KeyCode::Char('n'));
    for c in name.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Tab);
    for c in from.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter)
}

const BACKUP_GOOD: [&str; 10] = ["local", "tank/data", "", "hourly", "", "/mnt/usb/restic", "/root/pw", "7d 4w", "30 3 * * *", "y"];

fn backup_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::BackupJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn backup_fields(values: [&str; 10]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: "x".to_string(), value: (*v).to_string() }).collect()
}


fn destroy_app() -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    let ds = |name: &str| crate::zfs::dataset::Dataset { name: name.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 };
    app.datasets = vec![ds("tank/data"), ds("tank/data/a"), ds("tank/data/b"), ds("tank/data2"), ds("tank/other")];
    app.datasets_table.select(Some(0));
    let snap = |d: &str, n: &str| crate::zfs::snapshot::Snapshot { dataset: d.to_string(), name: n.to_string(), used: 0, holds: 0, written: 0 };
    app.snapshots = vec![snap("tank/data", "s1"), snap("tank/data", "s2"), snap("tank/data/a", "s1"), snap("tank/data2", "x"), snap("tank/other", "x")];
    app.snapshots_loaded = true;
    app
}


fn rename_to(app: &mut App, new_name: &str) -> PendingAction {
    app.handle_key(KeyCode::Char('R'));
    for c in new_name.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter).expect("the rename form submits")
}

fn names(app: &App) -> Vec<&str> {
    app.bootenvs.iter().map(|b| b.name.as_str()).collect()
}

fn open_form(app: &mut App, kind: FormKind, values: &[&str]) {
    let fields = values.iter().map(|v| FormField { label: "f".to_string(), value: (*v).to_string() }).collect::<Vec<_>>();
    app.modal = Some(Modal::Form { title: "t".to_string(), focused: fields.len() - 1, fields, description: None, kind });
}

fn submit_problem(kind: FormKind, values: &[&str]) -> String {
    let mut app = App::new(vec!["local".to_string()]);
    open_form(&mut app, kind, values);
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "an invalid form submits nothing");
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "and stays open so the typing is not lost");
    app.status_message.unwrap_or_default()
}

fn acl_app_with_acltype(value: Option<&str>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: Some("/tank/data".to_string()), quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    enter_datasets_acl_tab(&mut app);
    app.acl_entries = vec![crate::acl::AclEntry { tag: crate::acl::AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: crate::acl::AclEntryType::Allow }];
    app.acl_entries_table.select(Some(0));
    app.acl_entries_for = Some("/tank/data".to_string());
    if let Some(v) = value {
        app.dataset_properties_for = Some("tank/data".to_string());
        app.dataset_properties = vec![Property { name: "acltype".to_string(), value: v.to_string(), source: PropertySource::Local }];
    }
    app
}

fn ds_named(name: &str, mountpoint: Option<&str>) -> crate::zfs::dataset::Dataset {
    crate::zfs::dataset::Dataset { name: name.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: mountpoint.map(str::to_string), quota: 0, refquota: 0 }
}

fn datasets_app_selecting(datasets: Vec<crate::zfs::dataset::Dataset>, selected: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    let index = datasets.iter().position(|d| d.name == selected).unwrap();
    app.datasets = datasets;
    app.datasets_table.select(Some(index));
    app.snapshots_loaded = true;
    app
}

fn probes_app() -> App {
    App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string()])
}

/// Enter on a finished irreversible form, then the confirmation's `y` and Enter.
fn submit_and_confirm(app: &mut App) -> Option<PendingAction> {
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None);
    // a vdev add waits for the host's layout before the question can be answered
    if !app.pending_add_checks.is_empty() {
        let check = app.pending_add_checks.remove(0);
        app.on_add_checked(check, Ok("would update 'tank' to the following configuration:\n\n\ttank\n".to_string()));
    }
    app.handle_modal_key(KeyCode::Char('y'));
    app.handle_modal_key(KeyCode::Enter)
}

/// Presses Enter on a finished irreversible form and returns the prompt of the confirmation it opens,
/// asserting nothing ran yet.
fn submit_to_confirm(app: &mut App) -> String {
    assert_eq!(app.handle_modal_key(KeyCode::Enter), None, "an irreversible form must not run on Enter");
    match &app.modal {
        Some(Modal::Confirm { prompt, choice: None, .. }) => prompt.clone(),
        other => panic!("expected an unanswered confirmation, got {other:?}"),
    }
}

fn pool_for_forms(app: &mut App) {
    app.pools = vec![Pool { name: "tank".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online }];
    app.handle_key(KeyCode::Tab);
    app.pools_table.select(Some(0));
    app.drilled_pool = Some("tank".to_string());
}

// ---- job-save confirms ----

fn snap_job(keep: Option<u32>) -> crate::jobs::Job {
    crate::jobs::Job {
        id: "snap-local-tank-data".into(), host: "local".into(), kind: crate::jobs::KIND_SNAPSHOT.into(), pool: None, schedule: "0 * * * *".into(), catch_up: true, enabled: true,
        created_at: None, dataset: Some("tank/data".into()), recursive: false, prefix: Some("hourly".into()), keep, dest_host: None, dest_dataset: None, dest_dir: None, dest_keep: None, check_days: None, remote: None, bwlimit: None, force: false, raw: false,
        repository: None, password_file: None, retention: None, source: None, snapshot: None, path: None,
    }
}

fn repl_job(force: bool) -> crate::jobs::Job {
    crate::jobs::Job { kind: crate::jobs::KIND_REPLICATE.into(), keep: None, dest_host: Some("fbsd16".into()), dest_dataset: Some("backup/data".into()), force, ..snap_job(None) }
}


fn submit_backup_form(values: [&str; 10]) -> (App, Option<PendingAction>) {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: backup_fields(values), focused: 9, description: None, kind: backup_kind(None, vec![]) });
    let out = app.handle_modal_key(KeyCode::Enter);
    (app, out)
}

/// The values and focused index of the open form, if one is open.
fn open_form_values(app: &App) -> Option<(Vec<String>, usize)> {
    match &app.modal {
        Some(Modal::Form { fields, focused, .. }) => Some((fields.iter().map(|f| f.value.clone()).collect(), *focused)),
        _ => None,
    }
}

fn backup_form_app(values: [&str; 10]) -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.modal = Some(Modal::Form { title: "x".into(), fields: backup_fields(values), focused: 5, description: None, kind: backup_kind(None, vec![]) });
    app
}

fn keep_form_app(keep: &str) -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    let mut values: Vec<&str> = GOOD.to_vec();
    values.push(keep);
    let fields = values.iter().map(|v| FormField { label: String::new(), value: (*v).to_string() }).collect();
    app.modal = Some(Modal::Form { title: "x".into(), fields, focused: 8, description: None, kind: repl_kind(None, vec![]) });
    app
}

fn target_snaps(n: u64) -> Vec<crate::zfs::replication::SnapInfo> {
    (1..=n).map(|i| crate::zfs::replication::SnapInfo { name: format!("hourly-20261006T{i:02}0000Z"), guid: 100 + i, createtxg: i }).collect()
}

fn answer_prune_check(app: &mut App, listing: crate::error::Result<Vec<crate::zfs::replication::SnapInfo>>) {
    app.tx.send(WorkerMsg::PrunePreview(1, "backup/data".into(), listing)).unwrap();
    app.drain_worker_messages();
}

fn encrypted(app: &mut App, name: &str, host: usize) {
    app.encryption.insert(name.into(), crate::zfs::encryption::Enc { cipher: "aes-256-gcm".into(), key_format: "passphrase".into(), root: name.into(), locked: false });
    app.encryption_for = host;
}

const MIRROR_GOOD: [&str; 9] = ["local", "tank/data", "", "hourly", "", "aws:bkt/photos", "", "30 3 * * *", "y"];

fn mirror_kind(existing: Option<crate::jobs::Job>, jobs: Vec<crate::jobs::Job>) -> FormKind {
    FormKind::MirrorJob { hosts: vec!["local".to_string(), "fbsd16".to_string()], existing: existing.map(Box::new), jobs }
}

fn mirror_fields(values: [&str; 9]) -> Vec<FormField> {
    values.iter().map(|v| FormField { label: "x".to_string(), value: (*v).to_string() }).collect()
}

fn mirror_build(values: [&str; 9], jobs: Vec<crate::jobs::Job>) -> std::result::Result<PendingAction, String> {
    build_mirror_job_action(&mirror_kind(None, jobs), &mirror_fields(values), std::path::Path::new("/state"), chrono::Utc::now())
}

// ---- Cloud ----

fn cloud_mirror(id: &str, host: &str, remote: &str) -> crate::jobs::Job {
    let mut j = crate::jobs::Job::test_mirror(id, "tank/data", "hourly", remote);
    j.host = host.into();
    j
}

fn cloud_app(jobs: &[crate::jobs::Job]) -> (App, tempfile::TempDir) {
    let (mut app, dir) = app_with_jobs(jobs);
    app.hosts = vec!["local".to_string(), "fbsd16".to_string()];
    app.selected_section = app.sections.iter().position(|s| *s == Section::Cloud).unwrap();
    (app, dir)
}

// ---- host resources (Dashboard) ----

fn reading(free_gib: u64) -> crate::zfs::host_resources::Resources {
    crate::zfs::host_resources::Resources { cores: 4, physical: 20 << 30, free: free_gib << 30, inactive: 0, arc: 0, load: None, cpu_ticks: None, swap: None, cache: None }
}

// ---- Dashboard ----

fn dash_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.config_path = dir.path().join("config.toml");
    app.selected_section = app.sections.iter().position(|s| *s == Section::Dashboard).unwrap();
    (app, dir)
}

fn reached_probe(capacity: u8) -> crate::zfs::host_health::HostProbe {
    let pool = crate::zfs::host_health::PoolProbe { name: "tank".into(), health: crate::zfs::pool::PoolHealth::Online, capacity_percent: capacity, frag_percent: None, errors: Some(0), scan: Some(crate::zfs::host_health::ScanSummary::Never), last_scrub: None };
    crate::zfs::host_health::HostProbe::Reached { rtt_ms: 2, pools: vec![pool], kind: None, os: None, os_name: None }
}

/// A Dashboard app whose daemon is up and whose `local` host has these datasets, a week old and unprotected.
fn unprotected_app(names: &[&str]) -> App {
    let (mut app, dir) = dash_app();
    std::mem::forget(dir);
    app.jobs_view.loaded = true;
    app.jobs_view.heartbeat = Some(crate::daemon::heartbeat::Heartbeat { pid: 1, started_at: chrono::Utc::now() - chrono::Duration::days(1), last_tick: chrono::Utc::now() });
    app.host_probes.insert("local".into(), HostProbeState { last: Some(reached_probe(10)), ..HostProbeState::default() });
    let created = Some((chrono::Utc::now() - chrono::Duration::days(7)).timestamp());
    let seen = names.iter().map(|n| crate::daemon::storage_check::SeenDataset { name: (*n).into(), volume: false, created }).collect();
    let entry = crate::daemon::storage_check::StorageResult { checked_at: chrono::Utc::now(), datasets: vec![], seen, pools: vec![], zfs_version: None, locked: vec![], integrity: crate::daemon::storage_check::Integrity::default(), integrity_stale: false, error: None };
    app.storage_check.hosts.insert("local".into(), entry);
    app
}

fn unprotected(snapshots: Option<crate::daemon::protection::Gap>, copies: Option<crate::daemon::protection::CopyGap>, dataset: &str) -> crate::daemon::protection::Unprotected {
    crate::daemon::protection::Unprotected { host: "fbsd16".into(), dataset: dataset.into(), tier: crate::daemon::protection::Tier::Data, snapshots, copies }
}

fn list_of(gaps: Vec<crate::daemon::protection::Unprotected>) -> Modal {
    Modal::Choose { title: "Unprotected data".into(), note: "n".into(), rows: gaps.iter().map(|g| g.dataset.clone()).collect(), selected: 0, then: crate::ui::modal::ChooseThen::Unprotected(gaps), look: crate::ui::modal::ChooseLook::default() }
}

fn field(modal: Option<&Modal>, n: usize) -> (String, String) {
    match modal {
        Some(Modal::Form { fields, .. }) => (fields[n].label.clone(), fields[n].value.clone()),
        other => panic!("expected a form, got {other:?}"),
    }
}

fn settings_app(config: &str) -> (App, tempfile::TempDir) {
    let (mut app, dir) = alerts_app(config);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
    app.refresh_settings();
    (app, dir)
}

fn select_setting(app: &mut App, label: &str) {
    let at = app.settings_view.rows.iter().position(|r| r.label == label).unwrap_or_else(|| panic!("no row {label}"));
    app.settings_table.select(Some(at));
}

/// Enter through every field of the open form, then answer the confirmation with `y` and Enter: the save to run.
fn submit_setting(app: &mut App) -> Option<PendingAction> {
    let fields = match &app.modal {
        Some(Modal::Form { fields, .. }) => fields.len(),
        other => panic!("expected a form, got {other:?}"),
    };
    for _ in 0..fields {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    if !matches!(app.modal, Some(Modal::Confirm { .. })) {
        return None;
    }
    app.handle_modal_key(KeyCode::Char('y'));
    app.handle_modal_key(KeyCode::Enter)
}

fn set_setting_fields(app: &mut App, values: &[&str]) {
    let Some(Modal::Form { fields, .. }) = &mut app.modal else { panic!("no form open") };
    for (f, v) in fields.iter_mut().zip(values) {
        f.value = (*v).to_string();
    }
}

fn run_save(action: &PendingAction) {
    action.call.run(&crate::exec::test_support::FakeRunner::new("")).unwrap();
}

fn saved_settings(app: &mut App, dir: &tempfile::TempDir, config: &str) {
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    app.tx.send(WorkerMsg::MutationDone(0, Section::Settings, Ok(()))).unwrap();
    app.drain_worker_messages();
}

fn hosts_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string()]);
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Hosts).unwrap();
    app.selected_host = 1;
    (app, dir)
}

fn wait_for_host_check(app: &mut App) {
    for _ in 0..200 {
        app.drain_worker_messages();
        if !app.host_check_running {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the check never finished");
}

/// Two hosts: the hub (FreeBSD) and `ub24`, whose probe said it runs `os`.
fn two_os_app(os: crate::zfs::host_health::HostOs) -> App {
    use crate::zfs::host_health::{HostOs, HostProbe};
    let mut app = App::new(vec!["local".to_string(), "ub24".to_string()]);
    let reached = |os| HostProbe::Reached { rtt_ms: 1, pools: vec![], kind: None, os: Some(os), os_name: None };
    app.tx.send(WorkerMsg::HostHealth("local".into(), reached(HostOs::FreeBsd))).unwrap();
    app.tx.send(WorkerMsg::HostHealth("ub24".into(), reached(os))).unwrap();
    app.drain_worker_messages();
    app
}

fn go_to(app: &mut App, section: Section) {
    app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
}

/// A runner that must never be asked anything: a dimmed section runs no command.
struct NoCalls;

impl crate::exec::CommandRunner for NoCalls {
    fn run(&self, cmd: &str, _: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
        panic!("a dimmed section ran {cmd}")
    }
    fn run_with_stdin(&self, cmd: &str, a: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
        self.run(cmd, a)
    }
    fn run_unprivileged(&self, cmd: &str, a: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
        self.run(cmd, a)
    }
}

/// Datasets with the Properties tab focused on a filesystem (or volume) that uses 5G (3G of its own), has 100G available
/// and the given limits (bytes, `0` = none).
fn quota_app(volume: bool, quota: u64, refquota: u64, reservation: u64, refreservation: u64) -> App {
    let g: u64 = 1 << 30;
    let mut app = App::new(vec!["local".to_string()]);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Right);
    app.dataset_properties_for = Some("tank/data".to_string());
    let prop = |name: &str, value: u64| Property { name: name.to_string(), value: value.to_string(), source: PropertySource::Local };
    app.dataset_properties = vec![
        prop("quota", quota),
        prop("refquota", refquota),
        prop("reservation", reservation),
        prop("refreservation", refreservation),
        Property { name: "used".into(), value: (5 * g).to_string(), source: PropertySource::Other("-".into()) },
        Property { name: "referenced".into(), value: (3 * g).to_string(), source: PropertySource::Other("-".into()) },
        Property { name: "available".into(), value: (100 * g).to_string(), source: PropertySource::Other("-".into()) },
        Property { name: "type".into(), value: if volume { "volume" } else { "filesystem" }.into(), source: PropertySource::Other("-".into()) },
    ];
    app
}

/// Opens the edit form on the property `name` (selects its row, presses Enter).
fn open_property(app: &mut App, name: &str) {
    let row = app.dataset_properties.iter().position(|p| p.name == name).unwrap();
    app.dataset_properties_table.select(Some(row));
    app.handle_key(KeyCode::Enter);
}

/// Replaces the single field's text and presses Enter; returns what the form gave back.
fn submit_value(app: &mut App, text: &str) -> Option<PendingAction> {
    for _ in 0..40 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in text.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    app.handle_modal_key(KeyCode::Enter)
}

/// A limit change always asks first: takes the Confirm the form raised and answers y then Enter.
fn confirm_limit_change(app: &mut App) -> PendingAction {
    match app.modal.take() {
        Some(crate::ui::modal::Modal::Confirm { action, prompt, .. }) => {
            assert!(prompt.contains("zfs set"), "{prompt}");
            action
        }
        other => panic!("expected a Confirm, got {other:?}"),
    }
}

/// The Add vdev form of the Disks section with the given selection, pool, role and redundancy, focused on its last field.
fn add_from_disks(disks: &[&str], role: &str, redundancy: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.modal = Some(Modal::Form {
        title: "t".to_string(),
        fields: vec![
            FormField { label: "pool".into(), value: "tank".into() },
            FormField { label: "role".into(), value: role.into() },
            FormField { label: "redundancy".into(), value: redundancy.into() },
        ],
        focused: 2,
        description: None,
        kind: FormKind::AddVdevFromDisks { host: 0, disks: disks.iter().map(|d| (*d).to_string()).collect() },
    });
    app
}

/// `quota_app` with a checksum and a dedup property on the dataset.
fn integrity_app(checksum: &str, dedup: &str) -> App {
    let mut app = quota_app(false, 0, 0, 0, 0);
    app.dataset_properties.push(Property { name: "checksum".into(), value: checksum.into(), source: PropertySource::Default });
    app.dataset_properties.push(Property { name: "dedup".into(), value: dedup.into(), source: PropertySource::Default });
    app
}

/// `quota_app` with a `sync` property on the dataset.
fn sync_app(value: &str) -> App {
    let mut app = quota_app(false, 0, 0, 0, 0);
    app.dataset_properties.push(Property { name: "sync".into(), value: value.into(), source: PropertySource::Default });
    app
}

/// A Datasets screen with `tank/s` (an encryption root) and `tank/s/kid` (uses its key) selected on the first row.
fn key_app(locked: bool, key_format: &str) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    go_to(&mut app, Section::Datasets);
    let ds = |name: &str| crate::zfs::dataset::Dataset { name: name.to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 };
    app.datasets = vec![ds("tank/s"), ds("tank/s/kid"), ds("tank/plain")];
    app.datasets_table.select(Some(0));
    for name in ["tank/s", "tank/s/kid"] {
        app.encryption.insert(name.into(), crate::zfs::encryption::Enc { cipher: "aes-256-gcm".into(), key_format: key_format.into(), root: "tank/s".into(), locked });
    }
    app.encryption_for = 0;
    app
}

fn type_into(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
}

fn hold_app(holds: u64, tags: Option<&[&str]>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "s1".to_string(), used: 0, holds, written: 0 }];
    if let Some(tags) = tags {
        app.snapshot_tags.insert("tank/data@s1".to_string(), tags.iter().map(|t| (*t).to_string()).collect());
    }
    app.selected_section = app.sections.iter().position(|s| *s == Section::Snapshots).unwrap();
    app.snapshots_table.select(Some(0));
    app
}

struct PanickingList;
impl CommandRunner for PanickingList {
    fn run(&self, _: &str, args: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
        assert_ne!(args[0], "holds", "boom");
        Ok(crate::exec::CommandOutput { stdout: "tank/data@s1\t10\t1\t0\n".into(), stderr: String::new(), status: 0 })
    }
    fn run_with_stdin(&self, cmd: &str, args: &[&str], _: &str) -> crate::error::Result<crate::exec::CommandOutput> {
        self.run(cmd, args)
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> crate::error::Result<crate::exec::CommandOutput> {
        self.run(cmd, args)
    }
}

// ---- the changes view (c and C in Snapshots) ----

fn changes_app() -> App {
    let mut app = hold_app(0, None);
    let host = app.hosts[0].clone();
    app.host_probes.insert(host, HostProbeState { os: Some(crate::zfs::host_health::HostOs::FreeBsd), ..HostProbeState::default() });
    app.snapshots.push(Snapshot { dataset: "tank/data".to_string(), name: "s2".to_string(), used: 0, holds: 0, written: 0 });
    app.snapshots_table.select(Some(1));
    app
}

fn ready_outcome(entries: &[(&str, crate::zfs::snapshot_diff::Change)]) -> crate::ui::changes::ChangesOutcome {
    use crate::zfs::snapshot_changes::{ChangeSummary, Limits};
    use crate::zfs::snapshot_diff::{DiffEntry, FileKind};
    let mut summary = ChangeSummary::new(&Limits::default(), Some("/ds".to_string()));
    for (path, change) in entries {
        summary.add(DiffEntry { changed_at: 1_788_654_605, change: *change, kind: FileKind::File, path: (*path).to_string(), renamed_to: None });
    }
    crate::ui::changes::ChangesOutcome::Done { older: "tank/data@s1".to_string(), newer: "tank/data@s2".to_string(), summary }
}

fn deliver(app: &mut App, host: usize, request: u64, outcome: crate::error::Result<crate::ui::changes::ChangesOutcome>) {
    app.tx.send(WorkerMsg::Changes(host, request, outcome)).unwrap();
    app.drain_worker_messages();
}

fn ready_app() -> App {
    use crate::zfs::snapshot_diff::Change;
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('c'));
    let request = app.changes_view.as_ref().unwrap().request;
    deliver(&mut app, 0, request, Ok(ready_outcome(&[("/ds/a/f1", Change::Added), ("/ds/a/f2", Change::Added), ("/ds/b/g", Change::Removed)])));
    app
}

fn browse_outcome(names: &[(&str, crate::zfs::snapshot_browse::EntryKind)]) -> crate::ui::browse::BrowseOutcome {
    use crate::zfs::snapshot_browse::{ListEntry, Listing, Row};
    let entries = names
        .iter()
        .map(|(n, kind)| Row { entry: ListEntry { name: (*n).to_string(), kind: *kind, mode: "drwxr-xr-x".to_string(), owner: "root".to_string(), group: "wheel".to_string(), size: 5, time: Some(1_700_000_000), link_target: None, ambiguous: false }, changed: None, created: None })
        .collect();
    crate::ui::browse::BrowseOutcome { listing: Listing { entries, truncated: false, unreadable: 0, has_created: true, times_incomplete: false }, mountpoint: "/tank/data".to_string() }
}

fn deliver_browse(app: &mut App, host: usize, request: u64, outcome: crate::error::Result<crate::ui::browse::BrowseOutcome>) {
    app.tx.send(WorkerMsg::Browse(host, request, outcome)).unwrap();
    app.drain_worker_messages();
}

fn browsing_app() -> App {
    use crate::zfs::snapshot_browse::EntryKind;
    let mut app = changes_app();
    app.handle_key(KeyCode::Char('f'));
    let request = app.browse_view.as_ref().unwrap().request;
    deliver_browse(&mut app, 0, request, Ok(browse_outcome(&[("pics", EntryKind::Directory), ("a.txt", EntryKind::File)])));
    app
}

// ---- R: restore a file from the snapshot as a copy ----

fn browsing_on_file() -> App {
    let mut app = browsing_app();
    app.handle_key(KeyCode::Down);
    assert_eq!(app.browse_view.as_ref().unwrap().rows[app.browse_view.as_ref().unwrap().selected].entry.name, "a.txt");
    app
}

fn restore_form_text(app: &App) -> String {
    match &app.modal {
        Some(crate::ui::modal::Modal::Form { fields, .. }) => fields[0].value.clone(),
        other => panic!("expected the restore form, got {other:?}"),
    }
}

fn type_into_form(app: &mut App, text: &str) {
    for _ in 0..restore_form_text(app).chars().count() {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in text.chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
}

fn confirm_prompt(app: &App) -> String {
    match &app.modal {
        Some(crate::ui::modal::Modal::Confirm { prompt, .. }) => prompt.clone(),
        other => panic!("expected a Confirm, got {other:?}"),
    }
}

mod alerts_settings;
mod backups;
mod bootenvs;
mod cloud;
mod dashboard;
mod datasets;
mod disks;
mod forms;
mod hosts;
mod jobs;
mod navigation;
mod permissions;
mod pools;
mod refresh;
mod shares;
mod snapshots;
