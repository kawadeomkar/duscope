//! P0-REPO-04: `DEPENDENCIES.md` against `cargo metadata`, the licences of the
//! dev-only crates, and the one exemption dev-only crates get from `cargo deny`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_metadata::semver::{Op, VersionReq};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Node, Package, PackageId};

/// The columns of both tables in `DEPENDENCIES.md`, in order.
const COLUMNS: [&str; 7] = [
    "Crate",
    "Purpose",
    "Alternative considered",
    "Why not in-tree",
    "Pin",
    "Maintainers",
    "Last release",
];
const PIN: usize = 4;

/// The direct normal and build dependencies of the published crates.
const PUBLISHED_TABLE: &str = "## Dependencies of the published crates";
/// Everything else a workspace member depends on directly: dev-dependencies, and
/// the dependencies of unpublished members (the test kit, the fixture generator).
const DEV_TABLE: &str = "## Dev-dependencies";

/// Crate name to the version requirements the manifests give it.
type Requirements = BTreeMap<String, BTreeSet<String>>;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(file: &str) -> String {
    std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn is_published(package: &Package) -> bool {
    package
        .publish
        .as_ref()
        .is_none_or(|registries| !registries.is_empty())
}

/// The external crates the workspace members name directly, split into the two
/// tables. A crate in both is listed once, in the published table.
fn direct_dependencies(metadata: &Metadata) -> (Requirements, Requirements) {
    let packages = metadata.workspace_packages();
    let members: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let (mut published, mut dev) = (Requirements::new(), Requirements::new());
    for package in packages {
        for dep in &package.dependencies {
            if members.contains(dep.name.as_str()) {
                continue;
            }
            let table = if is_published(package) && dep.kind != DependencyKind::Development {
                &mut published
            } else {
                &mut dev
            };
            table
                .entry(dep.name.clone())
                .or_default()
                .insert(dep.req.to_string());
        }
    }
    dev.retain(|name, _| !published.contains_key(name));
    (published, dev)
}

/// The lines between `heading` and the next heading, or `None` without the heading.
fn section<'a>(markdown: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let mut lines = markdown
        .lines()
        .skip_while(|line| line.trim_end() != heading);
    lines.next()?;
    Some(lines.take_while(|line| !line.starts_with('#')).collect())
}

/// The cells of a table row, trimmed; `\|` is a literal pipe inside a cell.
fn cells(row: &str) -> Vec<String> {
    let row = row.trim().replace("\\|", "\u{0}");
    let row = row.strip_prefix('|').unwrap_or(&row);
    let row = row.strip_suffix('|').unwrap_or(row);
    row.split('|')
        .map(|cell| cell.trim().replace('\u{0}', "|"))
        .collect()
}

fn unquote(cell: &str) -> &str {
    cell.trim_matches('`')
}

/// What is wrong with the table under `heading` against the crates the manifests
/// name: a header other than `COLUMNS`, a missing, extra or repeated crate, a pin
/// that is not the manifests' requirement, or a 0.x crate not pinned with `=`.
fn table_problems(markdown: &str, heading: &str, expected: &Requirements) -> Vec<String> {
    let Some(lines) = section(markdown, heading) else {
        return vec![format!("no `{heading}` section")];
    };
    let mut problems = Vec::new();
    let mut rows = lines
        .into_iter()
        .filter(|line| line.trim_start().starts_with('|'));
    if rows.next().map(cells) != Some(COLUMNS.map(String::from).to_vec()) {
        problems.push(format!(
            "{heading}: the header is not | {} |",
            COLUMNS.join(" | ")
        ));
    }
    let mut listed = BTreeMap::new();
    for row in rows.map(cells) {
        if row
            .iter()
            .all(|cell| !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':'))
        {
            continue; // the separator row
        }
        if row.len() != COLUMNS.len() {
            problems.push(format!("{heading}: a row has {} cells: {row:?}", row.len()));
            continue;
        }
        let name = unquote(&row[0]).to_owned();
        if listed
            .insert(name.clone(), unquote(&row[PIN]).to_owned())
            .is_some()
        {
            problems.push(format!("{heading}: `{name}` is listed twice"));
        }
    }
    for (name, requirements) in expected {
        let pin = requirements.iter().cloned().collect::<Vec<_>>().join(", ");
        match listed.get(name) {
            None => problems.push(format!("{heading}: `{name}` ({pin}) is missing")),
            Some(listed_pin) if *listed_pin != pin => problems.push(format!(
                "{heading}: `{name}` is pinned `{listed_pin}` here and `{pin}` in the manifests"
            )),
            Some(_) => {}
        }
        for requirement in requirements {
            let loose_zero = VersionReq::parse(requirement)
                .unwrap()
                .comparators
                .iter()
                .any(|c| c.major == 0 && c.op != Op::Exact);
            if loose_zero {
                problems.push(format!(
                    "{heading}: `{name}` {requirement}: a 0.x crate is pinned with `=`"
                ));
            }
        }
    }
    for name in listed.keys().filter(|name| !expected.contains_key(*name)) {
        problems.push(format!(
            "{heading}: `{name}` is listed but is not a direct dependency in this table's scope"
        ));
    }
    problems
}

#[test]
fn p0_repo_04_dependencies_md_matches_cargo_metadata() {
    let metadata = MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .no_deps()
        .exec()
        .unwrap();
    let (published, dev) = direct_dependencies(&metadata);
    assert!(
        dev.contains_key("cargo_metadata"),
        "cargo_metadata, this test's dev-dependency, is missing from the dev table's scope"
    );
    let markdown = read("DEPENDENCIES.md");
    let mut problems = table_problems(&markdown, PUBLISHED_TABLE, &published);
    problems.extend(table_problems(&markdown, DEV_TABLE, &dev));
    assert!(
        problems.is_empty(),
        "DEPENDENCIES.md disagrees with cargo metadata:\n{}",
        problems.join("\n")
    );
}

#[test]
fn p0_repo_04_table_check_reports_missing_extra_and_stale_rows() {
    let markdown = "\
# Dependencies

## Dev-dependencies

| Crate | Purpose | Alternative considered | Why not in-tree | Pin | Maintainers | Last release |
| --- | --- | --- | --- | --- | --- | --- |
| `kept` | a \\| b | c | d | `=0.1.0` | 2 | 0.1.0, 2026-01-01 |
| `stale` | a | b | c | `=0.2.0` | 2 | 0.3.0, 2026-01-01 |
| `extra` | a | b | c | `^1.0.0` | 2 | 1.0.0, 2026-01-01 |
| `stale` | a | b | c | `=0.2.0` | 2 | 0.3.0, 2026-01-01 |

## Something else

| `missing` | a | b | c | `^1.2.0` | 2 | 1.2.0, 2026-01-01 |
";
    let expected: Requirements = [
        ("kept", "=0.1.0"),
        ("stale", "=0.3.0"),
        ("missing", "^1.2.0"),
        ("loose", "^0.4.0"),
    ]
    .into_iter()
    .map(|(name, req)| (name.to_owned(), BTreeSet::from([req.to_owned()])))
    .collect();
    assert_eq!(
        table_problems(markdown, DEV_TABLE, &expected),
        [
            "## Dev-dependencies: `stale` is listed twice",
            "## Dev-dependencies: `loose` (^0.4.0) is missing",
            "## Dev-dependencies: `loose` ^0.4.0: a 0.x crate is pinned with `=`",
            "## Dev-dependencies: `missing` (^1.2.0) is missing",
            "## Dev-dependencies: `stale` is pinned `=0.2.0` here and `=0.3.0` in the manifests",
            "## Dev-dependencies: `extra` is listed but is not a direct dependency in this table's scope",
        ]
    );
    assert_eq!(
        table_problems(markdown, PUBLISHED_TABLE, &Requirements::new()),
        ["no `## Dependencies of the published crates` section"]
    );
    let renamed = markdown.replace("| Pin |", "| Version |");
    assert_eq!(
        table_problems(&renamed, DEV_TABLE, &expected)[0],
        "## Dev-dependencies: the header is not | Crate | Purpose | Alternative considered \
         | Why not in-tree | Pin | Maintainers | Last release |"
    );
}

/// The `[licenses] allow` list of `deny.toml`.
fn allowed_licences() -> BTreeSet<String> {
    let deny = read("deny.toml");
    let mut list = String::new();
    for line in deny
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(code, _)| code).trim())
        .skip_while(|line| *line != "[licenses]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .skip_while(|line| !line.starts_with("allow "))
    {
        list.push_str(line);
        if line.ends_with(']') {
            break;
        }
    }
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// Whether an SPDX expression is satisfied by the allow-list: `OR` needs one
/// operand allowed, `AND` needs every operand, `id WITH exception` is allowed only
/// when listed whole, and `/` is the legacy spelling of `OR`. `None` when the
/// expression does not parse.
fn licence_allowed(expression: &str, allow: &BTreeSet<String>) -> Option<bool> {
    let spaced = expression
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut position = 0;
    let allowed = any_of(&tokens, &mut position, allow)?;
    (position == tokens.len()).then_some(allowed)
}

fn any_of(tokens: &[&str], position: &mut usize, allow: &BTreeSet<String>) -> Option<bool> {
    let mut allowed = all_of(tokens, position, allow)?;
    while tokens.get(*position) == Some(&"OR") {
        *position += 1;
        allowed |= all_of(tokens, position, allow)?;
    }
    Some(allowed)
}

fn all_of(tokens: &[&str], position: &mut usize, allow: &BTreeSet<String>) -> Option<bool> {
    let mut allowed = licence(tokens, position, allow)?;
    while tokens.get(*position) == Some(&"AND") {
        *position += 1;
        allowed &= licence(tokens, position, allow)?;
    }
    Some(allowed)
}

fn licence(tokens: &[&str], position: &mut usize, allow: &BTreeSet<String>) -> Option<bool> {
    let token = *tokens.get(*position)?;
    *position += 1;
    match token {
        "(" => {
            let allowed = any_of(tokens, position, allow)?;
            if tokens.get(*position) != Some(&")") {
                return None;
            }
            *position += 1;
            Some(allowed)
        }
        ")" | "OR" | "AND" | "WITH" => None,
        id if tokens.get(*position) == Some(&"WITH") => {
            let exception = tokens.get(*position + 1)?;
            *position += 2;
            Some(allow.contains(&format!("{id} WITH {exception}")))
        }
        id => Some(allow.contains(id)),
    }
}

#[test]
fn p0_repo_04_licence_expressions_follow_spdx_operators() {
    let allow: BTreeSet<String> = [
        "MIT",
        "Apache-2.0",
        "Unicode-3.0",
        "Apache-2.0 WITH LLVM-exception",
    ]
    .map(String::from)
    .into();
    for (expression, expected) in [
        ("MIT", Some(true)),
        ("GPL-3.0-only", Some(false)),
        ("MIT OR GPL-3.0-only", Some(true)),
        ("MIT AND GPL-3.0-only", Some(false)),
        ("(MIT OR Apache-2.0) AND Unicode-3.0", Some(true)),
        ("(MIT OR Apache-2.0) AND Unicode-DFS-2016", Some(false)),
        ("MIT/Apache-2.0", Some(true)),
        ("Apache-2.0 WITH LLVM-exception", Some(true)),
        ("GPL-2.0-only WITH Classpath-exception-2.0", Some(false)),
        ("MIT OR", None),
        ("(MIT", None),
        ("MIT Apache-2.0", None),
        ("", None),
    ] {
        assert_eq!(
            licence_allowed(expression, &allow),
            expected,
            "{expression:?}"
        );
    }
}

/// The host triple of the cargo running the tests.
fn host() -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo).arg("-vV").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("cargo -vV prints the host")
        .to_owned()
}

/// The packages reachable from the workspace only through a dev-dependency edge:
/// the ones `[graph] exclude-dev` removes from every `cargo deny` check.
fn dev_only_packages(metadata: &Metadata) -> Vec<&Package> {
    let nodes: BTreeMap<&PackageId, &Node> = metadata
        .resolve
        .as_ref()
        .expect("cargo metadata resolved the graph")
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect();
    let mut checked_by_deny = BTreeSet::new();
    let mut queue: Vec<&PackageId> = metadata.workspace_members.iter().collect();
    while let Some(id) = queue.pop() {
        if checked_by_deny.insert(id) {
            queue.extend(
                nodes[id]
                    .deps
                    .iter()
                    .filter(|dep| {
                        dep.dep_kinds
                            .iter()
                            .any(|k| k.kind != DependencyKind::Development)
                    })
                    .map(|dep| &dep.pkg),
            );
        }
    }
    metadata
        .packages
        .iter()
        .filter(|package| !checked_by_deny.contains(&package.id))
        .collect()
}

#[test]
fn p0_repo_04_dev_only_crates_pass_the_licence_allow_list() {
    let allow = allowed_licences();
    assert!(allow.contains("MIT"), "deny.toml allows {allow:?}");
    // The host's graph only: other platforms' crates may not be downloaded, and CI
    // runs this test on both macOS and Linux.
    let metadata = MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .other_options(vec!["--filter-platform".to_owned(), host()])
        .exec()
        .unwrap();
    let dev_only = dev_only_packages(&metadata);
    assert!(
        dev_only.iter().any(|p| p.name.as_str() == "cargo_metadata"),
        "cargo_metadata, a dev-dependency, was reached through normal or build edges"
    );
    let refused: Vec<String> = dev_only
        .iter()
        .filter_map(|package| {
            let expression = package.license.as_deref().unwrap_or("");
            match licence_allowed(expression, &allow) {
                Some(true) => None,
                Some(false) => Some(format!(
                    "{} {}: {expression}",
                    package.name, package.version
                )),
                None => Some(format!(
                    "{} {}: no SPDX `license` expression this test can read ({expression:?})",
                    package.name, package.version
                )),
            }
        })
        .collect();
    assert!(
        refused.is_empty(),
        "dev-only crates whose licence is not on deny.toml's [licenses] allow list:\n{}",
        refused.join("\n")
    );
}

#[test]
fn p0_repo_04_dev_dependencies_are_exempt_from_the_bans_only() {
    let code = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| line.split_once('#').map_or(line, |(code, _)| code).trim())
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    };
    assert!(
        !code(&read("deny.toml"))
            .iter()
            .any(|line| line.starts_with("exclude-dev")),
        "deny.toml's [graph] exclude-dev takes dev-dependencies out of every check, \
         not only the bans"
    );
    let justfile = read("justfile");
    let recipe: Vec<String> = justfile
        .lines()
        .skip_while(|line| !line.starts_with("deny "))
        .take_while(|line| line.starts_with("deny ") || line.starts_with("    "))
        .map(str::to_owned)
        .collect();
    assert!(
        recipe.first().is_some_and(|l| l.contains("bans")),
        "the deny recipe's default checks include the bans: {recipe:?}"
    );
    let runs: Vec<&String> = recipe.iter().filter(|l| l.contains("cargo deny")).collect();
    assert!(
        !runs.is_empty(),
        "the deny recipe runs no cargo deny: {recipe:?}"
    );
    for run in runs {
        if run.contains("--exclude-dev") {
            let checks: Vec<&str> = run
                .split_once(" check ")
                .map_or("", |(_, rest)| rest)
                .split(|c: char| !c.is_ascii_alphabetic())
                .filter(|word| !word.is_empty() && *word != "fi")
                .collect();
            assert_eq!(checks, ["bans"], "{run}");
        }
    }
}
