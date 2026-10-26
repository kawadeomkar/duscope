//! Builds a tree from a plan.
//!
//! The build is deterministic: directories are created in plan order, the
//! files of each directory by one worker in plan order (so even filesystems
//! whose `readdir` order follows insertion list them identically on every
//! run, and an ext4 directory ends up the same size, which it does not when
//! its entries are inserted concurrently), the special entries after them,
//! and every mode and timestamp is set explicitly from the plan, never from
//! the umask or the clock. Directory times are set last, children before
//! parents.
//!
//! Cases a platform or filesystem cannot build are recorded as skipped with
//! the reason and what stands in for them, never left silently missing.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes, OpenOptions, Permissions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::Instant;

use crate::plan::{Entry, Kind, Plan};
use crate::record::{CaseRecord, Record, Status};
use crate::{Error, Result, manifest::encode};

/// Arguments per `touch`/`chmod` invocation.
const BATCH: usize = 200;

/// Chunk size for writing generated content.
const CHUNK: usize = 64 * 1024;

/// A directory with at least this many files is built by a pipeline: its
/// worker only creates the files, still one by one in plan order, and
/// helpers write their contents and set their modes and times. Creating is
/// about half the cost of a file, so the 100,000-entry directory, the
/// build's longest single job, takes about half as long.
const PIPELINE_MIN: usize = 4096;

/// Helper threads per pipelined directory.
const HELPERS: usize = 3;

/// Created files a pipeline holds open while they wait for a helper.
const IN_FLIGHT: usize = 64;

/// The absolute path of a tree-relative path.
pub fn abs(root: &Path, rel: &[u8]) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(OsStr::from_bytes(rel))
    }
}

/// The line generated content repeats: ASCII text naming the file, so the
/// content is compressible (for the compressed case and for CI caches) and
/// identifies its file. Changing it means bumping
/// [`crate::plan::CONTENT_VERSION`].
fn pattern(path: &[u8]) -> Vec<u8> {
    format!("duscope fixture {}\n", encode(path)).into_bytes()
}

/// `len` bytes of the generated content of `path` starting at `offset`.
pub fn content(path: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let line = pattern(path);
    let n = line.len() as u64;
    (0..len as u64)
        .map(|i| line[usize::try_from((offset + i) % n).unwrap_or(0)])
        .collect()
}

fn write_generated(file: &mut File, path: &[u8], size: u64, buf: &mut Vec<u8>) -> io::Result<()> {
    let line = pattern(path);
    buf.clear();
    for _ in 0..(CHUNK / line.len()).max(1) {
        buf.extend_from_slice(&line);
    }
    let mut left = size;
    while left > 0 {
        let n = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        file.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(())
}

fn times(e: &Entry) -> FileTimes {
    FileTimes::new()
        .set_modified(e.mtime.system_time())
        .set_accessed(e.atime.system_time())
}

fn io_err(what: &str, path: &Path, err: &io::Error) -> Error {
    Error::new(format!("{what} {}: {err}", path.display()))
}

/// Why a tool could not be started, for skip reasons.
fn spawn_error(err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::NotFound {
        "not available".to_owned()
    } else {
        err.to_string()
    }
}

/// Whether a file of `size` bytes with `allocated` bytes on disk is stored
/// compressed: in less than half its size, which the generated text reaches
/// with every filesystem compressor.
fn stored_compressed(allocated: u64, size: u64) -> bool {
    allocated * 2 < size.max(1)
}

/// The first line of a command's stderr, for skip reasons.
fn stderr_line(out: &Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        format!("exit status {}", out.status)
    } else {
        line.to_owned()
    }
}

/// The filesystem type of the volume holding `path`, as the system names it
/// (`apfs`, `ext4`, `overlay`, `btrfs`, …), from `df -P` and `mount`.
pub fn filesystem_type(path: &Path) -> String {
    let probe = || -> Option<String> {
        let df = Command::new("df").arg("-P").arg(path).output().ok()?;
        let text = String::from_utf8_lossy(&df.stdout);
        let line = text.lines().nth(1)?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        let cap = fields.iter().position(|f| f.ends_with('%'))?;
        let mount_point = fields.get(cap + 1..)?.join(" ");
        let mounts = Command::new("mount").output().ok()?;
        let needle = format!(" on {mount_point} ");
        let mounts = String::from_utf8_lossy(&mounts.stdout);
        // The last mount on a mount point is the visible one.
        let rest = mounts
            .lines()
            .filter_map(|l| l.split_once(&needle).map(|(_, r)| r.to_owned()))
            .next_back()?;
        // Linux: "type ext4 (rw,…)"; macOS: "(apfs, local, …)".
        let kind = match rest.strip_prefix("type ") {
            Some(r) => r.split_whitespace().next()?.to_owned(),
            None => rest
                .trim_start_matches('(')
                .split([',', ')'])
                .next()?
                .trim()
                .to_owned(),
        };
        Some(kind)
    };
    probe().unwrap_or_else(|| "unknown".to_owned())
}

/// Per-case outcome while building.
#[derive(Default)]
struct Outcome {
    built: Vec<String>,
    skipped: Vec<(String, String)>,
    absent: Vec<String>,
}

struct Builder<'p> {
    plan: &'p Plan,
    dest: &'p Path,
    scratch: &'p Path,
    filesystem: String,
    absent: Vec<bool>,
    outcomes: BTreeMap<usize, Outcome>,
}

impl Builder<'_> {
    fn path(&self, e: &Entry) -> PathBuf {
        abs(self.dest, &e.path)
    }

    fn outcome(&mut self, e: &Entry) -> &mut Outcome {
        self.outcomes
            .entry(e.case.unwrap_or(usize::MAX))
            .or_default()
    }

    fn built(&mut self, e: &Entry, detail: String) {
        let outcome = self.outcome(e);
        if !outcome.built.contains(&detail) {
            outcome.built.push(detail);
        }
    }

    fn skipped(&mut self, e: &Entry, reason: &str, fallback: &str) {
        // Tool messages name the half-built tree, whose path changes with
        // every build; the record must not.
        let reason = reason.replace(&*self.dest.to_string_lossy(), "<tree>");
        let outcome = self.outcome(e);
        let item = (reason, fallback.to_owned());
        if !outcome.skipped.contains(&item) {
            outcome.skipped.push(item);
        }
    }

    /// Records an entry the filesystem refused to create. Only names that
    /// are not valid UTF-8 may be refused; any other failure is an error.
    fn refuse(&mut self, i: usize, err: &io::Error) -> Result<()> {
        let e = &self.plan.entries[i];
        if e.utf8() {
            return Err(io_err("create", &self.path(e), err));
        }
        self.absent[i] = true;
        // An entry below a refused directory fails for want of its parent;
        // only the first refusal says why.
        let parent_absent = self
            .plan
            .position(e.parent())
            .is_some_and(|j| self.absent[j]);
        if !parent_absent {
            let reason = format!("the filesystem refuses names that are not valid UTF-8 ({err})");
            self.skipped(e, &reason, "entries absent");
        }
        let path = e.display();
        self.outcome(e).absent.push(path);
        Ok(())
    }

    fn directories(&mut self) -> Result<()> {
        for i in 0..self.plan.entries.len() {
            let e = &self.plan.entries[i];
            if e.kind != Kind::Dir {
                continue;
            }
            if let Err(err) = fs::create_dir(self.path(e)) {
                self.refuse(i, &err)?;
            }
        }
        Ok(())
    }

    /// Regular files, grouped by directory: one worker creates all files of
    /// a directory in plan order (helped by a pipeline in a big directory);
    /// the biggest directory starts first.
    fn files(&mut self) -> Result<()> {
        let mut groups: BTreeMap<&[u8], Vec<usize>> = BTreeMap::new();
        for (i, e) in self.plan.entries.iter().enumerate() {
            if matches!(e.kind, Kind::File { .. }) {
                groups.entry(e.parent()).or_default().push(i);
            }
        }
        let mut jobs: Vec<Vec<usize>> = groups.into_values().collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.len()));
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
        let next = AtomicUsize::new(0);
        let failed: Mutex<Vec<(usize, io::Error)>> = Mutex::new(Vec::new());
        let (plan, dest) = (self.plan, self.dest);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    let mut buf = Vec::with_capacity(CHUNK);
                    while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if job.len() >= PIPELINE_MIN {
                            create_files_pipelined(plan, dest, job, &failed);
                            continue;
                        }
                        for &i in job {
                            if let Err(err) = create_file(plan, dest, i, &mut buf) {
                                let mut failed =
                                    failed.lock().unwrap_or_else(PoisonError::into_inner);
                                failed.push((i, err));
                            }
                        }
                    }
                });
            }
        });
        let mut failed = failed.into_inner().unwrap_or_else(PoisonError::into_inner);
        failed.sort_by_key(|(i, _)| *i);
        for (i, err) in failed {
            self.refuse(i, &err)?;
        }
        Ok(())
    }

    /// Sparse, compressed, cloned and special entries, then hard links.
    fn specials(&mut self) -> Result<()> {
        for pass in 0..2 {
            for i in 0..self.plan.entries.len() {
                let e = &self.plan.entries[i];
                let links = matches!(e.kind, Kind::Hardlink { .. });
                if (pass == 0) == links || matches!(e.kind, Kind::Dir | Kind::File { .. }) {
                    continue;
                }
                if let Err(err) = self.special(i) {
                    match err {
                        Special::Io(err) => self.refuse(i, &err)?,
                        Special::Other(err) => return Err(err),
                    }
                }
            }
        }
        Ok(())
    }

    fn special(&mut self, i: usize) -> std::result::Result<(), Special> {
        let plan = self.plan;
        let e = &plan.entries[i];
        let path = self.path(e);
        match &e.kind {
            Kind::Sparse { size, data } => {
                // Extend with ftruncate, then write the extents into the
                // hole. APFS zero-fills holes smaller than 16 MiB, so the
                // manifest keeps every hole larger.
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                f.set_len(*size)?;
                for &(offset, len) in data {
                    f.seek(SeekFrom::Start(offset))?;
                    f.write_all(&content(&e.path, offset, usize::try_from(len).unwrap_or(0)))?;
                }
                f.set_permissions(Permissions::from_mode(e.mode))?;
                f.set_times(times(e))?;
                drop(f);
                let allocated = fs::symlink_metadata(&path)?.blocks() * 512;
                if allocated < *size {
                    self.built(e, "holes kept".to_owned());
                } else {
                    let reason =
                        format!("the filesystem allocated the holes ({allocated} of {size} bytes)");
                    self.skipped(e, &reason, "fully allocated files");
                }
            }
            Kind::Compressed { size } => {
                let (ok, detail) = self.compressed(e, &path, *size)?;
                set_meta(&path, e)?;
                if ok {
                    self.built(e, detail);
                } else {
                    self.skipped(e, &detail, "uncompressed files");
                }
            }
            Kind::Clone { source } => {
                let (ok, detail) = self.clone_file(&abs(self.dest, source), &path)?;
                set_meta(&path, e)?;
                if ok {
                    self.built(e, detail);
                } else {
                    self.skipped(e, &detail, "plain copies");
                }
            }
            Kind::Hardlink { target } => fs::hard_link(abs(self.dest, target), &path)?,
            Kind::Symlink { target } => {
                std::os::unix::fs::symlink(OsStr::from_bytes(target), &path)?;
            }
            Kind::Fifo => {
                let out = Command::new("mkfifo")
                    .arg("-m")
                    .arg(format!("{:o}", e.mode))
                    .arg(&path)
                    .output()?;
                if !out.status.success() {
                    return Err(Special::Other(Error::new(format!(
                        "mkfifo {}: {}",
                        path.display(),
                        stderr_line(&out)
                    ))));
                }
                fs::set_permissions(&path, Permissions::from_mode(e.mode))?;
            }
            Kind::Socket => {
                if let Some(reason) = Self::socket(&path)? {
                    self.absent[i] = true;
                    let display = e.display();
                    self.skipped(e, &reason, "entries absent");
                    self.outcome(e).absent.push(display);
                } else {
                    fs::set_permissions(&path, Permissions::from_mode(e.mode))?;
                }
            }
            Kind::Dir | Kind::File { .. } => {}
        }
        Ok(())
    }

    /// macOS: `ditto --hfsCompression`, kept only if the file really is
    /// stored compressed. Elsewhere: `chattr +c` (btrfs and others), else an
    /// uncompressed file.
    fn compressed(&self, e: &Entry, path: &Path, size: u64) -> io::Result<(bool, String)> {
        let src = self.scratch.join("compress-source");
        let mut f = File::create(&src)?;
        write_generated(&mut f, &e.path, size, &mut Vec::new())?;
        drop(f);
        let mut reasons = Vec::new();
        match Command::new("ditto")
            .arg("--hfsCompression")
            .arg(&src)
            .arg(path)
            .output()
        {
            Ok(out) if out.status.success() => {
                let allocated = fs::symlink_metadata(path)?.blocks() * 512;
                if stored_compressed(allocated, size) {
                    return Ok((true, "ditto --hfsCompression".to_owned()));
                }
                reasons.push(format!(
                    "ditto --hfsCompression left {allocated} of {size} bytes allocated"
                ));
            }
            Ok(out) => reasons.push(format!("ditto: {}", stderr_line(&out))),
            Err(err) => reasons.push(format!("ditto: {}", spawn_error(&err))),
        }
        if fs::symlink_metadata(path).is_ok() {
            fs::remove_file(path)?;
        }
        File::create(path)?;
        let chattr = Command::new("chattr").arg("+c").arg(path).output();
        let flagged = match chattr {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                reasons.push(format!("chattr +c: {}", stderr_line(&out)));
                false
            }
            Err(err) => {
                reasons.push(format!("chattr: {}", spawn_error(&err)));
                false
            }
        };
        let mut f = OpenOptions::new().write(true).truncate(true).open(path)?;
        write_generated(&mut f, &e.path, size, &mut Vec::new())?;
        if flagged {
            // btrfs compresses at writeback, so the allocation is read after
            // a sync. ext4 keeps the flag (statx reports
            // STATX_ATTR_COMPRESSED) and stores the data as it is.
            f.sync_all()?;
            drop(f);
            let allocated = fs::symlink_metadata(path)?.blocks() * 512;
            if stored_compressed(allocated, size) {
                return Ok((true, format!("chattr +c on {}", self.filesystem)));
            }
            reasons.push(format!(
                "chattr +c set the compressed attribute, but {} left {allocated} of {size} bytes allocated",
                self.filesystem
            ));
            // The fallback is a plain file, without the attribute.
            fs::remove_file(path)?;
            let mut f = File::create(path)?;
            write_generated(&mut f, &e.path, size, &mut Vec::new())?;
        }
        Ok((false, reasons.join("; ")))
    }

    /// `cp -c` on APFS (it falls back to a plain copy silently elsewhere, so
    /// it is used only where `clonefile` works), `cp --reflink=always` on
    /// other filesystems, and a plain copy when neither can clone.
    fn clone_file(&self, src: &Path, dst: &Path) -> io::Result<(bool, String)> {
        let reason;
        if self.filesystem == "apfs" {
            let out = Command::new("cp").arg("-c").arg(src).arg(dst).output()?;
            if out.status.success() {
                return Ok((true, "cp -c (clonefile on apfs)".to_owned()));
            }
            reason = format!("cp -c: {}", stderr_line(&out));
        } else {
            match Command::new("cp")
                .arg("--reflink=always")
                .arg(src)
                .arg(dst)
                .output()
            {
                Ok(out) if out.status.success() => {
                    return Ok((true, "cp --reflink=always".to_owned()));
                }
                Ok(out) => {
                    // "cp: failed to clone 'a' from 'b': <why>": keep <why>.
                    let line = stderr_line(&out);
                    let why = line.rsplit(": ").next().unwrap_or(&line);
                    reason = format!(
                        "{} cannot clone (cp --reflink=always: {why})",
                        self.filesystem
                    );
                }
                Err(err) => reason = format!("cp: {}", spawn_error(&err)),
            }
        }
        if fs::symlink_metadata(dst).is_ok() {
            fs::remove_file(dst)?;
        }
        fs::copy(src, dst)?;
        Ok((false, reason))
    }

    /// Binds a Unix socket at `path`. A path longer than `sun_path` is bound
    /// in the temporary directory and renamed into place; returns a skip
    /// reason when that is impossible (another filesystem).
    fn socket(path: &Path) -> io::Result<Option<String>> {
        use std::os::unix::net::UnixListener;
        match UnixListener::bind(path) {
            Ok(_) => return Ok(None),
            Err(err) if err.kind() != io::ErrorKind::InvalidInput => return Err(err),
            Err(_) => {}
        }
        let short = std::env::temp_dir().join(format!("mktree-{}.sock", std::process::id()));
        let _ = fs::remove_file(&short);
        drop(UnixListener::bind(&short)?);
        match fs::rename(&short, path) {
            Ok(()) => Ok(None),
            Err(err) => {
                let _ = fs::remove_file(&short);
                Ok(Some(format!(
                    "the socket path is longer than sun_path and the temporary directory is on another filesystem ({err})"
                )))
            }
        }
    }

    /// Times of symlinks, FIFOs and sockets (through `touch -h`, which never
    /// opens or follows them), symlink modes where the system keeps one, and
    /// the times of clone sources, which copying may have read.
    fn finish_specials(&mut self) -> Result<()> {
        let mut modified: BTreeMap<i64, Vec<PathBuf>> = BTreeMap::new();
        let mut accessed: BTreeMap<i64, Vec<PathBuf>> = BTreeMap::new();
        let mut symlinks = Vec::new();
        for (i, e) in self.plan.entries.iter().enumerate() {
            if self.absent[i] {
                continue;
            }
            match &e.kind {
                Kind::Symlink { .. } | Kind::Fifo | Kind::Socket => {
                    modified.entry(e.mtime.secs).or_default().push(self.path(e));
                    accessed.entry(e.atime.secs).or_default().push(self.path(e));
                    if matches!(e.kind, Kind::Symlink { .. }) {
                        symlinks.push(self.path(e));
                    }
                }
                Kind::Clone { source } => {
                    let src = self
                        .plan
                        .get(source)
                        .ok_or_else(|| Error::new("clone source"))?;
                    let p = self.path(src);
                    File::open(&p)
                        .and_then(|f| f.set_times(times(src)))
                        .map_err(|err| io_err("set times", &p, &err))?;
                }
                _ => {}
            }
        }
        // macOS keeps a mode per symlink, taken from the umask: normalise it
        // to 0755 with `chmod -h`. Linux shows 0777 for every symlink and
        // has no `chmod -h`; the first refusal ends the attempt.
        for chunk in symlinks.chunks(BATCH) {
            let changed = Command::new("chmod")
                .arg("-h")
                .arg("755")
                .args(chunk)
                .output()
                .is_ok_and(|out| out.status.success());
            if !changed {
                break;
            }
        }
        for (flag, groups) in [("-m", modified), ("-a", accessed)] {
            for (secs, paths) in groups {
                for chunk in paths.chunks(BATCH) {
                    run(Command::new("touch")
                        .env("TZ", "UTC")
                        .args(["-h", "-c", flag, "-t"])
                        .arg(crate::time::touch_stamp(secs))
                        .args(chunk))?;
                }
            }
        }
        Ok(())
    }

    /// Directory modes and times, children before parents.
    fn finish_directories(&self) -> Result<()> {
        for (i, e) in self.plan.entries.iter().enumerate().rev() {
            if e.kind != Kind::Dir || self.absent[i] {
                continue;
            }
            let p = self.path(e);
            fs::set_permissions(&p, Permissions::from_mode(e.mode))
                .and_then(|()| File::open(&p))
                .and_then(|f| f.set_times(times(e)))
                .map_err(|err| io_err("set directory mode and times", &p, &err))?;
        }
        Ok(())
    }

    fn record(self, unreadable_effective: bool, started: Instant) -> Record {
        let plan = self.plan;
        let mut outcomes = self.outcomes;
        let armed: Vec<usize> = plan
            .entries
            .iter()
            .filter(|e| e.test_mode.is_some())
            .filter_map(|e| e.case)
            .collect();
        let cases = plan
            .cases
            .iter()
            .enumerate()
            .map(|(ci, case)| {
                let mut outcome = outcomes.remove(&ci).unwrap_or_default();
                if armed.contains(&ci) && !unreadable_effective {
                    outcome.skipped.push((
                        "the build runs with privileges that bypass permission bits (root), so mode 000 denies nothing"
                            .to_owned(),
                        "directories readable despite their test mode".to_owned(),
                    ));
                }
                if outcome.skipped.is_empty() {
                    CaseRecord {
                        name: case.name.clone(),
                        status: Status::Built,
                        detail: outcome.built.join("; "),
                        fallback: String::new(),
                        absent: Vec::new(),
                    }
                } else {
                    let (reasons, fallbacks): (Vec<String>, Vec<String>) =
                        outcome.skipped.into_iter().unzip();
                    let mut fallbacks = fallbacks;
                    fallbacks.dedup();
                    CaseRecord {
                        name: case.name.clone(),
                        status: Status::Skipped,
                        detail: reasons.join("; "),
                        fallback: fallbacks.join("; "),
                        absent: outcome.absent,
                    }
                }
            })
            .collect();
        let present = self.absent.iter().filter(|a| !**a).count();
        Record {
            hash: plan.hash(),
            instant: plan.instant_text(),
            generator: format!("mktree {}", env!("CARGO_PKG_VERSION")),
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            filesystem: self.filesystem,
            entries: present as u64,
            build_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            unreadable_effective,
            cases,
        }
    }
}

enum Special {
    Io(io::Error),
    Other(Error),
}

impl From<io::Error> for Special {
    fn from(err: io::Error) -> Special {
        Special::Io(err)
    }
}

fn create_file(plan: &Plan, dest: &Path, i: usize, buf: &mut Vec<u8>) -> io::Result<()> {
    fill_file(plan, i, open_new(plan, dest, i)?, buf)
}

/// Creates the files of one directory in plan order on this thread while
/// `HELPERS` threads fill them, so the order of insertion stays the plan's.
fn create_files_pipelined(
    plan: &Plan,
    dest: &Path,
    job: &[usize],
    failed: &Mutex<Vec<(usize, io::Error)>>,
) {
    let fail = |i: usize, err: io::Error| {
        failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((i, err));
    };
    let (created, waiting) = mpsc::sync_channel::<(usize, File)>(IN_FLIGHT);
    let waiting = Mutex::new(waiting);
    std::thread::scope(|scope| {
        for _ in 0..HELPERS {
            scope.spawn(|| {
                let mut buf = Vec::with_capacity(CHUNK);
                loop {
                    let next = waiting
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .recv();
                    let Ok((i, f)) = next else { break };
                    if let Err(err) = fill_file(plan, i, f, &mut buf) {
                        fail(i, err);
                    }
                }
            });
        }
        for &i in job {
            match open_new(plan, dest, i) {
                // The helpers outlive the sender, so a send cannot fail.
                Ok(f) => {
                    let _ = created.send((i, f));
                }
                Err(err) => fail(i, err),
            }
        }
        drop(created);
    });
}

fn open_new(plan: &Plan, dest: &Path, i: usize) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(abs(dest, &plan.entries[i].path))
}

/// Writes the contents of a newly created file and sets its mode and times.
fn fill_file(plan: &Plan, i: usize, mut f: File, buf: &mut Vec<u8>) -> io::Result<()> {
    let e = &plan.entries[i];
    match &e.kind {
        Kind::File {
            text: Some(text), ..
        } => f.write_all(text)?,
        Kind::File { size, .. } => write_generated(&mut f, &e.path, *size, buf)?,
        _ => {}
    }
    f.set_permissions(Permissions::from_mode(e.mode))?;
    f.set_times(times(e))
}

fn set_meta(path: &Path, e: &Entry) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(e.mode))?;
    File::open(path)?.set_times(times(e))
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd
        .output()
        .map_err(|err| Error::new(format!("{cmd:?}: {err}")))?;
    if out.status.success() {
        Ok(())
    } else {
        let program: OsString = cmd.get_program().to_owned();
        Err(Error::new(format!(
            "{} failed: {}",
            program.to_string_lossy(),
            stderr_line(&out)
        )))
    }
}

/// Whether mode 000 denies this process access to a directory (false under
/// root or `CAP_DAC_OVERRIDE`). Probed in `scratch`.
pub fn mode_000_denies(scratch: &Path) -> Result<bool> {
    let probe = scratch.join("unreadable-probe");
    fs::create_dir_all(&probe).map_err(|err| io_err("create", &probe, &err))?;
    fs::set_permissions(&probe, Permissions::from_mode(0o000))
        .map_err(|err| io_err("chmod", &probe, &err))?;
    let denied = fs::read_dir(&probe).is_err();
    fs::set_permissions(&probe, Permissions::from_mode(0o700))
        .map_err(|err| io_err("chmod", &probe, &err))?;
    fs::remove_dir(&probe).map_err(|err| io_err("remove", &probe, &err))?;
    Ok(denied)
}

/// Builds the tree of `plan` at `dest`, which must not exist, using
/// `scratch` (on the same filesystem, outside the tree) for temporary files.
/// Returns the record of the build.
pub fn build_tree(plan: &Plan, dest: &Path, scratch: &Path) -> Result<Record> {
    let started = Instant::now();
    if fs::symlink_metadata(dest).is_ok() {
        return Err(Error::new(format!("{} already exists", dest.display())));
    }
    fs::create_dir_all(scratch).map_err(|err| io_err("create", scratch, &err))?;
    let filesystem = filesystem_type(scratch);
    let mut builder = Builder {
        plan,
        dest,
        scratch,
        filesystem,
        absent: vec![false; plan.entries.len()],
        outcomes: BTreeMap::new(),
    };
    builder.directories()?;
    builder.files()?;
    builder.specials()?;
    builder.finish_specials()?;
    builder.finish_directories()?;
    let effective = mode_000_denies(scratch)?;
    let _ = fs::remove_file(scratch.join("compress-source"));
    Ok(builder.record(effective, started))
}
