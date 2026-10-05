# Color Themes and Per-Focus Hint Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add functional color themes (status encoded by color *and* glyph) and a one-line, per-focus key-hint bar at the bottom of every screen, with the status message moved to a compact line above it.

**Architecture:** A new `ui/theme.rs` owns every color decision as semantic roles (`ok`, `warn`, `err`, `info`, `muted`, `accent`, `danger`) and exposes style helpers; draw code never names a `Color`. A new `ui/hints.rs` is a pure `fn hints(&App) -> Vec<Hint>` keyed off section / pane focus / detail tab / modal / picker, with tests that drive the real `handle_key` so hints cannot drift from key handling. `layout.rs` is re-laid-out as `[panes, status line, hint bar]`.

**Tech Stack:** Rust 2021, ratatui 0.29, crossterm 0.28, serde + toml 0.8. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-05-themes-and-hint-bar-design.md`

## Global Constraints

Copied from the spec; every task's requirements include these.

- Themes: `ansi` (default), `mono`, `solarized-dark`, `solarized-light`.
- Health states: ONLINE green `● ONLINE`, DEGRADED yellow `▲ DEGRADED`, FAULTED/UNAVAIL/REMOVED red `✖ …`, OFFLINE gray `○ OFFLINE`. Unrecognized states render as raw text in the default style and are never styled as healthy.
- Capacity: `ok` below 70%, `warn` from 70% to 85% inclusive, `err` above 85%; above 85% the percentage gets a `!` suffix; `size == 0` shows `—` (no divide by zero).
- Selection order: `[ui] theme` in `config.toml` → `NO_COLOR` set to a **non-empty** value gives `mono` → `ansi`. An unknown theme name is a startup `AppError::Config` naming the four valid values, never a silent fallback.
- Solarized uses RGB only when `COLORTERM` is `truecolor` or `24bit`; otherwise Solarized's published 256-color indexes plus a one-line startup status note.
- The 16 ANSI colors only for `ansi`; `mono` emits no colors at all (bold/dim/reverse and glyphs only).
- Layout: outer vertical `[Min(0), Length(status_h), Length(1)]`; status line borderless, 1–3 rows plus one gauge row only while a scan reports a percentage, 0 rows when there is no message and no scan; hint bar exactly 1 row; the sidebar's `? Help` / `A About` footer is removed.
- `? Help` and `q Quit` are pinned and never dropped from the hint bar in non-modal contexts; narrow widths drop other hints from the right in priority order.
- Hints for keys that are no-ops in the current state are hidden.
- `MutationCall` classification is an exhaustive `match` with no wildcard arm. Danger: `DestroyPool`, `DestroyDataset`, `DestroySnapshot`, `RollbackSnapshot`. Warn: `OfflineDisk`, `ReplaceDisk`, `UnallowPermission`, `RemoveAclEntry`. Everything else neutral.
- No change to key bindings. No runtime theme switching. No user-defined palettes. No refactor of `handle_key`.
- Do **not** run `cargo fmt` (the repo's long-line style would churn unrelated code). Baseline before this plan: `cargo test` = 403 passed.

## Review Focus

Failure modes the spec implies but its tasks would not otherwise exercise; each has a test in the named task.

1. **Tiny terminal** (e.g. 20x4, 30x6): drawing must not panic; the hint bar still renders its pinned hints. (Task 7)
2. **A pool with `allocated > size` or `size == 0`** must clamp to 100% / show `—`, never panic or overflow the bar. (Task 1)
3. **A very long multi-line status message** (e.g. a `zpool add` failure) must be capped at the status line's 3 rows and must not push the hint bar off-screen. This is a deliberate narrowing from the old cap of 9 rows (named constant `STATUS_MAX_ROWS`). (Task 7)
4. **`NO_COLOR=""` (set but empty)** is ignored, and an explicit `theme = "ansi"` in config beats `NO_COLOR=1`. (Task 1)
5. **A stale error flag**: after a failure message is replaced by an ordinary message (or cleared), the new message must not stay red. (Task 7)

---

## File Structure

| File | Responsibility |
|---|---|
| `src/ui/theme.rs` (create) | `Theme`, palettes, health/capacity/name/border helpers, `resolve()` selection. No ratatui widgets. |
| `src/ui/hints.rs` (create) | `Hint`, `hints(&App)`, `fit()`, `hint_line()`. Pure; unit-testable without a terminal. |
| `src/ui/modal.rs` (modify) | `Tone` enum plus `MutationCall::tone()` / `label()` (exhaustive). |
| `src/ui/layout.rs` (modify) | Replace every hard-coded style with theme calls; new bottom layout; health/capacity columns; modal/picker styling. |
| `src/ui/state.rs` (modify) | `App.theme`, `App.status_error`, `set_error()`, `status_is_error()`; route failure sites through `set_error`. |
| `src/ui/mod.rs` (modify) | `pub mod theme; pub mod hints;`; `run_app` takes the resolved theme. |
| `src/config.rs` (modify) | `UiConfig { theme: Option<String> }` and `Config.ui`. |
| `src/hosts.rs` (modify, tests only) | Two `Config { .. }` literals need `..Default::default()`. |
| `src/main.rs` (modify) | Resolve the theme from config + environment before starting the TUI. |
| `README.md` (modify) | Document `[ui] theme`, `NO_COLOR`, and the hint bar. |

---

### Task 1: Theme module

**Files:**
- Create: `src/ui/theme.rs`
- Modify: `src/ui/mod.rs:1-4` (add `pub mod theme;`)
- Test: inline `#[cfg(test)] mod tests` in `src/ui/theme.rs`

**Interfaces:**
- Consumes: `crate::zfs::pool::PoolHealth` (`Online | Degraded | Faulted | Offline | Unavail | Removed | Other(String)`), `crate::error::{AppError, Result}`.
- Produces (later tasks rely on these exact names):
  - `pub const THEME_NAMES: [&str; 4]`
  - `pub enum ThemeName { Ansi, Mono, SolarizedDark, SolarizedLight }` with `parse(&str) -> Option<ThemeName>` and `as_str(self) -> &'static str`
  - `pub struct Theme { pub name: ThemeName, pub ok: Style, pub warn: Style, pub err: Style, pub info: Style, pub muted: Style, pub accent: Style, pub danger: Style }` (`Clone, PartialEq, Debug`)
  - `Theme::ansi() / mono() / from_name(ThemeName, truecolor: bool)`
  - `theme.selected_row() -> Style`, `theme.pane_border(focused: bool) -> Style`, `theme.dim() -> Style`
  - `theme.health_style(&PoolHealth) -> Style`, `Theme::health_label(&PoolHealth) -> String`, `theme.health_span(&PoolHealth) -> Span<'static>`, `theme.name_style(&PoolHealth) -> Style`
  - `pub fn capacity_pct(allocated: u64, size: u64) -> Option<u8>`, `theme.capacity_style(pct: u8) -> Style`, `pub fn capacity_bar(pct: u8, width: usize) -> String`, `theme.capacity_cell(allocated: u64, size: u64) -> Line<'static>`
  - `pub struct ThemeEnv { pub no_color: Option<String>, pub colorterm: Option<String> }` with `from_process()`; `pub struct Resolved { pub theme: Theme, pub note: Option<String> }`; `pub fn resolve(configured: Option<&str>, env: &ThemeEnv) -> Result<Resolved>`

- [ ] **Step 1: Register the module and write the failing tests**

In `src/ui/mod.rs`, add `pub mod theme;` after `pub mod state;` (keep alphabetical: `layout, modal, picker, state, theme`).

Create `src/ui/theme.rs` containing only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Line;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn mono_theme_uses_no_colors() {
        let t = Theme::mono();
        for s in [t.ok, t.warn, t.err, t.info, t.muted, t.accent, t.danger] {
            assert_eq!(s.fg, None);
            assert_eq!(s.bg, None);
        }
    }

    #[test]
    fn ansi_theme_uses_only_the_16_ansi_colors() {
        let t = Theme::ansi();
        for s in [t.ok, t.warn, t.err, t.info, t.muted, t.accent, t.danger] {
            assert!(!matches!(s.fg, Some(Color::Rgb(..)) | Some(Color::Indexed(_))), "{s:?}");
        }
        // Matches the colour the old hard-coded pane border used.
        assert_eq!(t.accent, Style::new().fg(Color::Cyan));
    }

    #[test]
    fn health_labels_pair_a_glyph_with_the_word() {
        use crate::zfs::pool::PoolHealth::*;
        for (h, glyph, word) in [
            (Online, "●", "ONLINE"),
            (Degraded, "▲", "DEGRADED"),
            (Faulted, "✖", "FAULTED"),
            (Unavail, "✖", "UNAVAIL"),
            (Removed, "✖", "REMOVED"),
            (Offline, "○", "OFFLINE"),
        ] {
            let label = Theme::health_label(&h);
            assert!(label.starts_with(glyph), "{label}");
            assert!(label.ends_with(word), "{label}");
        }
    }

    #[test]
    fn health_glyphs_are_distinct_per_severity_class() {
        use crate::zfs::pool::PoolHealth::*;
        let glyphs: std::collections::HashSet<String> = [Online, Degraded, Faulted, Offline]
            .iter()
            .map(|h| Theme::health_label(h).chars().next().unwrap().to_string())
            .collect();
        assert_eq!(glyphs.len(), 4);
    }

    #[test]
    fn health_styles_follow_severity() {
        use crate::zfs::pool::PoolHealth::*;
        let t = Theme::ansi();
        assert_eq!(t.health_style(&Online), t.ok);
        assert_eq!(t.health_style(&Degraded), t.warn);
        assert_eq!(t.health_style(&Faulted), t.err);
        assert_eq!(t.health_style(&Unavail), t.err);
        assert_eq!(t.health_style(&Removed), t.err);
        assert_eq!(t.health_style(&Offline), t.muted);
    }

    #[test]
    fn unrecognized_health_is_raw_text_and_never_styled_as_healthy() {
        let t = Theme::ansi();
        let h = crate::zfs::pool::PoolHealth::Other("SUSPENDED".to_string());
        assert_eq!(Theme::health_label(&h), "SUSPENDED");
        assert_eq!(t.health_style(&h), Style::new());
        assert_ne!(t.health_style(&h), t.ok);
    }

    #[test]
    fn name_style_is_bold_and_tinted_only_when_not_online() {
        use crate::zfs::pool::PoolHealth::*;
        let t = Theme::ansi();
        assert_eq!(t.name_style(&Online), Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(t.name_style(&Degraded), Style::new().add_modifier(Modifier::BOLD).fg(Color::Yellow));
        assert_eq!(t.name_style(&Faulted), Style::new().add_modifier(Modifier::BOLD).fg(Color::Red));
    }

    #[test]
    fn capacity_style_thresholds_are_70_and_85() {
        let t = Theme::ansi();
        assert_eq!(t.capacity_style(0), t.ok);
        assert_eq!(t.capacity_style(69), t.ok);
        assert_eq!(t.capacity_style(70), t.warn);
        assert_eq!(t.capacity_style(85), t.warn);
        assert_eq!(t.capacity_style(86), t.err);
        assert_eq!(t.capacity_style(100), t.err);
    }

    #[test]
    fn capacity_pct_floors_clamps_and_handles_zero_size() {
        assert_eq!(capacity_pct(0, 0), None);
        assert_eq!(capacity_pct(5, 0), None);
        assert_eq!(capacity_pct(31, 100), Some(31));
        assert_eq!(capacity_pct(859, 1000), Some(85)); // 85.9% shows as 85, stays "warn"
        assert_eq!(capacity_pct(101, 100), Some(100)); // allocated > size clamps
        assert_eq!(capacity_pct(u64::MAX, u64::MAX), Some(100)); // no overflow
    }

    #[test]
    fn capacity_bar_fills_proportionally_and_never_overflows() {
        assert_eq!(capacity_bar(0, 8), "░░░░░░░░");
        assert_eq!(capacity_bar(50, 8), "████░░░░");
        assert_eq!(capacity_bar(100, 8), "████████");
        assert_eq!(capacity_bar(250, 8), "████████"); // out-of-range input clamps
        assert_eq!(capacity_bar(50, 0), "");
    }

    #[test]
    fn capacity_cell_shows_bar_percent_and_a_bang_above_85() {
        let t = Theme::mono();
        assert_eq!(text(&t.capacity_cell(31, 100)), format!("{} 31%", capacity_bar(31, 8)));
        assert!(text(&t.capacity_cell(86, 100)).ends_with("86%!"));
        assert!(!text(&t.capacity_cell(85, 100)).ends_with('!'));
        assert_eq!(text(&t.capacity_cell(0, 0)), "—");
    }

    fn env(no_color: Option<&str>, colorterm: Option<&str>) -> ThemeEnv {
        ThemeEnv { no_color: no_color.map(str::to_string), colorterm: colorterm.map(str::to_string) }
    }

    #[test]
    fn resolve_defaults_to_ansi() {
        let r = resolve(None, &env(None, None)).unwrap();
        assert_eq!(r.theme.name, ThemeName::Ansi);
        assert_eq!(r.note, None);
    }

    #[test]
    fn resolve_no_color_non_empty_selects_mono() {
        assert_eq!(resolve(None, &env(Some("1"), None)).unwrap().theme.name, ThemeName::Mono);
    }

    #[test]
    fn resolve_ignores_an_empty_no_color() {
        assert_eq!(resolve(None, &env(Some(""), None)).unwrap().theme.name, ThemeName::Ansi);
    }

    #[test]
    fn resolve_explicit_config_beats_no_color() {
        assert_eq!(resolve(Some("ansi"), &env(Some("1"), None)).unwrap().theme.name, ThemeName::Ansi);
    }

    #[test]
    fn resolve_unknown_theme_is_an_error_naming_every_valid_theme() {
        let err = resolve(Some("neon"), &env(None, None)).unwrap_err().to_string();
        assert!(err.contains("neon"), "{err}");
        for name in THEME_NAMES {
            assert!(err.contains(name), "{err}");
        }
    }

    #[test]
    fn solarized_uses_rgb_only_with_truecolor_and_otherwise_notes_the_fallback() {
        let tc = resolve(Some("solarized-dark"), &env(None, Some("truecolor"))).unwrap();
        assert!(matches!(tc.theme.ok.fg, Some(Color::Rgb(..))));
        assert_eq!(tc.note, None);

        let tc24 = resolve(Some("solarized-light"), &env(None, Some("24bit"))).unwrap();
        assert!(matches!(tc24.theme.ok.fg, Some(Color::Rgb(..))));

        let fallback = resolve(Some("solarized-dark"), &env(None, None)).unwrap();
        assert!(matches!(fallback.theme.ok.fg, Some(Color::Indexed(_))));
        assert!(fallback.note.unwrap().contains("256-color"));
    }

    #[test]
    fn solarized_dark_and_light_differ_only_in_the_muted_tone() {
        let d = Theme::from_name(ThemeName::SolarizedDark, true);
        let l = Theme::from_name(ThemeName::SolarizedLight, true);
        assert_ne!(d.muted, l.muted);
        assert_eq!(d.ok, l.ok);
        assert_eq!(d.err, l.err);
    }

    #[test]
    fn pane_border_is_accent_only_when_focused() {
        let t = Theme::ansi();
        assert_eq!(t.pane_border(true), t.accent);
        assert_eq!(t.pane_border(false), Style::new());
    }

    #[test]
    fn selected_row_is_reverse_video_in_every_theme() {
        for t in [Theme::ansi(), Theme::mono(), Theme::from_name(ThemeName::SolarizedDark, true)] {
            assert_eq!(t.selected_row(), Style::new().add_modifier(Modifier::REVERSED));
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ui::theme 2>&1 | tail -15`
Expected: compile errors — `Theme`, `resolve`, `capacity_pct` … not found.

- [ ] **Step 3: Write the implementation**

Prepend this to `src/ui/theme.rs` (above the `#[cfg(test)]` module):

```rust
//! Every color decision in the UI lives here, as semantic roles. Draw
//! code asks the theme for a `Style` and never names a `Color`, so a
//! palette can be swapped without touching widget code.
//!
//! Color is never the only signal: health states pair a glyph with the
//! word, and a capacity above 85% gets a `!` suffix, so everything still
//! reads over a monochrome SSH session or for a colorblind user.

use crate::error::{AppError, Result};
use crate::zfs::pool::PoolHealth;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const THEME_NAMES: [&str; 4] = ["ansi", "mono", "solarized-dark", "solarized-light"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Ansi,
    Mono,
    SolarizedDark,
    SolarizedLight,
}

impl ThemeName {
    pub fn parse(s: &str) -> Option<ThemeName> {
        match s {
            "ansi" => Some(ThemeName::Ansi),
            "mono" => Some(ThemeName::Mono),
            "solarized-dark" => Some(ThemeName::SolarizedDark),
            "solarized-light" => Some(ThemeName::SolarizedLight),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Ansi => "ansi",
            ThemeName::Mono => "mono",
            ThemeName::SolarizedDark => "solarized-dark",
            ThemeName::SolarizedLight => "solarized-light",
        }
    }
}

/// Roles are `Style`s (not bare `Color`s) so `mono` can express itself
/// with modifiers instead of colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: ThemeName,
    pub ok: Style,
    pub warn: Style,
    pub err: Style,
    pub info: Style,
    pub muted: Style,
    pub accent: Style,
    pub danger: Style,
}

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

impl Theme {
    /// The 16 ANSI colors only, so the user's terminal theme applies.
    pub fn ansi() -> Theme {
        Theme {
            name: ThemeName::Ansi,
            ok: fg(Color::Green),
            warn: fg(Color::Yellow),
            err: fg(Color::Red),
            info: fg(Color::Cyan),
            muted: fg(Color::DarkGray),
            accent: fg(Color::Cyan),
            danger: fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }

    /// No colors at all: bold / dim / reverse and the glyphs carry everything.
    pub fn mono() -> Theme {
        let plain = Style::new();
        Theme {
            name: ThemeName::Mono,
            ok: plain,
            warn: plain,
            err: plain.add_modifier(Modifier::BOLD),
            info: plain,
            muted: plain.add_modifier(Modifier::DIM),
            accent: plain.add_modifier(Modifier::BOLD),
            danger: plain.add_modifier(Modifier::BOLD),
        }
    }

    /// Solarized (Ethan Schoonover), a tribute to Sun for giving us ZFS.
    /// RGB when `truecolor`, otherwise Solarized's published 256-color
    /// indexes. Only the muted tone differs between dark and light.
    fn solarized(dark: bool, truecolor: bool) -> Theme {
        let c = |rgb: (u8, u8, u8), idx: u8| {
            if truecolor {
                Color::Rgb(rgb.0, rgb.1, rgb.2)
            } else {
                Color::Indexed(idx)
            }
        };
        let green = c((133, 153, 0), 64);
        let yellow = c((181, 137, 0), 136);
        let red = c((220, 50, 47), 160);
        let cyan = c((42, 161, 152), 37);
        let blue = c((38, 139, 210), 33);
        let muted = if dark { c((88, 110, 117), 240) } else { c((147, 161, 161), 245) };
        Theme {
            name: if dark { ThemeName::SolarizedDark } else { ThemeName::SolarizedLight },
            ok: fg(green),
            warn: fg(yellow),
            err: fg(red),
            info: fg(cyan),
            muted: fg(muted),
            accent: fg(blue),
            danger: fg(red).add_modifier(Modifier::BOLD),
        }
    }

    pub fn from_name(name: ThemeName, truecolor: bool) -> Theme {
        match name {
            ThemeName::Ansi => Theme::ansi(),
            ThemeName::Mono => Theme::mono(),
            ThemeName::SolarizedDark => Theme::solarized(true, truecolor),
            ThemeName::SolarizedLight => Theme::solarized(false, truecolor),
        }
    }

    /// Row highlight. Reverse video in every theme: it needs no color, so
    /// it works everywhere, and the accent is reserved for the focused pane.
    pub fn selected_row(&self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    pub fn pane_border(&self, focused: bool) -> Style {
        if focused {
            self.accent
        } else {
            Style::new()
        }
    }

    /// Secondary info: mountpoints, GUIDs, read-only properties.
    pub fn dim(&self) -> Style {
        self.muted
    }

    pub fn health_style(&self, h: &PoolHealth) -> Style {
        match h {
            PoolHealth::Online => self.ok,
            PoolHealth::Degraded => self.warn,
            PoolHealth::Faulted | PoolHealth::Unavail | PoolHealth::Removed => self.err,
            PoolHealth::Offline => self.muted,
            // An unrecognized state is never shown as healthy.
            PoolHealth::Other(_) => Style::new(),
        }
    }

    pub fn health_label(h: &PoolHealth) -> String {
        match h {
            PoolHealth::Online => "● ONLINE".to_string(),
            PoolHealth::Degraded => "▲ DEGRADED".to_string(),
            PoolHealth::Faulted => "✖ FAULTED".to_string(),
            PoolHealth::Unavail => "✖ UNAVAIL".to_string(),
            PoolHealth::Removed => "✖ REMOVED".to_string(),
            PoolHealth::Offline => "○ OFFLINE".to_string(),
            PoolHealth::Other(s) => s.clone(),
        }
    }

    pub fn health_span(&self, h: &PoolHealth) -> Span<'static> {
        Span::styled(Theme::health_label(h), self.health_style(h))
    }

    /// Bold for pool/dataset names, tinted by health unless healthy, so a
    /// bad pool stands out even while scanning only the Name column.
    pub fn name_style(&self, h: &PoolHealth) -> Style {
        let base = Style::new().add_modifier(Modifier::BOLD);
        match h {
            PoolHealth::Online => base,
            other => base.patch(self.health_style(other)),
        }
    }

    pub fn capacity_style(&self, pct: u8) -> Style {
        if pct > 85 {
            self.err
        } else if pct >= 70 {
            self.warn
        } else {
            self.ok
        }
    }

    /// `████░░░░ 31%`, colored by `capacity_style`; `—` when `size == 0`.
    pub fn capacity_cell(&self, allocated: u64, size: u64) -> Line<'static> {
        let Some(pct) = capacity_pct(allocated, size) else {
            return Line::styled("—", self.muted);
        };
        let style = self.capacity_style(pct);
        let bang = if pct > 85 { "!" } else { "" };
        Line::from(vec![
            Span::styled(capacity_bar(pct, 8), style),
            Span::styled(format!(" {pct}%{bang}"), style),
        ])
    }
}

/// Whole-percent used, floored (so 85.9% shows — and is styled — as 85),
/// clamped to 100. `None` when `size == 0`.
pub fn capacity_pct(allocated: u64, size: u64) -> Option<u8> {
    if size == 0 {
        return None;
    }
    Some(((allocated as u128 * 100) / size as u128).min(100) as u8)
}

pub fn capacity_bar(pct: u8, width: usize) -> String {
    let pct = (pct as usize).min(100);
    let filled = ((pct * width + 50) / 100).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// The slice of the process environment theme selection looks at,
/// injected so tests never mutate real env vars.
#[derive(Debug, Default, Clone)]
pub struct ThemeEnv {
    pub no_color: Option<String>,
    pub colorterm: Option<String>,
}

impl ThemeEnv {
    pub fn from_process() -> ThemeEnv {
        ThemeEnv { no_color: std::env::var("NO_COLOR").ok(), colorterm: std::env::var("COLORTERM").ok() }
    }

    fn truecolor(&self) -> bool {
        matches!(self.colorterm.as_deref(), Some("truecolor") | Some("24bit"))
    }

    /// Per no-color.org, an empty `NO_COLOR` does not count.
    fn no_color_requested(&self) -> bool {
        self.no_color.as_deref().is_some_and(|v| !v.is_empty())
    }
}

pub struct Resolved {
    pub theme: Theme,
    /// A one-line startup notice (shown as the status message), if any.
    pub note: Option<String>,
}

/// Config key → `NO_COLOR` → `ansi`. An unknown configured name is an
/// error, never a silent fallback.
pub fn resolve(configured: Option<&str>, env: &ThemeEnv) -> Result<Resolved> {
    let name = match configured {
        Some(s) => ThemeName::parse(s).ok_or_else(|| {
            AppError::Config(format!(
                "unknown theme '{s}' in config.toml ([ui] theme) -- valid themes: {}",
                THEME_NAMES.join(", ")
            ))
        })?,
        None if env.no_color_requested() => ThemeName::Mono,
        None => ThemeName::Ansi,
    };
    let truecolor = env.truecolor();
    let note = (matches!(name, ThemeName::SolarizedDark | ThemeName::SolarizedLight) && !truecolor).then(|| {
        format!("Theme {}: COLORTERM is not truecolor, using 256-color approximations", name.as_str())
    });
    Ok(Resolved { theme: Theme::from_name(name, truecolor), note })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ui::theme 2>&1 | tail -15`
Expected: all `ui::theme::tests::*` PASS (20 tests). Fix any compile error (most likely a missing `use`) before continuing.

- [ ] **Step 5: Commit**

```bash
git add src/ui/theme.rs src/ui/mod.rs
git commit -m "feat: add Theme module with semantic roles, health/capacity helpers, and selection

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Config key and startup wiring

**Files:**
- Modify: `src/config.rs` (add `UiConfig`, `Config.ui`; tests)
- Modify: `src/hosts.rs:76,128` (test literals)
- Modify: `src/ui/state.rs` (`App.theme` field + `App::new`)
- Modify: `src/ui/mod.rs:44-71` (`run_app` signature)
- Modify: `src/main.rs:18-24`
- Test: `src/config.rs` tests

**Interfaces:**
- Consumes: `theme::{Theme, Resolved, ThemeEnv, resolve}` (Task 1).
- Produces: `Config.ui: UiConfig` with `UiConfig.theme: Option<String>`; `App.theme: Theme` (public, defaults to `Theme::ansi()` so every existing `App::new(...)` test is untouched); `run_app(inventory, config_path, resolved: theme::Resolved)`.

- [ ] **Step 1: Write the failing config tests**

Append inside `mod tests` in `src/config.rs`:

```rust
    #[test]
    fn ui_theme_defaults_to_none_and_is_not_serialized_when_unset() {
        let config = Config::default();
        assert_eq!(config.ui.theme, None);
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(!text.contains("[ui]"), "{text}");
        assert!(!text.contains("theme"), "{text}");
    }

    #[test]
    fn ui_theme_parses_from_toml_alongside_hosts() {
        let text = "[ui]\ntheme = \"solarized-dark\"\n\n[[hosts]]\nname = \"backup\"\nssh_target = \"backup.example.com\"\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.ui.theme.as_deref(), Some("solarized-dark"));
        assert_eq!(config.hosts.len(), 1);
    }

    #[test]
    fn ui_theme_survives_a_save_load_round_trip_with_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
            ui: UiConfig { theme: Some("mono".to_string()) },
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test config:: 2>&1 | tail -10`
Expected: compile error — `UiConfig` / `config.ui` not found.

- [ ] **Step 3: Implement config + fix literals**

In `src/config.rs` replace the `Config` struct with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
    #[serde(default, skip_serializing_if = "UiConfig::is_default")]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UiConfig {
    /// One of `ui::theme::THEME_NAMES`; validated at startup by
    /// `ui::theme::resolve`, not here, so the error can list the choices.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub theme: Option<String>,
}

impl UiConfig {
    fn is_default(&self) -> bool {
        self.theme.is_none()
    }
}
```

The existing `Config { hosts: vec![...] }` literals now need the new field. In `src/hosts.rs` (two test literals, ~lines 76 and 128) and `src/config.rs` (the existing round-trip test, ~line 72) add `..Default::default()` as the last field, e.g.:

```rust
        let config = Config {
            hosts: vec![HostConfig { name: "backup".to_string(), ssh_target: Some("backup.example.com".to_string()) }],
            ..Default::default()
        };
```

Run `cargo test config:: hosts:: 2>&1 | tail -10` — expected PASS (including the three new tests).

- [ ] **Step 4: Add `App.theme`**

In `src/ui/state.rs`, add the import `use crate::ui::theme::Theme;` next to the other `crate::ui::…` imports, add a public field to `struct App` right after `pub status_message: Option<String>,`:

```rust
    pub theme: Theme,
```

and in `App::new`, right after `status_message: None,`:

```rust
            theme: Theme::ansi(),
```

- [ ] **Step 5: Wire selection into startup**

`src/main.rs` — replace the body of `run()`:

```rust
fn run() -> error::Result<()> {
    check_root()?;
    let config_path = PathBuf::from(config::DEFAULT_CONFIG_PATH);
    let config = config::Config::load(&config_path)?;
    // Resolved before the terminal enters raw mode, so a bad `theme`
    // value prints a plain error and exits instead of garbling the screen.
    let resolved = ui::theme::resolve(config.ui.theme.as_deref(), &ui::theme::ThemeEnv::from_process())?;
    let inventory = hosts::HostInventory::from_config(&config);
    ui::run_app(inventory, config_path, resolved)
}
```

`src/ui/mod.rs` — change the signature and apply the result right after `App::new`:

```rust
pub fn run_app(mut inventory: HostInventory, config_path: std::path::PathBuf, resolved: theme::Resolved) -> Result<()> {
```
```rust
            let mut app = App::new(host_names);
            app.theme = resolved.theme;
            app.status_message = resolved.note;
```

(`resolved` is moved into the closure; if the borrow checker complains because the closure is `FnOnce` inside `and_then`, that is fine — it is called once.)

- [ ] **Step 6: Verify**

Run: `cargo build 2>&1 | tail -5 && cargo test 2>&1 | tail -4`
Expected: builds; `test result: ok. 426 passed` (403 + 20 theme + 3 config). Fix the count in your head if a test was added elsewhere; the requirement is **0 failed**.

- [ ] **Step 7: Commit**

```bash
git add src/config.rs src/hosts.rs src/main.rs src/ui/mod.rs src/ui/state.rs
git commit -m "feat: [ui] theme config key and startup theme selection

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Route every existing style through the theme (no visual change yet)

Mechanical re-plumbing: after this task the `ansi` theme renders exactly as before, but nothing in `layout.rs` except the status bar names a `Color` or `Modifier::REVERSED`.

**Files:**
- Modify: `src/ui/layout.rs`
- Test: `src/ui/layout.rs` tests (the existing `pane_border_style` test moves to the new signature)

**Interfaces:**
- Consumes: `Theme` (Task 1), `App.theme` (Task 2).
- Produces: `fn pane_border_style(theme: &Theme, current: PaneFocus, pane: PaneFocus) -> Style` (private to `layout.rs`); `draw_picker(f, picker, theme, area)`.

- [ ] **Step 1: Update the existing test first (it will fail to compile)**

Replace the `tests` module at the bottom of `src/ui/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;
    // Explicit (not via `super::*`): Task 7 removes `Color` from the
    // module's own imports once the status bar stops naming colors.
    use ratatui::style::Color;

    #[test]
    fn pane_border_style_highlights_only_the_current_pane() {
        let theme = Theme::ansi();
        assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Middle), Style::default().fg(Color::Cyan));
        assert_eq!(pane_border_style(&theme, PaneFocus::Middle, PaneFocus::Right), Style::default());
        assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Right), Style::default().fg(Color::Cyan));
        assert_eq!(pane_border_style(&theme, PaneFocus::Right, PaneFocus::Middle), Style::default());
    }
}
```

Run: `cargo test ui::layout 2>&1 | tail -6` — expected: compile error (signature mismatch).

- [ ] **Step 2: Mechanical substitutions**

Run these from the repo root (each is idempotent):

```bash
# 1. every reverse-video row/list highlight -> the theme
sed -i 's/Style::default()\.add_modifier(Modifier::REVERSED)/theme.selected_row()/g' src/ui/layout.rs
# 2. pane borders now take the theme
sed -i 's/pane_border_style(app\.pane_focus, PaneFocus::\(Middle\|Right\))/pane_border_style(\&theme, app.pane_focus, PaneFocus::\1)/g' src/ui/layout.rs
# 3. read-only property dimming takes the theme
sed -i 's/read_only_property_style(p)/read_only_property_style(\&theme, p)/g' src/ui/layout.rs
# 4. give every draw fn that needs it a local theme
sed -i -E '/^fn draw_(sidebar|hosts|pools|pool_properties|vdevs|datasets|dataset_properties|dataset_permissions|dataset_acl|snapshots|disks|shares)\(/a\    let theme = app.theme.clone();' src/ui/layout.rs
```

- [ ] **Step 3: Hand edits for the rest of the sites**

`pane_border_style` and `read_only_property_style` — replace both functions:

```rust
/// Dims a read-only property row so it's visually obvious at a glance
/// that it can't be edited — separate from the existing "read-only
/// property" status-message hint, which only appears after you've
/// already tried.
fn read_only_property_style(theme: &Theme, prop: &Property) -> Style {
    if prop.source.is_editable() {
        Style::default()
    } else {
        theme.dim()
    }
}
```
```rust
/// The border style for a Pools/Datasets split-pane block: the theme's
/// accent when `pane` is the one `current` currently points at, the
/// terminal's default style otherwise.
fn pane_border_style(theme: &Theme, current: PaneFocus, pane: PaneFocus) -> Style {
    theme.pane_border(current == pane)
}
```

Imports at the top of `layout.rs`: add `use crate::ui::theme::Theme;` and keep `Color` (the status bar still uses it until Task 7); drop `Modifier` if the compiler warns it is unused.

`draw_hosts` — replace the dim hint style and give the list the focused border:

```rust
            .style(theme.dim());
```
(replacing `.style(Style::default().fg(Color::DarkGray))` on the "Only this host is configured" hint) and
```rust
    let list = List::new(items).block(Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title("Hosts"));
```

`draw_vdevs`, `draw_snapshots`, `draw_disks`, `draw_shares` — these single-pane screens are always the focused pane. In each, change `Block::default().borders(Borders::ALL).title(title)` to:

```rust
Block::default().borders(Borders::ALL).border_style(theme.pane_border(true)).title(title)
```

`draw_disks` — the in-pool dimming:

```rust
            DiskMembership::InPool { .. } => theme.dim(),
```
(replacing `Style::default().fg(Color::DarkGray)`).

`draw_sidebar` — leave the border neutral; leave the `? Help / A About` footer for Task 7.

`draw` — pass the theme to the picker:

```rust
    if let Some(picker) = app.picker.clone() {
        draw_picker(f, &picker, &app.theme, area);
    }
```
and update `draw_picker`'s signature and its border:

```rust
fn draw_picker(f: &mut Frame, picker: &crate::ui::picker::PickerState, theme: &Theme, area: Rect) {
```
```rust
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title))
        .highlight_style(theme.selected_row());
```
(the `sed` in Step 2 already turned the highlight into `theme.selected_row()`; `theme` is now the parameter).

- [ ] **Step 4: Verify nothing regressed and no stray color names remain**

Run: `cargo build 2>&1 | grep -E "^(warning|error)" | sort | uniq -c`
Expected: no errors; no new warnings.

Run: `grep -n "Color::\|Modifier::" src/ui/layout.rs`
Expected: only the status-bar lines (`Color::Yellow`, `Color::Red`, the gauge) and the test module.

Run: `cargo test 2>&1 | tail -3` — expected 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src/ui/layout.rs
git commit -m "refactor: route layout styles through Theme (ansi renders as before)

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Health symbols and capacity bars in the tables

**Files:**
- Modify: `src/ui/layout.rs` (`draw_pools`, `draw_vdevs`, test helper + tests)
- Test: `src/ui/layout.rs` tests

**Interfaces:**
- Consumes: `theme.health_span`, `theme.name_style`, `theme.capacity_cell` (Task 1).
- Produces: a test helper `render(app: &mut App, w: u16, h: u16) -> Vec<String>` in `layout.rs` tests, reused by Tasks 7–8.

**Deliberate deviation to flag at handoff:** the Pools table column order becomes `Name, Health, Used, Size, Alloc, Free` (Health and Used moved forward). Today Health is the *last* column, so on a narrow terminal ratatui clips it first — and Health is the thing the user scans for. Size/Alloc/Free all remain; only their order relative to Health changes.

- [ ] **Step 1: Write the failing render tests**

Add to the `tests` module in `src/ui/layout.rs`:

```rust
    use crate::zfs::pool::{Pool, PoolHealth, VdevLine};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Renders one frame and returns it as one string per terminal row.
    fn render(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    fn pool(name: &str, allocated: u64, size: u64, health: PoolHealth) -> Pool {
        Pool { name: name.to_string(), size, allocated, free: size.saturating_sub(allocated), health }
    }

    fn app_in_pools(pools: Vec<Pool>) -> App {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.pools = pools;
        app.pools_table.select(Some(0));
        app
    }

    #[test]
    fn pools_table_shows_health_glyphs_and_capacity_bars() {
        let mut app = app_in_pools(vec![
            pool("tank", 31, 100, PoolHealth::Online),
            pool("scratch", 90, 100, PoolHealth::Degraded),
        ]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("● ONLINE"), "{rows}");
        assert!(rows.contains("▲ DEGRADED"), "{rows}");
        assert!(rows.contains("31%"), "{rows}");
        assert!(rows.contains("90%!"), "{rows}");
    }

    #[test]
    fn pools_table_survives_zero_size_and_overfull_pools() {
        let mut app = app_in_pools(vec![
            pool("empty", 0, 0, PoolHealth::Offline),
            pool("over", 150, 100, PoolHealth::Faulted),
        ]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("○ OFFLINE"), "{rows}");
        assert!(rows.contains("✖ FAULTED"), "{rows}");
        assert!(rows.contains("100%!"), "{rows}");
    }

    #[test]
    fn unrecognized_pool_health_is_rendered_as_its_raw_text() {
        let mut app = app_in_pools(vec![pool("odd", 1, 100, PoolHealth::Other("SUSPENDED".to_string()))]);
        let rows = render(&mut app, 200, 30).join("\n");
        assert!(rows.contains("SUSPENDED"), "{rows}");
        assert!(!rows.contains("● SUSPENDED"), "{rows}");
    }

    #[test]
    fn health_is_visible_at_80_columns() {
        let mut app = app_in_pools(vec![pool("tank", 31, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24).join("\n");
        assert!(rows.contains("ONLINE"), "Health must survive a narrow terminal:\n{rows}");
    }

    #[test]
    fn vdev_tree_shows_health_glyphs() {
        let mut app = App::new(vec!["local".to_string()]);
        app.selected_section = app.sections.iter().position(|s| *s == Section::Pools).unwrap();
        app.drilled_pool = Some("tank".to_string());
        app.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Degraded },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Faulted },
        ];
        let rows = render(&mut app, 100, 20).join("\n");
        assert!(rows.contains("▲ DEGRADED"), "{rows}");
        assert!(rows.contains("✖ FAULTED"), "{rows}");
    }
```

Run: `cargo test ui::layout 2>&1 | tail -12` — expected: the five new tests FAIL (current output is `Online` / no bar).

- [ ] **Step 2: Implement**

Add `use ratatui::text::Span; use ratatui::widgets::Cell;` to the imports (`Cell` joins the existing `widgets::{…}` list).

Replace `draw_pools`:

```rust
fn draw_pools(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let rows = app.pools.iter().map(|p| {
        Row::new(vec![
            Cell::from(Span::styled(p.name.clone(), theme.name_style(&p.health))),
            Cell::from(theme.health_span(&p.health)),
            Cell::from(theme.capacity_cell(p.allocated, p.size)),
            Cell::from(p.size.to_string()),
            Cell::from(p.allocated.to_string()),
            Cell::from(p.free.to_string()),
        ])
    });
    // Health and Used come right after Name so a narrow terminal clips the
    // raw byte counts, not the status the user is scanning for.
    let widths = [
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(14),
        Constraint::Length(14),
    ];
    let title = format!("Pools — {} ('r' refresh, → for properties)", app.active_host_name());
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Name", "Health", "Used", "Size", "Alloc", "Free"]))
        .block(Block::default().borders(Borders::ALL).border_style(pane_border_style(&theme, app.pane_focus, PaneFocus::Middle)).title(title))
        .row_highlight_style(theme.selected_row());
    f.render_stateful_widget(table, area, &mut app.pools_table);
}
```

In `draw_vdevs` replace the row construction and the width of the Health column:

```rust
    let rows = app.vdevs.iter().map(|v| {
        let indented = format!("{}{}", "  ".repeat(v.depth), v.name);
        Row::new(vec![Cell::from(indented), Cell::from(theme.health_span(&v.health))])
    });
    let widths = [Constraint::Length(30), Constraint::Length(14)];
```

- [ ] **Step 3: Run the tests**

Run: `cargo test ui::layout 2>&1 | tail -12`
Expected: all PASS, including `health_is_visible_at_80_columns`. **If that one fails**, the Health column is being clipped at 80 columns: shrink the Name column to `Constraint::Min(8)`-style or `Length(14)` until `ONLINE` is visible, keeping Health second. Do not move Health back to the end.

- [ ] **Step 4: Commit**

```bash
git add src/ui/layout.rs
git commit -m "feat: health glyphs and capacity bars in the pool and vdev tables

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Action tone classification

**Files:**
- Modify: `src/ui/modal.rs` (add `Tone`, `MutationCall::tone`, `MutationCall::label`)
- Test: `src/ui/modal.rs` tests (append to the existing `#[cfg(test)]` module, or create one at the bottom if absent)

**Interfaces:**
- Consumes: the existing `MutationCall` variants (`ScrubPool`, `ResilverPool`, `DestroyPool`, `CreatePool`, `AddVdev`, `OfflineDisk`, `OnlineDisk`, `ReplaceDisk`, `SetPoolProperty`, `CreateDataset`, `DestroyDataset`, `SetDatasetProperty`, `CreateSnapshot`, `DestroySnapshot`, `RollbackSnapshot`, `AllowPermission`, `UnallowPermission`, `AddAclEntry`, `RemoveAclEntry`).
- Produces: `pub enum Tone { Neutral, Warn, Danger }` (`Copy, PartialEq, Eq, Debug`); `MutationCall::tone(&self) -> Tone`; `MutationCall::label(&self) -> &'static str`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tone_tests {
    use super::*;

    #[test]
    fn irreversible_actions_are_danger() {
        for call in [
            MutationCall::DestroyPool { pool: "tank".into() },
            MutationCall::DestroyDataset { name: "tank/a".into() },
            MutationCall::DestroySnapshot { dataset: "tank/a".into(), name: "s".into() },
            MutationCall::RollbackSnapshot { dataset: "tank/a".into(), name: "s".into() },
        ] {
            assert_eq!(call.tone(), Tone::Danger, "{call:?}");
        }
    }

    #[test]
    fn disruptive_but_recoverable_actions_are_warn() {
        for call in [
            MutationCall::OfflineDisk { pool: "tank".into(), disk: "da0".into() },
            MutationCall::ReplaceDisk { pool: "tank".into(), old_disk: "da0".into(), new_disk: "da1".into() },
            MutationCall::RemoveAclEntry { path: "/tank".into(), position: 0 },
        ] {
            assert_eq!(call.tone(), Tone::Warn, "{call:?}");
        }
    }

    #[test]
    fn routine_actions_are_neutral() {
        for call in [
            MutationCall::ScrubPool { pool: "tank".into() },
            MutationCall::OnlineDisk { pool: "tank".into(), disk: "da0".into() },
            MutationCall::CreateDataset { name: "tank/a".into() },
        ] {
            assert_eq!(call.tone(), Tone::Neutral, "{call:?}");
        }
    }

    #[test]
    fn labels_name_the_action_in_capitals() {
        assert_eq!(MutationCall::DestroyPool { pool: "t".into() }.label(), "DESTROY POOL");
        assert_eq!(MutationCall::RollbackSnapshot { dataset: "t".into(), name: "s".into() }.label(), "ROLLBACK SNAPSHOT");
        assert_eq!(MutationCall::OfflineDisk { pool: "t".into(), disk: "d".into() }.label(), "OFFLINE DISK");
    }
}
```

Run: `cargo test ui::modal 2>&1 | tail -6` — expected: compile error (`Tone` / `tone` not found).

- [ ] **Step 2: Implement**

Insert after the `MutationCall` enum definition in `src/ui/modal.rs` (keep the `match`es **free of wildcard arms** so a new variant fails to compile until classified):

```rust
/// How alarming an action's confirmation should look. Drives the modal
/// border/title style and the hint bar's confirm key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Warn,
    Danger,
}

impl MutationCall {
    /// Deliberately has no wildcard arm: adding a `MutationCall` variant
    /// must not compile until someone decides how alarming it is.
    pub fn tone(&self) -> Tone {
        match self {
            MutationCall::DestroyPool { .. }
            | MutationCall::DestroyDataset { .. }
            | MutationCall::DestroySnapshot { .. }
            | MutationCall::RollbackSnapshot { .. } => Tone::Danger,
            MutationCall::OfflineDisk { .. }
            | MutationCall::ReplaceDisk { .. }
            | MutationCall::UnallowPermission { .. }
            | MutationCall::RemoveAclEntry { .. } => Tone::Warn,
            MutationCall::ScrubPool { .. }
            | MutationCall::ResilverPool { .. }
            | MutationCall::OnlineDisk { .. }
            | MutationCall::CreatePool { .. }
            | MutationCall::AddVdev { .. }
            | MutationCall::CreateDataset { .. }
            | MutationCall::CreateSnapshot { .. }
            | MutationCall::AllowPermission { .. }
            | MutationCall::AddAclEntry { .. }
            | MutationCall::SetPoolProperty { .. }
            | MutationCall::SetDatasetProperty { .. } => Tone::Neutral,
        }
    }

    /// Short capitalised name shown in a danger/warn modal's title.
    pub fn label(&self) -> &'static str {
        match self {
            MutationCall::ScrubPool { .. } => "SCRUB POOL",
            MutationCall::ResilverPool { .. } => "RESILVER POOL",
            MutationCall::DestroyPool { .. } => "DESTROY POOL",
            MutationCall::CreatePool { .. } => "CREATE POOL",
            MutationCall::AddVdev { .. } => "ADD VDEV",
            MutationCall::OfflineDisk { .. } => "OFFLINE DISK",
            MutationCall::OnlineDisk { .. } => "ONLINE DISK",
            MutationCall::ReplaceDisk { .. } => "REPLACE DISK",
            MutationCall::SetPoolProperty { .. } => "SET POOL PROPERTY",
            MutationCall::CreateDataset { .. } => "CREATE DATASET",
            MutationCall::DestroyDataset { .. } => "DESTROY DATASET",
            MutationCall::SetDatasetProperty { .. } => "SET DATASET PROPERTY",
            MutationCall::CreateSnapshot { .. } => "CREATE SNAPSHOT",
            MutationCall::DestroySnapshot { .. } => "DESTROY SNAPSHOT",
            MutationCall::RollbackSnapshot { .. } => "ROLLBACK SNAPSHOT",
            MutationCall::AllowPermission { .. } => "GRANT PERMISSION",
            MutationCall::UnallowPermission { .. } => "REVOKE PERMISSION",
            MutationCall::AddAclEntry { .. } => "ADD ACL ENTRY",
            MutationCall::RemoveAclEntry { .. } => "REMOVE ACL ENTRY",
        }
    }
}
```

If the compiler reports a missing or extra variant, the enum in `modal.rs` is the source of truth — match it exactly (this task was written against the 19 variants listed in Interfaces).

- [ ] **Step 3: Verify**

Run: `cargo test ui::modal 2>&1 | tail -6` — expected PASS (4 new tests).

- [ ] **Step 4: Commit**

```bash
git add src/ui/modal.rs
git commit -m "feat: classify mutation calls as neutral/warn/danger

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Hints module with drift-guard tests

**Files:**
- Create: `src/ui/hints.rs`
- Modify: `src/ui/mod.rs` (add `pub mod hints;`)
- Test: inline in `src/ui/hints.rs`

**Interfaces:**
- Consumes: `App` (public fields `drilled_pool, vdevs, vdevs_table, pools, pools_table, datasets, datasets_table, snapshots, snapshots_table, disks, disks_table, selected_disks, shares, shares_table, pool_properties_table, dataset_properties_table, dataset_permissions, dataset_permissions_table, acl_entries, acl_entries_table, acl_entries_for, pane_focus, dataset_detail_tab, shares_loaded, nfs_available, modal, picker, sections, selected_section, hosts, selected_host, should_quit, status_message`; methods `current_section()`, `selected_dataset_name()`, `selected_dataset_mountpoint()`, `handle_key(KeyCode)`), `Modal`, `Tone`, `picker::build_picker_state`, `Theme` fields (`accent`, `warn`, `danger`, `muted`).
- Produces: `pub struct Hint { pub key: &'static str, pub label: &'static str, pub priority: u8, pub pinned: bool, pub tone: Tone }`; `pub fn hints(app: &App) -> Vec<Hint>`; `pub fn fit(hints: &[Hint], width: usize) -> Vec<Hint>`; `pub fn hint_line(hints: &[Hint], width: usize, theme: &Theme) -> Line<'static>`.

Priority convention: lower number = more important (0 = never dropped before others); `fit` drops the highest number first, rightmost among ties; `pinned` hints are never dropped.

- [ ] **Step 1: Register the module**

`src/ui/mod.rs`: add `pub mod hints;` (alphabetical: after `pub mod layout;`'s predecessor — order `hints, layout, modal, picker, state, theme`).

- [ ] **Step 2: Write the failing tests**

Create `src/ui/hints.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{AclEntry, AclEntryType, AclTag};
    use crate::ui::modal::{FormField, FormKind, MutationCall, PendingAction};
    use crate::ui::state::PermissionRow;
    use crate::zfs::dataset::{Dataset, DatasetKind};
    use crate::zfs::disk::{DiskInfo, DiskMembership};
    use crate::zfs::pool::{Pool, PoolHealth, VdevLine};
    use crate::zfs::property::{Property, PropertySource};
    use crate::zfs::share::Share;
    use crate::zfs::snapshot::Snapshot;
    use crossterm::event::KeyCode;
    use ratatui::style::Style;

    // ---- fixtures -------------------------------------------------

    fn base() -> App {
        App::new(vec!["local".to_string(), "other".to_string()])
    }

    fn go(app: &mut App, section: Section) {
        app.selected_section = app.sections.iter().position(|s| *s == section).unwrap();
        app.pane_focus = PaneFocus::Middle;
    }

    fn pool(name: &str) -> Pool {
        Pool { name: name.to_string(), size: 100, allocated: 10, free: 90, health: PoolHealth::Online }
    }

    fn dataset(name: &str) -> Dataset {
        Dataset { name: name.to_string(), kind: DatasetKind::Filesystem, used: 1, available: 1, referenced: 1, mountpoint: Some(format!("/{name}")) }
    }

    fn prop(name: &str) -> Property {
        Property { name: name.to_string(), value: "off".to_string(), source: PropertySource::Local }
    }

    fn pools_middle() -> App {
        let mut a = base();
        go(&mut a, Section::Pools);
        a.pools = vec![pool("tank"), pool("scratch")];
        a.pools_table.select(Some(0));
        a
    }

    fn pools_middle_empty() -> App {
        let mut a = base();
        go(&mut a, Section::Pools);
        a
    }

    fn pools_right() -> App {
        let mut a = pools_middle();
        a.pane_focus = PaneFocus::Right;
        a.pool_properties = vec![prop("comment"), prop("autoexpand")];
        a.pool_properties_table.select(Some(0));
        a
    }

    fn drilled(selected: bool) -> App {
        let mut a = pools_middle();
        a.drilled_pool = Some("tank".to_string());
        a.vdevs = vec![
            VdevLine { depth: 0, name: "tank".to_string(), health: PoolHealth::Online },
            VdevLine { depth: 1, name: "da0".to_string(), health: PoolHealth::Online },
        ];
        a.vdevs_table.select(if selected { Some(1) } else { None });
        a
    }

    fn datasets_middle(shares_loaded: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Datasets);
        a.datasets = vec![dataset("tank/a"), dataset("tank/b")];
        a.datasets_table.select(Some(0));
        a.shares_loaded = shares_loaded;
        a.nfs_available = Some(true);
        a
    }

    fn datasets_right(tab: DatasetDetailTab) -> App {
        let mut a = datasets_middle(false);
        a.pane_focus = PaneFocus::Right;
        a.dataset_detail_tab = tab;
        a.dataset_properties = vec![prop("compression"), prop("atime")];
        a.dataset_properties_table.select(Some(0));
        a.dataset_permissions = vec![
            PermissionRow::Set { name: "s1".to_string(), perms: vec!["create".to_string()] },
            PermissionRow::CreateTime { perms: vec!["mount".to_string()] },
        ];
        a.dataset_permissions_table.select(Some(0));
        a.acl_entries_for = Some("/tank/a".to_string());
        a.acl_entries = vec![
            AclEntry { tag: AclTag::Owner, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
            AclEntry { tag: AclTag::Everyone, permissions: vec!["read_data".to_string()], flags: vec![], entry_type: AclEntryType::Allow },
        ];
        a.acl_entries_table.select(Some(0));
        a
    }

    fn snapshots() -> App {
        let mut a = base();
        go(&mut a, Section::Snapshots);
        a.snapshots = vec![
            Snapshot { dataset: "tank/a".to_string(), name: "s1".to_string(), used: 1 },
            Snapshot { dataset: "tank/a".to_string(), name: "s2".to_string(), used: 1 },
        ];
        a.snapshots_table.select(Some(0));
        a
    }

    fn disks(selected: bool) -> App {
        let mut a = base();
        go(&mut a, Section::Disks);
        a.disks = vec![
            DiskInfo { name: "da0".to_string(), size: 1, membership: DiskMembership::Available },
            DiskInfo { name: "da1".to_string(), size: 1, membership: DiskMembership::Available },
        ];
        a.disks_table.select(Some(0));
        if selected {
            a.selected_disks.insert("da0".to_string());
        }
        a
    }

    fn shares() -> App {
        let mut a = base();
        go(&mut a, Section::Shares);
        a.shares = vec![
            Share::Nfs { dataset: "tank/a".to_string(), raw: "on".to_string(), networks: vec![], hosts: vec![], read_only: false, root: None },
            Share::Smb { dataset: "tank/b".to_string(), name: None },
        ];
        a.shares_table.select(Some(0));
        a
    }

    fn hosts() -> App {
        let mut a = base();
        go(&mut a, Section::Hosts);
        a
    }

    /// Every non-modal scenario the drift guard exercises.
    fn scenarios() -> Vec<(&'static str, App)> {
        vec![
            ("hosts", hosts()),
            ("pools middle", pools_middle()),
            ("pools middle, empty", pools_middle_empty()),
            ("pools right", pools_right()),
            ("vdevs, row selected", drilled(true)),
            ("vdevs, nothing selected", drilled(false)),
            ("datasets middle", datasets_middle(false)),
            ("datasets middle, shares loaded", datasets_middle(true)),
            ("datasets right: properties", datasets_right(DatasetDetailTab::Properties)),
            ("datasets right: permissions", datasets_right(DatasetDetailTab::Permissions)),
            ("datasets right: acl", datasets_right(DatasetDetailTab::Acl)),
            ("snapshots", snapshots()),
            ("disks, none selected", disks(false)),
            ("disks, one selected", disks(true)),
            ("shares", shares()),
        ]
    }

    // ---- fingerprint ----------------------------------------------

    /// Everything a key press can change, EXCEPT `status_message`. A key
    /// that only sets a status message ("select a disk first…") is, for
    /// the hint bar's purposes, a no-op: it must not be advertised.
    #[derive(Debug, PartialEq)]
    struct Fingerprint {
        modal_open: bool,
        picker_open: bool,
        section: usize,
        focus: PaneFocus,
        tab: DatasetDetailTab,
        drilled: Option<String>,
        host: usize,
        quit: bool,
        selected_disks: Vec<String>,
        selections: Vec<Option<usize>>,
    }

    fn fingerprint(a: &App) -> Fingerprint {
        let mut disks: Vec<String> = a.selected_disks.iter().cloned().collect();
        disks.sort();
        Fingerprint {
            modal_open: a.modal.is_some(),
            picker_open: a.picker.is_some(),
            section: a.selected_section,
            focus: a.pane_focus,
            tab: a.dataset_detail_tab,
            drilled: a.drilled_pool.clone(),
            host: a.selected_host,
            quit: a.should_quit,
            selected_disks: disks,
            selections: vec![
                a.pools_table.selected(),
                a.datasets_table.selected(),
                a.snapshots_table.selected(),
                a.disks_table.selected(),
                a.shares_table.selected(),
                a.vdevs_table.selected(),
                a.pool_properties_table.selected(),
                a.dataset_properties_table.selected(),
                a.dataset_permissions_table.selected(),
                a.acl_entries_table.selected(),
            ],
        }
    }

    /// Maps a hint's displayed key to the real key it stands for, or
    /// `None` for keys this test cannot or should not press.
    fn key_code(key: &str) -> Option<KeyCode> {
        match key {
            "Enter" => Some(KeyCode::Enter),
            "Esc" => Some(KeyCode::Esc),
            "Tab" => Some(KeyCode::Tab),
            "↑↓" => Some(KeyCode::Down),
            "←" => Some(KeyCode::Left),
            "→" => Some(KeyCode::Right),
            "Space" => Some(KeyCode::Char(' ')),
            "[ ]" => Some(KeyCode::Char(']')),
            // `r` is handled in the event loop (ui/mod.rs), not in
            // `handle_key`, so it cannot be driven from here.
            "r" => None,
            k if k.chars().count() == 1 => k.chars().next().map(KeyCode::Char),
            _ => None,
        }
    }

    // ---- the drift guard ------------------------------------------

    #[test]
    fn every_advertised_key_actually_does_something() {
        for (name, app) in scenarios() {
            for hint in hints(&app) {
                let Some(code) = key_code(hint.key) else { continue };
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                let before = fingerprint(&probe);
                probe.handle_key(code);
                assert_ne!(
                    fingerprint(&probe),
                    before,
                    "scenario '{name}' advertises '{} {}' but pressing {code:?} changes nothing",
                    hint.key,
                    hint.label
                );
            }
        }
    }

    #[test]
    fn keys_that_are_not_advertised_are_no_ops() {
        let probes = ['s', 'R', 'd', 'n', 'N', 'b', 'a', 'x', 'o', 'O', 'c', ' ', '[', ']'];
        for (name, app) in scenarios() {
            let advertised: Vec<KeyCode> = hints(&app).iter().filter_map(|h| key_code(h.key)).collect();
            for ch in probes {
                let code = KeyCode::Char(ch);
                // `[ ]` is advertised as a pair; treat both brackets as advertised together.
                let bracket_pair_advertised = (ch == '[' || ch == ']') && advertised.contains(&KeyCode::Char(']'));
                if advertised.contains(&code) || bracket_pair_advertised {
                    continue;
                }
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                let before = fingerprint(&probe);
                probe.handle_key(code);
                assert_eq!(fingerprint(&probe), before, "scenario '{name}' does NOT advertise {ch:?} yet pressing it changes state");
            }
            // Enter, when not advertised, must also be inert.
            if !advertised.contains(&KeyCode::Enter) {
                let mut probe = scenarios().into_iter().find(|(n, _)| *n == name).unwrap().1;
                let before = fingerprint(&probe);
                probe.handle_key(KeyCode::Enter);
                assert_eq!(fingerprint(&probe), before, "scenario '{name}' does NOT advertise Enter yet it changes state");
            }
        }
    }

    // ---- content ---------------------------------------------------

    fn keys(app: &App) -> Vec<&'static str> {
        hints(app).iter().map(|h| h.key).collect()
    }

    #[test]
    fn pools_middle_advertises_the_pool_actions_and_marks_destroy_dangerous() {
        let app = pools_middle();
        let hs = hints(&app);
        for k in ["Enter", "s", "R", "d", "n", "→"] {
            assert!(hs.iter().any(|h| h.key == k), "missing {k}: {hs:?}");
        }
        let destroy = hs.iter().find(|h| h.key == "d").unwrap();
        assert_eq!(destroy.tone, Tone::Danger);
    }

    #[test]
    fn pool_actions_are_hidden_while_the_right_pane_has_focus() {
        let ks = keys(&pools_right());
        for k in ["s", "R", "d", "n"] {
            assert!(!ks.contains(&k), "{k} must be hidden: {ks:?}");
        }
    }

    #[test]
    fn pool_actions_are_hidden_when_no_pool_is_selected() {
        let ks = keys(&pools_middle_empty());
        for k in ["s", "R", "d", "Enter", "↑↓"] {
            assert!(!ks.contains(&k), "{k} must be hidden: {ks:?}");
        }
        assert!(ks.contains(&"n"));
    }

    #[test]
    fn global_hints_are_always_present_and_pinned() {
        for (name, app) in scenarios() {
            let hs = hints(&app);
            for k in ["?", "q"] {
                let h = hs.iter().find(|h| h.key == k).unwrap_or_else(|| panic!("{name}: missing {k}"));
                assert!(h.pinned, "{name}: {k} must be pinned");
            }
        }
    }

    #[test]
    fn hints_for_the_hosts_screen_do_not_include_refresh() {
        // `r` is a no-op in the Hosts section (see event_loop's refresh match).
        assert!(!keys(&hosts()).contains(&"r"));
    }

    #[test]
    fn a_modal_shows_only_its_own_keys() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Info { title: "Help".to_string(), body: String::new() });
        let hs = hints(&app);
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].key, "any key");
        assert!(!hs.iter().any(|h| h.pinned));
    }

    #[test]
    fn confirm_hint_inherits_the_actions_tone() {
        let mut app = pools_middle();
        app.modal = Some(Modal::ConfirmTyped {
            prompt: "Type the pool name to destroy 'tank':".to_string(),
            expected: "tank".to_string(),
            input: String::new(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::DestroyPool { pool: "tank".to_string() } },
        });
        let hs = hints(&app);
        let confirm = hs.iter().find(|h| h.label == "Confirm").unwrap();
        assert_eq!(confirm.tone, Tone::Danger);
        assert!(hs.iter().any(|h| h.key == "type"));
        assert!(hs.iter().any(|h| h.key == "Esc"));
    }

    #[test]
    fn plain_confirm_shows_y_and_n() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Confirm {
            prompt: "Scrub pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
        });
        let hs = hints(&app);
        assert!(hs.iter().any(|h| h.key == "y/Enter"));
        assert!(hs.iter().any(|h| h.key == "n/Esc"));
        assert_eq!(hs.iter().find(|h| h.key == "y/Enter").unwrap().tone, Tone::Neutral);
    }

    #[test]
    fn form_enter_label_is_next_until_the_last_field_then_submit() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Form {
            title: "Create Pool".to_string(),
            fields: vec![
                FormField { label: "Name".to_string(), value: String::new() },
                FormField { label: "Vdev spec".to_string(), value: String::new() },
            ],
            focused: 0,
            description: None,
            kind: FormKind::CreatePool { host: 0 },
        });
        assert_eq!(hints(&app).iter().find(|h| h.key == "Enter").unwrap().label, "Next");
        if let Some(Modal::Form { focused, .. }) = &mut app.modal {
            *focused = 1;
        }
        assert_eq!(hints(&app).iter().find(|h| h.key == "Enter").unwrap().label, "Submit");
    }

    #[test]
    fn form_shows_f2_pick_only_on_a_field_with_a_picker() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Form {
            title: "Grant".to_string(),
            fields: vec![
                FormField { label: "Recipient".to_string(), value: String::new() },
                FormField { label: "Scope".to_string(), value: String::new() },
                FormField { label: "Permissions (F2 to pick)".to_string(), value: String::new() },
            ],
            focused: 0,
            description: None,
            kind: FormKind::GrantPermission { host: 0, dataset: "tank/a".to_string() },
        });
        assert!(!hints(&app).iter().any(|h| h.key == "F2"), "field 0 has no vocabulary");
        if let Some(Modal::Form { focused, .. }) = &mut app.modal {
            *focused = 2;
        }
        assert!(hints(&app).iter().any(|h| h.key == "F2"), "field 2 has a vocabulary");
    }

    #[test]
    fn an_open_picker_wins_over_the_form_underneath() {
        let mut app = pools_middle();
        app.modal = Some(Modal::Info { title: "x".to_string(), body: String::new() });
        app.picker = Some(crate::ui::picker::PickerState {
            values: vec!["a".to_string()],
            filter: String::new(),
            selected: 0,
            checked: Default::default(),
            multi_select: true,
        });
        let ks = keys(&app);
        assert!(ks.contains(&"Space"), "{ks:?}");
        assert!(!ks.contains(&"any key"), "{ks:?}");
    }

    // ---- fit / render ----------------------------------------------

    fn sample() -> Vec<Hint> {
        vec![
            Hint { key: "s", label: "Scrub", priority: 2, pinned: false, tone: Tone::Neutral },
            Hint { key: "d", label: "Destroy", priority: 2, pinned: false, tone: Tone::Danger },
            Hint { key: "Tab", label: "Section", priority: 5, pinned: false, tone: Tone::Neutral },
            Hint { key: "?", label: "Help", priority: 0, pinned: true, tone: Tone::Neutral },
            Hint { key: "q", label: "Quit", priority: 0, pinned: true, tone: Tone::Neutral },
        ]
    }

    #[test]
    fn fit_keeps_everything_when_it_fits() {
        assert_eq!(fit(&sample(), 200), sample());
    }

    #[test]
    fn fit_drops_the_lowest_priority_hint_first_and_never_a_pinned_one() {
        // Full width: "s Scrub  d Destroy  Tab Section  ? Help  q Quit" = 47.
        let kept = fit(&sample(), 40);
        assert!(!kept.iter().any(|h| h.key == "Tab"), "{kept:?}"); // priority 5 goes first
        assert!(kept.iter().any(|h| h.key == "?") && kept.iter().any(|h| h.key == "q"));
    }

    #[test]
    fn fit_among_equal_priorities_drops_the_rightmost() {
        let kept = fit(&sample(), 31); // forces Tab, then one of s/d (priority 2) out
        assert!(kept.iter().any(|h| h.key == "s"), "{kept:?}");
        assert!(!kept.iter().any(|h| h.key == "d"), "{kept:?}");
    }

    #[test]
    fn fit_at_a_hopeless_width_keeps_only_the_pinned_hints() {
        let kept = fit(&sample(), 3);
        assert_eq!(kept.iter().map(|h| h.key).collect::<Vec<_>>(), vec!["?", "q"]);
    }

    #[test]
    fn hint_line_styles_danger_keys_with_the_danger_style_and_labels_muted() {
        let theme = crate::ui::theme::Theme::ansi();
        let line = hint_line(&sample(), 200, &theme);
        let d = line.spans.iter().find(|s| s.content == "d").unwrap();
        assert_eq!(d.style, theme.danger);
        let label = line.spans.iter().find(|s| s.content == " Destroy").unwrap();
        assert_eq!(label.style, theme.muted);
        let plain = line.spans.iter().find(|s| s.content == "s").unwrap();
        assert_ne!(plain.style, Style::new());
    }
}
```

Run: `cargo test ui::hints 2>&1 | tail -8` — expected: compile errors (`hints`, `fit`, `Hint`, `hint_line` not defined).

- [ ] **Step 3: Implement**

Prepend to `src/ui/hints.rs`, above the test module:

```rust
//! The one-line key-hint bar. `hints()` is a pure function of `&App`
//! that mirrors what `App::handle_key` / `handle_modal_key` /
//! `handle_picker_key` actually do — the tests in this file drive the
//! real handlers to keep the two from drifting apart.

use crate::ui::modal::{Modal, Tone};
use crate::ui::picker::{build_picker_state, PickerState};
use crate::ui::state::{App, DatasetDetailTab, PaneFocus, Section};
use crate::ui::theme::Theme;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

/// One `key label` pair. Lower `priority` = more important; `fit` drops
/// the highest number first (rightmost among ties). `pinned` hints are
/// never dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: u8,
    pub pinned: bool,
    pub tone: Tone,
}

fn h(key: &'static str, label: &'static str, priority: u8) -> Hint {
    Hint { key, label, priority, pinned: false, tone: Tone::Neutral }
}

impl Hint {
    fn pinned(mut self) -> Hint {
        self.pinned = true;
        self
    }

    fn tone(mut self, tone: Tone) -> Hint {
        self.tone = tone;
        self
    }
}

/// Precedence: picker, then modal, then (drilled pool | section → pane →
/// detail tab). `? Help` and `q Quit` are pinned on every non-modal
/// screen; a modal or picker shows only its own keys.
pub fn hints(app: &App) -> Vec<Hint> {
    if let Some(picker) = &app.picker {
        return picker_hints(picker);
    }
    if let Some(modal) = &app.modal {
        return modal_hints(modal);
    }
    let mut v = context_hints(app);
    v.push(h("A", "About", 9));
    v.push(h("?", "Help", 0).pinned());
    v.push(h("q", "Quit", 0).pinned());
    v
}

fn picker_hints(picker: &PickerState) -> Vec<Hint> {
    let mut v = vec![h("↑↓", "Move", 2), h("type", "Filter", 3)];
    if picker.multi_select {
        v.push(h("Space", "Toggle", 1));
    }
    v.push(h("Enter", "OK", 0));
    v.push(h("Esc", "Cancel", 0));
    v
}

fn modal_hints(modal: &Modal) -> Vec<Hint> {
    match modal {
        Modal::Info { .. } => vec![h("any key", "Close", 0)],
        Modal::Confirm { action, .. } => {
            vec![h("y/Enter", "Confirm", 0).tone(action.call.tone()), h("n/Esc", "Cancel", 0)]
        }
        Modal::ConfirmTyped { action, .. } => vec![
            h("type", "name to confirm", 2),
            h("Enter", "Confirm", 0).tone(action.call.tone()),
            h("Esc", "Cancel", 0),
        ],
        Modal::Form { fields, focused, kind, .. } => {
            let enter = if focused + 1 < fields.len() { "Next" } else { "Submit" };
            let mut v = vec![h("Enter", enter, 0), h("Tab", "Next field", 2), h("Shift+Tab", "Prev field", 3)];
            let value = fields.get(*focused).map(|f| f.value.as_str()).unwrap_or("");
            if build_picker_state(kind, *focused, value).is_some() {
                v.push(h("F2", "Pick", 1));
            }
            v.push(h("Esc", "Cancel", 0));
            v
        }
    }
}

fn context_hints(app: &App) -> Vec<Hint> {
    let mut v = Vec::new();

    if app.drilled_pool.is_some() {
        if !app.vdevs.is_empty() {
            v.push(h("↑↓", "Move", 1));
        }
        if app.vdevs_table.selected().is_some() {
            v.push(h("o", "Offline", 2));
            v.push(h("O", "Online", 2));
            v.push(h("x", "Replace", 2));
        }
        v.push(h("a", "Add vdev", 3));
        v.push(h("r", "Refresh", 4));
        v.push(h("Esc", "Back", 1));
        return v;
    }

    let section = app.current_section();
    let right = app.pane_focus == PaneFocus::Right;
    match (section, right) {
        (Section::Hosts, _) => {
            if !app.hosts.is_empty() {
                v.push(h("↑↓", "Host", 1));
            }
            v.push(h("n", "Add host", 2));
        }
        (Section::Pools, false) => {
            let selected = app.pools_table.selected().is_some();
            if !app.pools.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if selected {
                v.push(h("Enter", "Vdevs", 1));
            }
            v.push(h("→", "Properties", 2));
            if selected {
                v.push(h("s", "Scrub", 2));
                v.push(h("R", "Resilver", 3));
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
            }
            v.push(h("n", "New pool", 2));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Pools, true) => {
            if !app.pool_properties.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.pool_properties_table.selected().is_some() {
                v.push(h("Enter", "Edit", 1));
            }
            v.push(h("←", "Back", 1));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Datasets, false) => {
            let selected = app.datasets_table.selected().is_some();
            if !app.datasets.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            v.push(h("→", "Details", 2));
            v.push(h("n", "New", 2));
            if selected {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("s", "Snapshot", 2));
                if app.shares_loaded && app.nfs_available != Some(false) {
                    v.push(h("N", "NFS share", 3));
                }
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Datasets, true) => {
            match app.dataset_detail_tab {
                DatasetDetailTab::Properties => {
                    if !app.dataset_properties.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.dataset_properties_table.selected().is_some() {
                        v.push(h("Enter", "Edit", 1));
                    }
                }
                DatasetDetailTab::Permissions => {
                    if !app.dataset_permissions.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.selected_dataset_name().is_some() {
                        v.push(h("n", "Grant", 2));
                        v.push(h("s", "Grant set", 3));
                        v.push(h("c", "Create-time", 3));
                        if app.dataset_permissions_table.selected().is_some() {
                            v.push(h("d", "Revoke all", 2).tone(Tone::Warn));
                            v.push(h("x", "Revoke some", 3));
                        }
                    }
                }
                DatasetDetailTab::Acl => {
                    if !app.acl_entries.is_empty() {
                        v.push(h("↑↓", "Move", 1));
                    }
                    if app.selected_dataset_mountpoint().is_some() {
                        v.push(h("n", "Add entry", 2));
                        if app.acl_entries_table.selected().is_some_and(|i| i < app.acl_entries.len()) {
                            v.push(h("d", "Remove", 2).tone(Tone::Warn));
                        }
                    }
                }
            }
            v.push(h("[ ]", "Tabs", 3));
            v.push(h("←", "Back", 1));
            v.push(h("r", "Refresh", 4));
        }
        (Section::Snapshots, _) => {
            if !app.snapshots.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.snapshots_table.selected().is_some() {
                v.push(h("d", "Destroy", 2).tone(Tone::Danger));
                v.push(h("b", "Rollback", 2).tone(Tone::Danger));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Disks, _) => {
            if !app.disks.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.disks_table.selected().is_some() {
                v.push(h("Space", "Select", 1));
            }
            if !app.selected_disks.is_empty() {
                v.push(h("n", "New pool", 2));
                v.push(h("a", "Add vdev", 2));
                v.push(h("Esc", "Clear", 3));
            }
            v.push(h("r", "Refresh", 4));
        }
        (Section::Shares, _) => {
            if !app.shares.is_empty() {
                v.push(h("↑↓", "Move", 1));
            }
            if app.shares_table.selected().is_some() {
                v.push(h("Enter", "Edit", 1));
                v.push(h("d", "Remove", 2).tone(Tone::Warn));
            }
            v.push(h("r", "Refresh", 4));
        }
        // Never in `App::sections`; listed so a future section forces a decision here.
        (Section::Vdevs | Section::PoolProperties | Section::DatasetProperties | Section::DatasetPermissions | Section::DatasetAcl, _) => {}
    }
    v.push(h("Tab", "Section", 5));
    v
}

fn total_width(hints: &[Hint]) -> usize {
    if hints.is_empty() {
        return 0;
    }
    hints.iter().map(|x| x.key.chars().count() + 1 + x.label.chars().count()).sum::<usize>() + 2 * (hints.len() - 1)
}

/// Drops hints until the rest fit in `width` columns: highest `priority`
/// number first, rightmost among ties, never a pinned one. Display order
/// of the survivors is unchanged.
pub fn fit(hints: &[Hint], width: usize) -> Vec<Hint> {
    let mut kept: Vec<Hint> = hints.to_vec();
    while total_width(&kept) > width {
        let victim = kept
            .iter()
            .enumerate()
            .filter(|(_, hint)| !hint.pinned)
            .max_by_key(|(i, hint)| (hint.priority, *i))
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                kept.remove(i);
            }
            None => break,
        }
    }
    kept
}

pub fn hint_line(hints: &[Hint], width: usize, theme: &Theme) -> Line<'static> {
    let kept = fit(hints, width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, hint) in kept.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let key_style = match hint.tone {
            Tone::Neutral => theme.accent.add_modifier(Modifier::BOLD),
            Tone::Warn => theme.warn.add_modifier(Modifier::BOLD),
            Tone::Danger => theme.danger,
        };
        spans.push(Span::styled(hint.key, key_style));
        spans.push(Span::styled(format!(" {}", hint.label), theme.muted));
    }
    Line::from(spans)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test ui::hints 2>&1 | tail -20`

Expected: all PASS. The two drift guards are the point of this task; if one fails, read its message — it names the scenario and the key. There are exactly two legitimate fixes, and you must pick the right one by reading `handle_key` in `src/ui/state.rs`:
- the hint is **wrong** (key is a no-op in that state) → change the condition in `context_hints` so it is hidden;
- the key **does** act and the hint is merely missing → add the hint.

Never weaken a drift test or add the key to an ignore list to make it pass. (`r` is already excluded by design — it lives in `ui/mod.rs`'s event loop.) Known subtlety: the `Left` hint `←` in the right pane relies on `pane_focus == Right`; the `[ ]` hint presses `]`.

- [ ] **Step 5: Commit**

```bash
git add src/ui/hints.rs src/ui/mod.rs
git commit -m "feat: per-focus hint model with drift-guard tests against handle_key

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 7: New bottom layout — status line above a hint bar

**Files:**
- Modify: `src/ui/layout.rs` (`draw`, `status_bar_content`, `draw_status_bar` → `draw_status_line`, `draw_sidebar`)
- Modify: `src/ui/state.rs` (`status_error`, `set_error`, `status_is_error`; route failure sites)
- Test: `src/ui/layout.rs` tests, `src/ui/state.rs` tests

**Interfaces:**
- Consumes: `hints::{hints, hint_line}` (Task 6), `Theme` (Task 1), the `render` test helper and `app_in_pools` (Task 4).
- Produces: `App::set_error(&mut self, msg: String)`, `App::status_is_error(&self) -> bool`; `const STATUS_MAX_ROWS: u16 = 3` in `layout.rs`.

- [ ] **Step 1: Write the failing `App` tests**

In `src/ui/state.rs`'s `mod tests` append:

```rust
    #[test]
    fn set_error_marks_the_current_status_message_as_an_error() {
        let mut app = App::new(vec!["local".to_string()]);
        assert!(!app.status_is_error());
        app.set_error("zpool destroy failed".to_string());
        assert_eq!(app.status_message.as_deref(), Some("zpool destroy failed"));
        assert!(app.status_is_error());
    }

    #[test]
    fn a_later_ordinary_message_is_not_styled_as_an_error() {
        let mut app = App::new(vec!["local".to_string()]);
        app.set_error("boom".to_string());
        app.status_message = Some("select a disk with Space first".to_string());
        assert!(!app.status_is_error(), "stale error flag must not recolor an unrelated message");
    }

    #[test]
    fn clearing_the_message_clears_the_error_state() {
        let mut app = App::new(vec!["local".to_string()]);
        app.set_error("boom".to_string());
        app.handle_key(crossterm::event::KeyCode::Esc);
        assert_eq!(app.status_message, None);
        assert!(!app.status_is_error());
    }
```

In `src/ui/layout.rs` tests append (uses `render` / `app_in_pools` from Task 4):

```rust
    #[test]
    fn hint_bar_is_the_last_row_and_shows_the_pinned_keys() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24);
        let last = &rows[23];
        assert!(last.contains("? Help"), "{last:?}");
        assert!(last.contains("q Quit"), "{last:?}");
        assert!(last.contains("s Scrub"), "{last:?}");
    }

    #[test]
    fn the_hint_bar_follows_focus() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        assert!(render(&mut app, 100, 24)[23].contains("s Scrub"));
        app.pane_focus = PaneFocus::Right;
        let last = render(&mut app, 100, 24)[23].clone();
        assert!(!last.contains("s Scrub"), "{last:?}");
        assert!(last.contains("← Back"), "{last:?}");
    }

    #[test]
    fn status_message_sits_directly_above_the_hint_bar() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.status_message = Some("hello status".to_string());
        let rows = render(&mut app, 80, 24);
        assert!(rows[22].contains("hello status"), "{:?}", rows[22]);
        assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
    }

    #[test]
    fn with_no_message_or_scan_the_status_line_takes_no_row() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 80, 24);
        // The panes' bottom border is on row 22, directly above the hint bar.
        assert!(rows[22].contains('└'), "{:?}", rows[22]);
        assert!(rows[23].contains("Quit"));
    }

    #[test]
    fn a_very_long_status_message_is_capped_and_cannot_push_the_hint_bar_off_screen() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.status_message = Some((0..30).map(|i| format!("failure line {i}")).collect::<Vec<_>>().join("\n"));
        let rows = render(&mut app, 80, 24);
        assert!(rows[23].contains("Quit"), "{:?}", rows[23]);
        assert!(rows[20].contains("failure line 0"), "{:?}", rows[20]);
        assert!(rows[22].contains("failure line 2"), "{:?}", rows[22]);
        assert!(!rows.join("\n").contains("failure line 3"), "capped at STATUS_MAX_ROWS");
        assert!(rows[0].contains("zfsctl"), "the sidebar must still render");
    }

    #[test]
    fn the_sidebar_no_longer_has_its_own_help_footer() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 100, 24);
        // "? Help" now appears only once, in the bottom bar.
        let occurrences = rows.iter().filter(|r| r.contains("? Help")).count();
        assert_eq!(occurrences, 1, "{rows:#?}");
    }

    #[test]
    fn drawing_never_panics_on_a_tiny_terminal() {
        for (w, h) in [(20, 4), (30, 6), (10, 3), (1, 1), (80, 2)] {
            let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
            app.status_message = Some("a message that is far wider than this terminal".to_string());
            let _ = render(&mut app, w, h);
        }
    }

    #[test]
    fn tiny_terminals_still_show_the_pinned_hints() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let rows = render(&mut app, 30, 8);
        assert!(rows[7].contains("? Help"), "{:?}", rows[7]);
        assert!(rows[7].contains("q Quit"), "{:?}", rows[7]);
    }

    #[test]
    fn failure_messages_use_the_err_style_and_ordinary_ones_do_not() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        let theme = app.theme.clone();
        let style_of_row22 = |app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            terminal.backend().buffer()[(0, 22)].style()
        };
        app.set_error("zpool destroy failed".to_string());
        assert_eq!(style_of_row22(&mut app).fg, theme.err.fg);
        app.status_message = Some("select a disk with Space first".to_string());
        assert_eq!(style_of_row22(&mut app).fg, None);
    }
```

Run: `cargo test ui::layout ui::state::tests::set_error 2>&1 | tail -10` — expected: compile errors (`set_error`, `status_is_error`).

- [ ] **Step 2: `App` error-state plumbing**

In `src/ui/state.rs`, add a field after `pub status_message: Option<String>,`:

```rust
    /// The text of the status message that was set via `set_error`.
    /// Compared against `status_message` rather than kept as a bool, so
    /// it invalidates itself the moment the message is replaced or
    /// cleared — a stale flag can never recolor an unrelated message.
    status_error: Option<String>,
```
In `App::new`, after `status_message: None,` add `status_error: None,`.

Add the methods next to `active_host_name`:

```rust
    /// Sets a failure message (rendered with the theme's `err` style).
    pub fn set_error(&mut self, msg: String) {
        self.status_error = Some(msg.clone());
        self.status_message = Some(msg);
    }

    pub fn status_is_error(&self) -> bool {
        self.status_message.is_some() && self.status_message == self.status_error
    }
```

Route the failure sites through it. The result-handler `Err` arms are all textually identical:

```bash
grep -c 'Err(e) => self.status_message = Some(e.to_string()),' src/ui/state.rs   # expect 13
sed -i 's/Err(e) => self\.status_message = Some(e\.to_string()),/Err(e) => self.set_error(e.to_string()),/' src/ui/state.rs
```
Then three hand edits:
- `~line 1727`: `self.status_message = Some(format!("Could not add host '{name}': {e}"));` → `self.set_error(format!("Could not add host '{name}': {e}"));`
- `~line 1138`: the form's `Could not submit -- check the field values…` message → `self.set_error("Could not submit -- check the field values against the hints in their labels".to_string());`
- `~line 1126`: `A host named '{name}' already exists or is already being added` → `self.set_error(format!(…))`.

(`apply_pending_new_hosts` in `ui/mod.rs` sets "Host added, but could not save it…"; make that `app.set_error(format!(…))` too.)

Run: `cargo test ui::state 2>&1 | tail -4` — expected 0 failed (existing tests assert on `status_message`, which is unchanged).

- [ ] **Step 3: Rewrite the bottom of `draw`**

In `src/ui/layout.rs` replace `draw`, `status_bar_content`, `draw_status_bar`:

```rust
/// Most rows the status line may take (excluding the gauge row). A
/// narrowing from the old bordered box's 9: a long multi-line command
/// failure is now clipped after this many lines. Esc still dismisses it.
const STATUS_MAX_ROWS: u16 = 3;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let (status_text, status_style, gauge) = status_bar_content(app);
    let text_rows = if status_text.is_empty() {
        0
    } else {
        wrapped_line_count(&status_text, area.width).clamp(1, STATUS_MAX_ROWS)
    };
    let gauge_rows = if gauge.is_some() { 1 } else { 0 };
    let status_height = text_rows + gauge_rows;

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(status_height), Constraint::Length(1)])
        .split(area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(outer[0]);

    draw_sidebar(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_status_line(f, &status_text, status_style, gauge, &app.theme, outer[1]);
    let hint_line = crate::ui::hints::hint_line(&crate::ui::hints::hints(app), area.width as usize, &app.theme);
    f.render_widget(Paragraph::new(hint_line), outer[2]);

    if let Some(modal) = app.modal.clone() {
        draw_modal(f, &modal, area);
    }
    if let Some(picker) = app.picker.clone() {
        draw_picker(f, &picker, &app.theme, area);
    }
}

/// Builds the status line's text, style, and (while a resilver/scrub is
/// running and has reported a percentage) progress-gauge info: the
/// transient action-result message (if any), followed by the
/// drilled-into pool's scrub/resilver status (if any) on its own line.
/// A scan in progress uses the theme's `info` style, a scan that
/// completed with errors uses `err`; a failure message uses `err`. A clean
/// or absent scan status leaves the default style, since it isn't worth
/// drawing attention to.
fn status_bar_content(app: &App) -> (String, Style, Option<(f64, String)>) {
    let mut lines: Vec<String> = Vec::new();
    let mut style = Style::default();
    if let Some(msg) = &app.status_message {
        lines.push(msg.clone());
        if app.status_is_error() {
            style = app.theme.err;
        }
    }
    let mut gauge = None;
    if let Some(scan) = &app.pool_scan_status {
        let (summary, scan_style) = match scan {
            ScanStatus::InProgress { summary, .. } => (summary, app.theme.info),
            ScanStatus::Completed { summary, errors: true } => (summary, app.theme.err),
            ScanStatus::Completed { summary, errors: false } => (summary, Style::default()),
        };
        lines.push(summary.clone());
        style = scan_style;
        if let ScanStatus::InProgress { percent_done: Some(pct), eta, .. } = scan {
            let label = match eta {
                Some(e) => format!("{pct:.2}% — ETA {e}"),
                None => format!("{pct:.2}%"),
            };
            gauge = Some((*pct, label));
        }
    }
    (lines.join("\n"), style, gauge)
}

fn draw_status_line(f: &mut Frame, text: &str, style: Style, gauge: Option<(f64, String)>, theme: &Theme, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Some((percent, label)) = gauge else {
        f.render_widget(Paragraph::new(text).style(style).wrap(Wrap { trim: false }), area);
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(text).style(style).wrap(Wrap { trim: false }), rows[0]);
    let progress = Gauge::default()
        .gauge_style(theme.info)
        .ratio((percent / 100.0).clamp(0.0, 1.0))
        .label(label);
    f.render_widget(progress, rows[1]);
}
```

Replace `draw_sidebar`'s layout (remove the 2-line footer — the list now takes the whole area):

```rust
fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme.clone();
    let items: Vec<ListItem> = app
        .sections
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = match s {
                Section::Hosts => "Hosts",
                Section::Pools => "Pools",
                Section::Datasets => "Datasets",
                Section::Snapshots => "Snapshots",
                Section::Disks => "Disks",
                Section::Shares => "Shares",
                Section::Vdevs => "Vdevs",
                Section::PoolProperties => "Pool Properties",
                Section::DatasetProperties => "Dataset Properties",
                Section::DatasetPermissions => "Dataset Permissions",
                Section::DatasetAcl => "Dataset ACL",
            };
            let style = if i == app.selected_section { theme.selected_row() } else { Style::default() };
            ListItem::new(label).style(style)
        })
        .collect();
    let title = if app.is_loading() { "zfsctl (refreshing...)" } else { "zfsctl" };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}
```

Delete the now-unused `Color`/`Modifier` imports and the `Wrap`/`Gauge` imports only if the compiler warns they are unused (they are still used above).

- [ ] **Step 4: Run the tests**

Run: `cargo test 2>&1 | tail -6`
Expected: 0 failed. If `a_very_long_status_message…` fails on the exact row numbers, the layout arithmetic is the cause (rows 20–22 are the 3 status rows at height 24 with a 1-row hint bar) — fix `draw`, not the test.

- [ ] **Step 5: Commit**

```bash
git add src/ui/layout.rs src/ui/state.rs src/ui/mod.rs
git commit -m "feat: compact status line above a per-focus hint bar; error-styled failures

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Danger and warn modal styling

**Files:**
- Modify: `src/ui/layout.rs` (`draw`, `draw_modal`)
- Test: `src/ui/layout.rs` tests

**Interfaces:**
- Consumes: `MutationCall::tone()/label()`, `Tone` (Task 5), `Theme`, the `render` helper (Task 4).
- Produces: `draw_modal(f, modal, theme, area)`.

- [ ] **Step 1: Write the failing tests**

Append to `src/ui/layout.rs` tests:

```rust
    use crate::ui::modal::{Modal, MutationCall, PendingAction};

    fn buffer_of(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn style_of_symbol(buf: &ratatui::buffer::Buffer, symbol: &str) -> Option<Style> {
        buf.content().iter().find(|c| c.symbol() == symbol).map(|c| c.style())
    }

    fn destroy_pool_modal() -> Modal {
        Modal::ConfirmTyped {
            prompt: "Type the pool name to destroy 'tank':".to_string(),
            expected: "tank".to_string(),
            input: "ta".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::DestroyPool { pool: "tank".to_string() } },
        }
    }

    #[test]
    fn a_destroy_modal_is_titled_with_a_glyph_and_the_action_and_drawn_in_red() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
        let buf = buffer_of(&mut app, 100, 30);
        assert_eq!(style_of_symbol(&buf, "✖").unwrap().fg, Some(Color::Red));
    }

    #[test]
    fn a_destroy_modal_still_shows_the_typed_input_line() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("> ta"), "{rows}");
    }

    #[test]
    fn a_warn_confirm_is_titled_with_a_triangle_and_drawn_in_yellow() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(Modal::Confirm {
            prompt: "Offline disk 'da0' in pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Vdevs, call: MutationCall::OfflineDisk { pool: "tank".to_string(), disk: "da0".to_string() } },
        });
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("▲ OFFLINE DISK"), "{rows}");
        let buf = buffer_of(&mut app, 100, 30);
        assert_eq!(style_of_symbol(&buf, "▲").unwrap().fg, Some(Color::Yellow));
    }

    #[test]
    fn a_routine_confirm_keeps_its_plain_title() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.modal = Some(Modal::Confirm {
            prompt: "Scrub pool 'tank'? (y/n)".to_string(),
            action: PendingAction { host: 0, section: Section::Pools, call: MutationCall::ScrubPool { pool: "tank".to_string() } },
        });
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("Confirm (y/n)"), "{rows}");
        assert!(!rows.contains('✖'));
    }

    #[test]
    fn the_danger_modal_is_distinguishable_without_color_in_the_mono_theme() {
        let mut app = app_in_pools(vec![pool("tank", 10, 100, PoolHealth::Online)]);
        app.theme = Theme::mono();
        app.modal = Some(destroy_pool_modal());
        let rows = render(&mut app, 100, 30).join("\n");
        assert!(rows.contains("✖ DESTROY POOL"), "{rows}");
    }
```

Run: `cargo test ui::layout 2>&1 | tail -10` — expected: the five new tests FAIL (titles are still `Type to confirm (Enter/Esc)`).

- [ ] **Step 2: Implement**

Update `draw`: `draw_modal(f, &modal, &app.theme, area);`.

Add imports `use crate::ui::modal::Tone; use ratatui::style::Modifier; use ratatui::text::Line;` (`Span` is already imported from Task 4).

Add a helper above `draw_modal`:

```rust
/// Border and title for a Confirm/ConfirmTyped modal. Danger and warn
/// get a glyph plus the action's name in the title (so they read as such
/// without color) and the theme's danger/warn style on the border.
fn confirm_block(theme: &Theme, tone: Tone, label: &str, neutral_title: &str, risky_suffix: &str) -> Block<'static> {
    let (glyph, border) = match tone {
        Tone::Danger => ("✖", theme.danger),
        Tone::Warn => ("▲", theme.warn),
        Tone::Neutral => ("", Style::default()),
    };
    let title = if tone == Tone::Neutral { neutral_title.to_string() } else { format!("{glyph} {label} — {risky_suffix}") };
    Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(title, border))
}
```

Change `draw_modal`'s signature to `fn draw_modal(f: &mut Frame, modal: &Modal, theme: &Theme, area: Rect)` and replace the `Confirm` and `ConfirmTyped` render arms (leave the height computation above them untouched — the line counts are identical):

```rust
        Modal::Confirm { prompt, action } => {
            let block = confirm_block(theme, action.call.tone(), action.call.label(), "Confirm (y/n)", "confirm (y/n)");
            let p = Paragraph::new(prompt.as_str()).wrap(Wrap { trim: false }).block(block);
            f.render_widget(p, popup_area);
        }
        Modal::ConfirmTyped { prompt, input, action, .. } => {
            let block = confirm_block(
                theme,
                action.call.tone(),
                action.call.label(),
                "Type to confirm (Enter/Esc)",
                "type to confirm (Enter/Esc)",
            );
            let mut lines: Vec<Line> = prompt.lines().map(|l| Line::raw(l.to_string())).collect();
            lines.push(Line::raw(""));
            lines.push(Line::styled(format!("> {input}"), Style::default().add_modifier(Modifier::BOLD)));
            let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(block);
            f.render_widget(p, popup_area);
        }
```

For `Form`, give the block the accent border:

```rust
                .block(Block::default().borders(Borders::ALL).border_style(theme.accent).title(title.as_str()));
```
(`Info` stays neutral.)

- [ ] **Step 3: Run the tests**

Run: `cargo test 2>&1 | tail -5`
Expected: 0 failed.

- [ ] **Step 4: Commit**

```bash
git add src/ui/layout.rs
git commit -m "feat: distinct red/yellow confirmation style for destructive and disruptive actions

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Docs and end-to-end verification

**Files:**
- Modify: `README.md` (after the paragraph ending "…if it detects only "local" is configured." near line 30)
- Verify: whole branch

**Interfaces:** none.

- [ ] **Step 1: Document the feature**

Insert after the multi-host paragraph in `README.md`:

````markdown
### Themes and the key-hint bar

The bottom row of every screen lists the keys that work right now for whatever has focus (it changes as you move between sections, panes, detail tabs, modals and pickers; keys that would do nothing are hidden). A status message, when there is one, appears on the line above it.

Color carries meaning, never decoration: pool and vdev health are `● ONLINE` (green), `▲ DEGRADED` (yellow), `✖ FAULTED`/`UNAVAIL`/`REMOVED` (red), `○ OFFLINE` (gray); pool capacity bars turn yellow at 70% and red above 85% (`!` suffix); resilver/scrub progress is cyan; destructive confirmations (destroy, rollback) are red with a `✖` title and disruptive ones (offline, replace, revoke, remove) are yellow with `▲`. Every state also has a glyph and a word, so nothing depends on color alone.

Pick a theme in `config.toml` (optional):

```toml
[ui]
theme = "ansi"   # ansi (default) | mono | solarized-dark | solarized-light
```

- `ansi` uses only the 16 terminal colors, so your terminal's own theme applies.
- `mono` uses no color at all (bold/dim/reverse and the glyphs). It is also chosen automatically when `NO_COLOR` is set to a non-empty value.
- `solarized-dark` / `solarized-light` use the Solarized palette (a tribute to Sun, who gave us ZFS). They want a truecolor terminal (`COLORTERM=truecolor`); otherwise Solarized's 256-color approximations are used and a note says so at startup.
- An unrecognized value is a startup error that lists the valid names.
````

- [ ] **Step 2: Full automated verification**

Run each; all must be clean:

```bash
cargo build 2>&1 | grep -E "^(warning|error)" | sort | uniq -c    # expect no output
cargo test 2>&1 | tail -4                                        # expect: 0 failed
cargo clippy --all-targets 2>&1 | tail -5                        # only if clippy is installed; no NEW warnings vs. before this branch
```

Run: `grep -rn "Color::" src/ui/layout.rs | grep -v "mod tests" | head`
Expected: no output outside the test module (the spec's "no `Color` in widget code" constraint).

- [ ] **Step 3: Live end-to-end check (per the project's verify-end-to-end rule — a green unit-test run is not enough)**

Deploy the build to the FreeBSD hub the way previous features were deployed (see the `zfsctl_freebsd_test_box` notes), then in a real terminal on it:

1. **Default (`ansi`)**: in each of Hosts, Pools, Pools→Enter (vdevs), Pools→`→`, Datasets, Datasets→`→` then `[`/`]` through all three tabs, Snapshots, Disks (with and without a Space selection), Shares — confirm the bottom bar lists exactly the keys that work, and **press every advertised key** once to confirm it does something. Confirm a key *not* listed does nothing.
2. Health colors/glyphs on the Pools table and in the vdev tree; if no pool is degraded, `zpool offline` a mirror disk on a scratch pool to see `▲ DEGRADED` / `○ OFFLINE`, then `zpool online` it back.
3. Capacity bar thresholds: create a small scratch pool and fill it past 70% and 85% to see yellow then red `!`.
4. `d` on a pool: red `✖ DESTROY POOL` modal; type a wrong name and Enter (stays open); Esc. Offline a disk: yellow `▲ OFFLINE DISK`. Scrub: plain modal.
5. Trigger a failure (e.g. add a vdev with a bad spec) — the message is red above the hint bar, and pressing a key that sets a different message turns it plain.
6. Start a scrub and drill into the pool: cyan status line with the gauge under it.
7. Themes: run once each with `theme = "mono"`, `"solarized-dark"`, `"solarized-light"` (under `COLORTERM=truecolor` and without it, expecting the startup note), `NO_COLOR=1`, `NO_COLOR=` (ignored → ansi), and `theme = "neon"` (plain error listing the four names, no raw-mode garbage).
8. Resize the terminal down to ~80x24 and ~40x12: nothing panics, `? Help` / `q Quit` stay visible, Health stays visible in the Pools table at 80 columns.

Record any discrepancy as a failing test first, then fix it.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document themes, NO_COLOR, and the key-hint bar

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

## Self-Review (done while writing)

**Spec coverage**
- Theme struct + roles + helpers → Task 1. Four themes, selection order, `NO_COLOR` empty rule, unknown-name error, Solarized truecolor/256 fallback + note → Tasks 1–2.
- Hint bar, `hints.rs`, pinned `? Help`/`q Quit`, drop-from-right priority, no-op hints hidden, sidebar footer removed, drift guard (both directions) → Tasks 6–7.
- Status line 1–3 rows + gauge row, 0 rows when idle, borderless, status styling, error styling → Task 7.
- Health glyph columns, name tint, capacity column/`!`/`—`, dimming, scan colors, pane borders incl. vdevs/hosts → Tasks 3–4.
- Danger/warn/neutral modal style keyed off `MutationCall` with an exhaustive match → Tasks 5, 8; hint-bar confirm key tone → Task 6.
- Tests: theme boundaries, mono no-color, config parsing, precedence with injected env, hints, layout via `TestBackend`, modal style → in each task. Live check → Task 9.

**Deviations from the spec (flag these to the user at handoff)**
1. Pools table column order is now `Name, Health, Used, Size, Alloc, Free` (Task 4) so Health survives narrow terminals. The spec only said Size/Alloc/Free "remain".
2. `Theme` roles are `Style`s, not bare `Color`s, so `mono` can use bold/dim. Same single-place property.
3. The status line is capped at `STATUS_MAX_ROWS = 3` as the spec says, but this is a narrowing of the old 9-row cap that an earlier fix deliberately introduced for multi-line `zpool add` errors. It is one named constant if the user wants it higher.
4. The "Remove share" confirm (`SetDatasetProperty … = "off"`) is classified neutral by the spec's property-setter rule even though it removes a share. Left as specified.

**Placeholder scan:** no TBD/TODO; every code step has code. The only "fix as needed" instructions are the narrow-terminal Health width (Task 4 step 3) and the two drift-guard resolutions (Task 6 step 4), each with the decision rule spelled out.

**Type consistency:** `Theme` fields/methods, `Tone`, `Hint`, `fit`, `hint_line`, `set_error`/`status_is_error`, `draw_picker(f, picker, theme, area)`, `draw_modal(f, modal, theme, area)`, `pane_border_style(theme, current, pane)` are named identically wherever used. Test count after Task 2 (426) is informational; the requirement is 0 failures.
