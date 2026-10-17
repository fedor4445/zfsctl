# Settings Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Settings sidebar section in the TUI that edits the theme, auto-refresh, Dashboard thresholds and cloud caps of `config.toml`, saved safely and applied at once.

**Architecture:** A pure model (`src/ui/settings.rs`) turns the file into grouped rows and turns an edit form into a list of key changes, validated with the loader's own rules. A new `toml_edit`-based writer (`src/config_edit.rs`) applies those changes to one table, keeping comments and every other section, behind the existing `rewrite_config` safe write. State and key handling follow the Alerts screen (`e`-style form, confirm, `MutationCall`); live apply re-reads the file.

**Tech Stack:** Rust, ratatui, `toml` 0.8 and `toml_edit` 0.22 (already in `Cargo.lock` through `toml`; add it as a direct dependency).

**Spec:** `docs/superpowers/specs/2026-10-17-settings-screen-design.md`

## Global Constraints

- Settings is the last sidebar section, after Alerts. Title text: `Settings`.
- Groups and rows, in this order: Display (Theme, Auto-refresh); Dashboard thresholds (Memory warn / crit, Swap warn / crit, CPU warn, Scrub warn / crit, Scrub ignore); Cloud caps (one row per remote).
- Notify settings and the hook command are NOT in Settings. No daemon restart, no rclone call from Settings, no ZFS properties.
- Validation = the loader's: a warn/crit pair needs `0 < warn < crit <= 100`; CPU warn 1 to 100; auto-refresh `0` (off) or 1 to 3600; theme one of `ansi`, `mono`, `solarized-dark`, `solarized-light`; cap size per `config::parse_cap` (digits, optional decimal point, optional K/M/G/T, not zero); scrub ignore is comma-separated names, not checked against pools.
- A pair at its defaults (memory 90/97, swap 50/90, scrub 35/90) is removed from the file; any other pair writes both keys. An empty value resets a setting by removing its key. An emptied `[ui]`, `[dashboard]`, `[cloud.caps]` or `[cloud]` table is removed.
- Writing goes through `rewrite_config` (backup `config.toml.bak`, private temp file, owner/mode kept, rename). A save is refused, and the file untouched, if the result would not parse as `Config`, would not say what was asked, or would change anything outside the edited table.
- Settings re-reads the file immediately before writing (the writer takes the text it just read).
- Existing code style: comments explain why, no new dependency besides `toml_edit`, clippy clean (`cargo clippy --all-targets`), tests in the same file's `mod tests` or `src/ui/state/tests.rs`.

## Review Focus

- A `config.toml` with comments, `[[hosts]]`, `[notify]` and `[daemon]`: every byte outside the edited table survives (Task 1 test).
- `caps` written as an inline table (`caps = { b2 = "10G" }`) or `[cloud]` with other future keys: edit works or refuses cleanly, never corrupts (Task 1 test).
- A hand-edited file that no longer parses when Settings opens: screen says so, edits nothing (Task 3 test).
- The file changes on disk between opening the form and saving: the save works from the fresh text, or refuses; a hand edit is never overwritten (Task 3 test).
- Resetting the last key of a table leaves no empty header behind; resetting something not in the file is a no-op, not an error (Task 1 test).

---

### Task 1: The table editor

**Files:**
- Create: `src/config_edit.rs` (declare in `src/main.rs`/`lib.rs` next to `config`)
- Modify: `Cargo.toml` (add `toml_edit = "0.22"`), `src/config.rs` (make `rewrite_config` `pub(crate)`)

**Interfaces:**
- Produces:
  - `pub enum Change { Set(Vec<String> /* path of the key, e.g. ["dashboard","memory_warn"] */, toml_edit::Value), Remove(Vec<String>) }` with constructors `Change::set(&[&str], impl Into<toml_edit::Value>)` and `Change::remove(&[&str])`.
  - `pub fn apply(text: &str, changes: &[Change]) -> Result<String, String>`: the new file text, or the reason it was refused.
  - `pub fn save(path: &Path, changes: &[Change]) -> crate::error::Result<()>`: `rewrite_config(path, &|text| apply(text, changes))`.

- [ ] **Step 1: Write the failing tests** in `src/config_edit.rs`, one per behaviour, each asserting on the returned text:
  - `set_adds_a_missing_table_at_the_end_and_keeps_everything_else` (file with comments, two `[[hosts]]`, `[notify]`; `Change::set(&["dashboard","cpu_warn"], 80)`; assert the old text is a prefix of the new one and the new text parses with `dashboard.cpu_warn == Some(80)`).
  - `set_replaces_a_key_in_place_and_keeps_its_comment` (`cpu_warn = 90 # mine` becomes 80, comment kept).
  - `remove_deletes_the_key_and_an_emptied_table_with_it` (last key of `[dashboard]` removed; no `[dashboard]` left in the text).
  - `remove_of_a_missing_key_is_a_no_op` (text unchanged, `Ok`).
  - `a_cap_in_an_inline_table_is_edited` (`[cloud]\ncaps = { b2 = "10G" }` + set `["cloud","caps","s3"]` = `"5G"` parses with both caps).
  - `a_dotted_cloud_caps_header_and_removing_the_last_cap_removes_cloud_too`.
  - `a_change_that_would_alter_another_table_is_refused` (a `Change` on path `["notify","mail_to"]` is refused: only `ui`, `dashboard` and `cloud` may be edited).
  - `a_file_that_does_not_parse_is_refused_with_the_reason`.
  - `save_keeps_a_bak_copy_and_the_file_mode` (use a tempdir, mode 0640, assert `.bak` has the old text and the new file mode is 0640).

- [ ] **Step 2: Run** `cargo test config_edit` — expect compile failure / FAIL.

- [ ] **Step 3: Implement** `Change`, `apply` and `save` with `toml_edit::DocumentMut`. Rules: only first path segment `ui`, `dashboard` or `cloud` allowed; create missing tables implicit-dotted where needed so `[cloud.caps]` appears as a `[cloud.caps]` header; after applying, prune empty tables along each removed path; then check (a) the new text parses as `Config`, (b) `toml::Table` of the old and new text are equal once `ui`, `dashboard`, `cloud` are removed from both, (c) each `Set` reads back equal from the parsed result. Error strings follow the existing helper's wording style ("config.toml would no longer parse: ..." / "edit it by hand").

- [ ] **Step 4: Run** `cargo test config_edit` — expect PASS; run `cargo clippy --all-targets` — clean.

- [ ] **Step 5: Commit** `feat: a toml_edit based writer for the ui, dashboard and cloud tables`

### Task 2: The Settings model

**Files:**
- Create: `src/ui/settings.rs` (declare in `src/ui/mod.rs`)

**Interfaces:**
- Consumes: `config::{Config, DashboardConfig, Thresholds, parse_cap, DEFAULT_AUTO_REFRESH_SECS}`, `ui::theme::THEME_NAMES`, `config_edit::Change`.
- Produces:
  - `pub enum Setting { Theme, AutoRefresh, Memory, Swap, Cpu, Scrub, ScrubIgnore, Cap(String) }` (`Clone, PartialEq, Debug`).
  - `pub struct SettingsView { pub loaded: bool, pub problem: Option<String>, pub rows: Vec<Row> }` with `Default`; `pub struct Row { pub group: &'static str, pub setting: Setting, pub label: String, pub value: String, pub is_default: bool }`.
  - `pub fn load(config_path: &Path, remotes: &[String], no_color: bool) -> SettingsView`: rows from the file; cap rows = `remotes` plus capped names in the file, sorted, deduplicated; `problem` set (and `rows` empty) when the file cannot be read or parsed.
  - `pub fn form_fields(row: &Row) -> Vec<(String /*label*/, String /*current text, empty when default*/)>`: one or two fields (pairs have warn and crit).
  - `pub fn build_changes(setting: &Setting, values: &[String]) -> Result<Vec<Change>, String>`: validated changes; blank = reset (Remove).
  - `pub fn describe(setting: &Setting, values: &[String]) -> String`: one line for the confirm prompt, e.g. `Memory warn / crit 85 / 95 %`.

- [ ] **Step 1: Write the failing tests** (in `mod tests`):
  - `load_lists_the_groups_in_order_with_defaults_marked` (empty file: rows are Theme, Auto-refresh, Memory, Swap, CPU, Scrub, Scrub ignore; all `is_default`; Theme value `ansi`, or `mono` with `no_color`; Auto-refresh `10 s`; Memory `90 / 97 %`).
  - `load_shows_saved_values_and_cap_rows` (`[dashboard] memory_warn = 80`, `[cloud.caps] b2 = "10G"`, remotes `["zfsctl-aws","b2"]`: Memory `80 / 97 %` not default; cap rows `b2 10G`, `zfsctl-aws (no cap)`).
  - `an_unparsable_file_gives_a_problem_and_no_rows`.
  - `a_valid_pair_becomes_two_sets_and_defaults_become_removes` (`["85","95"]` → sets of `memory_warn`/`memory_crit`; `["90","97"]` → removes of both; `["",""]` → removes).
  - `a_pair_with_warn_not_below_crit_is_refused` (`["97","90"]`, `["0","50"]`, `["50","101"]`, `["abc","95"]`, `["85",""]`, a half-filled pair, is refused with "enter both numbers or leave both empty").
  - `cpu_refresh_theme_and_cap_validation` (cpu `0`/`101`/`x` refused, `80` ok; refresh `0` and `3600` ok, `3601`/`-1` refused; theme `nord` refused with the valid names listed, `mono` ok; cap `10G` ok, `0`, `10GB`, `abc` refused).
  - `scrub_ignore_is_split_on_commas_and_trimmed` (`" scratch, tmp ,,"` → `["scratch","tmp"]`; blank = remove).

- [ ] **Step 2: Run** `cargo test ui::settings` — expect FAIL.

- [ ] **Step 3: Implement** the signatures above. Reuse `Thresholds::default()` for the default pairs and `config::parse_cap` for caps; value texts follow the examples in the spec (`90 / 97 %`, `35 / 90 days`, `10 s`).

- [ ] **Step 4: Run** `cargo test ui::settings` — PASS; clippy clean.

- [ ] **Step 5: Commit** `feat: the Settings model: rows, validation and key changes`

### Task 3: State, keys and the save

**Files:**
- Modify: `src/ui/state.rs` (Section enum, `sections` list at the `vec![...]` near line 473, new fields, `refresh_settings`, key arms), `src/ui/state/forms.rs` (form builder), `src/ui/state/modal_keys.rs` (submit), `src/ui/modal.rs` (`FormKind::Setting`, `MutationCall::SaveSettings`), `src/ui/mod.rs` (refresh match arms at ~282 and ~378)
- Test: `src/ui/state/tests.rs`

**Interfaces:**
- Consumes: Task 1 `Change`, `config_edit::save`; Task 2 `Setting`, `SettingsView`, `load`, `form_fields`, `build_changes`, `describe`.
- Produces:
  - `Section::Settings` (after `Alerts`; the sidebar list gets it last).
  - `App.settings_view: SettingsView`, `App.settings_table: TableState`, `App::refresh_settings(&mut self)`; `refresh_settings_if_stale` is NOT needed (loaded on entering the section and on `r`).
  - `FormKind::Setting { setting: Setting }`; `MutationCall::SaveSettings { config_path: PathBuf, changes: Vec<Change> }` (runs `config_edit::save`).
  - After a successful save the worker refreshes `Section::Settings` and sets `done_note` to `Saved.`.

- [ ] **Step 1: Write the failing tests** in `src/ui/state/tests.rs`, using the existing App/key helpers:
  - `settings_is_the_last_section_and_loads_when_entered`.
  - `enter_on_a_setting_row_opens_its_form_with_the_current_values` (memory row with `memory_warn = 80` saved → form fields `["80", ""]`... i.e. current text of each half; a default half is empty).
  - `submitting_a_valid_form_asks_for_confirmation_then_saves` (drive Enter through the fields, `y`, assert `config.toml` in the tempdir now has the key, `.bak` exists, comments and `[[hosts]]` kept, view shows the new value).
  - `an_invalid_pair_is_rejected_in_the_form_and_nothing_is_written` (file bytes identical).
  - `a_hand_edit_made_while_the_form_is_open_is_not_overwritten` (change another key in the file after the form opens; after the save both are present).
  - `an_unparsable_config_shows_the_problem_and_enter_edits_nothing`.
  - `r_reloads_the_settings_from_the_file`.
  - `enter_on_a_cap_row_without_a_cap_sets_one_and_empty_clears_it`.

- [ ] **Step 2: Run** `cargo test settings` — FAIL.

- [ ] **Step 3: Implement.** `Section::Settings` is added to every exhaustive `match Section` (the compiler lists them). Entering the section calls `refresh_settings`, which passes the distinct names from `self.cloud_rows()` and `self.no_color` equivalent (`ThemeEnv`-free: use `std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())`). Enter on a row builds `Modal::Form` (fields from `form_fields`, title = row label, description = the valid range). Submit follows `submit_notify_form`: `build_changes`, then `Modal::Confirm` with `describe(..)` + `"Save to config.toml? (y/n)"`, declining returns to the form. The save arm calls `config_edit::save`, which re-reads the file itself.

- [ ] **Step 4: Run** `cargo test settings` — PASS; `cargo test` whole suite passes (`cargo test 2>&1 | grep -q "test result: ok"` style per repo practice, and read the summary); clippy clean.

- [ ] **Step 5: Commit** `feat: the Settings section: edit forms and the safe save`

### Task 4: Apply at once

**Files:**
- Modify: `src/ui/state.rs` (new field `auto_refresh_interval: Option<Duration>`, `apply_saved_settings`), `src/ui/mod.rs` (`run_app` seeds the field; `event_loop` reads `app.auto_refresh_interval` instead of its `auto_refresh` parameter)
- Test: `src/ui/state/tests.rs`

**Interfaces:**
- Consumes: `theme::resolve`, `Theme::bright`, `config::UiConfig::auto_refresh_interval`.
- Produces: `App::apply_saved_settings(&mut self)`: re-reads `[ui]`, sets `self.theme` (`resolve(configured, &ThemeEnv::from_process())` then `.theme.bright()`, status note from `Resolved.note`) and `self.auto_refresh_interval`, and refreshes the Dashboard thresholds and Cloud caps. Called after a successful `SaveSettings`.

- [ ] **Step 1: Write the failing tests:**
  - `saving_the_theme_changes_the_apps_theme_at_once` (save `mono`, assert `app.theme.name == ThemeName::Mono`).
  - `saving_a_solarized_theme_without_truecolor_shows_the_note`.
  - `saving_the_auto_refresh_changes_the_interval_and_zero_turns_it_off`.
  - `saving_a_threshold_changes_the_dashboards_thresholds` (`app.dashboard_thresholds.memory == (85, 95)` after the save) and `saving_a_cap_updates_the_cloud_caps`.

- [ ] **Step 2: Run** them — FAIL.

- [ ] **Step 3: Implement** `apply_saved_settings` and hook it where the worker reports a finished `SaveSettings`; replace the `auto_refresh` parameter plumbing as described (keep `run_app`'s signature).

- [ ] **Step 4: Run** `cargo test` — PASS; clippy clean.

- [ ] **Step 5: Commit** `feat: saved settings apply to the running TUI`

### Task 5: Drawing, hints and keys help

**Files:**
- Modify: `src/ui/layout.rs` (`draw` match near line 314: `Section::Settings => draw_settings`; the section-name match near line 190; new `draw_settings`), `src/ui/hints.rs` (arm for `Section::Settings`; the section list near line 601)
- Test: `src/ui/layout.rs` tests (next to the Alerts draw tests, ~line 2870) and `src/ui/hints.rs` tests

**Interfaces:**
- Consumes: `app.settings_view`, `app.settings_table`, `Theme::dim`, `selected_row`.
- Produces: `fn draw_settings(f: &mut Frame, app: &App, area: Rect)`. Group titles dim, rows `label  value`, default rows dimmed with `(default)`, selected row via the theme's `selected_row()`, problem line in `theme.err`, a final dim line `Cloud caps: open Cloud once to list your remotes` only when no remote is known. Hints: `↑↓ Move`, `Enter Edit`, `r Refresh`.

- [ ] **Step 1: Write the failing tests:** `draw_settings_shows_groups_rows_and_defaults` (render with a loaded view; assert the text contains `Display`, `Dashboard thresholds`, `Cloud caps`, `Memory warn / crit`, `(default)`), `draw_settings_shows_the_problem_when_the_file_is_unreadable`, `settings_hints_list_move_edit_refresh`.

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** to match `draw_alerts` conventions (block title `Settings`, `truncate_with_ellipsis`). **Step 4: Run** `cargo test` — PASS; clippy clean.

- [ ] **Step 5: Commit** `feat: draw the Settings section`

### Task 6: Book, README and the capture

**Files:**
- Modify: `docs/book/06-the-screen.md` (sidebar list, a Settings section with an embedded capture), `docs/book/20-reference-keys.md`, `docs/book/21-reference-config.md` (say which keys the Settings screen edits), `docs/book/16-alerts.md` (one line pointing to Settings for the rest), `README.md`
- Create: `docs/book/captures/06-settings.txt`

- [ ] **Step 1:** Take the capture with `docs/book/tools/capture.py` (see `docs/book/tools/README.md`) at 165 columns against a throwaway `config.toml`; embed it in chapter 6 as a `screen` block.
- [ ] **Step 2:** Update the prose and key tables; run `cargo test` — the book tests compare embedded `screen` blocks to `captures/*.txt` and check the config reference against the program's own list of settings; they must pass.
- [ ] **Step 3: Commit** `docs: the Settings section in the book and README`

### Task 7: Whole-branch review and hub check

- [ ] **Step 1:** Run the full suite and clippy; dispatch the whole-branch code review on the branch (fix findings, commit).
- [ ] **Step 2:** Deploy to the hub (tar over ssh, `cargo build --release`, install, `service zfsctl restart`), then in the live TUI: change a Dashboard threshold, a cap, the theme and the refresh interval; confirm the screens change at once, `config.toml` keeps its comments, hosts and `[notify]`, `config.toml.bak` exists, and the daemon keeps running. Restore the original values afterwards. No mutating keys in any scripted check beyond the settings under test.
- [ ] **Step 3:** Merge to `master`, install, update the memory note.
