use crate::config::{Config, HostConfig};
use crate::error::Result;
use crate::exec::local::LocalRunner;
use crate::exec::ssh::SshRunner;
use crate::exec::CommandRunner;
use std::path::Path;
use std::sync::Arc;

pub struct Host {
    pub name: String,
    pub runner: Arc<dyn CommandRunner>,
}

pub struct HostInventory {
    pub hosts: Vec<Host>,
}

impl HostInventory {
    /// Builds the inventory from config: a synthetic "local" host always
    /// comes first, followed by one entry per configured remote host.
    pub fn from_config(config: &Config) -> Self {
        let mut hosts = vec![Host {
            name: "local".to_string(),
            runner: Arc::new(LocalRunner),
        }];
        for host_cfg in &config.hosts {
            let runner: Arc<dyn CommandRunner> = match &host_cfg.ssh_target {
                Some(target) => Arc::new(SshRunner::new(target.clone())),
                None => Arc::new(LocalRunner),
            };
            hosts.push(Host {
                name: host_cfg.name.clone(),
                runner,
            });
        }
        HostInventory { hosts }
    }

    pub fn get(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Pushes `name`/`target` onto the live inventory (always, even if
    /// persisting below fails -- hot-reload's whole point is that the
    /// host is usable for the rest of this session regardless) and
    /// then persists it to `config_path` by loading the current
    /// config, appending, and saving. The inventory push happening
    /// first and unconditionally is what guarantees it's never rolled
    /// back by a later persistence failure.
    pub fn apply_new_host(&mut self, config_path: &Path, name: String, target: String) -> Result<()> {
        self.hosts.push(Host {
            name: name.clone(),
            runner: Arc::new(SshRunner::new(target.clone())),
        });
        let mut config = Config::load(config_path)?;
        config.hosts.push(HostConfig { name, ssh_target: Some(target) });
        config.save(config_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HostConfig;

    #[test]
    fn always_includes_a_local_host_first() {
        let config = Config::default();
        let inventory = HostInventory::from_config(&config);
        assert_eq!(inventory.hosts.len(), 1);
        assert_eq!(inventory.hosts[0].name, "local");
    }

    #[test]
    fn includes_configured_remote_hosts_in_order() {
        let config = Config {
            hosts: vec![
                HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) },
                HostConfig { name: "offsite".to_string(), ssh_target: Some("offsite.example.com".to_string()) },
            ],
            ..Default::default()
        };
        let inventory = HostInventory::from_config(&config);
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["local", "backup", "offsite"]);
    }

    #[test]
    fn apply_new_host_pushes_into_live_inventory_and_persists_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut inventory = HostInventory::from_config(&Config::default());
        assert_eq!(inventory.hosts.len(), 1);

        inventory.apply_new_host(&config_path, "newhost".to_string(), "newhost.example.com".to_string()).unwrap();

        assert_eq!(inventory.hosts.len(), 2);
        assert_eq!(inventory.hosts[1].name, "newhost");

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(
            persisted.hosts,
            vec![HostConfig { name: "newhost".to_string(), ssh_target: Some("newhost.example.com".to_string()) }]
        );
    }

    #[test]
    fn apply_new_host_keeps_the_inventory_push_even_if_persisting_fails() {
        // Force Config::save to fail deterministically (not relying on
        // permission bits, which behave inconsistently when tests run
        // as root): point the config path at a location where a
        // *file* already occupies what would need to be a directory
        // component, so `create_dir_all` fails with "not a directory".
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not-a-directory");
        std::fs::write(&blocking_file, b"").unwrap();
        let config_path = blocking_file.join("config.toml");

        let mut inventory = HostInventory::from_config(&Config::default());
        let result = inventory.apply_new_host(&config_path, "newhost".to_string(), "newhost.example.com".to_string());

        assert!(result.is_err());
        assert_eq!(inventory.hosts.len(), 2, "the inventory push must never be rolled back, even when persisting fails");
        assert_eq!(inventory.hosts[1].name, "newhost");
    }

    #[test]
    fn get_finds_host_by_name_or_returns_none() {
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
            ..Default::default()
        };
        let inventory = HostInventory::from_config(&config);
        assert!(inventory.get("backup").is_some());
        assert!(inventory.get("nonexistent").is_none());
    }
}
