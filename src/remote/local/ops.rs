//! [`FileOps`] on the local disk (04 §5.6): `renameat2(RENAME_NOREPLACE)` on Linux and
//! `renamex_np(RENAME_EXCL)` on macOS (else `link` + `unlink`), `mkdir`, `rmdir`, links, a copy
//! into a temp created exclusively (`O_CREAT|O_EXCL|O_NOFOLLOW`) and filled, given its mode and
//! times and flushed through that one descriptor — `copy_file_range` on Linux, else read and
//! write — published by the same never-replacing rename.
//!
//! No `clonefile` on macOS: a clone is made by name, and the name can be another file by the
//! time it is opened (a hard link to an unrelated file put there by a process of the same
//! account), which nothing can tell from the clone; its mode and times would then go onto that
//! file. A copy whose descriptor sopen made itself never touches anything else. A move on the
//! same volume is still a rename; only a same-volume copy loses the clone's speed.

use super::LocalFs;
use crate::remote::ops::{
    taken, FileMeta, FileOps, Ident, NoRename, Opened, Pin, Publish, Renamed, StageFailure, Staged,
};
use crate::remote::{join, random_hex, split_parent, Cancelled, Created, Hashing};
use anyhow::{anyhow, Context, Result};
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};

/// Bytes per read and write of a copy through this process.
const BUF: usize = 1024 * 1024;

fn c_path(path: &str) -> Result<CString> {
    CString::new(path).map_err(|_| anyhow!("{path}: a path with a NUL byte"))
}

/// An entry as `fstatat(…, AT_SYMLINK_NOFOLLOW)` sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct At {
    dev: u64,
    ino: u64,
    ctime: (i64, i64),
    dir: bool,
    link: bool,
    /// A regular file.
    file: bool,
}

/// `name` in the open folder `dir`, not following a link.
#[allow(clippy::unnecessary_cast)] // the field types differ between macOS and Linux
fn stat_at(dir: &fs::File, name: &CStr) -> std::io::Result<At> {
    // SAFETY: an all-zero stat is a valid value to be overwritten.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: an open descriptor, a NUL-terminated name and a valid place for the result.
    let r = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let kind = st.st_mode & libc::S_IFMT;
    Ok(At {
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        ctime: (st.st_ctime as i64, st.st_ctime_nsec as i64),
        dir: kind == libc::S_IFDIR,
        link: kind == libc::S_IFLNK,
        file: kind == libc::S_IFREG,
    })
}

/// A descriptor on the entry `name` itself (never what a link points at), held so its inode
/// cannot be freed and its number reused while sopen compares the name with it — `O_PATH` on
/// Linux; on macOS a read-only open (`O_SYMLINK` for a link). Only a regular file or a link is
/// ever opened: opening a device, a fifo or a socket can have effects of its own that
/// `O_NONBLOCK` does not prevent, so those are known by device, inode and status-change time
/// instead. [`Pinned::Unavailable`] for those and where no descriptor can be held (too many
/// open files; on macOS an entry this account cannot read); [`Pinned::Changed`] where what
/// opened is not `own` in full — device, inode and status-change time: an entry put there since
/// `own` was read, on the same inode number or not, is never pinned as if it were `own`, and
/// the caller must not go on as if no descriptor had merely been at hand.
fn pin_at(dir: &fs::File, name: &CStr, own: &At) -> Pinned {
    use std::os::fd::FromRawFd;
    if !own.file && !own.link {
        return Pinned::Unavailable;
    }
    #[cfg(target_os = "linux")]
    let flags = libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = if own.link {
        libc::O_RDONLY | libc::O_SYMLINK | libc::O_CLOEXEC
    } else {
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    #[cfg(test)]
    crate::remote::ops::fire_seam(&BEFORE_PIN_OPEN, &name.to_string_lossy());
    // SAFETY: an open descriptor and a NUL-terminated name.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    #[cfg(test)]
    let fd = if fd >= 0 && NO_PINS.with(|f| f.get()) {
        // SAFETY: a descriptor just opened, owned by nothing else.
        unsafe { libc::close(fd) };
        -1
    } else {
        fd
    };
    if fd < 0 {
        return Pinned::Unavailable;
    }
    // SAFETY: a descriptor just opened, owned by nothing else.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let Ok(m) = file.metadata() else {
        return Pinned::Unavailable;
    };
    let opened = (m.dev(), m.ino(), (m.ctime(), m.ctime_nsec()));
    if opened == (own.dev, own.ino, own.ctime) {
        Pinned::Held(file)
    } else {
        Pinned::Changed
    }
}

/// What [`pin_at`] found.
enum Pinned {
    /// The entry that was looked at, held open.
    Held(fs::File),
    /// Another entry has the name now (another device, inode or status-change time).
    Changed,
    /// No descriptor is to be had for it: nothing is known about the name either way.
    Unavailable,
}

/// `path` itself (`lstat`), as [`stat_at`] gives an entry.
fn stat_at_path(path: &str) -> std::io::Result<At> {
    let (folder, name) = open_folder(path).map_err(|e| std::io::Error::other(format!("{e:#}")))?;
    stat_at(&folder, &name)
}

/// The folder holding `path`, opened once: what is done to its entry goes through this
/// descriptor, so a folder swapped in above it meanwhile is never followed.
fn open_folder(path: &str) -> Result<(fs::File, CString)> {
    let (dir, base) = split_parent(path);
    let folder = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(dir)
        .with_context(|| format!("cannot open the folder {dir}"))?;
    Ok((folder, c_path(base)?))
}

/// The target of the link `name` in the open folder `dir`.
fn read_link_at(dir: &fs::File, name: &CStr) -> std::io::Result<CString> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: an open descriptor, a NUL-terminated name and a buffer of the length given.
    let n = unsafe {
        libc::readlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    CString::new(buf).map_err(|_| std::io::Error::other("a link target with a NUL byte"))
}

/// The result of a libc call that returns 0 on success.
fn zero_ok(r: libc::c_int) -> std::io::Result<()> {
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn meta_of(m: &fs::Metadata) -> FileMeta {
    FileMeta {
        size: m.len(),
        mode: Some(m.permissions().mode() & 0o7777),
        atime: m.accessed().ok(),
        mtime: m.modified().ok(),
    }
}

/// `rename(2)` that fails with `EEXIST` instead of replacing.
fn rename_excl(from: &CString, to: &CString) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    // SAFETY: two NUL-terminated paths.
    let r = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    // SAFETY: two NUL-terminated paths, relative to the working directory.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let r = {
        let _ = (from, to);
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// [`rename_excl`], or (test seam) the refusal of a filesystem without it.
fn try_rename_excl(from: &CString, to: &CString) -> std::io::Result<()> {
    #[cfg(test)]
    if NO_EXCLUSIVE_RENAME.with(|s| s.get()) {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    rename_excl(from, to)
}

/// [`rename_excl`] of two names in one open folder (the same test seam applies).
fn rename_excl_at(dir: &fs::File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    #[cfg(test)]
    if NO_EXCLUSIVE_RENAME.with(|s| s.get()) {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let fd = dir.as_raw_fd();
    #[cfg(target_os = "macos")]
    // SAFETY: an open descriptor and two NUL-terminated names in it.
    let r = unsafe { libc::renameatx_np(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    // SAFETY: an open descriptor and two NUL-terminated names in it.
    let r = unsafe { libc::renameat2(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_NOREPLACE) };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let r = {
        let _ = (fd, from, to);
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    };
    zero_ok(r)
}

/// `link` of two names in one open folder (the same test seam applies).
fn link_at(dir: &fs::File, from: &CStr, to: &CStr) -> std::io::Result<()> {
    if no_links() {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let fd = dir.as_raw_fd();
    // SAFETY: an open descriptor and two NUL-terminated names in it; flags 0 never follows.
    zero_ok(unsafe { libc::linkat(fd, from.as_ptr(), fd, to.as_ptr(), 0) })
}

/// A link refused because this filesystem has none (exFAT, FAT).
fn no_link_here(e: &std::io::Error) -> bool {
    is_errno(e, &[libc::ENOTSUP, libc::EOPNOTSUPP])
}

/// `e` is one of `codes` (a list, not a pattern: some are one value on Linux, two on macOS).
fn is_errno(e: &std::io::Error, codes: &[i32]) -> bool {
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// The filesystem does not offer the exclusive rename (some network and FUSE filesystems).
fn no_exclusive_rename(e: &std::io::Error) -> bool {
    is_errno(
        e,
        &[libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS],
    )
}

/// Opens a regular file for reading without following a link or blocking on a fifo swapped in
/// since it was listed.
fn open_source(path: &str) -> Result<(fs::File, fs::Metadata)> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("cannot open {path}"))?;
    let meta = file
        .metadata()
        .with_context(|| format!("cannot stat {path}"))?;
    if !meta.is_file() {
        return Err(anyhow!("{path} is no longer a regular file"));
    }
    Ok((file, meta))
}

/// Creates `temp` exclusively (`O_CREAT|O_EXCL|O_NOFOLLOW`), private until its copy is
/// complete. Everything after goes through the returned descriptor; the name is used again only
/// to publish it, after the engine checks that the name is still this file.
fn create_temp(temp: &str) -> std::result::Result<(fs::File, (u64, u64)), StageFailure> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(temp)
        .map_err(|e| {
            let error = if e.kind() == ErrorKind::AlreadyExists {
                taken(temp)
            } else {
                anyhow!(e).context(format!("cannot create {temp}"))
            };
            StageFailure::not_created(error)
        })?;
    // Its identity by the descriptor, before anything else can happen to its name.
    let made = file.metadata();
    #[cfg(test)]
    let made = if TEMP_STAT_FAILS.with(|f| f.get()) {
        Err(std::io::Error::other("injected: no stat"))
    } else {
        made
    };
    let made = made.map_err(|e| StageFailure {
        created: Created::Yes,
        id: None,
        pin: None,
        error: anyhow!(e).context(format!("cannot stat {temp}")),
    })?;
    let id = (made.dev(), made.ino());
    #[cfg(test)]
    crate::remote::ops::fire_seam(&AFTER_CREATE, temp);
    Ok((file, id))
}

/// After the content: the source's permission bits and times through the handle (a failure is
/// a note), then the bytes flushed to disk.
fn finish(file: &fs::File, path: &str, meta: &FileMeta) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    if let Some(mode) = meta.mode {
        if let Err(e) = file.set_permissions(fs::Permissions::from_mode(mode)) {
            notes.push(format!("cannot set the mode of {path} to {mode:o}: {e}"));
        }
    }
    if let Some(mtime) = meta.mtime {
        let times = fs::FileTimes::new()
            .set_modified(mtime)
            .set_accessed(meta.atime.unwrap_or(mtime));
        if let Err(e) = file.set_times(times) {
            notes.push(format!("cannot set the times of {path}: {e}"));
        }
    }
    // A plain fsync per file; the volume's write cache is flushed once per operation by
    // [`FileOps::barrier`] (macOS `F_FULLFSYNC`), not once per file.
    // SAFETY: an open descriptor.
    zero_ok(unsafe { libc::fsync(file.as_raw_fd()) })
        .with_context(|| format!("cannot sync {path}"))?;
    Ok(notes)
}

/// Copies `src` into `dst` through this process, [`BUF`] at a time, hashing what it writes.
fn pump(
    src: &mut dyn Read,
    dst: &fs::File,
    temp: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<[u8; 32]> {
    let mut out = Hashing::new(dst);
    let mut buf = vec![0u8; BUF];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(anyhow!(e).context("cannot read the source")),
        };
        out.write_all(&buf[..n])
            .with_context(|| format!("cannot write {temp}"))?;
        done += n as u64;
        progress(done);
    }
    Ok(out.finish().sha256)
}

/// Linux: `copy_file_range` in [`COPY_STEP`]s (the kernel copies, reflinks where it can).
/// `Ok(false)`: not on this pair of filesystems, nothing written — copy the bytes instead.
#[cfg(target_os = "linux")]
fn kernel_copy(
    src: &fs::File,
    dst: &fs::File,
    size: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<bool> {
    use crate::remote::ops::COPY_STEP;
    use std::os::fd::AsRawFd;
    let mut done = 0u64;
    while done < size {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let want = (size - done).min(COPY_STEP) as usize;
        // SAFETY: two open descriptors; null offsets use and advance the files' own.
        let n = unsafe {
            libc::copy_file_range(
                src.as_raw_fd(),
                std::ptr::null_mut(),
                dst.as_raw_fd(),
                std::ptr::null_mut(),
                want,
                0,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            let unsupported = is_errno(
                &e,
                &[libc::EXDEV, libc::ENOSYS, libc::EOPNOTSUPP, libc::EINVAL],
            );
            if done == 0 && unsupported {
                return Ok(false);
            }
            return Err(anyhow!(e).context("cannot copy"));
        }
        if n == 0 {
            // The source ended early: the size check after the copy says so.
            break;
        }
        done += n as u64;
        progress(done);
    }
    Ok(true)
}

impl FileOps for LocalFs {
    fn identity(&mut self, path: &str, follow: bool) -> Result<Option<Ident>> {
        let meta = if follow {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        match meta {
            Ok(m) => Ok(Some(Ident {
                dev: m.dev(),
                ino: m.ino(),
                ctime: (m.ctime(), m.ctime_nsec()),
            })),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot stat {path}")),
        }
    }

    fn pin(&mut self, path: &str, id: &Ident) -> Option<Pin> {
        #[cfg(test)]
        crate::remote::ops::fire_seam(&BEFORE_PIN, path);
        let (folder, name) = open_folder(path).ok()?;
        let own = stat_at(&folder, &name).ok()?;
        // In full: an entry that took the name — and, the inode freed, its number — since `id`
        // was read has another status-change time, and is not pinned as `id`'s.
        if (own.dev, own.ino, own.ctime) != (id.dev, id.ino, id.ctime) {
            return None;
        }
        match pin_at(&folder, &name, &own) {
            Pinned::Held(file) => Some(Pin::new(file)),
            // The engine then compares the identity it read, in full: a changed entry is kept.
            Pinned::Changed | Pinned::Unavailable => None,
        }
    }

    fn meta(&mut self, path: &str) -> Result<FileMeta> {
        let m = fs::symlink_metadata(path).with_context(|| format!("cannot stat {path}"))?;
        Ok(meta_of(&m))
    }

    fn rename_new(&mut self, from: &str, to: &str) -> Result<Renamed> {
        let (f, t) = (c_path(from)?, c_path(to)?);
        let e = match try_rename_excl(&f, &t) {
            Ok(()) => return Ok(Renamed::Moved),
            Err(e) => e,
        };
        match e.raw_os_error() {
            Some(libc::EEXIST) => return Err(taken(to)),
            Some(libc::EXDEV) => {
                return Err(anyhow!(NoRename(format!(
                    "{from} and {to} are on different filesystems"
                ))))
            }
            _ if no_exclusive_rename(&e) => {}
            _ => return Err(e).with_context(|| format!("cannot rename {from} to {to}")),
        }
        // No exclusive rename here: a second name, then the old one removed — both through the
        // old name's folder, opened once.
        let (folder, name) = open_folder(from)?;
        let own = stat_at(&folder, &name).with_context(|| format!("cannot stat {from}"))?;
        if own.dir {
            // A plain rename would replace an empty folder at `to`: a move copies it instead.
            return Err(anyhow!(NoRename(format!(
                "this filesystem cannot rename the folder {from} without the risk of replacing \
                 {to}; nothing was changed"
            ))));
        }
        // The old entry's inode, held until the check before its unlink: while it is open, no
        // other file can be given its number (ext4 reuses a freed one at once).
        let changed = || {
            anyhow!(
                "{from} changed while it was being renamed (another entry has its name now); \
                 nothing was changed"
            )
        };
        let pin = match pin_at(&folder, &name, &own) {
            Pinned::Held(file) => Some(file),
            // Not the entry that was looked at: nothing is linked, nothing removed.
            Pinned::Changed => return Err(changed()),
            // No descriptor: the name is looked at once more right before the link. Best
            // effort — it can still be swapped between this look and the link.
            Pinned::Unavailable => match stat_at(&folder, &name) {
                Ok(now) if now == own => None,
                _ => return Err(changed()),
            },
        };
        let made = if own.link {
            read_link_at(&folder, &name).and_then(|target| {
                // SAFETY: two NUL-terminated paths; the new link's is relative to the working
                // directory, as `to` is given.
                zero_ok(unsafe { libc::symlinkat(target.as_ptr(), libc::AT_FDCWD, t.as_ptr()) })
            })
        } else if no_links() {
            Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
        } else {
            // SAFETY: an open descriptor and two NUL-terminated paths; flags 0 never follows a
            // link at the old name.
            zero_ok(unsafe {
                libc::linkat(
                    folder.as_raw_fd(),
                    name.as_ptr(),
                    libc::AT_FDCWD,
                    t.as_ptr(),
                    0,
                )
            })
        };
        match made {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(taken(to)),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                return Err(anyhow!(NoRename(format!(
                    "{from} and {to} are on different filesystems"
                ))))
            }
            // Neither an exclusive rename nor links (exFAT, FAT): a move copies, checks and
            // removes; nothing was changed.
            Err(e) if no_link_here(&e) => {
                return Err(anyhow!(NoRename(format!(
                    "this filesystem can neither rename {from} without the risk of replacing \
                     {to} nor link it ({e})"
                ))))
            }
            Err(e) => return Err(e).with_context(|| format!("cannot rename {from} to {to}")),
        }
        // The new name as it is now (the link changed its inode's status-change time): what the
        // old name is compared with where no descriptor is held.
        let linked = stat_at_path(to);
        #[cfg(test)]
        crate::remote::ops::fire_seam(&BEFORE_UNLINK, from);
        // Right before the unlink: is the old name still the entry that was linked (its device
        // and inode)? Another writer may have put something else there; that is never removed.
        // Best effort: between this check and the unlink the name can still be swapped — the
        // two steps are not one atomic rename.
        let both = |why: String| {
            Renamed::OldRemains(format!(
                "{to} is in place, but {from} was not removed ({why}): both names exist — {from} \
                 and {to}"
            ))
        };
        match stat_at(&folder, &name) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Renamed::Moved),
            Err(e) => {
                return Ok(Renamed::Unconfirmed(format!(
                    "{to} is in place; {from} was left, as it could not be checked again ({e}): \
                     whether it is still there could not be confirmed"
                )))
            }
            Ok(now) => {
                let same = match &pin {
                    Some(pin) => pin
                        .metadata()
                        .is_ok_and(|m| (m.dev(), m.ino()) == (now.dev, now.ino)),
                    // No descriptor could be held. A link was made again elsewhere, so its old
                    // inode may be freed and reused: its device, inode and status-change time
                    // as first read. A file: the old name must be the very entry the new name
                    // is — device, inode (kept alive by the hard link just made) and
                    // status-change time. Best effort both: as fine as the timestamps.
                    None if own.link => {
                        (now.dev, now.ino, now.ctime) == (own.dev, own.ino, own.ctime)
                    }
                    None => linked.as_ref().is_ok_and(|new| {
                        (now.dev, now.ino, now.ctime) == (new.dev, new.ino, new.ctime)
                            && (now.dev, now.ino) == (own.dev, own.ino)
                    }),
                };
                if !same {
                    return Ok(both("another entry has its name now".into()));
                }
            }
        }
        // SAFETY: an open descriptor and a NUL-terminated name; flags 0 removes a non-folder.
        Ok(
            match zero_ok(unsafe { libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0) }) {
                Ok(()) => Renamed::Moved,
                Err(e) => both(format!("removing it failed: {e}")),
            },
        )
    }

    /// Probed once per folder with a file of its own, made and cleaned up through the folder
    /// opened once: renamed exclusively, else linked; where neither works (exFAT, FAT) copies
    /// are written in place. Only names still the probe's own (its inode) are removed — never
    /// the name a rename vacated, never one another file has taken since — and what is left is
    /// named. A probe that cannot even be made says nothing: the copy's own create will.
    fn publish_mode(&mut self, dir: &str) -> Result<(Publish, Vec<String>)> {
        use std::os::fd::FromRawFd;
        let Ok(folder) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(dir)
        else {
            return Ok((Publish::Atomic, Vec::new()));
        };
        let (first, second) = (
            format!(".sopen-probe-{}", random_hex()),
            format!(".sopen-probe-{}", random_hex()),
        );
        let (f, o) = (c_path(&first)?, c_path(&second)?);
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: an open descriptor, a NUL-terminated name, and a mode for the new file.
        let fd =
            unsafe { libc::openat(folder.as_raw_fd(), f.as_ptr(), flags, 0o600 as libc::c_uint) };
        if fd < 0 {
            return Ok((Publish::Atomic, Vec::new()));
        }
        // SAFETY: a descriptor just opened, owned by nothing else.
        let probe = unsafe { fs::File::from_raw_fd(fd) };
        let made = probe.metadata();
        #[cfg(test)]
        let made = if PROBE_STAT_FAILS.with(|f| f.get()) {
            Err(std::io::Error::other("injected: no stat"))
        } else {
            made
        };
        if let Err(e) = made {
            let path = join(dir, &first);
            return Ok((
                Publish::Atomic,
                vec![format!(
                    "sopen's probe file {path} could not be checked ({e}); it was left as it is"
                )],
            ));
        }
        let both = format!("{}\n{}", join(dir, &first), join(dir, &second));
        // The names that are still the probe's own.
        let (mode, ours): (Publish, Vec<(&CStr, &str)>) = match rename_excl_at(&folder, &f, &o) {
            Ok(()) => {
                #[cfg(test)]
                crate::remote::ops::fire_seam(&AFTER_PROBE, &both);
                (Publish::Atomic, vec![(&o, &second)])
            }
            Err(e) if no_exclusive_rename(&e) => match link_at(&folder, &f, &o) {
                Ok(()) => {
                    #[cfg(test)]
                    crate::remote::ops::fire_seam(&AFTER_PROBE, &both);
                    (Publish::Atomic, vec![(&f, &first), (&o, &second)])
                }
                Err(e) if no_link_here(&e) => (Publish::InPlace, vec![(&f, &first)]),
                Err(_) => (Publish::Atomic, vec![(&f, &first)]),
            },
            Err(_) => (Publish::Atomic, vec![(&f, &first)]),
        };
        let _ = both;
        let mut notes = Vec::new();
        for (name, shown) in ours {
            let path = join(dir, shown);
            // Compared with the probe's descriptor as it is now, not with the number it had when
            // it was made: an empty file's number follows its name on FAT and exFAT.
            let held = match probe.metadata() {
                Ok(m) => (m.dev(), m.ino()),
                Err(e) => {
                    notes.push(format!(
                        "sopen's probe file {path} could not be checked ({e}); it was left as it \
                         is"
                    ));
                    continue;
                }
            };
            match stat_at(&folder, name) {
                Ok(now) if held == (now.dev, now.ino) => {
                    // SAFETY: an open descriptor and a NUL-terminated name.
                    let gone =
                        zero_ok(unsafe { libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0) });
                    if let Err(e) = gone {
                        notes.push(format!(
                            "sopen's probe file {path} could not be removed: {e}"
                        ));
                    }
                }
                Ok(_) => notes.push(format!(
                    "{path} is no longer sopen's probe file (another file has its name); it was \
                     left as it is"
                )),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => notes.push(format!(
                    "sopen's probe file {path} could not be checked ({e}); it was left as it is"
                )),
            }
        }
        Ok((mode, notes))
    }

    /// Each folder in `dirs` (deepest first) flushed through its own descriptor (`fsync`: the
    /// entries it received — a file's `fsync` does not make its new name durable); then on macOS
    /// one `F_FULLFSYNC` to flush the drive's cache for the volume — a plain `fsync` only where
    /// the filesystem says it does not support it, interrupted calls tried again, a real I/O
    /// error returned (it keeps a moved source).
    fn barrier(&mut self, dirs: &[String]) -> Result<()> {
        let open = |dir: &str| {
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY)
                .open(dir)
                .with_context(|| format!("cannot open {dir} to flush it"))
        };
        for dir in dirs {
            let folder = open(dir)?;
            // SAFETY: an open descriptor.
            let flushed = zero_ok(unsafe { libc::fsync(folder.as_raw_fd()) });
            #[cfg(test)]
            let flushed = match DIR_FSYNC_ERRNO.with(|e| e.get()) {
                Some(errno) => Err(std::io::Error::from_raw_os_error(errno)),
                None => flushed,
            };
            // Refused for any reason — unsupported included — the entries are not known to be
            // on disk: no source may go on that.
            flushed.map_err(|e| {
                anyhow!(
                    "this filesystem cannot confirm that the entries of {dir} are on disk ({e})"
                )
            })?;
        }
        #[cfg(target_os = "macos")]
        if let Some(dir) = dirs.last() {
            let folder = open(dir)?;
            loop {
                // SAFETY: an open descriptor.
                let full = unsafe { libc::fcntl(folder.as_raw_fd(), libc::F_FULLFSYNC) };
                #[cfg(test)]
                let full = match FULL_FSYNC_ERRNO.with(|e| e.take()) {
                    Some(errno) => {
                        // SAFETY: errno is this thread's.
                        unsafe { *libc::__error() = errno };
                        -1
                    }
                    None => full,
                };
                if full == 0 {
                    break;
                }
                let e = std::io::Error::last_os_error();
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                let unsupported = [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOTTY];
                if is_errno(&e, &unsupported) {
                    // SAFETY: an open descriptor.
                    zero_ok(unsafe { libc::fsync(folder.as_raw_fd()) })
                        .with_context(|| format!("cannot flush the volume of {dir}"))?;
                    break;
                }
                return Err(e).with_context(|| format!("cannot flush the volume of {dir}"));
            }
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)] // the field type differs between macOS and Linux
    fn fs_id(&mut self, path: &str) -> Option<u64> {
        let c = c_path(path).ok()?;
        // SAFETY: an all-zero statvfs is a valid value to be overwritten.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and a valid place for the result.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some(st.f_fsid as u64)
    }

    fn make_dir(&mut self, path: &str) -> Result<()> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(taken(path)),
            Err(e) => Err(e).with_context(|| format!("cannot make the folder {path}")),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        fs::remove_dir(path).with_context(|| format!("cannot remove the folder {path}"))
    }

    fn read_link(&mut self, path: &str) -> Result<String> {
        fs::read_link(path)
            .with_context(|| format!("cannot read the link {path}"))?
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow!("the link {path} points at a name that is not UTF-8"))
    }

    fn make_symlink(&mut self, target: &str, link: &str) -> Result<()> {
        match std::os::unix::fs::symlink(target, link) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(taken(link)),
            Err(e) => Err(e).with_context(|| format!("cannot make the link {link}")),
        }
    }

    fn set_meta(&mut self, path: &str, meta: &FileMeta) -> Vec<String> {
        // Through the folder's own descriptor, never a link swapped in.
        let opened = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path);
        let dir = match opened {
            Ok(dir) => dir,
            Err(e) => return vec![format!("cannot set the mode and times of {path}: {e}")],
        };
        let mut notes = Vec::new();
        if let Some(mtime) = meta.mtime {
            let times = fs::FileTimes::new()
                .set_modified(mtime)
                .set_accessed(meta.atime.unwrap_or(mtime));
            if let Err(e) = dir.set_times(times) {
                notes.push(format!("cannot set the times of {path}: {e}"));
            }
        }
        if let Some(mode) = meta.mode {
            if let Err(e) = dir.set_permissions(fs::Permissions::from_mode(mode)) {
                notes.push(format!("cannot set the mode of {path} to {mode:o}: {e}"));
            }
        }
        notes
    }

    fn copy_within(
        &mut self,
        from: &str,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> std::result::Result<Staged, StageFailure> {
        let (mut src, src_meta) = open_source(from).map_err(StageFailure::not_created)?;
        let source = meta_of(&src_meta);
        let (dst, id) = create_temp(temp)?;
        let mut fill = || -> Result<Staged> {
            #[cfg(target_os = "linux")]
            let copied = kernel_copy(&src, &dst, source.size, cancel, progress)?;
            #[cfg(not(target_os = "linux"))]
            let copied = false;
            let sha256 = if copied {
                None
            } else {
                Some(pump(&mut src, &dst, temp, cancel, progress)?)
            };
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            let notes = finish(&dst, temp, &source)?;
            let made = dst.metadata()?;
            Ok(Staged {
                size: made.len(),
                sha256,
                source,
                notes,
                id: Some((made.dev(), made.ino())),
                pin: None,
            })
        };
        let filled = fill();
        pinned(filled, dst, id)
    }

    fn open_read(&mut self, path: &str) -> Result<Opened<'_>> {
        let (file, meta) = open_source(path)?;
        Ok(Opened {
            read: Box::new(file),
            meta: meta_of(&meta),
        })
    }

    fn stage_from(
        &mut self,
        src: &mut dyn Read,
        meta: &FileMeta,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> std::result::Result<Staged, StageFailure> {
        let (dst, id) = create_temp(temp)?;
        let mut fill = || -> Result<Staged> {
            let sha256 = pump(src, &dst, temp, cancel, progress)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            let notes = finish(&dst, temp, meta)?;
            let made = dst.metadata()?;
            Ok(Staged {
                size: made.len(),
                sha256: Some(sha256),
                source: *meta,
                notes,
                id: Some((made.dev(), made.ino())),
                pin: None,
            })
        };
        let filled = fill();
        pinned(filled, dst, id)
    }
}

/// A copy's result with the descriptor it was written through kept as its pin: the engine relies
/// on its inode until the read-back and any cleanup are done.
fn pinned(
    filled: Result<Staged>,
    dst: fs::File,
    id: (u64, u64),
) -> std::result::Result<Staged, StageFailure> {
    match filled {
        Ok(staged) => Ok(Staged {
            pin: Some(Pin::new(dst)),
            ..staged
        }),
        Err(error) => Err(StageFailure {
            created: Created::Yes,
            id: Some(id),
            pin: Some(Pin::new(dst)),
            error,
        }),
    }
}

/// Test seam: links fail as on a filesystem without them (`ENOTSUP`).
#[cfg(test)]
fn no_links() -> bool {
    NO_LINKS.with(|s| s.get())
}

#[cfg(not(test))]
fn no_links() -> bool {
    false
}

#[cfg(test)]
thread_local! {
    static NO_LINKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NO_EXCLUSIVE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static BEFORE_UNLINK: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_PIN: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_PIN_OPEN: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static TEMP_STAT_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NO_PINS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_CREATE: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_PROBE: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static DIR_FSYNC_ERRNO: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    static PROBE_STAT_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static FULL_FSYNC_ERRNO: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

/// Test seams on this thread (04 §8's faults at the exact step), each until its guard drops.
#[cfg(test)]
impl LocalFs {
    /// The exclusive rename fails as on a filesystem without it (`ENOTSUP`): the link + unlink
    /// fallback runs.
    pub(crate) fn seam_no_exclusive_rename() -> crate::remote::ops::SeamGuard {
        NO_EXCLUSIVE_RENAME.with(|s| s.set(true));
        crate::remote::ops::SeamGuard::new(|| NO_EXCLUSIVE_RENAME.with(|s| s.set(false)))
    }

    /// Links fail as on a filesystem without them (`ENOTSUP`, exFAT); with
    /// [`LocalFs::seam_no_exclusive_rename`], nothing can publish a finished copy atomically.
    pub(crate) fn seam_no_links() -> crate::remote::ops::SeamGuard {
        NO_LINKS.with(|s| s.set(true));
        crate::remote::ops::SeamGuard::new(|| NO_LINKS.with(|s| s.set(false)))
    }

    /// In that fallback: after the new name is made, before the old one is checked and
    /// unlinked; the hook gets the old name.
    pub(crate) fn seam_before_unlink(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&BEFORE_UNLINK, hook)
    }

    /// In [`FileOps::pin`]: after the caller read the entry's identity, before the pin looks at
    /// the name again; the hook gets the path.
    pub(crate) fn seam_before_pin(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&BEFORE_PIN, hook)
    }

    /// In a pin: after the name was looked at, before it is opened; the hook gets the name.
    pub(crate) fn seam_before_pin_open(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&BEFORE_PIN_OPEN, hook)
    }

    /// No descriptor can be held for a pin (as when too many files are open).
    pub(crate) fn seam_no_pins() -> crate::remote::ops::SeamGuard {
        NO_PINS.with(|f| f.set(true));
        crate::remote::ops::SeamGuard::new(|| NO_PINS.with(|f| f.set(false)))
    }

    /// A copy's temp cannot be stat'ed through its descriptor right after its create.
    pub(crate) fn seam_temp_stat_fails() -> crate::remote::ops::SeamGuard {
        TEMP_STAT_FAILS.with(|f| f.set(true));
        crate::remote::ops::SeamGuard::new(|| TEMP_STAT_FAILS.with(|f| f.set(false)))
    }

    /// The publish probe cannot stat its own file.
    pub(crate) fn seam_probe_stat_fails() -> crate::remote::ops::SeamGuard {
        PROBE_STAT_FAILS.with(|f| f.set(true));
        crate::remote::ops::SeamGuard::new(|| PROBE_STAT_FAILS.with(|f| f.set(false)))
    }

    /// Every folder `fsync` of a barrier fails with this errno.
    pub(crate) fn seam_dir_fsync_errno(errno: i32) -> crate::remote::ops::SeamGuard {
        DIR_FSYNC_ERRNO.with(|e| e.set(Some(errno)));
        crate::remote::ops::SeamGuard::new(|| DIR_FSYNC_ERRNO.with(|e| e.set(None)))
    }

    /// In the publish probe: after its file was renamed (or linked) to its second name, before
    /// the cleanup; the hook gets `"<first name>\n<second name>"`.
    pub(crate) fn seam_after_probe(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&AFTER_PROBE, hook)
    }

    /// macOS: the next `F_FULLFSYNC` of a barrier fails with this errno (once).
    #[cfg(target_os = "macos")]
    pub(crate) fn seam_full_fsync_errno(errno: i32) -> crate::remote::ops::SeamGuard {
        FULL_FSYNC_ERRNO.with(|e| e.set(Some(errno)));
        crate::remote::ops::SeamGuard::new(|| FULL_FSYNC_ERRNO.with(|e| e.set(None)))
    }

    /// Right after a copy's temp is made (its exclusive create), before any byte or attribute
    /// goes into it; the hook gets its path.
    pub(crate) fn seam_after_create(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&AFTER_CREATE, hook)
    }
}
