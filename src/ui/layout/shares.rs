//! The Shares list.
#![allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn smb_access_label(guest_ok: bool, valid_user: Option<&str>) -> String {
    match (guest_ok, valid_user) {
        (true, _) => "guest".to_string(),
        (false, Some(u)) => format!("user={u}"),
        (false, None) => "any Samba user".to_string(),
    }
}

pub(super) fn draw_shares(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.section_supported(Section::Shares) {
        draw_unsupported(f, app, area, Section::Shares);
        return;
    }
    let theme = app.theme.clone();
    let rows = app.shares.iter().map(|s| match s {
        Share::Nfs { dataset, raw, .. } => Row::new(vec![dataset.clone(), "NFS".to_string(), raw.clone()]),
        Share::Smb { dataset, name, valid_user, read_only, guest_ok, .. } => Row::new(vec![
            dataset.clone(),
            "SMB".to_string(),
            format!("\\\\{name}  {}  {}", smb_access_label(*guest_ok, valid_user.as_deref()), if *read_only { "ro" } else { "rw" }),
        ]),
    });
    let widths = [Constraint::Length(24), Constraint::Length(8), Constraint::Min(20)];
    let mut hints = Vec::new();
    // The services' own names: FreeBSD's `nfsd` and `smbd`, `nfs-server` and `smbd` on Linux, SMF's `nfs/server` and `smb/server`.
    let (nfs, smb) = match app.host_os(app.selected_host) {
        Some(os @ (crate::zfs::host_health::HostOs::Linux | crate::zfs::host_health::HostOs::Illumos)) => (crate::zfs::share::nfs_name(os), crate::zfs::share::smb_name(os)),
        _ => ("nfsd", "smbd"),
    };
    let (nfs_text, smb_text) = (format!("NFS unavailable: {nfs} not running"), format!("SMB unavailable: {smb} not running"));
    if app.nfs_available == Some(false) {
        hints.push(nfs_text.as_str());
    }
    let samba_listed = app.host_os(app.selected_host) != Some(crate::zfs::host_health::HostOs::Linux);
    if app.samba_available == Some(false) && samba_listed {
        hints.push(smb_text.as_str());
    }
    let hint_suffix = if hints.is_empty() { String::new() } else { format!(" [{}]", hints.join("; ")) };
    let actions = match app.host_os(app.selected_host) {
        Some(os) if !app.shares_editable() => format!("'r' refresh; read-only on {}", os.word()),
        _ => "'r' refresh, Enter edit, 'd' remove".to_string(),
    };
    let title = format!("Shares — {} ({actions}){hint_suffix}", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(theme.header_row(vec!["Dataset", "Protocol", "Details"]))
        .block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.shares_table);
}
