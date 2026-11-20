//! The text viewer: paging reads only the page on screen (a sparse line index, never the whole
//! file), long lines and bad bytes are shown without breaking, and the window highlights, numbers
//! and pages through real key events.

use super::*;
use crate::gui::chrome;
use crate::gui::testkit::Driver;
use eframe::egui::{Key, Modifiers, Pos2};
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
    // One row per line (W), so the line after the long one is on the first page; wrapped, the
    // same holds (`highlighting_runs_on_across_wrapped_rows_and_never_cuts_a_line`).
    drv.key(&mut win, Key::W, Modifiers::NONE);
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
/// truncated, rewritten in place — any difference is a change (grown or rewritten no shorter:
/// `Grown`, re-read soon; truncated: `Changed`). Reads never panic.
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
        let want = if case == "truncate" {
            Now::Changed
        } else {
            Now::Grown
        };
        assert_eq!(doc.now(), want, "{case}");
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
/// last line reads back whole, nothing panics, and the growth counts as a change (`Grown`: the
/// index stays readable, a re-read is due).
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
    // Appended to while indexed: only grown (the viewer reads it again soon).
    assert_eq!(doc.now(), Now::Grown);
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
/// their new numbers. (Until then a merge plus an append looks like growth: a page may be read
/// through the old index, bounded, for at most one re-read period.)
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

/// An append that lands between indexing and the window: the window opens at once with the lines
/// indexed (the file only grew), then re-reads it after the usual wait with every line.
#[test]
fn an_append_racing_the_open_still_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "race.log", b"first\nsecond\n");
    let doc = TextDoc::open(&path).unwrap();
    append(&path, "third\n");
    let mut win = TextWindow::new(doc, "race".into(), "race.log", vec![]);
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.idle(&mut win, 1);
    assert!(drv.find_text("second").is_some(), "shown at once");
    assert_eq!(win.reindexes, 0, "re-read after the usual wait");
    later(&clock, 1.1);
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

/// A re-read whose file grew again while it ran (before its result is taken) is installed: it
/// describes the bytes as scanned. The growth is picked up by the next pass, no sooner than the
/// usual wait.
#[test]
fn a_reread_overtaken_by_growth_is_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "busy.log", &body);
    let path = tmp.path().join("busy.log");
    let clock = on_clock(&mut win);
    let (reached, go) = held_rereads(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "row 51\n");
    later(&clock, 2.0);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    // The pass will index "row 51"; the file grows again before its result is taken.
    go.send(()).unwrap();
    let r = win.reindex.as_ref().unwrap().exited.clone();
    while !r.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    append(&path, "row 52\n");
    drv.idle(&mut win, 2);
    assert_eq!(win.doc.line_count(), 51, "installed, as scanned");
    assert!(drv.find_text("row 1").is_some(), "a page is shown");
    assert_eq!(win.reindexes, 1, "the next pass waits the usual time");
    later(&clock, 1.1);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(win.reindexes, 2);
    go.send(()).unwrap();
    drv.until(&mut win, 10, |w| {
        w.reindex.is_none() && w.doc.line_count() == 52
    });
    drop(go);
}

/// L1: a viewer that never showed anything retries a failing first read with the usual delay,
/// not at once every frame.
#[test]
fn repeated_initial_failures_are_throttled() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "flaky.log", b"one\n");
    let doc = TextDoc::open(&path).unwrap();
    append(&path, "two\n");
    let mut win = TextWindow::new(doc, "f".into(), "flaky.log", vec![]);
    let clock = on_clock(&mut win);
    // Every re-read fails: the path is a folder by the time it runs.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    drv.idle(&mut win, 1);
    drv.until(&mut win, 5, |w| w.reindex.is_none() && w.reindexes == 1);
    drv.idle(&mut win, 5);
    assert_eq!(win.reindexes, 1, "no immediate retry: {}", win.status.text);
    later(&clock, 1.1);
    drv.idle(&mut win, 1);
    assert_eq!(win.reindexes, 2, "retried after the usual wait");
}

/// L2: a change the highlighter finds (its read is `Stale`) starts the re-read — or schedules
/// it — in the frame that hears of it, with no further input.
#[test]
fn a_change_found_by_the_highlighter_starts_the_reread() {
    let tmp = tempfile::tempdir().unwrap();
    // Long enough not to be at its end (no follow repaints in the way).
    let body = "fn main() {}\n".repeat(500);
    let mut win = viewer(tmp.path(), "hl.rs", &body);
    win.doc.stale_worker_reads = true;
    let clock = on_clock(&mut win);
    later(&clock, 2.0);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.waiting || w.reindexes > 0);
    assert_eq!(
        win.reindexes, 1,
        "started in the frame that heard of the change"
    );
}

/// L2: before the deadline, the frame that hears of the change from the highlighter asks for a
/// repaint when the re-read is due (no further input or frames needed).
#[test]
fn a_change_found_by_the_highlighter_schedules_the_reread() {
    let tmp = tempfile::tempdir().unwrap();
    // Long enough not to be at its end (no follow repaints in the way).
    let body = "fn main() {}\n".repeat(500);
    let mut win = viewer(tmp.path(), "hl2.rs", &body);
    win.doc.stale_worker_reads = true;
    let _clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    drv.ctx
        .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
    drv.until(&mut win, 5, |w| w.waiting);
    assert_eq!(win.reindexes, 0);
    let delays = asked.lock().unwrap().clone();
    assert!(
        delays
            .iter()
            .any(|d| *d > std::time::Duration::from_millis(500)
                && *d <= std::time::Duration::from_secs(1)),
        "a repaint at the deadline: {delays:?}"
    );
}

/// L3: the wheel does not scroll while a re-read is under way, as the keys do not.
#[test]
fn the_wheel_waits_during_a_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=500).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "wheel.log", &body);
    let clock = on_clock(&mut win);
    let (reached, go) = held_rereads(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&tmp.path().join("wheel.log"), "row 501\n");
    later(&clock, 2.0);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    drv.wheel(&mut win, Pos2::new(300.0, 200.0), -10.0);
    assert_eq!(win.top, 0, "the wheel waited");
    drop(go);
}

/// L4: a local file viewed at its end is checked again about every second with no input (a
/// growing log is followed); away from the end, or for a remote copy, it is not. Counted at the
/// viewer's follow request, not by repaint delays (a one-second repaint can be another's).
#[test]
fn a_viewer_at_the_end_checks_again_every_second() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=500).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "tail.log", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    drv.ctx
        .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
    let follows_in_a_pass = |drv: &mut Driver, win: &mut TextWindow| {
        let before = follows();
        drv.idle(win, 1);
        follows() - before
    };
    assert_eq!(
        follows_in_a_pass(&mut drv, &mut win),
        0,
        "not at the end: no follow"
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    asked.lock().unwrap().clear();
    assert_eq!(
        follows_in_a_pass(&mut drv, &mut win),
        1,
        "at the end: the next check"
    );
    assert!(
        asked.lock().unwrap().contains(&FOLLOW_EVERY),
        "asked for a second later"
    );
    win.live = false;
    assert_eq!(
        follows_in_a_pass(&mut drv, &mut win),
        0,
        "a remote copy is not followed"
    );
}

/// L1: a change other than growth (here a shrink) landing right after a re-read is installed,
/// before its first page is read through it, keeps the last page up (it is not blanked) and the
/// next pass waits as usual.
#[test]
fn a_change_right_after_a_reread_lands_keeps_the_page() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "busy.log", &body);
    let path = tmp.path().join("busy.log");
    let clock = on_clock(&mut win);
    let (reached, go) = held_rereads(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "row 51\n");
    later(&clock, 2.0);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let again = path.clone();
    win.after_install = Some(Box::new(move || {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&again)
            .unwrap();
        f.set_len(20).unwrap();
    }));
    go.send(()).unwrap();
    drv.until(&mut win, 5, |w| {
        w.reindex.is_none() && w.doc.line_count() == 51
    });
    drv.idle(&mut win, 2);
    assert!(drv.find_text("row 1").is_some(), "the last page stays up");
    assert_eq!(win.reindexes, 1, "the next pass waits the usual time");
    later(&clock, 1.1);
    drv.idle(&mut win, 1);
    reached
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(win.reindexes, 2);
    drop(go);
}

/// A log written every millisecond (real time), opened while it is written: a page is shown at
/// once (through the index the opener built), and at its end the viewer shows the log growing —
/// each re-read installed though the file never stops growing — with no more passes than the
/// throttle allows.
#[test]
fn a_busy_log_shows_a_page_at_once_and_follows_it() {
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=2000).map(|i| format!("row {i}\n")).collect();
    let path = write(tmp.path(), "busy.log", body.as_bytes());
    let doc = TextDoc::open(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (st, p) = (stop.clone(), path.clone());
    let writer = std::thread::spawn(move || {
        let mut n = 0u64;
        while !st.load(Ordering::Relaxed) {
            append(&p, &format!("more {n}\n"));
            n += 1;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
    // Written to between the opener's index and the window (the browser's open).
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut win = TextWindow::new(doc, "busy".into(), "busy.log", vec![]);
    let opened = std::time::Instant::now();
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 1, |w| {
        w.page.as_ref().is_some_and(|p| !p.is_blank())
    });
    assert!(
        drv.find_text("row 1").is_some(),
        "a page within the first second"
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let mut installs = 0;
    let mut lines = win.doc.line_count();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while installs < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "{installs} installs in {} passes: {}",
            win.reindexes,
            win.status.text
        );
        drv.idle(&mut win, 1);
        if win.doc.line_count() > lines {
            installs += 1;
            lines = win.doc.line_count();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drv.idle(&mut win, 1);
    let elapsed = opened.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    assert!(
        drv.text_starting_with("more ").is_some(),
        "the end shows the appended lines"
    );
    assert!(
        (win.reindexes as f64) <= elapsed + 1.0,
        "{} passes in {elapsed:.1} s",
        win.reindexes
    );
}

/// What counts as only grown: the same file at the handle and the path, no byte indexed gone
/// as far as the size can tell. Anything else is a change; a missing path is gone.
#[test]
fn now_tells_growth_from_other_changes() {
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let body = "abc\ndef\n";
    let fresh = |name: &str| {
        let path = write(tmp.path(), name, body.as_bytes());
        (TextDoc::open(&path).unwrap(), path)
    };
    let (doc, path) = fresh("grow.txt");
    append(&path, "ghi\n");
    assert_eq!(doc.now(), Now::Grown, "appended");
    let (doc, path) = fresh("same.txt");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.write_all_at(b"x", 0).unwrap();
    assert_eq!(doc.now(), Now::Grown, "written in place, no shorter");
    let (doc, path) = fresh("shrink.txt");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(3)
        .unwrap();
    assert_eq!(doc.now(), Now::Changed, "shorter");
    let (doc, path) = fresh("swap.txt");
    let other = write(tmp.path(), ".swap.tmp", format!("{body}more\n").as_bytes());
    std::fs::rename(&other, &path).unwrap();
    assert_eq!(doc.now(), Now::Changed, "another file at the path, longer");
    let (doc, path) = fresh("gone.txt");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(doc.now(), Now::Gone);
    // A scan that met a shrink (it read fewer bytes than it opened with) is a change even once
    // the file is as long again.
    let big: String = (0..300_000u32).map(|i| format!("{i:07}\n")).collect();
    let path = write(tmp.path(), "cut.log", big.as_bytes());
    let mut cut = false;
    let doc = TextDoc::open_with(&path, &AtomicBool::new(false), &mut |_| {
        if !cut {
            cut = true;
            let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.set_len(1 << 20).unwrap();
        }
    })
    .unwrap();
    assert!(doc.size() < big.len() as u64);
    append(&path, &big);
    assert_eq!(doc.now(), Now::Changed, "a shrink met while scanning");
}

/// Runs `change` once, during the next re-read's pass (after its first chunk).
fn during_the_pass(win: &mut TextWindow, change: impl Fn() + Send + Sync + 'static) {
    let once = AtomicBool::new(false);
    win.reindex_chunk_hook = Some(Arc::new(move || {
        if !once.swap(true, Ordering::Relaxed) {
            change();
        }
    }));
}

/// Starts a re-read of the changed file and waits until its worker is done (its result not
/// taken yet).
fn pass_done(drv: &mut Driver, win: &mut TextWindow, clock: &Arc<Mutex<std::time::Instant>>) {
    later(clock, 2.0);
    drv.idle(win, 1);
    let exited = win.reindex.as_ref().expect("a re-read runs").exited.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !exited.load(Ordering::Relaxed) {
        assert!(std::time::Instant::now() < deadline, "the pass never ended");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A shrink during a re-read's pass: its index is not installed, the page on screen stays and
/// the next pass waits the usual time — also when the file is as long again by the time the
/// result is taken (the worker's own check, right after the pass, saw the shrink).
#[test]
fn a_shrink_during_a_reread_is_not_installed() {
    for regrown in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
        let mut win = viewer(tmp.path(), "cut.log", &body);
        let path = tmp.path().join("cut.log");
        let clock = on_clock(&mut win);
        let mut drv = Driver::new(600.0, 400.0);
        drv.until(&mut win, 5, |w| w.ready());
        append(&path, "row 51\n");
        let p = path.clone();
        during_the_pass(&mut win, move || {
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_len(20).unwrap();
        });
        pass_done(&mut drv, &mut win, &clock);
        if regrown {
            // As long again, in lines: the old index would look grown.
            let more: String = (1..=80).map(|i| format!("new {i}\n")).collect();
            append(&path, &more);
        }
        drv.idle(&mut win, 2);
        assert_eq!(win.doc.line_count(), 50, "regrown {regrown}: not installed");
        assert!(
            drv.find_text("row 1").is_some(),
            "regrown {regrown}: page kept"
        );
        // What the pass learned stands: nothing is read through the old index meanwhile.
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
        assert_eq!(win.top, 0, "regrown {regrown}: scrolling waits");
        assert!(
            drv.find_text("row 1").is_some(),
            "regrown {regrown}: page kept after a key"
        );
        assert_eq!(win.reindexes, 1, "regrown {regrown}: the next pass waits");
        later(&clock, 1.1);
        drv.idle(&mut win, 1);
        assert_eq!(win.reindexes, 2, "regrown {regrown}: then it runs");
    }
}

/// Another file renamed over the path during a re-read's pass (an editor's save): the pass's
/// index (of the old file) is not installed; the next pass, after the usual wait, shows the new
/// file.
#[test]
fn an_inode_swap_during_a_reread_is_not_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "saved.log", &body);
    let path = tmp.path().join("saved.log");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    append(&path, "row 51\n");
    let (dir, p) = (tmp.path().to_path_buf(), path.clone());
    // Longer than the old file: only the inode tells them apart.
    let anew: String = std::iter::once("saved anew\n".to_string())
        .chain((1..=60).map(|i| format!("row {i}\n")))
        .collect();
    during_the_pass(&mut win, move || {
        let fresh = write(&dir, ".saved.tmp", anew.as_bytes());
        std::fs::rename(&fresh, &p).unwrap();
    });
    pass_done(&mut drv, &mut win, &clock);
    drv.idle(&mut win, 2);
    assert_eq!(win.doc.line_count(), 50, "not installed");
    assert!(drv.find_text("row 1").is_some(), "page kept");
    assert_eq!(win.reindexes, 1, "the next pass waits");
    later(&clock, 1.1);
    reread(&mut drv, &mut win, 61);
    assert!(drv.find_text("saved anew").is_some());
}

/// The opener's index gets the same check right after its pass as a re-read's: a shrink there
/// makes it unusable even once the file is as long again — the window reads nothing through it
/// and indexes the file again.
#[test]
fn an_opening_index_whose_pass_met_a_shrink_is_not_read() {
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("row {i}\n")).collect();
    let path = write(tmp.path(), "open.log", body.as_bytes());
    let p = path.clone();
    let doc = TextDoc::open_with(&path, &AtomicBool::new(false), &mut |_| {
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(20).unwrap();
    })
    .unwrap();
    let more: String = (1..=80).map(|i| format!("new {i}\n")).collect();
    append(&path, &more);
    assert_eq!(doc.now(), Now::Grown, "it looks grown now");
    let win = TextWindow::new(doc, "open".into(), "open.log", vec![]);
    assert!(win.waiting, "not read through");
    assert_eq!(win.doc.page_reads(), 0);
}

/// The grammar a shebang names, as the viewer shows it.
fn grammar(win: &TextWindow) -> Option<&str> {
    win.syntax.map(|s| s.name.as_str())
}

/// An extensionless script whose name does not decide: its first line does, also when the file
/// grew between indexing and the window (an accepted, only-grown index).
#[test]
fn an_extensionless_script_racing_the_open_is_highlighted_by_its_shebang() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "run", b"#!/bin/sh\necho hello\n");
    let doc = TextDoc::open(&path).unwrap();
    append(&path, "echo more\n");
    let mut win = TextWindow::new(doc, "run".into(), "run", vec![]);
    let _clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(grammar(&win), Some("Bourne Again Shell (bash)"));
    let colors = drv.colors_of("echo hello").expect("shown");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
}

/// A re-read after the shebang changed switches the grammar; a name that decides keeps its own.
#[test]
fn a_reread_after_the_shebang_changed_switches_grammar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "tool", "#!/bin/sh\necho hi\n");
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(grammar(&win), Some("Bourne Again Shell (bash)"));
    let fresh = write(
        tmp.path(),
        ".tool.tmp",
        b"#!/usr/bin/env python3\nprint('hi')\n",
    );
    std::fs::rename(&fresh, tmp.path().join("tool")).unwrap();
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 2);
    assert_eq!(grammar(&win), Some("Python"));
    let colors = drv.colors_of("print('hi')").expect("shown");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
    // The name decides when it can: a `.rs` file stays Rust whatever its first line says.
    let mut named = viewer(tmp.path(), "main.rs", "fn main() {}\n");
    let clock = on_clock(&mut named);
    drv.until(&mut named, 5, |w| w.ready());
    let fresh = write(tmp.path(), ".main.tmp", b"#!/bin/sh\nfn main() {}\n");
    std::fs::rename(&fresh, tmp.path().join("main.rs")).unwrap();
    later(&clock, 2.0);
    reread(&mut drv, &mut named, 2);
    assert_eq!(grammar(&named), Some("Rust"));
}

/// As on a DOS screen, a control character shows as its code page 437 glyph (an escape
/// sequence reads `←[31m`), one cell each, so the columns stay where they are.
#[test]
fn control_characters_show_as_their_glyphs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "log.txt", "a\u{1}b\u{1b}[31mred\u{7f}\nnext\n");
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(drv.find_text("a☺b←[31mred⌂").is_some());
    assert!(drv.find_text("next").is_some());
}

/// Only the columns that fit are laid out: a line of thousands of characters is drawn as the
/// part on screen (its start whole, inside the window), and Right brings the next part.
#[test]
fn a_long_line_is_laid_out_as_far_as_the_window_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..400).map(|i| format!("{i:04} ")).collect();
    let mut win = viewer(tmp.path(), "wide.txt", &format!("{line}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    // One row per line (W): the sideways view.
    drv.key(&mut win, Key::W, Modifiers::NONE);
    let shown = drv
        .text_starting_with("0000 0001")
        .expect("the start, whole");
    assert!(shown.len() < 100, "{} characters laid out", shown.len());
    assert!(line.starts_with(&shown));
    drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    let moved = drv
        .text_starting_with(&line[8..20])
        .expect("scrolled by eight columns");
    assert!(line[8..].starts_with(&moved));
}

// ---------------------------------------------------------------------------------------------
// Word wrap (owner, message 13): on by default, the whole of every line, read a chunk at a time.

/// Where the rows of text start across the window: the left edge of the text drawn that starts
/// with `prefix` (the line numbers sit to the left of it).
fn text_x(drv: &Driver, prefix: &str) -> f32 {
    drv.texts()
        .into_iter()
        .find(|(_, _, t)| t.starts_with(prefix))
        .map(|(r, _, _)| r.left())
        .unwrap_or_else(|| panic!("no text starts with {prefix:?}"))
}

/// The rows of text drawn at column `x`, top to bottom — whether or not they fit their clip.
fn rows_at(drv: &Driver, x: f32) -> Vec<String> {
    let mut rows: Vec<(f32, String)> = drv
        .texts()
        .into_iter()
        .filter(|(r, _, _)| (r.left() - x).abs() < 0.5)
        .map(|(r, _, t)| (r.top(), t))
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, t)| t).collect()
}

/// Every row drawn at column `x` lies inside the rect it is clipped to: no glyph is cut at the
/// window's edge.
fn assert_rows_inside(drv: &Driver, x: f32) {
    for (bounds, clip, text) in drv.texts() {
        if (bounds.left() - x).abs() < 0.5 {
            assert!(
                clip.expand(0.5).contains_rect(bounds),
                "{text:?} runs out of the window: {bounds:?} in {clip:?}"
            );
        }
    }
}

/// The labels of the viewer's own F-keys.
fn labels(win: &TextWindow) -> Vec<(u8, &'static str)> {
    Window::keys(win).iter().map(|k| (k.n, k.label)).collect()
}

/// Lines wrap by default: a line wider than the window goes on in the rows below it, every row
/// whole inside the window and broken after a space; nothing of the line is lost.
#[test]
fn lines_wrap_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..400).map(|i| format!("{i:04} ")).collect();
    let mut win = viewer(tmp.path(), "wide.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "0000 0001");
    let rows = rows_at(&drv, x);
    assert!(rows.len() >= 3, "wrapped: {rows:?}");
    let shown: String = rows.iter().take_while(|r| *r != "next").cloned().collect();
    assert!(
        shown.len() > 200 && line.starts_with(&shown),
        "the rows are the line, in order: {rows:?}"
    );
    for row in &rows[..rows.len() - 1] {
        assert!(row.ends_with(' '), "{row:?} breaks after a space");
    }
    assert_rows_inside(&drv, x);
}

/// W, or F2 — the row says what it does: "Unwrap", then "Wrap" — goes back to the sideways view
/// (a row per line, ←/→ scroll it) and back again.
#[test]
fn w_and_f2_switch_wrapping_off_and_on() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..400).map(|i| format!("{i:04} ")).collect();
    let mut win = viewer(tmp.path(), "wide.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(labels(&win), [(2, "Unwrap")]);
    let x = text_x(&drv, "0000 0001");
    assert!(rows_at(&drv, x).len() >= 3);
    drv.key(&mut win, Key::W, Modifiers::NONE);
    assert_eq!(labels(&win), [(2, "Wrap")]);
    let rows = rows_at(&drv, x);
    assert_eq!(rows.len(), 2, "a row per line: {rows:?}");
    assert_eq!(rows[1], "next");
    drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    assert!(
        drv.text_starting_with(&line[8..20]).is_some(),
        "→ scrolls the unwrapped line"
    );
    drv.key(&mut win, Key::F2, Modifiers::NONE);
    assert_eq!(labels(&win), [(2, "Unwrap")]);
    assert!(rows_at(&drv, x).len() >= 3, "wrapped again");
    assert!(
        drv.text_starting_with("0000 0001").is_some(),
        "from the line's start"
    );
}

/// A line longer than LINE_MAX is shown whole when wrapped: its end is on screen at the end of
/// the file, and nothing says it was cut.
#[test]
fn a_line_over_line_max_is_shown_whole_across_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut line: String = (0..3 * LINE_MAX / 5).map(|i| format!("{i:04} ")).collect();
    line.push_str("THE END");
    let mut win = viewer(tmp.path(), "long.txt", &format!("{line}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "0000 0001");
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let rows = rows_at(&drv, x);
    assert!(
        rows.last().is_some_and(|r| r.ends_with("THE END")),
        "the end of the line: {rows:?}"
    );
    assert!(line.ends_with(&rows.concat()), "{rows:?}");
    assert!(
        !drv.texts().iter().any(|(_, _, t)| t.contains("cut")),
        "nothing is cut"
    );
    assert_rows_inside(&drv, x);
}

/// A line of megabytes, jumped into at its end (no way back remembered): PgUp and ↑ go back
/// through it a row at a time across its 16 KiB restart points, PgDn and ↓ come forward through
/// exactly the same rows — and each step reads a bounded number of bytes, wherever it is.
#[test]
fn a_huge_line_goes_back_and_forth_through_the_same_rows_with_bounded_reads() {
    let tmp = tempfile::tempdir().unwrap();
    // Seven-byte words: the restart points fall inside words.
    let line: String = (0..700_000u32).map(|i| format!("{i:06} ")).collect();
    let mut win = viewer(tmp.path(), "huge.txt", &format!("{line}\nafter\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 10, |w| w.ready());
    let x = text_x(&drv, "000000 000001");
    let top = |drv: &Driver| rows_at(drv, x).first().cloned().unwrap_or_default();
    let step = |drv: &mut Driver, win: &mut TextWindow, key: Key| {
        let before = win.doc.page_reads();
        drv.key(win, key, Modifiers::NONE);
        let read = win.doc.page_reads() - before;
        assert!(read <= 192 * 1024, "{key:?} read {read} bytes");
    };
    // Cmd/Ctrl+End: the end of the file (End alone goes to the end of the line first).
    let before = win.doc.page_reads();
    drv.key(&mut win, Key::End, crate::gui::testkit::command(false));
    let read = win.doc.page_reads() - before;
    assert!(read <= 192 * 1024, "Cmd+End read {read} bytes");
    assert!(rows_at(&drv, x).last().is_some_and(|r| r == "after"));
    let mut seen = vec![top(&drv)];
    let back: Vec<Key> = std::iter::repeat_n(Key::PageUp, 30)
        .chain(std::iter::repeat_n(Key::ArrowUp, 12))
        .collect();
    for &key in &back {
        step(&mut drv, &mut win, key);
        let now = top(&drv);
        assert_ne!(&now, seen.last().unwrap(), "{key:?} moved");
        seen.push(now);
    }
    // Back over more than two restart points: more than 32 KiB of the line.
    let word = |row: &str| {
        row.split(' ')
            .find(|w| w.len() == 6)
            .map(|w| w.parse::<u64>().unwrap())
    };
    let (end, start) = (word(&seen[0]).unwrap(), word(seen.last().unwrap()).unwrap());
    assert!((end - start) * 7 > 2 * 16 * 1024, "{start}..{end}");
    for (k, &key) in back.iter().rev().enumerate() {
        let forward = if key == Key::PageUp {
            Key::PageDown
        } else {
            Key::ArrowDown
        };
        step(&mut drv, &mut win, forward);
        assert_eq!(top(&drv), seen[seen.len() - 2 - k], "step {k} forward");
    }
    assert!(
        win.doc.page_reads() < 8 << 20,
        "{} bytes read in all",
        win.doc.page_reads()
    );
}

/// A character whose bytes straddle 16 KiB of a wrapped line — a read edge and a restart
/// point — is shown whole: never split into two U+FFFD.
#[test]
fn a_character_across_a_read_edge_is_never_split_in_a_wrapped_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut line = "x".repeat(16 * 1024 - 1);
    line.push_str("한글");
    line.push_str(&"y".repeat(20_000));
    let mut win = viewer(tmp.path(), "edge.txt", &format!("{line}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "xxxx");
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let mut found = None;
    for _ in 0..100 {
        let rows = rows_at(&drv, x);
        assert!(
            rows.iter().all(|r| !r.contains('\u{FFFD}')),
            "a character split: {rows:?}"
        );
        if rows.iter().any(|r| r.contains('한') || r.contains('글')) {
            found = Some(rows);
            break;
        }
        drv.key(&mut win, Key::PageUp, Modifiers::NONE);
    }
    found.expect("the Hangul is reached going back");
    // One row more: the restart point falls after 한, so 글 starts the next row.
    drv.key(&mut win, Key::ArrowUp, Modifiers::NONE);
    let rows = rows_at(&drv, x);
    assert!(rows.concat().contains("x한글y"), "{rows:?}");
    assert!(rows.iter().all(|r| !r.contains('\u{FFFD}')), "{rows:?}");
}

/// With the system fonts a window gets (Hangul, emoji: wider than a cell), rows are broken by
/// the measured width of their glyphs: every row inside the window, no glyph cut at its edge,
/// and the rows are the line.
#[test]
fn hangul_and_emoji_wrap_inside_the_window_with_system_fonts() {
    use egui_system_font_provider::SystemFontProvider;
    let tmp = tempfile::tempdir().unwrap();
    let words = [
        "한국어",
        "문장을",
        "줄바꿈해서",
        "보여준다",
        "😀",
        "emoji",
        "가나다라마바사아자차카타파하",
    ];
    let line = (0..50)
        .map(|i| words[i % words.len()])
        .collect::<Vec<_>>()
        .join(" ");
    let mut win = viewer(tmp.path(), "ko.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.ctx
        .add_font_provider(Arc::new(SystemFontProvider::new()));
    drv.idle(&mut win, 3);
    let f = chrome::font();
    let han = drv.ctx.fonts_mut(|fonts| {
        fonts
            .has_glyph(&f, '한')
            .then(|| fonts.glyph_width(&f, '한'))
    });
    let Some(han) = han else {
        eprintln!("skipping: no installed font with Hangul");
        return;
    };
    assert!(
        han > chrome::CELL.x,
        "the case: 한 ({han}) wider than a cell"
    );
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "한국어");
    let rows = rows_at(&drv, x);
    assert!(rows.len() >= 4, "wrapped: {rows:?}");
    assert_eq!(rows.last().map(String::as_str), Some("next"));
    let shown = rows[..rows.len() - 1].concat();
    assert_eq!(shown.replace(' ', ""), line.replace(' ', ""), "{rows:?}");
    assert_rows_inside(&drv, x);
}

/// A row is broken after a space when the word that follows fits on the next row; a word longer
/// than a row is cut where the row is full.
#[test]
fn rows_break_after_a_space_when_the_word_fits() {
    let tmp = tempfile::tempdir().unwrap();
    let words = [
        "lorem",
        "ipsum",
        "dolor",
        "sit",
        "amet,",
        "consectetur",
        "adipiscing",
        "elit",
    ];
    let line = (0..60)
        .map(|i| words[i % words.len()])
        .collect::<Vec<_>>()
        .join(" ");
    let long_word = "z".repeat(150);
    let mut win = viewer(tmp.path(), "words.txt", &format!("{line}\n{long_word}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "lorem ipsum");
    let rows = rows_at(&drv, x);
    let line_rows: Vec<String> = rows
        .iter()
        .take_while(|r| !r.starts_with('z'))
        .cloned()
        .collect();
    assert!(line_rows.len() >= 3, "{rows:?}");
    assert_eq!(line_rows.concat(), line);
    for row in &line_rows[..line_rows.len() - 1] {
        assert!(row.ends_with(' '), "{row:?} ends after a space");
    }
    let z_rows: Vec<String> = rows
        .iter()
        .filter(|r| r.starts_with('z'))
        .cloned()
        .collect();
    assert!(z_rows.len() >= 2, "{rows:?}");
    assert_eq!(z_rows.concat(), long_word);
    assert_rows_inside(&drv, x);
}

/// A log of long lines, wrapped and viewed at its end: appended to, it is followed to the new
/// end — the whole of the new last line.
#[test]
fn a_wrapped_log_appended_at_its_end_follows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=100)
        .map(|i| format!("row {i:03} {}\n", "-".repeat(300)))
        .collect();
    let mut win = viewer(tmp.path(), "wide.log", &body);
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::End, Modifiers::NONE);
    append(
        &tmp.path().join("wide.log"),
        &format!("{} APPENDED-END\n", "+".repeat(300)),
    );
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 101);
    assert!(
        drv.texts()
            .iter()
            .any(|(_, _, t)| t.ends_with("APPENDED-END")),
        "the new end is shown"
    );
    assert!(
        win.status.text.contains("following the end"),
        "{}",
        win.status.text
    );
}

/// A file of a few long lines, viewed at its top, is not at its end (it is not followed) until
/// the last row of its last line is on screen.
#[test]
fn a_few_long_lines_are_not_at_the_end_until_their_last_row_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=10)
        .map(|i| format!("line {i} {}\n", "w ".repeat(1000)))
        .collect();
    let mut win = viewer(tmp.path(), "few.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let before = follows();
    drv.idle(&mut win, 1);
    assert_eq!(follows() - before, 0, "the top is not the end");
    drv.key(&mut win, Key::End, crate::gui::testkit::command(false));
    let before = follows();
    drv.idle(&mut win, 1);
    assert_eq!(follows() - before, 1, "the end is");
}

/// Highlighting runs on across the rows of a wrapped line (a string's colour goes on to the
/// next row); a line too long to highlight stays whole when the highlighted page lands, and
/// what follows it is plain (a comment it closes past LINE_MAX colours nothing after it).
#[test]
fn highlighting_runs_on_across_wrapped_rows_and_never_cuts_a_line() {
    let tmp = tempfile::tempdir().unwrap();
    let string = "s".repeat(150);
    let long = "x".repeat(LINE_MAX + 500);
    let body = format!(
        "fn main() {{\n    let text = \"{string}\";\n}}\n/* {long} THE END */ let y = 1;\nfn after() {{}}\n"
    );
    let mut win = viewer(tmp.path(), "wrap.rs", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 10, |w| w.ready());
    let x = text_x(&drv, "fn main()");
    let rows = rows_at(&drv, x);
    let i = rows
        .iter()
        .position(|r| r.starts_with("    let text"))
        .expect("the string's line");
    assert!(rows.len() > i + 1, "{rows:?}");
    let first = drv.colors_of(&rows[i]).unwrap();
    let next = drv.colors_of(&rows[i + 1]).unwrap();
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    let string_color = *first.last().unwrap();
    assert!(first.len() >= 2 && string_color != plain, "{first:?}");
    assert_eq!(next, vec![string_color], "the string goes on");
    drv.key(&mut win, Key::End, crate::gui::testkit::command(false));
    drv.until(&mut win, 10, |w| w.ready());
    assert!(
        rows_at(&drv, x)
            .concat()
            .contains("xxx THE END */ let y = 1;fn after() {}"),
        "the long line whole after highlighting: {:?}",
        rows_at(&drv, x)
    );
    assert_eq!(drv.colors_of("fn after() {}"), Some(vec![plain]));
}

/// Home and End inside a line longer than the window go to its first and last rows first, then
/// to the start and end of the file; Cmd/Ctrl+Home and +End go to the file's straight away.
#[test]
fn home_and_end_go_to_a_long_lines_ends_then_the_files() {
    let tmp = tempfile::tempdir().unwrap();
    let long: String = (0..3000u32).map(|i| format!("{i:05} ")).collect();
    let tail: String = (0..200).map(|i| format!("tail {i}\n")).collect();
    let body = format!("first\n{long}LONG-END\n{tail}");
    let mut win = viewer(tmp.path(), "ends.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "first");
    let rows = |drv: &Driver| rows_at(drv, x);
    for _ in 0..3 {
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    }
    assert!(
        rows(&drv)
            .iter()
            .all(|r| !r.contains("first") && !r.contains("tail")),
        "inside the long line: {:?}",
        rows(&drv)
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let at = rows(&drv);
    assert!(
        at.last().is_some_and(|r| r.ends_with("LONG-END")),
        "End: the line's last row at the bottom: {at:?}"
    );
    drv.key(&mut win, Key::End, Modifiers::NONE);
    assert!(
        rows(&drv).last().is_some_and(|r| r == "tail 199"),
        "End again: the end of the file"
    );
    drv.key(&mut win, Key::Home, Modifiers::NONE);
    assert_eq!(rows(&drv)[0], "first", "Home at a line start: the file's");
    for _ in 0..3 {
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    }
    drv.key(&mut win, Key::Home, Modifiers::NONE);
    assert!(
        rows(&drv)[0].starts_with("00000 00001"),
        "Home inside the line: its first row: {:?}",
        rows(&drv)
    );
    for _ in 0..3 {
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    }
    let command = crate::gui::testkit::command(false);
    drv.key(&mut win, Key::End, command);
    assert!(
        rows(&drv).last().is_some_and(|r| r == "tail 199"),
        "Cmd/Ctrl+End"
    );
    drv.key(&mut win, Key::Home, command);
    assert_eq!(rows(&drv)[0], "first", "Cmd/Ctrl+Home");
    drv.key(&mut win, Key::End, Modifiers::CTRL);
    assert!(
        rows(&drv).last().is_some_and(|r| r == "tail 199"),
        "Ctrl+End"
    );
}

/// How many columns a wrapped row holds in a `w`×`h` window: the first row of a line of one
/// long word.
fn columns(w: f32, h: f32) -> usize {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "probe.txt", &format!("{}\n", "y".repeat(500)));
    let mut drv = Driver::new(w, h);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "yyy");
    rows_at(&drv, x)[0].chars().count()
}

/// A tab goes to the next stop of four cells counted from its row's start — on a row a line
/// wrapped onto too.
#[test]
fn tabs_go_to_stops_counted_from_the_rows_start() {
    let cols = columns(600.0, 400.0);
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("{}\tz\n\tq\n", "y".repeat(cols + 3));
    let mut win = viewer(tmp.path(), "tabs.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "yyy");
    let rows = rows_at(&drv, x);
    assert_eq!(
        rows[..3],
        ["y".repeat(cols), "yyy z".to_string(), "    q".to_string()]
    );
}

/// A word that ends exactly at the right edge ends its row; the space after it, which does not
/// fit, is not carried over: the next row starts with the next word.
#[test]
fn a_word_ending_at_the_edge_drops_the_space_after_it() {
    let cols = columns(600.0, 400.0);
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("{} next words\n", "b".repeat(cols));
    let mut win = viewer(tmp.path(), "edge.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "bbb");
    assert_eq!(
        rows_at(&drv, x),
        ["b".repeat(cols), "next words".to_string()]
    );
}

/// Inside a line longer than the window the status row says how far into the line the top row
/// is; the scroll bar's thumb goes by bytes (rows cannot be counted without reading it all).
#[test]
fn the_status_and_the_thumb_tell_where_in_a_long_line() {
    let tmp = tempfile::tempdir().unwrap();
    let long: String = (0..20_000u32).map(|i| format!("{i:05} ")).collect();
    let mut win = viewer(tmp.path(), "far.txt", &format!("first\n{long}\nlast\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    // The status row is drawn before the page is read: it shows it on the next pass.
    drv.idle(&mut win, 1);
    assert!(
        drv.find_text("lines 1–2 of 3").is_some(),
        "{:?}",
        drv.text_starting_with("line")
    );
    assert_eq!(win.thumb(), Some(0.0));
    for _ in 0..40 {
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    }
    let status = drv.text_starting_with("line 2 (").expect("where in line 2");
    let pct: u64 = status["line 2 (".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        pct > 0 && pct < 100 && status.ends_with("% of it) of 3"),
        "{status}"
    );
    let thumb = win.thumb().unwrap();
    assert!(thumb > 0.0 && thumb < 1.0, "{thumb}");
    drv.key(&mut win, Key::End, crate::gui::testkit::command(false));
    assert_eq!(win.thumb(), Some(1.0));
}

/// `SOPEN_STATE` `at=OFFSET` (a receipt): the viewer opens with the row that byte is in at the
/// top, deep inside a long line too.
#[test]
fn a_viewer_can_open_at_a_byte_deep_in_a_long_line() {
    let tmp = tempfile::tempdir().unwrap();
    let long: String = (0..100_000u32).map(|i| format!("{i:06} ")).collect();
    let mut win = viewer(tmp.path(), "deep.txt", &format!("first\n{long}\nlast\n"));
    win.jump = Some(Jump::Byte(6 + 7 * 50_000 + 3));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.idle(&mut win, 1);
    let first = drv
        .texts()
        .into_iter()
        .filter(|(_, _, t)| t.contains("0499") || t.contains("0500"))
        .min_by(|a, b| a.0.top().total_cmp(&b.0.top()))
        .map(|(_, _, t)| t)
        .expect("rows of the long line");
    assert!(
        first.contains("050000"),
        "the top row holds the byte: {first}"
    );
    assert_eq!(win.top, 1);
    let status = drv.text_starting_with("line 2 (").unwrap_or_default();
    assert!(
        ["line 2 (49 % of it) of 3", "line 2 (50 % of it) of 3"].contains(&status.as_str()),
        "{status}"
    );
}

/// A deleted file keeps its wrapped page through a resize and W: nothing is read any more, and
/// the page stays as it was read.
#[test]
fn a_deleted_file_keeps_its_wrapped_page_through_a_resize_and_w() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..400).map(|i| format!("{i:04} ")).collect();
    let mut win = viewer(tmp.path(), "gone.txt", &format!("{line}\n"));
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "0000 0001");
    let rows = rows_at(&drv, x);
    std::fs::remove_file(tmp.path().join("gone.txt")).unwrap();
    later(&clock, 2.0);
    drv.idle(&mut win, 3);
    assert!(win.status.text.contains("deleted"), "{}", win.status.text);
    let reads = win.doc.page_reads();
    drv.resize(420.0, 300.0);
    drv.idle(&mut win, 2);
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    assert_eq!(win.doc.page_reads(), reads, "nothing read");
    assert_eq!(rows_at(&drv, x), rows, "the page as it was");
}

/// Narrowed while inside a long line, the top goes back to the start of the row it is in at the
/// new width: going down and back up again comes back to it.
#[test]
fn a_resize_inside_a_long_line_keeps_the_top_on_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let long: String = (0..50_000u32).map(|i| format!("{i:05} ")).collect();
    let mut win = viewer(tmp.path(), "narrow.txt", &format!("{long}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "00000 00001");
    for _ in 0..50 {
        drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    }
    let wide = rows_at(&drv, x)[0].clone();
    drv.resize(437.0, 400.0);
    drv.idle(&mut win, 2);
    let top = rows_at(&drv, x)[0].clone();
    assert_ne!(top, wide, "wrapped anew");
    let word = wide.split(' ').find(|w| w.len() == 5).unwrap();
    assert!(
        word.parse::<u32>().unwrap() > 1000,
        "deep in the line: {wide}"
    );
    assert!(
        rows_at(&drv, x).concat().contains(word),
        "the same place: {word} in {:?}",
        rows_at(&drv, x)
    );
    for key in [Key::ArrowDown, Key::PageDown, Key::PageUp, Key::ArrowUp] {
        drv.key(&mut win, key, Modifiers::NONE);
    }
    assert_eq!(rows_at(&drv, x)[0], top, "back to the same row");
}

/// Wrapped, the wheel scrolls by rows: a notch (three lines' worth) moves three rows, inside a
/// line too.
#[test]
fn the_wheel_scrolls_by_rows_when_wrapped() {
    let tmp = tempfile::tempdir().unwrap();
    let long: String = (0..2000u32).map(|i| format!("{i:05} ")).collect();
    let mut win = viewer(tmp.path(), "wheel.txt", &format!("{long}\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "00000 00001");
    let before = rows_at(&drv, x);
    drv.wheel(&mut win, Pos2::new(300.0, 200.0), -1.0);
    assert_eq!(rows_at(&drv, x)[0], before[3], "three rows down");
    drv.wheel(&mut win, Pos2::new(300.0, 200.0), 1.0);
    assert_eq!(rows_at(&drv, x), before, "and back");
}

/// A line ending in CR LF shows no CR when wrapped (as unwrapped).
#[test]
fn a_crlf_line_shows_no_cr_when_wrapped() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "w ".repeat(100);
    let mut win = viewer(tmp.path(), "dos.txt", &format!("{long}end\r\nnext\r\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "w w");
    let rows = rows_at(&drv, x);
    assert!(rows.iter().any(|r| r.ends_with("end")), "{rows:?}");
    assert_eq!(rows.last().map(String::as_str), Some("next"));
    assert!(rows.iter().all(|r| !r.contains('♪')), "{rows:?}");
}

/// Thousands of short lines walked through wrapped (more line places than wrapping remembers at
/// once): never mistaken for a changed file — no re-read — and the place reached is shown.
#[test]
fn walking_past_thousands_of_lines_is_not_a_change() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..6000).map(|i| format!("row {i:04}\n")).collect();
    let mut win = viewer(tmp.path(), "many.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    win.moved = 5000;
    drv.idle(&mut win, 2);
    assert_eq!(win.reindexes, 0, "{}", win.status.text);
    assert!(!win.waiting);
    assert!(drv.find_text("row 5000").is_some(), "{}", win.top);
}

/// A re-read that lands while lines are not wrapped is what W wraps afterwards: nothing found
/// through the old index is used for the new one.
#[test]
fn a_reread_while_unwrapped_is_what_w_wraps() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(
        tmp.path(),
        "swap.txt",
        format!("{}\n", "old ".repeat(200)).as_bytes(),
    );
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "swap".into(),
        "swap.txt",
        vec![],
    );
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    let fresh = write(tmp.path(), ".swap.tmp", b"a\nnew line two\n");
    std::fs::rename(&fresh, &path).unwrap();
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 2);
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    assert!(
        drv.find_text("new line two").is_some(),
        "{}",
        win.status.text
    );
    assert!(!drv.texts().iter().any(|(_, _, t)| t.contains("old")));
}

/// Several tabs on one wrapped row each go to their own stop: a row that fits shows the same
/// columns as the unwrapped view (nested indentation, tab-separated columns).
#[test]
fn several_tabs_on_a_row_each_go_to_their_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let lines = ["\t\tfoo\tbar", "a\tb\tc", "ab\t\tcd\te"];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let mut win = viewer(tmp.path(), "tabs.tsv", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "a   b");
    let rows = rows_at(&drv, x);
    let want: Vec<String> = lines
        .iter()
        .map(|l| {
            let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
            expand_tabs(vec![(plain, l.to_string())])
                .into_iter()
                .map(|(_, t)| t)
                .collect()
        })
        .collect();
    assert_eq!(rows, want);
    assert_eq!(rows[0], "        foo bar");
}
