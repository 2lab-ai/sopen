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
    guarded, is_cachedir_tag, normalize, scan_step, Ask, Budget, Child, FsStat, Got, Kind, Listing,
    Mounts, Reply, Resolved, ScanAsk, ScanBudget, ScanEnd, Scanned, Target, Totals, TreeFs,
    IN_FLIGHT, NOT_ANSWERING,
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
    Send(u64, u64, Box<Ask>),
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
        let _ = self.cmds.send(Cmd::Send(id, prio, Box::new(ask)));
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

/// A request's "no longer wanted", which its future reads between its steps.
type Stop = std::rc::Rc<std::cell::Cell<bool>>;

/// What a request in flight is for.
enum Flight {
    /// An answer to a request.
    Ask {
        id: u64,
        since: Instant,
        warned: bool,
        /// It opens a handle (a listing): cancelled, it is drained, not dropped.
        handle: bool,
        stop: Stop,
        fut: Fut<Got>,
    },
    /// One folder of a scan: its listing, or `None` when its fence keeps it out.
    Scan {
        id: u64,
        dir: String,
        since: Instant,
        warned: bool,
        stop: Stop,
        fut: Fut<Result<Option<Listing>, String>>,
    },
}

impl Flight {
    fn id(&self) -> u64 {
        match self {
            Flight::Ask { id, .. } | Flight::Scan { id, .. } => *id,
        }
    }

    /// Cancels it: a future that may hold a handle keeps running until it has closed it — its
    /// OPENDIR may already be on the wire, and a handle whose answer nobody reads is never
    /// closed — and its answer is dropped; any other is dropped at once.
    fn drain(self) -> Option<Fut<()>> {
        match self {
            Flight::Ask {
                handle: true,
                stop,
                fut,
                ..
            } => {
                stop.set(true);
                Some(Box::pin(async move {
                    fut.await;
                }))
            }
            Flight::Scan { stop, fut, .. } => {
                stop.set(true);
                Some(Box::pin(async move {
                    let _ = fut.await;
                }))
            }
            Flight::Ask { .. } => None,
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
    /// Each folder's listing stops at this.
    listing: Option<Budget>,
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
    /// Cancelled listings running on until their handle is closed; they count toward
    /// [`IN_FLIGHT`] — they are requests on the wire still.
    draining: Vec<Fut<()>>,
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
            draining: Vec::new(),
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
                        self.waiting.push((prio, self.seq, id, *ask));
                    }
                    Poll::Ready(Some(Cmd::Cancel(id))) => self.cancel(id),
                    Poll::Ready(Some(Cmd::Close)) | Poll::Ready(None) => return Poll::Ready(()),
                    Poll::Pending => break,
                }
            }
            // A scan whose deadline passed ends on its own, whatever is in flight.
            let late: Vec<u64> = self
                .scans
                .iter()
                .filter(|(_, s)| s.budget.is_some_and(|b| s.started.elapsed() >= b.time))
                .map(|(id, _)| *id)
                .collect();
            for id in late {
                self.scan_moved(id);
            }
            self.draining
                .retain_mut(|fut| fut.as_mut().poll(cx).is_pending());
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
        self.scans.remove(&id);
        self.drop_flights(id);
    }

    /// Cancels every flight of `id` (a listing drains until its handle is closed).
    fn drop_flights(&mut self, id: u64) {
        let (gone, kept): (Vec<Flight>, Vec<Flight>) = std::mem::take(&mut self.flights)
            .into_iter()
            .partition(|f| f.id() == id);
        self.flights = kept;
        self.draining
            .extend(gone.into_iter().filter_map(Flight::drain));
    }

    /// Starts what waits, nearest first, while fewer than [`IN_FLIGHT`] are out: requests and
    /// the scans' next folders by one order.
    fn start(&mut self) {
        while self.flights.len() + self.draining.len() < IN_FLIGHT {
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
        let stop = Stop::default();
        let handle = matches!(ask, Ask::List { .. });
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
            Ask::List {
                path,
                budget,
                guard,
            } => {
                let row = Row {
                    opts: self.sftp.options(),
                    guard,
                };
                let stop = stop.clone();
                Box::pin(async move {
                    Got::Listed(list(fs, Some(row), &path, budget, request, &stop).await)
                })
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
            handle,
            stop,
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
                listing: ask.listing,
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
        let budget = scan.listing;
        let path = dir.clone();
        let stop = Stop::default();
        let stopped = stop.clone();
        let fut: Fut<Result<Option<Listing>, String>> = Box::pin(async move {
            if let Some(allowed) = fence {
                // 05 §4.1: a folder on another filesystem is a mount point, not entered — and a
                // folder whose filesystem cannot be told is not entered either (fail closed).
                let st = within(request, &format!("statvfs {path}"), fs.statvfs(&path)).await?;
                if !allowed.contains(&st.fsid) {
                    return Ok(None);
                }
            }
            list(fs, None, &path, budget, request, &stopped)
                .await
                .map(Some)
        });
        self.flights.push(Flight::Scan {
            id,
            dir,
            since: Instant::now(),
            warned: false,
            stop,
            fut,
        });
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
            self.drop_flights(id);
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
            let (Flight::Ask {
                id, since, warned, ..
            }
            | Flight::Scan {
                id, since, warned, ..
            }) = f;
            if *warned {
                continue;
            }
            let due = *since + limit;
            if Instant::now() >= due {
                *warned = true;
                if !late.contains(id) {
                    late.push(*id);
                }
            } else {
                next = Some(next.map_or(due, |n| n.min(due)));
            }
        }
        // A scan's own deadline wakes the worker too.
        for s in self.scans.values() {
            if let Some(b) = s.budget {
                let due = s.started + b.time;
                next = Some(next.map_or(due, |n| n.min(due)));
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
    if let Ok(bytes) = read_all(opts, "/proc/self/mountinfo", 1 << 20, request, None).await {
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
    stop: Option<&Stop>,
) -> Result<Vec<u8>, String> {
    let mut opts = opts;
    let file = within(request, &format!("open {path}"), opts.read(true).open(path)).await?;
    if stop.is_some_and(|s| s.get()) {
        let _ = tokio::time::timeout(Duration::from_secs(5), file.close()).await;
        return Err(format!("read {path}: cancelled"));
    }
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

/// What a row's listing adds to a scan's: the client to read `CACHEDIR.TAG` with, and the
/// mounts a link must not be followed into.
struct Row {
    opts: openssh_sftp_client::file::OpenOptions,
    guard: Vec<String>,
}

/// One folder: OPENDIR, READDIR … (each reply within the request timeout), CLOSE — by the
/// client's drop of the handle or the stream, also when it stops early or is cancelled (`stop`,
/// read between every two requests: a cancelled listing closes its handle as soon as it has it).
/// `budget` stops it between replies (once one has come — before that it waits, `not answering`
/// after 2 s, up to the request timeout).
///
/// A row's listing (`row`) then reads each link's target — `READLINK` first when there is a
/// guard, and a target at or below one is not `stat`ed ([`Target::Unread`]); else one `stat` —
/// and the 43-byte `CACHEDIR.TAG`, each only inside the budget's time and while wanted: past
/// it the links not read stay non-rows and the listing says it is incomplete.
async fn list(
    mut fs: Fs,
    row: Option<Row>,
    path: &str,
    budget: Option<Budget>,
    request: Duration,
    stop: &Stop,
) -> Result<Listing, String> {
    let cancelled = || Err::<Listing, _>(format!("list {path}: cancelled"));
    let started = Instant::now();
    // Cancelled before it was sent (a scan's folder after its fence): no OPENDIR at all.
    if stop.get() {
        return cancelled();
    }
    let dir = within(request, &format!("list {path}"), fs.open_dir(path)).await?;
    if stop.get() {
        // Dropping the handle closes it.
        return cancelled();
    }
    let mut listing = Listing {
        complete: true,
        ..Listing::default()
    };
    {
        let mut stream = std::pin::pin!(dir.read_dir());
        loop {
            if stop.get() {
                return cancelled();
            }
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
    let Some(row) = row else {
        return Ok(listing);
    };
    let deadline = budget.map(|b| started + b.time);
    let go = || !stop.get() && deadline.is_none_or(|d| Instant::now() < d);
    for i in 0..listing.children.len() {
        let child = &listing.children[i];
        if child.kind != Kind::Link(Target::Other) || child.lossy {
            continue;
        }
        if !go() {
            listing.complete = false;
            return Ok(listing);
        }
        let full = crate::remote::join(path, &child.name);
        if !row.guard.is_empty() {
            // Where it points, by its words: a target in a guarded mount is never asked for.
            let to = within(request, "readlink", fs.read_link(&full)).await;
            let unread = match to {
                Ok(to) => {
                    let to = to.to_string_lossy().into_owned();
                    let abs = if to.starts_with('/') {
                        to
                    } else {
                        crate::remote::join(path, &to)
                    };
                    guarded(&normalize(&abs), &row.guard)
                }
                Err(_) => false,
            };
            if unread {
                listing.children[i].kind = Kind::Link(Target::Unread);
                continue;
            }
            if !go() {
                listing.complete = false;
                return Ok(listing);
            }
        }
        listing.children[i].kind = match within(request, "stat", fs.metadata(&full)).await {
            Ok(m) if m.file_type().is_some_and(|t| t.is_dir()) => Kind::Link(Target::Dir),
            Ok(m) if m.file_type().is_some_and(|t| t.is_file()) => Kind::Link(Target::File),
            _ => Kind::Link(Target::Other),
        };
    }
    let tagged = listing
        .children
        .iter()
        .any(|c| c.name == "CACHEDIR.TAG" && c.kind == Kind::File);
    if tagged {
        if !go() {
            listing.complete = false;
            return Ok(listing);
        }
        let tag = crate::remote::join(path, "CACHEDIR.TAG");
        listing.cachedir = read_all(row.opts, &tag, 43, request, Some(stop))
            .await
            .is_ok_and(|head| is_cachedir_tag(&head));
    }
    Ok(listing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::sftp::Proxy;
    use crate::remote::treefs::{ScanAsk, ScanBudget, LIST_BUDGET, REMOTE_SCAN_BUDGET};
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
                guard: Vec::new(),
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
                guard: Vec::new(),
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
                    guard: Vec::new(),
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
            guard: Vec::new(),
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

    /// `SSH_FXP_STAT`, `SSH_FXP_READLINK`, `SSH_FXP_HANDLE`.
    const STAT: u8 = 17;
    const READLINK: u8 = 19;
    const HANDLE: u8 = 102;

    fn scan(start: Vec<String>) -> ScanAsk {
        ScanAsk {
            start,
            ..ScanAsk::default()
        }
    }

    fn end(replies: &[Reply], id: u64) -> Scanned {
        replies
            .iter()
            .rev()
            .find_map(|r| match (&r.got, r.id == id) {
                (Got::Scan(s @ Scanned { end: Some(_), .. }), true) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no end for {id}: {replies:?}"))
    }

    /// Item 3: a link whose target lies in a guarded mount is never `stat`ed (only read); one
    /// outside it is.
    #[test]
    fn a_link_into_a_guarded_mount_is_never_stated() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let (_g, guarded) = temp();
        std::fs::create_dir(guarded.join("x")).unwrap();
        std::fs::create_dir(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(guarded.join("x"), dir.join("in")).unwrap();
        std::os::unix::fs::symlink("real", dir.join("out")).unwrap();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::List {
                path: p(&dir),
                budget: None,
                guard: vec![p(&guarded)],
            },
        );
        let listing = listed(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        let kind = |n: &str| listing.children.iter().find(|c| c.name == n).unwrap().kind;
        assert_eq!(kind("in"), Kind::Link(Target::Unread));
        assert_eq!(kind("out"), Kind::Link(Target::Dir));
        assert_eq!(requests(&proxy, STAT), 1, "only the link outside the guard");
        tree.close();
    }

    /// Item 5: a folder whose `statvfs` fails under an active fence is not listed; it counts as
    /// unread (`+`).
    #[test]
    fn a_fence_that_cannot_be_read_is_not_crossed() {
        let Some(conn) = SftpRemote::local_server() else {
            return;
        };
        if !conn.sftp.as_ref().unwrap().support_statvfs() {
            eprintln!("skipping: the server has no statvfs@openssh.com");
            return;
        }
        drop(conn);
        let (_tmp, dir) = temp();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        // SSH_FXP_EXTENDED: the statvfs is answered SSH_FX_FAILURE.
        proxy.rules.lock().unwrap().fail = Some(200);
        tree.send(
            1,
            0,
            Ask::Scan(ScanAsk {
                fence: Some(vec![12345]),
                ..scan(vec![p(&dir.join("sub"))])
            }),
        );
        let got = end(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        assert_eq!((got.totals.listings, got.totals.unread), (0, 1), "{got:?}");
        assert_eq!(requests(&proxy, OPENDIR), 0);
        tree.close();
    }

    /// Item 7: a scan bounds each folder's listing; one read in part is counted as such.
    #[test]
    fn a_scan_bounds_each_listing() {
        let Some(conn) = SftpRemote::local_server() else {
            return;
        };
        let (_tmp, dir) = temp();
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}")), b"x").unwrap();
        }
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::Scan(ScanAsk {
                listing: Some(Budget {
                    entries: 2,
                    ..LIST_BUDGET
                }),
                ..scan(vec![p(&dir)])
            }),
        );
        let got = end(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        assert_eq!((got.totals.listings, got.totals.partial), (1, 1), "{got:?}");
        tree.close();
    }

    /// Item 7: a remote scan's own deadline fires while nothing answers.
    #[test]
    fn a_scan_deadline_fires_while_nothing_answers() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        proxy.hold.store(true, Ordering::Relaxed);
        let mut tree = Box::new(conn).into_tree_with(quick());
        let started = Instant::now();
        tree.send(
            1,
            0,
            Ask::Scan(ScanAsk {
                budget: Some(ScanBudget {
                    time: Duration::from_millis(300),
                    ..REMOTE_SCAN_BUDGET
                }),
                ..scan(vec![p(&dir)])
            }),
        );
        let got = end(&wait_for(&mut *tree, 1, Duration::from_secs(5)), 1);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(got.end, Some(ScanEnd::Budget(vec![p(&dir)])));
        proxy.hold.store(false, Ordering::Relaxed);
        tree.close();
    }

    /// Item 8: a row's listing out of time reads no link's target.
    #[test]
    fn a_listing_out_of_time_reads_no_link() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        for i in 0..5 {
            std::os::unix::fs::symlink("/", dir.join(format!("l{i}"))).unwrap();
        }
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::List {
                path: p(&dir),
                budget: Some(Budget {
                    entries: 10_000,
                    time: Duration::ZERO,
                }),
                guard: Vec::new(),
            },
        );
        let listing = listed(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        assert!(!listing.complete);
        assert_eq!(requests(&proxy, STAT) + requests(&proxy, READLINK), 0);
        tree.close();
    }

    /// Item 9: a scan's folder with no answer in 2 s is reported `not answering`.
    #[test]
    fn a_scan_folder_that_does_not_answer_is_reported() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        proxy.hold.store(true, Ordering::Relaxed);
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(1, 0, Ask::Scan(scan(vec![p(&dir)])));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut late = false;
        while !late && Instant::now() < deadline {
            late = matches!(
                tree.next(Duration::from_millis(20)),
                Some(Reply {
                    id: 1,
                    got: Got::NotAnswering
                })
            );
        }
        assert!(late, "no not answering for the scan");
        proxy.hold.store(false, Ordering::Relaxed);
        tree.close();
    }

    /// Item 11: a listing cancelled before its OPENDIR answered leaves no handle open: the late
    /// handle is closed (the server's own log, opens against closes).
    #[test]
    fn cancelled_listings_leave_no_open_handle() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let (_l, logs) = temp();
        let log = logs.join("server.log");
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_run(Some(proxy.clone()), &[], Some(&log)).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        for id in 0..100u64 {
            proxy.hold.store(true, Ordering::Relaxed);
            tree.send(
                id,
                0,
                Ask::List {
                    path: p(&dir),
                    budget: None,
                    guard: Vec::new(),
                },
            );
            let sent = id as usize + 1;
            assert!(until(Duration::from_secs(5), || requests(&proxy, OPENDIR) == sent));
            tree.cancel(id);
            proxy.hold.store(false, Ordering::Relaxed);
            // The late HANDLE reaches the client before the next round holds the link again.
            let handles = || {
                proxy
                    .wire
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|w| w.reply && w.kind == HANDLE)
                    .count()
            };
            assert!(until(Duration::from_secs(5), || handles() == sent));
        }
        let count = |word: &str| {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with(word))
                .count()
        };
        until(Duration::from_secs(5), || {
            count("opendir ") == 100 && count("closedir ") == 100
        });
        assert_eq!(count("opendir "), 100);
        assert_eq!(count("closedir "), 100, "every late handle was closed");
        tree.close();
    }
}
