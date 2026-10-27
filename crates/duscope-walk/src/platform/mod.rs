//! The platform providers. These modules hold the crate's only `unsafe`
//! code, and every block carries a `// SAFETY:` comment.

#![allow(unsafe_code)]

#[cfg(target_os = "macos")]
mod macos;
// Linux uses it from P1-WALK-02.
#[cfg(target_os = "macos")]
mod posix;

use crate::Provider;

/// This platform's provider.
#[cfg(target_os = "macos")]
#[allow(clippy::unnecessary_wraps)] // `None` where no provider is built
pub(crate) fn native() -> Option<Box<dyn Provider>> {
    Some(Box::new(macos::MacProvider::new()))
}

/// This platform's provider: none is built here yet.
#[cfg(not(target_os = "macos"))]
pub(crate) fn native() -> Option<Box<dyn Provider>> {
    None
}
