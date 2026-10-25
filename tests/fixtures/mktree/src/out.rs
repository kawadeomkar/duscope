//! The output directory (`target/fixtures.noindex`): the tree, its build
//! record, the build lock and the registry of test processes holding the tree.
//!
//! ```text
//! target/fixtures.noindex/
//!   tree/          the reference tree tests scan
//!   record.toml    the build record (manifest hash, skipped cases)
//!   holders/<pid>  processes holding the test modes (unreadable directories)
//!   .lock/         the lock serialising builds and test-mode changes
//!   .mktree        the marker that lets a rebuild replace what is here
//! ```
//!
//! The `.noindex` suffix keeps Spotlight out of the tree on macOS. Its
//! importers read files soon after they are written, and macOS moves the
//! atime of a file that is read even when the planned atime lies in the
//! future: within four minutes of a build, 270 of 400 sampled files had
//! moved, and none below a `.noindex` directory.
//!
//! Unreadable directories are kept readable at rest, so `cargo clean`, `rm
//! -rf` and CI caches (tar) can always walk the tree, and get their test
//! mode (000, 0444) only while a test environment holds the tree.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use crate::build::{abs, build_tree};
use crate::plan::Plan;
use crate::record::Record;
use crate::{Error, Result};

/// How long to wait for another process's build before giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(900);

/// The file that marks a directory as a mktree output directory.
const MARKER: &str = ".mktree";

fn io_err(what: &str, path: &Path, err: &io::Error) -> Error {
    Error::new(format!("{what} {}: {err}", path.display()))
}

/// The output directory of the fixture tree.
#[derive(Clone, Debug)]
pub struct OutDir {
    path: PathBuf,
    /// The directory is mktree's by its name (the workspace default), so a
    /// build may replace whatever it holds.
    reserved: bool,
}

impl OutDir {
    /// An output directory at `path`.
    pub fn new(path: impl Into<PathBuf>) -> OutDir {
        OutDir {
            path: path.into(),
            reserved: false,
        }
    }

    /// `target/fixtures.noindex` of this workspace:
    /// `$CARGO_TARGET_DIR/fixtures.noindex` when that is set (relative to the
    /// workspace root), else `<workspace>/target/fixtures.noindex`. `just
    /// fixtures` and the test kit both use it.
    ///
    /// It is reserved for mktree by its name, so a build replaces whatever it
    /// holds without the marker. A cache of `target/` can keep the tree and
    /// lose its files: rust-cache, before CI saves `target/`, deletes every
    /// file it does not take for a build artifact, the marker and the record
    /// with them, and keeps the directories.
    pub fn workspace_default() -> OutDir {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir.ancestors().nth(3).unwrap_or(manifest_dir);
        let target = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(dir) => workspace.join(dir),
            None => workspace.join("target"),
        };
        OutDir {
            path: target.join("fixtures.noindex"),
            reserved: true,
        }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tree's root.
    pub fn tree(&self) -> PathBuf {
        self.path.join("tree")
    }

    /// The build record's path.
    pub fn record_path(&self) -> PathBuf {
        self.path.join("record.toml")
    }

    fn holders(&self) -> PathBuf {
        self.path.join("holders")
    }

    /// The record of the current build, if there is one.
    pub fn record(&self) -> Result<Option<Record>> {
        match fs::read_to_string(self.record_path()) {
            Ok(text) => Record::parse(&text).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io_err("read", &self.record_path(), &err)),
        }
    }

    fn lock(&self) -> Result<DirLock> {
        fs::create_dir_all(&self.path).map_err(|err| io_err("create", &self.path, &err))?;
        DirLock::acquire(self.path.join(".lock"))
    }

    /// Makes sure a build may replace what the directory holds: it is the
    /// workspace default, carries the marker, holds a build record (a
    /// directory built before the marker existed), or holds nothing but the
    /// lock. A build deletes `tree/`, `holders/` and the `.tmp-*` and `.old-*`
    /// entries below it, so `--out` naming an unrelated directory must stop
    /// here. Called under the lock.
    fn claim(&self) -> Result<()> {
        let marker = self.path.join(MARKER);
        if marker.is_file() {
            return Ok(());
        }
        let entries =
            fs::read_dir(&self.path).map_err(|err| io_err("read_dir", &self.path, &err))?;
        let empty = entries
            .flatten()
            .all(|e| e.file_name() == MARKER || e.file_name() == ".lock");
        if !self.reserved && !empty && !matches!(self.record(), Ok(Some(_))) {
            return Err(Error::new(format!(
                "{} is not a mktree output directory (it holds no {MARKER} and no \
                 record.toml, and is not empty): pass a new or empty directory",
                self.path.display()
            )));
        }
        fs::write(&marker, b"mktree output directory\n")
            .map_err(|err| io_err("write", &marker, &err))
    }

    /// Builds the tree unless the current one was built from the same
    /// manifest hash (or `force` is set). The new tree is built beside the
    /// old one and swapped in; the record is written last, so a tree without
    /// a matching record is never mistaken for a finished one. Returns the
    /// record and whether a build ran.
    pub fn ensure(&self, plan: &Plan, force: bool) -> Result<(Record, bool)> {
        let _lock = self.lock()?;
        self.claim()?;
        let hash = plan.hash();
        // A record this generator cannot read (another format) means rebuild.
        if !force
            && let Ok(Some(record)) = self.record()
            && record.hash == hash
            && self.tree().is_dir()
        {
            return Ok((record, false));
        }
        self.remove_leftovers()?;
        let pid = std::process::id();
        let tmp = self.path.join(format!(".tmp-{pid}"));
        let record = build_tree(plan, &tmp.join("tree"), &tmp.join("scratch"))?;
        let tmp_record = tmp.join("record.toml");
        fs::write(&tmp_record, record.to_toml())
            .map_err(|err| io_err("write", &tmp_record, &err))?;
        match fs::remove_file(self.record_path()) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err("remove", &self.record_path(), &err)),
        }
        if fs::symlink_metadata(self.tree()).is_ok() {
            let old = self.path.join(format!(".old-{pid}"));
            fs::rename(self.tree(), &old).map_err(|err| io_err("rename", &self.tree(), &err))?;
            remove_tree(&old)?;
        }
        remove_tree(&self.holders())?;
        fs::rename(tmp.join("tree"), self.tree()).map_err(|err| io_err("rename", &tmp, &err))?;
        fs::rename(&tmp_record, self.record_path())
            .map_err(|err| io_err("rename", &tmp_record, &err))?;
        remove_tree(&tmp)?;
        Ok((record, true))
    }

    /// Removes half-built trees left by an interrupted build.
    fn remove_leftovers(&self) -> Result<()> {
        let entries =
            fs::read_dir(&self.path).map_err(|err| io_err("read_dir", &self.path, &err))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".tmp-") || name.starts_with(".old-") {
                remove_tree(&entry.path())?;
            }
        }
        Ok(())
    }

    /// Registers this process as a holder and applies the test modes.
    pub fn hold(&self, plan: &Plan) -> Result<()> {
        let _lock = self.lock()?;
        let holders = self.holders();
        fs::create_dir_all(&holders).map_err(|err| io_err("create", &holders, &err))?;
        let me = holders.join(std::process::id().to_string());
        fs::write(&me, b"").map_err(|err| io_err("write", &me, &err))?;
        arm(&self.tree(), plan)
    }

    /// Unregisters this process; restores the modes at rest when no live
    /// holder remains.
    pub fn release(&self, plan: &Plan) -> Result<()> {
        let _lock = self.lock()?;
        let holders = self.holders();
        let _ = fs::remove_file(holders.join(std::process::id().to_string()));
        let mut live = 0;
        if let Ok(entries) = fs::read_dir(&holders) {
            for entry in entries.flatten() {
                if alive(&entry.file_name().to_string_lossy()) {
                    live += 1;
                } else {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        if live == 0 {
            disarm(&self.tree(), plan)?;
        }
        Ok(())
    }

    /// Applies the test modes without registering a holder (`mktree --arm`).
    pub fn arm(&self, plan: &Plan) -> Result<()> {
        let _lock = self.lock()?;
        arm(&self.tree(), plan)
    }

    /// Restores the modes at rest and forgets every holder (`mktree
    /// --disarm`, for a tree left armed by a crashed test process).
    pub fn disarm(&self, plan: &Plan) -> Result<()> {
        let _lock = self.lock()?;
        remove_tree(&self.holders())?;
        disarm(&self.tree(), plan)
    }
}

/// Applies the test modes of `plan` to the tree at `root`: restores the
/// modes at rest from the top first (so nested entries stay reachable), then
/// applies the test modes from the bottom up.
pub fn arm(root: &Path, plan: &Plan) -> Result<()> {
    disarm(root, plan)?;
    for e in plan.entries.iter().rev() {
        if let Some(mode) = e.test_mode {
            let path = abs(root, &e.path);
            fs::set_permissions(&path, Permissions::from_mode(mode))
                .map_err(|err| io_err("chmod", &path, &err))?;
        }
    }
    Ok(())
}

/// Restores the modes at rest of every entry that has a test mode.
pub fn disarm(root: &Path, plan: &Plan) -> Result<()> {
    for e in &plan.entries {
        if e.test_mode.is_some() {
            let path = abs(root, &e.path);
            fs::set_permissions(&path, Permissions::from_mode(e.mode))
                .map_err(|err| io_err("chmod", &path, &err))?;
        }
    }
    Ok(())
}

/// Gives the owner `rwx` on every directory under `path` that lacks it, so
/// the tree can be removed.
pub fn make_removable(path: &Path) -> Result<()> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.is_dir() {
        return Ok(());
    }
    if meta.mode() & 0o700 != 0o700 {
        fs::set_permissions(path, Permissions::from_mode(meta.mode() & 0o7777 | 0o700))
            .map_err(|err| io_err("chmod", path, &err))?;
    }
    let entries = fs::read_dir(path).map_err(|err| io_err("read_dir", path, &err))?;
    for entry in entries {
        let entry = entry.map_err(|err| io_err("read_dir", path, &err))?;
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            make_removable(&entry.path())?;
        }
    }
    Ok(())
}

/// Removes a tree, restoring permissions first where a directory denies
/// its owner (an armed fixture tree, a test's unreadable directory).
pub fn remove_tree(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    if fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    make_removable(path)?;
    fs::remove_dir_all(path).map_err(|err| io_err("remove", path, &err))
}

/// Whether a process id is alive (`kill -0`).
fn alive(pid: &str) -> bool {
    pid.parse::<u32>().is_ok()
        && Command::new("sh")
            .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", pid])
            .status()
            .is_ok_and(|s| s.success())
}

/// A lock directory holding the owner's pid; a lock whose owner died is
/// taken over.
struct DirLock {
    path: PathBuf,
}

impl DirLock {
    fn acquire(path: PathBuf) -> Result<DirLock> {
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let pid = path.join("pid");
                    fs::write(&pid, std::process::id().to_string())
                        .map_err(|err| io_err("write", &pid, &err))?;
                    return Ok(DirLock { path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if DirLock::stale(&path) {
                        let _ = fs::remove_dir_all(&path);
                        continue;
                    }
                    if Instant::now() > deadline {
                        return Err(Error::new(format!(
                            "timed out waiting for {} (remove it if no build is running)",
                            path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(io_err("create", &path, &err)),
            }
        }
    }

    fn stale(path: &Path) -> bool {
        match fs::read_to_string(path.join("pid")) {
            Ok(pid) => !alive(pid.trim()),
            // Created but the pid not yet written: stale only if old.
            Err(_) => fs::symlink_metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age > Duration::from_secs(10)),
        }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn small_plan() -> Plan {
        let text = r#"
format = 1
hash = ""
instant = "2030-01-01T00:00:00Z"
words = ["alpha"]
[defaults]
dir_mode = 0o755
file_mode = 0o644
age = "30d"
[extensions]
code = ["rs"]
[[case]]
name = "c"
about = "a file and a symlink in nested directories"
root = "cases/c"
entries = [
  { path = "a/b/file", file = "4K" },
  { path = "link", symlink = "a/b/file" },
]
"#;
        Plan::new(&Manifest::parse(text).unwrap()).unwrap()
    }

    /// Deletes every regular file below `dir` and keeps the directories and
    /// symlinks, as rust-cache cleans `target/` before CI saves it.
    fn strip_files(dir: &Path) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                strip_files(&entry.path());
            } else if kind.is_file() {
                fs::remove_file(entry.path()).unwrap();
            }
        }
    }

    #[test]
    fn p0_test_01_workspace_default_rebuilds_a_tree_stripped_of_its_files() {
        let plan = small_plan();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixtures.noindex");
        let (first, built) = OutDir::new(&path).ensure(&plan, false).unwrap();
        assert!(built);
        let listing = crate::listing(&path.join("tree")).unwrap();

        strip_files(&path);
        assert!(!path.join(MARKER).exists());
        assert!(!path.join("record.toml").exists());
        assert!(path.join("tree/cases/c/a/b").is_dir());

        // Named by `--out`, the directory is refused: it could be anyone's.
        let err = OutDir::new(&path).ensure(&plan, false).unwrap_err();
        assert!(
            err.to_string().contains("is not a mktree output directory"),
            "{err}"
        );

        // As the workspace default it is mktree's, and is built again.
        let reserved = OutDir {
            path: path.clone(),
            reserved: true,
        };
        let (second, built) = reserved.ensure(&plan, false).unwrap();
        assert!(built);
        assert_eq!(second.hash, first.hash);
        assert_eq!(crate::listing(&reserved.tree()).unwrap(), listing);
        assert!(path.join(MARKER).is_file());
    }

    #[test]
    fn p0_test_01_only_the_workspace_default_is_reserved() {
        assert!(OutDir::workspace_default().reserved);
        assert!(!OutDir::new(OutDir::workspace_default().path()).reserved);
    }
}
