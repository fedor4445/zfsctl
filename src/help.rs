//! The text of `zfsctl --help`. The commands come from one table that a test holds against the argument parser
//! and the book; the feature names come from the privilege table, so neither can drift from the program.

use crate::exec::privilege::Feature;

/// Every way to start zfsctl: the form as typed, and what it does. `ANY_CAPS` words are placeholders.
pub const COMMANDS: &[(&str, &str)] = &[
    ("zfsctl", "Open the terminal interface (the sections, forms and keys are explained by `?` inside it)."),
    ("zfsctl daemon [--state-dir DIR]", "Run the scheduler and the health monitor in the foreground. The rc script starts it (`service zfsctl start`). State lives in DIR, default /var/db/zfsctl."),
    ("zfsctl print-sudoers HOST [--with FEATURE] [--without FEATURE]", "Print the sudoers entry a managed host needs, built from the programs found there. Reads only."),
    ("zfsctl check-host HOST [--with FEATURE] [--without FEATURE]", "Check that a host's sudo rules allow exactly what zfsctl needs: one line per program (ok, MISSING or TOO BROAD)."),
    ("zfsctl notify-test", "Send a short test message through every configured channel (mail and hook). Sends real mail."),
    ("zfsctl report [--state-dir DIR]", "Print the daily report as it would be sent now. Sends nothing."),
    ("zfsctl --help", "Show this text (also -h, and --help after any command). Needs no root."),
];

/// The widest line the help text uses, so it reads on a plain 100-column terminal.
const WIDTH: usize = 100;

/// `text` broken into lines of at most `width` characters at word boundaries (a longer word gets its own line).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.len() + 1 + word.len() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

/// Adds `text`, wrapped, to `out` with every line indented by `indent` spaces.
fn push_wrapped(out: &mut String, indent: usize, text: &str) {
    for line in wrap(text, WIDTH - indent) {
        out.push_str(&" ".repeat(indent));
        out.push_str(&line);
        out.push('\n');
    }
}

/// The whole `--help` text.
pub fn text() -> String {
    let mut out = String::new();
    push_wrapped(
        &mut out,
        0,
        &format!("zfsctl {}: manage ZFS pools, datasets, snapshots, shares, boot environments, scheduled jobs and backups, on this machine and on remote hosts.", env!("CARGO_PKG_VERSION")),
    );
    out.push_str("\nUSAGE\n");
    for (form, what) in COMMANDS {
        out.push_str("  ");
        out.push_str(form);
        out.push('\n');
        push_wrapped(&mut out, 6, what);
    }
    out.push_str("\nFEATURES (for --with and --without)\n  ");
    out.push_str(&Feature::ALL.map(Feature::name).join(", "));
    out.push('\n');
    push_wrapped(&mut out, 2, "`restic` is left out unless you add it with --with restic; --without removes a feature from the entry.");
    out.push_str("\nFILES\n  ");
    out.push_str(crate::config::DEFAULT_CONFIG_PATH);
    out.push_str("   hosts, theme, notifications\n  ");
    out.push_str(crate::daemon::engine::DEFAULT_STATE_DIR);
    out.push_str("/jobs.toml   scheduled jobs; history, heartbeat and reports sit beside it\n");
    out.push_str("\nEXIT CODES\n  0  success\n  1  a problem: a bad command line, a failed check, or an error (the message is on stderr)\n  2  print-sudoers and check-host: the host could not be reached\n");
    out.push_str("\nNOTES\n");
    push_wrapped(&mut out, 2, "Everything but --help must run as root: on the hub directly, or through sudo.");
    push_wrapped(&mut out, 2, "A managed host needs no zfsctl software: it is reached over ssh and sudo (see print-sudoers).");
    out.push_str("\nMORE\n");
    push_wrapped(&mut out, 2, "Inside the terminal interface press ? for the keys of the current section.");
    push_wrapped(&mut out, 2, "The README and docs/book (The zfsctl Book) describe every section, job kind and recipe.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form with each placeholder (`HOST`, `DIR`, `FEATURE`) given a plausible value and the optional
    /// brackets taken off.
    fn argv(form: &str) -> Vec<String> {
        form.split_whitespace()
            .skip(1)
            .map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string())
            .map(|w| if w.chars().all(|c| c.is_ascii_uppercase()) { "somevalue".to_string() } else { w })
            .collect()
    }

    #[test]
    fn every_command_in_the_help_is_accepted_by_the_argument_parser() {
        for (form, _) in COMMANDS {
            assert!(crate::parse_args_for_book(&argv(form)).is_ok(), "`{form}` is in --help but the program rejects it");
        }
    }

    #[test]
    fn the_help_names_every_feature_the_program_knows_and_every_command() {
        let help = text();
        // The descriptions are wrapped, so compare words, not lines.
        let words = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat = words(&help);
        for f in Feature::ALL {
            assert!(help.contains(f.name()), "feature {} is missing from --help", f.name());
        }
        for (form, what) in COMMANDS {
            assert!(help.contains(form), "{form}");
            assert!(flat.contains(&words(what)), "{what}");
        }
        for word in ["USAGE", "FEATURES", "FILES", "EXIT CODES", "MORE", crate::config::DEFAULT_CONFIG_PATH, crate::daemon::engine::DEFAULT_STATE_DIR] {
            assert!(help.contains(word), "{word}");
        }
        assert!(help.starts_with(&format!("zfsctl {}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn no_line_of_the_help_is_wider_than_a_plain_terminal() {
        for line in text().lines() {
            assert!(line.chars().count() <= WIDTH, "{} columns: {line}", line.chars().count());
        }
    }

    #[test]
    fn wrapping_breaks_at_words_and_keeps_a_long_word_whole() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("a verylongword b", 4), ["a", "verylongword", "b"]);
        assert!(wrap("", 10).is_empty());
    }
}
