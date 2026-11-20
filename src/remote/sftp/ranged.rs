//! [`Ranged`] over SFTP (06 §2). The connection the open made becomes the transport: the file is
//! opened on it once and its handle `fstat`ed, then a thread of its own drives the session's
//! current-thread runtime. One task there sends every request in the order queued — the crate
//! writes a request when its future is first polled, so requests polled from different tasks
//! could overtake each other — and hands the completions back in that order.

use super::{
    is_not_found, read_at, stat_of, timed_out, within, SftpRemote, META_TIMEOUT, STALL_TIMEOUT,
};
use crate::remote::ranged::{clipped, Asked, Completion, Ranged, Refused, Reply, Ticket, Wake};
use crate::remote::{RemoteFs, Stat};
use anyhow::{anyhow, bail, Result};
use bytes::{Bytes, BytesMut};
use openssh_sftp_client::file::File;
use openssh_sftp_client::fs::Fs;
use openssh_sftp_client::Sftp;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc as queue;

/// The transport's bounds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// How long a close waits for the server: the handle's when the connection is given back,
    /// `Sftp::close` when the transport closes (06 §2: 5 s).
    pub close: Duration,
    /// Requests are out and no reply at all comes for this long: each of them fails.
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
    Finish(Finish),
    /// Tests: the transport's thread panics.
    #[cfg(test)]
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// Close the handle and give the session back.
    Keep,
    /// Close the handle and the session.
    Close,
}

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
    events: mpsc::Receiver<Event>,
    wake: Wake,
    woken: Arc<AtomicBool>,
    next: u64,
    /// Requests queued and not yet returned by `next`, in order.
    queued: VecDeque<(Ticket, Asked)>,
    /// The transport's thread is gone: every request still queued fails, in its turn.
    gone: bool,
    thread: Option<std::thread::JoinHandle<Option<Session>>>,
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
        let (events_tx, events) = mpsc::channel();
        let wake_tx = events_tx.clone();
        let owned = path.to_string();
        let spawned = std::thread::Builder::new()
            .name("sopen-stream".into())
            .spawn(move || {
                let _gone = Gone(events_tx.clone());
                let (session, file) = handed.recv().ok()?;
                transport(session, file, owned, cmd_rx, events_tx, timing)
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
            events,
            wake,
            woken,
            next: 0,
            queued: VecDeque::new(),
            gone: false,
            thread: Some(thread),
        })
    }

    fn send(&mut self, asked: Asked) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        self.queued.push_back((ticket, asked));
        let _ = self.cmds.send(Cmd::Send(ticket, asked));
        ticket
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

    /// Ends the transport's thread as a panic would (tests).
    #[cfg(test)]
    pub(crate) fn kill_transport(&mut self) {
        let thread = self.thread.take().expect("a live transport");
        let _ = self.cmds.send(Cmd::Panic);
        assert!(thread.join().is_err(), "the transport's thread panicked");
    }

    /// Ends the transport and waits for its thread (tests: the close's bound).
    #[cfg(test)]
    pub(crate) fn close_and_wait(mut self) {
        let thread = self.thread.take().expect("a live transport");
        let _ = self.cmds.send(Cmd::Finish(Finish::Close));
        thread.join().expect("the transport's thread");
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

    fn into_connection(mut self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        let thread = self.thread.take().expect("a live transport");
        let _ = self.cmds.send(Cmd::Finish(Finish::Keep));
        let session = match thread.join() {
            Ok(session) => session.expect("the transport gives its session back"),
            Err(panic) => std::panic::resume_unwind(panic),
        };
        Box::new(SftpRemote {
            rt: Some(session.rt),
            sftp: Some(session.sftp),
            fs: Some(session.fs),
            #[cfg(test)]
            stats: session.stats,
        })
    }

    fn close(self: Box<Self>) {}
}

impl Drop for SftpRanged {
    /// The close runs on the transport's thread, bounded; nothing here waits for it (06 §9).
    fn drop(&mut self) {
        if self.thread.take().is_some() {
            let _ = self.cmds.send(Cmd::Finish(Finish::Close));
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
            events,
            flight: Vec::new(),
            answered: BTreeMap::new(),
            next_out: 0,
            stall: Box::pin(tokio::time::sleep(timing.stall)),
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
            Finish::Keep => {
                let _ = tokio::time::timeout(timing.close, file.close()).await;
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
    answer: Answer,
}

struct Driver {
    path: String,
    file: File,
    fs: Fs,
    cmds: queue::UnboundedReceiver<Cmd>,
    events: mpsc::Sender<Event>,
    /// Requests sent and not answered, in the order sent.
    flight: Vec<Out>,
    /// Completions waiting for an earlier ticket's.
    answered: BTreeMap<u64, Completion>,
    next_out: u64,
    /// Fires when requests are out and none has been answered for `timing.stall`.
    stall: Pin<Box<tokio::time::Sleep>>,
    timing: Timing,
}

impl Driver {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Finish> {
        loop {
            match self.cmds.poll_recv(cx) {
                Poll::Ready(Some(Cmd::Send(ticket, asked))) => self.start(ticket, asked, cx),
                Poll::Ready(Some(Cmd::Finish(f))) => return Poll::Ready(f),
                #[cfg(test)]
                Poll::Ready(Some(Cmd::Panic)) => panic!("the transport's thread, killed by a test"),
                Poll::Ready(None) => return Poll::Ready(Finish::Close),
                Poll::Pending => break,
            }
        }
        let mut flight = std::mem::take(&mut self.flight);
        let mut replied = false;
        flight.retain_mut(|out| match out.answer.as_mut().poll(cx) {
            Poll::Ready(reply) => {
                self.complete(out.ticket, out.asked, out.sent, reply);
                replied = true;
                false
            }
            Poll::Pending => true,
        });
        self.flight = flight;
        if replied {
            self.rearm();
        }
        if !self.flight.is_empty() && self.stall.as_mut().poll(cx).is_ready() {
            for out in std::mem::take(&mut self.flight) {
                let verb = match out.asked {
                    Asked::Read { .. } => "read",
                    Asked::Fstat | Asked::StatPath => "stat",
                };
                let e = timed_out(&format!("{verb} {}", self.path), self.timing.stall);
                self.complete(out.ticket, out.asked, out.sent, Err(e));
            }
        }
        while let Some(c) = self.answered.remove(&self.next_out) {
            self.next_out += 1;
            let _ = self.events.send(Event::Done(c));
        }
        Poll::Pending
    }

    fn rearm(&mut self) {
        let at = tokio::time::Instant::now() + self.timing.stall;
        self.stall.as_mut().reset(at);
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
        if self.flight.is_empty() {
            self.rearm();
        }
        let mut out = Out {
            ticket,
            asked,
            sent: Instant::now(),
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
