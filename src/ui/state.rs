use crate::error::Result;
use crate::exec::CommandRunner;
use crate::zfs::dataset::Dataset;
use crate::zfs::pool::Pool;
use crate::zfs::snapshot::Snapshot;
use crossterm::event::KeyCode;
use ratatui::widgets::TableState;
use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Hosts,
    Pools,
    Datasets,
    Snapshots,
}

/// Each variant carries the index (into `App::hosts`) of the host the
/// result is *about*, so a slow result for a host the user has since
/// navigated away from can be told apart from a fresh one instead of
/// silently overwriting the currently-selected host's pane.
pub enum WorkerMsg {
    Pools(usize, Result<Vec<Pool>>),
    Datasets(usize, Result<Vec<Dataset>>),
    Snapshots(usize, Result<Vec<Snapshot>>),
}

pub struct App {
    pub sections: Vec<Section>,
    pub selected_section: usize,
    pub hosts: Vec<String>,
    pub selected_host: usize,
    pub pools: Vec<Pool>,
    pub datasets: Vec<Dataset>,
    pub snapshots: Vec<Snapshot>,
    pub pools_table: TableState,
    pub datasets_table: TableState,
    pub snapshots_table: TableState,
    pub status_message: Option<String>,
    pub should_quit: bool,
    in_flight: HashSet<(Section, usize)>,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl App {
    pub fn new(hosts: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        App {
            sections: vec![Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots],
            selected_section: 0,
            hosts,
            selected_host: 0,
            pools: Vec::new(),
            datasets: Vec::new(),
            snapshots: Vec::new(),
            pools_table: TableState::default(),
            datasets_table: TableState::default(),
            snapshots_table: TableState::default(),
            status_message: None,
            should_quit: false,
            in_flight: HashSet::new(),
            tx,
            rx,
        }
    }

    pub fn current_section(&self) -> Section {
        self.sections[self.selected_section]
    }

    /// True while any (section, host) refresh is in flight. Used only
    /// for the coarse "refreshing..." indicator in the sidebar title.
    pub fn is_loading(&self) -> bool {
        !self.in_flight.is_empty()
    }

    fn is_refreshing(&self, section: Section, host: usize) -> bool {
        self.in_flight.contains(&(section, host))
    }

    fn begin_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.insert((section, host));
    }

    fn end_refresh(&mut self, section: Section, host: usize) {
        self.in_flight.remove(&(section, host));
    }

    /// Name of the currently-selected host, for pane titles.
    pub fn active_host_name(&self) -> &str {
        self.hosts.get(self.selected_host).map(|s| s.as_str()).unwrap_or("?")
    }

    /// Clears data panes so a stale render from the previously-selected
    /// host is never left on screen looking current after switching hosts.
    fn clear_host_data(&mut self) {
        self.pools.clear();
        self.datasets.clear();
        self.snapshots.clear();
        self.pools_table.select(None);
        self.datasets_table.select(None);
        self.snapshots_table.select(None);
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.status_message = None,
            KeyCode::Right | KeyCode::Tab => {
                self.selected_section = (self.selected_section + 1) % self.sections.len();
            }
            KeyCode::Left => {
                self.selected_section = if self.selected_section == 0 {
                    self.sections.len() - 1
                } else {
                    self.selected_section - 1
                };
            }
            KeyCode::Down if self.current_section() == Section::Hosts && !self.hosts.is_empty() => {
                self.selected_host = (self.selected_host + 1) % self.hosts.len();
                self.clear_host_data();
            }
            KeyCode::Up if self.current_section() == Section::Hosts && !self.hosts.is_empty() => {
                self.selected_host = if self.selected_host == 0 {
                    self.hosts.len() - 1
                } else {
                    self.selected_host - 1
                };
                self.clear_host_data();
            }
            KeyCode::Down if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Pools => {
                move_table_selection(&mut self.pools_table, self.pools.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Datasets => {
                move_table_selection(&mut self.datasets_table, self.datasets.len(), false);
            }
            KeyCode::Down if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), true);
            }
            KeyCode::Up if self.current_section() == Section::Snapshots => {
                move_table_selection(&mut self.snapshots_table, self.snapshots.len(), false);
            }
            _ => {}
        }
    }

    /// No-ops if a Pools refresh for the current host is already in
    /// flight — otherwise holding 'r' against an unreachable host would
    /// spawn an unbounded number of blocking threads/subprocesses, one
    /// per key-repeat tick. Keyed by host too, so a hung refresh for one
    /// host never blocks refreshing a different host.
    pub fn refresh_pools(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Pools, host) {
            return;
        }
        self.begin_refresh(Section::Pools, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::pool::list_pools(runner.as_ref());
            let _ = tx.send(WorkerMsg::Pools(host, result));
        });
    }

    /// See `refresh_pools` for why this no-ops while already in flight.
    pub fn refresh_datasets(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Datasets, host) {
            return;
        }
        self.begin_refresh(Section::Datasets, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::dataset::list_datasets(runner.as_ref());
            let _ = tx.send(WorkerMsg::Datasets(host, result));
        });
    }

    /// See `refresh_pools` for why this no-ops while already in flight.
    pub fn refresh_snapshots(&mut self, runner: Arc<dyn CommandRunner>) {
        let host = self.selected_host;
        if self.is_refreshing(Section::Snapshots, host) {
            return;
        }
        self.begin_refresh(Section::Snapshots, host);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::snapshot::list_snapshots(runner.as_ref());
            let _ = tx.send(WorkerMsg::Snapshots(host, result));
        });
    }

    pub fn drain_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Pools(host, result) => {
                    self.end_refresh(Section::Pools, host);
                    if host != self.selected_host {
                        // Late result for a host we've since navigated
                        // away from — discard silently, it's not an error.
                        continue;
                    }
                    match result {
                        Ok(pools) => {
                            self.pools = pools;
                            clamp_table_selection(&mut self.pools_table, self.pools.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Datasets(host, result) => {
                    self.end_refresh(Section::Datasets, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(datasets) => {
                            self.datasets = datasets;
                            clamp_table_selection(&mut self.datasets_table, self.datasets.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
                WorkerMsg::Snapshots(host, result) => {
                    self.end_refresh(Section::Snapshots, host);
                    if host != self.selected_host {
                        continue;
                    }
                    match result {
                        Ok(snapshots) => {
                            self.snapshots = snapshots;
                            clamp_table_selection(&mut self.snapshots_table, self.snapshots.len());
                            self.status_message = None;
                        }
                        Err(e) => self.status_message = Some(e.to_string()),
                    }
                }
            }
        }
    }
}

/// Moves a `TableState`'s selection by one row within `[0, len)`,
/// wrapping at either end (consistent with the Hosts/section navigation
/// style elsewhere in `App`). No-ops (selection stays `None`) when
/// `len == 0` — there's nothing to select in an empty list.
fn move_table_selection(state: &mut TableState, len: usize, forward: bool) {
    if len == 0 {
        state.select(None);
        return;
    }
    let next = match (state.selected(), forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => {
            if i == 0 {
                len - 1
            } else {
                i - 1
            }
        }
    };
    state.select(Some(next));
}

/// Pulls a `TableState`'s selection back into range after the
/// underlying list shrinks (e.g. a refresh returns fewer rows than
/// before). Selects `None` if the list is now empty.
fn clamp_table_selection(state: &mut TableState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    if let Some(i) = state.selected() {
        if i >= len {
            state.select(Some(len - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;
    use std::time::{Duration, Instant};

    #[test]
    fn handle_key_navigates_sections_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Right);
        assert_eq!(app.current_section(), Section::Pools);
        app.handle_key(KeyCode::Left);
        assert_eq!(app.current_section(), Section::Hosts);
        app.handle_key(KeyCode::Left);
        assert_eq!(app.current_section(), Section::Snapshots);
    }

    #[test]
    fn handle_key_quit_sets_should_quit() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn handle_key_navigates_hosts_and_wraps() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 1);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.selected_host, 0);
        app.handle_key(KeyCode::Up);
        assert_eq!(app.selected_host, 1);
    }

    #[test]
    fn drain_worker_messages_updates_pools_on_success() {
        let mut app = App::new(vec!["local".to_string()]);
        let pool = Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        };
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool.clone()]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools, vec![pool]);
        assert!(!app.is_refreshing(Section::Pools, 0));
    }

    #[test]
    fn drain_worker_messages_sets_status_message_on_failure() {
        let mut app = App::new(vec!["local".to_string()]);
        app.tx
            .send(WorkerMsg::Datasets(0, Err(crate::error::AppError::Parse("boom".to_string()))))
            .unwrap();
        app.drain_worker_messages();
        assert!(app.status_message.is_some());
    }

    #[test]
    fn esc_clears_existing_status_message() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("something went wrong".to_string());
        app.handle_key(KeyCode::Esc);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn drain_worker_messages_clears_status_message_on_subsequent_success() {
        let mut app = App::new(vec!["local".to_string()]);
        app.status_message = Some("stale error".to_string());
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![]))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn refresh_pools_populates_pools_from_fake_runner() {
        let mut app = App::new(vec!["local".to_string()]);
        let runner: Arc<dyn CommandRunner> = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        app.refresh_pools(runner);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
        assert_eq!(app.pools[0].name, "tank");
    }

    #[test]
    fn drain_worker_messages_discards_result_for_stale_host() {
        // Simulates: a refresh was in flight for host 0, the user switched
        // to host 1, and only then did host 0's (slow) result arrive.
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.selected_host = 1;
        let pool = Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        };
        app.begin_refresh(Section::Pools, 0);
        app.tx.send(WorkerMsg::Pools(0, Ok(vec![pool]))).unwrap();
        app.drain_worker_messages();

        assert!(app.pools.is_empty(), "stale result for host 0 must not overwrite the pane while host 1 is selected");
        assert!(!app.is_refreshing(Section::Pools, 0), "the in-flight flag must still clear even though the result was discarded");
        assert_eq!(app.status_message, None, "a discarded stale result is not an error");
    }

    #[test]
    fn switching_selected_host_clears_pane_data() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        app.pools = vec![Pool {
            name: "tank".to_string(),
            size: 100,
            allocated: 10,
            free: 90,
            health: crate::zfs::pool::PoolHealth::Online,
        }];
        app.datasets = vec![crate::zfs::dataset::Dataset {
            name: "tank/data".to_string(),
            kind: crate::zfs::dataset::DatasetKind::Filesystem,
            used: 1,
            available: 2,
            referenced: 1,
            mountpoint: None,
        }];
        app.snapshots = vec![Snapshot { dataset: "tank/data".to_string(), name: "snap1".to_string(), used: 1 }];
        app.pools_table.select(Some(0));
        app.datasets_table.select(Some(0));
        app.snapshots_table.select(Some(0));

        app.handle_key(KeyCode::Down); // still on Hosts section by default; switches host

        assert!(app.pools.is_empty());
        assert!(app.datasets.is_empty());
        assert!(app.snapshots.is_empty());
        assert_eq!(app.pools_table.selected(), None);
        assert_eq!(app.datasets_table.selected(), None);
        assert_eq!(app.snapshots_table.selected(), None);
    }

    #[test]
    fn handle_key_navigates_pool_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools = vec![
            Pool { name: "a".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
            Pool { name: "b".to_string(), size: 0, allocated: 0, free: 0, health: crate::zfs::pool::PoolHealth::Online },
        ];
        app.handle_key(KeyCode::Right); // Hosts -> Pools
        assert_eq!(app.current_section(), Section::Pools);
        assert_eq!(app.pools_table.selected(), None);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(0));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(1));
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), Some(0), "must wrap back to the first row");
        app.handle_key(KeyCode::Up);
        assert_eq!(app.pools_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn handle_key_pool_row_navigation_is_noop_on_empty_list() {
        let mut app = App::new(vec!["local".to_string()]);
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.pools_table.selected(), None);
    }

    #[test]
    fn drain_worker_messages_clamps_selected_pool_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.pools_table.select(Some(2));
        app.begin_refresh(Section::Pools, 0);
        app.tx
            .send(WorkerMsg::Pools(0, Ok(vec![Pool {
                name: "only".to_string(),
                size: 0,
                allocated: 0,
                free: 0,
                health: crate::zfs::pool::PoolHealth::Online,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.pools_table.selected(), Some(0));
    }

    #[test]
    fn handle_key_navigates_dataset_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets = vec![
            Dataset { name: "a".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
            Dataset { name: "b".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None },
        ];
        app.handle_key(KeyCode::Right);
        app.handle_key(KeyCode::Right); // Hosts -> Pools -> Datasets
        assert_eq!(app.current_section(), Section::Datasets);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.datasets_table.selected(), Some(0));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.datasets_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn drain_worker_messages_clamps_selected_dataset_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.datasets_table.select(Some(2));
        app.begin_refresh(Section::Datasets, 0);
        app.tx
            .send(WorkerMsg::Datasets(0, Ok(vec![Dataset {
                name: "only".to_string(),
                kind: crate::zfs::dataset::DatasetKind::Filesystem,
                used: 0,
                available: 0,
                referenced: 0,
                mountpoint: None,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.datasets_table.selected(), Some(0));
    }

    #[test]
    fn handle_key_navigates_snapshot_rows_and_wraps() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots = vec![
            Snapshot { dataset: "tank/data".to_string(), name: "a".to_string(), used: 0 },
            Snapshot { dataset: "tank/data".to_string(), name: "b".to_string(), used: 0 },
        ];
        app.handle_key(KeyCode::Left); // Hosts -> Snapshots (wraps backward)
        assert_eq!(app.current_section(), Section::Snapshots);
        app.handle_key(KeyCode::Down);
        assert_eq!(app.snapshots_table.selected(), Some(0));
        app.handle_key(KeyCode::Up);
        assert_eq!(app.snapshots_table.selected(), Some(1), "must wrap back to the last row");
    }

    #[test]
    fn drain_worker_messages_clamps_selected_snapshot_row_when_list_shrinks() {
        let mut app = App::new(vec!["local".to_string()]);
        app.snapshots_table.select(Some(2));
        app.begin_refresh(Section::Snapshots, 0);
        app.tx
            .send(WorkerMsg::Snapshots(0, Ok(vec![Snapshot {
                dataset: "tank/data".to_string(),
                name: "only".to_string(),
                used: 0,
            }])))
            .unwrap();
        app.drain_worker_messages();
        assert_eq!(app.snapshots_table.selected(), Some(0));
    }

    #[test]
    fn refresh_pools_does_not_spawn_second_thread_while_in_flight() {
        let mut app = App::new(vec!["local".to_string()]);
        let fake = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        let runner: Arc<dyn CommandRunner> = fake.clone();

        app.refresh_pools(runner.clone());
        assert!(app.is_refreshing(Section::Pools, 0));
        // A second refresh while the first is still in flight must be a
        // no-op — otherwise holding 'r' spawns unbounded threads/processes.
        app.refresh_pools(runner.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            fake.calls.lock().unwrap().len(),
            1,
            "second refresh_pools call while one is in flight must not spawn another thread"
        );
    }

    #[test]
    fn refresh_pools_for_one_host_does_not_block_refresh_for_another_host() {
        let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
        let fake_a = Arc::new(FakeRunner::new("tank\t100\t10\t90\tONLINE\n"));
        let runner_a: Arc<dyn CommandRunner> = fake_a.clone();

        // Host 0's refresh is "in flight" (never drained in this test, so it
        // stays in flight for the duration).
        app.refresh_pools(runner_a.clone());
        assert!(app.is_refreshing(Section::Pools, 0));

        // Switching to host 1 and refreshing must not be blocked by host 0's
        // still-in-flight refresh.
        app.selected_host = 1;
        let fake_b = Arc::new(FakeRunner::new("scratch\t50\t5\t45\tONLINE\n"));
        let runner_b: Arc<dyn CommandRunner> = fake_b.clone();
        app.refresh_pools(runner_b.clone());

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.drain_worker_messages();
            if !app.pools.is_empty() || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.pools.len(), 1);
        assert_eq!(app.pools[0].name, "scratch", "host 1's refresh must have run, not been blocked by host 0's in-flight one");
    }
}
