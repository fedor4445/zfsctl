//! The zfsctl wordmark in a 5x7 dot-matrix, drawn with half-block characters so each dot is square: `zfsctl` and an
//! underscore cursor. It is the same drawing as `docs/logo/zfsctl-logo.svg`.

/// Columns of the drawing: six letters of 5 dots, a blank column after each, then the 5-dot cursor.
pub const WIDTH: usize = 41;

const ROWS: usize = 7;

/// Each glyph's dots, top to bottom; a glyph shorter than `ROWS` sits on the baseline (the letters without an ascender).
const GLYPHS: [&[&str]; 6] = [
    &["11111", "00010", "00100", "01000", "11111"],
    &["00110", "01000", "11110", "01000", "01000", "01000", "01000"],
    &["01111", "10000", "01110", "00001", "11110"],
    &["01110", "10001", "10000", "10001", "01110"],
    &["01000", "01000", "11110", "01000", "01000", "01001", "00110"],
    &["00100", "00100", "00100", "00100", "00100", "00100", "00110"],
];

fn dots() -> [[bool; WIDTH]; ROWS] {
    let mut grid = [[false; WIDTH]; ROWS];
    for (letter, glyph) in GLYPHS.iter().enumerate() {
        let top = ROWS - glyph.len();
        for (r, line) in glyph.iter().enumerate() {
            for (c, dot) in line.bytes().enumerate() {
                if let Some(cell) = grid.get_mut(top + r).and_then(|row| row.get_mut(letter * 6 + c)) {
                    *cell = dot == b'1';
                }
            }
        }
    }
    // the cursor: the bottom row, after the last letter and its gap
    if let Some(row) = grid.get_mut(ROWS - 1) {
        row.iter_mut().skip(GLYPHS.len() * 6).for_each(|cell| *cell = true);
    }
    grid
}

/// The drawing as four lines of `WIDTH` characters: `█` where both dots of a pair are on, `▀` the upper one, `▄` the lower one.
pub fn wordmark() -> Vec<String> {
    let grid = dots();
    (0..ROWS).step_by(2).map(|top| {
        (0..WIDTH)
            .map(|c| {
                let at = |r: usize| grid.get(r).and_then(|row| row.get(c)).copied().unwrap_or(false);
                match (at(top), at(top + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                }
            })
            .collect()
    }).collect()
}

/// Whether `line` is a row of the drawing (blocks and spaces, at least one block), so the pop-up can colour it.
pub fn is_wordmark_line(line: &str) -> bool {
    line.chars().any(|c| matches!(c, '█' | '▀' | '▄')) && line.chars().all(|c| matches!(c, '█' | '▀' | '▄' | ' '))
}

/// Whether the About pop-up centres `line`: the wordmark and the `zfsctl vX.Y` title under it.
pub fn is_centred_line(line: &str) -> bool {
    is_wordmark_line(line) || line.starts_with("zfsctl v")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wordmark_is_four_lines_of_the_same_width() {
        let lines = wordmark();
        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|l| l.chars().count() == WIDTH), "{lines:?}");
        assert!(lines.iter().all(|l| is_wordmark_line(l)), "{lines:?}");
    }

    #[test]
    fn the_z_the_baseline_and_the_cursor_are_where_the_svg_has_them() {
        let lines = wordmark();
        // rows 6 and 7: the z's bottom bar and the cursor are on the upper half of the last line (row 7 is empty)
        let last: Vec<char> = lines[3].chars().collect();
        assert_eq!(last.iter().take(5).collect::<String>(), "▀▀▀▀▀", "the z's bottom bar");
        assert_eq!(last.iter().skip(36).collect::<String>(), "▀▀▀▀▀", "the cursor");
        assert_eq!(last.get(5), Some(&' '), "a blank column between letters");
        // the f's hook is above the x-height: nothing of the z or s is in the first line
        let first: Vec<char> = lines[0].chars().collect();
        assert!(first.iter().take(6).all(|c| *c == ' ') && first.iter().skip(12).take(6).all(|c| *c == ' '), "{first:?}");
        assert!(first.iter().skip(6).take(6).any(|c| *c != ' '), "the f has an ascender");
    }

    #[test]
    fn ordinary_text_is_not_a_wordmark_line() {
        assert!(!is_wordmark_line("zfsctl v0.2"));
        assert!(!is_wordmark_line("   "));
        assert!(!is_wordmark_line(""));
        assert!(is_wordmark_line("▀▀ █▄ "));
    }
}
