//! P2-MAP-01: `render` writes the treemap as SVG: golden pictures of the
//! fixture tree in both layouts, a full picture that `xmllint` reads as
//! well-formed XML with a gradient for every shaded tile, and the errors.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use duscope_core::query::Regex;
use duscope_testkit::{TestEnv, golden, normalize};

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Vec<u8> {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout.clone()
}

/// Saves a scan of the fixture tree; its path.
fn saved_home(env: &TestEnv) -> String {
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let path = home.to_str().unwrap().to_owned();
    ok(&run(
        env,
        &["scan", &path, "--save", "--json", "--threads", "4"],
    ));
    path
}

/// `svg` without its root and the saved scan's id and time, in words that
/// keep it an SVG a browser opens.
fn mask(svg: &str, root: &str) -> String {
    let footer = Regex::new(r"saved scan [0-9a-f]+ from [0-9-]+ [0-9:]+").unwrap();
    let svg = footer.replace_all(svg.as_bytes(), &b"SAVED SCAN"[..]);
    let svg = String::from_utf8(svg.into_owned()).unwrap();
    normalize(&svg, Path::new(root)).replace("<root>", "ROOT")
}

#[test]
fn p2_map_01_golden_pictures_of_the_fixture_tree() {
    let env = TestEnv::new();
    let root = saved_home(&env);
    let os = std::env::consts::OS;
    let cases: [(&str, &[&str]); 2] = [
        ("squarify", &[]),
        ("gp", &["--layout", "gp", "--color-by", "folder"]),
    ];
    for (name, extra) in cases {
        let file = env.tmp_dir().join(format!("{name}.svg"));
        let mut args = vec![
            "render",
            root.as_str(),
            "--width",
            "640",
            "--height",
            "400",
            "--depth",
            "2",
            "-o",
            file.to_str().unwrap(),
        ];
        args.extend(extra);
        ok(&run(&env, &args));
        let svg = fs::read_to_string(&file).unwrap();
        golden(&format!("render_svg_{name}@{os}.snap"), &mask(&svg, &root));
    }
}

#[test]
fn p2_map_01_a_full_picture_is_well_formed_with_its_gradients() {
    let env = TestEnv::new();
    let root = saved_home(&env);
    let file = env.tmp_dir().join("home.svg");
    let out = run(&env, &["render", &root, "-o", file.to_str().unwrap()]);
    ok(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("rendered 1200×800 svg"), "{stderr}");
    let svg = fs::read_to_string(&file).unwrap();
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    assert!(svg.contains("width=\"1200\" height=\"800\""));
    // Every gradient a tile names is defined, and each colour has both.
    let id = Regex::new(r"url\(#([vh]\d+)\)").unwrap();
    let used: BTreeSet<String> = id
        .captures_iter(svg.as_bytes())
        .map(|c| String::from_utf8_lossy(&c[1]).into_owned())
        .collect();
    let def = Regex::new(r#"<linearGradient id="([vh]\d+)""#).unwrap();
    let defined: BTreeSet<String> = def
        .captures_iter(svg.as_bytes())
        .map(|c| String::from_utf8_lossy(&c[1]).into_owned())
        .collect();
    assert!(!used.is_empty());
    assert_eq!(used, defined);
    // The largest files are labelled with their sizes; the legend and
    // the totals are under the map.
    assert!(svg.contains(">holiday-2035.mov</text>"), "no label");
    assert!(svg.contains("%</text>") && svg.contains(" files · "));
    // xmllint reads it as XML.
    match Command::new("xmllint").arg("--noout").arg(&file).output() {
        Ok(lint) => assert!(
            lint.status.success(),
            "xmllint: {}",
            String::from_utf8_lossy(&lint.stderr)
        ),
        Err(_) => duscope_testkit::skip("xmllint is not installed"),
    }
    // Into a pipe, the same picture.
    let piped = ok(&run(&env, &["render", &root]));
    assert_eq!(String::from_utf8(piped).unwrap(), svg);
}

#[test]
fn p2_map_01_render_wants_svg_and_a_pack_with_colours() {
    let env = TestEnv::new();
    let root = saved_home(&env);
    let png = env.tmp_dir().join("map.png");
    // PNG is the next package's.
    let out = run(
        &env,
        &["render", &root, "-o", png.to_str().unwrap(), "--json"],
    );
    assert_eq!(out.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "not_available");
    assert!(!png.exists());
    for args in [
        vec!["render", &root, "--format", "csv"],
        vec!["render", &root, "--theme", "no-color"],
        vec!["render", &root, "--theme", "terminal-native"],
        vec!["render", &root, "--width", "8"],
        vec!["top", &root, "--format", "svg"],
    ] {
        let out = run(&env, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    // A colourless DUSCOPE_THEME, set for the terminal, leaves a picture
    // in Mocha.
    let out = env
        .cmd()
        .env("DUSCOPE_THEME", "no-color")
        .args(["render", &root, "--depth", "1"])
        .output()
        .unwrap();
    let svg = String::from_utf8(ok(&out)).unwrap();
    assert!(svg.contains("fill=\"#1e1e2e\""), "Mocha's base");
}
