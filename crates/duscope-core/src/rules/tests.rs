//! P1-CORE-04: the vendored tables against the fixture tree, the loaders'
//! errors, the compiled cache rules and the examples in `docs/rules.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{DirId, Kind, ScanEvent, WalkOptions, walk};

use super::{Caches, Class, Classes, Confirm, Regenerable, ext};
use crate::CoreError;

/// The fixture tree: every directory as its path's components below the
/// root, and the names of the files in each directory.
struct Tree {
    dirs: Vec<Vec<Vec<u8>>>,
    files: BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>,
    root: PathBuf,
}

fn fixture_tree(env: &TestEnv) -> Tree {
    let root = env.fixtures().root();
    let mut paths: BTreeMap<DirId, Vec<Vec<u8>>> = BTreeMap::new();
    let mut dirs = Vec::new();
    let mut files: BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>> = BTreeMap::new();
    walk(&root, &WalkOptions::default(), |event| {
        if let ScanEvent::Entry(entry) = event {
            if entry.dir == Some(DirId::ROOT) {
                paths.insert(DirId::ROOT, Vec::new());
                return;
            }
            let mut path = paths[&entry.parent].clone();
            path.push(entry.name.as_bytes().to_vec());
            match entry.dir {
                Some(id) => {
                    paths.insert(id, path.clone());
                    dirs.push(path);
                }
                None if entry.kind != Kind::Dir => {
                    files
                        .entry(paths[&entry.parent].clone())
                        .or_default()
                        .insert(entry.name.as_bytes().to_vec());
                }
                None => {}
            }
        }
    })
    .unwrap();
    Tree { dirs, files, root }
}

impl Tree {
    /// Whether a directory strictly above `dir` holds a file `name`.
    fn has_marker(&self, dir: &[Vec<u8>], name: &str) -> bool {
        (0..dir.len()).any(|len| {
            self.files
                .get(&dir[..len])
                .is_some_and(|names| names.contains(name.as_bytes()))
        })
    }

    /// Whether `dir` holds a `CACHEDIR.TAG` that starts with the signature.
    fn cachedir_tag(&self, dir: &[Vec<u8>]) -> bool {
        let mut path = self.root.clone();
        for part in dir {
            path.push(std::str::from_utf8(part).unwrap_or("?"));
        }
        fs::read(path.join("CACHEDIR.TAG"))
            .is_ok_and(|bytes| bytes.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55"))
    }
}

/// A `/`-separated path as components.
fn path(p: &str) -> Vec<&[u8]> {
    p.split('/').map(str::as_bytes).collect()
}

fn parts(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

#[test]
fn p1_core_04_the_fixture_tree_exercises_every_rule() {
    let env = TestEnv::new();
    let tree = fixture_tree(&env);
    let caches = Caches::vendored();
    for rule in caches.rules() {
        let hit = tree.dirs.iter().any(|dir| {
            rule.matches(
                &parts(dir),
                |m| tree.has_marker(dir, m),
                tree.cachedir_tag(dir),
            )
        });
        assert!(
            hit,
            "no fixture directory exercises the cache rule {:?} {:?}",
            rule.name, rule.confirm
        );
    }
    let classes = Classes::vendored();
    for key in classes.dir_rules() {
        let single = Classes::parse(&format!("[dir]\n{key:?} = \"data\"\n"), "test").unwrap();
        let hit = tree
            .dirs
            .iter()
            .any(|dir| single.of_dir(&parts(dir)).is_some());
        assert!(
            hit,
            "no fixture directory exercises the directory rule {key:?}"
        );
    }
    // A name without its marker is not a cache: go-mod/vendor has no
    // composer.json.
    let go_vendor: Vec<Vec<u8>> = ["home", "Projects", "go-mod", "vendor"]
        .map(|s| s.as_bytes().to_vec())
        .to_vec();
    assert!(tree.dirs.contains(&go_vendor));
    assert!(
        caches
            .matching(
                &parts(&go_vendor),
                |m| tree.has_marker(&go_vendor, m),
                false
            )
            .is_none()
    );
}

#[test]
fn p1_core_04_the_vendored_tables_load() {
    let classes = Classes::vendored();
    assert_eq!(classes.of_ext("tar.gz"), Some(Class::Archives));
    assert_eq!(classes.of_ext("rs"), Some(Class::Code));
    assert_eq!(classes.of_ext("nope"), None);
    assert_eq!(classes.of_dir(&path("x/node_modules")), Some(Class::Code));
    assert_eq!(
        classes.of_dir(&path("repo/.git/objects")),
        Some(Class::Data)
    );
    assert_eq!(
        classes.of_dir(&path("A/Sparkle.framework")),
        Some(Class::Binaries)
    );
    assert_eq!(
        classes.of_dir(&path("Example.app/Contents/MacOS")),
        Some(Class::Binaries)
    );
    assert_eq!(
        classes.of_dir(&path("Example.app/Contents/Resources")),
        None
    );
    // Every extension key is one the ext rule produces, and every class is
    // used.
    let used: BTreeSet<Class> = classes.ext_rules().map(|(_, class)| class).collect();
    assert!(
        Class::ALL
            .iter()
            .filter(|c| **c != Class::Other)
            .all(|c| used.contains(c))
    );

    let caches = Caches::vendored();
    assert_eq!(caches.rules().len(), 20);
    let tag = caches
        .rules()
        .iter()
        .filter(|r| r.confirm == Confirm::CachedirTag)
        .count();
    assert_eq!(tag, 1);
    assert!(
        caches
            .rules()
            .iter()
            .all(|r| r.regenerable_by != Regenerable::Unknown || r.confirm == Confirm::CachedirTag)
    );
}

#[test]
fn p1_core_04_the_ext_rule() {
    assert_eq!(ext(b"photo-dump.tar.gz"), "tar.gz");
    assert_eq!(ext(b"BACKUP.TAR.ZST"), "tar.zst");
    assert_eq!(ext(b"report.PDF"), "pdf");
    assert_eq!(ext(b"archive.tgz"), "tgz");
    assert_eq!(ext(b".bashrc"), "");
    assert_eq!(ext(b"Makefile"), "");
    assert_eq!(ext(b"trailing."), "");
    assert_eq!(
        ext(b".tar.gz"),
        "gz",
        "a dotfile has no stem to keep tar.gz whole on"
    );
    assert_eq!(ext(b"x.abcdefghijklmnop"), "abcdefghijklmnop");
    assert_eq!(ext(b"x.abcdefghijklmnopq"), "", "more than 16 bytes");
    assert_eq!(ext(b"x.c++"), "c++");
    assert_eq!(ext(b"x.a_b"), "", "outside [A-Za-z0-9+~-]");
    assert_eq!(ext(b"caf\xc3\xa9.\xc3\xa9"), "", "non-ASCII");
}

fn rules_error(result: Result<impl std::fmt::Debug, CoreError>) -> (usize, String) {
    match result {
        Err(CoreError::Rules { line, message, .. }) => (line, message),
        other => panic!("not a rules error: {other:?}"),
    }
}

#[test]
fn p1_core_04_a_malformed_user_table_fails_with_a_line_number() {
    let (line, _) = rules_error(Classes::parse(
        "[ext]\nrs = \"code\"\nbad line\n",
        "user.toml",
    ));
    assert_eq!(line, 3);
    let (line, message) = rules_error(Classes::parse(
        "[ext]\nrs = \"code\"\npy = \"snakes\"\n",
        "user.toml",
    ));
    assert_eq!(line, 3, "{message}");
    let (line, message) = rules_error(Classes::parse(
        "[ext]\n\"tar.foo.bar\" = \"code\"\n",
        "user.toml",
    ));
    assert_eq!(line, 2, "{message}");
    let (line, message) = rules_error(Classes::parse(
        "[dir]\n\n\"a//b\" = \"data\"\n",
        "user.toml",
    ));
    assert_eq!(line, 3, "{message}");
    let (line, message) = rules_error(Classes::parse("[colors]\nx = 1\n", "user.toml"));
    assert_eq!(line, 1, "{message}");

    let both = "[[rule]]\nname = \"a\"\nregenerable_by = \"npm install\"\nmarkers = [\"p\"]\n\n[[rule]]\nname = \"b\"\nmarkers = [\"q\"]\nunder = \"x/y\"\nregenerable_by = \"npm install\"\n";
    let (line, message) = rules_error(Caches::parse(both, "user.toml"));
    assert_eq!(line, 6, "{message}");
    let free_text = "[[rule]]\nname = \"a\"\nmarkers = [\"p\"]\nregenerable_by = \"make\"\n";
    let (line, _) = rules_error(Caches::parse(free_text, "user.toml"));
    assert_eq!(line, 4, "regenerable_by is from the fixed set");
    let error = Caches::parse(free_text, "user.toml")
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("user.toml:4: "), "{error}");
}

#[test]
fn p1_core_04_user_tables_merge_over_the_vendored_ones() {
    let user = Classes::parse(
        "[ext]\nrs = \"docs\"\n\n[dir]\nnode_modules = \"data\"\n",
        "user",
    )
    .unwrap();
    let classes = Classes::vendored().merged(user);
    assert_eq!(classes.of_ext("rs"), Some(Class::Docs));
    assert_eq!(classes.of_ext("py"), Some(Class::Code));
    assert_eq!(
        classes.of_dir(&[b"node_modules".as_slice()]),
        Some(Class::Data)
    );

    let user = Caches::parse("[[rule]]\nname = \"node_modules\"\nmarkers = [\"deno.json\"]\nregenerable_by = \"unknown\"\n", "user").unwrap();
    let caches = Caches::vendored().merged(user);
    let path = [b"app".as_slice(), b"node_modules".as_slice()];
    let rule = caches
        .matching(&path, |m| m == "deno.json" || m == "package.json", false)
        .unwrap();
    assert_eq!(
        rule.regenerable_by,
        Regenerable::Unknown,
        "the user's rule is tried first"
    );
}

#[test]
fn p1_core_04_compiled_cache_rules_match_the_walker_contract() {
    let caches = Caches::vendored();
    let compiled = caches.compile();
    // The rules confirmed by markers compile: the walker checks a name and
    // the parent listing, so an ancestor-path rule has nothing to check
    // (and its name alone would exclude every directory so named), and the
    // CACHEDIR.TAG rule is the scanner's own.
    let with_markers = || {
        caches
            .rules()
            .iter()
            .filter(|r| matches!(r.confirm, Confirm::Markers(_)))
    };
    assert_eq!(compiled.len(), with_markers().count());
    assert!(compiled.iter().all(|r| !r.markers.is_empty()));
    assert!(!compiled.iter().any(|r| r.name.glob() == "DerivedData"));
    for (rule, walk_rule) in with_markers().zip(&compiled) {
        assert_eq!(Some(walk_rule.name.glob()), rule.name.as_deref());
        let markers: Vec<String> = walk_rule
            .markers
            .iter()
            .map(|m| m.to_string_lossy().into_owned())
            .collect();
        let Confirm::Markers(expected) = &rule.confirm else {
            unreachable!()
        };
        assert_eq!(&markers, expected);
        assert!(
            walk_rule
                .name
                .compile_matcher()
                .is_match(rule.name.as_deref().unwrap())
        );
    }
}

#[test]
fn p1_core_04_the_examples_in_docs_rules_md_parse() {
    let doc = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/rules.md"))
        .unwrap();
    let mut counts = BTreeMap::new();
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        let Some(kind) = line.strip_prefix("```toml ") else {
            continue;
        };
        let block: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        let text = block.join("\n");
        match kind.trim() {
            "classes" => {
                Classes::parse(&text, "docs/rules.md").unwrap();
            }
            "caches" => {
                Caches::parse(&text, "docs/rules.md").unwrap();
            }
            "theme" => check_theme(&text),
            other => panic!("unknown example kind {other:?}"),
        }
        *counts.entry(kind.trim().to_owned()).or_insert(0) += 1;
    }
    assert_eq!(
        counts.keys().map(String::as_str).collect::<Vec<_>>(),
        ["caches", "classes", "theme"]
    );
}

/// The user theme file's shape: thirteen roles, seven classes, a ten-colour
/// cycle and a five-step age ramp, every colour `#rrggbb`. P1-MAP-02 reads
/// it; until then this test holds the documented example to the schema.
fn check_theme(text: &str) {
    let table: toml::Table = toml::from_str(text).unwrap();
    let hex = |v: &toml::Value| {
        let s = v.as_str().unwrap();
        assert!(
            s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit()),
            "{s}"
        );
    };
    let roles = table["roles"].as_table().unwrap();
    let names = [
        "base", "mantle", "crust", "surface1", "surface2", "overlay1", "subtext", "text", "accent",
        "title", "link", "red", "green",
    ];
    assert_eq!(
        roles.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        names.into_iter().collect()
    );
    roles.values().for_each(hex);
    let classes = table["classes"].as_table().unwrap();
    assert_eq!(classes.len(), 7);
    for class in Class::ALL {
        hex(&classes[class.as_str()]);
    }
    let cycle = table["cycle"].as_array().unwrap();
    assert_eq!(cycle.len(), 10);
    cycle.iter().for_each(hex);
    let age = table["age"].as_array().unwrap();
    assert_eq!(age.len(), 5);
    age.iter().for_each(hex);
}

#[test]
fn p1_core_04_errors_point_at_the_line_that_defines_the_key() {
    // The same key under another table, before the bad one.
    let text = "[dir]\nPNG = \"media\"\n\n[ext]\nPNG = \"media\"\n";
    let (line, message) = rules_error(Classes::parse(text, "user.toml"));
    assert_eq!(line, 5, "{message}");
    // A single-quoted key.
    let text = "[ext]\nrs = \"code\"\n'tar.foo.bar' = \"code\"\n";
    let (line, message) = rules_error(Classes::parse(text, "user.toml"));
    assert_eq!(line, 3, "{message}");
    // A table header with a comment.
    let text = "[ext] # mine\n\n\"tar.foo.bar\" = \"code\"\n";
    let (line, message) = rules_error(Classes::parse(text, "user.toml"));
    assert_eq!(line, 3, "{message}");
}

#[test]
fn p1_core_04_an_empty_extension_key_is_an_error() {
    // Files without an extension are `other` (or `binaries` when
    // executable); no rule can reclassify them all.
    let (line, message) = rules_error(Classes::parse("[ext]\n\"\" = \"code\"\n", "user.toml"));
    assert_eq!(line, 2, "{message}");
}

#[test]
fn p1_core_04_the_most_specific_directory_rule_wins() {
    let user = Classes::parse(
        "[dir]\n\"foo*\" = \"data\"\nfoobar = \"code\"\n\"*/foo*\" = \"media\"\n\"a/foo*\" = \"docs\"\n",
        "user",
    )
    .unwrap();
    assert_eq!(
        user.dir_rules().collect::<Vec<_>>(),
        ["a/foo*", "*/foo*", "foobar", "foo*"]
    );
    assert_eq!(user.of_dir(&[b"foobar".as_slice()]), Some(Class::Code));
    assert_eq!(user.of_dir(&[b"foobaz".as_slice()]), Some(Class::Data));
    assert_eq!(
        user.of_dir(&[b"a".as_slice(), b"foobar".as_slice()]),
        Some(Class::Docs)
    );
    assert_eq!(
        user.of_dir(&[b"b".as_slice(), b"foobar".as_slice()]),
        Some(Class::Media)
    );
}
