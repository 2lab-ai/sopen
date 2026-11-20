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
use crate::remote::ops::{taken, FileMeta, FileOps, NoRename, Opened, Staged};
use crate::remote::{split_parent, Cancelled, CreateError, Created, Hashing};
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
    dir: bool,
    link: bool,
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
        dir: kind == libc::S_IFDIR,
        link: kind == libc::S_IFLNK,
    })
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
fn create_temp(temp: &str) -> std::result::Result<fs::File, CreateError> {
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
            CreateError::new(Created::No, error)
        })?;
    #[cfg(test)]
    crate::remote::ops::fire_seam(&AFTER_CREATE, temp);
    Ok(file)
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
    file.sync_all()
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
    fn identity(&mut self, path: &str, follow: bool) -> Result<Option<(u64, u64)>> {
        let meta = if follow {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        match meta {
            Ok(m) => Ok(Some((m.dev(), m.ino()))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot stat {path}")),
        }
    }

    fn meta(&mut self, path: &str) -> Result<FileMeta> {
        let m = fs::symlink_metadata(path).with_context(|| format!("cannot stat {path}"))?;
        Ok(meta_of(&m))
    }

    fn rename_new(&mut self, from: &str, to: &str) -> Result<Vec<String>> {
        let (f, t) = (c_path(from)?, c_path(to)?);
        #[cfg(test)]
        let tried = if NO_EXCLUSIVE_RENAME.with(|s| s.get()) {
            Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
        } else {
            rename_excl(&f, &t)
        };
        #[cfg(not(test))]
        let tried = rename_excl(&f, &t);
        let e = match tried {
            Ok(()) => return Ok(Vec::new()),
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
        let made = if own.link {
            read_link_at(&folder, &name).and_then(|target| {
                // SAFETY: two NUL-terminated paths; the new link's is relative to the working
                // directory, as `to` is given.
                zero_ok(unsafe { libc::symlinkat(target.as_ptr(), libc::AT_FDCWD, t.as_ptr()) })
            })
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
            Err(e) => return Err(e).with_context(|| format!("cannot rename {from} to {to}")),
        }
        #[cfg(test)]
        crate::remote::ops::fire_seam(&BEFORE_UNLINK, from);
        // Right before the unlink: is the old name still the entry that was linked (its device
        // and inode)? Another writer may have put something else there; that is never removed.
        // Best effort: between this check and the unlink the name can still be swapped — the
        // two steps are not one atomic rename.
        let both = |why: String| {
            vec![format!(
                "{to} is in place, but {from} was not removed ({why}): both names exist — {from} \
                 and {to}"
            )]
        };
        match stat_at(&folder, &name) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Ok(both(format!("it could not be checked again: {e}"))),
            Ok(now) if (now.dev, now.ino) != (own.dev, own.ino) => {
                return Ok(both("another entry has its name now".into()))
            }
            Ok(_) => {}
        }
        // SAFETY: an open descriptor and a NUL-terminated name; flags 0 removes a non-folder.
        Ok(
            match zero_ok(unsafe { libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0) }) {
                Ok(()) => Vec::new(),
                Err(e) => both(format!("removing it failed: {e}")),
            },
        )
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
    ) -> std::result::Result<Staged, CreateError> {
        let (mut src, src_meta) =
            open_source(from).map_err(|e| CreateError::new(Created::No, e))?;
        let source = meta_of(&src_meta);
        let ours = |e: anyhow::Error| CreateError::new(Created::Yes, e);
        let dst = create_temp(temp)?;
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
            })
        };
        fill().map_err(ours)
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
    ) -> std::result::Result<Staged, CreateError> {
        let dst = create_temp(temp)?;
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
            })
        };
        fill().map_err(|e| CreateError::new(Created::Yes, e))
    }
}

#[cfg(test)]
thread_local! {
    static NO_EXCLUSIVE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static BEFORE_UNLINK: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_CREATE: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
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

    /// In that fallback: after the new name is made, before the old one is checked and
    /// unlinked; the hook gets the old name.
    pub(crate) fn seam_before_unlink(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&BEFORE_UNLINK, hook)
    }

    /// Right after a copy's temp is made (its exclusive create), before any byte or attribute
    /// goes into it; the hook gets its path.
    pub(crate) fn seam_after_create(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&AFTER_CREATE, hook)
    }
}
