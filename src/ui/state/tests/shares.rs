//! Shares: NFS and SMB lists and forms.
#![allow(clippy::wildcard_imports)]
use super::*;

#[test]
fn refresh_shares_populates_app_state_from_fake_runner() {
    let mut app = App::new(vec!["local".to_string()]);
    let runner: Arc<dyn CommandRunner> = Arc::new(
        FakeRunner::new("")
            .route("zfs", "get", "tank/data\tsharenfs\ton\tlocal\n")
            .route("service", "nfsd", "nfsd is running as pid 1234.\n")
            .route("service", "samba_server", "smbd is not running.\n"),
    );
    app.refresh_shares(runner);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.drain_worker_messages();
        if !app.shares.is_empty() || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.shares.len(), 1);
    assert_eq!(app.nfs_available, Some(true));
    assert_eq!(app.samba_available, Some(false));
    assert!(app.shares_loaded);
}

#[test]
fn shares_result_for_a_stale_host_is_discarded() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.tx
        .send(WorkerMsg::Shares(
            1,
            Ok(crate::zfs::share::SharesSnapshot {
                shares: vec![smb("tank/data")],
                nfs_available: true,
                samba_available: true,
                smb_error: None,
                nfsv4_note: None,
            }),
        ))
        .unwrap();
    app.drain_worker_messages();
    assert!(app.shares.is_empty(), "a result for a host that isn't selected must not populate the pane");
}

#[test]
fn switching_host_clears_shares_state() {
    let mut app = App::new(vec!["local".to_string(), "backup".to_string()]);
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.nfs_available = Some(true);
    app.samba_available = Some(false);
    app.shares_loaded = true;
    app.handle_key(KeyCode::Tab); // -> Pools
    app.handle_key(KeyCode::BackTab); // -> Hosts
    app.handle_key(KeyCode::Down); // switch host, triggers clear_host_data
    assert!(app.shares.is_empty());
    assert_eq!(app.shares_table.selected(), None);
    assert_eq!(app.nfs_available, None);
    assert_eq!(app.samba_available, None);
    assert!(!app.shares_loaded);
}

#[test]
fn enter_on_nfs_share_row_opens_prefilled_edit_form() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Nfs {
        dataset: "tank/data".to_string(),
        raw: "-maproot=nobody -network 172.16.10.0/24".to_string(),
        networks: vec!["172.16.10.0/24".to_string()],
        hosts: vec![],
        read_only: false,
        root: Some("nobody".to_string()),
    }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields.len(), 4);
            assert_eq!(fields[0].value, "172.16.10.0/24");
            assert_eq!(fields[1].value, "");
            assert_eq!(fields[2].value, "n");
            assert_eq!(fields[3].value, "nobody");
            assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_d_on_nfs_share_row_opens_remove_confirm_modal() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![crate::zfs::share::Share::Nfs {
        dataset: "tank/data".to_string(),
        raw: "on".to_string(),
        networks: vec![],
        hosts: vec![],
        read_only: false,
        root: None,
    }];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => {
            assert_eq!(
                action.call,
                MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "off".to_string() }
            );
        }
        other => panic!("expected Confirm modal, got {other:?}"),
    }
}

#[test]
fn shift_m_on_datasets_opens_a_blank_smb_form_for_an_unshared_dataset() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.handle_key(KeyCode::Char('M'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(*kind, FormKind::ShareSmb { host: 0, dataset: "tank/media".into(), path: "/tank/media".into(), existing_user: None });
            assert_eq!(fields.len(), 5);
            assert_eq!(fields[4].value, "n");
            assert_eq!(fields[0].value, "media", "suggests the dataset's last path component");
            assert_eq!(fields[1].value, "");
        }
        other => panic!("expected Form, got {other:?}"),
    }
}

#[test]
fn shift_m_prefills_from_the_existing_managed_share() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")];
    app.handle_key(KeyCode::Char('M'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            assert_eq!(fields[0].value, "media");
            assert_eq!(fields[1].value, "carol");
            assert_eq!(fields[2].value, "", "the password is never pre-filled");
            assert_eq!(fields[3].value, "n");
            assert!(matches!(kind, FormKind::ShareSmb { existing_user: Some(u), .. } if u == "carol"));
        }
        other => panic!("expected Form, got {other:?}"),
    }
}

#[test]
fn shift_m_is_refused_without_a_mountpoint_before_shares_load_or_when_smbd_is_down() {
    let mut app = datasets_app_with_mount(None);
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("'tank/media' has no mounted mountpoint to share"));
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares_loaded = false;
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.samba_available = Some(false);
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message.as_deref(), Some("Samba (smbd) is not running on this host"));
}

#[test]
fn submitting_a_valid_smb_form_builds_a_set_smb_share_action() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["media", "carol", " pw 1 ", "\u{8}y", "n"]).expect("submitted");
    assert_eq!(
        action.call,
        MutationCall::SetSmbShare {
            share: crate::zfs::smb::SmbShare { name: "media".into(), dataset: "tank/media".into(), path: "/tank/media".into(), valid_user: Some("carol".into()), read_only: true, guest_ok: false },
            password: Some(crate::ui::modal::Secret(" pw 1 ".to_string())),
        },
        "the password is kept verbatim, not trimmed"
    );
    assert_eq!(action.section, Section::Shares);
}

#[test]
fn smb_form_validation_errors_keep_the_form_open_with_a_specific_message() {
    let cases: [([&str; 5], &str); 6] = [
        (["", "", "", "n", "n"], "required"),
        (["bad name", "", "", "n", "n"], "letters, digits"),
        (["media", "-evil", "pw", "n", "n"], "valid Unix username"),
        (["media", "carol", "", "n", "n"], "Samba password is required"),
        (["alice", "", "", "n", "n"], "already"),
        (["media", "carol", "pw", "n", "\u{8}y"], "Guest access can't be combined"),
    ];
    for (values, expect) in cases {
        let mut app = open_blank_smb_form();
        app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/other".into(), name: "alice".into(), path: "/x".into(), valid_user: None, read_only: true, guest_ok: false }];
        assert!(type_into_smb_form(&mut app, values).is_none());
        assert!(matches!(app.modal, Some(Modal::Form { .. })), "{values:?} must keep the form open");
        let msg = app.status_message.clone().unwrap_or_default();
        assert!(msg.contains(expect), "{values:?}: '{msg}' should contain '{expect}'");
    }
}

#[test]
fn editing_with_a_blank_password_is_allowed_only_when_the_user_is_unchanged() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")]; // existing user: carol
    app.handle_key(KeyCode::Char('M'));
    for _ in 0..4 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    let action = app.handle_modal_key(KeyCode::Enter).expect("unchanged user + blank password is fine");
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, .. }));

    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![smb("tank/media")];
    app.handle_key(KeyCode::Char('M'));
    app.handle_modal_key(KeyCode::Enter); // to Valid user
    for _ in 0..5 {
        app.handle_modal_key(KeyCode::Backspace);
    }
    for c in "bob".chars() {
        app.handle_modal_key(KeyCode::Char(c));
    }
    for _ in 0..4 {
        assert!(app.handle_modal_key(KeyCode::Enter).is_none());
    }
    assert!(matches!(app.modal, Some(Modal::Form { .. })), "changing the user requires a password");
}

#[test]
fn a_blank_user_ignores_any_typed_password() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "n"]).unwrap();
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.valid_user.is_none()));
}

#[test]
fn guest_y_with_a_blank_user_builds_a_guest_share_ignoring_the_password() {
    let mut app = open_blank_smb_form();
    let action = type_into_smb_form(&mut app, ["pub", "", "ignored", "n", "\u{8}y"]).unwrap();
    assert!(matches!(action.call, MutationCall::SetSmbShare { password: None, ref share } if share.guest_ok && share.valid_user.is_none()));
}

#[test]
fn editing_an_existing_guest_share_prefills_guest_y() {
    let mut app = datasets_app_with_mount(Some("/tank/media"));
    app.shares = vec![crate::zfs::share::Share::Smb { dataset: "tank/media".into(), name: "pub".into(), path: "/tank/media".into(), valid_user: None, read_only: true, guest_ok: true }];
    app.handle_key(KeyCode::Char('M'));
    let Some(Modal::Form { fields, .. }) = &app.modal else { panic!("expected form") };
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[4].value, "y");
}

#[test]
fn smb_form_warns_that_a_password_replaces_the_users_samba_password() {
    let modal = smb_edit_modal(None, "tank/data", "/tank/data", 0);
    let Modal::Form { description: Some(d), .. } = modal else { panic!("expected form with description") };
    assert!(d.contains("replaces"), "{d}");
}

#[test]
fn enter_on_an_smb_share_row_opens_its_prefilled_form_using_the_stanza_path() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    } // -> Shares
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Enter);
    assert!(matches!(&app.modal, Some(Modal::Form { kind: FormKind::ShareSmb { path, .. }, .. }) if path == "/tank/data"));
}

#[test]
fn handle_key_d_on_smb_share_row_opens_a_remove_confirm_for_the_managed_share() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    for _ in 0..5 {
        app.handle_key(KeyCode::Tab);
    }
    app.shares = vec![smb("tank/data")];
    app.shares_table.select(Some(0));
    app.handle_key(KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Confirm { action, .. }) => assert_eq!(action.call, MutationCall::RemoveSmbShare { dataset: "tank/data".to_string() }),
        other => panic!("expected Confirm, got {other:?}"),
    }
}

#[test]
fn handle_key_shift_n_on_datasets_opens_blank_share_nfs_form_when_not_yet_shared() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(true);
    app.shares_loaded = true;
    app.handle_key(KeyCode::Char('N'));
    match &app.modal {
        Some(Modal::Form { fields, kind, .. }) => {
            // read_only renders as "n" (not blank) for a false placeholder,
            // consistent with `share_edit_modal`'s handling of real shares
            // (see `enter_on_nfs_share_row_opens_prefilled_edit_form`) —
            // it still assembles to the same "on" value downstream.
            assert_eq!(fields[0].value, "");
            assert_eq!(fields[1].value, "");
            assert_eq!(fields[2].value, "n");
            assert_eq!(fields[3].value, "");
            assert_eq!(*kind, FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() });
        }
        other => panic!("expected Form modal, got {other:?}"),
    }
}

#[test]
fn handle_key_shift_n_on_datasets_sets_hint_when_nfs_unavailable() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(false);
    app.shares_loaded = true;
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none());
    assert_eq!(app.status_message, Some("NFS server not available on this host (nfsd is not running)".to_string()));
}

#[test]
fn handle_key_shift_n_with_unloaded_shares_names_the_fix() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab);
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none());
    assert!(app.status_message.as_deref().is_some_and(|m| m.contains("Press 'r' in the Shares section")));
}

#[test]
fn handle_key_shift_n_on_datasets_sets_hint_when_shares_not_yet_loaded() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_os_for_test("local", crate::zfs::host_health::HostOs::FreeBsd);
    app.handle_key(KeyCode::Tab);
    app.handle_key(KeyCode::Tab); // -> Datasets
    app.datasets = vec![Dataset { name: "tank/data".to_string(), kind: crate::zfs::dataset::DatasetKind::Filesystem, used: 0, available: 0, referenced: 0, mountpoint: None, quota: 0, refquota: 0 }];
    app.datasets_table.select(Some(0));
    app.nfs_available = Some(true); // availability known, but the share list itself was never refreshed
    app.handle_key(KeyCode::Char('N'));
    assert!(app.modal.is_none(), "must not pre-fill from a self.shares that was never populated — could silently widen an existing restricted share");
    assert!(app.status_message.is_some());
}

#[test]
fn build_pending_action_share_nfs_blank_fields_assembles_to_on() {
    let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![
        FormField { label: "Networks".to_string(), value: String::new() },
        FormField { label: "Hosts".to_string(), value: String::new() },
        FormField { label: "Read-only (y/n)".to_string(), value: String::new() },
        FormField { label: "Root user mapping".to_string(), value: String::new() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action,
        PendingAction {
            host: 0,
            section: Section::Shares,
            call: MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "on".to_string() },
        }
    );
}

#[test]
fn build_pending_action_share_nfs_assembles_filled_fields() {
    let kind = FormKind::ShareNfs { host: 0, dataset: "tank/data".to_string() };
    let fields = vec![
        FormField { label: "Networks".to_string(), value: "172.16.10.0/24".to_string() },
        FormField { label: "Hosts".to_string(), value: String::new() },
        FormField { label: "Read-only (y/n)".to_string(), value: "n".to_string() },
        FormField { label: "Root user mapping".to_string(), value: "nobody".to_string() },
    ];
    let action = build_pending_action(&kind, &fields).unwrap();
    assert_eq!(
        action.call,
        MutationCall::SetDatasetProperty { dataset: "tank/data".to_string(), key: "sharenfs".to_string(), value: "-maproot=nobody -network 172.16.10.0/24".to_string() }
    );
}

#[test]
fn set_error_marks_the_current_status_message_as_an_error() {
    let mut app = App::new(vec!["local".to_string()]);
    assert!(!app.status_is_error());
    app.set_error("zpool destroy failed".to_string());
    assert_eq!(app.status_message.as_deref(), Some("zpool destroy failed"));
    assert!(app.status_is_error());
}

#[test]
fn a_later_ordinary_message_is_not_styled_as_an_error() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("boom".to_string());
    app.status_message = Some("select a disk with Space first".to_string());
    assert!(!app.status_is_error(), "stale error flag must not recolor an unrelated message");
}

#[test]
fn clearing_the_message_clears_the_error_state() {
    let mut app = App::new(vec!["local".to_string()]);
    app.set_error("boom".to_string());
    app.handle_key(crossterm::event::KeyCode::Esc);
    assert_eq!(app.status_message, None);
    assert!(!app.status_is_error());
}

#[test]
fn a_share_refresh_keeps_the_selection_on_the_same_share_and_protocol() {
    let nfs = |d: &str| crate::zfs::share::Share::Nfs { dataset: d.to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None };
    let snapshot = |shares| crate::zfs::share::SharesSnapshot { shares, nfs_available: true, samba_available: true, smb_error: None, nfsv4_note: None };
    let mut app = in_section(Section::Shares);
    app.shares = vec![nfs("tank/a"), smb("tank/b")];
    app.shares_table.select(Some(1)); // the SMB share on tank/b
    app.tx.send(WorkerMsg::Shares(0, Ok(snapshot(vec![nfs("tank/0"), nfs("tank/b"), nfs("tank/a"), smb("tank/b")])))).unwrap();
    app.drain_worker_messages();
    assert_eq!(app.shares_table.selected(), Some(3), "tank/b's SMB share, not its NFS share");
}

#[test]
fn a_quiet_shares_refresh_keeps_an_informational_status() {
    let mut app = in_section(Section::Shares);
    app.status_message = Some("share created".to_string());
    app.auto_refresh(runner(""), Instant::now(), TEN);
    wait_until(&mut app, |a| a.shares_loaded);
    assert!(app.shares_loaded);
    assert_eq!(app.status_message.as_deref(), Some("share created"));
}

#[test]
fn the_share_keys_in_datasets_do_nothing_on_a_host_that_is_not_freebsd_and_are_not_advertised() {
    let mut app = two_os_app(crate::zfs::host_health::HostOs::Other);
    app.selected_host = 1;
    go_to(&mut app, Section::Datasets);
    app.datasets = vec![ds_named("tank/data", Some("/tank/data"))];
    app.datasets_table.select(Some(0));
    // as left over from a FreeBSD host's Shares screen
    (app.shares_loaded, app.nfs_available, app.samba_available) = (true, Some(true), Some(true));
    app.handle_key(KeyCode::Char('N'));
    app.handle_key(KeyCode::Char('M'));
    assert!(app.modal.is_none() && app.pending_mutations.is_empty(), "no share form, no zfs set sharenfs on a host that is not FreeBSD: {:?}", app.modal);
    assert!(!crate::ui::hints::hints(&app).iter().any(|h| h.key == "N" || h.key == "M"), "the keys are not offered");
    // and the FreeBSD host still gets them
    app.selected_host = 0;
    assert!(crate::ui::hints::hints(&app).iter().any(|h| h.key == "N"));
}
