//! The Shares screen.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn the_shares_section_shows_access_clients_and_space_for_nfs_and_smb() {
    use crate::zfs::metrics::{DatasetSpace, MetricsTarget, PoolSample, TargetSample};
    use crate::zfs::share::Share;
    use crate::zfs::share_detail::ShareLive;
    let mut app = App::new(vec!["local".to_string()]);
    app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
    app.shares = vec![
        Share::Nfs { dataset: "tank/a".to_string(), raw: "x".to_string(), networks: vec!["10.0.0.0/24".to_string()], hosts: vec![], read_only: true, root: Some("root".to_string()) },
        Share::Smb { dataset: "tank/b".to_string(), name: "bee".to_string(), path: "/b".to_string(), valid_user: Some("carol".to_string()), read_only: false, guest_ok: false },
    ];
    let space = DatasetSpace { used: 1 << 30, available: 3 << 30, quota: 10 << 30, compression: "lz4".to_string(), compress_ratio: 1.5, ..DatasetSpace::default() };
    let sample = |clients| {
        Ok(TargetSample {
            pool: PoolSample::default(),
            space: Some(space.clone()),
            host: None,
            snapshot: None,
            disk: None,
            share: Some(ShareLive { mountpoint: Some("/tank/a".to_string()), clients }),
            contents: None,
        })
    };
    // NFS row
    app.shares_table.select(Some(0));
    let nfs = MetricsTarget::Share { dataset: "tank/a".to_string(), smb_name: None };
    assert_eq!(app.metrics_target(), Some(nfs.clone()));
    app.metrics.retarget(Some(nfs.clone()));
    app.metrics.apply(&nfs, sample(Some(vec!["10.0.0.7".to_string(), "10.0.0.8".to_string()])), std::time::Instant::now());
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["Metrics — NFS share tank/a", "10.0.0.0/24  ro  root=root", "/tank/a", "2: 10.0.0.7, 10.0.0.8", "25%", "Quota    10.0G", "lz4 1.50x"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    // SMB row, host won't say who is connected
    app.shares_table.select(Some(1));
    let smb = MetricsTarget::Share { dataset: "tank/b".to_string(), smb_name: Some("bee".to_string()) };
    assert_eq!(app.metrics_target(), Some(smb.clone()));
    app.metrics.retarget(Some(smb.clone()));
    app.metrics.apply(&smb, sample(None), std::time::Instant::now());
    let text = render(&mut app, 120, 30).join("\n");
    for want in ["SMB share \\\\bee", "user=carol  rw", "unavailable"] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

#[test]
fn the_shares_title_names_the_services_of_a_freebsd_host() {
    let mut app = linux_host_app();
    app.selected_host = 0;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
    (app.nfs_available, app.samba_available) = (Some(false), Some(false));
    let screen = render(&mut app, 160, 12).join("\n");
    assert!(screen.contains("NFS unavailable: nfsd not running") && screen.contains("SMB unavailable: smbd not running"), "{screen}");
}

#[test]
fn the_shares_title_carries_the_nfsv4_warning() {
    let mut app = linux_host_app();
    app.selected_host = 0;
    app.selected_section = app.sections.iter().position(|s| *s == Section::Shares).unwrap();
    app.nfsv4_note = Some("NFSv4 is on, but no V4: line in /etc/exports marks its root".to_string());
    let screen = render(&mut app, 200, 12).join("\n");
    assert!(screen.contains("NFSv4 is on, but no V4: line"), "{screen}");
}
