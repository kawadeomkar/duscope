//! Reading one entry by name: `getattrlistat(2)` with the bulk read's
//! attribute list, so both paths build the same entry, and `lstat` where a
//! filesystem cannot answer it. The stat queue, mount points, the walk root
//! and records with missing attributes come through here.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::Path;

use super::bulk::{self, Decoded, Fields, Request};
use super::ffi::{self, AttrBuf};
use crate::platform::posix;
use crate::{Attrs, Entry, FullAttrs, Kind, NameRef, WalkOptions};

/// The buffer for one entry's record: the fixed attributes and a name of up
/// to 255 characters of four UTF-8 bytes each fit with room to spare.
pub(super) const BUFFER_BYTES: usize = 4096;

/// The entry `name` in the directory open on `dir`. A mount point's name
/// resolves to the root of the mounted volume. `attrs` says whether
/// `getattrlistat` answers in this directory: it turns false when the call
/// is refused, so the rest of a batch on a filesystem without attribute
/// reads costs one `fstatat` per name instead of two calls.
pub(super) fn entry_at(
    dir: BorrowedFd<'_>,
    name: &NameRef,
    opts: &WalkOptions,
    buf: &mut AttrBuf,
    attrs: &mut bool,
) -> io::Result<Entry> {
    posix::with_c_name(name.as_bytes(), |c_name| {
        if *attrs {
            let mut request = Request::single(opts);
            let mut read = ffi::getattrlistat(dir, c_name, &request.attrs, buf, request.options);
            if let Err(e) = &read
                && request.extended()
                && bulk::unsupported(e)
            {
                request = request.without_extended();
                read = ffi::getattrlistat(dir, c_name, &request.attrs, buf, request.options);
            }
            match read {
                Ok(()) => {
                    if let Some(entry) = decode(buf, &request, name)? {
                        return Ok(entry);
                    }
                }
                Err(e) if !bulk::unsupported(&e) => return Err(e),
                Err(_) => *attrs = false,
            }
        }
        let stat = posix::lstat_at(dir, c_name)?;
        Ok(from_stat(name.clone(), &stat, opts.attrs))
    })
}

/// The entry at `path`, not following a final symlink, named `name`.
pub(super) fn entry_of_path(path: &Path, name: NameRef, opts: &WalkOptions) -> io::Result<Entry> {
    let c_path = posix::c_path(path)?;
    let mut buf = AttrBuf::new(BUFFER_BYTES);
    let mut request = Request::single(opts);
    let mut read = ffi::getattrlist(&c_path, &request.attrs, &mut buf, request.options);
    if let Err(e) = &read
        && request.extended()
        && bulk::unsupported(e)
    {
        request = request.without_extended();
        read = ffi::getattrlist(&c_path, &request.attrs, &mut buf, request.options);
    }
    match read {
        Ok(()) => {
            if let Some(entry) = decode(&buf, &request, &name)? {
                return Ok(entry);
            }
        }
        Err(e) if !bulk::unsupported(&e) => return Err(e),
        Err(_) => {}
    }
    let stat = posix::lstat(&c_path)?;
    Ok(from_stat(name, &stat, opts.attrs))
}

/// The entry in a single-entry record, named `name`; `None` when the record
/// lacks attributes and `lstat` must answer instead.
fn decode(buf: &AttrBuf, request: &Request, name: &NameRef) -> io::Result<Option<Entry>> {
    let bytes = buf.bytes();
    let Some(record) = bulk::record_len(bytes).and_then(|len| bulk::parse(&bytes[..len], request))
    else {
        return Ok(None);
    };
    Ok(match record.decode(request) {
        Decoded::Entry(mut entry) => {
            entry.name = name.clone();
            Some(entry)
        }
        Decoded::Error(_, errno) => return Err(io::Error::from_raw_os_error(errno)),
        Decoded::Restat(..) | Decoded::Nameless => None,
    })
}

/// The entry an `lstat` describes, with the `--attrs full` set under
/// `attrs`.
pub(in crate::platform) fn from_stat(name: NameRef, stat: &libc::stat, attrs: Attrs) -> Entry {
    let kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Symlink,
        libc::S_IFSOCK => Kind::Socket,
        libc::S_IFIFO => Kind::Fifo,
        libc::S_IFCHR => Kind::Char,
        libc::S_IFBLK => Kind::Block,
        _ => Kind::Unknown,
    };
    let time = |sec: i64, nsec: i64| bulk::Timespec { sec, nsec }.ns();
    bulk::build(Fields {
        name,
        kind,
        apparent: u64::try_from(stat.st_size).unwrap_or(0),
        allocated: u64::try_from(stat.st_blocks).unwrap_or(0) * 512,
        mtime_ns: time(stat.st_mtime, stat.st_mtime_nsec),
        ctime_ns: time(stat.st_ctime, stat.st_ctime_nsec),
        nlink: u32::from(stat.st_nlink),
        dev: bulk::dev(stat.st_dev),
        // stat names no volume; the device stands in.
        mnt_id: bulk::dev(stat.st_dev),
        ino: stat.st_ino,
        bsd_flags: stat.st_flags,
        ext_flags: None,
        clone_id: None,
        full: (attrs == Attrs::Full).then(|| FullAttrs {
            atime_ns: time(stat.st_atime, stat.st_atime_nsec),
            // macOS reports a birth time of 0 where the filesystem keeps
            // none (devfs, autofs, some network and FUSE volumes).
            btime_ns: (stat.st_birthtime != 0 || stat.st_birthtime_nsec != 0)
                .then(|| time(stat.st_birthtime, stat.st_birthtime_nsec)),
            mode: u32::from(stat.st_mode),
            uid: stat.st_uid,
            gid: stat.st_gid,
        }),
    })
}
