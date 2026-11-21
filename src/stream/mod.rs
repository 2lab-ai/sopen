//! A remote file viewed as a stream (06 §3–§6): a [`Source`] is one file open on one
//! [`Ranged`](crate::remote::Ranged), with its block cache ([`BlockCache`]), its readers and
//! what they have queued ([`Prefetcher`]). Nothing is shown that was not validated: a block
//! becomes visible only once an `fstat` sent after it returned the fingerprint taken at open.
//!
//! Viewers read through [`Bytes`]: a [`Reader`] for a remote file, a `std::fs::File` for a local
//! one, which is read in place and never gets a stream (06 §1).

mod cache;
mod prefetch;
mod source;
#[cfg(test)]
mod tests;

/// Tests elsewhere in the crate step a Source by hand (`Source::build`, `Engine::step`) on the
/// `Fake`'s simulated clock; `Source::open`'s thread is for real time only.
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use source::{Engine, Flow, Settings, Traced};

pub use cache::{
    shares, BlockCache, Protect, BLOCK, CACHE_BYTES, FLOOR_AHEAD, FLOOR_BEHIND, PROTECTED_BYTES,
};
pub use prefetch::{limits, Class, Prefetcher, CAP_MAX, CAP_MIN};
pub use source::{Mark, Opening, Reader, Source, Stats, Waiting};

use crate::remote::{Completion, Ranged, Refused, RemoteFs, Stat, Ticket, Wake};
use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

/// How often a viewer on screen polls `fstat(handle)` and `stat(path)` (06 §5).
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// A failed stream is retried at most this often (06 §5).
pub const RETRY_EVERY: Duration = Duration::from_secs(1);
/// The longest span one read may ask: a quarter of the cache, so a reader's waiting blocks
/// always fit beside the other readers' windows.
pub const MAX_READ: usize = 16 << 20;

/// Called when something a viewer shows may have changed (a block arrived, the state moved):
/// egui's `request_repaint` for the viewer's context. Never called with a lock held.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Opens a new connection (the viewer's `Reconnect`) and makes it a `Ranged` of the same path:
/// what a retry after a failure runs, on the stream's own thread.
pub type Reopen = Box<dyn FnMut() -> anyhow::Result<Box<dyn Ranged>> + Send>;

/// 06 §5's states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Unchanged.
    Live,
    /// The path names another file (or nothing); the handle's file is unchanged and still read.
    Replaced,
    /// The handle's size or mtime moved: the cache is dropped and nothing more is read.
    Changed,
    /// A read got an error, or the channel closed. What is cached still reads; the next read
    /// of anything else retries, at most once per second.
    Failed(String),
    /// The viewer is gone.
    Closed,
}

impl State {
    /// The status row's words for the state (06 §5); `None` while it is `Live` or `Closed`.
    pub fn status(&self) -> Option<String> {
        match self {
            State::Live | State::Closed => None,
            State::Replaced => Some(
                "the remote file was replaced; showing the version opened — Cmd/Ctrl+R reloads"
                    .into(),
            ),
            State::Changed => Some("the file changed on the remote — Cmd/Ctrl+R reloads".into()),
            State::Failed(reason) => Some(reason.clone()),
        }
    }
}

/// A consumer with a position (06 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderKind {
    /// A viewer's screen: hex, text.
    Interactive,
    /// One HTTP connection of ffmpeg's, at the probed byte rate.
    Media { bytes_per_s: u64 },
    /// A pass over the file: an index pass, the waveform, an image read, Save As.
    Bulk,
}

/// Bytes received from the remote, refetches counted (06 §11).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fetched {
    pub total: u64,
    pub demand: u64,
    pub prefetch: u64,
    pub bulk: u64,
}

/// Why a read gave no bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The caller's cancel flag was set.
    Cancelled,
    /// The file changed on the remote (06 §5): only a reload reads again.
    Changed,
    /// The source was reloaded since this reader was made (or last renewed): it read the
    /// version before, and reads no other — [`Reader::renew`] moves it to the one open now.
    Reloaded,
    /// The source is closed.
    Closed,
    /// The stream failed; the reason is the status row's text.
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Cancelled => f.write_str("cancelled"),
            ReadError::Changed => f.write_str("the file changed on the remote"),
            ReadError::Reloaded => f.write_str("the file was reloaded"),
            ReadError::Closed => f.write_str("the stream is closed"),
            ReadError::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for ReadError {}

pub type Result<T> = std::result::Result<T, ReadError>;

/// What a viewer reads (06 §5): a [`Reader`], or a local `File` read in place (always ready).
pub trait Bytes {
    /// The file's size: for a stream, the handle's at open.
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Frames: up to `len` bytes at `off` (fewer at the end) when every block is there, else
    /// `Pending` — the fetch is queued, and the source's waker is called when it arrives.
    fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<bytes::Bytes>>;

    /// Workers and HTTP connections: blocks until `buf` is full or the file ends, and returns
    /// the count. `Err(Cancelled)` once `cancel` is set.
    fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize>;
}

fn local_read(file: &std::fs::File, off: u64, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match file.read_at(&mut buf[got..], off + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ReadError::Failed(format!("cannot read the file: {e}"))),
        }
    }
    Ok(got)
}

impl Bytes for std::fs::File {
    fn len(&self) -> u64 {
        self.metadata().map_or(0, |m| m.len())
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<bytes::Bytes>> {
        let mut buf = vec![0; len.min(MAX_READ)];
        Poll::Ready(local_read(self, off, &mut buf).map(|n| {
            buf.truncate(n);
            buf.into()
        }))
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize> {
        if cancel.load(Ordering::SeqCst) {
            return Err(ReadError::Cancelled);
        }
        local_read(self, off, buf)
    }
}

impl Bytes for Reader {
    fn len(&self) -> u64 {
        Reader::len(self)
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<bytes::Bytes>> {
        Reader::try_read(self, off, len)
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize> {
        Reader::read_at(self, off, buf, cancel)
    }
}

/// 06 §6: what a window says when sshd refuses another session on the host's connection.
pub const SESSION_REFUSED: &str = "the remote refused another SSH session (sshd allows \
     MaxSessions per connection, 10 by default, shared by every sopen window on this host) — \
     close a window there and try again";

/// Whether an error's text is ssh's report of a refused session: the master's own words for a
/// channel the server would not open, or the mux client's for the same.
pub fn session_refused(error: &str) -> bool {
    error.contains(SESSION_REFUSED)
        || error.contains("Session open refused by peer")
        || error.contains("open failed: administratively prohibited")
}

/// A connection error in the words a window shows: [`SESSION_REFUSED`] for a refused session,
/// the error itself otherwise.
pub fn explain(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    if session_refused(&text) {
        SESSION_REFUSED.to_string()
    } else {
        text
    }
}

/// `SOPEN_STREAM_THROTTLE=<ms>,<bytes/s>` (06 §11): every read delayed and paced, as the tests'
/// `Fake` does, so a screenshot can catch rows still loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttle {
    pub delay: Duration,
    pub rate: u64,
}

impl Throttle {
    /// `<ms>,<bytes/s>`; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let (ms, rate) = text.split_once(',')?;
        let rate: u64 = rate.trim().parse().ok().filter(|r| *r > 0)?;
        Some(Self {
            delay: Duration::from_millis(ms.trim().parse().ok()?),
            rate,
        })
    }
}

/// A `Ranged` whose read replies are held back: each leaves `delay` after its request and then
/// crosses at `rate` bytes/s behind the reply before it. Order is kept: a status reply waits
/// behind the reads before it.
pub(crate) struct Throttled {
    inner: Box<dyn Ranged>,
    throttle: Throttle,
    sent: HashMap<Ticket, Instant>,
    held: VecDeque<(Instant, Completion)>,
    free: Instant,
}

impl Throttled {
    pub(crate) fn wrap(inner: Box<dyn Ranged>, throttle: Throttle) -> Box<dyn Ranged> {
        let free = inner.now();
        Box::new(Self {
            inner,
            throttle,
            sent: HashMap::new(),
            held: VecDeque::new(),
            free,
        })
    }

    fn hold(&mut self, c: Completion) {
        let asked = self.sent.remove(&c.ticket);
        let bytes = c.data().map_or(0, |b| b.len() as u64);
        let start = asked.map_or(c.at, |t| t + self.throttle.delay);
        let due = start.max(self.free).max(c.at)
            + Duration::from_secs_f64(bytes as f64 / self.throttle.rate as f64);
        self.free = due;
        self.held.push_back((due, c));
    }
}

impl Ranged for Throttled {
    fn path(&self) -> &str {
        self.inner.path()
    }

    fn opened(&self) -> Stat {
        self.inner.opened()
    }

    fn read(&mut self, off: u64, len: u32) -> Ticket {
        let ticket = self.inner.read(off, len);
        self.sent.insert(ticket, self.inner.now());
        ticket
    }

    fn fstat(&mut self) -> Ticket {
        self.inner.fstat()
    }

    fn stat_path(&mut self) -> Ticket {
        self.inner.stat_path()
    }

    fn pending(&self) -> usize {
        self.inner.pending() + self.held.len()
    }

    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let started = self.inner.now();
        loop {
            let now = self.inner.now();
            let due = self.held.front().map(|(due, _)| *due);
            if due.is_some_and(|due| due <= now) {
                return self.held.pop_front().map(|(_, c)| c);
            }
            let left = timeout.map(|t| t.saturating_sub(now - started));
            if left == Some(Duration::ZERO) {
                return None;
            }
            let wait = match (due.map(|due| due - now), left) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match self.inner.next(wait) {
                Some(c) => self.hold(c),
                // The wait ran out (a held reply may be due now), a wake, or nothing more.
                None => {
                    let now = self.inner.now();
                    if self.held.front().is_some_and(|(due, _)| *due <= now) {
                        return self.held.pop_front().map(|(_, c)| c);
                    }
                    return None;
                }
            }
        }
    }

    fn waker(&self) -> Wake {
        self.inner.waker()
    }

    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        let throttle = self.throttle;
        Ok(Self::wrap(self.inner.reopen(path)?, throttle))
    }

    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        self.inner.into_connection()
    }

    fn close(self: Box<Self>) {
        self.inner.close()
    }
}
