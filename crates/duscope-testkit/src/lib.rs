//! The environment of every duscope test.
//!
//! [`TestEnv`] gives a test a private temporary `HOME`, XDG directories,
//! index directory and `TMPDIR`, and pins what output may depend on:
//! `TZ=UTC`, `LC_ALL=C.UTF-8`, an 80x24 terminal (`COLUMNS`, `LINES`) and
//! `DUSCOPE_TEST_NOW`, the fixture manifest's instant. Every other variable
//! is cleared (so `NO_COLOR`, `TERM`, `COLORTERM`, `CLICOLOR_FORCE`,
//! `FORCE_COLOR`, `LS_COLORS`, `CI`, `CLAUDECODE` and the rest of the
//! caller's environment never leak into output); `PATH` is kept, and the
//! coverage and backtrace variables are passed through. A test that wants
//! monochrome output sets `NO_COLOR` on its command itself.
//!
//! The environment applies to the commands the test environment builds
//! ([`TestEnv::cmd`] for the duscope binary, [`TestEnv::command`] for any
//! other program); the test process's own environment is never modified
//! (`set_var` is unsafe in edition 2024 and races across test threads), so
//! in-process code takes these values from the accessors instead.
//!
//! [`TestEnv::fixtures`] gives the reference tree under `target/fixtures.noindex`,
//! building it first when it is missing or was built from another manifest,
//! and applies the test modes of its unreadable directories for as long as
//! any test environment of the process holds it.
//!
//! Every integration test that runs the duscope binary or reads the fixture
//! tree constructs a `TestEnv`; a test in this crate enforces it.
//!
//! ```no_run
//! let env = duscope_testkit::TestEnv::new();
//! let tree = env.fixtures().root().to_owned();
//! let out = env.cmd().arg("top").arg(&tree).arg("--json").output().unwrap();
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::{OsStr, OsString};
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock, PoisonError};

pub use mktree::{CaseRecord, Plan, Record, Status};
use mktree::{Manifest, OutDir};

/// Variables passed through from the caller when set: coverage profiles
/// (`cargo llvm-cov`) and backtraces.
const PASS_THROUGH: &[&str] = &["LLVM_PROFILE_FILE", "RUST_BACKTRACE", "RUST_LIB_BACKTRACE"];

/// The terminal size every test sees.
pub const COLUMNS: u16 = 80;
/// The terminal size every test sees.
pub const LINES: u16 = 24;

/// The manifest's instant (RFC 3339), read once per process.
fn instant() -> &'static str {
    static INSTANT: OnceLock<String> = OnceLock::new();
    INSTANT.get_or_init(|| {
        let manifest = Manifest::embedded().unwrap_or_else(|e| panic!("duscope-testkit: {e}"));
        let secs = mktree::time::parse_instant(&manifest.instant)
            .unwrap_or_else(|e| panic!("duscope-testkit: {e}"));
        mktree::time::format_instant(secs)
    })
}

/// The value tests export as `DUSCOPE_TEST_NOW`: the fixture manifest's
/// instant, RFC 3339 UTC.
pub fn test_now() -> &'static str {
    instant()
}

/// The duscope binary of the current build: beside the `deps/` directory
/// that holds the running test executable.
///
/// Cargo builds that binary only when the run includes the duscope
/// package's tests (`cargo test --workspace`, `just test`, `cargo test -p
/// duscope`). A test in another crate run on its own (`cargo test -p
/// duscope-core`) finds an older build there, or none: [`TestEnv::cmd`]
/// fails on a missing binary, and such a test is run with the workspace.
///
/// # Panics
///
/// When the path of the running executable is unknown.
pub fn duscope_bin() -> PathBuf {
    let exe =
        std::env::current_exe().unwrap_or_else(|e| panic!("duscope-testkit: current_exe: {e}"));
    let mut dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join(format!("duscope{}", std::env::consts::EXE_SUFFIX))
}

/// The variable that makes [`golden`] rewrite golden files instead of
/// comparing with them, when set to `1` in the test process's environment.
pub const UPDATE_GOLDEN: &str = "DUSCOPE_TEST_UPDATE_GOLDEN";

/// The directory of the golden files, `duscope/tests/golden/`.
pub fn golden_dir() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or(Path::new("."));
    workspace.join("duscope/tests/golden")
}

/// Compares `actual` with the golden file `name` in [`golden_dir`]. With
/// [`UPDATE_GOLDEN`]`=1` the file is written instead; review the diff
/// before committing it.
///
/// # Panics
///
/// When `actual` differs from the file, or the file cannot be read or
/// written: it is a test assertion.
pub fn golden(name: &str, actual: &str) {
    let path = golden_dir().join(name);
    if std::env::var_os(UPDATE_GOLDEN).is_some_and(|v| v == "1") {
        fs::write(&path, actual)
            .unwrap_or_else(|e| panic!("duscope-testkit: {}: {e}", path.display()));
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("duscope-testkit: {}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with {UPDATE_GOLDEN}=1 to regenerate"
    );
}

/// A test's private environment. Dropping it removes its temporary
/// directory (restoring permissions first) and releases the fixture tree.
#[derive(Debug)]
pub struct TestEnv {
    temp: Option<tempfile::TempDir>,
    root: PathBuf,
    holds_fixtures: OnceLock<()>,
}

impl Default for TestEnv {
    fn default() -> Self {
        TestEnv::new()
    }
}

impl TestEnv {
    /// A fresh environment with its directories created.
    ///
    /// # Panics
    ///
    /// When a directory cannot be created: it is test support, and a test
    /// cannot run without it.
    pub fn new() -> TestEnv {
        let temp = tempfile::Builder::new()
            .prefix("duscope-test-")
            .tempdir()
            .unwrap_or_else(|e| panic!("duscope-testkit: temporary directory: {e}"));
        // Resolve symlinks (macOS: /var -> /private/var) so paths printed by
        // the binary and paths built by the test compare equal.
        let root = fs::canonicalize(temp.path())
            .unwrap_or_else(|e| panic!("duscope-testkit: canonicalize: {e}"));
        let env = TestEnv {
            temp: Some(temp),
            root,
            holds_fixtures: OnceLock::new(),
        };
        for dir in [
            env.home(),
            env.xdg_cache_home(),
            env.xdg_config_home(),
            env.xdg_data_home(),
            env.xdg_state_home(),
            env.xdg_runtime_dir(),
            env.root.join("xdg/config-dirs"),
            env.root.join("xdg/data-dirs"),
            env.index_dir(),
            env.tmp_dir(),
            env.work_dir(),
        ] {
            fs::create_dir_all(&dir)
                .unwrap_or_else(|e| panic!("duscope-testkit: create {}: {e}", dir.display()));
        }
        fs::set_permissions(env.xdg_runtime_dir(), Permissions::from_mode(0o700))
            .unwrap_or_else(|e| panic!("duscope-testkit: chmod XDG_RUNTIME_DIR: {e}"));
        env
    }

    /// The environment's temporary root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `HOME`.
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// `XDG_CACHE_HOME` (where the index and spill files go by default).
    pub fn xdg_cache_home(&self) -> PathBuf {
        self.root.join("xdg/cache")
    }

    /// `XDG_CONFIG_HOME`.
    pub fn xdg_config_home(&self) -> PathBuf {
        self.root.join("xdg/config")
    }

    /// `XDG_DATA_HOME` (the XDG Trash lives below it).
    pub fn xdg_data_home(&self) -> PathBuf {
        self.root.join("xdg/data")
    }

    /// `XDG_STATE_HOME`.
    pub fn xdg_state_home(&self) -> PathBuf {
        self.root.join("xdg/state")
    }

    /// `XDG_RUNTIME_DIR` (mode 0700).
    pub fn xdg_runtime_dir(&self) -> PathBuf {
        self.root.join("xdg/runtime")
    }

    /// `DUSCOPE_INDEX_DIR`.
    pub fn index_dir(&self) -> PathBuf {
        self.root.join("index")
    }

    /// `TMPDIR`.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// The working directory of commands built here.
    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    /// `DUSCOPE_TEST_NOW`: the fixture manifest's instant, RFC 3339.
    pub fn test_now(&self) -> &'static str {
        instant()
    }

    /// The complete environment of commands built here, sorted by name.
    pub fn vars(&self) -> Vec<(OsString, OsString)> {
        let path = |p: PathBuf| p.into_os_string();
        let mut vars: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), path(self.home())),
            ("XDG_CACHE_HOME".into(), path(self.xdg_cache_home())),
            ("XDG_CONFIG_HOME".into(), path(self.xdg_config_home())),
            ("XDG_DATA_HOME".into(), path(self.xdg_data_home())),
            ("XDG_STATE_HOME".into(), path(self.xdg_state_home())),
            ("XDG_RUNTIME_DIR".into(), path(self.xdg_runtime_dir())),
            (
                "XDG_CONFIG_DIRS".into(),
                path(self.root.join("xdg/config-dirs")),
            ),
            (
                "XDG_DATA_DIRS".into(),
                path(self.root.join("xdg/data-dirs")),
            ),
            ("DUSCOPE_INDEX_DIR".into(), path(self.index_dir())),
            ("TMPDIR".into(), path(self.tmp_dir())),
            ("TZ".into(), "UTC".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
            ("COLUMNS".into(), COLUMNS.to_string().into()),
            ("LINES".into(), LINES.to_string().into()),
            ("DUSCOPE_TEST_NOW".into(), instant().into()),
        ];
        if let Some(p) = std::env::var_os("PATH") {
            vars.push(("PATH".into(), p));
        }
        for name in PASS_THROUGH {
            if let Some(v) = std::env::var_os(name) {
                vars.push(((*name).into(), v));
            }
        }
        vars.sort();
        vars
    }

    /// Clears `cmd`'s environment, applies [`TestEnv::vars`] and sets its
    /// working directory to [`TestEnv::work_dir`].
    pub fn apply<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        cmd.env_clear()
            .envs(self.vars())
            .current_dir(self.work_dir())
    }

    /// A command for any program under this environment.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        self.apply(&mut cmd);
        cmd
    }

    /// A command for the duscope binary under this environment.
    ///
    /// # Panics
    ///
    /// When the binary has not been built (see [`duscope_bin`]).
    pub fn cmd(&self) -> Command {
        let bin = duscope_bin();
        assert!(
            bin.is_file(),
            "duscope-testkit: no duscope binary at {}: cargo builds it only when the run \
             includes the duscope package's tests, so run this test with --workspace",
            bin.display()
        );
        self.command(bin)
    }

    /// The reference fixture tree, built first if it is missing or stale.
    /// From the first call until this environment is dropped, its
    /// unreadable directories carry their test modes.
    ///
    /// # Panics
    ///
    /// When the tree cannot be built or its test modes applied.
    pub fn fixtures(&self) -> &'static Fixtures {
        let fixtures = Fixtures::shared();
        self.holds_fixtures.get_or_init(|| fixtures.hold());
        fixtures
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if self.holds_fixtures.get().is_some() {
            Fixtures::shared().release();
        }
        if let Some(temp) = self.temp.take() {
            // Restore permissions a test may have removed, then remove.
            let path = temp.keep();
            let _ = mktree::remove_tree(&path);
        }
    }
}

/// The reference tree and what is known about it.
#[derive(Debug)]
pub struct Fixtures {
    out: OutDir,
    plan: Plan,
    record: Record,
    unreadable_effective: bool,
    holders: Mutex<usize>,
}

impl Fixtures {
    /// The process-wide fixture tree, built on first use.
    fn shared() -> &'static Fixtures {
        static SHARED: OnceLock<Fixtures> = OnceLock::new();
        SHARED.get_or_init(|| {
            Fixtures::open(OutDir::workspace_default()).unwrap_or_else(|e| panic!("{e}"))
        })
    }

    fn open(out: OutDir) -> Result<Fixtures, String> {
        let fail = |e: mktree::Error| {
            format!(
                "duscope-testkit: fixture tree at {}: {e}",
                out.path().display()
            )
        };
        let plan = Plan::embedded().map_err(fail)?;
        // ensure rebuilds a tree recorded for another kind of process, so
        // the record's flag holds for this one.
        let (record, _) = out.ensure(&plan, false).map_err(fail)?;
        Ok(Fixtures {
            out,
            plan,
            unreadable_effective: record.unreadable_effective,
            record,
            holders: Mutex::new(0),
        })
    }

    fn hold(&self) {
        let mut holders = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        if *holders == 0 {
            self.out
                .hold(&self.plan)
                .unwrap_or_else(|e| panic!("duscope-testkit: arming the fixture tree: {e}"));
        }
        *holders += 1;
    }

    fn release(&self) {
        let mut holders = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        *holders = holders.saturating_sub(1);
        if *holders == 0 {
            // Errors here would only leave the tree armed; the next build or
            // `mktree --disarm` restores it.
            let _ = self.out.release(&self.plan);
        }
    }

    /// The tree's root, `target/fixtures.noindex/tree`.
    pub fn root(&self) -> PathBuf {
        self.out.tree()
    }

    /// The path of a tree-relative path.
    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.out.tree().join(rel)
    }

    /// The root directory of a case, by name.
    ///
    /// # Panics
    ///
    /// When the manifest has no such case.
    pub fn case_root(&self, name: &str) -> PathBuf {
        let (_, case) = self
            .plan
            .case(name)
            .unwrap_or_else(|| panic!("duscope-testkit: no fixture case {name:?}"));
        mktree::build::abs(&self.out.tree(), &case.root)
    }

    /// A case's build outcome: built, or skipped here with the reason and
    /// what stands in for it.
    ///
    /// # Panics
    ///
    /// When the manifest has no such case.
    pub fn case(&self, name: &str) -> &CaseRecord {
        self.record
            .case(name)
            .unwrap_or_else(|| panic!("duscope-testkit: no fixture case {name:?}"))
    }

    /// Whether a case was built as specified on this platform.
    ///
    /// # Panics
    ///
    /// When the manifest has no such case.
    pub fn built(&self, name: &str) -> bool {
        self.case(name).status == Status::Built
    }

    /// The cases skipped on this platform or filesystem.
    pub fn skipped(&self) -> impl Iterator<Item = &CaseRecord> {
        self.record.skipped()
    }

    /// Whether mode 000 denies this process access (false under root): tests
    /// of unreadable directories skip when it does not.
    pub fn unreadable_effective(&self) -> bool {
        self.unreadable_effective
    }

    /// The manifest hash the tree was built from.
    pub fn hash(&self) -> &str {
        &self.record.hash
    }

    /// The manifest's instant (RFC 3339), as exported in `DUSCOPE_TEST_NOW`.
    pub fn instant(&self) -> &str {
        &self.record.instant
    }

    /// The expanded manifest.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The build record.
    pub fn record(&self) -> &Record {
        &self.record
    }
}
