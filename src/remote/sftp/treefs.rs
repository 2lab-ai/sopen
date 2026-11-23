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
    files_used, guarded, is_cachedir_tag, scan_step, Ask, Budget, Child, FsStat, Got, Kind,
    Listing, Mounts, Reply, Resolved, ScanAsk, ScanBudget, ScanEnd, Scanned, Target, Totals,
    TreeFs, IN_FLIGHT, NOT_ANSWERING,
};
use crate::remote::Wake;
use openssh_sftp_client::fs::{Fs, Statvfs};
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
    /// Handles that may have been left open on the server (an open whose answer did not come
    /// in time: the client crate never closes a late one) before the connection is renewed.
    pub recycle_after: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            not_answering: NOT_ANSWERING,
            request: META_TIMEOUT,
            progress: Duration::from_millis(100),
            close: Duration::from_secs(5),
            recycle_after: 32,
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

    /// [`SftpRemote::into_tree`], with a way to open a fresh connection (`Connect` again, 02 §6):
    /// once 32 handles may have been left open on the server, the worker opens a new session
    /// through `renew` and lets the old one close (the server frees what it held).
    pub fn into_tree_renewing(self: Box<Self>, renew: Renew) -> Box<dyn TreeFs> {
        self.into_tree_full(Timing::default(), Some(renew))
    }

    pub(crate) fn into_tree_with(self: Box<Self>, timing: Timing) -> Box<dyn TreeFs> {
        self.into_tree_full(timing, None)
    }

    pub(crate) fn into_tree_full(
        mut self: Box<Self>,
        timing: Timing,
        renew: Option<Renew>,
    ) -> Box<dyn TreeFs> {
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
                let mut renew = renew;
                let mut rt = rt;
                let mut worker = Worker::new(sftp, fs, out, timing);
                let mut cmd_rx = cmd_rx;
                loop {
                    let renewable = renew.is_some();
                    let end = rt.block_on(std::future::poll_fn(|cx| {
                        worker.poll(cx, &mut cmd_rx, renewable)
                    }));
                    if end == End::Closed {
                        break;
                    }
                    // Too many handles may be left open on the server: a fresh session, opened
                    // here (outside the runtime: connecting runs one of its own), takes over;
                    // what was in flight is asked again on it, and the old one closes — the
                    // server frees whatever it held.
                    worker.leaks.set(0);
                    let Some(mut fresh) = renew.as_mut().and_then(|r| r()) else {
                        continue;
                    };
                    let (Some(new_rt), Some(new_sftp), Some(new_fs)) =
                        (fresh.rt.take(), fresh.sftp.take(), fresh.fs.take())
                    else {
                        continue;
                    };
                    drop(fresh);
                    // The old session's futures and timer go in its own runtime's context.
                    let old = {
                        let _in = rt.enter();
                        worker.requeue();
                        worker.timer = None;
                        worker.fs = new_fs;
                        std::mem::replace(&mut worker.sftp, new_sftp)
                    };
                    let _ = rt
                        .block_on(async { tokio::time::timeout(timing.close, old.close()).await });
                    rt = new_rt;
                }
                let sftp = {
                    let _in = rt.enter();
                    worker.flights.clear();
                    worker.draining.clear();
                    worker.scans.clear();
                    worker.timer = None;
                    let Worker { sftp, .. } = worker;
                    sftp
                };
                let _ =
                    rt.block_on(async { tokio::time::timeout(timing.close, sftp.close()).await });
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

/// Opens a fresh connection for the tree (on the worker's thread, outside its runtime);
/// `None`: it could not, and the old one goes on.
pub type Renew = Box<dyn FnMut() -> Option<SftpRemote> + Send>;

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

/// Opens whose answer did not come in time: each may have left a handle open on the server
/// (the client crate drops a late HANDLE without closing it).
type Leaks = std::rc::Rc<std::cell::Cell<u32>>;

/// Why the worker's loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Closed,
    /// Time for a fresh connection.
    Renew,
}

/// A request's "no longer wanted", which its future reads between its steps.
type Stop = std::rc::Rc<std::cell::Cell<bool>>;

/// What a request in flight is for.
enum Flight {
    /// An answer to a request.
    Ask {
        id: u64,
        /// The request, its priority and arrival: asked again on a fresh connection.
        ask: Box<Ask>,
        prio: u64,
        seq: u64,
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
    hold: HashSet<String>,
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
    leaks: Leaks,
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
            leaks: Leaks::default(),
        }
    }

    /// Puts what is in flight back to wait (a fresh connection takes over): requests in the
    /// queue in their order, a scan's folders at the front of its queue. Their futures, and the
    /// drains, go with the old session.
    fn requeue(&mut self) {
        for f in std::mem::take(&mut self.flights) {
            match f {
                Flight::Ask {
                    id, ask, prio, seq, ..
                } => self.waiting.push((prio, seq, id, *ask)),
                Flight::Scan { id, dir, .. } => {
                    if let Some(scan) = self.scans.get_mut(&id) {
                        scan.flying -= 1;
                        scan.queue.push_front(dir);
                    }
                }
            }
        }
        self.draining.clear();
    }

    /// Drives everything; ready once the tree closed (or went away).
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        cmds: &mut queue::UnboundedReceiver<Cmd>,
        renewable: bool,
    ) -> Poll<End> {
        loop {
            if renewable && self.leaks.get() >= self.timing.recycle_after {
                return Poll::Ready(End::Renew);
            }
            loop {
                match cmds.poll_recv(cx) {
                    Poll::Ready(Some(Cmd::Send(id, prio, ask))) => {
                        self.seq += 1;
                        self.waiting.push((prio, self.seq, id, *ask));
                    }
                    Poll::Ready(Some(Cmd::Cancel(id))) => self.cancel(id),
                    Poll::Ready(Some(Cmd::Close)) | Poll::Ready(None) => {
                        return Poll::Ready(End::Closed)
                    }
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
        let handle = matches!(ask, Ask::List { .. } | Ask::Mounts);
        let leaks = self.leaks.clone();
        let kept = Box::new(ask.clone());
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
                Box::pin(
                    async move { Got::Mounts(mounts(fs, opts, statvfs, request, &leaks).await) },
                )
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
                    Got::Listed(list(fs, Some(row), &path, budget, request, &stop, &leaks).await)
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
            ask: kept,
            prio,
            seq,
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
                hold: ask.hold.into_iter().collect(),
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
        let leaks = self.leaks.clone();
        let fut: Fut<Result<Option<Listing>, String>> = Box::pin(async move {
            if let Some(allowed) = fence {
                // 05 §4.1: a folder on another filesystem is a mount point, not entered — and a
                // folder whose filesystem cannot be told is not entered either (fail closed).
                let st = within(request, &format!("statvfs {path}"), fs.statvfs(&path)).await?;
                if !allowed.contains(&st.fsid) {
                    return Ok(None);
                }
            }
            list(fs, None, &path, budget, request, &stopped, &leaks)
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
                            hold,
                            seen,
                            totals,
                            queue,
                            ..
                        } = scan;
                        queue.extend(scan_step(&dir, &listing, skip, hold, seen, totals));
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
    leaks: &Leaks,
) -> Mounts {
    // Bounded in time as a whole, whatever the server answers.
    let started = Instant::now();
    let go = || started.elapsed() < 2 * request;
    let mountinfo = read_all(opts, "/proc/self/mountinfo", 1 << 20, request, &go, leaks).await;
    if let Ok(bytes) = mountinfo {
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
/// request for the rest; the crate's cursor moves by the length asked). `go` is asked before
/// every READ — a stop or a deadline ends it there, and the handle it holds is closed. An open
/// whose answer did not come in time is counted in `leaks`.
async fn read_all(
    opts: openssh_sftp_client::file::OpenOptions,
    path: &str,
    max: u64,
    request: Duration,
    go: &dyn Fn() -> bool,
    leaks: &Leaks,
) -> Result<Vec<u8>, String> {
    let mut opts = opts;
    let file = match tokio::time::timeout(request, opts.read(true).open(path)).await {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => return Err(format!("cannot open {path}: {e}")),
        Err(_) => {
            leaks.set(leaks.get() + 1);
            return Err(format!("open {path}: no reply in {}s", request.as_secs()));
        }
    };
    let mut out = Vec::new();
    let result = async {
        while (out.len() as u64) < max {
            if !go() {
                return Err(format!("read {path}: stopped"));
            }
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
    Ok(fs_stat(&st))
}

/// What a `statvfs@openssh.com` reply says of its filesystem.
fn fs_stat(st: &Statvfs) -> FsStat {
    FsStat {
        used_bytes: st.blocks.saturating_sub(st.bfree).saturating_mul(st.frsize),
        used_files: files_used(st.files, st.ffree),
        fsid: st.fsid,
    }
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
    leaks: &Leaks,
) -> Result<Listing, String> {
    let cancelled = || Err::<Listing, _>(format!("list {path}: cancelled"));
    let started = Instant::now();
    // Cancelled before it was sent (a scan's folder after its fence): no OPENDIR at all.
    if stop.get() {
        return cancelled();
    }
    let dir = match tokio::time::timeout(request, fs.open_dir(path)).await {
        Ok(Ok(dir)) => dir,
        Ok(Err(e)) => return Err(format!("cannot list {path}: {e}")),
        Err(_) => {
            // Its HANDLE, if it comes, will never be closed by the client.
            leaks.set(leaks.get() + 1);
            return Err(format!("list {path}: no reply in {}s", request.as_secs()));
        }
    };
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
            // Resolved one component at a time, never looked up at or below a guard.
            let name = child.name.clone();
            match follow(&mut fs, path, &name, &row.guard, &go, request).await {
                Some(t) => listing.children[i].kind = Kind::Link(t),
                None => {
                    listing.complete = false;
                    return Ok(listing);
                }
            }
            continue;
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
        listing.cachedir = read_all(row.opts, &tag, 43, request, &go, leaks)
            .await
            .is_ok_and(|head| is_cachedir_tag(&head));
        if !go() {
            listing.complete = false;
        }
    }
    Ok(listing)
}

/// What the link `dir/name` points at, resolved one component at a time over SFTP (`LSTAT`,
/// and `READLINK` for each link on the way, at most 40), never looking up a path at or below one
/// of `guard`: one there is [`Target::Unread`], as is anything that cannot be checked (fail
/// closed). `None`: `go` said stop.
async fn follow(
    fs: &mut Fs,
    dir: &str,
    name: &str,
    guard: &[String],
    go: &dyn Fn() -> bool,
    request: Duration,
) -> Option<Target> {
    // `dir` was listed by its path: it is a folder, and no link is in it.
    let mut at = dir.to_string();
    let mut todo: VecDeque<String> = VecDeque::from([name.to_string()]);
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        match part.as_str() {
            "" | "." => continue,
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
        let meta = match tokio::time::timeout(request, fs.symlink_metadata(&next)).await {
            Ok(Ok(meta)) => meta,
            // Broken: nothing was followed.
            Ok(Err(e)) if super::is_not_found(&e) => return Some(Target::Other),
            _ => return Some(Target::Unread),
        };
        match meta.file_type() {
            Some(t) if t.is_symlink() => {
                hops += 1;
                if hops > 40 {
                    return Some(Target::Other);
                }
                if !go() {
                    return None;
                }
                let to = match tokio::time::timeout(request, fs.read_link(&next)).await {
                    Ok(Ok(to)) => to.to_string_lossy().into_owned(),
                    _ => return Some(Target::Unread),
                };
                if to.starts_with('/') {
                    at = "/".to_string();
                }
                for p in to.split('/').rev() {
                    todo.push_front(p.to_string());
                }
            }
            Some(t) if t.is_dir() => at = next,
            Some(t) if t.is_file() && todo.iter().all(|p| p.is_empty() || p == ".") => {
                return Some(Target::File)
            }
            Some(_) => return Some(Target::Other),
            None => return Some(Target::Unread),
        }
    }
    Some(Target::Dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::sftp::{Proxy, Wired};
    use crate::remote::treefs::{ScanAsk, ScanBudget, LIST_BUDGET, REMOTE_SCAN_BUDGET};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    /// `SSH_FXP_OPENDIR`.
    const OPENDIR: u8 = 11;

    /// 05 §6.1: a `statvfs@openssh.com` reply with `f_files` 0 (btrfs) gives no file count; one
    /// that counts files gives `f_files − f_ffree`.
    #[test]
    fn a_statvfs_reply_without_file_counting_has_no_file_count() {
        let reply = |files, ffree| Statvfs {
            bsize: 4096,
            frsize: 4096,
            blocks: 100,
            bfree: 40,
            bavail: 40,
            files,
            ffree,
            favail: ffree,
            fsid: 7,
            flag: 0,
            namemax: 255,
        };
        let btrfs = fs_stat(&reply(0, 0));
        assert_eq!(
            btrfs,
            FsStat {
                used_bytes: 60 * 4096,
                used_files: None,
                fsid: 7,
            }
        );
        assert_eq!(fs_stat(&reply(1000, 10)).used_files, Some(990));
    }

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
        // With a guard the target is resolved by LSTAT and READLINK, one component at a time
        // (round 2): no STAT, which would follow the whole chain on the server.
        assert_eq!(requests(&proxy, STAT), 0, "nothing followed by the server");
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

    /// Round 2, item 1: a chain of links into a guarded mount is never followed (no `STAT`, no
    /// lookup in it); a `READLINK` that fails leaves the target unread (fail closed).
    #[test]
    fn a_link_chain_into_a_guarded_mount_is_never_followed() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let (_g, guarded) = temp();
        std::fs::create_dir_all(guarded.join("share/x")).unwrap();
        std::os::unix::fs::symlink("next", dir.join("direct")).unwrap();
        std::os::unix::fs::symlink(guarded.join("share/x"), dir.join("next")).unwrap();
        let (_t2, other) = temp();
        std::fs::create_dir(other.join("sub")).unwrap();
        std::os::unix::fs::symlink("sub", other.join("plain")).unwrap();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        let ask = |path: &Path| Ask::List {
            path: p(path),
            budget: None,
            guard: vec![p(&guarded)],
        };
        tree.send(1, 0, ask(&dir));
        let listing = listed(&wait_for(&mut *tree, 1, Duration::from_secs(10)), 1);
        let kind = |l: &Listing, n: &str| l.children.iter().find(|c| c.name == n).unwrap().kind;
        assert_eq!(kind(&listing, "direct"), Kind::Link(Target::Unread));
        assert_eq!(kind(&listing, "next"), Kind::Link(Target::Unread));
        assert_eq!(requests(&proxy, STAT), 0, "nothing followed");
        // SSH_FXP_READLINK answered SSH_FX_FAILURE: the target is not read.
        proxy.rules.lock().unwrap().fail = Some(READLINK);
        tree.send(2, 0, ask(&other));
        let listing = listed(&wait_for(&mut *tree, 2, Duration::from_secs(10)), 2);
        assert_eq!(kind(&listing, "plain"), Kind::Link(Target::Unread));
        tree.close();
    }

    /// Round 2, item 2: the `CACHEDIR.TAG` read stops at its listing's deadline, and on a cancel
    /// (Esc): no further READ (a server answering every READ with no bytes would go on forever).
    #[test]
    fn a_cache_tag_read_stops_at_its_deadline_and_on_esc() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let reads = |proxy: &Proxy| requests(proxy, Wired::READ);
        // Past the deadline.
        let proxy = Proxy::default();
        proxy.rules.lock().unwrap().empty = vec![0];
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut tree = Box::new(conn).into_tree_with(quick());
        tree.send(
            1,
            0,
            Ask::List {
                path: p(&dir),
                budget: Some(Budget {
                    entries: 10_000,
                    time: Duration::from_millis(300),
                }),
                guard: Vec::new(),
            },
        );
        let listing = listed(&wait_for(&mut *tree, 1, Duration::from_secs(5)), 1);
        assert!(!listing.complete && !listing.cachedir);
        let at = reads(&proxy);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(reads(&proxy), at, "no READ after the deadline");
        tree.close();
        // On Esc (an owner's whole listing has no deadline).
        let proxy = Proxy::default();
        proxy.rules.lock().unwrap().empty = vec![0];
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
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
        assert!(until(Duration::from_secs(5), || reads(&proxy) >= 3));
        tree.cancel(1);
        std::thread::sleep(Duration::from_millis(100));
        let at = reads(&proxy);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(reads(&proxy), at, "no READ after Esc");
        tree.close();
    }

    /// Round 2, item 3: opens whose answer came too late leave handles on the server (the client
    /// crate never closes a late one); past `recycle_after` of them the worker opens a fresh
    /// connection and the tree goes on on it.
    #[test]
    fn orphaned_handles_renew_the_connection() {
        if SftpRemote::local_server().is_none() {
            return;
        }
        let (_tmp, dir) = temp();
        let proxy = Proxy::default();
        let conn = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        proxy.hold.store(true, Ordering::Relaxed);
        let renewed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = renewed.clone();
        let renew: Renew = Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            SftpRemote::local_server()
        });
        let timing = Timing {
            request: Duration::from_millis(300),
            close: Duration::from_millis(300),
            recycle_after: 2,
            ..quick()
        };
        let mut tree = Box::new(conn).into_tree_full(timing, Some(renew));
        let list = || Ask::List {
            path: p(&dir),
            budget: None,
            guard: Vec::new(),
        };
        tree.send(1, 0, list());
        tree.send(2, 0, list());
        for id in [1, 2] {
            let got = wait_for(&mut *tree, id, Duration::from_secs(5));
            assert!(
                matches!(got.last().map(|r| &r.got), Some(Got::Listed(Err(_)))),
                "{got:?}"
            );
        }
        assert!(until(Duration::from_secs(5), || renewed
            .load(Ordering::SeqCst)
            == 1));
        tree.send(3, 0, list());
        listed(&wait_for(&mut *tree, 3, Duration::from_secs(5)), 3);
        assert_eq!(renewed.load(Ordering::SeqCst), 1);
        proxy.hold.store(false, Ordering::Relaxed);
        tree.close();
    }
}
