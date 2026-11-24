//! 06 §7.2 W5a and §12's V13 and V13a: the text viewer on a remote file read through a
//! `Source`. The Source is a real one on the `Fake` (06 §2), its engine stepped by hand on the
//! simulated clock — a latency, a rate, a fault — so a test decides when a reply arrives; a
//! [`Gate`] in front of the screen's reader holds single reads Pending or fails them. Tests
//! assert what is on screen and what was fetched, never wall-clock time.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::remote::{fake, Asked, Change, Clock, FakeControl, FakeFile, Fault, Link};
use crate::stream::{Engine, Opening, Settings, State as Streamed, BLOCK};
use eframe::egui::{Key, Modifiers};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Only catches a hang.
const HANG: Duration = Duration::from_secs(120);
/// No poll within a test that does not ask for one.
const NEVER: Duration = Duration::from_secs(1_000_000);

/// A Source on the Fake, its engine stepped by hand.
struct Net {
    source: Source,
    engine: Engine,
    control: FakeControl,
    /// The stats lines (06 §11), as `SOPEN_STREAM_STATS=1` prints them.
    lines: Arc<Mutex<Vec<String>>>,
    /// Why a new connection is refused, while one is.
    down: Arc<Mutex<Option<String>>>,
    /// How often the Source asked for a new connection.
    reopens: Arc<AtomicUsize>,
}

/// 06 §12's gate link: 20 ms and 5 MB/s, in simulated time.
fn gate() -> Link {
    Link::gate()
}

/// A slow line: 150 ms, 100 KiB/s.
fn slow() -> Link {
    Link {
        latency: Duration::from_millis(150),
        rate: 100 * 1024,
        clip: None,
        clock: Clock::Simulated,
    }
}

fn net_polling(bytes: &[u8], link: Link, poll_every: Duration) -> Net {
    net_on(FakeFile::bytes(bytes.to_vec()), link, poll_every)
}

/// A Source on the Fake's `file` (its bytes, or [`FakeFile::pattern`] of any size).
fn net_on(file: FakeFile, link: Link, poll_every: Duration) -> Net {
    let (transport, control) = fake("/remote/file", file, link);
    let base = transport.now() - control.elapsed();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let down = Arc::new(Mutex::new(None::<String>));
    let (sink, clock, refused, again) = (
        lines.clone(),
        control.clone(),
        down.clone(),
        control.clone(),
    );
    let settings = Settings {
        poll_every,
        stats: Some(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        })),
        throttle: None,
        clock: Arc::new(move || base + clock.elapsed()),
    };
    let opening = Opening {
        label: "box:/remote/file".into(),
        began: Instant::now(),
        sniff: 0,
    };
    let reopens = Arc::new(AtomicUsize::new(0));
    let opens = reopens.clone();
    let (source, engine) = Source::build(
        transport,
        Box::new(move || {
            opens.fetch_add(1, Ordering::SeqCst);
            match refused.lock().unwrap().clone() {
                Some(why) => Err(anyhow::anyhow!(why)),
                None => again.open(),
            }
        }),
        Arc::new(|| {}),
        opening,
        settings,
    );
    Net {
        source,
        engine,
        control,
        lines,
        down,
        reopens,
    }
}

fn net(bytes: &[u8], link: Link) -> Net {
    net_polling(bytes, link, NEVER)
}

impl Net {
    /// The viewer on the file, as the routes will open it: nothing read yet.
    fn window(&self, name: &str) -> TextWindow {
        TextWindow::new(
            TextDoc::streamed(self.source.clone()),
            "box:/remote/file".into(),
            name,
            Vec::new(),
        )
    }

    /// Sends what may go; how many replies are on their way.
    fn out(&mut self) -> usize {
        self.engine.pump();
        self.engine.transport().map_or(0, |r| r.pending())
    }

    /// Takes one reply, if one is on its way.
    fn reply(&mut self) -> bool {
        if self.out() == 0 {
            return false;
        }
        for _ in 0..1000 {
            if self.engine.step(None) {
                return true;
            }
        }
        panic!("no reply came");
    }

    /// Takes replies until nothing is on its way and nothing more goes.
    fn settle(&mut self) {
        for _ in 0..1_000_000 {
            if !self.reply() {
                return;
            }
        }
        panic!("the stream did not settle");
    }

    /// Simulated time passing, the engine stepped through it (its poll, its retries).
    fn pass_time(&mut self, d: Duration) {
        let until = self.control.elapsed() + d;
        for _ in 0..1_000_000 {
            let left = until.saturating_sub(self.control.elapsed());
            if left.is_zero() {
                return;
            }
            self.engine.step(Some(left));
        }
        panic!("simulated time did not pass");
    }

    /// Simulated time passing with nothing sent meanwhile.
    fn advance(&self, d: Duration) {
        let mut idle = self.control.open().expect("the path is there");
        assert!(idle.next(Some(d)).is_none());
    }

    /// Every read sent so far, as `(offset, length)`.
    fn reads(&self) -> Vec<(u64, u32)> {
        self.control
            .sent()
            .iter()
            .filter_map(|s| match s.asked {
                Asked::Read { off, len } => Some((off, len)),
                _ => None,
            })
            .collect()
    }

    /// New connections the Source has asked for so far.
    fn reopens(&self) -> usize {
        self.reopens.load(Ordering::SeqCst)
    }

    fn requests(&self) -> usize {
        self.control.sent().len()
    }

    /// Bytes received from the remote so far.
    fn fetched(&self) -> u64 {
        self.source.stats().fetched.total
    }

    /// The stats line of `event`, if it was written.
    fn line(&self, event: &str) -> Option<String> {
        let lines = self.lines.lock().unwrap();
        lines
            .iter()
            .find(|l| l.starts_with(&format!("stream {event} ")))
            .cloned()
    }
}

/// Passes and replies in turn — a pass, then one reply if one is on its way — until `done`.
fn run(
    drv: &mut Driver,
    win: &mut TextWindow,
    net: &mut Net,
    what: &str,
    mut done: impl FnMut(&TextWindow, &Driver) -> bool,
) {
    let started = Instant::now();
    loop {
        drv.idle(win, 1);
        if done(win, drv) {
            return;
        }
        if !net.reply() {
            // Nothing is on its way: a worker may be about to ask, or to answer.
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(started.elapsed() < HANG, "{what}: not within {HANG:?}");
    }
}

/// Runs a test's body on a thread of its own and fails when it does not return: a pass that
/// blocks (a frame waiting for bytes, as a worker may) would hang the test instead.
fn bounded(what: &str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(2 * HANG) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = thread.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: did not return in {:?} (a pass blocked?)", 2 * HANG)
        }
    }
}

/// The lines of `body` as a viewer shows them: without their newline, or a `\r` before it.
fn lines_of(body: &str) -> Vec<&str> {
    body.strip_suffix('\n')
        .unwrap_or(body)
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// How many rows of the page the last pass painted as `loading…` (the body's, not the status
/// row's).
fn loading_rows(drv: &Driver) -> usize {
    let said = usize::from(status(drv) == "loading…");
    painted(drv).iter().filter(|t| *t == "loading…").count() - said
}

/// Every text the last pass painted.
fn painted(drv: &Driver) -> Vec<String> {
    drv.texts().into_iter().map(|(_, _, t)| t).collect()
}

fn says(drv: &Driver, needle: &str) -> bool {
    painted(drv).iter().any(|t| t.contains(needle))
}

/// The rows of the page on screen, as text.
fn rows(win: &TextWindow) -> Vec<String> {
    match &win.page {
        Some(p) => p.rows.iter().map(|r| r.cut.text()).collect(),
        None => Vec::new(),
    }
}

fn has_rows(win: &TextWindow) -> bool {
    !rows(win).is_empty()
}

/// Each line's colours on the page on screen, once highlighted.
fn colors(win: &TextWindow) -> Vec<Vec<Color32>> {
    win.page.as_ref().map_or(Vec::new(), |p| p.colors.clone())
}

/// Where the text painted with `prefix` at its start begins across the window.
fn text_x(drv: &Driver, prefix: &str) -> f32 {
    drv.texts()
        .into_iter()
        .find(|(_, _, t)| t.starts_with(prefix))
        .map(|(r, _, _)| r.left())
        .unwrap_or_else(|| panic!("no text starts with {prefix:?}: {:?}", painted(drv)))
}

/// The status row's words for the page: the lowest text painted that says lines, bytes,
/// `loading…` or `empty` (the rows of the body, which may say the same, are above it).
fn status(drv: &Driver) -> String {
    drv.texts()
        .into_iter()
        .filter(|(_, _, t)| {
            t.starts_with("line ")
                || t.starts_with("lines ")
                || t.starts_with("bytes 0x")
                || t.starts_with("loading…")
                || t == "empty"
        })
        .max_by(|a, b| a.0.top().total_cmp(&b.0.top()))
        .map(|(_, _, t)| t)
        .unwrap_or_default()
}

/// `n` numbered lines of 26 bytes.
fn numbered(n: u32) -> String {
    (0..n)
        .map(|i| format!("line {i:06} of a long log\n"))
        .collect()
}

/// A Rust file of `n` statements.
fn rust(n: u32) -> String {
    let mut body = String::from("// numbers, one a line\nfn main() {\n");
    for i in 0..n {
        body.push_str(&format!("    let v{i:05} = \"{i}\"; // {i}\n"));
    }
    body.push_str("}\n");
    body
}

/// A Rust file of `n` statements as wide as fits a row: several blocks of few lines, so that
/// highlighting to its end stays cheap (the parser's time goes by lines).
fn wide_rust(n: u32) -> String {
    let mut body = String::from("// numbers, one a line\nfn main() {\n");
    let filler = "012345678".repeat(5);
    for i in 0..n {
        body.push_str(&format!("    let v{i:05} = \"{filler}\"; // {i}\n"));
    }
    body.push_str("}\n");
    body
}

/// The same bytes as a local file, in today's viewer: what a streamed file must come to.
fn twin(dir: &Path, name: &str, body: &[u8]) -> TextWindow {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:/remote/file".into(),
        name,
        Vec::new(),
    );
    win.live = false;
    win
}

/// A document on `net` whose index is whole though nothing of the file is cached: every page
/// read is a cold one (what any jump is in a file larger than the cache, and W5b's every jump).
fn indexed(net: &Net, body: &[u8]) -> TextDoc {
    let mut doc = TextDoc::streamed(net.source.clone());
    let mut scan = Scan {
        marks: vec![Mark { line: 0, offset: 0 }],
        ..Scan::default()
    };
    scan.feed(body, body.len() as u64);
    doc.install(&scan);
    let Backing::Stream(stream) = &mut doc.backing else {
        unreachable!("a streamed document");
    };
    stream.scan = scan;
    doc
}

/// What a [`Gate`] does to the next reads.
#[derive(Default)]
struct Held {
    /// This many reads go through first.
    skip: usize,
    /// Then this many reads come back Pending.
    pending: usize,
    /// Then this many fail.
    failing: usize,
}

/// A slow `Bytes` in front of the screen's reader: the next reads are Pending or fail, as
/// [`Held`] says, whatever the Source has cached; the others go through.
struct Gate {
    inner: Box<dyn Bytes + Send>,
    held: Arc<Mutex<Held>>,
}

const HUNG_UP: &str = "the remote hung up";

impl Bytes for Gate {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<crate::stream::Result<bytes::Bytes>> {
        let mut held = self.held.lock().unwrap();
        if held.skip > 0 {
            held.skip -= 1;
            drop(held);
            return self.inner.try_read(off, len);
        }
        if held.pending > 0 {
            held.pending -= 1;
            return Poll::Pending;
        }
        if held.failing > 0 {
            held.failing -= 1;
            return Poll::Ready(Err(ReadError::Failed(HUNG_UP.into())));
        }
        drop(held);
        self.inner.try_read(off, len)
    }

    fn read_at(
        &mut self,
        off: u64,
        buf: &mut [u8],
        cancel: &AtomicBool,
    ) -> crate::stream::Result<usize> {
        self.inner.read_at(off, buf, cancel)
    }
}

/// Puts a [`Gate`] in front of the reader `win`'s pages are read through.
fn gated(win: &mut TextWindow) -> Arc<Mutex<Held>> {
    gated_doc(&mut win.doc)
}

/// Puts a [`Gate`] in front of the reader `doc`'s lines are read through.
fn gated_doc(doc: &mut TextDoc) -> Arc<Mutex<Held>> {
    let Backing::Stream(stream) = &mut doc.backing else {
        unreachable!("a streamed document");
    };
    let held = Arc::new(Mutex::new(Held::default()));
    let inner = std::mem::replace(
        &mut stream.reader,
        Box::new(File::open("/dev/null").unwrap()),
    );
    stream.reader = Box::new(Gate {
        inner,
        held: held.clone(),
    });
    held
}

/// **V13.** A remote file of at most 16 MiB shows its first screen before its index finishes:
/// one block fetched, by one read. Once indexed it has the line count, the rows and the
/// highlighting today's viewer gives the same bytes — at its start and at its end.
#[test]
fn v13_a_small_remote_file_shows_its_first_screen_before_its_index_finishes() {
    bounded(
        "v13_a_small_remote_file_shows_its_first_screen_before_its_index_finishes",
        || {
            let body = wide_rust(2_600);
            assert!(body.len() as u64 > 2 * BLOCK);
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("main.rs");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            assert_eq!(rows(&win)[1], "fn main() {");
            // What that pass queued goes out, as the engine's own thread sends it: nothing did.
            net.out();
            assert_eq!(net.reads(), [(0, BLOCK as u32)], "one read before `first`");
            assert_eq!(net.fetched(), BLOCK);
            assert_eq!(win.doc.size(), BLOCK, "the index is of the first block");
            assert!(!win.doc.whole());
            let first = net.line("first").expect("`first` is on the stats line");
            assert!(
                first.contains(" fetched=65536 demand=65536 prefetch=0 bulk=0 "),
                "{first}"
            );
            // Once indexed: as today's viewer has the same bytes.
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            let tmp = tempfile::tempdir().unwrap();
            let mut local = twin(tmp.path(), "main.rs", body.as_bytes());
            let mut local_drv = Driver::new(800.0, 500.0);
            local_drv.until(&mut local, 60, |w| w.ready());
            assert_eq!(win.doc.line_count(), 2_603);
            assert_eq!(win.doc.line_count(), local.doc.line_count());
            assert_eq!(win.doc.marks, local.doc.marks);
            assert_eq!(rows(&win), rows(&local));
            // The file's own first lines (none is wide enough to wrap).
            let lines = lines_of(&body);
            let visible = win.visible as usize;
            assert_eq!(rows(&win), lines[..visible]);
            assert!(colors(&win).iter().any(|line| {
                let distinct: std::collections::HashSet<_> = line.iter().collect();
                distinct.len() >= 2
            }));
            assert_eq!(colors(&win), colors(&local), "highlighted as today");
            assert!(says(&drv, "2,603 lines"), "{:?}", painted(&drv));
            assert_eq!(status(&drv), status(&local_drv));
            // The end of the file: its last page, highlighted with the state carried from byte 0.
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                w.ready()
            });
            local_drv.key(&mut local, Key::End, command(false));
            local_drv.until(&mut local, 60, |w| w.ready());
            assert_eq!(rows(&win).last().map(String::as_str), Some("}"));
            assert_eq!(rows(&win), rows(&local));
            assert_eq!(
                rows(&win),
                lines[lines.len() - visible..],
                "its own last lines"
            );
            assert_eq!(colors(&win), colors(&local), "highlighted as today");
            assert_eq!(status(&drv), status(&local_drv));
            assert!(
                net.fetched() <= body.len() as u64,
                "no block is fetched twice"
            );
        },
    );
}

/// 06 §10: the first screen costs one 64 KiB block at offset 0 and nothing else — wrapped or
/// not, whatever the lines (one of a megabyte, CRLF ones behind a first line longer than the
/// block), on a fast line or a slow one. `first` and `ready()` mean its rows are drawn.
#[test]
fn the_first_screen_costs_one_block_whatever_the_lines() {
    bounded(
        "the_first_screen_costs_one_block_whatever_the_lines",
        || {
            let one_line = "x".repeat(1 << 20);
            let mut crlf = "y".repeat(100_000);
            crlf.push_str("\r\n");
            for i in 0..5_000 {
                crlf.push_str(&format!("row {i}\r\n"));
            }
            let log = numbered(12_000);
            for (what, body) in [("one line", &one_line), ("crlf", &crlf), ("a log", &log)] {
                for wrap in [true, false] {
                    for link in [gate(), slow()] {
                        let mut net = net(body.as_bytes(), link);
                        let mut win = net.window("notes.txt");
                        win.wrap = wrap;
                        let mut drv = Driver::new(800.0, 500.0);
                        run(&mut drv, &mut win, &mut net, what, |w, _| has_rows(w));
                        let at = format!("{what}, wrap {wrap}");
                        assert!(win.ready(), "{at}: the rows are drawn");
                        // What that pass queued goes out, as the engine's own thread sends it.
                        net.out();
                        assert_eq!(net.reads(), [(0, BLOCK as u32)], "{at}");
                        assert_eq!(net.fetched(), BLOCK, "{at}");
                        let first = net.line("first").expect("`first` is on the stats line");
                        assert!(first.contains(" fetched=65536 "), "{at}: {first}");
                        // The rows are the file's own bytes, from byte 0.
                        let lines = lines_of(body);
                        let visible = win.visible as usize;
                        let shown = rows(&win);
                        assert!(shown.iter().all(|r| !r.is_empty()), "{at}: an empty row");
                        // The lines the first block holds (its last one may go on past it).
                        let block = &body.as_bytes()[..BLOCK as usize];
                        let in_block = block.iter().filter(|b| **b == b'\n').count()
                            + usize::from(block.last() != Some(&b'\n'));
                        if !wrap {
                            // One row to a line: the line whole, or — a row is read under
                            // a bound of its own (06 §7.2) — its first bytes, past the edge.
                            assert_eq!(shown.len(), visible.min(in_block), "{at}");
                            for (row, line) in shown.iter().zip(&lines) {
                                assert!(line.starts_with(row.as_str()), "{at}: {row}");
                                assert!(row == line || row.len() >= 90, "{at}: {row}");
                            }
                        } else if in_block >= visible {
                            assert_eq!(shown, lines[..visible], "{at}");
                        } else {
                            assert_eq!(shown.len(), visible, "{at}: the block fills the screen");
                            let all = shown.concat();
                            assert_eq!(all, lines[0][..all.len()], "{at}");
                        }
                        if wrap || in_block >= visible {
                            continue;
                        }
                        // Unwrapped, below a line that runs past the block: `loading…` rows, which
                        // the index pass replaces with the lines (or with nothing: there are none).
                        drv.idle(&mut win, 1);
                        assert_eq!(loading_rows(&drv), visible - in_block, "{at}");
                        run(&mut drv, &mut win, &mut net, what, |w, _| {
                            w.doc.whole() && w.ready()
                        });
                        drv.idle(&mut win, 1);
                        assert_eq!(loading_rows(&drv), 0, "{at}: {:?}", painted(&drv));
                        let shown = rows(&win);
                        assert_eq!(shown.len(), visible.min(lines.len()), "{at}");
                        for (row, line) in shown.iter().zip(&lines) {
                            assert!(line.starts_with(row.as_str()), "{at}: once indexed");
                            assert!(row == line || row.len() >= 90, "{at}: {row}");
                        }
                    }
                }
            }
        },
    );
}

/// `key` going down, `modifiers` held.
fn key_down(key: Key, modifiers: Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

/// One pass with no input at all, as eframe runs one that a repaint request brought.
fn quiet_pass(drv: &Driver, win: &mut TextWindow, time: f64) -> Duration {
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            Pos2::ZERO,
            egui::Vec2::new(800.0, 500.0),
        )),
        time: Some(time),
        max_texture_side: Some(8192),
        focused: true,
        ..Default::default()
    };
    let out = drv.ctx.run_ui(raw, |ui| win.ui(ui));
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(Duration::MAX, |v| v.repaint_delay)
}

/// A window waiting for bytes costs no passes: once it has settled, no pass asks for another
/// at once, and none schedules one sooner than the slow notice's (06 §5: 10 s after the wait
/// began) — the Source's waker brings the pass that reads again.
fn waits_quietly(drv: &Driver, win: &mut TextWindow, what: &str) {
    let asked = Arc::new(AtomicUsize::new(0));
    let count = asked.clone();
    drv.ctx.set_request_repaint_callback(move |info| {
        if info.delay.is_zero() {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    for k in 0..4 {
        quiet_pass(drv, win, 100.0 + f64::from(k));
    }
    let before = asked.load(Ordering::SeqCst);
    for k in 4..10 {
        let next = quiet_pass(drv, win, 100.0 + f64::from(k));
        assert!(
            next > Duration::from_secs(8),
            "{what}: a waiting pass scheduled another in {next:?}"
        );
    }
    assert_eq!(
        asked.load(Ordering::SeqCst),
        before,
        "{what}: a waiting pass asked for another"
    );
    drv.ctx.set_request_repaint_callback(|_| {});
}

/// **V13a.** Before the first block the body and the status row say `loading…`; neither
/// `empty` nor `0 lines` appears for a file whose size is not 0; the top bar has the file's
/// size from the Source. The window is not ready, and no pass asks for another while it waits:
/// the Source's waker brings the next.
#[test]
fn before_the_first_block_the_viewer_says_loading_and_never_empty() {
    bounded(
        "before_the_first_block_the_viewer_says_loading_and_never_empty",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            for pass in 0..6 {
                drv.idle(&mut win, 1);
                // The request goes out; no reply is taken.
                assert!(net.out() > 0);
                let texts = painted(&drv);
                assert_eq!(
                    texts.iter().filter(|t| *t == "loading…").count(),
                    2,
                    "pass {pass}: the body and the status row: {texts:?}"
                );
                assert!(
                    !texts
                        .iter()
                        .any(|t| t.contains("empty") || t.contains("0 lines")),
                    "pass {pass}: {texts:?}"
                );
                assert!(
                    says(&drv, "lines known to 0 B of 312.0 KB"),
                    "pass {pass}: {texts:?}"
                );
                assert!(!win.ready(), "pass {pass}");
                assert!(!has_rows(&win), "pass {pass}");
            }
            waits_quietly(&drv, &mut win, "before the first block");
            assert_eq!(net.reads(), [(0, BLOCK as u32)], "asked once");
            // The block arrives: the screen, and no more `loading…`.
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            // (The bars are drawn before the page is read: the pass it asked for has them.)
            drv.idle(&mut win, 1);
            assert!(!says(&drv, "loading…"), "{:?}", painted(&drv));
            assert!(
                says(&drv, "lines known to 65.5 KB of 312.0 KB"),
                "{:?}",
                painted(&drv)
            );
            assert_eq!(status(&drv), format!("lines 1–{}", rows(&win).len()));
            assert!(win.ready());
            // A file of no bytes is empty, and says so at once.
            let mut net = net_polling(b"", gate(), NEVER);
            let mut win = net.window("none.log");
            drv.idle(&mut win, 2);
            assert_eq!(status(&drv), "empty", "{:?}", painted(&drv));
            assert!(says(&drv, "0 lines · 0 B"), "{:?}", painted(&drv));
            assert!(!says(&drv, "loading…"));
            assert_eq!(net.out(), 0, "nothing to read");
            assert!(win.ready());
        },
    );
}

/// **V13a.** Keys pressed while a page read is Pending are applied when its block arrives, and
/// the page on screen stays until then; the status row describes that page, with `loading…`,
/// never where the keys asked to go; the window is not ready meanwhile.
#[test]
fn keys_pressed_while_a_page_is_on_its_way_are_applied_when_it_arrives() {
    bounded(
        "keys_pressed_while_a_page_is_on_its_way_are_applied_when_it_arrives",
        || {
            let body = numbered(40_000);
            for wrap in [true, false] {
                let mut net = net(body.as_bytes(), gate());
                let mut win = TextWindow::new(
                    indexed(&net, body.as_bytes()),
                    "box:/remote/file".into(),
                    "big.log",
                    Vec::new(),
                );
                win.wrap = wrap;
                let mut drv = Driver::new(800.0, 500.0);
                run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                    has_rows(w)
                });
                net.settle();
                drv.idle(&mut win, 2);
                let before = rows(&win);
                let said = status(&drv);
                assert_eq!(said, format!("lines 1–{} of 40000", before.len()));
                assert!(win.ready());
                // The end of the file is megabytes away, and not here.
                let end = if wrap {
                    command(false)
                } else {
                    Modifiers::NONE
                };
                drv.key(&mut win, Key::End, end);
                assert!(net.out() > 0, "the far block is asked for");
                assert_eq!(rows(&win), before, "wrap {wrap}: the page on screen stays");
                assert_eq!(status(&drv), format!("{said} · loading…"), "wrap {wrap}");
                assert!(!win.ready(), "wrap {wrap}");
                waits_quietly(&drv, &mut win, "a page on its way");
                // Two more keys while it waits.
                drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
                drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
                assert_eq!(rows(&win), before, "wrap {wrap}: the page on screen stays");
                assert_eq!(status(&drv), format!("{said} · loading…"), "wrap {wrap}");
                run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                    w.ready()
                });
                drv.idle(&mut win, 1);
                // As today's viewer answers the same keys.
                let tmp = tempfile::tempdir().unwrap();
                let mut local = twin(tmp.path(), "big.log", body.as_bytes());
                local.wrap = wrap;
                let mut local_drv = Driver::new(800.0, 500.0);
                local_drv.until(&mut local, 60, |w| w.ready());
                local_drv.key(&mut local, Key::End, end);
                local_drv.key(&mut local, Key::ArrowUp, Modifiers::NONE);
                local_drv.key(&mut local, Key::ArrowUp, Modifiers::NONE);
                assert_eq!(rows(&win), rows(&local), "wrap {wrap}");
                // The file's own lines: its last page, two rows up.
                let shown = rows(&win);
                let last = 40_000 - 2;
                let want: Vec<String> = (last - shown.len()..last)
                    .map(|i| format!("line {i:06} of a long log"))
                    .collect();
                assert_eq!(shown, want, "wrap {wrap}");
                assert_eq!(shown.len() as u64, win.visible, "wrap {wrap}");
                assert_eq!(status(&drv), status(&local_drv), "wrap {wrap}");
                assert!(!says(&drv, "loading…"));
            }
        },
    );
}

/// X3's file: block 0 is 1024 lines of 64 bytes, then one line of 200 `y`, then more lines.
fn edge() -> Vec<u8> {
    let mut body = Vec::new();
    for _ in 0..1024 {
        body.extend_from_slice(&[b'x'; 63]);
        body.push(b'\n');
    }
    assert_eq!(body.len() as u64, BLOCK);
    body.extend_from_slice(&[b'y'; 200]);
    body.push(b'\n');
    for _ in 0..3000 {
        body.extend_from_slice(&[b'z'; 63]);
        body.push(b'\n');
    }
    body
}

/// **V13a.** After a Pending read and after a failed read the read window still holds the
/// bytes it had, and no row shows a byte the file does not contain: a row down onto a longer
/// line, its read held Pending for 1 to 4 passes or failed once, comes out as the line.
#[test]
fn a_pending_or_a_failed_read_puts_no_byte_on_screen_that_the_file_does_not_hold() {
    bounded(
        "a_pending_or_a_failed_read_puts_no_byte_on_screen_that_the_file_does_not_hold",
        || {
            let body = edge();
            for (pending, failing) in [(1, 0), (2, 0), (3, 0), (4, 0), (0, 1), (2, 1)] {
                let at = format!("{pending} Pending, {failing} failed");
                let mut net = net(&body, gate());
                let mut win = net.window("edge.txt");
                let mut drv = Driver::new(800.0, 500.0);
                run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                    w.doc.whole() && w.ready()
                });
                // The page whose last row is the last line of block 0.
                let visible = win.visible;
                win.jump = Some(Jump::Byte(BLOCK - 64 * visible));
                drv.idle(&mut win, 3);
                let before = rows(&win);
                assert_eq!(before.len() as u64, visible, "{at}");
                assert!(before.iter().all(|r| *r == "x".repeat(63)), "{at}");
                // One row down brings the 200 y's in: a longer read than the window's last.
                let held = gated(&mut win);
                *held.lock().unwrap() = Held {
                    skip: 0,
                    pending,
                    failing,
                };
                drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
                for _ in 0..8 {
                    let now = rows(&win);
                    for row in &now {
                        assert!(
                            row.chars().all(|c| matches!(c, 'x' | 'y' | 'z')),
                            "{at}: a row of bytes the file does not hold: {row:?}"
                        );
                    }
                    drv.idle(&mut win, 1);
                }
                let after = rows(&win);
                assert_eq!(after.len() as u64, visible, "{at}");
                // The row the key asked for: the start of the 200 y's, as wide as the window.
                let last = after.last().unwrap();
                assert!(last.len() > 63 && *last == "y".repeat(last.len()), "{at}");
                assert_eq!(after[..after.len() - 1], before[1..], "{at}");
                let left = held.lock().unwrap();
                assert_eq!(
                    (left.pending, left.failing),
                    (0, 0),
                    "{at}: the held reads were made"
                );
            }
        },
    );
}

/// 06 §5: a read that fails keeps the screen and the keys; the status row has the reason, and
/// the next pass reads again.
#[test]
fn a_failed_page_read_keeps_the_screen_and_says_why() {
    bounded("a_failed_page_read_keeps_the_screen_and_says_why", || {
        let body = numbered(40_000);
        let mut net = net(body.as_bytes(), gate());
        let mut win = net.window("big.log");
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the index", |w, _| {
            w.doc.whole() && w.ready()
        });
        let before = rows(&win);
        let held = gated(&mut win);
        held.lock().unwrap().failing = 1;
        // One pass with the key: its read fails.
        drv.frame(
            &mut win,
            vec![key_down(Key::PageDown, Modifiers::NONE)],
            Modifiers::NONE,
        );
        assert_eq!(rows(&win), before, "the screen stays");
        assert!(win.ready(), "a failed read is not a pending one");
        // The next pass says why, and reads again.
        drv.idle(&mut win, 1);
        assert!(says(&drv, HUNG_UP), "{:?}", painted(&drv));
        drv.idle(&mut win, 2);
        assert!(!says(&drv, HUNG_UP), "{:?}", painted(&drv));
        assert_ne!(rows(&win), before, "the key was kept and read again");
        assert_eq!(
            rows(&win)[0],
            format!("line {:06} of a long log", win.visible - 1)
        );
    });
}

/// **V13a.** A file whose syntax only its first line names (no extension) gets that syntax in
/// the pass that shows the first screen — its first block is still Pending when the window is
/// made — and is highlighted by it.
#[test]
fn the_first_line_picks_the_syntax_when_the_first_block_arrives() {
    bounded(
        "the_first_line_picks_the_syntax_when_the_first_block_arrives",
        || {
            let mut body = String::from("#!/bin/sh\necho hello\n");
            for i in 0..200 {
                body.push_str(&format!("echo \"{i}\" # {i}\n"));
            }
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("run");
            assert_eq!(
                win.syntax.map(|s| s.name.as_str()),
                None,
                "nothing read yet"
            );
            let mut drv = Driver::new(800.0, 500.0);
            drv.idle(&mut win, 3);
            assert_eq!(win.syntax.map(|s| s.name.as_str()), None, "still Pending");
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            assert_eq!(
                win.syntax.map(|s| s.name.as_str()),
                Some("Bourne Again Shell (bash)"),
                "in the pass that shows the first screen"
            );
            run(&mut drv, &mut win, &mut net, "highlighting", |w, _| {
                w.ready() && w.worker.is_some()
            });
            assert!(says(&drv, "Bourne Again Shell (bash)"));
            let line = &colors(&win)[1];
            let distinct: std::collections::HashSet<_> = line.iter().collect();
            assert!(distinct.len() >= 2, "highlighted: {line:?}");
        },
    );
}

/// **V13a.** Highlighting still completes after the worker's read has waited on a block that
/// was not there: the worker reads through a reader of its own with the blocking `read_at`,
/// so Pending never reaches it. Nothing is cached when it is asked; it sends nothing — no
/// page, no failure — while its read is out, and the page once the block is in.
#[test]
fn the_highlight_worker_waits_for_a_block_and_then_highlights() {
    bounded(
        "the_highlight_worker_waits_for_a_block_and_then_highlights",
        || {
            let body = wide_rust(1_500);
            let mut net = net(body.as_bytes(), gate());
            let doc = indexed(&net, body.as_bytes());
            let syntax = syntaxes().find_syntax_by_extension("rs").unwrap();
            let hl = Highlight::new(syntax, Color32::WHITE, u64::MAX).unwrap();
            let ctx = egui::Context::default();
            let worker =
                Worker::start(doc.share().unwrap(), hl, ctx, &Pool::new(1)).expect("a slot");
            worker.ask(1, 0, 5);
            // Its read goes out, and is not answered.
            let started = Instant::now();
            while net.out() == 0 {
                assert!(started.elapsed() < HANG, "the worker never asked");
                std::thread::sleep(Duration::from_millis(2));
            }
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(net.reads(), [(0, BLOCK as u32)]);
            assert!(
                matches!(worker.done.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "the worker said something before its block arrived"
            );
            assert!(!worker.exited.load(Ordering::Relaxed), "the worker stopped");
            // The block arrives.
            let page = |net: &mut Net, gen: u64| loop {
                net.reply();
                match worker.done.try_recv() {
                    Ok(Done::Page { gen: got, runs }) if got == gen => break runs,
                    Ok(_) => panic!("the worker stopped instead of highlighting"),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => panic!("the worker is gone"),
                }
                assert!(started.elapsed() < HANG, "no page came");
                std::thread::sleep(Duration::from_millis(2));
            };
            let runs = page(&mut net, 1);
            assert_eq!(runs.len(), 5);
            let text: String = runs[1].iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(text, "fn main() {");
            let distinct: std::collections::HashSet<_> = runs[1].iter().map(|(c, _)| *c).collect();
            assert!(distinct.len() >= 2, "highlighted: {:?}", runs[1]);
            // A page megabytes on: every block on the way is waited for, in turn.
            worker.ask(2, doc.line_count() - 5, 5);
            let runs = page(&mut net, 2);
            let text: String = runs[4].iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(text, "}");
            assert!(net.fetched() >= body.len() as u64, "it read its way there");
        },
    );
}

/// 06 §5 / §7.2: a stream that fails under the highlight worker does not end highlighting for
/// good. The page stays, the status row has the reason, and once a key has brought the stream
/// back the page is highlighted as today's viewer highlights it.
#[test]
fn highlighting_comes_back_after_the_stream_failed_under_the_worker() {
    bounded(
        "highlighting_comes_back_after_the_stream_failed_under_the_worker",
        || {
            let body = wide_rust(2_600);
            let mut net = net(body.as_bytes(), gate());
            let mut win = TextWindow::new(
                indexed(&net, body.as_bytes()),
                "box:/remote/file".into(),
                "main.rs",
                Vec::new(),
            );
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                w.ready()
            });
            net.settle();
            // The last page: its own block arrives, then the line goes down under the worker, which
            // is reading its way there from byte 0.
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                rows(w).last().map(String::as_str) == Some("}")
            });
            *net.down.lock().unwrap() = Some("ssh: connect to host: Connection refused".into());
            net.control.fault(net.requests(), Fault::Lost);
            let shown = rows(&win);
            run(&mut drv, &mut win, &mut net, "the failure", |w, _| {
                matches!(w.doc.source().unwrap().state(), Streamed::Failed(_)) && w.worker.is_none()
            });
            drv.idle(&mut win, 2);
            assert_eq!(rows(&win), shown, "the screen stays");
            // The status row has the Source's reason as it is now. Which one is a race the
            // worker's thread decides: its blocked read asked for a retry, and a reconnect
            // already refused replaces the lost channel's own words.
            let failed = |net: &Net| match net.source.state() {
                Streamed::Failed(reason) => reason,
                other => panic!("{other:?}"),
            };
            let reason = failed(&net);
            assert!(
                reason.contains("the connection closed") || reason.contains("Connection refused"),
                "{reason}"
            );
            assert!(says(&drv, &reason), "{:?}", painted(&drv));
            // Whatever retry the worker's read left asked for is made now, with no input: the
            // attempts counted from here on are the keys' own.
            net.advance(Duration::from_secs(2));
            for _ in 0..3 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            let attempts = net.reopens();
            net.advance(Duration::from_secs(2));
            for _ in 0..3 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            assert_eq!(net.reopens(), attempts, "a pass retried by itself");
            // A key while the line is still down: it retries — its page is cached, so no read
            // of its own asks — once, and the refusal is the reason shown.
            drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
            net.settle();
            assert_eq!(net.reopens(), attempts + 1, "the key's own attempt");
            drv.idle(&mut win, 2);
            assert!(failed(&net).contains("Connection refused"));
            assert!(says(&drv, "Connection refused"), "{:?}", painted(&drv));
            assert!(win.worker.is_none());
            // The line is back. Nothing retries until the next key, which makes a fresh attempt.
            *net.down.lock().unwrap() = None;
            net.advance(Duration::from_secs(2));
            for _ in 0..3 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            assert_eq!(net.reopens(), attempts + 1, "a pass retried by itself");
            assert!(win.stream_down());
            drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
            net.settle();
            assert_eq!(net.reopens(), attempts + 2, "the second key's own attempt");
            run(
                &mut drv,
                &mut win,
                &mut net,
                "highlighting again",
                |w, _| w.worker.is_some() && w.ready(),
            );
            let tmp = tempfile::tempdir().unwrap();
            let mut local = twin(tmp.path(), "main.rs", body.as_bytes());
            let mut local_drv = Driver::new(800.0, 500.0);
            local_drv.until(&mut local, 60, |w| w.ready());
            local_drv.key(&mut local, Key::End, command(false));
            local_drv.key(&mut local, Key::ArrowUp, Modifiers::NONE);
            local_drv.key(&mut local, Key::ArrowUp, Modifiers::NONE);
            local_drv.until(&mut local, 60, |w| w.ready());
            assert_eq!(net.source.state(), Streamed::Live);
            assert_eq!(rows(&win), rows(&local));
            assert_eq!(colors(&win), colors(&local), "highlighted as today");
        },
    );
}

/// **V13a.** The gutter is sized once: its width at `first` is its width once the index is
/// whole, and no row is read or wrapped again in between — the same page, the same layouts,
/// the same bytes read for it — though the line count grows from 4 digits to 5 meanwhile.
#[test]
fn the_gutter_is_sized_once_and_growing_the_index_wraps_nothing_again() {
    bounded(
        "the_gutter_is_sized_once_and_growing_the_index_wraps_nothing_again",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            let x = text_x(&drv, "line 000000");
            let first = rows(&win);
            let page = win.page.as_ref().unwrap().gen;
            let (read, laid) = (win.doc.page_reads(), wrap::laid());
            assert_eq!(win.doc.line_count().to_string().len(), 4, "of one block");
            let mut widths = vec![x];
            run(&mut drv, &mut win, &mut net, "the index", |w, d| {
                widths.push(text_x(d, "line 000000"));
                w.doc.whole() && w.ready()
            });
            assert_eq!(win.doc.line_count(), 12_000);
            assert!(widths.iter().all(|w| *w == x), "the text moved: {widths:?}");
            assert_eq!(rows(&win), first);
            assert_eq!(
                win.page.as_ref().unwrap().gen,
                page,
                "the page was read again"
            );
            assert_eq!(
                win.doc.page_reads(),
                read,
                "bytes were read for the page again"
            );
            assert_eq!(wrap::laid(), laid, "rows were wrapped again");
            // Room for the digits of min(size, 16 MiB): 312,000 bytes, 6 digits — where a local file
            // of 100 000 lines has its text.
            let tmp = tempfile::tempdir().unwrap();
            let wide = numbered(100_000);
            let mut local = twin(tmp.path(), "wide.log", wide.as_bytes());
            let mut local_drv = Driver::new(800.0, 500.0);
            local_drv.until(&mut local, 60, |w| w.ready());
            assert_eq!(x, text_x(&local_drv, "line 000000"));
            // A file that fits the first block uses its line count, as a local file does.
            let small = numbered(50);
            let mut net = net_polling(small.as_bytes(), gate(), NEVER);
            let mut win = net.window("small.log");
            run(&mut drv, &mut win, &mut net, "the small file", |w, _| {
                has_rows(w)
            });
            let mut local = twin(tmp.path(), "small.log", small.as_bytes());
            local_drv.until(&mut local, 60, |w| w.ready());
            assert_eq!(
                text_x(&drv, "line 000000"),
                text_x(&local_drv, "line 000000")
            );
        },
    );
}

/// 06 §7.2: unwrapped, a first line that runs past the first block leaves the rows below it
/// without a known start: each shows a dim `loading…` and fills in as the index pass reaches
/// it. The first screen is the row the block supplies, for one block.
#[test]
fn unwrapped_rows_below_a_first_line_longer_than_the_block_fill_in() {
    bounded(
        "unwrapped_rows_below_a_first_line_longer_than_the_block_fill_in",
        || {
            let mut body = "x".repeat(100_000);
            body.push('\n');
            for i in 0..50 {
                body.push_str(&format!("row {i}\n"));
            }
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("notes.txt");
            win.wrap = false;
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            // The row the block supplies: the line's first bytes, read under a row's bound
            // (06 §7.2: no longer its first 4,096 — the row scrolls to any byte of it).
            let shown = rows(&win);
            assert_eq!(shown.len(), 1);
            assert!(shown[0].len() >= 80 && shown[0].bytes().all(|b| b == b'x'));
            assert!(shown[0].len() <= 4096, "{}", shown[0].len());
            assert_eq!(net.reads(), [(0, BLOCK as u32)]);
            drv.idle(&mut win, 1);
            assert_eq!(status(&drv), "line 1");
            let waiting = painted(&drv).iter().filter(|t| *t == "loading…").count() as u64;
            assert_eq!(waiting, win.visible - 1, "{:?}", painted(&drv));
            assert!(
                win.ready(),
                "the first screen is the rows the block supplies"
            );
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready() && rows(w).len() > 1
            });
            drv.idle(&mut win, 1);
            assert_eq!(rows(&win)[1], "row 0");
            assert_eq!(rows(&win).len() as u64, win.visible);
            assert!(!says(&drv, "loading…"), "{:?}", painted(&drv));
        },
    );
}

/// 06 §7.2: where the keys asked to go is kept past the indexed prefix too. End pressed on the
/// first screen, when one block is indexed, is not the end of that block: the page stays, the
/// status row says `loading…`, and the file's last page comes when the pass has reached it.
#[test]
fn end_pressed_on_the_first_screen_waits_for_the_index_and_goes_to_the_files_end() {
    bounded(
        "end_pressed_on_the_first_screen_waits_for_the_index_and_goes_to_the_files_end",
        || {
            let body = numbered(12_000);
            for wrap in [true, false] {
                let mut net = net(body.as_bytes(), gate());
                let mut win = net.window("big.log");
                win.wrap = wrap;
                let mut drv = Driver::new(800.0, 500.0);
                run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                    has_rows(w)
                });
                let before = rows(&win);
                let end = if wrap {
                    command(false)
                } else {
                    Modifiers::NONE
                };
                drv.frame(&mut win, vec![key_down(Key::End, end)], end);
                drv.idle(&mut win, 1);
                assert_eq!(rows(&win), before, "wrap {wrap}: the page on screen stays");
                assert_eq!(
                    status(&drv),
                    format!("lines 1–{} · loading…", before.len()),
                    "wrap {wrap}"
                );
                assert!(!win.ready(), "wrap {wrap}");
                // Two rows up from there, asked while it waits.
                for _ in 0..2 {
                    drv.frame(
                        &mut win,
                        vec![key_down(Key::ArrowUp, Modifiers::NONE)],
                        Modifiers::NONE,
                    );
                }
                assert_eq!(rows(&win), before, "wrap {wrap}: the page on screen stays");
                run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                    w.ready() && rows(w) != before
                });
                assert!(win.doc.whole());
                assert_eq!(
                    rows(&win).last().map(String::as_str),
                    Some("line 011997 of a long log"),
                    "wrap {wrap}"
                );
                assert_eq!(rows(&win).len() as u64, win.visible, "wrap {wrap}");
            }
        },
    );
}

/// 06 §7.2: the background pass indexes at most the first 16 MiB of a file read through a
/// Source, and nothing is fetched to index the rest; the top bar says how far the lines are
/// known. A local file keeps its whole-file pass, however large.
#[test]
fn the_pass_stops_at_16_mib_for_a_source_and_never_for_a_local_file() {
    bounded(
        "the_pass_stops_at_16_mib_for_a_source_and_never_for_a_local_file",
        || {
            let body = numbered(970_000);
            let total = body.len() as u64;
            assert!(total > HIGHLIGHT_MAX + (8 << 20));
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
                w.doc.size() >= HIGHLIGHT_MAX
            });
            net.settle();
            drv.idle(&mut win, 3);
            net.settle();
            drv.idle(&mut win, 3);
            assert_eq!(win.doc.size(), HIGHLIGHT_MAX);
            assert!(!win.doc.whole());
            assert!(
                says(&drv, "lines known to 16.8 MB of 25.2 MB"),
                "{:?}",
                painted(&drv)
            );
            assert_eq!(net.fetched(), HIGHLIGHT_MAX, "fetched to index 16 MiB");
            let offsets: std::collections::HashSet<u64> = net.reads().iter().map(|r| r.0).collect();
            assert_eq!(
                offsets.len(),
                net.reads().len(),
                "a block was fetched twice"
            );
            // W5b lifted W5a's stop-gap (06 §7.2): End is the file's end, past the lines known —
            // a jump by bytes, for two blocks — and the status row does not call the lines
            // known the file's line count.
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                w.ready() && rows(w).last().is_some_and(|r| r.starts_with("line 969999"))
            });
            drv.idle(&mut win, 1);
            assert!(!status(&drv).contains(" of "), "{}", status(&drv));
            assert!(!status(&drv).contains("loading"), "{}", status(&drv));
            assert!(says(&drv, "bytes 0x"), "{:?}", painted(&drv));
            net.settle();
            assert!(
                net.fetched() <= HIGHLIGHT_MAX + 2 * BLOCK,
                "{}",
                net.fetched()
            );
            // The same bytes as a local file: indexed whole before its window is made.
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("big.log");
            std::fs::write(&path, &body).unwrap();
            let doc = TextDoc::open(&path).unwrap();
            assert_eq!(doc.size(), total);
            assert_eq!(doc.line_count(), 970_000);
            assert!(doc.whole());
            let indexed: u64 = index_reads(&path).iter().map(|r| r.1).sum();
            assert_eq!(indexed, total);
        },
    );
}

/// 06 §7.2: as a preview (02 §4.3) the pass stops at 4 MiB and says so; handed over, it goes
/// on from there — no block is fetched twice.
#[test]
fn a_previews_pass_stops_at_4_mib_and_the_hand_over_goes_on_from_there() {
    bounded(
        "a_previews_pass_stops_at_4_mib_and_the_hand_over_goes_on_from_there",
        || {
            let body = numbered(220_000);
            let total = body.len() as u64;
            assert!(total > (5 << 20));
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            // The budget is the route's to give (02 §4.3): the viewer has none of its own.
            win.preview_index = Some(4 << 20);
            Window::set_preview(&mut win, true);
            let mut drv = Driver::new(800.0, 500.0);
            run(
                &mut drv,
                &mut win,
                &mut net,
                "the preview's pass",
                |w, _| w.doc.size() >= (4 << 20),
            );
            net.settle();
            drv.idle(&mut win, 3);
            net.settle();
            drv.idle(&mut win, 3);
            assert_eq!(win.doc.size(), 4 << 20);
            assert!(
                says(&drv, "preview of the first 4 MiB"),
                "{:?}",
                painted(&drv)
            );
            assert_eq!(net.fetched(), 4 << 20, "fetched for the preview");
            Window::set_preview(&mut win, false);
            run(&mut drv, &mut win, &mut net, "the rest", |w, _| {
                w.doc.whole() && w.ready()
            });
            assert_eq!(win.doc.line_count(), 220_000);
            let offsets: std::collections::HashSet<u64> = net.reads().iter().map(|r| r.0).collect();
            assert_eq!(
                offsets.len(),
                net.reads().len(),
                "a block was fetched twice"
            );
            assert_eq!(net.fetched(), total);
        },
    );
}

/// 06 §5 through the window (W8a): told it is held without being drawn
/// (`Window::set_shown(false)`: a browser preview behind a viewer), the text viewer's stream
/// does not poll — nothing is asked of the remote; shown again, it polls every 2 s.
#[test]
fn a_viewer_held_without_being_drawn_does_not_poll() {
    bounded("a_viewer_held_without_being_drawn_does_not_poll", || {
        let body = numbered(2_000);
        let mut net = net_polling(body.as_bytes(), gate(), Duration::from_secs(2));
        let mut win = net.window("small.log");
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the index", |w, _| {
            w.doc.whole() && w.ready()
        });
        net.settle();
        Window::set_shown(&mut win, false);
        let sent = net.requests();
        net.pass_time(Duration::from_secs(9));
        assert_eq!(net.requests(), sent, "a hidden viewer asks nothing");
        Window::set_shown(&mut win, true);
        net.pass_time(Duration::from_secs(5));
        let asked: Vec<Asked> = net.control.sent()[sent..].iter().map(|s| s.asked).collect();
        assert!(asked.len() >= 4, "it polls again: {asked:?}");
        assert!(
            asked
                .iter()
                .all(|a| matches!(a, Asked::Fstat | Asked::StatPath)),
            "{asked:?}"
        );
    });
}

/// 06 §5 / §7.2: a remote file that changed freezes the page — the screen stays, nothing more
/// is read, the status row offers Cmd/Ctrl+R — and Cmd/Ctrl+R reloads it: indexed again, the
/// new bytes on screen.
#[test]
fn a_changed_remote_file_freezes_the_page_until_cmd_r_reloads_it() {
    bounded(
        "a_changed_remote_file_freezes_the_page_until_cmd_r_reloads_it",
        || {
            let body = numbered(12_000);
            let mut net = net_polling(body.as_bytes(), gate(), Duration::from_secs(2));
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            let before = rows(&win);
            net.control.change(Change::Write {
                off: 0,
                bytes: b"LINE".to_vec(),
                same_second: false,
            });
            net.pass_time(Duration::from_secs(3));
            assert_eq!(net.source.state(), Streamed::Changed);
            drv.idle(&mut win, 2);
            assert!(
                says(&drv, "the file changed on the remote — Cmd/Ctrl+R reloads"),
                "{:?}",
                painted(&drv)
            );
            let sent = net.reads().len();
            drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            drv.key(&mut win, Key::End, command(false));
            net.out();
            assert_eq!(rows(&win), before, "the screen stays");
            assert_eq!(net.reads().len(), sent, "nothing more is read");
            // Cmd/Ctrl+R.
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the reload", |w, _| {
                w.doc.whole() && w.ready() && rows(w) != before
            });
            assert_eq!(net.source.state(), Streamed::Live);
            assert_eq!(rows(&win)[0], "LINE 000000 of a long log");
            assert_eq!(rows(&win)[1..], before[1..]);
            assert_eq!(win.doc.line_count(), 12_000);
            drv.idle(&mut win, 1);
            assert!(!says(&drv, "the file changed"), "{:?}", painted(&drv));
            // A reload from the end of the file comes back to the place it was at, wrapped or not.
            for wrap in [true, false] {
                if !wrap {
                    drv.key(&mut win, Key::W, Modifiers::NONE);
                }
                let end = if wrap {
                    command(false)
                } else {
                    Modifiers::NONE
                };
                drv.key(&mut win, Key::End, end);
                run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                    w.ready()
                });
                let at_end = rows(&win);
                assert_eq!(at_end.last().unwrap(), "line 011999 of a long log");
                let x = text_x(&drv, "line 011999");
                drv.key(&mut win, Key::R, command(false));
                assert!(!win.ready(), "wrap {wrap}: reloading");
                let mut xs = Vec::new();
                run(&mut drv, &mut win, &mut net, "the reload", |w, d| {
                    xs.push(text_x(d, "line 0119"));
                    w.doc.whole() && w.ready()
                });
                assert_eq!(rows(&win), at_end, "wrap {wrap}");
                assert!(xs.iter().all(|v| *v == x), "wrap {wrap}: the text moved");
            }
        },
    );
}

/// CRLF lines through a stream: no row shows a `\r`, at the end of a line hundreds of
/// kilobytes long as at the end of a short one. (06 §7.2's
/// rule that a line's last byte is not read before its row is on screen: through the growing
/// index every byte the index names is one the pass has fetched, so that read costs no round
/// trip; the first screen's one block is `the_first_screen_costs_one_block_whatever_the_lines`.)
#[test]
fn crlf_lines_show_no_cr_at_the_end_of_a_long_line_or_a_short_one() {
    bounded(
        "crlf_lines_show_no_cr_at_the_end_of_a_long_line_or_a_short_one",
        || {
            let mut body = "y".repeat(300_000);
            body.push_str("\r\n");
            for i in 0..100 {
                body.push_str(&format!("row {i}\r\n"));
            }
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("notes.txt");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            assert_eq!(net.reads(), [(0, BLOCK as u32)], "only the first block");
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            // End: the line's last row at the bottom; one row down: the line after it.
            drv.key(&mut win, Key::End, Modifiers::NONE);
            drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
            let shown = rows(&win);
            assert_eq!(shown.len() as u64, win.visible);
            assert_eq!(shown.last().unwrap(), "row 0");
            // The line's last row: the last of its y's, and nothing after them.
            let before = &shown[shown.len() - 2];
            assert!(!before.is_empty() && before.len() <= shown[0].len());
            assert_eq!(*before, "y".repeat(before.len()));
            // The short lines after it.
            drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            let shown = rows(&win);
            let from: usize = shown[0].strip_prefix("row ").unwrap().parse().unwrap();
            let want: Vec<String> = (from..from + shown.len())
                .map(|i| format!("row {i}"))
                .collect();
            assert_eq!(shown, want);
        },
    );
}

/// 06 §7.2: highlighting runs over the indexed lines as they grow, and a line over
/// `LINE_MAX` bytes met after the first block stops it there, as in today's viewer: the pages
/// before it are highlighted, the ones after it plain, and the status row says where.
#[test]
fn a_long_line_met_after_the_first_block_stops_highlighting_there() {
    bounded(
        "a_long_line_met_after_the_first_block_stops_highlighting_there",
        || {
            let mut body = rust(3_000);
            assert!(body.len() as u64 > BLOCK);
            body.push_str(&format!("// {}\n", "z".repeat(LINE_MAX + 100)));
            body.push_str(&rust(2_000));
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("main.rs");
            let mut drv = Driver::new(800.0, 500.0);
            let tmp = tempfile::tempdir().unwrap();
            let mut local = twin(tmp.path(), "main.rs", body.as_bytes());
            let mut local_drv = Driver::new(800.0, 500.0);
            local_drv.until(&mut local, 60, |w| w.ready());
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            drv.idle(&mut win, 1);
            assert!(
                says(&drv, "highlighting stops at line 3004"),
                "{:?}",
                painted(&drv)
            );
            assert_eq!(colors(&win), colors(&local), "the first page");
            for (key, modifiers) in [
                (Key::End, command(false)),
                (Key::PageUp, Modifiers::NONE),
                (Key::Home, command(false)),
                (Key::PageDown, Modifiers::NONE),
            ] {
                drv.key(&mut win, key, modifiers);
                run(&mut drv, &mut win, &mut net, "the page", |w, _| w.ready());
                local_drv.key(&mut local, key, modifiers);
                local_drv.until(&mut local, 60, |w| w.ready());
                assert_eq!(rows(&win), rows(&local), "{key:?}");
                assert_eq!(colors(&win), colors(&local), "{key:?}");
            }
        },
    );
}

/// 06 §7.2: a viewer opened at a byte (`SOPEN_STATE` `at=`) the index has not reached says
/// `loading…` until the pass gets there, and then shows that place — not the end of what
/// happened to be indexed.
#[test]
fn a_place_past_the_indexed_lines_waits_for_the_pass() {
    bounded("a_place_past_the_indexed_lines_waits_for_the_pass", || {
        let body = numbered(12_000);
        for wrap in [true, false] {
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            win.wrap = wrap;
            win.jump = Some(Jump::Byte(26 * 9_000 + 3));
            let mut drv = Driver::new(800.0, 500.0);
            let mut early = Vec::new();
            run(&mut drv, &mut win, &mut net, "the place", |w, d| {
                if !has_rows(w) {
                    early.push((status(d), w.ready()));
                }
                has_rows(w)
            });
            assert!(early.len() > 3, "wrap {wrap}");
            assert!(
                early
                    .iter()
                    .all(|(said, ready)| said == "loading…" && !ready),
                "wrap {wrap}: {early:?}"
            );
            assert_eq!(rows(&win)[0], "line 009000 of a long log", "wrap {wrap}");
        }
    });
}

/// 06 §5: bytes that are not what was indexed (a same-second rewrite SFTP cannot see) do not
/// freeze the viewer for good. A valid page is on screen; the file's end is then written over
/// in place, unseen; End reads there and finds no lines. The page stays, the status row says so
/// and offers Cmd/Ctrl+R, and the reload shows what the file holds now.
#[test]
fn bytes_that_are_not_what_was_indexed_are_said_and_a_reload_reads_them() {
    bounded(
        "bytes_that_are_not_what_was_indexed_are_said_and_a_reload_reads_them",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = TextWindow::new(
                indexed(&net, body.as_bytes()),
                "box:/remote/file".into(),
                "big.log",
                Vec::new(),
            );
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                w.ready()
            });
            net.settle();
            drv.idle(&mut win, 1);
            let before = rows(&win);
            let visible = win.visible as usize;
            assert_eq!(before, lines_of(&body)[..visible], "a valid page first");
            // The last 300 lines become one run of `q` with no line end, within the same second and
            // the same size: no fence and no poll can tell.
            let gone = 26 * 300;
            net.control.change(Change::Write {
                off: (body.len() - gone) as u64,
                bytes: vec![b'q'; gone],
                same_second: true,
            });
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the stale read", |w, _| {
                w.failure.is_some()
            });
            for _ in 0..4 {
                drv.idle(&mut win, 1);
                net.settle();
                assert_eq!(rows(&win), before, "the page stays through the failure");
            }
            assert_eq!(net.source.state(), Streamed::Live, "the Source saw nothing");
            assert!(
                says(
                    &drv,
                    "the file changed since it was opened — Cmd/Ctrl+R reloads"
                ),
                "{:?}",
                painted(&drv)
            );
            assert!(
                !win.waiting && win.reindex.is_none(),
                "no re-read from a path"
            );
            // The reload: indexed again, and the End that was asked for shows the replacement.
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the reload", |w, _| {
                w.doc.whole() && w.ready() && rows(w) != before
            });
            drv.idle(&mut win, 1);
            assert_eq!(win.doc.line_count(), 12_000 - 300 + 1);
            let shown = rows(&win);
            assert_eq!(shown.len(), visible);
            assert!(
                shown
                    .iter()
                    .all(|r| !r.is_empty() && *r == "q".repeat(r.len())),
                "{shown:?}"
            );
            assert!(!says(&drv, "the file changed"), "{:?}", painted(&drv));
            drv.key(&mut win, Key::Home, command(false));
            assert_eq!(rows(&win), before, "the lines before it are as they were");
        },
    );
}

/// A viewer that goes while its highlighter waits for a block does not leave the worker
/// waiting: its read ends, and its slot in the pool is free again.
#[test]
fn a_worker_waiting_for_a_block_ends_with_its_viewer() {
    bounded("a_worker_waiting_for_a_block_ends_with_its_viewer", || {
        let body = wide_rust(1_500);
        let mut net = net(body.as_bytes(), gate());
        let doc = indexed(&net, body.as_bytes());
        let syntax = syntaxes().find_syntax_by_extension("rs").unwrap();
        let hl = Highlight::new(syntax, Color32::WHITE, u64::MAX).unwrap();
        let pool = Pool::new(1);
        let ctx = egui::Context::default();
        let worker = Worker::start(doc.share().unwrap(), hl, ctx, &pool).expect("a slot");
        worker.ask(1, doc.line_count() - 5, 5);
        let started = Instant::now();
        while net.out() == 0 {
            assert!(started.elapsed() < HANG, "the worker never asked");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(pool.live(), 1);
        let exited = worker.exited.clone();
        drop(worker);
        // No reply is taken: the wait itself ends.
        while !exited.load(Ordering::Relaxed) || pool.live() > 0 {
            assert!(started.elapsed() < HANG, "the worker is still waiting");
            std::thread::sleep(Duration::from_millis(2));
        }
    });
}

/// 06 §5 / §11: a first block that fails is not `loading…` for ever. The window is ready (the
/// screen's bytes have arrived or failed), the status row has the reason and no bare
/// `loading…`, no pass retries by itself, and the next key brings the first screen.
#[test]
fn a_first_block_that_fails_says_why_and_a_key_brings_the_first_screen() {
    bounded(
        "a_first_block_that_fails_says_why_and_a_key_brings_the_first_screen",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            net.control.fault(0, Fault::Status("Permission denied"));
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the failure", |w, _| {
                w.stream_down()
            });
            drv.idle(&mut win, 2);
            assert!(win.ready(), "ready: its bytes failed");
            assert!(!has_rows(&win));
            assert!(says(&drv, "Permission denied"), "{:?}", painted(&drv));
            assert!(!says(&drv, "loading…"), "{:?}", painted(&drv));
            assert!(!says(&drv, "empty"), "{:?}", painted(&drv));
            // Passes without input ask for nothing.
            let sent = net.requests();
            net.advance(Duration::from_secs(5));
            for _ in 0..5 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            assert_eq!(net.requests(), sent, "a pass retried by itself");
            // A key.
            drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w) && w.ready()
            });
            drv.idle(&mut win, 1);
            assert!(rows(&win)[0].starts_with("line 00000"));
            assert!(!says(&drv, "Permission denied"), "{:?}", painted(&drv));
        },
    );
}

/// 06 §5: a Cmd/Ctrl+R that cannot re-open the file does not freeze the viewer: the page
/// stays, the status row has the reason, and a later Cmd/Ctrl+R reloads.
#[test]
fn a_reload_that_fails_keeps_the_page_and_a_later_one_works() {
    bounded(
        "a_reload_that_fails_keeps_the_page_and_a_later_one_works",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            let before = rows(&win);
            net.control.change(Change::Remove);
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the failed reload", |w, _| {
                w.stream_down() && !w.reloading
            });
            drv.idle(&mut win, 2);
            assert_eq!(rows(&win), before, "the page stays");
            assert!(
                says(&drv, "no such file or directory"),
                "{:?}",
                painted(&drv)
            );
            assert!(win.ready(), "not waiting for a reload any more");
            // The file is back, with another first line.
            let mut now = body.clone();
            now.replace_range(0..4, "LINE");
            net.control
                .change(Change::Replace(FakeFile::bytes(now.into_bytes())));
            net.advance(Duration::from_secs(2));
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the reload", |w, _| {
                w.doc.whole() && w.ready() && !w.stream_down()
            });
            drv.idle(&mut win, 1);
            assert_eq!(net.source.state(), Streamed::Live);
            assert_eq!(rows(&win), before, "the place it was at");
            drv.key(&mut win, Key::Home, command(false));
            assert_eq!(rows(&win)[0], "LINE 000000 of a long log");
        },
    );
}

/// A stream's highlighter that stops for a reason of its own — the parser panicked — is not
/// started again: the page is plain and says why, as for a local file, however many passes
/// follow. (Only a stream's read giving no bytes starts it again.)
#[test]
fn a_streams_highlighter_that_panics_is_not_started_again() {
    bounded(
        "a_streams_highlighter_that_panics_is_not_started_again",
        || {
            let mut body = String::from("// PANIC\n");
            body.push_str(&rust(50));
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("panic.rs");
            let pool = Pool::new(8);
            win.pool = pool.clone();
            let starts = Arc::new(AtomicUsize::new(0));
            let count = starts.clone();
            win.hook = Some(Arc::new(move |line: &str| {
                if line.contains("PANIC") {
                    count.fetch_add(1, Ordering::SeqCst);
                    panic!("the parser broke");
                }
            }));
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the panic", |_, d| {
                says(d, "highlighting stopped")
            });
            for _ in 0..30 {
                drv.idle(&mut win, 1);
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(starts.load(Ordering::SeqCst), 1, "started again");
            assert!(win.worker.is_none() && win.ready());
            // What is painted: why, and the file's own lines, each in the plain colour alone.
            assert!(
                says(
                    &drv,
                    "highlighting stopped (the highlighter panicked); shown plain"
                ),
                "{:?}",
                painted(&drv)
            );
            let lines = lines_of(&body);
            let shown = rows(&win);
            assert_eq!(shown, lines[..win.visible as usize]);
            let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
            for line in &shown {
                let colors = drv.colors_of(line).expect("the row is painted");
                assert!(
                    !colors.is_empty() && colors.iter().all(|c| *c == plain),
                    "{line:?}: {colors:?}"
                );
            }
            let started = Instant::now();
            while pool.live() > 0 {
                assert!(started.elapsed() < HANG, "the worker's slot is still taken");
                std::thread::sleep(Duration::from_millis(2));
            }
        },
    );
}

/// The window's clock, moved by hand.
fn on_clock(win: &mut TextWindow) -> Arc<Mutex<Instant>> {
    let now = Arc::new(Mutex::new(Instant::now()));
    let read = now.clone();
    win.clock = Box::new(move || *read.lock().unwrap());
    now
}

/// 06 §5 / §7.2: a page read that has been Pending for 10 s says `the remote is slow` beside
/// `loading…` — before the first block and under a page on screen — and no sooner; a pass is
/// asked for when the 10 s are up, and the notice goes when the bytes arrive.
#[test]
fn a_read_pending_for_ten_seconds_says_the_remote_is_slow() {
    bounded(
        "a_read_pending_for_ten_seconds_says_the_remote_is_slow",
        || {
            let body = numbered(40_000);
            // Before the first block.
            let mut net1 = net(body.as_bytes(), gate());
            let mut win = net1.window("big.log");
            let clock = on_clock(&mut win);
            let mut drv = Driver::new(800.0, 500.0);
            drv.idle(&mut win, 4);
            assert_eq!(status(&drv), "loading…");
            for k in 0..4 {
                quiet_pass(&drv, &mut win, 50.0 + f64::from(k));
            }
            let next = quiet_pass(&drv, &mut win, 55.0);
            assert!(
                next <= Duration::from_secs(10) && next > Duration::from_secs(9),
                "a pass is scheduled for when the 10 s are up: {next:?}"
            );
            *clock.lock().unwrap() += Duration::from_millis(9_900);
            drv.idle(&mut win, 2);
            assert_eq!(status(&drv), "loading…", "not before 10 s");
            *clock.lock().unwrap() += Duration::from_millis(200);
            drv.idle(&mut win, 2);
            assert_eq!(status(&drv), "loading… · the remote is slow");
            assert!(!says(&drv, "0x"), "the 1 s form is the hex window's");
            quiet_pass(&drv, &mut win, 60.0);
            let next = quiet_pass(&drv, &mut win, 61.0);
            assert!(
                next > Duration::from_secs(10),
                "nothing more to say: {next:?}"
            );
            run(&mut drv, &mut win, &mut net1, "the first screen", |w, _| {
                has_rows(w)
            });
            drv.idle(&mut win, 1);
            assert!(!says(&drv, "the remote is slow"), "{:?}", painted(&drv));
            // Under a page on screen.
            let mut net2 = net(body.as_bytes(), gate());
            let mut win = TextWindow::new(
                indexed(&net2, body.as_bytes()),
                "box:/remote/file".into(),
                "big.log",
                Vec::new(),
            );
            let clock = on_clock(&mut win);
            run(&mut drv, &mut win, &mut net2, "the first screen", |w, _| {
                has_rows(w)
            });
            net2.settle();
            drv.idle(&mut win, 2);
            let said = status(&drv);
            drv.key(&mut win, Key::End, command(false));
            assert_eq!(status(&drv), format!("{said} · loading…"));
            *clock.lock().unwrap() += Duration::from_secs(11);
            drv.idle(&mut win, 2);
            assert_eq!(
                status(&drv),
                format!("{said} · loading… · the remote is slow")
            );
            // The wait starts again with each read that is on its way.
            run(&mut drv, &mut win, &mut net2, "the last page", |w, _| {
                w.ready()
            });
            net2.settle();
            drv.key(&mut win, Key::Home, command(false));
            drv.idle(&mut win, 1);
            assert!(!says(&drv, "the remote is slow"), "{:?}", painted(&drv));
        },
    );
}

/// 06 §7.2: the index pass starts only once the first screen is up — also when the first
/// block has arrived and the first page's own read is still Pending, or fails: nothing is
/// asked for beyond block 0 until its rows are drawn.
#[test]
fn the_pass_waits_for_a_first_page_that_is_pending_or_failed_after_block_0() {
    bounded(
        "the_pass_waits_for_a_first_page_that_is_pending_or_failed_after_block_0",
        || {
            let body = numbered(12_000);
            for (pending, failing) in [(8, 0), (0, 6), (4, 4)] {
                let at = format!("{pending} Pending, {failing} failed");
                let mut net = net(body.as_bytes(), gate());
                let mut win = net.window("big.log");
                let held = gated(&mut win);
                let mut drv = Driver::new(800.0, 500.0);
                // Block 0 is asked for, and arrives with no pass in between.
                drv.idle(&mut win, 1);
                net.settle();
                // The pass that indexes it: its own read goes through, the page's reads do not.
                *held.lock().unwrap() = Held {
                    skip: 1,
                    pending,
                    failing,
                };
                let mut passes = 0;
                loop {
                    drv.idle(&mut win, 1);
                    net.out();
                    let left = {
                        let held = held.lock().unwrap();
                        held.pending + held.failing
                    };
                    if has_rows(&win) {
                        assert_eq!(left, 0, "{at}: a page from reads that were held");
                        break;
                    }
                    passes += 1;
                    assert_eq!(win.doc.size(), BLOCK, "{at}, pass {passes}");
                    assert_eq!(
                        net.reads(),
                        [(0, BLOCK as u32)],
                        "{at}, pass {passes}: the pass ran on before the first screen"
                    );
                    if failing > 0 && left == 0 {
                        assert!(win.failure.is_some(), "{at}");
                    }
                    assert!(passes < 50, "{at}: no first screen");
                }
                assert!(passes >= 2, "{at}: the first page waited {passes} passes");
                // The first screen, and only then the pass.
                assert_eq!(net.reads(), [(0, BLOCK as u32)], "{at}: at `first`");
                run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                    w.doc.whole() && w.ready()
                });
                assert_eq!(win.doc.line_count(), 12_000, "{at}");
            }
        },
    );
}

/// 06 §7.2 (X3's defect 1), at the read window itself: looked at while the reads are still
/// held — a Pending one, then a failed one — it has the bytes and the place it had. Each held
/// outcome was the read's own: exactly one is used up by each attempt.
#[test]
fn the_read_window_is_as_it_was_while_a_read_is_pending_or_has_failed() {
    bounded(
        "the_read_window_is_as_it_was_while_a_read_is_pending_or_has_failed",
        || {
            let body: Vec<u8> = (0..40_000u32).map(|i| b'a' + (i % 26) as u8).collect();
            let mut net = net(&body, gate());
            let mut doc = indexed(&net, &body);
            let held = gated_doc(&mut doc);
            let mut window = Window_::default();
            let started = Instant::now();
            loop {
                match window.at(&mut doc, 0, 10) {
                    Ok(got) => {
                        assert_eq!(got, &body[..got.len()]);
                        break;
                    }
                    Err(e) => assert!(is_pending(&e), "{e:#}"),
                }
                net.settle();
                assert!(started.elapsed() < HANG);
            }
            let had = window.bytes.clone();
            assert!(!had.is_empty());
            for (pending, failing) in [(2, 0), (0, 2)] {
                *held.lock().unwrap() = Held {
                    skip: 0,
                    pending,
                    failing,
                };
                let e = window.at(&mut doc, 30_000, 100).unwrap_err();
                assert_eq!(is_pending(&e), pending > 0, "{e:#}");
                {
                    let left = held.lock().unwrap();
                    assert_eq!(left.pending + left.failing, 1, "one held read was made");
                }
                // Reads are still held: this is the window, not a refill.
                assert_eq!(window.start, 0);
                assert_eq!(window.bytes, had, "the window was not left as it was");
                assert_eq!(window.bytes, &body[..had.len()]);
            }
            *held.lock().unwrap() = Held::default();
            assert_eq!(
                window.at(&mut doc, 30_000, 100).unwrap(),
                &body[30_000..30_100]
            );
        },
    );
}

/// 06 §7.2: one pass of the index never feeds more than its budget and one block — also at
/// the cap, where the last stretch is read in one piece: that piece waits for a pass of its
/// own when it does not fit in what is left of this one.
#[test]
fn one_pass_of_the_index_never_feeds_more_than_its_budget_and_a_block() {
    bounded(
        "one_pass_of_the_index_never_feeds_more_than_its_budget_and_a_block",
        || {
            let body = numbered(970_000);
            let mut net = net(body.as_bytes(), gate());
            // Everything below the cap is here already: a pass is held back by nothing but its budget.
            let mut all = net.source.reader(ReaderKind::Bulk);
            let mut off = 0u64;
            while off < HIGHLIGHT_MAX {
                match all.try_read(off, 1 << 20) {
                    Poll::Ready(got) => off += got.expect("a read").len() as u64,
                    Poll::Pending => net.settle(),
                }
            }
            let mut doc = TextDoc::streamed(net.source.clone());
            let budget = PASS_READ as u64;
            let mut fed = Vec::new();
            let started = Instant::now();
            loop {
                let before = doc.size();
                match doc.grow(HIGHLIGHT_MAX, budget) {
                    Ok(Some(_)) => fed.push(doc.size() - before),
                    Ok(None) => break,
                    Err(e) if is_pending(&e) => net.settle(),
                    Err(e) => panic!("{e:#}"),
                }
                assert!(started.elapsed() < HANG);
            }
            assert_eq!(doc.size(), HIGHLIGHT_MAX);
            assert_eq!(fed[0], BLOCK, "the first block alone");
            let most = fed.iter().max().unwrap();
            assert!(*most <= budget + BLOCK, "a pass fed {most} bytes: {fed:?}");
            assert!(fed.len() <= 20, "{} passes for 16 MiB", fed.len());
        },
    );
}

/// **V13a**, before the first page: keys pressed while block 0 is on its way are kept and
/// applied when it arrives — Down ×3 and PgDn open the page that far down, unwrapped as
/// wrapped.
#[test]
fn keys_pressed_before_the_first_block_are_applied_when_it_arrives() {
    bounded(
        "keys_pressed_before_the_first_block_are_applied_when_it_arrives",
        || {
            let body = numbered(12_000);
            for wrap in [false, true] {
                let mut net = net(body.as_bytes(), gate());
                let mut win = net.window("big.log");
                win.wrap = wrap;
                let mut drv = Driver::new(800.0, 500.0);
                drv.idle(&mut win, 2);
                assert!(net.out() > 0);
                for key in [
                    Key::ArrowDown,
                    Key::ArrowDown,
                    Key::ArrowDown,
                    Key::PageDown,
                ] {
                    drv.key(&mut win, key, Modifiers::NONE);
                    assert!(!has_rows(&win), "wrap {wrap}");
                    assert_eq!(status(&drv), "loading…", "wrap {wrap}");
                }
                let moved = 3 + win.visible as usize - 1;
                run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                    has_rows(w)
                });
                let visible = win.visible as usize;
                assert_eq!(
                    rows(&win),
                    lines_of(&body)[moved..moved + visible],
                    "wrap {wrap}: the page the keys asked for"
                );
                drv.idle(&mut win, 1);
                assert_eq!(
                    status(&drv),
                    format!("lines {}–{}", moved + 1, moved + visible),
                    "wrap {wrap}"
                );
            }
        },
    );
}

/// 06 §7.2: growing the index reads the page again only when a line on screen got longer or
/// the page was short of the window. A full page whose last line is the last one indexed —
/// and ends with its newline, at the block's edge — is not read again as the index grows;
/// one whose last line is cut by the block's edge is, once, and then shows the whole line.
#[test]
fn a_full_page_at_the_indexed_end_is_read_again_only_if_its_last_line_grew() {
    bounded(
        "a_full_page_at_the_indexed_end_is_read_again_only_if_its_last_line_grew",
        || {
            for (ended, line) in [(true, 64usize), (false, 80)] {
                // Lines of `line` bytes: block 0 ends on a newline (64 divides it) or inside a line.
                let count = 3_000u32;
                let body: String = (0..count)
                    .map(|i| format!("{i:06}{}\n", "-".repeat(line - 7)))
                    .collect();
                assert_eq!((BLOCK as usize).is_multiple_of(line), ended);
                let lines = lines_of(&body);
                let in_block = (BLOCK as usize).div_ceil(line);
                for wrap in [false, true] {
                    let at = format!("ended {ended}, wrap {wrap}");
                    let mut net = net(body.as_bytes(), gate());
                    let mut win = net.window("notes.txt");
                    win.wrap = wrap;
                    // The page that ends with the last line block 0 has.
                    let rows_fit = 26;
                    win.jump = Some(Jump::Byte(((in_block - rows_fit) * line) as u64));
                    let mut drv = Driver::new(800.0, 500.0);
                    run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                        has_rows(w)
                    });
                    assert_eq!(win.visible as usize, rows_fit, "{at}");
                    assert_eq!(win.doc.size(), BLOCK, "{at}");
                    let first = rows(&win);
                    assert_eq!(first.len(), rows_fit, "{at}: a full page");
                    let cut = &lines[in_block - 1][..if ended {
                        line - 1
                    } else {
                        BLOCK as usize % line
                    }];
                    assert_eq!(first.last().unwrap(), cut, "{at}: the last indexed line");
                    let read = win.page.as_ref().unwrap().gen;
                    run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                        w.doc.whole() && w.ready()
                    });
                    assert_eq!(rows(&win), lines[in_block - rows_fit..in_block], "{at}");
                    let again = win.page.as_ref().unwrap().gen - read;
                    assert_eq!(again, u64::from(!ended), "{at}: read again {again} times");
                }
            }
        },
    );
}

/// A viewer that goes while its own highlighter waits for a block that gets no answer does
/// not leave it waiting: dropping the window ends the worker's read, and its slot in the pool
/// is free again — with no reply taken.
#[test]
fn dropping_the_viewer_ends_its_highlighter_waiting_for_a_block() {
    bounded(
        "dropping_the_viewer_ends_its_highlighter_waiting_for_a_block",
        || {
            let body = wide_rust(6_000);
            assert!(body.len() as u64 > 6 * BLOCK);
            let mut net = net(body.as_bytes(), gate());
            let mut win = TextWindow::new(
                indexed(&net, body.as_bytes()),
                "box:/remote/file".into(),
                "main.rs",
                Vec::new(),
            );
            let pool = Pool::new(1);
            win.pool = pool.clone();
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                w.ready() && w.worker.is_some()
            });
            net.settle();
            // The last page: its own block arrives; the highlighter, reading its way there from byte
            // 0, asks for the blocks between — and gets no more answers.
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the last page", |w, _| {
                rows(w).last().map(String::as_str) == Some("}")
            });
            let worker = win.worker.as_ref().expect("the highlighter");
            let exited = worker.exited.clone();
            // The handshake: only the highlighter reads with the blocking `read_at`, and the
            // Source counts each time such a read's wait runs out with its block still missing.
            // Two more of those, with waits of 10 ms: a read of its is blocked right now.
            net.source.probe().wait = Some(Duration::from_millis(10));
            let waits = net.source.probe().timed_out;
            let started = Instant::now();
            while net.source.probe().timed_out < waits + 2 {
                assert!(
                    started.elapsed() < HANG,
                    "the highlighter never reached a blocking read"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(net.out() > 0, "its block is asked for, and not answered");
            assert!(!exited.load(Ordering::Relaxed), "it stopped by itself");
            assert_eq!(pool.live(), 1);
            assert!(!win.ready(), "the page is not highlighted yet");
            drop(win);
            while !exited.load(Ordering::Relaxed) || pool.live() > 0 {
                assert!(started.elapsed() < HANG, "the worker is still waiting");
                std::thread::sleep(Duration::from_millis(2));
            }
        },
    );
}

/// 06 §5: a reload from deep inside a long wrapped line comes back to the same rows — the
/// row the top was on, not the line's start.
#[test]
fn a_reload_inside_a_long_wrapped_line_comes_back_to_its_rows() {
    bounded(
        "a_reload_inside_a_long_wrapped_line_comes_back_to_its_rows",
        || {
            let line: String = (0..40_000).map(|i| format!("{i:07}_")).collect();
            let body = format!("head\n{line}\ntail\n");
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("notes.txt");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            for _ in 0..4 {
                drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            }
            let before = rows(&win);
            let (top, off) = (win.line, win.top_off as usize);
            assert_eq!(top, Some(1), "inside the long line");
            assert!(off > 1_000, "at byte {off} of it");
            // The rows are the line's own bytes from there.
            let all = before.concat();
            assert_eq!(all, line[off..off + all.len()]);
            assert_eq!(before.len() as u64, win.visible);
            drv.key(&mut win, Key::R, command(false));
            assert!(!win.ready(), "reloading");
            run(&mut drv, &mut win, &mut net, "the reload", |w, _| {
                w.doc.whole() && w.ready()
            });
            assert_eq!((win.line, win.top_off as usize), (top, off));
            assert_eq!(rows(&win), before);
        },
    );
}

/// 06 §5: after a reload that could not re-open the file, a plain key — not Cmd/Ctrl+R —
/// retries the failed stream, and what the path holds now is shown.
#[test]
fn a_plain_key_retries_after_a_failed_reload_and_shows_the_replacement() {
    bounded(
        "a_plain_key_retries_after_a_failed_reload_and_shows_the_replacement",
        || {
            let body = numbered(12_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the index", |w, _| {
                w.doc.whole() && w.ready()
            });
            let before = rows(&win);
            net.control.change(Change::Remove);
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the failed reload", |w, _| {
                w.stream_down() && !w.reloading
            });
            drv.idle(&mut win, 2);
            assert_eq!(rows(&win), before, "the page stays");
            assert!(
                says(&drv, "no such file or directory"),
                "{:?}",
                painted(&drv)
            );
            // The file is back, with another first line. No pass retries by itself.
            let mut now = body.clone();
            now.replace_range(0..4, "LINE");
            net.control
                .change(Change::Replace(FakeFile::bytes(now.clone().into_bytes())));
            net.advance(Duration::from_secs(2));
            for _ in 0..3 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            assert!(win.stream_down(), "a pass retried by itself");
            assert_eq!(rows(&win), before);
            // A plain key.
            drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "the retry", |w, _| {
                w.doc.whole() && w.ready() && !w.stream_down() && rows(w) != before
            });
            drv.idle(&mut win, 1);
            assert_eq!(net.source.state(), Streamed::Live);
            let visible = win.visible as usize;
            assert_eq!(
                rows(&win),
                lines_of(&now)[1..=visible],
                "the replacement, a row down"
            );
            assert!(
                !says(&drv, "no such file or directory"),
                "{:?}",
                painted(&drv)
            );
        },
    );
}

/// 06 §5: Cmd/Ctrl+R on a stream that has already failed is one reload, not a reload and a
/// retry: when that reload fails, no reconnect attempt follows it until the next key.
#[test]
fn a_reload_of_a_failed_stream_that_fails_is_not_followed_by_a_retry() {
    bounded(
        "a_reload_of_a_failed_stream_that_fails_is_not_followed_by_a_retry",
        || {
            let body = numbered(40_000);
            let mut net = net(body.as_bytes(), gate());
            let mut win = TextWindow::new(
                indexed(&net, body.as_bytes()),
                "box:/remote/file".into(),
                "big.log",
                Vec::new(),
            );
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                w.ready()
            });
            net.settle();
            let before = rows(&win);
            // The line goes down under a read of the file's end.
            *net.down.lock().unwrap() = Some("ssh: connect to host: Connection refused".into());
            net.control.fault(net.requests(), Fault::Lost);
            drv.key(&mut win, Key::End, command(false));
            run(&mut drv, &mut win, &mut net, "the failure", |w, _| {
                w.stream_down()
            });
            // What the failed read itself asked for is made, and refused.
            net.advance(Duration::from_secs(2));
            for _ in 0..3 {
                drv.idle(&mut win, 1);
                net.settle();
            }
            assert!(win.stream_down());
            let attempts = net.reopens();
            // Cmd/Ctrl+R: one attempt, refused.
            net.advance(Duration::from_secs(2));
            drv.key(&mut win, Key::R, command(false));
            run(&mut drv, &mut win, &mut net, "the failed reload", |w, _| {
                w.stream_down() && !w.reloading
            });
            let after_reload = net.reopens();
            assert!(
                after_reload <= attempts + 1,
                "{attempts} then {after_reload}"
            );
            // Time passes, passes run: nothing more is attempted.
            for _ in 0..3 {
                net.advance(Duration::from_secs(2));
                for _ in 0..3 {
                    drv.idle(&mut win, 1);
                    net.settle();
                }
            }
            assert_eq!(
                net.reopens(),
                after_reload,
                "a reconnect followed the failed reload with no key"
            );
            assert_eq!(rows(&win), before, "the page stays");
            // The next key retries.
            drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
            net.settle();
            assert_eq!(net.reopens(), after_reload + 1, "the key's own attempt");
        },
    );
}

/// 06 §5: one gesture is one operation, whenever the stream's thread ends it. A Cmd/Ctrl+R
/// whose reload has already failed by the time the same pass looks at the stream — between
/// its keys and its poll — is still not followed by a retry: no reconnect attempt after the
/// reload's own until the next key.
#[test]
fn a_reload_that_fails_within_its_own_pass_is_not_followed_by_a_retry() {
    bounded(
        "a_reload_that_fails_within_its_own_pass_is_not_followed_by_a_retry",
        || {
            use std::cell::RefCell;
            use std::rc::Rc;
            let body = numbered(40_000);
            for was_down in [true, false] {
                let at = format!("down before the reload: {was_down}");
                let net = Rc::new(RefCell::new(net(body.as_bytes(), gate())));
                let mut win = TextWindow::new(
                    indexed(&net.borrow(), body.as_bytes()),
                    "box:/remote/file".into(),
                    "big.log",
                    Vec::new(),
                );
                let mut drv = Driver::new(800.0, 500.0);
                run(
                    &mut drv,
                    &mut win,
                    &mut net.borrow_mut(),
                    "the first screen",
                    |w, _| w.ready(),
                );
                net.borrow_mut().settle();
                let before = rows(&win);
                *net.borrow().down.lock().unwrap() =
                    Some("ssh: connect to host: Connection refused".into());
                if was_down {
                    // The line goes down under a read of the file's end, and what that read
                    // asked for is made, and refused.
                    let next = net.borrow().requests();
                    net.borrow().control.fault(next, Fault::Lost);
                    drv.key(&mut win, Key::End, command(false));
                    run(
                        &mut drv,
                        &mut win,
                        &mut net.borrow_mut(),
                        "the failure",
                        |w, _| w.stream_down(),
                    );
                    net.borrow().advance(Duration::from_secs(2));
                    for _ in 0..3 {
                        drv.idle(&mut win, 1);
                        net.borrow_mut().settle();
                    }
                }
                let attempts = net.borrow().reopens();
                net.borrow().advance(Duration::from_secs(2));
                if !was_down {
                    // The path is gone: the reload's re-open on the live connection fails.
                    net.borrow().control.change(Change::Remove);
                }
                // Cmd/Ctrl+R, the reload ending — failed — inside the pass that asked for it.
                let engine = net.clone();
                win.before_poll = Some(Box::new(move || engine.borrow_mut().settle()));
                drv.key(&mut win, Key::R, command(false));
                win.before_poll = None;
                assert!(win.stream_down() && !win.reloading, "{at}");
                let after_reload = net.borrow().reopens();
                assert!(
                    after_reload <= attempts + 1,
                    "{at}: {attempts} then {after_reload}"
                );
                if !was_down {
                    // Something is at the path again (time is moved through it); the line
                    // stays down.
                    let again = FakeFile::bytes(body.clone().into_bytes());
                    net.borrow().control.change(Change::Replace(again));
                }
                let sent = net.borrow().requests();
                for _ in 0..3 {
                    net.borrow().advance(Duration::from_secs(2));
                    for _ in 0..3 {
                        drv.idle(&mut win, 1);
                        net.borrow_mut().settle();
                    }
                }
                assert_eq!(
                    net.borrow().reopens(),
                    after_reload,
                    "{at}: a reconnect followed the failed reload with no key"
                );
                assert_eq!(net.borrow().requests(), sent, "{at}: nothing was asked for");
                assert_eq!(rows(&win), before, "{at}: the page stays");
                // The next key retries.
                drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
                net.borrow_mut().settle();
                assert_eq!(
                    net.borrow().reopens(),
                    after_reload + 1,
                    "{at}: the key's own"
                );
            }
        },
    );
}

mod cp949;
mod large;

// 08 §7 (VM-2) through the stream: the document reads its lines in an encoding, from the first
// block on, and every ranged read is held to the whole-file decode (08 §12).

use super::tests::{all_lines, encoded, hangul_lines};
use crate::encoding::Encoding::{self, *};

/// **EA-1** (VM-2) through the Fake: a preview's pass that stops at an odd byte — inside a
/// UTF-16 unit, a UTF-32 one — and the hand-over going on from there give the lines, marks and
/// encoding the local file has: a unit cut by the end of one read is finished by the next.
#[test]
fn a_streamed_wide_encoding_cut_inside_a_unit_reads_as_the_local_file() {
    bounded("a streamed wide encoding cut inside a unit", || {
        let lines = hangul_lines(3000);
        let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
        for enc in [Utf16Le, Utf32Be] {
            let body = encoded(&text, enc, true);
            let cut = 40_001u64;
            assert!(body.len() as u64 > 2 * cut);
            let mut net = net(&body, gate());
            let mut win = net.window("log.txt");
            win.preview_index = Some(cut);
            Window::set_preview(&mut win, true);
            let mut drv = Driver::new(800.0, 500.0);
            run(
                &mut drv,
                &mut win,
                &mut net,
                "the preview's pass",
                |w, _| w.doc.size() >= cut,
            );
            assert_eq!(win.doc.size(), cut, "{enc:?}: stopped inside a unit");
            Window::set_preview(&mut win, false);
            run(&mut drv, &mut win, &mut net, "the rest", |w, _| {
                w.doc.whole() && w.ready()
            });
            let tmp = tempfile::tempdir().unwrap();
            let mut local = twin(tmp.path(), "log.txt", &body);
            assert_eq!(win.doc.line_count(), 3000, "{enc:?}");
            assert_eq!(win.doc.marks, local.doc.marks, "{enc:?}");
            assert_eq!(all_lines(&mut win.doc), lines, "{enc:?}");
            assert_eq!(all_lines(&mut local.doc), lines);
            let label = format!("{} (BOM)", enc.label());
            assert_eq!(win.doc.encoding_label(), Some(label.clone()));
            drv.idle(&mut win, 1);
            assert!(says(&drv, &label), "{:?}", painted(&drv));
            assert!(
                rows(&win).iter().any(|r| r.contains('갊')),
                "{:?}",
                rows(&win)
            );
        }
    });
}

/// A remote file of nothing but a BOM: once its first block is here it is `empty`, never
/// `loading…` — it has bytes and no line.
#[test]
fn a_streamed_file_of_only_a_bom_is_empty() {
    bounded("a file of only a BOM", || {
        let mut net = net(&[0xFF, 0xFE], gate());
        let mut win = net.window("bom.txt");
        let mut drv = Driver::new(600.0, 300.0);
        run(&mut drv, &mut win, &mut net, "the first block", |w, _| {
            w.doc.size() > 0 && w.ready()
        });
        drv.idle(&mut win, 2);
        assert_eq!(status(&drv), "empty", "{:?}", painted(&drv));
        assert_eq!(win.doc.encoding_label().as_deref(), Some("UTF-16LE (BOM)"));
    });
}

/// **EA-6** (VM-2) through the Fake: `enc=` before the first block costs nothing — the pass
/// reads in it from block 0, nothing twice; another LF unit later indexes the stream again
/// from its first block, which the cache still has.
#[test]
fn enc_on_a_stream_is_read_from_its_first_block_and_another_lf_unit_reads_it_again() {
    bounded("enc= on a stream", || {
        // Korean UTF-16LE with no BOM: no pattern (§7.2), so the default would be UTF-8.
        let lines: Vec<String> = (0..2000)
            .map(|i| format!("한글 줄입니다 갊 {}", i % 10))
            .collect();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let body = encoded(&text, Utf16Le, false);
        assert_eq!(crate::encoding::pick(&body, true).encoding, Utf8);
        let mut net = net(&body, gate());
        let mut win = net.window("ko.txt");
        win.enc_wanted = Some(Utf16Le);
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the index", |w, _| {
            w.doc.whole() && w.ready()
        });
        assert_eq!(win.doc.encoding(), Utf16Le);
        assert_eq!(all_lines(&mut win.doc), lines);
        assert_eq!(net.fetched(), body.len() as u64, "nothing read twice");
        assert_eq!(rows(&win)[0], lines[0]);
        // UTF-8 ends lines at another unit: indexed again, from block 0.
        win.enc_wanted = Some(Utf8);
        drv.idle(&mut win, 1);
        run(&mut drv, &mut win, &mut net, "indexed again", |w, _| {
            w.doc.encoding() == Utf8 && w.doc.whole() && w.ready()
        });
        let utf8 = body.iter().filter(|&&b| b == b'\n').count() as u64;
        assert_eq!(win.doc.line_count(), utf8 + 1);
        // Its one block is still in the cache: nothing is fetched again.
        assert_eq!(net.fetched(), body.len() as u64);
        drv.idle(&mut win, 1);
        assert!(says(&drv, "UTF-8"), "{:?}", painted(&drv));
    });
}

/// **EA-2** (VM-2): the Fake's 24,000,000,000-byte file set to UTF-32LE (`enc=`), Go to
/// `0x300000003` → the row holding `0x300000000` at the top, every row starting at a multiple
/// of 4 and showing the core's UTF-32LE decode of the bytes from there; the jump fetches what
/// a UTF-8 jump to the same place fetches, no more.
#[test]
fn ea2_a_24_gb_jump_in_utf32le_lands_on_a_unit_and_fetches_what_utf8_does() {
    bounded("EA-2", || {
        let size = 24_000_000_000u64;
        let jumped = |enc: Option<Encoding>| {
            let mut net = net_on(FakeFile::pattern(size), gate(), NEVER);
            let mut win = net.window("data.txt");
            win.enc_wanted = enc;
            let mut drv = Driver::new(800.0, 500.0);
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
                w.doc.size() >= HIGHLIGHT_MAX
            });
            net.settle();
            drv.idle(&mut win, 3);
            net.settle();
            drv.idle(&mut win, 2);
            let before = net.fetched();
            drv.key(&mut win, Key::G, command(false));
            drv.frame(
                &mut win,
                vec![egui::Event::Text("0x300000003".into())],
                Modifiers::NONE,
            );
            drv.key(&mut win, Key::Enter, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "the jump", |w, _| {
                w.ready() && w.jump.is_none() && has_rows(w)
            });
            let rows = win.page.as_ref().unwrap().rows.clone();
            (rows, net.fetched() - before, win.doc.encoding())
        };
        let (rows, fetched, enc) = jumped(Some(Utf32Le));
        assert_eq!(enc, Utf32Le);
        let place = 0x3_0000_0000u64;
        let top = &rows[0];
        let (from, to) = (
            top.at + top.start,
            top.at + top.start + top.cut.bytes as u64,
        );
        assert!(
            from <= place && place < to,
            "the top row is {from:#x}..{to:#x}"
        );
        for row in &rows {
            let at = row.at + row.start;
            assert_eq!(at % 4, 0, "a row at {at:#x}");
            let bytes = crate::remote::pattern(at, row.cut.bytes);
            let whole: String = Utf32Le
                .decode(&bytes, true)
                .0
                .iter()
                .map(|c| wrap::face(c.ch))
                .collect();
            assert!(whole.starts_with(&row.cut.text()), "a row at {at:#x}");
        }
        let (_, by_utf8, enc) = jumped(None);
        assert_eq!(enc, Utf8);
        assert!(
            fetched <= by_utf8,
            "fetched {fetched}, a UTF-8 jump {by_utf8}"
        );
    });
}

/// **EA-1 and EA-7** (VM-2) past the 16 MiB index, through the Fake: UTF-16LE (BOM) CRLF
/// Hangul lines to past the cap, then a line longer than REACH + GRID of surrogate pairs — every
/// grid point it crosses on a pair's low half, its high half at g − 2 — but one grid point
/// holding a standalone low surrogate, then CRLF lines to the end. End, Go to past the cap
/// (into that line, whose start is out of reach) and PgUp from there show rows that start at
/// character starts of the whole-file decode and show its characters: a pair cut by a grid
/// point is one character in the piece after it, the standalone low surrogate one U+FFFD.
#[test]
fn a_wide_encoding_past_the_16_mib_index_reads_as_the_whole_file_decode() {
    bounded("UTF-16LE past 16 MiB", || {
        let unit = |text: &str| encoded(text, Utf16Le, false);
        let mut body = Utf16Le.bom().to_vec();
        let mut i = 0;
        while body.len() < HIGHLIGHT_MAX as usize + (1 << 20) {
            body.extend(unit(&format!("{i:07} 한글 줄 갊 text\r\n")));
            i += 1;
        }
        let line = body.len() as u64;
        if line.is_multiple_of(4) {
            // The pairs start at 2 mod 4: each grid point falls on a low half.
            body.extend(unit("x"));
        }
        let pairs = body.len();
        assert_eq!(pairs % 4, 2);
        while body.len() < pairs + 160 * 1024 {
            body.extend(unit("😀"));
        }
        // A grid point past the line start's reach: its pair's high half becomes `x`.
        let lone = (line + REACH + GRID) / GRID * GRID + GRID;
        assert!((lone as usize) < body.len());
        body[lone as usize - 2..lone as usize].copy_from_slice(&unit("x"));
        body.extend(unit("\r\n"));
        for j in 0..200 {
            body.extend(unit(&format!("tail {j:03} 끝\r\n")));
        }
        // The whole-file decode from the long line's start on.
        let mut starts = Vec::new();
        let mut chars = Vec::new();
        let mut at = line;
        for c in Utf16Le.decode(&body[line as usize..], true).0 {
            starts.push(at);
            chars.push(wrap::face(c.ch));
            at += u64::from(c.len);
        }
        assert_eq!(chars.iter().filter(|&&c| c == '\u{FFFD}').count(), 1);
        let check = |win: &TextWindow, what: &str| {
            let page = win.page.as_ref().expect("a page");
            assert!(!page.rows.is_empty(), "{what}");
            for row in &page.rows {
                let at = row.at + row.start;
                assert_eq!(at % 2, 0, "{what}: a row at {at:#x}");
                let k = starts
                    .binary_search(&at)
                    .unwrap_or_else(|_| panic!("{what}: a row at {at:#x} is no character start"));
                let want: String = chars[k..].iter().take(row.cut.taken).collect();
                let shown = row.cut.text();
                assert!(
                    want.starts_with(&shown),
                    "{what}: {at:#x}: {shown:?} / {want:?}"
                );
            }
        };
        let mut net = net(&body, gate());
        let mut win = net.window("big.txt");
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        net.settle();
        drv.idle(&mut win, 3);
        let settled = |w: &TextWindow, _: &Driver| w.ready() && w.jump.is_none() && has_rows(w);
        drv.key(&mut win, Key::End, command(false));
        run(&mut drv, &mut win, &mut net, "End", settled);
        check(&win, "End");
        assert_eq!(rows(&win).last().map(String::as_str), Some("tail 199 끝"));
        assert_eq!(win.doc.encoding_label().as_deref(), Some("UTF-16LE (BOM)"));
        drv.key(&mut win, Key::G, command(false));
        let place = format!("0x{:X}", lone + 1);
        drv.frame(&mut win, vec![egui::Event::Text(place)], Modifiers::NONE);
        drv.key(&mut win, Key::Enter, Modifiers::NONE);
        run(&mut drv, &mut win, &mut net, "Go to", settled);
        check(&win, "Go to");
        let top = &win.page.as_ref().unwrap().rows[0];
        assert_eq!(
            (top.at + top.start, top.loose),
            (lone, true),
            "from the grid"
        );
        assert!(rows(&win)[0].starts_with('\u{FFFD}'), "{:?}", rows(&win)[0]);
        for k in 0..3 {
            drv.key(&mut win, Key::PageUp, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "PgUp", settled);
            check(&win, &format!("PgUp {k}"));
        }
    });
}

/// A `Bytes` that gives at most `most` bytes a read — a pass whose first reads are smaller
/// than the default's 8 KiB sample.
struct Trickle {
    inner: Box<dyn Bytes + Send>,
    most: usize,
}

impl Bytes for Trickle {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<crate::stream::Result<bytes::Bytes>> {
        self.inner.try_read(off, len.min(self.most))
    }

    fn read_at(
        &mut self,
        off: u64,
        buf: &mut [u8],
        cancel: &AtomicBool,
    ) -> crate::stream::Result<usize> {
        self.inner.read_at(off, buf, cancel)
    }
}

/// Through the Fake, a pass whose reads come a byte at a time picks the default and the BOM
/// from the first 8 KiB as a block-sized read does: a UTF-16LE file with a BOM, its Hangul
/// holding 0x0A bytes, has the lines and the label the local file has.
#[test]
fn a_pass_read_a_byte_at_a_time_picks_as_a_whole_block_does() {
    bounded("a pass read a byte at a time", || {
        let lines = hangul_lines(300);
        let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
        let body = encoded(&text, Utf16Le, true);
        assert!(body.len() > crate::kind::SNIFF_BYTES);
        let mut net = net(&body, gate());
        let mut win = net.window("log.txt");
        let Backing::Stream(stream) = &mut win.doc.backing else {
            unreachable!("a streamed document");
        };
        let inner = stream.pass.take().expect("the pass's reader");
        stream.pass = Some(Box::new(Trickle { inner, most: 1 }));
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the index", |w, _| {
            w.doc.whole() && w.ready()
        });
        assert_eq!(win.doc.encoding_label().as_deref(), Some("UTF-16LE (BOM)"));
        assert_eq!(all_lines(&mut win.doc), lines);
        assert_eq!(rows(&win)[0], lines[0]);
    });
}

/// A `Bytes` whose file ends at `end` though the Source's size says more (a file cut short on
/// the remote): reads there come back empty.
struct Short {
    inner: Box<dyn Bytes + Send>,
    end: u64,
}

impl Bytes for Short {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<crate::stream::Result<bytes::Bytes>> {
        let len = self.end.saturating_sub(off).min(len as u64) as usize;
        if len == 0 {
            return Poll::Ready(Ok(bytes::Bytes::new()));
        }
        self.inner.try_read(off, len)
    }

    fn read_at(
        &mut self,
        off: u64,
        buf: &mut [u8],
        cancel: &AtomicBool,
    ) -> crate::stream::Result<usize> {
        self.inner.read_at(off, buf, cancel)
    }
}

/// A pass whose reads come back empty before the size the Source opened the file at, short of
/// the default's 8 KiB sample: the default is picked from what it read, and those lines are
/// indexed — never nothing at all.
#[test]
fn a_pass_that_ends_short_of_the_sample_indexes_what_it_read() {
    bounded("a pass that ends short of the sample", || {
        let lines = hangul_lines(300);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let body = encoded(&text, Utf16Le, true);
        let end = 3000u64;
        assert!(body.len() as u64 > crate::kind::SNIFF_BYTES as u64);
        let mut net = net(&body, gate());
        let mut win = net.window("log.txt");
        let Backing::Stream(stream) = &mut win.doc.backing else {
            unreachable!("a streamed document");
        };
        let inner = stream.pass.take().expect("the pass's reader");
        stream.pass = Some(Box::new(Short { inner, end }));
        let mut drv = Driver::new(800.0, 500.0);
        for _ in 0..400 {
            drv.idle(&mut win, 1);
            if !net.reply() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(win.doc.encoding_label().as_deref(), Some("UTF-16LE (BOM)"));
        assert!(win.doc.line_count() > 10, "{} lines", win.doc.line_count());
        assert_eq!(rows(&win)[0], lines[0]);
    });
}
