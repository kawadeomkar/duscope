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

/// Sets the process up for walking, once, whichever provider is built
/// first: the descriptor limit is raised, and on macOS dataless files are
/// never downloaded.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn setup() {
    static SETUP: Once = Once::new();
    SETUP.call_once(|| {
        posix::raise_nofile_limit();
        #[cfg(target_os = "macos")]
        macos::keep_dataless_files();
    });
}

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
