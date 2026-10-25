//! The drawing code's tests, by screen. The builders several screens use live here.

use super::*;
use crate::ui::theme::Theme;
// Explicit (not via `super::*`): Task 7 removes `Color` from the
// module's own imports once the status bar stops naming colors.
use ratatui::style::Color;

use crate::zfs::pool::{Pool, PoolHealth, VdevLine};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Renders one frame and returns it as one string per terminal row.
fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
}

fn identity_disk(name: &str, model: &str, serial: &str) -> crate::zfs::disk::DiskInfo {
    crate::zfs::disk::DiskInfo {
        name: name.to_string(),
        size: 4_000_787_030_016,
        model: Some(model.to_string()),
        serial: Some(serial.to_string()),
        media: crate::zfs::disk::DiskMedia::Hdd(5400),
        ..Default::default()
    }
}

fn app_with_form_and_disks(n: usize, w: u16, h: u16) -> Vec<String> {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Disks).unwrap();
    app.disks = (0..n)
        .map(|i| identity_disk(&format!("da{i}"), &format!("Some Very Long Manufacturer Model Name {i}"), &format!("SERIAL-NUMBER-{i:04}-ABCDEFGH")))
        .collect();
    for d in &app.disks {
        app.selected_disks.insert(d.name.clone());
    }
    app.disks_table.select(Some(0));
    app.handle_key(crossterm::event::KeyCode::Char('n'));
    render(&mut app, w, h)
}

fn pool(name: &str, allocated: u64, size: u64, health: PoolHealth) -> Pool {
    Pool { name: name.to_string(), size, allocated, free: size.saturating_sub(allocated), health }
}

fn app_in_datasets(names: &[&str]) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Datasets).unwrap();
    app.datasets = names
        .iter()
        .map(|n| crate::zfs::dataset::Dataset { name: (*n).to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 })
        .collect();
    app.datasets_table.select(Some(0));
    app
}

fn app_in_pools(pools: Vec<Pool>) -> App {
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
    app.pools = pools;
    app.pools_table.select(Some(0));
    app
}

fn pools_app_with_info(contents: Option<Result<crate::zfs::pool_info::PoolContents, String>>) -> App {
    use crate::zfs::host_health::ScanSummary;
    use crate::zfs::metrics::{MetricsTarget, PoolFacts, PoolIo, PoolSample, TargetSample};
    use crate::zfs::pool_info::PoolLayout;
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    let target = MetricsTarget::Pool("tank".to_string());
    app.metrics.retarget(Some(target.clone()));
    let layout = PoolLayout { vdevs: 4, kinds: vec![("mirror".into(), 2), ("raidz1".into(), 1), ("disk".into(), 1)], disks: 10, classes: vec![("data", 6), ("log", 2), ("cache", 1), ("spare", 1)] };
    let facts = PoolFacts { size: 1000, allocated: 100, layout: Some(layout), scrub: Some(ScanSummary::Finished { when: "Thu Oct  8 19:48:07 2026".into(), errors: 0 }), ..PoolFacts::default() };
    app.metrics.apply(&target, Ok(TargetSample { pool: PoolSample { io: PoolIo::default(), facts }, space: None, host: None, snapshot: None, disk: None, share: None, contents }), std::time::Instant::now());
    app
}

fn linux_host_app() -> App {
    use crate::zfs::host_health::HostOs;
    let mut app = App::new(vec!["local".to_string(), "ub24".to_string()]);
    app.host_probes.insert("ub24".to_string(), crate::ui::state::HostProbeState { os: Some(HostOs::Other), ..Default::default() });
    app.selected_host = 1;
    app
}

use crate::ui::modal::{Modal, MutationCall, PendingAction};

fn buffer_of(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn confirm_with(choice: Option<bool>, prompt: &str) -> App {
    let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
    app.modal = Some(Modal::Confirm { choice, prompt: prompt.to_string(), action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } } });
    app
}

fn hosts_app() -> App {
    let mut app = App::new(vec!["local".to_string(), "fbsd16".to_string(), "fbsd16t1".to_string(), "deadbox".to_string()]);
    app.set_host_addresses(vec!["local".into(), "172.16.10.60".into(), "172.16.10.61".into(), "172.16.10.99".into()]);
    let probe = |p: crate::zfs::host_health::HostProbe| crate::ui::state::HostProbeState { last: Some(p), in_flight: false, finished_at: Some(std::time::Instant::now()), started_at: None, os: None, os_name: None, kind: None, resources: vec![], utc_offset_secs: None };
    let pool = |n: &str, h: crate::zfs::pool::PoolHealth, c: u8| crate::zfs::host_health::PoolProbe { name: n.to_string(), health: h, capacity_percent: c, frag_percent: None, errors: Some(0), scan: None, last_scrub: None };
    use crate::zfs::host_health::HostProbe;
    app.host_probes.insert("local".into(), probe(HostProbe::Reached { rtt_ms: 3, pools: vec![pool("zroot", crate::zfs::pool::PoolHealth::Online, 20)], kind: None, os: None, os_name: None }));
    app.host_probes.insert("fbsd16".into(), probe(HostProbe::Reached { rtt_ms: 12, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Degraded, 20)], kind: None, os: None, os_name: None }));
    app.host_probes.insert("fbsd16t1".into(), probe(HostProbe::Reached { rtt_ms: 7, pools: vec![pool("tank", crate::zfs::pool::PoolHealth::Online, 85)], kind: None, os: None, os_name: None }));
    app.host_probes.insert("deadbox".into(), probe(HostProbe::Disconnected));
    use crate::zfs::host_health::HostKind;
    app.host_probes.get_mut("local").unwrap().kind = Some(HostKind::Metal);
    app.host_probes.get_mut("fbsd16").unwrap().kind = Some(HostKind::Vm("bhyve".into()));
    app.host_probes.get_mut("fbsd16t1").unwrap().kind = Some(HostKind::Jail);
    app.set_host_tags(vec![String::new(), "backup".into(), String::new(), String::new()]);
    app
}

/// The foreground color of the first cell of `needle` on the first row that contains it.
fn fg_of(app: &mut App, needle: &str, w: u16, h: u16) -> Option<ratatui::style::Color> {
    let rows = render(app, w, h);
    let buf = buffer_of(app, w, h);
    for (y, row) in rows.iter().enumerate() {
        if let Some(x) = row.find(needle) {
            let col = row[..x].chars().count();
            return buf[(u16::try_from(col).ok()?, u16::try_from(y).ok()?)].fg.into();
        }
    }
    None
}

fn alerts_render_app(config: &str) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let mut app = App::new(vec!["local".to_string()]);
    app.config_path = dir.path().join("config.toml");
    app.state_dir = dir.path().to_path_buf();
    app.selected_section = app.sections.iter().position(|s| *s == Section::Alerts).unwrap();
    app.refresh_alerts();
    (app, dir)
}

fn settings_render_app(config: &str) -> (App, tempfile::TempDir) {
    let (mut app, dir) = alerts_render_app(config);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Settings).unwrap();
    app.refresh_settings();
    (app, dir)
}

/// The foreground colour and boldness of the first cell of `word` on the first row that contains it.
fn style_of(buf: &ratatui::buffer::Buffer, w: u16, h: u16, word: &str) -> (ratatui::style::Color, bool) {
    for y in 0..h {
        let row: Vec<String> = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        let text = row.concat();
        if let Some(at) = text.find(word) {
            let x = u16::try_from(text[..at].chars().count()).unwrap();
            let cell = &buf[(x, y)];
            return (cell.fg, cell.modifier.contains(ratatui::style::Modifier::BOLD));
        }
    }
    panic!("{word:?} not on screen");
}

fn render_buffer(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn host_panel_app(w_extra: ()) -> App {
    let _ = w_extra;
    let mut app = App::new(vec!["local".to_string()]);
    let target = MetricsTarget::Host;
    app.metrics.retarget(Some(target.clone()));
    app.metrics.apply(&target, Ok(crate::zfs::metrics::TargetSample { pool: crate::zfs::metrics::PoolSample::default(), space: None, host: Some(crate::ui::metrics::tests_support::rich_host()), snapshot: None, disk: None, share: None, contents: None }), std::time::Instant::now());
    app
}

fn check(host: usize, repo: &str, pw: &str, result: Option<crate::restic::Preflight>) -> crate::ui::state::PreflightCheck {
    crate::ui::state::PreflightCheck { host, repository: repo.into(), password_file: pw.into(), result, file_note: None }
}

mod bootenvs;
mod cloud;
mod datasets;
mod disks;
mod frame;
mod headers;
mod hosts;
mod jobs;
mod modals;
mod pools;
mod settings;
mod shares;
