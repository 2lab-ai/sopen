//! What the views of one remote file share (`.prd/08-viewer.md` §5–§6, D13, D14): the stream
//! the opening read it through, kept beside the views so that a view made later reads nothing
//! again, and the viewer's one save session, which a hidden view gives back.
//!
//! Every remote `Origin` of a viewer's views connects through the viewer's [`SaveSlot`]: the
//! session it opens on its first use (a Save, a Save copy, a Save As, an Export, the recovery
//! of an earlier session's edits) is the slot's, and the `Origin` reaches it through a handle
//! ([`SaveSlot::handle`]). A view hidden gives it back ([`SaveSlot::release`]): the slot is
//! empty until the next use connects again — `Origin::fs`'s probe, or the next call of a handle
//! taken before (an Export takes it before its render and publishes on it after) — never while
//! a request runs. So a viewer holds its stream and at most one save session — 06 §6's "1; 2
//! from its first Save" — however many of its views saved.

use super::super::Connect;
use crate::gui::{Placeholder, Reconnect};
use crate::remote::patch::{CopyCtl, Each, Handle, OpenFor, PatchFs};
use crate::remote::{
    CreateError, Created, Entry, Fingerprint, Like, Ranged, Refused, RemoteFs, Stat, Step,
    Uploaded, WriteOutcome,
};
use crate::stream::Source;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
/// one of them is first used, given back when the view that used it is hidden.
#[derive(Clone)]
pub struct SaveSlot(Arc<Mutex<Slot>>);

struct Slot {
    conn: Option<Box<dyn RemoteFs + Send>>,
    /// What the server can do: as given until a session answers, then that session's answers,
    /// kept while the slot is empty.
    can: Placeholder,
    /// How a session is opened (the `Origin`s' reconnect's).
    connect: Option<Connect>,
    /// The session was given back ([`SaveSlot::release`]): the next call connects again.
    given_back: bool,
    /// Which session the slot holds, counted from 1: a file handle is stamped with the one it
    /// was opened on and reaches no other ([`Stamped`]).
    session: u64,
}

impl Slot {
    fn put(&mut self, conn: Box<dyn RemoteFs + Send>) {
        self.can = Placeholder {
            can_replace: conn.can_replace(),
            can_link: conn.can_link(),
        };
        self.conn = Some(conn);
        self.given_back = false;
        self.session += 1;
    }

    /// The session, connected again first if it was given back. Before the first use there is
    /// none: that one is `Origin::fs`'s, through its reconnect, as a placeholder's.
    fn conn(&mut self) -> Result<&mut Box<dyn RemoteFs + Send>> {
        if self.conn.is_none() && self.given_back {
            if let Some(connect) = self.connect.clone() {
                self.put(connect()?);
            }
        }
        self.conn.as_mut().ok_or_else(not_connected)
    }
}

impl SaveSlot {
    /// An empty slot whose handles answer `can` until a session does.
    pub fn new(can: Placeholder) -> Self {
        Self(Arc::new(Mutex::new(Slot {
            conn: None,
            can,
            connect: None,
            given_back: false,
            session: 0,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An `Origin`'s connection: the slot's session, while it has one.
    pub fn handle(&self) -> Box<dyn RemoteFs + Send> {
        Box::new(SlotFs(self.clone()))
    }

    /// An `Origin`'s reconnect: a session of the slot's, opened by `connect` — the one before it
    /// (gone, or given back) dropped first, so never two at once — and a handle onto it.
    pub fn reconnect(&self, connect: Connect) -> Reconnect {
        let slot = self.clone();
        slot.lock().connect.get_or_insert_with(|| connect.clone());
        Box::new(move || {
            drop(slot.lock().conn.take());
            let conn = connect()?;
            slot.lock().put(conn);
            Ok(slot.handle())
        })
    }

    /// D14: the session goes back — closed — so the slot is empty until the next use connects
    /// again. Not while a request holds it (`false`: it stays). `true`: there was one.
    pub fn release(&self) -> bool {
        let mut slot = match self.0.try_lock() {
            Ok(slot) => slot,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        let had = slot.conn.take().is_some();
        slot.given_back |= had;
        had
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

/// A file handle as a [`SlotFs`] gives it out: the session's own, and which session that was.
struct Stamped {
    session: u64,
    handle: Handle,
}

/// A handle onto a [`SaveSlot`]'s session: every call goes to the session the slot holds at
/// that moment — connected again if it was given back — and fails as a placeholder's does
/// before the first use. The server's answers (`can_replace`, `can_link`) come from the slot
/// while it holds none.
struct SlotFs(SaveSlot);

impl SlotFs {
    fn with<T>(&self, f: impl FnOnce(&mut (dyn RemoteFs + Send)) -> Result<T>) -> Result<T> {
        let mut slot = self.0.lock();
        f(slot.conn()?.as_mut())
    }

    /// A file opened (or created) on the session the slot holds — connected again first if it
    /// was given back — stamped with that session.
    fn opening<E>(
        &self,
        path: &str,
        refused: impl FnOnce(anyhow::Error) -> E,
        f: impl FnOnce(&mut dyn PatchFs) -> std::result::Result<Handle, E>,
    ) -> std::result::Result<Handle, E> {
        let mut slot = self.0.lock();
        let patcher = match slot.conn().map(|c| c.patcher().is_some()) {
            Ok(true) => slot.conn.as_mut().and_then(|c| c.patcher()).expect("one"),
            Ok(false) => return Err(refused(no_patcher())),
            Err(e) => return Err(refused(e)),
        };
        let handle = f(patcher)?;
        let session = slot.session;
        Ok(Handle::new(path, Stamped { session, handle }))
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
        let mut slot = self.0.lock();
        let session = slot.session;
        let gone = |h: &Handle| anyhow!("{} was open on a session given back", h.path());
        let stale = |h: &mut Handle| h.get::<Stamped>().map(|s| s.session != session);
        if stale(file)? {
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
        match slot.conn.as_mut().and_then(|c| c.patcher()) {
            Some(patcher) => f(patcher, file, other),
            None => Err(not_connected()),
        }
    }

    fn create(
        &self,
        f: impl FnOnce(&mut (dyn RemoteFs + Send)) -> std::result::Result<Uploaded, CreateError>,
    ) -> std::result::Result<Uploaded, CreateError> {
        let mut slot = self.0.lock();
        match slot.conn() {
            Ok(conn) => f(conn.as_mut()),
            Err(e) => Err(CreateError::new(Created::No, e)),
        }
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
        self.0.lock().conn().ok()?.free_space(path)
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
        let slot = self.0.lock();
        slot.conn
            .as_ref()
            .map_or(slot.can.can_link, |c| c.can_link())
    }
    fn can_replace(&self) -> bool {
        let slot = self.0.lock();
        slot.conn
            .as_ref()
            .map_or(slot.can.can_replace, |c| c.can_replace())
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.with(|c| c.remove(path))
    }
    fn hash_check_limit(&self) -> u64 {
        let slot = self.0.lock();
        slot.conn
            .as_ref()
            .map_or(crate::remote::HASH_CHECK_MAX, |c| c.hash_check_limit())
    }
    /// A hex Save runs on the slot's session by handle (07 §4): through this handle too.
    fn patcher(&mut self) -> Option<&mut dyn PatchFs> {
        let patches = (self.0.lock().conn().ok()).is_some_and(|c| c.patcher().is_some());
        if patches {
            Some(self)
        } else {
            None
        }
    }
    /// The session becomes the stream's (06 §2): the slot holds none from then on, and the
    /// next call connects again, as after a release. Refused, it is the slot's again.
    fn into_ranged(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        let taken = {
            let mut slot = self.0.lock();
            match slot.conn() {
                Ok(_) => {
                    slot.given_back = true;
                    Ok(slot.conn.take().expect("connected"))
                }
                Err(error) => Err(error),
            }
        };
        match taken {
            Err(error) => Err(Refused { conn: self, error }),
            Ok(conn) => conn.into_ranged(path).map_err(|refused| {
                self.0.lock().put(refused.conn);
                Refused {
                    conn: self,
                    error: refused.error,
                }
            }),
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
    fn write_new_sibling(&mut self, local: &Path, dir: &str, wanted: &str) -> Result<String> {
        self.with(|c| c.write_new_sibling(local, dir, wanted))
    }
}

impl PatchFs for SlotFs {
    fn can_copy(&self) -> bool {
        let mut slot = self.0.lock();
        slot.conn
            .as_mut()
            .and_then(|c| c.patcher())
            .is_some_and(|p| p.can_copy())
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
        let mut slot = self.0.lock();
        slot.conn
            .as_mut()
            .and_then(|c| c.patcher())
            .is_some_and(|p| p.can_fsync())
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
    /// A handle of a session given back is gone with it: nothing to close on another one.
    fn close(&mut self, file: Handle) {
        let mut slot = self.0.lock();
        let session = slot.session;
        let Some(stamped) = file.into_inner::<Stamped>() else {
            return;
        };
        if let Some(patcher) =
            (slot.conn.as_mut().and_then(|c| c.patcher())).filter(|_| stamped.session == session)
        {
            patcher.close(stamped.handle);
        }
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

    /// Two `Origin`s on one slot: before the first use a handle fails as a placeholder does;
    /// the first use connects once and the other `Origin` uses that session; given back, the
    /// slot holds none until the next use connects again — one session.
    #[test]
    fn origins_share_the_slots_one_session_and_connect_again_after_it_is_given_back() {
        let tmp = tempfile::tempdir().unwrap();
        let abs = tmp.path().join("f.bin");
        std::fs::write(&abs, b"bytes").unwrap();
        let abs = abs.to_str().unwrap();
        let n = Arc::new(AtomicUsize::new(0));
        let slot = SaveSlot::new(Placeholder::YES);
        let (mut a, mut b) = (
            origin(&slot, counted(&n), abs),
            origin(&slot, counted(&n), abs),
        );
        assert!(!slot.connected() && a.can_replace(), "placeholder answers");
        assert!(slot.handle().stat_opt(abs).is_err(), "not connected yet");
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert_eq!(a.fs().unwrap().stat(abs).unwrap().size, 5);
        assert_eq!(n.load(Ordering::SeqCst), 1, "the first use connects");
        assert!(b.fs().unwrap().patcher().is_some(), "a hex Save runs on it");
        assert_eq!(n.load(Ordering::SeqCst), 1, "the other uses that session");
        assert!(slot.release(), "given back");
        assert!(!slot.connected() && !slot.release());
        assert_eq!(b.fs().unwrap().stat(abs).unwrap().size, 5);
        assert_eq!(n.load(Ordering::SeqCst), 2, "the next use connects again");
        assert!(slot.connected());
    }

    /// A file handle reaches only the session it was opened on: once that session went back,
    /// a request by it is refused — never sent on the session made since, where the same
    /// handle may name another file — and closing it sends nothing.
    #[test]
    fn a_file_handle_of_a_session_given_back_is_refused() {
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
        assert!(slot.release());
        let patcher = origin.fs().unwrap().patcher().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 2, "a session made since");
        let refused = patcher.read_at(&mut file, 0, 5).unwrap_err();
        assert!(format!("{refused:#}").contains("given back"), "{refused:#}");
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
        let mut origin = origin(&slot, counted(&n), abs);
        let fs = origin.fs().unwrap();
        assert!(slot.release(), "given back between two requests");
        let sibling = format!("{abs}.sibling");
        fs.create_from(Path::new(abs), &sibling, None).unwrap();
        assert_eq!(std::fs::read(&sibling).unwrap(), b"bytes", "published");
        assert_eq!(n.load(Ordering::SeqCst), 2, "connected again, once");
        assert!(slot.connected());
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
            true
        }
        fn can_replace(&self) -> bool {
            self.saw("can_replace");
            true
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
            true
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
    /// connection behind it. What is driven is every method the two traits name today and the
    /// handle implements, read from their sources: a method added to `RemoteFs` or `PatchFs`
    /// fails here until the handle forwards it and this drives it.
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
        calls.0.lock().unwrap().clear();
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
        drive("can_link", fs.can_link());
        drive("can_replace", fs.can_replace());
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
        drive("can_fsync", pf.can_fsync());
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
