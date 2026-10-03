use crate::config::Config;
use crate::exec::local::LocalRunner;
use crate::exec::ssh::SshRunner;
use crate::exec::CommandRunner;
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
        };
        let inventory = HostInventory::from_config(&config);
        let names: Vec<&str> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["local", "backup", "offsite"]);
    }

    #[test]
    fn get_finds_host_by_name_or_returns_none() {
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
        };
        let inventory = HostInventory::from_config(&config);
        assert!(inventory.get("backup").is_some());
        assert!(inventory.get("nonexistent").is_none());
    }
}
