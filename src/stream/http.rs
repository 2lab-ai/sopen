//! The loopback byte server (06 §9): ffmpeg and ffprobe read a remote file through it, as
//! `http://127.0.0.1:<port>/<token>/<kind>/<name>`, and every byte they get comes from the
//! window's [`Source`]. One server per media window, started with it and dropped with it.
//!
//! One request per connection, answered with `Connection: close`. The path's `kind` decides
//! the connection's reader ([`Kind`]). A response the server itself ends before its
//! `Content-Length` is a **cut**, and the server keeps the account of them ([`Server::cuts`]):
//! ffmpeg exits 0 on a body cut short (06 §7.4), so a run whose input had a cut is failed by
//! that account, whatever ffmpeg's status says.

use super::{ReadError, ReaderKind, Source, BLOCK};
use anyhow::Context;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Connections served at once; the next one gets 503 (06 §9).
pub const MAX_CONNECTIONS: usize = 32;
/// The request line and headers together, at most; a longer head gets 431.
pub const MAX_HEAD: usize = 8 << 10;
/// The whole head must arrive within this of the accept, else the connection closes unanswered.
pub const HEAD_WITHIN: Duration = Duration::from_secs(5);
/// A response whose client takes no bytes for this long is closed (01 §5's no-progress bound).
pub const NO_PROGRESS: Duration = Duration::from_secs(60);

/// Connections past [`MAX_CONNECTIONS`] whose head is still read so that they get their 503;
/// past these too, a connection is closed unanswered.
const TURNED_AWAY: usize = 32;
/// After an answer the server waits this long, and for this many bytes, for the client to
/// close: closing over bytes the client still sends would reset the connection under the
/// answer.
const LINGER: Duration = Duration::from_secs(1);
const LINGER_BYTES: usize = 64 << 10;

/// Which reader serves a connection: the URL's second path segment (06 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Playback, the paused frame and the probe: a Media reader.
    Play,
    /// The waveform: a Bulk reader, which yields to playback's demand.
    Bulk,
    /// Export, a 07 render and Save As: a Bulk reader.
    Render,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Play, Kind::Bulk, Kind::Render];

    /// The path segment.
    pub fn segment(self) -> &'static str {
        match self {
            Kind::Play => "play",
            Kind::Bulk => "bulk",
            Kind::Render => "render",
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.segment() == segment)
    }

    fn reader(self, bytes_per_s: u64) -> ReaderKind {
        match self {
            Kind::Play => ReaderKind::Media { bytes_per_s },
            Kind::Bulk | Kind::Render => ReaderKind::Bulk,
        }
    }
}

/// Why the server ended a response before its `Content-Length`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutReason {
    /// The file changed on the remote (06 §5).
    Changed,
    /// The Source was reloaded: the connection read the version before.
    Reloaded,
    /// A read failed; the text is the Source's own (its status row's).
    Failed(String),
    /// The Source closed under the response.
    Closed,
    /// The client took no bytes for [`NO_PROGRESS`].
    Stalled,
    /// No body was begun: the server was serving [`MAX_CONNECTIONS`] already (503).
    Busy,
}

/// One response the server ended early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub kind: Kind,
    /// The response's first byte in the file, how many bytes it had sent, and how many its
    /// `Content-Length` promised.
    pub start: u64,
    pub sent: u64,
    pub length: u64,
    pub reason: CutReason,
}

/// Where the server's deadlines read the time. A window's server reads the real clock and
/// waits on a socket for exactly what is left; tests give a clock they move by hand and a
/// short `tick`, the longest one socket wait before the clock is read again.
#[derive(Clone)]
pub(crate) struct Config {
    pub clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    pub tick: Option<Duration>,
}

impl Config {
    fn real() -> Self {
        Self {
            clock: Arc::new(Instant::now),
            tick: None,
        }
    }

    /// How long one socket wait may last with `left` until the deadline.
    fn wait(&self, left: Duration) -> Duration {
        let wait = self.tick.map_or(left, |tick| tick.min(left));
        // A zero timeout would mean "no timeout" to the socket.
        wait.max(Duration::from_millis(1))
    }
}

#[derive(Default)]
struct Tally {
    count: u64,
    last: Option<Cut>,
}

/// The cut account (06 §7.4), by reader kind.
#[derive(Default)]
struct Account {
    play: Tally,
    bulk: Tally,
    render: Tally,
    /// Connections closed without an answer for want of a thread or a seat: whose they were
    /// is not known, so they count against every kind.
    unanswered: u64,
}

impl Account {
    fn tally(&mut self, kind: Kind) -> &mut Tally {
        match kind {
            Kind::Play => &mut self.play,
            Kind::Bulk => &mut self.bulk,
            Kind::Render => &mut self.render,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seat {
    /// One of the [`MAX_CONNECTIONS`].
    Served,
    /// Past them: its head is read and it gets 503.
    Turned,
}

/// The accepted sockets, for the shutdown, and how many of each seat are taken.
#[derive(Default)]
struct Conns {
    next: u64,
    open: HashMap<u64, TcpStream>,
    served: usize,
    turned: usize,
}

struct Shared {
    source: Source,
    token: String,
    port: u16,
    /// What a `play` connection's Media reader is opened with (06 §4): 0 until the probe.
    rate: AtomicU64,
    /// Every connection's cancel flag: set once, by the drop.
    cancel: AtomicBool,
    conns: Mutex<Conns>,
    account: Mutex<Account>,
    config: Config,
    /// Tests: socket waits that ran out and looked at the clock.
    #[cfg(test)]
    checks: AtomicU64,
}

impl Shared {
    fn conns(&self) -> MutexGuard<'_, Conns> {
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn account(&self) -> MutexGuard<'_, Account> {
        self.account.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> Instant {
        (self.config.clock)()
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// A socket wait ran out: the caller reads the clock next.
    fn checked(&self) {
        #[cfg(test)]
        self.checks.fetch_add(1, Ordering::SeqCst);
    }

    fn cuts(&self, kind: Kind) -> u64 {
        let mut account = self.account();
        account.tally(kind).count + account.unanswered
    }
}

/// The byte server of one media window. Dropping it closes the listener and every connection,
/// without waiting for their threads (06 §9).
pub struct Server {
    shared: Arc<Shared>,
    /// Dropped to wake the accept thread, which then closes the listener.
    wake: Option<UnixStream>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Binds `127.0.0.1:0` and serves `source` until the drop.
    pub fn start(source: Source) -> anyhow::Result<Server> {
        Self::start_with(source, Config::real())
    }

    pub(crate) fn start_with(source: Source, config: Config) -> anyhow::Result<Server> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("cannot bind the byte server to 127.0.0.1")?;
        // The accept thread waits in `poll`, on the listener and on the drop.
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (wake, woken) = UnixStream::pair()?;
        let shared = Arc::new(Shared {
            source,
            token: token().context("cannot make the byte server's token")?,
            port,
            rate: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            conns: Mutex::default(),
            account: Mutex::default(),
            config,
            #[cfg(test)]
            checks: AtomicU64::new(0),
        });
        let theirs = shared.clone();
        let accept = std::thread::Builder::new()
            .name("sopen-http-accept".into())
            .spawn(move || accept_loop(listener, woken, theirs))
            .context("cannot start the byte server's thread")?;
        Ok(Server {
            shared,
            wake: Some(wake),
            accept: Some(accept),
        })
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// `http://127.0.0.1:<port>/<token>/<kind>/<name>`: what ffmpeg reads the file as. `name`
    /// is the file's name, last so that ffmpeg sees its extension; every byte of it outside
    /// `A–Z a–z 0–9 - . _ ~` is percent-encoded.
    pub fn url(&self, kind: Kind, name: &str) -> String {
        let mut url = format!(
            "http://127.0.0.1:{}/{}/{}/",
            self.shared.port,
            self.shared.token,
            kind.segment()
        );
        for b in name.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                url.push(b as char);
            } else {
                url.push_str(&format!("%{b:02X}"));
            }
        }
        url
    }

    /// The file's byte rate, once probed: `play` connections opened from now on read ahead by
    /// it (06 §4's Media reader). Until then they read ahead by the Media floor.
    pub fn set_rate(&self, bytes_per_s: u64) {
        self.shared.rate.store(bytes_per_s, Ordering::SeqCst);
    }

    /// How many responses of this kind the server has ended early, since it started. A run
    /// reads it before it starts ffmpeg and again when ffmpeg has exited: a higher count means
    /// its input was cut, and the run is failed whatever ffmpeg's status (06 §7.4). Counted
    /// before the connection closes, so ffmpeg cannot see the end before the count has it.
    pub fn cuts(&self, kind: Kind) -> u64 {
        self.shared.cuts(kind)
    }

    /// The last response of this kind ended early: what a failed run says of its input.
    pub fn last_cut(&self, kind: Kind) -> Option<Cut> {
        self.shared.account().tally(kind).last.clone()
    }

    #[cfg(test)]
    pub(crate) fn watch(&self) -> Watch {
        Watch(self.shared.clone())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // The listener first: the accept thread only polls, accepts and spawns, so this join
        // waits for no client and no read.
        drop(self.wake.take());
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        self.shared.cancel.store(true, Ordering::SeqCst);
        for stream in self.shared.conns().open.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        // A thread blocked in `read_at` looks at its flag now; none is joined.
        self.shared.source.interrupt();
    }
}

/// Tests: what the server's threads are doing, also after the drop.
#[cfg(test)]
pub(crate) struct Watch(Arc<Shared>);

#[cfg(test)]
impl Watch {
    /// Connections whose thread has not ended.
    pub(crate) fn open(&self) -> usize {
        let conns = self.0.conns();
        conns.served + conns.turned
    }

    pub(crate) fn checks(&self) -> u64 {
        self.0.checks.load(Ordering::SeqCst)
    }

    pub(crate) fn cuts(&self, kind: Kind) -> u64 {
        self.0.cuts(kind)
    }
}

/// 128 bits from the OS, as 32 hex digits.
fn token() -> std::io::Result<String> {
    let mut raw = [0u8; 16];
    // SAFETY: `raw` is 16 writable bytes, within `getentropy`'s limit of 256.
    if unsafe { libc::getentropy(raw.as_mut_ptr().cast(), raw.len()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether two tokens are the same, in time that depends only on their lengths.
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn accept_loop(listener: TcpListener, woken: UnixStream, shared: Arc<Shared>) {
    loop {
        let mut fds = [listener.as_raw_fd(), woken.as_raw_fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        // SAFETY: `fds` is two valid `pollfd`s, both descriptors open for the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        // The server dropped its end: returning closes the listener.
        if fds[1].revents != 0 {
            return;
        }
        loop {
            match listener.accept() {
                Ok((stream, _)) => admit(&shared, stream),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                // A connection gone from the backlog, or no descriptor left for one: back to
                // the poll, after a moment so that the second cannot spin.
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                    break;
                }
            }
        }
    }
}

/// Seats one accepted connection and starts its thread, or closes it unanswered.
fn admit(shared: &Arc<Shared>, stream: TcpStream) {
    // An accepted socket can inherit the listener's non-blocking mode.
    if stream.set_nonblocking(false).is_err() || !peer_ok(&stream) {
        return;
    }
    let Ok(twin) = stream.try_clone() else {
        return;
    };
    let accepted = shared.now();
    let mut conns = shared.conns();
    let seat = if conns.served < MAX_CONNECTIONS {
        conns.served += 1;
        Seat::Served
    } else if conns.turned < TURNED_AWAY {
        conns.turned += 1;
        Seat::Turned
    } else {
        drop(conns);
        shared.account().unanswered += 1;
        return;
    };
    let id = conns.next;
    conns.next += 1;
    conns.open.insert(id, twin);
    drop(conns);
    let seated = Seated {
        shared: shared.clone(),
        id,
        seat,
    };
    let conn = Conn {
        shared: shared.clone(),
        stream,
        accepted,
        seat,
    };
    let spawned = std::thread::Builder::new()
        .name("sopen-http-conn".into())
        .spawn(move || {
            let _seated = seated;
            conn.run();
        });
    // No thread: the closure was dropped, which gave the seat back and closed the socket.
    if spawned.is_err() {
        shared.account().unanswered += 1;
    }
}

/// A connection's seat, given back when its thread ends.
struct Seated {
    shared: Arc<Shared>,
    id: u64,
    seat: Seat,
}

impl Drop for Seated {
    fn drop(&mut self) {
        let mut conns = self.shared.conns();
        conns.open.remove(&self.id);
        match self.seat {
            Seat::Served => conns.served -= 1,
            Seat::Turned => conns.turned -= 1,
        }
    }
}

/// Linux: the accepted socket is sopen's own, so the client is found by its end of the
/// connection in `/proc/net/tcp`, and must be sopen's uid (06 §9). Anything else — another
/// uid, no entry, two, or no table to read — is closed unanswered.
#[cfg(target_os = "linux")]
fn peer_ok(stream: &TcpStream) -> bool {
    use std::net::SocketAddr;
    let (Ok(SocketAddr::V4(server_end)), Ok(SocketAddr::V4(client_end))) =
        (stream.local_addr(), stream.peer_addr())
    else {
        return false;
    };
    let Ok(table) = std::fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    // SAFETY: `getuid` cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    peer_allowed(&table, server_end, client_end, uid)
}

/// Elsewhere there is no such table (06 §9's residual on macOS).
#[cfg(not(target_os = "linux"))]
fn peer_ok(_stream: &TcpStream) -> bool {
    true
}

/// Whether `/proc/net/tcp` (`table`) has exactly one socket that is the client's end of the
/// connection — local address `client_end`, remote address `server_end`: the server's tuple
/// reversed — and `uid` owns it. The kernel's layout: addresses as `%08X:%04X`, the IPv4
/// address in host byte order and the port as a number; `uid` the eighth column, in decimal.
#[cfg(any(target_os = "linux", test))]
fn peer_allowed(
    table: &str,
    server_end: std::net::SocketAddrV4,
    client_end: std::net::SocketAddrV4,
    uid: u32,
) -> bool {
    let hex = |end: std::net::SocketAddrV4| {
        format!(
            "{:08X}:{:04X}",
            u32::from_ne_bytes(end.ip().octets()),
            end.port()
        )
    };
    let (local, remote) = (hex(client_end), hex(server_end));
    let mut owners = table.lines().filter_map(|line| {
        let mut cols = line.split_ascii_whitespace();
        let (l, r) = (cols.nth(1)?, cols.next()?);
        (l.eq_ignore_ascii_case(&local) && r.eq_ignore_ascii_case(&remote))
            // Columns 4 to 7 (state, queues, timer, retransmits) lie between; a uid that
            // does not read as one is nobody's.
            .then(|| cols.nth(4).and_then(|uid| uid.parse::<u32>().ok()))
    });
    matches!((owners.next(), owners.next()), (Some(Some(owner)), None) if owner == uid)
}

/// What a request asked of the range table (06 §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeAsk {
    /// No `Range` header.
    Whole,
    /// `bytes=N-` or `bytes=N-M`, with `N ≤ M`.
    From(u64, Option<u64>),
    /// A suffix, several ranges, `N > M`, a number past 2^63, any other unit or syntax.
    Refused,
}

impl RangeAsk {
    fn parse(values: &[&str]) -> Self {
        let value = match values {
            [] => return RangeAsk::Whole,
            [one] => one,
            _ => return RangeAsk::Refused,
        };
        // No sign, no space, nothing past 2^63 − 1.
        let number = |text: &str| {
            let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
            let n = text.parse::<u64>().ok().filter(|_| digits)?;
            (n <= i64::MAX as u64).then_some(n)
        };
        let Some((from, to)) = value
            .strip_prefix("bytes=")
            .and_then(|spec| spec.split_once('-'))
        else {
            return RangeAsk::Refused;
        };
        match (number(from), to.is_empty(), number(to)) {
            (Some(n), true, _) => RangeAsk::From(n, None),
            (Some(n), false, Some(m)) if n <= m => RangeAsk::From(n, Some(m)),
            _ => RangeAsk::Refused,
        }
    }

    /// `(first byte, count, partial)` in a file of `size`, or `Err` for 416.
    fn within(self, size: u64) -> Result<(u64, u64, bool), ()> {
        match self {
            RangeAsk::Whole => Ok((0, size, false)),
            RangeAsk::From(n, _) if n >= size => Err(()),
            RangeAsk::From(n, m) => {
                let last = m.map_or(size - 1, |m| m.min(size - 1));
                Ok((n, last - n + 1, true))
            }
            RangeAsk::Refused => Err(()),
        }
    }
}

/// A request the server answers from the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    /// `HEAD`: the `GET`'s status and headers, no body.
    head_only: bool,
    kind: Kind,
    range: RangeAsk,
}

/// A request's head read as 06 §9 reads it: the request, or the status that refuses it.
/// `head` ends with its empty line. A wrong token is told apart only after everything else
/// was read and checked, as for a right one.
fn parse(head: &[u8], port: u16, token: &str) -> Result<Request, u16> {
    let text = std::str::from_utf8(head).map_err(|_| 400u16)?;
    let mut lines = text.strip_suffix("\r\n\r\n").ok_or(400u16)?.split("\r\n");
    let line: Vec<&str> = lines.next().unwrap_or_default().split(' ').collect();
    let [method, target, version] = line[..] else {
        return Err(400);
    };
    if method.is_empty() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(400);
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(400u16)?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(400);
        }
        headers.push((name, value.trim_matches([' ', '\t'])));
    }
    let named = |name: &str| -> Vec<&str> {
        headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, value)| *value)
            .collect()
    };
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Err(405),
    };
    // Exactly one Host, and it is this server: a page in a browser, rebound by DNS to
    // 127.0.0.1, still sends its own name.
    if named("host")[..] != [format!("127.0.0.1:{port}").as_str()] {
        return Err(403);
    }
    let body = !named("transfer-encoding").is_empty()
        || named("content-length")
            .iter()
            .any(|v| v.is_empty() || v.bytes().any(|b| b != b'0'));
    if body {
        return Err(400);
    }
    let range = RangeAsk::parse(&named("range"));
    // `/<token>/<kind>/<name>`: the name is not looked at beyond being there.
    let mut segments = target.strip_prefix('/').unwrap_or("\0").split('/');
    let (theirs, kind, name) = (segments.next(), segments.next(), segments.next());
    let known = same_token(theirs.unwrap_or_default(), token);
    let kind = kind.and_then(Kind::parse);
    let named_file = name.is_some_and(|n| !n.is_empty()) && segments.next().is_none();
    match kind {
        Some(kind) if known && named_file => Ok(Request {
            head_only,
            kind,
            range,
        }),
        _ => Err(404),
    }
}

/// An answer with no body: every status but 200 and 206.
fn refusal(status: u16, size: Option<u64>) -> String {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        431 => "Request Header Fields Too Large",
        _ => "Service Unavailable",
    };
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    if status == 405 {
        head.push_str("Allow: GET, HEAD\r\n");
    }
    if let Some(size) = size {
        head.push_str(&format!("Content-Range: bytes */{size}\r\n"));
    }
    head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    head
}

/// Why a write stopped short.
enum Stop {
    /// The client closed or the server is shutting down: nobody's cut.
    Gone,
    /// The client took nothing for [`NO_PROGRESS`].
    Stalled,
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// One accepted connection, on its own thread.
struct Conn {
    shared: Arc<Shared>,
    stream: TcpStream,
    /// When it was accepted, by the server's clock.
    accepted: Instant,
    seat: Seat,
}

impl Conn {
    fn run(mut self) {
        let Some(head) = self.read_head() else {
            return;
        };
        let request = match head {
            Ok(head) => parse(&head, self.shared.port, &self.shared.token),
            Err(TooLong) => Err(431),
        };
        match request {
            Err(status) => self.refuse(status, None),
            Ok(request) if self.seat == Seat::Turned => {
                // Counted for its kind: a run that was refused an input must not pass.
                self.count(Cut {
                    kind: request.kind,
                    start: 0,
                    sent: 0,
                    length: 0,
                    reason: CutReason::Busy,
                });
                self.refuse(503, None);
            }
            Ok(request) => self.serve(request),
        }
    }

    /// The head, up to and with its empty line; `Err` when it is longer than [`MAX_HEAD`];
    /// `None` when the connection closes unanswered: the client went, the server is shutting
    /// down, or [`HEAD_WITHIN`] passed since the accept.
    fn read_head(&mut self) -> Option<Result<Vec<u8>, TooLong>> {
        let deadline = self.accepted + HEAD_WITHIN;
        let mut head = Vec::with_capacity(1024);
        let mut buf = [0u8; 2048];
        loop {
            if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
                if end + 4 > MAX_HEAD {
                    return Some(Err(TooLong));
                }
                // What follows the head is never read as a request.
                head.truncate(end + 4);
                return Some(Ok(head));
            }
            if head.len() >= MAX_HEAD {
                return Some(Err(TooLong));
            }
            let now = self.shared.now();
            if self.shared.cancelled() || now >= deadline {
                return None;
            }
            let wait = self.shared.config.wait(deadline - now);
            self.stream.set_read_timeout(Some(wait)).ok()?;
            match self.stream.read(&mut buf) {
                Ok(0) => return None,
                Ok(n) => head.extend_from_slice(&buf[..n]),
                Err(e) if timed_out(&e) => self.shared.checked(),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }

    /// Writes `bytes`, calling `sent` with each count the socket took. A wait with nothing
    /// taken reads the clock: [`NO_PROGRESS`] since the last byte went is a stall.
    fn send(&mut self, mut bytes: &[u8], mut sent: impl FnMut(usize)) -> Result<(), Stop> {
        let mut since = self.shared.now();
        while !bytes.is_empty() {
            let idle = self.shared.now().saturating_duration_since(since);
            if self.shared.cancelled() {
                return Err(Stop::Gone);
            }
            if idle >= NO_PROGRESS {
                return Err(Stop::Stalled);
            }
            let wait = self.shared.config.wait(NO_PROGRESS - idle);
            if self.stream.set_write_timeout(Some(wait)).is_err() {
                return Err(Stop::Gone);
            }
            match self.stream.write(bytes) {
                Ok(0) => return Err(Stop::Gone),
                Ok(n) => {
                    sent(n);
                    bytes = &bytes[n..];
                    since = self.shared.now();
                }
                Err(e) if timed_out(&e) => self.shared.checked(),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return Err(Stop::Gone),
            }
        }
        Ok(())
    }

    /// The end of an answer: the server's side closes, and the client's is waited for, a
    /// little — closing over bytes it still sends (a body, a second request, the rest of a
    /// long head) would reset the connection under the answer.
    fn finish(mut self) {
        let _ = self.stream.shutdown(Shutdown::Write);
        let until = self.shared.now() + LINGER;
        let (mut taken, mut buf) = (0, [0u8; 2048]);
        while taken <= LINGER_BYTES && !self.shared.cancelled() {
            let now = self.shared.now();
            if now >= until {
                return;
            }
            let wait = self.shared.config.wait(until - now);
            if self.stream.set_read_timeout(Some(wait)).is_err() {
                return;
            }
            match self.stream.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => taken += n,
                Err(e) if timed_out(&e) => self.shared.checked(),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    fn refuse(mut self, status: u16, size: Option<u64>) {
        if self.send(refusal(status, size).as_bytes(), |_| {}).is_ok() {
            self.finish();
        }
    }

    /// Into the account, before the socket closes: the client sees the end only after.
    fn count(&self, cut: Cut) {
        let mut account = self.shared.account();
        let tally = account.tally(cut.kind);
        tally.count += 1;
        tally.last = Some(cut);
    }

    fn serve(mut self, request: Request) {
        let shared = self.shared.clone();
        let rate = shared.rate.load(Ordering::SeqCst);
        // The reader first, then the size: a reload between them gives the reader `Reloaded`
        // for every read, so a head of the new size is never followed by bytes.
        let mut reader = shared.source.reader(request.kind.reader(rate));
        let size = reader.len();
        let Ok((start, length, partial)) = request.range.within(size) else {
            return self.refuse(416, Some(size));
        };
        let mut head = match partial {
            true => format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{size}\r\n",
                start + length - 1
            ),
            false => "HTTP/1.1 200 OK\r\n".to_string(),
        };
        head.push_str(&format!(
            "Accept-Ranges: bytes\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
        ));
        let mut at = start;
        let end = if request.head_only {
            start
        } else {
            start + length
        };
        let mut stop = self
            .send(head.as_bytes(), |_| {})
            .err()
            .map(|stop| match stop {
                Stop::Gone => None,
                Stop::Stalled => Some(CutReason::Stalled),
            });
        let mut buf = vec![0u8; BLOCK as usize];
        while stop.is_none() && at < end {
            // Up to the block's end: one read never waits for a second block.
            let want = (end - at).min(BLOCK - at % BLOCK) as usize;
            let got = reader.read_at(at, &mut buf[..want], &shared.cancel);
            stop = match got {
                Ok(n) if n == want => {
                    let mut off = at;
                    let sent = self.send(&buf[..want], |n| {
                        shared.source.sent_http(off, n as u64);
                        off += n as u64;
                    });
                    at = off;
                    match sent {
                        Ok(()) => None,
                        Err(Stop::Gone) => Some(None),
                        Err(Stop::Stalled) => Some(Some(CutReason::Stalled)),
                    }
                }
                // The version it reads ends where its size says.
                Ok(n) => Some(Some(CutReason::Failed(format!(
                    "the file ended {} bytes early",
                    want - n
                )))),
                Err(ReadError::Cancelled) => Some(None),
                Err(ReadError::Changed) => Some(Some(CutReason::Changed)),
                Err(ReadError::Reloaded) => Some(Some(CutReason::Reloaded)),
                Err(ReadError::Closed) => Some(Some(CutReason::Closed)),
                Err(ReadError::Failed(reason)) => Some(Some(CutReason::Failed(reason))),
            };
        }
        match stop {
            // Whole.
            None => self.finish(),
            // The client's own end, or the shutdown.
            Some(None) => {}
            Some(Some(_)) if shared.cancelled() => {}
            Some(Some(reason)) => {
                self.count(Cut {
                    kind: request.kind,
                    start,
                    sent: at - start,
                    length,
                    reason,
                });
                shared.source.cut();
                let _ = self.stream.shutdown(Shutdown::Both);
            }
        }
    }
}

/// A head longer than [`MAX_HEAD`].
struct TooLong;

#[cfg(test)]
mod tests;
