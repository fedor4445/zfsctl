# Replication target retention — Design

Date: 2026-10-16

## Goal

A host-to-host replication job keeps only the newest N of its own snapshots on the destination dataset. Today `keep` prunes the sender only; the receiver accumulates every received snapshot (found in the 2026-10-11 test week: ~129 `@zt-*` snapshots on `tpool1/zt-recv/carol`, growing hourly).

## Decisions made while brainstorming

1. **Host targets only.** File-target jobs (`dest_host = "file"`) are untouched: stream files form chains, and deleting one breaks restoring the later ones. Pruning them is a separate design.
2. **Preview, then confirm.** Saving a setting that would destroy snapshots shows the count and names and needs a second-keystroke confirm. After that, runs prune automatically.
3. **Portable commands only** (`zfs list`, `zfs destroy`), and the new code validates dataset names instead of putting `--` before them (OmniOS `zfs`/`zpool` treat `--` as a dataset operand; see the multi-OS notes).
4. **Data safety first:** every rule below errs toward not destroying.

## The setting

- New optional job field `dest_keep` (`jobs.toml`: absent = keep everything, which is today's behaviour; minimum 1).
- Valid only on host-target replication jobs; a file-target replication, snapshot, scrub or backup job with `dest_keep` is rejected at validation.
- An existing `jobs.toml` without the field loads unchanged.
- The replication form gets a "Keep on target" field; blank keeps everything, and the hint says so.

## What gets pruned (safety rules)

After a replication run, a snapshot on the destination dataset is destroyed only if ALL hold:

1. Its name is one of the job's own: exact `<prefix>-<UTC timestamp>` form (the existing `is_own` check). Snapshots made by hand, or that merely look similar, are never touched.
2. It is not among the newest N own snapshots, ordered by `createtxg` (never by name).
3. It is not the last-replicated snapshot (the base of the next incremental). Because N is at least 1, the newest own snapshot is always kept, which covers this.
4. It has no hold and no dependent clone. A destroy refused for either reason leaves the snapshot alone and counts it as "kept", not as an error (the snapshot job already does the same for clones).

Nothing is pruned when: the transfer failed, was cancelled, or was resumed; the destination listing fails; the dataset has no more than N own snapshots; or any listed line cannot be parsed (the whole selection fails rather than guess).

Scope is the destination dataset's own snapshots only (depth 1). No `-r`, no wildcards, no child datasets: a replication job sends one dataset, and the prune looks where the planner already looks.

## Daemon behaviour

- The prune is a final step of `run_replicate` (`src/daemon/replicate.rs`), under the destination lock the run already holds, so two runs never prune the same dataset at once.
- It runs after a successful send **and** after an "up to date" skip, so a target that already piled up snapshots is cleaned on the next slot instead of waiting for a new send.
- It lists the destination with the existing `name,guid,createtxg` listing, selects with a pure function (`prune_selection`: listing, prefix, N, returns names to destroy, oldest first), then runs one `zfs destroy <dataset>@<snap>` per snapshot over the destination's existing runner (sudo-wrapped SSH). Hold/clone refusals are kept; any other destroy failure marks the run failed, the message still saying the transfer succeeded, and the remaining snapshots are still tried (same precedent as the snapshot job).
- The history message gains a suffix such as `; pruned 12, kept 6 on t2`.
- Cancelled, failed and resumed runs return before the prune step, as they do before the final result today.

## Form and confirm

- On save of a host-target replication job with `dest_keep` set, the form asks the destination (in the worker, not on the UI thread) for its snapshot list and computes the same selection the daemon would.
- If the selection is empty (including raising N, or blank): saves without a confirm.
- If not empty (a new job, or lowering N): a confirm modal says "This will destroy N snapshots on `<host>` `<dataset>`: oldest `<name>` … newest destroyed `<name>`; N kept" and needs the second keystroke, the same pattern as the other irreversible-form confirms. Declining returns to the form with its values intact.
- If the listing fails or cannot be parsed, the save is refused with the reason; nothing is guessed.
- The daemon re-selects at run time, so a stale preview can never destroy more than the rules above allow.

## Testing

- `prune_selection`: exact-prefix names only (look-alikes and hand-made snapshots excluded); ordering by `createtxg` when names sort differently; the newest kept at N=1; fewer than N+1 selects nothing; an unparsable line fails the whole selection.
- Daemon with the fake runner: prune after a send and after an up-to-date skip; nothing after a failed, cancelled or resumed transfer; nothing when the listing fails; held or cloned snapshot kept and not an error; another destroy failure gives a failed run while the rest are still pruned; exactly one `zfs destroy dataset@snap` per snapshot, no `-r`; the history suffix.
- Validation: `dest_keep` of 0 rejected; rejected on file-target and non-replication jobs; blank = none; old `jobs.toml` loads.
- Form: raise or blank needs no confirm; lowering shows count and names and needs the second keystroke; a failed listing refuses the save; declining keeps the form values.
- Live (after the build, on the hub with throwaway datasets, never real data; TUI driven by pexpect/pyte, no mutating keys sent to real jobs): scratch replication to t2, a real keep-N prune, the newest and base kept, the next incremental still works after a prune, a held snapshot left alone.

## Docs

README, the replication chapter of the book, the `--help` text if it lists job fields, and the user-manual job-fields reference. The existing test that ties the help text to the book's headings must stay green.

## Out of scope

- File-target stream files; the sender side (`keep` already prunes it); child datasets and recursion.
- `zfs hold` on the last replicated snapshot (a known deferred item, a different problem).
- A manual "prune now" action: pruning happens only through a replication run.
- OS-specific code: this feature adds none. The portability note is dataset-name validation instead of `--` in the new commands.
