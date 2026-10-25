//! What a host supports: zfsctl manages FreeBSD hosts. A host that answers as something else is read with `zpool` and `zfs`
//! only: the sections that run FreeBSD's own tools are dimmed for it, the section says why, nothing is run for them, and
//! nothing is ever changed on it. A host whose OS is not read yet is treated as FreeBSD for reading.

use crate::ui::state::Section;
use crate::zfs::host_health::HostOs;

/// Whether `section` works on a host running `os` (`None`: not read yet). A host that is not FreeBSD is read with `zpool` and
/// `zfs` only: the sections that run FreeBSD's own tools (`bectl`, `geom`, `service`, the ACL tools) are not offered for it.
pub fn supports(os: Option<HostOs>, section: Section) -> bool {
    !(os == Some(HostOs::Other) && matches!(section, Section::BootEnvs | Section::DatasetAcl | Section::Disks | Section::Shares))
}

/// Shares are created, edited and removed with FreeBSD's tools (the `exports(5)` syntax of `sharenfs`, the managed Samba block,
/// `service`), so only on a host whose OS has been read and is FreeBSD.
pub fn shares_editable(os: Option<HostOs>) -> bool {
    os == Some(HostOs::FreeBsd)
}

/// The name of a section as the sidebar and the messages call it.
pub fn section_label(section: Section) -> &'static str {
    match section {
        Section::Dashboard => "Dashboard",
        Section::Hosts => "Hosts",
        Section::Pools => "Pools",
        Section::Datasets => "Datasets",
        Section::Snapshots => "Snapshots",
        Section::Disks => "Disks",
        Section::Shares => "Shares",
        Section::BootEnvs => "Boot Envs",
        Section::Jobs => "Jobs",
        Section::Cloud => "Cloud",
        Section::Alerts => "Alerts",
        Section::Settings => "Settings",
        Section::Vdevs => "Vdevs",
        Section::PoolProperties => "Pool Properties",
        Section::DatasetProperties => "Dataset Properties",
        Section::DatasetPermissions => "Dataset Permissions",
        Section::DatasetAcl => "Dataset ACL",
    }
}

/// `Boot Envs needs a FreeBSD host.`
pub fn unsupported_text(section: Section) -> String {
    format!("{} needs a FreeBSD host.", section_label(section))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Section; 17] = [
        Section::Dashboard, Section::Hosts, Section::Pools, Section::Datasets, Section::Snapshots, Section::Disks, Section::Shares, Section::BootEnvs, Section::Jobs, Section::Cloud, Section::Alerts,
        Section::Settings, Section::Vdevs, Section::PoolProperties, Section::DatasetProperties, Section::DatasetPermissions, Section::DatasetAcl,
    ];

    #[test]
    fn unknown_and_freebsd_support_everything() {
        for os in [None, Some(HostOs::FreeBsd)] {
            assert!(ALL.iter().all(|s| supports(os, *s)), "{os:?}");
        }
    }

    #[test]
    fn a_host_that_is_not_freebsd_is_read_with_zpool_and_zfs_only() {
        let gated = ALL.iter().copied().filter(|s| !supports(Some(HostOs::Other), *s)).collect::<Vec<Section>>();
        assert_eq!(gated, [Section::Disks, Section::Shares, Section::BootEnvs, Section::DatasetAcl]);
    }

    #[test]
    fn shares_can_be_changed_only_where_the_freebsd_tools_exist() {
        assert!(shares_editable(Some(HostOs::FreeBsd)));
        // Data safety: until the OS is read (the first probe, or one that failed) nothing FreeBSD-specific is sent.
        for os in [None, Some(HostOs::Other)] {
            assert!(!shares_editable(os), "{os:?}");
        }
    }

    #[test]
    fn the_text_names_the_section() {
        assert_eq!(unsupported_text(Section::BootEnvs), "Boot Envs needs a FreeBSD host.");
        assert_eq!(unsupported_text(Section::DatasetAcl), "Dataset ACL needs a FreeBSD host.");
    }
}
