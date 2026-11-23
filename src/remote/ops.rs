//! The primitives the browser's rename, copy and move are built from (04 §5.6): renames that never
//! replace, `MKDIR`, `RMDIR`, links read and made, a same-host `copy-data` in chunks, staged
//! temps. They are a trait of their own, implemented for [`SftpRemote`](super::SftpRemote) in
//! `sftp/ops.rs` and for [`LocalFs`](super::LocalFs) in `local/ops.rs` — submodules, which see
//! their parent's private fields — so [`RemoteFs`] and its write algorithm stay as they are (02
//! §5). Nothing runs on the remote: no `cp`, `mv` or `rm` over ssh. S1-C writes them; S6-A adds
//! F7's folder, the local [`Trash`], and SFTP's plain rename (a remote folder's rename and move,
//! and a copy's publish where the server has no `hardlink@openssh.com`, after the server's rename
//! is probed: [`FileOps::publish_probe`], [`FileOps::publish_new`]).
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
/// a new file given a freed inode's number (ext4 reuses one at once) will as a rule not share.
/// Compared without a held descriptor ([`Pin`]) this is best effort: it is as fine as the
/// filesystem's timestamps and as true as its clock.
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

    /// The identity of what the descriptor holds, as it is now (`fstat`).
    pub fn ident(&self) -> Option<Ident> {
        use std::os::unix::fs::MetadataExt;
        let m = self.0.metadata().ok()?;
        Some(Ident {
            dev: m.dev(),
            ino: m.ino(),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
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

/// The rename cannot happen here (another filesystem; a remote folder where the connection has
/// no plain-rename channel, or its open failed) and nothing changed: a move goes on as a copy, a
/// check and a removal (04 §5.2).
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

/// Why a remote folder is not renamed where the session has no plain-rename channel to open (04
/// §5.3, §5.6): a session `SftpRemote::connect` did not make — a test's local server, a stream's
/// session given back. Every connection a file operation makes has one; where its open fails,
/// the reason is said instead (`sftp/rename.rs`).
pub const REMOTE_FOLDER_RENAME: &str = "renaming a folder on a remote needs SFTP's plain \
    rename, and this connection has no channel for it; nothing was changed";

/// The system Trash of this machine (04 §5.5): macOS's through `NSFileManager`, the freedesktop
/// Trash elsewhere (`remote/local/trash.rs`). A trait, so that tests reach a stand-in and never
/// the owner's Trash.
pub trait Trash: Sync {
    /// Whether an entry of the folder `dir` can go to the Trash; `Err` says why not (the delete
    /// dialog then says `permanent — no Trash here` before anything is removed).
    fn available(&self, dir: &str) -> std::result::Result<(), String>;

    /// Moves the entry `path` itself — a link as a link, a folder with everything inside it —
    /// into the Trash, never over anything there; where it is now. A failure removed nothing.
    fn put(&self, path: &str) -> Result<String>;
}

/// What a permanent delete of a folder ([`FileOps::remove_tree`]) tells its caller as it goes.
/// `rel` is an entry's path below the folder (`sub/name`).
pub trait TreeWatch {
    /// Esc. Asked after every look at an entry and right before every request that removes
    /// one (over SFTP, before every request): once it says so, no removal starts.
    fn stopped(&mut self) -> bool;
    /// The entry `rel` is next.
    fn at(&mut self, rel: &str);
    /// The entry `rel` was removed.
    fn removed(&mut self, rel: &str);
    /// The entry `rel` stays, and why — a folder that stays because something in it stays too.
    fn kept(&mut self, rel: &str, why: String);
}

/// How a folder's permanent delete ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEnd {
    /// It and everything in it are gone (or it was gone already).
    Removed,
    /// It stays: why. Each entry in it that stays was told to [`TreeWatch::kept`].
    Kept(String),
    /// Esc: what was removed stays removed, the rest stays.
    Stopped,
}

/// A name that is not UTF-8 is never acted on (01 §3a).
pub const NOT_UTF8: &str = "its name is not UTF-8, and sopen never acts on such a name";

#[cfg(test)]
thread_local! {
    /// Right after a permanent delete has looked at an entry (`lstat`) and before it acts on it:
    /// the entry's path.
    static TREE_LOOKED: std::cell::RefCell<Option<SeamHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam on this thread: `hook` runs with an entry's path each time a permanent delete has
/// looked at it and not yet acted on it — on this machine and over SFTP alike.
#[cfg(test)]
pub(crate) fn seam_tree_looked(hook: impl FnMut(&str) + 'static) -> SeamGuard {
    set_seam(&TREE_LOOKED, hook)
}

#[cfg(test)]
pub(crate) fn fire_tree_looked(path: &str) {
    fire_seam(&TREE_LOOKED, path);
}

#[cfg(test)]
thread_local! {
    /// Right after each request of a permanent delete by paths (SFTP's walk): `"<request>
    /// <path>"`, the request one of `realpath`, `lstat`, `list`, `remove` and `rmdir`.
    static TREE_ASKED: std::cell::RefCell<Option<SeamHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam on this thread: `hook` runs right after each request of a permanent delete by
/// paths, with `"<request> <path>"` — the walk's request log, and the place to press Esc in the
/// middle of a request.
#[cfg(test)]
pub(crate) fn seam_tree_asked(hook: impl FnMut(&str) + 'static) -> SeamGuard {
    set_seam(&TREE_ASKED, hook)
}

/// A request of the walk by paths has been answered.
fn asked(_request: &str, _path: &str) {
    #[cfg(test)]
    fire_seam(&TREE_ASKED, &format!("{_request} {_path}"));
}

/// Does `path` still resolve to `canon` (`realpath`)? Why not, if not.
fn resolves<F: FileOps + ?Sized>(
    fs: &mut F,
    path: &str,
    canon: &str,
) -> std::result::Result<(), String> {
    let now = fs.realpath(path);
    asked("realpath", path);
    match now {
        Ok(now) if now == canon => Ok(()),
        Ok(now) => Err(format!(
            "it is not where it was listed any more (it now resolves to {now}), so nothing is \
             removed through it"
        )),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// [`FileOps::remove_tree`] by paths, as SFTP must (it has no request relative to a folder):
/// bottom-up, each entry looked at (`lstat`) right before it is acted on — a link removed as a
/// link, never entered — and each request that goes through a folder (its listing, a `REMOVE`
/// in it, its `RMDIR`) made only while that folder still resolves (`realpath`) where it was
/// listed, the selected folder's own first listing too: a folder swapped for a link meanwhile
/// is neither listed, nor entered, nor removed through. Esc is looked at before every request,
/// whatever it is: once it is set, no request starts. The window left is one request wide:
/// between the last check and the request, the server resolves the path again.
fn remove_tree_by_path<F: FileOps + ?Sized>(
    fs: &mut F,
    dir: &str,
    canon: &str,
    rel: &str,
    watch: &mut dyn TreeWatch,
) -> TreeEnd {
    if watch.stopped() {
        return TreeEnd::Stopped;
    }
    if let Err(why) = resolves(fs, dir, canon) {
        return TreeEnd::Kept(why);
    }
    if watch.stopped() {
        return TreeEnd::Stopped;
    }
    let entries = fs.list_entries(dir);
    asked("list", dir);
    let entries = match entries {
        Ok(entries) => entries,
        Err(e) => return TreeEnd::Kept(format!("{e:#}")),
    };
    let mut stays = 0usize;
    for entry in entries {
        let child_rel = match rel {
            "" => entry.name.clone(),
            _ => format!("{rel}/{}", entry.name),
        };
        watch.at(&child_rel);
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        if entry.lossy {
            watch.kept(&child_rel, NOT_UTF8.to_string());
            stays += 1;
            continue;
        }
        let child = super::join(dir, &entry.name);
        let own = fs.lstat_opt(&child);
        asked("lstat", &child);
        #[cfg(test)]
        fire_tree_looked(&child);
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        let own = match own {
            Ok(Some(own)) => own,
            // Gone meanwhile: nothing to remove.
            Ok(None) => continue,
            Err(e) => {
                watch.kept(&child_rel, format!("{e:#}"));
                stays += 1;
                continue;
            }
        };
        if own.is_dir && !own.is_symlink {
            // Entered (listed) only while it still resolves where it was listed.
            let child_canon = super::join(canon, &entry.name);
            match remove_tree_by_path(fs, &child, &child_canon, &child_rel, watch) {
                TreeEnd::Removed => watch.removed(&child_rel),
                TreeEnd::Stopped => return TreeEnd::Stopped,
                TreeEnd::Kept(why) => {
                    watch.kept(&child_rel, why);
                    stays += 1;
                }
            }
            continue;
        }
        // A file, a link or a special file: its name goes, through its folder only while that
        // folder still resolves where it was listed.
        if let Err(why) = resolves(fs, dir, canon) {
            watch.kept(&child_rel, why);
            stays += 1;
            continue;
        }
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        let removed = fs.remove(&child);
        asked("remove", &child);
        match removed {
            Ok(()) => watch.removed(&child_rel),
            Err(e) => {
                watch.kept(&child_rel, format!("{e:#}"));
                stays += 1;
            }
        }
    }
    if stays > 0 {
        return TreeEnd::Kept(format!("{stays} of the entries in it stay"));
    }
    if watch.stopped() {
        return TreeEnd::Stopped;
    }
    if let Err(why) = resolves(fs, dir, canon) {
        return TreeEnd::Kept(why);
    }
    if watch.stopped() {
        return TreeEnd::Stopped;
    }
    let removed = fs.remove_dir(dir);
    asked("rmdir", dir);
    match removed {
        Ok(()) => TreeEnd::Removed,
        Err(e) => TreeEnd::Kept(format!("{e:#}")),
    }
}

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

    /// Whether a staged copy can get its name here without the risk of replacing anything:
    /// by a link ([`RemoteFs::can_link`]), or over SFTP without `hardlink@openssh.com` by the
    /// plain rename once [`FileOps::publish_probe`] has found that it refuses (04 §5.6).
    fn can_publish(&self) -> bool {
        self.can_link()
    }

    /// Before a copy into the folder `dir` is staged (04 §5.6, "copy within one host"): where
    /// the publish needs SFTP's plain rename, the server's rename is probed once per session —
    /// `Err` (the reason) when it replaces, which is kept for the session, or when the probe
    /// could not run, which is not. `Ok` with what the report must hear (a probe entry left).
    /// Nothing to do where the publish is a link.
    fn publish_probe(&mut self, _dir: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Gives a staged copy `temp` its final name `target`, never replacing: [`Taken`] when the
    /// name exists. [`FileOps::rename_new`] (a link, or the exclusive rename) except over SFTP
    /// without `hardlink@openssh.com`, where it is the plain rename that
    /// [`FileOps::publish_probe`] proved refuses an existing name.
    fn publish_new(&mut self, temp: &str, target: &str) -> Result<Renamed> {
        self.rename_new(temp, target)
    }

    /// Makes the folder `path` (mode 0700 until its contents are in, 04 §5.1): [`Taken`] when
    /// the name exists.
    fn make_dir(&mut self, path: &str) -> Result<()>;

    /// F7 (04 §5.4): makes the folder `path` as `mkdir` does — the default mode, 0777 less the
    /// umask (over SFTP the server's) — never over an existing name: [`Taken`] when it exists.
    fn make_folder(&mut self, path: &str) -> Result<()>;

    /// The Trash an entry here can go to (F8, 04 §5.5): this machine's; `None` on a remote
    /// (SFTP has none), where a delete is permanent.
    fn trash(&self) -> Option<&dyn Trash> {
        None
    }

    /// Removes the empty folder `path` (`RMDIR`, `rmdir`).
    fn remove_dir(&mut self, path: &str) -> Result<()>;

    /// Shift+F8, and F8 on a remote (04 §5.5): the folder `path` and everything in it removed
    /// permanently, bottom-up — files, links (as links, never their targets) and special files
    /// by name, each folder once empty. Nothing is removed through a folder swapped for a link
    /// meanwhile: it is left, and so is what holds it. A folder swapped for another real folder
    /// under the same name is told apart on this machine (its device and inode) and left too;
    /// over SFTP it resolves the same and is not told apart — its entries, at the selected path,
    /// are removed. What stays is told to `watch`, entry by entry; once Esc (`watch.stopped`) is
    /// set, no removal starts (and over SFTP no request at all).
    ///
    /// By paths here, as SFTP must: the selected folder's canonical path (`realpath`) is taken
    /// before it is looked at (`lstat`: a folder, not a link); then each listing, `REMOVE` and
    /// `RMDIR` is made only while the folder it goes through still resolves there (or below it,
    /// by the names listed), the entry looked at right before. Left: one round trip, between a
    /// check's reply and the request, where the server resolves the path again; and a server
    /// whose `realpath` does not resolve links has no check at all (measured on OpenSSH's only).
    /// This machine walks open folder descriptors instead (`local/ops.rs`), so nothing is
    /// resolved again from the top.
    fn remove_tree(&mut self, path: &str, watch: &mut dyn TreeWatch) -> TreeEnd {
        // Where the folder resolves is taken first, and only then is it looked at: a folder
        // swapped for a link before the look is refused by the look; one swapped after it no
        // longer resolves to `canon`, and nothing is listed or removed through it.
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        let canon = self.realpath(path);
        asked("realpath", path);
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        let own = self.lstat_opt(path);
        asked("lstat", path);
        #[cfg(test)]
        fire_tree_looked(path);
        if watch.stopped() {
            return TreeEnd::Stopped;
        }
        match own {
            Ok(Some(own)) if own.is_dir && !own.is_symlink => {}
            Ok(Some(_)) => {
                return TreeEnd::Kept(
                    "it is no longer a folder; nothing of it was removed".to_string(),
                )
            }
            Ok(None) => return TreeEnd::Removed,
            Err(e) => return TreeEnd::Kept(format!("{e:#}")),
        }
        let canon = match canon {
            Ok(canon) => canon,
            Err(e) => return TreeEnd::Kept(format!("{e:#}")),
        };
        remove_tree_by_path(self, path, &canon, "", watch)
    }

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
