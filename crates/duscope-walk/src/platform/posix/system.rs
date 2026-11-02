//! What an index records about the system that wrote it: the operating
//! system and its kernel release, and the user the scan ran as; and whether
//! the process that was writing one is still running.

use std::ffi::CStr;

/// `sysname release` from `uname`: `Linux 6.8.0-45-generic`,
/// `Darwin 25.6.0`; empty if `uname` fails.
pub(crate) fn system() -> String {
    // SAFETY: `utsname` is plain data, for which all-zero bytes are valid.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: uname writes one `utsname` through the pointer, which points
    // to a live, writable `utsname`.
    if unsafe { libc::uname(&raw mut uts) } != 0 {
        return String::new();
    }
    let field = |f: &[libc::c_char]| {
        // SAFETY: on success every field holds a NUL-terminated string
        // within its array.
        unsafe { CStr::from_ptr(f.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    format!("{} {}", field(&uts.sysname), field(&uts.release))
}

/// The local time now, `YYYY-MM-DDThh:mm:ss`, as a `.trashinfo` file's
/// `DeletionDate` writes it; the epoch where the local time cannot be read.
#[cfg(target_os = "linux")]
pub(crate) fn local_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let secs = libc::time_t::try_from(secs).unwrap_or(0);
    // SAFETY: `tm` is plain data, for which all-zero bytes are valid.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r reads one `time_t` and writes one `tm` through the
    // pointers, which point to live values of those types.
    if unsafe { libc::localtime_r(&raw const secs, &raw mut tm) }.is_null() {
        return "1970-01-01T00:00:00".to_owned();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The effective user id.
pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments and always succeeds.
    unsafe { libc::geteuid() }
}

/// Whether a process with id `pid` exists: `kill(pid, 0)` succeeds, or
/// fails only because the process belongs to another user.
pub(crate) fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 delivers nothing; kill only checks that the process
    // exists and that it could be signalled.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
