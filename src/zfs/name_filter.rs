//! The name filter of the snapshot browser and of the changes view: what a person types after `/` to narrow a list of names.
//! `a|b` keeps names matching either; text with `*` or `?` is a wildcard pattern over the whole name; anything else is a
//! piece of the name. Matching ignores case.

/// One alternative of a filter, already lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alt {
    pattern: Vec<char>,
    wildcard: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameFilter {
    text: String,
    alts: Vec<Alt>,
}

impl NameFilter {
    pub fn new(text: &str) -> Self {
        let alts = text
            .split('|')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|a| Alt { pattern: a.to_lowercase().chars().collect(), wildcard: a.contains(['*', '?']) })
            .collect();
        Self { text: text.to_string(), alts }
    }

    /// The text as it was typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the filter keeps everything (nothing, or only separators, was typed).
    pub fn is_empty(&self) -> bool {
        self.alts.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        if self.alts.is_empty() {
            return true;
        }
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.alts.iter().any(|alt| if alt.wildcard { glob(&alt.pattern, &name) } else { contains(&name, &alt.pattern) })
    }
}

/// Whether `needle` occurs in `haystack`.
fn contains(haystack: &[char], needle: &[char]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// `*` matches any run of characters (also none), `?` exactly one; the whole of `name` must match.
fn glob(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = n;
                p += 1;
            }
            Some(&c) if c == '?' || name.get(n) == Some(&c) => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some(s) => {
                    resume += 1;
                    n = resume;
                    p = s + 1;
                }
                None => return false,
            },
        }
    }
    pattern.get(p..).is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_filter_matches_everything() {
        for text in ["", "   ", "|", " | | "] {
            let f = NameFilter::new(text);
            assert!(f.is_empty() && f.matches("anything") && f.matches(""), "{text:?}");
        }
        assert!(NameFilter::default().is_empty());
    }

    #[test]
    fn plain_text_is_a_case_insensitive_piece_of_the_name() {
        let f = NameFilter::new(".JPEG");
        assert!(f.matches("holiday.jpeg") && f.matches("A.Jpeg.bak") && !f.matches("holiday.jpg"));
        assert!(!NameFilter::new("%*\\").matches("a%b*c\\d"), "a star is a wildcard, so this is a glob over the whole name, not a literal");
    }

    #[test]
    fn a_bar_separates_alternatives_and_any_one_may_match() {
        let f = NameFilter::new(" .jpg | .jpeg|.PNG ");
        for yes in ["a.jpg", "b.JPEG", "c.png", "x.jpg.zip"] {
            assert!(f.matches(yes), "{yes}");
        }
        for no in ["a.gif", "jpg", "notes.txt"] {
            assert!(!f.matches(no), "{no}");
        }
        assert!(NameFilter::new(".jpg|").matches("a.jpg") && !NameFilter::new(".jpg|").matches("a.gif"), "a trailing bar adds no empty alternative");
    }

    #[test]
    fn star_and_question_mark_are_wildcards_over_the_whole_name() {
        let f = NameFilter::new("*.jp*g");
        assert!(f.matches("a.jpg") && f.matches("a.JPEG") && f.matches("x y.jpeg") && !f.matches("a.jpg.bak") && !f.matches("a.png"));
        let q = NameFilter::new("img_????.jpg");
        assert!(q.matches("IMG_0042.jpg") && !q.matches("img_42.jpg") && !q.matches("img_00042.jpg"));
        assert!(NameFilter::new("*").matches("") && NameFilter::new("a*").matches("a") && !NameFilter::new("a?").matches("a"));
        assert!(NameFilter::new("**a**").matches("xax"), "repeated stars are fine");
    }

    #[test]
    fn unicode_and_awkward_names_do_not_panic() {
        let f = NameFilter::new("é*|İ|\u{1b}");
        assert!(f.matches("élan") && !f.matches("x"));
        let _ = f.matches("\u{fffd}\n\0");
        assert_eq!(NameFilter::new(" a|b ").text(), " a|b ", "the text is kept as typed");
    }

    #[test]
    fn a_long_pattern_is_cheap() {
        let f = NameFilter::new(&"a*".repeat(200));
        assert!(!f.matches(&"a".repeat(100).replace('a', "b")));
        assert!(f.matches(&"a".repeat(300)));
    }
}
