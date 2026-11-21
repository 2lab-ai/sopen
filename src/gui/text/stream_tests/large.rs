//! 06 §7.2 W5b and §12's V12: large text by byte position. The file is the `Fake`'s, any size,
//! with its bytes written by a rule of the offset ([`Written`]) — a log of numbered lines, one
//! line with no end — so a 4 GiB file costs no memory and every row on screen can be checked
//! against the offset it was read from.

use super::*;
use crate::remote::{Completion, Ranged, Refused, RemoteFs, Reply, Stat, Ticket, Wake};
use crate::stream::Fetched;
use eframe::egui::Event;

/// What the bytes of a file are, by offset.
type Text = Arc<dyn Fn(u64, usize) -> Vec<u8> + Send + Sync>;

/// A `Fake` whose reads come back as `text` says: the transport is the Fake's own — its clock,
/// its link, its faults — and only the bytes of a reply are replaced.
struct Written {
    inner: Box<dyn Ranged>,
    text: Text,
}

impl Ranged for Written {
    fn path(&self) -> &str {
        self.inner.path()
    }
    fn opened(&self) -> Stat {
        self.inner.opened()
    }
    fn read(&mut self, off: u64, len: u32) -> Ticket {
        self.inner.read(off, len)
    }
    fn fstat(&mut self) -> Ticket {
        self.inner.fstat()
    }
    fn stat_path(&mut self) -> Ticket {
        self.inner.stat_path()
    }
    fn pending(&self) -> usize {
        self.inner.pending()
    }
    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let mut done = self.inner.next(timeout)?;
        if let (Asked::Read { off, .. }, Ok(Reply::Data(bytes))) = (done.asked, &done.reply) {
            done.reply = Ok(Reply::Data((self.text)(off, bytes.len()).into()));
        }
        Some(done)
    }
    fn waker(&self) -> Wake {
        self.inner.waker()
    }
    fn now(&self) -> Instant {
        self.inner.now()
    }
    fn reopen(self: Box<Self>, path: &str) -> std::result::Result<Box<dyn Ranged>, Refused> {
        let text = self.text;
        let inner = self.inner.reopen(path)?;
        Ok(Box::new(Written { inner, text }))
    }
    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        self.inner.into_connection()
    }
    fn close(self: Box<Self>) {
        self.inner.close()
    }
}

/// A Source on a file of `size` bytes that reads as `text` says, on 06 §12's gate link.
fn written(size: u64, text: Text) -> Net {
    let (transport, control) = fake("/remote/file", FakeFile::pattern(size), gate());
    let base = transport.now() - control.elapsed();
    let transport: Box<dyn Ranged> = Box::new(Written {
        inner: transport,
        text: text.clone(),
    });
    let lines = Arc::new(Mutex::new(Vec::new()));
    let down = Arc::new(Mutex::new(None::<String>));
    let (sink, clock, again) = (lines.clone(), control.clone(), control.clone());
    let settings = Settings {
        poll_every: NEVER,
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
            let inner = again.open()?;
            Ok(Box::new(Written {
                inner,
                text: text.clone(),
            }) as Box<dyn Ranged>)
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

/// Bytes `[off, off + len)` of a file made of `unit`-byte pieces, piece `i` being `piece(i)`.
fn pieces(off: u64, len: usize, unit: u64, piece: impl Fn(u64) -> Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + unit as usize);
    let mut i = off / unit;
    let skip = (off % unit) as usize;
    while out.len() < len + skip {
        out.extend_from_slice(&piece(i));
        i += 1;
    }
    out.drain(..skip);
    out.truncate(len);
    out
}

/// Bytes in one line of [`log`].
const LINE: u64 = 64;

/// Line `i` (0-based) of the log, without its newline: 63 bytes.
fn log_line(i: u64) -> String {
    format!("line {i:012} {}", ".".repeat(45))
}

/// A log of numbered lines, each [`LINE`] bytes with its newline.
fn log() -> Text {
    Arc::new(|off, len| {
        pieces(off, len, LINE, |i| {
            let mut line = log_line(i).into_bytes();
            line.push(b'\n');
            line
        })
    })
}

/// The log, `size` bytes of it (a whole number of lines).
fn log_net(size: u64) -> Net {
    assert_eq!(size % LINE, 0);
    written(size, log())
}

/// Word `i` of [`endless`]: 16 bytes with its space.
fn word(i: u64) -> String {
    format!("w{i:014} ")
}

/// One line that never ends: numbered words, no newline anywhere.
fn endless() -> Text {
    Arc::new(|off, len| pieces(off, len, 16, |i| word(i).into_bytes()))
}

fn fetched(net: &Net) -> Fetched {
    net.source.stats().fetched
}

/// Passes and replies until nothing is on its way, nothing more goes and the window is ready
/// (or `passes` went by: the assertions after it say what is missing).
fn quiesce(drv: &mut Driver, win: &mut TextWindow, net: &mut Net, passes: usize) {
    let mut calm = 0;
    for _ in 0..passes {
        drv.idle(win, 1);
        if net.reply() {
            calm = 0;
            continue;
        }
        calm += 1;
        if calm >= 4 && win.ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The viewer on `net` with its first screen up and its 16 MiB pass done.
fn opened(net: &mut Net, name: &str) -> (TextWindow, Driver) {
    let mut win = net.window(name);
    let mut drv = Driver::new(800.0, 500.0);
    run(&mut drv, &mut win, net, "the first screen", |w, _| {
        has_rows(w)
    });
    let cap = HIGHLIGHT_MAX.min(net.source.size());
    run(&mut drv, &mut win, net, "the pass", |w, _| {
        w.doc.size() >= cap
    });
    net.settle();
    drv.idle(&mut win, 3);
    net.settle();
    drv.idle(&mut win, 2);
    (win, drv)
}

/// Cmd/Ctrl+G, `text`, Enter.
fn go_to(drv: &mut Driver, win: &mut TextWindow, text: &str) {
    drv.key(win, Key::G, command(false));
    drv.frame(win, vec![Event::Text(text.to_string())], Modifiers::NONE);
    drv.key(win, Key::Enter, Modifiers::NONE);
}

/// The gutter's texts of the last pass: what is painted left of the rows.
fn gutter(drv: &Driver, text_x: f32) -> Vec<String> {
    drv.texts()
        .into_iter()
        .filter(|(r, _, t)| r.right() < text_x && !t.is_empty())
        .map(|(_, _, t)| t)
        .collect()
}

/// **V12**, the open: a 4 GiB log shows its first screen from one block, and the background
/// pass then reads the first 16 MiB and no more.
#[test]
fn v12_a_4_gib_log_opens_from_one_block_and_its_pass_stops_at_16_mib() {
    bounded("v12 open", || {
        let size = 4u64 << 30;
        let mut net = log_net(size);
        let mut win = net.window("big.log");
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        assert_eq!(rows(&win)[0], log_line(0));
        net.out();
        assert_eq!(net.reads(), [(0, BLOCK as u32)], "one read before `first`");
        let first = net.line("first").expect("`first` is on the stats line");
        assert!(first.contains(" fetched=65536 "), "{first}");
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        net.settle();
        drv.idle(&mut win, 3);
        net.settle();
        let got = fetched(&net);
        assert_eq!(got.bulk + got.demand, HIGHLIGHT_MAX, "{got:?}");
        assert!(got.bulk <= HIGHLIGHT_MAX);
        assert_eq!(got.total, HIGHLIGHT_MAX, "nothing past the first 16 MiB");
        assert!(
            says(&drv, "lines known to 16.8 MB of 4.3 GB"),
            "{:?}",
            painted(&drv)
        );
    });
}

/// **V12**, End and PgUp: End shows the file's last lines for at most two blocks of demand and
/// prefetch — asked at once, before any reply — with byte offsets in the gutter, the top bar
/// still saying how far the lines are known, and the text where it was. Each PgUp from there
/// costs at most one block of demand.
#[test]
fn v12_end_costs_two_blocks_and_shows_byte_offsets_and_pgup_one_block_a_page() {
    bounded("v12 end", || {
        let size = 4u64 << 30;
        let lines = size / LINE;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        let x = text_x(&drv, "line 000000000000");
        let before = fetched(&net);
        let sent = net.reads().len();
        drv.key(&mut win, Key::End, command(false));
        // The jump asks once: everything it needs is on its way before the first reply.
        net.out();
        let asked: Vec<(u64, u32)> = net.reads()[sent..].to_vec();
        assert_eq!(
            asked,
            [
                (size - 2 * BLOCK, BLOCK as u32),
                (size - BLOCK, BLOCK as u32)
            ],
            "the last two blocks, in one ask"
        );
        quiesce(&mut drv, &mut win, &mut net, 400);
        let visible = win.visible;
        let shown = rows(&win);
        let want: Vec<String> = (lines - visible..lines).map(log_line).collect();
        assert_eq!(shown, want, "the file's last lines");
        let after = fetched(&net);
        assert_eq!(after.bulk, before.bulk, "the pass read nothing more");
        assert_eq!(
            after.demand + after.prefetch - before.demand - before.prefetch,
            2 * BLOCK,
            "{after:?}"
        );
        // Byte offsets in the gutter, in the place the line numbers had; the text did not move.
        assert_eq!(text_x(&drv, &log_line(lines - 1)), x, "the gutter moved");
        let gutter = gutter(&drv, x);
        let first = format!("0x{:X}", (lines - visible) * LINE);
        let last = format!("0x{:X}", (lines - 1) * LINE);
        assert!(gutter.contains(&first), "{gutter:?}");
        assert!(gutter.contains(&last), "{gutter:?}");
        assert!(
            says(&drv, "lines known to 16.8 MB of 4.3 GB"),
            "{:?}",
            painted(&drv)
        );
        assert!(status(&drv).starts_with("bytes 0x"), "{}", status(&drv));
        assert!(!status(&drv).contains(" of "), "{}", status(&drv));
        // The events of the jump (06 §11).
        let input = net.line("input").expect("the jump is an `input`");
        assert!(input.contains(" place=0x"), "{input}");
        assert!(net.line("ready").is_some(), "its page is a `ready`");
        // PgUp, page after page: the lines before, a block of demand at most for each.
        let page = visible - 1;
        let mut top = lines - visible;
        for n in 1..=40u64 {
            let demand = fetched(&net).demand;
            drv.key(&mut win, Key::PageUp, Modifiers::NONE);
            quiesce(&mut drv, &mut win, &mut net, 400);
            top -= page;
            let want: Vec<String> = (top..top + visible).map(log_line).collect();
            assert_eq!(rows(&win), want, "PgUp {n}");
            let cost = fetched(&net).demand - demand;
            assert!(cost <= BLOCK, "PgUp {n} cost {cost} of demand");
        }
        assert!(fetched(&net).bulk == before.bulk);
    });
}

/// **V12**, Go to a line past the lines known: the pass runs on from where it stopped, the
/// status row counts, and Esc stops it — nothing more is sent, and only what was on its way
/// arrives.
#[test]
fn v12_go_to_a_far_line_shows_progress_and_esc_stops_it() {
    bounded("v12 go to", || {
        let size = 4u64 << 30;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        let top = rows(&win);
        go_to(&mut drv, &mut win, "10000000");
        let mut said = false;
        for _ in 0..200 {
            drv.idle(&mut win, 1);
            said |= says(&drv, "indexing… ") && says(&drv, " of 4.3 GB");
            if said && win.doc.size() >= HIGHLIGHT_MAX + (8 << 20) {
                break;
            }
            net.reply();
        }
        assert!(said, "no progress on the status row: {:?}", painted(&drv));
        assert!(
            win.doc.size() > HIGHLIGHT_MAX,
            "the pass did not go on: {}",
            win.doc.size()
        );
        assert_eq!(rows(&win), top, "the page stays while the pass runs");
        drv.key(&mut win, Key::Escape, Modifiers::NONE);
        // What is on its way now is all that still arrives.
        let (sent, had) = (net.requests(), fetched(&net).total);
        net.settle();
        drv.idle(&mut win, 3);
        net.settle();
        drv.idle(&mut win, 3);
        assert_eq!(net.requests(), sent, "a request went out after Esc");
        let came = fetched(&net).total - had;
        assert!(came <= crate::stream::CAP_MAX, "{came} bytes after Esc");
        assert!(!says(&drv, "indexing… "), "{:?}", painted(&drv));
        assert_eq!(rows(&win), top, "the page stays");
        assert!(win.ready());
    });
}

/// Go to a line past the lines known, left to finish: the line is at the top with its number,
/// and the lines are known as far as it.
#[test]
fn go_to_a_line_past_the_prefix_indexes_as_far_as_it_and_shows_it() {
    bounded("go to line", || {
        let size = 64u64 << 20;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        // Line 300,001 (1-based) starts at 19.2 MB: past the 16 MiB the pass read.
        go_to(&mut drv, &mut win, "300001");
        quiesce(&mut drv, &mut win, &mut net, 4000);
        assert_eq!(rows(&win)[0], log_line(300_000));
        // One place asked for: one `input`, and the `ready` of its page after it (06 §11).
        let events: Vec<String> = net
            .lines
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| l.split(' ').nth(1).map(str::to_string))
            .filter(|e| e == "input" || e == "ready")
            .collect();
        assert_eq!(events, ["input", "ready"]);
        let x = text_x(&drv, &log_line(300_000));
        assert!(
            gutter(&drv, x).contains(&"300001".to_string()),
            "{:?}",
            gutter(&drv, x)
        );
        assert!(win.doc.size() >= 300_000 * LINE);
        // No further than the readahead of the pass that looked for it.
        assert!(
            fetched(&net).total <= 300_000 * LINE + crate::stream::CAP_MAX + 4 * BLOCK,
            "{:?}",
            fetched(&net)
        );
        // A line the index has goes there at once: nothing is asked for but its page.
        let had = fetched(&net).total;
        go_to(&mut drv, &mut win, "1001");
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert_eq!(rows(&win)[0], log_line(1000));
        assert!(fetched(&net).total - had <= 3 * BLOCK);
    });
}

/// A jump asks once (06 §7.2): Go to a byte offset fetches the three blocks around it, all of
/// them on their way before the first reply, as demand, with no margin beside them — and the
/// line that holds the offset is at the top, by its byte offset.
#[test]
fn a_jump_asks_once_for_at_most_three_blocks() {
    bounded("a jump asks once", || {
        let size = 4u64 << 30;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        let x = text_x(&drv, "line 000000000000");
        let before = fetched(&net);
        let sent = net.reads().len();
        // 2 GiB, a grid point on and a little: 37 bytes into a line that starts on that grid
        // point. The 64 KiB before the grid point and the 64 KiB after it lie in three blocks.
        let block = 2u64 << 30;
        let grid = block + 16 * 1024;
        go_to(&mut drv, &mut win, &format!("0x{:X}", grid + 37));
        // What the pass of the key queued goes out: all of it, before any reply is taken.
        net.out();
        assert_eq!(
            net.reads()[sent..],
            [
                (block - BLOCK, BLOCK as u32),
                (block, BLOCK as u32),
                (block + BLOCK, BLOCK as u32)
            ],
            "three blocks, in one ask"
        );
        quiesce(&mut drv, &mut win, &mut net, 400);
        let line = grid / LINE;
        assert_eq!(rows(&win)[0], log_line(line));
        assert!(
            gutter(&drv, x).contains(&format!("0x{grid:X}")),
            "{:?}",
            gutter(&drv, x)
        );
        let after = fetched(&net);
        assert_eq!(after.demand - before.demand, 3 * BLOCK, "{after:?}");
        assert_eq!(after.prefetch, before.prefetch, "a margin was fetched");
        assert_eq!(after.bulk, before.bulk);
        assert_eq!(net.reads().len(), sent + 3);
    });
}

/// A line whose start is not within 64 KiB (06 §7.2): wrapped, its rows begin at the 16 KiB
/// grid point at or before the target and the status row says the start is not known;
/// unwrapped, the row is from the same grid point with `…` and its offset in the gutter, and
/// ↓ and ↑ step one grid point. Nothing of the line but those rows is read.
#[test]
fn a_jump_into_a_line_with_no_start_in_reach_begins_on_the_grid() {
    bounded("unknown line start", || {
        let size = 1u64 << 30;
        let mut net = written(size, endless());
        let mut win = net.window("one-line.txt");
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        assert!(rows(&win)[0].starts_with("w00000000000000 w00000000000001"));
        let before = fetched(&net);
        let grid = 512u64 << 20;
        go_to(&mut drv, &mut win, &format!("0x{:X}", grid + 5000));
        quiesce(&mut drv, &mut win, &mut net, 400);
        // Wrapped: rows from the grid point, and the row the target is in at the top.
        let all = rows(&win).join(" ");
        let target = word((grid + 5000) / 16);
        assert!(
            rows(&win)[0].contains(target.trim_end()),
            "{:?}",
            rows(&win)[0]
        );
        assert!(!all.contains('\u{fffd}'), "a character was cut: {all}");
        assert!(
            says(&drv, "line start not known"),
            "{:?} {:?}",
            status(&drv),
            painted(&drv)
        );
        let after = fetched(&net);
        assert!(
            after.demand + after.prefetch - before.demand - before.prefetch <= 5 * BLOCK,
            "{after:?}"
        );
        // Unwrapped: one row, from the grid point.
        drv.key(&mut win, Key::W, Modifiers::NONE);
        quiesce(&mut drv, &mut win, &mut net, 400);
        let row = rows(&win)[0].clone();
        assert!(row.starts_with(&word(grid / 16)), "{row}");
        let x = text_x(&drv, &row[..31]);
        let mark = |at: u64| format!("…0x{at:X}");
        assert!(
            gutter(&drv, x).contains(&mark(grid)),
            "{:?}",
            gutter(&drv, x)
        );
        // ↓: no line start within reach, so one grid point on; ↑ back, and one more back.
        let step = 16 * 1024;
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert!(rows(&win)[0].starts_with(&word((grid + step) / 16)));
        assert!(gutter(&drv, x).contains(&mark(grid + step)));
        for back in [grid, grid - step] {
            drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
            quiesce(&mut drv, &mut win, &mut net, 400);
            assert!(
                rows(&win)[0].starts_with(&word(back / 16)),
                "{}",
                rows(&win)[0]
            );
            assert!(gutter(&drv, x).contains(&mark(back)));
        }
        // The line was never read whole: beside the pass's 16 MiB, only the pieces shown (a
        // page of them, one row each, is 16 KiB a row).
        let got = fetched(&net);
        assert!(got.bulk <= HIGHLIGHT_MAX, "{got:?}");
        assert!(got.demand + got.prefetch < 8 << 20, "{got:?}");
        assert!(win.doc.page_reads() < 16 << 20, "{}", win.doc.page_reads());
    });
}

/// Unwrapped, a long line can be scrolled through (06 §7.2): today it is cut at 4,096 bytes
/// and nothing past that can be reached. → moves by bytes of the line, End goes to the line's
/// end, Home back to its start — and the row shows the bytes there.
#[test]
fn an_unwrapped_long_line_scrolls_to_any_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let words = 6_400u64;
    let mut body: Vec<u8> = (0..words).flat_map(|i| word(i).into_bytes()).collect();
    body.push(b'\n');
    body.extend_from_slice(b"the line after\n");
    let mut win = twin(tmp.path(), "long.txt", &body);
    let mut drv = Driver::new(800.0, 500.0);
    drv.until(&mut win, 60, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.until(&mut win, 60, |w| w.ready());
    assert!(rows(&win)[0].starts_with(&word(0)), "{:?}", rows(&win)[0]);
    assert_eq!(rows(&win)[1], "the line after");
    // → twice: 16 bytes on, one word.
    for _ in 0..2 {
        drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    }
    drv.until(&mut win, 60, |w| w.ready());
    assert!(rows(&win)[0].starts_with(&word(1)), "{:?}", rows(&win)[0]);
    // End: the end of the line at the top, past the 4,096 bytes it used to be cut at.
    drv.key(&mut win, Key::End, Modifiers::NONE);
    drv.until(&mut win, 60, |w| w.ready());
    let row = rows(&win)[0].clone();
    assert!(
        row.trim_end().ends_with(word(words - 1).trim_end()),
        "{row}"
    );
    assert_eq!(rows(&win).len(), 2, "still the two lines: {:?}", rows(&win));
    assert!(
        !says(&drv, "bytes are cut"),
        "nothing is cut any more: {:?}",
        painted(&drv)
    );
    // ← comes back by as much; Home is the line's start again.
    drv.key(&mut win, Key::ArrowLeft, Modifiers::NONE);
    drv.until(&mut win, 60, |w| w.ready());
    assert_ne!(rows(&win)[0], row);
    drv.key(&mut win, Key::Home, Modifiers::NONE);
    drv.until(&mut win, 60, |w| w.ready());
    assert!(rows(&win)[0].starts_with(&word(0)));
}

/// The prefix grows from what the screen reads (06 §7.2): scrolling on past the 16 MiB the
/// pass read keeps line numbers in the gutter, because the screen's blocks are contiguous
/// with the lines known.
#[test]
fn the_lines_known_grow_with_the_screen_past_16_mib() {
    bounded("the prefix grows with the screen", || {
        let size = 64u64 << 20;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        assert_eq!(win.doc.size(), HIGHLIGHT_MAX);
        // The last lines known, then on from there.
        let last = HIGHLIGHT_MAX / LINE;
        go_to(&mut drv, &mut win, &last.to_string());
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert_eq!(rows(&win)[0], log_line(last - 1));
        let x = text_x(&drv, &log_line(last - 1));
        for _ in 0..8 {
            drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            quiesce(&mut drv, &mut win, &mut net, 400);
        }
        let page = win.visible - 1;
        let top = last - 1 + 8 * page;
        assert_eq!(rows(&win)[0], log_line(top));
        assert!(win.doc.size() > HIGHLIGHT_MAX, "{}", win.doc.size());
        assert!(
            gutter(&drv, x).contains(&(top + 1).to_string()),
            "{:?}",
            gutter(&drv, x)
        );
        // Not read through: only what the screen and its readahead brought in.
        assert!(
            fetched(&net).total <= HIGHLIGHT_MAX + (3 << 20),
            "{:?}",
            fetched(&net)
        );
        assert!(says(&drv, "lines known to "), "{:?}", painted(&drv));
    });
}

/// The scroll bar is by bytes while the line count is not known (06 §7.2), and a click on it
/// is a jump: the page is the one at that share of the file's bytes.
#[test]
fn the_scroll_bar_jumps_by_bytes_while_the_line_count_is_unknown() {
    bounded("the scroll bar", || {
        let size = 4u64 << 30;
        let lines = size / LINE;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        let track = drv
            .texts()
            .into_iter()
            .find(|(_, _, t)| t.starts_with('↑'))
            .map(|(r, _, _)| r)
            .expect("the scroll bar");
        let before = fetched(&net);
        drv.click(&mut win, track.center());
        quiesce(&mut drv, &mut win, &mut net, 400);
        let top = rows(&win)[0].clone();
        let number: u64 = top[5..17].parse().expect("a line of the log");
        let share = number as f64 / lines as f64;
        assert!((0.4..0.6).contains(&share), "line {number} of {lines}");
        let after = fetched(&net);
        assert!(after.demand - before.demand <= 3 * BLOCK, "{after:?}");
        assert_eq!(after.bulk, before.bulk);
        // The thumb is where the page is, by bytes.
        let thumb = crate::gui::chrome::PALETTE.thumb;
        let at = drv
            .rects()
            .into_iter()
            .find(|(_, fill, _)| *fill == thumb)
            .map(|(r, _, _)| r.center().y)
            .expect("the thumb");
        assert!(
            (at - track.center().y).abs() <= 24.0,
            "thumb at {at}, track {track:?}"
        );
    });
}

/// The `\r` of a line that ends far away is not looked for before its last row is on screen
/// (06 §7.2): through an index that names bytes not yet fetched, the first screen of a 300 KB
/// CRLF line fetches its first block and not its last — and the row that holds the line's end
/// drops the `\r`.
#[test]
fn the_cr_of_a_far_line_end_is_not_fetched_before_its_last_row() {
    bounded("the \\r probe", || {
        let mut body = "word ".repeat(60_000).into_bytes();
        body.extend_from_slice(b"the end\r\nand the next line\r\n");
        let mut net = net(&body, gate());
        let doc = indexed(&net, &body);
        let mut win = TextWindow::new(doc, "box:/remote/file".into(), "crlf.txt", Vec::new());
        win.first = true;
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        net.settle();
        drv.idle(&mut win, 2);
        net.settle();
        let far: Vec<(u64, u32)> = net
            .reads()
            .into_iter()
            .filter(|r| r.0 >= 2 * BLOCK)
            .collect();
        assert_eq!(far, [], "the line's end was fetched for its first rows");
        // The line's last rows, then the next line: no `\r` is shown (it would be `♪`).
        drv.key(&mut win, Key::End, Modifiers::NONE);
        quiesce(&mut drv, &mut win, &mut net, 400);
        let shown = rows(&win);
        assert!(
            shown.iter().any(|r| r.trim_end().ends_with("the end")),
            "{shown:?}"
        );
        assert!(shown.iter().all(|r| !r.contains('♪')), "{shown:?}");
        drv.key(&mut win, Key::End, command(false));
        quiesce(&mut drv, &mut win, &mut net, 400);
        let shown = rows(&win);
        assert_eq!(shown.last().map(String::as_str), Some("and the next line"));
        assert!(shown.iter().all(|r| !r.contains('♪')), "{shown:?}");
    });
}

/// 06 Tensions, settled: a local file over 16 MiB keeps its whole-file pass — every byte is
/// indexed before its first screen, and every line has its number — and it is highlighted as
/// a stream is: over its first 16 MiB, plain past them.
#[test]
fn a_local_file_over_16_mib_is_indexed_whole_and_highlighted_over_its_first_16_mib() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = wide_rust(260_000);
    let total = body.len() as u64;
    assert!(total > HIGHLIGHT_MAX + (2 << 20), "{total}");
    body.push_str("// the end\n");
    let total = body.len() as u64;
    let lines = body.lines().count() as u64;
    let mut win = twin(tmp.path(), "big.rs", body.as_bytes());
    // The pass read the whole file, once, before the window was made.
    let read: u64 = index_reads(&tmp.path().join("big.rs"))
        .iter()
        .map(|r| r.1)
        .sum();
    assert_eq!(read, total);
    assert_eq!(win.doc.line_count(), lines);
    assert!(win.doc.whole());
    let mut drv = Driver::new(800.0, 500.0);
    drv.until(&mut win, 60, |w| w.ready());
    let coloured = |win: &TextWindow| {
        colors(win).iter().any(|line| {
            let distinct: std::collections::HashSet<_> = line.iter().collect();
            distinct.len() >= 2
        })
    };
    assert!(coloured(&win), "the first page is plain");
    assert!(
        !says(&drv, "shown without highlighting"),
        "{:?}",
        painted(&drv)
    );
    assert!(says(
        &drv,
        &format!("{} lines", crate::gui::hex::grouped(lines))
    ));
    // Past 16 MiB: plain, with its line numbers.
    drv.key(&mut win, Key::End, command(false));
    drv.until(&mut win, 60, |w| w.ready());
    drv.idle(&mut win, 3);
    assert_eq!(rows(&win).last().map(String::as_str), Some("// the end"));
    assert!(!coloured(&win), "highlighted past 16 MiB");
    let x = text_x(&drv, "// the end");
    assert!(gutter(&drv, x).contains(&lines.to_string()));
}

/// A pass over nothing yet.
fn scan() -> Scan {
    Scan {
        marks: vec![Mark { line: 0, offset: 0 }],
        ..Scan::default()
    }
}

/// The most the index of `size` bytes of text may take (06 §7.2): 16 MiB, or — past the
/// 21 GiB where that is more — three marks to each 64 KiB.
fn index_bound(size: u64) -> usize {
    let marks = MARKS_MAX.max(3 * size.div_ceil(SPAN) as usize + 1);
    marks * std::mem::size_of::<Mark>()
}

/// The index of a file of `size` bytes whose lines are `shape`'s, over and over: runs of
/// `(count, width)` lines. Made as the pass makes it, a mark at a time, without the bytes.
fn index_of(size: u64, shape: &[(u64, u64)]) -> Scan {
    let mut scan = scan();
    'file: loop {
        for &(count, width) in shape {
            let left = (size - scan.at) / width;
            if left == 0 {
                break 'file;
            }
            scan.lines_of(count.min(left), width, size);
        }
    }
    scan
}

/// Every line start that is not a mark lies less than a span past the mark before it: what
/// finding a line through the index relies on (it reads at most a span).
fn assert_in_reach(marks: &[Mark], starts: &[u64]) {
    let mut k = 0;
    for (line, &start) in starts.iter().enumerate() {
        while k + 1 < marks.len() && marks[k + 1].line <= line as u64 {
            k += 1;
        }
        let mark = marks[k];
        assert!(mark.offset <= start, "line {line}");
        if mark.line == line as u64 {
            assert_eq!(mark.offset, start, "the mark of line {line}");
        } else {
            assert!(
                start - mark.offset < SPAN,
                "line {line} starts {} past its mark",
                start - mark.offset
            );
        }
    }
}

/// The marks the test's shortcut gives are the marks the pass gives the same bytes.
#[test]
fn the_index_without_its_bytes_is_the_index_of_the_bytes() {
    for limit in [0, 64, 500] {
        for shape in [
            vec![(1_000_000u64, 1u64)],
            vec![(30_000, 37)],
            vec![(300, 1), (1, 70_000), (255, 3), (2, 40_000)],
            vec![(257, 255), (1, 65_536)],
        ] {
            let size: u64 = 3 * shape.iter().map(|(n, w)| n * w).sum::<u64>() + 1;
            let mut fast = Scan { limit, ..scan() };
            let mut slow = Scan { limit, ..scan() };
            for _ in 0..3 {
                for &(count, width) in &shape {
                    fast.lines_of(count, width, size);
                    let mut line = vec![b'x'; width as usize];
                    line[width as usize - 1] = b'\n';
                    for _ in 0..count {
                        slow.feed(&line, size);
                    }
                }
            }
            assert_eq!((fast.at, fast.line), (slow.at, slow.line));
            assert_eq!(fast.marks, slow.marks, "limit {limit}, {shape:?}");
            assert_eq!(fast.held, slow.held);
            assert_eq!(fast.spacing(), slow.spacing());
        }
    }
}

/// **V12**, the index: 4 GiB of empty lines — a mark every 256 bytes as the index was,
/// 268 MB of them — stay within 16 MiB of index.
#[test]
fn v12_the_index_of_4_gib_of_empty_lines_stays_within_16_mib() {
    let size = 4u64 << 30;
    let scan = index_of(size, &[(size, 1)]);
    assert_eq!((scan.at, scan.line), (size, size));
    let bytes = scan.marks.len() * std::mem::size_of::<Mark>();
    assert!(bytes <= 16 << 20, "{bytes} bytes of index");
    assert!(scan.marks.len() > MARKS_MAX / 4, "{}", scan.marks.len());
}

/// The index's memory is bounded (06 §7.2, §10), whatever the lines: for a 24 GB text file it
/// is at most 17.6 MB — 16 MiB, or three marks to each 64 KiB where that is more — and there
/// is one copy of it. Lines of nothing, a log's, the shapes that keep the most marks.
#[test]
fn the_index_of_a_24_gb_file_is_bounded_whatever_its_lines() {
    let shapes: [&[(u64, u64)]; 7] = [
        // Empty lines: the most lines a file can have.
        &[(u64::MAX, 1)],
        // A log.
        &[(u64::MAX, 64)],
        &[(u64::MAX, 200)],
        // 256 lines in a kilobyte, then a line that ends a span on: two marks to a span,
        // neither of which may go.
        &[(256, 1), (1, 65_536)],
        &[(255, 1), (1, 65_281)],
        &[(256, 4), (1, 64_512)],
        // Long lines only: a mark each.
        &[(u64::MAX, 70_000)],
    ];
    for size in [24_000_000_000u64, 24 << 30, 64 << 30] {
        for shape in shapes {
            let scan = index_of(size, shape);
            let bytes = scan.marks.len() * std::mem::size_of::<Mark>();
            assert!(
                bytes <= index_bound(size),
                "{size} bytes of {shape:?}: {bytes} bytes of index, over {}",
                index_bound(size)
            );
        }
    }
    assert_eq!(index_bound(24_000_000_000), 17_578_144);
    assert_eq!(index_bound(4 << 30), 16 << 20);
    assert_eq!(std::mem::size_of::<Mark>(), 16);
}

/// A thinned index still finds every line (06 §7.2): no line start is a span or more past the
/// mark before it, the lines read through it are the file's, and the document holds the one
/// copy of the marks.
#[test]
fn a_thinned_index_still_finds_every_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = Vec::new();
    for i in 0..9_000u32 {
        match i % 700 {
            0 => body.extend_from_slice("long ".repeat(15_000).as_bytes()),
            1..=300 => {}
            _ => body.extend_from_slice(format!("line {i}").as_bytes()),
        }
        body.push(b'\n');
    }
    let path = tmp.path().join("mixed.txt");
    std::fs::write(&path, &body).unwrap();
    let mut doc = TextDoc::open(&path).unwrap();
    let whole = doc.marks.clone();
    let mut thin = Scan {
        limit: 24,
        ..scan()
    };
    for piece in body.chunks(10_007) {
        thin.feed(piece, body.len() as u64);
    }
    assert!(thin.spacing() > BLOCK_LINES, "the index was never thinned");
    assert!(
        thin.marks.len() < whole.len(),
        "{} of {}",
        thin.marks.len(),
        whole.len()
    );
    let starts: Vec<u64> = std::iter::once(0)
        .chain(
            body.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i as u64 + 1),
        )
        .filter(|s| *s < body.len() as u64)
        .collect();
    assert_in_reach(&whole, &starts);
    assert_in_reach(&thin.marks, &starts);
    // Read through the thinned index: the same lines, at the same places.
    let want = doc.read_lines(0, 9_000).unwrap();
    // (Whether a line's last byte was looked at depends on where the marks are: not compared.)
    let place = |s: &Span| (s.line, s.start, s.len, s.next);
    let places = doc.spans(0, 9_000).unwrap();
    doc.install(&thin);
    assert_eq!(doc.line_count(), 9_000);
    assert_eq!(doc.read_lines(0, 9_000).unwrap(), want);
    let thinned = doc.spans(0, 9_000).unwrap();
    assert_eq!(
        thinned.iter().map(place).collect::<Vec<_>>(),
        places.iter().map(place).collect::<Vec<_>>()
    );
    for line in [0u64, 1, 299, 700, 701, 4_321, 8_999] {
        let alone = doc.read_lines(line, 1).unwrap();
        assert_eq!(alone[0], want[line as usize], "line {line}");
        let at = places[line as usize].start;
        assert_eq!(doc.span_at(at).unwrap().unwrap().line, Some(line));
    }
}

/// The index has one copy (06 §7.2): a stream's pass adds to the document's marks and keeps
/// none of its own, and the highlighter's copy is of the first 16 MiB only.
#[test]
fn the_index_is_held_once_and_the_highlighter_has_the_first_16_mib_of_it() {
    bounded("one copy of the index", || {
        let size = 24u64 << 20;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        // The pass's 16 MiB, then on to 22 MiB by Go to.
        go_to(&mut drv, &mut win, "350000");
        quiesce(&mut drv, &mut win, &mut net, 4000);
        assert_eq!(rows(&win)[0], log_line(349_999));
        let Backing::Stream(stream) = &win.doc.backing else {
            unreachable!("a streamed document");
        };
        assert!(
            stream.scan.marks.is_empty(),
            "the pass kept marks of its own"
        );
        assert!(win.doc.marks() >= 350_000 / 256);
        let copy = win.doc.share_first(HIGHLIGHT_MAX).unwrap();
        assert!(copy.size() <= HIGHLIGHT_MAX, "{}", copy.size());
        assert!(copy.size() > HIGHLIGHT_MAX - 2 * SPAN, "{}", copy.size());
        assert_eq!(copy.line_count(), copy.size() / LINE);
        assert!(copy.marks() <= (HIGHLIGHT_MAX / LINE / BLOCK_LINES) as usize + 1);
        assert!(copy.marks() < win.doc.marks());
    });
}

/// Go to a line whose start is known reads nothing through (06 §7.2): line 1 of a file that
/// is one endless line is its first byte — the pass is not sent to look for the line's end.
/// And what is neither a line number nor an offset is said, and goes nowhere.
#[test]
fn go_to_reads_through_only_for_a_line_past_the_lines_known() {
    bounded("go to, lines known", || {
        let size = 1u64 << 30;
        let mut net = written(size, endless());
        let (mut win, mut drv) = opened(&mut net, "one-line.txt");
        go_to(&mut drv, &mut win, &format!("0x{:X}", 300u64 << 20));
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert!(!rows(&win)[0].starts_with(&word(0)));
        let had = fetched(&net);
        go_to(&mut drv, &mut win, "1");
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert!(rows(&win)[0].starts_with(&word(0)), "{:?}", rows(&win)[0]);
        assert!(!says(&drv, "indexing… "), "{:?}", painted(&drv));
        let got = fetched(&net);
        assert_eq!(got.bulk, had.bulk, "the pass read on");
        assert!(got.total - had.total <= 4 * BLOCK, "{got:?}");
        // Not a place: said, and the page stays.
        let before = rows(&win);
        go_to(&mut drv, &mut win, "half way");
        quiesce(&mut drv, &mut win, &mut net, 50);
        assert!(
            says(
                &drv,
                "Go to: 'half way' is not a line number or a 0x… offset"
            ),
            "{:?}",
            painted(&drv)
        );
        assert_eq!(rows(&win), before);
        // Esc leaves the field: nothing goes anywhere, and the keys are the window's again.
        drv.key(&mut win, Key::G, command(false));
        drv.frame(
            &mut win,
            vec![Event::Text("0x100000".into())],
            Modifiers::NONE,
        );
        assert!(says(&drv, "Go to"), "{:?}", painted(&drv));
        drv.key(&mut win, Key::Escape, Modifiers::NONE);
        quiesce(&mut drv, &mut win, &mut net, 50);
        assert_eq!(rows(&win), before);
        assert!(win.goto.is_none());
    });
}

/// While the Go to field has the typing, the window's keys are not its own: a `w` typed there
/// does not switch wrapping.
#[test]
fn typing_in_go_to_is_not_the_windows_keys() {
    bounded("go to, typing", || {
        let mut net = log_net(1 << 20);
        let (mut win, mut drv) = opened(&mut net, "small.log");
        assert!(win.wrap);
        drv.key(&mut win, Key::G, command(false));
        drv.frame(&mut win, vec![Event::Text("w".into())], Modifiers::NONE);
        drv.key(&mut win, Key::W, Modifiers::NONE);
        drv.key(&mut win, Key::End, Modifiers::NONE);
        assert!(win.wrap, "a key typed in the field switched wrapping");
        assert_eq!(
            rows(&win)[0],
            log_line(0),
            "End in the field moved the page"
        );
        drv.key(&mut win, Key::Escape, Modifiers::NONE);
        drv.key(&mut win, Key::W, Modifiers::NONE);
        assert!(!win.wrap, "the keys are the window's again");
    });
}

/// The thumb dragged along the scroll bar fetches nothing until it is let go (06 §11: a place
/// the thumb only passes is not an `input`): then one jump, to where it was let go.
#[test]
fn a_dragged_thumb_jumps_once_where_it_is_let_go() {
    bounded("the thumb, dragged", || {
        let size = 4u64 << 30;
        let lines = size / LINE;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        let track = drv
            .texts()
            .into_iter()
            .find(|(_, _, t)| t.starts_with('↑'))
            .map(|(r, _, _)| r)
            .expect("the scroll bar");
        let sent = net.requests();
        let x = track.center().x;
        let from = Pos2::new(x, track.top() + 24.0);
        let to = Pos2::new(x, track.top() + track.height() * 0.75);
        let mut inputs = 0;
        drv.drag_watching(&mut win, egui::PointerButton::Primary, from, to, |_| {
            inputs += 1;
        });
        assert!(inputs > 2, "the drag had steps");
        net.out();
        let asked = net.requests() - sent;
        assert!(asked <= 4, "{asked} requests: only the jump's");
        let marked = net
            .lines
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("stream input "))
            .count();
        assert_eq!(marked, 1, "one `input`, where the thumb was let go");
        quiesce(&mut drv, &mut win, &mut net, 400);
        let number: u64 = rows(&win)[0][5..17].parse().expect("a line of the log");
        let share = number as f64 / lines as f64;
        assert!((0.6..0.9).contains(&share), "line {number} of {lines}");
    });
}

/// A stream over 16 MiB is highlighted over its first 16 MiB and plain past them (06 §7.2),
/// as a local file is: the highlighter's state runs from the top of the file.
#[test]
fn a_stream_over_16_mib_is_highlighted_over_its_first_16_mib() {
    bounded("highlighting, 16 MiB", || {
        let mut body = wide_rust(260_000);
        body.push_str("// the end\n");
        assert!(body.len() as u64 > HIGHLIGHT_MAX + (2 << 20));
        let mut net = net(body.as_bytes(), gate());
        let (mut win, mut drv) = opened(&mut net, "big.rs");
        let coloured = |win: &TextWindow| {
            colors(win).iter().any(|line| {
                let distinct: std::collections::HashSet<_> = line.iter().collect();
                distinct.len() >= 2
            })
        };
        run(&mut drv, &mut win, &mut net, "the first page", |w, _| {
            w.ready() && coloured(w)
        });
        // The end: by bytes, plain.
        drv.key(&mut win, Key::End, command(false));
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert_eq!(rows(&win).last().map(String::as_str), Some("// the end"));
        assert!(!coloured(&win), "highlighted past 16 MiB");
        assert!(win.ready());
        // Back at the top: its colours again.
        drv.key(&mut win, Key::Home, command(false));
        run(
            &mut drv,
            &mut win,
            &mut net,
            "the first page again",
            |w, _| w.ready() && coloured(w),
        );
        assert_eq!(rows(&win)[0], "// numbers, one a line");
    });
}

/// Cmd/Ctrl+R at a place past the lines known comes back to it (06 §5): the file is opened
/// again, its first block read, and the same bytes are on screen — by a jump, not by the pass.
#[test]
fn a_reload_at_a_place_past_the_lines_known_comes_back_to_it() {
    bounded("reload, far", || {
        let size = 4u64 << 30;
        let mut net = log_net(size);
        let (mut win, mut drv) = opened(&mut net, "big.log");
        go_to(&mut drv, &mut win, &format!("0x{:X}", 3u64 << 30));
        quiesce(&mut drv, &mut win, &mut net, 400);
        let before = rows(&win);
        assert_eq!(before[0], log_line((3u64 << 30) / LINE));
        drv.key(&mut win, Key::R, command(false));
        assert!(!win.ready(), "reloading");
        run(&mut drv, &mut win, &mut net, "the reload", |w, _| {
            w.ready() && w.doc.size() > 0
        });
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert_eq!(rows(&win), before);
        let got = fetched(&net);
        assert!(got.total <= 2 * HIGHLIGHT_MAX + (2 << 20), "{got:?}");
    });
}

/// A viewer opened at a place far into a large file (`SOPEN_STATE` `at=`, as [`embed_source`]
/// sets it): the first block, then the jump's three — the pass does not start before that
/// screen is up, nor does the place wait for it — with the jump's `input` and `ready` on the
/// stats line (06 §11).
#[test]
fn a_viewer_opened_at_a_far_place_fetches_four_blocks_before_its_first_screen() {
    bounded("opened at a far place", || {
        let size = 4u64 << 30;
        let at = (3u64 << 30) + 16 * 1024 + 5;
        let mut net = log_net(size);
        let mut win = net.window("big.log");
        win.jump_to(Jump::Byte(at));
        let mut drv = Driver::new(800.0, 500.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        assert_eq!(rows(&win)[0], log_line(at / LINE));
        net.out();
        let block = 3u64 << 30;
        // All that was sent by then: the pass has asked for nothing.
        assert_eq!(
            net.reads(),
            [
                (0, BLOCK as u32),
                (block - BLOCK, BLOCK as u32),
                (block, BLOCK as u32),
                (block + BLOCK, BLOCK as u32)
            ]
        );
        let first = net.line("first").expect("`first` is on the stats line");
        assert!(first.contains(" fetched=262144 "), "{first}");
        assert!(first.contains(" bulk=0 "), "{first}");
        let lines = net.lines.lock().unwrap().clone();
        let at_of = |event: &str| {
            lines
                .iter()
                .position(|l| l.starts_with(&format!("stream {event} ")))
        };
        let (input, ready) = (at_of("input"), at_of("ready"));
        assert!(input.is_some() && input < ready, "{lines:?}");
        assert!(lines[input.unwrap()].contains(&format!(" place={at:#x}")));
        // The pass then reads its 16 MiB, and the page stays where it is.
        let shown = rows(&win);
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        quiesce(&mut drv, &mut win, &mut net, 400);
        assert_eq!(rows(&win), shown);
        assert!(fetched(&net).total <= HIGHLIGHT_MAX + 4 * BLOCK);
    });
}

/// Past the lines known, the rows are the same going down and going back up (06 §7.2), and
/// they are the file's bytes: short lines, lines of tens of kilobytes whose start is found by
/// looking back, a line too long for that (cut in pieces on the grid), CRLF ends, and
/// characters of several bytes across the grid points.
#[test]
fn rows_past_the_lines_known_are_the_same_down_and_up() {
    bounded("rows by bytes, down and up", || {
        let words = |n: usize, w: &str| vec![w; n].join(" ");
        let mut lines: Vec<String> = Vec::new();
        for i in 0..40 {
            lines.push(format!("short line {i}"));
        }
        lines.push(words(6_000, "thirty"));
        lines.push(String::new());
        lines.push(words(11_000, "seventy"));
        lines.push("between".into());
        lines.push(words(30_000, "한글 two"));
        for i in 0..300 {
            lines.push(format!("tail {i} {}", words(i % 40, "x")));
        }
        lines.push(words(9_000, "sixty5"));
        lines.push("the last line".into());
        // Every third line ends CRLF.
        let mut body = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            body.extend_from_slice(line.as_bytes());
            body.extend_from_slice(if i % 3 == 0 { b"\r\n" } else { b"\n" });
        }
        assert!(body.len() > 500_000);
        let mut net = net(&body, gate());
        let mut doc = TextDoc::streamed(net.source.clone());
        // The lines are known as far as the first block, and the pass goes no further.
        doc.set_cap(BLOCK);
        loop {
            match doc.grow(BLOCK, BLOCK) {
                Ok(Some(_)) if doc.size() >= BLOCK => break,
                Ok(_) => {}
                Err(e) if is_pending(&e) => net.settle(),
                Err(e) => panic!("{e:#}"),
            }
        }
        assert!(!doc.whole() && !doc.passing());
        // (A pass of any window first: the fonts are there from then on.)
        let tmp = tempfile::tempdir().unwrap();
        let mut any = twin(tmp.path(), "any.txt", b"any\n");
        let mut drv = Driver::new(800.0, 500.0);
        drv.idle(&mut any, 1);
        let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
        let font = chrome::font();
        let cell = drv.ctx.fonts_mut(|f| f.glyph_width(&font, '0'));
        let room = 640.0;
        let mut memo = wrap::Memo::default();
        memo.reset(room);
        let mut w = wrap::Wrapper {
            doc: &mut doc,
            memo: &mut memo,
            m: wrap::Measure::new(&drv.ctx, font, plain),
            width: room,
            read: wrap::read_cap(room, cell),
        };
        // A read that is on its way is asked again once it has come.
        macro_rules! got {
            ($op:expr) => {
                loop {
                    match $op {
                        Ok(v) => break v,
                        Err(e) if is_pending(&e) => net.settle(),
                        Err(e) => panic!("{e:#}"),
                    }
                }
            };
        }
        let (mut down, mut p) = (Vec::new(), wrap::Pos::default());
        let mut text = String::new();
        loop {
            let row = got!(w.page(p, 1)).0.remove(0);
            if row.start == 0 && !row.loose && !down.is_empty() {
                text.push('\n');
            }
            text.push_str(&row.cut.text());
            if row.cut.taken > row.cut.shown.len() {
                text.push(' ');
            }
            down.push(p);
            match got!(w.next(p)) {
                Some(q) => p = q,
                None => break,
            }
        }
        assert_eq!(text, lines.join("\n"), "the rows are the file");
        assert!(down.iter().any(|p| p.loose), "no line was cut on the grid");
        let mut up = vec![p];
        while let Some(q) = got!(w.prev(p)) {
            up.push(q);
            p = q;
        }
        up.reverse();
        assert_eq!(up.len(), down.len());
        assert_eq!(up, down, "the same rows going back up");
        // And from any byte: the row that holds it is one of them.
        for x in (0..body.len() as u64).step_by(20_011) {
            let at = got!(w.at_byte(x));
            assert!(down.contains(&at), "byte {x}: {at:?} is no row start");
            assert!(at.at + at.off <= x, "byte {x}: {at:?}");
        }
        // The end of the file, found from its end alone, is the last of them.
        let end = got!(w.end(1, false));
        assert_eq!(Some(&end), down.last());
    });
}

/// The same with lines of 65 bytes in a larger window, as the live check ran it: the first
/// mark's 256 lines are longer than one read, and placing byte 0 — which the jump replaces —
/// read them in two, a sequential run that brought block 1 as readahead.
#[test]
fn a_viewer_opened_at_a_far_place_does_not_read_the_start_of_the_file() {
    bounded("opened at a far place, 65-byte lines", || {
        let size = 2_181_038_080u64;
        let at = (20_000_000 - 1) * 65 + 3;
        let line = |i: u64| format!("line {:010} of 33554432 {}", i + 1, ".".repeat(36));
        let text: Text = Arc::new(move |off, len| {
            pieces(off, len, 65, |i| {
                let mut bytes = line(i).into_bytes();
                bytes.push(b'\n');
                bytes
            })
        });
        let mut net = written(size, text);
        let mut win = net.window("big.txt");
        win.jump_to(Jump::Byte(at));
        let mut drv = Driver::new(1000.0, 640.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        assert_eq!(rows(&win)[0], line(20_000_000 - 1));
        net.out();
        let block = at / BLOCK * BLOCK;
        assert_eq!(
            net.reads(),
            [
                (0, BLOCK as u32),
                (block - BLOCK, BLOCK as u32),
                (block, BLOCK as u32),
                (block + BLOCK, BLOCK as u32)
            ]
        );
    });
}

/// `SOPEN_STATE` `loading=1` (06 §11): a streamed text window is ready to be shot at the first
/// frame drawn with rows from the stream, and a page read still on its way is shot as it is.
/// Without it the window is ready once the screen's bytes have arrived. Neither is ready
/// before a row has come.
#[test]
fn loading_1_is_ready_at_the_first_rows_and_shoots_what_is_on_its_way() {
    bounded("loading=1", || {
        let body = numbered(8_000);
        for loading in [false, true] {
            let at = format!("loading {loading}");
            let mut net = net(body.as_bytes(), gate());
            let mut win = net.window("big.log");
            win.shoot_loading = loading;
            let mut drv = Driver::new(800.0, 500.0);
            // Nothing has come: a dim `loading…` row, and not ready either way.
            drv.idle(&mut win, 2);
            assert_eq!(loading_rows(&drv), 1, "{at}: {:?}", painted(&drv));
            assert!(!win.ready(), "{at}: ready before a row came");
            run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
                has_rows(w)
            });
            assert!(win.ready(), "{at}: the first rows are drawn");
            let first = rows(&win);
            // A page read that does not arrive: the page on screen stays, the row says so.
            let held = gated(&mut win);
            held.lock().unwrap().pending = usize::MAX;
            drv.key(&mut win, Key::PageDown, Modifiers::NONE);
            drv.idle(&mut win, 2);
            assert_eq!(rows(&win), first, "{at}: the page stays");
            assert!(status(&drv).ends_with("loading…"), "{at}: {}", status(&drv));
            assert_eq!(
                win.ready(),
                loading,
                "{at}: a page read on its way is shot only with loading=1"
            );
            // It arrives: ready either way, on the page asked for.
            held.lock().unwrap().pending = 0;
            run(&mut drv, &mut win, &mut net, "the page", |w, _| {
                rows(w) != first
            });
            assert!(win.ready(), "{at}");
            // (The status row is drawn before the page is read: the pass after says it.)
            drv.idle(&mut win, 1);
            assert!(!status(&drv).contains("loading"), "{at}: {}", status(&drv));
        }
        // A file read in place has nothing on its way: the key changes nothing for it.
        let tmp = tempfile::tempdir().unwrap();
        let mut local = twin(tmp.path(), "local.log", body.as_bytes());
        local.shoot_loading = true;
        let mut drv = Driver::new(800.0, 500.0);
        assert!(!local.ready(), "not before a pass has drawn it");
        drv.until(&mut local, 60, |w| w.ready());
        assert!(has_rows(&local));
    });
}
