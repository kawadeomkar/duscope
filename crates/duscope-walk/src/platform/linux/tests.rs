//! P1-WALK-02: the Linux provider against `lstat`, its `statx` count, and
//! a wide directory spread over every worker.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::thread::{self, ThreadId};
use std::time::Duration;

use duscope_testkit::TestEnv;

use super::mounts::{Mount, parse_mountinfo};
use super::{LinuxProvider, dents};
use crate::{
    DirHandle, DirId, Entry, Kind, NameRef, Provider, STAT_BATCH, ScanError, ScanEvent, VolumeInfo,
    WalkOptions, walk_with,
};

/// Every entry of a walk by its path.
fn walk_paths(
    provider: &impl Provider,
    root: &Path,
    opts: &WalkOptions,
) -> BTreeMap<PathBuf, Entry> {
    let mut dirs: BTreeMap<DirId, PathBuf> = BTreeMap::new();
    let mut entries = BTreeMap::new();
    walk_with(provider, root, opts, |event| {
        if let ScanEvent::Entry(entry) = event {
            let path = if entry.dir == Some(DirId::ROOT) {
                root.to_owned()
            } else {
                dirs[&entry.parent].join(entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            entries.insert(path, entry);
        }
    })
    .unwrap();
    entries
}

#[test]
fn p1_walk_02_every_entry_matches_lstat() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let entries = walk_paths(&LinuxProvider::new(), &root, &WalkOptions::default());
    assert!(
        entries.len() > 100_000,
        "the fixture tree was walked: {}",
        entries.len()
    );
    for (path, entry) in &entries {
        let meta = fs::symlink_metadata(path).unwrap();
        let file_type = meta.file_type();
        let kind = if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_file() {
            Kind::File
        } else if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_socket() {
            Kind::Socket
        } else if file_type.is_fifo() {
            Kind::Fifo
        } else if file_type.is_char_device() {
            Kind::Char
        } else {
            Kind::Block
        };
        assert_eq!(entry.kind, kind, "{path:?}");
        let allocated = match kind {
            Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block => 0,
            _ => meta.blocks() * 512,
        };
        assert_eq!(entry.allocated, allocated, "allocated {path:?}");
        assert_eq!(entry.apparent, meta.size(), "apparent {path:?}");
        let ns = |s: i64, n: i64| s * 1_000_000_000 + n;
        assert_eq!(
            entry.mtime_ns,
            ns(meta.mtime(), meta.mtime_nsec()),
            "mtime {path:?}"
        );
        assert_eq!(
            entry.ctime_ns,
            ns(meta.ctime(), meta.ctime_nsec()),
            "ctime {path:?}"
        );
        assert_eq!(entry.dev, meta.dev(), "dev {path:?}");
        assert_eq!(entry.ino, meta.ino(), "ino {path:?}");
        assert_eq!(u64::from(entry.nlink), meta.nlink(), "nlink {path:?}");
        assert_ne!(
            entry.mnt_id, 0,
            "the kernel reports mount ids (5.8): {path:?}"
        );
    }
}

#[test]
fn p1_walk_02_one_statx_per_entry() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let provider = LinuxProvider::new();
    let mut summary = None;
    let opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    summary.replace(walk_with(&provider, &root, &opts, |_| {}).unwrap());
    let summary = summary.unwrap();
    // Every listed name is read once, the root included; nothing else is
    // stat'ed, by statx or otherwise (the provider makes no lstat call).
    let calls = provider.statx_calls.load(Ordering::Relaxed) as u64;
    assert_eq!(calls, summary.entries + summary.entry_errors);
}

/// Records which threads read the stat batches of the wrapped provider.
struct Threads<'a> {
    inner: &'a LinuxProvider,
    batches: Mutex<Vec<(usize, ThreadId)>>,
}

impl Provider for Threads<'_> {
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
        self.inner.read_dir_split(dir, opts, out, errs, later)
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
        // A batch of empty files is quick on a warm cache; this keeps the
        // other workers in the race for the rest.
        thread::sleep(Duration::from_millis(1));
        self.inner.stat_names(dir, names, opts, out, errs)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

#[test]
fn p1_walk_02_a_wide_directory_is_read_by_every_worker() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("wide-directory");
    let inner = LinuxProvider::new();
    let provider = Threads {
        inner: &inner,
        batches: Mutex::new(Vec::new()),
    };
    let workers = 4;
    let opts = WalkOptions {
        threads: workers,
        ..WalkOptions::default()
    };
    let summary = walk_with(&provider, &root, &opts, |_| {}).unwrap();
    let batches = provider.batches.lock().unwrap();
    let names: usize = batches.iter().map(|&(len, _)| len).sum();
    // The directory's first 1,024 names are read with its listing; the
    // other 98,976 go out in 97 batches.
    assert_eq!(summary.entries, 100_001, "the directory and its files");
    assert_eq!(names, 100_000 - STAT_BATCH);
    assert_eq!(batches.len(), (100_000 - STAT_BATCH).div_ceil(STAT_BATCH));
    let threads: HashSet<ThreadId> = batches.iter().map(|&(_, thread)| thread).collect();
    assert_eq!(
        threads.len(),
        workers,
        "every worker read some of the batches"
    );
}

#[test]
fn p1_walk_02_listings_parse_dirent_records() {
    // d_ino, d_off, d_reclen, d_type, name, NUL, padding to 8 bytes.
    let record = |name: &[u8]| {
        let reclen = (19 + name.len() + 1).div_ceil(8) * 8;
        let mut r = vec![0u8; reclen];
        r[16..18].copy_from_slice(&u16::try_from(reclen).unwrap().to_ne_bytes());
        r[18] = 8;
        r[19..19 + name.len()].copy_from_slice(name);
        r
    };
    let mut buf = Vec::new();
    for name in [
        &b"."[..],
        b"..",
        b"a",
        b"\xff\xfe-not-utf8",
        b"long-name-of-twenty",
    ] {
        buf.extend(record(name));
    }
    let mut names = Vec::new();
    dents::parse(&buf, &mut names).unwrap();
    let names: Vec<&[u8]> = names.iter().map(NameRef::as_bytes).collect();
    assert_eq!(
        names,
        [&b"a"[..], b"\xff\xfe-not-utf8", b"long-name-of-twenty"]
    );

    let mut short = record(b"cut");
    short.truncate(20);
    assert!(dents::parse(&short, &mut Vec::new()).is_err());
}

#[test]
fn p1_walk_02_mountinfo_lines_parse() {
    let text = b"22 1 0:21 / /proc rw,nosuid shared:12 - proc proc rw\n\
        36 35 98:0 /mnt1 /mnt/with\\040space rw,noatime master:1 - ext4 /dev/root rw\n\
        garbage\n";
    assert_eq!(
        parse_mountinfo(text),
        [
            Mount {
                id: 22,
                path: PathBuf::from("/proc"),
                fs_type: "proc".to_owned(),
                options: vec!["rw".to_owned(), "nosuid".to_owned(), "rw".to_owned()],
            },
            Mount {
                id: 36,
                path: PathBuf::from("/mnt/with space"),
                fs_type: "ext4".to_owned(),
                options: vec!["rw".to_owned(), "noatime".to_owned(), "rw".to_owned()],
            },
        ]
    );
}

#[test]
fn p1_walk_02_the_volume_comes_from_mountinfo_and_statvfs() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let volume = LinuxProvider::new().volume(&root).unwrap();
    let meta = fs::symlink_metadata(&root).unwrap();
    assert_eq!(volume.dev, meta.dev());
    assert!(
        fs::canonicalize(&root)
            .unwrap()
            .starts_with(&volume.mount_point)
    );
    assert!(volume.total_bytes >= volume.free_bytes && volume.total_bytes > 0);
    assert!(!volume.fs_type.is_empty());
}

#[test]
fn p1_walk_05_full_attrs_match_lstat() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let opts = WalkOptions {
        attrs: crate::Attrs::Full,
        ..WalkOptions::default()
    };
    let entries = walk_paths(&LinuxProvider::new(), &root, &opts);
    let ns = |s: i64, n: i64| s * 1_000_000_000 + n;
    for (path, entry) in &entries {
        let meta = fs::symlink_metadata(path).unwrap();
        let full = entry
            .full
            .unwrap_or_else(|| panic!("no full attributes: {path:?}"));
        assert_eq!(full.mode, meta.mode(), "mode {path:?}");
        assert_eq!(
            (full.uid, full.gid),
            (meta.uid(), meta.gid()),
            "owner {path:?}"
        );
        assert_eq!(
            full.atime_ns,
            ns(meta.atime(), meta.atime_nsec()),
            "atime {path:?}"
        );
        // std reads the creation time with statx too; None where the
        // filesystem keeps none.
        let created = meta.created().ok().map(|time| {
            let since = time.duration_since(std::time::UNIX_EPOCH).unwrap();
            i64::try_from(since.as_nanos()).unwrap()
        });
        assert_eq!(full.btime_ns, created, "btime {path:?}");
    }
    let basic = walk_paths(&LinuxProvider::new(), &root, &WalkOptions::default());
    assert!(basic.values().all(|entry| entry.full.is_none()));
}

#[test]
fn p1_walk_05_full_attrs_add_no_calls() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let count = |attrs| {
        let provider = LinuxProvider::new();
        let opts = WalkOptions {
            attrs,
            ..WalkOptions::default()
        };
        walk_with(&provider, &root, &opts, |_| {}).unwrap();
        provider.statx_calls.load(Ordering::Relaxed)
    };
    // The five attributes are mask bits of the same statx.
    assert_eq!(count(crate::Attrs::Full), count(crate::Attrs::Basic));
}

#[test]
fn p1_walk_06_mount_classes() {
    use super::mounts::{case_sensitive, class};
    use crate::VolumeClass;
    assert_eq!(class("ext4"), VolumeClass::Local);
    assert_eq!(class("proc"), VolumeClass::Kernel);
    assert_eq!(class("nfs4"), VolumeClass::Network);
    assert_eq!(class("fuse.sshfs"), VolumeClass::Network);
    assert_eq!(class("ceph"), VolumeClass::Network);
    assert_eq!(class("autofs"), VolumeClass::Network);
    assert_eq!(
        class("fuseblk"),
        VolumeClass::Foreign,
        "ntfs-3g on a local disk"
    );
    assert_eq!(class("tmpfs"), VolumeClass::Memory);
    assert_eq!(class("overlay"), VolumeClass::Overlay);
    assert_eq!(class("squashfs"), VolumeClass::ReadOnlyImage);
    assert_eq!(case_sensitive("exfat", &[]), Some(false));
    assert_eq!(case_sensitive("ntfs3", &["nocase".to_owned()]), Some(false));
    assert_eq!(case_sensitive("hfsplus", &[]), None);
    assert_eq!(case_sensitive("ext4", &[]), Some(true));
}

#[test]
fn p1_walk_06_the_fallback_root_has_a_mount_id() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let entry = crate::platform::fallback::FallbackProvider::default()
        .root(&root, &WalkOptions::default())
        .unwrap();
    assert_ne!(entry.mnt_id, 0);
}

#[test]
fn p1_walk_06_mount_info_reads_the_mount_table() {
    let info = super::mounts::Mounts::default()
        .mount_info(Path::new("/proc"))
        .unwrap();
    assert_eq!(info.class, crate::VolumeClass::Kernel);
    assert_eq!((info.total_bytes, info.free_bytes), (0, 0));
}
