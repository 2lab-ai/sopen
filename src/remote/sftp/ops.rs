//! [`FileOps`] over SFTP (04 §5.6): a file renamed by `hardlink@openssh.com` then `REMOVE`
//! (never `posix-rename@openssh.com`, which replaces), `MKDIR`, `RMDIR`, `Fs::symlink(target,
//! link)`, `copy-data` in 8 MiB chunks into a temp created `CREAT|EXCL`, published by a hard link.
//! The `copy-data` loop here is sopen's only one: a hex Save's server copy (07 §4.2 step 5,
//! `sftp/patch.rs`) runs it too, paced by time instead of in fixed steps ([`Pace`]).
//!
//! Every call is one `block_on` of this session's runtime, never one inside another: a copy from
//! another session reads its source (that session's `block_on`) between this one's writes.

use super::{
    answered, changed, failed_change, failed_create, perms, timed_out, unknown, within, SftpRemote,
    CHUNK, META_TIMEOUT, STALL_TIMEOUT,
};
use crate::remote::ops::{
    taken, FileMeta, FileOps, NoRename, Opened, Renamed, StageFailure, Staged, COPY_STEP,
    REMOTE_FOLDER_RENAME,
};
use crate::remote::patch::{CopyCtl, CopyEvent, Pace};
use crate::remote::{split_parent, Cancelled, CreateError, Created, Hashing, RemoteFs};
use anyhow::{anyhow, Context, Result};
use bytes::BytesMut;
use openssh_sftp_client::error::{Error as SftpErr, SftpErrorKind};
use openssh_sftp_client::file::File;
use openssh_sftp_client::metadata::{MetaData, MetaDataBuilder};
use openssh_sftp_client::{Sftp, UnixTimeStamp};
use std::io::{Read, Write};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::runtime::Runtime;

fn file_meta(m: &MetaData) -> FileMeta {
    FileMeta {
        size: m.len().unwrap_or(0),
        mode: m.permissions().map(|p| p.as_raw().bits() & 0o7777),
        atime: m.accessed().map(|t| t.as_system_time()),
        mtime: m.modified().map(|t| t.as_system_time()),
    }
}

/// The source of a copy: opened for reading, and a regular file by its own handle.
async fn open_source(sftp: &Sftp, path: &str) -> Result<(File, FileMeta)> {
    let mut file = within(META_TIMEOUT, sftp.open(path))
        .await
        .map_err(|()| timed_out(&format!("open {path}"), META_TIMEOUT))?
        .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
    let meta = within(META_TIMEOUT, file.metadata())
        .await
        .map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))?
        .map_err(|e| anyhow!("cannot stat {path}: {e}"))?;
    if meta.file_type().is_some_and(|t| !t.is_file()) {
        return Err(anyhow!("{path} is no longer a regular file"));
    }
    Ok((file, file_meta(&meta)))
}

/// Creates `temp` exclusively (`CREAT|EXCL`), made private (0600) through its handle before any
/// content; a server that refuses that leaves its default mode until the copy's own is set after
/// the content. Opened for reading too: the library sends `fstat` (the size checked after the
/// copy) only on a readable handle.
pub(super) async fn create_temp(sftp: &Sftp, temp: &str) -> std::result::Result<File, CreateError> {
    let opened = within(
        META_TIMEOUT,
        sftp.options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(temp),
    )
    .await;
    let mut file = match opened {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => return Err(failed_create(temp, e)),
        Err(()) => {
            return Err(CreateError::new(
                Created::Unknown,
                unknown(format!(
                    "create {temp}: request sent, no reply in {}s",
                    META_TIMEOUT.as_secs()
                )),
            ))
        }
    };
    if let Ok(Err(e)) = within(META_TIMEOUT, file.set_permissions(perms(0o600))).await {
        crate::debug!("set mode 600 on {temp}: {e}");
    }
    Ok(file)
}

/// Sets `meta`'s times, then its permission bits, through the handle — two requests, so a
/// server refusing one still gets the other. What could not be set is returned as notes.
async fn set_handle_meta(file: &mut File, path: &str, meta: &FileMeta) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(times) = times_of(meta, path, &mut notes) {
        match within(META_TIMEOUT, file.set_metadata(times)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => notes.push(format!("cannot set the times of {path}: {e}")),
            Err(()) => notes.push(format!("setting the times of {path} got no reply")),
        }
    }
    if let Some(mode) = meta.mode {
        match within(META_TIMEOUT, file.set_permissions(perms(mode))).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => notes.push(format!("cannot set the mode of {path} to {mode:o}: {e}")),
            Err(()) => notes.push(format!("setting the mode of {path} got no reply")),
        }
    }
    notes
}

/// The `setstat` attributes for `meta`'s times (whole seconds: SFTP v3), or `None` with a note.
fn times_of(meta: &FileMeta, path: &str, notes: &mut Vec<String>) -> Option<MetaData> {
    let mtime = meta.mtime?;
    let stamp = |t| UnixTimeStamp::new(t).ok();
    match (stamp(meta.atime.unwrap_or(mtime)), stamp(mtime)) {
        (Some(a), Some(m)) => Some(MetaDataBuilder::new().time(a, m).create()),
        _ => {
            notes.push(format!(
                "the times of {path} cannot be sent over SFTP (before 1970 or after 2106)"
            ));
            None
        }
    }
}

/// After the content: mode and times through the handle (notes), flushed to disk where the
/// server offers `fsync@openssh.com`, the size by the handle, closed.
async fn finish(
    sftp: &Sftp,
    mut file: File,
    path: &str,
    meta: &FileMeta,
) -> Result<(u64, Vec<String>)> {
    let notes = set_handle_meta(&mut file, path, meta).await;
    if sftp.support_fsync() {
        within(STALL_TIMEOUT, file.sync_all())
            .await
            .map_err(|()| timed_out(&format!("fsync {path}"), STALL_TIMEOUT))?
            .map_err(|e| anyhow!("cannot fsync {path}: {e}"))?;
    }
    let size = within(META_TIMEOUT, file.metadata())
        .await
        .map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))?
        .map_err(|e| anyhow!("cannot stat {path}: {e}"))?
        .len()
        .unwrap_or(0);
    within(META_TIMEOUT, file.close())
        .await
        .map_err(|()| timed_out(&format!("close {path}"), META_TIMEOUT))?
        .map_err(|e| anyhow!("cannot close {path}: {e}"))?;
    Ok((size, notes))
}

/// Reads and writes `src` into `dst` through this process, from both files' current offsets,
/// hashing what it writes.
async fn pump(
    src: &mut File,
    dst: &mut File,
    path: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<[u8; 32]> {
    let mut out = Hashing::new(std::io::sink());
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!(Cancelled));
        }
        let chunk = within(
            STALL_TIMEOUT,
            src.read(CHUNK as u32, BytesMut::with_capacity(CHUNK)),
        )
        .await
        .map_err(|()| timed_out("read the source", STALL_TIMEOUT))?
        .map_err(|e| anyhow!("cannot read the source: {e}"))?;
        let Some(bytes) = chunk else { break };
        within(STALL_TIMEOUT, dst.write_all(&bytes))
            .await
            .map_err(|()| timed_out(&format!("write {path}"), STALL_TIMEOUT))?
            .map_err(|e| anyhow!("cannot write {path}: {e}"))?;
        out.write_all(&bytes)?;
        done += bytes.len() as u64;
        progress(done);
    }
    Ok(out.finish().sha256)
}

/// The server's `copy-data`, one request outstanding, each sized by `ctl.pace`: [`COPY_STEP`]s
/// for a file copy (04 §5.1), requests of about a second for a hex Save (07 §4.2 step 5), which
/// also sends `fsync@openssh.com` on `dst` whenever the pace says one is due (`fsync`: the server
/// offers it) and counts its time in the rate. Every request has `limit` to be answered.
/// `ctl.cancel` is asked before each request; `ctl.progress` hears of each one answered.
/// `Ok(false)`: the server refused the first request (it advertises the extension but will not
/// run it) — nothing was copied.
pub(super) async fn copy_data(
    src: &mut File,
    dst: &mut File,
    size: u64,
    path: &str,
    limit: Duration,
    fsync: bool,
    ctl: &mut CopyCtl<'_>,
) -> Result<bool> {
    let mut done = 0u64;
    while done < size {
        if (ctl.cancel)() {
            return Err(anyhow!(Cancelled));
        }
        let n = ctl.pace.next(size - done);
        let step = NonZeroU64::new(n).expect("a step of at least one byte");
        let began = (ctl.clock)();
        match within(limit, src.copy_to(dst, step)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if done == 0 && answered(&e) => {
                crate::debug!("the server refused copy-data into {path} ({e})");
                return Ok(false);
            }
            Ok(Err(e)) => return Err(anyhow!("cannot copy into {path}: {e}")),
            Err(()) => return Err(timed_out(&format!("copy into {path}"), limit)),
        }
        ctl.pace.copied(n, (ctl.clock)().saturating_sub(began));
        done += n;
        (ctl.progress)(CopyEvent::Copied(done));
        if fsync && done < size && ctl.pace.sync_due() {
            let began = (ctl.clock)();
            within(STALL_TIMEOUT, dst.sync_all())
                .await
                .map_err(|()| timed_out(&format!("fsync {path}"), STALL_TIMEOUT))?
                .map_err(|e| anyhow!("cannot fsync {path}: {e}"))?;
            ctl.pace.synced((ctl.clock)().saturating_sub(began));
            (ctl.progress)(CopyEvent::Synced(done));
        }
    }
    Ok(true)
}

/// A remote file read by a copy to another host, one request at a time, each a `block_on` of its
/// own session's runtime.
struct Reader<'a> {
    rt: &'a Runtime,
    file: Option<File>,
    path: String,
    buf: BytesMut,
}

impl Read for Reader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.buf.is_empty() {
            let Some(file) = self.file.as_mut() else {
                return Ok(0);
            };
            let got = self.rt.block_on(within(
                STALL_TIMEOUT,
                file.read(CHUNK as u32, BytesMut::with_capacity(CHUNK)),
            ));
            match got {
                Ok(Ok(Some(bytes))) => self.buf = bytes,
                Ok(Ok(None)) => return Ok(0),
                Ok(Err(e)) => {
                    return Err(std::io::Error::other(format!(
                        "cannot read {}: {e}",
                        self.path
                    )))
                }
                Err(()) => {
                    let e = timed_out(&format!("read {}", self.path), STALL_TIMEOUT);
                    return Err(std::io::Error::other(e.to_string()));
                }
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf.split_to(n));
        Ok(n)
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = self
                .rt
                .block_on(within(Duration::from_secs(5), file.close()));
        }
    }
}

impl SftpRemote {
    /// A hard link refused for permission: nothing was moved, and the likely cause besides the
    /// folders' permissions (on Linux, `fs.protected_hardlinks=1` refuses a link to a file this
    /// account does not own, 04 §5.6).
    pub(crate) fn link_refused(from: &str, to: &str, why: &dyn std::fmt::Display) -> anyhow::Error {
        anyhow!(
            "cannot link {from} to {to}: {why}; nothing was moved (besides the folders' \
             permissions: on Linux, fs.protected_hardlinks refuses a link to a file this account \
             does not own)"
        )
    }

    /// A failed request that makes `path`: [`taken`] when the name exists now, else the server's
    /// reason (or outcome unknown).
    fn made_or_taken(&mut self, what: &str, path: &str, e: SftpErr) -> anyhow::Error {
        if answered(&e) && matches!(self.lstat_opt(path), Ok(Some(_))) {
            return taken(path);
        }
        failed_change(what, e)
    }

    /// The filesystem ids of two paths' folders, where the server offers `statvfs@openssh.com`.
    fn same_filesystem(&mut self, a: &str, b: &str) -> Option<bool> {
        if !self.sftp.as_ref().is_some_and(|s| s.support_statvfs()) {
            return None;
        }
        let (rt, _, fs) = self.parts();
        let mut fsid = |p: &str| match rt.block_on(within(META_TIMEOUT, fs.statvfs(p))) {
            Ok(Ok(st)) => Some(st.fsid),
            _ => None,
        };
        Some(fsid(split_parent(a).0)? == fsid(split_parent(b).0)?)
    }

    /// `hardlink@openssh.com` from `from` to `to`: [`taken`] when `to` exists, [`NoRename`] when
    /// the two are on different filesystems (or the server cannot say they are not), the
    /// server's reason otherwise.
    fn hard_link_new(&mut self, from: &str, to: &str) -> Result<()> {
        if !self.can_link() {
            return Err(anyhow!(
                "this server does not offer hardlink@openssh.com, which sopen needs to move {from} \
                 without the risk of replacing {to}; nothing was changed"
            ));
        }
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.hard_link(from, to)));
        let e = match got {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => e,
            Err(()) => return changed(&format!("link {from} to {to}"), Err(())),
        };
        if !answered(&e) {
            return Err(failed_change(&format!("link {from} to {to}"), e));
        }
        if matches!(self.lstat_opt(to), Ok(Some(_))) {
            return Err(taken(to));
        }
        if matches!(e, SftpErr::SftpError(SftpErrorKind::PermDenied, _)) {
            return Err(Self::link_refused(from, to, &e));
        }
        if self.same_filesystem(from, to) == Some(true) {
            return Err(anyhow!(
                "cannot link {from} to {to}: {e}; nothing was moved"
            ));
        }
        Err(anyhow!(NoRename(format!(
            "cannot link {from} to {to} ({e}): another filesystem"
        ))))
    }
}

impl FileOps for SftpRemote {
    fn identity(
        &mut self,
        _path: &str,
        _follow: bool,
    ) -> Result<Option<crate::remote::ops::Ident>> {
        Ok(None)
    }

    fn meta(&mut self, path: &str) -> Result<FileMeta> {
        let (rt, _, fs) = self.parts();
        let got = rt
            .block_on(within(META_TIMEOUT, fs.symlink_metadata(path)))
            .map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))?
            .map_err(|e| anyhow!("cannot stat {path}: {e}"))?;
        Ok(file_meta(&got))
    }

    fn rename_new(&mut self, from: &str, to: &str) -> Result<Renamed> {
        let own = self
            .lstat_opt(from)?
            .with_context(|| format!("{from}: no such file or directory"))?;
        if own.is_dir {
            return Err(anyhow!(NoRename(REMOTE_FOLDER_RENAME.to_string())));
        }
        let target = if own.is_symlink {
            // A link is made again at the new name (never a hard link to a link, whose meaning
            // differs between systems), then the old one removed.
            let target = self.read_link(from)?;
            self.make_symlink(&target, to)?;
            Some(target)
        } else {
            self.hard_link_new(from, to)?;
            None
        };
        #[cfg(test)]
        crate::remote::ops::fire_seam(&BEFORE_REMOVE, from);
        // Right before REMOVE: is the old name still the entry that was linked? Another writer
        // may have put something else there meanwhile; that is never removed. SFTP carries no
        // inode and whole-second times, so a file rewritten to the same size within the same
        // second cannot be told apart — the same best-effort limit as 01 §5's writes.
        let both = |why: String| {
            Renamed::OldRemains(format!(
                "{to} is in place, but {from} was not removed ({why}): both names exist — {from} \
                 and {to}"
            ))
        };
        // When it cannot be checked again, it is left, and the report says so without claiming
        // what is there.
        let unconfirmed = |e: anyhow::Error| {
            Renamed::Unconfirmed(format!(
                "{to} is in place; {from} was left, as it could not be checked again ({e:#}): \
                 whether it is still there could not be confirmed"
            ))
        };
        let now = match self.lstat_opt(from) {
            Ok(Some(now)) => now,
            Ok(None) => return Ok(Renamed::Moved),
            Err(e) => return Ok(unconfirmed(e)),
        };
        let kind = |s: &crate::remote::Stat| (s.is_dir, s.is_file, s.is_symlink);
        if kind(&now) != kind(&own) || now.size != own.size || now.mtime != own.mtime {
            let how = crate::remote::describe_change(&own.fingerprint(), &now);
            return Ok(both(format!("another entry has its name now: {how}")));
        }
        if let Some(target) = &target {
            match self.read_link(from) {
                Ok(t) if &t == target => {}
                Ok(t) => {
                    let how = format!("it is now a link to {t}, not {target}");
                    return Ok(both(format!("another entry has its name now: {how}")));
                }
                Err(e) => return Ok(unconfirmed(e)),
            }
        }
        match self.remove(from) {
            Ok(()) => Ok(Renamed::Moved),
            // Did the removal happen (a reply lost) or not? The name says.
            Err(e) => Ok(match self.lstat_opt(from) {
                Ok(None) => Renamed::Moved,
                Ok(Some(_)) => both(format!("removing it failed: {e:#}")),
                Err(_) => Renamed::Unconfirmed(format!(
                    "{to} is in place; the removal of {from} could not be confirmed ({e:#})"
                )),
            }),
        }
    }

    fn can_rename_dir(&self) -> bool {
        false
    }

    /// `statvfs@openssh.com`'s `f_fsid`, where the server offers it.
    fn fs_id(&mut self, path: &str) -> Option<u64> {
        if !self.sftp.as_ref().is_some_and(|s| s.support_statvfs()) {
            return None;
        }
        let (rt, _, fs) = self.parts();
        match rt.block_on(within(META_TIMEOUT, fs.statvfs(path))) {
            Ok(Ok(st)) => Some(st.fsid),
            _ => None,
        }
    }

    fn make_dir(&mut self, path: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(
            META_TIMEOUT,
            fs.dir_builder().permissions(perms(0o700)).create(path),
        ));
        match got {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(self.made_or_taken(&format!("make the folder {path}"), path, e)),
            Err(()) => changed(&format!("make the folder {path}"), Err(())),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<()> {
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.remove_dir(path)));
        changed(&format!("remove the folder {path}"), got)
    }

    fn read_link(&mut self, path: &str) -> Result<String> {
        let (rt, _, fs) = self.parts();
        rt.block_on(within(META_TIMEOUT, fs.read_link(path)))
            .map_err(|()| timed_out(&format!("read the link {path}"), META_TIMEOUT))?
            .map_err(|e| anyhow!("cannot read the link {path}: {e}"))?
            .into_os_string()
            .into_string()
            .map_err(|_| anyhow!("the link {path} points at a name that is not UTF-8"))
    }

    fn make_symlink(&mut self, target: &str, link: &str) -> Result<()> {
        // `Fs::symlink(target, link)`: the library writes SSH_FXP_SYMLINK's two paths in
        // OpenSSH's order (target first), so they are passed as named, never swapped.
        let (rt, _, fs) = self.parts();
        let got = rt.block_on(within(META_TIMEOUT, fs.symlink(target, link)));
        match got {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(self.made_or_taken(&format!("make the link {link}"), link, e)),
            Err(()) => changed(&format!("make the link {link}"), Err(())),
        }
    }

    /// By path (`setstat`): SFTP cannot set a folder's attributes through a handle. The folder is
    /// the one this operation made moments ago.
    fn set_meta(&mut self, path: &str, meta: &FileMeta) -> Vec<String> {
        let mut notes = Vec::new();
        let times = times_of(meta, path, &mut notes);
        let (rt, _, fs) = self.parts();
        if let Some(times) = times {
            match rt.block_on(within(META_TIMEOUT, fs.set_metadata(path, times))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => notes.push(format!("cannot set the times of {path}: {e}")),
                Err(()) => notes.push(format!("setting the times of {path} got no reply")),
            }
        }
        if let Some(mode) = meta.mode {
            match rt.block_on(within(META_TIMEOUT, fs.set_permissions(path, perms(mode)))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => notes.push(format!("cannot set the mode of {path} to {mode:o}: {e}")),
                Err(()) => notes.push(format!("setting the mode of {path} got no reply")),
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
        let (rt, sftp, _) = self.parts();
        rt.block_on(async {
            let (mut src, source) = open_source(sftp, from)
                .await
                .map_err(StageFailure::not_created)?;
            let mut dst = create_temp(sftp, temp).await?;
            // From here `temp` is ours.
            let fill = async {
                // A file copy keeps 04 §5.1's fixed steps, and no fsync until the content is in.
                let started = std::time::Instant::now();
                let mut ctl = CopyCtl {
                    pace: Pace::steps(COPY_STEP),
                    clock: &mut || started.elapsed(),
                    cancel: &|| cancel.load(Ordering::Relaxed),
                    progress: &mut |event| {
                        if let CopyEvent::Copied(done) = event {
                            progress(done)
                        }
                    },
                };
                let size = source.size;
                let copied = sftp.support_copy()
                    && copy_data(
                        &mut src,
                        &mut dst,
                        size,
                        temp,
                        STALL_TIMEOUT,
                        false,
                        &mut ctl,
                    )
                    .await?;
                let sha256 = if copied {
                    None
                } else {
                    if sftp.support_copy() {
                        crate::debug!("copying into {temp} through sopen instead");
                    }
                    Some(pump(&mut src, &mut dst, temp, cancel, progress).await?)
                };
                if cancel.load(Ordering::Relaxed) {
                    return Err(anyhow!(Cancelled));
                }
                let (size, notes) = finish(sftp, dst, temp, &source).await?;
                Ok(Staged {
                    size,
                    sha256,
                    source,
                    notes,
                    id: None,
                    pin: None,
                })
            };
            let staged = fill
                .await
                .map_err(|e| StageFailure::from(CreateError::new(Created::Yes, e)));
            let _ = within(Duration::from_secs(5), src.close()).await;
            staged
        })
    }

    fn open_read(&mut self, path: &str) -> Result<Opened<'_>> {
        let (rt, sftp, _) = self.parts();
        let (file, meta) = rt.block_on(open_source(sftp, path))?;
        Ok(Opened {
            read: Box::new(Reader {
                rt,
                file: Some(file),
                path: path.to_string(),
                buf: BytesMut::new(),
            }),
            meta,
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
        let (rt, sftp, _) = self.parts();
        let mut file = rt.block_on(create_temp(sftp, temp))?;
        // From here `temp` is ours. `src` may be another session's reader, so it is read here,
        // outside this session's `block_on`.
        let written = (|| -> Result<[u8; 32]> {
            let mut out = Hashing::new(std::io::sink());
            let mut buf = vec![0u8; CHUNK];
            let mut done = 0u64;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(anyhow!(Cancelled));
                }
                let n = match src.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(anyhow!(e).context("cannot read the source")),
                };
                rt.block_on(within(STALL_TIMEOUT, file.write_all(&buf[..n])))
                    .map_err(|()| timed_out(&format!("write {temp}"), STALL_TIMEOUT))?
                    .map_err(|e| anyhow!("cannot write {temp}: {e}"))?;
                out.write_all(&buf[..n])?;
                done += n as u64;
                progress(done);
            }
            Ok(out.finish().sha256)
        })();
        let staged = written.and_then(|sha256| {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            // The file moves into `finish`, which closes it (its last reference).
            let (size, notes) = rt.block_on(finish(sftp, file, temp, meta))?;
            Ok(Staged {
                size,
                sha256: Some(sha256),
                source: *meta,
                notes,
                id: None,
                pin: None,
            })
        });
        staged.map_err(|e| StageFailure::from(CreateError::new(Created::Yes, e)))
    }
}

#[cfg(test)]
thread_local! {
    static BEFORE_REMOVE: std::cell::RefCell<Option<crate::remote::ops::SeamHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
impl SftpRemote {
    /// Test seam on this thread: in a rename, after the new name is made (the hard link, or the
    /// link made again) and before the old one is checked and removed; the hook gets the old
    /// name.
    pub(crate) fn seam_before_remove(
        hook: impl FnMut(&str) + 'static,
    ) -> crate::remote::ops::SeamGuard {
        crate::remote::ops::set_seam(&BEFORE_REMOVE, hook)
    }

    /// Whether the server advertises `copy-data` (OpenSSH 9.0 and later, unless refused by `-P`).
    pub(crate) fn offers_copy_data(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.support_copy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_of_and_file_meta_agree() {
        let Some(mut fs) = SftpRemote::local_server() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, "abc").unwrap();
        let path = f.to_str().unwrap();
        let meta = fs.meta(path).unwrap();
        let stat = fs.lstat_opt(path).unwrap().unwrap();
        assert_eq!(meta.size, 3);
        assert_eq!(meta.mode, Some(stat.mode));
        assert_eq!(meta.mtime, Some(stat.mtime));
    }
}
