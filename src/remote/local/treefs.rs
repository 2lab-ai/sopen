//! [`TreeFs`] on the local disk (05 §6.2, §6.3, §7): bulk listings (macOS `getattrlistbulk`,
//! Linux `getdents64` + `statx` with `AT_STATX_DONT_SYNC`, asking only for type, size, inode,
//! device and link count), the mount table (`getfsstat(2)`, `/proc/self/mountinfo`) and
//! `statvfs` — [`Disk`]'s calls, each run on the watchdog pool ([`Pool`]) and never on the
//! caller's thread. A deep scan is a queue of folders whose listings are calls of their own, at
//! most [`MAX_CALLS`] at once, nearest the bar first, so a scan never holds a row's listing back.

use super::LocalFs;
use crate::remote::treefs::{
    is_cachedir_tag, scan_step, Ask, Budget, Child, Disk, FsStat, Got, Kind, Listing, Live, Mount,
    Mounts, Reply, Resolved, ScanAsk, ScanEnd, Scanned, Target, Totals, TreeFs,
};
use crate::remote::Wake;
use crate::tree::watchdog::{Job, Pool, Then, MAX_CALLS};
use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

impl LocalFs {
    /// The local tree's worker: the disk's calls on the process's watchdog pool.
    pub fn tree() -> Box<dyn TreeFs> {
        Box::new(LocalTree::new(Arc::new(RealDisk), Pool::global()))
    }

    /// The worker over another [`Disk`] and pool (tests: a fake behind the same trait).
    #[cfg(test)]
    pub(crate) fn tree_with(disk: Arc<dyn Disk>, pool: Pool) -> Box<dyn TreeFs> {
        Box::new(LocalTree::new(disk, pool))
    }

    /// The real disk's calls (tests wrap them).
    #[cfg(test)]
    pub(crate) fn disk() -> Arc<dyn Disk> {
        Arc::new(RealDisk)
    }
}

/// Answers on their way to the tree, and its repaint.
struct Outbox {
    tx: Mutex<mpsc::Sender<Reply>>,
    wake: Mutex<Option<Wake>>,
}

impl Outbox {
    fn send(&self, id: u64, got: Got) {
        let _ = self
            .tx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(Reply { id, got });
        let wake = self.wake.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(w) = wake {
            w.wake();
        }
    }
}

/// The requests still running, each with the flag that cancels it; a request leaves when it
/// ends (answered, abandoned or cancelled), so nothing piles up over a long session.
#[derive(Default)]
struct Wanted {
    closed: AtomicBool,
    live: Mutex<std::collections::HashMap<u64, Arc<AtomicBool>>>,
}

/// One request's "no longer wanted": the tree closed, or the request was cancelled.
#[derive(Clone)]
struct Stop {
    wanted: Arc<Wanted>,
    cancelled: Arc<AtomicBool>,
}

impl Stop {
    fn dropped(&self) -> bool {
        self.wanted.closed.load(Ordering::SeqCst) || self.cancelled.load(Ordering::SeqCst)
    }
}

impl Wanted {
    fn live(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, Arc<AtomicBool>>> {
        self.live.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts tracking `id`.
    fn track(self: &Arc<Self>, id: u64) -> Stop {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.live().insert(id, cancelled.clone());
        Stop {
            wanted: self.clone(),
            cancelled,
        }
    }

    /// `id` has ended.
    fn done(&self, id: u64) {
        self.live().remove(&id);
    }

    fn cancel(&self, id: u64) {
        if let Some(flag) = self.live().remove(&id) {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

struct LocalTree {
    disk: Arc<dyn Disk>,
    pool: Pool,
    out: Arc<Outbox>,
    rx: mpsc::Receiver<Reply>,
    wanted: Arc<Wanted>,
}

impl LocalTree {
    fn new(disk: Arc<dyn Disk>, pool: Pool) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            disk,
            pool,
            out: Arc::new(Outbox {
                tx: Mutex::new(tx),
                wake: Mutex::new(None),
            }),
            rx,
            wanted: Arc::default(),
        }
    }

    /// One call for `id`: `call` on a pool thread, its answer sent unless it was abandoned
    /// (then `not answering` is sent instead, once).
    fn call(
        &self,
        id: u64,
        prio: u64,
        call: impl FnOnce(&dyn Disk, &Live) -> Got + Send + 'static,
    ) {
        let (disk, out) = (self.disk.clone(), self.out.clone());
        let late = self.out.clone();
        let stop = self.wanted.track(id);
        let (when, gone) = (stop.clone(), stop.clone());
        self.pool.submit(Job {
            prio,
            dropped: Arc::new(move || when.dropped()),
            work: Box::new(move |live| {
                let got = call(&*disk, live);
                Box::new(move || {
                    stop.wanted.done(id);
                    if !stop.dropped() {
                        out.send(id, got);
                    }
                }) as Then
            }),
            abandoned: Box::new(move || {
                gone.wanted.done(id);
                late.send(id, Got::NotAnswering);
            }),
        });
    }
}

impl TreeFs for LocalTree {
    fn send(&mut self, id: u64, prio: u64, ask: Ask) {
        let text = |e: io::Error| e.to_string();
        match ask {
            Ask::Resolve(path) => self.call(id, prio, move |d, _| {
                Got::Resolved(d.resolve(&path).map_err(text))
            }),
            Ask::Home(_) => self.call(id, prio, move |d, _| Got::Home(d.home().map_err(text))),
            Ask::Mounts => self.call(id, prio, move |d, _| {
                Got::Mounts(d.mounts().unwrap_or(Mounts {
                    table: Some(Vec::new()),
                    macos: cfg!(target_os = "macos"),
                    statvfs: true,
                }))
            }),
            Ask::List {
                path,
                budget,
                guard,
            } => self.call(id, prio, move |d, live| {
                Got::Listed(d.list(&path, budget, true, &guard, live).map_err(text))
            }),
            Ask::Statvfs(path) => self.call(id, prio, move |d, _| {
                Got::Statvfs(d.statvfs(&path).map_err(text))
            }),
            Ask::Scan(scan) => Scan::start(self, id, prio, scan),
        }
    }

    fn cancel(&mut self, id: u64) {
        self.wanted.cancel(id);
    }

    fn next(&mut self, wait: Duration) -> Option<Reply> {
        if wait.is_zero() {
            self.rx.try_recv().ok()
        } else {
            self.rx.recv_timeout(wait).ok()
        }
    }

    fn set_wake(&mut self, wake: Wake) {
        *self.out.wake.lock().unwrap_or_else(|p| p.into_inner()) = Some(wake);
    }

    fn close(&mut self) {
        self.wanted.closed.store(true, Ordering::SeqCst);
    }

    fn held(&self) -> Option<String> {
        self.pool.held()
    }

    fn is_local(&self) -> bool {
        true
    }
}

#[cfg(test)]
impl LocalTree {
    /// Requests the worker still keeps track of (tests: none once all have ended).
    fn tracked(&self) -> usize {
        self.wanted.live().len()
    }
}

impl Drop for LocalTree {
    fn drop(&mut self) {
        self.close();
    }
}

/// A deep scan on the pool: a queue of folders, at most [`MAX_CALLS`] of its listings at once.
struct Scan {
    id: u64,
    prio: u64,
    disk: Arc<dyn Disk>,
    pool: Pool,
    out: Arc<Outbox>,
    stop: Stop,
    skip: HashSet<String>,
    budget: Option<crate::remote::treefs::ScanBudget>,
    /// Each folder's listing stops at this.
    listing: Option<Budget>,
    state: Mutex<ScanState>,
}

struct ScanState {
    queue: VecDeque<String>,
    flying: usize,
    totals: Totals,
    seen: HashSet<(u64, u64)>,
    started: Instant,
    reported: Instant,
    over: bool,
}

impl Scan {
    fn start(tree: &LocalTree, id: u64, prio: u64, ask: ScanAsk) {
        let now = Instant::now();
        let scan = Arc::new(Scan {
            id,
            prio,
            disk: tree.disk.clone(),
            pool: tree.pool.clone(),
            out: tree.out.clone(),
            stop: tree.wanted.track(id),
            skip: ask.skip.into_iter().collect(),
            budget: ask.budget,
            listing: ask.listing,
            state: Mutex::new(ScanState {
                queue: ask.start.into_iter().collect(),
                flying: 0,
                totals: ask.base,
                seen: ask.seen.into_iter().collect(),
                started: now - ask.base.time,
                reported: now,
                over: false,
            }),
        });
        scan.feed();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ScanState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts listings while fewer than [`MAX_CALLS`] of this scan's are out; ends the scan
    /// when nothing is left.
    fn feed(self: &Arc<Self>) {
        if self.stop.dropped() {
            return;
        }
        let mut end = None;
        let mut next = Vec::new();
        {
            let mut s = self.lock();
            s.totals.time = s.started.elapsed();
            if !s.over && s.totals.over(self.budget) {
                s.over = true;
            }
            if s.over {
                if s.flying == 0 {
                    end = Some(ScanEnd::Budget(s.queue.drain(..).collect()));
                }
            } else {
                while s.flying < MAX_CALLS {
                    let Some(dir) = s.queue.pop_front() else {
                        break;
                    };
                    s.flying += 1;
                    next.push(dir);
                }
                if s.flying == 0 && s.queue.is_empty() {
                    end = Some(ScanEnd::Complete);
                }
            }
            if let Some(end) = end {
                let totals = s.totals;
                drop(s);
                self.stop.wanted.done(self.id);
                self.out.send(
                    self.id,
                    Got::Scan(Scanned {
                        totals,
                        end: Some(end),
                    }),
                );
                return;
            }
        }
        for dir in next {
            self.list(dir);
        }
    }

    fn list(self: &Arc<Self>, dir: String) {
        let (me, late) = (self.clone(), self.clone());
        let stop = self.stop.clone();
        self.pool.submit(Job {
            prio: self.prio,
            dropped: Arc::new(move || stop.dropped()),
            work: Box::new(move |live| {
                let got = me.disk.list(&dir, me.listing, false, &[], live);
                Box::new(move || me.listed(&dir, got)) as Then
            }),
            abandoned: Box::new(move || {
                // One call that does not answer stops the scan: the row says so.
                late.lock().over = true;
                late.stop.wanted.done(late.id);
                late.out.send(late.id, Got::NotAnswering);
            }),
        });
    }

    fn listed(self: &Arc<Self>, dir: &str, got: io::Result<Listing>) {
        let progress = {
            let mut s = self.lock();
            s.flying -= 1;
            match got {
                Ok(listing) => {
                    let ScanState {
                        seen,
                        totals,
                        queue,
                        ..
                    } = &mut *s;
                    let below = scan_step(dir, &listing, &self.skip, seen, totals);
                    queue.extend(below);
                }
                Err(_) => s.totals.unread += 1,
            }
            s.totals.time = s.started.elapsed();
            let due = s.reported.elapsed() >= self.pool.timing().progress;
            if due {
                s.reported = Instant::now();
            }
            due.then_some(s.totals)
        };
        if let Some(totals) = progress {
            if !self.stop.dropped() {
                self.out
                    .send(self.id, Got::Scan(Scanned { totals, end: None }));
            }
        }
        self.feed();
    }
}

/// The disk.
struct RealDisk;

impl Disk for RealDisk {
    fn list(
        &self,
        path: &str,
        budget: Option<Budget>,
        full: bool,
        guard: &[String],
        live: &Live,
    ) -> io::Result<Listing> {
        let started = Instant::now();
        let mut listing = list_dir(path, budget, live)?;
        if full {
            let deadline = budget.map(|b| started + b.time);
            enrich(path, &mut listing, guard, deadline, live, &mut RealOps);
        }
        Ok(listing)
    }

    fn resolve(&self, path: &str) -> io::Result<Resolved> {
        resolve(path)
    }

    fn home(&self) -> io::Result<Resolved> {
        let home = std::env::var("HOME").map_err(|_| io::Error::other("HOME is not set"))?;
        resolve(&home)
    }

    fn mounts(&self) -> io::Result<Mounts> {
        Ok(Mounts {
            table: Some(mount_table()?),
            macos: cfg!(target_os = "macos"),
            statvfs: true,
        })
    }

    fn statvfs(&self, path: &str) -> io::Result<FsStat> {
        statvfs(path)
    }
}

fn cpath(path: &str) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(path).map_err(|_| io::Error::other("a path with a NUL byte"))
}

/// What an entry is, by `lstat` (a link is not followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seen {
    Dir,
    File,
    Link,
    Other,
}

/// The calls a row's listing makes after reading the folder (tests record them).
pub(crate) trait LinkOps {
    fn lstat(&mut self, path: &str) -> io::Result<Seen>;
    fn readlink(&mut self, path: &str) -> io::Result<String>;
    /// A file's first 43 bytes.
    fn head(&mut self, path: &str) -> io::Result<Vec<u8>>;
}

struct RealOps;

impl LinkOps for RealOps {
    fn lstat(&mut self, path: &str) -> io::Result<Seen> {
        let t = std::fs::symlink_metadata(path)?.file_type();
        Ok(if t.is_symlink() {
            Seen::Link
        } else if t.is_dir() {
            Seen::Dir
        } else if t.is_file() {
            Seen::File
        } else {
            Seen::Other
        })
    }

    fn readlink(&mut self, path: &str) -> io::Result<String> {
        std::fs::read_link(path)?
            .into_os_string()
            .into_string()
            .map_err(|_| io::Error::other("a link target that is not UTF-8"))
    }

    fn head(&mut self, path: &str) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut head = Vec::with_capacity(43);
        std::fs::File::open(path)?.take(43).read_to_end(&mut head)?;
        Ok(head)
    }
}

/// A row's listing after the folder is read: each link's target kind — never read at or below
/// one of `guard` (a mount of R4–R6: [`Target::Unread`]) — and the `CACHEDIR.TAG` read. Every
/// call is made only while the listing is wanted and inside its deadline; past it the links not
/// read yet stay non-rows and the listing says it is incomplete (05 §6.3).
pub(crate) fn enrich(
    dir: &str,
    listing: &mut Listing,
    guard: &[String],
    deadline: Option<Instant>,
    live: &Live,
    ops: &mut dyn LinkOps,
) {
    let go = || !(live.stop)() && deadline.is_none_or(|d| Instant::now() < d);
    for child in &mut listing.children {
        if child.kind != Kind::Link(Target::Other) || child.lossy {
            continue;
        }
        let target = follow(dir, &child.name, guard, &go, ops);
        (live.beat)();
        match target {
            Some(t) => child.kind = Kind::Link(t),
            None => {
                listing.complete = false;
                return;
            }
        }
    }
    let tagged = listing
        .children
        .iter()
        .any(|c| c.name == "CACHEDIR.TAG" && c.kind == Kind::File);
    if tagged {
        if !go() {
            listing.complete = false;
            return;
        }
        listing.cachedir = ops
            .head(&crate::remote::join(dir, "CACHEDIR.TAG"))
            .is_ok_and(|head| is_cachedir_tag(&head));
    }
}

/// What the link `dir/name` points at, resolved one component at a time (`lstat`, and
/// `readlink` for each link on the way, at most 40), never touching a path at or below one of
/// `guard`: one there is [`Target::Unread`] — a lookup inside an automounter or a hung network
/// mount is the thing to avoid. `None`: `go` said stop before the answer.
fn follow(
    dir: &str,
    name: &str,
    guard: &[String],
    go: &dyn Fn() -> bool,
    ops: &mut dyn LinkOps,
) -> Option<Target> {
    use crate::remote::treefs::guarded;
    // `dir` was listed by its path: it is a folder, and no link is in it.
    let mut at = dir.to_string();
    let mut todo: VecDeque<String> = VecDeque::from([name.to_string()]);
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        match part.as_str() {
            "" | "." => continue,
            // Above `/` stays at `/`, as the kernel has it.
            ".." => {
                if at != "/" {
                    at = crate::remote::split_parent(&at).0.to_string();
                }
                continue;
            }
            _ => {}
        }
        let next = crate::remote::join(&at, &part);
        if guarded(&next, guard) {
            return Some(Target::Unread);
        }
        if !go() {
            return None;
        }
        match ops.lstat(&next) {
            Ok(Seen::Dir) => at = next,
            Ok(Seen::Link) => {
                hops += 1;
                if hops > 40 || !go() {
                    return if hops > 40 { Some(Target::Other) } else { None };
                }
                let Ok(to) = ops.readlink(&next) else {
                    return Some(Target::Other);
                };
                if to.starts_with('/') {
                    at = "/".to_string();
                }
                for p in to.split('/').rev() {
                    todo.push_front(p.to_string());
                }
            }
            Ok(Seen::File) if todo.iter().all(|p| p.is_empty() || p == ".") => {
                return Some(Target::File)
            }
            Ok(_) | Err(_) => return Some(Target::Other),
        }
    }
    Some(Target::Dir)
}

/// `realpath`, with `(st_dev, st_ino)` of `/` and of each folder on the way to it.
fn resolve(path: &str) -> io::Result<Resolved> {
    use std::os::unix::fs::MetadataExt;
    let real = std::fs::canonicalize(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::other(format!("{path}: path is not valid UTF-8")))?;
    let mut ids = Vec::new();
    let mut at = String::from("/");
    let id = |p: &str| std::fs::symlink_metadata(p).map(|m| (m.dev(), m.ino()));
    ids.push((at.clone(), id(&at)?));
    for part in real.split('/').filter(|p| !p.is_empty()) {
        at = crate::remote::join(&at, part);
        ids.push((at.clone(), id(&at)?));
    }
    Ok(Resolved { path: real, ids })
}

#[allow(clippy::unnecessary_cast)] // the field types differ between macOS and Linux
fn statvfs(path: &str) -> io::Result<FsStat> {
    let c = cpath(path)?;
    // SAFETY: an all-zero statvfs is a valid value to be overwritten.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` a valid place for the result.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let frsize = st.f_frsize as u64;
    Ok(FsStat {
        used_bytes: (st.f_blocks as u64)
            .saturating_sub(st.f_bfree as u64)
            .saturating_mul(frsize),
        used_files: (st.f_files as u64).saturating_sub(st.f_ffree as u64),
        fsid: st.f_fsid as u64,
    })
}

/// Whether the listing should stop: its budget is spent (or it is no longer wanted).
fn spent(budget: Option<Budget>, n: usize, started: Instant, live: &Live) -> bool {
    (live.stop)() || budget.is_some_and(|b| n >= b.entries || started.elapsed() >= b.time)
}

/// The mount table: `getfsstat(2)` with `MNT_NOWAIT` (a network mount is not asked).
#[cfg(target_os = "macos")]
fn mount_table() -> io::Result<Vec<Mount>> {
    let cstr = |a: &[libc::c_char]| {
        // SAFETY: the kernel NUL-terminates these fixed-size fields.
        unsafe { std::ffi::CStr::from_ptr(a.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    // SAFETY: a null buffer asks for the count only.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // Room for a few mounts more than counted (one may be added meanwhile).
    let cap = n as usize + 8;
    // SAFETY: an all-zero statfs is a valid value to be overwritten.
    let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; cap];
    let bytes = (cap * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: `buf` holds `cap` statfs, `bytes` long.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf[..got as usize]
        .iter()
        .map(|s| Mount {
            at: cstr(&s.f_mntonname),
            fstype: cstr(&s.f_fstypename),
            nobrowse: s.f_flags & libc::MNT_DONTBROWSE as u32 != 0,
        })
        .collect())
}

/// The mount table: `/proc/self/mountinfo`.
#[cfg(not(target_os = "macos"))]
fn mount_table() -> io::Result<Vec<Mount>> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")?;
    Ok(crate::remote::treefs::parse_mountinfo(&text))
}

/// One folder by `getattrlistbulk(2)`: names, types, sizes, link counts, devices, inodes, flags
/// and each subfolder's mount status in batches, without a call per name.
#[cfg(target_os = "macos")]
fn list_dir(path: &str, budget: Option<Budget>, live: &Live) -> io::Result<Listing> {
    use crate::remote::treefs::Mnt;
    const RETURNED: u32 = 0x8000_0000;
    const ERROR: u32 = 0x2000_0000;
    const NAME: u32 = 0x1;
    const DEVID: u32 = 0x2;
    const OBJTYPE: u32 = 0x8;
    const FLAGS: u32 = 0x0004_0000;
    const FILEID: u32 = 0x0200_0000;
    const DIR_MOUNTSTATUS: u32 = 0x4;
    const FILE_LINKCOUNT: u32 = 0x1;
    const FILE_DATALENGTH: u32 = 0x200;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;
    const MNTPOINT: u32 = 0x1;
    const TRIGGER: u32 = 0x2;
    let u32_at = |b: &[u8], p: usize| u32::from_ne_bytes(b[p..p + 4].try_into().unwrap());
    let u64_at = |b: &[u8], p: usize| u64::from_ne_bytes(b[p..p + 8].try_into().unwrap());

    let c = cpath(path)?;
    // SAFETY: a NUL-terminated path; the descriptor is closed by `Fd` on every way out.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = Fd(fd);
    // SAFETY: an all-zero attrlist is valid; its fields are set below.
    let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = RETURNED | ERROR | NAME | DEVID | OBJTYPE | FLAGS | FILEID;
    al.dirattr = DIR_MOUNTSTATUS;
    al.fileattr = FILE_LINKCOUNT | FILE_DATALENGTH;
    let mut buf = vec![0u8; 256 * 1024];
    let mut listing = Listing {
        complete: true,
        ..Listing::default()
    };
    let started = Instant::now();
    loop {
        if spent(budget, listing.children.len(), started, live) {
            listing.complete = false;
            break;
        }
        // SAFETY: `fd` is an open directory, `al` a valid attrlist, `buf` writable for its length.
        let n = unsafe {
            libc::getattrlistbulk(
                fd.0,
                &mut al as *mut libc::attrlist as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        (live.beat)();
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            break;
        }
        let mut off = 0usize;
        for _ in 0..n {
            let base = off;
            let len = u32_at(&buf, base) as usize;
            off = base + len;
            let mut p = base + 4;
            let common = u32_at(&buf, p);
            let dir = u32_at(&buf, p + 8);
            let file = u32_at(&buf, p + 12);
            p += 20;
            let mut err = 0;
            if common & ERROR != 0 {
                err = u32_at(&buf, p);
                p += 4;
            }
            let mut name = (String::new(), false);
            if common & NAME != 0 {
                let rel = i32::from_ne_bytes(buf[p..p + 4].try_into().unwrap()) as isize;
                let rlen = u32_at(&buf, p + 4) as usize;
                let start = (p as isize + rel) as usize;
                let raw = &buf[start..start + rlen.saturating_sub(1)];
                name = match std::str::from_utf8(raw) {
                    Ok(s) => (s.to_string(), false),
                    Err(_) => (String::from_utf8_lossy(raw).into_owned(), true),
                };
                p += 8;
            }
            let mut dev = 0u64;
            if common & DEVID != 0 {
                dev = u64::from(u32_at(&buf, p));
                p += 4;
            }
            let mut objtype = 0;
            if common & OBJTYPE != 0 {
                objtype = u32_at(&buf, p);
                p += 4;
            }
            let mut flags = 0;
            if common & FLAGS != 0 {
                flags = u32_at(&buf, p);
                p += 4;
            }
            let mut ino = 0;
            if common & FILEID != 0 {
                ino = u64_at(&buf, p);
                p += 8;
            }
            let mut mnt = Mnt::No;
            if dir & DIR_MOUNTSTATUS != 0 {
                let status = u32_at(&buf, p);
                mnt = if status & TRIGGER != 0 {
                    Mnt::Trigger
                } else if status & MNTPOINT != 0 {
                    Mnt::Point
                } else {
                    Mnt::No
                };
                p += 4;
            }
            let mut nlink = 1;
            if file & FILE_LINKCOUNT != 0 {
                nlink = u32_at(&buf, p);
                p += 4;
            }
            let mut size = 0;
            if file & FILE_DATALENGTH != 0 {
                size = u64_at(&buf, p);
            }
            let kind = match (err, objtype) {
                (0, VDIR) => Kind::Dir,
                (0, VREG) => Kind::File,
                (0, VLNK) => Kind::Link(Target::Other),
                _ => Kind::Other,
            };
            listing.children.push(Child {
                name: name.0,
                lossy: name.1,
                kind,
                size: if kind == Kind::File { size } else { 0 },
                id: Some((dev, ino)),
                nlink,
                hidden: flags & libc::UF_HIDDEN != 0,
                mnt: if kind == Kind::Dir { mnt } else { Mnt::No },
            });
        }
    }
    drop(fd);
    Ok(listing)
}

/// One folder by `getdents64` (`readdir`) and one `statx` per name, relative to the folder's
/// descriptor, with `AT_STATX_DONT_SYNC` and only the fields the tree reads.
#[cfg(not(target_os = "macos"))]
fn list_dir(path: &str, budget: Option<Budget>, live: &Live) -> io::Result<Listing> {
    use crate::remote::treefs::Mnt;
    use std::os::unix::ffi::OsStrExt as _;
    let c = cpath(path)?;
    // SAFETY: a NUL-terminated path; the stream is closed by `Dir` on every way out.
    let dir = unsafe { libc::opendir(c.as_ptr()) };
    if dir.is_null() {
        return Err(io::Error::last_os_error());
    }
    let dir = Dir(dir);
    // SAFETY: `dir.0` is an open directory stream.
    let fd = unsafe { libc::dirfd(dir.0) };
    let mut listing = Listing {
        complete: true,
        ..Listing::default()
    };
    let started = Instant::now();
    loop {
        if listing.children.len().is_multiple_of(256) {
            (live.beat)();
        }
        if spent(budget, listing.children.len(), started, live) {
            listing.complete = false;
            break;
        }
        // SAFETY: errno is cleared so that an end and an error can be told apart.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: `dir.0` is an open directory stream.
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            let errno = io::Error::last_os_error();
            if errno.raw_os_error().unwrap_or(0) != 0 {
                return Err(errno);
            }
            break;
        }
        // SAFETY: `entry` points at a dirent the stream keeps until the next readdir.
        let raw = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = raw.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = std::ffi::OsStr::from_bytes(bytes);
        let (shown, lossy) = match name.to_str() {
            Some(s) => (s.to_string(), false),
            None => (name.to_string_lossy().into_owned(), true),
        };
        // SAFETY: an all-zero statx is a valid value to be overwritten.
        let mut st: libc::statx = unsafe { std::mem::zeroed() };
        let mask = libc::STATX_TYPE | libc::STATX_SIZE | libc::STATX_INO | libc::STATX_NLINK;
        // SAFETY: `fd` is the open folder, `raw` a NUL-terminated name in it, `st` writable.
        let r = unsafe {
            libc::statx(
                fd,
                raw.as_ptr(),
                // AT_NO_AUTOMOUNT: a child that is an automount point is not mounted by this.
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_STATX_DONT_SYNC | libc::AT_NO_AUTOMOUNT,
                mask,
                &mut st,
            )
        };
        let mode = u32::from(st.stx_mode) & libc::S_IFMT;
        let kind = match (r, mode) {
            (0, libc::S_IFDIR) => Kind::Dir,
            (0, libc::S_IFREG) => Kind::File,
            (0, libc::S_IFLNK) => Kind::Link(Target::Other),
            _ => Kind::Other,
        };
        let attr = |bit: libc::c_int| {
            st.stx_attributes_mask & bit as u64 != 0 && st.stx_attributes & bit as u64 != 0
        };
        let mnt = if kind != Kind::Dir {
            Mnt::No
        } else if attr(libc::STATX_ATTR_AUTOMOUNT) {
            Mnt::Trigger
        } else if attr(libc::STATX_ATTR_MOUNT_ROOT) {
            Mnt::Point
        } else {
            Mnt::No
        };
        let dev = libc::makedev(st.stx_dev_major, st.stx_dev_minor);
        listing.children.push(Child {
            name: shown,
            lossy,
            kind,
            size: if kind == Kind::File { st.stx_size } else { 0 },
            id: (r == 0).then_some((dev as u64, st.stx_ino)),
            nlink: st.stx_nlink,
            hidden: false,
            mnt,
        });
    }
    drop(dir);
    Ok(listing)
}

/// An open descriptor, closed when dropped.
#[cfg(target_os = "macos")]
struct Fd(libc::c_int);

#[cfg(target_os = "macos")]
impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: the descriptor is ours and closed once.
        unsafe { libc::close(self.0) };
    }
}

/// An open directory stream, closed when dropped.
#[cfg(not(target_os = "macos"))]
struct Dir(*mut libc::DIR);

#[cfg(not(target_os = "macos"))]
impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: the stream is ours and closed once.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A filesystem in memory for a row's listing's calls, every call recorded.
    #[derive(Default)]
    struct Memory {
        entries: HashMap<String, (Seen, String)>,
        calls: Vec<String>,
    }

    impl Memory {
        fn dir(mut self, path: &str) -> Self {
            self.entries.insert(path.into(), (Seen::Dir, String::new()));
            self
        }

        fn link(mut self, path: &str, to: &str) -> Self {
            self.entries.insert(path.into(), (Seen::Link, to.into()));
            self
        }
    }

    impl LinkOps for Memory {
        fn lstat(&mut self, path: &str) -> io::Result<Seen> {
            self.calls.push(path.to_string());
            self.entries
                .get(path)
                .map(|e| e.0)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn readlink(&mut self, path: &str) -> io::Result<String> {
            self.calls.push(path.to_string());
            match self.entries.get(path) {
                Some((Seen::Link, to)) => Ok(to.clone()),
                _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
            }
        }

        fn head(&mut self, path: &str) -> io::Result<Vec<u8>> {
            self.calls.push(path.to_string());
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }

    fn links(names: &[&str]) -> Listing {
        Listing {
            children: names
                .iter()
                .map(|n| Child::plain(n, Kind::Link(Target::Other), 0))
                .collect(),
            complete: true,
            cachedir: false,
        }
    }

    fn kind(listing: &Listing, name: &str) -> Kind {
        listing
            .children
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .kind
    }

    fn live() -> (impl Fn(), impl Fn() -> bool) {
        (|| {}, || false)
    }

    /// Item 3: a link's target is resolved one component at a time, never at or below a mount the
    /// rules forbid (an autofs trigger): directly, through another link, or by `..`.
    #[test]
    fn a_link_is_never_followed_into_a_guarded_mount() {
        let mut fs = Memory::default()
            .dir("/h")
            .dir("/auto")
            .dir("/data")
            .dir("/data/d")
            .link("/h/direct", "/auto/share/x")
            .link("/h/hop", "/h/next")
            .link("/h/next", "/auto/y")
            .link("/h/up", "../auto/z")
            .link("/h/deep", "../../../auto/z")
            .link("/h/abs", "/data/../../auto/z")
            .link("/h/ok", "/data/d");
        let mut listing = links(&["direct", "hop", "up", "deep", "abs", "ok"]);
        let (beat, stop) = live();
        let guard = vec!["/auto".to_string()];
        enrich(
            "/h",
            &mut listing,
            &guard,
            None,
            &Live {
                beat: &beat,
                stop: &stop,
            },
            &mut fs,
        );
        // `..` above `/` stays at `/` (never the working directory's).
        for name in ["direct", "hop", "up", "deep", "abs"] {
            assert_eq!(kind(&listing, name), Kind::Link(Target::Unread), "{name}");
        }
        assert!(
            fs.calls.iter().all(|c| c.starts_with('/')),
            "{:?}",
            fs.calls
        );
        assert_eq!(kind(&listing, "ok"), Kind::Link(Target::Dir));
        let touched: Vec<&String> = fs
            .calls
            .iter()
            .filter(|c| *c == "/auto" || c.starts_with("/auto/"))
            .collect();
        assert!(
            touched.is_empty(),
            "calls at or below the guard: {touched:?}"
        );
    }

    /// Item 8: past its deadline (or once stopped) a row's listing makes no more calls: the links
    /// not resolved stay non-rows, and the listing says it is incomplete.
    #[test]
    fn a_rows_listing_past_its_deadline_makes_no_more_calls() {
        let mut fs = Memory::default()
            .dir("/data")
            .link("/h/a", "/data")
            .link("/h/b", "/data");
        let mut listing = links(&["a", "b", "CACHEDIR.TAG"]);
        listing.children[2].kind = Kind::File;
        let (beat, stop) = live();
        enrich(
            "/h",
            &mut listing,
            &[],
            Some(Instant::now() - Duration::from_millis(1)),
            &Live {
                beat: &beat,
                stop: &stop,
            },
            &mut fs,
        );
        assert!(fs.calls.is_empty(), "{:?}", fs.calls);
        assert!(!listing.complete);
        assert_eq!(kind(&listing, "a"), Kind::Link(Target::Other));
        let stopped = || true;
        let mut fs = Memory::default().dir("/data").link("/h/a", "/data");
        let mut listing = links(&["a"]);
        enrich(
            "/h",
            &mut listing,
            &[],
            None,
            &Live {
                beat: &beat,
                stop: &stopped,
            },
            &mut fs,
        );
        assert!(fs.calls.is_empty() && !listing.complete);
    }

    /// A disk that answers at once.
    struct Instant0;

    impl Disk for Instant0 {
        fn list(
            &self,
            _: &str,
            _: Option<Budget>,
            _: bool,
            _: &[String],
            _: &Live,
        ) -> io::Result<Listing> {
            Ok(Listing::default())
        }
        fn resolve(&self, path: &str) -> io::Result<Resolved> {
            Ok(Resolved {
                path: path.into(),
                ids: Vec::new(),
            })
        }
        fn home(&self) -> io::Result<Resolved> {
            self.resolve("/")
        }
        fn mounts(&self) -> io::Result<Mounts> {
            Err(io::Error::other("none"))
        }
        fn statvfs(&self, _: &str) -> io::Result<FsStat> {
            Ok(FsStat {
                used_bytes: 0,
                used_files: 0,
                fsid: 0,
            })
        }
    }

    /// Item 12: a request the worker tracks is let go once it has ended — cancelled or not.
    #[test]
    fn cancelled_requests_are_not_kept() {
        let pool = Pool::new(crate::tree::watchdog::Timing::default());
        let mut tree = LocalTree::new(Arc::new(Instant0), pool.clone());
        for id in 0..300 {
            tree.send(id, 0, Ask::Statvfs("/".into()));
            if id % 2 == 0 {
                tree.cancel(id);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while (pool.outstanding().0 > 0 || tree.tracked() > 0) && Instant::now() < deadline {
            while tree.next(Duration::from_millis(5)).is_some() {}
        }
        assert_eq!(tree.tracked(), 0);
        pool.shutdown();
    }
}
