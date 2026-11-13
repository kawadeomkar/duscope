//! P5-DOC-02: `docs/troubleshooting.md` has an entry, a heading of its own,
//! for every warning code and every error code duscope has
//! (`duscope_core::envelope::WARNING_CODES` and `ERROR_CODES`), and none for
//! a code it does not have; and the relative links of it and
//! `docs/limits.md` lead to a file, and to a heading of it where they name
//! one. The docs job's lychee checks the links too, but only this test sees
//! a code added without an entry.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use duscope_core::envelope::{ERROR_CODES, WARNING_CODES};

/// The documents this package wrote.
const DOCUMENTS: [&str; 2] = ["limits.md", "troubleshooting.md"];

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The lines of a markdown text outside its fenced code blocks.
fn prose(text: &str) -> Vec<&str> {
    let mut fenced = false;
    text.lines()
        .filter(|line| {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                return false;
            }
            !fenced
        })
        .collect()
}

/// The headings of a markdown text: their level and their text.
fn headings(text: &str) -> Vec<(usize, &str)> {
    prose(text)
        .into_iter()
        .filter_map(|line| {
            let level = line.bytes().take_while(|&b| b == b'#').count();
            let rest = line.get(level..)?;
            (level > 0 && rest.starts_with(' ')).then(|| (level, rest.trim()))
        })
        .collect()
}

/// The codes the `## <section>` of troubleshooting.md has entries for: its
/// `` ### `code` `` headings.
fn entries<'t>(text: &'t str, section: &str) -> Vec<&'t str> {
    let mut inside = false;
    let mut codes = Vec::new();
    for (level, heading) in headings(text) {
        match level {
            1 | 2 => inside = heading == section,
            3 if inside => codes.push(
                heading
                    .strip_prefix('`')
                    .and_then(|h| h.strip_suffix('`'))
                    .unwrap_or_else(|| panic!("{section}: the heading {heading:?} is not a code")),
            ),
            _ => {}
        }
    }
    codes
}

/// The anchor GitHub gives a heading: lower case, every character but a
/// letter, a digit, a space, `-` and `_` dropped, spaces as `-`.
fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// The anchors of a markdown text's headings; a repeated one gets `-1`,
/// `-2`, as GitHub numbers them.
fn anchors(text: &str) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    headings(text)
        .into_iter()
        .map(|(_, heading)| {
            let slug = slug(heading);
            let n = seen.entry(slug.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                slug
            } else {
                format!("{slug}-{}", *n - 1)
            }
        })
        .collect()
}

/// A line without its inline code spans, whose text is no link.
fn without_code(line: &str) -> String {
    line.split('`').step_by(2).collect()
}

/// The targets of a markdown text's inline links, `[text](target)`.
fn links(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in prose(text) {
        let line = without_code(line);
        let mut rest = line.as_str();
        while let Some(at) = rest.find("](") {
            rest = &rest[at + 2..];
            let end = rest.find(')').unwrap_or(rest.len());
            found.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    found
}

/// What is wrong with the relative links of `text`, the document at `file`:
/// each that leads to no file, or to no heading of the markdown file it
/// names. Web links are left to the docs job's daily lychee run.
fn broken_links(file: &Path, text: &str) -> Vec<String> {
    let dir = file.parent().unwrap();
    let mut broken = Vec::new();
    for link in links(text) {
        if link.contains("://") || link.starts_with("mailto:") {
            continue;
        }
        let (path, fragment) = link.split_once('#').unwrap_or((&link, ""));
        let target = if path.is_empty() {
            file.to_owned()
        } else {
            dir.join(path)
        };
        if !target.exists() {
            broken.push(format!("{link}: no file {}", target.display()));
            continue;
        }
        let markdown = target.extension().is_some_and(|e| e == "md");
        if !fragment.is_empty() && markdown {
            let theirs = if target == file {
                text.to_owned()
            } else {
                read(&target)
            };
            if !anchors(&theirs).iter().any(|a| a == fragment) {
                broken.push(format!("{link}: no heading #{fragment}"));
            }
        }
    }
    broken
}

/// Each code that appears in `codes` more than once.
fn repeated<'c>(codes: &[&'c str]) -> Vec<&'c str> {
    let mut repeated: Vec<&str> = codes
        .iter()
        .copied()
        .filter(|code| codes.iter().filter(|c| c == &code).count() > 1)
        .collect();
    repeated.dedup();
    repeated
}

#[test]
fn p5_doc_02_every_warning_code_has_an_entry() {
    let text = read(&docs().join("troubleshooting.md"));
    let documented = entries(&text, "Warnings");
    let missing: Vec<&str> = WARNING_CODES
        .into_iter()
        .filter(|code| !documented.contains(code))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/troubleshooting.md has no entry under Warnings for {missing:?}"
    );
    let unknown: Vec<&str> = documented
        .iter()
        .copied()
        .filter(|code| !WARNING_CODES.contains(code))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs/troubleshooting.md has warning entries for codes duscope does not have: {unknown:?}"
    );
    assert!(repeated(&documented).is_empty(), "{documented:?}");
}

#[test]
fn p5_doc_02_every_error_code_has_an_entry() {
    let text = read(&docs().join("troubleshooting.md"));
    let documented = entries(&text, "Errors");
    // A code that is both a warning and an error has one entry, under
    // Warnings, so that its anchor is the code alone.
    let warnings = entries(&text, "Warnings");
    let missing: Vec<&str> = ERROR_CODES
        .into_iter()
        .map(|(code, _)| code)
        .filter(|code| !documented.contains(code) && !warnings.contains(code))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/troubleshooting.md has no entry for the error codes {missing:?}"
    );
    let unknown: Vec<&str> = documented
        .iter()
        .copied()
        .filter(|code| !ERROR_CODES.iter().any(|&(c, _)| c == *code))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs/troubleshooting.md has error entries for codes duscope does not have: {unknown:?}"
    );
    let both: Vec<&str> = documented
        .iter()
        .chain(&warnings)
        .copied()
        .collect::<Vec<_>>();
    assert!(repeated(&both).is_empty(), "{:?}", repeated(&both));
}

#[test]
fn p5_doc_02_each_error_entry_gives_its_exit_status() {
    let text = read(&docs().join("troubleshooting.md"));
    let lines = prose(&text);
    let errors = entries(&text, "Errors");
    for (code, exit) in ERROR_CODES {
        // A code that is a warning too has its entry under Warnings.
        if !errors.contains(&code) {
            continue;
        }
        let heading = format!("### `{code}`");
        let at = lines.iter().position(|line| *line == heading).unwrap();
        let first = lines[at + 1..]
            .iter()
            .find(|line| !line.trim().is_empty())
            .unwrap();
        let stated = first
            .strip_prefix("Exit ")
            .and_then(|rest| rest.split([',', '.']).next());
        assert_eq!(
            stated,
            Some(exit.to_string().as_str()),
            "{code}: the entry starts {first:?}, not with its exit status {exit}"
        );
    }
}

#[test]
fn p5_doc_02_the_relative_links_resolve() {
    for name in DOCUMENTS {
        let file = docs().join(name);
        let broken = broken_links(&file, &read(&file));
        assert!(broken.is_empty(), "docs/{name}: {broken:#?}");
    }
}

#[test]
fn p5_doc_02_the_link_check_finds_a_broken_link() {
    let file = docs().join("limits.md");
    let text = "# Title\n\n## A `code` heading\n\n\
                [ok](troubleshooting.md#partial_read) [ok](#a-code-heading) \
                [ok](https://example.org/none) `[code](nowhere.md)`\n\n\
                ```text\n[fenced](nowhere.md)\n```\n\n\
                [gone](nowhere.md) [no heading](troubleshooting.md#no_such_code) \
                [no anchor](#title-2)\n";
    let broken = broken_links(&file, text);
    assert_eq!(broken.len(), 3, "{broken:#?}");
    assert!(broken[0].starts_with("nowhere.md: no file"), "{broken:#?}");
    assert_eq!(
        broken[1],
        "troubleshooting.md#no_such_code: no heading #no_such_code"
    );
    assert_eq!(broken[2], "#title-2: no heading #title-2");
    assert_eq!(
        anchors("# A\n## A\n### `x_y` and --z\n"),
        ["a", "a-1", "x_y-and---z"]
    );
}
