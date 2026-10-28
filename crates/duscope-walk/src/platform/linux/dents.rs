//! Listing a directory with `getdents64(2)` on an `O_DIRECTORY` descriptor.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

use crate::NameRef;

/// The listing buffer. glibc's `opendir` picks `MIN(MAX(st_blksize, 32768),
/// 1048576)`; the review measured the size as a rounding error.
pub(super) const BUFFER_BYTES: usize = 64 * 1024;

/// A buffer for `linux_dirent64` records, which the kernel writes 8-byte
/// aligned: it is held as `u64`s and read as bytes.
pub(super) struct DentBuf(Box<[u64]>);

impl DentBuf {
    pub(super) fn new() -> DentBuf {
        DentBuf(vec![0; BUFFER_BYTES / 8].into_boxed_slice())
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the slice covers exactly the `u64` allocation, any bit
        // pattern is a valid `u8`, and `u8` has no alignment requirement.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.0.len() * 8) }
    }
}

/// Appends the name of every entry of the directory open on `dir` to
/// `names`, without `.` and `..`.
pub(super) fn list(
    dir: BorrowedFd<'_>,
    buf: &mut DentBuf,
    names: &mut Vec<NameRef>,
) -> io::Result<()> {
    loop {
        let len = buf.0.len() * 8;
        // SAFETY: `dir` is open for the borrow, and the buffer is writable
        // for `len` bytes and 8-byte aligned.
        let read = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                dir.as_raw_fd(),
                buf.0.as_mut_ptr().cast::<libc::c_void>(),
                len,
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            return Ok(());
        }
        let read = usize::try_from(read).map_err(|_| io::ErrorKind::InvalidData)?;
        parse(&buf.bytes()[..read.min(len)], names)?;
    }
}

/// The names in a buffer of `linux_dirent64` records: `d_ino` (8 bytes),
/// `d_off` (8), `d_reclen` (2), `d_type` (1), then the NUL-terminated name.
pub(super) fn parse(mut bytes: &[u8], names: &mut Vec<NameRef>) -> io::Result<()> {
    const NAME_AT: usize = 19;
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "getdents64 returned a malformed record",
        )
    };
    while !bytes.is_empty() {
        let reclen = bytes
            .get(16..18)
            .map(|b| usize::from(u16::from_ne_bytes([b[0], b[1]])))
            .ok_or_else(invalid)?;
        let record = bytes
            .get(..reclen)
            .filter(|_| reclen > NAME_AT)
            .ok_or_else(invalid)?;
        let name = &record[NAME_AT..];
        let name = &name[..name.iter().position(|&b| b == 0).ok_or_else(invalid)?];
        if name != b"." && name != b".." {
            names.push(NameRef::new(name));
        }
        bytes = &bytes[reclen..];
    }
    Ok(())
}
