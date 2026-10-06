use crate::error::{AppError, Result};
use crate::exec::CommandRunner;

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub name: String,
    pub id: u32,
}

/// Every account the host knows about, already ordered for display.
/// `group_error` is set when users loaded but `getent group` failed.
#[derive(Debug, Clone, PartialEq)]
pub struct HostAccounts {
    pub users: Vec<Account>,
    pub groups: Vec<Account>,
    pub group_error: Option<String>,
}

/// Lenient on purpose (like `parse_shares`): one odd line must not hide
/// every account. Skips short lines, empty names, non-numeric ids,
/// names with control characters, and nsswitch compat entries
/// (`+@netgroup`, `+::::::`, `-user`).
fn parse_colon_lines(output: &str, id_field: usize, min_fields: usize) -> Vec<Account> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < min_fields {
                return None;
            }
            let name = *fields.first()?;
            if name.is_empty() || name.starts_with('+') || name.starts_with('-') || name.chars().any(char::is_control) {
                return None;
            }
            let id = fields.get(id_field)?.parse::<u32>().ok()?;
            Some(Account { name: name.to_string(), id })
        })
        .collect()
}

/// `name:passwd:uid:gid:gecos:home:shell`
pub fn parse_passwd(output: &str) -> Vec<Account> {
    parse_colon_lines(output, 2, 7)
}

/// `name:passwd:gid:members` (members may be empty or absent)
pub fn parse_group(output: &str) -> Vec<Account> {
    parse_colon_lines(output, 2, 3)
}

fn is_real(id: u32) -> bool {
    (1000..60000).contains(&id)
}

/// Real accounts (id 1000..60000) first, then the rest; alphabetical
/// (byte order) within each block.
pub fn order(mut accounts: Vec<Account>) -> Vec<Account> {
    accounts.sort_by(|a, b| (!is_real(a.id), &a.name).cmp(&(!is_real(b.id), &b.name)));
    accounts
}

pub fn fetch_accounts(runner: &dyn CommandRunner) -> Result<HostAccounts> {
    let users = runner.run_unprivileged("getent", &["passwd"])?;
    if !users.success() {
        return Err(AppError::CommandFailed { cmd: "getent passwd".to_string(), status: users.status, stderr: users.stderr });
    }
    let (groups, group_error) = match runner.run_unprivileged("getent", &["group"]) {
        Ok(out) if out.success() => (order(parse_group(&out.stdout)), None),
        Ok(out) => {
            let reason = out.stderr.trim();
            let reason = if reason.is_empty() { format!("getent group exited with status {}", out.status) } else { reason.to_string() };
            (Vec::new(), Some(reason))
        }
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    Ok(HostAccounts { users: order(parse_passwd(&users.stdout)), groups, group_error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::FakeRunner;

    const PASSWD: &str = "root:*:0:0:Charlie &:/root:/bin/sh\ndaemon:*:1:1:Owner of many system processes:/root:/usr/sbin/nologin\ncarol:*:1003:1003:Carol:/home/carol:/bin/sh\nalice:*:1002:1002:Alice:/home/alice:/bin/sh\nnobody:*:65534:65534:Unprivileged user:/nonexistent:/usr/sbin/nologin\n";

    fn names(a: &[Account]) -> Vec<&str> {
        a.iter().map(|x| x.name.as_str()).collect()
    }

    #[test]
    fn parse_passwd_reads_name_and_uid() {
        let a = parse_passwd(PASSWD);
        assert_eq!(a.len(), 5);
        assert_eq!(a[2], Account { name: "carol".into(), id: 1003 });
    }

    #[test]
    fn parse_skips_malformed_compat_and_control_character_lines() {
        let input = "ok:*:1001:1001:x:/h:/s\nshort:*:5\n:*:7:7:x:/h:/s\nbad:*:abc:1:x:/h:/s\n+@netgroup\n+::::::\n-baduser:*:9:9:x:/h:/s\nctl\u{7}name:*:8:8:x:/h:/s\n";
        assert_eq!(names(&parse_passwd(input)), vec!["ok"]);
    }

    #[test]
    fn parse_group_reads_name_and_gid_with_and_without_members() {
        let a = parse_group("wheel:*:0:root,carol\ncarol:*:1003:\nodd:*:12\n+@x:*:1:\nbroken:*:x:\n");
        assert_eq!(names(&a), vec!["wheel", "carol", "odd"]);
        assert_eq!(a[1].id, 1003);
    }

    #[test]
    fn order_puts_real_accounts_first_then_system_alphabetically_nobody_counts_as_system() {
        let ordered = order(parse_passwd(PASSWD));
        assert_eq!(names(&ordered), vec!["alice", "carol", "daemon", "nobody", "root"]);
    }

    #[test]
    fn the_real_account_id_range_is_1000_inclusive_to_60000_exclusive() {
        let mk = |n: &str, id: u32| Account { name: n.into(), id };
        let ordered = order(vec![mk("z999", 999), mk("a1000", 1000), mk("b59999", 59999), mk("c60000", 60000)]);
        assert_eq!(names(&ordered), vec!["a1000", "b59999", "c60000", "z999"]);
    }

    #[test]
    fn fetch_accounts_uses_unprivileged_getent_never_the_privileged_run() {
        let r = FakeRunner::new("")
            .route("getent", "passwd", PASSWD)
            .route("getent", "group", "wheel:*:0:root\nstaff:*:1100:\n");
        let got = fetch_accounts(&r).unwrap();
        assert_eq!(names(&got.users), vec!["alice", "carol", "daemon", "nobody", "root"]);
        assert_eq!(names(&got.groups), vec!["staff", "wheel"]);
        assert_eq!(got.group_error, None);
        let un = r.unprivileged_calls.lock().unwrap();
        assert_eq!(un.len(), 2);
        assert_eq!(un[0], ("getent".to_string(), vec!["passwd".to_string()]));
        assert_eq!(un[1], ("getent".to_string(), vec!["group".to_string()]));
        assert_eq!(r.calls.lock().unwrap().len(), 2, "every call went through run_unprivileged");
    }

    #[test]
    fn a_users_failure_is_an_error_naming_the_stderr() {
        let r = FakeRunner::new("").route_failure("getent", "passwd", "getent: not found", 127);
        let err = fetch_accounts(&r).unwrap_err().to_string();
        assert!(err.contains("getent passwd") && err.contains("not found"), "{err}");
    }

    #[test]
    fn a_groups_only_failure_keeps_the_users_and_records_the_reason() {
        let r = FakeRunner::new("").route("getent", "passwd", PASSWD).route_failure("getent", "group", "", 2);
        let got = fetch_accounts(&r).unwrap();
        assert_eq!(got.users.len(), 5);
        assert!(got.groups.is_empty());
        assert!(got.group_error.unwrap().contains('2'), "an empty stderr still yields a reason");
    }
}
