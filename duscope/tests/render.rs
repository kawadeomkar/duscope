//! P2-MAP-01 and P2-MAP-02: `render` writes the treemap as SVG and PNG:
//! golden pictures of the fixture tree in both layouts (the PNGs by a
//! perceptual hash), a full SVG that `xmllint` reads as well-formed XML
//! with a gradient for every shaded tile, a PNG every chunk of which
//! checks, and the errors.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output};

use duscope_core::query::Regex;
use duscope_testkit::{TestEnv, golden, golden_dir, normalize};

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
    // The largest files are labelled with their sizes, a name cut to its
    // tile (whose size differs between the platforms' layouts); the legend
    // and the totals are under the map.
    assert!(svg.contains(">holiday-"), "no label");
    assert!(svg.contains(".0M</text>"), "no size");
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
    // A picture is the output: --json, with a PNG as with an SVG, is
    // refused before anything is written.
    let out = run(
        &env,
        &["render", &root, "-o", png.to_str().unwrap(), "--json"],
    );
    assert_eq!(out.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "usage");
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
    // The configuration file's theme, beneath DUSCOPE_THEME.
    let dir = env.xdg_config_home().join("duscope");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), "[defaults]\ntheme = \"dracula\"\n").unwrap();
    let svg = String::from_utf8(ok(&run(&env, &["render", &root, "--depth", "1"]))).unwrap();
    assert!(svg.contains("fill=\"#282a36\""), "Dracula's base");
    let out = env
        .cmd()
        .env("DUSCOPE_THEME", "gruvbox")
        .args(["render", &root, "--depth", "1"])
        .output()
        .unwrap();
    let svg = String::from_utf8(ok(&out)).unwrap();
    assert!(!svg.contains("fill=\"#282a36\""), "the variable wins");
}

/// A tree of exact sizes, through `import`: an empty directory that weighs
/// its own blocks, a directory with a file, and two files of no bytes.
const TREE: &str = r#"[1,2,{"progname":"ncdu","timestamp":2114380800},
[{"name":"/r","asize":0,"dsize":0},
{"name":"z1","asize":0,"dsize":0},
{"name":"z2","asize":0,"dsize":0},
[{"name":"empty","asize":400000,"dsize":400000}],
[{"name":"big","asize":0,"dsize":0},
{"name":"a.bin","asize":500000,"dsize":500000}]]]
"#;

#[test]
fn p2_map_01_every_tile_is_painted_and_labels_keep_to_their_tiles() {
    let env = TestEnv::new();
    fs::create_dir_all(env.tmp_dir()).unwrap();
    let file = env.tmp_dir().join("tree.json");
    fs::write(&file, TREE).unwrap();
    let imported: serde_json::Value = serde_json::from_slice(&ok(&run(
        &env,
        &["import", file.to_str().unwrap(), "--json"],
    )))
    .unwrap();
    let id = imported["scan"]["id"].as_str().unwrap().to_owned();
    for layout in ["squarify", "gp"] {
        let out = run(
            &env,
            &[
                "render", "/r", "--scan", &id, "--layout", layout, "--width", "640", "--height",
                "400",
            ],
        );
        let svg = String::from_utf8(ok(&out)).unwrap();
        // The empty directory is a leaf of its own, and the files of no
        // bytes are pooled, in both layouts: three tiles.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("svg, 3 tiles"), "{layout}: {stderr}");
        assert!(svg.contains(">empty/</text>"), "{layout}");
        assert!(svg.contains(">a.bin</text>"), "{layout}");
        // Each label is clipped to its tile.
        let labels = svg.matches("<g clip-path=").count();
        assert_eq!(labels, svg.matches("<clipPath id=").count());
        assert!(labels >= 2, "{layout}");
    }
    // A picture is the output: --json and --plain describe none.
    for flag in ["--json", "--plain"] {
        let out = run(&env, &["render", "/r", "--scan", &id, flag]);
        assert_eq!(out.status.code(), Some(2), "{flag}");
    }
}

/// The width, height and RGB pixels of a PNG `render` wrote (8-bit RGB,
/// each row Sub-filtered), every chunk's CRC checked.
fn decode(png: &[u8]) -> (usize, usize, Vec<u8>) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let be32 = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    let (mut width, mut height, mut idat) = (0, 0, Vec::new());
    let mut at = 8;
    loop {
        let length = be32(at) as usize;
        let kind = &png[at + 4..at + 8];
        let data = &png[at + 8..at + 8 + length];
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        assert_eq!(crc.sum(), be32(at + 8 + length), "{kind:?}");
        match kind {
            b"IHDR" => {
                width = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
                height = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
                assert_eq!(&data[8..13], &[8, 2, 0, 0, 0]);
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + length;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..])
        .read_to_end(&mut raw)
        .unwrap();
    let stride = 3 * width;
    assert_eq!(raw.len(), height * (stride + 1));
    let mut pixels = Vec::with_capacity(height * stride);
    for row in raw.chunks_exact(stride + 1) {
        assert_eq!(row[0], 1, "Sub filter");
        let start = pixels.len();
        for (i, &byte) in row[1..].iter().enumerate() {
            let left = if i >= 3 { pixels[start + i - 3] } else { 0 };
            pixels.push(byte.wrapping_add(left));
        }
    }
    (width, height, pixels)
}

/// A 256-bit difference hash: the picture in grey, shrunk to 17 × 16 cells
/// by averaging, each bit whether a cell is lighter than the one on its
/// right; as hex.
fn dhash(width: usize, height: usize, pixels: &[u8]) -> String {
    let mut cells = [[0.0f64; 17]; 16];
    for (y, row) in pixels.chunks_exact(3 * width).enumerate() {
        for (x, rgb) in row.chunks_exact(3).enumerate() {
            let grey =
                0.299 * f64::from(rgb[0]) + 0.587 * f64::from(rgb[1]) + 0.114 * f64::from(rgb[2]);
            cells[y * 16 / height][x * 17 / width] += grey;
        }
    }
    let mut hex = String::new();
    for row in cells {
        let mut bits = 0u32;
        for x in 0..16 {
            bits = (bits << 1) | u32::from(row[x] > row[x + 1]);
        }
        let _ = write!(hex, "{bits:04x}");
    }
    hex
}

/// Compares `hash` with the golden `name`, within 16 bits of 256.
fn hash_golden(name: &str, hash: &str) {
    let path = golden_dir().join(name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, format!("{hash}\n")).unwrap();
        return;
    }
    let want = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    let differ: u32 = u128::from_str_radix(&want.trim()[..32], 16)
        .map(|a| (a ^ u128::from_str_radix(&hash[..32], 16).unwrap()).count_ones())
        .unwrap()
        + u128::from_str_radix(&want.trim()[32..], 16)
            .map(|a| (a ^ u128::from_str_radix(&hash[32..], 16).unwrap()).count_ones())
            .unwrap();
    assert!(differ <= 16, "{name}: {differ} bits differ from {want}");
}

#[test]
fn p2_map_02_golden_pngs_by_their_perceptual_hash() {
    let env = TestEnv::new();
    let root = saved_home(&env);
    let os = std::env::consts::OS;
    let cases: [(&str, &[&str]); 2] = [
        ("squarify", &[]),
        ("gp", &["--layout", "gp", "--color-by", "folder"]),
    ];
    for (name, extra) in cases {
        let file = env.tmp_dir().join(format!("{name}.png"));
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
        let out = run(&env, &args);
        ok(&out);
        assert!(String::from_utf8_lossy(&out.stderr).contains("rendered 640×400 png"));
        let (width, height, pixels) = decode(&fs::read(&file).unwrap());
        assert_eq!((width, height), (640, 400));
        hash_golden(
            &format!("render_png_{name}@{os}.snap"),
            &dhash(width, height, &pixels),
        );
    }
    // Into a pipe with --format png; a picture over 64 Mi pixels refused.
    let piped = ok(&run(
        &env,
        &["render", &root, "--format", "png", "--depth", "1"],
    ));
    assert_eq!(&piped[..4], b"\x89PNG");
    let out = run(
        &env,
        &[
            "render", &root, "--format", "png", "--width", "16384", "--height", "16384",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
}
