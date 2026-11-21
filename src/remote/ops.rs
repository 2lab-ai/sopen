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

use super::{CreateError, Created, RemoteFs};
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

/// An entry's identity on this machine: its device and inode — and its status-change time, which
/// a new file given a freed inode's number (ext4 reuses one at once) does not share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub dev: u64,
    pub ino: u64,
    /// `st_ctime`, seconds and nanoseconds.
    pub ctime: (i64, i64),
}

impl Ident {
    /// The device and inode alone (one entry seen twice at one moment).
    pub fn inode(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
}

/// An open descriptor held so its inode is not freed — nor its number given to another file —
/// while sopen still compares a name with it (04 §5.1).
#[derive(Clone)]
pub struct Pin(std::sync::Arc<std::fs::File>);

impl Pin {
    pub fn new(file: std::fs::File) -> Self {
        Self(std::sync::Arc::new(file))
    }

    /// The device and inode of what the descriptor holds (`fstat`): never another file's, as the
    /// number cannot be reused while it is open.
    pub fn inode(&self) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let m = self.0.metadata().ok()?;
        Some((m.dev(), m.ino()))
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin")
    }
}

/// A file copied under a temp name, not yet published.
#[derive(Debug, Clone)]
pub struct Staged {
    /// The temp's size when it was closed.
    pub size: u64,
    /// SHA-256 of the bytes written, when they passed through sopen; `None` when the host
    /// copied them itself (`copy-data`, `copy_file_range`), so sopen never saw them.
    pub sha256: Option<[u8; 32]>,
    /// The source as its open handle described it before the copy.
    pub source: FileMeta,
    /// What the copy could not keep (its mode, its times), for the report.
    pub notes: Vec<String>,
    /// The temp's device and inode, by the descriptor the copy was written through (this
    /// machine): the engine publishes the name only while it is still this file. `None` over
    /// SFTP, which carries no inode.
    pub id: Option<(u64, u64)>,
    /// That descriptor, held while the engine relies on `id` (publish, read-back, any cleanup):
    /// the inode cannot be freed and its number reused meanwhile.
    pub pin: Option<Pin>,
}

impl Staged {
    /// The inode of the copy now: what the held descriptor reports (it follows the file through
    /// a rename, also where the number itself changes with the name — an empty file on FAT or
    /// exFAT), else `id` as it was when the copy was closed.
    pub fn id_now(&self) -> Option<(u64, u64)> {
        self.pin.as_ref().and_then(Pin::inode).or(self.id)
    }
}

/// What a never-replacing rename did besides giving the new name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renamed {
    /// The old name is gone.
    Moved,
    /// The new name is in place, but the old name remains — its removal failed, or another
    /// entry has it now (left as it is): the reason, naming both paths.
    OldRemains(String),
    /// The new name is in place; whether the old name remains could not be confirmed (it could
    /// not be checked again, or its removal got no answer): the reason.
    Unconfirmed(String),
}

/// How finished copies get their final names in a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    /// A complete temp gets the name by a rename that never replaces (or a link).
    Atomic,
    /// The filesystem has neither an exclusive rename nor links (exFAT, FAT): the copy is
    /// written straight into its final name, created exclusively — still never replacing, but
    /// an interrupted copy leaves a partial file under that name.
    InPlace,
}

/// A copy into a temp (or, written in place, into its final name) that went no further.
#[derive(Debug)]
pub struct StageFailure {
    /// Whether the file had been created ([`Created::Yes`]: it may hold partial content).
    pub created: Created,
    /// The created file's device and inode, by the descriptor it was written through (this
    /// machine): it is removed only while its name is still that file. `None` over SFTP, which
    /// carries no inode, or when nothing was created.
    pub id: Option<(u64, u64)>,
    /// The descriptor it was written through, held until the cleanup compared `id`.
    pub pin: Option<Pin>,
    pub error: anyhow::Error,
}

impl StageFailure {
    /// Nothing was created.
    pub fn not_created(error: anyhow::Error) -> Self {
        Self {
            created: Created::No,
            id: None,
            pin: None,
            error,
        }
    }

    /// The inode of what was created, as [`Staged::id_now`].
    pub fn id_now(&self) -> Option<(u64, u64)> {
        self.pin.as_ref().and_then(Pin::inode).or(self.id)
    }
}

impl From<CreateError> for StageFailure {
    fn from(e: CreateError) -> Self {
        Self {
            created: e.created,
            id: None,
            pin: None,
            error: e.error,
        }
    }
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
    /// The identity of `path` on this machine — of the entry itself (`lstat`), or with `follow`
    /// of what a link there points at (`stat`); `None` over SFTP, which carries no inode.
    fn identity(&mut self, path: &str, follow: bool) -> Result<Option<Ident>>;

    /// A descriptor on the entry `path` itself (never what a link points at), held so that the
    /// inode of `id` — as [`FileOps::identity`] gave it just before — cannot be freed and its
    /// number given to another file while the caller compares the name with it. `None` over
    /// SFTP, where there is no inode; and locally where no descriptor can be had (too many open
    /// files, an entry this account cannot open) or the name is no longer `id`'s entry.
    fn pin(&mut self, _path: &str, _id: &Ident) -> Option<Pin> {
        None
    }

    /// `path`'s own permission bits, size and times (`lstat`).
    fn meta(&mut self, path: &str) -> Result<FileMeta>;

    /// Renames `from` to `to`, never replacing: [`Taken`] when `to` exists, [`NoRename`] when
    /// this host cannot rename it without replacing (another filesystem; a filesystem with
    /// neither an exclusive rename nor links; over SFTP, a folder). Over SFTP a file is linked
    /// to the new name with `hardlink@openssh.com` and the old name removed; a link is made again
    /// at the new name and the old one removed. [`Renamed::OldRemains`]: `to` is in place but
    /// `from` was not removed; [`Renamed::Unconfirmed`]: whether it was could not be confirmed.
    fn rename_new(&mut self, from: &str, to: &str) -> Result<Renamed>;

    /// How finished copies get their names in the folder `dir` (04 §5.6), probed once per folder
    /// by the engine; with what the report must hear about the probe (a name it could not clean
    /// up).
    fn publish_mode(&mut self, _dir: &str) -> Result<(Publish, Vec<String>)> {
        Ok((Publish::Atomic, Vec::new()))
    }

    /// Makes durable what an operation put in the folders `dirs` (deepest first): the entries
    /// each folder received, and the files' data on the volume (04 §5.1). Called before a moved
    /// item's source is removed — a failure keeps it — and at an operation's end.
    fn barrier(&mut self, _dirs: &[String]) -> Result<()> {
        Ok(())
    }

    /// The id of the filesystem holding `path` (`statvfs`'s `f_fsid`), where it costs one call:
    /// two connections that report the same id for nested paths are one filesystem.
    fn fs_id(&mut self, _path: &str) -> Option<u64> {
        None
    }

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
    /// [`COPY_STEP`] at a time where the host copies (`copy-data`, `copy_file_range`), with
    /// `from`'s permission bits and times set through the temp's handle, flushed to disk and
    /// closed. `cancel` is checked between steps
    /// ([`Cancelled`](super::Cancelled)); `progress` gets the bytes copied so far. A failure says
    /// whether `temp` was created; the caller removes it.
    fn copy_within(
        &mut self,
        from: &str,
        temp: &str,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> std::result::Result<Staged, StageFailure>;

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
    ) -> std::result::Result<Staged, StageFailure>;
}
