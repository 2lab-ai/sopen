//! The primitives the browser's rename, copy and move are built from (04 §5.6): renames that never
//! replace, `MKDIR`, `RMDIR`, links read and made, a same-host `copy-data` in chunks, staged
//! temps. They are a trait of their own, implemented for [`SftpRemote`](super::SftpRemote) in
//! `sftp/ops.rs` and for [`LocalFs`](super::LocalFs) in `local/ops.rs` — submodules, which see
//! their parent's private fields — so [`RemoteFs`] and its write algorithm stay as they are (02
//! §5). Nothing runs on the remote: no `cp`, `mv` or `rm` over ssh. S1-C writes them; S6-A adds
//! the plain rename.
//!
//! "Is a name taken?" is the filesystem's answer, never a comparison of strings (04 §5.2): an
//! exclusive create, a link or an exclusive rename that fails while the name exists is
//! [`Taken`]. Two names that look different may be one entry on a case-insensitive or
//! Unicode-normalising filesystem, and two that look alike may be two.

use super::{CreateError, RemoteFs};
use anyhow::{anyhow, Result};
use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

/// One step of a same-host copy (04 §5.1): `copy-data` and the local copies run in steps of this
/// size, so progress moves and Esc acts between them.
pub const COPY_STEP: u64 = 8 * 1024 * 1024;

/// What a copy keeps of its source (04 §5.1 "Metadata"): the permission bits and the times;
/// `None` where the server did not say (nothing is set for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    /// Permission bits (`& 0o7777`).
    pub mode: Option<u32>,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
}

/// A file copied under a temp name, not yet published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The temp's size when it was closed.
    pub size: u64,
    /// SHA-256 of the bytes written, when they passed through sopen; `None` when the host
    /// copied them itself (`copy-data`, `clonefile`), so sopen never saw them.
    pub sha256: Option<[u8; 32]>,
    /// The source as its open handle described it before the copy.
    pub source: FileMeta,
    /// What the copy could not keep (its mode, its times), for the report.
    pub notes: Vec<String>,
}

/// A source file opened for a copy to another host: its bytes, and what its open handle says.
pub struct Opened<'a> {
    pub read: Box<dyn Read + 'a>,
    pub meta: FileMeta,
}

/// The name is taken: the filesystem refused to make it because something is there (04 §5.2).
#[derive(Debug)]
pub struct Taken(pub String);

impl std::fmt::Display for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} exists", self.0)
    }
}

impl std::error::Error for Taken {}

/// The rename cannot happen here (another filesystem; a remote folder before slice S6) and
/// nothing changed: a move goes on as a copy, a check and a removal (04 §5.2).
#[derive(Debug)]
pub struct NoRename(pub String);

impl std::fmt::Display for NoRename {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoRename {}

pub fn taken(path: &str) -> anyhow::Error {
    anyhow!(Taken(path.to_string()))
}

pub fn is_taken(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Taken>())
}

pub fn is_no_rename(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<NoRename>())
}

/// A test seam set on this thread (a fault or a race at an exact step inside a backend), cleared
/// when this guard drops — even when the test fails.
#[cfg(test)]
pub(crate) struct SeamGuard(Option<Box<dyn FnOnce()>>);

#[cfg(test)]
impl SeamGuard {
    pub(crate) fn new(clear: impl FnOnce() + 'static) -> Self {
        Self(Some(Box::new(clear)))
    }
}

#[cfg(test)]
impl Drop for SeamGuard {
    fn drop(&mut self) {
        if let Some(clear) = self.0.take() {
            clear();
        }
    }
}

/// A seam's hook: gets the path at that step.
#[cfg(test)]
pub(crate) type SeamHook = Box<dyn FnMut(&str)>;

/// Runs the hook in `slot` with `path`, taken out while it runs (so it may use the file layer).
#[cfg(test)]
pub(crate) fn fire_seam(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<SeamHook>>>,
    path: &str,
) {
    let hook = slot.with(|s| s.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(path);
        slot.with(|s| *s.borrow_mut() = Some(hook));
    }
}

/// Sets `slot` to `hook` until the returned guard drops.
#[cfg(test)]
pub(crate) fn set_seam(
    slot: &'static std::thread::LocalKey<std::cell::RefCell<Option<SeamHook>>>,
    hook: impl FnMut(&str) + 'static,
) -> SeamGuard {
    slot.with(|s| *s.borrow_mut() = Some(Box::new(hook)));
    SeamGuard::new(move || slot.with(|s| *s.borrow_mut() = None))
}

/// Why a remote folder is not renamed before slice S6 (04 §5.3, §5.6).
pub const REMOTE_FOLDER_RENAME: &str = "renaming a folder on a remote needs SFTP's plain \
    rename, which sopen cannot send yet (it comes with slice S6); nothing was changed";

/// The file-operation primitives of one host. [`RemoteFs`] is its supertrait, so one connection
/// (a `Box<dyn FileOps + Send>`, an operation's own, 04 §5.1) serves both the stats and transfers
/// of `RemoteFs` and these. None of them replaces an existing name.
pub trait FileOps: RemoteFs {
    /// The device and inode of `path` on this machine — of the entry itself (`lstat`), or with
    /// `follow` of what a link there points at (`stat`); `None` over SFTP, which carries no
    /// inode.
    fn identity(&mut self, path: &str, follow: bool) -> Result<Option<(u64, u64)>>;

    /// `path`'s own permission bits, size and times (`lstat`).
    fn meta(&mut self, path: &str) -> Result<FileMeta>;

    /// Renames `from` to `to`, never replacing: [`Taken`] when `to` exists, [`NoRename`] when
    /// this host cannot rename it without replacing (another filesystem; over SFTP, a folder).
    /// Over SFTP a file is linked to the new name with `hardlink@openssh.com` and the old name
    /// removed; a link is made again at the new name and the old one removed. `Ok` carries what
    /// the report must say (both names left on one file when the old one could not be removed).
    fn rename_new(&mut self, from: &str, to: &str) -> Result<Vec<String>>;

    /// Whether [`FileOps::rename_new`] can rename a folder here.
    fn can_rename_dir(&self) -> bool {
        true
    }

    /// Makes the folder `path` (mode 0700 until its contents are in, 04 §5.1): [`Taken`] when
    /// the name exists.
    fn make_dir(&mut self, path: &str) -> Result<()>;

    /// Removes the empty folder `path`.
    fn remove_dir(&mut self, path: &str) -> Result<()>;

    /// The target of the symbolic link `path`, as stored.
    fn read_link(&mut self, path: &str) -> Result<String>;

    /// Makes a symbolic link at `link` pointing at `target`: [`Taken`] when `link` exists.
    fn make_symlink(&mut self, target: &str, link: &str) -> Result<()>;

    /// Gives the folder `path` the permission bits and times of `meta` (after its contents, 04
    /// §5.1). Never fails: what could not be set is returned as notes for the report.
    fn set_meta(&mut self, path: &str, meta: &FileMeta) -> Vec<String>;

    /// Copies the regular file `from` into `temp` on this same host — `temp` created exclusively,
    /// [`COPY_STEP`] at a time where the host copies, with `from`'s permission bits and times set
    /// through the temp's handle, flushed to disk and closed. `cancel` is checked between steps
    /// ([`Cancelled`](super::Cancelled)); `progress` gets the bytes copied so far. A failure says
    /// whether `temp` was created; the caller removes it.
    fn copy_within(
        &mut self,
        from: &str,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> std::result::Result<Staged, CreateError>;

    /// Opens the regular file `path` for reading by a copy to another host.
    fn open_read(&mut self, path: &str) -> Result<Opened<'_>>;

    /// Writes `src` into `temp`, created exclusively, then gives it `meta`'s permission bits and
    /// times through its handle, flushes and closes it — the write side of a copy from another
    /// host. `cancel` and `progress` as for [`FileOps::copy_within`].
    fn stage_from(
        &mut self,
        src: &mut dyn Read,
        meta: &FileMeta,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> std::result::Result<Staged, CreateError>;
}
