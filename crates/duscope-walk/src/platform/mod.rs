//! The platform providers. These modules hold the crate's only `unsafe`
//! code, and every block carries a `// SAFETY:` comment.

#![allow(unsafe_code)]

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
