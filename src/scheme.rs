//! The page scheme's contract (09 §3.1, X-W1's result, change 4): what an engine's glue and the
//! page server say to each other about one view's `sopen-page://<host>/<path>` requests. WV-0
//! owns it so that the page server (`stream::page`, WV-C) and the two engines' glue (WV-A, WV-B)
//! are built beside it.
//!
//! - **In**, on the engine's thread: a request starts ([`Table::start`]: its id, its path, its
//!   `Range`) and may be stopped ([`Table::stop`]) — before its head, mid-body, or after its last
//!   byte but before its end (macOS); on Linux a request stopped before its head has no signal of
//!   its own, and the view's resource failing names it by its address ([`Table::stop_headless`]).
//!   The table hands each to the [`Server`].
//! - **Out**, from the server's worker: one complete [`Head`] — status, type, length,
//!   `Content-Range` — before any byte, then the body's pieces, then its end or a failure
//!   ([`Reply`], then [`Body`]); 403, 404 and 416 are a head with no body. The types make any
//!   other order unwritable: there is no body without a head, a body is exactly its head's
//!   length (a short one fails, a longer one is refused), and nothing is sent after the end.
//! - **Across**: the answer reaches the engine's thread through the table's outbox, which the
//!   glue drains there ([`Table::next_call`]) when the wake it gave the table fires (macOS: a
//!   block on the main queue; GTK: an idle on its context). So every call to the engine is made
//!   on its thread and in that order, and none after a stop: a request stopped or ended is gone
//!   from the table, ids are never reused, and whatever still arrives for it is dropped — the
//!   tombstone. The table returns each call instead of making it, so the glue never calls into
//!   WebKit while the table is borrowed (a GTK completion can re-enter the stream's read).
//! - **Push and pull** ([`Mode`]): WKWebView takes each piece as it comes (`didReceiveData:`);
//!   WebKitGTK pulls the body from the request's stream, 8 KiB a read ([`Table::read`]). Either
//!   way the server has at most [`WINDOW`] bytes of a request's body sent and not yet taken by
//!   the engine ([`Body::room`]): the table gives that credit back as the engine takes bytes —
//!   at once for a push engine, as the reads drain the request's buffer for a pull engine, whose
//!   buffer is so bounded — and says so ([`Server::credit`]).
//!
//! What the server does with a request — the root, the checks, Range, the type and charset,
//! turns, the preview budget — is the page server's; what the engine does with a call —
//! `WKURLSchemeTask`, `WebKitURISchemeRequest` and its stream — is each backend's glue.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The page scheme's name.
pub const SCHEME: &str = "sopen-page";

/// The most of one request's body the server may have sent and the engine not yet taken: 09
/// §3.1's piece (≤ 1 MiB).
pub const WINDOW: usize = 1 << 20;

/// A view's scheme host: 128 random bits as 32 hex digits, so no two views share an origin and
/// each view's handler serves its own only (§3.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Host(String);

impl Host {
    /// A new host, unlike any other of this process's.
    pub fn new() -> Self {
        // Two of the process's random, never-repeating 64-bit draws.
        let (a, b) = (crate::remote::random_hex(), crate::remote::random_hex());
        Host(format!("{a}{b}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The address of `path` (a path under the page's root, `/`-separated, not encoded) in this
    /// view: `sopen-page://<host>/<path>`, every byte but letters, digits, `-._~` and `/`
    /// percent-encoded.
    pub fn url(&self, path: &str) -> String {
        let mut url = format!("{SCHEME}://{}/", self.0);
        for b in path.trim_start_matches('/').bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                url.push(char::from(b));
            } else {
                let _ = write!(url, "%{b:02X}");
            }
        }
        url
    }

    /// The path of `url` — still percent-encoded, from its first `/`, without its query or
    /// fragment; `/` for none — when `url` is this view's; `None` for another view's address or
    /// another scheme's.
    pub fn path_of<'u>(&self, url: &'u str) -> Option<&'u str> {
        let (scheme, rest) = url.split_once("://")?;
        if !scheme.eq_ignore_ascii_case(SCHEME) {
            return None;
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if !rest[..end].eq_ignore_ascii_case(&self.0) {
            return None;
        }
        let rest = &rest[end..];
        let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
        Some(if path.is_empty() { "/" } else { path })
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

/// One request's id in its view: given by the table at [`Table::start`], never reused, so an id
/// the table no longer has is a request that ended or was stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What the server is asked (X-W1: `start(id, path, range)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: RequestId,
    /// The address's path ([`Host::path_of`]): still percent-encoded, the server decodes it
    /// (§3.1 step 1).
    pub path: String,
    /// The request's `Range` header as the engine got it (`bytes=0-1`), if any: the server reads
    /// it (206, 416).
    pub range: Option<String>,
}

/// A response's status: the five §3.1 answers. Anything else is a failure, not a head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// 200, the whole file.
    Whole,
    /// 206, a range of it.
    Part,
    /// 403: outside the root.
    Forbidden,
    /// 404: no such file.
    NotFound,
    /// 416: a range the file does not have.
    Unsatisfiable,
}

/// A response's head, complete before any byte of its body (X-W1: WebKitGTK takes status,
/// length and headers at once, in the response object). Made only by its constructors, so it
/// is always one of §3.1's answers with a length its body will have exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    status: Status,
    mime: String,
    length: u64,
    /// `Content-Range`: `(first, last, total)` for a 206, `(_, _, total)` for a 416.
    range: Option<(u64, u64, u64)>,
}

impl Head {
    /// 200: the whole file, `length` bytes of type `mime`.
    pub fn whole(mime: impl Into<String>, length: u64) -> Self {
        Head {
            status: Status::Whole,
            mime: mime.into(),
            length,
            range: None,
        }
    }

    /// 206: bytes `first..=last` of a `total`-byte file; `None` unless `first <= last < total`.
    pub fn part(mime: impl Into<String>, first: u64, last: u64, total: u64) -> Option<Self> {
        (first <= last && last < total).then(|| Head {
            status: Status::Part,
            mime: mime.into(),
            length: last - first + 1,
            range: Some((first, last, total)),
        })
    }

    /// 403, no body.
    pub fn forbidden() -> Self {
        Self::empty(Status::Forbidden, None)
    }

    /// 404, no body.
    pub fn not_found() -> Self {
        Self::empty(Status::NotFound, None)
    }

    /// 416 for a `total`-byte file, no body.
    pub fn unsatisfiable(total: u64) -> Self {
        Self::empty(Status::Unsatisfiable, Some((0, 0, total)))
    }

    fn empty(status: Status, range: Option<(u64, u64, u64)>) -> Self {
        Head {
            status,
            mime: String::new(),
            length: 0,
            range,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// The status code: 200, 206, 403, 404, 416.
    pub fn code(&self) -> u16 {
        match self.status {
            Status::Whole => 200,
            Status::Part => 206,
            Status::Forbidden => 403,
            Status::NotFound => 404,
            Status::Unsatisfiable => 416,
        }
    }

    /// The reason phrase that goes with [`Head::code`] (GTK's `set_status` takes one).
    pub fn reason(&self) -> &'static str {
        match self.status {
            Status::Whole => "OK",
            Status::Part => "Partial Content",
            Status::Forbidden => "Forbidden",
            Status::NotFound => "Not Found",
            Status::Unsatisfiable => "Range Not Satisfiable",
        }
    }

    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// The body's length: exactly this many bytes follow the head.
    pub fn length(&self) -> u64 {
        self.length
    }

    /// The header fields, in this order, for both engines alike: `Content-Type` (none for a head
    /// without a type), `Content-Length`, `Content-Range` (a 206: `bytes F-L/T`; a 416: `bytes
    /// */T`) and `Accept-Ranges: bytes`.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::with_capacity(4);
        if !self.mime.is_empty() {
            out.push(("Content-Type", self.mime.clone()));
        }
        out.push(("Content-Length", self.length.to_string()));
        match (self.status, self.range) {
            (Status::Part, Some((first, last, total))) => {
                out.push(("Content-Range", format!("bytes {first}-{last}/{total}")));
            }
            (Status::Unsatisfiable, Some((_, _, total))) => {
                out.push(("Content-Range", format!("bytes */{total}")));
            }
            _ => {}
        }
        out.push(("Accept-Ranges", "bytes".to_string()));
        out
    }
}

/// Why a [`Body`] refused a piece or its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The engine stopped the request: send nothing more.
    Stopped,
    /// More than [`Body::room`] (the request's credit): wait for [`Server::credit`].
    NoRoom { room: usize },
    /// More than the head's length still allows.
    PastLength { left: u64 },
    /// The end before the head's length: the request has failed instead.
    Short { left: u64 },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Stopped => write!(f, "the request was stopped"),
            Refused::NoRoom { room } => write!(f, "only {room} bytes may be sent now"),
            Refused::PastLength { left } => write!(f, "only {left} bytes are left of its length"),
            Refused::Short { left } => write!(f, "{left} bytes of its length were never sent"),
        }
    }
}

impl std::error::Error for Refused {}

/// The page server, for one view (one worker, one connection, §3.1). Called only by the view's
/// [`Table`], on the engine's thread; each call returns at once — the work is the worker's.
pub trait Server: Send {
    /// A new request: its answer goes through `reply`, from any thread.
    fn start(&mut self, request: Request, reply: Reply);
    /// The engine stopped `id` (or the view closed): end it after the read in flight, or the
    /// steps in progress, and send nothing more — what is sent is dropped anyway.
    fn stop(&mut self, id: RequestId);
    /// The engine took bytes of `id`'s body: it has [`Body::room`] again.
    fn credit(&mut self, id: RequestId);
}

/// What the outbox carries, one request's next message.
#[derive(Debug)]
enum Piece {
    Head(Head),
    Data(Vec<u8>),
    Done,
    Fail(String),
}

#[derive(Debug)]
struct Message {
    id: RequestId,
    piece: Piece,
}

/// What a request's two sides share: the credit the engine gave back and the stop.
#[derive(Debug, Default)]
struct Flow {
    /// Bytes of the body the engine has taken.
    taken: AtomicU64,
    stopped: AtomicBool,
}

/// The way from the server's worker to the engine's thread: messages in order, and one wake per
/// batch (the glue drains until [`Table::next_call`] says `None`).
#[derive(Clone)]
struct Outbox(Arc<Shared>);

struct Shared {
    queue: Mutex<VecDeque<Message>>,
    /// A wake went out and its drain has not emptied the queue yet.
    scheduled: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Outbox {
    fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Outbox(Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            scheduled: AtomicBool::new(false),
            wake: Box::new(wake),
        }))
    }

    fn post(&self, id: RequestId, piece: Piece) {
        let wake = {
            let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.push_back(Message { id, piece });
            // Under the lock, as `pop` clears it: a message is never left without a wake.
            !self.0.scheduled.swap(true, Ordering::SeqCst)
        };
        if wake {
            (self.0.wake)();
        }
    }

    fn pop(&self) -> Option<Message> {
        let mut queue = self.0.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let message = queue.pop_front();
        if message.is_none() {
            self.0.scheduled.store(false, Ordering::SeqCst);
        }
        message
    }
}

/// The server's answer to one request, before its head. Send it to any thread. Dropped
/// unanswered, the request fails.
pub struct Reply {
    id: RequestId,
    outbox: Outbox,
    flow: Arc<Flow>,
    answered: bool,
}

impl Reply {
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// The engine stopped the request.
    pub fn stopped(&self) -> bool {
        self.flow.stopped.load(Ordering::SeqCst)
    }

    /// The head; then the body, `head.length()` bytes.
    pub fn head(mut self, head: Head) -> Body {
        self.answered = true;
        let left = head.length;
        if !self.stopped() {
            self.outbox.post(self.id, Piece::Head(head));
        }
        Body {
            id: self.id,
            outbox: self.outbox.clone(),
            flow: self.flow.clone(),
            left,
            sent: 0,
            ended: false,
        }
    }

    /// The request fails before its head (a network error to the engine).
    pub fn fail(mut self, why: impl Into<String>) {
        self.answered = true;
        if !self.stopped() {
            self.outbox.post(self.id, Piece::Fail(why.into()));
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if !self.answered && !self.stopped() {
            let why = "the page server dropped the request".to_string();
            self.outbox.post(self.id, Piece::Fail(why));
        }
    }
}

/// A producer that breaks the order (tests only): the wire itself, past the types.
#[cfg(test)]
pub(crate) struct Raw {
    id: RequestId,
    outbox: Outbox,
}

#[cfg(test)]
impl Raw {
    pub(crate) fn head(&self, head: Head) {
        self.outbox.post(self.id, Piece::Head(head));
    }

    pub(crate) fn data(&self, bytes: Vec<u8>) {
        self.outbox.post(self.id, Piece::Data(bytes));
    }

    pub(crate) fn done(&self) {
        self.outbox.post(self.id, Piece::Done);
    }

    pub(crate) fn fail(&self, why: &str) {
        self.outbox.post(self.id, Piece::Fail(why.to_string()));
    }
}

#[cfg(test)]
impl Reply {
    pub(crate) fn raw(&self) -> Raw {
        Raw {
            id: self.id,
            outbox: self.outbox.clone(),
        }
    }
}

/// A response's body after its head: pieces up to the head's length, then its end. Dropped
/// before its end, the request fails.
pub struct Body {
    id: RequestId,
    outbox: Outbox,
    flow: Arc<Flow>,
    /// Bytes of the head's length still to send.
    left: u64,
    /// Bytes sent so far.
    sent: u64,
    ended: bool,
}

impl Body {
    pub fn id(&self) -> RequestId {
        self.id
    }

    /// The engine stopped the request.
    pub fn stopped(&self) -> bool {
        self.flow.stopped.load(Ordering::SeqCst)
    }

    /// Bytes of the head's length still to send.
    pub fn left(&self) -> u64 {
        self.left
    }

    /// How much may be sent now: [`WINDOW`] less what the engine has not taken yet.
    pub fn room(&self) -> usize {
        let taken = self.flow.taken.load(Ordering::SeqCst);
        let held = self.sent.saturating_sub(taken);
        WINDOW.saturating_sub(usize::try_from(held).unwrap_or(usize::MAX))
    }

    /// A piece (an empty one sends nothing).
    pub fn data(&mut self, bytes: Vec<u8>) -> Result<(), Refused> {
        if self.stopped() {
            return Err(Refused::Stopped);
        }
        let n = bytes.len() as u64;
        if n > self.left {
            return Err(Refused::PastLength { left: self.left });
        }
        let room = self.room();
        if bytes.len() > room {
            return Err(Refused::NoRoom { room });
        }
        if !bytes.is_empty() {
            self.left -= n;
            self.sent += n;
            self.outbox.post(self.id, Piece::Data(bytes));
        }
        Ok(())
    }

    /// The end; before the head's length it is a failure instead (§3.1: a short reply fails the
    /// request).
    pub fn done(mut self) -> Result<(), Refused> {
        self.ended = true;
        if self.stopped() {
            return Err(Refused::Stopped);
        }
        if self.left > 0 {
            let left = self.left;
            let why = format!("the page server ended {left} bytes short of the length");
            self.outbox.post(self.id, Piece::Fail(why));
            return Err(Refused::Short { left });
        }
        self.outbox.post(self.id, Piece::Done);
        Ok(())
    }

    /// The request fails mid-body.
    pub fn fail(mut self, why: impl Into<String>) {
        self.ended = true;
        if !self.stopped() {
            self.outbox.post(self.id, Piece::Fail(why.into()));
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        if !self.ended && !self.stopped() {
            let why = "the page server dropped the request".to_string();
            self.outbox.post(self.id, Piece::Fail(why));
        }
    }
}

/// How the engine takes a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Each piece as it comes (macOS: `didReceiveData:`).
    Push,
    /// Through the request's stream, read by read (GTK: `read_async`), from a buffer of at most
    /// [`WINDOW`] bytes.
    Pull,
}

/// A call the glue makes to the engine, on its thread, for one request — in this order: `Head`,
/// then `Data`* and `Done` (push) or `Readable`* (pull); or `Fail` before the head, and for a
/// push engine after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// The head: `didReceiveResponse:`; GTK: `finish_with_response` over the request's stream.
    Head(Head),
    /// Push: a piece, `didReceiveData:`.
    Data(Vec<u8>),
    /// Push: the end, `didFinish`.
    Done,
    /// Pull: the request's buffer has bytes, its end or its failure: a pending read can be
    /// completed now ([`Table::read`]).
    Readable,
    /// The request failed: `didFailWithError:`; GTK before the head `finish_error`.
    Fail(String),
}

/// What a pull engine's read gets ([`Table::read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// Up to the bytes asked for.
    Bytes(Vec<u8>),
    /// Nothing yet: keep the read pending until [`Call::Readable`].
    Pending,
    /// The body's end: the stream is at its end.
    End,
    /// The body failed: the read fails.
    Failed(String),
    /// No such request: it was stopped, or its end was read.
    Gone,
}

/// A request the table still has.
struct Live {
    path: String,
    flow: Arc<Flow>,
    headed: bool,
    /// Pull: the body's bytes the engine has not read yet.
    buffer: VecDeque<u8>,
    /// Pull: the body's end (`Ok`) or failure, not read yet.
    end: Option<Result<(), String>>,
}

/// One view's requests, on the engine's thread (not `Send`: it never leaves the thread that made
/// it). The glue makes it with the view's server, starts and stops requests on it, and when its
/// wake fires drains it ([`Table::next_call`]) until `None`, making each call it returns; a pull
/// engine reads bodies from it ([`Table::read`]). Dropped (the view closed), it stops every
/// request it still has.
pub struct Table {
    host: Host,
    server: Box<dyn Server>,
    mode: Mode,
    outbox: Outbox,
    next_id: u64,
    live: BTreeMap<RequestId, Live>,
    _engine_thread: PhantomData<*const ()>,
}

impl Table {
    /// `wake` is called, from any thread, when something waits in the outbox: it must only
    /// arrange for [`Table::next_call`] to run on the engine's thread soon, never run it itself.
    /// It may fire after the table is gone (the server's worker races the view's close), so
    /// what it arranges must then find nothing and do nothing.
    pub fn new(
        host: Host,
        server: Box<dyn Server>,
        mode: Mode,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Table {
            host,
            server,
            mode,
            outbox: Outbox::new(wake),
            next_id: 0,
            live: BTreeMap::new(),
            _engine_thread: PhantomData,
        }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The requests not ended or stopped yet.
    pub fn live(&self) -> usize {
        self.live.len()
    }

    /// The engine asks for `url` (with its `Range` header, if any): the request's id. Another
    /// view's address or another scheme's is never asked of the server: it fails.
    pub fn start(&mut self, url: &str, range: Option<String>) -> RequestId {
        self.next_id += 1;
        let id = RequestId(self.next_id);
        let flow = Arc::new(Flow::default());
        let path = self.host.path_of(url).map(str::to_string);
        self.live.insert(
            id,
            Live {
                path: path.clone().unwrap_or_default(),
                flow: flow.clone(),
                headed: false,
                buffer: VecDeque::new(),
                end: None,
            },
        );
        let reply = Reply {
            id,
            outbox: self.outbox.clone(),
            flow,
            answered: false,
        };
        match path {
            Some(path) => self.server.start(Request { id, path, range }, reply),
            None => reply.fail("not this page's address"),
        }
        id
    }

    /// The engine stopped `id`: nothing more of it reaches the engine, and the server is told.
    /// An id that ended already is nothing.
    pub fn stop(&mut self, id: RequestId) {
        if let Some(live) = self.live.remove(&id) {
            live.flow.stopped.store(true, Ordering::SeqCst);
            self.server.stop(id);
        }
    }

    /// Linux's stop before the head (X-W1): the view's resource for `url` failed while its
    /// request had no head yet. The oldest such request on that address is stopped; its id, or
    /// `None` when none waits for its head there.
    pub fn stop_headless(&mut self, url: &str) -> Option<RequestId> {
        let path = self.host.path_of(url)?;
        let id = self
            .live
            .iter()
            .find(|(_, live)| !live.headed && live.path == path)
            .map(|(id, _)| *id)?;
        self.stop(id);
        Some(id)
    }

    /// The next call to make, if any: what arrived, through the order and the tombstone.
    pub fn next_call(&mut self) -> Option<(RequestId, Call)> {
        loop {
            let Message { id, piece } = self.outbox.pop()?;
            // The tombstone: an id no longer here ended or was stopped.
            let Some(live) = self.live.get_mut(&id) else {
                continue;
            };
            let call = match (live.headed, piece, self.mode) {
                // A pull request whose end is in already: nothing can follow it.
                (true, _, Mode::Pull) if live.end.is_some() => continue,
                (false, Piece::Head(head), _) => {
                    live.headed = true;
                    Call::Head(head)
                }
                (false, Piece::Fail(why), _) | (true, Piece::Fail(why), Mode::Push) => {
                    self.live.remove(&id);
                    Call::Fail(why)
                }
                (true, Piece::Data(bytes), Mode::Push) => {
                    live.flow
                        .taken
                        .fetch_add(bytes.len() as u64, Ordering::SeqCst);
                    self.server.credit(id);
                    Call::Data(bytes)
                }
                (true, Piece::Done, Mode::Push) => {
                    self.live.remove(&id);
                    Call::Done
                }
                (true, Piece::Data(bytes), Mode::Pull) => {
                    live.buffer.extend(bytes);
                    Call::Readable
                }
                (true, Piece::Done, Mode::Pull) => {
                    live.end = Some(Ok(()));
                    Call::Readable
                }
                (true, Piece::Fail(why), Mode::Pull) => {
                    live.buffer.clear();
                    live.end = Some(Err(why));
                    Call::Readable
                }
                // Out of order: a body before its head, or a second head. Refused: the request
                // fails, the server stops it, nothing more of it reaches the engine.
                (headed, piece, _) => {
                    let what = match piece {
                        Piece::Head(_) => "a second head",
                        Piece::Data(_) => "a piece",
                        Piece::Done | Piece::Fail(_) => "the end",
                    };
                    let when = if headed {
                        "after its head"
                    } else {
                        "before its head"
                    };
                    self.stop(id);
                    Call::Fail(format!("the page server sent {what} {when}"))
                }
            };
            return Some((id, call));
        }
    }

    /// A pull engine's read of `id`'s body, at most `max` bytes.
    pub fn read(&mut self, id: RequestId, max: usize) -> Read {
        if self.mode != Mode::Pull {
            return Read::Gone;
        }
        let Some(live) = self.live.get_mut(&id) else {
            return Read::Gone;
        };
        if !live.headed {
            return Read::Pending;
        }
        if !live.buffer.is_empty() {
            let n = max.min(live.buffer.len());
            let bytes: Vec<u8> = live.buffer.drain(..n).collect();
            live.flow.taken.fetch_add(n as u64, Ordering::SeqCst);
            self.server.credit(id);
            return Read::Bytes(bytes);
        }
        match live.end.clone() {
            None => Read::Pending,
            Some(end) => {
                self.live.remove(&id);
                match end {
                    Ok(()) => Read::End,
                    Err(why) => Read::Failed(why),
                }
            }
        }
    }
}

impl Drop for Table {
    /// The view closed: every request it still has stops.
    fn drop(&mut self) {
        let ids: Vec<RequestId> = self.live.keys().copied().collect();
        for id in ids {
            self.stop(id);
        }
    }
}
