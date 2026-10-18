//! What each host OS supports: the sections that work with FreeBSD's commands and have no equivalent built for Linux or
//! illumos yet. The sidebar dims those entries for the selected host, the section says why, and nothing is run for them.
//! A host whose OS is unknown, or not read yet, is treated as FreeBSD: a setup that worked before keeps working.

use crate::ui::state::Section;
use crate::zfs::host_health::HostOs;

/// Whether `section` works on a host running `os` (`None`: not read yet).
pub fn supports(os: Option<HostOs>, section: Section) -> bool {
    !matches!((os, section), (Some(HostOs::Linux | HostOs::Illumos), Section::BootEnvs | Section::Shares | Section::DatasetAcl))
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

/// `Boot Envs is not supported on Linux hosts yet.`
pub fn unsupported_text(os: HostOs, section: Section) -> String {
    format!("{} is not supported on {} hosts yet.", section_label(section), os.word())
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
        for os in [None, Some(HostOs::FreeBsd), Some(HostOs::Other)] {
            assert!(ALL.iter().all(|s| supports(os, *s)), "{os:?}");
        }
    }

    #[test]
    fn linux_and_illumos_support_disks_but_not_boot_envs_shares_or_acl() {
        for os in [HostOs::Linux, HostOs::Illumos] {
            assert!(supports(Some(os), Section::Disks), "{os:?}");
            for gated in [Section::BootEnvs, Section::Shares, Section::DatasetAcl] {
                assert!(!supports(Some(os), gated), "{os:?} {gated:?}");
            }
        }
    }

    #[test]
    fn only_those_sections_are_ever_gated() {
        for os in [HostOs::Linux, HostOs::Illumos] {
            let gated: Vec<Section> = ALL.iter().copied().filter(|s| !supports(Some(os), *s)).collect();
            assert_eq!(gated, [Section::Shares, Section::BootEnvs, Section::DatasetAcl], "{os:?}");
        }
    }

    #[test]
    fn the_text_names_the_section_and_the_os() {
        assert_eq!(unsupported_text(HostOs::Linux, Section::BootEnvs), "Boot Envs is not supported on Linux hosts yet.");
        assert_eq!(unsupported_text(HostOs::Illumos, Section::DatasetAcl), "Dataset ACL is not supported on illumos hosts yet.");
    }
}
