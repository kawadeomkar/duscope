//! P0-REPO-02: the agent and contributor files agree with the justfile.
//!
//! These tests only read files from the repository; none of them runs the
//! binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The ceiling the spec sets for `CLAUDE.md`: it must stay under this many lines.
const CLAUDE_MD_MAX_LINES: usize = 200;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &str) -> String {
    let full = repo_root().join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("cannot read {}: {e}", full.display()))
}

/// The byte offset of the first `:` outside quotes and backticks.
fn first_unquoted_colon(text: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (None, '"' | '\'' | '`') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ':') => return Some(i),
            _ => {}
        }
    }
    None
}

/// The recipe and alias names the justfile defines.
fn justfile_recipes() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in read("justfile").lines() {
        // Recipe bodies are indented; comments and attributes are not recipes.
        if line.trim().is_empty() || line.starts_with([' ', '\t', '#', '[']) {
            continue;
        }
        let mut words = line.split_whitespace();
        match words.next() {
            Some("alias") => {
                if let Some(name) = words.next() {
                    names.insert(name.to_owned());
                }
                continue;
            }
            Some("set" | "export" | "import" | "mod") | None => continue,
            Some(_) => {}
        }
        // A recipe header is `[@]name [params]: [deps]`; an assignment is `name := value`.
        let header = line.strip_prefix('@').unwrap_or(line);
        let name_len = header
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(header.len());
        let (name, rest) = header.split_at(name_len);
        if name.is_empty() {
            continue;
        }
        if let Some(colon) = first_unquoted_colon(rest)
            && !rest[colon + 1..].starts_with('=')
        {
            names.insert(name.to_owned());
        }
    }
    names
}

/// The shell commands a markdown file shows: every line of a fenced code block
/// and every inline code span outside one, with the line it starts on. A code
/// span may wrap onto the next line (the line break reads as a space) but
/// never past a blank line or a fence.
fn commands_in_markdown(text: &str) -> Vec<(usize, String)> {
    let mut commands = Vec::new();
    let mut fenced = false;
    let mut open: Option<(usize, String)> = None;
    for (number, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l)) {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            open = None;
            continue;
        }
        if fenced {
            commands.push((number, line.to_owned()));
            continue;
        }
        if line.trim().is_empty() {
            open = None;
            continue;
        }
        // Each backtick opens a span or closes the open one; the text before
        // the line's first backtick continues a span left open above.
        let mut pieces = line.split('`');
        if let (Some((_, span)), Some(first)) = (open.as_mut(), pieces.next()) {
            span.push(' ');
            span.push_str(first);
        }
        for piece in pieces {
            match open.take() {
                Some(closed) => commands.push(closed),
                None => open = Some((number, piece.to_owned())),
            }
        }
    }
    commands
}

/// Every `just <recipe>` a markdown file names, with its line number.
fn just_recipes_named(text: &str) -> Vec<(usize, String)> {
    let mut named = Vec::new();
    for (number, command) in commands_in_markdown(text) {
        for piece in command.split(['&', ';', '|']) {
            let mut words = piece.split_whitespace();
            let mut first = words.next();
            if first == Some("$") {
                first = words.next();
            }
            if first != Some("just") {
                continue;
            }
            // `just` alone runs the default recipe; `just --list` is a flag, not a recipe.
            if let Some(recipe) = words.next().filter(|w| !w.starts_with('-')) {
                named.push((number, recipe.to_owned()));
            }
        }
    }
    named
}

fn assert_named_recipes_exist(path: &str, must_name: &[&str]) {
    let recipes = justfile_recipes();
    let named = just_recipes_named(&read(path));
    let missing: Vec<String> = named
        .iter()
        .filter(|(_, recipe)| !recipes.contains(recipe))
        .map(|(line, recipe)| format!("{path}:{line}: just {recipe}"))
        .collect();
    assert!(
        missing.is_empty(),
        "commands {path} names that the justfile does not define: {missing:?} (recipes: {recipes:?})"
    );
    let named: BTreeSet<&str> = named.iter().map(|(_, recipe)| recipe.as_str()).collect();
    for recipe in must_name {
        assert!(
            named.contains(recipe),
            "{path} does not name `just {recipe}`"
        );
    }
}

/// The table under a `## <heading>` section, keyed by the first code span (or
/// the text) of each row's first cell, with the second cell as the value.
fn markdown_table(text: &str, heading: &str) -> BTreeMap<String, String> {
    let mut rows = BTreeMap::new();
    let mut in_section = false;
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            in_section = title.trim() == heading;
            continue;
        }
        if !in_section || !line.trim_start().starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 2 || cells[0].chars().all(|c| matches!(c, '-' | ':' | ' ')) {
            continue;
        }
        let key = cells[0]
            .split('`')
            .nth(1)
            .unwrap_or(cells[0])
            .trim()
            .to_owned();
        rows.insert(key, cells[1].replace('`', ""));
    }
    rows
}

/// The `<tool>_version := "<version>"` pins of the justfile, by tool name
/// (`cargo_deny_version` pins `cargo-deny`).
fn justfile_tool_pins() -> BTreeMap<String, String> {
    let mut pins = BTreeMap::new();
    for line in read("justfile").lines() {
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((variable, value)) = line.split_once(":=") else {
            continue;
        };
        if let Some(tool) = variable.trim().strip_suffix("_version") {
            pins.insert(
                tool.replace('_', "-"),
                value.trim().trim_matches('"').to_owned(),
            );
        }
    }
    pins
}

fn toolchain_channel() -> String {
    read("rust-toolchain.toml")
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "channel").then(|| value.trim().trim_matches('"').to_owned())
        })
        .expect("rust-toolchain.toml has a channel")
}

fn is_exact_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn p0_repo_02_claude_md_is_under_200_lines() {
    let lines = read("CLAUDE.md").lines().count();
    assert!(
        lines < CLAUDE_MD_MAX_LINES,
        "CLAUDE.md has {lines} lines; the spec keeps it under {CLAUDE_MD_MAX_LINES}"
    );
}

#[test]
fn p0_repo_02_claude_md_commands_exist_in_justfile() {
    assert_named_recipes_exist(
        "CLAUDE.md",
        &["setup", "check", "check-full", "test-pkg", "test-crate"],
    );
}

#[test]
fn p0_repo_02_contributing_commands_exist_in_justfile() {
    assert_named_recipes_exist("CONTRIBUTING.md", &["setup", "check", "test-pkg"]);
}

#[test]
fn p0_repo_02_contributing_tool_versions_match_justfile() {
    let pins = justfile_tool_pins();
    assert!(!pins.is_empty(), "the justfile pins no tool versions");
    let table = markdown_table(&read("CONTRIBUTING.md"), "Tool versions");
    assert!(
        !table.is_empty(),
        "CONTRIBUTING.md has no `## Tool versions` table"
    );

    for (tool, version) in &pins {
        assert_eq!(
            table.get(tool).map(String::as_str),
            Some(version.as_str()),
            "CONTRIBUTING.md's tool versions table must list `{tool}` at the justfile's {version}"
        );
    }
    for (tool, version) in &table {
        assert!(
            !is_exact_version(version) || pins.contains_key(tool),
            "CONTRIBUTING.md pins `{tool}` {version}, but the justfile does not install it"
        );
    }

    let channel = toolchain_channel();
    let rust = table
        .get("rustup")
        .expect("the tool versions table has a `rustup` row for the toolchains");
    assert!(
        rust.starts_with(&channel),
        "the `rustup` row says {rust:?}, but rust-toolchain.toml pins {channel}"
    );
}

#[test]
fn p0_repo_02_gitignore_keeps_plan_first() {
    let gitignore = read(".gitignore");
    let lines: Vec<&str> = gitignore.lines().collect();
    assert_eq!(
        lines.first(),
        Some(&"plan/"),
        "plan/ must stay the first line of .gitignore"
    );
    assert!(lines.contains(&"target/"), ".gitignore must ignore target/");
}

#[test]
fn p0_repo_02_pull_request_template_asks_for_every_section() {
    let template = read(".github/PULL_REQUEST_TEMPLATE.md");
    for section in [
        "## What was done",
        "## Acceptance",
        "## Not covered by the spec",
        "## Deviation",
        "## Changelog",
        "macOS",
        "Linux",
        "changelog.d/",
        "no-changelog",
        "BLOCKED — proposed deviation",
    ] {
        assert!(
            template.contains(section),
            "the pull request template does not ask for {section:?}"
        );
    }
    let last = template
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    assert!(
        last.starts_with("Tracker: "),
        "the pull request template must end with the `Tracker:` line, not {last:?}"
    );
}

#[test]
fn p0_repo_02_issue_templates_have_front_matter() {
    let dir = repo_root().join(".github/ISSUE_TEMPLATE");
    let mut templates = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "md") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            templates.insert(name, fs::read_to_string(&path).unwrap());
        }
    }
    for (file, text) in &templates {
        // GitHub lists a markdown template in the chooser only with `name` and `about`.
        let Some((front, _)) = text
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
        else {
            panic!("{file} does not start with a `---` front matter block");
        };
        for key in ["name", "about"] {
            let value = front
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}:")))
                .map(|v| v.trim().trim_matches(['"', '\'']))
                .unwrap_or_default();
            assert!(
                !value.is_empty(),
                "{file} has no `{key}` in its front matter"
            );
        }
    }

    let find = |needle: &str| {
        let Some((_, text)) = templates.iter().find(|(file, _)| file.contains(needle)) else {
            panic!("no issue template named *{needle}*: {templates:?}");
        };
        text.as_str()
    };
    assert!(
        find("bug").contains("duscope --debug"),
        "the bug template asks for the `duscope --debug` output"
    );
    find("feature");
    let deviation = find("deviation");
    for field in [
        "Finding",
        "Evidence",
        "Proposal rows revised",
        "Packages affected",
        "Proposed change",
    ] {
        assert!(
            deviation.contains(field),
            "the deviation template asks for {field:?}"
        );
    }
}

#[test]
fn p0_repo_02_codeowners_names_the_maintainer() {
    let codeowners = read(".github/CODEOWNERS");
    assert!(
        codeowners
            .lines()
            .any(|line| line.split_whitespace().collect::<Vec<_>>() == ["*", "@kawadeomkar"]),
        "CODEOWNERS must give every path to @kawadeomkar"
    );
}

#[test]
fn p0_repo_02_justfile_parser_sees_recipes_not_assignments() {
    let recipes = justfile_recipes();
    for recipe in ["setup", "check", "test-pkg", "deny", "repro", "default"] {
        assert!(
            recipes.contains(recipe),
            "missing recipe {recipe}: {recipes:?}"
        );
    }
    for assignment in ["size_budget", "cargo_deny_version", "set"] {
        assert!(
            !recipes.contains(assignment),
            "{assignment} is not a recipe: {recipes:?}"
        );
    }
}

#[test]
fn p0_repo_02_markdown_code_spans_may_wrap() {
    let text = "Run `just\ncheck` first, then `just test-pkg P0-REPO-02`.\n\n`unclosed\n\n\
                prose `just docs`\n```sh\njust size\n```\n";
    assert_eq!(
        commands_in_markdown(text),
        [
            (1, "just check".to_owned()),
            (2, "just test-pkg P0-REPO-02".to_owned()),
            (6, "just docs".to_owned()),
            (8, "just size".to_owned()),
        ]
    );
    // CLAUDE.md wraps a code span this way: the span after it is still code.
    let claude = "(`#![cfg_attr(test, allow(clippy::unwrap_used,\n  clippy::expect_used))]` \
                  at each crate root); no `println!` outside";
    let spans: Vec<String> = commands_in_markdown(claude)
        .into_iter()
        .map(|(_, span)| span)
        .collect();
    assert_eq!(
        spans.last().map(String::as_str),
        Some("println!"),
        "{spans:?}"
    );
}
