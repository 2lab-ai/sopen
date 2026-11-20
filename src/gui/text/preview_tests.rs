//! The text viewer as a preview (02 §4.2–§4.4): told it is one (`set_preview(true)`), it takes
//! no input of its own — no key, wheel, click or drag moves it, even with nothing around it
//! disabling it or withholding events — draws without its bars in a single-lined frame, neither
//! follows nor re-reads its file, and stops its index after the preview's bytes; handed over
//! (`set_preview(false)`), it is the viewer again and its index goes on from where it stopped,
//! reading no byte of the file twice.

use super::*;
use crate::gui::chrome;
use crate::gui::testkit::{command, Driver};
use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2};
use std::sync::Mutex;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn rows(n: u32) -> String {
    (1..=n).map(|i| format!("row {i}\n")).collect()
}

/// The viewer on a local file, told it is a preview before its first pass (as the browser's
/// preview host does it).
fn previewed(dir: &Path, name: &str, body: &str) -> TextWindow {
    let path = write(dir, name, body.as_bytes());
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        path.display().to_string(),
        name,
        Vec::new(),
    );
    win.set_preview(true);
    win
}

/// What input could change in a text viewer.
fn place(win: &TextWindow) -> (u64, u64, bool, usize) {
    (win.top, win.top_off, win.wrap, win.left)
}

/// Every kind of input the owner could aim at the viewer: the wheel, the keys it knows
/// (wrapped and not), clicks, a drag, typed text and a paste.
fn every_input(drv: &mut Driver, win: &mut TextWindow) {
    let centre = Pos2::new(300.0, 200.0);
    drv.wheel(win, centre, -3.0);
    for key in [
        Key::ArrowDown,
        Key::PageDown,
        Key::End,
        Key::ArrowRight,
        Key::W,
        Key::F2,
        Key::Space,
    ] {
        drv.key(win, key, Modifiers::NONE);
    }
    drv.key(win, Key::End, command(false));
    drv.click(win, centre);
    drv.drag(
        win,
        PointerButton::Primary,
        Pos2::new(200.0, 100.0),
        Pos2::new(400.0, 300.0),
    );
    drv.frame(win, vec![Event::Text("ab".into())], Modifiers::NONE);
    drv.paste_text(win, "pasted");
    drv.idle(win, 2);
}

/// P5 (text): no key, wheel, click, drag, typing or paste moves a previewed viewer — on its own,
/// with nothing disabling it or withholding the events (02 §4.4: the viewer must not rely on
/// the host for that; E3 measured the wheel and W acting through `ui.disable()`).
#[test]
fn a_previewed_text_viewer_takes_no_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = previewed(tmp.path(), "rows.txt", &rows(500));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(drv.find_text("row 1").is_some());
    let before = place(&win);
    every_input(&mut drv, &mut win);
    assert_eq!(place(&win), before, "the preview moved");
    assert!(drv.find_text("row 1").is_some(), "the first page stays");
    assert!(win.close_requested(), "dropping it asks nothing");
}

/// P5 (text): a preview draws its lines inside its own frame, single-lined (inactive), without
/// the path row above them or the status row below.
#[test]
fn a_previewed_text_viewer_has_no_bars_and_an_inactive_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = previewed(tmp.path(), "rows.txt", &rows(500));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(drv.find_text("row 1").is_some());
    assert_eq!(
        drv.text_starting_with("lines "),
        None,
        "no status row in a preview"
    );
    assert_eq!(
        drv.text_starting_with("500 lines"),
        None,
        "no path row in a preview"
    );
    assert_eq!(
        drv.colors_of(TITLE),
        Some(vec![chrome::PALETTE.frame_idle.color]),
        "the frame is the inactive one"
    );
}

/// No following (02 §4.2): a preview at the end of a local file never asks to look at it again,
/// and a file that grows under it is not re-read while it is a preview.
#[test]
fn a_previewed_text_viewer_neither_follows_nor_rereads() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = previewed(tmp.path(), "tail.log", &rows(3));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(
        drv.find_text("row 3").is_some(),
        "the whole file is on screen"
    );
    let before = follows();
    drv.idle(&mut win, 3);
    assert_eq!(follows(), before, "a preview at the end is not followed");
    let path = tmp.path().join("tail.log");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut f, rows(9).as_bytes()).unwrap();
    win.clock = Box::new(|| std::time::Instant::now() + std::time::Duration::from_secs(60));
    drv.idle(&mut win, 5);
    assert!(
        win.reindex.is_none() && win.reindexes == 0,
        "a preview never re-reads its file"
    );
    assert!(drv.find_text("row 3").is_some(), "its page stays");
    assert!(
        win.ready(),
        "a preview frozen on a changed file is still ready"
    );
}

/// The hand-over (02 §4.4): `set_preview(false)` turns the bars, the active frame and the keys
/// back on.
#[test]
fn a_handed_over_text_viewer_takes_input_and_shows_its_bars() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = previewed(tmp.path(), "rows.txt", &rows(500));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    win.set_preview(false);
    drv.idle(&mut win, 2);
    drv.until(&mut win, 5, |w| w.ready());
    let texts: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
    assert!(
        drv.text_starting_with("lines 1").is_some(),
        "the status row: {texts:?}"
    );
    assert_eq!(
        drv.colors_of(TITLE),
        Some(vec![chrome::PALETTE.frame_active.color])
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    assert!(drv.find_text("row 500").is_some(), "End acts again");
}

/// A file of `n` numbered lines of 64 bytes (a MiB is 16 384 of them), with a few lines over [`LINE_MAX`] and CRLF ends
/// mixed in when `odd`.
fn body_of(n: usize, odd: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..n {
        if odd && i % 9_000 == 4_500 {
            out.extend(std::iter::repeat_n(b'x', LINE_MAX * 3));
            out.push(b'\n');
            continue;
        }
        let line = format!("{i:08} {}", "y".repeat(54));
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(if odd && i % 3 == 0 { b"\r\n" } else { b"\n" });
    }
    out
}

/// What an index says about a file: what a resumed index must equal, field by field.
fn index_of(doc: &TextDoc) -> (u64, u64, Vec<Mark>, Option<u64>, bool) {
    (
        doc.size,
        doc.lines,
        doc.marks.clone(),
        doc.first_long,
        doc.ends_newline,
    )
}

/// The index pass's reads of `path`, in order, merged where they touch: one range per run.
fn read_runs(path: &Path) -> Vec<(u64, u64)> {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for (at, n) in index_reads(path) {
        match runs.last_mut() {
            Some((start, len)) if *start + *len == at => *len += n,
            _ => runs.push((at, n)),
        }
    }
    runs
}

/// An index cut at `cap` bytes (the preview's 4 MiB, smaller here) and resumed is exactly the
/// index of one whole pass — wherever the cut falls: inside a line, right after a newline,
/// inside a line over [`LINE_MAX`], between `\r` and `\n` — and the pass and its resume read
/// every byte once.
#[test]
fn a_resumed_index_is_the_whole_files_index() {
    let tmp = tempfile::tempdir().unwrap();
    let body = body_of(70_000, true);
    let path = write(tmp.path(), "mixed.log", &body);
    let whole = index_of(&TextDoc::open(&path).unwrap());
    let newline = body.iter().position(|&b| b == b'\n').unwrap() as u64;
    let long = body.windows(8).position(|w| w == b"xxxxxxxx").unwrap() as u64 + 100;
    let cr = body.iter().position(|&b| b == b'\r').unwrap() as u64 + 1;
    for cap in [1 << 20, 3 * (1 << 20) + 17, newline + 1, long, cr] {
        let name = format!("cut-{cap}.log");
        let path = write(tmp.path(), &name, &body);
        let never = AtomicBool::new(false);
        let cut = TextDoc::open_capped(&path, &|| Some(cap), &|| Ok(())).unwrap();
        assert_eq!(cut.size(), cap, "cut at {cap}");
        assert!(cut.cut_short(), "cut at {cap}");
        let resumed = cut.resume(&never).unwrap();
        assert!(!resumed.cut_short());
        assert_eq!(index_of(&resumed), whole, "cut at {cap}");
        assert_eq!(
            read_runs(&path),
            vec![(0, body.len() as u64)],
            "every byte read once (cut at {cap})"
        );
    }
}

/// P12 (text, local): a 6 MiB file previewed shows its first page and, as its last row,
/// `preview of the first 4 MiB`, having read only those 4 MiB; handed over, the index goes on
/// from 4 MiB (the window is woken when it lands, nothing waits for input) and End shows the
/// file's last line — every byte of the file read by the index once.
#[test]
fn a_handed_over_preview_indexes_the_rest_without_reading_a_byte_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let body = body_of(6 * 16_384, false);
    assert_eq!(body.len(), 6 << 20);
    let path = write(tmp.path(), "six.log", &body);
    let doc = TextDoc::open_capped(&path, &|| Some(4 << 20), &|| Ok(())).unwrap();
    assert_eq!(
        read_runs(&path),
        vec![(0, 4 << 20)],
        "the preview read 4 MiB"
    );
    let mut win = TextWindow::new(doc, path.display().to_string(), "six.log", Vec::new());
    win.set_preview(true);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(
        drv.text_starting_with("00000000 ").is_some(),
        "the first page"
    );
    assert!(
        drv.find_text("preview of the first 4 MiB").is_some(),
        "its last row says how much it shows"
    );
    let woken = Arc::new(Mutex::new(0usize));
    let seen = woken.clone();
    drv.ctx
        .set_repaint_observer(move |_| *seen.lock().unwrap() += 1);
    win.set_preview(false);
    drv.idle(&mut win, 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while win.doc.cut_short() || win.reindex.is_some() {
        // Only passes the window asked for: the landing must wake it by itself.
        let asked = std::mem::take(&mut *woken.lock().unwrap());
        assert!(
            std::time::Instant::now() < deadline,
            "the rest never landed"
        );
        if asked > 0 {
            drv.idle(&mut win, 1);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(win.doc.line_count(), 6 * 16_384);
    assert_eq!(
        read_runs(&path),
        vec![(0, 6 << 20)],
        "each byte indexed once"
    );
    // Apart from the index: the 4 MiB before the cut read again to check them (02 §4.4), before
    // the rest was indexed and after.
    let checked: u64 = verify_reads(&path).iter().map(|r| r.1).sum();
    assert_eq!(checked, 2 * (4 << 20), "verification reads, counted apart");
    assert!(drv.find_text("preview of the first 4 MiB").is_none());
    drv.key(&mut win, Key::End, command(false));
    assert!(
        drv.text_starting_with(&format!("{:08} ", 6 * 16_384 - 1))
            .is_some(),
        "End shows the file's last line"
    );
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
}

/// P3 on a log being written: a file appended to after its preview's pass stopped at the budget
/// reads as grown, not changed — the bytes indexed are still the file's first bytes — so the
/// preview shows its first page and its last row (it is not left blank, waiting for a re-read it
/// never does), and the hand-over goes on from where the pass stopped, not from the start: the
/// bytes before the cut, written to since, are read once more to check them (their hash), never
/// indexed again.
#[test]
fn a_cut_short_preview_of_a_growing_log_shows_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let body = body_of(3 * 16_384, false);
    let path = write(tmp.path(), "busy.log", &body);
    let doc = TextDoc::open_capped(&path, &|| Some(1 << 20), &|| Ok(())).unwrap();
    append(&path, b"written before the window opened\n");
    let mut win = TextWindow::new(doc, path.display().to_string(), "busy.log", Vec::new());
    win.set_preview(true);
    // The test's clock never moves: no re-read of the grown file becomes due.
    let now = std::time::Instant::now();
    win.clock = Box::new(move || now);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(
        drv.text_starting_with("00000000 ").is_some(),
        "the first page"
    );
    assert!(drv.find_text("preview of the first 1 MiB").is_some());
    append(&path, b"written while it was shown\n");
    drv.idle(&mut win, 3);
    assert!(drv.text_starting_with("00000000 ").is_some(), "still shown");
    win.set_preview(false);
    drv.until(&mut win, 10, |w| {
        !w.doc.cut_short() && w.reindex.is_none() && w.ready()
    });
    assert_eq!(
        win.doc.line_count(),
        3 * 16_384,
        "the file as it was opened"
    );
    assert_eq!(
        read_runs(&path),
        vec![(0, body.len() as u64)],
        "resumed: nothing indexed twice"
    );
}

/// 04 §3: F2 switches wrapping with no modifier only; Shift+F2 and Alt+F2 leave it.
#[test]
fn f2_switches_wrapping_only_without_modifiers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "rows.txt", rows(50).as_bytes());
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        path.display().to_string(),
        "rows.txt",
        Vec::new(),
    );
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    for mods in [Modifiers::SHIFT, Modifiers::ALT] {
        drv.key(&mut win, Key::F2, mods);
        assert!(win.wrap, "{mods:?}+F2 left wrapping on");
    }
    drv.key(&mut win, Key::F2, Modifiers::NONE);
    assert!(!win.wrap, "F2 alone switches it");
}

/// `n` numbered lines of 32 bytes: other newline positions than [`body_of`]'s.
fn short_lines(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("{i:08} {}\n", "s".repeat(22)).into_bytes())
        .collect()
}

/// How a file is rewritten under a preview whose pass stopped at its budget.
#[derive(Debug, Clone, Copy)]
enum Rewrite {
    /// Written over in place at the same size, the newlines elsewhere.
    SameSize,
    /// Written over in place, longer.
    Larger,
    /// Truncated, then written again, longer.
    TruncatedAndRegrown,
    /// Written over in place in the middle of the bytes the preview indexed only — its first
    /// and last 4 KiB as they were, the size the same — the newlines elsewhere.
    MiddleOnly,
    /// As `MiddleOnly`, and its modification time put back as it was: nothing a stat shows
    /// tells it was written to.
    MiddleOnlyMtimeKept,
}

/// Opens a 3 MiB file capped at 1 MiB as a preview, rewrites it as `how` says (the inode and a
/// size no shorter than before: it looks grown), and hands it over. Returns the window and the
/// clock the test moves.
fn rewritten_under_a_preview(
    dir: &Path,
    how: Rewrite,
) -> (TextWindow, Driver, Arc<Mutex<std::time::Instant>>) {
    use std::io::{Seek, Write as _};
    let name = format!("{how:?}.log");
    let path = write(dir, &name, &body_of(3 * 16_384, false));
    let doc = TextDoc::open_capped(&path, &|| Some(1 << 20), &|| Ok(())).unwrap();
    let mut win = TextWindow::new(doc, path.display().to_string(), &name, Vec::new());
    win.set_preview(true);
    let clock = Arc::new(Mutex::new(std::time::Instant::now()));
    let now = clock.clone();
    win.clock = Box::new(move || *now.lock().unwrap());
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let (at, bytes) = match how {
        Rewrite::SameSize => (0, short_lines(6 * 16_384)),
        Rewrite::Larger | Rewrite::TruncatedAndRegrown => (0, short_lines(8 * 16_384)),
        // 8 KiB in, up to 8 KiB before the cut: 32-byte lines where there were 64-byte ones.
        Rewrite::MiddleOnly | Rewrite::MiddleOnlyMtimeKept => {
            (8 << 10, short_lines(((1 << 20) - (16 << 10)) / 32))
        }
    };
    if matches!(how, Rewrite::TruncatedAndRegrown) {
        f.set_len(0).unwrap();
    }
    f.seek(std::io::SeekFrom::Start(at)).unwrap();
    f.write_all(&bytes).unwrap();
    if matches!(how, Rewrite::MiddleOnlyMtimeKept) {
        f.set_modified(mtime).unwrap();
    }
    drop(f);
    win.set_preview(false);
    (win, drv, clock)
}

/// The hand-over never joins the marks the preview found to bytes written since: a file
/// rewritten in place — at the same size with its newlines elsewhere (also only in the middle of
/// what the preview read), longer, or truncated and written again — is read again whole (after
/// the usual wait) instead of resumed; what is installed is the new file's own index.
#[test]
fn a_rewritten_file_is_read_again_whole_not_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    for how in [
        Rewrite::SameSize,
        Rewrite::Larger,
        Rewrite::TruncatedAndRegrown,
        Rewrite::MiddleOnly,
        Rewrite::MiddleOnlyMtimeKept,
    ] {
        let (mut win, mut drv, clock) = rewritten_under_a_preview(tmp.path(), how);
        drv.until(&mut win, 10, |w| {
            w.reindex.is_none() && (w.waiting || !w.doc.cut_short())
        });
        assert!(
            win.doc.cut_short() && win.waiting,
            "{how:?}: a resumed index of the rewritten file was installed"
        );
        *clock.lock().unwrap() += std::time::Duration::from_secs(60);
        drv.until(&mut win, 10, |w| {
            !w.doc.cut_short() && w.reindex.is_none() && !w.waiting && w.ready()
        });
        let path = win.doc.path().to_path_buf();
        assert_eq!(
            index_of(&win.doc),
            index_of(&TextDoc::open(&path).unwrap()),
            "{how:?}: the new file's own index"
        );
    }
}

/// The bytes before the cut are checked again after the resumed pass too: written over while
/// the rest is being indexed, they fail it, and the resume is Stale (the file is read again
/// whole) — no index joining the old marks to the new bytes comes out of it.
#[test]
fn a_prefix_rewritten_during_the_resume_fails_it() {
    use std::io::{Seek, Write as _};
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "during.log", &body_of(3 * 16_384, false));
    let cut = TextDoc::open_capped(&path, &|| Some(1 << 20), &|| Ok(())).unwrap();
    let mut written = false;
    let resumed = cut.resume_counted(&AtomicBool::new(false), &mut |_| {
        if !written {
            written = true;
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(std::io::SeekFrom::Start(100 << 10)).unwrap();
            f.write_all(&short_lines(1000)).unwrap();
        }
    });
    assert!(written, "rewritten mid-continuation");
    match resumed {
        Err(e) => assert!(is_stale(&e), "{e:#}"),
        Ok(doc) => panic!(
            "a resumed index was made of a rewritten prefix ({} lines)",
            doc.line_count()
        ),
    }
}
