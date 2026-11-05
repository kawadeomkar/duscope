//! P5-WALK-03: `--skip-containers` lists a home's `Library/Containers` and
//! `Library/Group Containers` without entering them, known by identity; and
//! the Full Disk Access probe's mocked refusal.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    CONTAINERS, DirId, MemProvider, ScanEvent, Skip, WalkOptions, WalkSummary, walk, walk_with,
};

/// Entries delivered and directories skipped, by path relative to the root.
fn walked(
    run: impl FnOnce(&mut dyn FnMut(ScanEvent)) -> WalkSummary,
) -> (Vec<String>, BTreeMap<String, Skip>, WalkSummary) {
    let mut dirs: HashMap<DirId, PathBuf> = HashMap::new();
    let mut entries = Vec::new();
    let mut skipped = BTreeMap::new();
    let summary = run(&mut |event| match event {
        ScanEvent::Entry(entry) => {
            let path = if entry.dir == Some(DirId::ROOT) {
                PathBuf::new()
            } else {
                dirs[&entry.parent].join(entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            entries.push(path.to_string_lossy().into_owned());
        }
        ScanEvent::Skipped { dir, reason } => {
            skipped.insert(dirs[&dir].to_string_lossy().into_owned(), reason);
        }
        _ => {}
    });
    entries.sort();
    (entries, skipped, summary)
}

/// A home with both containers, a look-alike outside it, and a scan root
/// above it.
fn tree() -> MemProvider {
    let mut tree = MemProvider::new("/m");
    tree.file("home/Library/Containers/com.app/Data/db", 8192)
        .file("home/Library/Group Containers/group.app/x", 4096)
        .file("home/Library/Caches/c", 4096)
        .file("home/Documents/d", 4096)
        .file("other/Library/Containers/y", 4096);
    tree
}

fn skipping(home: &str) -> WalkOptions {
    WalkOptions {
        skip_containers: Some(PathBuf::from(home)),
        ..WalkOptions::default()
    }
}

#[test]
fn p5_walk_03_skip_containers_lists_both_without_entering() {
    let tree = tree();
    let root = Path::new("/m");
    let (entries, skipped, summary) =
        walked(|sink| walk_with(&tree, root, &skipping("/m/home"), sink).unwrap());
    assert_eq!(
        skipped,
        BTreeMap::from([
            ("home/Library/Containers".to_owned(), Skip::Containers),
            ("home/Library/Group Containers".to_owned(), Skip::Containers),
        ])
    );
    assert_eq!(Skip::Containers.as_str(), "skipped_containers");
    // Listed, not entered; and only the home's: the same names elsewhere
    // are entered.
    assert!(entries.contains(&"home/Library/Containers".to_owned()));
    assert!(!entries.iter().any(|e| e.contains("com.app")));
    assert!(!entries.iter().any(|e| e.contains("group.app")));
    assert!(entries.contains(&"other/Library/Containers/y".to_owned()));
    assert!(entries.contains(&"home/Library/Caches/c".to_owned()));
    // Counted with what the excludes leave out.
    assert_eq!((summary.excluded, summary.skipped_dirs), (2, 2));
    assert_eq!(
        CONTAINERS,
        ["Library/Containers", "Library/Group Containers"]
    );
}

#[test]
fn p5_walk_03_without_the_flag_or_the_folders_nothing_is_skipped() {
    let tree = tree();
    let root = Path::new("/m");
    let (all, skipped, summary) =
        walked(|sink| walk_with(&tree, root, &WalkOptions::default(), sink).unwrap());
    assert!(skipped.is_empty());
    assert_eq!(summary.excluded, 0);
    assert!(all.iter().any(|e| e.contains("com.app")));
    // A home without containers (a Linux one): the walk is the plain one.
    let (entries, skipped, _) =
        walked(|sink| walk_with(&tree, root, &skipping("/m/other/Documents"), sink).unwrap());
    assert!(skipped.is_empty());
    assert_eq!(entries, all);
    // A scan rooted inside a container reads it: only the walk's
    // subdirectories are skipped, never its root.
    let inside = Path::new("/m/home/Library/Containers");
    let (entries, skipped, _) =
        walked(|sink| walk_with(&tree, inside, &skipping("/m/home"), sink).unwrap());
    assert!(skipped.is_empty());
    assert!(entries.iter().any(|e| e.ends_with("db")));
}

#[test]
fn p5_walk_03_containers_are_known_by_identity_on_disk() {
    // On this platform's provider: a home whose Library is a link to the
    // scanned tree's own skips the tree's containers, which a comparison of
    // paths would miss.
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    for dir in [
        "Library/Containers/com.app",
        "Library/Group Containers/g",
        "Library/Keep",
    ] {
        fs::create_dir_all(tree.join(dir)).unwrap();
        fs::write(tree.join(dir).join("f"), [0_u8; 100]).unwrap();
    }
    let home = env.tmp_dir().join("home");
    fs::create_dir_all(&home).unwrap();
    symlink(tree.join("Library"), home.join("Library")).unwrap();
    let opts = WalkOptions {
        threads: 2,
        skip_containers: Some(home),
        ..WalkOptions::default()
    };
    let (entries, skipped, summary) = walked(|sink| walk(&tree, &opts, sink).unwrap());
    assert_eq!(
        skipped.keys().collect::<Vec<_>>(),
        ["Library/Containers", "Library/Group Containers"]
    );
    assert!(skipped.values().all(|&reason| reason == Skip::Containers));
    assert!(entries.contains(&"Library/Keep/f".to_owned()));
    assert!(!entries.iter().any(|e| e.contains("com.app")));
    assert_eq!(summary.excluded, 2);
}

#[test]
fn p5_walk_03_a_home_without_protected_folders_has_access() {
    // The probe against a temporary home, never the real one: Mail and
    // Safari are not there, which is no refusal. The mocked refusal
    // (`DUSCOPE_TEST_TCC=denied`) reaches the binary's tests through the
    // environment of the commands they run.
    let env = TestEnv::new();
    assert!(!duscope_walk::full_disk_access_missing(&env.home()));
}
