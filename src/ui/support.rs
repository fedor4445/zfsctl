//! What each host OS supports: the sections that work with FreeBSD's commands and have no equivalent built for Linux or
//! illumos yet. The sidebar dims those entries for the selected host, the section says why, and nothing is run for them.
//! A host whose OS is unknown, or not read yet, is treated as FreeBSD: a setup that worked before keeps working.

use crate::ui::state::Section;
use crate::zfs::host_health::HostOs;

/// Whether `section` works on a host running `os` (`None`: not read yet).
pub fn supports(os: Option<HostOs>, section: Section) -> bool {
    match (os, section) {
        (Some(HostOs::Linux), Section::BootEnvs | Section::Shares | Section::DatasetAcl) | (Some(HostOs::Illumos), Section::Shares | Section::DatasetAcl) => false,
        _ => true,
    }
}

/// Boot environments can be listed on illumos (`beadm list`) but not yet changed: the section is read-only there.
pub fn bootenvs_read_only(os: Option<HostOs>) -> bool {
    os == Some(HostOs::Illumos)
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
    fn linux_supports_disks_but_not_boot_envs_shares_or_acl_and_illumos_adds_boot_envs() {
        assert!(supports(Some(HostOs::Linux), Section::Disks) && supports(Some(HostOs::Illumos), Section::Disks));
        for gated in [Section::BootEnvs, Section::Shares, Section::DatasetAcl] {
            assert!(!supports(Some(HostOs::Linux), gated), "Linux {gated:?}");
        }
        assert!(supports(Some(HostOs::Illumos), Section::BootEnvs), "beadm list works on illumos");
        for gated in [Section::Shares, Section::DatasetAcl] {
            assert!(!supports(Some(HostOs::Illumos), gated), "illumos {gated:?}");
        }
    }

    #[test]
    fn only_those_sections_are_ever_gated() {
        let gated = |os| ALL.iter().copied().filter(|s| !supports(Some(os), *s)).collect::<Vec<Section>>();
        assert_eq!(gated(HostOs::Linux), [Section::Shares, Section::BootEnvs, Section::DatasetAcl]);
        assert_eq!(gated(HostOs::Illumos), [Section::Shares, Section::DatasetAcl]);
    }

    #[test]
    fn boot_environments_are_read_only_on_illumos_only() {
        assert!(bootenvs_read_only(Some(HostOs::Illumos)));
        for os in [None, Some(HostOs::FreeBsd), Some(HostOs::Linux), Some(HostOs::Other)] {
            assert!(!bootenvs_read_only(os), "{os:?}");
        }
    }

    #[test]
    fn the_text_names_the_section_and_the_os() {
        assert_eq!(unsupported_text(HostOs::Linux, Section::BootEnvs), "Boot Envs is not supported on Linux hosts yet.");
        assert_eq!(unsupported_text(HostOs::Illumos, Section::DatasetAcl), "Dataset ACL is not supported on illumos hosts yet.");
    }
}
