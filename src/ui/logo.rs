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

/// The sidebar's compact mark: the same drawing as the wordmark above, with its x-height and its underscore cursor, in braille
/// (two dots across, four down in each character), so it is half as wide and a third of the height: 21 columns, two lines.
pub const COMPACT_WIDTH: usize = 21;

pub fn compact() -> Vec<String> {
    let grid = dots();
    let on = |r: usize, c: usize| grid.get(r).and_then(|row| row.get(c)).copied().unwrap_or(false);
    // Braille dots 1-8: the left column top to bottom is bits 0, 1, 2 and 6, the right one 3, 4, 5 and 7.
    const BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
    (0..2)
        .map(|line| {
            (0..COMPACT_WIDTH)
                .map(|cell| {
                    let bits: u32 = BIT.iter().enumerate().flat_map(|(dr, pair)| pair.iter().enumerate().map(move |(dc, bit)| (dr, dc, *bit))).filter(|(dr, dc, _)| on(line * 4 + dr, cell * 2 + dc)).map(|(_, _, bit)| bit).sum();
                    char::from_u32(0x2800 + bits).unwrap_or(' ')
                })
                .collect()
        })
        .collect()
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
    fn the_compact_mark_is_the_wordmark_in_braille_two_lines_of_21_columns() {
        let lines = compact();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() == COMPACT_WIDTH && l.chars().all(|c| ('\u{2800}'..='\u{28ff}').contains(&c))), "{lines:?}");
        // the underscore cursor is the bottom dot row of the last three cells: two full pairs and a single dot
        assert_eq!(lines[1].chars().skip(18).collect::<String>(), "⠤⠤⠄", "{lines:?}");
        // the letters keep the wordmark's x-height: the first line (dot rows 0-3) has the f, t and l ascenders over empty z, s, c
        let first: Vec<char> = lines[0].chars().collect();
        assert_eq!((first[0], first[1], first[2]), ('⠤', '⢤', '⠄'), "the z starts at dot row 2 {lines:?}");
        // every one of the six letters and the cursor is there: each of the 21 cells of the second line outside the blank gaps has a dot
        assert!(lines[1].chars().filter(|c| *c != '\u{2800}').count() >= 15, "{lines:?}");
    }

    #[test]
    fn ordinary_text_is_not_a_wordmark_line() {
        assert!(!is_wordmark_line("zfsctl v0.2"));
        assert!(!is_wordmark_line("   "));
        assert!(!is_wordmark_line(""));
        assert!(is_wordmark_line("▀▀ █▄ "));
    }
}
