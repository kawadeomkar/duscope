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
