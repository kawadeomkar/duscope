//! P1-PKG-01: the `.deb` packaging. debian/changelog.sh turns CHANGELOG.md's
//! released sections into debian/changelog, and the cargo-deb metadata
//! installs exactly what `just deb` writes. Building the package, lintian and
//! the install tests in Debian 12 and Ubuntu 22.04 run in release.yml.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use duscope_testkit::TestEnv;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

/// The Python 3 the script runs: /usr/bin/python3 where it exists, as the
/// bench harness prefers, else python3 on the PATH.
fn python() -> &'static str {
    if Path::new("/usr/bin/python3").exists() {
        "/usr/bin/python3"
    } else {
        "python3"
    }
}

#[test]
fn p1_pkg_01_the_debian_changelog_comes_from_the_released_sections() {
    let env = TestEnv::new();
    let changelog = env.tmp_dir().join("CHANGELOG.md");
    // A URL longer than a line stays whole.
    let url = format!("https://example.com/{}end", "a-very-long-path/".repeat(6));
    fs::write(
        &changelog,
        format!(
            "# Changelog\n\nIntro.\n\n## [Unreleased]\n\n### Added\n\n- Not yet.\n\n\
         ## [0.3.0-rc.1] - 2026-12-15\n\n### Fixed\n\n- See {url} for the details.\n\n\
         ## [0.2.0] - 2026-12-01\n\nNotes before the sections stay out.\n\n- A note.\n\n\
         ### Added\n\n- `top --files` lists files, wrapped\n  over two lines.\n\n\
         ### Fixed\n\n- A crash.\n\n## [Unlisted]\n\n### Added\n\n- Not packaged.\n\n\
         ## [0.1.0] - 2026-11-01 [YANKED]\n\n### Added\n\n- The scanner.\n"
        ),
    )
    .unwrap();
    let out = env.tmp_dir().join("changelog");
    let ran = Command::new(root().join("debian/changelog.sh"))
        .arg(&changelog)
        .arg(&out)
        .env("PYTHON", python())
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let text = fs::read_to_string(&out).unwrap();
    let maintainer = "Omkar Kawade <kawadeomkar@users.noreply.github.com>";
    // A pre-release's version sorts before the release's in dpkg: 0.3.0~rc.1.
    assert_eq!(
        text,
        format!(
            "duscope (0.3.0~rc.1-1) unstable; urgency=medium\n\n  \
             * Fixed: See\n    {url}\n    for the details.\n\n \
             -- {maintainer}  Tue, 15 Dec 2026 00:00:00 +0000\n\n\
             duscope (0.2.0-1) unstable; urgency=medium\n\n  \
             * Added: top --files lists files, wrapped over two lines.\n  \
             * Fixed: A crash.\n\n -- {maintainer}  Tue, 01 Dec 2026 00:00:00 +0000\n\n\
             duscope (0.1.0-1) unstable; urgency=medium\n\n  \
             * Added: The scanner.\n\n -- {maintainer}  Sun, 01 Nov 2026 00:00:00 +0000\n\n"
        )
    );
    // The repository's own changelog has a released section to package.
    let ran = Command::new(root().join("debian/changelog.sh"))
        .arg(root().join("CHANGELOG.md"))
        .arg(env.tmp_dir().join("ours"))
        .env("PYTHON", python())
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}

#[test]
fn p1_pkg_01_the_package_installs_what_just_deb_writes() {
    let manifest = fs::read_to_string(root().join("duscope/Cargo.toml")).unwrap();
    let justfile = fs::read_to_string(root().join("justfile")).unwrap();
    // Every generated asset the metadata installs is one the recipe writes.
    for asset in ["duscope.1", "duscope.bash", "_duscope", "duscope.fish"] {
        assert!(
            manifest.contains(&format!("\"../target/deb-assets/{asset}\"")),
            "the metadata does not install {asset}"
        );
        assert!(
            justfile.contains(&format!("\"$assets/{asset}\"")),
            "just deb does not write {asset}"
        );
    }
    // The package's version is the one debian/changelog.sh writes: a
    // pre-release's `-` becomes `~`, which dpkg sorts before the release.
    assert!(justfile.contains("| head -n 1 | sed 's/-/~/')"));
    assert!(justfile.contains("--deb-version \"$version-1\""));
    // The copyright is machine-readable, the overrides are named for the
    // package, and the generated changelog stays out of git.
    let copyright = fs::read_to_string(root().join("debian/copyright")).unwrap();
    assert!(copyright.starts_with(
        "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
    ));
    let overrides = fs::read_to_string(root().join("debian/duscope.lintian-overrides")).unwrap();
    assert!(
        overrides
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .all(|l| l.starts_with("duscope: "))
    );
    let ignored = fs::read_to_string(root().join("debian/.gitignore")).unwrap();
    assert!(ignored.lines().any(|l| l == "/changelog"));
}

#[test]
fn p1_pkg_01_the_copyright_names_every_vendored_file() {
    let copyright = fs::read_to_string(root().join("debian/copyright")).unwrap();
    let stanzas: Vec<&str> = copyright
        .lines()
        .filter_map(|line| line.strip_prefix("Files: "))
        .collect();
    // The palette tables are compiled into the binary as the scheduler is.
    let mut vendored = vec!["crates/duscope-treemap/src/packs.rs".to_owned()];
    // Every row of VENDOR.md's table: its first cell is the path in backticks.
    let vendor = fs::read_to_string(root().join("VENDOR.md")).unwrap();
    vendored.extend(vendor.lines().filter_map(|line| {
        let cell = line.strip_prefix("| `")?;
        Some(cell.split('`').next()?.to_owned())
    }));
    assert!(vendored.len() > 1, "VENDOR.md lists no vendored file");
    for path in &vendored {
        assert!(
            stanzas.contains(&path.as_str()),
            "debian/copyright has no `Files: {path}` stanza"
        );
    }
    // Each licence a stanza names has its text in a standalone paragraph.
    for license in copyright
        .lines()
        .filter_map(|line| line.strip_prefix("License: "))
        .flat_map(|names| names.split([' ']))
        .filter(|name| !["or", "and"].contains(name))
    {
        assert!(
            copyright.contains(&format!("\n\nLicense: {license}\n ")),
            "debian/copyright has no text for {license}"
        );
    }
}
