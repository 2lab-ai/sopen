//! [`FileOps`] on the local disk (04 §5.6): `renameat2(RENAME_NOREPLACE)` on Linux and
//! `renamex_np(RENAME_EXCL)` on macOS (else `link` + `unlink`), `mkdir`, `rmdir`, links, a copy
//! into a temp created exclusively (`clonefile` on macOS, `copy_file_range` on Linux, else read
//! and write), published by the same never-replacing rename.

use super::LocalFs;
use crate::remote::ops::{taken, FileMeta, FileOps, NoRename, Opened, Staged};
use crate::remote::{Cancelled, CreateError, Created, Hashing};
use anyhow::{anyhow, Context, Result};
use std::ffi::CString;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};

/// Bytes per read and write of a copy through this process.
const BUF: usize = 1024 * 1024;

fn c_path(path: &str) -> Result<CString> {
    CString::new(path).map_err(|_| anyhow!("{path}: a path with a NUL byte"))
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

/// Creates `temp` exclusively, private until its copy is complete.
fn create_temp(temp: &str) -> std::result::Result<fs::File, CreateError> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)
        .map_err(|e| {
            let error = if e.kind() == ErrorKind::AlreadyExists {
                taken(temp)
            } else {
                anyhow!(e).context(format!("cannot create {temp}"))
            };
            CreateError::new(Created::No, error)
        })
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

/// macOS: the filesystem's own copy (`fclonefileat`, APFS), which creates `temp` itself and
/// fails if it exists. `None`: not here (another volume, another filesystem) — copy the bytes.
#[cfg(target_os = "macos")]
fn clone_into(src: &fs::File, temp: &str) -> Option<std::result::Result<(), CreateError>> {
    use std::os::fd::AsRawFd;
    let to = match c_path(temp) {
        Ok(to) => to,
        Err(e) => return Some(Err(CreateError::new(Created::No, e))),
    };
    // SAFETY: an open descriptor and a NUL-terminated path.
    let r = unsafe { libc::fclonefileat(src.as_raw_fd(), libc::AT_FDCWD, to.as_ptr(), 0) };
    if r == 0 {
        return Some(Ok(()));
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EEXIST) {
        return Some(Err(CreateError::new(Created::No, taken(temp))));
    }
    let elsewhere = [
        libc::EXDEV,
        libc::ENOTSUP,
        libc::EOPNOTSUPP,
        libc::EINVAL,
        libc::ENOSYS,
    ];
    if is_errno(&e, &elsewhere) {
        return None;
    }
    Some(Err(CreateError::new(
        Created::No,
        anyhow!(e).context(format!("cannot create {temp}")),
    )))
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
        let e = match rename_excl(&f, &t) {
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
        // No exclusive rename here: a second name, then the old one removed.
        let own = fs::symlink_metadata(from).with_context(|| format!("cannot stat {from}"))?;
        if own.is_dir() {
            return Err(anyhow!(
                "this filesystem cannot rename the folder {from} without the risk of replacing \
                 {to}; nothing was changed"
            ));
        }
        let made = if own.file_type().is_symlink() {
            let target = fs::read_link(from).with_context(|| format!("cannot read {from}"))?;
            std::os::unix::fs::symlink(target, to)
        } else {
            fs::hard_link(from, to)
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
        Ok(match fs::remove_file(from) {
            Ok(()) => Vec::new(),
            Err(e) => vec![format!(
                "{to} is in place, but {from} could not be removed ({e}): both names exist"
            )],
        })
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
        #[cfg(target_os = "macos")]
        if let Some(cloned) = clone_into(&src, temp) {
            cloned?;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(temp)
                .with_context(|| format!("cannot open {temp}"))
                .map_err(ours)?;
            progress(source.size);
            if cancel.load(Ordering::Relaxed) {
                return Err(ours(anyhow!(Cancelled)));
            }
            let notes = finish(&file, temp, &source).map_err(ours)?;
            let size = file.metadata().map_err(|e| ours(e.into()))?.len();
            return Ok(Staged {
                size,
                sha256: None,
                source,
                notes,
            });
        }
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
            Ok(Staged {
                size: dst.metadata()?.len(),
                sha256,
                source,
                notes,
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
            Ok(Staged {
                size: dst.metadata()?.len(),
                sha256: Some(sha256),
                source: *meta,
                notes,
            })
        };
        fill().map_err(|e| CreateError::new(Created::Yes, e))
    }
}
