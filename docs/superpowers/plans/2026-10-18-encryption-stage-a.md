# Encryption, stage A (visibility + job guard) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (native execution). Checkbox steps.

**Goal:** zfsctl shows which datasets are encrypted and locked, tells the Dashboard when a locked dataset is in use, and keeps jobs from sending an encrypted dataset in the clear.

**Architecture:** A pure module `zfs::encryption` parses one `zfs get` of `encryption,keyformat,keystatus,encryptionroot` into `Enc` records (only encrypted datasets are kept). The Datasets refresh sends them as a second worker message (`WorkerMsg::Encryption`) so `list_datasets` and the 101 `Dataset` literals stay as they are; the Lock column, the panel (via `DatasetSpace.enc`), the save-time job guard and the Dashboard read them. The storage check stores `locked` datasets (+ whether shared) and `locked_problems(file, jobs)` yields the Dashboard / report line. The daemon's four job runners call `guard::check_source` right after taking their lock.

**Tech Stack:** Rust; `zfs get`; no new dependencies in stage A (`zeroize` arrives with stage B).

**Spec:** `docs/superpowers/specs/2026-10-18-encryption-design.md` (stage A, "Jobs and replication", decisions).

## Global Constraints

- TDD; Rust only; core sudoers (`zfs get` is already allowed); three OSes; reads only in this stage.
- Verified 2026-10-18 on hub, omnios, ub24: `keystatus` is `available`/`unavailable`/`-`; `encryption` is `off` or the cipher (`aes-256-gcm`); `keyformat` is `passphrase|raw|hex|none`; `encryptionroot` is a dataset name or `-`.
- Lock column text: blank = not encrypted, `open` = key loaded, `LOCKED` (red) = key unavailable. Shown only when some dataset is encrypted and there is room (like Quota).
- Panel line (only when encrypted): `Encrypt aes-256-gcm   Key passphrase   Root tank/secure` and ` LOCKED` appended when locked.
- Job guard: host-to-host replication, replication to a file target and rclone mirror REFUSE an encrypted source (open or locked): `<ds> is encrypted: sending it without a raw option would deliver it decrypted, so this job kind does not run on encrypted datasets`. Restic backup runs when the key is loaded and FAILS when locked: `<ds> is locked (key not loaded): load the key first`. A query the host cannot answer because the connection dropped (`Err` / status 255) fails the run with `could not check whether <ds> is encrypted: <reason>`; any other error answer (old `zfs` without the properties) means "not encrypted".
- Save-time guard: `submit_job_form` for replication and mirror jobs rejects a dataset that the loaded Datasets list of that host marks encrypted (same message). Nothing is refused when the list is not loaded (the run-time guard still holds).
- Dashboard: Watch line per host `tank/secure is locked (a job uses it)` / `(shared)`; several: first two names + `(+N more)`; goes to Datasets. A job "uses" a locked dataset when it is the job's `dataset` on the job's host, or the job's `dest_dataset` on its `dest_host`, or (job `recursive`) a descendant of it. A dataset is "shared" when its `sharenfs` or `sharesmb` is neither `off` nor `-`. Disabled jobs do not count. Daily report carries the same lines.

## Review Focus

- A pool called `encryption` or a dataset named `keystatus`; output lines with 2 or 4 fields; blank output; `-` everywhere (old zfs).
- Locked child of a locked root (both listed); a job on a sibling with a common name prefix (`tank/sec` vs `tank/secure`) must not match.
- `recursive` job on the parent of a locked dataset; disabled job; job on another host with the same dataset name.
- Connection drop vs old-zfs error in the run-time guard; guard must run before any snapshot listing / clone / send.
- Storage check keeps the last `locked` reading when the connection drops (like `integrity_stale`).
- Panel for a volume; Lock column on a narrow terminal.

---

### Task 1: `zfs::encryption`

**Files:** Create `src/zfs/encryption.rs`; modify `src/zfs/mod.rs`.

**Interfaces — Produces:**
- `pub struct Enc { pub cipher: String, pub key_format: String, pub root: String, pub locked: bool }` (derive Debug, Clone, PartialEq, Eq, Default).
- `pub const GET_ARGS: [&str; 7]` = `get -H -t filesystem,volume -o name,property,value encryption,keyformat,keystatus,encryptionroot`; the single-dataset form appends the name (use a `fn dataset_args(name) -> Vec<String>` with `--`-free form because `zfs get` takes the name last; names starting with `-` are rejected up front by `Err`).
- `pub fn parse(output: &str) -> BTreeMap<String, Enc>`: only datasets whose `encryption` is neither `off` nor `-`; unknown/short lines ignored.
- `pub fn list(runner) -> BTreeMap<String, Enc>` best effort (any failure: empty).
- `pub fn query(runner, dataset: &str) -> std::result::Result<Option<Enc>, String>`: Err only for `Err(_)`/status 255 (message `first_line`), Ok(None) for an error answer or an unencrypted dataset.
- `pub fn lock_text(e: Option<&Enc>) -> &'static str`: `""`, `"open"`, `"LOCKED"`.

- [ ] Tests: `parse` of a real capture (3 encrypted incl. a locked child; `off` and `-` rows skipped; dataset literally named `keystatus`); `query` Err on runner error and on status 255, Ok(None) on exit 1; `lock_text`.
- [ ] Implement; run `cargo test zfs::encryption`; commit.

### Task 2: Lock column, panel line, refresh message

**Files:** `src/ui/state.rs` (field `encryption: BTreeMap<String, Enc>`, `WorkerMsg::Encryption(usize, BTreeMap<..>)`, `refresh_datasets` sends it before `Datasets`), `src/ui/state/worker.rs` (`on_encryption`: selected host only), `src/ui/layout.rs` (`draw_datasets` Lock column, width `LOCK_COL_W = 6`, only when some dataset is encrypted and the pane has room, after Quota and before Kind), `src/zfs/metrics.rs` (`DatasetSpace.enc: Option<Enc>`, `fetch_dataset_space` makes a best-effort second `zfs get` through `encryption::query`), `src/ui/metrics.rs` (`dataset_lines` extra line).

- [ ] Tests: layout test with 3 datasets (open, LOCKED, plain) at width 120 shows `Lock`, `open`, `LOCKED`; none encrypted: no `Lock`; narrow width drops Lock while Kind stays; `dataset_lines` adds exactly one line for an encrypted dataset and none otherwise (`Encrypt aes-256-gcm   Key passphrase   Root tank/secure`, locked adds `LOCKED`); `on_encryption` for another host is ignored; `refresh_datasets` with a FakeRunner produces the map.
- [ ] Implement; full `cargo test`; commit.

### Task 3: Run-time job guard

**Files:** Create `src/daemon/guard.rs` (+ `mod guard;` in `src/daemon/mod.rs`); modify `replicate.rs`, `replicate_file.rs`, `mirror.rs`, `backup.rs`.

**Interfaces — Produces:** `pub enum Need { NotEncrypted, KeyLoaded }`; `pub fn check_source(runner: &dyn CommandRunner, dataset: &str, need: Need) -> std::result::Result<(), String>` with the messages in Global Constraints.

- [ ] Tests (guard): encrypted+open with `NotEncrypted` -> Err(contains "is encrypted"); locked with `KeyLoaded` -> Err("is locked"); open with `KeyLoaded` -> Ok; plain -> Ok; runner error -> Err("could not check"); old-zfs error answer -> Ok.
- [ ] Tests (runners): for each of the four runs a source that `zfs get` reports encrypted ends `Failed` with the message, and the FakeRunner saw no `send`/`clone`/`snapshot` call (replicate: also no `list` of snapshots); a plain source still runs as before (existing tests stay green).
- [ ] Implement: in each runner call it right after the lock is taken, `failed(msg)` on Err; commit.

### Task 4: Save-time guard

**Files:** `src/ui/state/modal_keys.rs` (`submit_job_form`).

- [ ] Tests: replication form for `tank/secure` with `app.encryption` containing it -> modal stays a form with the refusal; same for mirror; backup form saves (goes to its confirm); with an empty `encryption` map the replication saves as before.
- [ ] Implement: after `built` is `Ok(action)` and before the prune/confirm branch, if the job kind is replicate (host target or file) or mirror and `self.encryption` holds `job.dataset`, `self.reject(form, msg)`; commit.

### Task 5: Storage check `locked` + Dashboard + report

**Files:** `src/daemon/storage_check.rs`, `src/ui/dashboard.rs`, `src/daemon/health_text.rs`.

**Interfaces — Produces:** `pub struct LockedDataset { pub name: String, pub shared: bool }` (serde, `#[serde(default)]`); `StorageResult.locked: Vec<LockedDataset>` (skip if empty, kept on a dropped connection via `integrity_stale`-style flag — reuse `integrity_stale`); `pub fn parse_locked(output: &str) -> Vec<LockedDataset>` from `zfs get -H -t filesystem -o name,property,value keystatus,sharenfs,sharesmb`; `pub fn locked_problems(file: &StorageFile, jobs: &[crate::jobs::Job]) -> Vec<StorageProblem>`.

- [ ] Tests: `parse_locked` (locked + shared nfs; locked unshared; open skipped; `sharesmb` `-`); `locked_problems` rules from the constraints incl. prefix trap, recursive, disabled job, other host, no job and not shared -> nothing; `apply` keeps `locked` when the connection dropped; Dashboard item text and `goto Datasets`; `storage_lines` carries it (signature gains `jobs: &[Job]`, caller `report_message` already has them).
- [ ] Implement; full `cargo test`; commit.

### Task 6: Docs

- [ ] Book ch. 09 (Datasets: Lock column, panel line), ch. 14 (Jobs: encrypted sources refused, restic exception), ch. 18 (Safety), ch. 11 (Dashboard line), README bullet; book drift test green; plan notes; commit.
