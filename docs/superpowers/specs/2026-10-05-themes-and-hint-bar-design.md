# Color Themes and Per-Focus Hint Bar Design

## Summary

Two related UI changes, delivered together because both rework the bottom of the screen and every draw function:

1. **Color themes, used functionally.** Color encodes status (health, capacity, scan state, danger), never decoration. All colors live in one `Theme` struct; widget code asks the theme for styles and never names a `Color`.
2. **A one-line hint bar** at the very bottom showing the keys valid for whatever currently has focus. The status message moves to a compact line directly above it.

## Goals

- ZFS status is scannable at a glance: ONLINE green, DEGRADED yellow, FAULTED/UNAVAIL red, OFFLINE gray, resilver/scrub cyan or blue, capacity bars green/yellow/red at 70% and 85%.
- Color is never the only signal: every health state has a glyph and a word, so it works for colorblind users and over monochrome SSH sessions.
- Themes: `ansi` (default), `mono`, `solarized-dark`, `solarized-light` (Solarized, as a tribute to Sun for giving us ZFS).
- Honor `NO_COLOR`; use the 16 ANSI colors by default so the user's terminal theme applies; use RGB only when explicitly requested.
- Every focus state (section, pane, detail tab, modal, picker) has accurate hints, and a test guards against drift between hints and real key handling.
- Destructive confirmations look unmistakably different from routine ones.

## Non-goals

- Runtime theme switching, and user-defined palettes.
- Any change to key bindings.
- Refactoring `handle_key` in `src/ui/state.rs` to be table-driven (considered and rejected as too risky for this feature).

## Current state (confirmed by reading the code)

- `src/ui/layout.rs` has ~32 hard-coded `Color::`/`Style::`/`Modifier::` sites: yellow/red scan status, `DarkGray` for dimming, `Cyan` for the focused middle/right pane border (`pane_border_style`, with tests), and `Modifier::REVERSED` for every row highlight.
- Pool health and vdev health render via `format!("{:?}", health)` (`draw_pools`, `draw_vdevs`) with no styling.
- `draw` in `layout.rs` builds an outer vertical layout `[Min(0), Length(status_height)]` where the bottom is a bordered, wrapped status box (height clamped 3..9, plus a gauge row during a scan). Status content comes from `status_bar_content`.
- The sidebar has a 2-line footer: `? Help` / `A About`.
- `Config` (`src/config.rs`) holds only `hosts`. `Config::load` returns `AppError::Config` on parse failure.
- Modals are `Modal::{Confirm, ConfirmTyped, Form, Info}` plus a separate picker. `PendingAction.call` is a `MutationCall`.

## Design

### 1. Theme module — `src/ui/theme.rs`

`Theme` is a plain struct of `ratatui::style::Color` values, one per semantic role: `ok`, `warn`, `err`, `info`, `muted`, `accent`, `border`, `selected`, `danger`.

Style helpers keep draw code free of raw colors:

- `health(PoolHealth) -> (Style, &'static str)` — style plus glyph+word: `● ONLINE` (ok), `▲ DEGRADED` (warn), `✖ FAULTED` / `✖ UNAVAIL` / `✖ REMOVED` (err), `○ OFFLINE` (muted). An unrecognized state renders as its raw text in the default style; it is never shown as healthy.
- `capacity(pct) -> Style` — `ok` below 70, `warn` from 70 to 85 inclusive, `err` above 85.
- `pane_border(focused)`, `dim()`, `bold_name()`, `selected_row()`. Dim is for mountpoints, GUIDs, read-only properties. Bold is for pool and dataset names. `accent` is reserved for the selected row and the focused pane.

**Themes:** `ansi` (default; 16 ANSI colors only), `mono` (no colors; bold/dim/reverse and glyphs only), `solarized-dark`, `solarized-light` (Solarized RGB palette).

**Selection**, once at startup, with the environment injected as a parameter (so tests do not mutate process env):

1. `theme = "<name>"` in a new `[ui]` table of `config.toml`. Unknown names are a startup error (`AppError::Config`) naming the four valid values — never a silent fallback.
2. Otherwise, `NO_COLOR` set to a non-empty value selects `mono` (empty is ignored, per no-color.org).
3. Otherwise `ansi`.

Solarized uses RGB, so it is only chosen explicitly. If chosen while `COLORTERM` is not `truecolor`/`24bit`, it uses Solarized's published 256-color indexed equivalents and a one-line status note says so at startup.

`Theme` is held on `App` (or passed to `draw`). `pane_border_style` and its tests move into the theme.

### 2. Hint bar and layout

**Layout.** The outer vertical layout becomes `[Min(0), Length(status_h), Length(1)]`:

- The status line is borderless; `status_h` is the wrapped line count of the message clamped to 1–3, plus one row for the gauge only while a resilver/scrub is running. With no message and no scan, it is 0 rows.
- The hint bar is always exactly 1 row at the bottom.
- Net: the idle bottom area shrinks from 3 rows to at most 2.

**Status line styling.** `theme.info` for a scan in progress, `theme.err` for a scan that completed with errors, and `theme.err` for failed operations (wording from the existing failed-submission handling is unchanged). Other messages use the default style.

**Hint bar content.** `key` in bold/accent, label muted, e.g. `s Scrub  d Destroy  n New  Tab Pane  ? Help`.

- Hints for keys that are no-ops in the current state are hidden (e.g. `s`/`d`/`n` while the right pane is focused).
- When too narrow, hints are dropped from the right in priority order. `? Help` and `q Quit` are pinned and never dropped.
- The sidebar's `? Help` / `A About` footer is removed; that information moves into the bar.

**`src/ui/hints.rs`.** `fn hints(app: &App) -> Vec<Hint>` with `Hint { key, label, priority }`. It is pure (takes `&App`, no terminal). Precedence: modal or picker first, then section, then pane focus, then detail tab. A modal shows only its own keys (`Enter Confirm  Esc Cancel`; typed-confirm shows the type-the-name prompt).

**Drift guard.** A table of `(state setup, advertised key)` pairs drives `handle_key` and asserts each advertised key changes state (opens a modal, moves selection, etc.). A second test asserts the converse for the known no-op cases (the existing `s`/`d`/`n`-while-right-focused behavior). Rebinding a key without updating `hints.rs` fails the tests.

### 3. Health symbols and capacity

- **Health columns.** `draw_pools` and `draw_vdevs` use `theme.health(..)` instead of `format!("{:?}", ..)`. Column widths grow (10→12, 12→14) for the glyph; verify the right-pane widths still hold at 80 columns.
- **Row cue.** A degraded or faulted pool's Name cell gets a faint tint so bad pools stand out when scanning the Name column; the glyph carries the meaning in `mono`.
- **Capacity.** The pools table gains a `Used` column, e.g. `████░░░░ 31%`, from `allocated / size`, colored by `theme.capacity(pct)`. Above 85% a `!` suffix is added so the signal survives `mono`. `size == 0` shows `—` (no divide by zero). Size, Alloc and Free columns remain.
- **Other applications.** Disk "in pool" dimming and read-only property dimming use `theme.dim()`. The scan gauge uses `theme.info` (`theme.err` if the scan finished with errors). Pane borders use `theme.pane_border`, which also gives the vdevs, datasets and hosts blocks the focused-border highlight they lack today.

### 4. Destructive modals

The existing modal split already carries the risk distinction (`ConfirmTyped` for irreversible, `Confirm` for lower-risk). The style is keyed off the action's `MutationCall`, derived from the variants in `src/ui/modal.rs`:

- **Danger** (`theme.danger` border and title, glyph-prefixed title naming the action, bold input line): `DestroyPool`, `DestroyDataset`, `DestroySnapshot`, `RollbackSnapshot`.
- **Warn** (`theme.warn` border): `OfflineDisk`, `ReplaceDisk`, `UnallowPermission`, `RemoveAclEntry`.
- **Neutral** (default border): `ScrubPool`, `ResilverPool`, `OnlineDisk`, `CreatePool`, `AddVdev`, `CreateDataset`, `CreateSnapshot`, `AllowPermission`, `AddAclEntry`, and the property setters.
- `Form`, `Info` and the picker stay neutral, with the accent border on the focused element.
- A `MutationCall` match with no wildcard arm, so adding a new variant fails to compile until it is classified.
- A danger modal's hint bar shows `Enter` in `theme.danger`.

## Error handling

- Unknown `theme` value: startup error naming the valid values, following the existing `Config::load` parse-error pattern.
- Solarized without truecolor: 256-color fallback plus a startup status note.
- `NO_COLOR` set but empty: ignored.

## Testing

- **Theme:** health and capacity boundaries (69, 70, 85, 86 percent and size 0); `mono` emits no colors and every health state keeps a distinct glyph; config parsing for each theme name and the invalid-value error; selection precedence with the environment injected.
- **Hints:** the drift-guard tests above; per-focus content tests including modals; pinned `? Help` and `q Quit` survive narrow widths.
- **Layout:** `TestBackend` at 80x24: hint bar is the last row, status line sits directly above it, panes lose no more than the expected rows.
- **Modal style:** a test per danger/warn class mapping.
- **Existing tests:** `pane_border_style` tests move with the function to the theme.
- **Live check** (per the project's verify-end-to-end rule): run in a real terminal under each of the four themes and `NO_COLOR=1`; confirm the hints match actual key behavior on each screen; check a degraded pool if one is available on the test boxes.
