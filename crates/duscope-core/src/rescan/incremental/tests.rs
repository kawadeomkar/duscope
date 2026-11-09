//! P5-CORE-01: the incremental rescan's checks and merge on trees on disk,
//! with the history scripted as `FSEvents` reports such changes, so they
//! run on every platform: a merged index equals a full scan's, and every
//! fallback reason is reached. The real history's replay of the fixture
//! tree is `duscope/tests/incremental.rs`, in the platform tests.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use duscope_testkit::TestEnv;
use duscope_walk::fsevents::Replay;
use duscope_walk::{
    Attrs, FsEventsError, FsEventsMark, ScanEvent, WalkOptions, WalkSummary, provider, walk_with,
};
use rusqlite::types::Value;

use super::{Changes, Checks, Fallback, History, MAX_AGE_NS, Merged, Stop, merge, plan, sample};
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::rollup::roll_up;

/// The mark every volume of a test's scans carries, on every platform.
fn scan_mark() -> FsEventsMark {
    FsEventsMark {
        uuid: "5A1E0000-0000-0000-0000-000000000000".to_owned(),
        event_id: 100,
        os_build: "25G332".to_owned(),
    }
}

/// A replay's answer, given the directory it is limited to.
type Answer<'a> = &'a dyn Fn(&Path) -> Result<Replay, FsEventsError>;

/// A history that answers every replay with `replay`, at event id 200.
struct Script<'a> {
    now: Option<u64>,
    replay: Answer<'a>,
    replays: Cell<usize>,
    /// Whether the volumes' histories may stand in for a read.
    reusable: bool,
}

impl History for Script<'_> {
    fn now(&self) -> Option<u64> {
        self.now
    }

    fn replay(
        &self,
        _dev: u64,
        mark: &FsEventsMark,
        under: &Path,
    ) -> Result<Replay, FsEventsError> {
        assert_eq!(*mark, scan_mark());
        self.replays.set(self.replays.get() + 1);
        (self.replay)(under)
    }

    fn mark(&self, _dev: u64, now: u64) -> Option<FsEventsMark> {
        Some(FsEventsMark {
            event_id: now,
            ..scan_mark()
        })
    }

    fn reusable(&self, _fs_type: &str) -> bool {
        self.reusable
    }
}

fn script(replay: Answer<'_>) -> Script<'_> {
    Script {
        now: Some(200),
        replay,
        replays: Cell::new(0),
        reusable: true,
    }
}

/// A replay of `root` that reports `dirs` (relative to it) changed.
fn reporting(root: &Path, dirs: &[&str]) -> Replay {
    Replay {
        root: root.to_owned(),
        dirs: dirs
            .iter()
            .map(|d| {
                if d.is_empty() {
                    root.to_owned()
                } else {
                    root.join(d)
                }
            })
            .collect(),
        history_done: true,
        ..Replay::default()
    }
}

/// The time now, Unix nanoseconds.
fn now_ns() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_nanos()).unwrap()
}

/// A scan's `scan_meta`, started at `started`.
fn meta(root: &Path, id: &str, started: i64, options: &str, excluded: u64) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(id.as_bytes(), started, [0; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at: started,
        finished_at: started + 1,
        options: options.to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 4,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count: excluded,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

fn opts() -> WalkOptions {
    WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    }
}

/// Writes `arena`'s index under `dir` and opens it.
fn write(mut arena: Arena, meta: &ScanMeta, dir: &Path) -> Index {
    let rollup = roll_up(&mut arena);
    IndexWriter::write(&arena, &rollup, meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// A full scan of `root`, each volume marked, saved under `dir`.
fn full(root: &Path, dir: &Path, id: &str, started: i64, options: &str) -> (Index, WalkSummary) {
    full_with(&opts(), root, dir, id, started, options)
}

/// [`full`] with the walk's options `walk`.
fn full_with(
    walk: &WalkOptions,
    root: &Path,
    dir: &Path,
    id: &str,
    started: i64,
    options: &str,
) -> (Index, WalkSummary) {
    let mut arena = Arena::new(Attrs::Basic);
    let provider = provider(walk).unwrap();
    let summary = walk_with(&*provider, root, walk, |mut event| {
        if let ScanEvent::Volume(volume) = &mut event {
            volume.fsevents = volume.entered().then(scan_mark);
        }
        arena.push(event).unwrap();
    })
    .unwrap();
    let meta = meta(root, id, started, options, summary.excluded);
    (write(arena, &meta, dir), summary)
}

/// The incremental rescan of `parent`'s root, saved under `dir`.
fn incremental(
    parent: &Index,
    root: &Path,
    dir: &Path,
    history: &dyn History,
) -> Result<(Index, Merged), Stop> {
    incremental_with(&opts(), parent, root, dir, history)
}

/// [`incremental`] with the walk's options `walk`.
fn incremental_with(
    walk: &WalkOptions,
    parent: &Index,
    root: &Path,
    dir: &Path,
    history: &dyn History,
) -> Result<(Index, Merged), Stop> {
    let checks = Checks {
        now_ns: parent.meta().started_at + 3_600_000_000_000,
        max_age_ns: MAX_AGE_NS,
        tool_version: "0.0.0",
    };
    let planned = plan(parent, &checks, history)?;
    let mut arena = Arena::new(Attrs::Basic);
    let provider = provider(walk).unwrap();
    let merged = merge(
        &planned,
        parent,
        root,
        walk,
        &*provider,
        history,
        &mut |event| arena.push(event).unwrap(),
    )?;
    let mut meta = meta(
        root,
        "merged",
        parent.meta().started_at + 1,
        "{}",
        merged.summary.excluded,
    );
    meta.incremental = true;
    Ok((write(arena, &meta, dir), merged))
}

/// Every row of `table`, in its key's order.
fn rows(index: &Index, table: &str, order: &str) -> Vec<Vec<Value>> {
    let mut stmt = index
        .db()
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
        .unwrap();
    let width = stmt.column_count();
    stmt.query_map([], |row| {
        (0..width).map(|i| row.get::<_, Value>(i)).collect()
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// The tables two scans of one tree must hold alike.
const TABLES: [(&str, &str); 7] = [
    ("dirs", "id"),
    ("files", "id"),
    ("hardlinks", "dev, ino"),
    ("clones", "dev, clone_id"),
    ("shared_members", "id"),
    ("scan_errors", "dir_id, name"),
    ("type_stats", "ext"),
];

fn assert_same(merged: &Index, fresh: &Index) {
    for (table, order) in TABLES {
        let (got, want) = (rows(merged, table, order), rows(fresh, table, order));
        if got != want {
            let first = got
                .iter()
                .zip(&want)
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(want.len()));
            panic!(
                "{table} differs ({} rows against {}), first at row {first}:\n merged {:?}\n  fresh {:?}",
                got.len(),
                want.len(),
                got.get(first),
                want.get(first)
            );
        }
    }
}

fn file(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; bytes]).unwrap();
}

/// Clones `from` to `to` (APFS `clonefile`, a reflink elsewhere); false
/// where the filesystem has neither.
fn clone(from: &Path, to: &Path) -> bool {
    let tries: [&[&str]; 2] = [&["-c"], &["--reflink=always"]];
    tries.iter().any(|flags| {
        Command::new("cp")
            .args(*flags)
            .arg(from)
            .arg(to)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// A tree of 140 directories: 120 of bulk under `keep/`, beside the ones
/// the scripted changes touch.
fn tree(root: &Path) {
    for k in 0..120 {
        file(&root.join(format!("keep/k{k}/f.txt")), 100 + k);
    }
    for d in ["a/a1", "a/a2", "a/a3", "a/a4", "a/a5"] {
        file(&root.join(d).join("f"), 2000);
    }
    file(&root.join("a/a5/src.bin"), 64 * 1024);
    file(&root.join("b/bfile"), 1000);
    file(&root.join("links/x/one"), 30_000);
    fs::create_dir_all(root.join("links/y")).unwrap();
    file(&root.join("keep/k5/pair1"), 40_000);
    fs::create_dir_all(root.join("pair/z")).unwrap();
    fs::hard_link(root.join("keep/k5/pair1"), root.join("pair/z/pair2")).unwrap();
    file(&root.join("deep/d1/d2/d3/file"), 10);
    file(&root.join("doomed/sub1/sub2/file"), 10);
    file(&root.join("mover/m/file"), 500);
    // Entries other than files and directories, which nothing touches.
    let made = Command::new("mkfifo")
        .arg(root.join("keep/k11/pipe"))
        .status()
        .unwrap();
    assert!(made.success());
    std::os::unix::fs::symlink("f.txt", root.join("keep/k12/link")).unwrap();
}

#[test]
fn p5_core_01_a_merged_index_equals_a_full_scan_after_scripted_changes() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    // Clones where the filesystem makes them (APFS): a pair, one of which
    // goes, leaving the other alone in a directory nothing touches.
    let clones = clone(&root.join("keep/k7/f.txt"), &root.join("keep/k8/c2"));
    let index_dir = env.tmp_dir().join("index");
    let (parent, _) = full(&root, &index_dir, "parent", now_ns(), "{}");

    // Creates, deletes, renames, links and chmods, each with the
    // directories FSEvents reports for it.
    let mut reported = Vec::new();
    file(&root.join("a/a1/new.txt"), 3000);
    reported.push("a/a1");
    fs::remove_file(root.join("deep/d1/d2/d3/file")).unwrap();
    reported.push("deep/d1/d2/d3");
    fs::remove_dir_all(root.join("doomed/sub1")).unwrap();
    reported.extend(["doomed", "doomed/sub1", "doomed/sub1/sub2"]);
    fs::rename(root.join("mover/m"), root.join("b/m")).unwrap();
    reported.extend(["mover", "b"]);
    // A new link of a file nothing else touches: FSEvents reports the new
    // name's directory, and the original's as a change of its parent.
    fs::hard_link(root.join("links/x/one"), root.join("links/y/one2")).unwrap();
    reported.extend(["links/y", "links"]);
    // One link of a pair gone: the other, untouched, has one link now.
    fs::remove_file(root.join("pair/z/pair2")).unwrap();
    reported.push("pair/z");
    fs::set_permissions(root.join("a/a2/f"), fs::Permissions::from_mode(0o600)).unwrap();
    reported.push("a/a2");
    // A directory's mode is a change of its parent's entries.
    fs::set_permissions(root.join("a/a3"), fs::Permissions::from_mode(0o700)).unwrap();
    reported.push("a");
    fs::write(root.join("b/bfile"), vec![9u8; 50_000]).unwrap();
    file(&root.join("a/a4/newdir/inner/file"), 4000);
    reported.extend(["a/a4", "a/a4/newdir", "a/a4/newdir/inner"]);
    if clones {
        // The pair's other member, now sharing nothing; and a new clone of
        // a file that shared nothing, whose directory FSEvents reports.
        fs::remove_file(root.join("keep/k8/c2")).unwrap();
        reported.push("keep/k8");
        assert!(clone(
            &root.join("a/a5/src.bin"),
            &root.join("keep/k9/dst.bin")
        ));
        reported.extend(["keep/k9", "a/a5"]);
    }

    let replay = |under: &Path| Ok(reporting(under, &reported));
    let history = script(&replay);
    let (merged, outcome) =
        incremental(&parent, &root, &env.tmp_dir().join("merged"), &history).unwrap();
    assert_eq!(history.replays.get(), 1, "one volume, replayed at the root");
    let (fresh, walked) = full(&root, &env.tmp_dir().join("fresh"), "fresh", now_ns(), "{}");
    assert_same(&merged, &fresh);
    assert_eq!(outcome.summary.entries, walked.entries);
    assert_eq!(outcome.summary.dirs, walked.dirs);
    // The reported directories, their subdirectories and the way down to
    // them were read; `keep/`'s untouched subdirectories were not.
    let total = rows(&fresh, "dirs", "id").len() as u64;
    assert!(
        outcome.rescanned_dirs < total / 3,
        "{} of {total} directories read",
        outcome.rescanned_dirs
    );
    // The new scan's volumes carry the event id sampled before the replay.
    let marks = rows(&merged, "scan_volumes", "rowid");
    assert_eq!(marks[0][10], Value::Integer(200), "{marks:?}");
}

/// A saved scan of the test tree, and what `incremental` gives with the
/// history `replay`.
fn fallback_of(env: &TestEnv, change: impl FnOnce(&Path), replay: Answer<'_>) -> Option<Fallback> {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    let (parent, _) = full(
        &root,
        &env.tmp_dir().join("index"),
        "parent",
        now_ns(),
        "{}",
    );
    change(&root);
    let history = script(replay);
    match incremental(&parent, &root, &env.tmp_dir().join("merged"), &history) {
        Ok(_) => None,
        Err(Stop::Fallback(reason, detail)) => {
            assert!(!detail.is_empty());
            Some(reason)
        }
        Err(other) => panic!("{other:?}"),
    }
}

fn nothing(_: &Path) {}

#[test]
fn p5_core_01_each_failed_check_names_its_fallback_reason() {
    let quiet = |under: &Path| Ok(reporting(under, &[]));
    assert_eq!(fallback_of(&TestEnv::new(), nothing, &quiet), None);
    let failing = |error: fn(&Path) -> FsEventsError| {
        move |under: &Path| -> Result<Replay, FsEventsError> { Err(error(under)) }
    };
    let cases: [(Fallback, Answer<'_>); 6] = [
        (
            Fallback::VolumeChanged,
            &failing(|_| FsEventsError::OtherHistory {
                dev: 1,
                recorded: "A".to_owned(),
                found: "B".to_owned(),
            }),
        ),
        (
            Fallback::HistoryGone,
            &failing(|_| FsEventsError::IdsBehind {
                since: 100,
                current: 3,
            }),
        ),
        (
            Fallback::FseventsDisabled,
            &failing(|under| FsEventsError::Disabled {
                mount_point: under.to_owned(),
            }),
        ),
        (Fallback::EventsDropped, &|under: &Path| {
            Ok(Replay {
                dropped: true,
                ..reporting(under, &[])
            })
        }),
        (Fallback::HistoryGone, &|under: &Path| {
            Ok(Replay {
                history_done: false,
                ..reporting(under, &[])
            })
        }),
        (Fallback::TooManyChanges, &|under: &Path| {
            let dirs: Vec<String> = (0..30).map(|k| format!("keep/k{k}")).collect();
            let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
            Ok(reporting(under, &dirs))
        }),
    ];
    for (reason, replay) in cases {
        assert_eq!(
            fallback_of(&TestEnv::new(), nothing, replay),
            Some(reason),
            "{}",
            reason.as_str()
        );
    }
    // The post-check: a directory changed that the history does not
    // report, deep in a subtree taken from the saved scan.
    assert_eq!(
        fallback_of(
            &TestEnv::new(),
            |root| file(&root.join("keep/k3/unreported"), 10),
            &quiet
        ),
        Some(Fallback::PostCheckFailed)
    );
    // The root itself replaced while a directory above it changed
    // (DEV-122): its ctime is not the scan's.
    let above = |under: &Path| {
        Ok(Replay {
            ancestors_changed: true,
            ..reporting(under, &[])
        })
    };
    assert_eq!(fallback_of(&TestEnv::new(), nothing, &above), None);
    assert_eq!(
        fallback_of(
            &TestEnv::new(),
            |root| fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap(),
            &above
        ),
        Some(Fallback::PostCheckFailed)
    );
}

#[test]
fn p5_core_01_age_options_and_history_are_checked_before_any_replay() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    let excludes = env.tmp_dir().join("excludes.txt");
    fs::write(&excludes, "*.tmp\n").unwrap();
    let options = format!(r#"{{"exclude_files": [{:?}]}}"#, excludes.to_str().unwrap());
    let started = now_ns();
    let (parent, _) = full(
        &root,
        &env.tmp_dir().join("index"),
        "parent",
        started,
        &options,
    );
    let replay = |_: &Path| -> Result<Replay, FsEventsError> { panic!("replayed") };
    let history = script(&replay);
    let checked = |now_ns: i64, tool_version: &str, history: &dyn History| {
        let checks = Checks {
            now_ns,
            max_age_ns: MAX_AGE_NS,
            tool_version,
        };
        match plan(&parent, &checks, history) {
            Err(Stop::Fallback(reason, _)) => Some(reason),
            Err(Stop::NoHistory) => None,
            other => panic!("{other:?}"),
        }
    };
    let day = 86_400_000_000_000;
    assert_eq!(
        checked(started + 8 * day, "0.0.0", &history),
        Some(Fallback::IndexTooOld)
    );
    assert_eq!(
        checked(started + day, "9.9.9", &history),
        Some(Fallback::OptionsChanged),
        "a scan another duscope took"
    );
    // The file `--exclude-from` named, edited since the scan.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&excludes, "*.tmp\n*.log\n").unwrap();
    assert_eq!(
        checked(started + day, "0.0.0", &history),
        Some(Fallback::OptionsChanged)
    );
    // No history on this platform: no check failed, and none ran.
    let none = Script {
        now: None,
        replay: &replay,
        replays: Cell::new(0),
        reusable: true,
    };
    assert_eq!(checked(started + 8 * day, "9.9.9", &none), None);
    // A scan that followed links read outside the root's history.
    let linked = fs::canonicalize(env.tmp_dir()).unwrap().join("linked");
    tree(&linked);
    let (followed, _) = full(
        &linked,
        &env.tmp_dir().join("index"),
        "followed",
        started,
        r#"{"follow_symlinks": true}"#,
    );
    let checks = Checks {
        now_ns: started,
        max_age_ns: MAX_AGE_NS,
        tool_version: "0.0.0",
    };
    assert!(matches!(
        plan(&followed, &checks, &history),
        Err(Stop::Fallback(Fallback::HistoryGone, _))
    ));
}

#[test]
fn p5_core_01_a_scan_without_a_mark_is_volume_changed() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    // Saved with no mark, as on a volume FSEvents keeps no history of.
    let mut arena = Arena::new(Attrs::Basic);
    let provider = provider(&opts()).unwrap();
    walk_with(&*provider, &root, &opts(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let parent = write(
        arena,
        &meta(&root, "unmarked", now_ns(), "{}", 0),
        &env.tmp_dir(),
    );
    let quiet = |under: &Path| Ok(reporting(under, &[]));
    let history = script(&quiet);
    assert!(matches!(
        incremental(&parent, &root, &env.tmp_dir().join("merged"), &history),
        Err(Stop::Fallback(Fallback::VolumeChanged, _))
    ));
    assert_eq!(history.replays.get(), 0);
}

#[test]
fn p5_core_01_reasons_are_the_conventions_values() {
    assert_eq!(
        Fallback::ALL.map(Fallback::as_str),
        [
            "index_too_old",
            "volume_changed",
            "history_gone",
            "fsevents_disabled",
            "options_changed",
            "too_many_changes",
            "events_dropped",
            "post_check_failed",
        ]
    );
    assert_eq!(Fallback::OptionsChanged.words(), "options changed");
    for (walk, core) in [
        (
            duscope_walk::fsevents::Fallback::VolumeChanged,
            Fallback::VolumeChanged,
        ),
        (
            duscope_walk::fsevents::Fallback::HistoryGone,
            Fallback::HistoryGone,
        ),
        (
            duscope_walk::fsevents::Fallback::FseventsDisabled,
            Fallback::FseventsDisabled,
        ),
        (
            duscope_walk::fsevents::Fallback::EventsDropped,
            Fallback::EventsDropped,
        ),
    ] {
        assert_eq!(Fallback::from(walk), core);
        assert_eq!(walk.as_str(), core.as_str());
    }
}

#[test]
fn p5_core_01_the_way_down_and_subtrees_to_read_whole() {
    let mut changes = Changes::default();
    changes.changed.insert(b"a/b/c".to_vec());
    changes.deep.insert(b"x/y".to_vec());
    changes.close();
    let above: Vec<&[u8]> = vec![b"", b"a", b"a/b", b"x"];
    for rel in above {
        assert!(changes.above.contains(rel), "{}", rel.escape_ascii());
    }
    assert_eq!(changes.above.len(), 4);
    // The children of a reported directory are read, not its grandchildren.
    assert!(changes.reads(b"a/b/c", b"a/b/c/d"));
    assert!(!changes.reads(b"a/b/c/d", b"a/b/c/d/e"));
    assert!(changes.reads(b"x", b"x/y"));
    assert!(changes.reads(b"x/y/z", b"x/y/z/w"));
    assert!(!changes.reads(b"x", b"x/yy"));
    assert!(!changes.reads(b"", b"q"));
}

#[test]
fn p5_core_01_the_post_check_sample_is_distinct_and_in_order() {
    assert_eq!(sample(5, 10), vec![0, 1, 2, 3, 4]);
    for _ in 0..20 {
        let picked = sample(10_000, 1000);
        assert_eq!(picked.len(), 1000);
        assert!(picked.windows(2).all(|w| w[0] < w[1]));
        assert!(picked.iter().all(|&i| i < 10_000));
    }
}

/// Globs as the binary compiles `--exclude`: anywhere below the root.
fn globs(patterns: &[&str]) -> globset::GlobSet {
    let mut set = globset::GlobSetBuilder::new();
    for pattern in patterns {
        set.add(
            globset::GlobBuilder::new(&format!("**/{pattern}"))
                .literal_separator(true)
                .build()
                .unwrap(),
        );
    }
    set.build().unwrap()
}

#[test]
fn p5_core_01_what_the_excludes_left_out_is_counted_once() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    // A file and a directory left out where the history reports a change,
    // and the same where it does not.
    for dir in ["a/a1", "keep/k3"] {
        file(&root.join(dir).join("left.log"), 10);
        file(&root.join(dir).join("cachey/inside"), 10);
    }
    let walk = WalkOptions {
        excludes: globs(&["*.log", "cachey"]),
        ..opts()
    };
    let index_dir = env.tmp_dir().join("index");
    let (parent, saved) = full_with(&walk, &root, &index_dir, "parent", now_ns(), "{}");
    assert_eq!(saved.excluded, 4);
    file(&root.join("a/a1/new.txt"), 10);
    let replay = |under: &Path| Ok(reporting(under, &["a/a1"]));
    let history = script(&replay);
    let (merged, outcome) = incremental_with(
        &walk,
        &parent,
        &root,
        &env.tmp_dir().join("merged"),
        &history,
    )
    .unwrap();
    let (fresh, walked) = full_with(
        &walk,
        &root,
        &env.tmp_dir().join("fresh"),
        "fresh",
        now_ns(),
        "{}",
    );
    assert_same(&merged, &fresh);
    assert_eq!(outcome.summary.excluded, walked.excluded);
    assert_eq!(outcome.summary.skipped_dirs, walked.skipped_dirs);
}

#[test]
fn p5_core_01_a_volume_whose_history_is_never_reused_is_read_whole() {
    let quiet = |under: &Path| Ok(reporting(under, &[]));
    let history = Script {
        reusable: false,
        ..script(&quiet)
    };
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    tree(&root);
    let (parent, _) = full(
        &root,
        &env.tmp_dir().join("index"),
        "parent",
        now_ns(),
        "{}",
    );
    assert!(matches!(
        incremental(&parent, &root, &env.tmp_dir().join("merged"), &history),
        Err(Stop::Fallback(Fallback::FseventsDisabled, _))
    ));
    assert_eq!(history.replays.get(), 0, "not replayed");
}

#[test]
fn p5_core_01_a_root_put_in_place_of_the_scanned_one_is_read_again() {
    let env = TestEnv::new();
    let base = fs::canonicalize(env.tmp_dir()).unwrap();
    let root = base.join("tree");
    tree(&root);
    let (parent, _) = full(&root, &base.join("index"), "parent", now_ns(), "{}");
    // Another tree in its place, a copy that keeps every time but the
    // status-change times no copy can keep, with an entry of its own: the
    // history reports the root changed, and a directory above it.
    fs::rename(&root, base.join("scanned")).unwrap();
    let copied = Command::new("cp")
        .arg("-Rp")
        .arg(base.join("scanned"))
        .arg(&root)
        .status()
        .unwrap();
    assert!(copied.success());
    file(&root.join("own.txt"), 10);
    let above = |under: &Path| {
        Ok(Replay {
            ancestors_changed: true,
            ..reporting(under, &[""])
        })
    };
    let history = script(&above);
    let (merged, outcome) = incremental(&parent, &root, &base.join("merged"), &history).unwrap();
    let (fresh, walked) = full(&root, &base.join("fresh"), "fresh", now_ns(), "{}");
    assert_same(&merged, &fresh);
    // No directory of the copy passed for the scanned one's.
    assert_eq!(outcome.rescanned_dirs, walked.dirs);
}
