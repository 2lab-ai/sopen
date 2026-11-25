//! What the views of one remote file share (`.prd/08-viewer.md` §5–§6, D13, D14): the stream
//! the opening read it through, kept beside the views so that a view made later reads nothing
//! again, and the viewer's one save session, which a hidden view gives back.
//!
//! Every remote `Origin` of a viewer's views connects through the viewer's [`SaveSlot`] and
//! reaches its session through a handle ([`SaveSlot::handle`]), which forwards every method of
//! the file layer — the provided ones too, so none answers by its default beside the session.
//! The rules:
//! - **One connect path, one at a time.** A use that finds the slot empty — the first use (a
//!   Save, a Save copy, a Save As, an Export, the recovery of an earlier session's edits), or the
//!   first after a give-back — connects under the slot's lock: a second use at the same moment
//!   waits for that session and uses it, never a second one. `Origin::fs`'s reconnect replaces a
//!   session only when it failed a request, so two of them replace it once.
//! - **A give-back waits for the operation** ([`SaveSlot::give_back`]): never while a request
//!   runs, a file handle is open (a hex Save's by handle, 07 §4) or its last request is less than
//!   [`LEASE_QUIET`] old (its publish and cleanup may follow: an operation's end is known only
//!   once its requests stop); never once another view used the session after it was asked for.
//!   Refused for now, it is asked again on a later pass. The session goes out of the slot under
//!   its lock and is closed beside the caller — an SFTP session's close waits for its reply — so
//!   the window's pass never waits for it.
//! - **A file handle reaches only the session it was opened on**: once that one went back it is
//!   refused, never sent on the session made since.
//! - **What the server can do** (`can_replace`, `can_link`, `can_copy`, `can_fsync`, the hash
//!   limit) is kept beside the lock, as the session last made answered (before the first, as
//!   given): the window's pass asks it while an upload holds the session.
//!
//! So a viewer holds its stream and at most one save session — 06 §6's "1; 2 from its first
//! Save" — however many of its views saved.

use super::super::Connect;
use crate::gui::{Placeholder, Reconnect};
use crate::remote::patch::{CopyCtl, Each, Handle, OpenFor, PatchFs};
use crate::remote::{
    CreateError, Created, Entry, FileId, Fingerprint, Guard, Like, Ranged, Refused, RemoteFs,
    Replaced, Stat, Step, Uploaded, WriteOutcome,
};
use crate::stream::Source;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

/// How long an operation keeps the session after its last request (a give-back waits so long).
pub const LEASE_QUIET: Duration = Duration::from_secs(1);

/// The stream of a remote file, kept beside its views (D13): a view made later reads through
/// it, so nothing it shows is read again.
#[derive(Clone)]
pub struct Stream {
    pub source: Source,
    /// The file it reads: the opening's path, resolved.
    pub abs: String,
    /// The handle's stat when the stream was opened.
    pub at: Stat,
}

impl Stream {
    /// What a view made now saves against: the handle's stat at open, with the size and mtime
    /// of the version the stream reads now (after a reload, the one reloaded).
    pub fn at_now(&self) -> Stat {
        let now = self.source.opened();
        Stat {
            size: now.size,
            mtime: now.mtime,
            sha256: None,
            ..self.at
        }
    }
}

/// What the views of one file share (08 §6): its stream and its save session. A local file's
/// views share nothing — each reads the file in place, and writes on this machine.
#[derive(Clone, Default)]
pub struct Shared {
    pub stream: Option<Stream>,
    pub save: Option<SaveSlot>,
}

/// The save session of a remote viewer's views (D14), shared by their `Origin`s: empty until
/// one of them is first used, given back when the view that used it is hidden or closed.
#[derive(Clone)]
pub struct SaveSlot(Arc<Inner>);

struct Inner {
    slot: Mutex<Slot>,
    answers: Answers,
    /// Requests sent so far: a give-back asked for at `n` gives nothing once one came since.
    requests: AtomicU64,
    /// File handles open on the session: each holds it.
    handles: Arc<AtomicUsize>,
}

/// What the server can do, as the session last made answered (as given before the first).
struct Answers {
    can_replace: AtomicBool,
    can_link: AtomicBool,
    can_copy: AtomicBool,
    can_fsync: AtomicBool,
    hash_limit: AtomicU64,
}

struct Slot {
    conn: Option<Box<dyn RemoteFs + Send>>,
    /// How a session is opened (the `Origin`s' reconnect's).
    connect: Option<Connect>,
    /// Which session the slot holds or last held, counted from 1: a file handle is stamped with
    /// the one it was opened on ([`Stamped`]).
    session: u64,
    /// The session held failed a request: `Origin::fs`'s reconnect replaces it.
    failed: bool,
    /// The connect of this use just failed: `Origin::fs`'s reconnect, right after, says so
    /// rather than ask the server a second time.
    refused: Option<anyhow::Error>,
    /// When the last request ended.
    last: Option<Instant>,
    /// [`LEASE_QUIET`]; tests shorten it.
    quiet: Duration,
}

/// What [`SaveSlot::give_back`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveBack {
    /// The slot is empty now (its session, if it had one, closes beside).
    Done,
    /// An operation holds the session: ask again later.
    NotNow,
    /// Another view used the session since it was asked for: it is that one's now.
    InUse,
}

/// Closes `conn` on a thread of its own: an SFTP session's close waits for its reply.
fn close_beside(conn: Box<dyn RemoteFs + Send>) {
    let mut held = Some(conn);
    let spawned = std::thread::Builder::new()
        .name("sopen-session-close".into())
        .spawn({
            let conn = held.take();
            move || drop(conn)
        });
    if let Err(e) = spawned {
        crate::debug!("closing a save session here: {e}");
    }
}

impl Slot {
    /// The session, connected first when there is none — under the slot's lock, so one connect
    /// at a time and a second use waits for the first's session. `answers` learn the new one's.
    fn conn(&mut self, answers: &Answers) -> Result<&mut Box<dyn RemoteFs + Send>> {
        if self.conn.is_none() {
            let connect = self.connect.clone().ok_or_else(not_connected)?;
            match connect() {
                Ok(conn) => self.put(conn, answers),
                Err(e) => {
                    let said = anyhow!("{e:#}");
                    self.refused = Some(e);
                    return Err(said);
                }
            }
        }
        Ok(self.conn.as_mut().expect("connected"))
    }

    fn put(&mut self, mut conn: Box<dyn RemoteFs + Send>, answers: &Answers) {
        let store = |flag: &AtomicBool, v: bool| flag.store(v, Ordering::SeqCst);
        store(&answers.can_replace, conn.can_replace());
        store(&answers.can_link, conn.can_link());
        let hash = conn.hash_check_limit();
        answers.hash_limit.store(hash, Ordering::SeqCst);
        let (copy, fsync) = conn
            .patcher()
            .map_or((false, true), |p| (p.can_copy(), p.can_fsync()));
        store(&answers.can_copy, copy);
        store(&answers.can_fsync, fsync);
        self.conn = Some(conn);
        self.session += 1;
        self.failed = false;
        self.refused = None;
    }
}

impl SaveSlot {
    /// An empty slot whose handles answer `can` until a session does.
    pub fn new(can: Placeholder) -> Self {
        Self(Arc::new(Inner {
            slot: Mutex::new(Slot {
                conn: None,
                connect: None,
                session: 0,
                failed: false,
                refused: None,
                last: None,
                quiet: LEASE_QUIET,
            }),
            answers: Answers {
                can_replace: AtomicBool::new(can.can_replace),
                can_link: AtomicBool::new(can.can_link),
                can_copy: AtomicBool::new(false),
                can_fsync: AtomicBool::new(true),
                hash_limit: AtomicU64::new(crate::remote::HASH_CHECK_MAX),
            },
            requests: AtomicU64::new(0),
            handles: Arc::default(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.0.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An `Origin`'s connection: the slot's session, connected at its first use.
    pub fn handle(&self) -> Box<dyn RemoteFs + Send> {
        Box::new(SlotFs(self.clone()))
    }

    /// An `Origin`'s reconnect, for when its probe failed: the slot's session replaced when it
    /// failed a request (closed beside), connected when there is none — the one connect path,
    /// so another `Origin`'s reconnect at the same moment finds the new one and connects no
    /// more — and a handle onto it.
    pub fn reconnect(&self, connect: Connect) -> Reconnect {
        let slot = self.clone();
        slot.lock().connect.get_or_insert(connect);
        Box::new(move || {
            let mut held = slot.lock();
            if let Some(e) = held.refused.take() {
                return Err(e);
            }
            if held.failed {
                if let Some(old) = held.conn.take() {
                    close_beside(old);
                }
            }
            held.conn(&slot.0.answers)?;
            drop(held);
            Ok(slot.handle())
        })
    }

    /// Where the slot's requests stand: a give-back asked for now gives nothing back once
    /// another request comes ([`GiveBack::InUse`]).
    pub fn mark(&self) -> u64 {
        self.0.requests.load(Ordering::SeqCst)
    }

    /// D14: the session goes back — out of the slot under its lock, closed beside — so the slot
    /// is empty until the next use connects again. Not while an operation holds it
    /// ([`GiveBack::NotNow`]: ask again), and not once a request came since `since`
    /// ([`GiveBack::InUse`]). Never waits: for the window's pass.
    pub fn give_back(&self, since: u64) -> GiveBack {
        if self.mark() > since {
            return GiveBack::InUse;
        }
        let mut slot = match self.0.slot.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return GiveBack::NotNow,
        };
        if self.mark() > since {
            return GiveBack::InUse;
        }
        let recent = slot.last.is_some_and(|at| at.elapsed() < slot.quiet);
        if self.0.handles.load(Ordering::SeqCst) > 0 || recent {
            return GiveBack::NotNow;
        }
        let conn = slot.conn.take();
        drop(slot);
        if let Some(conn) = conn {
            close_beside(conn);
        }
        GiveBack::Done
    }

    /// [`SaveSlot::give_back`] asked now; `true`: done.
    #[cfg_attr(not(test), allow(dead_code))] // The tests' give-back.
    pub fn release(&self) -> bool {
        self.give_back(self.mark()) == GiveBack::Done
    }

    /// Tests: how long an operation keeps the session after its last request.
    #[cfg(test)]
    pub fn set_quiet(&self, quiet: Duration) {
        self.lock().quiet = quiet;
    }

    /// The slot holds a session now.
    #[cfg_attr(not(test), allow(dead_code))] // The tests' count of sessions.
    pub fn connected(&self) -> bool {
        self.lock().conn.is_some()
    }
}

fn not_connected() -> anyhow::Error {
    anyhow!("not connected yet")
}

fn no_patcher() -> anyhow::Error {
    anyhow!("this connection cannot write part of a file")
}

/// A file handle as a [`SlotFs`] gives it out: the session's own, which session that was, and
/// its hold on the session while it is open.
struct Stamped {
    session: u64,
    handle: Handle,
    _held: Held,
}

/// An open file's hold on the session (a give-back waits for it).
struct Held(Arc<AtomicUsize>);

impl Held {
    fn new(handles: &Arc<AtomicUsize>) -> Held {
        handles.fetch_add(1, Ordering::SeqCst);
        Held(handles.clone())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A handle onto a [`SaveSlot`]'s session: every call goes to the session the slot holds at
/// that moment, connected first when there is none.
struct SlotFs(SaveSlot);

impl SlotFs {
    fn inner(&self) -> &Inner {
        &self.0 .0
    }

    /// One request on the session (or a composed one: the slot's lock is held throughout, so
    /// no give-back lands inside it). A failure marks the session for `Origin::fs`'s reconnect.
    fn request<T, E>(
        &self,
        refused: impl FnOnce(anyhow::Error) -> E,
        f: impl FnOnce(&mut (dyn RemoteFs + Send)) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let inner = self.inner();
        let mut slot = self.0.lock();
        inner.requests.fetch_add(1, Ordering::SeqCst);
        let result = match slot.conn(&inner.answers) {
            Ok(conn) => f(conn.as_mut()),
            Err(e) => Err(refused(e)),
        };
        slot.failed |= result.is_err() && slot.conn.is_some();
        slot.last = Some(Instant::now());
        result
    }

    fn with<T>(&self, f: impl FnOnce(&mut (dyn RemoteFs + Send)) -> Result<T>) -> Result<T> {
        self.request(|e| e, f)
    }

    fn create(
        &self,
        f: impl FnOnce(&mut (dyn RemoteFs + Send)) -> std::result::Result<Uploaded, CreateError>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.request(|e| CreateError::new(Created::No, e), f)
    }

    /// A file opened (or created) on the session the slot holds, stamped with that session and
    /// holding it while it is open.
    fn opening<E>(
        &self,
        path: &str,
        refused: impl Fn(anyhow::Error) -> E,
        f: impl FnOnce(&mut dyn PatchFs) -> std::result::Result<Handle, E>,
    ) -> std::result::Result<Handle, E> {
        let inner = self.inner();
        let mut session = 0;
        let handle = self.request(&refused, |conn| match conn.patcher() {
            Some(patcher) => f(patcher),
            None => Err(refused(no_patcher())),
        });
        if handle.is_ok() {
            session = self.0.lock().session;
        }
        let handle = handle?;
        let _held = Held::new(&inner.handles);
        Ok(Handle::new(
            path,
            Stamped {
                session,
                handle,
                _held,
            },
        ))
    }

    /// A request by `file`'s handle (and `other`'s), on the session it was opened on — never
    /// connected again for it, and never another session's: a handle of a session given back
    /// is refused.
    fn by<T>(
        &self,
        file: &mut Handle,
        other: Option<&mut Handle>,
        f: impl FnOnce(&mut dyn PatchFs, &mut Handle, Option<&mut Handle>) -> Result<T>,
    ) -> Result<T> {
        let inner = self.inner();
        let mut slot = self.0.lock();
        inner.requests.fetch_add(1, Ordering::SeqCst);
        let session = slot.session;
        let gone = |h: &Handle| {
            anyhow!(
                "{} was open on another session (given back or lost)",
                h.path()
            )
        };
        let stale = |h: &mut Handle| h.get::<Stamped>().map(|s| s.session != session);
        if stale(file)? || slot.conn.is_none() {
            return Err(gone(file));
        }
        let other = match other {
            Some(o) => {
                if stale(o)? {
                    return Err(gone(o));
                }
                Some(&mut o.get::<Stamped>()?.handle)
            }
            None => None,
        };
        let file = &mut file.get::<Stamped>()?.handle;
        let result = match slot.conn.as_mut().and_then(|c| c.patcher()) {
            Some(patcher) => f(patcher, file, other),
            None => Err(no_patcher()),
        };
        slot.failed |= result.is_err();
        slot.last = Some(Instant::now());
        result
    }
}

impl RemoteFs for SlotFs {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.with(|c| c.stat_opt(path))
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.with(|c| c.lstat_opt(path))
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.with(|c| c.realpath(path))
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.with(|c| c.read_head(path, max))
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.with(|c| c.list_dir(path))
    }
    fn list_entries(&mut self, path: &str) -> Result<Vec<Entry>> {
        self.with(|c| c.list_entries(path))
    }
    fn free_space(&mut self, path: &str) -> Option<u64> {
        self.with(|c| Ok(c.free_space(path))).ok().flatten()
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.with(|c| c.read_to(path, out, progress))
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.create(|c| c.create_from(local, path, like))
    }
    fn create_from_stoppable(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
        stop: &dyn Fn() -> bool,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.create(|c| c.create_from_stoppable(local, path, like, stop))
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.with(|c| c.set_mode(path, mode))
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.with(|c| c.rename_over(from, to))
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.with(|c| c.link_new(from, to))
    }
    fn can_link(&self) -> bool {
        self.inner().answers.can_link.load(Ordering::SeqCst)
    }
    fn can_replace(&self) -> bool {
        self.inner().answers.can_replace.load(Ordering::SeqCst)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.with(|c| c.remove(path))
    }
    fn hash_check_limit(&self) -> u64 {
        self.inner().answers.hash_limit.load(Ordering::SeqCst)
    }
    fn exchange(&mut self, a: &str, b: &str) -> Result<()> {
        self.with(|c| c.exchange(a, b))
    }
    fn file_id(&mut self, path: &str) -> Result<Option<(FileId, u64)>> {
        self.with(|c| c.file_id(path))
    }
    fn sync_dir(&mut self, dir: &str) -> Result<()> {
        self.with(|c| c.sync_dir(dir))
    }
    fn move_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.with(|c| c.move_new(from, to))
    }
    /// The session becomes the stream's (06 §2): the slot holds none from then on, and the next
    /// use connects again. Refused, the session comes back only to a slot still without one
    /// and at the same session — one made meanwhile stays, and the refused one closes beside.
    fn into_ranged(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        let taken = {
            let inner = self.inner();
            let mut slot = self.0.lock();
            inner.requests.fetch_add(1, Ordering::SeqCst);
            match slot.conn(&inner.answers) {
                Ok(_) => Ok((slot.conn.take().expect("connected"), slot.session)),
                Err(error) => Err(error),
            }
        };
        let (conn, session) = match taken {
            Ok(taken) => taken,
            Err(error) => return Err(Refused { conn: self, error }),
        };
        match conn.into_ranged(path) {
            Ok(ranged) => Ok(ranged),
            Err(refused) => {
                let back = {
                    let mut slot = self.0.lock();
                    let empty = slot.conn.is_none() && slot.session == session;
                    if empty {
                        slot.conn = Some(refused.conn);
                        None
                    } else {
                        Some(refused.conn)
                    }
                };
                if let Some(conn) = back {
                    close_beside(conn);
                }
                Err(Refused {
                    conn: self,
                    error: refused.error,
                })
            }
        }
    }
    /// A hex Save runs on the slot's session by handle (07 §4): through this handle too.
    fn patcher(&mut self) -> Option<&mut dyn PatchFs> {
        let patches = self.with(|c| Ok(c.patcher().is_some())).unwrap_or(false);
        if patches {
            Some(self)
        } else {
            None
        }
    }
    fn stat(&mut self, path: &str) -> Result<Stat> {
        self.with(|c| c.stat(path))
    }
    fn fetch(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Stat> {
        self.with(|c| c.fetch(remote, local, progress))
    }
    fn fetch_cancellable(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<Stat> {
        self.with(|c| c.fetch_cancellable(remote, local, progress, cancel))
    }
    fn write_atomic(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
    ) -> Result<WriteOutcome> {
        self.with(|c| c.write_atomic(local, target, expected))
    }
    fn write_atomic_with(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
        steps: &mut dyn FnMut(Step<'_>) -> Result<()>,
    ) -> Result<WriteOutcome> {
        self.with(|c| c.write_atomic_with(local, target, expected, steps))
    }
    fn replace_guarded(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
        id: Option<FileId>,
        steps: &mut dyn FnMut(Guard<'_>) -> Result<()>,
    ) -> Result<Replaced> {
        self.with(|c| c.replace_guarded(local, target, expected, id, steps))
    }
    fn write_new_sibling(&mut self, local: &Path, dir: &str, wanted: &str) -> Result<String> {
        self.with(|c| c.write_new_sibling(local, dir, wanted))
    }
}

impl PatchFs for SlotFs {
    fn can_copy(&self) -> bool {
        self.inner().answers.can_copy.load(Ordering::SeqCst)
    }
    fn open(&mut self, path: &str, how: OpenFor) -> Result<Handle> {
        self.opening(path, |e| e, |p| p.open(path, how))
    }
    fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError> {
        let refused = |e| CreateError::new(Created::No, e);
        self.opening(path, refused, |p| p.create_temp(path))
    }
    fn fstat(&mut self, file: &mut Handle) -> Result<Stat> {
        self.by(file, None, |p, h, _| p.fstat(h))
    }
    fn read_at(&mut self, file: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.by(file, None, |p, h, _| p.read_at(h, offset, len))
    }
    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64> {
        self.by(src, dst, |p, s, d| p.read_through(s, size, d, each))
    }
    fn write_at(&mut self, file: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()> {
        self.by(file, None, |p, h, _| p.write_at(h, offset, bytes))
    }
    fn set_owner(&mut self, file: &mut Handle, uid: u32, gid: u32) -> Result<()> {
        self.by(file, None, |p, h, _| p.set_owner(h, uid, gid))
    }
    fn set_handle_mode(&mut self, file: &mut Handle, mode: u32) -> Result<()> {
        self.by(file, None, |p, h, _| p.set_handle_mode(h, mode))
    }
    fn can_fsync(&self) -> bool {
        self.inner().answers.can_fsync.load(Ordering::SeqCst)
    }
    fn fsync(&mut self, file: &mut Handle) -> Result<()> {
        self.by(file, None, |p, h, _| p.fsync(h))
    }
    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()> {
        self.by(src, Some(dst), |p, s, d| {
            p.copy(s, d.expect("given"), size, ctl)
        })
    }
    /// A handle of a session given back is gone with it: nothing to close on another one. The
    /// handle's hold on the session ends either way.
    fn close(&mut self, file: Handle) {
        let Some(stamped) = file.into_inner::<Stamped>() else {
            return;
        };
        let Stamped {
            session,
            handle,
            _held,
        } = stamped;
        let inner = self.inner();
        let mut slot = self.0.lock();
        inner.requests.fetch_add(1, Ordering::SeqCst);
        if slot.session == session {
            if let Some(patcher) = slot.conn.as_mut().and_then(|c| c.patcher()) {
                patcher.close(handle);
            }
        }
        slot.last = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    //! The slot over this machine's disk posing as a remote: a session made on first use, the
    //! same one for every handle, given back, and made again by the next use.

    use super::*;
    use crate::gui::Origin;
    use crate::remote::LocalFs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn counted(n: &Arc<AtomicUsize>) -> Connect {
        let n = n.clone();
        Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
        })
    }

    fn origin(slot: &SaveSlot, connect: Connect, abs: &str) -> Origin {
        let at = LocalFs::new().stat(abs).unwrap();
        Origin::new(
            slot.handle(),
            Some(slot.reconnect(connect)),
            Some("box".to_string()),
            abs.to_string(),
            at,
        )
    }

    /// Two `Origin`s on one slot: nothing is asked of the server before the first use, which
    /// connects once, and the other `Origin` uses that session; given back, the slot holds none
    /// until the next use connects again — one session.
    #[test]
    fn origins_share_the_slots_one_session_and_connect_again_after_it_is_given_back() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("f.bin");
        std::fs::write(&abs, b"bytes").unwrap();
        let abs = abs.to_str().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let slot = SaveSlot::new(Placeholder::YES);
        slot.set_quiet(Duration::ZERO);
        let (mut a, mut b) = (
            origin(&slot, counted(&n), abs),
            origin(&slot, counted(&n), abs),
        );
        assert!(!slot.connected() && a.can_replace(), "placeholder answers");
        assert_eq!(n.load(Ordering::SeqCst), 0, "nothing asked yet");
        assert_eq!(a.fs().unwrap().stat(abs).unwrap().size, 5);
        assert_eq!(n.load(Ordering::SeqCst), 1, "the first use connects");
        assert!(b.fs().unwrap().patcher().is_some(), "a hex Save runs on it");
        assert_eq!(n.load(Ordering::SeqCst), 1, "the other uses that session");
        assert!(slot.release(), "given back");
        assert!(!slot.connected());
        assert_eq!(b.fs().unwrap().stat(abs).unwrap().size, 5);
        assert_eq!(n.load(Ordering::SeqCst), 2, "the next use connects again");
        assert!(slot.connected());
    }

    /// A file handle reaches only the session it was opened on: once that session is replaced
    /// (it failed a request, and `Origin::fs`'s reconnect made another), a request by it is
    /// refused — never sent on the session made since, where the same handle may name another
    /// file — and closing it sends nothing.
    #[test]
    fn a_file_handle_of_a_session_replaced_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("f.bin");
        std::fs::write(&abs, b"bytes").unwrap();
        let abs = abs.to_str().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let slot = SaveSlot::new(Placeholder::YES);
        let mut origin = origin(&slot, counted(&n), abs);
        let patcher = origin.fs().unwrap().patcher().unwrap();
        let mut file = patcher.open(abs, OpenFor::Read).unwrap();
        assert_eq!(
            patcher.fstat(&mut file).unwrap().size,
            5,
            "on its own session"
        );
        assert!(!slot.release(), "an open file holds the session");
        let missing = format!("{abs}.missing");
        assert!(patcher.read_head(&missing, 1).is_err(), "a failed request");
        let mut reconnect = slot.reconnect(counted(&n));
        drop(reconnect().unwrap());
        assert_eq!(n.load(Ordering::SeqCst), 2, "the failed session replaced");
        let patcher = origin.fs().unwrap().patcher().unwrap();
        let refused = patcher.read_at(&mut file, 0, 5).unwrap_err();
        assert!(
            format!("{refused:#}").contains("another session"),
            "{refused:#}"
        );
        patcher.close(file);
        let mut again = patcher.open(abs, OpenFor::Read).unwrap();
        assert_eq!(patcher.read_at(&mut again, 0, 5).unwrap(), b"bytes");
        patcher.close(again);
    }

    /// A handle taken before the session was given back — an Export takes it before its render
    /// and publishes on it after — connects again at its next call, and the slot holds that one
    /// session: the work goes on.
    #[test]
    fn a_handle_taken_before_the_session_went_back_connects_again() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("f.bin");
        std::fs::write(&abs, b"bytes").unwrap();
        let abs = abs.to_str().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let slot = SaveSlot::new(Placeholder::YES);
        slot.set_quiet(Duration::ZERO);
        let mut origin = origin(&slot, counted(&n), abs);
        let fs = origin.fs().unwrap();
        assert!(slot.release(), "given back between two requests");
        let sibling = format!("{abs}.sibling");
        fs.create_from(Path::new(abs), &sibling, None).unwrap();
        assert_eq!(std::fs::read(&sibling).unwrap(), b"bytes", "published");
        assert_eq!(n.load(Ordering::SeqCst), 2, "connected again, once");
        assert!(slot.connected());
    }

    /// A connection for the slot's races: its number (what `realpath` answers), how long its
    /// drop takes (an SFTP session's close waits for its reply), a `read_to` held at a gate, and
    /// what its `into_ranged` does before it refuses. Alive ones are counted.
    struct Probe {
        id: usize,
        drop_takes: Duration,
        gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        before_refusing: Option<Box<dyn FnMut() + Send>>,
        alive: Arc<AtomicUsize>,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            std::thread::sleep(self.drop_takes);
            self.alive.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl RemoteFs for Probe {
        fn stat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
            Ok(Some(a_stat()))
        }
        fn lstat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
            Ok(Some(a_stat()))
        }
        fn realpath(&mut self, _: &str) -> Result<String> {
            Ok(self.id.to_string())
        }
        fn read_head(&mut self, _: &str, _: usize) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn list_entries(&mut self, _: &str) -> Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        fn read_to(
            &mut self,
            _: &str,
            _: &mut dyn std::io::Write,
            _: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            if let Some(gate) = &self.gate {
                let (open, turned) = &**gate;
                let mut open = open.lock().unwrap();
                *open = true;
                turned.notify_all();
                while *open {
                    open = turned.wait(open).unwrap();
                }
            }
            Ok(0)
        }
        fn create_from(
            &mut self,
            _: &Path,
            _: &str,
            _: Option<Like>,
        ) -> std::result::Result<Uploaded, CreateError> {
            Ok(uploaded())
        }
        fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
            Ok(())
        }
        fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn into_ranged(
            mut self: Box<Self>,
            _: &str,
        ) -> std::result::Result<Box<dyn Ranged>, Refused> {
            if let Some(mut before) = self.before_refusing.take() {
                before();
            }
            Err(Refused {
                conn: self,
                error: anyhow!("no stream here"),
            })
        }
    }

    /// Connects [`Probe`]s numbered from 1, each `taking` to connect and `drop_takes` to close.
    fn probes(
        taking: Duration,
        drop_takes: Duration,
    ) -> (Connect, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let (made, alive) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (m, a) = (made.clone(), alive.clone());
        let connect: Connect = Arc::new(move || {
            std::thread::sleep(taking);
            let id = m.fetch_add(1, Ordering::SeqCst) + 1;
            a.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Probe {
                id,
                drop_takes,
                gate: None,
                before_refusing: None,
                alive: a.clone(),
            }) as Box<dyn RemoteFs + Send>)
        });
        (connect, made, alive)
    }

    fn probe_origin(slot: &SaveSlot, connect: Connect) -> Origin {
        Origin::new(
            slot.handle(),
            Some(slot.reconnect(connect)),
            Some("box".to_string()),
            "/f".to_string(),
            a_stat(),
        )
    }

    /// Until `done`, at most 5 s.
    fn within(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}: not in 5 s");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A hex Save's requests by handle are one operation: the session is not given back between
    /// two `write_at`s — nor right after the last request (its publish may follow) — and is
    /// once the operation is over.
    #[test]
    fn the_session_is_not_given_back_inside_an_operation() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("f.bin");
        std::fs::write(&abs, b"bytes").unwrap();
        let abs = abs.to_str().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let slot = SaveSlot::new(Placeholder::YES);
        let mut origin = origin(&slot, counted(&n), abs);
        let patcher = origin.fs().unwrap().patcher().unwrap();
        let temp = format!("{abs}.temp");
        let mut file = patcher.create_temp(&temp).unwrap();
        patcher.write_at(&mut file, 0, b"ab").unwrap();
        assert!(!slot.release(), "a handle is open: refused");
        patcher.write_at(&mut file, 2, b"cd").unwrap();
        patcher.close(file);
        patcher.rename_over(&temp, abs).unwrap();
        assert!(!slot.release(), "right after the last request: refused");
        assert_eq!(n.load(Ordering::SeqCst), 1, "one session throughout");
        within("the give-back", || slot.release());
        assert_eq!(std::fs::read(abs).unwrap(), b"abcd");
    }

    /// Two first uses at once connect once: the second waits for the first's session and uses
    /// it — never two sessions.
    #[test]
    fn two_first_uses_at_once_connect_once() {
        let (connect, made, alive) = probes(Duration::from_millis(150), Duration::ZERO);
        let slot = SaveSlot::new(Placeholder::YES);
        let users: Vec<_> = (0..2)
            .map(|_| {
                let mut origin = probe_origin(&slot, connect.clone());
                std::thread::spawn(move || origin.fs().map(|fs| fs.realpath("/f").unwrap()))
            })
            .collect();
        for user in users {
            assert_eq!(user.join().unwrap().unwrap(), "1", "the one session");
        }
        assert_eq!(made.load(Ordering::SeqCst), 1, "one connect");
        assert_eq!(alive.load(Ordering::SeqCst), 1);
    }

    /// A refused stream gives back the session it took only to a slot still without one: a
    /// session made meanwhile (another use) stays, and the refused one is closed.
    #[test]
    fn a_refused_stream_gives_back_only_into_an_empty_slot() {
        let (made, alive) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let slot = SaveSlot::new(Placeholder::YES);
        let (m, a, s) = (made.clone(), alive.clone(), slot.clone());
        let connect: Connect = Arc::new(move || {
            let id = m.fetch_add(1, Ordering::SeqCst) + 1;
            a.fetch_add(1, Ordering::SeqCst);
            // The first session, taken by the stream, sees another use make the second.
            let other = s.clone();
            let before: Option<Box<dyn FnMut() + Send>> = (id == 1).then(|| {
                Box::new(move || {
                    other.handle().realpath("/f").unwrap();
                }) as Box<dyn FnMut() + Send>
            });
            Ok(Box::new(Probe {
                id,
                drop_takes: Duration::ZERO,
                gate: None,
                before_refusing: before,
                alive: a.clone(),
            }) as Box<dyn RemoteFs + Send>)
        });
        let mut origin = probe_origin(&slot, connect);
        assert_eq!(origin.fs().unwrap().realpath("/f").unwrap(), "1");
        let Err(refused) = slot.handle().into_ranged("/f") else {
            panic!("refused");
        };
        assert!(format!("{:#}", refused.error).contains("no stream here"));
        assert_eq!(
            slot.handle().realpath("/f").unwrap(),
            "2",
            "the one made meanwhile"
        );
        within("the refused one closed", || {
            alive.load(Ordering::SeqCst) == 1
        });
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    /// Giving the session back never waits for its close (an SFTP session's waits for a reply,
    /// up to 5 s): the caller — the window's pass — goes on at once, and it closes beside.
    #[test]
    fn giving_the_session_back_does_not_wait_for_its_close() {
        let (connect, _made, alive) = probes(Duration::ZERO, Duration::from_millis(600));
        let slot = SaveSlot::new(Placeholder::YES);
        let mut origin = probe_origin(&slot, connect);
        origin.fs().unwrap();
        let mut took = Duration::MAX;
        within("the give-back", || {
            let at = std::time::Instant::now();
            let done = slot.release();
            took = at.elapsed();
            done
        });
        assert!(took < Duration::from_millis(200), "it waited {took:?}");
        within("closed beside", || alive.load(Ordering::SeqCst) == 0);
    }

    /// What the server can do is answered at once while a request holds the session (an upload
    /// on a worker): the window's pass asks it.
    #[test]
    fn the_servers_answers_never_wait_for_a_request() {
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let alive = Arc::new(AtomicUsize::new(0));
        let (g, a) = (gate.clone(), alive.clone());
        let connect: Connect = Arc::new(move || {
            a.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Probe {
                id: 1,
                drop_takes: Duration::ZERO,
                gate: Some(g.clone()),
                before_refusing: None,
                alive: a.clone(),
            }) as Box<dyn RemoteFs + Send>)
        });
        let slot = SaveSlot::new(Placeholder::YES);
        let mut origin = probe_origin(&slot, connect);
        let worker = std::thread::spawn(move || {
            origin
                .fs()
                .unwrap()
                .read_to("/f", &mut Vec::new(), &mut |_| {})
                .unwrap();
        });
        within("the request runs", || *gate.0.lock().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let asker = slot.handle();
        let answers = std::thread::spawn(move || {
            let said = (
                asker.can_replace(),
                asker.can_link(),
                asker.hash_check_limit(),
            );
            let _ = tx.send(said);
        });
        let said = rx.recv_timeout(Duration::from_millis(300));
        *gate.0.lock().unwrap() = false;
        gate.1.notify_all();
        worker.join().unwrap();
        answers.join().unwrap();
        assert!(said.is_ok(), "the answers waited for the request");
    }

    /// The names of the methods a block names at its first indent (`    fn name`): a trait's
    /// body, or an impl's.
    fn methods(source: &str, opening: &str) -> std::collections::BTreeSet<String> {
        let body = &source[source.find(opening).expect(opening)..];
        let body = &body[..body.find("\n}\n").expect("the block's end")];
        body.lines()
            .filter_map(|l| l.strip_prefix("    fn "))
            .map(|l| l.split(['(', '<']).next().unwrap().to_string())
            .collect()
    }

    /// The calls a [`Counting`] connection got, by method.
    #[derive(Clone, Default)]
    struct Calls(Arc<Mutex<Vec<&'static str>>>);

    /// A connection that answers every method of the file layer — the provided ones too —
    /// by recording it.
    struct Counting(Calls);

    impl Counting {
        fn saw(&self, method: &'static str) {
            self.0 .0.lock().unwrap().push(method);
        }
    }

    fn a_stat() -> Stat {
        Stat {
            is_dir: false,
            is_file: true,
            is_symlink: false,
            size: 0,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            mode: 0o644,
            uid: None,
            gid: None,
            missing: None,
            sha256: None,
        }
    }

    fn uploaded() -> Uploaded {
        Uploaded {
            size: 0,
            sha256: [0; 32],
        }
    }

    fn written() -> WriteOutcome {
        WriteOutcome::Written {
            now: a_stat(),
            notes: Vec::new(),
        }
    }

    impl RemoteFs for Counting {
        fn stat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
            self.saw("stat_opt");
            Ok(Some(a_stat()))
        }
        fn lstat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
            self.saw("lstat_opt");
            Ok(None)
        }
        fn realpath(&mut self, p: &str) -> Result<String> {
            self.saw("realpath");
            Ok(p.to_string())
        }
        fn read_head(&mut self, _: &str, _: usize) -> Result<Vec<u8>> {
            self.saw("read_head");
            Ok(Vec::new())
        }
        fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
            self.saw("list_dir");
            Ok(Vec::new())
        }
        fn list_entries(&mut self, _: &str) -> Result<Vec<Entry>> {
            self.saw("list_entries");
            Ok(Vec::new())
        }
        fn free_space(&mut self, _: &str) -> Option<u64> {
            self.saw("free_space");
            None
        }
        fn read_to(
            &mut self,
            _: &str,
            _: &mut dyn std::io::Write,
            _: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            self.saw("read_to");
            Ok(0)
        }
        fn create_from(
            &mut self,
            _: &Path,
            _: &str,
            _: Option<Like>,
        ) -> std::result::Result<Uploaded, CreateError> {
            self.saw("create_from");
            Ok(uploaded())
        }
        fn create_from_stoppable(
            &mut self,
            _: &Path,
            _: &str,
            _: Option<Like>,
            _: &dyn Fn() -> bool,
        ) -> std::result::Result<Uploaded, CreateError> {
            self.saw("create_from_stoppable");
            Ok(uploaded())
        }
        fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
            self.saw("set_mode");
            Ok(())
        }
        fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
            self.saw("rename_over");
            Ok(())
        }
        fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
            self.saw("link_new");
            Ok(())
        }
        fn can_link(&self) -> bool {
            self.saw("can_link");
            false
        }
        fn can_replace(&self) -> bool {
            self.saw("can_replace");
            false
        }
        fn remove(&mut self, _: &str) -> Result<()> {
            self.saw("remove");
            Ok(())
        }
        fn hash_check_limit(&self) -> u64 {
            self.saw("hash_check_limit");
            1
        }
        fn into_ranged(self: Box<Self>, _: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
            self.saw("into_ranged");
            Err(Refused {
                conn: self,
                error: anyhow!("no stream here"),
            })
        }
        fn patcher(&mut self) -> Option<&mut dyn PatchFs> {
            self.saw("patcher");
            Some(self)
        }
        fn stat(&mut self, _: &str) -> Result<Stat> {
            self.saw("stat");
            Ok(a_stat())
        }
        fn fetch(&mut self, _: &str, _: &Path, _: &mut dyn FnMut(u64, u64)) -> Result<Stat> {
            self.saw("fetch");
            Ok(a_stat())
        }
        fn fetch_cancellable(
            &mut self,
            _: &str,
            _: &Path,
            _: &mut dyn FnMut(u64, u64),
            _: &AtomicBool,
        ) -> Result<Stat> {
            self.saw("fetch_cancellable");
            Ok(a_stat())
        }
        fn write_atomic(&mut self, _: &Path, _: &str, _: Fingerprint) -> Result<WriteOutcome> {
            self.saw("write_atomic");
            Ok(written())
        }
        fn write_atomic_with(
            &mut self,
            _: &Path,
            _: &str,
            _: Fingerprint,
            _: &mut dyn FnMut(Step<'_>) -> Result<()>,
        ) -> Result<WriteOutcome> {
            self.saw("write_atomic_with");
            Ok(written())
        }
        fn write_new_sibling(&mut self, _: &Path, _: &str, wanted: &str) -> Result<String> {
            self.saw("write_new_sibling");
            Ok(wanted.to_string())
        }
        fn exchange(&mut self, _: &str, _: &str) -> Result<()> {
            self.saw("exchange");
            Ok(())
        }
        fn file_id(&mut self, _: &str) -> Result<Option<(FileId, u64)>> {
            self.saw("file_id");
            Ok(None)
        }
        fn sync_dir(&mut self, _: &str) -> Result<()> {
            self.saw("sync_dir");
            Ok(())
        }
        fn move_new(&mut self, _: &str, _: &str) -> Result<()> {
            self.saw("move_new");
            Ok(())
        }
        fn replace_guarded(
            &mut self,
            _: &Path,
            _: &str,
            _: Fingerprint,
            _: Option<FileId>,
            _: &mut dyn FnMut(Guard<'_>) -> Result<()>,
        ) -> Result<Replaced> {
            self.saw("replace_guarded");
            Ok(Replaced {
                outcome: written(),
                kept: None,
                unguarded: None,
            })
        }
    }

    impl PatchFs for Counting {
        fn can_copy(&self) -> bool {
            self.saw("can_copy");
            true
        }
        fn open(&mut self, path: &str, _: OpenFor) -> Result<Handle> {
            self.saw("open");
            Ok(Handle::new(path, ()))
        }
        fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError> {
            self.saw("create_temp");
            Ok(Handle::new(path, ()))
        }
        fn fstat(&mut self, _: &mut Handle) -> Result<Stat> {
            self.saw("fstat");
            Ok(a_stat())
        }
        fn read_at(&mut self, _: &mut Handle, _: u64, _: usize) -> Result<Vec<u8>> {
            self.saw("read_at");
            Ok(Vec::new())
        }
        fn read_through(
            &mut self,
            _: &mut Handle,
            _: u64,
            _: Option<&mut Handle>,
            _: &mut Each<'_>,
        ) -> Result<u64> {
            self.saw("read_through");
            Ok(0)
        }
        fn write_at(&mut self, _: &mut Handle, _: u64, _: &[u8]) -> Result<()> {
            self.saw("write_at");
            Ok(())
        }
        fn set_owner(&mut self, _: &mut Handle, _: u32, _: u32) -> Result<()> {
            self.saw("set_owner");
            Ok(())
        }
        fn set_handle_mode(&mut self, _: &mut Handle, _: u32) -> Result<()> {
            self.saw("set_handle_mode");
            Ok(())
        }
        fn can_fsync(&self) -> bool {
            self.saw("can_fsync");
            false
        }
        fn fsync(&mut self, _: &mut Handle) -> Result<()> {
            self.saw("fsync");
            Ok(())
        }
        fn copy(
            &mut self,
            _: &mut Handle,
            _: &mut Handle,
            _: u64,
            _: &mut CopyCtl<'_>,
        ) -> Result<()> {
            self.saw("copy");
            Ok(())
        }
        fn close(&mut self, _: Handle) {
            self.saw("close");
        }
    }

    /// Every method of the file layer reaches the slot's session through a handle — the
    /// provided ones too, which a handle answering by default would keep from the session (its
    /// own `fetch`, `write_atomic`, …): each is driven once through the handle and heard by the
    /// connection behind it — the server's answers (`can_link`, …) as the session gave them when
    /// it was made, each unlike the slot's own before it. What is driven is every method the two
    /// traits name today and the handle implements, read from their sources: a method added to
    /// `RemoteFs` or `PatchFs` fails here until the handle forwards it and this drives it.
    #[test]
    fn every_method_of_the_file_layer_reaches_the_session() {
        let calls = Calls::default();
        let theirs = calls.clone();
        let connect: Connect =
            Arc::new(move || Ok(Box::new(Counting(theirs.clone())) as Box<dyn RemoteFs + Send>));
        let slot = SaveSlot::new(Placeholder::YES);
        let mut origin = Origin::new(
            slot.handle(),
            Some(slot.reconnect(connect)),
            Some("box".to_string()),
            "/f".to_string(),
            a_stat(),
        );
        let fs = origin.fs().unwrap();
        let (p, local) = ("/f", Path::new("/nowhere"));
        let print = Fingerprint {
            size: 0,
            mtime: std::time::SystemTime::UNIX_EPOCH,
            sha256: None,
        };
        let mut driven = vec![];
        let mut drive = |name: &'static str, ok: bool| {
            assert!(ok, "{name}");
            driven.push(name);
        };
        drive("stat_opt", fs.stat_opt(p).is_ok());
        drive("lstat_opt", fs.lstat_opt(p).is_ok());
        drive("realpath", fs.realpath(p).is_ok());
        drive("read_head", fs.read_head(p, 1).is_ok());
        drive("list_dir", fs.list_dir(p).is_ok());
        drive("list_entries", fs.list_entries(p).is_ok());
        drive("free_space", fs.free_space(p).is_none());
        drive(
            "read_to",
            fs.read_to(p, &mut Vec::new(), &mut |_| {}).is_ok(),
        );
        drive("create_from", fs.create_from(local, p, None).is_ok());
        let stoppable = fs.create_from_stoppable(local, p, None, &|| false);
        drive("create_from_stoppable", stoppable.is_ok());
        drive("set_mode", fs.set_mode(p, 0o600).is_ok());
        drive("rename_over", fs.rename_over(p, p).is_ok());
        drive("link_new", fs.link_new(p, p).is_ok());
        drive("can_link", !fs.can_link());
        drive("can_replace", !fs.can_replace());
        drive("remove", fs.remove(p).is_ok());
        drive("hash_check_limit", fs.hash_check_limit() == 1);
        drive("stat", fs.stat(p).is_ok());
        drive("fetch", fs.fetch(p, local, &mut |_, _| {}).is_ok());
        let never = AtomicBool::new(false);
        let fetched = fs.fetch_cancellable(p, local, &mut |_, _| {}, &never);
        drive("fetch_cancellable", fetched.is_ok());
        drive("write_atomic", fs.write_atomic(local, p, print).is_ok());
        let with = fs.write_atomic_with(local, p, print, &mut |_| Ok(()));
        drive("write_atomic_with", with.is_ok());
        drive(
            "write_new_sibling",
            fs.write_new_sibling(local, "/", "g").is_ok(),
        );
        drive("exchange", fs.exchange(p, p).is_ok());
        drive("file_id", fs.file_id(p).is_ok());
        drive("sync_dir", fs.sync_dir("/").is_ok());
        drive("move_new", fs.move_new(p, p).is_ok());
        let guarded = fs.replace_guarded(local, p, print, None, &mut |_| Ok(()));
        drive("replace_guarded", guarded.is_ok());
        let pf = fs.patcher().expect("a patcher");
        drive("patcher", true);
        drive("can_copy", pf.can_copy());
        let mut a = pf.open(p, OpenFor::Read).unwrap();
        drive("open", true);
        let mut b = pf.create_temp(p).unwrap();
        drive("create_temp", true);
        drive("fstat", pf.fstat(&mut a).is_ok());
        drive("read_at", pf.read_at(&mut a, 0, 1).is_ok());
        let through = pf.read_through(&mut a, 0, Some(&mut b), &mut |_, _| Ok(None));
        drive("read_through", through.is_ok());
        drive("write_at", pf.write_at(&mut b, 0, b"x").is_ok());
        drive("set_owner", pf.set_owner(&mut b, 0, 0).is_ok());
        drive("set_handle_mode", pf.set_handle_mode(&mut b, 0o600).is_ok());
        drive("can_fsync", !pf.can_fsync());
        drive("fsync", pf.fsync(&mut b).is_ok());
        let (mut clock, mut heard) = (crate::remote::patch::stopwatch(), |_| {});
        let mut ctl = CopyCtl {
            pace: crate::remote::patch::Pace::steps(1),
            clock: &mut clock,
            cancel: &|| false,
            progress: &mut heard,
        };
        drive("copy", pf.copy(&mut a, &mut b, 0, &mut ctl).is_ok());
        pf.close(a);
        pf.close(b);
        drive("close", true);
        // The stream last: the session goes to it (refused here, the slot's again).
        let Err(refused) = slot.handle().into_ranged(p) else {
            panic!("refused");
        };
        assert!(format!("{:#}", refused.error).contains("no stream here"));
        drive("into_ranged", true);
        assert!(slot.connected(), "the refused session is the slot's again");

        let heard = calls.0.lock().unwrap().clone();
        for name in &driven {
            assert!(
                heard.contains(name),
                "{name} did not reach the session: {heard:?}"
            );
        }
        let driven: std::collections::BTreeSet<String> =
            driven.iter().map(|n| n.to_string()).collect();
        let (remote, patch, here) = (
            include_str!("../../../remote/mod.rs"),
            include_str!("../../../remote/patch.rs"),
            include_str!("shared.rs"),
        );
        let named = |src, opening| methods(src, opening);
        let traits: std::collections::BTreeSet<String> = named(remote, "pub trait RemoteFs:")
            .into_iter()
            .chain(named(patch, "pub trait PatchFs:"))
            .collect();
        assert_eq!(driven, traits, "every method the traits name, driven");
        let forwarded: std::collections::BTreeSet<String> =
            named(here, "impl RemoteFs for SlotFs {")
                .into_iter()
                .chain(named(here, "impl PatchFs for SlotFs {"))
                .collect();
        assert_eq!(forwarded, traits, "every method the traits name, forwarded");
    }
}
