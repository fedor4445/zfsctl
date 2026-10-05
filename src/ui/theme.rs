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

    /// Just `31%` / `100%!` for panes too narrow for the bar; same style and
    /// the same `!` rule as `capacity_cell`.
    pub fn capacity_compact(&self, allocated: u64, size: u64) -> Line<'static> {
        let Some(pct) = capacity_pct(allocated, size) else {
            return Line::styled("—", self.muted);
        };
        let bang = if pct > 85 { "!" } else { "" };
        Line::from(Span::styled(format!("{pct}%{bang}"), self.capacity_style(pct)))
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

/// Stops crossterm from applying `NO_COLOR` on its own. Left alone it
/// strips every color and emits a bare `ESC[m` (a full SGR reset) in its
/// place, which also wipes the bold/reverse ratatui has just set for the
/// same cell. `resolve` already honours `NO_COLOR` by choosing `mono`
/// (which emits no colors), so an explicitly configured theme must win.
pub fn apply_color_policy() {
    crossterm::style::force_color_output(true);
}

#[derive(Debug)]
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

    /// crossterm strips every color when NO_COLOR is set, and writes a bare
    /// `ESC[;m` (a full SGR reset) in its place, which also wipes the
    /// bold/reverse ratatui just set. `resolve` already applies NO_COLOR
    /// (by choosing `mono`), so an explicitly configured theme must win.
    /// NOTE: touches process env; this is the only test that does.
    #[test]
    fn explicit_theme_colors_survive_no_color_in_the_environment() {
        std::env::set_var("NO_COLOR", "1");
        apply_color_policy();
        let out = format!("{}", crossterm::style::SetForegroundColor(crossterm::style::Color::Red));
        std::env::remove_var("NO_COLOR");
        assert!(out.contains("38;5;"), "colors were stripped: {out:?}");
    }

    #[test]
    fn capacity_compact_is_just_the_percentage_with_the_same_bang_and_style() {
        let t = Theme::ansi();
        assert_eq!(text(&t.capacity_compact(31, 100)), "31%");
        assert_eq!(text(&t.capacity_compact(86, 100)), "86%!");
        assert_eq!(text(&t.capacity_compact(150, 100)), "100%!");
        assert_eq!(text(&t.capacity_compact(0, 0)), "—");
        assert_eq!(t.capacity_compact(70, 100).spans[0].style, t.warn);
    }
}
