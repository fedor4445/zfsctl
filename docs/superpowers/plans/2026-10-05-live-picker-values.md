# Live-Fetched Account Values for the F2 Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `F2` on four form fields (SMB "Valid user", NFS "Root user mapping", `zfs allow` Recipient, ACL Recipient) opens a picker listing the managed host's real Unix users/groups, fetched asynchronously over SSH without `sudo`.

**Architecture:** A new unprivileged runner method (`run_unprivileged`) feeds a new `src/zfs/accounts.rs` (`getent passwd`/`getent group`). `PickerState` gains a load state (`Ready`/`Loading`/`Failed`) and an optional account-field marker; `App` keeps a per-host 60 s cache, an in-flight map (30 s stuck expiry) and a queue the event loop drains into worker threads, following the existing `pending_new_hosts` pattern.

**Tech Stack:** Rust, ratatui/crossterm, `std::thread` + `mpsc` worker messages (existing pattern).

**Spec:** `docs/superpowers/specs/2026-10-05-live-picker-values-design.md`

## Global Constraints

- Account lookups run **without `sudo`**; the sudoers entries on managed hosts and the README's sudoers section do **not** change. `run_unprivileged` is for read-only, privilege-free lookups only; the privileged `run` path is unchanged.
- "Real" accounts: id ≥ 1000 and < 60000 (so `nobody`, uid 65534, is *not* real). Real accounts first, then the rest; alphabetical (byte order) within each block.
- Cache per host: **60 s**. Fetch on the first F2 press only; **no prefetch**. Failures are **never** cached. An in-flight marker older than **30 s** counts as stuck.
- Recipient picker values: `zfs allow` → `everyone`, then `user:<name>`, then `group:<name>`; ACL → `owner`, `group`, `everyone`, then `user:<name>`, then `group:<name>`. User fields → bare user names. All single-select. Selecting replaces the field text; typing by hand keeps working in every state.
- Failure text in the picker: `Could not load accounts: <reason>`; `Esc` closes with the field untouched. Groups-only failure: users shown, popup title carries `groups unavailable: <reason>`.
- No fallback to the local machine's accounts. No account creation/modification. Existing hint-bar drift guards must pass **unmodified**.
- Project rules: verify end-to-end on the real boxes (a green unit-test run is not enough); derive exhaustive lists from the code.

## Review Focus

Failure modes the spec implies; each is pinned by a test in the task named.

1. The privileged `run` must still add `sudo -n`; the unprivileged variant must not; `fetch_accounts` must never call `run` (Tasks 1, 2).
2. A late result for host A must never fill a picker opened for host B, nor a picker whose form was closed (Task 3).
3. A hung fetch must not wedge F2: the in-flight marker expires after 30 s and the next F2 queues a fresh fetch; `Esc` always closes (Task 3).
4. `getent` compat lines (`+@netgroup`, `+::::::`, `-user`) and malformed lines must be skipped, not listed (Task 2).
5. A name the recipient parsers could reject (spaces, `\`, `$`, control characters) must not be offered (Task 2).
6. Thousands of accounts (LDAP hosts) must not make filtering/rendering sluggish — values are plain `Vec<String>` and filtering stays the existing linear scan; a 5000-entry test pins that fill and filter work (Task 3).

## File Structure

- Modify `src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs` — `run_unprivileged`.
- Create `src/zfs/accounts.rs`; modify `src/zfs/mod.rs` (`pub mod accounts;`).
- Modify `src/ui/picker.rs` — `AccountField`, `PickerLoad`, new `PickerState` fields, `account_field_for`, `build_values`, `fill_account_picker`.
- Modify `src/ui/state.rs` — cache/queue/in-flight, F2 handler, `WorkerMsg::Accounts`, drain arm, `handle_picker_key` states.
- Modify `src/ui/mod.rs` — `apply_pending_account_fetches` + event-loop call.
- Modify `src/ui/layout.rs` (`draw_picker` states), `src/ui/hints.rs` (`picker_hints` states).
- Modify `README.md` (one short paragraph).

---

### Task 1: `CommandRunner::run_unprivileged`

**Files:** Modify `src/exec/mod.rs`, `src/exec/local.rs`, `src/exec/ssh.rs`

**Interfaces:**
- Produces: `CommandRunner::run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>`; `SshRunner::build_args_unprivileged(&self, cmd, args) -> Vec<String>`; `FakeRunner.unprivileged_calls: Mutex<Vec<(String, Vec<String>)>>` (calls made through `run_unprivileged`; they are also recorded in `calls`).

- [ ] **Step 1: Write failing tests.** In `src/exec/ssh.rs` `mod tests`:

```rust
    #[test]
    fn unprivileged_args_have_the_same_options_and_quoting_but_no_sudo() {
        let runner = SshRunner::new("backup-host");
        let privileged = runner.build_args("getent", &["passwd"]);
        let unprivileged = runner.build_args_unprivileged("getent", &["passwd"]);
        assert_eq!(privileged.last().unwrap(), "sudo -n getent passwd");
        assert_eq!(unprivileged.last().unwrap(), "getent passwd");
        assert_eq!(privileged[..privileged.len() - 1], unprivileged[..unprivileged.len() - 1], "same ssh options, `--`, and host");
    }

    #[test]
    fn unprivileged_args_still_quote_unsafe_arguments() {
        let runner = SshRunner::new("h");
        let args = runner.build_args_unprivileged("echo", &["a b; rm -rf /"]);
        assert_eq!(args.last().unwrap(), "echo 'a b; rm -rf /'");
    }
```

In `src/exec/local.rs` `mod tests`:

```rust
    #[test]
    fn run_unprivileged_runs_the_command_directly() {
        let output = LocalRunner.run_unprivileged("echo", &["hi"]).unwrap();
        assert_eq!(output.stdout.trim(), "hi");
        assert!(output.success());
    }
```

In `src/exec/mod.rs` `mod tests`:

```rust
    #[test]
    fn fake_runner_records_unprivileged_calls_separately_and_honours_routes() {
        let runner = FakeRunner::new("").route("getent", "passwd", "root:*:0:0::/root:/bin/sh\n");
        runner.run("zfs", &["list"]).unwrap();
        let out = runner.run_unprivileged("getent", &["passwd"]).unwrap();
        assert_eq!(out.stdout, "root:*:0:0::/root:/bin/sh\n");
        assert_eq!(runner.unprivileged_calls.lock().unwrap().len(), 1);
        assert_eq!(runner.unprivileged_calls.lock().unwrap()[0].0, "getent");
        assert_eq!(runner.calls.lock().unwrap().len(), 2, "also recorded in the general call log");
    }
```

- [ ] **Step 2: Run** `cargo test exec::` — Expected: compile FAIL (methods missing).

- [ ] **Step 3: Implement.** `src/exec/mod.rs` trait:

```rust
    /// Runs `cmd` WITHOUT privilege escalation: over SSH this omits the
    /// `sudo -n` prefix every other call carries; locally it is the same as
    /// `run`. For read-only lookups that need no privilege (e.g.
    /// `getent passwd`). Never use it for anything that mutates state.
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>;
```

`FakeRunner`: add field `pub unprivileged_calls: Mutex<Vec<(String, Vec<String>)>>` (init `Mutex::new(Vec::new())` in `new` and `failing`) and

```rust
        fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
            self.unprivileged_calls.lock().unwrap().push((cmd.to_string(), args.iter().map(|s| s.to_string()).collect()));
            self.run(cmd, args)
        }
```

`LocalRunner`:

```rust
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.run(cmd, args)
    }
```

`SshRunner`: keep `build_args` (and its long doc comment) as the privileged entry point and share the body:

```rust
    fn build_args(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, true)
    }

    /// Same as `build_args` but without the `sudo -n` prefix; see
    /// `CommandRunner::run_unprivileged`.
    fn build_args_unprivileged(&self, cmd: &str, args: &[&str]) -> Vec<String> {
        self.build_args_with(cmd, args, false)
    }

    fn build_args_with(&self, cmd: &str, args: &[&str], sudo: bool) -> Vec<String> {
        let mut remote = if sudo { "sudo -n ".to_string() } else { String::new() };
        remote.push_str(&shell_quote(cmd));
        for arg in args {
            remote.push(' ');
            remote.push_str(&shell_quote(arg));
        }
        vec![
            "-o".to_string(), "BatchMode=yes".to_string(),
            "-o".to_string(), "ConnectTimeout=10".to_string(),
            "--".to_string(), self.host.clone(), remote,
        ]
    }
```

and factor the existing `run` body into `fn run_ssh(&self, ssh_args: Vec<String>) -> Result<CommandOutput>` (the existing `Command::new("ssh").args(..).output()` + `CommandOutput` construction), so:

```rust
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.run_ssh(self.build_args(cmd, args))
    }
    fn run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput> {
        self.run_ssh(self.build_args_unprivileged(cmd, args))
    }
```

(`run_ssh` is an inherent method of `SshRunner`, not part of the trait.)

- [ ] **Step 4: Run** `cargo test` — Expected: all PASS, including every pre-existing `build_args` test unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/exec && git commit -m "feat: CommandRunner::run_unprivileged for read-only lookups without sudo

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `src/zfs/accounts.rs` — parse, order, fetch

**Files:** Create `src/zfs/accounts.rs`; Modify `src/zfs/mod.rs` (add `pub mod accounts;`)

**Interfaces:**
- Consumes: `CommandRunner::run_unprivileged`, `FakeRunner::{route, route_failure, unprivileged_calls, calls}` (Task 1).
- Produces: `Account { name: String, id: u32 }`; `HostAccounts { users: Vec<Account>, groups: Vec<Account>, group_error: Option<String> }` (both `Debug, Clone, PartialEq`); `parse_passwd(&str) -> Vec<Account>`; `parse_group(&str) -> Vec<Account>`; `order(Vec<Account>) -> Vec<Account>`; `fetch_accounts(&dyn CommandRunner) -> Result<HostAccounts>` (users and groups already ordered).

- [ ] **Step 1: Write failing tests** (create the file with signature stubs `todo!()` and this test module):

```rust
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
        assert!(got.group_error.unwrap().contains("2"), "an empty stderr still yields a reason");
    }
}
```

- [ ] **Step 2: Run** `cargo test zfs::accounts` — Expected: FAIL (`todo!()` panics).

- [ ] **Step 3: Implement** (replace the stubs):

```rust
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
            let name = fields[0];
            if name.is_empty() || name.starts_with('+') || name.starts_with('-') || name.chars().any(char::is_control) {
                return None;
            }
            let id = fields[id_field].parse::<u32>().ok()?;
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
```

- [ ] **Step 4: Run** `cargo test zfs::accounts` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/zfs/accounts.rs src/zfs/mod.rs && git commit -m "feat: fetch and order a host's users and groups via unprivileged getent

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Picker model, cache, queue, and F2 round trip (state logic)

**Files:** Modify `src/ui/picker.rs`, `src/ui/state.rs`

**Interfaces:**
- Consumes: `zfs::accounts::{HostAccounts, Account, fetch_accounts}`, `zfs::smb::validate_user`, `FormKind::{ShareSmb, ShareNfs, GrantPermission, AddAclEntry}` (each has a `host: usize`).
- Produces (picker.rs): `AccountField { User, ZfsAllowRecipient, AclRecipient }` (`Debug, Clone, Copy, PartialEq, Eq`); `PickerLoad { Ready, Loading, Failed(String) }` (`Debug, Clone, PartialEq`); `PickerState` gains `pub load: PickerLoad`, `pub note: Option<String>`, `pub account: Option<(usize, AccountField)>`; `account_field_for(&FormKind, usize) -> Option<(usize, AccountField)>`; `build_values(AccountField, &HostAccounts) -> Vec<String>`; `fill_account_picker(&mut PickerState, &HostAccounts, current_value: &str)`.
- Produces (state.rs): consts `ACCOUNT_CACHE_TTL` (60 s), `ACCOUNT_FETCH_STUCK_AFTER` (30 s); `App.{account_cache: HashMap<usize, (Instant, HostAccounts)>, account_fetch_in_flight: HashMap<usize, Instant>, pending_account_fetch: Vec<usize>}`; `App::fetch_accounts(&mut self, runner: Arc<dyn CommandRunner>, host: usize)`; `WorkerMsg::Accounts(usize, Result<HostAccounts>)`.

Behaviour contract: `build_picker_state` stays pure — for an account field it returns a `Loading` skeleton (so the F2 hint's `build_picker_state(..).is_some()` check keeps working); the F2 handler in `App` then fills it from a fresh cache, or leaves it `Loading` and queues a fetch.

- [ ] **Step 1: Write failing tests.**

`picker.rs` `mod tests`:

```rust
    use crate::zfs::accounts::{Account, HostAccounts};

    fn acct(name: &str, id: u32) -> Account {
        Account { name: name.to_string(), id }
    }
    fn sample() -> HostAccounts {
        HostAccounts { users: vec![acct("carol", 1003), acct("root", 0), acct("we ird", 1500)], groups: vec![acct("staff", 1100)], group_error: None }
    }

    #[test]
    fn account_field_for_maps_exactly_the_four_live_fields_and_carries_the_host() {
        let smb = FormKind::ShareSmb { host: 2, dataset: "d".into(), path: "/d".into(), existing_user: None };
        let nfs = FormKind::ShareNfs { host: 1, dataset: "d".into() };
        let grant = FormKind::GrantPermission { host: 3, dataset: "d".into() };
        let acl = FormKind::AddAclEntry { host: 4, path: "/d".into(), position: 0 };
        assert_eq!(account_field_for(&smb, 1), Some((2, AccountField::User)));
        assert_eq!(account_field_for(&nfs, 3), Some((1, AccountField::User)));
        assert_eq!(account_field_for(&grant, 0), Some((3, AccountField::ZfsAllowRecipient)));
        assert_eq!(account_field_for(&acl, 0), Some((4, AccountField::AclRecipient)));
        assert_eq!(account_field_for(&smb, 0), None);
        assert_eq!(account_field_for(&smb, 2), None, "password field");
        assert_eq!(account_field_for(&nfs, 0), None);
        assert_eq!(account_field_for(&grant, 1), None);
        assert_eq!(account_field_for(&acl, 1), None);
    }

    #[test]
    fn build_picker_state_for_an_account_field_is_a_loading_single_select_skeleton() {
        let kind = FormKind::ShareSmb { host: 2, dataset: "d".into(), path: "/d".into(), existing_user: None };
        let p = build_picker_state(&kind, 1, "").unwrap();
        assert_eq!(p.load, PickerLoad::Loading);
        assert!(p.values.is_empty() && !p.multi_select);
        assert_eq!(p.account, Some((2, AccountField::User)));
    }

    #[test]
    fn static_pickers_are_ready_with_no_account() {
        let kind = FormKind::GrantPermission { host: 0, dataset: "d".into() };
        let p = build_picker_state(&kind, 2, "").unwrap();
        assert_eq!((p.load, p.account, p.note), (PickerLoad::Ready, None, None));
    }

    #[test]
    fn build_values_orders_and_prefixes_per_field_and_drops_unsafe_names() {
        let s = sample();
        assert_eq!(build_values(AccountField::User, &s), vec!["carol", "root"]);
        assert_eq!(
            build_values(AccountField::ZfsAllowRecipient, &s),
            vec!["everyone", "user:carol", "user:root", "group:staff"]
        );
        assert_eq!(
            build_values(AccountField::AclRecipient, &s),
            vec!["owner", "group", "everyone", "user:carol", "user:root", "group:staff"]
        );
    }

    #[test]
    fn fill_account_picker_sets_values_ready_preselects_the_current_value_and_notes_group_errors() {
        let mut p = build_picker_state(&FormKind::ShareNfs { host: 0, dataset: "d".into() }, 3, "").unwrap();
        let mut accounts = sample();
        accounts.group_error = Some("boom".to_string());
        fill_account_picker(&mut p, &accounts, " root ");
        assert_eq!(p.load, PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert_eq!(p.selected, 1, "current text matches 'root'");
        assert_eq!(p.note.as_deref(), Some("groups unavailable: boom"));
    }
```

`state.rs` `mod tests` (reuse the call existing F2 tests use to press F2 inside a form — search `KeyCode::F(2)` in this file's tests — and the same way existing tests build a `Modal::Form`; `smb_form_app()` below builds the app; adapt names to the existing helpers):

```rust
    fn accounts_fixture() -> crate::zfs::accounts::HostAccounts {
        use crate::zfs::accounts::{Account, HostAccounts};
        HostAccounts {
            users: vec![Account { name: "carol".into(), id: 1003 }, Account { name: "root".into(), id: 0 }],
            groups: vec![Account { name: "staff".into(), id: 1100 }],
            group_error: None,
        }
    }

    /// An App with an SMB share form open on its "Valid user" field (index 1) for `host`.
    fn smb_form_app(host: usize) -> App {
        let mut app = App::new(vec!["local".to_string(), "other".to_string()]);
        app.modal = Some(Modal::Form {
            title: "SMB share".to_string(),
            fields: vec![
                FormField { label: "Share name".into(), value: "media".into() },
                FormField { label: "Valid user".into(), value: "root".into() },
                FormField { label: "Password".into(), value: String::new() },
                FormField { label: "Read-only".into(), value: "n".into() },
                FormField { label: "Guest".into(), value: "n".into() },
            ],
            focused: 1,
            description: None,
            kind: FormKind::ShareSmb { host, dataset: "tank/a".into(), path: "/tank/a".into(), existing_user: None },
        });
        app
    }

    #[test]
    fn f2_on_an_account_field_with_no_cache_opens_a_loading_picker_and_queues_one_fetch() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        let p = app.picker.as_ref().expect("picker opens");
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Loading);
        assert_eq!(app.pending_account_fetch, vec![1]);
        assert!(app.account_fetch_in_flight.contains_key(&1));
    }

    #[test]
    fn a_second_f2_while_the_fetch_is_in_flight_queues_nothing_more() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.picker = None;
        press_f2(&mut app);
        assert_eq!(app.pending_account_fetch, vec![1]);
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
    }

    #[test]
    fn a_stuck_in_flight_marker_older_than_30s_lets_the_next_f2_queue_a_fresh_fetch() {
        let mut app = smb_form_app(1);
        app.account_fetch_in_flight.insert(1, Instant::now() - Duration::from_secs(31));
        press_f2(&mut app);
        assert_eq!(app.pending_account_fetch, vec![1]);
    }

    #[test]
    fn a_fresh_cache_opens_ready_immediately_and_queues_nothing() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
        press_f2(&mut app);
        let p = app.picker.as_ref().unwrap();
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert_eq!(p.selected, 1, "field text 'root' is preselected");
        assert!(app.pending_account_fetch.is_empty());
    }

    #[test]
    fn a_cache_older_than_60s_is_treated_as_missing() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now() - Duration::from_secs(61), accounts_fixture()));
        press_f2(&mut app);
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
        assert_eq!(app.pending_account_fetch, vec![1]);
    }

    #[test]
    fn an_ok_result_fills_the_open_loading_picker_caches_and_clears_in_flight() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        let p = app.picker.as_ref().unwrap();
        assert_eq!(p.load, crate::ui::picker::PickerLoad::Ready);
        assert_eq!(p.values, vec!["carol", "root"]);
        assert!(app.account_cache.contains_key(&1));
        assert!(!app.account_fetch_in_flight.contains_key(&1));
    }

    #[test]
    fn an_err_result_marks_the_picker_failed_and_is_not_cached() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        let err = crate::error::AppError::Parse("ssh: connect timed out".to_string());
        app.tx.send(WorkerMsg::Accounts(1, Err(err))).unwrap();
        app.drain_worker_messages();
        assert!(matches!(&app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Failed(m) if m.contains("timed out")));
        assert!(app.account_cache.is_empty());
        assert!(app.account_fetch_in_flight.is_empty(), "a retry after Esc must be able to queue again");
    }

    #[test]
    fn a_result_for_another_host_never_touches_the_open_picker_but_is_cached() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        app.tx.send(WorkerMsg::Accounts(0, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        assert_eq!(app.picker.as_ref().unwrap().load, crate::ui::picker::PickerLoad::Loading);
        assert!(app.account_cache.contains_key(&0));
    }

    #[test]
    fn a_late_result_with_no_picker_open_is_cached_and_does_not_open_one() {
        let mut app = smb_form_app(1);
        app.tx.send(WorkerMsg::Accounts(1, Ok(accounts_fixture()))).unwrap();
        app.drain_worker_messages();
        assert!(app.picker.is_none());
        assert!(app.account_cache.contains_key(&1));
    }

    #[test]
    fn loading_and_failed_pickers_accept_only_esc() {
        let mut app = smb_form_app(1);
        press_f2(&mut app);
        for key in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Enter, KeyCode::Backspace] {
            app.handle_picker_key(key);
            assert!(app.picker.is_some(), "{key:?} must not close or alter a Loading picker");
            assert!(app.picker.as_ref().unwrap().filter.is_empty());
        }
        app.handle_picker_key(KeyCode::Esc);
        assert!(app.picker.is_none());
        // The field is untouched.
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "root"),
            other => panic!("form must stay open: {other:?}"),
        }
    }

    #[test]
    fn selecting_a_live_value_replaces_the_field_text() {
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), accounts_fixture()));
        press_f2(&mut app);
        app.handle_picker_key(KeyCode::Up); // carol
        app.handle_picker_key(KeyCode::Enter);
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "carol"),
            other => panic!("{other:?}"),
        }
        assert!(app.picker.is_none());
    }

    #[test]
    fn five_thousand_accounts_fill_and_filter_correctly() {
        use crate::zfs::accounts::{Account, HostAccounts};
        let users = (0..5000).map(|i| Account { name: format!("user{i:04}"), id: 2000 + i }).collect();
        let mut app = smb_form_app(1);
        app.account_cache.insert(1, (Instant::now(), HostAccounts { users, groups: vec![], group_error: None }));
        press_f2(&mut app);
        assert_eq!(app.picker.as_ref().unwrap().values.len(), 5000);
        for c in "4999".chars() {
            app.handle_picker_key(KeyCode::Char(c));
        }
        app.handle_picker_key(KeyCode::Enter);
        match &app.modal {
            Some(Modal::Form { fields, .. }) => assert_eq!(fields[1].value, "user4999"),
            other => panic!("{other:?}"),
        }
    }
```

Add this local helper beside them (replace its body with whatever the existing F2 tests use to deliver `KeyCode::F(2)` to an open form):

```rust
    fn press_f2(app: &mut App) {
        app.handle_modal_key(KeyCode::F(2));
    }
```

- [ ] **Step 2: Run** `cargo test ui::` — Expected: compile FAIL.

- [ ] **Step 3: Implement `picker.rs`.** Add the new types, extend `PickerState` (add the three fields; update **every** existing `PickerState { .. }` literal in the crate — `build_picker_state`, and test literals in `picker.rs`, `hints.rs`, `state.rs` — with `load: PickerLoad::Ready, note: None, account: None`), and add:

```rust
use crate::zfs::accounts::HostAccounts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountField {
    User,
    ZfsAllowRecipient,
    AclRecipient,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerLoad {
    Ready,
    Loading,
    Failed(String),
}

/// Which live account list (and for which host) a form field's picker is
/// sourced from, if any. Keyed on `(kind, field_index)` like `vocabulary_for`.
pub fn account_field_for(kind: &FormKind, field_index: usize) -> Option<(usize, AccountField)> {
    match (kind, field_index) {
        (FormKind::ShareSmb { host, .. }, 1) | (FormKind::ShareNfs { host, .. }, 3) => Some((*host, AccountField::User)),
        (FormKind::GrantPermission { host, .. }, 0) => Some((*host, AccountField::ZfsAllowRecipient)),
        (FormKind::AddAclEntry { host, .. }, 0) => Some((*host, AccountField::AclRecipient)),
        _ => None,
    }
}

/// The displayed list for an account picker. Names that
/// `smb::validate_user` rejects (spaces, `\`, `$`, a leading `-`, ...) are
/// never offered: they could be selected and then refused by the form's
/// own parser.
pub fn build_values(field: AccountField, accounts: &HostAccounts) -> Vec<String> {
    let safe = |a: &&crate::zfs::accounts::Account| crate::zfs::smb::validate_user(&a.name).is_ok();
    let users = accounts.users.iter().filter(safe).map(|a| a.name.clone());
    let groups = accounts.groups.iter().filter(safe).map(|a| a.name.clone());
    match field {
        AccountField::User => users.collect(),
        AccountField::ZfsAllowRecipient => std::iter::once("everyone".to_string())
            .chain(users.map(|n| format!("user:{n}")))
            .chain(groups.map(|n| format!("group:{n}")))
            .collect(),
        AccountField::AclRecipient => ["owner", "group", "everyone"]
            .into_iter()
            .map(str::to_string)
            .chain(users.map(|n| format!("user:{n}")))
            .chain(groups.map(|n| format!("group:{n}")))
            .collect(),
    }
}

pub fn fill_account_picker(picker: &mut PickerState, accounts: &HostAccounts, current_value: &str) {
    let Some((_, field)) = picker.account else { return };
    picker.values = build_values(field, accounts);
    picker.selected = picker.values.iter().position(|v| v == current_value.trim()).unwrap_or(0);
    picker.note = accounts.group_error.as_ref().map(|e| format!("groups unavailable: {e}"));
    picker.load = PickerLoad::Ready;
}
```

and at the top of `build_picker_state`:

```rust
    if let Some((host, field)) = account_field_for(kind, field_index) {
        return Some(PickerState {
            values: Vec::new(),
            filter: String::new(),
            selected: 0,
            checked: std::collections::HashSet::new(),
            multi_select: false,
            load: PickerLoad::Loading,
            note: None,
            account: Some((host, field)),
        });
    }
```

- [ ] **Step 4: Implement `state.rs`.**

Constants and fields (init in `App::new`: empty maps/vec):

```rust
pub const ACCOUNT_CACHE_TTL: Duration = Duration::from_secs(60);
pub const ACCOUNT_FETCH_STUCK_AFTER: Duration = Duration::from_secs(30);
```
```rust
    pub account_cache: HashMap<usize, (Instant, crate::zfs::accounts::HostAccounts)>,
    pub account_fetch_in_flight: HashMap<usize, Instant>,
    pub pending_account_fetch: Vec<usize>,
```

(Use the file's existing `HashMap`/`Instant`/`Duration` imports; add any that are missing.)

`WorkerMsg` variant, placed after `HostProbe`:

```rust
    /// The outcome of a live account fetch for host index `usize`.
    Accounts(usize, Result<crate::zfs::accounts::HostAccounts>),
```

F2 handler arm in `handle_modal_key` (replace the existing body):

```rust
                KeyCode::F(2) => {
                    if let Some(mut picker) = build_picker_state(&kind, focused, &fields[focused].value) {
                        if let Some((host, _)) = picker.account {
                            self.open_account_picker(&mut picker, host, &fields[focused].value, Instant::now());
                        }
                        self.picker = Some(picker);
                    }
                    self.modal = Some(Modal::Form { title, fields, focused, description, kind });
                    None
                }
```

New methods on `App`:

```rust
    /// Fills `picker` from a fresh cache entry for `host`, or leaves it
    /// `Loading` and queues one fetch (unless a non-stuck one is already in
    /// flight). `now` is a parameter so tests can age cache/in-flight entries.
    fn open_account_picker(&mut self, picker: &mut PickerState, host: usize, current_value: &str, now: Instant) {
        if let Some((fetched_at, accounts)) = self.account_cache.get(&host) {
            if now.duration_since(*fetched_at) < ACCOUNT_CACHE_TTL {
                crate::ui::picker::fill_account_picker(picker, accounts, current_value);
                return;
            }
        }
        let stuck = self.account_fetch_in_flight.get(&host).map_or(true, |t| now.duration_since(*t) >= ACCOUNT_FETCH_STUCK_AFTER);
        if stuck {
            self.account_fetch_in_flight.insert(host, now);
            self.pending_account_fetch.push(host);
        }
    }

    /// Runs the account lookup on a worker thread and reports it as
    /// `WorkerMsg::Accounts`. Called by the event loop for each queued host
    /// (`App` cannot resolve a host index to a runner on its own).
    pub fn fetch_accounts(&mut self, runner: Arc<dyn CommandRunner>, host: usize) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = crate::zfs::accounts::fetch_accounts(runner.as_ref());
            let _ = tx.send(WorkerMsg::Accounts(host, result));
        });
    }

    fn focused_form_value(&self) -> String {
        match &self.modal {
            Some(Modal::Form { fields, focused, .. }) => fields.get(*focused).map(|f| f.value.clone()).unwrap_or_default(),
            _ => String::new(),
        }
    }
```

`drain_worker_messages` arm:

```rust
                WorkerMsg::Accounts(host, result) => {
                    self.account_fetch_in_flight.remove(&host);
                    let current = self.focused_form_value();
                    let waiting = |p: &PickerState| p.load == crate::ui::picker::PickerLoad::Loading && p.account.map(|(h, _)| h) == Some(host);
                    match result {
                        Ok(accounts) => {
                            if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                                crate::ui::picker::fill_account_picker(p, &accounts, &current);
                            }
                            self.account_cache.insert(host, (Instant::now(), accounts));
                        }
                        Err(e) => {
                            if let Some(p) = self.picker.as_mut().filter(|p| waiting(p)) {
                                p.load = crate::ui::picker::PickerLoad::Failed(e.to_string());
                            }
                        }
                    }
                }
```

`handle_picker_key`, right after `let Some(mut picker) = self.picker.take() else { return };`:

```rust
        if picker.load != crate::ui::picker::PickerLoad::Ready {
            // Loading / Failed: nothing to filter or pick; only Esc leaves.
            if key != KeyCode::Esc {
                self.picker = Some(picker);
            }
            return;
        }
```

Add an arm for `WorkerMsg::Accounts` anywhere the compiler reports a non-exhaustive match on `WorkerMsg`.

- [ ] **Step 5: Run** `cargo test` and `cargo clippy --all-targets` — Expected: all PASS, no new warnings. The hint-bar drift guards in `hints.rs` must pass unmodified (F2 is still advertised via `build_picker_state(..).is_some()`; for the four new fields pressing it opens a picker).

- [ ] **Step 6: Commit**

```bash
git add -A src && git commit -m "feat: live account picker state: load states, per-host cache, queued fetch

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Event loop, rendering, hint bar, docs

**Files:** Modify `src/ui/mod.rs`, `src/ui/layout.rs`, `src/ui/hints.rs`, `README.md`

**Interfaces:**
- Consumes: `App::{pending_account_fetch, fetch_accounts}`, `PickerLoad`, `PickerState.note`.
- Produces: `fn apply_pending_account_fetches(app: &mut App, inventory: &HostInventory)` in `ui/mod.rs`.

- [ ] **Step 1: Write failing tests.**

`ui/mod.rs` `mod tests` (model on the existing `apply_pending_new_hosts_*` tests for how an `App`/`HostInventory` pair is built):

```rust
    #[test]
    fn apply_pending_account_fetches_drains_the_queue_and_spawns_one_fetch_per_host() {
        let mut app = App::new(vec!["local".to_string()]);
        let mut inventory = HostInventory::from_config(&Config::default());
        app.pending_account_fetch = vec![0];
        apply_pending_account_fetches(&mut app, &inventory);
        assert!(app.pending_account_fetch.is_empty());
        // The local host really runs `getent` and reports back through the channel.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.drain_worker_messages();
            if app.account_cache.contains_key(&0) || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(app.account_cache.contains_key(&0), "worker result must arrive and be cached");
        let _ = &mut inventory;
    }

    #[test]
    fn apply_pending_account_fetches_ignores_a_host_index_that_no_longer_exists() {
        let mut app = App::new(vec!["local".to_string()]);
        let inventory = HostInventory::from_config(&Config::default());
        app.pending_account_fetch = vec![9];
        apply_pending_account_fetches(&mut app, &inventory);
        assert!(app.pending_account_fetch.is_empty());
    }
```

(The first test needs `getent` on the machine running the tests; it exists on Linux and FreeBSD. If the existing tests build `Config`/`HostInventory` differently, copy their construction.)

`hints.rs` `mod tests`:

```rust
    fn picker_with(load: crate::ui::picker::PickerLoad) -> PickerState {
        PickerState {
            values: vec![],
            filter: String::new(),
            selected: 0,
            checked: Default::default(),
            multi_select: false,
            load,
            note: None,
            account: None,
        }
    }

    #[test]
    fn a_loading_or_failed_picker_advertises_only_esc() {
        for load in [crate::ui::picker::PickerLoad::Loading, crate::ui::picker::PickerLoad::Failed("x".into())] {
            let mut a = base();
            a.picker = Some(picker_with(load));
            assert_eq!(keys(&a), vec!["Esc"]);
        }
    }

    #[test]
    fn f2_is_advertised_on_the_four_live_account_fields_and_actually_opens_a_picker() {
        let forms: Vec<(FormKind, usize)> = vec![
            (FormKind::ShareSmb { host: 0, dataset: "d".into(), path: "/d".into(), existing_user: None }, 1),
            (FormKind::ShareNfs { host: 0, dataset: "d".into() }, 3),
            (FormKind::GrantPermission { host: 0, dataset: "d".into() }, 0),
            (FormKind::AddAclEntry { host: 0, path: "/d".into(), position: 0 }, 0),
        ];
        for (kind, idx) in forms {
            let n = match &kind {
                FormKind::ShareSmb { .. } => 5,
                FormKind::ShareNfs { .. } => 4,
                FormKind::GrantPermission { .. } => 3,
                _ => 4,
            };
            let fields = (0..n).map(|i| FormField { label: format!("f{i}"), value: String::new() }).collect();
            let mut a = base();
            a.modal = Some(Modal::Form { title: "t".into(), fields, focused: idx, description: None, kind: kind.clone() });
            assert!(keys(&a).contains(&"F2"), "{kind:?} field {idx}");
            a.handle_modal_key(KeyCode::F(2));
            assert!(a.picker.is_some(), "{kind:?} field {idx}: F2 must open a picker");
        }
    }
```

- [ ] **Step 2: Run** `cargo test ui::` — Expected: compile FAIL / FAIL.

- [ ] **Step 3: Implement.**

`ui/mod.rs`:

```rust
/// Drains `app.pending_account_fetch` and starts one worker per host.
/// `App` cannot resolve a host index to a runner, so the event loop does
/// it (same split as `apply_pending_new_hosts`). An index with no host
/// (e.g. the inventory changed) is dropped.
fn apply_pending_account_fetches(app: &mut App, inventory: &HostInventory) {
    for host in std::mem::take(&mut app.pending_account_fetch) {
        if let Some(h) = inventory.hosts.get(host) {
            app.fetch_accounts(h.runner.clone(), host);
        }
    }
}
```

and in `event_loop`, directly after `apply_pending_new_hosts(app, inventory, config_path);` add `apply_pending_account_fetches(app, inventory);`. (The queue is filled by a key press and drained at the top of the next loop turn; confirm there is no code path that processes a key and then blocks before the next `drain_worker_messages` iteration — the loop already polls with a timeout.)

`hints.rs` `picker_hints`:

```rust
fn picker_hints(picker: &PickerState) -> Vec<Hint> {
    if picker.load != crate::ui::picker::PickerLoad::Ready {
        return vec![h("Esc", "Close", 0)];
    }
    // ... existing body unchanged
```

`layout.rs` `draw_picker`: move `PICKER_WIDTH_PERCENT` above the filtering code if needed, and at the very start of the function add:

```rust
    if picker.load != crate::ui::picker::PickerLoad::Ready {
        let popup_area = centered_rect(PICKER_WIDTH_PERCENT, 7, area);
        f.render_widget(Clear, popup_area);
        let (title, body) = match &picker.load {
            crate::ui::picker::PickerLoad::Loading => ("Pick a value", "Loading accounts…\n\nEsc to close".to_string()),
            crate::ui::picker::PickerLoad::Failed(msg) => ("Pick a value", format!("Could not load accounts: {msg}\n\nEsc to close")),
            crate::ui::picker::PickerLoad::Ready => unreachable!(),
        };
        f.render_widget(
            Paragraph::new(body).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)),
            popup_area,
        );
        return;
    }
```

and in the `Ready` title construction append the note when present: `if let Some(note) = &picker.note { title = format!("{title} — {note}"); }` (make `title` `let mut`).

`README.md`: add one short paragraph under the F2/picker description: for the four account fields F2 lists the target host's real users/groups (real accounts first), fetched on first use, cached 60 s, over SSH **without sudo** — no sudoers change needed; if the lookup fails the popup says so and typing still works.

- [ ] **Step 4: Run** `cargo test` and `cargo clippy --all-targets` — Expected: all PASS, no new warnings.

- [ ] **Step 5: Commit**

```bash
git add -A src README.md && git commit -m "feat: wire live account fetch into the event loop, picker states and hints

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Live verification (controller + user; not delegated)

- [ ] **Step 1:** `cargo test && cargo clippy --all-targets && cargo build --release`; deploy to the hub with `scp -qr src Cargo.toml Cargo.lock README.md fb@172.16.10.90:zfsctl/` and run `cargo test && cargo build --release` there.
- [ ] **Step 2 (user, from the hub as root against `fbsd16t1` `.61` / `fbsd16t2` `.62`):** F2 on each of the four fields lists that VM's real users first (e.g. `fbsd`), then system accounts; groups appear in the two Recipient pickers; picking a value fills the field and the form still validates it.
- [ ] **Step 3:** Take a VM down (or point a host at an unreachable address): F2 shows `Could not load accounts: …`, `Esc` closes it, the field is intact, and F2 again retries.
- [ ] **Step 4:** Confirm no sudoers change was needed (`ssh fbsd@<vm> getent passwd` works without sudo; the entry file `/usr/local/etc/sudoers.d/zfsctl-smb` and the older entry are unchanged).
- [ ] **Step 5:** Record any discrepancy as a failing test first, then fix.

---

## Self-Review (done while writing)

**Spec coverage**
- Four fields + value shapes → Task 3 (`account_field_for`, `build_values`), Task 4 (hint/F2 test).
- Unprivileged run mode, no sudoers change → Task 1; verified Task 5 step 4.
- Parse/order/real-id range/compat lines/control chars → Task 2; unsafe names → Task 3 `build_values`.
- Lazy fetch on first F2, 60 s cache, no prefetch, failures uncached, 30 s stuck expiry, dedup of in-flight → Task 3.
- Stale-host and no-picker results → Task 3 tests.
- Loading/Failed rendering, Esc-only, groups note, hint bar → Tasks 3, 4.
- Selecting replaces text; typing unaffected; validation unchanged → Task 3 tests.
- Large lists → Task 3 5000-entry test.
- README/docs → Task 4. Live verification → Task 5.

**Deviations from the spec (flag at handoff)**
1. The spec's `picker_source_for`/`PickerSource` enum is realised as the smaller `account_field_for` + the existing `vocabulary_for`, plus fields on `PickerState`; same behaviour, less churn.
2. `build_picker_state` stays pure and returns a `Loading` skeleton for account fields; the `App` F2 handler fills it from the cache or queues the fetch. This keeps the hint bar's `build_picker_state(..).is_some()` check valid without giving `hints.rs` access to the cache.
3. Names are additionally filtered through `smb::validate_user` before being offered (spec Review focus: no selectable-then-rejected names). This hides accounts with unusual names (e.g. `DOMAIN\user`, names with `$`).
4. The picker `Esc`-closes while `Loading`; a fetch started by that F2 still completes and is cached.

**Placeholder scan:** none; the one adaptive instruction (use the existing F2-press mechanism in `press_f2`, and existing `HostInventory`/`Config` construction in the Task 4 tests) names where to look.

**Type consistency:** `AccountField`, `PickerLoad`, `PickerState.{load,note,account}`, `HostAccounts`, `Account`, `WorkerMsg::Accounts(usize, Result<HostAccounts>)`, `App.{account_cache,account_fetch_in_flight,pending_account_fetch}`, `run_unprivileged`, `fetch_accounts` (zfs) and `App::fetch_accounts` (ui) are named identically wherever used.
