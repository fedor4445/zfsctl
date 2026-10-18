//! What each host OS supports: the sections that work with FreeBSD's commands and have no equivalent built for Linux or
//! illumos yet. The sidebar dims those entries for the selected host, the section says why, and nothing is run for them.
//! A host whose OS is unknown, or not read yet, is treated as FreeBSD: a setup that worked before keeps working.

use crate::ui::state::Section;
use crate::zfs::host_health::HostOs;

/// Whether `section` works on a host running `os` (`None`: not read yet).
pub fn supports(os: Option<HostOs>, section: Section) -> bool {
    match (os, section) {
        (Some(HostOs::Linux), Section::BootEnvs | Section::DatasetAcl) | (Some(HostOs::Illumos), Section::DatasetAcl) => false,
        _ => true,
    }
}

/// Shares are listed everywhere but created, edited and removed only with FreeBSD's tools (the `exports(5)` syntax of
/// `sharenfs`, the managed Samba block, `service`): on Linux and illumos the section is read-only.
pub fn shares_editable(os: Option<HostOs>) -> bool {
    !matches!(os, Some(HostOs::Linux | HostOs::Illumos))
}

/// Whether a key of the Boot Envs section is offered on a host running `os`: on illumos `beadm` has no equivalent of
/// mounting (`m`, `u`) or of destroying together with the origin snapshot (`D`).
pub fn bootenv_key_offered(os: Option<HostOs>, key: char) -> bool {
    !(os == Some(HostOs::Illumos) && matches!(key, 'm' | 'u' | 'D'))
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
    fn linux_supports_disks_and_shares_but_not_boot_envs_or_acl_and_illumos_adds_boot_envs() {
        for os in [HostOs::Linux, HostOs::Illumos] {
            assert!(supports(Some(os), Section::Disks) && supports(Some(os), Section::Shares), "{os:?}");
            assert!(!supports(Some(os), Section::DatasetAcl), "{os:?}");
        }
        assert!(!supports(Some(HostOs::Linux), Section::BootEnvs));
        assert!(supports(Some(HostOs::Illumos), Section::BootEnvs), "beadm list works on illumos");
    }

    #[test]
    fn only_those_sections_are_ever_gated() {
        let gated = |os| ALL.iter().copied().filter(|s| !supports(Some(os), *s)).collect::<Vec<Section>>();
        assert_eq!(gated(HostOs::Linux), [Section::BootEnvs, Section::DatasetAcl]);
        assert_eq!(gated(HostOs::Illumos), [Section::DatasetAcl]);
    }

    #[test]
    fn shares_can_be_changed_only_where_the_freebsd_tools_exist() {
        assert!(shares_editable(None) && shares_editable(Some(HostOs::FreeBsd)) && shares_editable(Some(HostOs::Other)));
        assert!(!shares_editable(Some(HostOs::Linux)) && !shares_editable(Some(HostOs::Illumos)));
    }

    #[test]
    fn on_illumos_only_the_keys_beadm_cannot_serve_are_withheld() {
        for key in ['n', 'a', 't', 'T', 'R', 'd'] {
            assert!(bootenv_key_offered(Some(HostOs::Illumos), key), "{key}");
        }
        for key in ['m', 'u', 'D'] {
            assert!(!bootenv_key_offered(Some(HostOs::Illumos), key), "{key}");
            assert!(bootenv_key_offered(Some(HostOs::FreeBsd), key) && bootenv_key_offered(None, key) && bootenv_key_offered(Some(HostOs::Other), key), "{key}");
        }
    }

    #[test]
    fn the_text_names_the_section_and_the_os() {
        assert_eq!(unsupported_text(HostOs::Linux, Section::BootEnvs), "Boot Envs is not supported on Linux hosts yet.");
        assert_eq!(unsupported_text(HostOs::Illumos, Section::DatasetAcl), "Dataset ACL is not supported on illumos hosts yet.");
    }
}
