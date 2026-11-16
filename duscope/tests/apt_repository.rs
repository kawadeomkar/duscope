//! P5-PKG-01: the signed apt repository's own files. reprepro's configuration
//! (`apt/conf/`) has the `stable` and `testing` suites, the deb822 sources file
//! names the repository and its `Signed-By` keyring, docs/apt.md's install
//! commands fetch both and records no fingerprint until the key exists, and
//! `scripts/apt-repo.sh select` gives each suite its release.
//!
//! These tests read files and run the script's `select` and `check-key` under
//! bash, which need jq alone (and `check-key` nothing more while there is no
//! key). Building, signing and installing from the repository need reprepro,
//! gpg and Docker on Linux: apt.yml's `repo-test` job does that with a
//! throwaway key, and its `self-test` step runs the key checks.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SCRIPT: &str = "scripts/apt-repo.sh";
const SOURCES: &str = "apt/duscope.sources";
const DISTRIBUTIONS: &str = "apt/conf/distributions";
const DOC: &str = "docs/apt.md";
const KEY: &str = "apt/duscope.gpg";

/// Where the repository is served (plan/SPEC.md, P5-PKG-01).
const APT_URL: &str = "https://kawadeomkar.github.io/duscope/apt";

/// The keyring the sources file trusts for the repository alone.
const KEYRING: &str = "/etc/apt/keyrings/duscope.gpg";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The stanzas of a deb822 or reprepro file: `Field: value` pairs, separated
/// by blank lines, comments dropped.
fn stanzas(text: &str) -> Vec<Vec<(String, String)>> {
    let mut stanzas = vec![Vec::new()];
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        if line.trim().is_empty() {
            if !stanzas.last().unwrap().is_empty() {
                stanzas.push(Vec::new());
            }
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("not a field: {line}"));
        stanzas
            .last_mut()
            .unwrap()
            .push((field.to_owned(), value.trim().to_owned()));
    }
    stanzas.retain(|stanza| !stanza.is_empty());
    stanzas
}

fn field<'a>(stanza: &'a [(String, String)], name: &str) -> Option<&'a str> {
    stanza
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// docs/apt.md's install commands, between their markers and outside the fence.
fn install_commands() -> Vec<String> {
    let doc = read(DOC);
    let block = doc
        .split("<!-- apt-install:begin -->")
        .nth(1)
        .and_then(|rest| rest.split("<!-- apt-install:end -->").next())
        .expect("docs/apt.md marks its install commands");
    block
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("```"))
        .map(str::to_owned)
        .collect()
}

/// The value of docs/apt.md's `name: value` line.
fn doc_value(name: &str) -> String {
    read(DOC)
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
        .unwrap_or_else(|| panic!("docs/apt.md has no `{name}:` line"))
        .to_owned()
}

/// The script under bash, with `stdin`, from the top of the repository.
fn script(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new("bash")
        .arg(root().join(SCRIPT))
        .args(args)
        .current_dir(root())
        .env_remove("GITHUB_ACTIONS")
        .env_remove("APT_KEYRING")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// P5-PKG-01: two suites, `stable` and `testing`, each for amd64 and arm64 in
/// one component; the script signs each Release file, and passes reprepro its
/// directories, so the configuration names neither a key nor a directory.
#[test]
fn p5_pkg_01_the_suites_are_stable_and_testing_for_amd64_and_arm64() {
    let suites = stanzas(&read(DISTRIBUTIONS));
    let codenames: Vec<&str> = suites
        .iter()
        .map(|stanza| field(stanza, "Codename").unwrap())
        .collect();
    assert_eq!(codenames, ["stable", "testing"]);
    for stanza in &suites {
        let codename = field(stanza, "Codename").unwrap();
        assert_eq!(field(stanza, "Suite"), Some(codename), "{stanza:?}");
        assert_eq!(field(stanza, "Architectures"), Some("amd64 arm64"));
        assert_eq!(field(stanza, "Components"), Some("main"));
        assert_eq!(field(stanza, "Origin"), Some("duscope"));
        assert_eq!(field(stanza, "Label"), Some("duscope"));
        assert!(
            field(stanza, "SignWith").is_none(),
            "{codename}: scripts/apt-repo.sh signs the Release files"
        );
    }
    let options = read("apt/conf/options");
    for line in options.lines().filter(|l| !l.starts_with('#')) {
        assert!(
            !["basedir", "outdir", "dbdir", "confdir"]
                .iter()
                .any(|dir| line.trim_start().starts_with(dir)),
            "apt/conf/options sets `{line}`; the script passes the directories"
        );
    }
    let script = read(SCRIPT);
    for passed in [
        "--basedir",
        "--confdir \"$root/apt/conf\"",
        "--dbdir",
        "--outdir",
    ] {
        assert!(script.contains(passed), "the script does not pass {passed}");
    }
}

/// P5-PKG-01: the deb822 sources file names the repository, the `stable`
/// suite, and a keyring trusted for this repository alone; docs/apt.md shows
/// it as it is.
#[test]
fn p5_pkg_01_the_sources_file_names_the_repository_and_its_keyring() {
    let text = read(SOURCES);
    let sources = stanzas(&text);
    assert_eq!(sources.len(), 1, "{SOURCES} holds one source");
    let fields: Vec<(&str, &str)> = sources[0]
        .iter()
        .map(|(f, v)| (f.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        fields,
        [
            ("Types", "deb"),
            ("URIs", APT_URL),
            ("Suites", "stable"),
            ("Components", "main"),
            ("Architectures", "amd64 arm64"),
            ("Signed-By", KEYRING),
        ]
    );
    assert!(
        !text.to_lowercase().contains("trusted"),
        "the repository is verified, never trusted unsigned"
    );
    let bare: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    let doc = read(DOC);
    let shown = doc
        .split("The sources file is [apt/duscope.sources](../apt/duscope.sources):")
        .nth(1)
        .and_then(|rest| rest.split("```text\n").nth(1))
        .and_then(|rest| rest.split("```").next())
        .expect("docs/apt.md shows the sources file");
    assert_eq!(shown.lines().collect::<Vec<_>>(), bare);
    assert!(
        read(SCRIPT).contains(&format!("\nAPT_URL={APT_URL}\n")),
        "the script serves the repository at {APT_URL}"
    );
}

/// P5-PKG-01: docs/apt.md's install commands, which apt.yml runs as they
/// stand: the key into its own keyring, then the sources file, then duscope;
/// no `apt-key`, and each as root through sudo.
#[test]
fn p5_pkg_01_the_install_commands_fetch_the_key_and_the_sources_then_install() {
    let commands = install_commands();
    let position = |wanted: &str| -> usize {
        commands
            .iter()
            .position(|line| line == wanted)
            .unwrap_or_else(|| panic!("the install commands lack `{wanted}`: {commands:#?}"))
    };
    let key = position(&format!("sudo curl -fsSLo {KEYRING} {APT_URL}/duscope.gpg"));
    let sources = position(&format!(
        "sudo curl -fsSLo /etc/apt/sources.list.d/duscope.sources {APT_URL}/duscope.sources"
    ));
    let install = position("sudo apt-get install -y duscope");
    let updates: Vec<usize> = commands
        .iter()
        .enumerate()
        .filter(|(_, line)| *line == "sudo apt-get update")
        .map(|(i, _)| i)
        .collect();
    assert!(
        key < sources
            && updates.iter().any(|&u| sources < u && u < install)
            && updates.first().is_some_and(|&u| u < key),
        "{commands:#?}"
    );
    for line in &commands {
        assert!(line.starts_with("sudo "), "`{line}` does not run as root");
        assert!(!line.contains("apt-key"), "`{line}`: apt-key is deprecated");
        assert!(
            !line.contains("trusted=yes"),
            "`{line}` skips the signature"
        );
    }
}

/// P5-PKG-01 and P5-SEC-02: until the key ceremony there is no public key, and
/// docs/apt.md records a marked placeholder, never a fingerprint, which
/// `check-key` (apt.yml's first step) refuses, naming the ceremony. Once the
/// key exists, docs/apt.md records a fingerprint and an expiry.
#[test]
fn p5_pkg_01_no_fingerprint_until_the_key_exists() {
    let fingerprint = doc_value("fingerprint");
    let expiry = doc_value("signing subkey expires");
    if root().join(KEY).exists() {
        let hex: String = fingerprint.split_whitespace().collect();
        assert!(
            hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "docs/apt.md's fingerprint is not 40 hexadecimal digits: {fingerprint}"
        );
        let parts: Vec<&str> = expiry.split('-').collect();
        assert!(
            parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()),
            "docs/apt.md's subkey expiry is not a date: {expiry}"
        );
        return;
    }
    assert!(
        fingerprint.starts_with("PLACEHOLDER") && expiry.starts_with("PLACEHOLDER"),
        "no {KEY}, but docs/apt.md records `{fingerprint}`, `{expiry}`"
    );
    // No fingerprint-like run of hexadecimal digits, grouped or not.
    for file in [DOC, SOURCES, DISTRIBUTIONS, "apt/conf/options"] {
        for (n, line) in read(file).lines().enumerate() {
            let joined: String = line.split_whitespace().collect();
            let longest = joined
                .split(|c: char| !c.is_ascii_hexdigit())
                .map(str::len)
                .max()
                .unwrap_or(0);
            assert!(
                longest < 32,
                "{file}:{}: a fingerprint before the key exists: {line}",
                n + 1
            );
        }
    }
    let out = script(&["check-key"], "");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check-key passed without a key");
    assert!(
        stderr.contains("apt/duscope.gpg is missing") && stderr.contains("(P5-SEC-02)"),
        "{stderr}"
    );
}

/// A release line for `select`: the tag, GitHub's pre-release flag, and
/// whether it has its two .debs.
fn release(tag: &str, prerelease: bool, debs: bool, draft: bool) -> String {
    let version = tag.trim_start_matches('v');
    let assets = if debs {
        format!(
            "[\"duscope-x86_64-unknown-linux-musl.tar.xz\", \"duscope_{version}_amd64.deb\", \
             \"duscope_{version}_arm64.deb\"]"
        )
    } else {
        "[\"duscope-x86_64-unknown-linux-musl.tar.xz\"]".to_owned()
    };
    format!(
        "{{\"tag\": \"{tag}\", \"prerelease\": {prerelease}, \"draft\": {draft}, \"assets\": {assets}}}\n"
    )
}

/// `select TAG` over these releases: its stdout, or its error.
fn select(tag: &str, releases: &[String]) -> Result<String, String> {
    let out = script(&["select", tag], &releases.concat());
    if out.status.success() {
        Ok(String::from_utf8(out.stdout).unwrap())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// P5-PKG-01: `stable` takes the newest release, `testing` the newest release
/// or pre-release, by semver order, each among the releases that have both
/// .debs and are not drafts; the run's own release must be one of those.
#[test]
fn p5_pkg_01_the_suites_take_the_newest_releases_with_both_debs() {
    let suites = |stable: &str, testing: &str| format!("stable={stable}\ntesting={testing}\n");
    let base = vec![
        release("v0.1.0", false, true, false),
        release("v0.2.0", false, true, false),
        release("v0.0.1", false, false, false),
    ];
    assert_eq!(select("v0.2.0", &base), Ok(suites("v0.2.0", "v0.2.0")));

    // A pre-release goes to testing alone; rc.10 follows rc.2, and 0.10
    // follows 0.9.
    let mut pre = base.clone();
    pre.push(release("v0.10.0-rc.2", true, true, false));
    pre.push(release("v0.10.0-rc.10", true, true, false));
    pre.push(release("v0.9.0", false, true, false));
    assert_eq!(
        select("v0.10.0-rc.10", &pre),
        Ok(suites("v0.9.0", "v0.10.0-rc.10"))
    );
    // The release itself then takes both.
    pre.push(release("v0.10.0", false, true, false));
    assert_eq!(select("v0.10.0", &pre), Ok(suites("v0.10.0", "v0.10.0")));

    // A patch of an older minor leaves both suites at the newer release.
    let mut patch = base.clone();
    patch.push(release("v0.3.0", false, true, false));
    patch.push(release("v0.2.1", false, true, false));
    assert_eq!(select("v0.2.1", &patch), Ok(suites("v0.3.0", "v0.3.0")));

    // A pulled release: marked a pre-release, its assets deleted. A dispatch
    // for the previous release republishes it in both suites.
    let mut pulled = base.clone();
    pulled.push(release("v0.3.0", true, false, false));
    assert_eq!(select("v0.2.0", &pulled), Ok(suites("v0.2.0", "v0.2.0")));
    // One marked a pre-release whose assets are still there stays out of stable.
    let mut marked = base.clone();
    marked.push(release("v0.3.0", true, true, false));
    assert_eq!(select("v0.2.0", &marked), Ok(suites("v0.2.0", "v0.3.0")));

    // A draft is not published.
    let mut draft = base.clone();
    draft.push(release("v0.3.0", false, true, true));
    assert_eq!(select("v0.2.0", &draft), Ok(suites("v0.2.0", "v0.2.0")));

    for (tag, releases, error) in [
        ("v0.3.0", &draft, "v0.3.0 is not a published release"),
        ("v0.0.1", &base, "v0.0.1 has no .deb for amd64 and arm64"),
        ("v0.3.0", &pulled, "v0.3.0 has no .deb for amd64 and arm64"),
        ("v9", &base, "v9 is not a release tag"),
    ] {
        let got = select(tag, releases).expect_err(tag);
        assert!(got.contains(error), "{tag}: {got}");
    }
    let only_pre = vec![release("v0.1.0-rc.1", true, true, false)];
    let got = select("v0.1.0-rc.1", &only_pre).unwrap_err();
    assert!(got.contains("stable would be empty"), "{got}");
}
