# Form Field Picker (F2) Implementation Design

## Summary

Adds an `F2`-triggered picker popup for form fields whose valid values come from a known, fixed vocabulary — `zfs allow`'s `Permissions` field, and the ACL form's `Permissions`, `Flags`, and `Type` fields. Lets a user browse/filter/select valid values instead of having to remember and type exact names by hand (the trigger for this feature: a live test session where `user:fbsd` and the right `zfs allow` permission names weren't obvious without checking documentation).

## Goals

- `F2` on a field with an attached vocabulary opens a picker overlay on top of the still-open form.
- Typing filters the vocabulary by substring match.
- `Up`/`Down` move the highlight through the filtered list.
- Multi-select fields (`Permissions`, `Flags`): `Space` toggles a value on/off; `Enter` joins every checked value with commas and writes the result into the field, then closes the picker.
- Single-select fields (ACL `Type`): `Enter` on the highlighted row writes just that value into the field, then closes the picker.
- `Esc` cancels the picker without touching the field.
- Opening the picker on a field that already has text pre-checks/pre-highlights whichever known values already appear in it, so `F2` on a half-typed field doesn't discard what's there.
- `F2` on a field with no attached vocabulary (`Name`, `Recipient`, `SSH target`, `Scope`, etc.) is a no-op — same as any other key that doesn't apply to a given field today.

## Non-goals

- **Live-fetched values** (e.g. real usernames from the target host, TrueNAS-style). Explicitly deferred: it needs an async SSH round-trip, caching, and a failure path — real complexity on its own, not a free add-on to the static picker. A future pass, not this one.
- **Scope field** (`local`/`descendent`/`both`) and any other already-validated-but-not-requested field. Scoped to exactly four fields (see Architecture).
- **Property-name picking** for the generic pool/dataset property editor. Out of scope for this pass.

## Architecture

### Current state (confirmed by reading the code)

- `App.modal: Option<Modal>` is the only modal-like state today. `Modal::Form { title, fields: Vec<FormField>, focused: usize, description: Option<String>, kind: FormKind }` is handled by `handle_modal_key`, whose `Modal::Form` arm matches on `key`: `Esc` cancels, `Tab` advances focus, `Enter` on the last field submits (via `build_pending_action`), `Backspace`/`Char(c)` edit the focused field's `value`, and a wildcard `_` arm re-inserts the form unchanged and returns `None` — meaning `KeyCode::F(2)` is already a silent no-op within an open form today. No collision to worry about.
- The five forms this feature touches, with their exact field order (confirmed by reading each `Modal::Form` construction site in `src/ui/state.rs`):
  - `FormKind::GrantPermission`: `[Recipient(0), Scope(1), Permissions(2)]`
  - `FormKind::GrantPermissionSet`: `[Set name(0), Permissions(1)]`
  - `FormKind::GrantCreateTimePermission`: `[Permissions(0)]`
  - `FormKind::RevokePermissions`: `[Permissions to remove(0)]`
  - `FormKind::AddAclEntry`: `[Recipient(0), Permissions(1), Flags(2), Type(3)]`

### What changes

1. **New `App` field**: `pub picker: Option<PickerState>`, separate from `modal` — the form stays open underneath; the picker renders on top of it. `None` in `App::new`.
2. **New type** (in `src/ui/state.rs` or a new small module — the plan decides):
   ```rust
   pub struct PickerState {
       pub values: Vec<String>,          // the field's full, unfiltered vocabulary
       pub filter: String,               // typed since the picker opened
       pub selected: usize,              // index into the FILTERED list
       pub checked: std::collections::HashSet<String>, // multi-select only
       pub multi_select: bool,
   }
   ```
3. **Vocabulary lookup**: a pure function mapping `(kind: &FormKind, field_index: usize)` to `Option<(&'static [&'static str], bool)>` (vocabulary, `multi_select`) — `None` for every field not in scope. Four constants back it:
   - `ZFS_ALLOW_PERMISSIONS`: the full, live-confirmed list from `zfs allow`'s own usage output on the real FreeBSD box (see Vocabularies below) — attached to `GrantPermission` field 2, `GrantPermissionSet` field 1, `GrantCreateTimePermission` field 0, `RevokePermissions` field 0. Multi-select.
   - `ACL_PERMISSIONS`: the 14 NFSv4 ACE permission names — attached to `AddAclEntry` field 1. Multi-select.
   - `ACL_FLAGS`: `file_inherit`, `dir_inherit`, `inherit_only`, `no_propagate` — attached to `AddAclEntry` field 2. Multi-select. (`inherited` deliberately excluded — it's a system-applied marker for an entry inherited *from* a parent, not something meaningful to manually set when creating a new entry.)
   - `ACL_TYPES`: `allow`, `deny` — attached to `AddAclEntry` field 3. Single-select.
4. **`handle_modal_key`'s `Modal::Form` arm** gains a `KeyCode::F(2)` case: looks up the vocabulary for `(kind, focused)`; if `None`, no-op (form stays open as-is, matching the wildcard arm's existing behavior); if `Some((values, multi_select))`, builds a `PickerState` — pre-seeded (see Pre-seeding below) — and sets `self.picker = Some(...)`. The form itself (`self.modal`) is untouched.
5. **New `App::handle_picker_key(&mut self, key: KeyCode)`**: handles keys while `self.picker.is_some()`. `Char(c)`/`Backspace` edit `filter` (and reset `selected` to `0` so the highlight stays valid against the newly-filtered list); `Up`/`Down` move `selected` within the filtered list (clamped, no wraparound — consistent with every other list-navigation in this app); `Space` toggles the highlighted value in `checked` (no-op if `!multi_select`); `Enter` confirms — writes the joined (`multi_select`) or single (`!multi_select`) value into `self.modal`'s current `Modal::Form`'s `fields[focused].value` (pattern-matching into `self.modal` to reach it), then clears `self.picker`; `Esc` clears `self.picker` without touching the form.
6. **`src/ui/mod.rs`'s event loop**: the existing `if app.modal.is_some() { ... } else { app.handle_key(...) }` dispatch gains a new, higher-priority branch: `if app.picker.is_some() { app.handle_picker_key(key.code); } else if app.modal.is_some() { ... } else { ... }` — the picker, being layered on top of the form, must intercept keys before the form's own `handle_modal_key` gets a turn.
7. **`src/ui/layout.rs`**: a new `draw_picker` function, called after (so visually on top of) `draw_modal` whenever `app.picker.is_some()` — a centered popup listing the filtered, checked/highlighted values, using the same popup-sizing convention `draw_modal` already established (content-driven height, not a flat percentage).

### Pre-seeding

Opening the picker reads the field's *current* `value` (whatever's already typed) before building `PickerState`:
- Multi-select: split on commas, trim each piece, and pre-check any that exactly match a vocabulary entry. Unrecognized pieces (typos, or values the vocabulary doesn't cover) are simply not pre-checked — they stay in the field untouched if the picker is cancelled, and are *replaced* (not merged in verbatim) if confirmed, since the confirmed value is always rebuilt from `checked` alone. This is a deliberate simplification: a typo'd value a user already carefully typed before reaching for the picker would be silently discarded if they then confirm the picker — flagged in Review Focus below as worth a second look, but accepted for this pass since the alternative (merging unrecognized free-text with picker-confirmed values) adds real complexity for a corner case.
- Single-select: if the current value exactly matches a vocabulary entry, pre-highlight it (`selected` points at that entry's index in the unfiltered list); otherwise `selected = 0`.

## Vocabularies

### `ZFS_ALLOW_PERMISSIONS` (97 entries, confirmed live via `zfs allow` on `fb@172.16.10.90`, 2026-10-04)

Subcommands: `allow`, `clone`, `create`, `destroy`, `diff`, `hold`, `mount`, `promote`, `receive`, `release`, `rename`, `rollback`, `send`, `send:raw`, `share`, `snapshot`, `bookmark`, `load-key`, `change-key`.

"Other" (quota/user-property related): `groupquota`, `groupused`, `userprop`, `userquota`, `userused`, `userobjquota`, `userobjused`, `groupobjquota`, `groupobjused`, `projectused`, `projectquota`, `projectobjused`, `projectobjquota`.

Properties: `aclinherit`, `aclmode`, `acltype`, `atime`, `canmount`, `casesensitivity`, `checksum`, `compression`, `context`, `copies`, `dedup`, `defaultgroupobjquota`, `defaultgroupquota`, `defaultprojectobjquota`, `defaultprojectquota`, `defaultuserobjquota`, `defaultuserquota`, `defcontext`, `devices`, `direct`, `dnodesize`, `encryption`, `exec`, `filesystem_limit`, `fscontext`, `jailed`, `keyformat`, `keylocation`, `logbias`, `longname`, `mlslabel`, `mountpoint`, `nbmand`, `normalization`, `overlay`, `pbkdf2iters`, `prefetch`, `primarycache`, `quota`, `readonly`, `recordsize`, `redundant_metadata`, `refquota`, `refreservation`, `relatime`, `reservation`, `rootcontext`, `secondarycache`, `setuid`, `sharenfs`, `sharesmb`, `snapdev`, `snapdir`, `snapshot_limit`, `special_small_blocks`, `sync`, `utf8only`, `version`, `volblocksize`, `volmode`, `volsize`, `volthreading`, `vscan`, `xattr`.

### `ACL_PERMISSIONS` (14 entries, matches the NFSv4 ACE permission vocabulary already confirmed live against real `getfacl -v` output during the File ACLs feature)

`read_data`, `write_data`, `append_data`, `read_xattr`, `write_xattr`, `execute`, `delete_child`, `read_attributes`, `write_attributes`, `delete`, `read_acl`, `write_acl`, `write_owner`, `synchronize`.

### `ACL_FLAGS` (4 entries)

`file_inherit`, `dir_inherit`, `inherit_only`, `no_propagate`.

### `ACL_TYPES` (2 entries, single-select)

`allow`, `deny`.

## Testing

- Pure, no-SSH-needed unit tests in `src/ui/state.rs` (same style as every other `handle_modal_key`/`handle_key` test in this file):
  - `F2` on a field with an attached vocabulary opens `self.picker` with the right values and `multi_select` flag, for at least one multi-select case (`GrantPermission`'s Permissions field) and the one single-select case (`AddAclEntry`'s Type field).
  - `F2` on a field with no attached vocabulary (e.g. `GrantPermission`'s Recipient field) is a no-op — `self.picker` stays `None`.
  - Typing in an open picker filters `values` by substring (case-insensitive) and resets `selected` to `0`.
  - `Space` toggles a value in `checked` for a multi-select picker; no-op for a single-select one.
  - `Enter` on a multi-select picker joins every checked value with commas into the underlying form's focused field, and clears `self.picker`, leaving `self.modal` otherwise untouched (still a `Modal::Form`, same `kind`, same other fields).
  - `Enter` on the single-select `Type` picker writes just the highlighted value.
  - `Esc` clears `self.picker` without changing the form field's value.
  - Pre-seeding: opening the picker on a `Permissions` field already containing `"create,destroy"` pre-checks exactly those two entries; opening it on an `AddAclEntry` Type field already containing `"deny"` pre-highlights `deny`.
- No rendering tests for `draw_picker` — matches this project's existing, already-established convention (no `draw_*` function anywhere in the codebase has a dedicated rendering test).

## Review focus

- The pre-seeding simplification named above: an unrecognized (typo'd, or vocabulary-uncovered) value already typed into a multi-select field is silently dropped if the picker is then confirmed, since the field's new value is rebuilt entirely from `checked`. A reasonable person who typed a correct-but-obscure value, then opened the picker just to add one more, might be surprised to find their first value gone.
- `Up`/`Down` at the top/bottom of a *filtered* (not full) list — confirm the clamp uses the filtered list's length, not the full vocabulary's, or it could appear to do nothing (stuck at an index past the filtered list's end) or panic on an out-of-bounds access.
- Typing a filter that matches zero entries — confirm the picker shows an empty (not a panicking or stale) list, and that `Enter`/`Space` on an empty filtered list are safe no-ops.
- Switching `focused` fields *while* a picker from a previous field is still conceptually "attached" — confirmed not reachable: the event loop routes all keys to the picker first whenever `self.picker.is_some()`, so `Tab` (which changes `focused` on the form) can never reach the form while a picker is open; the picker must be closed (`Enter`/`Esc`) first, which is exactly what's intended.
- The 14-entry `ACL_PERMISSIONS` list vs. the 97-entry `ZFS_ALLOW_PERMISSIONS` list are genuinely different vocabularies attached to fields that are both just labeled "Permissions" in their forms — confirm the vocabulary-lookup function keys on `(kind, field_index)`, never on the field's *label* text, so the two are never accidentally conflated.
