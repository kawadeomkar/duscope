//! SIGINT: a handler that sets a walk's cancellation flag.

use std::io;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// The flag SIGINT sets; null until a handler is installed. It holds a
/// reference from `Arc::into_raw` that is never given back, so the flag
/// outlives every signal.
static FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(ptr::null_mut());

extern "C" fn on_interrupt(_signal: libc::c_int) {
    let flag = FLAG.load(Ordering::Acquire);
    if !flag.is_null() {
        // SAFETY: a non-null FLAG came from `Arc::into_raw` and is never
        // released, so it points to a live `AtomicBool`; an atomic store is
        // async-signal-safe.
        unsafe { (*flag).store(true, Ordering::SeqCst) };
    }
}

/// Installs the handler, which sets `flag` on SIGINT; `false`, installing
/// nothing, when SIGINT is ignored (as the shell leaves it for a background
/// job). A flag set by an earlier call stays allocated, since the handler
/// may be reading it.
pub(crate) fn cancel_on_interrupt(flag: &Arc<AtomicBool>) -> io::Result<bool> {
    // SAFETY: `sigaction` is plain data, for which all-zero bytes are a
    // valid value.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action only reads the current one into `current`,
    // which lives through the call.
    if unsafe { libc::sigaction(libc::SIGINT, ptr::null(), &raw mut current) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if current.sa_sigaction == libc::SIG_IGN {
        return Ok(false);
    }
    FLAG.store(
        Arc::into_raw(Arc::clone(flag)).cast_mut(),
        Ordering::Release,
    );
    // SAFETY: `sigaction` is plain data, for which all-zero bytes are a
    // valid value; the fields that matter are set below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: `sa_mask` is a valid `sigset_t` for `sigemptyset` to clear,
    // and `action` lives through the `sigaction` call, which copies it.
    let installed = unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &raw const action, ptr::null_mut())
    };
    if installed == 0 {
        Ok(true)
    } else {
        Err(io::Error::last_os_error())
    }
}
