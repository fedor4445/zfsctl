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
