//! An in-memory directory tree that implements [`Provider`], for tests of the
//! walk and of what consumes it.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::{
    Attrs, Boundary, DirHandle, DirId, Entry, Flags, FullAttrs, Kind, NameRef, Normalization,
    Provider, ScanError, Share, VolumeClass, VolumeInfo, WalkOptions,
};

/// Every entry's mtime and ctime: 2026-10-22T00:00:00Z.
const TIME_NS: i64 = 1_792_627_200_000_000_000;
/// The allocation unit sizes round up to.
const BLOCK: u64 = 4096;
/// The device and mount id of every entry.
const DEV: u64 = 1;

/// A directory tree held in memory. Entries get fixed metadata: one device,
/// a unique inode each, allocation rounded up to 4 KiB, and the same times.
/// Paths given to the builder methods are relative to the root, and missing
/// parent directories are added.
#[derive(Debug)]
pub struct MemProvider {
    root: PathBuf,
    /// Every entry by its full path's bytes, which hash much faster than a
    /// `Path`'s components in a deep tree.
    nodes: HashMap<Vec<u8>, Node>,
    /// How the volume compares names: case-sensitively, and its
    /// normalisation.
    case_sensitive: bool,
    normalization: Normalization,
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    /// Apparent size: a file's length, a symlink's target length.
    size: u64,
    ino: u64,
    /// Children's names, in the order they were added.
    children: Vec<NameRef>,
    /// Reading this directory fails with this error number.
    read_errno: Option<i32>,
    /// Reading this entry from its parent fails with this error number.
    stat_errno: Option<i32>,
    /// A directory holding a valid `CACHEDIR.TAG`.
    tagged: bool,
    /// A symbolic link's target, when the tree says where it leads.
    target: Option<PathBuf>,
}

impl MemProvider {
    /// A tree holding only the root directory, at `root`.
    pub fn new(root: impl AsRef<Path>) -> MemProvider {
        let mut tree = MemProvider {
            root: normal(root.as_ref()),
            nodes: HashMap::new(),
            case_sensitive: true,
            normalization: Normalization::None,
        };
        let root = tree.root.clone();
        tree.insert(root, Kind::Dir, 0);
        tree
    }

    /// The root path.
    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Makes the volume compare names as APFS or exFAT does: without case
    /// unless `case_sensitive`, and with `normalization`.
    pub fn compares_names(
        &mut self,
        case_sensitive: bool,
        normalization: Normalization,
    ) -> &mut MemProvider {
        self.case_sensitive = case_sensitive;
        self.normalization = normalization;
        self
    }

    /// Adds the directory at `path`.
    pub fn dir(&mut self, path: impl AsRef<Path>) -> &mut MemProvider {
        self.add(path.as_ref(), Kind::Dir, 0)
    }

    /// Adds a regular file of `size` bytes at `path`.
    pub fn file(&mut self, path: impl AsRef<Path>, size: u64) -> &mut MemProvider {
        self.add(path.as_ref(), Kind::File, size)
    }

    /// Adds a symbolic link at `path` with a target of `target_len` bytes.
    pub fn symlink(&mut self, path: impl AsRef<Path>, target_len: u64) -> &mut MemProvider {
        self.add(path.as_ref(), Kind::Symlink, target_len)
    }

    /// Adds a symbolic link at `path` that leads to `target`, absolute or
    /// relative to the link's directory, which
    /// [`follow`](Provider::follow) resolves through the tree.
    pub fn link(&mut self, path: impl AsRef<Path>, target: impl AsRef<Path>) -> &mut MemProvider {
        let target = target.as_ref();
        let len = target.as_os_str().len() as u64;
        self.add(path.as_ref(), Kind::Symlink, len);
        self.node_mut(path.as_ref()).target = Some(target.to_owned());
        self
    }

    /// Gives the directory at `path` a valid `CACHEDIR.TAG`, which
    /// [`cachedir_tag`](Provider::cachedir_tag) reports.
    ///
    /// # Panics
    ///
    /// If nothing was added at `path`.
    pub fn cachedir_tag(&mut self, path: impl AsRef<Path>) -> &mut MemProvider {
        self.node_mut(path.as_ref()).tagged = true;
        self
    }

    /// Makes reading the directory at `path` fail with `errno`.
    ///
    /// # Panics
    ///
    /// If nothing was added at `path`.
    pub fn unreadable(&mut self, path: impl AsRef<Path>, errno: i32) -> &mut MemProvider {
        self.node_mut(path.as_ref()).read_errno = Some(errno);
        self
    }

    /// Makes the entry at `path` fail with `errno` when its parent is read: it
    /// is listed, but reported as a [`ScanError`] instead of an entry.
    ///
    /// # Panics
    ///
    /// If nothing was added at `path`.
    pub fn unstatable(&mut self, path: impl AsRef<Path>, errno: i32) -> &mut MemProvider {
        self.node_mut(path.as_ref()).stat_errno = Some(errno);
        self
    }

    /// The full path of every entry, the root included.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.nodes
            .keys()
            .map(|key| Path::new(OsStr::from_bytes(key)))
    }

    /// The full path of every directory, the root included.
    pub fn dir_paths(&self) -> impl Iterator<Item = &Path> {
        self.nodes
            .iter()
            .filter(|(_, node)| node.kind == Kind::Dir)
            .map(|(key, _)| Path::new(OsStr::from_bytes(key)))
    }

    /// The number of entries, the root included.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.nodes.len()
    }

    fn add(&mut self, relative: &Path, kind: Kind, size: u64) -> &mut MemProvider {
        let path = normal(&self.root.join(relative));
        let mut missing: Vec<&Path> = path
            .ancestors()
            .skip(1)
            .take_while(|dir| !self.nodes.contains_key(key(dir)))
            .collect();
        missing.reverse();
        for dir in missing {
            self.insert(dir.to_owned(), Kind::Dir, 0);
        }
        self.insert(path, kind, size);
        self
    }

    fn insert(&mut self, path: PathBuf, kind: Kind, size: u64) {
        if self.nodes.contains_key(key(&path)) {
            return;
        }
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
            && let Some(parent) = self.nodes.get_mut(key(parent))
        {
            parent.children.push(NameRef::from(name));
        }
        let ino = self.nodes.len() as u64 + 2;
        self.nodes.insert(
            path.into_os_string().into_vec(),
            Node {
                kind,
                size,
                ino,
                children: Vec::new(),
                read_errno: None,
                stat_errno: None,
                tagged: false,
                target: None,
            },
        );
    }

    fn node_mut(&mut self, relative: &Path) -> &mut Node {
        let path = normal(&self.root.join(relative));
        self.nodes
            .get_mut(key(&path))
            .unwrap_or_else(|| panic!("MemProvider: nothing at {}", path.display()))
    }
}

/// `path` with its separators normalised, so equal paths have equal bytes.
fn normal(path: &Path) -> PathBuf {
    path.components().collect()
}

/// `path` with `.` and `..` resolved by name, as a link target is in a tree
/// that holds no links on the way.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn key(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

impl Node {
    fn entry(&self, name: NameRef, opts: &WalkOptions) -> Entry {
        let (nlink, mode) = match self.kind {
            Kind::Dir => (2, 0o040_755),
            Kind::Symlink => (1, 0o120_777),
            _ => (1, 0o100_644),
        };
        Entry {
            name,
            parent: DirId::ROOT,
            dir: None,
            kind: self.kind,
            apparent: self.size,
            allocated: self.size.div_ceil(BLOCK) * BLOCK,
            mtime_ns: TIME_NS,
            ctime_ns: TIME_NS,
            nlink,
            dev: DEV,
            ino: self.ino,
            mnt_id: DEV,
            share: Share::None,
            boundary: Boundary::None,
            flags: Flags::empty(),
            full: (opts.attrs == Attrs::Full).then_some(FullAttrs {
                atime_ns: TIME_NS,
                btime_ns: Some(TIME_NS),
                mode,
                uid: 1000,
                gid: 1000,
            }),
        }
    }
}

impl Provider for MemProvider {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let path = normal(dir.path());
        let node = self
            .nodes
            .get(key(&path))
            .filter(|node| node.kind == Kind::Dir)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if let Some(errno) = node.read_errno {
            return Err(io::Error::from_raw_os_error(errno));
        }
        for name in &node.children {
            let child = &self.nodes[key(&path.join(name.as_os_str()))];
            match child.stat_errno {
                Some(errno) => errs.push(ScanError {
                    dir: dir.id(),
                    name: name.clone(),
                    errno,
                }),
                None => out.push(child.entry(name.clone(), opts)),
            }
        }
        Ok(())
    }

    fn volume(&self, _path: &Path) -> io::Result<VolumeInfo> {
        Ok(VolumeInfo {
            dev: DEV,
            mnt_id: DEV,
            mount_point: self.root.clone(),
            fs_type: "mem".to_owned(),
            class: VolumeClass::Local,
            read_only: false,
            case_sensitive: Some(self.case_sensitive),
            normalization: self.normalization,
            options: Vec::new(),
            total_bytes: 1 << 40,
            free_bytes: 1 << 39,
            reason: None,
        })
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let node = self
            .nodes
            .get(key(&normal(path)))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let name = path.file_name().unwrap_or(path.as_os_str());
        Ok(node.entry(NameRef::from(name), opts))
    }

    fn cachedir_tag(&self, dir: &Path) -> bool {
        self.nodes
            .get(key(&normal(dir)))
            .is_some_and(|node| node.tagged)
    }

    /// Follows the links the tree says where they lead, as the kernel
    /// would: `ELOOP` after 40 hops, `ENOENT` for a target not in the tree.
    /// A link added without a target leads nowhere.
    fn follow(&self, path: &Path, opts: &WalkOptions) -> io::Result<(PathBuf, Entry)> {
        let mut at = normal(path);
        for _ in 0..40 {
            let node = self
                .nodes
                .get(key(&at))
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            if node.kind != Kind::Symlink {
                let entry = self.root(&at, opts)?;
                return Ok((at, entry));
            }
            let target = node
                .target
                .as_ref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            at = lexical(&at.parent().unwrap_or(&at).join(target));
        }
        Err(io::Error::from_raw_os_error(libc::ELOOP))
    }
}
