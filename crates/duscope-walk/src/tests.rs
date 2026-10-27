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
            ScanEvent::Skipped { dir, .. } => {
                assert!(dirs.contains_key(dir), "{dir:?} skipped before it was seen");
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
                vanished: 0,
                skipped_dirs: 0,
                volumes: 1,
                mounts_excluded: 0,
                clones_unavailable: false,
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
        .unstatable("d/gone", libc::EIO);
    let (result, events) = walk_events(&tree, tree.root_path(), &WalkOptions::default());
    let (paths, dirs) = replay(tree.root_path(), &events);
    assert!(!paths.contains(&PathBuf::from("/mem/d/gone")));
    assert!(events.contains(&ScanEvent::Error(ScanError {
        dir: id_of(&dirs, "/mem/d"),
        name: NameRef::from("gone"),
        errno: libc::EIO,
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
    // The root's volume follows its entry (P1-WALK-06).
    let [ScanEvent::Entry(entry), ScanEvent::Volume(_)] = events.as_slice() else {
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
    // Two batches, of 1,024 and 476 names, both failed as if the directory
    // had vanished: only the root was read, and every name of both batches
    // is recorded as vanished.
    let batches = provider.batches.lock().unwrap();
    let mut sizes: Vec<usize> = batches.iter().map(|&(len, _)| len).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [476, 1_024]);
    assert_eq!(summary.entries, 1);
    assert_eq!(summary.vanished, 1_500);
    assert_eq!(summary.entry_errors, 0);
    let named: HashSet<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            ScanEvent::Vanished {
                name: Some(name), ..
            } => Some(name.as_bytes().to_vec()),
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

/// Skips every directory named `skipped`.
struct Skipping<'a> {
    inner: &'a MemProvider,
}

impl Provider for Skipping<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        assert!(
            !dir.path().ends_with("skipped"),
            "a skipped directory was read"
        );
        self.inner.read_dir(dir, opts, out, errs)
    }

    fn skip(&self, _dir: &DirHandle, entry: &Entry, _opts: &WalkOptions) -> Option<Skip> {
        (entry.name.as_bytes() == b"skipped").then_some(Skip::Firmlinked)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

#[test]
fn p1_walk_07_a_skipped_directory_is_listed_not_entered() {
    let mut tree = MemProvider::new("/mem");
    tree.file("a/skipped/inside", 10).file("a/kept/inside", 10);
    let provider = Skipping { inner: &tree };
    let (result, events) = walk_events(&provider, tree.root_path(), &WalkOptions::default());
    let summary = result.unwrap();
    let (paths, dirs) = replay(tree.root_path(), &events);
    assert!(paths.contains(&PathBuf::from("/mem/a/skipped")));
    assert!(!paths.contains(&PathBuf::from("/mem/a/skipped/inside")));
    assert!(paths.contains(&PathBuf::from("/mem/a/kept/inside")));
    assert_eq!(summary.skipped_dirs, 1);
    // Skipped, then closed with no error, right after its parent's entries.
    let skipped = id_of(&dirs, "/mem/a/skipped");
    let at = events
        .iter()
        .position(|e| {
            *e == ScanEvent::Skipped {
                dir: skipped,
                reason: Skip::Firmlinked,
            }
        })
        .unwrap();
    assert_eq!(
        events[at + 1],
        ScanEvent::DirClosed {
            dir: skipped,
            errno: None
        }
    );
    assert_eq!(Skip::Firmlinked.as_str(), "firmlinked");
}

/// A mount in [`Mounted`]'s table: its path, mount id and class.
struct MountRow {
    path: &'static str,
    mnt_id: u64,
    class: VolumeClass,
}

/// A [`MemProvider`] with mounts: each entry takes the mount id of the
/// longest mount path above it, a mount point carries `Boundary::Mount`, and
/// `bind_of` gives a mount point the identity of another directory, as a
/// bind mount does.
/// A mount table row.
fn row(path: &'static str, mnt_id: u64, class: VolumeClass) -> MountRow {
    MountRow {
        path,
        mnt_id,
        class,
    }
}

struct Mounted<'a> {
    inner: &'a MemProvider,
    mounts: Vec<MountRow>,
    bind_of: Option<(&'static str, &'static str)>,
    /// Paths that are firmlinks, and the one mount point the provider skips
    /// as reached through them.
    firmlinks: Vec<&'static str>,
    firmlinked: Option<&'static str>,
    /// Paths `volume` and `mount_info` were asked about.
    volume_calls: Mutex<Vec<PathBuf>>,
    mount_info_calls: Mutex<Vec<PathBuf>>,
}

impl<'a> Mounted<'a> {
    fn new(inner: &'a MemProvider, mounts: Vec<MountRow>) -> Mounted<'a> {
        Mounted {
            inner,
            mounts,
            bind_of: None,
            firmlinks: Vec::new(),
            firmlinked: None,
            volume_calls: Mutex::new(Vec::new()),
            mount_info_calls: Mutex::new(Vec::new()),
        }
    }
}

impl Mounted<'_> {
    fn mount_of(&self, path: &Path) -> Option<&MountRow> {
        self.mounts
            .iter()
            .filter(|m| path.starts_with(m.path))
            .max_by_key(|m| m.path.len())
    }

    fn fix(&self, path: &Path, entry: &mut Entry) {
        if let Some(mount) = self.mount_of(path) {
            entry.mnt_id = mount.mnt_id;
            if path == Path::new(mount.path) {
                entry.boundary = Boundary::Mount;
            }
        }
        if self.firmlinks.iter().any(|f| path == Path::new(f)) {
            entry.boundary = Boundary::Firmlink;
        }
        if let Some((point, target)) = self.bind_of
            && path == Path::new(point)
        {
            let target = self
                .inner
                .root(Path::new(target), &WalkOptions::default())
                .unwrap();
            (entry.dev, entry.ino) = (target.dev, target.ino);
        }
    }
}

impl Provider for Mounted<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let start = out.len();
        self.inner.read_dir(dir, opts, out, errs)?;
        for entry in &mut out[start..] {
            self.fix(&dir.path().join(entry.name.as_os_str()), entry);
        }
        Ok(())
    }

    fn skip(&self, _dir: &DirHandle, entry: &Entry, _opts: &WalkOptions) -> Option<Skip> {
        let firmlinked = self.firmlinked?;
        (entry.boundary == Boundary::Mount
            && self
                .mount_of(Path::new(firmlinked))
                .is_some_and(|m| m.mnt_id == entry.mnt_id))
        .then_some(Skip::Firmlinked)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.volume_calls.lock().unwrap().push(path.to_owned());
        let mut volume = self.inner.volume(path)?;
        if let Some(mount) = self.mount_of(path) {
            volume.mount_point = PathBuf::from(mount.path);
            volume.mnt_id = mount.mnt_id;
            volume.class = mount.class;
        }
        Ok(volume)
    }

    fn mount_info(&self, path: &Path, _dev: u64) -> io::Result<VolumeInfo> {
        self.mount_info_calls.lock().unwrap().push(path.to_owned());
        let mut volume = self.inner.volume(path)?;
        if let Some(mount) = self.mount_of(path) {
            volume.mount_point = PathBuf::from(mount.path);
            volume.class = mount.class;
            (volume.total_bytes, volume.free_bytes) = (0, 0);
        }
        Ok(volume)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let mut entry = self.inner.root(path, opts)?;
        self.fix(path, &mut entry);
        Ok(entry)
    }
}

/// What a walk of the mount tree read and reported.
struct MountWalk {
    paths: Vec<PathBuf>,
    volumes: Vec<VolumeInfo>,
    skips: Vec<(PathBuf, Skip)>,
    summary: WalkSummary,
}

/// A tree with a mount of each kind under /mem, walked with `opts`.
fn walk_mounts(opts: &WalkOptions) -> MountWalk {
    let mut tree = MemProvider::new("/mem");
    tree.file("disk/a", 1)
        .file("tmp/b", 1)
        .file("proc/c", 1)
        .file("nfs/d", 1)
        .file("plain/e", 1)
        .dir("disk/loop");
    let mut provider = Mounted::new(
        &tree,
        vec![
            row("/mem", 1, VolumeClass::Local),
            row("/mem/disk", 2, VolumeClass::Local),
            row("/mem/tmp", 3, VolumeClass::Memory),
            row("/mem/proc", 4, VolumeClass::Kernel),
            row("/mem/nfs", 5, VolumeClass::Network),
            row("/mem/disk/loop", 6, VolumeClass::Local),
        ],
    );
    // /mem bind-mounted on /mem/disk/loop: entering it would loop.
    provider.bind_of = Some(("/mem/disk/loop", "/mem"));
    let (result, events) = walk_events(&provider, tree.root_path(), opts);
    let summary = result.unwrap();
    let (paths, dirs) = replay(tree.root_path(), &events);
    let mut volumes = Vec::new();
    let mut skips = Vec::new();
    for event in events {
        match event {
            ScanEvent::Volume(volume) => volumes.push(volume),
            ScanEvent::Skipped { dir, reason } => skips.push((dirs[&dir].clone(), reason)),
            _ => {}
        }
    }
    skips.sort_by(|a, b| a.0.cmp(&b.0));
    MountWalk {
        paths,
        volumes,
        skips,
        summary,
    }
}

#[test]
fn p1_walk_06_mounts_are_crossed_by_default() {
    let MountWalk {
        paths,
        volumes,
        skips,
        summary,
    } = walk_mounts(&WalkOptions::default());
    assert!(paths.contains(&PathBuf::from("/mem/disk/a")));
    assert!(paths.contains(&PathBuf::from("/mem/tmp/b")));
    assert!(
        !paths.contains(&PathBuf::from("/mem/proc/c")),
        "kernel mounts are never entered"
    );
    assert!(
        !paths.contains(&PathBuf::from("/mem/nfs/d")),
        "nor network mounts by default"
    );
    assert_eq!(
        skips,
        [
            (PathBuf::from("/mem/disk/loop"), Skip::BindCycle),
            (PathBuf::from("/mem/nfs"), Skip::Network),
            (PathBuf::from("/mem/proc"), Skip::Kernel),
        ]
    );
    // The root's volume, then one per mount met.
    let ids: Vec<(u64, bool, Option<Skip>)> = volumes
        .iter()
        .map(|v| (v.mnt_id, v.entered(), v.reason))
        .collect();
    assert_eq!(ids[0], (1, true, None));
    let mut rest = ids[1..].to_vec();
    rest.sort_by_key(|v| v.0);
    assert_eq!(
        rest,
        [
            (2, true, None),
            (3, true, None),
            (4, false, Some(Skip::Kernel)),
            (5, false, Some(Skip::Network)),
            (6, false, Some(Skip::BindCycle)),
        ]
    );
    assert_eq!((summary.volumes, summary.mounts_excluded), (6, 3));
    assert_eq!(
        volumes.iter().find(|v| v.mnt_id == 3).unwrap().class,
        VolumeClass::Memory
    );
}

#[test]
fn p1_walk_06_include_network_enters_network_mounts() {
    let opts = WalkOptions {
        include_network: true,
        ..WalkOptions::default()
    };
    let MountWalk {
        paths,
        skips,
        summary,
        ..
    } = walk_mounts(&opts);
    assert!(paths.contains(&PathBuf::from("/mem/nfs/d")));
    assert!(!skips.iter().any(|(_, reason)| *reason == Skip::Network));
    assert!(
        skips.contains(&(PathBuf::from("/mem/proc"), Skip::Kernel)),
        "kernel mounts still not"
    );
    assert_eq!(
        summary.mounts_excluded, 2,
        "the kernel mount and the bind cycle"
    );
}

#[test]
fn p1_walk_06_one_file_system_stays_on_the_root_mount() {
    let opts = WalkOptions {
        one_file_system: true,
        ..WalkOptions::default()
    };
    let MountWalk {
        paths,
        skips,
        summary,
        ..
    } = walk_mounts(&opts);
    assert!(paths.contains(&PathBuf::from("/mem/plain/e")));
    for other in ["/mem/disk/a", "/mem/tmp/b"] {
        assert!(!paths.contains(&PathBuf::from(other)), "{other}");
    }
    assert!(skips.contains(&(PathBuf::from("/mem/disk"), Skip::OneFileSystem)));
    assert!(skips.contains(&(PathBuf::from("/mem/tmp"), Skip::OneFileSystem)));
    assert_eq!(summary.mounts_excluded, 4);
}

#[test]
fn p1_walk_06_skipped_mounts_are_never_asked_for_sizes() {
    let mut tree = MemProvider::new("/mem");
    tree.file("nfs/d", 1).file("proc/c", 1).file("disk/a", 1);
    let provider = Mounted::new(
        &tree,
        vec![
            row("/mem", 1, VolumeClass::Local),
            row("/mem/nfs", 5, VolumeClass::Network),
            row("/mem/proc", 4, VolumeClass::Kernel),
            row("/mem/disk", 2, VolumeClass::Local),
        ],
    );
    let (result, events) = walk_events(&provider, tree.root_path(), &WalkOptions::default());
    result.unwrap();
    // The root's and the entered disk's volumes are read in full; the
    // network and kernel mounts only from the mount table.
    let mut sized = provider.volume_calls.lock().unwrap().clone();
    sized.sort();
    assert_eq!(sized, [PathBuf::from("/mem"), PathBuf::from("/mem/disk")]);
    let info = provider.mount_info_calls.lock().unwrap().len();
    assert_eq!(
        info, 3,
        "each crossing classified once; a skipped volume's row reuses it"
    );
    let excluded: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            ScanEvent::Volume(v) if !v.entered() => Some(v.total_bytes),
            _ => None,
        })
        .collect();
    assert_eq!(excluded, [0, 0], "skipped volumes carry no sizes");
}

#[test]
fn p1_walk_06_a_same_device_bind_mount_is_still_a_cycle() {
    // A kernel before 5.8, or the fallback provider, reports a bind mount on
    // the same device with its parent's mount id and no mount boundary.
    let mut tree = MemProvider::new("/mem");
    tree.file("data/f", 1).dir("data/sub/loop");
    let mut provider = Mounted::new(&tree, vec![row("/mem", 1, VolumeClass::Local)]);
    provider.bind_of = Some(("/mem/data/sub/loop", "/mem/data"));
    let (result, events) = walk_events(&provider, tree.root_path(), &WalkOptions::default());
    let summary = result.unwrap();
    let (paths, dirs) = replay(tree.root_path(), &events);
    assert!(paths.contains(&PathBuf::from("/mem/data/sub/loop")));
    let skipped = id_of(&dirs, "/mem/data/sub/loop");
    assert!(events.contains(&ScanEvent::Skipped {
        dir: skipped,
        reason: Skip::BindCycle
    }));
    assert_eq!(
        summary.mounts_excluded, 0,
        "no mount was detected, so no volume row"
    );
}

#[test]
fn p1_walk_06_firmlinks_are_no_crossing_for_one_file_system() {
    // macOS: / on the System volume (1); /users a firmlink onto the Data
    // volume (2), also mounted at /sys/data, which the provider skips as
    // reached through the firmlinks.
    let mut tree = MemProvider::new("/mem");
    tree.file("users/me/notes", 1)
        .file("sys/data/users/me/notes", 1)
        .file("bin/ls", 1);
    let mut provider = Mounted::new(
        &tree,
        vec![
            row("/mem", 1, VolumeClass::Local),
            row("/mem/users", 2, VolumeClass::Local),
            row("/mem/sys/data", 2, VolumeClass::Local),
        ],
    );
    provider.firmlinks = vec!["/mem/users"];
    provider.firmlinked = Some("/mem/sys/data");
    for one_file_system in [false, true] {
        let opts = WalkOptions {
            one_file_system,
            ..WalkOptions::default()
        };
        let (result, events) = walk_events(&provider, tree.root_path(), &opts);
        let summary = result.unwrap();
        let (paths, _) = replay(tree.root_path(), &events);
        assert!(
            paths.contains(&PathBuf::from("/mem/users/me/notes")),
            "-x {one_file_system}"
        );
        assert!(!paths.contains(&PathBuf::from("/mem/sys/data/users/me/notes")));
        // The Data volume is reported once, entered, whichever of its two
        // paths the workers reached first.
        let data: Vec<bool> = events
            .iter()
            .filter_map(|e| match e {
                ScanEvent::Volume(v) if v.mnt_id == 2 => Some(v.entered()),
                _ => None,
            })
            .collect();
        assert_eq!(data, [true], "-x {one_file_system}");
        assert_eq!(summary.mounts_excluded, 0);
    }
}

#[test]
fn p1_walk_06_a_handle_walks_back_to_the_root() {
    let root = Arc::new(DirHandle::detached("/r", 0));
    let entry = MemProvider::new("/r")
        .root(Path::new("/r"), &WalkOptions::default())
        .unwrap();
    let child = DirHandle::new(
        DirId::new(1),
        PathBuf::from("/r/a"),
        &entry,
        Some(Arc::clone(&root)),
    );
    let paths: Vec<&Path> = child.ancestors().map(DirHandle::path).collect();
    assert_eq!(paths, [Path::new("/r/a"), Path::new("/r")]);
    assert_eq!(
        (child.depth(), child.parent().map(DirHandle::depth)),
        (1, Some(0))
    );
}
