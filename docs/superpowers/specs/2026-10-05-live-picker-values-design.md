# Live-Fetched Account Values for the F2 Picker — Design

## Summary

The `F2` field picker (spec: `2026-10-04-form-field-picker-design.md`) offers only fixed vocabularies today. That spec explicitly deferred "live-fetched values (e.g. real usernames from the target host, TrueNAS-style)" because it needs an async SSH round trip, caching and a failure path. This spec adds exactly that, for one kind of live value: the Unix users and groups that exist on the **managed host** (the host the form will act on, not the workstation running `zfsctl`).

Trigger: a live test session where `user:fbsd` and valid names were not obvious without leaving the TUI to check.

## Goals

- `F2` on any of four field kinds opens a picker populated from the target host's real accounts:

  | Field | Form | Picker offers (single-select) |
  |---|---|---|
  | Valid user | SMB share (`FormKind::ShareSmb`, field 1) | user names |
  | Root user mapping | NFS share (`FormKind::ShareNfs`, field 3) | user names |
  | Recipient | `zfs allow` grant (`FormKind::GrantPermission`, field 0) | `everyone`, then `user:<name>` and `group:<name>` |
  | Recipient | ACL entry (`FormKind::AddAclEntry`, field 0) | `owner`, `group`, `everyone`, then `user:<name>` and `group:<name>` |

  Selecting a value replaces the field's text. Typing a value by hand keeps working exactly as today, in every state including failure.
- One fetch (`getent passwd` + `getent group`) serves all four fields.
- The popup opens immediately; it never blocks typing or the UI thread.
- A failure is shown inside the picker and never leaves the form in a worse state than before F2 was pressed.

## Non-goals

- No account creation, modification or lookup beyond listing names. No home directories, shells or gecos fields are shown or stored.
- No multi-select for these fields (each field takes one value).
- No fallback to the *local* machine's accounts when a remote fetch fails (it would offer names that don't exist on the target).
- No toggle for hiding system accounts (they are listed after real users instead).
- No change to the other, static pickers.
- No prefetching when a form opens.

## Decisions (made with the user, 2026-10-05)

1. All four fields above are in scope.
2. **Unprivileged run mode, no sudoers change.** `getent passwd`/`getent group` need no privilege; they run over SSH *without* `sudo -n`. The sudoers entries on managed hosts do not change.
3. **List everything, real users first.** "Real" means id ≥ 1000 and < 60000 (so `nobody`, uid 65534, sorts with system accounts). Real accounts first, then `root` and the rest, each block sorted alphabetically (case-sensitive byte order).
4. **Fetch on the first F2 press, cache per host for 60 seconds.** No prefetch.
5. **Failures are shown inside the picker** ("Could not load accounts: <reason>"); Esc closes it and the field is untouched; failures are never cached.

## Architecture

### 1. `CommandRunner::run_unprivileged`

New trait method `run_unprivileged(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput>`.

- `SshRunner`: same SSH argument construction as `run` (`-o BatchMode=yes -o ConnectTimeout=10 -- <host> <quoted command>`) but the remote command string has **no `sudo -n` prefix**. Factor `build_args` so both paths share the quoting and option logic and differ only in the prefix.
- `LocalRunner`: identical to `run` (local commands never use sudo).
- `FakeRunner`: records the call in `calls` like `run` (so tests can assert the command), honours `route`/`route_failure`, and additionally records that the call was unprivileged (a `Vec<bool>` or equivalent) so a test can assert no `sudo` semantics were requested.

Only read-only, privilege-free lookups may use it; the doc comment on the trait method says so.

### 2. `src/zfs/accounts.rs` (new)

```rust
pub struct Account { pub name: String, pub id: u32 }
pub struct HostAccounts { pub users: Vec<Account>, pub groups: Vec<Account>, pub group_error: Option<String> }
pub fn fetch_accounts(runner: &dyn CommandRunner) -> Result<HostAccounts>
pub fn parse_passwd(output: &str) -> Vec<Account>
pub fn parse_group(output: &str) -> Vec<Account>
pub fn order(accounts: Vec<Account>) -> Vec<Account>      // real first, then rest; alphabetical within each
```

- `getent passwd` lines are `name:passwd:uid:gid:gecos:home:shell`; `getent group` lines are `name:passwd:gid:members`. A line with too few fields, an empty name, or a non-numeric id is skipped (lenient, like `parse_shares`: one odd line must not hide every account).
- Commands: `getent passwd` and `getent group`, via `run_unprivileged`.
- If the **users** fetch fails (nonzero exit, spawn/SSH failure), `fetch_accounts` returns `Err`. If only the **groups** fetch fails, it returns `Ok` with `group_error: Some(reason)` and `groups` empty — the user still gets the users plus a note.
- A host whose `getent` is missing fails like any other command failure and surfaces its stderr.

### 3. Picker vocabulary becomes static-or-live

`vocabulary_for` currently returns `Option<(&'static [&'static str], bool)>`. Introduce an enum for what a field's picker is sourced from:

```rust
pub enum PickerSource { Static(&'static [&'static str], bool /*multi*/), Accounts(AccountField) }
pub enum AccountField { User, ZfsAllowRecipient, AclRecipient }
```

`vocabulary_for` is replaced by (or wrapped as) `picker_source_for(kind, field_index) -> Option<PickerSource>`; the existing static mappings return `Static`. The four live fields return `Accounts(..)` per the table above.

`PickerState` gains a `load: PickerLoad` field:

```rust
pub enum PickerLoad { Ready, Loading, Failed(String) }
```

Static pickers are always `Ready`. For `Failed`/`Loading`, `values` is empty (plus, for a groups-only failure, the `group_error` note, see below). `build_values(field: AccountField, accounts: &HostAccounts) -> Vec<String>` produces the displayed list:

- `User`: user names (ordered).
- `ZfsAllowRecipient`: `everyone`, then `user:<name>` for each user, then `group:<name>` for each group.
- `AclRecipient`: `owner`, `group`, `everyone`, then `user:<name>`..., then `group:<name>`....

Pre-selection: if the field's current text exactly equals a listed value, that row starts selected (same convention as the existing single-select picker).

### 4. App state, cache, and the fetch round trip

`App` has no way to resolve a host index to a runner (the event loop does that, see `ui/mod.rs`), so the F2 handler follows the same queue pattern as `pending_new_hosts`:

- `App.account_cache: HashMap<usize /*host*/, (Instant, HostAccounts)>`.
- `App.pending_account_fetch: Vec<usize>` (host indexes). The F2 handler pushes the host and sets `picker.load = Loading`; the event loop drains the queue and calls `app.fetch_accounts(host.runner.clone(), host_idx)`, which spawns one worker thread and sends `WorkerMsg::Accounts(host_idx, Result<HostAccounts>)`.
- A second F2 while a fetch for that host is already in flight does not queue another; the picker simply shows `Loading` and receives the same result. (Track `account_fetch_in_flight: HashMap<usize, Instant>`; an entry older than **30 s** is treated as stuck (a stalled SSH command is not bounded by `ConnectTimeout`), so the next F2 queues a fresh fetch, and a late result from the abandoned one is still accepted if it arrives. The marker is removed whenever a result for that host arrives.)
- On F2 for an account field: if the cache has an entry for the form's host younger than **60 s**, open the picker `Ready` immediately; otherwise open it `Loading` and queue a fetch.
- `drain_worker_messages` handles `WorkerMsg::Accounts(host, result)`: on `Ok`, store in `account_cache` with `Instant::now()` and, if a picker is open that is `Loading`, whose form is for `host`, fill it (`values`, `load = Ready`); on `Err`, do **not** cache; if such a picker is open, set `load = Failed(reason)`. A result for a host with no open picker (form closed, picker dismissed) is cached on `Ok` and otherwise ignored. A result whose host differs from the open picker's form host never touches that picker.
- The host a form acts on is the `host` field already carried by every `FormKind` that has one (`ShareSmb`, `ShareNfs`, `GrantPermission`, `AddAclEntry` all carry `host`).
- Switching host (`clear_host_data`) does not need to clear `account_cache`: it is keyed by host index and expires by age.

### 5. Picker keys and rendering

- While `Loading`: the popup shows "Loading accounts…" and no list; only `Esc` is accepted (closes the picker; any later result is still cached). Typed characters are ignored in this state rather than filtering an empty list.
- While `Failed(msg)`: the popup shows "Could not load accounts: <msg>" and "Esc to close"; only `Esc` is accepted. The next `F2` retries (failures are not cached, so there is nothing to invalidate).
- If users loaded but groups failed (`group_error`), the list is shown normally and the popup title carries a one-line note ("groups unavailable: <reason>"); group entries are simply absent.
- `Ready`: behaves exactly like the existing single-select picker (type-to-filter, Up/Down, Enter confirms).
- Confirming writes the selected value into the field (replacing it), as the existing picker does for single-select.
- Hint bar: `picker_hints` gains the `Loading`/`Failed` cases (`Esc Close` only). The existing F2 hint on a form still means "F2 Pick" and must be advertised for the four new fields exactly when pressing it does something (the drift guards in `hints.rs` stay unweakened).

### 6. Security and privilege

- No new sudoers entries; nothing in the README's sudoers section changes. The new method must not be used for anything that mutates state.
- Account names come from the host and are only ever *displayed* and, if chosen, placed into a form field. They go through the same validation the fields already have (`validate_user`, `parse_recipient_field`, `parse_acl_recipient_field`); selecting a picker value never bypasses a form's validation.
- Names are rendered as plain text; a name containing control characters is skipped by the parser (a `getent` line with a control character in the name field is treated as malformed).

## Data flow

```
F2 on SMB "Valid user" (host: fbsd16t1)
  -> picker_source_for(ShareSmb, 1) = Accounts(User)
  -> cache for host fresh (<60s)?  yes -> picker Ready, immediate
                                    no  -> picker Loading; pending_account_fetch.push(host)
  -> event loop: app.fetch_accounts(host.runner, host)  [worker thread]
       ssh fbsd@host getent passwd ; getent group     (no sudo)
  -> WorkerMsg::Accounts(host, Ok/Err)
       Ok  -> cache; if the picker is still open for that host -> fill, Ready
       Err -> if the picker is still open for that host -> Failed(msg); nothing cached
  -> user filters / selects / Enter -> field text replaced; form validation unchanged
```

## Testing

- `parse_passwd`/`parse_group`: well-formed lines; short lines, empty names, non-numeric ids and control characters skipped; gecos containing colons-in-wrong-places don't crash.
- `order`: real (1000–59999) before system; `nobody` (65534) with system; `root` with system; alphabetical within blocks.
- `fetch_accounts` with `FakeRunner`: success; users fail → `Err` naming the stderr; groups fail → `Ok` with `group_error` and empty groups; both commands issued through `run_unprivileged`, never `run`.
- `SshRunner` argument construction: the unprivileged variant has the same options and quoting but **no** `sudo -n`; the privileged variant is unchanged (existing tests stay green).
- `build_values` for the three `AccountField` variants, exact ordering and prefixes.
- Picker/state: F2 on each of the four fields (host carried correctly); fresh cache → `Ready` and no queued fetch; stale/missing cache → `Loading` and exactly one queued fetch; second F2 while in flight queues nothing; `Ok` result fills an open `Loading` picker; `Err` produces `Failed` and is not cached; a result for another host or with no picker open doesn't touch a picker; cache expiry at 60 s (inject the clock or the `Instant`); `Esc` closes in `Loading` and `Failed`; typing in `Loading` is ignored.
- Selecting a value replaces the field text; the form's own validation still rejects a bad value.
- Hint-bar drift guards pass unmodified, with F2 advertised on the four new fields.
- **Live verification** (cannot be unit-tested): on `fbsd16t1`/`fbsd16t2` from the hub, F2 on each of the four fields lists the VM's real users first (`fbsd` etc.) then system accounts; an unreachable host shows the in-picker error and retry works; the sudoers entry on the VMs is unchanged and still sufficient.

## Review focus

- The unprivileged path must be impossible to reach from a mutating call, and must not accidentally drop `sudo` from the privileged `run`.
- Stale results: a slow fetch for host A arriving after the user switched to host B, or after the form was closed and a different one opened, must never fill the wrong picker.
- A fetch that never returns (hung SSH) must not wedge the picker: `Esc` always closes it, and a later F2 starts a fresh fetch (the in-flight marker is cleared on result and expires after 30 s, see Architecture §4; both are tested with an injected clock).
- Large account lists (LDAP-backed hosts can return thousands of entries): the picker must stay responsive when filtering and scrolling.
- `getent` output on hosts with NIS/LDAP can contain `+@netgroup` style compat lines on FreeBSD; they must be skipped, not shown.
- Recipient pickers write `user:<name>`; a user name containing a character the form's own recipient parser rejects must be flagged or skipped rather than selectable-then-rejected.
