//! Keeps "The zfsctl Book" (docs/book) honest: its structure, its links, its callouts, its captures, and (for the
//! reference chapters) that what it documents is what the program has.

use std::path::{Path, PathBuf};

fn book_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs").join("book")
}

/// `(file name, text)` of every chapter file (`NN-slug.md`), in order.
fn chapters() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(book_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() > 3 && n.as_bytes()[..2].iter().all(u8::is_ascii_digit) && n.ends_with(".md"))
        .map(|n| {
            let text = std::fs::read_to_string(book_dir().join(&n)).unwrap();
            (n, text)
        })
        .collect();
    v.sort();
    v
}

fn readme() -> String {
    std::fs::read_to_string(book_dir().join("README.md")).expect("docs/book/README.md exists")
}

/// The chapter files the cover's table of contents links to, in order.
fn toc_links() -> Vec<String> {
    readme().lines().filter_map(|l| l.trim_start().strip_prefix("- [")).filter_map(|rest| rest.split_once("](")).filter_map(|(_, r)| r.split_once(')')).map(|(file, _)| file.to_string()).filter(|f| f.ends_with(".md")).collect()
}

#[test]
fn the_toc_lists_every_chapter_file_in_order_and_nothing_else() {
    let files: Vec<String> = chapters().into_iter().map(|(n, _)| n).collect();
    assert_eq!(toc_links(), files, "docs/book/README.md must list exactly the chapter files, in order");
}

#[test]
fn the_mdbook_summary_lists_the_same_chapters_as_the_readme() {
    let summary = std::fs::read_to_string(book_dir().join("SUMMARY.md")).expect("docs/book/SUMMARY.md exists");
    let linked: Vec<String> = summary.lines().filter_map(|l| l.split_once("](")).filter_map(|(_, r)| r.split_once(')')).map(|(file, _)| file.to_string()).filter(|f| f.ends_with(".md") && f != "README.md").collect();
    assert_eq!(linked, toc_links(), "docs/book/SUMMARY.md (for mdBook) must list the chapters in the same order as the README");
}

#[test]
fn chapters_one_to_nineteen_have_the_standard_shape() {
    for (name, text) in chapters() {
        let number: u32 = name[..2].parse().unwrap();
        if number > 19 {
            continue;
        }
        let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("# ") || l.starts_with("## ")).collect();
        assert!(headings.first().is_some_and(|h| h.starts_with("# ")), "{name}: starts with a title");
        for want in ["## What you will learn", "## Try it", "## Where next"] {
            assert!(headings.contains(&want), "{name}: missing {want}");
        }
        let pos = |h: &str| headings.iter().position(|x| *x == h).unwrap();
        assert!(pos("## What you will learn") == 1 && pos("## Try it") < pos("## Where next"), "{name}: the sections are out of order");
        assert_eq!(*headings.last().unwrap(), "## Where next", "{name}: ends with Where next");
    }
}

/// GitHub-style heading anchor: lower case, punctuation dropped, spaces to dashes.
fn slug(heading: &str) -> String {
    heading.trim_start_matches('#').trim().to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect::<String>().replace(' ', "-")
}

#[test]
fn relative_links_resolve_to_a_file_and_an_existing_heading() {
    let mut texts = chapters();
    texts.push(("README.md".to_string(), readme()));
    for (name, text) in texts {
        let mut in_fence = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
            }
            if in_fence {
                continue;
            }
            let mut rest = line;
            while let Some(i) = rest.find("](") {
                let after = &rest[i + 2..];
                let Some(end) = after.find(')') else { break };
                let target = &after[..end];
                rest = &after[end..];
                if target.starts_with("http") || target.starts_with('#') || target.starts_with("mailto:") {
                    continue;
                }
                let (file, anchor) = target.split_once('#').map_or((target, None), |(f, a)| (f, Some(a)));
                let path = book_dir().join(file);
                assert!(path.exists(), "{name}: link to {target} does not resolve");
                if let (Some(anchor), true) = (anchor, file.ends_with(".md")) {
                    let body = std::fs::read_to_string(&path).unwrap();
                    assert!(body.lines().filter(|l| l.starts_with('#')).any(|l| slug(l) == anchor), "{name}: {target}: no heading '{anchor}' in {file}");
                }
            }
        }
    }
}

#[test]
fn callouts_use_only_the_known_labels() {
    const LABELS: [&str; 5] = ["Coming from Linux", "Coming from Windows", "Safety", "Not built yet", "Tip"];
    for (name, text) in chapters() {
        for (n, line) in text.lines().enumerate() {
            if let Some(rest) = line.strip_prefix("> **") {
                let label = rest.split_once(":**").map_or(rest, |(l, _)| l);
                assert!(LABELS.contains(&label), "{name}:{}: unknown callout label '{label}'", n + 1);
            }
        }
    }
}

#[test]
fn embedded_captures_are_non_empty_and_equal_a_capture_file() {
    let files: Vec<String> = std::fs::read_dir(book_dir().join("captures")).unwrap().filter_map(Result::ok).filter_map(|e| std::fs::read_to_string(e.path()).ok()).map(|c| c.trim_end().to_string()).collect();
    for (name, text) in chapters() {
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if line.trim() == "```screen" {
                let mut body = Vec::new();
                for l in lines.by_ref() {
                    if l.trim() == "```" {
                        break;
                    }
                    body.push(l);
                }
                let body = body.join("\n");
                assert!(!body.trim().is_empty(), "{name}: an empty screen block");
                assert!(files.iter().any(|c| c == body.trim_end()), "{name}: a screen block that equals no file in docs/book/captures (captures are never edited by hand)");
            }
        }
    }
}

#[test]
fn the_book_has_the_title_and_the_standard_cover_sections() {
    let readme = readme();
    assert!(readme.starts_with("# The zfsctl Book"), "{}", readme.lines().next().unwrap_or(""));
    for want in ["## Who this is for", "## How to read it", "## The running example", "## Chapters"] {
        assert!(readme.contains(want), "missing {want}");
    }
}

fn chapter(name: &str) -> String {
    std::fs::read_to_string(book_dir().join(name)).unwrap()
}

/// Keys named in the first backticked cell of a Markdown table row, by section heading.
fn key_rows(text: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("| `") {
            if let Some((key, _)) = rest.split_once("` |") {
                out.push((section.clone(), key.to_string()));
            }
        }
    }
    out
}

#[test]
fn every_key_in_the_keys_reference_is_in_the_apps_help_and_every_help_key_is_in_the_book() {
    let help = crate::ui::state::help_text_for_book();
    let book = chapter("20-reference-keys.md");
    let rows = key_rows(&book);
    assert!(rows.len() > 40, "the keys reference lists the keys of every section ({} found)", rows.len());
    for (section, key) in &rows {
        let probe = key.split(" / ").next().unwrap();
        assert!(help.contains(probe), "{section}: the book documents key `{key}` that the app's Help does not mention");
    }
    // Every key line of Help (two leading spaces, a key column, two spaces, a description) is in the book.
    for line in help.lines().filter(|l| l.starts_with("  ") && !l.trim().is_empty()) {
        let key = line.trim_start().split("  ").next().unwrap_or("").trim();
        if key.is_empty() || key.len() > 24 {
            continue;
        }
        assert!(rows.iter().any(|(_, k)| k.split(" / ").any(|part| part == key) || k == key), "Help documents `{key}` but the keys reference does not");
    }
}

#[test]
fn every_config_key_in_the_reference_is_a_real_field_and_every_field_is_listed() {
    let text = chapter("21-reference-config.md");
    let documented: Vec<String> = key_rows(&text).into_iter().map(|(_, k)| k).collect();
    let fields = crate::config::documented_config_keys();
    for key in &documented {
        assert!(fields.contains(&key.as_str()), "the book documents config key `{key}` that does not exist");
    }
    for key in fields {
        assert!(documented.iter().any(|d| d == key), "config key `{key}` is not in the reference");
    }
}

#[test]
fn every_command_form_in_the_reference_parses_and_every_form_is_documented() {
    let text = chapter("22-reference-commands.md");
    let headings: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("### `")).filter_map(|r| r.strip_suffix('`')).map(str::to_string).collect();
    for h in &headings {
        assert!(h.starts_with("zfsctl"), "{h}");
        let words: Vec<String> = h.split_whitespace().skip(1).map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string()).collect();
        // An upper-case word is a placeholder (`HOST`, `DIR`, `FEATURE`); give it a plausible value.
        let with_placeholders: Vec<String> = words.iter().map(|w| if w.chars().all(|c| c.is_ascii_uppercase()) { "somevalue".to_string() } else { w.clone() }).collect();
        assert!(crate::parse_args_for_book(&with_placeholders).is_ok(), "the book documents `{h}` but the program rejects it");
    }
    for want in ["zfsctl", "zfsctl daemon", "zfsctl print-sudoers HOST", "zfsctl check-host HOST", "zfsctl notify-test", "zfsctl report"] {
        assert!(headings.iter().any(|h| h.starts_with(want)), "command `{want}` is missing from the reference");
    }
}

#[test]
fn the_commands_reference_and_the_help_describe_the_same_commands() {
    let text = chapter("22-reference-commands.md");
    let headings: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("### `")).filter_map(|r| r.strip_suffix('`')).map(str::to_string).collect();
    // The command a heading or help form names: `zfsctl daemon ...` is `daemon`, `zfsctl --help` is `--help`, `zfsctl` is none.
    let command_of = |form: &str| form.split_whitespace().nth(1).map(str::to_string);
    let help: Vec<Option<String>> = crate::help::COMMANDS.iter().map(|(form, _)| command_of(form)).collect();
    for h in &headings {
        assert!(help.contains(&command_of(h)), "the book documents `{h}` but `zfsctl --help` does not describe that command");
    }
    for form in crate::help::COMMANDS.iter().map(|(form, _)| *form) {
        assert!(headings.iter().any(|h| command_of(h) == command_of(form)), "`zfsctl --help` describes `{form}` but the commands reference has no heading for it");
    }
}

#[test]
fn every_privileged_command_the_program_runs_is_named_in_the_sudoers_part() {
    let text = chapter("22-reference-commands.md");
    for name in crate::exec::privilege::privileged_names() {
        assert!(text.contains(&format!("`{name}`")), "privileged command `{name}` is not named in the commands reference");
    }
}

#[test]
fn every_glossary_term_is_defined_once_and_every_core_term_used_is_in_the_glossary() {
    let glossary = chapter("23-glossary.md");
    let terms: Vec<String> = glossary.lines().filter_map(|l| l.strip_prefix("**")).filter_map(|r| r.split_once("**")).map(|(t, _)| t.to_lowercase()).collect();
    let mut sorted = terms.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), terms.len(), "a term is defined twice");
    assert!(terms.len() >= 60, "the glossary defines about 80 terms ({} found)", terms.len());
    for term in ["pool", "vdev", "dataset", "snapshot", "scrub", "resilver", "replication", "boot environment", "hub"] {
        assert!(terms.iter().any(|t| t == term), "the glossary does not define '{term}'");
    }
}
