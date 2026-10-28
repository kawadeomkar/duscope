//! P1-DOC-01: the README's tour, as a doc test on the fixture tree. Each
//! `console` block of the "A tour" section is a command and its output; the
//! command runs on the fixture tree's home (`~`) under the testkit's clock.
//!
//! The README shows macOS's output (APFS): on macOS each block must be what
//! the command prints, stdout then the summary line on stderr (stdout alone
//! for `--plain` and `--json`), with only the scan's elapsed time and thread
//! count masked. Allocated sizes differ on
//! Linux, so there the blocks are held to `golden/readme_tour@linux.snap`
//! (regenerated as every golden is), and the README's own rows to the paths
//! or classes Linux prints.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use duscope_testkit::TestEnv;

/// One block of the tour: the command line after `$ `, duscope's arguments,
/// the `jq` arguments when the command pipes into jq, and the shown lines.
struct Block {
    command: String,
    args: Vec<String>,
    jq: Option<Vec<String>>,
    shown: Vec<String>,
}

/// The tour's blocks, in order.
fn tour() -> Vec<Block> {
    let readme =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../README.md")).unwrap();
    let section = readme
        .split("\n## A tour\n")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    let mut blocks = Vec::new();
    for block in section.split("```console\n").skip(1) {
        let body = block.split("\n```").next().unwrap();
        let mut lines = body.lines();
        let command = lines.next().unwrap().strip_prefix("$ ").unwrap();
        let (duscope, jq) = match command.split_once(" | jq ") {
            // `jq [flags] '<filter>'`
            Some((duscope, jq)) => {
                let (flags, filter) = jq.split_once('\'').unwrap();
                let mut args: Vec<String> = flags.split_whitespace().map(str::to_owned).collect();
                args.push(filter.strip_suffix('\'').unwrap().to_owned());
                (duscope, Some(args))
            }
            None => (command, None),
        };
        let args = duscope
            .strip_prefix("duscope ")
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        blocks.push(Block {
            command: command.to_owned(),
            args,
            jq,
            shown: lines.map(str::to_owned).collect(),
        });
    }
    blocks
}

/// What the block's command prints on this machine, as the README shows it:
/// stdout (through jq when piped), then stderr unless the block is in a
/// machine mode, the home's path as `~`.
fn run(env: &TestEnv, home: &str, block: &Block) -> Vec<String> {
    let mut cmd = env.cmd();
    for arg in &block.args {
        cmd.arg(arg.replacen('~', home, 1));
    }
    let machine = block.args.iter().any(|a| a == "--plain" || a == "--json");
    if !machine {
        cmd.args(["--format", "table"]);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}: {}",
        block.command,
        String::from_utf8_lossy(&out.stderr)
    );
    let mut stdout = out.stdout;
    if let Some(args) = &block.jq {
        let mut jq = Command::new("jq")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("jq (it ships with macOS 15 and later; apt-get install jq)");
        jq.stdin.take().unwrap().write_all(&stdout).unwrap();
        let filtered = jq.wait_with_output().unwrap();
        assert!(filtered.status.success(), "{}: jq failed", block.command);
        stdout = filtered.stdout;
    }
    // The machine modes' blocks show the payload alone; the README says the
    // summary line goes to stderr.
    let mut text = String::from_utf8(stdout).unwrap();
    if !machine {
        text += &String::from_utf8(out.stderr).unwrap();
    }
    text.replace(home, "~").lines().map(str::to_owned).collect()
}

/// `line` with what differs between runs and machines masked: the elapsed
/// time and the thread count at the end of the summary line.
fn masked(line: &str) -> String {
    match line.rfind(" · ") {
        Some(at) if line.contains(" allocated · ") => {
            format!("{} · <elapsed, threads>", &line[..at])
        }
        _ => line.to_owned(),
    }
}

/// What a line shows besides numbers: its last column (a path, a class or a
/// header), or nothing for the summary line and a JSON row's sizes.
fn shown(line: &str) -> Option<String> {
    if line.contains(" allocated · ") {
        return None;
    }
    if line.starts_with('{') {
        return line
            .split("\"path\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .map(str::to_owned);
    }
    let last = line
        .split(['\t'])
        .next_back()
        .unwrap()
        .rsplit("  ")
        .next()
        .unwrap()
        .trim();
    Some(last.to_owned())
}

#[test]
fn p1_doc_01_the_readme_tour_runs_on_the_fixture_tree() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let home = home.to_str().unwrap();
    let blocks = tour();
    assert!(blocks.len() >= 6, "the tour has {} blocks", blocks.len());
    for block in &blocks {
        let actual = run(&env, home, block);
        let mut want: Vec<String> = block.shown.iter().filter_map(|l| shown(l)).collect();
        let mut got: Vec<String> = actual.iter().filter_map(|l| shown(l)).collect();
        // The header first, then the rows in any order: sizes, and so the
        // order of near rows, differ between filesystems.
        assert_eq!(want.first(), got.first(), "{}: the header", block.command);
        want.sort();
        got.sort();
        assert_eq!(want, got, "{}:\n{}", block.command, actual.join("\n"));
    }
}

#[test]
fn p1_doc_01_the_readme_tour_shows_this_platforms_output() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let home = home.to_str().unwrap();
    let blocks = tour();
    let actual: Vec<Vec<String>> = blocks.iter().map(|b| run(&env, home, b)).collect();
    if std::env::consts::OS == "macos" {
        for (block, actual) in blocks.iter().zip(&actual) {
            let want: Vec<String> = block.shown.iter().map(|l| masked(l)).collect();
            let got: Vec<String> = actual.iter().map(|l| masked(l)).collect();
            assert_eq!(
                want,
                got,
                "README.md's `{}` block is not what it prints; paste:\n{}",
                block.command,
                actual.join("\n")
            );
        }
        return;
    }
    // Elsewhere the README's numbers are macOS's; the platform's own are a
    // golden file, one block per command.
    let text: String = blocks
        .iter()
        .zip(&actual)
        .map(|(block, actual)| {
            let lines: Vec<String> = actual.iter().map(|l| masked(l)).collect();
            format!("$ {}\n{}\n", block.command, lines.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let name = format!("readme_tour@{}.snap", std::env::consts::OS);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(&name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, &text).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        text, expected,
        "golden {name} differs; rerun with DUSCOPE_TEST_UPDATE_GOLDEN=1 to regenerate"
    );
}
