//! [`Ranged`] over SFTP (06 §2). The connection the open made becomes the transport: the file is
//! opened on it once and its handle `fstat`ed, then a thread of its own drives the session's
//! current-thread runtime. One task there sends every request in the order queued — the crate
//! writes a request when its future is first polled, so requests polled from different tasks
//! could overtake each other — and hands the completions back in that order.

use super::{
    is_not_found, read_at, stat_of, timed_out, within, SftpRemote, FLUSH_INTERVAL, META_TIMEOUT,
    STALL_TIMEOUT,
};
use crate::remote::ranged::{
    clipped, Asked, Completion, Lost, Ranged, Refused, Reply, Ticket, Wake,
};
use crate::remote::{RemoteFs, Stat};
use anyhow::{anyhow, bail, Result};
use bytes::{Bytes, BytesMut};
use openssh_sftp_client::file::File;
use openssh_sftp_client::fs::Fs;
use openssh_sftp_client::Sftp;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc as queue;

/// The transport's bounds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// How long `Sftp::close` may wait for the server when the transport closes (06 §2: 5 s).
    pub close: Duration,
    /// A request with no reply this long after it was sent fails (01 §5's 60 s).
    pub stall: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            close: Duration::from_secs(5),
            stall: STALL_TIMEOUT,
        }
    }
}

/// The session as [`SftpRemote`] holds it, moved to the transport's thread and back.
struct Session {
    rt: Runtime,
    sftp: Sftp,
    fs: Fs,
    #[cfg(test)]
    stats: u64,
}

enum Cmd {
    Send(Ticket, Asked),
    /// Look at the finish flag: the transport ends.
    Finish,
    /// Tests: the transport's thread panics.
    #[cfg(test)]
    Panic,
    /// Tests: the driver stops (its thread blocks) until this receives.
    #[cfg(test)]
    Pause(mpsc::Receiver<()>),
}

/// Tests: set once the transport's thread has ended — its session closed or given back, its
/// runtime dropped.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Teardown(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

#[cfg(test)]
impl Teardown {
    pub(crate) fn done(&self) -> bool {
        *self.0 .0.lock().unwrap()
    }

    /// Whether it ended within `limit`.
    pub(crate) fn wait(&self, limit: Duration) -> bool {
        let (lock, cv) = &*self.0;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv.wait_timeout_while(guard, limit, |d| !*d).unwrap();
        *guard
    }
}

/// Tests: marks the [`Teardown`] when the transport's thread ends.
#[cfg(test)]
struct Ended(Teardown);

#[cfg(test)]
impl Drop for Ended {
    fn drop(&mut self) {
        let (lock, cv) = &*self.0 .0;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// Close the handle and give the session back.
    Keep,
    /// Close the handle and the session.
    Close,
}

/// After the last clone of a handle drops, its CLOSE waits in the session's write buffer and the
/// flush task is waiting out its interval. The transport drives the runtime for a few intervals —
/// by then the flush task's tick has fired — and then for this many scheduler passes, so the
/// woken flush task runs: tokio wakes a yielded future only after the ready tasks have run and
/// the driver has been polled. It waits for the local write alone, never for the server's reply.
const FLUSH_PASSES: usize = 8;
/// A bound on all of it: a pipe the server has stopped draining ends it here, and the CLOSE then
/// goes out with the session's next use.
const FLUSH_BOUND: Duration = Duration::from_millis(200);

/// The finish flag's values. The driver reads it before every queued request, so what is queued
/// behind an end is discarded, never sent.
const RUNNING: u8 = 0;
const KEEP: u8 = 1;
const CLOSE: u8 = 2;

enum Event {
    Done(Completion),
    Wake,
    /// The transport's thread ended (a panic): nothing more will come from it.
    Gone,
}

/// Sends [`Event::Gone`] when the transport's thread ends, unwinding included.
struct Gone(mpsc::Sender<Event>);

impl Drop for Gone {
    fn drop(&mut self) {
        let _ = self.0.send(Event::Gone);
    }
}

pub(crate) struct SftpRanged {
    path: String,
    opened: Stat,
    cmds: queue::UnboundedSender<Cmd>,
    finish: Arc<AtomicU8>,
    events: mpsc::Receiver<Event>,
    wake: Wake,
    woken: Arc<AtomicBool>,
    next: u64,
    /// Requests queued and not yet returned by `next`, in order.
    queued: VecDeque<(Ticket, Asked)>,
    /// The transport's thread is gone: every request still queued fails, in its turn.
    gone: bool,
    thread: Option<std::thread::JoinHandle<Option<Session>>>,
    #[cfg(test)]
    ended: Teardown,
}

impl SftpRemote {
    /// [`RemoteFs::into_ranged`], with the transport's bounds.
    pub(crate) fn open_ranged(
        mut self: Box<Self>,
        path: &str,
        timing: Timing,
    ) -> std::result::Result<SftpRanged, Refused> {
        let (rt, sftp, _) = self.parts();
        let opened = rt.block_on(async {
            let mut file = within(META_TIMEOUT, sftp.open(path))
                .await
                .map_err(|()| timed_out(&format!("open {path}"), META_TIMEOUT))?
                .map_err(|e| anyhow!("cannot open {path}: {e}"))?;
            let checked = async {
                let meta = within(META_TIMEOUT, file.metadata())
                    .await
                    .map_err(|()| timed_out(&format!("stat {path}"), META_TIMEOUT))?
                    .map_err(|e| anyhow!("cannot stat {path}: {e}"))?;
                if meta.len().is_none() {
                    bail!("the server does not report the size of {path}, so it cannot stream");
                }
                if meta.file_type().is_some_and(|t| !t.is_file()) {
                    bail!("{path} is not a regular file");
                }
                Ok(stat_of(&meta))
            };
            match checked.await {
                Ok(stat) => Ok((file, stat)),
                Err(e) => {
                    let _ = within(Duration::from_secs(5), file.close()).await;
                    Err(e)
                }
            }
        });
        match opened {
            Ok((file, opened)) => SftpRanged::start(self, file, path, opened, timing),
            Err(error) => Err(Refused { conn: self, error }),
        }
    }
}

impl SftpRanged {
    fn start(
        mut conn: Box<SftpRemote>,
        file: File,
        path: &str,
        opened: Stat,
        timing: Timing,
    ) -> std::result::Result<Self, Refused> {
        let (hand_over, handed) = mpsc::channel::<(Session, File)>();
        let (cmds, cmd_rx) = queue::unbounded_channel();
        let finish = Arc::new(AtomicU8::new(RUNNING));
        let flag = finish.clone();
        let (events_tx, events) = mpsc::channel();
        let wake_tx = events_tx.clone();
        let owned = path.to_string();
        #[cfg(test)]
        let ended = Teardown::default();
        #[cfg(test)]
        let marker = Ended(ended.clone());
        let spawned = std::thread::Builder::new()
            .name("sopen-stream".into())
            .spawn(move || {
                #[cfg(test)]
                let _ended = marker;
                let _gone = Gone(events_tx.clone());
                let (session, file) = handed.recv().ok()?;
                transport(session, file, owned, cmd_rx, flag, events_tx, timing)
            });
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                drop(file);
                let error = anyhow!("cannot start the transport of {path}: {e}");
                return Err(Refused { conn, error });
            }
        };
        // `conn` keeps nothing from here: its drop closes nothing.
        let session = Session {
            rt: conn.rt.take().expect("open session"),
            sftp: conn.sftp.take().expect("open session"),
            fs: conn.fs.take().expect("open session"),
            #[cfg(test)]
            stats: conn.stats,
        };
        drop(conn);
        hand_over
            .send((session, file))
            .expect("the transport's thread waits for its session");
        let woken = Arc::new(AtomicBool::new(false));
        let flag = woken.clone();
        let wake = Wake::new(move || {
            // At most one wake waits in the channel.
            if !flag.swap(true, Ordering::SeqCst) {
                let _ = wake_tx.send(Event::Wake);
            }
        });
        Ok(Self {
            path: path.to_string(),
            opened,
            cmds,
            finish,
            events,
            wake,
            woken,
            next: 0,
            queued: VecDeque::new(),
            gone: false,
            thread: Some(thread),
            #[cfg(test)]
            ended,
        })
    }

    fn send(&mut self, asked: Asked) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        self.queued.push_back((ticket, asked));
        let _ = self.cmds.send(Cmd::Send(ticket, asked));
        ticket
    }

    /// Tells the driver to end: it sends nothing more of what is queued.
    fn end(&self, how: u8) {
        self.finish.store(how, Ordering::SeqCst);
        let _ = self.cmds.send(Cmd::Finish);
    }

    /// The oldest request still queued, failed: the transport is gone.
    fn orphan(&mut self) -> Option<Completion> {
        let (ticket, asked) = self.queued.pop_front()?;
        Some(Completion {
            ticket,
            asked,
            reply: Err(anyhow!("{}: the stream's transport stopped", self.path)),
            took: Duration::ZERO,
            at: Instant::now(),
        })
    }

    /// Tests: the driver stops until the returned sender sends (or is dropped). Requests queued
    /// meanwhile wait behind the pause.
    #[cfg(test)]
    pub(crate) fn pause(&mut self) -> mpsc::Sender<()> {
        let (go, wait) = mpsc::channel();
        let _ = self.cmds.send(Cmd::Pause(wait));
        go
    }

    /// Tests: whether an end (close, drop, `into_connection`) has been signalled to the driver.
    #[cfg(test)]
    pub(crate) fn end_signalled(&self) -> impl Fn() -> bool + Send + 'static {
        let flag = self.finish.clone();
        move || flag.load(Ordering::SeqCst) != RUNNING
    }

    /// Tests: the watch on the transport thread's end.
    #[cfg(test)]
    pub(crate) fn teardown(&self) -> Teardown {
        self.ended.clone()
    }

    /// Ends the transport's thread as a panic would (tests).
    #[cfg(test)]
    pub(crate) fn kill_transport(&mut self) {
        let thread = self.thread.take().expect("a live transport");
        let _ = self.cmds.send(Cmd::Panic);
        assert!(thread.join().is_err(), "the transport's thread panicked");
    }
}

impl Ranged for SftpRanged {
    fn path(&self) -> &str {
        &self.path
    }

    fn opened(&self) -> Stat {
        self.opened
    }

    fn read(&mut self, off: u64, len: u32) -> Ticket {
        let len = clipped(self.opened.size, off, len);
        self.send(Asked::Read { off, len })
    }

    fn fstat(&mut self) -> Ticket {
        self.send(Asked::Fstat)
    }

    fn stat_path(&mut self) -> Ticket {
        self.send(Asked::StatPath)
    }

    fn pending(&self) -> usize {
        self.queued.len()
    }

    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        use mpsc::RecvTimeoutError;
        loop {
            if self.gone {
                return self.orphan();
            }
            let got = match timeout {
                Some(t) => self.events.recv_timeout(t),
                None => self
                    .events
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match got {
                Ok(Event::Done(c)) => {
                    self.queued.pop_front();
                    return Some(c);
                }
                Ok(Event::Wake) => {
                    self.woken.store(false, Ordering::SeqCst);
                    return None;
                }
                // Every completion it sent came before this, in order: what is still queued
                // will never be answered.
                Ok(Event::Gone) | Err(RecvTimeoutError::Disconnected) => self.gone = true,
                Err(RecvTimeoutError::Timeout) => return None,
            }
        }
    }

    fn waker(&self) -> Wake {
        self.wake.clone()
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        self.into_connection().into_ranged(path)
    }

    /// Does not wait for the server: the handle's CLOSE is written to the session before this
    /// returns, and its reply is read by the session's next use. When the transport's thread stopped (a bug) it took the session with
    /// it: the connection given back is [`Lost`], which says so at every call.
    fn into_connection(mut self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        let lost = format!(
            "{}: the stream's transport stopped and took its SFTP session with it",
            self.path
        );
        let Some(thread) = self.thread.take() else {
            return Box::new(Lost(lost));
        };
        self.end(KEEP);
        match thread.join() {
            Ok(Some(session)) => Box::new(SftpRemote {
                rt: Some(session.rt),
                sftp: Some(session.sftp),
                fs: Some(session.fs),
                #[cfg(test)]
                stats: session.stats,
            }),
            Ok(None) | Err(_) => Box::new(Lost(lost)),
        }
    }

    fn close(self: Box<Self>) {}
}

impl Drop for SftpRanged {
    /// The close runs on the transport's thread, bounded; nothing here waits for it (06 §9).
    fn drop(&mut self) {
        if self.thread.take().is_some() {
            self.end(CLOSE);
        }
    }
}

/// The transport's thread: drives the session until it is told to finish, then closes the
/// handle — and the session, or gives it back.
fn transport(
    session: Session,
    file: File,
    path: String,
    cmds: queue::UnboundedReceiver<Cmd>,
    finish: Arc<AtomicU8>,
    events: mpsc::Sender<Event>,
    timing: Timing,
) -> Option<Session> {
    let Session {
        rt,
        sftp,
        fs,
        #[cfg(test)]
        stats,
    } = session;
    let kept = rt.block_on(async move {
        let mut d = Driver {
            path,
            file,
            fs,
            cmds,
            finish,
            events,
            flight: Vec::new(),
            answered: BTreeMap::new(),
            next_out: 0,
            deadline: Box::pin(tokio::time::sleep(timing.stall)),
            timing,
        };
        let finish = std::future::poll_fn(|cx| d.poll(cx)).await;
        let Driver {
            file, fs, flight, ..
        } = d;
        // Every request still out holds a clone of the handle (and a stat one of `fs`), and
        // `Sftp::close` waits while any clone lives (sftp.rs:305-330, auxiliary.rs:162-169):
        // they go first.
        drop(flight);
        match finish {
            // The last clone's drop queues the CLOSE and leaves a task on this runtime for its
            // reply. The CLOSE is written now, before the session goes back idle; its reply is
            // read by the session's next use (or `Sftp::close`). Nothing waits for the server.
            Finish::Keep => {
                drop(file);
                let flushed = async {
                    tokio::time::sleep(FLUSH_INTERVAL * 4).await;
                    for _ in 0..FLUSH_PASSES {
                        tokio::task::yield_now().await;
                    }
                };
                let _ = tokio::time::timeout(FLUSH_BOUND, flushed).await;
                Some((sftp, fs))
            }
            Finish::Close => {
                drop(file);
                drop(fs);
                let _ = tokio::time::timeout(timing.close, sftp.close()).await;
                None
            }
        }
    });
    kept.map(|(sftp, fs)| Session {
        rt,
        sftp,
        fs,
        #[cfg(test)]
        stats,
    })
}

type Answer = Pin<Box<dyn Future<Output = Result<Reply>>>>;

struct Out {
    ticket: Ticket,
    asked: Asked,
    sent: Instant,
    /// `sent` + the stall bound: the request fails then, whatever else is answered meanwhile.
    due: tokio::time::Instant,
    answer: Answer,
}

struct Driver {
    path: String,
    file: File,
    fs: Fs,
    cmds: queue::UnboundedReceiver<Cmd>,
    finish: Arc<AtomicU8>,
    events: mpsc::Sender<Event>,
    /// Requests sent and not answered, in the order sent — so the first is due first.
    flight: Vec<Out>,
    /// Completions waiting for an earlier ticket's.
    answered: BTreeMap<u64, Completion>,
    next_out: u64,
    /// Set to the first request's `due`.
    deadline: Pin<Box<tokio::time::Sleep>>,
    timing: Timing,
}

impl Driver {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Finish> {
        loop {
            match self.finish.load(Ordering::SeqCst) {
                KEEP => return Poll::Ready(Finish::Keep),
                CLOSE => return Poll::Ready(Finish::Close),
                _ => {}
            }
            match self.cmds.poll_recv(cx) {
                Poll::Ready(Some(Cmd::Send(ticket, asked))) => self.start(ticket, asked, cx),
                Poll::Ready(Some(Cmd::Finish)) => {}
                #[cfg(test)]
                Poll::Ready(Some(Cmd::Panic)) => panic!("the transport's thread, killed by a test"),
                #[cfg(test)]
                Poll::Ready(Some(Cmd::Pause(wait))) => {
                    let _ = wait.recv();
                }
                Poll::Ready(None) => return Poll::Ready(Finish::Close),
                Poll::Pending => break,
            }
        }
        let mut flight = std::mem::take(&mut self.flight);
        flight.retain_mut(|out| match out.answer.as_mut().poll(cx) {
            Poll::Ready(reply) => {
                self.complete(out.ticket, out.asked, out.sent, reply);
                false
            }
            Poll::Pending => true,
        });
        self.flight = flight;
        // Each request fails at its own deadline; the first sent is due first.
        while let Some(due) = self.flight.first().map(|out| out.due) {
            if due > tokio::time::Instant::now() {
                if self.deadline.deadline() != due {
                    self.deadline.as_mut().reset(due);
                }
                if self.deadline.as_mut().poll(cx).is_pending() {
                    break;
                }
            }
            let out = self.flight.remove(0);
            let verb = match out.asked {
                Asked::Read { .. } => "read",
                Asked::Fstat | Asked::StatPath => "stat",
            };
            let e = timed_out(&format!("{verb} {}", self.path), self.timing.stall);
            self.complete(out.ticket, out.asked, out.sent, Err(e));
        }
        while let Some(c) = self.answered.remove(&self.next_out) {
            self.next_out += 1;
            let _ = self.events.send(Event::Done(c));
        }
        Poll::Pending
    }

    /// Builds the request and polls it once, which writes it: before anything queued after it.
    fn start(&mut self, ticket: Ticket, asked: Asked, cx: &mut Context<'_>) {
        let path = self.path.clone();
        let answer: Answer = match asked {
            Asked::Read { len: 0, .. } => {
                let nothing = Ok(Reply::Data(Bytes::new()));
                self.complete(ticket, asked, Instant::now(), nothing);
                return;
            }
            Asked::Read { off, len } => {
                let read = read_at(&self.file, off, len);
                Box::pin(async move {
                    match read.await {
                        Ok(got) => Ok(Reply::Data(got.map_or_else(Bytes::new, BytesMut::freeze))),
                        Err(e) => Err(anyhow!("cannot read {path}: {e}")),
                    }
                })
            }
            Asked::Fstat => {
                let mut file = self.file.clone();
                Box::pin(async move {
                    match file.metadata().await {
                        Ok(meta) => Ok(Reply::Stat(Some(stat_of(&meta)))),
                        Err(e) => Err(anyhow!("cannot stat {path}: {e}")),
                    }
                })
            }
            Asked::StatPath => {
                let mut fs = self.fs.clone();
                Box::pin(async move {
                    match fs.metadata(&path).await {
                        Ok(meta) => Ok(Reply::Stat(Some(stat_of(&meta)))),
                        Err(e) if is_not_found(&e) => Ok(Reply::Stat(None)),
                        Err(e) => Err(anyhow!("cannot stat {path}: {e}")),
                    }
                })
            }
        };
        let mut out = Out {
            ticket,
            asked,
            sent: Instant::now(),
            due: tokio::time::Instant::now() + self.timing.stall,
            answer,
        };
        match out.answer.as_mut().poll(cx) {
            Poll::Ready(reply) => self.complete(ticket, asked, out.sent, reply),
            Poll::Pending => self.flight.push(out),
        }
    }

    fn complete(&mut self, ticket: Ticket, asked: Asked, sent: Instant, reply: Result<Reply>) {
        let at = Instant::now();
        let c = Completion {
            ticket,
            asked,
            reply,
            took: at - sent,
            at,
        };
        self.answered.insert(ticket.0, c);
    }
}
