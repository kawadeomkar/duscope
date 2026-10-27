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

use crate::Provider;

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
    posix::raise_nofile_limit();
    Some(Box::new(fallback::FallbackProvider::default()))
}

/// Makes SIGINT set `flag`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn cancel_on_interrupt(
    flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<bool> {
    posix::signal::cancel_on_interrupt(flag)
}

/// The size of the terminal on stdout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
    posix::winsize::terminal_size()
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
