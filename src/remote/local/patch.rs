//! [`PatchFs`] on the local disk (07 §4): a plain copy and `pwrite`. It is the half of the trait
//! the tests run beside the real `sftp-server`; a local file's own Save stays 01 §6a's.

use super::{stat_of, LocalFs};
use crate::remote::patch::{CopyCtl, CopyEvent, Each, Handle, OpenFor, PatchFs};
use crate::remote::{Cancelled, CreateError, Created, Stat};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt, PermissionsExt};

/// Bytes per read of a copy and of a whole read.
const BUF: usize = 1 << 20;

/// What this backend moved on this thread, counted where it reads and writes (07 §12: "at the
/// client's read and write payloads"): the bytes a Save's own requests read and wrote, and —
/// apart — the bytes [`PatchFs::copy`] copied, which over SFTP the server moves and no request
/// carries.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Moved {
    pub read: u64,
    pub written: u64,
    pub copied: u64,
}

#[cfg(test)]
thread_local! {
    static MOVED: std::cell::Cell<Moved> = const {
        std::cell::Cell::new(Moved { read: 0, written: 0, copied: 0 })
    };
}

#[cfg(test)]
impl LocalFs {
    /// What [`PatchFs`] on the local disk has moved on this thread so far.
    pub(crate) fn moved() -> Moved {
        MOVED.with(|m| m.get())
    }
}

/// Accounts `read` and `written` payload bytes and `copied` bytes of a copy.
fn moved(read: usize, written: usize, copied: usize) {
    #[cfg(test)]
    MOVED.with(|m| {
        let mut now = m.get();
        now.read += read as u64;
        now.written += written as u64;
        now.copied += copied as u64;
        m.set(now);
    });
    let _ = (read, written, copied);
}

/// Up to `buf.len()` bytes at `offset`; fewer only at the end of the file.
fn read_full(file: &fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match file.read_at(&mut buf[got..], offset + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// A plain `fsync` (not macOS's `F_FULLFSYNC`), as the file copies' `finish` does.
fn sync(file: &fs::File) -> std::io::Result<()> {
    // SAFETY: an open descriptor.
    match unsafe { libc::fsync(file.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

impl PatchFs for LocalFs {
    fn can_copy(&self) -> bool {
        true
    }

    fn open(&mut self, path: &str, how: OpenFor) -> Result<Handle> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(how == OpenFor::Patch)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("cannot open {path}"))?;
        Ok(Handle::new(path, file))
    }

    fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("cannot create {path}"))
            .map_err(|e| CreateError::new(Created::No, e))?;
        Ok(Handle::new(path, file))
    }

    fn fstat(&mut self, file: &mut Handle) -> Result<Stat> {
        let path = file.path().to_string();
        let meta = file
            .get::<fs::File>()?
            .metadata()
            .with_context(|| format!("cannot stat {path}"))?;
        Ok(stat_of(&meta))
    }

    fn read_at(&mut self, file: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        let path = file.path().to_string();
        let mut buf = vec![0u8; len];
        let got = read_full(file.get::<fs::File>()?, offset, &mut buf)
            .with_context(|| format!("cannot read {path}"))?;
        moved(got, 0, 0);
        buf.truncate(got);
        Ok(buf)
    }

    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64> {
        let path = src.path().to_string();
        let file = src.get::<fs::File>()?;
        let (to, out) = match dst {
            Some(dst) => (dst.path().to_string(), Some(&*dst.get::<fs::File>()?)),
            None => (String::new(), None),
        };
        let mut buf = vec![0u8; BUF];
        let mut done = 0u64;
        while done < size {
            let want = (size - done).min(BUF as u64) as usize;
            let got = read_full(file, done, &mut buf[..want])
                .with_context(|| format!("cannot read {path}"))?;
            if got == 0 {
                break;
            }
            moved(got, 0, 0);
            if let (Some(write), Some(out)) = (each(done, &buf[..got])?, out) {
                out.write_all_at(&write, done)
                    .with_context(|| format!("cannot write {to}"))?;
                moved(0, write.len(), 0);
            }
            done += got as u64;
        }
        Ok(done)
    }

    fn write_at(&mut self, file: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()> {
        let path = file.path().to_string();
        file.get::<fs::File>()?
            .write_all_at(bytes, offset)
            .with_context(|| format!("cannot write {path}"))?;
        moved(0, bytes.len(), 0);
        Ok(())
    }

    fn set_owner(&mut self, file: &mut Handle, uid: u32, gid: u32) -> Result<()> {
        let path = file.path().to_string();
        std::os::unix::fs::fchown(&*file.get::<fs::File>()?, Some(uid), Some(gid))
            .with_context(|| format!("cannot set owner on {path}"))
    }

    fn set_handle_mode(&mut self, file: &mut Handle, mode: u32) -> Result<()> {
        let path = file.path().to_string();
        file.get::<fs::File>()?
            .set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| format!("cannot set mode on {path}"))
    }

    fn fsync(&mut self, file: &mut Handle) -> Result<()> {
        let path = file.path().to_string();
        sync(file.get::<fs::File>()?).with_context(|| format!("cannot sync {path}"))
    }

    /// A plain copy through this process, in the pace's requests: what `copy-data` does on a
    /// server, with the same stops between requests and the same flushes on the way.
    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()> {
        let (from, to) = (src.path().to_string(), dst.path().to_string());
        let (src, dst) = (&*src.get::<fs::File>()?, &*dst.get::<fs::File>()?);
        let mut buf = vec![0u8; BUF];
        let mut done = 0u64;
        while done < size {
            if (ctl.cancel)() {
                return Err(anyhow!(Cancelled));
            }
            let n = ctl.pace.next(size - done);
            let began = (ctl.clock)();
            let mut copied = 0u64;
            while copied < n {
                let want = (n - copied).min(BUF as u64) as usize;
                let got = read_full(src, done + copied, &mut buf[..want])
                    .with_context(|| format!("cannot read {from}"))?;
                if got == 0 {
                    break;
                }
                dst.write_all_at(&buf[..got], done + copied)
                    .with_context(|| format!("cannot write {to}"))?;
                moved(0, 0, got);
                copied += got as u64;
            }
            if copied < n {
                // Only what was copied counts: a source shorter than asked is an error here,
                // never a request reported whole.
                let short = std::io::Error::from(ErrorKind::UnexpectedEof);
                return Err(anyhow!(short).context(format!(
                    "{from} ended after {} of {size} bytes",
                    done + copied
                )));
            }
            ctl.pace.copied(copied, (ctl.clock)().saturating_sub(began));
            done += copied;
            (ctl.progress)(CopyEvent::Copied(done));
            if done < size && ctl.pace.sync_due() {
                let began = (ctl.clock)();
                sync(dst).with_context(|| format!("cannot sync {to}"))?;
                ctl.pace.synced((ctl.clock)().saturating_sub(began));
                (ctl.progress)(CopyEvent::Synced(done));
            }
        }
        Ok(())
    }

    fn close(&mut self, file: Handle) {
        drop(file);
    }
}
