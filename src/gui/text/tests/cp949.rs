//! 08 §7.4 (VM-4) in the text document, locally: a CP949 `.txt` picked by step 3 and read as
//! encoding_rs decodes it, set to UTF-8 and back with no index pass (EA-3); a line longer than
//! REACH with no sync byte — one ASCII byte first, so every even offset is inside a syllable —
//! shown as placeholder marks past REACH until its phase checkpoints are read, each stretch
//! once, and then as the whole-file decode (EA-3 (ii)); wrapped rows, restarts and places not
//! wrapped on character starts of the whole-file decode (EA-7).

use super::*;
use crate::encoding::Encoding::Cp949;

/// `text` in CP949, by encoding_rs's encoder (windows-949).
pub(in crate::gui::text) fn cp949(text: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding_rs::EUC_KR.encode(text);
    assert!(!unmappable, "{text}");
    bytes.into_owned()
}

/// `n` KS X 1001 syllables in a row, as CP949 bytes: no sync byte among them. (A few, over
/// and over: each new glyph costs a test window a rasterization.)
pub(in crate::gui::text) fn syllables(n: usize) -> Vec<u8> {
    let five = cp949("가갊한글나");
    five.chunks(2).cycle().take(n).flatten().copied().collect()
}

/// Korean CRLF lines (갊 = `b0 a7` among them) as CP949, and the lines.
pub(in crate::gui::text) fn korean_lines(n: usize) -> (Vec<u8>, Vec<String>) {
    let lines: Vec<String> = (0..n)
        .map(|i| match i % 7 {
            0 => format!("{i:05} 갊 사이 ascii {i}"),
            _ => format!("{i:05} 한글과 ASCII 줄입니다"),
        })
        .collect();
    let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
    (cp949(&text), lines)
}

/// The whole-file decode of one line of a file, `line..end`: where each character starts, and
/// the characters as painted.
pub(in crate::gui::text) struct Whole {
    line: u64,
    end: u64,
    starts: Vec<u64>,
    chars: Vec<char>,
}

impl Whole {
    pub(in crate::gui::text) fn of(body: &[u8], line: u64, end: u64) -> Self {
        let mut starts = Vec::new();
        let mut chars = Vec::new();
        let mut at = line;
        for c in Cp949.decode(&body[line as usize..end as usize], true).0 {
            starts.push(at);
            chars.push(wrap::face(c.ch));
            at += u64::from(c.len);
        }
        starts.push(at);
        Self {
            line,
            end,
            starts,
            chars,
        }
    }
}

/// The rows of the page on screen inside `whole`'s line: marks only before characters, one a
/// byte, dim; the characters the whole-file decode's from a character start of it (one-byte
/// units: the first comes right after the marks). How many rows have marks.
pub(in crate::gui::text) fn check_rows(win: &TextWindow, whole: &Whole, what: &str) -> usize {
    let page = win.page.as_ref().expect("a page");
    let mut marked = 0;
    for row in &page.rows {
        let at = row.at + row.start;
        if at < whole.line || at >= whole.end {
            continue;
        }
        let shown: Vec<char> = row.cut.text().chars().collect();
        let dim = row.cut.dim;
        assert!(
            shown[..dim].iter().all(|&c| c == wrap::MARK),
            "{what}: a row at {at}"
        );
        assert!(!shown[dim..].contains(&wrap::MARK), "{what}: a row at {at}");
        marked += usize::from(dim > 0);
        if dim == shown.len() {
            continue;
        }
        let from = at + dim as u64;
        let k = whole
            .starts
            .binary_search(&from)
            .unwrap_or_else(|_| panic!("{what}: a row at {at}: {from} is no character start"));
        let want: String = whole.chars[k..].iter().take(shown.len() - dim).collect();
        let got: String = shown[dim..].iter().collect();
        assert_eq!(got, want, "{what}: a row at {at}");
    }
    marked
}

/// Lets the next checkpoint read's worker go only when the returned sender is used.
fn held_phases(win: &mut TextWindow) -> mpsc::Sender<()> {
    let (go, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    win.phase_hook = Some(Arc::new(move || {
        let _ = wait.lock().unwrap().recv();
    }));
    go
}

/// The top row's bytes.
fn top(win: &TextWindow) -> (u64, u64) {
    let row = &win.page.as_ref().unwrap().rows[0];
    let from = row.at + row.start;
    (from, from + row.cut.bytes as u64)
}

/// **EA-3** (VM-4), locally: Korean CP949 with CRLF (갊 = `b0 a7`) as a `.txt` → `CP949
/// (guessed)`, every line encoding_rs's whole-file decode; set to UTF-8 → U+FFFD shown, the line
/// count the same, no index pass; back → `CP949 (guessed)`. The bar says it beside the syntax.
#[test]
fn ea3_a_cp949_txt_is_guessed_and_reads_as_encoding_rs_decodes() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, lines) = korean_lines(3000);
    assert!(body.windows(2).any(|w| w == [0xB0, 0xA7]), "갊 is b0 a7");
    let path = write(tmp.path(), "notes.txt", &body);
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.encoding_label().as_deref(), Some("CP949 (guessed)"));
    assert_eq!(doc.line_count(), 3000);
    assert_eq!(all_lines(&mut doc), lines);
    let decoded = encoding_rs::EUC_KR.decode_without_bom_handling(&body).0;
    assert_eq!(all_lines(&mut doc).join("\r\n") + "\r\n", decoded);
    let passes = index_reads(&path).len();
    assert!(!doc.needs_index_for(Utf8));
    doc.reencode(Utf8);
    assert_eq!(doc.line_count(), 3000);
    assert_eq!(doc.encoding_label().as_deref(), Some("UTF-8"));
    let utf8 = all_lines(&mut doc);
    assert!(utf8.iter().all(|l| l.contains('\u{FFFD}')), "{}", utf8[0]);
    doc.reencode(Cp949);
    assert_eq!(doc.encoding_label().as_deref(), Some("CP949 (guessed)"));
    assert_eq!(all_lines(&mut doc), lines);
    assert_eq!(
        index_reads(&path).len(),
        passes,
        "no pass: the same LF byte"
    );
    // On screen.
    let mut win = TextWindow::new(doc, "box:notes".into(), "notes.txt", vec![]);
    let mut drv = Driver::new(900.0, 500.0);
    drv.until(&mut win, 10, |w| w.ready());
    drv.idle(&mut win, 1);
    let bar = drv.texts();
    assert!(
        bar.iter()
            .any(|(_, _, t)| t.contains("plain text · CP949 (guessed)")),
        "{bar:?}"
    );
    for line in &lines[..5] {
        assert!(drv.find_text(line).is_some(), "{line}");
    }
}

/// **EA-3** (VM-4), the model's part of "an extension-less CP949 file → Hex; 3 → Text": the
/// sniff does not route it to text (step 3 does not route: D18), and its text document is
/// `CP949 (guessed)` (view 3's binding is VM-7's).
#[test]
fn ea3_an_extension_less_cp949_file_stays_other_and_decodes_as_cp949() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, lines) = korean_lines(400);
    let size = body.len() as u64;
    let head = body[..crate::kind::SNIFF_BYTES].to_vec();
    let kind = crate::kind::classify("notes", || head, size);
    assert_eq!(kind, crate::kind::Kind::Other);
    let path = write(tmp.path(), "notes", &body);
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.encoding_label().as_deref(), Some("CP949 (guessed)"));
    assert_eq!(all_lines(&mut doc), lines);
}

/// `SOPEN_STATE` `enc=CP949` names it, as any other encoding of §7.1 (VM-4 decodes it).
#[test]
fn sopen_state_enc_names_cp949() {
    use crate::gui::State;
    assert_eq!(State::parse("enc=CP949").unwrap().enc, Some(Cp949));
    assert_eq!(State::parse("enc=cp949").unwrap().enc, Some(Cp949));
}

/// A line of `a` and then 256 KiB of syllables with no sync byte, then a short line.
fn long_line() -> (Vec<u8>, u64) {
    let mut body = vec![b'a'];
    body.extend(syllables(128 * 1024));
    let end = body.len() as u64;
    body.push(b'\n');
    body.extend(cp949("끝\n"));
    (body, end)
}

/// **EA-3 (ii)** (VM-4), locally: inside the index, a 256 KiB line of Hangul with no sync byte
/// (longer than REACH + GRID). Go to 100 KiB past its start → placeholder marks and the status
/// line, while the line's checkpoint read runs (`ready` waits for it); once it ends — having
/// read the line's first six GRID points, less than 100 KiB of it — rows equal to the whole-file
/// decode, the row holding the place at the top. A place past the seventh GRID point reads on
/// from the sixth: no stretch twice.
#[test]
fn ea3_ii_a_long_line_with_no_sync_byte_waits_for_its_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, end) = long_line();
    let whole = Whole::of(&body, 0, end);
    let path = write(tmp.path(), "long.txt", &body);
    let doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.encoding_label().as_deref(), Some("CP949 (guessed)"));
    let mut win = TextWindow::new(doc, "box:long".into(), "long.txt", vec![]);
    let go = held_phases(&mut win);
    let mut drv = Driver::new(500.0, 200.0);
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(check_rows(&win, &whole, "the first page"), 0);
    assert!(
        win.phase_read.is_none(),
        "within REACH the line start proves it"
    );
    let place = 100 * 1024;
    win.jump_to(Jump::Byte(place));
    drv.idle(&mut win, 3);
    let read = win.phase_read.as_ref().expect("a checkpoint read");
    assert_eq!((read.line, read.from), (0, 0));
    assert!(!win.ready(), "the page waits for its checkpoints");
    let marked = check_rows(&win, &whole, "before the checkpoints");
    let rows = win.page.as_ref().unwrap().rows.len();
    assert_eq!(
        marked, rows,
        "every row is marks: no sync byte, no checkpoint yet"
    );
    assert!(
        drv.texts()
            .iter()
            .any(|(_, _, t)| t.contains("CP949: no character boundary known here")),
        "{:?}",
        drv.texts()
    );
    let _ = go.send(());
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(check_rows(&win, &whole, "with the checkpoints"), 0);
    let (from, to) = top(&win);
    assert!(from <= place && place < to, "{place} on top: {from}..{to}");
    assert_eq!(
        win.doc.phases.lines[&0].len(),
        6,
        "read to the sixth GRID point"
    );
    assert!(
        win.doc.phases.lines[&0].iter().all(|&bit| bit),
        "each inside a syllable"
    );
    // Past the seventh GRID point: read on from the sixth.
    let further = 7 * GRID + 1000;
    win.jump_to(Jump::Byte(further));
    drv.idle(&mut win, 3);
    let read = win.phase_read.as_ref().expect("a checkpoint read");
    assert_eq!((read.line, read.from), (0, 6));
    let _ = go.send(());
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(check_rows(&win, &whole, "further on"), 0);
    assert_eq!(win.doc.phases.lines[&0].len(), 7);
    let (from, to) = top(&win);
    assert!(from <= further && further < to, "{further}: {from}..{to}");
    // Paging on and back over restarts and GRID points inside syllables.
    win.phase_hook = None;
    for (key, n) in [(Key::PageDown, 3), (Key::PageUp, 6)] {
        for i in 0..n {
            drv.key(&mut win, key, Modifiers::NONE);
            drv.until(&mut win, 10, |w| w.ready());
            assert_eq!(check_rows(&win, &whole, &format!("{key:?} {i}")), 0);
        }
    }
}

/// **EA-7** (VM-4), locally: a CP949 line that starts with one ASCII byte, not wrapped — Right
/// ×1–3, a one-byte sideways place within REACH and past it, End — and wrapped rows across the
/// restarts inside syllables: every row starts at a character start of the whole-file decode and
/// shows its characters; the row before a restart never shows a U+FFFD the decode lacks.
#[test]
fn ea7_cp949_rows_and_places_not_wrapped_are_character_starts() {
    let tmp = tempfile::tempdir().unwrap();
    // With spaces too: the restarts' windows have sync bytes, the line start is a long way back.
    let mut spaced = vec![b'a'];
    for i in 0..2000 {
        spaced.extend(syllables(20 + i % 13));
        spaced.push(b' ');
    }
    let spaced_end = spaced.len() as u64;
    spaced.push(b'\n');
    let (bare, bare_end) = long_line();
    for (name, body, end) in [("spaced", spaced, spaced_end), ("bare", bare, bare_end)] {
        let whole = Whole::of(&body, 0, end);
        let path = write(tmp.path(), &format!("{name}.txt"), &body);
        let doc = TextDoc::open(&path).unwrap();
        assert_eq!(doc.encoding(), Cp949, "{name}");
        let mut win = TextWindow::new(doc, "box:k".into(), "k.txt", vec![]);
        let mut drv = Driver::new(500.0, 200.0);
        drv.until(&mut win, 10, |w| w.ready());
        // Wrapped: at restarts within REACH and past it (each inside a syllable in the bare
        // line), the rows after one, and those before it going back up.
        for k in [1, 4, 5, 6] {
            let at = k * wrap::RESTART + 3;
            win.jump_to(Jump::Byte(at));
            drv.idle(&mut win, 1);
            drv.until(&mut win, 10, |w| w.ready());
            let what = format!("{name}: restart {k}");
            assert_eq!(check_rows(&win, &whole, &what), 0, "{what}");
            let (from, to) = top(&win);
            assert!(from <= at && at < to, "{what}: {at} on top: {from}..{to}");
            for key in [Key::PageUp, Key::PageDown, Key::PageDown] {
                drv.key(&mut win, key, Modifiers::NONE);
                drv.until(&mut win, 10, |w| w.ready());
                let what = format!("{name}: restart {k}, {key:?}");
                assert_eq!(check_rows(&win, &whole, &what), 0, "{what}");
            }
        }
        // Not wrapped.
        drv.key(&mut win, Key::W, Modifiers::NONE);
        drv.key(&mut win, Key::Home, crate::gui::testkit::command(false));
        drv.until(&mut win, 10, |w| w.ready());
        for i in 0..3 {
            drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
            drv.until(&mut win, 10, |w| w.ready());
            let what = format!("{name}: Right {i}");
            assert_eq!(check_rows(&win, &whole, &what), 0, "{what}");
        }
        for left in [1002, 1003, 3 * GRID + 2, 100 * 1024 + 2, 100 * 1024 + 3] {
            win.left = left;
            win.stale_page = true;
            drv.idle(&mut win, 1);
            drv.until(&mut win, 10, |w| w.ready());
            let what = format!("{name}: left {left}");
            assert_eq!(check_rows(&win, &whole, &what), 0, "{what}");
            let (from, _) = top(&win);
            assert!(from >= left && from <= left + 1, "{what}: from {from}");
        }
        drv.key(&mut win, Key::End, Modifiers::NONE);
        drv.until(&mut win, 10, |w| w.ready());
        assert_eq!(check_rows(&win, &whole, &format!("{name}: End")), 0);
    }
}

/// MF-2 (review of VM-4): a checkpoint read under way when the document is made again (here a
/// rebuild into another LF unit) is stopped at once — its cancel flag set, nothing installed
/// into the new document — not left to end on its own.
#[test]
fn a_checkpoint_read_stops_when_the_document_is_made_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, _) = long_line();
    let path = write(tmp.path(), "long.txt", &body);
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:l".into(),
        "l.txt",
        vec![],
    );
    let go = held_phases(&mut win);
    let mut drv = Driver::new(500.0, 200.0);
    drv.until(&mut win, 10, |w| w.ready());
    win.jump_to(Jump::Byte(100 * 1024));
    drv.idle(&mut win, 3);
    let cancel = win
        .phase_read
        .as_ref()
        .expect("a checkpoint read")
        .cancel
        .clone();
    win.enc_wanted = Some(Utf16Le);
    drv.until(&mut win, 10, |w| w.doc.encoding() == Utf16Le);
    drv.idle(&mut win, 1);
    assert!(cancel.load(Ordering::Relaxed), "the read was told to stop");
    assert!(win.phase_read.is_none());
    let _ = go.send(());
    drv.until(&mut win, 10, |w| w.ready());
    assert!(win.doc.phases.lines.is_empty());
}

/// MF-2: a change of encoding in place starts the checkpoints afresh (they are CP949's: a new
/// epoch, nothing kept, nothing asked); a read for another encoding's document never makes any.
#[test]
fn reencoding_drops_the_checkpoints_and_a_read_needs_cp949() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, _) = long_line();
    let path = write(tmp.path(), "long.txt", &body);
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.encoding(), Cp949);
    doc.phases.install(0, 0, vec![true; 4]);
    doc.phases.wanted = Some((0, 6));
    let epoch = doc.phases.epoch;
    doc.reencode(Latin1);
    assert!(doc.phases.lines.is_empty() && doc.phases.wanted.is_none());
    assert_ne!(doc.phases.epoch, epoch);
    let mut copy = doc.share().unwrap();
    assert!(copy.phases_of(0, &[], 4, &AtomicBool::new(false)).is_err());
}

/// MF-2, end to end: a preview asks for a long line's checkpoints (100 KiB in, no sync byte)
/// and reads none; it is set to Latin-1, handed over, then set to CP949 again: the rows are the
/// whole-file decode — no checkpoint read under Latin-1 (all "no lead pending") is made, or
/// trusted.
#[test]
fn a_latin1_round_trip_never_trusts_checkpoints_made_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let (body, end) = long_line();
    let whole = Whole::of(&body, 0, end);
    let path = write(tmp.path(), "long.txt", &body);
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:l".into(),
        "l.txt",
        vec![],
    );
    Window::set_preview(&mut win, true);
    let mut drv = Driver::new(500.0, 200.0);
    win.jump_to(Jump::Byte(100 * 1024));
    drv.until(&mut win, 10, |w| w.ready() && w.page.is_some());
    assert!(win.doc.phases.wanted.is_some() && win.phase_read.is_none());
    win.enc_wanted = Some(Latin1);
    drv.idle(&mut win, 2);
    assert_eq!(win.doc.encoding(), Latin1);
    Window::set_preview(&mut win, false);
    drv.idle(&mut win, 2);
    drv.until(&mut win, 10, |w| w.ready());
    win.enc_wanted = Some(Cp949);
    drv.idle(&mut win, 1);
    drv.until(&mut win, 10, |w| w.ready() && w.doc.encoding() == Cp949);
    assert_eq!(check_rows(&win, &whole, "back in CP949"), 0);
}

/// MF-3: while the checkpoints are read, a restart nothing proves is the point itself; the row
/// before it never decodes the lead pending there as a U+FFFD the whole decode lacks.
#[test]
fn the_row_before_a_restart_nothing_proves_shows_no_wrong_character() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = vec![b'a'];
    body.extend(syllables(64 * 1024));
    // A sync byte 100 bytes before the fifth restart: the rows near it are proven, the restart
    // (whose window starts 64 bytes before it) is not.
    let space = 5 * wrap::RESTART as usize - 100;
    body[space] = b' ';
    let end = body.len() as u64;
    body.push(b'\n');
    let whole = Whole::of(&body, 0, end);
    let path = write(tmp.path(), "r.txt", &body);
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:r".into(),
        "r.txt",
        vec![],
    );
    let go = held_phases(&mut win);
    let mut drv = Driver::new(500.0, 200.0);
    drv.until(&mut win, 10, |w| w.ready());
    win.jump_to(Jump::Byte(5 * wrap::RESTART - 50));
    drv.idle(&mut win, 3);
    assert!(win.phase_read.is_some(), "the checkpoints are on their way");
    check_rows(&win, &whole, "held");
    let _ = go.send(());
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(check_rows(&win, &whole, "after"), 0);
}

/// MF-4: more long lines with no sync byte on one page than a document keeps checkpoints for
/// (not wrapped, each 70 KiB in): every line on the page gets its checkpoints once — none the
/// page asked for is forgotten to make room — so the reads end and the page is ready.
#[test]
fn more_long_lines_on_a_page_than_the_checkpoint_room_still_settle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = Vec::new();
    let mut lines = Vec::new();
    for _ in 0..100 {
        let start = body.len() as u64;
        body.push(b'a');
        body.extend(syllables(40 * 1024));
        lines.push((start, body.len() as u64));
        body.push(b'\n');
    }
    let path = write(tmp.path(), "many.txt", &body);
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:m".into(),
        "m.txt",
        vec![],
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    win.phase_hook = Some(Arc::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    }));
    let mut drv = Driver::new(500.0, 1500.0);
    drv.until(&mut win, 10, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.idle(&mut win, 1);
    win.left = 70 * 1024;
    win.stale_page = true;
    drv.idle(&mut win, 1);
    drv.until(&mut win, 60, |w| w.ready());
    let rows = win.page.as_ref().unwrap().rows.len();
    assert!(rows > PHASE_LINES, "{rows} rows");
    assert!(
        reads.load(Ordering::Relaxed) <= rows + 2,
        "{} reads",
        reads.load(Ordering::Relaxed)
    );
    for &(s, e) in &lines[..rows] {
        let whole = Whole::of(&body, s, e);
        assert_eq!(check_rows(&win, &whole, &format!("line at {s}")), 0);
    }
}
