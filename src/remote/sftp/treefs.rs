//! [`TreeFs`] over SFTP (05 §6.4, §7): the tree's own connection, driven by a worker thread that
//! owns the session's runtime, clones the cheap `Fs` handle for each request and keeps up to
//! [`IN_FLIGHT`] requests in flight instead of one `block_on` per call; each answer is sent as
//! it arrives, a request with no answer in 2 s is reported `not answering` (and keeps waiting up
//! to the request timeout), and a cancelled request's future is dropped — a listing's handle is
//! still closed, by the client's own drop. `expand-path@openssh.com`, `statvfs@openssh.com` and
//! `/proc/self/mountinfo` are read over SFTP; nothing is run on the remote.
//!
//! `limits@openssh.com`'s `max-open-handles` would lower the 16: `openssh-sftp-client` 0.15.12
//! reads the limits at its start but keeps them private, so the 16 stand.

use super::{read_at, SftpRemote, META_TIMEOUT};
use crate::remote::treefs::{
    is_cachedir_tag, scan_step, Ask, Budget, Child, FsStat, Got, Kind, Listing, Mounts, Reply,
    Resolved, ScanAsk, ScanBudget, ScanEnd, Scanned, Target, Totals, TreeFs, IN_FLIGHT,
    NOT_ANSWERING,
};
use crate::remote::Wake;
use openssh_sftp_client::fs::Fs;
use openssh_sftp_client::Sftp;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as queue;

/// The worker's clocks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// A request with no answer this long is reported `not answering`.
    pub not_answering: Duration,
    /// A request (or one READDIR reply) with no answer this long fails (01 §5's 30 s).
    pub request: Duration,
    /// A scan reports its progress at most this often.
    pub progress: Duration,
    /// How long closing the session may wait for the server.
    pub close: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            not_answering: NOT_ANSWERING,
            request: META_TIMEOUT,
            progress: Duration::from_millis(100),
            close: Duration::from_secs(5),
        }
    }
}

impl SftpRemote {
    /// The tree's worker on this connection (05 §6.4): the session and its runtime move to a
    /// thread of the worker's own; the connection is the tree's from here on, and closing the tree
    /// closes it.
    pub fn into_tree(self: Box<Self>) -> Box<dyn TreeFs> {
        self.into_tree_with(Timing::default())
    }

    pub(crate) fn into_tree_with(mut self: Box<Self>, timing: Timing) -> Box<dyn TreeFs> {
        let (cmds, cmd_rx) = queue::unbounded_channel();
        let (tx, rx) = mpsc::channel();
        let wake: Arc<Mutex<Option<Wake>>> = Arc::default();
        let rt = self.rt.take().expect("open session");
        let sftp = self.sftp.take().expect("open session");
        let fs = self.fs.take().expect("open session");
        // `self` keeps nothing from here: its drop closes nothing.
        drop(self);
        let woken = wake.clone();
        let spawned = std::thread::Builder::new()
            .name("sopen-tree-sftp".into())
            .spawn(move || {
                let out = Out { tx, wake: woken };
                rt.block_on(async move {
                    let mut worker = Worker::new(sftp, fs, out, timing);
                    let mut cmd_rx = cmd_rx;
                    std::future::poll_fn(|cx| worker.poll(cx, &mut cmd_rx)).await;
                    worker.flights.clear();
                    worker.scans.clear();
                    let Worker { sftp, .. } = worker;
                    let _ = tokio::time::timeout(timing.close, sftp.close()).await;
                });
            });
        if spawned.is_err() {
            // The worker could not start: every request will go unanswered; the tree says
            // `not answering` for each after 2 s.
            crate::debug!("cannot start the tree's sftp worker");
        }
        Box::new(SftpTree {
            cmds,
            rx,
            wake,
            closed: false,
        })
    }
}

enum Cmd {
    Send(u64, u64, Ask),
    Cancel(u64),
    Close,
}

struct SftpTree {
    cmds: queue::UnboundedSender<Cmd>,
    rx: mpsc::Receiver<Reply>,
    wake: Arc<Mutex<Option<Wake>>>,
    closed: bool,
}

impl TreeFs for SftpTree {
    fn send(&mut self, id: u64, prio: u64, ask: Ask) {
        let _ = self.cmds.send(Cmd::Send(id, prio, ask));
    }

    fn cancel(&mut self, id: u64) {
        let _ = self.cmds.send(Cmd::Cancel(id));
    }

    fn next(&mut self, wait: Duration) -> Option<Reply> {
        if wait.is_zero() {
            self.rx.try_recv().ok()
        } else {
            self.rx.recv_timeout(wait).ok()
        }
    }

    fn set_wake(&mut self, wake: Wake) {
        *self.wake.lock().unwrap_or_else(|p| p.into_inner()) = Some(wake);
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            let _ = self.cmds.send(Cmd::Close);
        }
    }

    fn held(&self) -> Option<String> {
        None
    }

    fn is_local(&self) -> bool {
        false
    }
}

impl Drop for SftpTree {
    fn drop(&mut self) {
        self.close();
    }
}

struct Out {
    tx: mpsc::Sender<Reply>,
    wake: Arc<Mutex<Option<Wake>>>,
}

impl Out {
    fn send(&self, id: u64, got: Got) {
        let _ = self.tx.send(Reply { id, got });
        let wake = self.wake.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(w) = wake {
            w.wake();
        }
    }
}

type Fut<T> = Pin<Box<dyn Future<Output = T>>>;

/// What a request in flight is for.
enum Flight {
    /// An answer to a request.
    Ask {
        id: u64,
        since: Instant,
        warned: bool,
        fut: Fut<Got>,
    },
    /// One folder of a scan: its listing, or `None` when its fence keeps it out.
    Scan {
        id: u64,
        dir: String,
        fut: Fut<Result<Option<Listing>, String>>,
    },
}

impl Flight {
    fn id(&self) -> u64 {
        match self {
            Flight::Ask { id, .. } | Flight::Scan { id, .. } => *id,
        }
    }
}

struct RScan {
    prio: u64,
    seq: u64,
    queue: VecDeque<String>,
    flying: usize,
    totals: Totals,
    started: Instant,
    reported: Instant,
    budget: Option<ScanBudget>,
    skip: HashSet<String>,
    seen: HashSet<(u64, u64)>,
    fence: Option<Vec<u64>>,
}

struct Worker {
    sftp: Sftp,
    fs: Fs,
    out: Out,
    timing: Timing,
    /// Requests waiting: priority, arrival, id, the request.
    waiting: Vec<(u64, u64, u64, Ask)>,
    seq: u64,
    flights: Vec<Flight>,
    scans: HashMap<u64, RScan>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Worker {
    fn new(sftp: Sftp, fs: Fs, out: Out, timing: Timing) -> Self {
        Self {
            sftp,
            fs,
            out,
            timing,
            waiting: Vec::new(),
            seq: 0,
            flights: Vec::new(),
            scans: HashMap::new(),
            timer: None,
        }
    }

    /// Drives everything; ready once the tree closed (or went away).
    fn poll(&mut self, cx: &mut Context<'_>, cmds: &mut queue::UnboundedReceiver<Cmd>) -> Poll<()> {
        loop {
            loop {
                match cmds.poll_recv(cx) {
                    Poll::Ready(Some(Cmd::Send(id, prio, ask))) => {
                        self.seq += 1;
                        self.waiting.push((prio, self.seq, id, ask));
                    }
                    Poll::Ready(Some(Cmd::Cancel(id))) => self.cancel(id),
                    Poll::Ready(Some(Cmd::Close)) | Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => break,
                }
            }
            self.start();
            let mut done = Vec::new();
            let mut i = 0;
            while i < self.flights.len() {
                let ready = match &mut self.flights[i] {
                    Flight::Ask { fut, .. } => fut.as_mut().poll(cx).map(Done::Ask),
                    Flight::Scan { fut, .. } => fut.as_mut().poll(cx).map(Done::Scan),
                };
                match ready {
                    Poll::Ready(d) => done.push((self.flights.swap_remove(i), d)),
                    Poll::Pending => i += 1,
                }
            }
            if done.is_empty() {
                break;
            }
            // Every answer that came is counted before a scan is ended: one whose budget ran
            // out keeps the folders still out, not the ones just listed.
            let mut moved = HashSet::new();
            for (flight, d) in done {
                if let Flight::Scan { id, .. } = &flight {
                    moved.insert(*id);
                }
                self.finish(flight, d);
            }
            for id in moved {
                self.scan_moved(id);
            }
        }
        self.warn(cx);
        Poll::Pending
    }

    fn cancel(&mut self, id: u64) {
        self.waiting.retain(|w| w.2 != id);
        // A dropped listing's handle is closed by the client's own drop, on this runtime.
        self.flights.retain(|f| f.id() != id);
        self.scans.remove(&id);
    }

    /// Starts what waits, nearest first, while fewer than [`IN_FLIGHT`] are out: requests and
    /// the scans' next folders by one order.
    fn start(&mut self) {
        while self.flights.len() < IN_FLIGHT {
            let ask = self
                .waiting
                .iter()
                .enumerate()
                .min_by_key(|(_, w)| (w.0, w.1))
                .map(|(i, w)| (w.0, w.1, i));
            let scan = self
                .scans
                .iter()
                .filter(|(_, s)| !s.queue.is_empty() && !s.totals.over(s.budget))
                .min_by_key(|(_, s)| (s.prio, s.seq))
                .map(|(id, s)| (s.prio, s.seq, *id));
            match (ask, scan) {
                (Some(a), Some(s)) if (s.0, s.1) < (a.0, a.1) => self.start_folder(s.2),
                (Some((_, _, i)), _) => {
                    let (prio, seq, id, ask) = self.waiting.swap_remove(i);
                    self.start_ask(id, prio, seq, ask);
                }
                (None, Some(s)) => self.start_folder(s.2),
                (None, None) => break,
            }
        }
    }

    fn start_ask(&mut self, id: u64, prio: u64, seq: u64, ask: Ask) {
        let fs = self.fs.clone();
        let request = self.timing.request;
        let fut: Fut<Got> = match ask {
            Ask::Resolve(path) => Box::pin(async move {
                Got::Resolved(realpath(fs, &path, request).await.map(|path| Resolved {
                    path,
                    ids: Vec::new(),
                }))
            }),
            Ask::Home(user) => {
                let expand = self.sftp.support_expand_path();
                Box::pin(async move { Got::Home(home(fs, user, expand, request).await) })
            }
            Ask::Mounts => {
                let (opts, statvfs) = (self.sftp.options(), self.sftp.support_statvfs());
                Box::pin(async move { Got::Mounts(mounts(fs, opts, statvfs, request).await) })
            }
            Ask::List { path, budget } => {
                let opts = self.sftp.options();
                Box::pin(
                    async move { Got::Listed(list(fs, Some(opts), &path, budget, request).await) },
                )
            }
            Ask::Statvfs(path) => {
                let offered = self.sftp.support_statvfs();
                Box::pin(async move { Got::Statvfs(statvfs(fs, &path, offered, request).await) })
            }
            Ask::Scan(scan) => {
                self.start_scan(id, prio, seq, scan);
                return;
            }
        };
        self.flights.push(Flight::Ask {
            id,
            since: Instant::now(),
            warned: false,
            fut,
        });
    }

    fn start_scan(&mut self, id: u64, prio: u64, seq: u64, ask: ScanAsk) {
        let now = Instant::now();
        let fence = ask.fence.filter(|_| self.sftp.support_statvfs());
        self.scans.insert(
            id,
            RScan {
                prio,
                seq,
                queue: ask.start.into_iter().collect(),
                flying: 0,
                totals: ask.base,
                started: now - ask.base.time,
                reported: now,
                budget: ask.budget,
                skip: ask.skip.into_iter().collect(),
                seen: ask.seen.into_iter().collect(),
                fence,
            },
        );
        // A scan with nothing to list ends at once.
        self.scan_moved(id);
    }

    fn start_folder(&mut self, id: u64) {
        let Some(scan) = self.scans.get_mut(&id) else {
            return;
        };
        let Some(dir) = scan.queue.pop_front() else {
            return;
        };
        scan.flying += 1;
        let (mut fs, request, fence) = (self.fs.clone(), self.timing.request, scan.fence.clone());
        let path = dir.clone();
        let fut: Fut<Result<Option<Listing>, String>> = Box::pin(async move {
            if let Some(allowed) = fence {
                // 05 §4.1: a folder on another filesystem is a mount point, not entered.
                if let Ok(Ok(st)) = tokio::time::timeout(request, fs.statvfs(&path)).await {
                    if !allowed.contains(&st.fsid) {
                        return Ok(None);
                    }
                }
            }
            list(fs, None, &path, None, request).await.map(Some)
        });
        self.flights.push(Flight::Scan { id, dir, fut });
    }

    fn finish(&mut self, flight: Flight, done: Done) {
        match (flight, done) {
            (Flight::Ask { id, .. }, Done::Ask(got)) => self.out.send(id, got),
            (Flight::Scan { id, dir, .. }, Done::Scan(got)) => {
                let Some(scan) = self.scans.get_mut(&id) else {
                    return;
                };
                scan.flying -= 1;
                match got {
                    Ok(Some(listing)) => {
                        let RScan {
                            skip,
                            seen,
                            totals,
                            queue,
                            ..
                        } = scan;
                        queue.extend(scan_step(&dir, &listing, skip, seen, totals));
                    }
                    Ok(None) => {}
                    Err(_) => scan.totals.unread += 1,
                }
                scan.totals.time = scan.started.elapsed();
                if scan.reported.elapsed() >= self.timing.progress {
                    scan.reported = Instant::now();
                    let totals = scan.totals;
                    self.out.send(id, Got::Scan(Scanned { totals, end: None }));
                }
            }
            _ => {}
        }
    }

    /// Ends a scan that has nothing more to do: everything listed, or its budget spent (its
    /// folders in flight dropped, and kept for a resume).
    fn scan_moved(&mut self, id: u64) {
        let Some(scan) = self.scans.get_mut(&id) else {
            return;
        };
        scan.totals.time = scan.started.elapsed();
        let end = if scan.totals.over(scan.budget) {
            let mut left: Vec<String> = Vec::new();
            for f in &self.flights {
                if let Flight::Scan { id: of, dir, .. } = f {
                    if *of == id {
                        left.push(dir.clone());
                    }
                }
            }
            left.extend(scan.queue.drain(..));
            Some(ScanEnd::Budget(left))
        } else if scan.queue.is_empty() && scan.flying == 0 {
            Some(ScanEnd::Complete)
        } else {
            None
        };
        if let Some(end) = end {
            let totals = scan.totals;
            self.scans.remove(&id);
            self.flights.retain(|f| f.id() != id);
            self.out.send(
                id,
                Got::Scan(Scanned {
                    totals,
                    end: Some(end),
                }),
            );
        }
    }

    /// Reports each request still unanswered after [`Timing::not_answering`] once, and sets the
    /// timer for the next.
    fn warn(&mut self, cx: &mut Context<'_>) {
        let limit = self.timing.not_answering;
        let mut next: Option<Instant> = None;
        let mut late = Vec::new();
        for f in &mut self.flights {
            if let Flight::Ask {
                id, since, warned, ..
            } = f
            {
                if *warned {
                    continue;
                }
                let due = *since + limit;
                if Instant::now() >= due {
                    *warned = true;
                    late.push(*id);
                } else {
                    next = Some(next.map_or(due, |n| n.min(due)));
                }
            }
        }
        for id in late {
            self.out.send(id, Got::NotAnswering);
        }
        self.timer = next.map(|at| Box::pin(tokio::time::sleep_until(at.into())));
        if let Some(t) = &mut self.timer {
            if t.as_mut().poll(cx).is_ready() {
                cx.waker().wake_by_ref();
            }
        }
    }
}

enum Done {
    Ask(Got),
    Scan(Result<Option<Listing>, String>),
}

async fn within<T, E: std::fmt::Display>(
    limit: Duration,
    what: &str,
    fut: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("cannot {what}: {e}")),
        Err(_) => Err(format!("{what}: no reply in {}s", limit.as_secs())),
    }
}

fn text(p: std::path::PathBuf, of: &str) -> Result<String, String> {
    p.into_os_string()
        .into_string()
        .map_err(|_| format!("{of}: path is not valid UTF-8"))
}

async fn realpath(mut fs: Fs, path: &str, request: Duration) -> Result<String, String> {
    let got = within(request, &format!("resolve {path}"), fs.canonicalize(path)).await?;
    text(got, path)
}

/// `expand-path@openssh.com` of `~<user>` — the account's home even when `sftp-server -d` moved
/// the start folder — else `realpath(".")`.
async fn home(
    fs: Fs,
    user: Option<String>,
    expand: bool,
    request: Duration,
) -> Result<Resolved, String> {
    if let (Some(user), true) = (user, expand) {
        if let Ok(path) = realpath(fs.clone(), &format!("~{user}"), request).await {
            return Ok(Resolved {
                path,
                ids: Vec::new(),
            });
        }
    }
    let path = realpath(fs, ".", request).await?;
    Ok(Resolved {
        path,
        ids: Vec::new(),
    })
}

/// A Linux host's `/proc/self/mountinfo`, read to its end (its size reads 0); else whether it
/// is a macOS host (05 §4.1).
async fn mounts(
    mut fs: Fs,
    opts: openssh_sftp_client::file::OpenOptions,
    statvfs: bool,
    request: Duration,
) -> Mounts {
    if let Ok(bytes) = read_all(opts, "/proc/self/mountinfo", 1 << 20, request).await {
        let table = crate::remote::treefs::parse_mountinfo(&String::from_utf8_lossy(&bytes));
        if !table.is_empty() {
            return Mounts {
                table: Some(table),
                macos: false,
                statvfs,
            };
        }
    }
    let tmp = realpath(fs.clone(), "/tmp", request).await;
    let data = within(
        request,
        "stat /System/Volumes/Data",
        fs.symlink_metadata("/System/Volumes/Data"),
    )
    .await;
    Mounts {
        table: None,
        macos: tmp.as_deref() == Ok("/private/tmp") && data.is_ok(),
        statvfs,
    }
}

/// Up to `max` bytes of `path`, each read at its own offset (a short reply is followed by a
/// request for the rest; the crate's cursor moves by the length asked).
async fn read_all(
    opts: openssh_sftp_client::file::OpenOptions,
    path: &str,
    max: u64,
    request: Duration,
) -> Result<Vec<u8>, String> {
    let mut opts = opts;
    let file = within(request, &format!("open {path}"), opts.read(true).open(path)).await?;
    let mut out = Vec::new();
    let result = async {
        while (out.len() as u64) < max {
            let want = (max - out.len() as u64).min(32 * 1024) as u32;
            match within(
                request,
                &format!("read {path}"),
                read_at(&file, out.len() as u64, want),
            )
            .await?
            {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => break,
            }
        }
        Ok::<(), String>(())
    }
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(5), file.close()).await;
    result.map(|()| out)
}

async fn statvfs(
    mut fs: Fs,
    path: &str,
    offered: bool,
    request: Duration,
) -> Result<FsStat, String> {
    if !offered {
        return Err("the server does not offer statvfs@openssh.com".into());
    }
    let st = within(request, &format!("statvfs {path}"), fs.statvfs(path)).await?;
    Ok(FsStat {
        used_bytes: st.blocks.saturating_sub(st.bfree).saturating_mul(st.frsize),
        used_files: st.files.saturating_sub(st.ffree),
        fsid: st.fsid,
    })
}

/// One folder: OPENDIR, READDIR … (each reply within the request timeout), CLOSE — by the
/// client's drop of the stream, also when it stops early. A row's listing (`opts` given) adds a
/// `stat` per link and the `CACHEDIR.TAG` read; `budget` stops it between replies (once one has
/// come — before that it waits, `not answering` after 2 s, up to the request timeout).
async fn list(
    mut fs: Fs,
    opts: Option<openssh_sftp_client::file::OpenOptions>,
    path: &str,
    budget: Option<Budget>,
    request: Duration,
) -> Result<Listing, String> {
    let dir = within(request, &format!("list {path}"), fs.open_dir(path)).await?;
    let started = Instant::now();
    let mut listing = Listing {
        complete: true,
        ..Listing::default()
    };
    {
        let mut stream = std::pin::pin!(dir.read_dir());
        loop {
            let wait = match budget {
                Some(b) if !listing.children.is_empty() => {
                    let left = b.time.saturating_sub(started.elapsed());
                    if left.is_zero() || listing.children.len() >= b.entries {
                        listing.complete = false;
                        break;
                    }
                    left.min(request)
                }
                _ => request,
            };
            let next =
                std::future::poll_fn(|cx| futures_core::Stream::poll_next(stream.as_mut(), cx));
            match tokio::time::timeout(wait, next).await {
                Err(_) if wait < request => {
                    listing.complete = false;
                    break;
                }
                Err(_) => return Err(format!("list {path}: no reply in {}s", request.as_secs())),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(format!("cannot list {path}: {e}")),
                Ok(Some(Ok(entry))) => {
                    let name = entry.filename().as_os_str();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let (shown, lossy) = match name.to_str() {
                        Some(s) => (s.to_string(), false),
                        None => (name.to_string_lossy().into_owned(), true),
                    };
                    let meta = entry.metadata();
                    let kind = match meta.file_type() {
                        Some(t) if t.is_dir() => Kind::Dir,
                        Some(t) if t.is_file() => Kind::File,
                        Some(t) if t.is_symlink() => Kind::Link(Target::Other),
                        _ => Kind::Other,
                    };
                    let size = if kind == Kind::File {
                        meta.len().unwrap_or(0)
                    } else {
                        0
                    };
                    let mut child = Child::plain(&shown, kind, size);
                    child.lossy = lossy;
                    listing.children.push(child);
                }
            }
        }
    }
    if let Some(opts) = opts {
        for child in &mut listing.children {
            if child.kind == Kind::Link(Target::Other) && !child.lossy {
                let full = crate::remote::join(path, &child.name);
                child.kind = match within(request, "stat", fs.metadata(&full)).await {
                    Ok(m) if m.file_type().is_some_and(|t| t.is_dir()) => Kind::Link(Target::Dir),
                    Ok(m) if m.file_type().is_some_and(|t| t.is_file()) => Kind::Link(Target::File),
                    _ => Kind::Link(Target::Other),
                };
            }
        }
        if listing
            .children
            .iter()
            .any(|c| c.name == "CACHEDIR.TAG" && c.kind == Kind::File)
        {
            let tag = crate::remote::join(path, "CACHEDIR.TAG");
            listing.cachedir = read_all(opts, &tag, 43, request)
                .await
                .is_ok_and(|head| is_cachedir_tag(&head));
        }
    }
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::sftp::Proxy;
    use crate::remote::treefs::{ScanAsk, LIST_BUDGET, REMOTE_SCAN_BUDGET};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    /// `SSH_FXP_OPENDIR`.
    const OPENDIR: u8 = 11;

    fn p(path: &Path) -> String {
        path.to_str().unwrap().to_string()
    }

    fn temp() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(tmp.path()).unwrap();
        (tmp, real)
    }

    fn quick() -> Timing {
        Timing {
            not_answering: Duration::from_millis(200),
            progress: Duration::ZERO,
            ..Timing::default()
        }
    }

    /// The next answers until `id`'s last one (or `limit`), in order.
    fn wait_for(tree: &mut dyn TreeFs, id: u64, limit: Duration) -> Vec<Reply> {
        let deadline = Instant::now() + limit;
        let mut got = Vec::new();
        while Instant::now() < deadline {
            if let Some(r) = tree.next(Duration::from_millis(20)) {
                let last = r.id == id
                    && !matches!(
                        r.got,
                        Got::NotAnswering | Got::Scan(Scanned { end: None, .. })
                    );
                got.push(r);
                if last {
                    break;
                }
            }
        }
        got
    }

    fn listed(replies: &[Reply], id: u64) -> Listing {
        replies
            .iter()
            .find_map(|r| match (&r.got, r.id == id) {
                (Got::Listed(Ok(l)), true) => Some(l.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no listing for {id}: {replies:?}"))
    }

    fn requests(proxy: &Proxy, kind: u8) -> usize {
        proxy
            .wire
            .lock()
            .unwrap()
            .iter()
            .filter(|w| !w.reply && w.kind == kind)
            .count()
    }

    fn until(limit: Duration, done: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// A row's listing over SFTP: each entry's type from the READDIR reply, a `stat` per link
    /// for its target, the 43-byte `CACHEDIR.TAG` read; a budget stops it between replies.
    #[test]
    fn a_listing_reads_types_link_targets_and_the_cache_tag() {
        let Some(conn) = SftpRemote::local_server() else {
            return;
        };
        let (_tmp, dir) = temp();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("file"), b"12345").unwrap();
        std::os::unix::fs::symlink("sub", dir.join("to-sub")).unwrap();
        std::os::unix::fs::symlink("file", dir.join("to-file")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("broken")).unwrap();
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::List {
                path: p(&dir),
                budget: None,
            },
        );
        let listing = listed(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        assert!(listing.complete && listing.cachedir);
        let kind = |n: &str| {
            listing
                .children
                .iter()
                .find(|c| c.name == n)
                .map(|c| (c.kind, c.size))
                .unwrap()
        };
        assert_eq!(kind("sub").0, Kind::Dir);
        assert_eq!(kind("file"), (Kind::File, 5));
        assert_eq!(kind("to-sub").0, Kind::Link(Target::Dir));
        assert_eq!(kind("to-file").0, Kind::Link(Target::File));
        assert_eq!(kind("broken").0, Kind::Link(Target::Other));
        assert_eq!(listing.children.len(), 6);
        // A budget of two entries stops it, incomplete.
        tree.send(
            2,
            0,
            Ask::List {
                path: p(&dir),
                budget: Some(Budget {
                    entries: 2,
                    ..LIST_BUDGET
                }),
            },
        );
        let cut = listed(&wait_for(&mut *tree, 2, Duration::from_secs(10)), 2);
        assert_eq!((cut.children.len(), cut.complete), (2, false));
        tree.close();
    }

    /// The home is `expand-path@openssh.com` of `~<user>` — the account's home even when
    /// `sftp-server -d` moved the start folder — and without a user `realpath(".")`.
    #[test]
    fn the_home_is_the_accounts_even_when_the_start_folder_moved() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let conn = SftpRemote::local_server_args(&["-d", &p(&dir)], None).unwrap();
        let expand = conn.sftp.as_ref().unwrap().support_expand_path();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(1, 0, Ask::Home(None));
        let start = wait_for(&mut *tree, 1, Duration::from_secs(10));
        assert!(
            matches!(&start.last().unwrap().got, Got::Home(Ok(r)) if r.path == p(&dir)),
            "{start:?}"
        );
        let user = std::env::var("USER").ok().filter(|u| !u.is_empty());
        let home = std::env::var("HOME")
            .ok()
            .and_then(|h| std::fs::canonicalize(h).ok());
        match (user, home, expand) {
            (Some(user), Some(home), true) => {
                tree.send(2, 0, Ask::Home(Some(user)));
                let got = wait_for(&mut *tree, 2, Duration::from_secs(10));
                assert!(
                    matches!(&got.last().unwrap().got, Got::Home(Ok(r)) if r.path == p(&home)),
                    "{got:?}"
                );
            }
            _ => eprintln!("skipping ~<user>: no USER, no HOME, or no expand-path"),
        }
        tree.close();
    }

    /// A Linux server's mount table is its `/proc/self/mountinfo`; a macOS server has none and
    /// is recognised by `/tmp` → `/private/tmp` and `/System/Volumes/Data` (05 §4.1).
    #[test]
    fn mounts_are_the_linux_table_or_a_macos_guess() {
        let Some(conn) = SftpRemote::local_server() else {
            return;
        };
        let statvfs = conn.sftp.as_ref().unwrap().support_statvfs();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(1, 0, Ask::Mounts);
        let got = wait_for(&mut *tree, 1, Duration::from_secs(10));
        let Some(Got::Mounts(m)) = got.last().map(|r| &r.got) else {
            panic!("{got:?}");
        };
        assert_eq!(m.statvfs, statvfs);
        if cfg!(target_os = "linux") {
            let table = m.table.as_ref().expect("a Linux table");
            assert!(table.iter().any(|t| t.at == "/"), "{table:?}");
            assert!(!m.macos);
        } else if cfg!(target_os = "macos") {
            assert_eq!((m.table.is_none(), m.macos), (true, true));
        }
        if statvfs {
            tree.send(2, 0, Ask::Statvfs("/".into()));
            let got = wait_for(&mut *tree, 2, Duration::from_secs(10));
            assert!(
                matches!(&got.last().unwrap().got, Got::Statvfs(Ok(f)) if f.used_bytes > 0),
                "{got:?}"
            );
        }
        tree.close();
    }

    /// At most 16 requests in flight; one with no answer is reported `not answering` after the
    /// limit and still answered when the server answers.
    #[test]
    fn sixteen_in_flight_and_not_answering_after_the_limit() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        proxy.hold.store(true, Ordering::Relaxed);
        let mut tree = Box::new(conn).into_tree_with(quick());
        for id in 1..=20 {
            tree.send(
                id,
                id,
                Ask::List {
                    path: p(&dir),
                    budget: Some(LIST_BUDGET),
                },
            );
        }
        assert!(until(Duration::from_secs(5), || requests(&proxy, OPENDIR) >= 16));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(requests(&proxy, OPENDIR), 16, "16 in flight, the rest wait");
        let mut late = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while late.len() < 16 && Instant::now() < deadline {
            if let Some(Reply {
                id,
                got: Got::NotAnswering,
            }) = tree.next(Duration::from_millis(20))
            {
                late.insert(id);
            }
        }
        assert_eq!(late, (1..=16).collect::<HashSet<u64>>());
        proxy.hold.store(false, Ordering::Relaxed);
        let mut answered = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while answered.len() < 20 && Instant::now() < deadline {
            if let Some(Reply {
                id,
                got: Got::Listed(Ok(_)),
            }) = tree.next(Duration::from_millis(20))
            {
                answered.insert(id);
            }
        }
        assert_eq!(answered.len(), 20);
        tree.close();
    }

    /// A cancelled request's future is dropped: it is never answered, and the next one is.
    #[test]
    fn a_cancelled_request_is_never_answered() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        proxy.hold.store(true, Ordering::Relaxed);
        let mut tree = Box::new(conn).into_tree_with(Timing::default());
        let list = |path: &Path| Ask::List {
            path: p(path),
            budget: None,
        };
        tree.send(1, 0, list(&dir));
        assert!(until(Duration::from_secs(5), || requests(&proxy, OPENDIR) == 1));
        tree.cancel(1);
        tree.send(2, 0, list(&dir));
        proxy.hold.store(false, Ordering::Relaxed);
        let got = wait_for(&mut *tree, 2, Duration::from_secs(10));
        assert!(got.iter().all(|r| r.id == 2), "{got:?}");
        listed(&got, 2);
        assert!(tree.next(Duration::from_millis(200)).is_none());
        tree.close();
    }

    /// A remote deep scan stops at 300 listings with what is left; resumed without the budget
    /// it ends complete; nothing is listed once the tree has closed.
    #[test]
    fn a_remote_scan_stops_at_its_budget_and_resumes() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let mut start = Vec::new();
        for i in 0..320 {
            let sub = dir.join(format!("s{i:03}"));
            std::fs::create_dir(&sub).unwrap();
            std::fs::write(sub.join("f"), [0u8; 10]).unwrap();
            start.push(p(&sub));
        }
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::Scan(ScanAsk {
                start: start.clone(),
                budget: Some(REMOTE_SCAN_BUDGET),
                ..ScanAsk::default()
            }),
        );
        let got = wait_for(&mut *tree, 1, Duration::from_secs(30));
        let Some(Got::Scan(Scanned {
            totals,
            end: Some(ScanEnd::Budget(left)),
        })) = got.last().map(|r| r.got.clone())
        else {
            panic!("{:?}", got.last());
        };
        // At the first of 300 listings: what came with the 300th is counted too.
        assert!(
            (300..300 + IN_FLIGHT as u64).contains(&totals.listings),
            "{totals:?}"
        );
        assert_eq!(totals.bytes, 10 * totals.listings);
        assert_eq!(left.len() as u64, 320 - totals.listings);
        let mut grew = got.iter().filter_map(|r| match &r.got {
            Got::Scan(s) => Some(s.totals.bytes),
            _ => None,
        });
        let first = grew.next().unwrap();
        assert!(
            first < totals.bytes,
            "progress came before the end: {first}"
        );
        tree.send(
            2,
            0,
            Ask::Scan(ScanAsk {
                start: left,
                base: totals,
                ..ScanAsk::default()
            }),
        );
        let got = wait_for(&mut *tree, 2, Duration::from_secs(30));
        let Some(Got::Scan(Scanned {
            totals,
            end: Some(ScanEnd::Complete),
        })) = got.last().map(|r| r.got.clone())
        else {
            panic!("{:?}", got.last());
        };
        assert_eq!((totals.listings, totals.bytes), (320, 3200));
        // Closed mid-scan: no listing afterwards.
        tree.send(
            3,
            0,
            Ask::Scan(ScanAsk {
                start,
                ..ScanAsk::default()
            }),
        );
        let before = requests(&proxy, OPENDIR);
        assert!(until(Duration::from_secs(10), || requests(&proxy, OPENDIR)
            > before + 20));
        tree.close();
        std::thread::sleep(Duration::from_millis(50));
        let at_close = requests(&proxy, OPENDIR);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            requests(&proxy, OPENDIR),
            at_close,
            "no listing after the close"
        );
        assert!(at_close < before + 320, "the scan had not finished");
    }
}
