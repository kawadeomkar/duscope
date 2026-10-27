//! P1-WALK-03: the macOS provider against `lstat`, its stat queue against
//! its bulk reads, and the record decoder on hand-packed records.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;

use super::bulk::{self, Decoded, Outcome, Probe, Request};
use super::{MacProvider, ffi, stat};
use crate::platform::posix;
use crate::{Boundary, DirHandle, DirId, Entry, Flags, Kind, ScanEvent, WalkOptions, walk_with};

/// A clock on which every bulk refill waits on I/O: each reading moves the
/// wall clock 10 ms and the CPU clock 1 µs.
pub(super) struct Waiting;

static TICKS: AtomicU64 = AtomicU64::new(1);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

impl bulk::Clock for Waiting {
    fn cpu(&self) -> Option<Duration> {
        Some(Duration::from_micros(TICKS.fetch_add(1, Ordering::Relaxed)))
    }

    fn now(&self) -> Instant {
        *EPOCH + Duration::from_millis(10 * TICKS.fetch_add(1, Ordering::Relaxed))
    }
}

/// Every entry of a walk by its path, and the sorted paths of the
/// directories that closed with an error.
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
    // Directories close in the order the workers finish them.
    unreadable.sort();
    (entries, unreadable)
}

fn lstat(path: &Path) -> libc::stat {
    ffi::lstat(&CString::new(path.as_os_str().as_bytes()).unwrap()).unwrap()
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
        let kind = super::stat::from_stat(entry.name.clone(), &st).kind;
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

/// The walk of the fixture tree read by bulk reads and by `queued`, which
/// sends some or all directories through the stat queue: both must find the
/// same entries.
fn same_as_bulk_reads(queued: &MacProvider) {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    let (bulk, bulk_unreadable) = walk_paths(&MacProvider::new(), &root, &opts);
    let (queue, queue_unreadable) = walk_paths(queued, &root, &opts);
    if env.fixtures().unreadable_effective() {
        assert_eq!(
            bulk_unreadable.len(),
            2,
            "closed/ and empty-closed/: {bulk_unreadable:?}"
        );
    }
    assert_eq!(bulk.len(), queue.len());
    // Directory ids follow the order the workers found directories in.
    let unnumbered = |entry: &Entry| Entry {
        parent: crate::DirId::ROOT,
        dir: entry.dir.map(|_| crate::DirId::ROOT),
        ..entry.clone()
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

/// The fixture tree read through the stat queue as if every directory's
/// bulk read had answered `pretend`.
fn same_through_the_stat_queue(pretend: Outcome) {
    same_as_bulk_reads(&MacProvider {
        pretend: Some(pretend),
        ..MacProvider::default()
    });
}

#[test]
fn p1_walk_03_the_stat_queue_reads_what_bulk_reads_read() {
    same_through_the_stat_queue(Outcome::IoBound);
}

#[test]
fn p1_walk_03_enotsup_rereads_the_directory_by_name() {
    same_through_the_stat_queue(Outcome::Unsupported);
}

#[test]
fn p1_walk_03_an_io_bound_directory_keeps_its_probed_entries() {
    // Every directory of two refills or more switches after its second;
    // the entries those refills decoded stay, the rest go to the queue,
    // and nothing is read twice or missed.
    let provider = MacProvider {
        waiting: true,
        ..MacProvider::default()
    };
    same_as_bulk_reads(&provider);
    assert!(
        provider.switched.load(Ordering::Relaxed) > 0,
        "the probe never switched"
    );
}

#[test]
fn p1_walk_03_a_record_without_a_name_is_an_error() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let fd = posix::open_dir(&root).unwrap();
    let dir = DirHandle {
        id: DirId::ROOT,
        path: root.clone(),
        depth: 0,
    };
    let (mut out, mut errs) = (Vec::new(), Vec::new());
    let mut single = ffi::AttrBuf::new(stat::BUFFER_BYTES);
    super::take(
        &dir,
        fd.as_fd(),
        &WalkOptions::default(),
        &mut single,
        Decoded::Nameless,
        &mut out,
        &mut errs,
    );
    assert!(out.is_empty());
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].errno, crate::EIO);
    assert!(errs[0].name.as_bytes().is_empty());
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
