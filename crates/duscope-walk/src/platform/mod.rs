//! The platform providers. These modules hold the crate's only `unsafe`
//! code, and every block carries a `// SAFETY:` comment.

#![allow(unsafe_code)]

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod fallback;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod posix;

use std::sync::Once;

use crate::Provider;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use posix::DirFd;

/// A directory's open descriptor: none is opened on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub(crate) enum DirFd {}

/// Sets the process up for walking, once, whichever provider is built
/// first: the descriptor limit is raised and the descriptor table grown, and
/// on macOS dataless files are never downloaded.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn setup() {
    static SETUP: Once = Once::new();
    SETUP.call_once(|| {
        posix::raise_nofile_limit();
        #[cfg(target_os = "macos")]
        macos::keep_dataless_files();
    });
}

/// Sets the process up for walking: nothing to do on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn setup() {}

/// This platform's provider.
#[cfg(target_os = "macos")]
#[allow(clippy::unnecessary_wraps)] // `None` where no provider is built
pub(crate) fn native() -> Option<Box<dyn Provider>> {
    Some(Box::new(macos::MacProvider::new()))
}

/// This platform's provider.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_wraps)] // `None` where no provider is built
pub(crate) fn native() -> Option<Box<dyn Provider>> {
    Some(Box::new(linux::LinuxProvider::new()))
}

/// This platform's provider: none is built here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn native() -> Option<Box<dyn Provider>> {
    None
}

/// The fallback provider, `readdir` and `fstatat`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)] // `None` where no provider is built
pub(crate) fn fallback() -> Option<Box<dyn Provider>> {
    setup();
    Some(Box::new(fallback::FallbackProvider::default()))
}

/// Makes SIGINT set `flag`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn cancel_on_interrupt(
    flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<bool> {
    posix::signal::cancel_on_interrupt(flag)
}

/// Whether the directory at `dir` holds a valid `CACHEDIR.TAG`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn cachedir_tag(dir: &std::path::Path) -> bool {
    posix::cachedir_tag(dir)
}

/// Whether the directory at `dir` holds a valid `CACHEDIR.TAG`: not read on
/// this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn cachedir_tag(_dir: &std::path::Path) -> bool {
    false
}

/// Blocks until SIGINT arrives (`true`) or [`stand_down_interrupt`] is
/// called (`false`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn wait_for_interrupt() -> bool {
    posix::signal::wait_for_interrupt()
}

/// Wakes the thread waiting for SIGINT; the handler stays.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn stand_down_interrupt() {
    posix::signal::stand_down();
}

/// Blocks until SIGINT arrives: never here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn wait_for_interrupt() -> bool {
    false
}

/// Wakes the thread waiting for SIGINT: none here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn stand_down_interrupt() {}

/// The size of the terminal on stdout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
    posix::winsize::terminal_size()
}

/// The operating system and its kernel release.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn system() -> String {
    posix::system::system()
}

/// The operating system: its name alone on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn system() -> String {
    std::env::consts::OS.to_owned()
}

/// The effective user id.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn effective_uid() -> u32 {
    posix::system::effective_uid()
}

/// The effective user id: none on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn effective_uid() -> u32 {
    0
}

/// Whether the process `pid` is running.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn process_alive(pid: u32) -> bool {
    posix::system::process_alive(pid)
}

/// Whether the process `pid` is running: taken to be, on this platform, so
/// only an age decides.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_alive(_pid: u32) -> bool {
    true
}

/// The user's cache directory under `home` when `XDG_CACHE_HOME` is unset:
/// macOS's `~/Library/Caches`.
#[cfg(target_os = "macos")]
pub(crate) fn cache_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join("Library").join("Caches")
}

/// The user's cache directory under `home` when `XDG_CACHE_HOME` is unset:
/// the XDG default, `~/.cache`.
#[cfg(not(target_os = "macos"))]
pub(crate) fn cache_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".cache")
}

/// Whether a test run asked that the Trash take only items it can move into
/// a Trash of the test's own (`DUSCOPE_TEST_TRASH`, honoured in debug builds
/// only, as `DUSCOPE_TEST_NOW` is).
fn trash_guarded() -> bool {
    cfg!(debug_assertions) && std::env::var_os("DUSCOPE_TEST_TRASH").is_some()
}

/// Moves `path` to the Trash.
#[cfg(target_os = "macos")]
pub(crate) fn trash(path: &std::path::Path) -> Result<(), crate::TrashError> {
    macos::trash::trash(path, trash_guarded())
}

/// Moves `path` to the Trash.
#[cfg(target_os = "linux")]
pub(crate) fn trash(path: &std::path::Path) -> Result<(), crate::TrashError> {
    linux::trash::trash(path, trash_guarded())
}

/// Moves `path` to the Trash: there is none on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn trash(_path: &std::path::Path) -> Result<(), crate::TrashError> {
    let _ = trash_guarded();
    Err(crate::TrashError::Unavailable(
        "duscope has no Trash on this platform".to_owned(),
    ))
}

/// Whether `path` is a firmlink.
#[cfg(target_os = "macos")]
pub(crate) fn firmlink(path: &std::path::Path) -> bool {
    macos::trash::firmlink(path)
}

/// Whether `path` is a firmlink: there are none on this platform.
#[cfg(not(target_os = "macos"))]
pub(crate) fn firmlink(_path: &std::path::Path) -> bool {
    false
}

/// The stamp of each directory at `dirs`, in order.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn stamp_dirs<'a>(
    dirs: impl Iterator<Item = &'a std::path::Path>,
    follow_symlinks: bool,
) -> Vec<std::io::Result<crate::DirStamp>> {
    posix::stamp::stamp_dirs(dirs, follow_symlinks)
}

/// The stamp of each directory at `dirs`: none is read on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn stamp_dirs<'a>(
    dirs: impl Iterator<Item = &'a std::path::Path>,
    _follow_symlinks: bool,
) -> Vec<std::io::Result<crate::DirStamp>> {
    dirs.map(|_| Err(std::io::ErrorKind::Unsupported.into()))
        .collect()
}

/// Whether the volume holding `path` is mounted read-only.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn read_only(path: &std::path::Path) -> std::io::Result<bool> {
    posix::read_only(path)
}

/// Whether the volume holding `path` is mounted read-only: not known on this
/// platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn read_only(_path: &std::path::Path) -> std::io::Result<bool> {
    Ok(false)
}

/// Whether a test run asked that the Full Disk Access probe meet the
/// refusal of macOS's privacy protections on both its folders
/// (`DUSCOPE_TEST_TCC=denied`, honoured in debug builds only, as
/// `DUSCOPE_TEST_NOW` is).
fn tcc_denied() -> bool {
    cfg!(debug_assertions) && std::env::var_os("DUSCOPE_TEST_TCC").is_some_and(|v| v == "denied")
}

/// Whether this process lacks Full Disk Access, by the probe of `home`.
#[cfg(target_os = "macos")]
pub(crate) fn full_disk_access_missing(home: &std::path::Path) -> bool {
    macos::full_disk_access_missing(home, tcc_denied())
}

/// Whether this process lacks Full Disk Access: there is no such grant on
/// this platform, so only a test's `DUSCOPE_TEST_TCC` says so.
#[cfg(not(target_os = "macos"))]
pub(crate) fn full_disk_access_missing(_home: &std::path::Path) -> bool {
    tcc_denied()
}

/// The app a Full Disk Access grant for this process belongs to.
#[cfg(target_os = "macos")]
pub(crate) fn host_app() -> Option<String> {
    macos::host_app()
}

/// The app a Full Disk Access grant for this process belongs to: there are
/// no such grants on this platform.
#[cfg(not(target_os = "macos"))]
pub(crate) fn host_app() -> Option<String> {
    None
}

/// The size of the terminal on stdout: unknown on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
    None
}

/// Makes SIGINT set `flag`: not on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn cancel_on_interrupt(
    _flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<bool> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// The fallback provider: none is built here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn fallback() -> Option<Box<dyn Provider>> {
    None
}
