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
    /// SSH destination this host is reached at; `None` for the local host.
    pub ssh_target: Option<String>,
    /// The free label from `config.toml` (or the Add Host form).
    pub tag: Option<String>,
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
            ssh_target: None,
            tag: None,
        }];
        for host_cfg in &config.hosts {
            let runner: Arc<dyn CommandRunner> = match &host_cfg.ssh_target {
                Some(target) => Arc::new(SshRunner::new(target.clone())),
                None => Arc::new(LocalRunner),
            };
            hosts.push(Host {
                name: host_cfg.name.clone(),
                runner,
                ssh_target: host_cfg.ssh_target.clone(),
                tag: host_cfg.tag.clone(),
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
    pub fn apply_new_host(&mut self, config_path: &Path, name: String, target: String, tag: Option<String>) -> Result<()> {
        self.hosts.push(Host {
            name: name.clone(),
            runner: Arc::new(SshRunner::new(target.clone())),
            ssh_target: Some(target.clone()),
            tag: tag.clone(),
        });
        let mut config = Config::load(config_path)?;
        config.hosts.push(HostConfig { name, ssh_target: Some(target), tag });
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
                HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()), tag: None },
                HostConfig { name: "offsite".to_string(), ssh_target: Some("offsite.example.com".to_string()), tag: None },
            ],
            ..Default::default()
        };
        let inventory = HostInventory::from_config(&config);
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["local", "backup", "offsite"]);
    }

    #[test]
    fn from_config_carries_the_tag_to_the_host() {
        let config = Config { hosts: vec![HostConfig { name: "a".into(), ssh_target: Some("u@a".into()), tag: Some("backup".into()) }], ..Config::default() };
        let inv = HostInventory::from_config(&config);
        assert_eq!((inv.hosts[0].tag.clone(), inv.hosts[1].tag.clone()), (None, Some("backup".to_string())));
    }

    #[test]
    fn apply_new_host_pushes_into_live_inventory_and_persists_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut inventory = HostInventory::from_config(&Config::default());
        assert_eq!(inventory.hosts.len(), 1);

        inventory.apply_new_host(&config_path, "newhost".to_string(), "newhost.example.com".to_string(), Some("lab".to_string())).unwrap();

        assert_eq!(inventory.hosts.len(), 2);
        assert_eq!(inventory.hosts[1].name, "newhost");

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(
            persisted.hosts,
            vec![HostConfig { name: "newhost".to_string(), ssh_target: Some("newhost.example.com".to_string()), tag: Some("lab".to_string()) }]
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
        let result = inventory.apply_new_host(&config_path, "newhost".to_string(), "newhost.example.com".to_string(), Some("lab".to_string()));

        assert!(result.is_err());
        assert_eq!(inventory.hosts.len(), 2, "the inventory push must never be rolled back, even when persisting fails");
        assert_eq!(inventory.hosts[1].name, "newhost");
    }

    #[test]
    fn hosts_remember_their_ssh_target_and_the_local_host_has_none() {
        let config = Config {
            hosts: vec![
                HostConfig { name: "backup".to_string(), ssh_target: Some("fbsd@backup.example.com".to_string()), tag: None },
                HostConfig { name: "alias".to_string(), ssh_target: None, tag: None },
            ],
            ..Default::default()
        };
        let inventory = HostInventory::from_config(&config);
        assert_eq!(inventory.hosts[0].ssh_target, None, "the synthetic local host");
        assert_eq!(inventory.hosts[1].ssh_target.as_deref(), Some("fbsd@backup.example.com"));
        assert_eq!(inventory.hosts[2].ssh_target, None, "a configured host without a target is local");
    }

    #[test]
    fn a_host_added_at_runtime_remembers_its_ssh_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut inventory = HostInventory::from_config(&Config::default());
        inventory.apply_new_host(&dir.path().join("config.toml"), "newhost".to_string(), "fbsd@newhost".to_string(), None).unwrap();
        assert_eq!(inventory.hosts[1].ssh_target.as_deref(), Some("fbsd@newhost"));
    }

    #[test]
    fn get_finds_host_by_name_or_returns_none() {
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()), tag: None }],
            ..Default::default()
        };
        let inventory = HostInventory::from_config(&config);
        assert!(inventory.get("backup").is_some());
        assert!(inventory.get("nonexistent").is_none());
    }
}
