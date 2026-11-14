//! P5-DOC-02: `docs/troubleshooting.md` has an entry, a heading of its own
//! with text under it, for every warning code and every error code duscope
//! has (`duscope_core::envelope::WARNING_CODES` and `ERROR_CODES`), and none
//! for a code it does not have; the lists of `clean`'s refusal reasons, of a
//! volume's reasons and of a rescan's fall-back reasons are the envelope's
//! (`duscope_core::envelope::values`), the refusals as `clean.rs` writes
//! them and a volume's reasons apart from a directory's; and the relative
//! links of it, `docs/limits.md` and `README.md` lead to a file, and to a
//! heading of it where they name one. The docs job's lychee checks the links
//! too, but only this test sees a code or a reason added without an entry.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use duscope_core::envelope::{ERROR_CODES, WARNING_CODES, values};

/// The documents this package wrote.
const DOCUMENTS: [&str; 2] = ["limits.md", "troubleshooting.md"];

/// The reasons no volume carries, a directory's: an exclude pattern's and
/// `--skip-containers`'s, which leave a mount point among the directories
/// `scan.excluded` names, and macOS's way into the Data volume, which the
/// walk gives no volume row (`Walker::decide`).
const DIRECTORY_REASONS: [&str; 3] = ["excluded", "skipped_containers", "firmlinked"];

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs")
}

/// The repository's top directory.
fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
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

/// A line of prose as a heading, if it is one: its level and its text.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let rest = line.get(level..)?;
    (level > 0 && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

/// The headings of a markdown text: their level and their text.
fn headings(text: &str) -> Vec<(usize, &str)> {
    prose(text).into_iter().filter_map(heading).collect()
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

/// Each heading of a markdown text with the prose lines under it, up to the
/// next heading: its level, its text and those lines.
fn sections(text: &str) -> Vec<(usize, &str, Vec<&str>)> {
    let mut found: Vec<(usize, &str, Vec<&str>)> = Vec::new();
    for line in prose(text) {
        match heading(line) {
            Some((level, heading)) => found.push((level, heading, Vec::new())),
            None => {
                if let Some((_, _, body)) = found.last_mut() {
                    body.push(line);
                }
            }
        }
    }
    found
}

/// The prose lines under the heading `heading` of a markdown text, up to the
/// next heading.
fn section<'t>(text: &'t str, heading: &str) -> Vec<&'t str> {
    let mut matching = sections(text)
        .into_iter()
        .filter(|(_, h, _)| *h == heading)
        .map(|(_, _, body)| body);
    let body = matching
        .next()
        .unwrap_or_else(|| panic!("no heading {heading:?}"));
    assert!(matching.next().is_none(), "two headings {heading:?}");
    body
}

/// The names a list in `lines` gives, one per item: each line that starts
/// ``- `name`:``.
fn listed<'t>(lines: &[&'t str]) -> Vec<&'t str> {
    lines
        .iter()
        .filter_map(|line| line.strip_prefix("- `")?.split_once("`:"))
        .map(|(name, _)| name)
        .collect()
}

/// The text of `lines` with the exit status an error entry starts with
/// (`Exit 3.`) taken off: what the entry says beyond it.
fn beyond_the_exit_status(lines: &[&str]) -> String {
    let text = lines.join(" ");
    let text = text.trim();
    let rest = text
        .strip_prefix("Exit ")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(status, _)| status.parse::<u8>().is_ok())
        .map_or(text, |(_, rest)| rest);
    rest.trim().to_owned()
}

/// What is wrong with the names `documented` against the code's list
/// `code`: a name the code has and the document does not, one the document
/// has and the code does not, and one the document gives twice.
fn against(documented: &[&str], code: &[&str]) -> Vec<String> {
    let mut wrong: Vec<String> = code
        .iter()
        .filter(|name| !documented.contains(name))
        .map(|name| format!("{name}: in the code, not in the document"))
        .collect();
    let mut unknown: Vec<&str> = documented
        .iter()
        .copied()
        .filter(|name| !code.contains(name))
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    wrong.extend(
        unknown
            .into_iter()
            .map(|name| format!("{name}: in the document, not in the code")),
    );
    wrong.extend(
        repeated(documented)
            .into_iter()
            .map(|name| format!("{name}: twice in the document")),
    );
    wrong
}

/// The reasons the table of `lines` gives a volume: the first cell of each
/// row below the header, a name in backticks.
fn table_reasons<'t>(lines: &[&'t str]) -> Vec<&'t str> {
    lines
        .iter()
        .filter(|line| line.starts_with('|'))
        // The header and the line under it.
        .skip(2)
        .map(|row| {
            row.split('|')
                .nth(1)
                .and_then(|cell| cell.trim().strip_prefix('`')?.strip_suffix('`'))
                .unwrap_or_else(|| panic!("the row {row:?} names no reason"))
        })
        .collect()
}

/// Which of `names` the lines of `lines` outside its table name, each in
/// backticks.
fn named_beside_the_table<'n>(lines: &[&str], names: &[&'n str]) -> Vec<&'n str> {
    let words: String = lines
        .iter()
        .filter(|line| !line.starts_with('|'))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    names
        .iter()
        .copied()
        .filter(|name| words.contains(&format!("`{name}`")))
        .collect()
}

/// The reasons `clean`'s plan refuses an item for, as `source` (its
/// `clean.rs`) writes them, each once: what `fn refusal` returns
/// (`return Some("…")`), what a caller sets in its place (`(Some("…"), …)`),
/// and `UNEXPECTED`.
fn refusals_in(source: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut in_refusal = false;
    for line in source.lines() {
        if line.starts_with("fn refusal(") {
            in_refusal = true;
        } else if line == "}" {
            in_refusal = false;
        }
        let mut patterns = vec!["(Some(\"", "UNEXPECTED: &str = \""];
        if in_refusal {
            patterns.push("return Some(\"");
        }
        for pattern in patterns {
            let mut rest = line;
            while let Some(at) = rest.find(pattern) {
                rest = &rest[at + pattern.len()..];
                let end = rest.find('"').unwrap_or(rest.len());
                found.push(&rest[..end]);
                rest = &rest[end..];
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
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
fn p5_doc_02_every_entry_has_text_under_its_heading() {
    for name in DOCUMENTS {
        let text = read(&docs().join(name));
        let empty: Vec<&str> = sections(&text)
            .into_iter()
            // A `##` heading may only group the entries under it.
            .filter(|(level, _, body)| *level >= 3 && beyond_the_exit_status(body).is_empty())
            .map(|(_, heading, _)| heading)
            .collect();
        assert!(
            empty.is_empty(),
            "docs/{name}: nothing under the headings {empty:?}"
        );
    }
}

#[test]
fn p5_doc_02_the_refusal_reasons_are_clean_s() {
    let text = read(&docs().join("troubleshooting.md"));
    let documented = listed(&section(&text, "`refused_path`"));
    let wrong = against(&documented, &values::REFUSALS);
    assert!(
        wrong.is_empty(),
        "docs/troubleshooting.md, refused_path's reasons: {wrong:#?}"
    );
}

#[test]
fn p5_doc_02_the_refusal_list_is_what_clean_writes() {
    let source = read(&top().join("crates/duscope-core/src/clean.rs"));
    let written = refusals_in(&source);
    let wrong = against(&written, &values::REFUSALS);
    assert!(
        wrong.is_empty(),
        "values::REFUSALS against the reasons clean.rs refuses an item for: {wrong:#?}"
    );
}

#[test]
fn p5_doc_02_the_volume_reasons_are_the_scan_s() {
    assert!(
        DIRECTORY_REASONS
            .iter()
            .all(|reason| values::SKIP_REASONS.contains(reason)),
        "{DIRECTORY_REASONS:?} against {:?}",
        values::SKIP_REASONS
    );
    let volume_reasons: Vec<&str> = values::SKIP_REASONS
        .into_iter()
        .filter(|reason| !DIRECTORY_REASONS.contains(reason))
        .collect();
    let text = read(&docs().join("troubleshooting.md"));
    let entry = section(&text, "`mounts_excluded`");
    // A volume's reasons are the table's rows, and only those.
    let table = table_reasons(&entry);
    let wrong = against(&table, &volume_reasons);
    assert!(
        wrong.is_empty(),
        "docs/troubleshooting.md, mounts_excluded's table: {wrong:#?}"
    );
    // A directory's are named beside the table, and only those.
    let beside = named_beside_the_table(&entry, &values::SKIP_REASONS);
    let wrong = against(&beside, &DIRECTORY_REASONS);
    assert!(
        wrong.is_empty(),
        "docs/troubleshooting.md, mounts_excluded, beside the table: {wrong:#?}"
    );
}

#[test]
fn p5_doc_02_the_fall_back_reasons_are_rescan_s() {
    let text = read(&docs().join("limits.md"));
    let documented = listed(&section(
        &text,
        "FSEvents history is advisory (macOS rescans)",
    ));
    let wrong = against(&documented, &values::FALLBACK_REASONS);
    assert!(
        wrong.is_empty(),
        "docs/limits.md, the fall-back reasons: {wrong:#?}"
    );
}

#[test]
fn p5_doc_02_the_checks_find_an_empty_entry_and_a_wrong_list() {
    let text = "# T\n\n## Errors\n\n### `a`\n\nExit 2.\n\n### `b`\n\nExit 1. Why.\n\n\
                ```text\n### `fenced`\n```\n\n### `c`\n\n- `x`: one.\n- `y`: two.\n\
                - `x`: again.\n\n| `reason` | The volume |\n| --- | --- |\n| `p` | q |\n";
    let empty: Vec<&str> = sections(text)
        .into_iter()
        .filter(|(level, _, body)| *level >= 3 && beyond_the_exit_status(body).is_empty())
        .map(|(_, heading, _)| heading)
        .collect();
    assert_eq!(empty, ["`a`"]);
    let entry = section(text, "`c`");
    let documented = listed(&entry);
    assert_eq!(documented, ["x", "y", "x"]);
    assert_eq!(
        against(&documented, &["y", "z"]),
        [
            "z: in the code, not in the document",
            "x: in the document, not in the code",
            "x: twice in the document",
        ]
    );
    assert_eq!(table_reasons(&entry), ["p"]);
    let entry = [
        "| `p` | q |",
        "Beside it, `r` and `s`, not `t`.",
        "| `t` | u |",
    ];
    assert_eq!(
        named_beside_the_table(&entry, &["p", "r", "s", "t", "v"]),
        ["r", "s", "t"]
    );
    let source = "pub const UNEXPECTED: &str = \"c\";\n\
                  fn refusal(x: u8) -> Option<&'static str> {\n    \
                  if x == 0 {\n        return Some(\"a\");\n    }\n    \
                  None\n}\n\
                  fn judged() {\n    let r = (Some(\"b\"), 1);\n    \
                  return Some(\"elsewhere\");\n}\n\
                  fn other() {\n    let r = (Some(\"a\"), 2);\n}\n";
    assert_eq!(refusals_in(source), ["a", "b", "c"]);
}

#[test]
fn p5_doc_02_the_relative_links_resolve() {
    for name in DOCUMENTS {
        let file = docs().join(name);
        let broken = broken_links(&file, &read(&file));
        assert!(broken.is_empty(), "docs/{name}: {broken:#?}");
    }
    // The README leads to both.
    let readme = top().join("README.md");
    let text = read(&readme);
    let broken = broken_links(&readme, &text);
    assert!(broken.is_empty(), "README.md: {broken:#?}");
    for name in DOCUMENTS {
        assert!(
            links(&text)
                .iter()
                .any(|link| link == &format!("docs/{name}")),
            "README.md has no link to docs/{name}"
        );
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
