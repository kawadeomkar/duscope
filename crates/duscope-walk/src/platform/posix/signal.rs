//! SIGINT: a handler that sets a walk's cancellation flag and wakes a
//! waiting thread through a pipe.

use std::io;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

/// The flag SIGINT sets; null until a handler is installed. It holds a
/// reference from `Arc::into_raw` that is never given back, so the flag
/// outlives every signal.
static FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(ptr::null_mut());

/// The pipe the handler wakes a waiting thread through: it writes to
/// `WAKE`, the waiter reads `WAIT`. -1 until the first handler is
/// installed; open for the rest of the process.
static WAKE: AtomicI32 = AtomicI32::new(-1);
static WAIT: AtomicI32 = AtomicI32::new(-1);

/// What a byte on the pipe means.
const INTERRUPTED: u8 = b'i';
const STOOD_DOWN: u8 = b's';

extern "C" fn on_interrupt(_signal: libc::c_int) {
    let flag = FLAG.load(Ordering::Acquire);
    if !flag.is_null() {
        // SAFETY: a non-null FLAG came from `Arc::into_raw` and is never
        // released, so it points to a live `AtomicBool`; an atomic store is
        // async-signal-safe.
        unsafe { (*flag).store(true, Ordering::SeqCst) };
    }
    let saved = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    wake(INTERRUPTED);
    // The interrupted code may be about to read errno.
    super::set_errno(saved);
}

/// Writes `byte` to the wake-up pipe, if there is one. A full pipe only
/// loses a wake-up that is already pending.
fn wake(byte: u8) {
    let fd = WAKE.load(Ordering::Acquire);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; `fd` is the pipe's write
        // end, open for the rest of the process, and `byte` lives through
        // the call.
        unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    }
}

/// Opens the wake-up pipe once: both ends close on exec, and the write end
/// never blocks, so the handler cannot hang on a full pipe.
fn open_pipe() -> io::Result<()> {
    if WAIT.load(Ordering::Acquire) >= 0 {
        return Ok(());
    }
    let mut fds = [-1; 2];
    // SAFETY: `fds` is writable for the two descriptors pipe(2) returns.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for (fd, flags) in [(fds[0], 0), (fds[1], libc::O_NONBLOCK)] {
        // SAFETY: `fd` was just returned by pipe(2) and is ours.
        let ok = unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && (flags == 0 || libc::fcntl(fd, libc::F_SETFL, flags) == 0)
        };
        if !ok {
            let error = io::Error::last_os_error();
            // SAFETY: both descriptors are ours and closed once.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
    }
    WAKE.store(fds[1], Ordering::Release);
    WAIT.store(fds[0], Ordering::Release);
    Ok(())
}

/// Blocks until SIGINT arrives (`true`) or [`stand_down`] is called
/// (`false`); `false` at once when no handler was installed.
pub(crate) fn wait_for_interrupt() -> bool {
    let fd = WAIT.load(Ordering::Acquire);
    if fd < 0 {
        return false;
    }
    let mut byte = 0u8;
    loop {
        // SAFETY: `fd` is the pipe's read end, open for the rest of the
        // process, and `byte` is writable for one byte.
        let read = unsafe { libc::read(fd, (&raw mut byte).cast(), 1) };
        if read == 1 {
            return byte == INTERRUPTED;
        }
        if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return false;
    }
}

/// Wakes a thread waiting in [`wait_for_interrupt`], which returns
/// `false`. The handler stays: a later SIGINT still sets the flag.
pub(crate) fn stand_down() {
    wake(STOOD_DOWN);
}

/// Empties the pipe of wake-ups no thread waited for (a SIGINT after the
/// last stand-down), so the next waiter is not woken by an old one.
fn drain() {
    let fd = WAIT.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    let mut byte = 0u8;
    loop {
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is one valid pollfd that lives through the call;
        // a zero timeout returns at once.
        if unsafe { libc::poll(&raw mut ready, 1, 0) } != 1 || ready.revents & libc::POLLIN == 0 {
            return;
        }
        // SAFETY: `fd` is the pipe's read end, which poll says holds a byte,
        // and `byte` is writable for one byte.
        if unsafe { libc::read(fd, (&raw mut byte).cast(), 1) } != 1 {
            return;
        }
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
    open_pipe()?;
    drain();
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
