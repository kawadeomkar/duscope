//! `FSEvents`: the declarations of the `FSEvents`, `CoreFoundation` and
//! libdispatch calls a volume's history needs, written here rather than
//! taken from `objc2-core-services` or `notify` (D36), the marks a walk
//! records, and the replay of a history from a mark.
//!
//! A replay opens a stream on the volume's device
//! (`FSEventStreamCreateRelativeToDevice`), watching the volume's root (on
//! the running system's own volumes, the caller's directory alone) and
//! starting at the mark's event id with the full history
//! (`kFSEventStreamCreateFlagFullHistory`), schedules it on a serial
//! dispatch queue, and collects what the callback delivers until
//! `HistoryDone` or the timeout. Paths arrive relative to the device's root
//! and are spelled back from the directory the caller gave.

use std::collections::BTreeSet;
use std::ffi::{CStr, OsStr, c_char, c_void};
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::ffi;
use crate::fsevents::Replay;
use crate::{FsEventsError, FsEventsMark};

/// `CFIndex`.
type CFIndex = isize;
/// `CFTypeRef` and the object references below, all opaque.
type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFStringRef = *const c_void;
type CFUUIDRef = *const c_void;
/// `FSEventStreamRef` and `ConstFSEventStreamRef`.
type FSEventStreamRef = *mut c_void;
type ConstFSEventStreamRef = *const c_void;
/// `dispatch_queue_t`.
type DispatchQueue = *mut c_void;

/// `FSEventStreamCallback`: the stream, the context's `info`, the number of
/// events, their paths (a C array of C strings without
/// `kFSEventStreamCreateFlagUseCFTypes`), flags and ids.
type FSEventStreamCallback =
    extern "C" fn(ConstFSEventStreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

/// `FSEventStreamContext`: copied into the stream when it is created.
#[repr(C)]
struct FSEventStreamContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<extern "C" fn(*const c_void)>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

/// `CFUUIDBytes`: sixteen `UInt8` fields, laid out as this array.
#[repr(C)]
struct CFUUIDBytes {
    bytes: [u8; 16],
}

/// `CFArrayCallBacks`; only `kCFTypeArrayCallBacks`' address is used.
#[repr(C)]
struct CFArrayCallBacks {
    version: CFIndex,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;
/// `kFSEventStreamCreateFlagFullHistory` (macOS 10.15).
const FULL_HISTORY: u32 = 0x0000_0080;
/// `MNT_EXT_ROOT_DATA_VOL` of `<sys/mount.h>`, in `f_flags_ext`: the Data
/// volume of the running system's volume group.
const MNT_EXT_ROOT_DATA_VOL: u32 = 0x0000_0001;

/// The event flags of `<FSEvents/FSEvents.h>` a replay reads.
pub(super) const MUST_SCAN_SUB_DIRS: u32 = 0x0000_0001;
pub(super) const USER_DROPPED: u32 = 0x0000_0002;
pub(super) const KERNEL_DROPPED: u32 = 0x0000_0004;
pub(super) const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
pub(super) const HISTORY_DONE: u32 = 0x0000_0010;
pub(super) const MOUNT: u32 = 0x0000_0040;
pub(super) const UNMOUNT: u32 = 0x0000_0080;

// SAFETY: the declarations match `<FSEvents/FSEvents.h>` (CoreServices):
// the stream functions take the `FSEventStreamRef` the create call
// returned, `Boolean` is an unsigned char, `dev_t` is `libc::dev_t`,
// `CFTimeInterval` a double and `FSEventStreamEventId` a 64-bit unsigned
// integer.
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreateRelativeToDevice(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        device: libc::dev_t,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventsCopyUUIDForDevice(dev: libc::dev_t) -> CFUUIDRef;
}

// SAFETY: the declarations match `<CoreFoundation/CFUUID.h>`,
// `<CoreFoundation/CFString.h>`, `<CoreFoundation/CFArray.h>` and
// `<CoreFoundation/CFBase.h>`; `CFUUIDBytes` is returned by value as the
// sixteen-byte struct above, and `kCFTypeArrayCallBacks` is a constant
// struct that is only read.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        len: CFIndex,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    fn CFRelease(cf: CFTypeRef);
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
}

// SAFETY: the declaration matches `<dispatch/queue.h>` (libSystem); a null
// attribute asks for a serial queue.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
}

/// The system's latest event id; `None` when `fseventsd` gives none.
pub(in crate::platform) fn current_event_id() -> Option<u64> {
    // SAFETY: FSEventsGetCurrentEventId takes nothing and returns an
    // integer.
    let id = unsafe { FSEventsGetCurrentEventId() };
    (id != 0 && id != SINCE_NOW).then_some(id)
}

/// The UUID of the `FSEvents` database of the volume on `dev`, as
/// `CFUUIDCreateString` spells one; `None` where the volume's history is
/// not kept.
pub(in crate::platform) fn volume_uuid(dev: u64) -> Option<String> {
    let dev = dev_t(dev)?;
    // SAFETY: FSEventsCopyUUIDForDevice takes a device number; its result
    // follows the Copy rule, and `Cf` releases it.
    let uuid = Cf::new(unsafe { FSEventsCopyUUIDForDevice(dev) })?;
    // SAFETY: `uuid` is a live CFUUID for the call.
    let bytes = unsafe { CFUUIDGetUUIDBytes(uuid.as_ptr()) }.bytes;
    Some(spell_uuid(&bytes))
}

/// The mark of the volume on `dev` at the event id `now`; `None` without a
/// UUID or a system build.
pub(in crate::platform) fn mark(dev: u64, now: u64) -> Option<FsEventsMark> {
    Some(FsEventsMark {
        uuid: volume_uuid(dev)?,
        event_id: now,
        os_build: os_build()?.to_owned(),
    })
}

/// The running system's build, read once.
fn os_build() -> Option<&'static str> {
    static BUILD: OnceLock<Option<String>> = OnceLock::new();
    BUILD.get_or_init(ffi::os_build).as_deref()
}

/// See [`fsevents::replay`](crate::fsevents::replay).
pub(in crate::platform) fn replay(
    dev: u64,
    mark: &FsEventsMark,
    under: &Path,
    timeout: Duration,
) -> Result<Replay, FsEventsError> {
    let place = locate(under).map_err(|source| FsEventsError::Path {
        path: under.to_owned(),
        source,
    })?;
    let device = dev_t(dev).filter(|_| place.dev == dev);
    let Some(device) = device else {
        return Err(FsEventsError::OtherDevice {
            path: under.to_owned(),
            dev,
            found: place.dev,
        });
    };
    // Before the UUID: logging turned off leaves the volume without one,
    // and this is the reason why.
    if no_log(&place.mount_point) {
        return Err(FsEventsError::Disabled {
            mount_point: place.mount_point,
        });
    }
    let Some(uuid) = volume_uuid(dev) else {
        return Err(FsEventsError::NoHistory { dev });
    };
    if uuid != mark.uuid {
        return Err(FsEventsError::OtherHistory {
            dev,
            recorded: mark.uuid.clone(),
            found: uuid,
        });
    }
    let build = os_build().unwrap_or_default();
    if build != mark.os_build {
        return Err(FsEventsError::OtherSystem {
            recorded: mark.os_build.clone(),
            found: build.to_owned(),
        });
    }
    let current = current_event_id().unwrap_or(0);
    if current < mark.event_id {
        return Err(FsEventsError::IdsBehind {
            since: mark.event_id,
            current,
        });
    }
    let shared = Arc::new(Shared {
        state: Mutex::new(Collector::new(
            mark.event_id,
            place.watched.clone(),
            under.to_owned(),
        )),
        done: Condvar::new(),
    });
    let mut stream = Stream::open(device, place.stream_path(), mark.event_id, &shared)
        .ok_or(FsEventsError::Stream { dev })?;
    if !stream.start() {
        return Err(FsEventsError::Stream { dev });
    }
    let deadline = Instant::now().checked_add(timeout);
    let mut state = lock(&shared.state);
    while !state.history_done {
        state = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                shared
                    .done
                    .wait_timeout(state, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner),
        };
    }
    drop(state);
    // Stopped, the stream calls back no more.
    drop(stream);
    let replay = lock(&shared.state).finish();
    log::debug!(
        "FSEvents replay of {} from {}: {} directories, {} subtrees, dropped {}, \
         wrapped {}, done {}",
        under.display(),
        mark.event_id,
        replay.dirs.len(),
        replay.must_scan_subdirs.len(),
        replay.dropped,
        replay.ids_wrapped,
        replay.history_done,
    );
    Ok(replay)
}

/// Whether `FSEvents` logging is off on the volume mounted at
/// `mount_point`: `.fseventsd/no_log` exists at its root. A volume whose
/// `.fseventsd` cannot be read (the Data volume, owned by root) counts as
/// logging; with logging off it has no UUID either, which the replay checks
/// next.
pub(super) fn no_log(mount_point: &Path) -> bool {
    match fs::symlink_metadata(mount_point.join(".fseventsd").join("no_log")) {
        Ok(_) => true,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::debug!(
                    "cannot tell whether FSEvents logs {}: {e}",
                    mount_point.display()
                );
            }
            false
        }
    }
}

/// Where a directory is: its volume's device and mount point, and its path
/// below that mount point without firmlinks, as `FSEvents` spells it.
pub(super) struct Place {
    pub(super) dev: u64,
    pub(super) mount_point: PathBuf,
    /// No leading or trailing slash; empty at the mount point.
    pub(super) watched: Vec<u8>,
    /// The volume is one of the running system's own: `/` (`MNT_ROOTFS`)
    /// or its Data volume (`MNT_EXT_ROOT_DATA_VOL`), mounted when it boots
    /// and unmounted only when it shuts down.
    pub(super) system: bool,
}

impl Place {
    /// What a replay's stream watches, relative to the device's root.
    ///
    /// The volume's root, for the collector to keep what is at or above
    /// `watched`: fseventsd records a mount as a `Mount` event there and
    /// sends it only to a stream that watches the root. A stream on
    /// `watched` alone never learns that the volume was attached again
    /// since the mark, perhaps after changes made where nothing logged
    /// them (attached with `-nobrowse`, or on another system).
    ///
    /// `watched` alone on the running system's own volumes. They cannot be
    /// attached again while it runs, fseventsd keeps their history across
    /// a restart, and the Data volume's whole history is too long to read:
    /// ten million event ids of it, minutes on a busy Mac, take 14 to 18 s
    /// against the 1.3 s of one directory's.
    pub(super) fn stream_path(&self) -> &[u8] {
        if self.system { &self.watched } else { b"" }
    }
}

/// Places the directory `under`.
pub(super) fn locate(under: &Path) -> io::Result<Place> {
    let dir = fs::File::open(under)?;
    let fs = ffi::fstatfs(dir.as_fd())?;
    let mount: Vec<u8> = fs
        .f_mntonname
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.cast_unsigned())
        .collect();
    let path = ffi::nofirmlink_path(dir.as_fd())?;
    // The mount point as given to mount(2), which can pass through a
    // firmlink (an image attached under /private/var), spelled as the path
    // is.
    let base = fs::File::open(OsStr::from_bytes(&mount))
        .and_then(|root| ffi::nofirmlink_path(root.as_fd()))
        .unwrap_or_else(|_| mount.clone());
    let watched = relative(&path, &base).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not below its volume's mount point {}",
                String::from_utf8_lossy(&path),
                String::from_utf8_lossy(&base)
            ),
        )
    })?;
    Ok(Place {
        dev: ffi::fsid_dev(&fs),
        mount_point: PathBuf::from(OsStr::from_bytes(&mount)),
        watched: watched.to_vec(),
        system: fs.f_flags & libc::MNT_ROOTFS.cast_unsigned() != 0
            || fs.f_flags_ext & MNT_EXT_ROOT_DATA_VOL != 0,
    })
}

/// `path` relative to the mount point `mount`, which holds it.
fn relative<'a>(path: &'a [u8], mount: &[u8]) -> Option<&'a [u8]> {
    let rest = path.strip_prefix(mount)?;
    if mount.ends_with(b"/") || rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(b"/")
    }
}

/// What a replay has read so far. The stream's callback hands it each
/// event; the replay takes the result once the history is done.
// The replay's four flags, as it hands them on.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Collector {
    /// The mark's event id.
    since: u64,
    /// The caller's directory, relative to the device's root; the stream
    /// watches it or the volume's root ([`Place::stream_path`]).
    watched: Vec<u8>,
    /// That directory as the caller spelled it.
    root: PathBuf,
    dirs: BTreeSet<PathBuf>,
    deep: BTreeSet<PathBuf>,
    ancestors_changed: bool,
    dropped: bool,
    ids_wrapped: bool,
    history_done: bool,
}

/// Where an event's path is, against the watched directory.
enum Placed {
    /// At or below it: the directory, spelled from the root.
    At(PathBuf),
    /// Above it: the device's root or a directory on the way down.
    Above,
}

impl Collector {
    pub(super) fn new(since: u64, watched: Vec<u8>, root: PathBuf) -> Collector {
        Collector {
            since,
            watched,
            root,
            dirs: BTreeSet::new(),
            deep: BTreeSet::new(),
            ancestors_changed: false,
            dropped: false,
            ids_wrapped: false,
            history_done: false,
        }
    }

    /// Takes one event: its path relative to the device's root (a
    /// directory, with a trailing slash), its flags and its id.
    ///
    /// Every record counts, whatever its id. The full history delivers the
    /// whole stored chunk that holds the mark, and a record there can keep
    /// the id of the first change it holds while holding a later one too: a
    /// file created after the scan in a directory made just before it can
    /// come back under the directory's older id. A directory changed
    /// shortly before the scan may be read again; none changed after it is
    /// missed. The id decides one thing: a mount or unmount, which
    /// fseventsd numbers as it sees it, widens to the subtree only after
    /// the mark; one before it was in place when the walk read the tree.
    pub(super) fn take(&mut self, path: &[u8], flags: u32, id: u64) {
        // Events after the history are changes made during the replay, not
        // part of it.
        if self.history_done {
            return;
        }
        // Lost events and wrapped ids void the history wherever they show.
        if flags & (USER_DROPPED | KERNEL_DROPPED) != 0 {
            self.dropped = true;
        }
        if flags & EVENT_IDS_WRAPPED != 0 {
            self.ids_wrapped = true;
        }
        if flags & HISTORY_DONE != 0 {
            self.history_done = true;
            // The end of a history that cannot reach back to the mark.
            if flags & MUST_SCAN_SUB_DIRS != 0 {
                self.deep.insert(self.root.clone());
            }
            return;
        }
        let deep =
            flags & MUST_SCAN_SUB_DIRS != 0 || (flags & (MOUNT | UNMOUNT) != 0 && id > self.since);
        match self.place(path) {
            // The stream also reports the directories above the watched
            // one. A subtree to read again there holds the root's; a
            // plain change there renamed, removed or added an entry beside
            // the path to the root, perhaps one on it.
            Some(Placed::Above) if deep => {
                self.deep.insert(self.root.clone());
            }
            Some(Placed::Above) => self.ancestors_changed = true,
            Some(Placed::At(dir)) if deep => {
                self.deep.insert(dir);
            }
            Some(Placed::At(dir)) => {
                self.dirs.insert(dir);
            }
            None => {}
        }
    }

    /// Where an event's path is: a directory at or below the watched one,
    /// spelled from the root; above it; or `None`, beside it.
    fn place(&self, path: &[u8]) -> Option<Placed> {
        let path = trim_slashes(path);
        let rest = if self.watched.is_empty() {
            path
        } else if let Some(rest) = below(path, &self.watched) {
            rest
        } else if path.is_empty() || below(&self.watched, path).is_some() {
            return Some(Placed::Above);
        } else {
            return None;
        };
        Some(Placed::At(if rest.is_empty() {
            self.root.clone()
        } else {
            self.root.join(OsStr::from_bytes(rest))
        }))
    }

    /// The replay so far; a directory inside a subtree to read again is
    /// left out.
    pub(super) fn finish(&mut self) -> Replay {
        let deep = std::mem::take(&mut self.deep);
        let inside = |dir: &Path, from: usize| dir.ancestors().skip(from).any(|a| deep.contains(a));
        let dirs = std::mem::take(&mut self.dirs)
            .into_iter()
            .filter(|dir| !inside(dir, 0))
            .collect();
        let must_scan_subdirs = deep.iter().filter(|dir| !inside(dir, 1)).cloned().collect();
        Replay {
            root: self.root.clone(),
            dirs,
            must_scan_subdirs,
            ancestors_changed: self.ancestors_changed,
            dropped: self.dropped,
            ids_wrapped: self.ids_wrapped,
            history_done: self.history_done,
        }
    }
}

/// `path` without leading or trailing slashes.
fn trim_slashes(mut path: &[u8]) -> &[u8] {
    while let Some(rest) = path.strip_prefix(b"/") {
        path = rest;
    }
    while let Some(rest) = path.strip_suffix(b"/") {
        path = rest;
    }
    path
}

/// What follows `dir/` in `path`: empty at `dir` itself, `None` outside it.
fn below<'a>(path: &'a [u8], dir: &[u8]) -> Option<&'a [u8]> {
    let rest = path.strip_prefix(dir)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(b"/")
    }
}

/// A device number as `dev_t`: the walk's devices widen it, sign included.
fn dev_t(dev: u64) -> Option<libc::dev_t> {
    libc::dev_t::try_from(dev.cast_signed()).ok()
}

/// Sixteen bytes as an upper-case, hyphenated UUID.
pub(super) fn spell_uuid(bytes: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _ = write!(text, "{byte:02X}");
    }
    text
}

/// The collector the callback fills and the replay waits on.
struct Shared {
    state: Mutex<Collector>,
    done: Condvar,
}

fn lock(state: &Mutex<Collector>) -> MutexGuard<'_, Collector> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stream's callback: hands each event to the collector and wakes the
/// replay once the history is done.
extern "C" fn on_events(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() || count == 0 || paths.is_null() || flags.is_null() || ids.is_null() {
        return;
    }
    // SAFETY: `info` is the `Shared` of an `Arc` the stream holds a
    // reference to (`retain_info`) until it is deallocated, after its last
    // callback.
    let shared = unsafe { &*info.cast::<Shared>() };
    // SAFETY: without kFSEventStreamCreateFlagUseCFTypes `paths` is a C
    // array of `count` C strings, and `flags` and `ids` arrays of `count`
    // values, all valid until the callback returns.
    let (paths, flags, ids) = unsafe {
        (
            std::slice::from_raw_parts(paths.cast::<*const c_char>(), count),
            std::slice::from_raw_parts(flags, count),
            std::slice::from_raw_parts(ids, count),
        )
    };
    let mut state = lock(&shared.state);
    for ((&path, &flag), &id) in paths.iter().zip(flags).zip(ids) {
        let path = if path.is_null() {
            &[][..]
        } else {
            // SAFETY: a non-null entry is a NUL-terminated string, valid
            // until the callback returns.
            unsafe { CStr::from_ptr(path) }.to_bytes()
        };
        state.take(path, flag, id);
    }
    if state.history_done {
        shared.done.notify_all();
    }
}

/// The context's retain callback: the stream's own reference to `Shared`.
extern "C" fn retain_info(info: *const c_void) -> *const c_void {
    // SAFETY: `info` comes from `Arc::as_ptr` of an `Arc<Shared>` still
    // alive: the replay's own, or a reference this callback took.
    unsafe { Arc::increment_strong_count(info.cast::<Shared>()) };
    info
}

/// The context's release callback, when the stream is deallocated.
extern "C" fn release_info(info: *const c_void) {
    // SAFETY: balances one `retain_info` on the same pointer.
    unsafe { Arc::decrement_strong_count(info.cast::<Shared>()) };
}

/// An owned `CoreFoundation` object, released when dropped.
struct Cf(NonNull<c_void>);

impl Cf {
    fn new(raw: *const c_void) -> Option<Cf> {
        NonNull::new(raw.cast_mut()).map(Cf)
    }

    fn as_ptr(&self) -> *const c_void {
        self.0.as_ptr()
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: `Cf` holds the one reference a Create or Copy call
        // returned.
        unsafe { CFRelease(self.0.as_ptr()) };
    }
}

/// The serial queue every stream calls back on.
struct Queue(NonNull<c_void>);

// SAFETY: a dispatch queue is a thread-safe object; any thread may hand it
// to FSEventStreamSetDispatchQueue.
unsafe impl Send for Queue {}
// SAFETY: as above; the pointer is never written through.
unsafe impl Sync for Queue {}

/// The queue, made on first use and kept for the life of the process, so
/// it is never released.
fn queue() -> Option<DispatchQueue> {
    static QUEUE: OnceLock<Option<Queue>> = OnceLock::new();
    QUEUE
        .get_or_init(|| {
            // SAFETY: the label is NUL-terminated and copied; a null
            // attribute makes a serial queue.
            let queue =
                unsafe { dispatch_queue_create(c"duscope.fsevents".as_ptr(), std::ptr::null()) };
            NonNull::new(queue).map(Queue)
        })
        .as_ref()
        .map(|queue| queue.0.as_ptr())
}

/// A stream scheduled on the queue, stopped, invalidated and released when
/// dropped; the path it watches lives as long.
struct Stream {
    raw: NonNull<c_void>,
    started: bool,
    _paths: Cf,
    _path: Cf,
}

impl Stream {
    /// A stream on `dev` watching `watched` (relative to the device's root)
    /// from the event id `since` with the full history, calling back into
    /// `shared`; `None` when it cannot be made.
    fn open(dev: libc::dev_t, watched: &[u8], since: u64, shared: &Arc<Shared>) -> Option<Stream> {
        let queue = queue()?;
        let len = CFIndex::try_from(watched.len()).ok()?;
        // SAFETY: `watched` is valid for `len` bytes, read during the call;
        // the result follows the Create rule, and `Cf` releases it.
        let path = Cf::new(unsafe {
            CFStringCreateWithBytes(std::ptr::null(), watched.as_ptr(), len, UTF8, 0)
        })?;
        let values = [path.as_ptr()];
        // SAFETY: `values` holds one live CFString, which the array retains
        // through the CFType callbacks; the result follows the Create rule.
        let paths = Cf::new(unsafe {
            CFArrayCreate(
                std::ptr::null(),
                values.as_ptr(),
                1,
                &raw const kCFTypeArrayCallBacks,
            )
        })?;
        let mut context = FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(shared).cast_mut().cast(),
            retain: Some(retain_info),
            release: Some(release_info),
            copy_description: None,
        };
        // SAFETY: the callback has the FSEventStreamCallback signature; the
        // context is read during the call, its `info` retained through
        // `retain_info`; `paths` is a live CFArray of one CFString.
        let raw = unsafe {
            FSEventStreamCreateRelativeToDevice(
                std::ptr::null(),
                on_events,
                &raw mut context,
                dev,
                paths.as_ptr(),
                since,
                0.0,
                FULL_HISTORY,
            )
        };
        let raw = NonNull::new(raw)?;
        // SAFETY: `raw` is the stream just created, not yet started, and
        // `queue` a live serial queue.
        unsafe { FSEventStreamSetDispatchQueue(raw.as_ptr(), queue) };
        Some(Stream {
            raw,
            started: false,
            _paths: paths,
            _path: path,
        })
    }

    /// Starts the stream; `false` when it does not start.
    fn start(&mut self) -> bool {
        // SAFETY: the stream is scheduled on the queue and not started.
        self.started = unsafe { FSEventStreamStart(self.raw.as_ptr()) } != 0;
        self.started
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream is scheduled on the queue, as Invalidate
        // requires; it is stopped only if it started, and once Stop returns
        // its callback is not called again (FSEvents.h). Release drops the
        // one reference the create call returned.
        unsafe {
            if self.started {
                FSEventStreamStop(self.raw.as_ptr());
            }
            FSEventStreamInvalidate(self.raw.as_ptr());
            FSEventStreamRelease(self.raw.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests;
