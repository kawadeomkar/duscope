//! P4-TUI-07: `--treemap pixels` off a terminal. A build without the
//! `pixels` feature refuses it with exit 2 and `not_compiled`, before any
//! scan; a build with it draws the cell map where stdin and stdout are not
//! both a terminal, which the probe needs, and says so. The probe against
//! mocked terminals is in `tests/pty/pixels.rs`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use duscope_testkit::TestEnv;

#[cfg(not(feature = "pixels"))]
#[test]
fn p4_tui_07_the_default_build_answers_not_compiled() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    // On a path that does not exist the refusal still wins: nothing was
    // scanned, and nothing asked of a terminal.
    let missing = env.tmp_dir().join("does-not-exist");
    for (args, path) in [
        (&["scan", "--treemap", "pixels"][..], &tree),
        (&["--treemap", "pixels"], &tree),
        (&["scan", "--treemap", "pixels"], &missing),
    ] {
        let out = env.cmd().args(args).arg(path).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            stderr.contains(
                "duscope: --treemap pixels needs the pixels feature, which this build does not have"
            ),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains("hint: Install with cargo install duscope --features pixels"),
            "{args:?}: {stderr}"
        );
    }
}

#[cfg(feature = "pixels")]
#[test]
fn p4_tui_07_off_a_terminal_the_map_is_drawn_in_cells() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("extensions");
    let out = env
        .cmd()
        .args(["scan", "--treemap", "pixels", "--threads", "4"])
        .arg(&tree)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // The cell map at 80×24, as `--treemap always` draws it off a
    // terminal, with no image escape in it.
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 23, "{text}");
    assert!(text.contains("next: duscope top"), "{text}");
    for escape in ["\x1b_G", "\x1bP", "\x1b]1337;"] {
        assert!(!text.contains(escape), "{escape:?} in {text}");
    }
    assert!(
        stderr.contains(
            "duscope: warn: --treemap pixels: stdin and stdout are not both a terminal, so the map is drawn in cells"
        ),
        "{stderr}"
    );
}
