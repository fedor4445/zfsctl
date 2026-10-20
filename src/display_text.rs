//! Text helpers shared by the UI and the layers below it, so messages built from
//! host-supplied text can be made safe where they are built.

/// Replaces every control character (newline, tab, ESC, ...) with a space
/// and collapses runs of whitespace, so text that may carry raw remote
/// stderr renders on one line and cannot inject terminal escapes.
pub(crate) fn sanitise_for_display(s: &str) -> String {
    let spaced: String = s
        .chars()
        .map(|c| {
            let bidi = matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || bidi { ' ' } else { c }
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncates `s` to at most `max` characters, ending in "…" when cut.
pub(crate) fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Hold tags (host-supplied text) ready for a message: each one sanitised and cut to 40 characters,
/// tags that are empty after sanitising dropped, joined with ", ". Empty when none is left.
pub(crate) fn tag_list(tags: &[String]) -> String {
    tags.iter()
        .map(|t| truncate_with_ellipsis(&sanitise_for_display(t), 40))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_for_display_strips_control_characters_and_collapses_lines() {
        let out = sanitise_for_display("boom\n\u{1b}[31mred\u{1b}[0m\tbad\r\nend");
        assert!(!out.chars().any(char::is_control), "{out:?}");
        assert_eq!(out, "boom [31mred [0m bad end");
    }

    #[test]
    fn sanitise_for_display_replaces_bidi_and_format_characters() {
        let out = sanitise_for_display("a\u{202E}b\u{200F}c\u{2066}d\u{2069}e\u{200E}f\u{202A}g");
        assert_eq!(out, "a b c d e f g");
    }

    #[test]
    fn truncate_with_ellipsis_cuts_long_text_only() {
        assert_eq!(truncate_with_ellipsis("abc", 3), "abc");
        assert_eq!(truncate_with_ellipsis("abcdef", 4), "abc…");
    }

    #[test]
    fn tag_list_cleans_cuts_and_drops_empty_tags() {
        let tags = vec!["a\u{1b}[31mb\u{202e}".to_string(), "\u{1b}\u{7}".to_string(), "x".repeat(50)];
        assert_eq!(tag_list(&tags), format!("a [31mb, {}…", "x".repeat(39)));
        assert_eq!(tag_list(&["\u{1b}".to_string()]), "");
    }
}
