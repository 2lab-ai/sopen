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
    assert_eq!(
        win.line,
        Some(visible - 1),
        "a page keeps one line of context"
    );
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    assert_eq!(win.line, Some(visible));
    drv.key(&mut win, Key::PageUp, Modifiers::NONE);
    assert_eq!(win.line, Some(1));
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

/// The most word characters (letters, digits, `_`) in a row in `text`.
fn longest_word_run(text: &str) -> usize {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|w| w.chars().count())
        .max()
        .unwrap_or(0)
}

/// Whether the one pass finds anything to shorten in `line`.
fn finds(line: &str, shape: Shape) -> bool {
    match shape {
        Shape::Code => code_shortens(line, 0..line.len()),
        Shape::Yaml => yaml_shortens(line),
    }
}

/// What the one pass finds is what the shortening shortens: on lines made of names, numbers,
/// runs of every length about [`RUN_MAX`], chains about it (whole, parted by a number, two
/// separators joined), spaces, YAML's stops and letters of several bytes, put together at random
/// (the same each run), the pass finds something in a line, in code and in YAML, just when
/// building its shortened form returns `Some`: it never misses a line to shorten, and never
/// sends an ordinary one to be built (and allocate; `tests/highlight_alloc.rs` counts that).
#[test]
fn the_pass_that_finds_a_shortening_agrees_with_making_it() {
    let mut seed: u64 = 0x5eed;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize % n
    };
    let pieces = [
        ".", ",", "-", "$", " ", "  ", "\t", ":", "#", "[", "\"", "\\", "(", ";", "é", "漢",
    ];
    // Each part of a chain shorter than RUN_MAX, longer together: parted by a number, or two
    // chains of two separators side by side — nothing to shorten; one more name, and there is.
    let ab = |sep: &str, n: usize| format!("ab{sep}").repeat(n);
    for (line, shortened) in [
        (format!("v = {}7x.{}ab;", ab(".", 30), ab(".", 30)), false),
        (format!("v = {}ab{};", ab(".", 40), ",ab".repeat(40)), false),
        // 43 names are 128 characters; 44, 131.
        (format!("v = {}ab;", ab(".", 42)), false),
        (format!("v = {}ab;", ab(".", 43)), true),
    ] {
        for shape in [Shape::Code, Shape::Yaml] {
            assert_eq!(Shortened::made(&line, shape).is_some(), shortened, "{line}");
            assert_eq!(finds(&line, shape), shortened, "{shape:?}: {line}");
        }
    }
    let (mut shortened, mut lines) = (0, 0);
    for _ in 0..3000 {
        let mut line = String::new();
        let target = [40, 130, 300, 700][next(4)];
        while line.chars().count() < target {
            match next(6) {
                // A name, sometimes long, sometimes a number.
                0..=2 => {
                    let len = if next(8) == 0 {
                        [127, 128, 129, 200][next(4)]
                    } else {
                        [1, 3, 8, 60][next(4)]
                    };
                    let first = if next(5) == 0 { "7" } else { "n" };
                    line.push_str(first);
                    line.push_str(&"x".repeat(len - 1));
                }
                // A chain of names about the length that is shortened; or two parts of one, each
                // shorter and longer together, parted by a number or joined by two separators.
                3 => {
                    let seps = [".", ",", "-", "$", " "];
                    let sep = seps[next(5)];
                    let unit = format!("{}{sep}", "ab".repeat(1 + next(3)));
                    match next(3) {
                        0 => line.push_str(&unit.repeat(10 + next(40))),
                        1 => {
                            line.push_str(&unit.repeat(10 + next(15)));
                            line.push_str(&format!("7x{sep}"));
                            line.push_str(&unit.repeat(10 + next(15)));
                        }
                        _ => {
                            let other = format!("{}{}", "ab".repeat(1 + next(3)), seps[next(5)]);
                            line.push_str(&unit.repeat(10 + next(15)));
                            line.push_str(&other.repeat(10 + next(15)));
                        }
                    }
                }
                _ => line.push_str(pieces[next(pieces.len())]),
            }
        }
        for shape in [Shape::Code, Shape::Yaml] {
            let made = Shortened::made(&line, shape).is_some();
            assert_eq!(finds(&line, shape), made, "{shape:?}: {line}");
            shortened += usize::from(made);
            lines += 1;
        }
    }
    // Both answers came up often.
    assert!(
        shortened > lines / 10 && shortened < lines * 9 / 10,
        "{shortened} of {lines}"
    );
}

/// The most characters in a row in `text` that `part` takes.
fn longest_where(text: &str, part: impl Fn(char) -> bool) -> usize {
    let (mut longest, mut now) = (0, 0);
    for c in text.chars() {
        now = if part(c) { now + 1 } else { 0 };
        longest = longest.max(now);
    }
    longest
}

/// The longest chain in `text`: word characters and the separators `.` `,` `-` `$`.
fn longest_chain(text: &str) -> usize {
    longest_where(text, |c| c.is_alphanumeric() || "_.,-$".contains(c))
}

/// The longest stretch of `text` with none of YAML's `: # [ ] { } " ' \`.
fn longest_yaml_span(text: &str) -> usize {
    longest_where(text, |c| !":#[]{}\"'\\".contains(c))
}

/// A highlighter for files named `*.ext`, and what its parser was handed: the longest word run,
/// and how many lines.
fn counting_highlighter(ext: &str) -> (Highlight, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    measuring_highlighter(ext, longest_word_run)
}

/// A highlighter for files named `*.ext`, and what its parser was handed: the most `measure`
/// gives of any line, and how many lines.
fn measuring_highlighter(
    ext: &str,
    measure: fn(&str) -> usize,
) -> (Highlight, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let syntax = syntaxes().find_syntax_by_extension(ext).unwrap();
    let mut hl = Highlight::new(syntax, Color32::WHITE, u64::MAX).unwrap();
    let (longest, lines) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (seen, count) = (longest.clone(), lines.clone());
    hl.hook = Some(Arc::new(move |text: &str| {
        seen.fetch_max(measure(text), Ordering::Relaxed);
        count.fetch_add(1, Ordering::Relaxed);
    }));
    (hl, longest, lines)
}

/// What `ext`'s parser is handed of `lines`: the most `measure` gives of any line (every line
/// handed over, the page highlighted).
fn handed(ext: &str, lines: &[String], measure: fn(&str) -> usize) -> usize {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let path = write(tmp.path(), &format!("shape.{ext}"), body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    let (mut hl, longest, count) = measuring_highlighter(ext, measure);
    let runs = hl
        .page(&mut doc, 0, lines.len(), &|| false)
        .unwrap()
        .unwrap();
    assert_eq!(runs.len(), lines.len());
    assert_eq!(
        count.load(Ordering::Relaxed),
        lines.len(),
        "every line was parsed"
    );
    longest.load(Ordering::Relaxed)
}

/// A chain — short word runs joined by one separator, `x.x.x`, `x,x,x`, `x$x$x`, `x-x-x` —
/// costs the patterns that walk a dotted name, an argument list or an identifier with `$` in it
/// the square of its length (1,000 characters: Python's dotted `x.` 28 ms, Java's `x$` 61 ms,
/// Go's `x,` 10 ms; their backtrack limit from about 1,000 or 2,000). However long a chain is,
/// the parser is handed it shortened: longer than [`RUN_MAX`], shorter than twice it.
#[test]
fn the_parser_is_never_handed_a_long_chain() {
    let chain = |unit: &str| format!("{}x", unit.repeat(500));
    for (ext, line) in [
        ("py", format!("v = {}", chain("x."))),
        ("java", format!("v = {};", chain("x."))),
        ("java", format!("v = {};", chain("x$"))),
        ("js", format!("let v = {};", chain("x."))),
        ("go", format!("f({})", chain("x,"))),
        ("md", format!("see {} here", chain("x-"))),
    ] {
        let longest = handed(ext, &[line, "x = 1".to_string()], longest_chain);
        assert!(
            RUN_MAX < longest && longest < 2 * RUN_MAX,
            "{ext}: the parser was handed a chain of {longest}"
        );
    }
}

/// YAML looks for a second key on every line — `(?=… ( [^\s:] | :\S | \s+(?![#\s]) )* \s*:(\s|$))`
/// scans the rest of the line from each place a key could start — so any long value costs the
/// square of its length: plain, quoted or in a list, 5–7 ms at 500 characters, past the
/// backtrack limit from 1,000. Its long stretches without `: # [ ] { } " ' \` reach the parser
/// shortened: longer than [`RUN_MAX`], shorter than twice it.
#[test]
fn a_long_yaml_value_reaches_the_parser_shortened() {
    let prose = "Lorem ipsum dolor sit amet, consectetur (adipiscing) elit. ".repeat(17);
    let lines = [
        format!("description: {prose}"),
        format!("- {prose}"),
        format!("key: \"{prose}\""),
        format!("  - name: {prose}"),
        // One unit, no spaces: shortened as code is (a chain, a run).
        format!("key: {}x", "x-".repeat(500)),
        format!("key: {}", "0123456789abcdef".repeat(60)),
        "after: 1".to_string(),
    ];
    let longest = handed("yaml", &lines, longest_yaml_span);
    assert!(
        RUN_MAX < longest && longest < 2 * RUN_MAX,
        "the parser was handed a stretch of {longest}"
    );
}

/// A run of word characters costs the grammars' patterns the square of its length (on a pattern
/// fancy-regex runs itself, a search tries every start in the run and each try runs to its end):
/// a comment of 1,000 `x`s took Rust's 20 ms, 1,400 or more hit fancy-regex's backtrack limit.
/// However long a file's runs are, the parser is handed every line, and a long run shortened:
/// longer than [`RUN_MAX`] (the longest handed over as it is), shorter than twice it.
#[test]
fn the_parser_is_never_handed_a_long_word_run() {
    let tmp = tempfile::tempdir().unwrap();
    let run = "x".repeat(300);
    let hex = "f".repeat(300);
    let body = format!(
        "fn a() {{}}\nlet v = 1; // {run}\nlet s = \"{run}\";\n{run}::new(0x{hex}u64);\nfn b() {{}}\n"
    );
    let path = write(tmp.path(), "runs.rs", body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    let (mut hl, longest, lines) = counting_highlighter("rs");
    let runs = hl.page(&mut doc, 0, 5, &|| false).unwrap().unwrap();
    assert_eq!(runs.len(), 5);
    assert_eq!(lines.load(Ordering::Relaxed), 5, "every line was parsed");
    let longest = longest.load(Ordering::Relaxed);
    assert!(
        RUN_MAX < longest && longest < 2 * RUN_MAX,
        "the parser was handed a run of {longest} word characters"
    );
}

/// Each character of `lines` and its colour, the lines parsed whole, in order, from the top.
fn whole_line_colours(ext: &str, lines: &[String]) -> Vec<Vec<(char, Color32)>> {
    let syntax = syntaxes().find_syntax_by_extension(ext).unwrap();
    let highlighter = Highlighter::new(chrome::syntax_theme());
    let mut parse = ParseState::new(syntax);
    let mut colour = HighlightState::new(&highlighter, ScopeStack::new());
    lines
        .iter()
        .map(|line| {
            let ops = parse.parse_line(line, syntaxes()).unwrap();
            HighlightIterator::new(&mut colour, &ops, line, &highlighter)
                .flat_map(|(style, text)| {
                    let c = style.foreground;
                    let c = Color32::from_rgb(c.r, c.g, c.b);
                    text.chars().map(move |ch| (ch, c))
                })
                .collect()
        })
        .collect()
}

/// For each character of `line`, whether its shortened form keeps it (every one, when no run in
/// it is shortened).
fn kept_characters(line: &str, shape: Shape) -> Vec<bool> {
    let Some(short) = Shortened::of(line, shape) else {
        return vec![true; line.chars().count()];
    };
    let mut kept = vec![false; line.len()];
    for (i, &(text, at)) in short.pieces.iter().enumerate() {
        let end = short.pieces.get(i + 1).map_or(short.text.len(), |p| p.0);
        kept[at..at + (end - text)].fill(true);
    }
    line.char_indices().map(|(i, _)| kept[i]).collect()
}

/// Each character of `lines` and its colour as the viewer's highlighter gives it (a file named
/// `*.ext`, each line ended by `end`), its long runs shortened: handed over at fewer than twice
/// [`RUN_MAX`] characters, and over [`RUN_MAX`] just when a line has a run that long.
fn highlighted(ext: &str, lines: &[String], end: &str) -> Vec<Vec<(char, Color32)>> {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = lines.iter().map(|l| format!("{l}{end}")).collect();
    let path = write(tmp.path(), &format!("runs.{ext}"), body.as_bytes());
    let mut doc = TextDoc::open(&path).unwrap();
    let (mut hl, longest, _) = counting_highlighter(ext);
    let runs = hl
        .page(&mut doc, 0, lines.len(), &|| false)
        .unwrap()
        .unwrap();
    let longest = longest.load(Ordering::Relaxed);
    let long = lines.iter().any(|l| longest_word_run(l) > RUN_MAX);
    assert!(
        longest < 2 * RUN_MAX && (longest > RUN_MAX) == long,
        "{ext}: handed a run of {longest}"
    );
    assert_eq!(runs.len(), lines.len());
    runs.iter()
        .map(|line| {
            line.iter()
                .flat_map(|(c, text)| text.chars().map(move |ch| (ch, *c)))
                .collect()
        })
        .collect()
}

/// The viewer highlights `lines` (a file named `*.ext`, each line ended by `end`) with every
/// character coloured as the lines parsed whole give it.
fn assert_colours_of_whole_lines(ext: &str, lines: &[String], end: &str) {
    let got = highlighted(ext, lines, end);
    let whole = whole_line_colours(ext, lines);
    for (i, (got, want)) in got.iter().zip(&whole).enumerate() {
        assert_eq!(got.len(), want.len(), "{ext} line {i}: its characters");
        if let Some(at) = (0..got.len()).find(|&k| got[k] != want[k]) {
            panic!(
                "{ext} line {i}, character {at}: {:?}, parsed whole {:?}",
                got[at], want[at]
            );
        }
    }
}

/// A shortened run is coloured as the whole one: every character of every line — the run, what
/// follows it on its line, the lines after it — has the colour the line parsed whole gives it.
/// Runs 200 long: past [`RUN_MAX`], and far from the backtrack limit — parsing these lines and
/// those of the tests below whole ends no fancy-regex search in an error (counted once, with a
/// syntect that counts them: 0) — so the colours parsed whole are exact. A prefix and suffix on
/// a run (`0x…u64`, `…_t`, `t_t…_t`), capitals with a small letter in the middle — in a middle of
/// a few stretches and of many —, letters of two bytes, two runs on a line, and strings, a raw
/// string and a heredoc (whose delimiter is a long run) open across lines.
#[test]
fn a_line_with_long_word_runs_has_the_colours_of_the_whole_line() {
    let x = "x".repeat(200);
    let y = "y".repeat(200);
    let cases: [(&str, Vec<String>); 5] = [
        (
            "rs",
            vec![
                format!("let v = 1; // {x}"),
                format!("let s = \"{x}\";"),
                format!("{x}::new(0x{}u64);", "f".repeat(200)),
                format!("let k = {}a{};", "A".repeat(100), "A".repeat(100)),
                // The small letter past the stretches kept: only the rest of the middle has it.
                format!("let k = {}a{};", "AB".repeat(60), "AB".repeat(40)),
                format!("let k = {};", "A".repeat(201)),
                format!("type {x}_t = u8;"),
                // A suffix made of characters the run already had: only its tail keeps it.
                format!("let t = {}t;", "t_".repeat(100)),
                format!("let e = \"{}\"; // {y}", "é".repeat(200)),
                format!("let r = r#\"{x}"),
                "\"#; fn f() { let a = \"b\"; }".to_string(),
            ],
        ),
        (
            "js",
            vec![
                format!("{x}.prototype.f = function() {{}};"),
                format!("let t = `{x}"),
                "`; let u = 'v';".to_string(),
            ],
        ),
        (
            "py",
            vec![
                format!("s = '''{x}"),
                "''' ; def f(a): pass".to_string(),
                format!("{x}(1)  # {y}"),
            ],
        ),
        (
            "sh",
            vec![
                format!("cat <<{}", "E".repeat(200)),
                format!("body {y} \"quote"),
                "E".repeat(200),
                "echo \"$HOME\" # done".to_string(),
            ],
        ),
        (
            "md",
            vec![format!("Some **{x}** text `{y}`"), format!("{x}@{y}.com")],
        ),
    ];
    for (ext, lines) in cases {
        assert_colours_of_whole_lines(ext, &lines, "\n");
    }
}

/// Shortening at its edges, each character against the lines parsed whole: runs of exactly
/// [`RUN_MAX`] (handed over as they are) and one more (shortened); CRLF line ends; characters of
/// three and four bytes on both sides of where the ends are cut; an emoji (no word character)
/// against a run; and a character met once in the middle that decides the token — an exponent's
/// `e`, a `z` that ends a hex number — with a token right after the run.
#[test]
fn shortening_at_its_edges_keeps_the_colours_of_the_whole_line() {
    let x = |n: usize| "x".repeat(n);
    // 漢 and 𠀀 are the run's 32nd and 33rd characters; 𝐀 and 漢 its 33rd and 32nd from the end.
    let cut = format!("{}漢𠀀{}𝐀漢{}", x(31), x(134), x(31));
    let float = format!("1{}e{}", "0".repeat(100), "0".repeat(99));
    let hex = format!("0x{}z{}", "F".repeat(100), "F".repeat(99));
    let rust = [
        format!("let v = 1; // {}", x(128)),
        format!("let s = \"{}\";", x(129)),
        format!("{}::new(1); {}::new(2);", x(128), x(129)),
        format!("let s = \"{cut}\"; // {cut}"),
        format!("let {cut} = 1;"),
        format!("// {}😀{}", x(200), x(200)),
        format!("let s = \"😀{}😀\";", x(200)),
        format!("let f = {float} + 1; // after"),
        format!("let h = {hex} + 1; // after"),
        "fn after() { let a = \"b\"; }".to_string(),
    ];
    for end in ["\n", "\r\n"] {
        assert_colours_of_whole_lines("rs", &rust, end);
    }
    for (ext, line) in [
        ("js", format!("let f = {float} + 1; // after")),
        ("py", format!("f = {float} + 1  # after")),
        ("c", format!("double f = {float} + 1; // after")),
        ("java", format!("double f = {float} + 1; // after")),
        ("js", format!("{}.prototype.f = function() {{}};", x(129))),
        ("md", format!("{cut} **bold** {} `code`", x(129))),
    ] {
        assert_colours_of_whole_lines(ext, &[line, "x = 1".to_string()], "\n");
    }
}

/// The one difference in colour the `Shortened` doc states for what a shortened line leaves
/// out, on line `at` of `lines` (a file named `*.ext`): parsed whole, its colour changes inside
/// the part left out; in the viewer each left-out character takes the colour of the character
/// kept before it, every kept character has the colour parsed whole, and so has every other line
/// — the lines ended by LF, and by CRLF.
fn assert_left_out_take_the_colour_before(ext: &str, lines: &[String], at: usize, shape: Shape) {
    for end in ["\n", "\r\n"] {
        assert_left_out_part_colour(ext, lines, at, shape, end);
    }
}

fn assert_left_out_part_colour(ext: &str, lines: &[String], at: usize, shape: Shape, end: &str) {
    let got = highlighted(ext, lines, end);
    let whole = whole_line_colours(ext, lines);
    let kept = kept_characters(&lines[at], shape);
    assert!(
        (1..kept.len()).any(|k| !kept[k] && whole[at][k].1 != whole[at][k - 1].1),
        "{ext}: parsed whole, the colour changes inside the part left out"
    );
    let (line, want) = (&got[at], &whole[at]);
    assert_eq!(line.len(), want.len());
    let mut before = None;
    for (k, (&(ch, colour), &whole)) in line.iter().zip(want).enumerate() {
        assert_eq!(ch, whole.0, "{ext}: character {k}");
        if kept[k] {
            assert_eq!(
                colour, whole.1,
                "{ext}: kept character {k}: as parsed whole"
            );
            before = Some(colour);
        } else {
            let before = before.expect("a line starts with kept characters");
            assert_eq!(
                colour, before,
                "{ext}: left-out character {k}: the kept one's before it"
            );
        }
    }
    assert_ne!(
        line, want,
        "{ext}: the left-out part differs from the line parsed whole"
    );
    for (i, (got, whole)) in got.iter().zip(&whole).enumerate() {
        if i != at {
            assert_eq!(got, whole, "{ext} line {i}: as parsed whole");
        }
    }
}

/// A grammar that colours inside a word: CSS colours the digits in one, so its scope changes
/// inside the part of a varied run the shortened form leaves out.
#[test]
fn inside_a_word_only_the_left_out_part_of_a_run_differs() {
    let hex: String = (0..150)
        .map(|i| char::from_digit(i * 7 % 16, 16).unwrap())
        .collect();
    let lines = [
        format!("a {{ color: #{hex}g{}; }}", "a".repeat(59)),
        "b { margin: 10px; }".to_string(),
    ];
    assert_left_out_take_the_colour_before("css", &lines, 0, Shape::Code);
}

/// A shortened chain is coloured as the whole one: every character of every line — the chain,
/// the token after it, the lines after it — has the colour the lines parsed whole give it, in the
/// grammars whose patterns cost a chain's square (Python, Java, JavaScript, Go, Markdown) and
/// in others that name things in chains. Chains of about 300 characters: past [`RUN_MAX`], short
/// of the backtrack limit (parsing these lines whole ends no fancy-regex search in an error —
/// counted once, with a syntect that counts them: 0). Names of one letter and of several, one
/// separator a chain, a chain that ends in a call, an assignment of a function, `:=`, and one in
/// a string, its closing quote and code after it; the lines ended by LF, and by CRLF.
#[test]
fn long_chains_have_the_colours_of_the_whole_line() {
    let chain = |unit: &str, n: usize| format!("{}x", unit.repeat(n));
    let names = |sep: &str, n: usize| {
        let names = ["alpha", "beta", "gamma_2", "Delta", "épsilon"];
        (0..n)
            .map(|i| names[i % names.len()])
            .collect::<Vec<_>>()
            .join(sep)
    };
    let cases: [(&str, Vec<String>); 11] = [
        (
            "py",
            vec![
                format!("v = {} + 1  # after", chain("x.", 150)),
                format!("s = \"{}\" + v.attr  # after", chain("x.", 150)),
                format!("v = {}(1)", names(".", 45)),
                format!("f({})  # after", chain("x,", 150)),
                "def g(a): return a".to_string(),
            ],
        ),
        (
            "java",
            vec![
                format!("v = {}; // after", chain("x.", 150)),
                format!("v = {};", chain("x$", 150)),
                format!("{}<String> v = null;", names(".", 45)),
                "int g() { return 1; }".to_string(),
            ],
        ),
        (
            "js",
            vec![
                format!("let v = {}; // after", chain("x.", 150)),
                format!("let s = \"{}\" + v.length; // after", chain("x.", 150)),
                format!("let v = {}.f(1);", names(".", 45)),
                "let u = 'v';".to_string(),
            ],
        ),
        (
            "go",
            vec![
                format!("f({}) // after", chain("x,", 150)),
                format!("{} := g()", names(",", 45)),
                "func h() {}".to_string(),
            ],
        ),
        (
            "md",
            vec![
                format!("see {} **bold** after", chain("x-", 150)),
                format!("and {} `code`", names(".", 45)),
            ],
        ),
        (
            "rs",
            vec![
                format!("let v = {}; // after", chain("x.", 150)),
                "fn f() {}".to_string(),
            ],
        ),
        (
            "c",
            vec![
                format!("int v = {}; // after", chain("x-", 150)),
                "int f(void);".to_string(),
            ],
        ),
        (
            "cs",
            vec![
                format!("var v = {}; // after", chain("x.", 150)),
                "class C {}".to_string(),
            ],
        ),
        (
            "sh",
            vec![
                format!("echo {} # after", chain("x.", 150)),
                "echo \"$HOME\"".to_string(),
            ],
        ),
        (
            "lua",
            vec![
                format!("v = {} -- after", chain("x-", 150)),
                "w = 'v'".to_string(),
            ],
        ),
        (
            "hs",
            vec![
                format!("v = {} -- after", chain("x.", 150)),
                "w = 1".to_string(),
            ],
        ),
    ];
    for (ext, lines) in cases {
        for end in ["\n", "\r\n"] {
            assert_colours_of_whole_lines(ext, &lines, end);
        }
    }
}

/// Where a grammar colours the names of a chain apart from what joins them — SQL's `a.b.c`,
/// TeX's `$`, which turns math on and off, JavaScript's `a.b.c.f = function`, PHP's constants
/// `A-B-C`, a list of `true,false,null` in JSON or JavaScript — the left-out units take the
/// colour of the character kept before them: the difference the `Shortened` doc states. The
/// rest is as parsed whole, the line after too: an even number of `$` is left out, so after the
/// chain TeX is in text or in math as it is (`12 \alpha` is coloured apart in math).
#[test]
fn a_chain_coloured_name_by_name_differs_only_in_its_left_out_units() {
    let chain = |unit: &str| format!("{}x", unit.repeat(150));
    let sql = [
        format!("SELECT {} FROM t", chain("x.")),
        "SELECT 1 FROM u".to_string(),
    ];
    assert_left_out_take_the_colour_before("sql", &sql, 0, Shape::Code);
    // 150 and 151 units: whichever count a chain has, an even number is left out.
    for units in [150, 151] {
        let tex = [
            // In math, digits and `\\alpha` are coloured apart: after the chain, they show which.
            format!("Some {}x after 12 \\alpha", "x$".repeat(units)),
            "more text, then $y$".to_string(),
        ];
        assert_left_out_take_the_colour_before("tex", &tex, 0, Shape::Code);
    }
    let literals = "true,false,null,".repeat(20) + "true";
    let json = [format!("{{\"a\": [{literals}]}}"), "{}".to_string()];
    assert_left_out_take_the_colour_before("json", &json, 0, Shape::Code);
    let js = [format!("let a = [{literals}];"), "let u = 'v';".to_string()];
    assert_left_out_take_the_colour_before("js", &js, 0, Shape::Code);
    let names: Vec<&str> = ["alpha", "beta", "gamma_2", "Delta", "épsilon"]
        .into_iter()
        .cycle()
        .take(45)
        .collect();
    let js = [
        format!("{}.f = function() {{}};", names.join(".")),
        "let u = 'v';".to_string(),
    ];
    assert_left_out_take_the_colour_before("js", &js, 0, Shape::Code);
    let php = [
        format!("<?php $v = {}; // after", chain("x-")),
        "$w = 1;".to_string(),
    ];
    assert_left_out_take_the_colour_before("php", &php, 0, Shape::Code);
}

/// A long YAML value is coloured as the whole one, every character of every line: plain prose
/// with its punctuation, in a list, quoted twice and once, under a key in a list, after an
/// anchor, before a comment, a block scalar's lines, a value of one long unit (a chain, a hex
/// word) and a long unit between two long stretches of prose. Values of 300–450 characters: YAML's
/// whole parse ends no search in an error there (counted, as above: 0); at 1,000 it does.
#[test]
fn long_yaml_values_have_the_colours_of_the_whole_line() {
    let prose =
        "Lorem ipsum dolor sit amet, consectetur (adipiscing) elit; sed-do eiusmod 42 true! ";
    let lines = [
        format!("description: {}", prose.repeat(5)),
        format!("- {}", prose.repeat(5)),
        format!("key: \"{}\"", prose.repeat(5)),
        format!("key: '{}it''s'", prose.repeat(5)),
        format!("  - name: {}", prose.repeat(4)),
        format!("key: &anchor {} *alias", prose.repeat(4)),
        format!("key: {} # a comment", prose.repeat(4)),
        "block: |".to_string(),
        format!("  {}", prose.repeat(5)),
        format!("  {}", prose.repeat(4)),
        format!("key: {}x", "x-".repeat(150)),
        format!("key: {}", "0123456789abcdef".repeat(19)),
        // A long unit between two long stretches of prose: each shortened apart.
        format!(
            "key: {} {} {}",
            prose.repeat(2),
            "0123456789abcdef".repeat(10),
            prose.repeat(2)
        ),
        "after: 1".to_string(),
    ];
    for end in ["\n", "\r\n"] {
        assert_colours_of_whole_lines("yaml", &lines, end);
    }
}

/// A flow collection in YAML is coloured item by item (its commas, numbers, `true`), so a long
/// one's left-out part takes the colour of the character kept before it: the difference the
/// `Shortened` doc states. The rest is as parsed whole, the next lines too — a collection closed
/// on a later line, and the mapping after it, included.
#[test]
fn a_long_yaml_flow_collection_differs_only_in_its_left_out_part() {
    let lines = [
        format!("key: [{}abc]", "abc, 123, true, ".repeat(20)),
        "after: 1".to_string(),
    ];
    assert_left_out_take_the_colour_before("yaml", &lines, 0, Shape::Yaml);
    // Opened on one line, shortened there, closed on a later one: the lines after it are as
    // parsed whole, the mapping after it too.
    let lines = [
        format!("key: [{}", "abc, 123, true, ".repeat(20)),
        "  def, 456]".to_string(),
        "other: value".to_string(),
        "- item".to_string(),
    ];
    assert_left_out_take_the_colour_before("yaml", &lines, 0, Shape::Yaml);
}

/// How chains and YAML stretches are shortened. A chain keeps whole units — a name with the
/// separator before it — from its start and its end, leaves out an even number of them, and is
/// longer than [`RUN_MAX`]: every two characters side by side in it are side by side in the
/// chain. A list of numbers, a chain of two separators and a chain with a long run in it are
/// not shortened as chains (that run is, as alone). YAML keeps a line's indentation and every
/// one of its stops.
#[test]
fn chains_are_shortened_by_whole_units_and_yaml_keeps_its_structure() {
    let chain_in = |text: &str| -> String {
        let longest = longest_chain(text);
        let chains: Vec<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || "_.,-$".contains(c)))
            .filter(|w| w.chars().count() == longest)
            .collect();
        chains[0].to_string()
    };
    for (sep, unit) in [(".", "x."), (",", "ab,"), ("$", "x$"), ("-", "q-")] {
        let whole = format!("{}x", unit.repeat(301));
        let line = format!("v = {whole};");
        let short = Shortened::of(&line, Shape::Code).expect("shortened");
        let kept = chain_in(&short.text);
        let n = kept.chars().count();
        assert!(RUN_MAX < n && n < 2 * RUN_MAX, "{unit}: {n}");
        let left_out = whole.matches(sep).count() - kept.matches(sep).count();
        assert_eq!(left_out % 2, 0, "{unit}: an even number of units left out");
        let pairs = |s: &str| -> std::collections::HashSet<(char, char)> {
            s.chars().zip(s.chars().skip(1)).collect()
        };
        assert!(pairs(&kept).is_subset(&pairs(&whole)), "{unit}: {kept}");
        assert!(short.text.starts_with("v = ") && short.text.ends_with("x;"));
    }
    assert!(Shortened::of(&format!("v = [{}1];", "12,".repeat(100)), Shape::Code).is_none());
    assert!(Shortened::of(&format!("v = {}x;", "x.x,".repeat(100)), Shape::Code).is_none());
    // Names of 100 characters: at most RUN_MAX and three of its units.
    let names = vec!["n".repeat(100); 9];
    let line = format!("v = {};", names.join("."));
    let n = longest_chain(&Shortened::of(&line, Shape::Code).expect("shortened").text);
    assert!(RUN_MAX < n && n <= RUN_MAX + 3 * 101, "long names: {n}");
    let long = format!("{}.prototype.f", "x".repeat(200));
    let short = Shortened::of(&long, Shape::Code).unwrap().text;
    let run = Shortened::of(&"x".repeat(200), Shape::Code).unwrap().text;
    assert_eq!(short, format!("{run}.prototype.f"));
    let prose = "lorem: ipsum # dolor [sit] {amet} \"x\" 'y' \\n, consectetur adipiscing elit. ";
    let line = format!(
        "      key: {}",
        "lorem ipsum, dolor sit amet. ".repeat(20) + prose
    );
    let short = Shortened::of(&line, Shape::Yaml).expect("shortened").text;
    assert!(short.starts_with("      key: "));
    // A long unit parts a stretch: the prose on each side of it is still shortened.
    let prose = "lorem ipsum, dolor sit amet. ".repeat(28);
    let parted = format!("key: {prose} {} {prose}", "0123456789abcdef".repeat(13));
    let shorter = Shortened::of(&parted, Shape::Yaml).expect("shortened").text;
    assert!(
        shorter.chars().count() < 4 * RUN_MAX,
        "{}",
        shorter.chars().count()
    );
    for stop in YAML_STOPS {
        assert_eq!(
            short.matches(stop).count(),
            line.matches(stop).count(),
            "{stop}"
        );
    }
}

/// A heredoc ends only on its own delimiter. A run shortened to the length of a real, shorter
/// delimiter (129 `x`s against 65, or 81: what its ends and middle would make without growing),
/// or to the text of a different run with the same ends and characters (`E…EFE…E` against
/// `E…FE…E`), must not close it early: the body after it stays in the heredoc, as it does parsed
/// whole. A long delimiter — of one character, and varied — still closes on its own line.
#[test]
fn a_shortened_run_never_ends_a_heredoc_it_is_not_the_delimiter_of() {
    for short in ["x".repeat(65), "x".repeat(81)] {
        for open in ["<<'D'", "<<D", "<<-D"] {
            let lines = [
                format!("cat {}", open.replace('D', &short)),
                "x".repeat(129),
                "echo still_heredoc \"$HOME\"".to_string(),
                short.clone(),
                "echo after \"$HOME\"".to_string(),
            ];
            assert_colours_of_whole_lines("sh", &lines, "\n");
        }
    }
    let delimiter = format!("{}F{}", "E".repeat(100), "E".repeat(99));
    let body = format!("{}F{}", "E".repeat(99), "E".repeat(100));
    let varied: String = (0..200)
        .map(|i| char::from_digit(i * 7 % 16, 16).unwrap())
        .collect();
    for (delimiter, body) in [
        (delimiter, body),
        ("E".repeat(200), format!("body {}", "y".repeat(200))),
        (varied, format!("body {}", "y".repeat(200))),
    ] {
        let lines = [
            format!("cat <<'{delimiter}'"),
            body,
            "echo still_heredoc \"$HOME\"".to_string(),
            delimiter,
            "echo after \"$HOME\"".to_string(),
        ];
        assert_colours_of_whole_lines("sh", &lines, "\n");
    }
}

/// The known difference the `Shortened` doc names last — a back-reference compares shortened
/// runs, so two runs over [`RUN_MAX`] with the same ends and stretches, each the same length
/// modulo [`RUN_STRETCH`], read as one — end to end. A heredoc opened by 200 `x`s, with a body
/// line of 216: parsed whole, that line, the next and the real 200 `x`s are the heredoc's; in
/// the viewer the 216 end it — coloured as its delimiter is —, the next line is shell code, and
/// so are the 200 `x`s (a command of that name); the line after them is as parsed whole again.
/// A body line of 201 or 300 `x`s shortens otherwise, and the file is as parsed whole.
#[test]
fn a_long_run_that_shortens_like_a_heredoc_delimiter_ends_it() {
    let delimiter = "x".repeat(200);
    let lines = |body: usize| {
        [
            format!("cat <<'{delimiter}'"),
            "x".repeat(body),
            "echo still_heredoc \"$HOME\"".to_string(),
            delimiter.clone(),
            "echo after \"$HOME\"".to_string(),
        ]
    };
    for body in [201, 300] {
        assert_colours_of_whole_lines("sh", &lines(body), "\n");
    }
    let lines = lines(216);
    let got = highlighted("sh", &lines, "\n");
    let whole = whole_line_colours("sh", &lines);
    // As code, each line on its own.
    let code = |line: &String| whole_line_colours("sh", std::slice::from_ref(line)).remove(0);
    assert_eq!(got[0], whole[0], "the opener: as parsed whole");
    let end = whole[3][0].1;
    assert!(whole[3].iter().all(|&(_, c)| c == end));
    assert!(
        whole[1].iter().all(|&(_, c)| c != end),
        "parsed whole, the 216 `x`s are the heredoc's body"
    );
    assert!(
        got[1].iter().map(|&(ch, _)| ch).eq(lines[1].chars()),
        "the body line is shown whole: {} characters of {}",
        got[1].len(),
        lines[1].chars().count()
    );
    assert!(
        got[1].iter().all(|&(_, c)| c == end),
        "in the viewer they end it, coloured as its delimiter: {:?}",
        got[1][0]
    );
    assert_ne!(
        whole[2],
        code(&lines[2]),
        "parsed whole, still in the heredoc"
    );
    assert_eq!(got[2], code(&lines[2]), "in the viewer, shell code");
    assert_ne!(whole[3], code(&lines[3]), "parsed whole, the delimiter");
    assert_eq!(got[3], code(&lines[3]), "in the viewer, a command");
    assert_eq!(got[4], whole[4], "after them: as parsed whole");
}

/// Shortened runs: each one longer than [`RUN_MAX`] — longer than any run handed over as it is —
/// and shorter than twice it, one run still. Runs that differ in where a lone character sits, in
/// a stretch's length (but by a multiple of [`RUN_STRETCH`]), or late in a varied middle, shorten
/// differently. What remains alike: runs with the same ends and the same stretches, each the
/// same length modulo [`RUN_STRETCH`] — 200 and 216 `x`s (the module doc's known difference).
#[test]
fn shortened_runs_are_longer_than_any_whole_run_and_tell_runs_apart() {
    assert!(
        Shortened::of(&"x".repeat(RUN_MAX), Shape::Code).is_none(),
        "a run of RUN_MAX is handed over as it is"
    );
    let short = |run: &str| Shortened::of(run, Shape::Code).expect("shortened").text;
    let varied = |n: u32| -> String {
        (0..n)
            .map(|i| char::from_digit(i * 7 % 16, 16).unwrap())
            .collect()
    };
    let han: String = (0..300)
        .map(|i| char::from_u32(0x4E00 + i).unwrap())
        .collect();
    let runs = [
        "x".repeat(129),
        "x".repeat(200),
        "x".repeat(4096),
        format!("{}F{}", "E".repeat(100), "E".repeat(99)),
        varied(129),
        varied(4000),
        "xy".repeat(100),
        format!("{}t", "t_".repeat(100)),
        format!("{}{}", "a".repeat(80), varied(120)),
        han,
    ];
    for run in &runs {
        let text = short(run);
        let n = text.chars().count();
        assert!(
            RUN_MAX < n && n < 2 * RUN_MAX && longest_word_run(&text) == n,
            "{run:.20}…: shortened to {n}"
        );
    }
    let near = |a: usize, b: usize| format!("{}F{}", "E".repeat(a), "E".repeat(b));
    assert_ne!(short(&near(100, 99)), short(&near(99, 100)));
    assert_ne!(short(&"x".repeat(200)), short(&"x".repeat(201)));
    let mut late = varied(200);
    late.replace_range(150..151, if &late[150..151] == "0" { "1" } else { "0" });
    assert_ne!(
        short(&varied(200)),
        short(&late),
        "a change late in a varied run"
    );
    assert_eq!(
        short(&"x".repeat(200)),
        short(&"x".repeat(216)),
        "the known difference"
    );
}

/// The patterns of the Makefile grammar the highlighter parses with ([`parsing`]) that walk
/// balanced parentheses (their source has the comment `level 0`).
fn makefile_paren_patterns() -> Vec<String> {
    let makefile = syntaxes().find_syntax_by_name("Makefile").unwrap();
    paren_patterns_of(parsing(makefile).0)
}

/// The Makefile grammar's patterns in `set` that walk balanced parentheses.
fn paren_patterns_of(set: &SyntaxSet) -> Vec<String> {
    use syntect::parsing::syntax_definition::Pattern;
    let set = set.clone().into_builder();
    let grammar = set
        .syntaxes()
        .iter()
        .find(|s| s.name == "Makefile")
        .unwrap();
    let mut found = Vec::new();
    for context in grammar.contexts.values() {
        for pattern in &context.patterns {
            if let Pattern::Match(m) = pattern {
                if m.regex.regex_str().contains("#       level 0") {
                    found.push(m.regex.regex_str().to_string());
                }
            }
        }
    }
    found
}

/// Lines of real Makefiles: variables of every flavour, rules, pattern and double-colon rules,
/// target-specific variables, recipes, conditionals, `$(…)` and `${…}` nested 1–6 deep, and
/// unbalanced parentheses.
fn makefile_lines() -> Vec<String> {
    let mut lines: Vec<String> = [
        "# Project build",
        ".DEFAULT_GOAL := all",
        "CC ?= cc",
        "CFLAGS = -O2 -Wall -Wextra -pedantic -std=c11",
        "CFLAGS += -g -fsanitize=address,undefined -DNDEBUG",
        "LDFLAGS ::= -lm -lpthread",
        "VERSION != git describe --tags --always",
        "BUILDDIR := build/$(MODE)",
        "OBJS_1 := $(patsubst $(SRCDIR)/%.c,$(BUILDDIR)/%.o,$(SRCS_1))",
        "DEPS_1 := $(OBJS_1:.o=.d)",
        "SRCS := $(foreach m,$(MODULES),$(wildcard $(SRCDIR)/$(m)/*.c))",
        "OBJS := $(sort $(patsubst %.c,%.o,$(filter-out $(SRCDIR)/test/%,$(SRCS))))",
        "objects := $(patsubst %.c,%.o,$(wildcard src/*.c src/**/*.c)) $(EXTRA_OBJECTS) $(MORE)",
        "mix := ${A} $(B) ${C $(D)} $$(shell date)",
        "unbal1 := $(foo",
        "unbal2 := foo)",
        "unbal3 := $(a $(b) c",
        "weird)name: dep",
        "rule $(a $(b: c",
        "x := (a:b) = c",
        "ifeq ($(MODE),debug)",
        "else ifeq ($(origin CC),default)",
        "endif",
        "define compile_rule",
        "$(BUILDDIR)/$(1)/%.o: $(SRCDIR)/$(1)/%.c | $(BUILDDIR)/$(1)",
        "\t$(Q)$(CC) $(CFLAGS) -MMD -MP -c $$< -o $$@",
        "endef",
        "$(foreach m,$(MODULES),$(eval $(call compile_rule,$(m))))",
        ".PHONY: all clean install test check dist",
        "$(BUILDDIR)/mod1/%.o: $(SRCDIR)/mod1/%.c | $(BUILDDIR)/mod1",
        "\t@mkdir -p $(dir $@)",
        "\t@echo \"  CC      $(notdir $<) -> $(notdir $@)\"",
        "%.o: %.c",
        "\t$(CC) $(CFLAGS) -c $< -o $@",
        "install:: all",
        "test: CFLAGS += -DTEST",
        "\t@for t in $(TESTS); do \\",
        "\t  ./$$t || exit 1; \\",
        "\tdone",
        "\t@if [ -n \"$$(git status --porcelain)\" ]; then echo dirty; fi",
        "dist: VERSION := $(shell git describe)",
        "-include $(DEPS_0) $(DEPS_1) $(DEPS_2)",
        "export PATH := $(PWD)/bin:$(PATH)",
        "print-%: ; @echo $* = $($*)",
        "target7: dep7.o $(OBJS_7) | $(BUILDDIR)",
        "\techo building the project now please wait while everything compiles fine",
    ]
    .iter()
    .map(|l| l.to_string())
    .collect();
    let mut nested = String::from("x");
    for depth in 1..=6 {
        nested = format!("$(f{depth} {nested},y)");
        for form in [
            "v := {}",
            "{}: dep",
            "dep: {}",
            "\techo {}",
            "{} = 1",
            "a: v := {}",
        ] {
            lines.push(form.replace("{}", &nested));
        }
        lines.push(format!("v := {}", &nested[..nested.len() - 1]));
        lines.push(format!("{nested}) := 1"));
    }
    lines
}

/// Makefile's grammar looks ahead on every line for a rule or a variable through balanced
/// parentheses written out four levels deep; as bundled, two runs of characters with nothing
/// between them may share a stretch every way there is (`[^()]*`, an optional group skipped,
/// `[^()]*`), so a line costs a power of its length: an ordinary 73-character recipe line 720,000
/// backtracking steps, 100 `x`s past fancy-regex's limit of 1,000,000 (an error: no match). As the
/// highlighter parses it, every line of a real Makefile is searched within 50,000 steps (at most
/// 36,297 here), and lines of 100 to 300 characters with no parentheses within that limit (the
/// lookaheads still try every start: at most 604,225). The limits are set on the engine syntect
/// runs the grammars on: a search past one is an error.
#[test]
fn makefile_lines_are_searched_within_a_small_backtrack_budget() {
    let patterns = makefile_paren_patterns();
    assert_eq!(patterns.len(), 2, "the rule and the variable lookaheads");
    let mut long = Vec::new();
    for n in [100, 200, 300] {
        long.push("x".repeat(n));
        long.push(format!("all: {}", "obj.o ".repeat(n / 6)));
        long.push(format!("\t{}", "x ".repeat(n / 2)));
    }
    for pattern in &patterns {
        for (lines, budget) in [(makefile_lines(), 50_000), (long.clone(), 1_000_000)] {
            let regex = fancy_regex::RegexBuilder::new(pattern)
                .oniguruma_mode(true)
                .backtrack_limit(budget)
                .build()
                .unwrap();
            for line in &lines {
                assert!(
                    regex.captures_from_pos(line, 0).is_ok(),
                    "over {budget} steps: {line:.60}"
                );
            }
        }
    }
    // The control: as bundled, each lookahead goes past 50,000 steps on an ordinary recipe line
    // (720,000 and 76,000), and the search ends in the backtrack limit's error.
    let recipe = "\techo building the project now please wait while everything compiles fine";
    let bundled = paren_patterns_of(syntaxes());
    assert_eq!(bundled.len(), 2);
    for pattern in &bundled {
        let regex = fancy_regex::RegexBuilder::new(pattern)
            .oniguruma_mode(true)
            .backtrack_limit(50_000)
            .build()
            .unwrap();
        assert!(
            matches!(
                regex.captures_from_pos(recipe, 0),
                Err(fancy_regex::Error::RuntimeError(
                    fancy_regex::RuntimeError::BacktrackLimitExceeded
                ))
            ),
            "the bundled lookahead stays within 50,000 steps"
        );
    }
}

/// Makefile's lookaheads rewritten keep what they mean ([`balanced_once`]): every line of a real
/// Makefile — variables of every flavour, rules, recipes, conditionals, `$(…)` nested 1–6 deep,
/// unbalanced parentheses — has the colours the bundled grammar gives it, the lines parsed in
/// order as one file, ended by LF and by CRLF. All but one: on
/// `objects := $(patsubst %.c,%.o,$(wildcard …)) …` the bundled grammar's variable lookahead
/// goes past fancy-regex's backtrack limit and is taken for no match, so the line is not read as
/// a variable's and its value is not coloured as one (`.c` in `$(patsubst %.c,…)` plain);
/// rewritten, it is, as on `OBJS := $(sort $(patsubst %.c,…))`, a line the bundled grammar gets
/// through. (Counted with a syntect that counts its search errors: over these lines the
/// bundled grammar errs 5 times, on 3 lines, the rewritten never; this is the one whose colours
/// it changed.)
#[test]
fn makefile_lines_keep_the_bundled_grammars_colours() {
    let lines = makefile_lines();
    let gave_up = "objects := $(patsubst %.c,%.o,$(wildcard src/*.c src/**/*.c)) $(EXTRA_OBJECTS) \
                   $(MORE)";
    let others: Vec<String> = lines.iter().filter(|l| *l != gave_up).cloned().collect();
    assert_eq!(others.len(), lines.len() - 1);
    // The bundled grammar's colours once (slow: what this rewrite is for), the viewer's twice.
    let bundled = whole_line_colours("mk", &others);
    for end in ["\n", "\r\n"] {
        let got = highlighted("mk", &others, end);
        assert_eq!(got.len(), bundled.len(), "its lines");
        for (i, (got, want)) in got.iter().zip(&bundled).enumerate() {
            assert_eq!(got.len(), want.len(), "line {i}: its characters");
            if let Some(at) = (0..got.len()).find(|&k| got[k] != want[k]) {
                panic!(
                    "line {i} {:?}, character {at}: {:?}, bundled {:?}",
                    others[i], got[at], want[at]
                );
            }
        }
    }
    let pair = [
        "OBJS := $(sort $(patsubst %.c,%.o,$(filter-out $(SRCDIR)/test/%,$(SRCS))))".to_string(),
        gave_up.to_string(),
    ];
    let got = highlighted("mk", &pair, "\n");
    let bundled = whole_line_colours("mk", &pair);
    // The `.` of the first `.c`: a value's text.
    let dot = |line: &str| line.chars().position(|c| c == '.').unwrap();
    let (in_objs, in_objects) = (dot(&pair[0]), dot(&pair[1]));
    let value = bundled[0][in_objs].1;
    assert_eq!(got[0][in_objs].1, value, "as bundled on the `OBJS` line");
    assert_eq!(got[1][in_objects].1, value, "a value on the `objects` line");
    assert_ne!(
        bundled[1][in_objects].1, value,
        "the bundled grammar gave up on the `objects` line"
    );
    assert!(got[1].iter().map(|&(c, _)| c).eq(pair[1].chars()));
}

/// The scope stack at each character of each line, and at its end, the lines parsed in order
/// as one file with `set`'s Makefile grammar.
fn makefile_scopes(
    set: &SyntaxSet,
    lines: &[String],
) -> Vec<(
    Vec<Vec<syntect::parsing::Scope>>,
    Vec<syntect::parsing::Scope>,
)> {
    let grammar = set.find_syntax_by_name("Makefile").unwrap();
    let mut parse = ParseState::new(grammar);
    let mut stack = ScopeStack::new();
    lines
        .iter()
        .map(|line| {
            let ops = parse.parse_line(line, set).unwrap();
            let mut next = 0;
            let mut at_chars = Vec::new();
            for (at, _) in line.char_indices() {
                while next < ops.len() && ops[next].0 <= at {
                    stack.apply(&ops[next].1).unwrap();
                    next += 1;
                }
                at_chars.push(stack.as_slice().to_vec());
            }
            for (_, op) in &ops[next..] {
                stack.apply(op).unwrap();
            }
            (at_chars, stack.as_slice().to_vec())
        })
        .collect()
}

/// Beneath the colours, the scopes: every character of every line of a real Makefile but the
/// `objects` one (the bundled grammar gives up on it; above) is in the same scopes, the rewritten
/// grammar against the bundled one, and each line ends with the same stack, the lines parsed in
/// order as one file.
#[test]
fn makefile_lines_keep_the_bundled_grammars_scopes() {
    let others: Vec<String> = makefile_lines()
        .into_iter()
        .filter(|l| !l.starts_with("objects := "))
        .collect();
    let bundled = makefile_scopes(syntaxes(), &others);
    let rewritten = makefile_scopes(makefile_syntaxes().expect("rewritten"), &others);
    assert_eq!(bundled.len(), rewritten.len());
    for (i, (want, got)) in bundled.iter().zip(&rewritten).enumerate() {
        assert_eq!(got.0.len(), want.0.len(), "line {i}: its characters");
        if let Some(at) = (0..got.0.len()).find(|&k| got.0[k] != want.0[k]) {
            panic!(
                "line {i} {:?}, character {at}: {:?}, bundled {:?}",
                others[i], got.0[at], want.0[at]
            );
        }
        assert_eq!(
            got.1, want.1,
            "line {i} {:?}: the stack it ends with",
            others[i]
        );
    }
}

/// The rewrite is of the bundled Makefile grammar's two lookaheads, exactly: in the set a
/// Makefile is parsed with — taken on the highlighting worker, never when a viewer makes its
/// highlighter on a frame — both are the rewritten text, and no group closed is followed by a run
/// outside it. Every other syntax is parsed with the bundled set. A grammar whose lookahead is
/// not the bundled text — here one space more, which `(?x)` ignores — is left as it is.
#[test]
fn the_makefile_lookaheads_are_rewritten_only_when_they_are_the_bundled_ones() {
    use syntect::parsing::syntax_definition::Pattern;
    let makefile = syntaxes().find_syntax_by_name("Makefile").unwrap();
    // A viewer's highlighter takes its syntaxes on its worker, at its first page: a frame that
    // makes it does not ask for the rewritten set (counted on this thread), the first page does.
    let asked = || MAKEFILE_SYNTAXES_ASKED.with(|n| n.get());
    let before = asked();
    let mut hl = Highlight::new(makefile, Color32::WHITE, u64::MAX).unwrap();
    assert_eq!(asked(), before, "a frame asked for the rewritten set");
    assert!(hl.set.is_none() && hl.states.is_empty());
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "Makefile", b"all: dep\n\t$(CC) -o $@ $^\n");
    let mut doc = TextDoc::open(&path).unwrap();
    hl.page(&mut doc, 0, 2, &|| false).unwrap().unwrap();
    assert_eq!(asked(), before + 1, "the first page asks once");
    hl.page(&mut doc, 0, 2, &|| false).unwrap().unwrap();
    assert_eq!(asked(), before + 1, "and the next pages do not");
    assert!(std::ptr::eq(hl.set.unwrap(), makefile_syntaxes().unwrap()));
    let (set, _) = parsing(makefile);
    assert!(std::ptr::eq(set, makefile_syntaxes().expect("rewritten")));
    let patterns = makefile_paren_patterns();
    let mut hashes: Vec<u64> = patterns.iter().map(|p| fnv1a(p.as_bytes())).collect();
    hashes.sort();
    assert_eq!(hashes, [0x5daf_d94e_51a7_e64c, 0xda7d_2adc_b711_d0ce]);
    for pattern in &patterns {
        let lines: Vec<&str> = pattern.lines().map(str::trim_start).collect();
        assert!(!lines
            .windows(2)
            .any(|w| w[0].starts_with("\\))?") && w[1].starts_with("[^()]*")));
    }
    let rust = syntaxes().find_syntax_by_extension("rs").unwrap();
    assert!(std::ptr::eq(parsing(rust).0, syntaxes()));
    // One space more in the rule's lookahead: not the bundled text.
    let bundled = syntaxes().clone().into_builder();
    let mut changed = syntect::parsing::SyntaxSetBuilder::new();
    for syntax in bundled.syntaxes() {
        let mut syntax = syntax.clone();
        if syntax.name == "Makefile" {
            for context in syntax.contexts.values_mut() {
                for pattern in &mut context.patterns {
                    if let Pattern::Match(m) = pattern {
                        if m.regex.regex_str().starts_with("(?=(?x)") {
                            let spaced = format!("{} ", m.regex.regex_str());
                            m.regex = syntect::parsing::Regex::new(spaced);
                        }
                    }
                }
            }
        }
        changed.add(syntax);
    }
    assert!(with_makefile_rewritten(changed.build()).is_none());
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
        eprintln!(
            "SKIPPED: no Hangul font (hangul_and_emoji_wrap_inside_the_window_with_system_fonts)"
        );
        return;
    };
    if !drv.ctx.fonts_mut(|fonts| fonts.has_glyph(&f, '😀')) {
        eprintln!(
            "SKIPPED: no emoji font (hangul_and_emoji_wrap_inside_the_window_with_system_fonts)"
        );
        return;
    }
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
    assert_eq!(win.line, Some(1));
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
            // Each tab as spaces up to the next stop.
            let mut out = String::new();
            for c in l.chars() {
                match c {
                    '\t' => {
                        let n = TAB - out.chars().count() % TAB;
                        out.extend(std::iter::repeat_n(' ', n));
                    }
                    c => out.push(c),
                }
            }
            out
        })
        .collect();
    assert_eq!(rows, want);
    assert_eq!(rows[0], "        foo bar");
}

// ---------------------------------------------------------------------------------------------
// Shaped scripts (review round 2): Arabic and Hebrew come out of egui in visual order, an Indic
// cluster's glyphs all carry its first character — rows are measured by laying them out, broken
// only between grapheme clusters, with a bounded number of layouts.

/// The installed fonts a window gets (eframe's system font provider), found for every character
/// in `need` — laid out once by a probe window first — or why not (the test then skips).
fn system_fonts(drv: &mut Driver, dir: &Path, need: &str) -> bool {
    use egui_system_font_provider::SystemFontProvider;
    drv.ctx
        .add_font_provider(Arc::new(SystemFontProvider::new()));
    let mut probe = viewer(dir, "probe.txt", &format!("{need}\n"));
    drv.idle(&mut probe, 3);
    let f = chrome::font();
    let missing: String = need
        .chars()
        .filter(|&c| c != ' ' && !drv.ctx.fonts_mut(|fonts| fonts.has_glyph(&f, c)))
        .collect();
    if !missing.is_empty() {
        eprintln!("SKIPPED: no installed font for {missing:?}");
    }
    missing.is_empty()
}

/// Every row of the file as wrapping cuts it at the window's width, from the top (through a memo
/// of its own), and the row starts found going back up from the last row.
fn all_rows(drv: &Driver, win: &mut TextWindow) -> (Vec<wrap::Row>, Vec<wrap::Pos>) {
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    let font = chrome::font();
    let cell = drv.ctx.fonts_mut(|f| f.glyph_width(&font, '0'));
    let room = win.room;
    let mut memo = wrap::Memo::default();
    memo.reset(room);
    let mut w = wrap::Wrapper {
        doc: &mut win.doc,
        memo: &mut memo,
        m: wrap::Measure::new(&drv.ctx, font, plain),
        width: room,
        read: wrap::read_cap(room, cell),
    };
    let (mut rows, mut p) = (Vec::new(), wrap::Pos::default());
    loop {
        rows.extend(w.page(p, 1).unwrap().0);
        match w.next(p).unwrap() {
            Some(q) => p = q,
            None => break,
        }
    }
    let mut back = vec![p];
    while let Some(q) = w.prev(p).unwrap() {
        back.push(q);
        p = q;
    }
    back.reverse();
    (rows, back)
}

/// The rows of line `line` put back together: each row's text and the space it was full at.
fn rejoined(rows: &[wrap::Row], line: u64) -> String {
    rows.iter()
        .filter(|r| r.line == Some(line))
        .map(|r| {
            let skipped = r.cut.taken > r.cut.shown.len();
            format!("{}{}", r.cut.text(), if skipped { " " } else { "" })
        })
        .collect()
}

/// A long line of `unit` (words shorter than a row) at 480 points: the first page and a page
/// down take a bounded number of layouts; every row is inside the window, starts between
/// grapheme clusters and ends after a space (a word that fits is never split); the rows put
/// together are the line; going back up from the end finds the same rows.
fn shaped_line_wraps_at_clusters(name: &str, unit: &str) {
    use unicode_segmentation::UnicodeSegmentation;
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(480.0, 300.0);
    if !system_fonts(&mut drv, tmp.path(), unit) {
        return;
    }
    // Past a restart point (16 KiB).
    let line = unit.repeat(20 * 1024 / unit.len() + 1);
    let mut win = viewer(tmp.path(), name, &format!("{line}\nnext\n"));
    let before = wrap::laid();
    drv.until(&mut win, 120, |w| w.ready());
    let rows = win.page.as_ref().unwrap().rows.len();
    let cols = (win.room / chrome::CELL.x) as usize;
    let laid = |since: (usize, usize)| {
        let now = wrap::laid();
        (now.0 - since.0, now.1 - since.1)
    };
    let (layouts, chars) = laid(before);
    assert!(
        layouts <= 8 * rows + 16 && chars <= 8 * rows * (2 * cols + 16),
        "{name}: {layouts} layouts of {chars} characters for a page of {rows} rows"
    );
    let x = drv
        .texts()
        .into_iter()
        .filter(|(_, _, t)| !t.is_empty() && line.starts_with(t.as_str()))
        .map(|(r, _, _)| r.left())
        .next()
        .expect("the first row");
    assert_rows_inside(&drv, x);
    let before = wrap::laid();
    drv.key(&mut win, Key::PageDown, Modifiers::NONE);
    let (layouts, _) = laid(before);
    assert!(
        layouts <= 8 * rows + 16,
        "{name}: {layouts} layouts for PgDn"
    );
    assert_rows_inside(&drv, x);

    let (all, back) = all_rows(&drv, &mut win);
    let starts: std::collections::HashSet<usize> = line
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([line.len()])
        .collect();
    let font = chrome::font();
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    let of_line: Vec<&wrap::Row> = all.iter().filter(|r| r.line == Some(0)).collect();
    for (k, row) in of_line.iter().enumerate() {
        assert!(
            starts.contains(&(row.start as usize)),
            "{name}: row {k} starts inside a cluster: {:?}",
            row.cut.text()
        );
        let wide = drv
            .ctx
            .fonts_mut(|f| f.layout_no_wrap(row.cut.text(), font.clone(), plain))
            .size()
            .x;
        assert!(
            wide <= win.room + 0.5,
            "{name}: row {k} is {wide} wide in {}",
            win.room
        );
        // A restart point (16 KiB) moves on to the next word start too.
        if k + 1 < of_line.len() {
            assert!(
                row.cut.taken > row.cut.shown.len() || row.cut.text().ends_with(' '),
                "{name}: row {k} splits a word: {:?}",
                row.cut.text()
            );
        }
    }
    assert_eq!(rejoined(&all, 0), line, "{name}: the rows are the line");
    let forward: Vec<wrap::Pos> = all
        .iter()
        .map(|r| wrap::Pos {
            at: r.at,
            off: r.start,
            loose: false,
        })
        .collect();
    assert_eq!(forward, back, "{name}: the same rows going back up");
}

#[test]
fn a_long_arabic_line_wraps_at_clusters_with_bounded_layouts() {
    shaped_line_wraps_at_clusters("arabic.txt", "مرحبا بالعالم هذا نص عربي طويل للاختبار ");
}

#[test]
fn a_long_hindi_line_wraps_at_clusters_with_bounded_layouts() {
    shaped_line_wraps_at_clusters("hindi.txt", "हिन्दी भाषा किताब नमस्ते दुनिया ");
}

#[test]
fn a_mixed_rtl_and_ltr_line_wraps_at_clusters_with_bounded_layouts() {
    shaped_line_wraps_at_clusters(
        "mixed.txt",
        "English שלום עולם مرحبا بالعالم mixed עברית text العربية ",
    );
}

/// A line well over LINE_MAX (words, a word longer than a row, across a restart point), walked
/// row by row: the rows put together are the line, byte for byte.
#[test]
fn a_long_line_walked_row_by_row_is_the_whole_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut line = String::new();
    let mut i = 0u32;
    while line.len() < 5 * LINE_MAX {
        let word = match i % 9 {
            0 => "x".repeat(150),
            k => format!("w{i}{}", "o".repeat(k as usize)),
        };
        line.push_str(&word);
        line.push(' ');
        i += 1;
    }
    line.push_str("THE-END");
    let mut win = viewer(tmp.path(), "whole.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let (all, back) = all_rows(&drv, &mut win);
    assert!(all.iter().filter(|r| r.line == Some(0)).count() > 50);
    assert_eq!(rejoined(&all, 0), line);
    assert_eq!(rejoined(&all, 1), "next");
    assert_eq!(back.len(), all.len());
}

/// The same with bytes that are not UTF-8 — scattered, at a restart point (stray continuation
/// bytes, a character cut short) and a character straddling another: the rows put together are
/// the line as Rust's lossy decoding reads it.
#[test]
fn an_invalid_utf8_line_walked_row_by_row_is_the_lossy_line() {
    let tmp = tempfile::tempdir().unwrap();
    let restart = wrap::RESTART as usize;
    let mut bytes = Vec::new();
    let mut i = 0u32;
    while bytes.len() < restart - 64 {
        bytes.extend_from_slice(format!("word{i} ").as_bytes());
        if i.is_multiple_of(37) {
            bytes.extend_from_slice(b"\xff\xfe bad \xe2\x82 cut ");
        }
        i += 1;
    }
    // Up to the first restart point, then stray continuation bytes and a character cut short.
    bytes.resize(restart - 1, b'a');
    bytes.extend_from_slice(b"\x80\x80\x80\xe2\x82y ");
    while bytes.len() < 2 * restart - 64 {
        bytes.extend_from_slice(format!("more{i} ").as_bytes());
        i += 1;
    }
    // "한" across the second restart point.
    bytes.resize(2 * restart - 1, b'b');
    bytes.extend_from_slice("한 tail".as_bytes());
    let mut body = bytes.clone();
    body.extend_from_slice(b"\nnext\n");
    let path = write(tmp.path(), "bad.txt", &body);
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "bad".into(),
        "bad.txt",
        vec![],
    );
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let (all, _) = all_rows(&drv, &mut win);
    assert_eq!(rejoined(&all, 0), String::from_utf8_lossy(&bytes));
}

/// A restart point (16 KiB into a line) that falls inside a grapheme cluster — here on the vowel
/// sign of "कि" — moves on to the cluster's end: no row starts inside it.
#[test]
fn a_restart_point_inside_a_cluster_moves_past_it() {
    use unicode_segmentation::UnicodeSegmentation;
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(480.0, 300.0);
    if !system_fonts(&mut drv, tmp.path(), "कि") {
        return;
    }
    let restart = wrap::RESTART as usize;
    // "क" ends where the restart point is; "ि" starts there.
    let mut line = "a".repeat(restart - 3);
    line.push_str("कि");
    line.push_str(&" कि".repeat(200));
    assert_eq!(&line.as_bytes()[restart..restart + 3], "ि".as_bytes());
    let mut win = viewer(tmp.path(), "restart.txt", &format!("{line}\n"));
    drv.until(&mut win, 10, |w| w.ready());
    let (all, back) = all_rows(&drv, &mut win);
    let starts: std::collections::HashSet<usize> =
        line.grapheme_indices(true).map(|(i, _)| i).collect();
    for row in &all {
        assert!(
            starts.contains(&(row.start as usize)),
            "a row starts inside a cluster at {}: {:?}",
            row.start,
            row.cut.text()
        );
    }
    assert_eq!(rejoined(&all, 0), line);
    let forward: Vec<wrap::Pos> = all
        .iter()
        .map(|r| wrap::Pos {
            at: r.at,
            off: r.start,
            loose: false,
        })
        .collect();
    assert_eq!(forward, back);
}

// ---------------------------------------------------------------------------------------------
// Review round 3: rows that start right to left, and Chinese / Japanese without spaces.

/// Where each glyph of the painted row holding `needle` sits, left to right: (character, x).
fn painted(drv: &Driver, needle: &str) -> Vec<(char, f32)> {
    let (_, galley) = drv
        .galleys()
        .into_iter()
        .find(|(_, g)| g.text().contains(needle))
        .unwrap_or_else(|| panic!("no row holds {needle:?}"));
    let mut glyphs: Vec<(char, f32)> = galley
        .rows
        .iter()
        .flat_map(|r| r.glyphs.iter())
        // Only glyphs with an advance: egui also lists a zero-width one per character it shaped
        // into a glyph run (in logical order, at the run's end).
        .filter(|g| g.advance_width > 0.0)
        .map(|g| (g.chr, g.pos.x))
        .collect();
    glyphs.sort_by(|a, b| a.1.total_cmp(&b.1));
    glyphs
}

/// The x of each letter of `word` as painted (the first glyph found for each, in reading order).
fn word_xs(glyphs: &[(char, f32)], word: &str) -> Vec<f32> {
    // The painted order left to right, as a string: the word must read left to right in it.
    let order: String = glyphs.iter().map(|g| g.0).collect();
    let at = order
        .find(word)
        .unwrap_or_else(|| panic!("{word:?} does not read left to right in {order:?}"));
    let start = order[..at].chars().count();
    glyphs[start..start + word.chars().count()]
        .iter()
        .map(|g| g.1)
        .collect()
}

/// A row whose first strong character is right to left (a line starting in Hebrew, and a wrapped
/// row starting mid-line with a Hebrew word) still paints its English words left to right, and
/// each Hebrew word right to left; the row is as wide as it was measured.
#[test]
fn a_row_starting_right_to_left_keeps_english_left_to_right() {
    let cols = columns(600.0, 400.0);
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    if !system_fonts(&mut drv, tmp.path(), "שלום") {
        return;
    }
    // Row 1 starts with Hebrew; the wrapped line has a row starting with "כהן".
    let filler = "x".repeat(cols - 2);
    let body = format!("שלום text here and more\n{filler} כהן\", \"email\": \"u0\"\n");
    let mut win = viewer(tmp.path(), "rtl.txt", &body);
    drv.until(&mut win, 5, |w| w.ready());
    let x = text_x(&drv, "xxxx");
    let rows = rows_at(&drv, x);
    assert!(
        rows.iter().any(|r| r.starts_with("כהן")),
        "a wrapped row starts with Hebrew: {rows:?}"
    );
    for (needle, english, hebrew) in [
        ("text here", ["text", "here"], "שלום"),
        ("email", ["email", "u0"], "כהן"),
    ] {
        let glyphs = painted(&drv, needle);
        for word in english {
            let xs = word_xs(&glyphs, word);
            assert!(xs.windows(2).all(|w| w[0] < w[1]), "{word:?} at {xs:?}");
        }
        // The Hebrew word reads right to left: its last letter leftmost.
        let reversed: String = hebrew.chars().rev().collect();
        word_xs(&glyphs, &reversed);
        // As wide as the row was measured (laid out as one run).
        let (_, galley) = drv
            .galleys()
            .into_iter()
            .find(|(_, g)| g.text().contains(needle))
            .unwrap();
        let font = chrome::font();
        let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
        let measured = drv
            .ctx
            .fonts_mut(|f| f.layout_no_wrap(galley.text().to_string(), font.clone(), plain))
            .size()
            .x;
        assert_eq!(galley.size().x, measured, "{needle:?}");
    }
    assert_rows_inside(&drv, x);
}

/// A Chinese or Japanese paragraph with only a few spaces: a row breaks between any two
/// ideographs or kana (as UAX #14 allows), so rows fill the window to within a glyph — never
/// starting with closing punctuation such as 。、」 — while Latin words stay whole; the rows put
/// together are the paragraph.
fn cjk_rows_fill_the_width(name: &str, unit: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(480.0, 300.0);
    if !system_fonts(&mut drv, tmp.path(), unit) {
        return;
    }
    let line = unit.repeat(30);
    let mut win = viewer(tmp.path(), name, &format!("{line}\n"));
    drv.until(&mut win, 10, |w| w.ready());
    let (all, back) = all_rows(&drv, &mut win);
    assert_eq!(
        rejoined(&all, 0),
        line,
        "{name}: the rows are the paragraph"
    );
    let forward: Vec<wrap::Pos> = all
        .iter()
        .map(|r| wrap::Pos {
            at: r.at,
            off: r.start,
            loose: false,
        })
        .collect();
    assert_eq!(forward, back, "{name}: the same rows going back up");
    let font = chrome::font();
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    let wide = |t: &str| {
        drv.ctx
            .fonts_mut(|f| f.layout_no_wrap(t.to_string(), font.clone(), plain))
            .size()
            .x
    };
    let closing = "。、，．」』）！？：；";
    let latin_words: std::collections::HashSet<&str> = unit
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() > 1)
        .collect();
    for (k, pair) in all.windows(2).enumerate() {
        let (row, next) = (pair[0].cut.text(), pair[1].cut.text());
        let first: String = next.chars().take(1).collect();
        assert!(
            !closing.contains(first.as_str()),
            "{name}: row {} starts with {first:?}: {next:?}",
            k + 1
        );
        // The next row's first character (and the closing mark it carries) would not fit.
        let carried: String = next
            .chars()
            .take(1)
            .chain(next.chars().skip(1).take_while(|c| closing.contains(*c)))
            .collect();
        // Ideographs and kana break anywhere: a row before one is full (a Latin word may move
        // whole to the next row instead).
        let breakable = next.chars().next().is_some_and(|c| {
            matches!(c, '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{9FFF}' | '\u{FF01}'..='\u{FF60}')
        });
        if breakable {
            assert!(
                wide(&format!("{row}{carried}")) > win.room,
                "{name}: row {k} ({} of {}) leaves room for {carried:?}: {row:?}",
                wide(&row),
                win.room
            );
        }
        // Latin words are not split.
        let tail: String = row
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let head: String = next
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        if !tail.is_empty() && !head.is_empty() {
            assert!(
                !latin_words.contains(format!("{tail}{head}").as_str()),
                "{name}: {tail}|{head} split"
            );
        }
    }
}

#[test]
fn a_japanese_paragraph_fills_its_rows() {
    cjk_rows_fill_the_width(
        "ja.txt",
        "日本語の文章を折り返して表示します。「かぎ括弧」と English words を含み、句読点で行を始めない。 ",
    );
}

#[test]
fn a_chinese_paragraph_fills_its_rows() {
    cjk_rows_fill_the_width(
        "zh.txt",
        "中文文本换行显示测试，这是一个很长的段落。其中也有 Mixed English 单词（括号）和标点！ ",
    );
}

/// Sections of a painted row never split a grapheme cluster: a combining mark, a ZWJ or a
/// variation selector after a right-to-left letter — mid-row or at the row's end — goes in its
/// base's section (egui would shape it alone, detached from its base).
#[test]
fn directed_sections_never_split_a_cluster() {
    use unicode_segmentation::UnicodeSegmentation;
    let font = chrome::font();
    for s in [
        "\u{05D0}\u{0301} x",
        "x \u{05D0}\u{0301}",
        "\u{0628}\u{0301} x",
        "x \u{0628}\u{0301}",
        "\u{05D0}\u{0591}\u{05D1} text",
        "x \u{0628}\u{200D}",
        "\u{05D0}\u{FE0F} y",
    ] {
        let job = directed_job(s.chars().map(|c| (c, Color32::WHITE)).collect(), &font);
        assert_eq!(job.text, s);
        let starts: std::collections::HashSet<usize> = s
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([s.len()])
            .collect();
        let ranges: Vec<(usize, usize)> = job
            .sections
            .iter()
            .map(|sec| (sec.byte_range.start.0, sec.byte_range.end.0))
            .collect();
        for &(from, to) in &ranges {
            assert!(
                starts.contains(&from) && starts.contains(&to),
                "{s:?}: a section boundary inside a cluster: {ranges:?}"
            );
        }
    }
}

/// On screen such a mark sits over its base — mid-row and at the row's end — and the row is as
/// wide as it was measured.
#[test]
fn a_mark_after_a_right_to_left_letter_stays_on_its_base() {
    let tmp = tempfile::tempdir().unwrap();
    let mut drv = Driver::new(600.0, 400.0);
    if !system_fonts(&mut drv, tmp.path(), "\u{0628}") {
        return;
    }
    let font = chrome::font();
    let plain = chrome::Palette::of(&drv.ctx.global_style().visuals).text;
    for s in ["\u{0628}\u{0301} x", "x \u{0628}\u{0301}"] {
        let job = directed_job(s.chars().map(|c| (c, plain)).collect(), &font);
        let galley = drv.ctx.fonts_mut(|f| f.layout_job(job));
        let measured = drv
            .ctx
            .fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), plain));
        assert_eq!(galley.size().x, measured.size().x, "{s:?}");
        let glyphs = &galley.rows[0].glyphs;
        let base = glyphs
            .iter()
            .position(|g| g.chr == '\u{0628}' && g.advance_width > 0.0)
            .expect("the base");
        let (left, right) = (glyphs[base].pos.x, glyphs[base].max_x());
        for (i, g) in glyphs.iter().enumerate() {
            if i != base && (g.chr == '\u{0628}' || g.chr == '\u{0301}') {
                assert!(
                    g.pos.x >= left - 1.5 && g.max_x() <= right + 0.5,
                    "{s:?}: the mark at {}..{} is off its base at {left}..{right}",
                    g.pos.x,
                    g.max_x()
                );
            }
        }
    }
}

/// 06 §7.2 (X3's defect 1): a read that fails leaves the read window as it was — what it holds
/// afterwards is still the file's bytes, never a buffer made ready for the read that failed.
#[test]
fn a_failed_read_leaves_the_read_window_as_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = (0..40_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let path = write(tmp.path(), "letters.txt", &body);
    let mut doc = TextDoc::open(&path).unwrap();
    let mut window = Window_::default();
    assert_eq!(window.at(&mut doc, 0, 10).unwrap(), &body[..10]);
    let held = window.bytes.clone();
    assert!(!held.is_empty());
    doc.fail_reads = true;
    assert!(window.at(&mut doc, 30_000, 100).is_err());
    // Looked at while reads still fail: the window itself, not what a later read refills.
    assert_eq!(window.start, 0);
    assert_eq!(window.bytes, held, "the window was not left as it was");
    assert_eq!(window.bytes, &body[..held.len()]);
    doc.fail_reads = false;
    // Inside what the window held before the failure: served from it, and the file's bytes.
    let got = window.at(&mut doc, 5, 100).unwrap().to_vec();
    assert!(!got.is_empty());
    assert_eq!(got, &body[5..5 + got.len()], "bytes the file does not hold");
}

/// 06 §7.2 (X3's defect 3): a first line that could not be read when the window was made is
/// read again; the syntax it names is not lost with the failed read.
#[test]
fn a_first_line_that_could_not_be_read_at_once_still_picks_the_syntax() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "run", b"#!/bin/sh\necho hello\n");
    let mut doc = TextDoc::open(&path).unwrap();
    doc.fail_reads = true;
    let mut win = TextWindow::new(doc, "run".into(), "run", vec![]);
    assert_eq!(grammar(&win), None, "not read yet");
    win.doc.fail_reads = false;
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(grammar(&win), Some("Bourne Again Shell (bash)"));
    drv.until(&mut win, 5, |w| w.ready() && w.worker.is_some());
    let colors = drv.colors_of("echo hello").expect("shown");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
}

/// A re-read keeps the place sideways too: not wrapped and scrolled along a long line, a file
/// that is appended to is read again and shows the same line from the same byte — nobody
/// navigated. A jump the owner makes (Go to, the end of the file) still lands on its line
/// from the line's start.
#[test]
fn a_reread_keeps_the_sideways_place_of_an_unwrapped_view() {
    let tmp = tempfile::tempdir().unwrap();
    let line = |i: usize| format!("line {i:03} {}", "word ".repeat(120));
    let body: String = (0..80).map(|i| format!("{}\n", line(i))).collect();
    let path = write(tmp.path(), "wide.log", body.as_bytes());
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "wide".into(),
        "wide.log",
        vec![],
    );
    let clock = on_clock(&mut win);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    for _ in 0..5 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    }
    drv.until(&mut win, 5, |w| w.ready());
    let top = |w: &TextWindow| w.page.as_ref().unwrap().rows[0].cut.text();
    assert_eq!((win.line, win.left), (Some(5), 32));
    let before = top(&win);
    assert!(before.starts_with(&line(5)[32..60]), "{before}");
    // Appended to by someone else: read again, in the same place.
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"appended\n").unwrap();
    drop(file);
    later(&clock, 2.0);
    reread(&mut drv, &mut win, 81);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(
        (win.line, win.left),
        (Some(5), 32),
        "the re-read moved the view"
    );
    assert_eq!(top(&win), before);
    // Go to a line: from its start.
    drv.key(&mut win, Key::G, crate::gui::testkit::command(false));
    drv.frame(
        &mut win,
        vec![egui::Event::Text("11".into())],
        Modifiers::NONE,
    );
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!((win.line, win.left), (Some(10), 0), "Go to");
    assert!(top(&win).starts_with("line 010 "));
    // The end of the file: from its lines' starts.
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    }
    drv.key(&mut win, Key::End, crate::gui::testkit::command(false));
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(win.left, 0, "the end of the file");
    let last = win.page.as_ref().unwrap().rows.last().unwrap().cut.text();
    assert_eq!(last, "appended");
    // Home within a line, then the start of the file.
    drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    drv.key(&mut win, Key::Home, crate::gui::testkit::command(false));
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!((win.line, win.left), (Some(0), 0), "the start of the file");
}
