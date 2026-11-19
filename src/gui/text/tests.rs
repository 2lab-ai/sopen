//! The text viewer: paging reads only the page on screen (a sparse line index, never the whole
//! file), long lines and bad bytes are shown without breaking, and the window highlights, numbers
//! and pages through real key events.

use super::*;
use crate::gui::chrome;
use crate::gui::testkit::Driver;
use eframe::egui::{Key, Modifiers};
use std::sync::Mutex;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// 300 000 numbered lines (7.5 MB): a page at the end reads that page and the bytes before it
/// back to the nearest index mark — not the file — and the index holds a mark per block.
#[test]
fn a_page_deep_in_a_large_file_reads_only_that_page() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for i in 0..300_000u32 {
        body.push_str(&format!("line {i:06} of a long log\n"));
    }
    let path = write(tmp.path(), "big.log", body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 300_000);
    assert!(doc.marks() <= 300_000 / 256 + 2, "{} marks", doc.marks());
    let before = doc.page_reads();
    let page = doc.read_lines(299_960, 40).unwrap();
    let read = doc.page_reads() - before;
    assert_eq!(page.len(), 40);
    assert_eq!(page[0].text, "line 299960 of a long log");
    assert_eq!(page[39].text, "line 299999 of a long log");
    assert!(page.iter().all(|l| !l.cut));
    // At most one block (256 lines of 26 bytes) before the page, and the page itself.
    assert!(
        read <= 256 * 26 + 40 * 26 + 16 * 1024,
        "read {read} bytes for one page"
    );
    // Past the end: nothing.
    assert!(doc.read_lines(300_000, 10).unwrap().is_empty());
    let tail = doc.read_lines(299_995, 10).unwrap();
    assert_eq!(tail.len(), 5);
}

/// A one-line JSON file of 3 MB: one line, shown cut at LINE_MAX bytes, and showing it reads
/// about that much — not the 3 MB.
#[test]
fn a_one_line_json_of_megabytes_shows_its_first_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::from("{\"data\":\"");
    body.push_str(&"x".repeat(3 << 20));
    body.push_str("\"}");
    let path = write(tmp.path(), "blob.json", body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 1);
    let before = doc.page_reads();
    let page = doc.read_lines(0, 40).unwrap();
    assert_eq!(page.len(), 1);
    assert!(page[0].cut);
    assert!(
        page[0].text.starts_with("{\"data\":\"xxx"),
        "{}",
        &page[0].text[..20]
    );
    assert!(page[0].text.len() <= LINE_MAX);
    let read = doc.page_reads() - before;
    assert!(
        read <= LINE_MAX as u64 + 16,
        "read {read} bytes for one cut line"
    );
}

/// Bytes that are not UTF-8 are shown as U+FFFD, the lines around them as they are.
#[test]
fn invalid_utf8_is_shown_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "app.log", b"ok\n\xff\xfe bad\r\nend");
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 3);
    let texts: Vec<String> = doc
        .read_lines(0, 10)
        .unwrap()
        .into_iter()
        .map(|l| l.text)
        .collect();
    assert_eq!(texts, ["ok", "\u{FFFD}\u{FFFD} bad", "end"]);
}

/// A character whose bytes straddle a read boundary is whole; a line cut at LINE_MAX in the
/// middle of a character ends before it (no U+FFFD of our making).
#[test]
fn a_code_point_across_a_boundary_is_never_split() {
    let tmp = tempfile::tempdir().unwrap();
    // 163 lines of 100 bytes, then a line whose "한" (3 bytes) sits at offsets 16383..16386:
    // across the 16 KiB reads that look for line ends.
    let mut body = Vec::new();
    for _ in 0..163 {
        body.extend_from_slice(&[b'x'; 99]);
        body.push(b'\n');
    }
    let mut line = vec![b'y'; 83];
    line.extend_from_slice("한글".as_bytes());
    line.push(b'\n');
    body.extend_from_slice(&line);
    // A line cut at LINE_MAX in the middle of "한".
    let mut long = vec![b'z'; LINE_MAX - 1];
    long.extend_from_slice("한".as_bytes());
    long.extend_from_slice(b" tail\n");
    body.extend_from_slice(&long);
    body.extend_from_slice(b"last\n");
    let path = write(tmp.path(), "mixed.txt", &body);
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 166);
    let page = doc.read_lines(163, 3).unwrap();
    assert_eq!(page[0].text, format!("{}한글", "y".repeat(83)));
    assert!(page[1].cut);
    assert_eq!(
        page[1].text,
        "z".repeat(LINE_MAX - 1),
        "cut before the character"
    );
    assert_eq!(page[2].text, "last");
}

/// Lines of very different lengths: every line is where the index says, whatever the mix of
/// short lines and lines longer than the index's span.
#[test]
fn mixed_line_lengths_page_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    let mut want = Vec::new();
    for i in 0..2000usize {
        let len = match i % 7 {
            0 => 70_000,
            3 => 5_000,
            _ => i % 50,
        };
        let line = format!("{i}:{}", "a".repeat(len));
        body.push_str(&line);
        body.push('\n');
        want.push(line);
    }
    let path = write(tmp.path(), "mix.txt", body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 2000);
    for first in [0u64, 1, 6, 7, 255, 256, 999, 1990] {
        let page = doc.read_lines(first, 10).unwrap();
        for (k, line) in page.iter().enumerate() {
            let full = &want[first as usize + k];
            let shown = &full[..full.len().min(LINE_MAX)];
            assert_eq!(line.text, shown, "line {}", first as usize + k);
            assert_eq!(line.cut, full.len() > LINE_MAX);
        }
    }
    // A page in the middle reads at most a span before it plus the page's shown bytes.
    let before = doc.page_reads();
    doc.read_lines(1500, 10).unwrap();
    let read = doc.page_reads() - before;
    assert!(
        read <= 64 * 1024 + 10 * (LINE_MAX as u64 + 16 * 1024),
        "read {read}"
    );
}

const RUST: &str =
    "// a comment\nfn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n";

fn viewer(dir: &Path, name: &str, body: &str) -> TextWindow {
    let path = write(dir, name, body.as_bytes());
    TextWindow::new(
        TextDoc::open(&path).unwrap(),
        format!("box:{}", path.display()),
        name,
        Vec::new(),
    )
}

/// The window shows each line with its number, highlighted by the syntax its name picks.
#[test]
fn the_window_highlights_with_line_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "main.rs", RUST);
    let mut drv = Driver::new(800.0, 500.0);
    drv.until(&mut win, 5, |w| w.ready());
    let colors = drv.colors_of("fn main() {").expect("the line is shown");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
    assert!(drv.find_text("2").is_some(), "line numbers are shown");
    // Plain text has no syntax: one colour.
    let mut plain = viewer(tmp.path(), "notes.txt", "fn main() {\n");
    drv.until(&mut plain, 5, |w| w.ready());
    let colors = drv.colors_of("fn main() {").unwrap();
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert_eq!(distinct.len(), 1, "{colors:?}");
}

/// Down / PgDn / End / Home move through the file by real key events.
#[test]
fn keys_page_through_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=500).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "rows.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(drv.find_text("row 1").is_some());
    drv.key(&mut win, Key::End, Modifiers::NONE);
    assert!(
        drv.find_text("row 500").is_some(),
        "End shows the last line"
    );
    assert!(drv.find_text("row 1").is_none());
    drv.key(&mut win, Key::Home, Modifiers::NONE);
    assert!(drv.find_text("row 1").is_some(), "Home shows the first");
    let visible = win.visible;
    drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    assert_eq!(win.top, visible - 1, "a page keeps one line of context");
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    assert_eq!(win.top, visible);
    drv.key(&mut win, Key::PageUp, Modifiers::NONE);
    assert_eq!(win.top, 1);
}

/// A parser hook that stops on any line holding `MARKER` until released: it says when it got
/// there, and lets go on `go`.
fn blocking_hook() -> (ParseHook, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (reached_tx, reached) = mpsc::channel::<()>();
    let (go, go_rx) = mpsc::channel::<()>();
    let (reached_tx, go_rx) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let hook: ParseHook = Arc::new(move |line: &str| {
        if line.contains("MARKER") {
            let _ = reached_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv();
        }
    });
    (hook, reached, go)
}

/// O1: the window's frames never parse. With the parser stuck (on the first line, for as long
/// as the test likes), the window still shows the page — plain —, scrolls, and goes away at once;
/// the worker then stops on its own.
#[test]
fn the_window_never_waits_for_the_parser() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = std::iter::once("// MARKER\n".to_string())
        .chain((1..600).map(|i| format!("fn f{i}() {{}}\n")))
        .collect();
    let mut win = viewer(tmp.path(), "stuck.rs", &body);
    let (hook, reached, go) = blocking_hook();
    win.hook = Some(hook);
    let mut drv = Driver::new(800.0, 500.0);
    drv.idle(&mut win, 2);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the worker reached the stuck line");
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    assert_eq!(
        drv.colors_of("fn f1() {}"),
        Some(vec![plain]),
        "shown plain meanwhile"
    );
    let started = std::time::Instant::now();
    drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    drv.key(&mut win, Key::End, Modifiers::NONE);
    assert!(drv.find_text("fn f599() {}").is_some(), "it scrolled");
    let (parsed, exited) = {
        let w = win.worker.as_ref().unwrap();
        (w.parsed.clone(), w.exited.clone())
    };
    drop(win);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "frames and closing did not wait for the parser: {:?}",
        started.elapsed()
    );
    assert!(!exited.load(Ordering::Relaxed), "still stuck in the parser");
    let before = parsed.load(Ordering::Relaxed);
    go.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !exited.load(Ordering::Relaxed) {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never stopped"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        parsed.load(Ordering::Relaxed) <= before + 1,
        "after the window went the worker parsed on"
    );
}

/// O1: highlighted lines replace the plain ones when the worker sends them.
#[test]
fn highlighted_lines_arrive_and_replace_plain_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "late.rs", "// MARKER\nfn main() {}\n");
    let (hook, reached, go) = blocking_hook();
    win.hook = Some(hook);
    let mut drv = Driver::new(800.0, 500.0);
    drv.idle(&mut win, 2);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    assert_eq!(drv.colors_of("fn main() {}"), Some(vec![plain]));
    assert!(!win.ready());
    go.send(()).unwrap();
    drv.until(&mut win, 10, |w| w.ready());
    let colors: std::collections::HashSet<_> =
        drv.colors_of("fn main() {}").unwrap().into_iter().collect();
    assert!(colors.len() >= 2, "highlighted now: {colors:?}");
}

/// T2: a line longer than LINE_MAX is never parsed cut short: a comment closed past the cut
/// would leave everything after it "in the comment". Highlighting stops at that line (the rest
/// is plain, across marks) and the status says where.
#[test]
fn a_line_cut_for_display_stops_highlighting_there() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::from("fn before() {}\n/* a comment that ");
    body.push_str(&"x".repeat(LINE_MAX + 100));
    body.push_str(" ends here */ let y = 1;\n");
    for i in 0..400 {
        body.push_str(&format!("fn after{i}() {{}}\n"));
    }
    let mut win = viewer(tmp.path(), "cut.rs", &body);
    let mut drv = Driver::new(800.0, 500.0);
    drv.until(&mut win, 10, |w| w.ready());
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    let before: std::collections::HashSet<_> = drv
        .colors_of("fn before() {}")
        .unwrap()
        .into_iter()
        .collect();
    assert!(
        before.len() >= 2,
        "lines before the long one are highlighted"
    );
    assert_eq!(drv.colors_of("fn after0() {}"), Some(vec![plain]));
    assert!(
        win.status.text.contains("line 2"),
        "the status says where highlighting stops: {}",
        win.status.text
    );
    // Past a mark (line 256 and on): still plain, not in a comment.
    drv.key(&mut win, Key::End, Modifiers::NONE);
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(drv.colors_of("fn after399() {}"), Some(vec![plain]));
}

/// T3: the file changes under the viewer (it reads it live, a page at a time): grown,
/// truncated, rewritten in place — any difference is a change. Reads never panic.
#[test]
fn a_file_changed_after_it_was_indexed_never_panics_and_is_noticed() {
    let tmp = tempfile::tempdir().unwrap();
    for case in ["grow", "truncate", "rewrite"] {
        let path = write(tmp.path(), &format!("{case}.txt"), b"a\nb\nc\n");
        let mut doc = TextDoc::open(&path).unwrap();
        assert!(!doc.changed());
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match case {
            "grow" => {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                f.write_all(b"d\ne\n").unwrap();
            }
            "truncate" => f.set_len(2).unwrap(),
            _ => {
                use std::os::unix::fs::FileExt;
                f.write_all_at(b"abcdefgh\nx\ny\n", 0).unwrap();
            }
        }
        drop(f);
        assert!(doc.changed(), "{case}: noticed");
        assert_eq!(doc.now(), Now::Changed);
        // Whatever the bytes now are, reading the old lines does not panic.
        let _ = doc.read_lines(0, 3);
        let _ = doc.read_lines(1, 5);
    }
    // The window says so and stops reading.
    let path = write(tmp.path(), "live.txt", b"one\ntwo\nthree\n");
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "live".into(),
        "live.txt",
        vec![],
    );
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    std::fs::write(&path, "changed\n").unwrap();
    drv.idle(&mut win, 2);
    assert!(win.status.text.contains("changed"), "{}", win.status.text);
}

/// T-nice: a bad byte early in a cut line does not stop the trailing half character from being
/// left out.
#[test]
fn a_cut_line_drops_its_half_character_even_after_a_bad_byte() {
    let mut bytes = vec![0xff, b'a'];
    bytes.extend_from_slice(&"한".as_bytes()[..2]);
    let line = shown(bytes, true);
    assert_eq!(line.text, "\u{FFFD}a");
}

/// Rewrites `path` in place (the same inode) as one line with no newline, `len` bytes long
/// (sparse: the file reads as zeros).
fn rewrite_as_one_huge_line(path: &Path, len: u64) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_len(0).unwrap();
    f.set_len(len).unwrap();
    f.write_all_at(b"abcdefgh", 0).unwrap();
}

/// T3 (constructor): a file rewritten after it was indexed and before the window is made —
/// here as one 100 MB line — is never read through the old index: it is indexed again, and the
/// page reads only what it shows.
#[test]
fn a_file_rewritten_before_the_window_opens_is_not_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "a.txt", b"a\nb\nc\n");
    let doc = TextDoc::open(&path).unwrap();
    rewrite_as_one_huge_line(&path, 100 << 20);
    let mut win = TextWindow::new(doc, "a".into(), "a.txt", vec![]);
    // Nothing read through the old index: the file is read again from its path instead.
    assert_eq!(win.doc.page_reads(), 0);
    let mut drv = Driver::new(600.0, 400.0);
    reread(&mut drv, &mut win, 1);
    assert!(
        win.doc.page_reads() <= 64 * 1024,
        "read {} bytes",
        win.doc.page_reads()
    );
    assert!(drv.text_starting_with("abcdefgh").is_some());
}

/// T3 (paging): a write that lands after the change check cannot make a page read past what
/// the index covers: a line that should end before the next mark and does not is the
/// changed-file error, after a bounded read.
#[test]
fn a_page_never_reads_past_the_indexed_span() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..10).map(|i| format!("line {i}\n")).collect();
    let path = write(tmp.path(), "b.txt", body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    rewrite_as_one_huge_line(&path, 100 << 20);
    let got = doc.read_lines(0, 5);
    assert!(
        doc.page_reads() <= 64 * 1024,
        "read {} bytes",
        doc.page_reads()
    );
    let err = got.expect_err("a line that does not end where the index says is an error");
    assert!(is_stale(&err), "{err:#}");
}

/// The same when the next mark is far away: indexed as `a`, a 100 MB second line, then `last`
/// (no mark between the first line and the line after the huge one), then rewritten with no
/// newline. The first line must end within a span of its mark (else the second line would
/// have started a mark), so its read stops there, not at the mark 100 MB away.
#[test]
fn a_page_never_reads_past_a_span_when_the_next_mark_is_far() {
    use std::os::unix::fs::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("huge-second-line.txt");
    let huge = 100u64 << 20;
    {
        let f = std::fs::File::create(&path).unwrap();
        f.write_all_at(b"a\n", 0).unwrap();
        // The second line: 100 MB that are not a newline (sparse zeros).
        f.set_len(2 + huge).unwrap();
        f.write_all_at(b"\nlast\n", 2 + huge).unwrap();
    }
    let mut doc = TextDoc::open(&path).unwrap();
    assert_eq!(doc.line_count(), 3);
    assert_eq!(doc.marks(), 2, "a mark at `last` only, 100 MB on");
    rewrite_as_one_huge_line(&path, 2 * huge);
    let got = doc.read_lines(0, 1);
    // At most one interval check: a span and a shown line from the mark, and one byte.
    assert!(
        doc.page_reads() <= 64 * 1024 + LINE_MAX as u64 + 1,
        "read {} bytes",
        doc.page_reads()
    );
    let err = got.expect_err("the first line no longer ends within its span");
    assert!(is_stale(&err), "{err:#}");
}

/// Only a genuinely incomplete character at the end of a cut line is left out: bytes that can
/// never start one (0xff, 0xc0, 0xc1, 0xf5…) or a start no character has (an overlong, a
/// surrogate, past U+10FFFF) are bad bytes and show as U+FFFD.
#[test]
fn a_cut_line_keeps_bad_bytes_and_drops_only_a_real_half_character() {
    let cut = |b: &[u8]| shown(b.to_vec(), true).text;
    assert_eq!(cut(&[b'a', 0xff]), "a\u{FFFD}");
    assert_eq!(cut(&[b'a', 0xc0]), "a\u{FFFD}");
    assert_eq!(cut(&[b'a', 0xc1]), "a\u{FFFD}");
    assert_eq!(cut(&[b'a', 0xf5]), "a\u{FFFD}");
    for bad in [
        &[0xe0, 0x80][..],
        &[0xed, 0xa0],
        &[0xf0, 0x80],
        &[0xf4, 0x90],
    ] {
        let mut b = vec![b'a'];
        b.extend_from_slice(bad);
        assert!(
            cut(&b).starts_with("a\u{FFFD}"),
            "{bad:x?} is not a half character"
        );
    }
    // Real halves: of "€" (e2 82 ac) and of "😀" (f0 9f 98 80).
    assert_eq!(cut(&[b'a', 0xe2, 0x82]), "a");
    assert_eq!(cut(&[b'a', 0xf0, 0x9f, 0x98]), "a");
    assert_eq!(cut(&[b'a', 0xc3]), "a");
    // A line that is not cut keeps everything.
    assert_eq!(shown(vec![b'a', 0xe2, 0x82], false).text, "a\u{FFFD}");
}

/// A save that writes a new file and renames it over the opened one (many editors do), or a
/// delete, leaves the viewer's handle on the old file: the path is checked too, so both count
/// as changed — also through a link the file was opened by.
#[test]
fn a_file_replaced_by_rename_or_deleted_counts_as_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "saved.txt", b"one\ntwo\n");
    let doc = TextDoc::open(&path).unwrap();
    assert!(!doc.changed());
    let fresh = write(tmp.path(), ".saved.txt.tmp", b"one\ntwo\n");
    std::fs::rename(&fresh, &path).unwrap();
    assert!(doc.changed(), "replaced by a rename");

    let doc = TextDoc::open(&path).unwrap();
    assert!(!doc.changed());
    std::fs::remove_file(&path).unwrap();
    assert!(doc.changed(), "deleted");

    // Opened through a link: the link's target is what is checked (and stays unchanged).
    let target = write(tmp.path(), "target.txt", b"x\n");
    let link = tmp.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let doc = TextDoc::open(&link).unwrap();
    assert!(!doc.changed(), "a link to the same file is not a change");
    let other = write(tmp.path(), "other.txt", b"x\n");
    std::fs::rename(&other, &target).unwrap();
    assert!(doc.changed(), "the link's target was replaced");
}

/// O2: indexing stops when asked (Esc while a big local text file is indexed): checked before
/// it starts and after every chunk, the error is `Cancelled`.
#[test]
fn indexing_stops_when_cancelled() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..400_000u32).map(|i| format!("{i:08}\n")).collect();
    let path = write(tmp.path(), "big.log", body.as_bytes());
    let stop = AtomicBool::new(true);
    let err = TextDoc::open_cancellable(&path, &stop)
        .err()
        .expect("cancelled");
    assert!(crate::remote::is_cancelled(&err), "{err:#}");
    // Asked after the first chunk: no second chunk is read.
    let stop = AtomicBool::new(false);
    let mut chunks = 0;
    let err = TextDoc::open_with(&path, &stop, &mut |_| {
        chunks += 1;
        stop.store(true, Ordering::Relaxed);
    })
    .err()
    .expect("cancelled");
    assert!(crate::remote::is_cancelled(&err), "{err:#}");
    assert_eq!(chunks, 1);
}

/// O3: a file that grows while it is indexed: the index covers the bytes it read as opened, the
/// last line reads back whole, nothing panics, and the growth counts as a change.
#[test]
fn a_file_growing_while_it_is_indexed() {
    use std::io::Write as _;
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..400_000u32).map(|i| format!("{i:08}\n")).collect();
    let path = write(tmp.path(), "growing.log", body.as_bytes());
    let mut grown = false;
    let mut doc = TextDoc::open_with(&path, &AtomicBool::new(false), &mut |_| {
        if !grown {
            grown = true;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"appended while indexing\nand more").unwrap();
        }
    })
    .unwrap();
    assert_eq!(doc.line_count(), 400_000);
    assert_eq!(doc.size(), body.len() as u64);
    let last = doc.read_lines(399_998, 5).unwrap();
    let texts: Vec<&str> = last.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["00399998", "00399999"]);
    // Appended to while indexed: a difference, so a change (the viewer reads it again).
    assert_eq!(doc.now(), Now::Changed);
}

/// A viewer drawing from its own admission pool of `cap` (tests do not share the process's).
fn pooled_viewer(dir: &Path, name: &str, body: &str, pool: &Arc<Pool>) -> TextWindow {
    let mut win = viewer(dir, name, body);
    win.pool = pool.clone();
    win
}

/// W1: highlighting workers are bounded across viewers: a worker stuck in the parser keeps its
/// slot after its viewer is gone, so with all slots taken a new viewer is plain (and says so)
/// instead of starting another parser; once the stuck ones finish, slots are free again.
#[test]
fn stuck_workers_are_bounded_across_viewers() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = Pool::new(3);
    let (hook, reached, go) = blocking_hook();
    let body = "// MARKER\nfn main() {}\n";
    for k in 0..5 {
        let mut win = pooled_viewer(tmp.path(), &format!("v{k}.rs"), body, &pool);
        win.hook = Some(hook.clone());
        let mut drv = Driver::new(800.0, 400.0);
        drv.idle(&mut win, 2);
        if k < 3 {
            reached
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("a stuck worker");
            assert!(win.worker.is_some());
        } else {
            assert!(
                win.worker.is_none(),
                "viewer {k} started a parser past the cap"
            );
            assert!(win.status.text.contains("busy"), "{}", win.status.text);
            drv.until(&mut win, 5, |w| w.ready());
        }
        assert!(pool.live() <= 3, "{} parsers", pool.live());
        drop(win);
    }
    assert_eq!(
        pool.live(),
        3,
        "the stuck ones hold their slots after their viewers went"
    );
    for _ in 0..3 {
        go.send(()).unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while pool.live() > 0 {
        assert!(std::time::Instant::now() < deadline, "slots never freed");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut win = pooled_viewer(tmp.path(), "after.rs", "fn main() {}\n", &pool);
    let mut drv = Driver::new(800.0, 400.0);
    drv.until(&mut win, 10, |w| w.ready());
    assert!(win.worker.is_some(), "a slot again");
}

/// W2: a worker that fails (a read error that is not a change, or a panic) says so: the page is
/// shown plain and the window is ready, with a status — never waiting forever.
#[test]
fn a_failed_worker_leaves_the_page_plain_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "err.rs", "fn main() {}\n");
    win.doc.fail_worker_reads = true;
    let mut drv = Driver::new(800.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(win.worker.is_none());
    assert!(
        win.status.text.contains("highlighting stopped"),
        "{}",
        win.status.text
    );
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    assert_eq!(drv.colors_of("fn main() {}"), Some(vec![plain]));
    // A worker that dies without a word (a panic in the parser) counts the same.
    let mut win = viewer(tmp.path(), "panic.rs", "// PANIC\nfn main() {}\n");
    win.hook = Some(Arc::new(|line: &str| {
        if line.contains("PANIC") {
            panic!("the parser broke");
        }
    }));
    drv.until(&mut win, 5, |w| w.ready());
    assert!(win.worker.is_none());
    assert!(
        win.status.text.contains("highlighting stopped"),
        "{}",
        win.status.text
    );
}

/// Nice: a page asked earlier and finished late never lands on the page now on screen.
#[test]
fn a_late_page_never_lands_on_the_current_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(800.0, 300.0);
    // How many lines a page holds at this size.
    let mut probe = viewer(tmp.path(), "probe.txt", "x\n");
    drv.idle(&mut probe, 2);
    let visible = probe.visible as usize;
    // The first page's last line stops the parser: released, that page completes (late).
    let body: String = (0..visible)
        .map(|i| {
            if i == visible - 1 {
                "// MARKER\n".to_string()
            } else {
                format!("fn first{i}() {{}}\n")
            }
        })
        .chain((0..400).map(|i| format!("fn second{i}() {{}}\n")))
        .collect();
    let mut win = viewer(tmp.path(), "late.rs", &body);
    let (hook, reached, go) = blocking_hook();
    win.hook = Some(hook);
    drv.idle(&mut win, 2);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let current = win.page.as_ref().unwrap().gen;
    // Released for good (the second page's states pass the same line again).
    go.send(()).unwrap();
    drop(go);
    drv.until(&mut win, 10, |w| w.ready());
    assert_eq!(win.page.as_ref().unwrap().gen, current);
    assert!(
        drv.find_text("fn first0() {}").is_none(),
        "the first page's lines never came back"
    );
    let colors: std::collections::HashSet<_> = drv
        .colors_of("fn second399() {}")
        .expect("the last page is shown")
        .into_iter()
        .collect();
    assert!(colors.len() >= 2, "and highlighted: {colors:?}");
}

/// W2 (ordering): when the worker's thread ends by a panic, the end is receivable before the
/// window is woken for it — the wake is never spent on an empty channel. The worker is paused
/// right after it asked for the wake; the window's side must already see `Failed` or the closed
/// channel then.
#[test]
fn a_panicked_worker_is_visible_before_the_wake() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "panic.rs", "// PANIC\nfn main() {}\n");
    let (go, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    win.hook = Some(Arc::new(move |line: &str| {
        if line.contains("PANIC") {
            let _ = go_rx.lock().unwrap().recv();
            panic!("the parser broke");
        }
    }));
    let (woken_tx, woken) = mpsc::channel::<()>();
    let (resume, resume_rx) = mpsc::channel::<()>();
    let (woken_tx, resume_rx) = (Mutex::new(woken_tx), Mutex::new(resume_rx));
    win.after_wake = Some(Arc::new(move || {
        let _ = woken_tx.lock().unwrap().send(());
        let _ = resume_rx.lock().unwrap().recv();
    }));
    let mut drv = Driver::new(800.0, 400.0);
    drv.idle(&mut win, 2);
    // No pass runs from here until the check: only the worker moves.
    go.send(()).unwrap();
    woken
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the worker asked for the wake");
    let seen = win.worker.as_ref().unwrap().done.try_recv();
    let visible = matches!(
        seen,
        Ok(Done::Failed(_)) | Err(mpsc::TryRecvError::Disconnected)
    );
    resume.send(()).unwrap();
    assert!(visible, "the wake came before the end was receivable");
    drv.until(&mut win, 5, |w| w.ready());
    assert!(
        win.status.text.contains("highlighting stopped"),
        "{}",
        win.status.text
    );
}

/// Appends `text` to the file at `path`.
fn append(path: &Path, text: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// A viewer on a clock the test moves: re-reads start only when the test says the time is right
/// (`TextWindow::clock`).
fn on_clock(win: &mut TextWindow) -> Arc<Mutex<std::time::Instant>> {
    let now = Arc::new(Mutex::new(std::time::Instant::now()));
    let read = now.clone();
    win.clock = Box::new(move || *read.lock().unwrap());
    win.index_done = *now.lock().unwrap();
    win.index_took = std::time::Duration::ZERO;
    now
}

fn later(clock: &Arc<Mutex<std::time::Instant>>, secs: f64) {
    *clock.lock().unwrap() += std::time::Duration::from_secs_f64(secs);
}

/// Passes until the viewer re-read the file (once more than so far) and shows it, `lines`
/// long.
fn reread(drv: &mut Driver, win: &mut TextWindow, lines: u64) {
    let before = win.reindexes;
    drv.until(win, 10, |w| {
        w.reindexes > before
            && w.reindex.is_none()
            && !w.waiting
            && w.doc.line_count() == lines
            && w.ready()
    });
}

/// A log appended to while it is viewed at its end: once re-read, the viewer follows the new
/// end (tail -f) and says so.
#[test]
fn a_log_appended_while_viewed_at_its_end_follows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=500).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "app.log", &body);
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::End, Modifiers::NONE);
    assert!(drv.find_text("row 500").is_some());
    let more: String = (501..=600).map(|i| format!("row {i}\n")).collect();
    append(&tmp.path().join("app.log"), &more);
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 600);
    assert!(drv.find_text("row 600").is_some(), "the new end is shown");
    assert!(
        win.status.text.contains("following the end"),
        "{}",
        win.status.text
    );
}

/// Two lines merged and more appended, then End: re-read, the lines are the new ones under
/// their new numbers — never the old numbering over the new bytes.
#[test]
fn a_merged_line_and_an_append_are_reread_with_the_new_lines() {
    use std::os::unix::fs::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..2000).map(|i| format!("line {i:05}\n")).collect();
    let mut win = viewer(tmp.path(), "merged.log", &body);
    let path = tmp.path().join("merged.log");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.write_all_at(b" ", 10).unwrap();
    drop(f);
    append(&path, "more\n");
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 2000);
    assert!(
        drv.find_text("line 00000 line 00001").is_some(),
        "line 1 is the merged one"
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(drv.find_text("more").is_some() && drv.find_text("line 01999").is_some());
}

/// Grown and re-read, then the same size written with another line structure: re-read again,
/// with the new lines.
#[test]
fn a_same_size_edit_after_growth_is_reread() {
    use std::os::unix::fs::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..300).map(|i| format!("line {i:05}\n")).collect();
    let mut win = viewer(tmp.path(), "edit.log", &body);
    let path = tmp.path().join("edit.log");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "grown\n");
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 301);
    // The same size, another structure: line 5's newline becomes a space.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.write_all_at(b" ", 6 * 11 - 1).unwrap();
    drop(f);
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 300);
    assert!(drv.find_text("line 00005 line 00006").is_some());
}

/// An editor's save by rename: the viewer re-reads the new file at the path.
#[test]
fn a_save_by_rename_shows_the_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "saved.txt", "old content\n");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let fresh = write(tmp.path(), ".saved.txt.tmp", b"new content\nsecond line\n");
    std::fs::rename(&fresh, tmp.path().join("saved.txt")).unwrap();
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 2);
    assert!(drv.find_text("new content").is_some() && drv.find_text("old content").is_none());
}

/// A deleted file: the page stays, the status says so, nothing more is read.
#[test]
fn a_deleted_file_keeps_its_page() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "gone.txt", "still here\n");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    std::fs::remove_file(tmp.path().join("gone.txt")).unwrap();
    later(&clock, 2.0);
    drv.idle(&mut win, 3);
    assert!(drv.find_text("still here").is_some(), "the page stays");
    assert!(win.status.text.contains("deleted"), "{}", win.status.text);
    assert!(win.ready() && win.reindex.is_none());
}

/// An append that lands between indexing and the window: the window re-reads at once (it has
/// nothing on screen yet) and opens with every line.
#[test]
fn an_append_racing_the_open_still_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "race.log", b"first\nsecond\n");
    let doc = TextDoc::open(&path).unwrap();
    append(&path, "third\n");
    let mut win = TextWindow::new(doc, "race".into(), "race.log", vec![]);
    let mut drv = Driver::new(600.0, 400.0);
    reread(&mut drv, &mut win, 3);
    assert!(drv.find_text("first").is_some() && drv.find_text("third").is_some());
}

/// Re-reads held at a gate (the test lets each go).
fn held_rereads(win: &mut TextWindow) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (reached_tx, reached) = mpsc::channel::<()>();
    let (go, go_rx) = mpsc::channel::<()>();
    let (reached_tx, go_rx) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    win.reindex_hook = Some(Arc::new(move || {
        let _ = reached_tx.lock().unwrap().send(());
        let _ = go_rx.lock().unwrap().recv();
    }));
    (reached, go)
}

/// At most one re-read at a time, and the next no sooner than max(1 s, 4 × the last one's time)
/// after it finished.
#[test]
fn rereads_are_throttled_and_never_overlap() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "busy.log", "a\n");
    let path = tmp.path().join("busy.log");
    let clock = on_clock(&mut win);
    win.index_took = std::time::Duration::from_millis(500);
    let (reached, go) = held_rereads(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "b\n");
    later(&clock, 1.9);
    drv.idle(&mut win, 2);
    assert_eq!(win.reindexes, 0, "4 × 500 ms is not over yet");
    assert!(win.status.text.contains("changed"), "{}", win.status.text);
    later(&clock, 0.2);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(win.reindexes, 1);
    append(&path, "c\n");
    later(&clock, 5.0);
    drv.idle(&mut win, 3);
    assert_eq!(win.reindexes, 1, "never two at once");
    go.send(()).unwrap();
    drv.until(&mut win, 10, |w| w.reindex.is_none());
    // Anything newer waits at least a second after it finished.
    append(&path, "d\n");
    later(&clock, 0.5);
    drv.idle(&mut win, 2);
    assert_eq!(win.reindexes, 1, "too soon after the last one");
    later(&clock, 0.6);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(win.reindexes, 2);
    go.send(()).unwrap();
    drv.until(&mut win, 10, |w| {
        w.reindex.is_none() && !w.waiting && w.doc.line_count() == 4 && w.ready()
    });
}

/// Esc (the viewer dropped) during a re-read cancels it; nothing waits for it.
#[test]
fn esc_during_a_reread_cancels_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "big.log", "a\n");
    let path = tmp.path().join("big.log");
    let clock = on_clock(&mut win);
    let (reached, go) = held_rereads(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "b\n");
    later(&clock, 2.0);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let (cancel, exited) = {
        let r = win.reindex.as_ref().expect("a re-read runs");
        (r.cancel.clone(), r.exited.clone())
    };
    let started = std::time::Instant::now();
    drop(win);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(cancel.load(Ordering::Relaxed), "told to stop");
    go.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !exited.load(Ordering::Relaxed) {
        assert!(
            std::time::Instant::now() < deadline,
            "the re-read never stopped"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
