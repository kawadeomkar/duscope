//! P1-WALK-01: the walk over in-memory trees; P1-WALK-03: directories split
//! into stat batches.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use proptest::prelude::*;

use super::*;

/// Directories in the synthetic trees of the acceptance checks.
const DIRS: u32 = 10_000;

/// A tree of `DIRS` directories: directory `i` hangs below one of the
/// `window` directories before it (`picks[i - 1]` chooses which), and holds
/// `files[i]` files. A small window makes a deep tree (a window of 16 nests
/// about 600 levels), a large one a wide tree.
fn synthetic(picks: &[u32], window: u32, files: &[u8]) -> MemProvider {
    let mut tree = MemProvider::new("/mem");
    let mut paths = vec![PathBuf::new()];
    for (i, pick) in (1..DIRS).zip(picks) {
        let back = pick % window.min(i) + 1;
        let path = paths[(i - back) as usize].join(format!("d{i}"));
        tree.dir(&path);
        paths.push(path);
    }
    for (path, &count) in paths.iter().zip(files) {
        for j in 0..count {
            tree.file(path.join(format!("f{j}")), u64::from(j) * 1000);
        }
    }
    tree
}

/// Records every directory read.
struct Counting<'a> {
    inner: &'a MemProvider,
    reads: Mutex<Vec<(DirId, PathBuf)>>,
}

impl Provider for Counting<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push((dir.id(), dir.path().to_owned()));
        self.inner.read_dir(dir, opts, out, errs)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

/// Sets the cancellation flag during its `after`-th read.
struct CancelAfter<'a> {
    inner: &'a MemProvider,
    after: usize,
    reads: AtomicUsize,
    cancel: Arc<AtomicBool>,
}

impl Provider for CancelAfter<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        if self.reads.fetch_add(1, Ordering::Relaxed) + 1 == self.after {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.inner.read_dir(dir, opts, out, errs)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

/// Holds each thread's first pass until `threads` threads have passed, or
/// ten seconds have gone by. A test proves that work reached that many
/// workers without relying on timing, and a worker that never comes fails
/// the test's assertion instead of hanging it.
pub(crate) struct Gate {
    threads: usize,
    seen: Mutex<HashSet<ThreadId>>,
    all: Condvar,
}

impl Gate {
    pub(crate) fn new(threads: usize) -> Gate {
        Gate {
            threads,
            seen: Mutex::new(HashSet::new()),
            all: Condvar::new(),
        }
    }

    pub(crate) fn pass(&self) {
        let mut seen = self.seen.lock().unwrap();
        if seen.insert(thread::current().id()) {
            self.all.notify_all();
            let wait = self
                .all
                .wait_timeout_while(seen, Duration::from_secs(10), |seen| {
                    seen.len() < self.threads
                });
            drop(wait.unwrap());
        }
    }
}

/// Lists the first `keep` children of every directory and leaves the rest
/// for stat batches, recording which threads read the batches. Each
/// worker's first batch waits at `gate`. With `fail_stats`, every batch
/// fails as if its directory had vanished.
struct Splitting<'a> {
    inner: &'a MemProvider,
    keep: usize,
    fail_stats: bool,
    batches: Mutex<Vec<(usize, ThreadId)>>,
    gate: Gate,
}

impl Provider for Splitting<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        self.inner.read_dir(dir, opts, out, errs)
    }

    fn read_dir_split(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
    ) -> io::Result<()> {
        let mut all = Vec::new();
        self.inner.read_dir(dir, opts, &mut all, errs)?;
        let rest = all.split_off(all.len().min(self.keep));
        out.extend(all);
        later.extend(rest.into_iter().map(|entry| entry.name));
        Ok(())
    }

    fn stat_names(
        &self,
        dir: &DirHandle,
        names: &[NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        self.batches
            .lock()
            .unwrap()
            .push((names.len(), thread::current().id()));
        self.gate.pass();
        if self.fail_stats {
            return Err(io::Error::from_raw_os_error(2));
        }
        let wanted: HashSet<&NameRef> = names.iter().collect();
        let mut all = Vec::new();
        self.inner.read_dir(dir, opts, &mut all, errs)?;
        out.extend(all.into_iter().filter(|entry| wanted.contains(&entry.name)));
        Ok(())
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

fn walk_events(
    provider: &impl Provider,
    root: &Path,
    opts: &WalkOptions,
) -> (Result<WalkSummary, WalkError>, Vec<ScanEvent>) {
    let mut events = Vec::new();
    let result = walk_with(provider, root, opts, |event| events.push(event));
    (result, events)
}

/// Rebuilds every entry's path from the events, checking the order rules on
/// the way: the root comes first, an entry's parent was announced before it
/// and is not yet closed, ids are never reused, and every directory closes
/// once. Returns the paths and each directory's path by id.
fn replay(root: &Path, events: &[ScanEvent]) -> (Vec<PathBuf>, HashMap<DirId, PathBuf>) {
    let mut dirs: HashMap<DirId, PathBuf> = HashMap::new();
    let mut closed = HashSet::new();
    let mut paths = Vec::new();
    let Some(ScanEvent::Entry(first)) = events.first() else {
        panic!(
            "the first event is not the root's entry: {:?}",
            events.first()
        );
    };
    assert_eq!((first.parent, first.dir), (DirId::ROOT, Some(DirId::ROOT)));
    dirs.insert(DirId::ROOT, root.to_owned());
    paths.push(root.to_owned());
    for event in &events[1..] {
        match event {
            ScanEvent::Entry(entry) => {
                let parent = dirs.get(&entry.parent).unwrap_or_else(|| {
                    panic!(
                        "{:?} arrived before its parent {:?}",
                        entry.name, entry.parent
                    )
                });
                assert!(
                    !closed.contains(&entry.parent),
                    "{:?} arrived after its parent closed",
                    entry.name
                );
                let path = parent.join(entry.name.as_os_str());
                assert_eq!(entry.dir.is_some(), entry.kind == Kind::Dir, "{path:?}");
                if let Some(id) = entry.dir {
                    assert!(dirs.insert(id, path.clone()).is_none(), "{id:?} reused");
                }
                paths.push(path);
            }
            ScanEvent::DirClosed { dir, .. } => {
                assert!(dirs.contains_key(dir), "{dir:?} closed before it was seen");
                assert!(closed.insert(*dir), "{dir:?} closed twice");
            }
            ScanEvent::Error(_) | ScanEvent::Vanished { .. } | ScanEvent::Volume(_) => {}
        }
    }
    assert_eq!(closed.len(), dirs.len(), "every directory closes");
    (paths, dirs)
}

/// The id `replay` found for the directory at `path`.
fn id_of(dirs: &HashMap<DirId, PathBuf>, path: &str) -> DirId {
    let found = dirs
        .iter()
        .find(|(_, dir)| dir.as_path() == Path::new(path));
    *found.unwrap_or_else(|| panic!("no directory {path}")).0
}

/// Paths as sorted bytes, for comparing large sets of deep paths cheaply.
fn sorted<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<&'a [u8]> {
    let mut bytes: Vec<&[u8]> = paths.map(|path| path.as_os_str().as_bytes()).collect();
    bytes.sort_unstable();
    bytes
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    #[test]
    fn p1_walk_01_every_directory_is_visited_exactly_once(
        picks in prop::collection::vec(any::<u32>(), DIRS as usize - 1),
        window in prop_oneof![16u32..64, 64u32..1024, Just(DIRS)],
        files in prop::collection::vec(0u8..4, DIRS as usize),
    ) {
        let tree = synthetic(&picks, window, &files);
        for threads in [1, 4, 32] {
            let counting = Counting { inner: &tree, reads: Mutex::default() };
            let opts = WalkOptions { threads, ..WalkOptions::default() };
            let (result, events) = walk_events(&counting, tree.root_path(), &opts);
            let summary = result.unwrap();

            // Every entry arrives once, in an order the sink can follow, and
            // the directories are numbered 0..DIRS.
            let (paths, dirs) = replay(tree.root_path(), &events);
            prop_assert_eq!(sorted(paths.iter().map(PathBuf::as_path)), sorted(tree.paths()));
            let mut ids: Vec<u32> = dirs.keys().map(|id| id.get()).collect();
            ids.sort_unstable();
            prop_assert!(ids.into_iter().eq(0..DIRS), "ids are dense");

            // Each of those directories was read exactly once, at its own path.
            let reads = counting.reads.into_inner().unwrap();
            let mut times = vec![0; DIRS as usize];
            for (id, path) in &reads {
                times[id.index()] += 1;
                prop_assert_eq!(path, &dirs[id]);
            }
            let wrong = times.iter().position(|&n| n != 1);
            prop_assert!(
                wrong.is_none(),
                "{} workers: directory {:?} read {:?} times", threads, wrong, wrong.map(|id| times[id])
            );

            prop_assert_eq!(summary, WalkSummary {
                entries: tree.entry_count() as u64,
                dirs: u64::from(DIRS),
                unreadable_dirs: 0,
                entry_errors: 0,
                threads,
            });
        }
    }
}

#[test]
fn p1_walk_01_cancellation_stops_within_one_directory_per_worker() {
    let picks: Vec<u32> = (0..DIRS - 1)
        .map(|i| i.wrapping_mul(2_654_435_761))
        .collect();
    let tree = synthetic(&picks, 64, &[1; DIRS as usize]);
    for threads in [1, 4, 32] {
        for after in [1, 40, 2_000] {
            let cancel = Arc::new(AtomicBool::new(false));
            let provider = CancelAfter {
                inner: &tree,
                after,
                reads: AtomicUsize::new(0),
                cancel: Arc::clone(&cancel),
            };
            let opts = WalkOptions {
                threads,
                cancel,
                ..WalkOptions::default()
            };
            let (result, events) = walk_events(&provider, tree.root_path(), &opts);
            assert!(
                matches!(result, Err(WalkError::Cancelled)),
                "{threads} workers, cancelled after {after}: {result:?}"
            );
            // The read that set the flag finishes, and each worker may finish
            // the one directory it had started; none starts another.
            let reads = provider.reads.load(Ordering::Relaxed);
            assert!(
                reads <= after + threads,
                "{threads} workers, cancelled after read {after}: {reads} reads"
            );
            assert!(events.len() < tree.entry_count(), "the walk stopped early");
        }
    }
}

#[test]
fn p1_walk_01_an_unreadable_directory_closes_with_its_errno() {
    let mut tree = MemProvider::new("/mem");
    tree.file("a/inside", 10)
        .file("b/visible", 20)
        .unreadable("a", 13);
    let (result, events) = walk_events(&tree, tree.root_path(), &WalkOptions::default());
    let (paths, dirs) = replay(tree.root_path(), &events);
    assert!(!paths.contains(&PathBuf::from("/mem/a/inside")));
    assert!(paths.contains(&PathBuf::from("/mem/b/visible")));
    let a = id_of(&dirs, "/mem/a");
    assert!(events.contains(&ScanEvent::DirClosed {
        dir: a,
        errno: Some(13)
    }));
    let summary = result.unwrap();
    assert_eq!((summary.unreadable_dirs, summary.entries), (1, 4));
}

#[test]
fn p1_walk_01_entries_that_cannot_be_read_become_scan_errors() {
    let mut tree = MemProvider::new("/mem");
    tree.file("d/ok", 1)
        .file("d/gone", 2)
        .unstatable("d/gone", 2);
    let (result, events) = walk_events(&tree, tree.root_path(), &WalkOptions::default());
    let (paths, dirs) = replay(tree.root_path(), &events);
    assert!(!paths.contains(&PathBuf::from("/mem/d/gone")));
    assert!(events.contains(&ScanEvent::Error(ScanError {
        dir: id_of(&dirs, "/mem/d"),
        name: NameRef::from("gone"),
        errno: 2,
    })));
    assert_eq!(result.unwrap().entry_errors, 1);
}

#[test]
fn p1_walk_01_symlinks_are_listed_and_not_entered() {
    let mut tree = MemProvider::new("/mem");
    tree.dir("real").symlink("link", 4);
    let counting = Counting {
        inner: &tree,
        reads: Mutex::default(),
    };
    let (result, events) = walk_events(&counting, tree.root_path(), &WalkOptions::default());
    let link = events.iter().find_map(|event| match event {
        ScanEvent::Entry(entry) if entry.name == NameRef::from("link") => Some(entry),
        _ => None,
    });
    let link = link.expect("the symlink is listed");
    assert_eq!(
        (link.kind, link.dir, link.apparent),
        (Kind::Symlink, None, 4)
    );
    let reads = counting.reads.into_inner().unwrap();
    assert!(reads.iter().all(|(_, path)| path != Path::new("/mem/link")));
    assert_eq!(result.unwrap().dirs, 2);
}

#[test]
fn p1_walk_01_a_file_root_is_the_whole_walk() {
    let mut tree = MemProvider::new("/mem");
    tree.file("only", 5000);
    let root = Path::new("/mem/only");
    let (result, events) = walk_events(&tree, root, &WalkOptions::default());
    let summary = result.unwrap();
    assert_eq!((summary.entries, summary.dirs), (1, 0));
    let [ScanEvent::Entry(entry)] = events.as_slice() else {
        panic!("one entry expected: {events:?}");
    };
    assert_eq!(entry.name, NameRef::from("only"));
    assert_eq!((entry.parent, entry.dir), (DirId::ROOT, None));
    assert_eq!((entry.apparent, entry.allocated), (5000, 8192));
}

#[test]
fn p1_walk_01_an_unreadable_root_is_an_error() {
    let tree = MemProvider::new("/mem");
    let (result, events) = walk_events(&tree, Path::new("/elsewhere"), &WalkOptions::default());
    match result {
        Err(WalkError::Root { path, source }) => {
            assert_eq!(path, Path::new("/elsewhere"));
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("expected a root error: {other:?}"),
    }
    assert!(events.is_empty());
}

#[test]
fn p1_walk_01_full_attrs_only_when_asked() {
    let mut tree = MemProvider::new("/mem");
    tree.file("a/b", 1).symlink("c", 3);
    for (attrs, full) in [(Attrs::Basic, false), (Attrs::Full, true)] {
        let opts = WalkOptions {
            attrs,
            ..WalkOptions::default()
        };
        let (result, events) = walk_events(&tree, tree.root_path(), &opts);
        result.unwrap();
        for event in &events {
            if let ScanEvent::Entry(entry) = event {
                assert_eq!(entry.full.is_some(), full, "{attrs:?} {:?}", entry.name);
            }
        }
    }
}

#[test]
fn p1_walk_01_threads_default_to_the_providers_choice() {
    let tree = MemProvider::new("/mem");
    let (result, _) = walk_events(&tree, tree.root_path(), &WalkOptions::default());
    let threads = result.unwrap().threads;
    assert_eq!(threads, tree.default_threads());
    assert!((4..=32).contains(&threads), "{threads}");
}

#[test]
fn p1_walk_01_flags_keep_their_bit_values() {
    // The index stores these bits; changing one would misread every index
    // written before.
    let bits = [
        (Flags::SPARSE, 1),
        (Flags::COMPRESSED, 2),
        (Flags::DATALESS, 4),
        (Flags::PURGEABLE, 8),
        (Flags::MAY_SHARE, 16),
        (Flags::SHARES_ALL, 32),
        (Flags::MOUNT_ROOT, 64),
        (Flags::SUBVOLUME, 128),
    ];
    for (flag, bit) in bits {
        assert_eq!(flag.bits(), bit, "{flag:?}");
    }
    assert_eq!(Flags::all().bits(), 0xff);
}

#[test]
fn p1_walk_03_a_wide_directory_is_read_in_batches_by_several_workers() {
    let mut tree = MemProvider::new("/mem");
    for i in 0..20_000 {
        tree.file(format!("wide/f{i}"), 10);
    }
    // Subdirectories that only the stat batches find are still entered.
    for i in 0..3 {
        tree.file(format!("wide/sub{i}/inside"), 1);
    }
    let provider = Splitting {
        inner: &tree,
        keep: 100,
        fail_stats: false,
        batches: Mutex::new(Vec::new()),
        gate: Gate::new(4),
    };
    let opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    let (result, events) = walk_events(&provider, tree.root_path(), &opts);
    let summary = result.unwrap();
    let (paths, _) = replay(tree.root_path(), &events);
    assert_eq!(
        sorted(paths.iter().map(PathBuf::as_path)),
        sorted(tree.paths())
    );
    assert_eq!(summary.entries, tree.entry_count() as u64);

    let batches = provider.batches.lock().unwrap();
    // wide/ leaves 19,903 names: 19 full batches of 1,024 and one of 447.
    assert_eq!(
        batches.len(),
        20,
        "{:?}",
        batches.iter().map(|b| b.0).collect::<Vec<_>>()
    );
    assert!(batches.iter().all(|&(len, _)| len <= STAT_BATCH));
    let threads: HashSet<ThreadId> = batches.iter().map(|&(_, thread)| thread).collect();
    assert_eq!(threads.len(), 4, "every worker read some of the batches");
}

#[test]
fn p1_walk_03_a_failed_stat_batch_records_every_name() {
    let mut tree = MemProvider::new("/mem");
    for i in 0..1_500 {
        tree.file(format!("f{i}"), 10);
    }
    let provider = Splitting {
        inner: &tree,
        keep: 0,
        fail_stats: true,
        batches: Mutex::new(Vec::new()),
        gate: Gate::new(1),
    };
    let opts = WalkOptions {
        threads: 2,
        ..WalkOptions::default()
    };
    let (result, events) = walk_events(&provider, tree.root_path(), &opts);
    let summary = result.unwrap();
    replay(tree.root_path(), &events);
    // Two batches, of 1,024 and 476 names, both failed: only the root was
    // read, and every name of both batches is an error.
    let batches = provider.batches.lock().unwrap();
    let mut sizes: Vec<usize> = batches.iter().map(|&(len, _)| len).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [476, 1_024]);
    assert_eq!(summary.entries, 1);
    assert_eq!(summary.entry_errors, 1_500);
    let named: HashSet<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            ScanEvent::Error(ScanError { errno: 2, name, .. }) => Some(name.as_bytes().to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(named.len(), 1_500);
    assert!((0..1_500).all(|i| named.contains(format!("f{i}").as_bytes())));
}

#[test]
fn p1_walk_03_stat_batches_hold_only_their_own_names() {
    let count: usize = 200_000;
    let later: Vec<NameRef> = (0..count)
        .map(|i| NameRef::from(format!("f{i}").as_str()))
        .collect();
    let batches: Vec<Vec<NameRef>> = stat_batches(later).collect();
    assert_eq!(batches.len(), count.div_ceil(STAT_BATCH));
    assert!(
        batches
            .iter()
            .all(|b| !b.is_empty() && b.capacity() <= STAT_BATCH)
    );
    assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), count);
    // The job Vecs together hold room for the names they carry, not the
    // tail of the listing each time: under 64 bytes per name.
    let held: usize = batches.iter().map(Vec::capacity).sum::<usize>() * size_of::<NameRef>();
    assert!(held <= 64 * count, "{held} bytes held for {count} names");
}
