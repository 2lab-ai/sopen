//! The byte transport of a streamed file (06 §2): [`Ranged`], one open handle on one connection,
//! taking reads at explicit offsets and status requests, and sending them in the order they were
//! queued. Its SFTP implementation is `sftp/ranged.rs`; here are the trait, the plan both
//! pipelined whole-file reads follow ([`Pipeline`]), and — for tests — the in-order `Fake` and the
//! local implementation over a `File`.
//!
//! A `Ranged` reports what the server answered, no more: a read's reply may be short, or empty
//! at the end of the file. Following a short reply with a read of the rest, and validating every
//! batch with an `fstat` sent behind it, is the stream's (06 §4, §5); the whole-file reads here
//! follow short replies themselves.

use super::{CreateError, Created, Entry, Like, RemoteFs, Stat, Uploaded};
use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A request's place in the order its [`Ranged`] sends them: 0, 1, 2, … Completions come back in
/// this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticket(pub u64);

/// What a request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// `len` bytes at `off`, after clipping to the size at open (0: nothing was sent).
    Read { off: u64, len: u32 },
    /// `fstat` of the open handle: the file being read, whatever the path names now.
    Fstat,
    /// `stat` of the path: what the name points at now.
    StatPath,
}

#[derive(Debug)]
pub enum Reply {
    /// A read's bytes, from the offset it asked. Fewer than asked is a short reply: the rest is
    /// read at its own offset. None at all is the end of the file (`SSH_FX_EOF`, or a reply of
    /// zero bytes): for a read inside the size at open, the file is shorter than it was.
    Data(Bytes),
    /// `fstat` or `stat`; `None` when the path names nothing.
    Stat(Option<Stat>),
}

/// The answer to one request.
#[derive(Debug)]
pub struct Completion {
    pub ticket: Ticket,
    pub asked: Asked,
    /// An SFTP status error, the connection closing, or no reply in time, as the error.
    pub reply: Result<Reply>,
    /// From the request leaving to its reply.
    pub took: Duration,
    /// When the reply came, on the transport's clock ([`Ranged::now`]).
    pub at: Instant,
}

impl Completion {
    /// A read's bytes; `None` for a status request or a failure.
    pub fn data(&self) -> Option<&Bytes> {
        match &self.reply {
            Ok(Reply::Data(b)) => Some(b),
            _ => None,
        }
    }

    /// A read inside the size at open that came back empty: the file ends before `off`.
    pub fn early_end(&self) -> bool {
        matches!(self.asked, Asked::Read { len, .. } if len > 0)
            && self.data().is_some_and(|b| b.is_empty())
    }
}

/// Makes a [`Ranged::next`] waiting on another thread return `None` at once (or the next one,
/// when none is waiting). Cheap to clone.
#[derive(Clone)]
pub struct Wake(Arc<dyn Fn() + Send + Sync>);

impl Wake {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn wake(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wake")
    }
}

/// A connection that could not become a [`Ranged`], given back unchanged (06 §2), and why.
pub struct Refused {
    pub conn: Box<dyn RemoteFs + Send>,
    pub error: anyhow::Error,
}

impl std::fmt::Debug for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Refused({:#})", self.error)
    }
}

/// One file open for streaming on one connection (06 §2). Requests are queued from one thread
/// and leave in the order queued; a status request queued after reads is written after them.
/// Completions come back through [`Ranged::next`] in that same order.
pub trait Ranged: Send {
    fn path(&self) -> &str;
    /// The handle's `fstat` at open: the fingerprint's `(size, mtime)` (06 §5).
    fn opened(&self) -> Stat;
    fn size(&self) -> u64 {
        self.opened().size
    }
    /// Queues a read of `len` bytes at `off`, clipped to the size at open; a read clipped to
    /// nothing is answered with no bytes and sends no request.
    fn read(&mut self, off: u64, len: u32) -> Ticket;
    /// Queues an `fstat` of the handle (the validation fence of 06 §5, and half of the poll).
    fn fstat(&mut self) -> Ticket;
    /// Queues a `stat` of the path (the other half of the poll).
    fn stat_path(&mut self) -> Ticket;
    /// Requests queued whose completion [`Ranged::next`] has not returned yet.
    fn pending(&self) -> usize;
    /// The next completion, waiting up to `timeout` (`None`: until one comes). `None` when the
    /// time ran out, a [`Wake`] fired, or nothing more can come (the transport is gone).
    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion>;
    fn waker(&self) -> Wake;
    /// The transport's clock: `Instant::now()`, or the `Fake`'s simulated time.
    fn now(&self) -> Instant;
    /// Closes the handle (bounded) and opens `path` on the same connection. A failure gives the
    /// connection back.
    fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused>;
    /// Closes the handle (bounded) and gives the connection back.
    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send>;
    /// Drops what is queued and closes the handle and the connection, without waiting: the
    /// close runs bounded on the transport's own thread (06 §2, §9). Dropping does the same.
    fn close(self: Box<Self>);
}

/// The connection a transport gives back when its thread stopped (a bug) and took the session
/// with it: every call fails with the reason, and nothing is sent anywhere. The owner's
/// reconnect makes a new one.
pub(crate) struct Lost(pub(crate) String);

impl RemoteFs for Lost {
    fn stat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        bail!("{}", self.0)
    }
    fn lstat_opt(&mut self, _: &str) -> Result<Option<Stat>> {
        bail!("{}", self.0)
    }
    fn realpath(&mut self, _: &str) -> Result<String> {
        bail!("{}", self.0)
    }
    fn read_head(&mut self, _: &str, _: usize) -> Result<Vec<u8>> {
        bail!("{}", self.0)
    }
    fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
        bail!("{}", self.0)
    }
    fn list_entries(&mut self, _: &str) -> Result<Vec<Entry>> {
        bail!("{}", self.0)
    }
    fn read_to(
        &mut self,
        _: &str,
        _: &mut dyn std::io::Write,
        _: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        bail!("{}", self.0)
    }
    fn create_from(
        &mut self,
        _: &std::path::Path,
        _: &str,
        _: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        Err(CreateError::new(Created::No, anyhow!("{}", self.0)))
    }
    fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
        bail!("{}", self.0)
    }
    fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
        bail!("{}", self.0)
    }
    fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
        bail!("{}", self.0)
    }
    fn remove(&mut self, _: &str) -> Result<()> {
        bail!("{}", self.0)
    }
    fn into_ranged(self: Box<Self>, _: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        let error = anyhow!("{}", self.0);
        Err(Refused { conn: self, error })
    }
}

impl std::fmt::Debug for dyn Ranged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ranged({}, {} bytes)", self.path(), self.size())
    }
}

/// Bytes each request of a whole-file read asks: the stream's block (06 §3). At equal bytes in
/// flight the request size does not change throughput (*view* §2.2), and it stays under any
/// server's read limit that OpenSSH ships (261 120), so a full reply needs no follow-up.
pub(crate) const PIPE_REQUEST: u32 = 64 * 1024;
/// At most this many bytes asked and not yet answered (06 §2).
pub(crate) const PIPE_IN_FLIGHT: u64 = 4 * 1024 * 1024;
/// A short reply mid-file of at least this many bytes is taken as the server's read limit, and
/// later requests ask that much: a server that clips one reply clips them all. Shorter ones are
/// followed but not learned from.
const LEARN_MIN: u32 = 16 * 1024;

/// The plan of one pipelined whole-file read (06 §2), with no I/O: which request to send next,
/// and the bytes to deliver in file order. Every request carries its own offset; a short reply
/// is followed by a request for the rest; an empty reply ends the file. The window starts at
/// one request and grows by one per reply, as OpenSSH's `sftp` does, up to [`PIPE_IN_FLIGHT`].
pub(crate) struct Pipeline {
    next: u64,
    end: Option<u64>,
    ask: u32,
    window: usize,
    slots: VecDeque<Slot>,
    ended: bool,
}

struct Slot {
    off: u64,
    len: u32,
    state: SlotState,
}

enum SlotState {
    /// The rest of a short reply, not asked yet.
    Wanted,
    Sent,
    Got(Bytes),
    End,
}

impl Pipeline {
    /// A read from offset 0 to the end of the file, or to `end` bytes at most.
    pub(crate) fn new(end: Option<u64>) -> Self {
        Self {
            next: 0,
            end,
            ask: PIPE_REQUEST,
            window: 1,
            slots: VecDeque::new(),
            ended: false,
        }
    }

    /// Tests: the plan of a read from `off` on, as if everything before it had been delivered.
    #[cfg(test)]
    pub(crate) fn starting_at(mut self, off: u64) -> Self {
        self.next = off;
        self
    }

    /// The next request to send, `(offset, length)`, if one may go now. The rest of a short
    /// reply goes first, ahead of the window: it replaces bytes of a request already answered.
    pub(crate) fn send(&mut self) -> Option<(u64, u32)> {
        if let Some(s) = self
            .slots
            .iter_mut()
            .find(|s| matches!(s.state, SlotState::Wanted))
        {
            s.state = SlotState::Sent;
            return Some((s.off, s.len));
        }
        if self.exhausted() {
            return None;
        }
        let (sent, bytes) = self.in_flight();
        // What is left to ask: up to `end`, or up to the last offset there is.
        let room = self.end.unwrap_or(u64::MAX) - self.next;
        let len = room.min(self.ask as u64) as u32;
        if sent >= self.window || (sent > 0 && bytes + len as u64 > PIPE_IN_FLIGHT) {
            return None;
        }
        self.slots.push_back(Slot {
            off: self.next,
            len,
            state: SlotState::Sent,
        });
        self.next += len as u64;
        Some((self.next - len as u64, len))
    }

    /// The reply to the request at `off`: its bytes, `None` or empty at the end of the file.
    pub(crate) fn reply(&mut self, off: u64, got: Option<Bytes>) -> Result<()> {
        let Some(i) = self
            .slots
            .iter()
            .position(|s| s.off == off && matches!(s.state, SlotState::Sent))
        else {
            bail!("a reply for a read at {off} that was not asked");
        };
        let len = self.slots[i].len;
        match got {
            Some(b) if b.len() > len as usize => {
                bail!("the server sent {} bytes for a read of {len}", b.len())
            }
            Some(b) if !b.is_empty() => {
                let n = b.len() as u32;
                self.slots[i].state = SlotState::Got(b);
                if n < len {
                    self.slots[i].len = n;
                    self.slots.insert(
                        i + 1,
                        Slot {
                            off: off + n as u64,
                            len: len - n,
                            state: SlotState::Wanted,
                        },
                    );
                    if n >= LEARN_MIN {
                        self.ask = self.ask.min(n);
                    }
                }
            }
            _ => {
                self.slots[i].state = SlotState::End;
                self.ended = true;
            }
        }
        self.window = (self.window + 1).min((PIPE_IN_FLIGHT / LEARN_MIN as u64) as usize);
        Ok(())
    }

    /// The next bytes in file order, once every byte before them has been delivered.
    pub(crate) fn take(&mut self) -> Option<Bytes> {
        if !matches!(self.slots.front()?.state, SlotState::Got(_)) {
            return None;
        }
        match self.slots.pop_front()?.state {
            SlotState::Got(b) => Some(b),
            _ => unreachable!(),
        }
    }

    /// Nothing more to ask: the file ended, `end` was reached, or the offsets ran out.
    fn exhausted(&self) -> bool {
        self.ended || self.next >= self.end.unwrap_or(u64::MAX)
    }

    /// Everything up to the end of the file (or `end`) has been delivered.
    pub(crate) fn done(&self) -> bool {
        match self.slots.front() {
            Some(s) => matches!(s.state, SlotState::End),
            None => self.exhausted(),
        }
    }

    /// Requests sent and not answered, and the bytes they asked.
    pub(crate) fn in_flight(&self) -> (usize, u64) {
        self.slots
            .iter()
            .filter(|s| matches!(s.state, SlotState::Sent))
            .fold((0, 0), |(n, b), s| (n + 1, b + s.len as u64))
    }
}

/// The length a read at `off` keeps once clipped to `size` (06 §2: every read is clipped to the
/// fingerprint's size).
pub(crate) fn clipped(size: u64, off: u64, len: u32) -> u32 {
    size.saturating_sub(off).min(len as u64) as u32
}

#[cfg(test)]
pub(crate) use fake::*;

#[cfg(test)]
mod fake {
    //! The in-order `Fake` (06 §2) and the local implementation over a `File`, which is the
    //! same queue answering from the local disk with no delay.

    use super::*;
    use crate::remote::local::stat_of;
    use crate::remote::LocalFs;
    use anyhow::Context;
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// What a status request counts, in bytes (06 §4 counts about 100 of reply).
    pub(crate) const STAT_REPLY: u64 = 100;

    /// The Fake's link. Every reply leaves `latency` after its request (a round trip), then
    /// crosses at `rate` bytes/s behind every reply before it: the server answers in order.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Link {
        pub latency: Duration,
        pub rate: u64,
        /// Every read's reply cut to this many bytes.
        pub clip: Option<u32>,
        pub clock: Clock,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Clock {
        /// Time passes only where the consumer waits ([`Ranged::next`]): gates assert it, never
        /// wall-clock time (06 §12).
        Simulated,
        /// `next` sleeps until the reply is due.
        Real,
    }

    impl Link {
        /// 06 §12's gate link: in order, 20 ms, 5 MB/s, simulated time.
        pub(crate) fn gate() -> Self {
            Self {
                latency: Duration::from_millis(20),
                rate: 5_000_000,
                clip: None,
                clock: Clock::Simulated,
            }
        }

        /// No delay at all, in real time.
        pub(crate) fn instant() -> Self {
            Self {
                latency: Duration::ZERO,
                rate: u64::MAX,
                clip: None,
                clock: Clock::Real,
            }
        }

        fn transfer(&self, bytes: u64) -> Duration {
            if self.rate == u64::MAX {
                return Duration::ZERO;
            }
            Duration::from_secs_f64(bytes as f64 / self.rate as f64)
        }
    }

    /// The byte at `off` of a patterned file: a byte of the offset of the 8-byte word it is in,
    /// so a read placed anywhere else cannot match.
    pub(crate) fn pattern_byte(off: u64) -> u8 {
        (off & !7).to_le_bytes()[(off & 7) as usize]
    }

    /// `len` bytes of the pattern from `off`, a word at a time.
    pub(crate) fn pattern(off: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 16);
        let mut word = off & !7;
        while out.len() < len + (off - (off & !7)) as usize {
            out.extend_from_slice(&word.to_le_bytes());
            word += 8;
        }
        out.drain(..(off & 7) as usize);
        out.truncate(len);
        out
    }

    /// The Fake's file: `size` bytes of [`pattern_byte`] (or given bytes) with writes over them.
    #[derive(Debug, Clone)]
    pub(crate) struct FakeFile {
        pub size: u64,
        pub mtime: SystemTime,
        base: Option<Arc<Vec<u8>>>,
        /// The base reads as zeros from here (a cut, or a write past the end, left a gap).
        zero_from: Option<u64>,
        writes: Vec<(u64, Vec<u8>)>,
    }

    impl FakeFile {
        pub(crate) fn pattern(size: u64) -> Self {
            Self {
                size,
                mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                base: None,
                zero_from: None,
                writes: Vec::new(),
            }
        }

        pub(crate) fn bytes(bytes: Vec<u8>) -> Self {
            Self {
                size: bytes.len() as u64,
                base: Some(Arc::new(bytes)),
                ..Self::pattern(0)
            }
        }

        /// From `at` on, the base reads as zeros.
        fn zero_from(&mut self, at: u64) {
            self.zero_from = Some(self.zero_from.map_or(at, |z| z.min(at)));
        }

        /// `len` bytes at `off`, fewer at the end.
        pub(crate) fn read(&self, off: u64, len: u64) -> Vec<u8> {
            let end = self.size.min(off.saturating_add(len)).max(off);
            let n = (end - off) as usize;
            let mut out = match &self.base {
                Some(b) => {
                    let at = (off as usize).min(b.len());
                    let mut v = b[at..(at + n).min(b.len())].to_vec();
                    v.resize(n, 0);
                    v
                }
                None => pattern(off, n),
            };
            if let Some(z) = self.zero_from.filter(|z| *z < end) {
                out[z.saturating_sub(off) as usize..].fill(0);
            }
            for (at, bytes) in &self.writes {
                for (i, b) in bytes.iter().enumerate() {
                    let o = at + i as u64;
                    if o >= off && o < end {
                        out[(o - off) as usize] = *b;
                    }
                }
            }
            out
        }

        pub(crate) fn stat(&self) -> Stat {
            Stat {
                is_dir: false,
                is_file: true,
                is_symlink: false,
                size: self.size,
                mtime: self.mtime,
                mode: 0o644,
                uid: Some(1000),
                gid: Some(1000),
                missing: None,
                sha256: None,
            }
        }
    }

    /// A change to the file the path names.
    #[derive(Debug, Clone)]
    pub(crate) enum Change {
        /// Bytes written in place. The mtime moves a second on, or stays (`same_second`: the
        /// same-size write within the second that SFTP cannot see, 06 §5).
        Write {
            off: u64,
            bytes: Vec<u8>,
            same_second: bool,
        },
        /// Cut (or extended) to this length; the mtime moves a second on.
        Truncate(u64),
        /// Another file renamed over the path: open handles keep reading the old one.
        Replace(FakeFile),
        /// The path removed: open handles keep reading.
        Remove,
    }

    /// A failure injected at one request.
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum Fault {
        /// The server answers with this status error.
        Status(&'static str),
        /// The channel closes: this request and every later one on it fail.
        Lost,
    }

    /// One request as the Fake received it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct SentRecord {
        pub ticket: Ticket,
        pub asked: Asked,
        /// When it was sent, since the Fake was made (simulated or real).
        pub at: Duration,
        /// Bytes asked by earlier requests on its channel whose completion [`Ranged::next`] had
        /// not returned when it was sent — a read's length, [`STAT_REPLY`] for a status request.
        /// What a scheduler admits against (06 §4's `in_flight`).
        pub outstanding: u64,
        /// The physical backlog: reply bytes of earlier requests still crossing the link when it
        /// was sent (a clipped reply counts what was sent, not what was asked).
        pub backlog: u64,
    }

    /// What every open handle on the Fake reads from.
    struct World {
        link: Link,
        base: Instant,
        now: Duration,
        path: String,
        files: Vec<FakeFile>,
        /// The file the path names; `None` once removed. `usize::MAX`: a local file (`disk`).
        at_path: Option<usize>,
        disk: Option<PathBuf>,
        requests: usize,
        sent: Vec<SentRecord>,
        faults: Vec<(usize, Fault)>,
        changes: Vec<(usize, Change)>,
        /// Fakes blocked in `next` right now.
        waiting: usize,
    }

    impl World {
        fn now(&self) -> Duration {
            match self.link.clock {
                Clock::Simulated => self.now,
                Clock::Real => self.base.elapsed(),
            }
        }

        fn apply(&mut self, change: Change) {
            assert!(self.disk.is_none(), "change a local file on disk instead");
            let later = |t: SystemTime| t + Duration::from_secs(1);
            let in_place = matches!(change, Change::Write { .. } | Change::Truncate(_));
            let at = match (self.at_path, in_place) {
                (Some(at), _) => at,
                (None, true) => panic!("nothing at the path to change in place"),
                (None, false) => usize::MAX,
            };
            match change {
                Change::Write {
                    off,
                    bytes,
                    same_second,
                } => {
                    let file = &mut self.files[at];
                    if off > file.size {
                        file.zero_from(file.size);
                    }
                    file.size = file.size.max(off + bytes.len() as u64);
                    file.writes.push((off, bytes));
                    if !same_second {
                        file.mtime = later(file.mtime);
                    }
                }
                Change::Truncate(len) => {
                    let file = &mut self.files[at];
                    // What lay past the cut reads as zeros if the file grows again.
                    file.zero_from(len.min(file.size));
                    for (at, bytes) in &mut file.writes {
                        bytes.truncate(len.saturating_sub(*at) as usize);
                    }
                    file.size = len;
                    file.mtime = later(file.mtime);
                }
                Change::Replace(new) => {
                    self.files.push(new);
                    self.at_path = Some(self.files.len() - 1);
                }
                Change::Remove => self.at_path = None,
            }
        }
    }

    /// What a request counts against what is outstanding: a read's length, about a status
    /// reply's size for a status request.
    fn asked_bytes(asked: &Asked) -> u64 {
        match asked {
            Asked::Read { len, .. } => *len as u64,
            Asked::Fstat | Asked::StatPath => STAT_REPLY,
        }
    }

    /// What one open handle reads.
    enum Handle {
        File(usize),
        Disk(std::fs::File),
    }

    struct Due {
        ticket: Ticket,
        asked: Asked,
        sent: Duration,
        done: Duration,
        bytes: u64,
        reply: Result<Reply>,
    }

    struct Channel {
        next: u64,
        taken: u64,
        queue: VecDeque<Due>,
        link_free: Duration,
        woken: bool,
        lost: bool,
    }

    /// One handle open on the Fake: its own channel (in order, as `sftp-server`).
    pub(crate) struct Fake {
        world: Arc<Mutex<World>>,
        channel: Arc<(Mutex<Channel>, Condvar)>,
        handle: Handle,
        path: String,
        opened: Stat,
    }

    /// The test's side of a Fake: the file, the link, the request log.
    #[derive(Clone)]
    pub(crate) struct FakeControl {
        world: Arc<Mutex<World>>,
    }

    /// A Fake streaming `file` as `path` over `link`, and its control.
    pub(crate) fn fake(path: &str, file: FakeFile, link: Link) -> (Box<dyn Ranged>, FakeControl) {
        let world = Arc::new(Mutex::new(World {
            link,
            base: Instant::now(),
            now: Duration::ZERO,
            path: path.to_string(),
            files: vec![file],
            at_path: Some(0),
            disk: None,
            requests: 0,
            sent: Vec::new(),
            faults: Vec::new(),
            changes: Vec::new(),
            waiting: 0,
        }));
        let control = FakeControl { world };
        let ranged = control.open().expect("a fresh Fake opens");
        (ranged, control)
    }

    /// The local implementation (tests only: a local file never gets a stream, 06 §1): the
    /// Fake's queue answering from `path` on the local disk, at once and in real time.
    pub(crate) fn local(path: &str) -> Result<Fake> {
        let file = std::fs::File::open(path).with_context(|| format!("cannot open {path}"))?;
        let opened = stat_of(&file.metadata()?);
        if !opened.is_file {
            bail!("{path} is not a regular file");
        }
        let world = Arc::new(Mutex::new(World {
            link: Link::instant(),
            base: Instant::now(),
            now: Duration::ZERO,
            path: path.to_string(),
            files: Vec::new(),
            at_path: None,
            disk: Some(PathBuf::from(path)),
            requests: 0,
            sent: Vec::new(),
            faults: Vec::new(),
            changes: Vec::new(),
            waiting: 0,
        }));
        Ok(Fake::new(world, Handle::Disk(file), path, opened))
    }

    impl FakeControl {
        /// A new handle (a new channel) on the file the path names now.
        pub(crate) fn open(&self) -> Result<Box<dyn Ranged>> {
            let w = self.world.lock().unwrap();
            let path = w.path.clone();
            let at = w
                .at_path
                .with_context(|| format!("{path}: no such file or directory"))?;
            let opened = w.files[at].stat();
            drop(w);
            Ok(Box::new(Fake::new(
                self.world.clone(),
                Handle::File(at),
                &path,
                opened,
            )))
        }

        /// Changes the file the path names, now.
        pub(crate) fn change(&self, change: Change) {
            self.world.lock().unwrap().apply(change);
        }

        /// Changes it right before request `n` (counted from 0 over every channel) is answered.
        pub(crate) fn change_before(&self, n: usize, change: Change) {
            self.world.lock().unwrap().changes.push((n, change));
        }

        /// Request `n` gets `fault`.
        pub(crate) fn fault(&self, n: usize, fault: Fault) {
            self.world.lock().unwrap().faults.push((n, fault));
        }

        /// Changes the link for requests sent from now on (a rate drop mid-stream).
        pub(crate) fn set_link(&self, f: impl FnOnce(&mut Link)) {
            f(&mut self.world.lock().unwrap().link);
        }

        /// Every request received, in order.
        pub(crate) fn sent(&self) -> Vec<SentRecord> {
            self.world.lock().unwrap().sent.clone()
        }

        /// The file the path names (for the bytes a read should give).
        pub(crate) fn file(&self) -> Option<FakeFile> {
            let w = self.world.lock().unwrap();
            w.at_path.map(|i| w.files[i].clone())
        }

        /// Time since the Fake was made: simulated, or real.
        pub(crate) fn elapsed(&self) -> Duration {
            self.world.lock().unwrap().now()
        }

        /// Fakes blocked in [`Ranged::next`] right now (a test syncs on a wait having begun).
        pub(crate) fn waiting(&self) -> usize {
            self.world.lock().unwrap().waiting
        }
    }

    impl Fake {
        fn new(world: Arc<Mutex<World>>, handle: Handle, path: &str, opened: Stat) -> Self {
            let free = world.lock().unwrap().now();
            Self {
                world,
                channel: Arc::new((
                    Mutex::new(Channel {
                        next: 0,
                        taken: 0,
                        queue: VecDeque::new(),
                        link_free: free,
                        woken: false,
                        lost: false,
                    }),
                    Condvar::new(),
                )),
                handle,
                path: path.to_string(),
                opened,
            }
        }

        /// The server's answer to `asked`, evaluated as it arrives (in order).
        fn answer(&self, w: &mut World, ch: &mut Channel, asked: Asked) -> Result<Reply> {
            let n = w.requests;
            while let Some(i) = w.changes.iter().position(|(at, _)| *at == n) {
                let (_, change) = w.changes.remove(i);
                w.apply(change);
            }
            let fault = w
                .faults
                .iter()
                .position(|(at, _)| *at == n)
                .map(|i| w.faults.remove(i).1);
            let path = &self.path;
            if ch.lost || matches!(fault, Some(Fault::Lost)) {
                ch.lost = true;
                return Err(anyhow!("{path}: the connection closed"));
            }
            if let Some(Fault::Status(msg)) = fault {
                return Err(anyhow!("cannot read {path}: {msg}"));
            }
            match (asked, &self.handle) {
                (Asked::Read { off, len }, Handle::File(i)) => {
                    let len = w.link.clip.map_or(len, |c| c.min(len));
                    Ok(Reply::Data(w.files[*i].read(off, len as u64).into()))
                }
                (Asked::Read { off, len }, Handle::Disk(f)) => {
                    let len = w.link.clip.map_or(len, |c| c.min(len)) as usize;
                    let mut buf = vec![0; len];
                    let mut got = 0;
                    while got < len {
                        let n = f
                            .read_at(&mut buf[got..], off + got as u64)
                            .with_context(|| format!("cannot read {path}"))?;
                        if n == 0 {
                            break;
                        }
                        got += n;
                    }
                    buf.truncate(got);
                    Ok(Reply::Data(buf.into()))
                }
                (Asked::Fstat, Handle::File(i)) => Ok(Reply::Stat(Some(w.files[*i].stat()))),
                (Asked::Fstat, Handle::Disk(f)) => Ok(Reply::Stat(Some(stat_of(&f.metadata()?)))),
                (Asked::StatPath, _) => match &w.disk {
                    Some(p) => match std::fs::metadata(p) {
                        Ok(m) => Ok(Reply::Stat(Some(stat_of(&m)))),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Reply::Stat(None)),
                        Err(e) => Err(e).with_context(|| format!("cannot stat {path}")),
                    },
                    None => Ok(Reply::Stat(w.at_path.map(|i| w.files[i].stat()))),
                },
            }
        }

        fn queue(&mut self, asked: Asked) -> Ticket {
            let (lock, wakeup) = &*self.channel;
            let mut ch = lock.lock().unwrap();
            let mut w = self.world.lock().unwrap();
            let ticket = Ticket(ch.next);
            ch.next += 1;
            let sent = w.now();
            let last = ch.queue.back().map_or(sent, |d| d.done.max(sent));
            if matches!(asked, Asked::Read { len: 0, .. }) {
                // Clipped to nothing: no request, answered in its turn.
                ch.queue.push_back(Due {
                    ticket,
                    asked,
                    sent,
                    done: last,
                    bytes: 0,
                    reply: Ok(Reply::Data(Bytes::new())),
                });
                wakeup.notify_all();
                return ticket;
            }
            let reply = self.answer(&mut w, &mut ch, asked);
            w.requests += 1;
            let bytes = match &reply {
                Ok(Reply::Data(b)) => b.len() as u64,
                _ => STAT_REPLY,
            };
            let backlog = ch
                .queue
                .iter()
                .filter(|d| d.done > sent)
                .map(|d| d.bytes)
                .sum();
            let outstanding = ch.queue.iter().map(|d| asked_bytes(&d.asked)).sum();
            let done = if ch.lost {
                last
            } else {
                let start = (sent + w.link.latency).max(ch.link_free);
                start + w.link.transfer(bytes)
            };
            ch.link_free = ch.link_free.max(done);
            w.sent.push(SentRecord {
                ticket,
                asked,
                at: sent,
                outstanding,
                backlog,
            });
            ch.queue.push_back(Due {
                ticket,
                asked,
                sent,
                done,
                bytes,
                reply,
            });
            wakeup.notify_all();
            ticket
        }

        fn take(ch: &mut Channel, base: Instant) -> Completion {
            let d = ch.queue.pop_front().expect("a completion due");
            ch.taken += 1;
            Completion {
                ticket: d.ticket,
                asked: d.asked,
                reply: d.reply,
                took: d.done.saturating_sub(d.sent),
                at: base + d.done,
            }
        }
    }

    impl Ranged for Fake {
        fn path(&self) -> &str {
            &self.path
        }

        fn opened(&self) -> Stat {
            self.opened
        }

        fn read(&mut self, off: u64, len: u32) -> Ticket {
            let len = clipped(self.opened.size, off, len);
            self.queue(Asked::Read { off, len })
        }

        fn fstat(&mut self) -> Ticket {
            self.queue(Asked::Fstat)
        }

        fn stat_path(&mut self) -> Ticket {
            self.queue(Asked::StatPath)
        }

        fn pending(&self) -> usize {
            let ch = self.channel.0.lock().unwrap();
            (ch.next - ch.taken) as usize
        }

        fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
            let (lock, wakeup) = &*self.channel;
            let mut ch = lock.lock().unwrap();
            let started = Instant::now();
            loop {
                if std::mem::take(&mut ch.woken) {
                    return None;
                }
                // Lost, and every completion handed back: nothing more can come.
                if ch.lost && ch.queue.is_empty() {
                    return None;
                }
                let mut w = self.world.lock().unwrap();
                let base = w.base;
                let head = ch.queue.front().map(|d| d.done);
                let wait = match (w.link.clock, head) {
                    (Clock::Simulated, Some(done)) => {
                        let wait = done.saturating_sub(w.now);
                        if let Some(t) = timeout.filter(|t| *t < wait) {
                            w.now += t;
                            return None;
                        }
                        w.now = w.now.max(done);
                        return Some(Self::take(&mut ch, base));
                    }
                    (Clock::Simulated, None) => match timeout {
                        Some(t) => {
                            w.now += t;
                            return None;
                        }
                        None => None,
                    },
                    (Clock::Real, head) => {
                        let now = w.now();
                        if head.is_some_and(|done| done <= now) {
                            return Some(Self::take(&mut ch, base));
                        }
                        let left = timeout.map(|t| t.saturating_sub(started.elapsed()));
                        if left == Some(Duration::ZERO) {
                            return None;
                        }
                        let due = head.map(|done| done - now);
                        match (due, left) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        }
                    }
                };
                w.waiting += 1;
                drop(w);
                ch = match wait {
                    Some(d) => wakeup.wait_timeout(ch, d).unwrap().0,
                    None => wakeup.wait(ch).unwrap(),
                };
                self.world.lock().unwrap().waiting -= 1;
            }
        }

        fn waker(&self) -> Wake {
            let channel = self.channel.clone();
            Wake::new(move || {
                let (lock, wakeup) = &*channel;
                lock.lock().unwrap().woken = true;
                wakeup.notify_all();
            })
        }

        fn now(&self) -> Instant {
            let w = self.world.lock().unwrap();
            w.base + w.now()
        }

        fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
            self.into_connection().into_ranged(path)
        }

        fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
            if self.world.lock().unwrap().disk.is_some() {
                return Box::new(LocalFs::new());
            }
            Box::new(FakeConn {
                control: FakeControl {
                    world: self.world.clone(),
                },
            })
        }

        fn close(self: Box<Self>) {}
    }

    /// The connection a Fake gives back: it stats and reads the Fake's one path, opens it as a
    /// new Fake channel, and refuses everything else.
    pub(crate) struct FakeConn {
        control: FakeControl,
    }

    impl FakeConn {
        fn file(&self, path: &str) -> Result<Option<FakeFile>> {
            let w = self.control.world.lock().unwrap();
            if w.path != path {
                return Ok(None);
            }
            Ok(w.at_path.map(|i| w.files[i].clone()))
        }
    }

    fn unsupported<T>(what: &str) -> Result<T> {
        bail!("the Fake's connection cannot {what}")
    }

    impl RemoteFs for FakeConn {
        fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
            Ok(self.file(path)?.map(|f| f.stat()))
        }
        fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
            self.stat_opt(path)
        }
        fn realpath(&mut self, path: &str) -> Result<String> {
            Ok(path.to_string())
        }
        fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
            let f = self.file(path)?.context("no such file")?;
            Ok(f.read(0, max as u64))
        }
        fn list_dir(&mut self, _: &str) -> Result<Vec<String>> {
            unsupported("list")
        }
        fn list_entries(&mut self, _: &str) -> Result<Vec<Entry>> {
            unsupported("list")
        }
        fn read_to(
            &mut self,
            path: &str,
            out: &mut dyn Write,
            progress: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            let f = self.file(path)?.context("no such file")?;
            out.write_all(&f.read(0, f.size))?;
            progress(f.size);
            Ok(f.size)
        }
        fn create_from(
            &mut self,
            _: &Path,
            _: &str,
            _: Option<Like>,
        ) -> std::result::Result<Uploaded, CreateError> {
            Err(CreateError::new(
                Created::No,
                anyhow!("the Fake cannot write"),
            ))
        }
        fn set_mode(&mut self, _: &str, _: u32) -> Result<()> {
            unsupported("set a mode")
        }
        fn rename_over(&mut self, _: &str, _: &str) -> Result<()> {
            unsupported("rename")
        }
        fn link_new(&mut self, _: &str, _: &str) -> Result<()> {
            unsupported("link")
        }
        fn remove(&mut self, _: &str) -> Result<()> {
            unsupported("remove")
        }
        fn into_ranged(
            self: Box<Self>,
            path: &str,
        ) -> std::result::Result<Box<dyn Ranged>, Refused> {
            let same = self.control.world.lock().unwrap().path == path;
            let opened = match same {
                true => self.control.open(),
                false => Err(anyhow!("{path}: no such file or directory")),
            };
            opened.map_err(|error| Refused { conn: self, error })
        }
    }

    /// 06 §2's pipelined whole read driven over a [`Ranged`]: the same [`Pipeline`] that
    /// `SftpRemote::read_to` runs, so the Fake's gate (V6) measures the plan that ships.
    pub(crate) fn read_to_over(r: &mut dyn Ranged, out: &mut dyn Write) -> Result<u64> {
        let mut plan = Pipeline::new(None);
        let mut offsets = std::collections::HashMap::new();
        let mut done = 0u64;
        loop {
            while let Some((off, len)) = plan.send() {
                offsets.insert(r.read(off, len), off);
            }
            if plan.done() {
                return Ok(done);
            }
            let c = r
                .next(Some(crate::remote::sftp::STALL_TIMEOUT))
                .context("no reply")?;
            let off = offsets.remove(&c.ticket).context("a reply for no read")?;
            let Reply::Data(bytes) = c.reply? else {
                bail!("a read answered with a stat");
            };
            plan.reply(off, Some(bytes))?;
            while let Some(b) = plan.take() {
                out.write_all(&b)?;
                done += b.len() as u64;
            }
        }
    }
}
