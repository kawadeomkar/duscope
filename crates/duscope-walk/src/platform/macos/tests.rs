//! P1-WALK-03: the macOS provider against `lstat`, its stat queue against
//! its bulk reads, and the record decoder on hand-packed records.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use duscope_testkit::TestEnv;

use super::MacProvider;
use super::bulk::{self, Decoded, Outcome, Probe, Request};
use super::ffi;
use crate::platform::posix;
use crate::{Boundary, Entry, Flags, Kind, ScanEvent, Share, WalkOptions, walk_with};

/// Every entry of a walk by its path, and the paths of the directories that
/// closed with an error.
fn walk_paths(
    provider: &MacProvider,
    root: &Path,
    opts: &WalkOptions,
) -> (BTreeMap<PathBuf, Entry>, Vec<PathBuf>) {
    let mut dirs: BTreeMap<crate::DirId, PathBuf> = BTreeMap::new();
    let mut entries = BTreeMap::new();
    let mut unreadable = Vec::new();
    walk_with(provider, root, opts, |event| match event {
        ScanEvent::Entry(entry) => {
            let path = if entry.dir == Some(crate::DirId::ROOT) {
                root.to_owned()
            } else {
                dirs[&entry.parent].join(entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            entries.insert(path, entry);
        }
        ScanEvent::DirClosed {
            dir,
            errno: Some(_),
        } => unreadable.push(dirs[&dir].clone()),
        _ => {}
    })
    .unwrap();
    (entries, unreadable)
}

fn lstat(path: &Path) -> libc::stat {
    posix::lstat(&CString::new(path.as_os_str().as_bytes()).unwrap()).unwrap()
}

#[test]
fn p1_walk_03_every_entry_matches_lstat() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let (entries, _) = walk_paths(&MacProvider::new(), &root, &WalkOptions::default());
    assert!(
        entries.len() > 100_000,
        "the fixture tree was walked: {}",
        entries.len()
    );
    for (path, entry) in &entries {
        let st = lstat(path);
        let kind = super::stat::from_stat(entry.name.clone(), &st, crate::Attrs::Basic).kind;
        assert_eq!(entry.kind, kind, "{path:?}");
        let allocated = match kind {
            Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block => 0,
            _ => u64::try_from(st.st_blocks).unwrap() * 512,
        };
        assert_eq!(entry.allocated, allocated, "allocated {path:?}");
        assert_eq!(
            entry.apparent,
            u64::try_from(st.st_size).unwrap(),
            "apparent {path:?}"
        );
        let ns = |s: i64, n: i64| s * 1_000_000_000 + n;
        assert_eq!(
            entry.mtime_ns,
            ns(st.st_mtime, st.st_mtime_nsec),
            "mtime {path:?}"
        );
        assert_eq!(
            entry.ctime_ns,
            ns(st.st_ctime, st.st_ctime_nsec),
            "ctime {path:?}"
        );
        assert_eq!(entry.dev, bulk::dev(st.st_dev), "dev {path:?}");
        assert_eq!(entry.mnt_id, entry.dev, "mnt_id {path:?}");
        assert_eq!(entry.ino, st.st_ino, "ino {path:?}");
        if kind != Kind::Dir {
            assert_eq!(entry.nlink, u32::from(st.st_nlink), "nlink {path:?}");
        }
        assert_eq!(
            entry.flags.contains(Flags::DATALESS),
            st.st_flags & ffi::SF_DATALESS != 0,
            "{path:?}"
        );
        assert_eq!(entry.boundary, Boundary::None, "{path:?}");
    }
}

/// The walk of the fixture tree read three ways: by bulk reads, through the
/// stat queue as if every directory were I/O-bound, and through the
/// fallback provider as if every bulk read failed with `ENOTSUP`.
fn same_through_the_stat_queue(pretend: Outcome) {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    let (bulk, mut bulk_unreadable) = walk_paths(&MacProvider::new(), &root, &opts);
    let queued = MacProvider {
        pretend: Some(pretend),
        ..MacProvider::default()
    };
    let (queue, mut queue_unreadable) = walk_paths(&queued, &root, &opts);
    bulk_unreadable.sort();
    queue_unreadable.sort();
    if env.fixtures().unreadable_effective() {
        assert_eq!(
            bulk_unreadable.len(),
            2,
            "closed/ and empty-closed/: {bulk_unreadable:?}"
        );
    }
    assert_eq!(bulk.len(), queue.len());
    // Directory ids follow the order the workers found directories in. The
    // fallback provider reports a directory's st_nlink, which counts its
    // subdirectories; ATTR_DIR_LINKCOUNT does not.
    let unnumbered = |entry: &Entry| {
        let mut entry = Entry {
            parent: crate::DirId::ROOT,
            dir: entry.dir.map(|_| crate::DirId::ROOT),
            ..entry.clone()
        };
        if pretend == Outcome::Unsupported {
            if entry.kind == Kind::Dir {
                entry.nlink = 0;
            }
            // The fallback reads no clone ids or extended flags.
            entry
                .flags
                .remove(Flags::SPARSE | Flags::PURGEABLE | Flags::MAY_SHARE | Flags::SHARES_ALL);
            if matches!(entry.share, Share::Clone(_) | Share::MayShare) {
                entry.share = if entry.nlink > 1 {
                    Share::Hardlink
                } else {
                    Share::None
                };
            }
        }
        entry
    };
    for (path, entry) in &bulk {
        assert_eq!(
            Some(unnumbered(entry)),
            queue.get(path).map(unnumbered),
            "{path:?}"
        );
    }
    assert_eq!(bulk_unreadable, queue_unreadable);
}

#[test]
fn p1_walk_03_the_stat_queue_reads_what_bulk_reads_read() {
    same_through_the_stat_queue(Outcome::IoBound);
}

#[test]
fn p1_walk_03_enotsup_rereads_the_directory_through_the_fallback() {
    same_through_the_stat_queue(Outcome::Unsupported);
}

#[test]
fn p1_walk_03_unsupported_errors_are_recognised() {
    for errno in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS] {
        assert!(
            bulk::unsupported(&std::io::Error::from_raw_os_error(errno)),
            "{errno}"
        );
    }
    for errno in [libc::EACCES, libc::ENOENT, libc::EIO] {
        assert!(
            !bulk::unsupported(&std::io::Error::from_raw_os_error(errno)),
            "{errno}"
        );
    }
}

#[test]
fn p1_walk_03_dataless_files_are_never_materialised() {
    let _ = MacProvider::new();
    assert_eq!(
        ffi::dataless_policy().unwrap(),
        ffi::IOPOL_MATERIALIZE_DATALESS_FILES_OFF
    );
    // An evicted iCloud file, as a bulk read reports it: its logical length
    // and no blocks. The entry comes from the record alone, flagged dataless.
    let request = Request::bulk(&WalkOptions::default());
    let record = Packed::file(b"report.pdf", 1 << 20, 0)
        .flags(ffi::SF_DATALESS)
        .bytes(&request);
    let Decoded::Entry(entry) = bulk::parse(&record, &request).unwrap().decode(&request) else {
        panic!("a complete record decodes to an entry");
    };
    assert_eq!((entry.apparent, entry.allocated), (1 << 20, 0));
    assert!(entry.flags.contains(Flags::DATALESS));
}

#[test]
fn p1_walk_03_records_decode_to_entries() {
    let request = Request::bulk(&WalkOptions::default());
    let record = Packed::file(b"a.txt", 1000, 4096).links(3).bytes(&request);
    let Decoded::Entry(file) = bulk::parse(&record, &request).unwrap().decode(&request) else {
        panic!("not an entry");
    };
    assert_eq!(file.name.as_bytes(), b"a.txt");
    assert_eq!(
        (file.kind, file.apparent, file.allocated, file.nlink),
        (Kind::File, 1000, 4096, 3)
    );
    assert_eq!(file.share, crate::Share::Hardlink);
    assert_eq!(
        (file.mtime_ns, file.ctime_ns),
        (1_500_000_000_000_000_007, -1_000_000_001)
    );
    assert_eq!((file.dev, file.ino), (u64::MAX, 77));

    // Allocation rounds up to whole 512-byte blocks, as st_blocks does.
    let record = Packed::file(b"odd", 10, 700).bytes(&request);
    let Decoded::Entry(odd) = bulk::parse(&record, &request).unwrap().decode(&request) else {
        panic!("not an entry");
    };
    assert_eq!(odd.allocated, 1024);

    let record = Packed::dir(b"sub", 0).bytes(&request);
    let Decoded::Entry(dir) = bulk::parse(&record, &request).unwrap().decode(&request) else {
        panic!("not an entry");
    };
    assert_eq!(
        (dir.kind, dir.apparent, dir.allocated, dir.share),
        (Kind::Dir, 96, 0, crate::Share::None)
    );

    let record = Packed::dir(b"mnt", libc::DIR_MNTSTATUS_MNTPOINT).bytes(&request);
    assert!(matches!(
        bulk::parse(&record, &request).unwrap().decode(&request),
        Decoded::Restat(name, Some(Boundary::Mount)) if name.as_bytes() == b"mnt"
    ));

    let record = Packed::file(b"gone", 0, 0)
        .error(libc::ENOENT)
        .bytes(&request);
    assert!(matches!(
        bulk::parse(&record, &request).unwrap().decode(&request),
        Decoded::Error(name, libc::ENOENT) if name.as_bytes() == b"gone"
    ));

    // A filesystem that leaves out the allocation: read again by name.
    let record = Packed::file(b"partial", 1, 1)
        .without_file(libc::ATTR_FILE_ALLOCSIZE)
        .bytes(&request);
    assert!(matches!(
        bulk::parse(&record, &request).unwrap().decode(&request),
        Decoded::Restat(_, None)
    ));

    // A filesystem that leaves ATTR_CMN_ERROR out of an error-free record.
    let record = Packed::file(b"plain", 5, 512)
        .no_error_field()
        .bytes(&request);
    let Decoded::Entry(plain) = bulk::parse(&record, &request).unwrap().decode(&request) else {
        panic!("not an entry");
    };
    assert_eq!((plain.apparent, plain.allocated), (5, 512));

    // Cut short: no record.
    let record = Packed::file(b"short", 1, 1).bytes(&request);
    assert!(bulk::parse(&record[..40], &request).is_none());
}

#[test]
fn p1_walk_03_the_probe_needs_two_waiting_refills() {
    let ms = Duration::from_millis;
    let mut probe = Probe::default();
    assert!(!probe.sample(Some(ms(1)), ms(10)));
    assert!(probe.sample(Some(ms(2)), ms(10)));

    // A refill that ran on the CPU ends the probe.
    let mut probe = Probe::default();
    assert!(!probe.sample(Some(ms(9)), ms(10)));
    assert!(!probe.active());
    assert!(!probe.sample(Some(ms(1)), ms(10)));

    // So does a clock that cannot be read or reads zero.
    for cpu in [None, Some(Duration::ZERO)] {
        let mut probe = Probe::default();
        assert!(!probe.sample(cpu, ms(10)));
        assert!(!probe.active());
    }
}

#[test]
fn p1_walk_03_the_root_is_read_by_path() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("special-files");
    let provider = MacProvider::new();
    let opts = WalkOptions::default();
    let entry = crate::Provider::root(&provider, &root, &opts).unwrap();
    assert_eq!(entry.kind, Kind::Dir);
    assert_eq!(entry.name.as_os_str(), root.file_name().unwrap());
    assert_eq!(entry.ino, lstat(&root).st_ino);
    let slash = crate::Provider::root(&provider, Path::new("/"), &opts).unwrap();
    assert_eq!(slash.name.as_bytes(), b"/");
}

#[test]
fn p1_walk_03_the_volume_comes_from_statfs() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let volume = crate::Provider::volume(&MacProvider::new(), &root).unwrap();
    assert_eq!(volume.dev, bulk::dev(lstat(&root).st_dev));
    assert!(volume.total_bytes >= volume.free_bytes && volume.free_bytes > 0);
    assert!(!volume.fs_type.is_empty() && volume.fs_type == volume.fs_type.to_lowercase());
    // The fixture tree may sit behind a firmlink (/Users is one), so its
    // path need not start with the mount point.
    assert!(volume.mount_point.is_absolute());
}

/// A record packed the way `getattrlistbulk` packs it for `Request::bulk`
/// under `FSOPT_PACK_INVAL_ATTRS`.
struct Packed {
    name: &'static [u8],
    objtype: u32,
    error: u32,
    flags: u32,
    links: u32,
    mount_status: u32,
    alloc: u64,
    len: u64,
    missing_file: libc::attrgroup_t,
    /// Leave `ATTR_CMN_ERROR` out of an error-free record's returned set,
    /// as a filesystem may; the kernels tested return it with a zero value.
    no_error_field: bool,
}

impl Packed {
    fn file(name: &'static [u8], len: u64, alloc: u64) -> Packed {
        Packed {
            name,
            objtype: ffi::VREG,
            error: 0,
            flags: 0,
            links: 1,
            mount_status: 0,
            alloc,
            len,
            missing_file: 0,
            no_error_field: false,
        }
    }

    fn dir(name: &'static [u8], mount_status: u32) -> Packed {
        Packed {
            objtype: ffi::VDIR,
            mount_status,
            len: 96,
            ..Packed::file(name, 0, 0)
        }
    }

    fn flags(mut self, flags: u32) -> Packed {
        self.flags = flags;
        self
    }

    fn links(mut self, links: u32) -> Packed {
        self.links = links;
        self
    }

    fn error(mut self, errno: i32) -> Packed {
        self.error = u32::try_from(errno).unwrap();
        self
    }

    fn no_error_field(mut self) -> Packed {
        self.no_error_field = true;
        self
    }

    fn without_file(mut self, attrs: libc::attrgroup_t) -> Packed {
        self.missing_file = attrs;
        self
    }

    fn bytes(&self, request: &Request) -> Vec<u8> {
        let attrs = &request.attrs;
        let dir = self.objtype == ffi::VDIR;
        let error_field = self.error != 0 || !self.no_error_field;
        let common = if error_field {
            attrs.commonattr | ffi::ATTR_CMN_ERROR
        } else {
            attrs.commonattr & !ffi::ATTR_CMN_ERROR
        };
        let returned = [
            common,
            0,
            if dir { attrs.dirattr } else { 0 },
            if dir {
                0
            } else {
                attrs.fileattr & !self.missing_file
            },
            attrs.forkattr,
        ];
        let mut fixed = Vec::new();
        for group in returned {
            fixed.extend(group.to_ne_bytes());
        }
        if error_field {
            fixed.extend(self.error.to_ne_bytes());
        }
        // The name's attrreference; its offset is patched below.
        let name_ref = fixed.len();
        fixed.extend([0u8; 8]);
        if self.error != 0 {
            // An error record may end after the name.
            return self.finish(fixed, name_ref);
        }
        fixed.extend((-1i32).to_ne_bytes()); // dev: -1 sign-extends to u64::MAX
        fixed.extend(self.objtype.to_ne_bytes());
        fixed.extend(1_500_000_000i64.to_ne_bytes()); // mtime
        fixed.extend(7i64.to_ne_bytes());
        fixed.extend((-1i64).to_ne_bytes()); // ctime: before 1970
        fixed.extend((-1i64).to_ne_bytes());
        fixed.extend(self.flags.to_ne_bytes());
        fixed.extend(77u64.to_ne_bytes()); // ino
        if self.error == 0 {
            if dir {
                fixed.extend(self.links.to_ne_bytes());
                fixed.extend(self.mount_status.to_ne_bytes());
                fixed.extend(self.alloc.to_ne_bytes());
                fixed.extend(self.len.to_ne_bytes());
            } else {
                fixed.extend(self.links.to_ne_bytes());
                fixed.extend(self.alloc.to_ne_bytes());
                fixed.extend(self.len.to_ne_bytes());
                fixed.extend(self.alloc.to_ne_bytes()); // data alloc
            }
            if attrs.forkattr != 0 {
                fixed.extend(0u64.to_ne_bytes());
                fixed.extend(0u64.to_ne_bytes());
            }
        }
        self.finish(fixed, name_ref)
    }

    /// Appends the name, pads to 8 bytes and prepends the length.
    fn finish(&self, mut fixed: Vec<u8>, name_ref: usize) -> Vec<u8> {
        // The length comes first; the reference's offset counts from itself.
        let name_at = 4 + fixed.len();
        let offset = i32::try_from(name_at - (4 + name_ref)).unwrap();
        let name_len = u32::try_from(self.name.len() + 1).unwrap();
        fixed[name_ref..name_ref + 4].copy_from_slice(&offset.to_ne_bytes());
        fixed[name_ref + 4..name_ref + 8].copy_from_slice(&name_len.to_ne_bytes());
        fixed.extend(self.name);
        fixed.push(0);
        while (fixed.len() + 4) % 8 != 0 {
            fixed.push(0);
        }
        let mut record = u32::try_from(fixed.len() + 4)
            .unwrap()
            .to_ne_bytes()
            .to_vec();
        record.extend(fixed);
        record
    }
}

#[test]
fn p1_walk_03_an_unsearchable_directory_lists_its_children_as_errors() {
    let env = TestEnv::new();
    if !env.fixtures().unreadable_effective() {
        return;
    }
    let root = env.fixtures().case_root("permissions");
    let mut errors = Vec::new();
    let mut closed_with = Vec::new();
    walk_with(
        &MacProvider::new(),
        &root,
        &WalkOptions::default(),
        |event| match event {
            ScanEvent::Error(error) => errors.push((error.name.as_bytes().to_vec(), error.errno)),
            ScanEvent::DirClosed {
                errno: Some(errno), ..
            } => closed_with.push(errno),
            _ => {}
        },
    )
    .unwrap();
    errors.sort();
    let eacces = |name: &[u8]| (name.to_vec(), libc::EACCES);
    assert_eq!(
        errors,
        [eacces(b"x.txt"), eacces(b"y.txt"), eacces(b"z.txt")]
    );
    // empty-closed/ (mode 000) cannot be opened at all.
    assert_eq!(closed_with, [libc::EACCES]);
}

/// Counts the calls the wrapped provider makes on the walk's threads:
/// (bulk reads, reads of one entry).
struct Counting<'a> {
    inner: &'a MacProvider,
    calls: std::sync::Mutex<(usize, usize)>,
}

impl Counting<'_> {
    fn count<T>(&self, call: impl FnOnce() -> T) -> T {
        let before = posix::calls::now();
        let result = call();
        let after = posix::calls::now();
        let mut calls = self.calls.lock().unwrap();
        calls.0 += after.0 - before.0;
        calls.1 += after.1 - before.1;
        result
    }
}

impl crate::Provider for Counting<'_> {
    fn read_dir(
        &self,
        dir: &crate::DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<crate::ScanError>,
    ) -> std::io::Result<()> {
        self.count(|| self.inner.read_dir(dir, opts, out, errs))
    }

    fn read_dir_split(
        &self,
        dir: &crate::DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<crate::ScanError>,
        later: &mut Vec<crate::NameRef>,
    ) -> std::io::Result<()> {
        self.count(|| self.inner.read_dir_split(dir, opts, out, errs, later))
    }

    fn stat_names(
        &self,
        dir: &crate::DirHandle,
        names: &[crate::NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<crate::ScanError>,
    ) -> std::io::Result<()> {
        self.count(|| self.inner.stat_names(dir, names, opts, out, errs))
    }

    fn volume(&self, path: &Path) -> std::io::Result<crate::VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> std::io::Result<Entry> {
        self.count(|| self.inner.root(path, opts))
    }
}

#[test]
fn p1_walk_05_full_attrs_match_lstat() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let opts = WalkOptions {
        attrs: crate::Attrs::Full,
        ..WalkOptions::default()
    };
    let (entries, _) = walk_paths(&MacProvider::new(), &root, &opts);
    let ns = |s: i64, n: i64| s * 1_000_000_000 + n;
    for (path, entry) in &entries {
        let st = lstat(path);
        let full = entry
            .full
            .unwrap_or_else(|| panic!("no full attributes: {path:?}"));
        assert_eq!(full.mode, u32::from(st.st_mode), "mode {path:?}");
        assert_eq!(
            (full.uid, full.gid),
            (st.st_uid, st.st_gid),
            "owner {path:?}"
        );
        assert_eq!(
            full.atime_ns,
            ns(st.st_atime, st.st_atime_nsec),
            "atime {path:?}"
        );
        assert_eq!(
            full.btime_ns,
            Some(ns(st.st_birthtime, st.st_birthtime_nsec)),
            "btime {path:?}"
        );
    }
    let (basic, _) = walk_paths(&MacProvider::new(), &root, &WalkOptions::default());
    assert!(basic.values().all(|entry| entry.full.is_none()));
}

#[test]
fn p1_walk_05_full_attrs_add_no_calls_per_entry() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let count = |attrs| {
        let provider = Counting {
            inner: &MacProvider::new(),
            calls: std::sync::Mutex::new((0, 0)),
        };
        // One worker: the probe, which can send a directory to the stat
        // queue depending on how busy the machine is, runs only with more.
        let opts = WalkOptions {
            attrs,
            threads: 1,
            ..WalkOptions::default()
        };
        walk_with(&provider, &root, &opts, |_| {}).unwrap();
        provider.calls.into_inner().unwrap()
    };
    let (basic_bulk, basic_entry) = count(crate::Attrs::Basic);
    let (full_bulk, full_entry) = count(crate::Attrs::Full);
    // The same reads of single entries (the root, mount points, records
    // that lack an attribute). Records grow by 44 bytes under --attrs full,
    // so a big directory takes a few more bulk refills of the same 64 KiB
    // buffer, and no call of any other kind.
    assert_eq!(full_entry, basic_entry);
    assert!(
        basic_bulk <= full_bulk && full_bulk * 2 <= basic_bulk * 3,
        "bulk reads: {basic_bulk} basic, {full_bulk} full"
    );
}

#[test]
fn p1_walk_07_clones_carry_equal_clone_ids() {
    let env = TestEnv::new();
    if !env.fixtures().built("clones") {
        return;
    }
    let root = env.fixtures().case_root("clones");
    let (entries, _) = walk_paths(&MacProvider::new(), &root, &WalkOptions::default());
    let share = |rel: &str| entries[&root.join(rel)].share;
    let Share::Clone(id) = share("original.img") else {
        panic!("not a clone: {:?}", share("original.img"));
    };
    assert_eq!(share("clone-1.img"), Share::Clone(id));
    assert_eq!(share("nested/clone-2.img"), Share::Clone(id));
    for rel in ["original.img", "clone-1.img", "nested/clone-2.img"] {
        assert!(
            entries[&root.join(rel)].flags.contains(Flags::MAY_SHARE),
            "{rel}"
        );
    }

    // Without --clones the attributes are not asked for.
    let opts = WalkOptions {
        clones: false,
        ..WalkOptions::default()
    };
    let (plain, _) = walk_paths(&MacProvider::new(), &root, &opts);
    let original = &plain[&root.join("original.img")];
    assert_eq!(original.share, Share::None);
    assert!(!original.flags.contains(Flags::MAY_SHARE));
}

#[test]
fn p1_walk_07_storage_flags_follow_the_filesystem() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let (entries, _) = walk_paths(
        &MacProvider::new(),
        &fixtures.root(),
        &WalkOptions::default(),
    );
    if fixtures.built("compressed") {
        let file = &entries[&fixtures.case_root("compressed").join("text-1m.txt")];
        assert!(file.flags.contains(Flags::COMPRESSED), "{:?}", file.flags);
    }
    if fixtures.built("sparse") {
        let file = &entries[&fixtures.case_root("sparse").join("hole-only.bin")];
        assert!(file.flags.contains(Flags::SPARSE), "{:?}", file.flags);
    }
    // Each flag agrees with st_flags, where stat reports it.
    for (path, entry) in &entries {
        let st = lstat(path);
        assert_eq!(
            entry.flags.contains(Flags::COMPRESSED),
            st.st_flags & libc::UF_COMPRESSED != 0,
            "{path:?}"
        );
    }
}

#[test]
fn p1_walk_07_private_size_under_the_hidden_option() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("clones");
    let opts = WalkOptions {
        attrs: crate::Attrs::Full,
        private_size: true,
        ..WalkOptions::default()
    };
    let (entries, _) = walk_paths(&MacProvider::new(), &root, &opts);
    let file = &entries[&root.join("original.img")];
    assert!(file.full.unwrap().private_size.is_some());
    let opts = WalkOptions {
        attrs: crate::Attrs::Full,
        ..WalkOptions::default()
    };
    let (entries, _) = walk_paths(&MacProvider::new(), &root, &opts);
    assert_eq!(
        entries[&root.join("original.img")]
            .full
            .unwrap()
            .private_size,
        None
    );
}

#[test]
fn p1_walk_07_apfs_has_clone_ids() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let summary = walk_with(&MacProvider::new(), &root, &WalkOptions::default(), |_| {}).unwrap();
    let apfs = volume_of(&root).fs_type == "apfs";
    assert_eq!(summary.clones_unavailable, !apfs);
    let opts = WalkOptions {
        clones: false,
        ..WalkOptions::default()
    };
    let summary = walk_with(&MacProvider::new(), &root, &opts, |_| {}).unwrap();
    assert!(!summary.clones_unavailable, "only asked of --clones");
}

fn volume_of(path: &Path) -> crate::VolumeInfo {
    crate::Provider::volume(&MacProvider::new(), path).unwrap()
}

/// The entries of one directory, read by the provider.
fn read_one(path: &str, depth: u32) -> Vec<Entry> {
    let dir = crate::DirHandle {
        id: crate::DirId::ROOT,
        path: PathBuf::from(path),
        depth,
    };
    let (mut out, mut errs) = (Vec::new(), Vec::new());
    crate::Provider::read_dir(
        &MacProvider::new(),
        &dir,
        &WalkOptions::default(),
        &mut out,
        &mut errs,
    )
    .unwrap();
    out
}

#[test]
fn p1_walk_07_firmlinks_carry_the_firmlink_boundary() {
    let firmlinks = super::firmlinks();
    if firmlinks.is_empty() {
        return;
    }
    let slash = read_one("/", 0);
    let users = slash
        .iter()
        .find(|e| e.name.as_bytes() == b"Users")
        .unwrap();
    assert_eq!(users.boundary, Boundary::Firmlink);
    assert_eq!(users.kind, Kind::Dir);
    // Read by name, the firmlink is the directory it shows: on the Data
    // volume, not the System volume holding /.
    assert_eq!(users.dev, bulk::dev(lstat(Path::new("/Users")).st_dev));
    assert_eq!(Some(users.dev), super::data_volume());
    let system = slash
        .iter()
        .find(|e| e.name.as_bytes() == b"System")
        .unwrap();
    assert_eq!(system.boundary, Boundary::None);
}

#[test]
fn p1_walk_07_a_scan_of_slash_skips_the_data_volume_mount() {
    if super::data_volume().is_none() || !Path::new("/System/Volumes/Data").exists() {
        return;
    }
    let provider = MacProvider::new();
    let opts = WalkOptions::default();
    let skip = |root_depth: u32| {
        // /System/Volumes, read in a scan rooted `root_depth` levels up.
        let dir = crate::DirHandle {
            id: crate::DirId::ROOT,
            path: PathBuf::from("/System/Volumes"),
            depth: root_depth,
        };
        let data = read_one("/System/Volumes", root_depth)
            .into_iter()
            .find(|e| e.name.as_bytes() == b"Data")
            .unwrap();
        assert_eq!(data.boundary, Boundary::Mount);
        crate::Provider::skip(&provider, &dir, &data, &opts)
    };
    // Rooted at /: every firmlink lies under the root.
    assert_eq!(skip(2), Some(crate::Skip::Firmlinked));
    // Rooted at /System/Volumes or /System: the volume is the only way in.
    assert_eq!(skip(0), None);
    assert_eq!(skip(1), None);
}

/// A scan of `/` on the machine running the test: `/Users` is counted once,
/// through its firmlink, and `/System/Volumes/Data` is listed but not
/// entered. It reads the whole disk, so it runs only with
/// `DUSCOPE_TEST_PLATFORM=1` (`just check-full`).
#[test]
#[ignore = "reads the whole disk; DUSCOPE_TEST_PLATFORM=1"]
fn p1_walk_07_a_scan_of_slash_counts_users_once() {
    let env = TestEnv::new();
    let mut dirs: BTreeMap<crate::DirId, PathBuf> = BTreeMap::new();
    let (mut users, mut under_data, mut skipped) = (0u64, 0u64, Vec::new());
    let mut seen = std::collections::HashSet::new();
    walk_with(
        &MacProvider::new(),
        Path::new("/"),
        &WalkOptions::default(),
        |event| match event {
            ScanEvent::Entry(entry) => {
                let path = if entry.dir == Some(crate::DirId::ROOT) {
                    PathBuf::from("/")
                } else {
                    dirs[&entry.parent].join(entry.name.as_os_str())
                };
                if let Some(id) = entry.dir {
                    dirs.insert(id, path.clone());
                }
                let first = entry.kind == Kind::Dir
                    || entry.nlink < 2
                    || seen.insert((entry.dev, entry.ino));
                if path.starts_with("/Users") && first {
                    users += entry.allocated;
                }
                // Path::starts_with compares components: count what lies
                // below the mount point, not the mount point itself.
                if path.starts_with("/System/Volumes/Data")
                    && path != Path::new("/System/Volumes/Data")
                {
                    under_data += 1;
                }
            }
            ScanEvent::Skipped { dir, reason } => skipped.push((dirs[&dir].clone(), reason)),
            _ => {}
        },
    )
    .unwrap();
    assert_eq!(
        under_data, 0,
        "nothing under the Data volume's mount point is read"
    );
    assert!(skipped.contains(&(
        PathBuf::from("/System/Volumes/Data"),
        crate::Skip::Firmlinked
    )));
    let out = env.command("du").args(["-skx", "/Users"]).output().unwrap();
    let du: u64 = String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let ours = users.div_ceil(1024);
    // /Users changes while it is read; the two agree to within 2%.
    assert!(
        ours.abs_diff(du) * 50 <= du,
        "/Users: {ours} KiB read, du -skx says {du} KiB"
    );
}

#[test]
fn p1_walk_05_no_creation_time_is_none_through_lstat_too() {
    // devfs keeps no creation time: the bulk read leaves ATTR_CMN_CRTIME
    // out, and lstat reports 0.
    let opts = WalkOptions {
        attrs: crate::Attrs::Full,
        ..WalkOptions::default()
    };
    let null = Path::new("/dev/null");
    let st = lstat(null);
    if st.st_birthtime != 0 {
        return;
    }
    let entry = super::stat::from_stat(crate::NameRef::from("null"), &st, opts.attrs);
    assert_eq!(entry.full.unwrap().btime_ns, None);
    let root = crate::Provider::root(&MacProvider::new(), null, &opts).unwrap();
    assert_eq!(root.full.unwrap().btime_ns, None, "the native read agrees");
}

#[test]
fn p1_walk_07_a_modified_clone_may_share_but_names_no_group() {
    let env = TestEnv::new();
    let dir = env.tmp_dir().join("clones");
    std::fs::create_dir_all(&dir).unwrap();
    let original = dir.join("original.bin");
    std::fs::write(&original, vec![7u8; 1 << 20]).unwrap();
    let cp = |name: &str| {
        let out = env
            .command("cp")
            .arg("-c")
            .arg(&original)
            .arg(dir.join(name))
            .output()
            .unwrap();
        out.status.success()
    };
    if !cp("pure.bin") || !cp("partial.bin") {
        return; // not APFS
    }
    // Overwrite the first 64 KiB of one clone in place.
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("partial.bin"))
            .unwrap();
        file.write_all(&vec![9u8; 64 * 1024]).unwrap();
        file.sync_all().unwrap();
    }
    let (entries, _) = walk_paths(&MacProvider::new(), &dir, &WalkOptions::default());
    let share = |name: &str| entries[&dir.join(name)].share;
    let Share::Clone(id) = share("original.bin") else {
        panic!("{:?}", share("original.bin"));
    };
    assert_eq!(share("pure.bin"), Share::Clone(id));
    assert_eq!(share("partial.bin"), Share::MayShare);
    assert!(
        entries[&dir.join("partial.bin")]
            .flags
            .contains(Flags::MAY_SHARE)
    );
}
