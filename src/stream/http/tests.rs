//! The byte server's gate (06 §12 V20): every row of §9's tables, asked by a std `TcpStream`
//! client, and ffmpeg itself reading a file through the server from the local `sftp-server`.
//! Nothing asserts wall-clock time: the two deadlines run on a clock the test moves by hand,
//! and every wait here only catches a hang.

use super::*;
use crate::remote::{
    fake, pattern, Change, Clock, FakeControl, FakeFile, Fault, Link, RemoteFs, SftpRemote,
};
use crate::stream::{limits, Class, Engine, Opening, Reopen, Settings, State, Traced, CAP_MAX};
use std::os::unix::fs::FileExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::UNIX_EPOCH;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// A file of five blocks, the last one short.
const SIZE: u64 = 300_000;
/// Only catches a hang.
const HANG: Duration = Duration::from_secs(120);
/// No poll within a test.
const NEVER: Duration = Duration::from_secs(1_000_000);

// ---- the rig ----

/// Runs `f` on a thread of its own and fails, with `what`, when it has not ended within `HANG`.
fn bounded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = done_tx.send(f());
    });
    match done.recv_timeout(HANG) {
        Ok(got) => got,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what}: not done in {HANG:?}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match worker.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => unreachable!("it ended without a result or a panic"),
        },
    }
}

/// Waits until `cond`, failing with `what` after `HANG`.
fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < HANG, "{what}: not in {HANG:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn no_reopen() -> Reopen {
    Box::new(|| anyhow::bail!("this test has no second connection"))
}

/// A Source on the Fake, its engine on its own thread, the link without delay.
fn threaded(size: u64) -> (Source, FakeControl) {
    let (transport, control) = fake("/m.mp4", FakeFile::pattern(size), Link::instant());
    let source = Source::open(transport, no_reopen(), Arc::new(|| {})).unwrap();
    (source, control)
}

/// A Source on the Fake with its engine stepped by the test: nothing is fetched between steps.
struct Stepped {
    source: Source,
    engine: Engine,
    control: FakeControl,
}

fn stepped(size: u64, link: Link) -> Stepped {
    let (transport, control) = fake("/m.mp4", FakeFile::pattern(size), link);
    let settings = Settings {
        poll_every: NEVER,
        ..Settings::plain()
    };
    let opening = Opening {
        label: "host:/m.mp4".into(),
        began: Instant::now(),
        sniff: 0,
    };
    let again = control.clone();
    let (source, engine) = Source::build(
        transport,
        Box::new(move || again.open()),
        Arc::new(|| {}),
        opening,
        settings,
    );
    Stepped {
        source,
        engine,
        control,
    }
}

impl Stepped {
    /// Steps the engine until `done`.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Source) -> bool) {
        let started = Instant::now();
        while !done(&self.source) {
            assert!(started.elapsed() < HANG, "{what}: not in {HANG:?}");
            self.engine.step(Some(Duration::from_millis(5)));
            std::thread::yield_now();
        }
    }

    fn trace(&self) -> Vec<Traced> {
        self.source.core().trace.clone()
    }

    fn waiting(&self) -> usize {
        self.source.core().pf.waiting_readers()
    }
}

/// A clock the test moves.
#[derive(Clone)]
struct Hand(Arc<Mutex<Instant>>);

impl Hand {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    fn config(&self) -> Config {
        self.with(Seams::default())
    }

    fn with(&self, seams: Seams) -> Config {
        let at = self.0.clone();
        Config {
            clock: Arc::new(move || *at.lock().unwrap()),
            tick: Some(Duration::from_millis(5)),
            seams,
        }
    }

    fn advance(&self, d: Duration) {
        *self.0.lock().unwrap() += d;
    }
}

/// Waits until `n` more socket waits have run out and read the clock: the server's threads
/// are where the test thinks they are, and have seen the time it set. Called with a connection
/// seated; one that ends meanwhile fails here, at once.
fn checks(watch: &Watch, n: u64) {
    let from = watch.checks();
    eventually("the server reads its clock", || {
        assert!(watch.open() > 0, "the connection was closed meanwhile");
        watch.checks() >= from + n
    });
}

/// The server has filled the client's socket and waits on it: three waits in a row with
/// nothing more sent. What it has sent.
fn settled(watch: &Watch, source: &Source) -> u64 {
    let mut sent = source.stats().http_sent;
    loop {
        checks(watch, 3);
        let now = std::mem::replace(&mut sent, source.stats().http_sent);
        if now == sent {
            return sent;
        }
    }
}

fn connect(server: &Server) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", server.port())).expect("the server listens");
    stream.set_read_timeout(Some(HANG)).unwrap();
    stream.set_write_timeout(Some(HANG)).unwrap();
    stream
}

/// `/<token>/<kind>/m.mp4`.
fn target(server: &Server, kind: Kind) -> String {
    let url = server.url(kind, "m.mp4");
    url.strip_prefix(&format!("http://127.0.0.1:{}", server.port()))
        .expect("the url is on the server")
        .to_string()
}

/// A request as ffmpeg sends it, with `extra` header lines (each ending `\r\n`).
fn request(server: &Server, method: &str, kind: Kind, extra: &str) -> Vec<u8> {
    format!(
        "{method} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{extra}\r\n",
        target(server, kind),
        server.port()
    )
    .into_bytes()
}

fn range(spec: &str) -> String {
    format!("Range: {spec}\r\n")
}

#[derive(Debug)]
struct Reply {
    status: u16,
    /// The status line and headers, as sent.
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        let mut found = self.head.split("\r\n").skip(1).filter_map(|line| {
            let (n, v) = line.split_once(": ")?;
            n.eq_ignore_ascii_case(name).then_some(v)
        });
        let first = found.next();
        assert!(found.next().is_none(), "{name} twice in {:?}", self.head);
        first
    }

    fn parse(raw: &[u8]) -> Reply {
        let end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or_else(|| panic!("no answer in {:?}", String::from_utf8_lossy(raw)));
        let head = String::from_utf8(raw[..end].to_vec()).unwrap();
        let status = head
            .strip_prefix("HTTP/1.1 ")
            .and_then(|rest| rest.get(..3))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status in {head:?}"));
        Reply {
            status,
            head,
            body: raw[end + 4..].to_vec(),
        }
    }
}

/// Everything the server sends until it closes. A reset counts as the close: what came
/// before it is what was sent.
fn read_all(stream: &mut TcpStream) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return raw,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::ConnectionReset => return raw,
            Err(e) => panic!("the server neither answers nor closes: {e}"),
        }
    }
}

/// One connection: the request, then everything until the server closes.
fn exchange(server: &Server, request: &[u8]) -> Reply {
    let mut stream = connect(server);
    stream.write_all(request).unwrap();
    Reply::parse(&read_all(&mut stream))
}

fn get(server: &Server, kind: Kind, extra: &str) -> Reply {
    exchange(server, &request(server, "GET", kind, extra))
}

/// The answer's head, a byte at a time: nothing of the body is taken.
fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("the answer's head");
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

fn content_length(head: &str) -> u64 {
    Reply::parse(head.as_bytes())
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .expect("a Content-Length")
}

fn assert_partial(reply: &Reply, from: u64, last: u64, size: u64) {
    assert_eq!(reply.status, 206, "{:?}", reply.head);
    assert_eq!(
        reply.header("Content-Range"),
        Some(format!("bytes {from}-{last}/{size}").as_str())
    );
    let count = last - from + 1;
    assert_eq!(
        reply.header("Content-Length"),
        Some(count.to_string().as_str())
    );
    assert_eq!(reply.header("Accept-Ranges"), Some("bytes"));
    assert_eq!(reply.header("Connection"), Some("close"));
    assert_eq!(reply.body.len() as u64, count, "exactly the count");
    assert!(
        reply.body[..] == pattern(from, count as usize)[..],
        "the file's bytes from {from}"
    );
}

fn assert_unsatisfiable(reply: &Reply, size: u64) {
    assert_eq!(reply.status, 416, "{:?}", reply.head);
    assert_eq!(
        reply.header("Content-Range"),
        Some(format!("bytes */{size}").as_str())
    );
    assert_eq!(reply.header("Content-Length"), Some("0"));
    assert_eq!(reply.header("Connection"), Some("close"));
    assert!(reply.body.is_empty());
}

fn assert_refused(reply: &Reply, status: u16) {
    assert_eq!(reply.status, status, "{:?}", reply.head);
    assert_eq!(reply.header("Content-Length"), Some("0"));
    assert_eq!(reply.header("Connection"), Some("close"));
    assert!(reply.body.is_empty(), "no body");
}

// ---- ranges ----

/// 06 §9's table, the 200 and 206 rows: no `Range` is the whole file; `bytes=N-` and
/// `bytes=N-M` give exactly the count, clamped to the file's end, with its `Content-Range`.
#[test]
fn ranges_give_exactly_the_clamped_count() {
    bounded("ranges_give_exactly_the_clamped_count", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        let whole = get(&server, Kind::Play, "");
        assert_eq!(whole.status, 200, "{:?}", whole.head);
        assert_eq!(
            whole.header("Content-Length"),
            Some(SIZE.to_string().as_str())
        );
        assert_eq!(whole.header("Content-Range"), None);
        assert_eq!(whole.header("Accept-Ranges"), Some("bytes"));
        assert_eq!(whole.header("Connection"), Some("close"));
        assert!(whole.body[..] == pattern(0, SIZE as usize)[..]);
        let last = SIZE - 1;
        // What ffmpeg sends, with its other headers.
        let ffmpeg = "User-Agent: Lavf/62.3.100\r\nAccept: */*\r\nRange: bytes=1000-\r\n\
                      Connection: close\r\nIcy-MetaData: 1\r\n";
        assert_partial(&get(&server, Kind::Play, ffmpeg), 1000, last, SIZE);
        for (spec, from, to) in [
            ("bytes=0-", 0, last),
            ("bytes=299999-", last, last),
            ("bytes=0-0", 0, 0),
            // Across a block's edge.
            ("bytes=65530-65545", 65530, 65545),
            ("bytes=5-299999", 5, last),
            // `M ≥ size` is clamped to `size − 1`.
            ("bytes=299990-300000", 299_990, last),
            ("bytes=100-999999", 100, last),
            ("bytes=7-9223372036854775807", 7, last),
        ] {
            let reply = get(&server, Kind::Play, &range(spec));
            assert_partial(&reply, from, to, SIZE);
        }
        // A header's name in any case.
        assert_partial(
            &get(&server, Kind::Play, "rAnGe: bytes=10-19\r\n"),
            10,
            19,
            SIZE,
        );
        assert_eq!(source.stats().http_sent, SIZE, "the union of what was sent");
        assert_eq!(server.cuts(Kind::Play), 0);
    });
}

/// `N ≥ size` is 416 with `bytes */size`, and so is every range of an empty file.
#[test]
fn a_start_at_or_past_the_end_is_416() {
    bounded("a_start_at_or_past_the_end_is_416", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source).unwrap();
        for spec in [
            "bytes=300000-",
            "bytes=300000-300005",
            "bytes=999999-",
            "bytes=9223372036854775807-",
        ] {
            assert_unsatisfiable(&get(&server, Kind::Play, &range(spec)), SIZE);
        }
        let (empty, _control) = threaded(0);
        let server = Server::start(empty).unwrap();
        for spec in ["bytes=0-", "bytes=0-0", "bytes=0-10", "bytes=1-"] {
            assert_unsatisfiable(&get(&server, Kind::Play, &range(spec)), 0);
        }
        let whole = get(&server, Kind::Play, "");
        assert_eq!(whole.status, 200);
        assert_eq!(whole.header("Content-Length"), Some("0"));
        assert!(whole.body.is_empty());
    });
}

/// What ffmpeg never sends is refused on purpose: a suffix, several ranges, `N > M`, a number
/// past 2^63, any other unit or syntax.
#[test]
fn ranges_ffmpeg_never_sends_are_416() {
    bounded("ranges_ffmpeg_never_sends_are_416", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        for spec in [
            "bytes=-500",
            "bytes=0-10,20-30",
            "bytes=0-10, 20-30",
            "bytes=10-5",
            "bytes=9223372036854775808-",
            "bytes=0-9223372036854775808",
            "bytes=18446744073709551616-",
            "bytes=0-18446744073709551616",
            "items=0-5",
            "bytes 0-5",
            "BYTES=0-5",
            "bytes=",
            "bytes=-",
            "bytes=a-",
            "bytes=0-b",
            "bytes=+1-",
            "bytes=0-+5",
            "bytes= 1-",
            "bytes=1",
            "bytes=1-2-3",
            "0-5",
            "",
        ] {
            let reply = get(&server, Kind::Play, &range(spec));
            assert_eq!(reply.status, 416, "{spec:?}: {:?}", reply.head);
            assert_unsatisfiable(&reply, SIZE);
        }
        // Several ranges as several headers.
        let two = "Range: bytes=0-1\r\nRange: bytes=2-3\r\n";
        assert_unsatisfiable(&get(&server, Kind::Play, two), SIZE);
        assert_eq!(source.stats().http_sent, 0, "none of them got a byte");
    });
}

/// `HEAD` gives the `GET`'s status and headers and no body, with and without a range.
#[test]
fn head_gives_the_gets_headers_and_no_body() {
    bounded("head_gives_the_gets_headers_and_no_body", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        // The status and the headers of each row, written out: `SIZE` is 300 000. An empty
        // string is a header that must not be there.
        struct Row {
            range: &'static str,
            status: u16,
            length: &'static str,
            content_range: &'static str,
            accept_ranges: &'static str,
        }
        let row = |range, status, length, content_range, accept_ranges| Row {
            range,
            status,
            length,
            content_range,
            accept_ranges,
        };
        let rows = [
            row("", 200, "300000", "", "bytes"),
            row("bytes=5-", 206, "299995", "bytes 5-299999/300000", "bytes"),
            row("bytes=5-9", 206, "5", "bytes 5-9/300000", "bytes"),
            // Clamped to the file's end.
            row(
                "bytes=5-999999",
                206,
                "299995",
                "bytes 5-299999/300000",
                "bytes",
            ),
            row(
                "bytes=299990-300000",
                206,
                "10",
                "bytes 299990-299999/300000",
                "bytes",
            ),
            row("bytes=999999-", 416, "0", "bytes */300000", ""),
            row("bytes=-5", 416, "0", "bytes */300000", ""),
        ];
        let extra = |row: &Row| match row.range {
            "" => String::new(),
            spec => range(spec),
        };
        let heads: Vec<Reply> = rows
            .iter()
            .map(|row| exchange(&server, &request(&server, "HEAD", Kind::Play, &extra(row))))
            .collect();
        assert_eq!(source.stats().http_sent, 0, "a HEAD is sent no byte");
        let there = |value: &'static str| Some(value).filter(|v| !v.is_empty());
        for (row, head) in rows.iter().zip(&heads) {
            let asked = row.range;
            assert_eq!(head.status, row.status, "{asked:?}");
            assert_eq!(head.header("Content-Length"), Some(row.length), "{asked:?}");
            assert_eq!(
                head.header("Content-Range"),
                there(row.content_range),
                "{asked:?}"
            );
            assert_eq!(
                head.header("Accept-Ranges"),
                there(row.accept_ranges),
                "{asked:?}"
            );
            assert_eq!(head.header("Connection"), Some("close"), "{asked:?}");
            assert!(head.body.is_empty(), "{asked:?}: a HEAD has no body");
            let got = get(&server, Kind::Play, &extra(row));
            assert_eq!(
                head.head, got.head,
                "{asked:?}: the GET's status and headers"
            );
            assert_eq!(got.body.len().to_string(), row.length, "{asked:?}");
        }
        assert_eq!(server.cuts(Kind::Play), 0, "a HEAD is whole without a body");
    });
}

// ---- requests ----

/// No token, a wrong one, or a kind other than `play`, `bulk` and `render` gets 404 and 0
/// bytes; the three kinds with the token are served.
#[test]
fn no_token_or_another_kind_is_404_with_no_body() {
    bounded("no_token_or_another_kind_is_404_with_no_body", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        let good = target(&server, Kind::Play);
        let token = good.split('/').nth(1).unwrap().to_string();
        assert_eq!(good, format!("/{token}/play/m.mp4"));
        let mut flipped = token.clone().into_bytes();
        flipped[31] = if flipped[31] == b'0' { b'1' } else { b'0' };
        let flipped = String::from_utf8(flipped).unwrap();
        for path in [
            "/".to_string(),
            "/play/m.mp4".to_string(),
            "/m.mp4".to_string(),
            format!("/{flipped}/play/m.mp4"),
            format!("/{}/play/m.mp4", &token[..31]),
            format!("/{token}0/play/m.mp4"),
            format!("/{}/play/m.mp4", "0".repeat(32)),
            "//play/m.mp4".to_string(),
            format!("/{token}/other/m.mp4"),
            format!("/{token}/Play/m.mp4"),
            format!("/{token}/m.mp4"),
            format!("/{token}/play"),
            format!("/{token}/play/"),
            format!("/{token}/play/dir/m.mp4"),
            format!("/{token}"),
            format!("{token}/play/m.mp4"),
            format!("http://127.0.0.1:{}/{token}/play/m.mp4", server.port()),
            "*".to_string(),
        ] {
            let head = format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nRange: bytes=0-9\r\n\r\n",
                server.port()
            );
            let reply = exchange(&server, head.as_bytes());
            assert_eq!(reply.status, 404, "{path:?}: {:?}", reply.head);
            assert_refused(&reply, 404);
        }
        assert_eq!(source.stats().http_sent, 0, "0 bytes of the file");
        for kind in Kind::ALL {
            assert_partial(&get(&server, kind, &range("bytes=0-9")), 0, 9, SIZE);
        }
    });
}

/// The kind is the connection's reader: a `bulk` connection (and a `render` one) reads as a
/// Bulk reader, a `play` one as demand — and none of the bulk connection's requests goes out
/// while a `play` demand is queued (06 §4, through the server).
#[test]
fn a_bulk_connection_is_served_as_a_bulk_reader() {
    bounded("a_bulk_connection_is_served_as_a_bulk_reader", || {
        let slow = Link {
            latency: Duration::from_millis(20),
            rate: 500_000,
            clip: None,
            clock: Clock::Simulated,
        };
        let mut rig = stepped(24_000_000_000, slow);
        let server = Server::start(rig.source.clone()).unwrap();
        // The waveform's pass: it reads until the server closes.
        let mut pass = connect(&server);
        pass.write_all(&request(&server, "GET", Kind::Bulk, &range("bytes=0-")))
            .unwrap();
        let reading = std::thread::spawn(move || read_all(&mut pass).len());
        rig.until("the pass streams", |s| s.stats().http_sent >= 8 * BLOCK);
        // With no step, the pass runs out of blocks and waits for its next one.
        eventually("the pass waits for a block", || rig.waiting() == 1);
        rig.engine.pump();
        let n = rig.trace().len();
        assert!(n > 0);
        for t in &rig.trace() {
            assert!(
                matches!(t.class, Some(Class::Bulk) | None),
                "a bulk connection's request in another class: {t:?}"
            );
        }
        // Playback seeks: a new connection, far away.
        let at = 10 * GIB + 1000;
        let mut play = connect(&server);
        play.write_all(&request(
            &server,
            "GET",
            Kind::Play,
            &range(&format!("bytes={at}-")),
        ))
        .unwrap();
        let (played_tx, played) = mpsc::channel();
        let player = std::thread::spawn(move || {
            read_head(&mut play);
            let mut got = vec![0u8; 480];
            play.read_exact(&mut got).unwrap();
            played_tx.send(()).unwrap();
            got
        });
        eventually("the seek's block is asked for", || rig.waiting() == 2);
        assert_eq!(rig.trace().len(), n, "nothing was sent meanwhile");
        rig.until("playback gets its bytes", |_| played.try_recv().is_ok());
        assert!(player.join().unwrap()[..] == pattern(at, 480)[..]);
        let trace = rig.trace();
        let demand = trace
            .iter()
            .position(|t| t.block == Some(at / BLOCK))
            .expect("the seek's block was sent");
        assert!(demand >= n);
        assert_eq!(trace[demand].class, Some(Class::Demand));
        for t in &trace[n..=demand] {
            assert!(
                matches!(t.class, Some(Class::Demand) | None),
                "sent while playback's demand was queued: {t:?}"
            );
        }
        // An export's connection is a Bulk reader too.
        let far = 20 * GIB + 1000;
        let mut render = connect(&server);
        render
            .write_all(&request(
                &server,
                "GET",
                Kind::Render,
                &range(&format!("bytes={far}-")),
            ))
            .unwrap();
        let (rendered_tx, rendered) = mpsc::channel();
        let renderer = std::thread::spawn(move || {
            read_head(&mut render);
            let mut got = vec![0u8; 100];
            render.read_exact(&mut got).unwrap();
            rendered_tx.send(()).unwrap();
            got
        });
        rig.until("the export gets its bytes", |_| rendered.try_recv().is_ok());
        assert!(renderer.join().unwrap()[..] == pattern(far, 100)[..]);
        let trace = rig.trace();
        let exported = trace
            .iter()
            .position(|t| t.block == Some(far / BLOCK))
            .expect("the export's block was sent");
        assert_eq!(trace[exported].class, Some(Class::Bulk));
        // The pass ends with the server.
        drop(server);
        assert!(reading.join().unwrap() > 0);
    });
}

/// Exactly one `Host: 127.0.0.1:<port>`: a missing, wrong or repeated one gets 403.
#[test]
fn a_missing_wrong_or_repeated_host_is_403() {
    bounded("a_missing_wrong_or_repeated_host_is_403", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        let (path, port) = (target(&server, Kind::Play), server.port());
        let own = format!("Host: 127.0.0.1:{port}\r\n");
        for hosts in [
            String::new(),
            "Host: 127.0.0.1\r\n".to_string(),
            format!("Host: localhost:{port}\r\n"),
            format!("Host: 127.0.0.1:{}\r\n", port.wrapping_add(1)),
            format!("Host: evil.example:{port}\r\n"),
            format!("Host: 127.0.0.1:{port}.evil.example\r\n"),
            "Host:\r\n".to_string(),
            format!("{own}{own}"),
            format!("{own}Host: evil.example\r\n"),
            format!("Host: evil.example\r\n{own}"),
            format!("{own}hOST: 127.0.0.1:{port}\r\n"),
        ] {
            let head = format!("GET {path} HTTP/1.1\r\n{hosts}Range: bytes=0-9\r\n\r\n");
            let reply = exchange(&server, head.as_bytes());
            assert_eq!(reply.status, 403, "{hosts:?}: {:?}", reply.head);
            assert_refused(&reply, 403);
        }
        assert_eq!(source.stats().http_sent, 0);
        // The name in any case, the value with space around it.
        let head = format!("GET {path} HTTP/1.1\r\nhOsT:   127.0.0.1:{port} \r\n\r\n");
        assert_eq!(exchange(&server, head.as_bytes()).status, 200);
    });
}

/// Any method but `GET` and `HEAD` gets 405.
#[test]
fn another_method_is_405() {
    bounded("another_method_is_405", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        for method in ["POST", "PUT", "DELETE", "OPTIONS", "get", "CONNECT"] {
            let reply = exchange(&server, &request(&server, method, Kind::Play, ""));
            assert_eq!(reply.status, 405, "{method}: {:?}", reply.head);
            assert_refused(&reply, 405);
            assert_eq!(reply.header("Allow"), Some("GET, HEAD"));
        }
        // A POST as a client sends one, body and all.
        let mut post = request(&server, "POST", Kind::Play, "Content-Length: 3\r\n");
        post.extend_from_slice(b"abc");
        assert_refused(&exchange(&server, &post), 405);
        assert_eq!(source.stats().http_sent, 0);
    });
}

/// A request with a body (`Content-Length` other than 0, or any `Transfer-Encoding`) gets 400,
/// and so does a head that is not a request.
#[test]
fn a_body_or_transfer_encoding_is_400() {
    bounded("a_body_or_transfer_encoding_is_400", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        for extra in [
            "Content-Length: 5\r\n",
            "content-length: 1\r\n",
            "Content-Length: 0\r\nContent-Length: 5\r\n",
            "Content-Length: x\r\n",
            "Content-Length:\r\n",
            "Transfer-Encoding: chunked\r\n",
            "transfer-encoding: identity\r\n",
            "Transfer-Encoding:\r\n",
        ] {
            let mut bytes = request(&server, "GET", Kind::Play, extra);
            bytes.extend_from_slice(b"hello");
            let reply = exchange(&server, &bytes);
            assert_eq!(reply.status, 400, "{extra:?}: {:?}", reply.head);
            assert_refused(&reply, 400);
        }
        // `Content-Length: 0` is no body.
        let none = get(
            &server,
            Kind::Play,
            "Content-Length: 0\r\nRange: bytes=0-9\r\n",
        );
        assert_partial(&none, 0, 9, SIZE);
        let (path, port) = (target(&server, Kind::Play), server.port());
        let host = format!("Host: 127.0.0.1:{port}\r\n");
        for head in [
            format!("GET {path}\r\n{host}\r\n"),
            format!("GET {path} HTTP/2.0\r\n{host}\r\n"),
            format!("GET  {path} HTTP/1.1\r\n{host}\r\n"),
            format!("GET {path} HTTP/1.1 \r\n{host}\r\n"),
            format!(" GET {path} HTTP/1.1\r\n{host}\r\n"),
            format!("GET {path} HTTP/1.1\r\n{host}no colon here\r\n\r\n"),
            format!("GET {path} HTTP/1.1\r\n{host} folded: line\r\n\r\n"),
            format!("GET {path} HTTP/1.1\r\n{host}: no name\r\n\r\n"),
            "\r\n\r\n".to_string(),
        ] {
            let reply = exchange(&server, head.as_bytes());
            assert_eq!(reply.status, 400, "{head:?}: {:?}", reply.head);
            assert_refused(&reply, 400);
        }
        let mut binary = format!("GET {path} HTTP/1.1\r\n{host}X: ").into_bytes();
        binary.extend_from_slice(&[0xFF, 0xFE, b'\r', b'\n', b'\r', b'\n']);
        assert_refused(&exchange(&server, &binary), 400);
        assert_eq!(source.stats().http_sent, 10, "only the one without a body");
    });
}

/// The request line and headers together are at most 8 KiB: a longer head gets 431.
#[test]
fn a_head_over_8_kib_is_431() {
    bounded("a_head_over_8_kib_is_431", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        // A head of exactly `len` bytes, its last header padded.
        let head_of = |len: usize| {
            let bare = request(
                &server,
                "GET",
                Kind::Play,
                "Range: bytes=0-9\r\nX-Pad: \r\n",
            );
            let pad = "a".repeat(len - bare.len());
            let head = request(
                &server,
                "GET",
                Kind::Play,
                &format!("Range: bytes=0-9\r\nX-Pad: {pad}\r\n"),
            );
            assert_eq!(head.len(), len);
            head
        };
        // The test's own numbers, not the server's constant: 8 KiB is 8 × 1024 bytes.
        assert_partial(&exchange(&server, &head_of(8 * 1024)), 0, 9, SIZE);
        assert_refused(&exchange(&server, &head_of(8 * 1024 + 1)), 431);
        assert_refused(&exchange(&server, &head_of(9 * 1024)), 431);
        // A head that never ends is answered once it is too long, not read on.
        let endless = vec![b'a'; 12 << 10];
        assert_refused(&exchange(&server, &endless), 431);
        assert_eq!(source.stats().http_sent, 10);
    });
}

/// The whole head must arrive within 5 s of the accept: one trickled over 6 s is closed
/// unanswered, one that takes 4 s is answered. On the server's clock, moved by hand.
#[test]
fn a_head_trickled_past_5_s_is_closed_unanswered() {
    bounded("a_head_trickled_past_5_s_is_closed_unanswered", || {
        let (source, _control) = threaded(SIZE);
        let hand = Hand::new();
        let server = Server::start_with(source.clone(), hand.config()).unwrap();
        let watch = server.watch();
        let whole = request(&server, "GET", Kind::Play, &range("bytes=0-9"));
        let (first, rest) = whole.split_at(whole.len() / 2);
        // 4 s between the halves.
        let mut slow = connect(&server);
        slow.write_all(first).unwrap();
        eventually("the connection is seated", || watch.open() == 1);
        checks(&watch, 2);
        hand.advance(Duration::from_secs(4));
        checks(&watch, 2);
        slow.write_all(rest).unwrap();
        assert_partial(&Reply::parse(&read_all(&mut slow)), 0, 9, SIZE);
        drop(slow);
        eventually("the answered connection ends", || watch.open() == 0);
        // A quarter every 2 s: at 6 s the last one has not come.
        let mut trickle = connect(&server);
        let quarter = whole.len() / 4;
        eventually("the connection is seated", || watch.open() == 1);
        for i in 0..3 {
            trickle
                .write_all(&whole[i * quarter..(i + 1) * quarter])
                .unwrap();
            checks(&watch, 2);
            assert_eq!(watch.open(), 1, "open at {} s", 2 * i);
            hand.advance(Duration::from_secs(2));
        }
        eventually("the trickled connection is closed", || watch.open() == 0);
        // The rest comes too late: whether or not it can still be written, no answer comes.
        let _ = trickle.write_all(&whole[3 * quarter..]);
        assert!(read_all(&mut trickle).is_empty(), "closed unanswered");
        assert_eq!(source.stats().http_sent, 10, "only the first was served");
    });
}

/// A head whose last bytes come after the 5 s is not accepted, however soon they are read:
/// the server's thread is waiting in its read when the clock passes the deadline, and the
/// rest of the head arrives in that same wait.
#[test]
fn a_head_completed_after_5_s_is_closed_unanswered() {
    bounded("a_head_completed_after_5_s_is_closed_unanswered", || {
        let (source, _control) = threaded(SIZE);
        let hand = Hand::new();
        // A long tick: the thread looks at the clock only when a read has waited this long.
        let config = Config {
            tick: Some(Duration::from_millis(500)),
            ..hand.config()
        };
        let server = Server::start_with(source.clone(), config).unwrap();
        let watch = server.watch();
        let whole = request(&server, "GET", Kind::Play, &range("bytes=0-9"));
        let (first, rest) = whole.split_at(whole.len() / 2);
        let mut late = connect(&server);
        late.write_all(first).unwrap();
        eventually("the connection is seated", || watch.open() == 1);
        // Two waits ran out since the first half was there: it is read, and the thread is
        // in a read again.
        checks(&watch, 2);
        hand.advance(Duration::from_secs(6));
        late.write_all(rest).unwrap();
        assert!(read_all(&mut late).is_empty(), "closed unanswered");
        eventually("its thread ends", || watch.open() == 0);
        assert_eq!(source.stats().http_sent, 0);
    });
}

/// A head that is not HTTP's syntax gets 400: a field name that is not a token, and a NUL or
/// a bare CR or LF in a field value or in the request target.
#[test]
fn a_head_that_is_not_http_syntax_is_400() {
    bounded("a_head_that_is_not_http_syntax_is_400", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        let (path, port) = (target(&server, Kind::Play), server.port());
        let host = format!("Host: 127.0.0.1:{port}\r\n");
        // Every character a field name may have besides letters and digits.
        let fine = format!(
            "GET {path} HTTP/1.1\r\n{host}X-!#$%&'*+.^_`|~9: v\r\nRange: bytes=0-9\r\n\r\n"
        );
        assert_partial(&exchange(&server, fine.as_bytes()), 0, 9, SIZE);
        for line in [
            // Field names that are not tokens.
            "X(a): v",
            "X@a: v",
            "X/a: v",
            "X\"a\": v",
            "X a: v",
            "Range : bytes=0-9",
            "X\ta: v",
            "X[]: v",
            "Ünï: v",
            // A NUL, a bare CR and a bare LF in a field value.
            "X: a\0b",
            "X: a\rb",
            "X: a\nb",
            "X: a\nHost: evil.example",
            // And in a field name.
            "X\0a: v",
            "X\ra: v",
            "X\na: v",
        ] {
            let head = format!("GET {path} HTTP/1.1\r\n{host}{line}\r\nRange: bytes=0-9\r\n\r\n");
            let reply = exchange(&server, head.as_bytes());
            assert_eq!(reply.status, 400, "{line:?}: {:?}", reply.head);
            assert_refused(&reply, 400);
        }
        // In the request target.
        for bad in ["\0", "\r", "\n"] {
            let head = format!("GET {path}{bad}x HTTP/1.1\r\n{host}Range: bytes=0-9\r\n\r\n");
            let reply = exchange(&server, head.as_bytes());
            assert_eq!(reply.status, 400, "{bad:?} in the target: {:?}", reply.head);
            assert_refused(&reply, 400);
        }
        assert_eq!(source.stats().http_sent, 10, "only the well-formed one");
    });
}

/// One request per connection: a second one on it is never answered — sent with the first, or
/// after the first's answer.
#[test]
fn a_second_request_on_the_connection_is_never_answered() {
    bounded(
        "a_second_request_on_the_connection_is_never_answered",
        || {
            let (source, _control) = threaded(SIZE);
            let server = Server::start(source.clone()).unwrap();
            let first = request(&server, "GET", Kind::Play, &range("bytes=0-9"));
            let second = request(&server, "GET", Kind::Play, &range("bytes=20-29"));
            let count = |raw: &[u8]| raw.windows(9).filter(|w| w == b"HTTP/1.1 ").count();
            // Pipelined.
            let mut both = connect(&server);
            both.write_all(&[&first[..], &second[..]].concat()).unwrap();
            let raw = read_all(&mut both);
            assert_eq!(count(&raw), 1, "{:?}", String::from_utf8_lossy(&raw));
            assert_partial(&Reply::parse(&raw), 0, 9, SIZE);
            // After the answer.
            let mut again = connect(&server);
            again.write_all(&first).unwrap();
            let head = read_head(&mut again);
            assert!(head.starts_with("HTTP/1.1 206 "), "{head:?}");
            let mut body = [0u8; 10];
            again.read_exact(&mut body).unwrap();
            let _ = again.write_all(&second);
            assert!(read_all(&mut again).is_empty(), "nothing after the first");
            assert_eq!(source.stats().http_sent, 10, "bytes 20-29 were never sent");
        },
    );
}

/// At most 32 connections are served at once: the 33rd gets 503, and the account has it
/// against its kind — a run refused an input must not pass. A seat given back is served.
#[test]
fn a_33rd_connection_gets_503() {
    bounded("a_33rd_connection_gets_503", || {
        let (source, _control) = threaded(SIZE);
        // The clock stands still: no idle connection runs into the head's deadline.
        let server = Server::start_with(source.clone(), Hand::new().config()).unwrap();
        let watch = server.watch();
        let idle: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| connect(&server)).collect();
        eventually("32 connections are seated", || {
            watch.open() == MAX_CONNECTIONS
        });
        let reply = get(&server, Kind::Render, &range("bytes=0-9"));
        assert_refused(&reply, 503);
        assert_eq!(server.cuts(Kind::Render), 1);
        assert_eq!((server.cuts(Kind::Play), server.cuts(Kind::Bulk)), (0, 0));
        let cut = server.last_cut(Kind::Render).expect("the refusal");
        assert_eq!((cut.kind, cut.reason), (Kind::Render, CutReason::Busy));
        assert_eq!(source.stats().cuts, 0, "no response was begun");
        // A request nobody would be served is refused as itself, and is nobody's cut.
        let stranger = format!(
            "GET /nothing HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.port()
        );
        assert_refused(&exchange(&server, stranger.as_bytes()), 404);
        assert_eq!(server.cuts(Kind::Render), 1);
        drop(idle);
        eventually("the seats are given back", || watch.open() == 0);
        assert_partial(&get(&server, Kind::Render, &range("bytes=0-9")), 0, 9, SIZE);
        assert_eq!(source.stats().http_sent, 10);
    });
}

// ---- lifetime ----

/// A response whose client takes no bytes for 60 s is closed, and counted: the server ended
/// it. On the server's clock, moved by hand: at 59 s it is still open.
#[test]
fn a_client_that_stops_reading_is_closed_after_60_s() {
    bounded("a_client_that_stops_reading_is_closed_after_60_s", || {
        let size = 256 * MIB;
        let (source, _control) = threaded(size);
        let hand = Hand::new();
        let server = Server::start_with(source.clone(), hand.config()).unwrap();
        let watch = server.watch();
        let mut client = connect(&server);
        client
            .write_all(&request(&server, "GET", Kind::Play, ""))
            .unwrap();
        let head = read_head(&mut client);
        assert_eq!(content_length(&head), size);
        let mut some = vec![0u8; 1000];
        client.read_exact(&mut some).unwrap();
        // It reads no more. The server fills the socket and then waits on it: three waits
        // in a row with nothing more sent.
        let sent = settled(&watch, &source);
        assert!(sent < size);
        hand.advance(Duration::from_secs(59));
        checks(&watch, 3);
        assert_eq!(
            (watch.open(), server.cuts(Kind::Play)),
            (1, 0),
            "open at 59 s"
        );
        hand.advance(Duration::from_secs(1));
        eventually("the stalled response is closed", || {
            server.cuts(Kind::Play) == 1
        });
        let cut = server.last_cut(Kind::Play).unwrap();
        assert_eq!(
            (cut.kind, cut.start, cut.sent, cut.length, cut.reason),
            (Kind::Play, 0, sent, size, CutReason::Stalled)
        );
        assert_eq!(source.stats().cuts, 1);
        // What was sent is all that comes.
        let rest = read_all(&mut client);
        assert_eq!(1000 + rest.len() as u64, sent);
        eventually("its thread ends", || watch.open() == 0);
    });
}

/// A client that takes exactly what the test grants: the server's writes go through a gate
/// that lets `grant`ed bytes through, and is a wait that ran out while none are left. The
/// client itself reads everything that comes.
struct Paced {
    hand: Hand,
    server: Server,
    watch: Watch,
    left: Arc<AtomicU64>,
    /// Reads until the server closes; what it got.
    client: std::thread::JoinHandle<usize>,
}

fn paced(first: u64) -> Paced {
    let (source, _control) = threaded(256 * MIB);
    let hand = Hand::new();
    let left = Arc::new(AtomicU64::new(first));
    let budget = left.clone();
    let seams = Seams {
        before_write: Some(Arc::new(move |_, offered| {
            let take = budget.load(Ordering::SeqCst).min(offered as u64);
            if take == 0 {
                std::thread::sleep(Duration::from_millis(1));
                return Gate::Fail(ErrorKind::WouldBlock.into());
            }
            budget.fetch_sub(take, Ordering::SeqCst);
            Gate::AtMost(take as usize)
        })),
        ..Seams::default()
    };
    let server = Server::start_with(source, hand.with(seams)).unwrap();
    let watch = server.watch();
    let mut stream = connect(&server);
    stream
        .write_all(&request(&server, "GET", Kind::Play, ""))
        .unwrap();
    let client = std::thread::spawn(move || read_all(&mut stream).len());
    let paced = Paced {
        hand,
        server,
        watch,
        left,
        client,
    };
    paced.taken();
    paced
}

impl Paced {
    /// Everything granted has gone, and the server waits on the client again.
    fn taken(&self) {
        eventually("the client takes what it was granted", || {
            self.left.load(Ordering::SeqCst) == 0
        });
        checks(&self.watch, 3);
    }

    /// `bytes` more go to the client.
    fn grant(&self, bytes: u64) {
        self.left.fetch_add(bytes, Ordering::SeqCst);
        self.taken();
    }

    /// `secs` pass on the server's clock, and the server has seen them.
    fn pass(&self, secs: u64) {
        self.hand.advance(Duration::from_secs(secs));
        checks(&self.watch, 3);
    }

    fn open(&self) -> bool {
        (self.watch.open(), self.server.cuts(Kind::Play)) == (1, 0)
    }

    /// One more second, and the response is closed as stalled. What the client got.
    fn closes_after_a_second(self) -> usize {
        self.hand.advance(Duration::from_secs(1));
        eventually("the response is closed", || {
            self.server.cuts(Kind::Play) == 1
        });
        let cut = self.server.last_cut(Kind::Play).unwrap();
        assert_eq!(cut.reason, CutReason::Stalled);
        let got = self.client.join().unwrap();
        assert_eq!(cut.sent + content_head(), got as u64);
        got
    }
}

/// The length of a 200's head for the 256 MiB file.
fn content_head() -> u64 {
    "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: 268435456\r\nConnection: close\r\n\r\n"
        .len() as u64
}

/// The 60 s are not started again by a trickle: only once 64 KiB have gone since they last
/// started. A client that takes 1 KiB every 50 s is closed at 60 s — and so is one that takes
/// 32 KiB, or one byte short of what starts the timer again.
#[test]
fn a_trickle_does_not_keep_a_response_open() {
    bounded("a_trickle_does_not_keep_a_response_open", || {
        // The first 1000 bytes go at once: the head and the body's start.
        for chunk in [1024, 32 * 1024, 64 * 1024 - 1001] {
            let paced = paced(1000);
            let resets = paced.watch.resets();
            paced.pass(50);
            assert!(paced.open(), "{chunk}: open at 50 s");
            paced.grant(chunk);
            assert_eq!(paced.watch.resets(), resets, "{chunk}: the timer runs on");
            paced.pass(9);
            assert!(paced.open(), "{chunk}: open at 59 s");
            let got = paced.closes_after_a_second();
            assert_eq!(got as u64, 1000 + chunk, "{chunk}");
        }
    });
}

/// A client that takes 64 KiB every 50 s is slow, not stalled: the 60 s start again at each
/// take, and the response stays open — until it takes nothing for 60 s.
#[test]
fn a_client_taking_64_kib_per_50_s_stays_open() {
    bounded("a_client_taking_64_kib_per_50_s_stays_open", || {
        let paced = paced(1000);
        for round in 1..=4 {
            let resets = paced.watch.resets();
            paced.pass(50);
            paced.grant(64 * 1024);
            assert_eq!(paced.watch.resets(), resets + 1, "at {} s", 50 * round);
            assert!(paced.open(), "open at {} s", 50 * round);
        }
        paced.pass(59);
        assert!(paced.open(), "open 59 s after its last take");
        let got = paced.closes_after_a_second();
        assert_eq!(got, 1000 + 4 * 64 * 1024);
    });
}

/// A client that leaves while its response waits for a block — ffmpeg closes its connection
/// at every seek — is found gone: the thread returns, its reader's wait is retired with
/// nothing sent for it, and the seat is free. The client left: no cut.
#[test]
fn a_client_that_leaves_a_waiting_response_frees_its_seat() {
    bounded(
        "a_client_that_leaves_a_waiting_response_frees_its_seat",
        || {
            let slow = Link {
                latency: Duration::from_millis(20),
                rate: 500_000,
                clip: None,
                clock: Clock::Simulated,
            };
            let mut rig = stepped(24_000_000_000, slow);
            let server = Server::start_with(rig.source.clone(), Hand::new().config()).unwrap();
            let watch = server.watch();
            let at = 10 * GIB + 1000;
            let mut client = connect(&server);
            client
                .write_all(&request(
                    &server,
                    "GET",
                    Kind::Play,
                    &range(&format!("bytes={at}-")),
                ))
                .unwrap();
            let head = read_head(&mut client);
            assert!(head.starts_with("HTTP/1.1 206 "), "{head:?}");
            // The engine is not stepped: the block it waits for does not arrive.
            eventually("the response waits for its block", || rig.waiting() == 1);
            assert!(rig.trace().is_empty());
            assert_eq!((watch.open(), watch.left()), (1, 0));
            drop(client);
            eventually("the seat is free", || watch.open() == 0);
            assert_eq!(watch.left(), 1);
            assert_eq!(rig.waiting(), 0, "its wait is retired");
            // The engine runs now, and has nothing to send for the reader that went.
            rig.engine.pump();
            assert!(
                rig.trace().iter().all(|t| t.block.is_none()),
                "a read was sent for a client that left: {:?}",
                rig.trace()
            );
            // A client that stays is served, and is not found gone.
            let mut stays = connect(&server);
            stays
                .write_all(&request(
                    &server,
                    "GET",
                    Kind::Play,
                    &range(&format!("bytes={at}-")),
                ))
                .unwrap();
            let (got_tx, got) = mpsc::channel();
            let reading = std::thread::spawn(move || {
                read_head(&mut stays);
                let mut bytes = vec![0u8; 480];
                stays.read_exact(&mut bytes).unwrap();
                got_tx.send(()).unwrap();
                (bytes, stays)
            });
            rig.until("the client that stays gets its bytes", |_| {
                got.try_recv().is_ok()
            });
            let (bytes, stays) = reading.join().unwrap();
            assert!(bytes[..] == pattern(at, 480)[..]);
            assert_eq!((watch.open(), watch.left()), (1, 1));
            for kind in Kind::ALL {
                assert_eq!(server.cuts(kind), 0, "a client that left is no cut");
            }
            assert_eq!(rig.source.stats().cuts, 0);
            drop(stays);
            eventually("its seat is free too", || watch.open() == 0);
            assert_eq!(server.cuts(Kind::Play), 0);
        },
    );
}

/// A write that fails once `at` bytes have gone on the connection.
fn failing_at(at: u64, error: impl Fn() -> std::io::Error + Send + Sync + 'static) -> Config {
    let seams = Seams {
        before_write: Some(Arc::new(move |written, _| match written >= at {
            true => Gate::Fail(error()),
            false => Gate::Go,
        })),
        ..Seams::default()
    };
    Config {
        seams,
        ..Config::real()
    }
}

/// A write that fails for a reason that is not the client leaving ends the body short, and
/// is a cut — in the account by the time the client sees the end.
#[test]
fn a_write_that_fails_is_a_cut_counted_before_the_client_sees_the_end() {
    bounded(
        "a_write_that_fails_is_a_cut_counted_before_the_client_sees_the_end",
        || {
            let size = 64 * MIB;
            let (source, _control) = threaded(size);
            let config = failing_at(200_000, || std::io::Error::other("injected write failure"));
            let server = Server::start_with(source.clone(), config).unwrap();
            let mut client = connect(&server);
            client
                .write_all(&request(
                    &server,
                    "GET",
                    Kind::Render,
                    &range("bytes=1000-"),
                ))
                .unwrap();
            let head = read_head(&mut client);
            let body = read_all(&mut client);
            // The client has seen the end: no wait before the account is read.
            assert_eq!(server.cuts(Kind::Render), 1, "counted before the close");
            let length = content_length(&head);
            assert_eq!(length, size - 1000);
            assert!(body.len() + head.len() >= 200_000 && (body.len() as u64) < length);
            assert!(body[..] == pattern(1000, body.len())[..]);
            let cut = server.last_cut(Kind::Render).expect("the cut");
            assert_eq!(
                (cut.kind, cut.start, cut.sent, cut.length),
                (Kind::Render, 1000, body.len() as u64, length)
            );
            match &cut.reason {
                CutReason::Failed(why) => assert!(why.contains("injected write failure"), "{why}"),
                other => panic!("{other:?}"),
            }
            assert_eq!(source.stats().cuts, 1);
            assert_eq!((server.cuts(Kind::Play), server.cuts(Kind::Bulk)), (0, 0));
        },
    );
}

/// Only an error that clearly means the client left is nobody's cut.
#[test]
fn a_write_error_that_means_the_client_left_is_not_a_cut() {
    bounded(
        "a_write_error_that_means_the_client_left_is_not_a_cut",
        || {
            for kind in [
                ErrorKind::BrokenPipe,
                ErrorKind::ConnectionReset,
                ErrorKind::ConnectionAborted,
            ] {
                let (source, _control) = threaded(64 * MIB);
                let config = failing_at(200_000, move || std::io::Error::from(kind));
                let server = Server::start_with(source.clone(), config).unwrap();
                let watch = server.watch();
                let mut client = connect(&server);
                client
                    .write_all(&request(&server, "GET", Kind::Render, ""))
                    .unwrap();
                let head = read_head(&mut client);
                let body = read_all(&mut client);
                assert!((body.len() as u64) < content_length(&head));
                eventually("its thread ends", || watch.open() == 0);
                assert_eq!(server.cuts(Kind::Render), 0, "{kind:?}");
                assert_eq!(source.stats().cuts, 0, "{kind:?}");
            }
        },
    );
}

/// A connection thread that panics mid-body leaves a body short of its length: the cut is in
/// the account by the time the client sees the end, and the server goes on serving.
#[test]
fn a_panic_mid_body_is_a_cut_counted_before_the_client_sees_the_end() {
    bounded(
        "a_panic_mid_body_is_a_cut_counted_before_the_client_sees_the_end",
        || {
            let size = 64 * MIB;
            let (source, _control) = threaded(size);
            let seams = Seams {
                before_write: Some(Arc::new(|written, _| {
                    assert!(written < 200_000, "injected panic in a connection thread");
                    Gate::Go
                })),
                ..Seams::default()
            };
            let config = Config {
                seams,
                ..Config::real()
            };
            let server = Server::start_with(source.clone(), config).unwrap();
            let watch = server.watch();
            let mut client = connect(&server);
            client
                .write_all(&request(&server, "GET", Kind::Play, &range("bytes=1000-")))
                .unwrap();
            let head = read_head(&mut client);
            let body = read_all(&mut client);
            // The client has seen the end: no wait before the account is read.
            assert_eq!(server.cuts(Kind::Play), 1, "counted before the close");
            let length = content_length(&head);
            assert!(body.len() + head.len() >= 200_000 && (body.len() as u64) < length);
            assert!(body[..] == pattern(1000, body.len())[..]);
            let cut = server.last_cut(Kind::Play).expect("the cut");
            assert_eq!(
                (cut.kind, cut.start, cut.sent, cut.length),
                (Kind::Play, 1000, body.len() as u64, length)
            );
            assert_eq!(
                cut.reason,
                CutReason::Failed("connection thread panicked".into())
            );
            assert_eq!(source.stats().cuts, 1);
            // Its seat came back, and the next connection is served.
            eventually("the seat is free", || watch.open() == 0);
            assert_partial(&get(&server, Kind::Play, &range("bytes=0-9")), 0, 9, size);
            assert_eq!(server.cuts(Kind::Play), 1);
        },
    );
}

/// The drop cancels every connection before it waits for the accept thread, and the accept
/// thread accepts nothing after the stop: held right after an accept, with more connections
/// waiting, it keeps no connection alive and takes none of those that wait.
#[test]
fn the_drop_cancels_before_it_waits_for_the_accept_thread() {
    bounded(
        "the_drop_cancels_before_it_waits_for_the_accept_thread",
        || {
            let (source, _control) = threaded(256 * MIB);
            let (entered_tx, entered) = mpsc::channel();
            let (release, held) = mpsc::channel::<()>();
            let held = Mutex::new(held);
            let armed = Arc::new(AtomicBool::new(false));
            let accepts = Arc::new(AtomicU64::new(0));
            let (arm, count) = (armed.clone(), accepts.clone());
            let seams = Seams {
                accepted: Some(Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    if arm.swap(false, Ordering::SeqCst) {
                        entered_tx.send(()).unwrap();
                        let _ = held.lock().unwrap().recv();
                    }
                })),
                ..Seams::default()
            };
            let server = Server::start_with(source.clone(), Hand::new().with(seams)).unwrap();
            let (watch, port) = (server.watch(), server.port());
            // A response under way, its client not reading.
            let mut stuck = connect(&server);
            stuck
                .write_all(&request(&server, "GET", Kind::Play, ""))
                .unwrap();
            read_head(&mut stuck);
            settled(&watch, &source);
            // The accept thread is held right after its next accept, and more wait behind it.
            armed.store(true, Ordering::SeqCst);
            let _second = connect(&server);
            entered
                .recv_timeout(HANG)
                .expect("the accept thread is held");
            let _backlog: Vec<TcpStream> = (0..3).map(|_| connect(&server)).collect();
            let dropping = std::thread::spawn(move || drop(server));
            eventually(
                "every connection is cancelled, the accept thread still held",
                || watch.open() == 0,
            );
            assert!(
                !dropping.is_finished(),
                "the drop waits for the accept thread"
            );
            release.send(()).unwrap();
            dropping.join().unwrap();
            assert_eq!(
                accepts.load(Ordering::SeqCst),
                2,
                "nothing is accepted after the stop"
            );
            assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
            for kind in Kind::ALL {
                assert_eq!(watch.cuts(kind), 0);
            }
        },
    );
}

/// The drop returns while connections keep coming, and every thread ends.
#[test]
fn the_drop_returns_while_connections_keep_coming() {
    bounded("the_drop_returns_while_connections_keep_coming", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source).unwrap();
        let (watch, port) = (server.watch(), server.port());
        let made = Arc::new(AtomicU64::new(0));
        let counter = made.clone();
        // A few hundred at most, one after the other, ending at the first that fails.
        let storm = std::thread::spawn(move || {
            for _ in 0..300 {
                if TcpStream::connect(("127.0.0.1", port)).is_err() {
                    break;
                }
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        eventually("connections are coming", || {
            made.load(Ordering::SeqCst) >= 20 || storm.is_finished()
        });
        drop(server);
        storm.join().unwrap();
        eventually("every thread ends", || watch.open() == 0);
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    });
}

/// After the window closes the port refuses connections, and every thread returns: one
/// blocked in a socket write, one blocked in `read_at`, one still waiting for its head. None
/// of them is a cut.
#[test]
fn after_the_drop_the_port_refuses_and_blocked_threads_return() {
    bounded(
        "after_the_drop_the_port_refuses_and_blocked_threads_return",
        || {
            // As a window's server waits: on the socket for all that is left of a deadline,
            // with no tick to look at the cancel flag between. The clock stands still.
            let untimed = |seams: Seams| Config {
                tick: None,
                ..Hand::new().with(seams)
            };
            // A client that does not read: its thread says when it is about to write to a
            // socket that takes nothing more, and blocks in that write.
            let (blocking_tx, blocking) = mpsc::channel();
            let seams = Seams {
                blocking: Some(Arc::new(move || {
                    let _ = blocking_tx.send(());
                })),
                ..Seams::default()
            };
            let (fast, _fast_control) = threaded(256 * MIB);
            let writing = Server::start_with(fast.clone(), untimed(seams)).unwrap();
            let mut stuck = connect(&writing);
            stuck
                .write_all(&request(&writing, "GET", Kind::Play, ""))
                .unwrap();
            let head = read_head(&mut stuck);
            blocking
                .recv_timeout(HANG)
                .expect("the write blocks on the full socket");
            // A link that never answers: its thread blocks in `read_at`. And a connection
            // that sends nothing: its thread waits for the head.
            let never = Link {
                latency: Duration::from_secs(100_000),
                clock: Clock::Real,
                ..Link::gate()
            };
            let (transport, control) = fake("/m.mp4", FakeFile::pattern(MIB), never);
            let slow = Source::open(transport, no_reopen(), Arc::new(|| {})).unwrap();
            let reading = Server::start_with(slow.clone(), untimed(Seams::default())).unwrap();
            let mut waiting = connect(&reading);
            waiting
                .write_all(&request(&reading, "GET", Kind::Render, ""))
                .unwrap();
            eventually("the read waits for its block", || {
                slow.core().pf.waiting_readers() == 1 && control.sent().len() >= 2
            });
            let mut silent = connect(&reading);
            let (watch_w, watch_r) = (writing.watch(), reading.watch());
            eventually("three connections are open", || {
                (watch_w.open(), watch_r.open()) == (1, 2)
            });
            let ports = [writing.port(), reading.port()];
            drop(writing);
            drop(reading);
            for port in ports {
                let refused = TcpStream::connect(("127.0.0.1", port));
                assert!(
                    refused.is_err(),
                    "the port still accepts after the server is dropped"
                );
            }
            eventually("every thread returns", || {
                (watch_w.open(), watch_r.open()) == (0, 0)
            });
            // No wait ran out: the socket's shutdown and the interrupt woke them.
            assert_eq!((watch_w.checks(), watch_r.checks()), (0, 0));
            for kind in Kind::ALL {
                assert_eq!((watch_w.cuts(kind), watch_r.cuts(kind)), (0, 0));
            }
            assert_eq!((fast.stats().cuts, slow.stats().cuts), (0, 0));
            // The clients see the end: short of the length, or with no answer.
            assert!((read_all(&mut stuck).len() as u64) < content_length(&head));
            assert!(Reply::parse(&read_all(&mut waiting)).body.is_empty());
            assert!(read_all(&mut silent).is_empty());
        },
    );
}

/// A client of a response under way, so that the test can act in the middle: it takes the
/// head and `first` bytes of the body, says so (`there`), waits for the word (`go`), and then
/// reads to the end. Its result is the head, the whole body it got, and the account's count
/// for `kind` read the moment it saw the end.
struct Midway {
    there: mpsc::Receiver<()>,
    go: mpsc::Sender<()>,
    client: std::thread::JoinHandle<(String, Vec<u8>, u64)>,
}

fn midway(mut stream: TcpStream, first: usize, watch: Watch, kind: Kind) -> Midway {
    let (there_tx, there) = mpsc::channel();
    let (go, gone) = mpsc::channel::<()>();
    let client = std::thread::spawn(move || {
        let head = read_head(&mut stream);
        let mut body = vec![0u8; first];
        stream.read_exact(&mut body).unwrap();
        there_tx.send(()).unwrap();
        let _ = gone.recv();
        body.extend_from_slice(&read_all(&mut stream));
        let counted = watch.cuts(kind);
        (head, body, counted)
    });
    Midway { there, go, client }
}

/// A response already started that cannot finish — the source changed, a read failed, the
/// source was reloaded — is closed short of its `Content-Length`, with no byte of a newer
/// version, and counted: by the server's account for its kind and on the stats line. The
/// count is there when the client reads the end of the body: it is taken first.
#[test]
fn a_source_that_changes_during_a_response_cuts_it_short() {
    bounded(
        "a_source_that_changes_during_a_response_cuts_it_short",
        || {
            let size = 64 * MIB;
            let first = 100_000;
            #[derive(Debug, Clone, Copy)]
            enum Ends {
                Changed,
                Failed,
                Reloaded,
            }
            for (ends, kind) in [
                (Ends::Changed, Kind::Play),
                (Ends::Failed, Kind::Render),
                (Ends::Reloaded, Kind::Bulk),
            ] {
                let mut rig = stepped(size, Link::instant());
                let server = Server::start(rig.source.clone()).unwrap();
                let mut stream = connect(&server);
                stream
                    .write_all(&request(&server, "GET", kind, &range("bytes=1000-")))
                    .unwrap();
                let Midway { there, go, client } = midway(stream, first, server.watch(), kind);
                rig.until("the response is under way", |_| there.try_recv().is_ok());
                assert_eq!(server.cuts(kind), 0);
                // The engine is not stepped here: nothing more is fetched before the change.
                match ends {
                    // Every byte of the file is another one now, a second later.
                    Ends::Changed => rig.control.change(Change::Write {
                        off: 0,
                        bytes: vec![0xEE; 16 * MIB as usize],
                        same_second: false,
                    }),
                    // The channel closes at the next request, and no other opens.
                    Ends::Failed => {
                        let next = rig.control.sent().len();
                        rig.control.fault(next, Fault::Lost);
                        rig.control.change(Change::Remove);
                    }
                    Ends::Reloaded => rig.source.request_reload(),
                }
                go.send(()).unwrap();
                // Until the client has seen the end — not until the count: that is the claim.
                rig.until("the client sees the end", |_| client.is_finished());
                let (head, body, counted) = client.join().unwrap();
                assert_eq!(
                    counted, 1,
                    "{ends:?}: counted before the client saw the end"
                );
                assert!(head.starts_with("HTTP/1.1 206 "), "{head:?}");
                let length = content_length(&head);
                assert_eq!(length, size - 1000);
                assert!(
                    (body.len() as u64) < length,
                    "{ends:?}: short of its Content-Length"
                );
                assert!(body.len() >= first);
                assert!(
                    body[..] == pattern(1000, body.len())[..],
                    "{ends:?}: only bytes of the version it began with"
                );
                let cut = server.last_cut(kind).expect("the cut");
                assert_eq!(
                    (cut.kind, cut.start, cut.sent, cut.length),
                    (kind, 1000, body.len() as u64, length),
                    "{ends:?}"
                );
                match (ends, &cut.reason) {
                    (Ends::Changed, CutReason::Changed)
                    | (Ends::Failed, CutReason::Failed(_))
                    | (Ends::Reloaded, CutReason::Reloaded) => {}
                    other => panic!("{other:?}"),
                }
                let stats = rig.source.stats();
                assert_eq!((stats.cuts, stats.http_sent), (1, body.len() as u64));
                for other in Kind::ALL.into_iter().filter(|k| *k != kind) {
                    assert_eq!(server.cuts(other), 0, "{ends:?}: only its own kind");
                }
                match ends {
                    Ends::Changed => assert_eq!(rig.source.state(), State::Changed),
                    Ends::Failed => assert!(matches!(rig.source.state(), State::Failed(_))),
                    // A connection made after the reload reads the version open now.
                    Ends::Reloaded => {
                        assert_eq!(rig.source.state(), State::Live);
                        let mut next = connect(&server);
                        next.write_all(&request(&server, "GET", kind, &range("bytes=0-99")))
                            .unwrap();
                        let (done_tx, done) = mpsc::channel();
                        let reader = std::thread::spawn(move || {
                            let raw = read_all(&mut next);
                            done_tx.send(()).unwrap();
                            raw
                        });
                        rig.until("the next connection is served", |_| done.try_recv().is_ok());
                        assert_partial(&Reply::parse(&reader.join().unwrap()), 0, 99, size);
                        assert_eq!(server.cuts(kind), 1);
                    }
                }
            }
        },
    );
}

/// A connection the client itself closes early — ffmpeg does so at every seek — is not a cut.
#[test]
fn a_connection_the_client_closes_is_not_a_cut() {
    bounded("a_connection_the_client_closes_is_not_a_cut", || {
        let (source, _control) = threaded(256 * MIB);
        let server = Server::start(source.clone()).unwrap();
        let watch = server.watch();
        for kind in Kind::ALL {
            let mut client = connect(&server);
            client
                .write_all(&request(&server, "GET", kind, &range("bytes=5000-")))
                .unwrap();
            read_head(&mut client);
            let mut some = vec![0u8; 70_000];
            client.read_exact(&mut some).unwrap();
            assert!(some[..] == pattern(5000, some.len())[..]);
            drop(client);
            eventually("its thread ends", || watch.open() == 0);
            assert_eq!(server.cuts(kind), 0);
            assert_eq!(server.last_cut(kind), None);
        }
        assert_eq!(source.stats().cuts, 0);
        assert!(source.stats().http_sent >= 70_000);
    });
}

// ---- the token, the URL, the peer ----

/// The token is 128 bits from the OS, in the path; the URL ends with the file's name, so
/// ffmpeg sees its extension; the server listens on 127.0.0.1 only.
#[test]
fn the_url_carries_the_token_the_kind_and_the_name() {
    bounded("the_url_carries_the_token_the_kind_and_the_name", || {
        let (source, _control) = threaded(SIZE);
        let (a, b) = (
            Server::start(source.clone()).unwrap(),
            Server::start(source.clone()).unwrap(),
        );
        let token_of = |server: &Server| {
            let url = server.url(Kind::Play, "m.mp4");
            let rest = url
                .strip_prefix(&format!("http://127.0.0.1:{}/", server.port()))
                .unwrap_or_else(|| panic!("{url}"))
                .to_string();
            let parts: Vec<&str> = rest.split('/').collect();
            assert_eq!(parts[1..], ["play", "m.mp4"]);
            parts[0].to_string()
        };
        let (ta, tb) = (token_of(&a), token_of(&b));
        for token in [&ta, &tb] {
            assert_eq!(token.len(), 32, "128 bits in hex");
            assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
            assert_ne!(token, &"0".repeat(32));
        }
        assert_ne!(ta, tb, "a token per server");
        assert!(a
            .url(Kind::Bulk, "x.mp3")
            .ends_with(&format!("/{ta}/bulk/x.mp3")));
        assert!(a
            .url(Kind::Render, "x.mkv")
            .ends_with(&format!("/{ta}/render/x.mkv")));
        // A name is one path segment whatever it holds, and keeps its extension.
        let odd = a.url(Kind::Play, "a b/c?d#e%f 한.MP4");
        assert!(
            odd.ends_with("/play/a%20b%2Fc%3Fd%23e%25f%20%ED%95%9C.MP4"),
            "{odd}"
        );
        let path = odd
            .strip_prefix(&format!("http://127.0.0.1:{}", a.port()))
            .unwrap();
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nRange: bytes=0-9\r\n\r\n",
            a.port()
        );
        assert_partial(&exchange(&a, head.as_bytes()), 0, 9, SIZE);
        // One server's token opens nothing on the other.
        let other = format!(
            "GET /{ta}/play/m.mp4 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            b.port()
        );
        assert_refused(&exchange(&b, other.as_bytes()), 404);
        // Loopback only: no other address of this machine reaches it.
        let elsewhere = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect("192.0.2.1:9").and(s.local_addr()))
            .ok()
            .map(|addr| addr.ip())
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
        if let Some(ip) = elsewhere {
            let reached = TcpStream::connect_timeout(
                &std::net::SocketAddr::new(ip, a.port()),
                Duration::from_secs(2),
            );
            assert!(reached.is_err(), "the server answers on {ip}");
        }
    });
}

/// `/proc/net/tcp` as the kernel prints it: the server's listener, the server's end of one
/// connection (8080 ← 50000) and the client's end of it, owned by 1000.
const PROC_NET_TCP: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 31271 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1F90 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 31280 1 0000000000000000 20 4 30 10 -1
   2: 0100007F:C350 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 31279 1 0000000000000000 20 4 30 10 -1
   3: 0100007F:C351 0100007F:1F91 01 00000000:00000000 00:00000000 00000000 65534        0 31290 1 0000000000000000 20 4 30 10 -1
";

/// The Linux peer check (06 §9): the client's end is the entry with the server's tuple
/// reversed, and its uid must be sopen's. An absent entry and a doubled one are rejected.
#[test]
fn the_peer_check_finds_the_clients_end_by_the_reversed_tuple() {
    use std::net::SocketAddrV4;
    let end = |port| SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let (server, client) = (end(8080), end(50000));
    assert!(peer_allowed(PROC_NET_TCP.as_bytes(), server, client, 1000));
    // Another uid owns the client's end.
    assert!(!peer_allowed(PROC_NET_TCP.as_bytes(), server, client, 1001));
    assert!(!peer_allowed(
        PROC_NET_TCP.as_bytes(),
        end(8081),
        end(50001),
        1000
    ));
    assert!(peer_allowed(
        PROC_NET_TCP.as_bytes(),
        end(8081),
        end(50001),
        65534
    ));
    // The server's own end (same tuple, not reversed) proves nothing: with only it in the
    // table there is no entry.
    let without: String = PROC_NET_TCP
        .lines()
        .filter(|line| !line.trim_start().starts_with("2:"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(
        !peer_allowed(without.as_bytes(), server, client, 1000),
        "absent"
    );
    assert!(
        !peer_allowed("".as_bytes(), server, client, 1000),
        "an empty table"
    );
    assert!(
        !peer_allowed(PROC_NET_TCP.as_bytes(), server, end(50002), 1000),
        "no such port"
    );
    // Two entries for the one tuple.
    let line = PROC_NET_TCP.lines().nth(3).unwrap();
    let doubled = format!("{PROC_NET_TCP}{line}\n");
    assert!(
        !peer_allowed(doubled.as_bytes(), server, client, 1000),
        "doubled"
    );
    // An entry whose uid is not a number is nobody's.
    let odd = PROC_NET_TCP.replace(
        "00000000  1000        0 31279",
        "00000000     x        0 31279",
    );
    assert_ne!(odd, PROC_NET_TCP);
    assert!(!peer_allowed(odd.as_bytes(), server, client, 1000));
    // An entry cut short of its uid.
    let short = "   2: 0100007F:C350 0100007F:1F90 01 00000000:00000000\n";
    assert!(!peer_allowed(short.as_bytes(), server, client, 1000));
}

/// On Linux, live: a client of sopen's own uid is served, and one run as another uid
/// (`sudo -n -u nobody`) is closed unanswered. Skipped with a note where `sudo -n` cannot run.
#[cfg(target_os = "linux")]
#[test]
fn a_client_of_another_uid_is_closed_unanswered() {
    bounded("a_client_of_another_uid_is_closed_unanswered", || {
        let (source, _control) = threaded(SIZE);
        let server = Server::start(source.clone()).unwrap();
        // bash's own TCP client: it connects, sends the request, says who it is and that it
        // did, and then prints everything that comes back. A send into a connection already
        // closed must not end it before it has spoken.
        let script = format!(
            "trap '' PIPE; exec 3<>/dev/tcp/127.0.0.1/{} || exit 7; \
             printf 'GET {} HTTP/1.1\\r\\nHost: 127.0.0.1:{}\\r\\nRange: bytes=0-9\\r\\n\\r\\n' >&3; \
             echo \"uid=$(id -u) connected\"; cat <&3",
            server.port(),
            target(&server, Kind::Play),
            server.port()
        );
        // SAFETY: `getuid` cannot fail and touches no memory.
        let own_uid = unsafe { libc::getuid() };
        // The client's uid, as it said it after connecting and sending, and what it got.
        let spoke = |out: &std::process::Output| -> (u32, Vec<u8>) {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let said = text
                .strip_prefix("uid=")
                .and_then(|rest| rest.split_once(" connected\n"))
                .and_then(|(uid, _)| uid.parse::<u32>().ok());
            let Some(uid) = said else {
                panic!(
                    "the client did not say it connected: {:?} {:?} {:?}",
                    out.status,
                    text,
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            let line = format!("uid={uid} connected\n");
            (uid, out.stdout[line.len()..].to_vec())
        };
        let own = Command::new("bash").args(["-c", &script]).output().unwrap();
        let (uid, answer) = spoke(&own);
        assert_eq!(uid, own_uid);
        assert!(
            answer.starts_with(b"HTTP/1.1 206 "),
            "sopen's own uid is served: {:?} {:?}",
            String::from_utf8_lossy(&answer),
            String::from_utf8_lossy(&own.stderr)
        );
        let sudo = |args: &[&str]| {
            Command::new("sudo")
                .args(["-n", "-u", "nobody"])
                .args(args)
                .stdin(Stdio::null())
                .output()
        };
        if !sudo(&["true"]).is_ok_and(|out| out.status.success()) {
            eprintln!("skipping the other-uid client: `sudo -n -u nobody` cannot run here");
            return;
        }
        let sent = source.stats().http_sent;
        // Its exit status is not looked at: reading a connection the server reset is an
        // error to `cat`.
        let other = sudo(&["bash", "-c", &script]).unwrap();
        let (uid, answer) = spoke(&other);
        assert_ne!(uid, own_uid, "the other client ran as another uid");
        assert!(
            answer.is_empty(),
            "another uid got an answer: {:?}",
            String::from_utf8_lossy(&answer)
        );
        assert_eq!(source.stats().http_sent, sent);
    });
}

// ---- ffmpeg itself ----

/// ffmpeg + ffprobe, or `None` with the reason printed; a failure under `SOPEN_REQUIRE_FFMPEG`.
fn tools_or_skip() -> Option<crate::ffmpeg::Tools> {
    match crate::ffmpeg::resolve() {
        Ok(t) => Some(t),
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping the byte server's ffmpeg test: {e}");
            None
        }
    }
}

/// 300 s of stereo PCM in a QuickTime file: 57.6 MB with its `moov` at the end (06 §12 V16's
/// shape), written in a moment.
const TONE_S: f64 = 300.0;

fn tone(tools: &crate::ffmpeg::Tools, dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("tone.mov");
    let status = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", "300", "-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success(), "generating {}", path.display());
    path
}

/// A run of ffmpeg or ffprobe, its output in files: waited for with `step` between looks, and
/// killed when it has not ended within `HANG`.
struct Run {
    child: std::process::Child,
    out: std::path::PathBuf,
    err: std::path::PathBuf,
}

impl Run {
    fn start(program: &std::path::Path, args: &[&str], dir: &std::path::Path, name: &str) -> Run {
        let (out, err) = (
            dir.join(format!("{name}.out")),
            dir.join(format!("{name}.err")),
        );
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&out).unwrap())
            .stderr(std::fs::File::create(&err).unwrap())
            .spawn()
            .unwrap();
        Run { child, out, err }
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// `(status, stdout, stderr)`.
    fn wait(&mut self, mut step: impl FnMut()) -> (std::process::ExitStatus, String, String) {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < HANG,
                "the run did not end in {HANG:?}: {}",
                std::fs::read_to_string(&self.err).unwrap_or_default()
            );
            step();
        };
        let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
        (status, read(&self.out), read(&self.err))
    }
}

/// A failed assertion leaves no ffmpeg running: the child is killed and reaped.
impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const WHITELIST: [&str; 2] = ["-protocol_whitelist", "http,tcp"];

fn duration_of(tools: &crate::ffmpeg::Tools, input: &str, dir: &std::path::Path) -> f64 {
    let mut args = vec!["-v", "error"];
    if input.starts_with("http://") {
        args.extend(WHITELIST);
    }
    args.extend(["-show_entries", "format=duration", "-of", "csv=p=0", input]);
    let mut run = Run::start(&tools.ffprobe, &args, dir, "probe");
    let (status, out, err) = run.wait(|| std::thread::sleep(Duration::from_millis(10)));
    assert!(status.success(), "ffprobe {input}: {err}");
    out.trim()
        .parse()
        .unwrap_or_else(|_| panic!("a duration from ffprobe: {out:?} {err:?}"))
}

/// The reads the `sftp-server` was asked for, in order, as `(offset, length)`: its own log at
/// `DEBUG1` has a line per request (`… read "<path>" (handle 0) off N len M`).
fn reads_logged(log: &std::path::Path) -> Vec<(u64, u64)> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter(|line| line.contains(": read \""))
        .filter_map(|line| {
            let (off, len) = line.rsplit_once(" off ")?.1.split_once(" len ")?;
            Some((off.trim().parse().ok()?, len.trim().parse().ok()?))
        })
        .collect()
}

/// Nothing of the stream is on its way or waits to go: what was asked for is in the log.
fn idle(source: &Source) {
    eventually("the stream is idle", || {
        let mut core = source.core();
        !core.queued() && core.pf.adm.in_flight() == 0
    });
}

/// ffmpeg and ffprobe read a file on the local `sftp-server` through the server: the probe
/// gets the file's duration having read its head and its `moov` at the end, and a seek reads
/// only around its target — by the ranges the `sftp-server` itself logged, each phase apart.
/// Neither is a cut, though ffmpeg closes connections early at every seek.
#[test]
fn ffmpeg_probes_and_seeks_through_the_server() {
    bounded("ffmpeg_probes_and_seeks_through_the_server", || {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("sftp-server.log");
        let Some(fs) = SftpRemote::local_server_args(&["-l", "DEBUG1"], Some(&log)) else {
            return;
        };
        let path = tone(&tools, tmp.path());
        let size = std::fs::metadata(&path).unwrap().len();
        let local = duration_of(&tools, path.to_str().unwrap(), tmp.path());
        assert!((local - TONE_S).abs() < 0.01, "{local}");
        // The file's layout, read from the file: `mdat` first, the `moov` after it.
        let front = std::fs::read(&path).unwrap()[..4096].to_vec();
        let tag = front
            .windows(4)
            .position(|w| w == b"mdat")
            .expect("an mdat atom at the front");
        let mdat_size = u32::from_be_bytes(front[tag - 4..tag].try_into().unwrap()) as u64;
        let (data, moov) = (tag as u64 + 4, tag as u64 - 4 + mdat_size);
        assert!(moov > size - MIB && moov < size, "the moov is at the end");
        // 48 kHz, stereo, 16 bits: 192 000 bytes a second.
        let second = 192_000;
        let target = data + 240 * second;
        let transport = Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap();
        let source = Source::open(transport, no_reopen(), Arc::new(|| {})).unwrap();
        assert_eq!(source.size(), size);
        let server = Server::start(source.clone()).unwrap();
        let url = server.url(Kind::Play, "tone.mov");
        // A `play` connection's reader, before any rate is set (06 §4).
        let (ahead, behind) = limits(ReaderKind::Media { bytes_per_s: 0 });
        // What may be fetched past a place when `demand` bytes from it were sent: the
        // reader's window, what was in flight, and the blocks' edges.
        let reach = |demand: u64| demand + ahead + CAP_MAX + 2 * BLOCK;
        // The probe: the head, then the `moov` at the end.
        let remote = duration_of(&tools, &url, tmp.path());
        assert!(
            (remote - local).abs() < 1e-6,
            "the duration through the server {remote}, of the file {local}"
        );
        idle(&source);
        let probed = source.stats();
        let probe_reads = reads_logged(&log);
        assert!(!probe_reads.is_empty(), "the server's log has the reads");
        let tail = moov.saturating_sub(behind + BLOCK);
        for &(off, len) in &probe_reads {
            assert!(
                off + len <= reach(probed.http_sent) || off >= tail,
                "the probe read {len} bytes at {off}: neither the head (to {}) nor the \
                 moov (from {tail})",
                reach(probed.http_sent)
            );
        }
        assert!(probe_reads.iter().any(|&(off, _)| off >= tail));
        assert!(probed.http_sent > 0);
        assert!(
            probed.http_sent <= size / 8,
            "the probe was sent {} of {size} bytes",
            probed.http_sent
        );
        // A seek to 240 s of 300, one second decoded.
        let mut args = vec!["-hide_banner", "-loglevel", "error", "-nostdin"];
        args.extend(WHITELIST);
        args.extend(["-ss", "240", "-i", &url, "-t", "1", "-f", "null", "-"]);
        let mut run = Run::start(&tools.ffmpeg, &args, tmp.path(), "seek");
        let (status, _out, err) = run.wait(|| std::thread::sleep(Duration::from_millis(10)));
        assert!(status.success(), "ffmpeg -ss 240: {err}");
        idle(&source);
        let sought = source.stats();
        let seek_reads = reads_logged(&log)[probe_reads.len()..].to_vec();
        let seek_sent = sought.http_sent - probed.http_sent;
        // ffmpeg starts at the chunk its sample is in: less than a second before it.
        let (from, to) = (
            target.saturating_sub(second + behind + BLOCK),
            target + reach(seek_sent),
        );
        let span = |reads: &[(u64, u64)]| {
            let low = reads.iter().map(|r| r.0).min().unwrap_or(0);
            let high = reads.iter().map(|r| r.0 + r.1).max().unwrap_or(0);
            (low, high, reads.iter().map(|r| r.1).sum::<u64>())
        };
        eprintln!(
            "byte server, ffmpeg on {size} bytes: probe sent {} in {} reads {:?}; \
             seek to byte {target} sent {seek_sent} in {} reads {:?}, window {from}..{to}",
            probed.http_sent,
            probe_reads.len(),
            span(&probe_reads),
            seek_reads.len(),
            span(&seek_reads)
        );
        assert!(!seek_reads.is_empty(), "the seek read its place");
        // The run is a new ffmpeg: it reads the head again on a new connection, whose own
        // readahead may pass what the probe's fetched, and the `moov`. Everything else it
        // fetches lies around the target.
        let mut around = 0;
        for &(off, len) in &seek_reads {
            let at_target = off >= from && off + len <= to;
            assert!(
                at_target || off + len <= reach(seek_sent) || off >= tail,
                "the seek to byte {target} read {len} bytes at {off}: outside {from}..{to}, \
                 the head (to {}) and the moov (from {tail})",
                reach(seek_sent)
            );
            around += u64::from(at_target) * len;
        }
        assert!(around > 0, "nothing was fetched around the target");
        assert!(
            seek_reads
                .iter()
                .any(|&(off, len)| off <= target && target < off + len + second),
            "nothing was read at the target"
        );
        // The coarse ceilings, as smoke checks.
        assert!(
            sought.http_sent <= size / 4,
            "probe and seek were sent {} of {size} bytes",
            sought.http_sent
        );
        assert!(
            sought.fetched.total <= size / 2,
            "probe and seek fetched {} of {size} bytes",
            sought.fetched.total
        );
        for kind in Kind::ALL {
            assert_eq!(server.cuts(kind), 0, "a connection ffmpeg closed is no cut");
        }
        assert_eq!(sought.cuts, 0);
        assert_eq!(source.state(), State::Live);
    });
}

/// The file changes on the `sftp-server` while ffmpeg reads it through the server: the
/// response under way is cut, and the account has it — whatever ffmpeg's exit status says
/// (06 §7.4: it exits 0 on a body cut short).
#[test]
fn a_change_under_ffmpeg_is_a_cut_the_account_records() {
    bounded("a_change_under_ffmpeg_is_a_cut_the_account_records", || {
        let Some(tools) = tools_or_skip() else { return };
        let Some(fs) = SftpRemote::local_server() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tone(&tools, tmp.path());
        let size = std::fs::metadata(&path).unwrap().len();
        let transport = Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap();
        let settings = Settings {
            poll_every: NEVER,
            ..Settings::plain()
        };
        let opening = Opening {
            label: "local".into(),
            began: Instant::now(),
            sniff: 0,
        };
        // The engine is stepped here, so the test decides when anything more is fetched.
        let (source, mut engine) =
            Source::build(transport, no_reopen(), Arc::new(|| {}), opening, settings);
        let server = Server::start(source.clone()).unwrap();
        let url = server.url(Kind::Render, "tone.mov");
        let mut args = vec!["-hide_banner", "-loglevel", "error", "-nostdin"];
        args.extend(WHITELIST);
        args.extend(["-i", &url, "-f", "null", "-"]);
        let mut run = Run::start(&tools.ffmpeg, &args, tmp.path(), "render");
        // A good part of the file has gone to ffmpeg, and most has not been fetched.
        let started = Instant::now();
        while source.stats().http_sent < 8 * MIB {
            assert!(started.elapsed() < HANG, "ffmpeg does not read");
            assert!(run.running(), "ffmpeg ended before the change");
            engine.step(Some(Duration::from_millis(50)));
        }
        // No step now: ffmpeg takes what is cached, and its connection waits for a block.
        eventually("ffmpeg's connection waits for a block", || {
            source.core().pf.waiting_readers() >= 1
        });
        let before = source.stats();
        assert!(before.fetched.total < size / 2, "{before:?}");
        assert_eq!((server.cuts(Kind::Render), before.cuts), (0, 0));
        // A write in place, in another whole second (SFTP carries no more than that).
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all_at(&[0xEE; 4096], size - 2 * MIB).unwrap();
        let opened = source.opened().mtime.duration_since(UNIX_EPOCH).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(opened.as_secs() + 10))
            .unwrap();
        // ffmpeg ends first, and the account is read the moment it has: nothing waits for
        // the count in between.
        let (status, _out, err) = run.wait(|| {
            engine.step(Some(Duration::from_millis(50)));
        });
        let counted = server.cuts(Kind::Render);
        assert!(
            counted >= 1,
            "ffmpeg ended ({status}) and the account has no cut"
        );
        let cut = server.last_cut(Kind::Render).expect("the cut");
        eprintln!(
            "byte server, a change under ffmpeg: {status}, stderr {:?}; cut {cut:?}",
            err.trim()
        );
        assert_eq!(cut.reason, CutReason::Changed);
        assert!(cut.sent < cut.length, "{cut:?}");
        assert_eq!(source.state(), State::Changed);
        let after = source.stats();
        assert_eq!(after.cuts, counted);
        assert!(after.http_sent < size, "ffmpeg was not sent the whole file");
        assert_eq!((server.cuts(Kind::Play), server.cuts(Kind::Bulk)), (0, 0));
    });
}
