//! P0-TEST-01: every integration test that runs the duscope binary or reads
//! the fixture tree constructs a `TestEnv`.
//!
//! The rule covers the `.rs` files under the workspace's `tests/`
//! directories (not the `src/` of a crate kept there, such as mktree's). A
//! file is in scope when it runs the binary (`CARGO_BIN_EXE_duscope`,
//! `.cmd()`, `duscope_bin()`, `cargo_bin("duscope")`,
//! `Command::new("duscope")`) or reads the shared fixture tree
//! (`fixtures()`, `target/fixtures`, `OutDir::workspace_default`); an
//! in-scope file must call `TestEnv::new()`. Comment lines count for
//! neither. Tests that only read repository files or run `cargo metadata`
//! are out of scope. Exemptions are listed below with their reason, and an
//! exemption for a file that no longer exists fails the test.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

const RUNS_THE_BINARY: &[&str] = &[
    "CARGO_BIN_EXE_duscope",
    ".cmd()",
    "duscope_bin()",
    "cargo_bin(\"duscope\")",
    "Command::new(\"duscope\")",
];
const READS_THE_FIXTURES: &[&str] = &["fixtures()", "target/fixtures", "OutDir::workspace_default"];
const CONSTRUCTS: &str = "TestEnv::new()";

/// Files exempt from the rule, with the reason.
const EXEMPT: &[(&str, &str)] = &[(
    "duscope/tests/scaffold.rs",
    "P0-REPO-01's test; it runs only `duscope --version`, which reads no environment variable, \
     and a package never edits another package's tests",
)];

fn workspace() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.ancestors().nth(2).unwrap().to_owned()
}

/// A test source without its comment lines (`//`, `///`, `//!`), so a
/// `TestEnv::new()` mentioned in a comment constructs nothing.
fn code(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a test source falls under the rule.
fn in_scope(source: &str) -> bool {
    let code = code(source);
    RUNS_THE_BINARY
        .iter()
        .chain(READS_THE_FIXTURES)
        .any(|p| code.contains(p))
}

/// Whether a test source breaks the rule.
fn violates(source: &str) -> bool {
    in_scope(source) && !code(source).contains(CONSTRUCTS)
}

/// The integration-test sources of the workspace, relative to its root.
fn test_sources(root: &Path, dir: &Path, under_tests: bool, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            let skip = name.starts_with('.')
                || ["target", "plan", "src", "node_modules"].contains(&name.as_ref());
            if !skip {
                test_sources(root, &path, under_tests || name == "tests", out);
            }
        } else if under_tests && kind.is_file() && name.ends_with(".rs") {
            out.push(path.strip_prefix(root).unwrap().to_owned());
        }
    }
}

#[test]
fn p0_test_01_every_integration_test_constructs_a_testenv() {
    let root = workspace();
    let mut files = Vec::new();
    test_sources(&root, &root, false, &mut files);
    let this = Path::new("crates/duscope-testkit/tests/testenv_everywhere.rs");
    assert!(
        files.iter().any(|f| f == this),
        "the walk found no tests: {files:?}"
    );

    let mut violations = Vec::new();
    let mut checked = Vec::new();
    for file in files.iter().filter(|f| f.as_path() != this) {
        let source = fs::read_to_string(root.join(file)).unwrap();
        if !in_scope(&source) {
            continue;
        }
        if EXEMPT.iter().any(|(path, _)| Path::new(path) == file) {
            continue;
        }
        checked.push(file.display().to_string());
        if violates(&source) {
            violations.push(file.display().to_string());
        }
    }
    eprintln!("p0_test_01: in scope and constructing a TestEnv: {checked:?}");
    assert!(
        violations.is_empty(),
        "these tests run the duscope binary or read the fixture tree without constructing \
         duscope_testkit::TestEnv (call TestEnv::new()): {violations:?}"
    );
    for (path, reason) in EXEMPT {
        assert!(
            root.join(path).is_file(),
            "stale exemption for {path} ({reason})"
        );
    }
}

#[test]
fn p0_test_01_grep_rule_classifies_sources() {
    let binary = "let out = Command::new(env!(\"CARGO_BIN_EXE_duscope\")).output();";
    assert!(violates(binary));
    assert!(!violates(&format!("let env = TestEnv::new();\n{binary}")));
    assert!(violates("let out = env.cmd().arg(\"top\").output();"));
    assert!(violates("let tree = fixtures().root();"));
    assert!(violates("let tree = env.fixtures().root();"));
    assert!(violates(
        "let p = Path::new(\"target/fixtures.noindex/tree\");"
    ));
    assert!(violates(
        "let out = Command::new(duscope_testkit::duscope_bin()).output();"
    ));
    assert!(violates(&format!(
        "// Construct one with TestEnv::new() first.\n{binary}"
    )));
    assert!(!in_scope(
        "/// Runs `env.cmd()` on the tree.\nfn helper() {}"
    ));
    assert!(!in_scope("let meta = MetadataCommand::new().exec();"));
    assert!(!in_scope(
        "let text = std::fs::read_to_string(\"CLAUDE.md\");"
    ));
}
