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
    Some(Box::new(fallback::FallbackProvider))
}

/// The fallback provider: none is built here.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn fallback() -> Option<Box<dyn Provider>> {
    None
}
