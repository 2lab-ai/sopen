//! The text viewer: paging reads only the page on screen (a sparse line index, never the whole
//! file), long lines and bad bytes are shown without breaking, and the window highlights, numbers
//! and pages through real key events.

use super::*;
use crate::gui::chrome;
use crate::gui::testkit::Driver;
use eframe::egui::{Key, Modifiers};

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

/// T1: highlighting is bounded per frame and resumes. With a budget of 100 parsed lines per
/// frame, jumping to the end of 3000 lines of Rust does at most 100 lines' work in a frame (the
/// page shows plain meanwhile) and finishes over the following frames.
#[test]
fn highlighting_yields_each_frame_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..3000)
        .map(|i| format!("fn f{i}() {{ let x = {i}; }}\n"))
        .collect();
    let mut win = viewer(tmp.path(), "many.rs", &body);
    win.budget_lines = 100;
    let mut drv = Driver::new(800.0, 500.0);
    drv.idle(&mut win, 1);
    drv.until(&mut win, 10, |w| w.ready());
    drv.key(&mut win, Key::End, Modifiers::NONE);
    let mut frames = 0;
    let mut before = win.parsed();
    while !win.ready() {
        drv.idle(&mut win, 1);
        let now = win.parsed();
        assert!(now - before <= 100, "a frame parsed {} lines", now - before);
        before = now;
        frames += 1;
        assert!(frames < 1000, "never finished");
    }
    assert!(
        frames >= 20,
        "finished in {frames} frames: the work was not spread out"
    );
    let last = "fn f2999() { let x = 2999; }";
    let colors = drv.colors_of(last).expect("the last line is shown");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
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

/// T3: the file changes under the viewer (it reads it live, a page at a time): growing,
/// truncated, rewritten in place. Reads never panic, and the change is noticed.
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
