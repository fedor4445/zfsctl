# Add Host Implementation Design

## Summary

Lets a user add a new remote host to `zfsctl` from inside the TUI's Hosts section, instead of hand-editing `config.toml` and restarting. The new host is validated for reachability over SSH before it's accepted, becomes usable immediately in the same running session (hot-reloaded into the live host inventory), and is persisted to `config.toml` so it survives a restart too.

This is architecturally distinct from every other feature in `zfsctl` so far: every existing mutation (`zpool scrub`, `zfs allow`, `setfacl`, etc.) runs a command against an *already-selected* host's already-resolved runner. Add Host instead changes `zfsctl`'s own configuration and the live set of hosts it knows about — there is no existing runner to dispatch against, because the host doesn't exist yet.

## Goals

- A `'n'` key in the Hosts section opens a form: `[Name, SSH target]`.
- Local, synchronous validation (blank fields, duplicate name) keeps the form open immediately, same as every other form in the app.
- Once those pass, a background SSH reachability probe (`ssh -o BatchMode=yes -o ConnectTimeout=10 -- <target> true`, reusing the existing `SshRunner` as-is) runs before the host is accepted.
- On success: the host is pushed into the live `HostInventory` (selectable right away, no restart) and persisted to `config.toml` (loaded, appended, saved — reusing the already-defined but currently-unused `Config::save`).
- On failure: the attempt is abandoned and the specific SSH error (`ssh`'s own stderr — "Could not resolve hostname", "Permission denied (publickey)", etc.) is shown as a status message.

## Non-goals

- **SSH key bootstrap.** This feature assumes passwordless SSH to the new host already works (the user sets this up at the VM console first, as already done twice this project). Automating a password-based key push (`ssh-copy-id`-style, with an interactive password prompt) is a separate, more security-sensitive feature and is explicitly out of scope.
- **Removing or editing a host from the TUI.** This pass is add-only. Removing a host is a different, smaller problem better scoped on its own later.
- **Keeping the Add Host form open across the async SSH probe.** See "Form lifecycle" below — the form closes on submit (consistent with every other mutation-triggering form in the app, e.g. scrub/destroy), not after the probe resolves. A failed probe is retried by pressing `'n'` again, not by reopening the same form instance.

## Architecture

### Current state (confirmed by reading the code, not assumed)

- `src/config.rs`: `Config { hosts: Vec<HostConfig> }`, `HostConfig { name: String, ssh_target: Option<String> }`. `Config::load`/`Config::save` already exist; `save` is currently unused anywhere in the codebase.
- `src/hosts.rs`: `HostInventory::from_config` builds `Vec<Host>` (a synthetic `"local"` first, then one `Host` per configured `HostConfig`) exactly once, at startup. `Host { name: String, runner: Arc<dyn CommandRunner> }`.
- `src/main.rs`: loads `Config` from `config::DEFAULT_CONFIG_PATH`, builds `HostInventory::from_config(&config)`, passes only `inventory` (by value) into `ui::run_app`. The config path and the loaded `Config` itself are never threaded any further than this.
- `src/ui/mod.rs`: `run_app(inventory: HostInventory)` constructs `App` from `inventory.hosts`' names, then calls `event_loop(terminal, &mut app, &inventory)` — `inventory` is an **immutable** reference there today; nothing in the loop ever changes which hosts exist.
- `src/exec/ssh.rs`: `SshRunner::run(cmd, args)` always builds `ssh -o BatchMode=yes -o ConnectTimeout=10 -- <host> "<cmd> <args...>"`. Calling `SshRunner::new(target).run("true", &[])` is exactly the lightweight reachability probe this feature needs — no new SSH argument-building code required.

### What changes

1. **`src/main.rs`**: also passes `config_path` into `ui::run_app` (it already constructs the `PathBuf`, just never forwarded it).
2. **`src/ui/mod.rs`**: `run_app(inventory: HostInventory, config_path: PathBuf)`; `event_loop`'s `inventory` parameter becomes `&mut HostInventory`, and it gains `config_path: &Path`. After `app.drain_worker_messages()` each tick, if `app.pending_new_host` (see below) is `Some((name, target))` (taken, i.e. cleared), the loop:
   - Pushes `Host { name: name.clone(), runner: Arc::new(SshRunner::new(target.clone())) }` onto `inventory.hosts`.
   - Loads `Config` from `config_path`, pushes `HostConfig { name, ssh_target: Some(target) }` onto `config.hosts`, saves it back. If the save fails, sets `app.status_message` to a warning that the host is usable for this session only and will not survive a restart (the in-memory `inventory` push already happened and is not rolled back — the host stays live either way, since the whole point of hot-reload is that the user can carry on working immediately).
3. **`src/ui/state.rs`**:
   - New `App` field: `pub pending_new_host: Option<(String, String)>` (name, ssh_target) — the signal described above. `None` in `App::new`.
   - New `WorkerMsg::HostProbe(String, String, Result<()>)` (name, target, probe outcome).
   - New `FormKind::AddHost` (no extra context needed — unlike every other `FormKind`, it carries no `host`/`dataset`/`pool` field, because it isn't about an existing resource).
   - `handle_key`: new `KeyCode::Char('n')` arm gated on `self.current_section() == Section::Hosts` (today the Hosts section has no key bindings of its own beyond `Up`/`Down`), opening a `Modal::Form` with `kind: FormKind::AddHost`.
   - `handle_modal_key`'s `Modal::Form` arm: when `kind` is `FormKind::AddHost`, submitting on the last field does **not** go through `build_pending_action`/return a `PendingAction` like every other `FormKind` — there is no existing host/runner to dispatch against. Instead it:
     - Validates `fields[0].value` (Name) and `fields[1].value` (SSH target) are both non-blank, and that the name doesn't already match (case-insensitively) an entry already in `self.hosts`. On failure, re-inserts the same `Modal::Form` (exactly like every other form's blank/invalid-field handling) and returns `None`.
     - On success: closes the modal (returns to no modal), sets `self.status_message = Some(format!("Connecting to '{target}'..."))`, spawns a thread that runs `SshRunner::new(target.clone()).run("true", &[])` and sends `WorkerMsg::HostProbe(name, target, result.map(|_| ()))`, and returns `None` (there is nothing for `mod.rs` to call `dispatch_mutation` with — the probe is already dispatched here, directly, unlike the `PendingAction` flow).
   - `drain_worker_messages`: new `WorkerMsg::HostProbe(name, target, result)` arm:
     - `Ok(())`: pushes `name.clone()` onto `self.hosts` (so it's visible in the sidebar immediately) and sets `self.pending_new_host = Some((name, target))` for `event_loop` to apply to the live inventory + persist (see above). Clears the "Connecting..." status message.
     - `Err(e)`: sets `self.status_message = Some(format!("Could not add host '{name}': {e}"))`. Nothing is pushed anywhere; the attempt is simply abandoned. The user fixes the typo and presses `'n'` again.

### Form lifecycle (why the form closes before the probe resolves)

Every existing slow operation in this app (`zpool scrub`, `zfs destroy`, etc.) closes its modal immediately on confirm/submit and reports the outcome asynchronously via `status_message` once the worker thread finishes — the modal is never kept open "waiting" for a background thread. Add Host follows the same convention for consistency, even though the earlier in-chat design said the form "stays open" on failure — in practice here that means the same *workflow* (fix input, resubmit) rather than literally the same form instance persisting across the async gap, since no other form in the codebase does that and adding that bookkeeping (a form that's still semantically "open" while unrelated keys might be pressed, re-rendering with a disabled state while a probe is in flight, etc.) is a meaningfully bigger and more fragile change for the same end-user outcome. The *validation* half (blank fields, duplicate name) still keeps the form open immediately, same as always — only the network round-trip itself is async-and-closes.

## Data flow (end to end)

```
'n' in Hosts section
  -> Modal::Form { kind: FormKind::AddHost, fields: [Name, SSH target] }
  -> Enter on last field
     -> blank/duplicate? -> re-show form with error, done
     -> else: close modal, status "Connecting...", spawn probe thread
        -> SshRunner::new(target).run("true", &[])
        -> WorkerMsg::HostProbe(name, target, result)
           -> drain_worker_messages:
              Ok  => self.hosts.push(name); self.pending_new_host = Some((name, target))
              Err => self.status_message = Some("Could not add host ...")
  -> event_loop (outside App, owns `inventory` + `config_path`):
     if app.pending_new_host.take() is Some((name, target)):
       inventory.hosts.push(Host { name, runner: SshRunner::new(target) })
       Config::load(config_path) -> push HostConfig -> Config::save(config_path)
       (save failure => status message warning, host still live for this session)
```

## Testing

- `HostInventory`: no new method needed — `event_loop` pushes directly onto `inventory.hosts` (a public field already), so no new library-layer function to unit test there beyond what `from_config` already covers.
- `src/ui/state.rs` (`FakeRunner`/pure `App` tests, no real SSH needed for any of these):
  - `handle_key_n_on_hosts_section_opens_add_host_form`.
  - `build_pending_action`... *not applicable* — `AddHost` never produces a `PendingAction`; instead, direct `handle_modal_key` tests:
    - Blank Name or blank SSH target keeps the form open (no `WorkerMsg` sent, modal still `Some`).
    - A name colliding case-insensitively with an existing entry in `self.hosts` (including `"local"`) keeps the form open with an error.
    - Valid input closes the modal and sets a "Connecting..." status message (the spawned thread's actual SSH call isn't exercised in this test — `FakeRunner` isn't usable here since the probe always constructs a real `SshRunner`; this test only asserts the pre-spawn behavior: modal closed, status message set).
  - `drain_worker_messages` tests for `WorkerMsg::HostProbe`:
    - `Ok(())` pushes the name onto `self.hosts` and sets `pending_new_host`.
    - `Err(..)` sets a status message and leaves `self.hosts`/`pending_new_host` untouched.
- No test exercises a real SSH probe (would require a reachable test host); this mirrors how the rest of the test suite already never shells out for real — live SSH behavior is verified manually against the real FreeBSD boxes, same as every other feature in this project.

## Review focus (things future review should check deliberately)

- The event loop's `config.hosts` push must not duplicate an entry if `Config::load` is re-read between two rapid Add Host operations and something external also changed `config.toml` in between — acceptable risk for a single-operator TUI, not guarded against, but worth a reviewer's eye.
- A `Config::save` failure must not roll back the already-applied `inventory.hosts.push` — the host must stay usable for the rest of the session even if persistence failed, only a warning should appear.
- The duplicate-name check must be case-insensitive and must include `"local"` specifically (not just whatever's already in `config.toml`), since `self.hosts` already contains `"local"` as its first entry at all times.
- `handle_modal_key`'s `FormKind::AddHost` branch must never return a `PendingAction` — if a future edit accidentally routes it through `build_pending_action`, it would panic or misbehave since there's no real host/dataset/pool context for it to use.
