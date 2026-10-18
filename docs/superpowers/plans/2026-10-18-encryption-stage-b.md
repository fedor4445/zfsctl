# Encryption, stage B (key handling) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Checkbox steps.

**Goal:** load, unload and change the key of an encrypted dataset from the Datasets screen without the passphrase ever reaching an argument, a log, the status line or a state file.

**Architecture:** `zfs::encryption` gains `load_key` / `unload_key` / `change_key` (passphrase through `run_with_stdin`, `--` before the name). Three `MutationCall`s carry the passphrase as `Secret` (now zeroed on drop with the `zeroize` crate). Two masked forms (`FormKind::LoadKey`, `FormKind::ChangeKey`); `L` / `U` / `C` on a Datasets row open them (or the confirm for unload). The target is always the encryption root.

**Spec:** `docs/superpowers/specs/2026-10-18-encryption-design.md` (stage B).

## Global Constraints

- Verified 2026-10-18 on hub (FreeBSD), ub24, omnios: `load-key`/`unload-key`/`change-key` accept `--` before the name and a passphrase on non-TTY stdin (newline optional). Messages: `Key load error: Keys must be loaded for encryption root of 'c' (root).` (a child cannot be loaded; load the root), `Key already loaded for 'x'.`, `Key unload error: 'x' is busy.` (mounted: unload is refused, no unmount is done for the user), `Key change error: Key must be loaded.`, `Key change error: Passphrase too short (min 8).`
- Passphrase keys only in stage B. A raw/hex dataset gets a status message ("load it at the shell, key files come in stage C"). A passphrase with a newline or empty is refused by the form; change-key needs 8+ characters.
- `change-key` only on an encryption root that is open (a child would become a new root); load/unload act on the root named by `Enc.root`.
- Two-key confirms (y then Enter) with the spec's wording for `unload-key` and `change-key`; `load-key` is a plain confirm-free form submit (same as SMB: form then Enter).
- Tests assert the passphrase is only in stdin: not in recorded argv, `Debug` of the call/modals, the status line or errors.

## Review Focus

- Passphrase in Debug output of `MutationCall`/`PendingAction`/Modal after submit; form fields wiped after build.
- Load on a child (must go to the root), already-loaded, wrong key, key on a raw dataset, name starting with `-`.
- change-key on a non-root, on a locked dataset; unload of a mounted dataset (busy) shows the hint.

### Task 1: zeroize Secret + key operations
**Files:** `Cargo.toml` (zeroize), `src/ui/modal.rs` (Secret), `src/zfs/encryption.rs`.
- [ ] Tests: `Secret` drop wipes (zeroize); `check_passphrase`; `load_key`/`change_key` put the passphrase on stdin only, `--` before the name, argv has no passphrase; `unload_key` args; failure text kept verbatim, busy gets a hint.
- [ ] Implement; commit.

### Task 2: MutationCall variants
**Files:** `src/ui/modal.rs`, `src/ui/state.rs` (exhaustive matches).
- [ ] `LoadKey{dataset, passphrase: Secret, mount: bool}`, `UnloadKey{dataset}`, `ChangeKey{dataset, passphrase: Secret}`; tone Neutral/Warn/Warn; labels; `run_on`; Debug redacted test; mount-after note when mount fails. Commit.

### Task 3: forms, keys, confirms
**Files:** `src/ui/modal.rs` (FormKind, masks_field), `src/ui/state/forms.rs`, `modal_keys.rs`, `src/ui/state.rs` (keys), `src/ui/hints.rs`.
- [ ] Keys `L`/`U`/`C` on Datasets rows with the checks above; masked forms (Passphrase, Mount after (y/n); New passphrase + Repeat); `irreversible_prompt` arms for unload and change; fields zeroed after build; hints. Commit.

### Task 4: docs
- [ ] Book 09 + 18, README bullet; drift test green; commit.
