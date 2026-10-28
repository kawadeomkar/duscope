//! P1-WALK-08: the error matrix, vanished entries, and symbolic links.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    DirId, Entry, Kind, MemProvider, ScanError, ScanEvent, WalkOptions, WalkSummary, walk,
    walk_with,
};

/// What a walk delivered, by path.
#[derive(Debug, Default)]
struct Seen {
    entries: HashMap<PathBuf, Entry>,
    /// Scan errors: (path of the directory, name) and errno.
    errors: HashMap<PathBuf, i32>,
    /// Directories closed with an error number.
    unreadable: HashMap<PathBuf, i32>,
    /// What vanished: a directory's path, or a listed entry's.
    vanished: Vec<PathBuf>,
    summary: WalkSummary,
}

fn seen(root: &Path, walked: impl FnOnce(&mut dyn FnMut(ScanEvent)) -> WalkSummary) -> Seen {
    let mut dirs: HashMap<DirId, PathBuf> = HashMap::new();
    let mut seen = Seen::default();
    let path_of = |dirs: &HashMap<DirId, PathBuf>, dir: DirId, name: &OsStr| dirs[&dir].join(name);
    let summary = walked(&mut |event| match event {
        ScanEvent::Entry(entry) => {
            let path = if entry.dir == Some(DirId::ROOT) {
                root.to_owned()
            } else {
                path_of(&dirs, entry.parent, entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            seen.entries.insert(path, entry);
        }
        ScanEvent::Error(ScanError { dir, name, errno }) => {
            seen.errors
                .insert(path_of(&dirs, dir, name.as_os_str()), errno);
        }
        ScanEvent::DirClosed {
            dir,
            errno: Some(errno),
        } => {
            seen.unreadable.insert(dirs[&dir].clone(), errno);
        }
        ScanEvent::Vanished { dir, name } => seen.vanished.push(match name {
            Some(name) => path_of(&dirs, dir, name.as_os_str()),
            None => dirs[&dir].clone(),
        }),
        _ => {}
    });
    seen.summary = summary;
    seen
}

fn mem_walk(tree: &MemProvider) -> Seen {
    let root = tree.root_path().to_owned();
    seen(&root, |sink| {
        walk_with(tree, &root, &WalkOptions::default(), sink).unwrap()
    })
}

#[test]
fn p1_walk_08_vanished_entries_and_directories_are_counted() {
    let mut tree = MemProvider::new("/t");
    tree.file("gone.txt", 10)
        .file("kept.txt", 10)
        .dir("gone-dir")
        .file("gone-dir/inside.txt", 10)
        .dir("stale-dir")
        .unstatable("gone.txt", libc::ENOENT)
        .unreadable("gone-dir", libc::ENOENT)
        .unreadable("stale-dir", libc::ESTALE);
    let seen = mem_walk(&tree);
    let mut vanished = seen.vanished.clone();
    vanished.sort();
    assert_eq!(
        vanished,
        ["/t/gone-dir", "/t/gone.txt", "/t/stale-dir"].map(PathBuf::from)
    );
    assert_eq!(seen.summary.vanished, 3);
    // Neither an error nor an unreadable directory, and a vanished file is
    // not delivered.
    assert!(seen.errors.is_empty(), "{:?}", seen.errors);
    assert!(seen.unreadable.is_empty(), "{:?}", seen.unreadable);
    assert_eq!(seen.summary.unreadable_dirs, 0);
    assert!(!seen.entries.contains_key(Path::new("/t/gone.txt")));
    assert!(seen.entries.contains_key(Path::new("/t/kept.txt")));
    // The vanished directory keeps its entry, empty.
    assert_eq!(seen.entries[Path::new("/t/gone-dir")].kind, Kind::Dir);
}

#[test]
fn p1_walk_08_the_error_matrix() {
    let mut tree = MemProvider::new("/t");
    tree.dir("denied")
        .file("denied/x", 1)
        .dir("perm")
        .dir("loop")
        .dir("long")
        .dir("io")
        .dir("swapped")
        .file("bad.txt", 1)
        .unreadable("denied", libc::EACCES)
        .unreadable("perm", libc::EPERM)
        .unreadable("loop", libc::ELOOP)
        .unreadable("long", libc::ENAMETOOLONG)
        .unreadable("io", libc::EIO)
        .unreadable("swapped", libc::ENOTDIR)
        .unstatable("bad.txt", libc::EIO);
    let seen = mem_walk(&tree);
    // EACCES and EPERM: listed empty, with the error number.
    assert_eq!(
        seen.unreadable.get(Path::new("/t/denied")),
        Some(&libc::EACCES)
    );
    assert_eq!(
        seen.unreadable.get(Path::new("/t/perm")),
        Some(&libc::EPERM)
    );
    assert!(!seen.entries.contains_key(Path::new("/t/denied/x")));
    // ELOOP, ENAMETOOLONG and EIO on a directory already listed: listed
    // empty with the error number too, so the unreadable flag and the
    // partial read cover it, and it is counted once.
    for (name, errno) in [
        ("loop", libc::ELOOP),
        ("long", libc::ENAMETOOLONG),
        ("io", libc::EIO),
    ] {
        let path = Path::new("/t").join(name);
        assert_eq!(seen.unreadable.get(&path), Some(&errno), "{name}");
        assert!(!seen.errors.contains_key(&path), "{name}");
        assert_eq!(seen.entries[&path].kind, Kind::Dir);
    }
    assert_eq!(seen.summary.unreadable_dirs, 5);
    // ENOTDIR: the directory was replaced by a file after it was listed.
    assert!(!seen.unreadable.contains_key(Path::new("/t/swapped")));
    assert_eq!(seen.summary.vanished, 1);
    // A file that cannot be read: a scan error of its directory.
    assert_eq!(seen.errors.get(Path::new("/t/bad.txt")), Some(&libc::EIO));
    assert_eq!(seen.summary.entry_errors, 1);
}

fn fixture_walk(root: &Path, follow: bool) -> Seen {
    let opts = WalkOptions {
        follow_symlinks: follow,
        ..WalkOptions::default()
    };
    seen(root, |sink| walk(root, &opts, sink).unwrap())
}

#[test]
fn p1_walk_08_symlinks_are_not_followed_by_default() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("symlinks");
    let seen = fixture_walk(&root, false);
    for link in ["to-dir", "to-file", "here", "self", "loop-a", "dangling"] {
        assert_eq!(seen.entries[&root.join(link)].kind, Kind::Symlink, "{link}");
    }
    assert!(!seen.entries.contains_key(&root.join("to-dir/inside.txt")));
    assert!(seen.errors.is_empty(), "{:?}", seen.errors);
}

#[test]
fn p1_walk_08_a_symlink_cycle_terminates_under_follow_symlinks() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("symlinks");
    let seen = fixture_walk(&root, true);
    // Links that loop, and links to a directory on their own path, are
    // errors of the directory holding them.
    let eloop = |rel: &str| seen.errors.get(&root.join(rel)).copied();
    for link in ["self", "loop-a", "loop-b", "here", "cycle/a/b/back-to-a"] {
        assert_eq!(eloop(link), Some(libc::ELOOP), "{link}: {:?}", seen.errors);
        assert!(!seen.entries.contains_key(&root.join(link)), "{link}");
    }
    // Links that lead somewhere are what they lead to, under their own name.
    let to_dir = &seen.entries[&root.join("to-dir")];
    assert_eq!(to_dir.kind, Kind::Dir);
    assert_eq!(to_dir.name.as_os_str().as_bytes(), b"to-dir");
    let inside = &seen.entries[&root.join("to-dir/inside.txt")];
    assert_eq!((inside.kind, inside.apparent), (Kind::File, 2048));
    for link in ["to-file", "chain-1"] {
        let entry = &seen.entries[&root.join(link)];
        assert_eq!((entry.kind, entry.apparent), (Kind::File, 2048), "{link}");
    }
    // A link that leads nowhere stays a link.
    for link in ["dangling", "absolute-dangling"] {
        assert_eq!(seen.entries[&root.join(link)].kind, Kind::Symlink, "{link}");
    }
}
