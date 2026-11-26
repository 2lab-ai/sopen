//! The editor's screen (12 §4, §8; ED-screen): its cells read back as both backends show them,
//! highlighting through the buffer's line source (EH-1, EH-2, the kept states' spacing), the
//! input method's output (E10, E20) and the text document's Bulk reader.

use super::*;
use crate::edit::ops::Doubt;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::Driver;
use eframe::egui::{Event, ImeEvent, Key, Modifiers, Pos2};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The editor on `text`, a file named `name`, its keys from `backend`.
fn view_in(text: &str, name: &str, backend: Backend) -> EditView {
    let ed = Editor::open(
        Original::new(text.as_bytes().to_vec(), Encoding::Utf8),
        Doubt::default(),
    );
    EditView::new(ed, name, backend, false)
}

fn view(text: &str, name: &str) -> EditView {
    view_in(text, name, Backend::Window)
}

/// `n` lines of Rust, joined by LF with none after the last: line `i` (from 0) is a `let`
/// whose number is `i` when `i % 8 == 7` (5,127 among them).
fn rust(n: usize) -> String {
    let lines: Vec<String> = (0..n)
        .map(|i| match (i + 3) % 8 {
            0 => format!("/// Item {i}: what it holds."),
            1 => format!("pub fn item_{i}(x: u32) -> u32 {{"),
            2 => format!("    let value = {i};"),
            3 => "    // a plain comment".to_string(),
            4 => format!("    let name = \"item {i}\";"),
            5 => "    x + value + name.len() as u32".to_string(),
            6 => "}".to_string(),
            _ => String::new(),
        })
        .collect();
    lines.join("\n")
}

/// The last pass's screen: the grid of the title, window and status rows.
fn screen(drv: &Driver) -> Arc<cells::Layer> {
    (cells::layers(&drv.ctx).into_iter())
        .max_by_key(|l| l.buf.area.height)
        .expect("the editor's cells")
}

fn row(l: &cells::Layer, y: u16) -> String {
    cells::row_text(&l.buf, y).0
}

/// Row `y`'s text from cell `x` on.
fn text_of(l: &cells::Layer, x: u16, y: u16) -> String {
    let (s, at) = cells::row_text(&l.buf, y);
    s.chars()
        .zip(at)
        .filter(|&(_, c)| c >= x)
        .map(|(ch, _)| ch)
        .collect()
}

/// The foreground of the glyphs of row `y` from cell `x` up to the right border (blanks left
/// out).
fn glyph_colors(l: &cells::Layer, x: u16, y: u16) -> Vec<Color32> {
    let seen = l.seen();
    let end = usize::from(l.buf.area.width - 1);
    seen[usize::from(y)][usize::from(x)..end]
        .iter()
        .filter(|s| !s.symbol.trim().is_empty())
        .filter_map(|s| s.fg)
        .collect()
}

/// The row line `n` is drawn in, and where its text starts.
fn place_of(v: &EditView, n: u64) -> (u16, u16) {
    let digits = DIGITS.max(v.ed.buffer().lines().to_string().len()) as u16;
    (2 + (n - v.top) as u16, 1 + digits + 2)
}

/// The caret to the start of line `n` (from 0), brought into view at the next pass.
fn go_to_line(v: &mut EditView, n: u64) {
    let at = v.ed.buffer().line_start(n).unwrap();
    v.ed.place(at, false);
    v.follow = true;
}

fn ctrl(key: Key) -> Press {
    Press::key(key, Modifiers::COMMAND)
}

/// Every highlighted line on screen: the text the worker coloured is the row's, character for
/// character — the BOM, a CR LF's CR and the encoding read alike on both sides.
fn texts_agree(v: &mut EditView) {
    let lines = v.ed.buffer().lines();
    for n in v.top..(v.top + v.rows).min(lines) {
        let (ls, end, raw) = v.body(n);
        if raw > LINE_MAX as u64 {
            continue;
        }
        let row: Vec<char> = v
            .visible((ls, end), 0, u64::MAX)
            .iter()
            .map(|c| c.c)
            .collect();
        let painted =
            v.hl.lines
                .get(&n)
                .unwrap_or_else(|| panic!("line {n} coloured"));
        assert_eq!(painted.text, row, "line {n}");
    }
}

/// §4 at 80×25: the title row's menus, the frame's buttons and name, the gutter, syntect's
/// colours on the rows, the scroll bar, `line:col`, the status row's fields and the F-key row.
#[test]
fn the_editor_in_cells() {
    let text: String = (0..30)
        .map(|i| format!("fn f{i}() {{}} // note {i}\n"))
        .collect();
    let mut v = view(&text, "main.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 20, |v| v.ready());
    let l = screen(&drv);
    assert_eq!((l.buf.area.width, l.buf.area.height), (80, 24));
    let p = PALETTE;
    // The title row: the program's block, the menus with each first letter red, the views.
    let title = row(&l, 0);
    assert!(
        title.starts_with(" xtools  File  Edit  Search  Block  Git  Options"),
        "{title}"
    );
    assert!(title.contains("1 Text·2 Hex·3 Raw │"), "{title}");
    let file = cells::find(std::slice::from_ref(&l), "File")[0].cells;
    assert_eq!(cells::colors(&l, file), vec![cga::RED, p.title_text]);
    // The frame: double while it has the keyboard, `[■]`, the name, `1` and `[↕]`.
    let top = row(&l, 1);
    assert!(
        top.starts_with("╔═[■]═") && top.ends_with("═1═[↕]═╗"),
        "{top}"
    );
    assert!(top.contains(" main.rs "), "{top}");
    let close = cells::find(std::slice::from_ref(&l), "[■]")[0].cells;
    assert_eq!(cells::colors(&l, close), vec![cga::LIGHT_GREEN]);
    // The gutter: a blank, three digits, a blank, in the dim colour; then the text.
    assert!(
        row(&l, 2).starts_with("║   1 fn f0() {} // note 0"),
        "{}",
        row(&l, 2)
    );
    assert_eq!(glyph_colors(&l, 1, 2)[0], p.dim);
    // syntect's runs: `fn` a keyword, the comment the comment's colour.
    let colors = glyph_colors(&l, 6, 2);
    assert_eq!(colors[0], cga::YELLOW, "fn");
    assert_eq!(*colors.last().unwrap(), cga::CYAN, "the comment");
    // The scroll bar on the right border, the place in the bottom one: 1:1, unchanged.
    assert_eq!(row(&l, 2).chars().last(), Some('↑'));
    assert_eq!(row(&l, 21).chars().last(), Some('↓'));
    assert!(row(&l, 22).starts_with("╚══ 1:1 ═"), "{}", row(&l, 22));
    // The status row's fields.
    assert_eq!(row(&l, 23).trim(), "Insert Indent │ Rust │ UTF-8 │ LF");
    // The F-key row.
    let keys = cells::layers(&drv.ctx)
        .into_iter()
        .find(|l| l.buf.area.height == 1)
        .expect("the F-key row");
    let keys = row(&keys, 0);
    let mut at = 0;
    for slot in [
        "1Help", "2Save", "3Exit", "4View", "5Save", "6Panel", "7SaveAs", "8", "9View", "10Menu",
    ] {
        at += keys[at..]
            .find(slot)
            .unwrap_or_else(|| panic!("{slot} in {keys}"))
            + slot.len();
    }
    // The caret: two scan lines at the file's start.
    assert_eq!(l.caret, Some(Position::new(6, 2)));
}

/// The frame and the status row follow the document: `*` and the place once it changed, a single
/// line while the column has the keyboard, the modes, the armings, `unsaved — kept`, the BOM, a
/// guessed encoding, CR LF and the git letter.
#[test]
fn the_frame_and_the_status_row_follow_the_document() {
    let mut v = view("\u{FEFF}fn main() {}\r\nlet x = 1;\r\n", "a.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    let status = |drv: &Driver| row(&screen(drv), 23).trim().to_string();
    assert_eq!(status(&drv), "Insert Indent │ Rust │ UTF-8 (BOM) │ CRLF");
    v.press(&Press::Text("x".into()));
    v.press(&Press::key(Key::Insert, Modifiers::NONE));
    v.set_git(Some('M'));
    drv.idle(&mut v, 1);
    assert!(row(&screen(&drv), 22).starts_with("╚═* 1:2 ═"));
    assert_eq!(
        status(&drv),
        "Indent │ Rust │ UTF-8 (BOM) │ CRLF │ M │ unsaved"
    );
    v.set_keep(Keep::Kept);
    v.press(&ctrl(Key::K));
    v.set_focused(false);
    drv.idle(&mut v, 1);
    assert_eq!(
        status(&drv),
        "^K │ Indent │ Rust │ UTF-8 (BOM) │ CRLF │ M │ unsaved — kept"
    );
    assert!(
        row(&screen(&drv), 1).starts_with("┌─[■]─"),
        "the column has it"
    );
    assert_eq!(
        screen(&drv).caret,
        None,
        "no caret while the column has the keyboard"
    );
    let ed = Editor::open(
        Original::new(b"p\0l\0a\0i\0n\0".to_vec(), Encoding::Utf16Le),
        Doubt::default(),
    );
    let mut guessed = EditView::new(ed, "notes", Backend::Window, true);
    drv.idle(&mut guessed, 1);
    assert_eq!(
        status(&drv),
        "Insert Indent │ Plain Text │ UTF-16LE (guessed) │ LF"
    );
}

/// The worker reads the rows' text in the file's encoding, after a BOM and without a CR LF's
/// CR, so its colours land on the characters drawn: UTF-8 and UTF-16LE alike.
#[test]
fn the_worker_reads_what_the_rows_show() {
    let text = "\u{FEFF}fn main() {}\r\nlet x = \"a\";\r\n// the end";
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    for (enc, bytes) in [
        (Encoding::Utf8, text.as_bytes().to_vec()),
        (Encoding::Utf16Le, utf16),
    ] {
        let ed = Editor::open(Original::new(bytes, enc), Doubt::default());
        let mut v = EditView::new(ed, "both.rs", Backend::Window, false);
        let mut drv = Driver::new(640.0, 400.0);
        drv.until(&mut v, 20, |v| v.ready());
        texts_agree(&mut v);
        let l = screen(&drv);
        assert!(text_of(&l, 6, 2).starts_with("fn main() {}"), "{enc:?}");
        assert_eq!(glyph_colors(&l, 6, 2)[0], cga::YELLOW, "{enc:?}");
        assert!(
            glyph_colors(&l, 6, 3).contains(&cga::LIGHT_CYAN),
            "{enc:?}: the string"
        );
        let status = format!("Insert Indent │ Rust │ {} (BOM) │ CRLF", enc.label());
        assert_eq!(row(&l, 23).trim(), status);
    }
}

/// `^S` in a terminal (E31): its prompt is the status row, `[Save]` in the bar's colours, and a
/// click on it is the Save the binding runs. A line a command says is shown after the fields.
#[test]
fn the_save_prompt_is_the_status_row_and_its_save_a_click() {
    let mut v = view_in("fn main() {}\n", "main.rs", Backend::Terminal);
    let mut drv = Driver::new(1000.0, 400.0);
    v.press(&ctrl(Key::S));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    assert_eq!(
        row(&l, 23).trim(),
        "Save over main.rs? F2 or a click on [Save] saves · C saves a copy · any other key cancels"
    );
    let save = cells::find(std::slice::from_ref(&l), "[Save]")[0].cells;
    let seen = l.seen();
    let cell = &seen[usize::from(save.y)][usize::from(save.x)];
    assert_eq!(
        (cell.fg, cell.bg),
        (Some(PALETTE.bar_text), Some(PALETTE.bar))
    );
    drv.click(&mut v, l.rect(save).center());
    assert_eq!(v.take_commands(), vec![Command::Save]);
    v.press(&ctrl(Key::S));
    v.press(&Press::Text("w".into()));
    drv.idle(&mut v, 1);
    assert_eq!(
        row(&screen(&drv), 23).trim(),
        "Insert Indent │ Rust │ UTF-8 │ LF │ not saved — F2, Ctrl+K S or a click saves"
    );
}

/// The block is Turbo's blue on cyan; Overwrite's caret is the whole cell, its colours swapped;
/// a click puts the caret where it lands.
#[test]
fn the_block_the_overwrite_caret_and_a_click() {
    let mut v = view("abcdef\nghijkl\n", "notes.txt");
    let mut drv = Driver::new(640.0, 400.0);
    for _ in 0..3 {
        v.press(&Press::key(Key::ArrowRight, Modifiers::SHIFT));
    }
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let seen = l.seen();
    for (x, s) in seen[2].iter().enumerate().take(9).skip(6) {
        assert_eq!((s.fg, s.bg), (Some(cga::BLUE), Some(cga::CYAN)), "cell {x}");
    }
    assert_eq!(seen[2][9].bg, Some(PALETTE.back), "past the block");
    assert_eq!(l.caret, Some(Position::new(9, 2)));
    v.press(&Press::key(Key::Insert, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let s = &l.seen()[2][9];
    assert_eq!(
        (s.fg, s.bg),
        (Some(PALETTE.back), Some(PALETTE.text)),
        "the whole cell"
    );
    // A click on `k` (line 2, column 5).
    drv.click(&mut v, l.rect(Cells::new(6 + 4, 3, 1, 1)).center());
    assert_eq!(v.ed.caret(), 7 + 4);
    assert_eq!(screen(&drv).caret, Some(Position::new(10, 3)));
}

/// E16: no wrap; the text scrolls sideways to keep the caret in view, and back. A wide glyph
/// cut by the left edge is blank, never half drawn.
#[test]
fn sideways_scroll_keeps_the_caret_in_view() {
    let long: String = (0..200)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let mut v = view(&format!("한{long}\nshort\n"), "wide.txt");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    v.press(&Press::key(Key::End, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    // 73 text cells (80 less the borders and five gutter cells): the line's last 72 and the caret.
    let shown = text_of(&l, 6, 2);
    assert!(shown.starts_with(&long[200 - 72..]), "{shown}");
    assert_eq!(l.caret, Some(Position::new(6 + 72, 2)));
    assert!(row(&l, 3).starts_with("║   2 "), "{}", row(&l, 3));
    v.press(&Press::key(Key::Home, Modifiers::NONE));
    drv.idle(&mut v, 1);
    assert!(text_of(&screen(&drv), 6, 2).starts_with("한abc"));
    // Column 1 at the left edge is 한's second half: blank, never half a glyph.
    v.left = 1;
    drv.idle(&mut v, 1);
    let shown = text_of(&screen(&drv), 6, 2);
    assert!(shown.starts_with(&format!(" {}", &long[..20])), "{shown}");
}

/// A parser hook that, while `hold` is set, stops on a line holding `needle` until released.
fn holding_hook(
    needle: &'static str,
) -> (
    text::ParseHook,
    Arc<AtomicBool>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let hold = Arc::new(AtomicBool::new(false));
    let (reached_tx, reached) = mpsc::channel::<()>();
    let (go, go_rx) = mpsc::channel::<()>();
    let (reached_tx, go_rx) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let held = hold.clone();
    let hook: text::ParseHook = Arc::new(move |line: &str| {
        if held.load(Ordering::SeqCst) && line.contains(needle) {
            let _ = reached_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv();
        }
    });
    (hook, hold, reached, go)
}

/// Lets a held worker go after `secs` unless `done` (returned) is sent first: frames that
/// waited on the parser then fail the test's time bound instead of hanging it.
fn watchdog(go: &mpsc::Sender<()>, secs: u64) -> mpsc::Sender<()> {
    let (done, rx) = mpsc::channel::<()>();
    let go = go.clone();
    std::thread::spawn(move || {
        if rx.recv_timeout(Duration::from_secs(secs)).is_err() {
            let _ = go.send(());
        }
    });
    done
}

/// EH-1: typing at line 5,128 of a 10,007-line Rust file draws the edited line in its old colours
/// — the frames go on while the parser is stuck on it — then in the new ones when the worker
/// answers. An unclosed `/*` at line 1 makes the rest comment, far pages too; closing it brings
/// the old colours back.
#[test]
fn eh1_an_edit_keeps_its_old_colours_until_the_worker_answers() {
    let mut v = view(&rust(10_007), "big.rs");
    assert_eq!(v.ed.buffer().lines(), 10_007);
    let (hook, hold, reached, go) = holding_hook("\"5127;");
    v.hook = Some(hook);
    let mut drv = Driver::new(1000.0, 700.0);
    go_to_line(&mut v, 5127);
    drv.until(&mut v, 120, |v| v.ready());
    let (y, x) = place_of(&v, 5127);
    let l = screen(&drv);
    assert!(text_of(&l, x, y)
        .trim_start()
        .starts_with("let value = 5127;"));
    let original = glyph_colors(&l, x, y);
    assert_eq!(original[original.len() - 5], cga::LIGHT_GREEN, "the number");
    // `"` before the number: the line is a string from there, once the worker says so.
    hold.store(true, Ordering::SeqCst);
    let number = v.ed.buffer().line_start(5127).unwrap() + 16;
    v.ed.place(number, false);
    v.press(&Press::Text("\"".into()));
    drv.idle(&mut v, 1);
    reached
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker reached the edited line");
    let started = Instant::now();
    let done = watchdog(&go, 10);
    drv.idle(&mut v, 30);
    let _ = done.send(());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the frames waited for the parser: {:?}",
        started.elapsed()
    );
    let l = screen(&drv);
    assert!(text_of(&l, x, y)
        .trim_start()
        .starts_with("let value = \"5127;"));
    let old = glyph_colors(&l, x, y);
    assert_eq!(
        old[..old.len() - 6],
        original[..original.len() - 5],
        "before the quote"
    );
    assert_eq!(
        old[old.len() - 5..],
        original[original.len() - 5..],
        "its old colours"
    );
    assert!(!v.ready());
    hold.store(false, Ordering::SeqCst);
    go.send(()).unwrap();
    drv.until(&mut v, 60, |v| v.ready());
    let new = glyph_colors(&screen(&drv), x, y);
    assert!(
        new[new.len() - 6..].iter().all(|&c| c == cga::LIGHT_CYAN),
        "a string now: {new:?}"
    );
    v.press(&Press::key(Key::Backspace, Modifiers::NONE));
    // An unclosed `/*` at line 1: the rest is comment, the first page and line 5,128 alike.
    v.ed.place(0, false);
    v.follow = true;
    v.press(&Press::Text("/*".into()));
    drv.until(&mut v, 60, |v| v.ready());
    let l = screen(&drv);
    for k in 0..10 {
        let colors = glyph_colors(&l, x, 2 + k);
        assert!(
            colors.iter().all(|&c| c == cga::CYAN),
            "row {k}: {colors:?}"
        );
    }
    go_to_line(&mut v, 5127);
    drv.until(&mut v, 120, |v| v.ready());
    let (y, x) = place_of(&v, 5127);
    let colors = glyph_colors(&screen(&drv), x, y);
    assert!(colors.iter().all(|&c| c == cga::CYAN), "{colors:?}");
    // Closed again: the old colours back.
    v.ed.place(0, false);
    v.press(&Press::key(Key::Delete, Modifiers::NONE));
    v.press(&Press::key(Key::Delete, Modifiers::NONE));
    assert_eq!(v.ed.buffer().line_start(1), Some(23));
    go_to_line(&mut v, 5127);
    drv.until(&mut v, 120, |v| v.ready());
    let (y, x) = place_of(&v, 5127);
    assert_eq!(glyph_colors(&screen(&drv), x, y), original);
}

/// EH-2: a line over `LINE_MAX` stops highlighting there, as in the viewer — never parsed — and
/// shortening it lifts the stop; lines past `HIGHLIGHT_MAX` are plain and not parsed.
#[test]
fn eh2_a_long_line_and_text_past_16_mib_are_plain() {
    let mut body = String::from("fn before() {}\n/* a comment that ");
    body.push_str(&"x".repeat(LINE_MAX + 100));
    body.push_str(" ends here */ let y = 1;\n");
    for i in 0..40 {
        body.push_str(&format!("fn after{i}() {{}}\n"));
    }
    let mut v = view(&body, "cut.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    let l = screen(&drv);
    let plain = PALETTE.text;
    assert!(glyph_colors(&l, 6, 2).contains(&cga::YELLOW), "before it");
    assert!(text_of(&l, 6, 4).starts_with("fn after0() {}"));
    assert!(
        glyph_colors(&l, 6, 4).iter().all(|&c| c == plain),
        "after it"
    );
    // The long line goes: the stop with it.
    go_to_line(&mut v, 1);
    v.press(&ctrl(Key::Y));
    drv.until(&mut v, 30, |v| v.ready());
    let l = screen(&drv);
    assert!(text_of(&l, 6, 3).starts_with("fn after0() {}"));
    assert!(
        glyph_colors(&l, 6, 3).contains(&cga::YELLOW),
        "highlighted again"
    );
    // Past 16 MiB: plain, and nothing parsed for it.
    let lines = (HIGHLIGHT_MAX / 16 + 1000) as usize;
    let big: String = (0..lines).map(|i| format!("fn f{i:09}() {{}}\n")).collect();
    let mut v = view(&big, "huge.rs");
    drv.until(&mut v, 30, |v| v.ready());
    let (_, x) = place_of(&v, 0);
    assert!(
        glyph_colors(&screen(&drv), x, 2).contains(&cga::YELLOW),
        "the first page"
    );
    let parsed = v.hl.worker.as_ref().unwrap().parsed.load(Ordering::Relaxed);
    v.press(&ctrl(Key::End));
    drv.until(&mut v, 30, |v| v.ready());
    assert!(v.ed.buffer().line_start(v.top).unwrap() > HIGHLIGHT_MAX);
    let l = screen(&drv);
    let (y, x) = place_of(&v, lines as u64 - 1);
    assert_eq!(
        text_of(&l, x, y).trim_end(),
        format!("fn f{:09}() {{}}", lines - 1)
    );
    assert!(glyph_colors(&l, x, y).iter().all(|&c| c == plain));
    assert_eq!(
        v.hl.worker.as_ref().unwrap().parsed.load(Ordering::Relaxed),
        parsed
    );
}

/// Edits made while the worker is stuck reach it merged: two asks queue behind a held parse, and
/// the earlier changed line of theirs decides what is parsed again — `/*` at line 3 makes the
/// page at line 152 comment, though the newest ask changed only line 171.
#[test]
fn edits_queued_behind_a_held_worker_reach_it_merged() {
    let text: Vec<String> = (0..400)
        .map(|i| match i {
            150 => "// MARKER".to_string(),
            i => format!("fn f{i}() {{}}"),
        })
        .collect();
    let mut v = view(&text.join("\n"), "queued.rs");
    let (hook, hold, reached, go) = holding_hook("MARKER");
    v.hook = Some(hook);
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    hold.store(true, Ordering::SeqCst);
    let mut type_at = |v: &mut EditView, line: u64, text: &str| {
        go_to_line(v, line);
        v.press(&Press::Text(text.into()));
        drv.idle(v, 1);
    };
    type_at(&mut v, 160, "x");
    reached
        .recv_timeout(Duration::from_secs(30))
        .expect("the worker held on line 151");
    type_at(&mut v, 2, "/*");
    type_at(&mut v, 170, "y");
    hold.store(false, Ordering::SeqCst);
    go.send(()).unwrap();
    drv.until(&mut v, 30, |v| v.ready());
    assert_eq!(v.top, 151);
    let l = screen(&drv);
    for k in 0..20 {
        let colors = glyph_colors(&l, 6, 2 + k);
        assert!(
            colors.iter().all(|&c| c == cga::CYAN),
            "row {k}: {colors:?}"
        );
    }
}

/// E19: the editor's kept states stand every 64 lines — one per 64 lines up to the last, at
/// those lines' starts.
#[test]
fn kept_states_stand_every_64_lines() {
    let text = rust(1_000);
    let mut kept = KeptStates::new(text.clone().into_bytes(), "spaced.rs");
    kept.fill();
    assert_eq!(STATE_LINES, 64);
    assert_eq!(kept.count(), 999 / 64 + 1);
    let starts: Vec<u64> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i as u64 + 1))
        .step_by(64)
        .collect();
    assert_eq!(kept.src.starts, starts);
    assert_eq!(
        (0..kept.count())
            .map(|i| kept.src.mark_line(i))
            .collect::<Vec<_>>(),
        (0..16).map(|i| i * 64).collect::<Vec<_>>()
    );
}

/// The stand-in window that keeps the IME output the last pass put out.
struct Ime(EditView, Option<egui::output::IMEOutput>);

impl Window for Ime {
    fn ui(&mut self, ui: &mut egui::Ui) {
        Window::ui(&mut self.0, ui);
        self.1 = ui.ctx().output(|o| o.ime);
    }
    fn ready(&self) -> bool {
        true
    }
}

/// E10, E20: while the editor has the keyboard the IME rectangle is the caret's cell, and the
/// preedit is drawn over the cells there, the caret after it; while Ctrl+K waits for a letter it
/// is unset (the input method off, the letter matched by its key), and back after.
#[test]
fn the_input_method_at_the_caret_and_off_while_armed() {
    let mut w = Ime(view("ab\n", "notes.txt"), None);
    let mut drv = Driver::new(640.0, 400.0);
    w.0.press(&Press::key(Key::ArrowRight, Modifiers::NONE));
    drv.idle(&mut w, 1);
    let l = screen(&drv);
    let cell = |x: u16| l.rect(Cells::new(x, 2, 1, 1));
    assert_eq!(w.1.expect("the IME rectangle").cursor_rect, cell(7));
    let preedit = Event::Ime(ImeEvent::Preedit {
        text: "하".into(),
        active_range_chars: None,
    });
    drv.frame(&mut w, vec![preedit], Modifiers::NONE);
    let l = screen(&drv);
    assert!(
        text_of(&l, 6, 2).starts_with("a하 "),
        "over the cells at the caret"
    );
    let s = &l.seen()[2][7];
    assert_eq!((s.fg, s.bg), (Some(PALETTE.field_text), Some(cga::BLACK)));
    assert_eq!(l.caret, Some(Position::new(9, 2)));
    assert_eq!(w.1.expect("the IME rectangle").cursor_rect, cell(9));
    drv.frame(
        &mut w,
        vec![Event::Ime(ImeEvent::Commit("한".into()))],
        Modifiers::NONE,
    );
    w.0.press(&Press::Text("한".into()));
    drv.idle(&mut w, 1);
    assert!(text_of(&screen(&drv), 6, 2).starts_with("a한b"));
    w.0.press(&ctrl(Key::K));
    drv.idle(&mut w, 1);
    assert_eq!(w.1, None, "the input method off while ^K waits");
    let b = Press::Key {
        key: Key::B,
        physical: Some(Key::B),
        modifiers: Modifiers::NONE,
    };
    w.0.press(&b);
    drv.idle(&mut w, 1);
    assert!(w.1.is_some(), "back once the letter came");
    w.0.set_focused(false);
    drv.idle(&mut w, 1);
    assert_eq!(w.1, None, "not while the column has the keyboard");
}

/// E14: the text document hands out a Bulk reader of its Source, which reads the file whole.
#[test]
fn the_text_document_hands_out_a_bulk_reader() {
    use crate::remote::{fake, FakeFile, Link};
    let bytes: Vec<u8> = rust(3_000).into_bytes();
    let (transport, _control) = fake("/r/big.rs", FakeFile::bytes(bytes.clone()), Link::gate());
    let no_reopen: crate::stream::Reopen = Box::new(|| anyhow::bail!("no second connection"));
    let source = crate::stream::Source::open(transport, no_reopen, Arc::new(|| {})).unwrap();
    let doc = text::TextDoc::streamed(source.clone());
    let mut reader = doc.bulk_reader().expect("a stream's document has one");
    let mut got = vec![0; bytes.len()];
    let n =
        crate::stream::Bytes::read_at(&mut reader, 0, &mut got, &AtomicBool::new(false)).unwrap();
    assert_eq!((n, &got), (bytes.len(), &bytes));
    // Fetched as a Bulk reader's blocks, none of them on demand (an Interactive reader's).
    let fetched = source.stats().fetched;
    assert!(fetched.bulk >= bytes.len() as u64, "{fetched:?}");
    assert_eq!(fetched.demand, 0, "{fetched:?}");
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();
    assert!(text::TextDoc::open(&path).unwrap().bulk_reader().is_none());
}

/// The editor's release frame cost at 1000×700 for a 10,007-line Rust file, highlighted, its
/// caret deep in it (`cargo test --release --lib -- --ignored --exact
/// gui::edit::tests::frame_cost_at_1000_by_700 --nocapture`): printed, no budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    let mut v = view(&rust(10_007), "big.rs");
    let mut drv = Driver::new(1000.0, 700.0);
    go_to_line(&mut v, 5127);
    drv.until(&mut v, 60, |v| v.ready());
    drv.idle(&mut v, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        drv.frame(&mut v, Vec::new(), Modifiers::NONE);
    }
    let each = started.elapsed() / n;
    println!("the editor's frame at 1000x700: {each:?}");
}

/// The screen at 80×25 as its cells' text, for a look (`cargo test --lib -- --ignored --exact
/// gui::edit::tests::cells_dump_80_by_25 --nocapture`): a block, an edit and `^K` waiting.
#[test]
#[ignore]
fn cells_dump_80_by_25() {
    let mut v = view(&rust(60), "item.rs");
    let mut drv = Driver::new(640.0, 400.0);
    go_to_line(&mut v, 7);
    v.press(&Press::Text("x".into()));
    v.press(&Press::key(Key::ArrowDown, Modifiers::SHIFT));
    v.press(&ctrl(Key::K));
    drv.until(&mut v, 20, |v| v.ready());
    let l = screen(&drv);
    for y in 0..l.buf.area.height {
        println!("{}", row(&l, y));
    }
    for l in cells::layers(&drv.ctx)
        .iter()
        .filter(|l| l.buf.area.height == 1)
    {
        println!("{}", row(l, 0));
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 1 (2906f39): each finding's test, failing without its fix.

/// R1 MUST 1: a line far longer than `LINE_MAX` with no LF is read no further than that past its
/// start: it comes cut, the read ends with it, and nothing past it is scanned for.
#[test]
fn a_long_line_is_read_no_further_than_line_max() {
    let long = "x".repeat(20 << 20);
    let mut v = view(&format!("a\n{long}\nb\n"), "long.rs");
    let mut src = BufLines::new(v.snap());
    SNAP_READ.with(|n| n.set(0));
    let lines = src.read_lines(0, 3).unwrap();
    let read = SNAP_READ.with(std::cell::Cell::get);
    assert_eq!(lines.len(), 2, "the read ends with the cut line");
    assert!(!lines[0].cut && lines[1].cut);
    assert!(read <= 4 * (LINE_MAX as u64 + CHUNK), "{read} bytes read");
}

/// R1 MUST 1: the page's first line is read from where the buffer's own index says it starts,
/// nothing before it scanned; and a newer ask stops a read between lines.
#[test]
fn a_page_is_read_from_the_index_and_a_newer_ask_stops_a_read() {
    let mut v = view(&rust(200_000), "many.rs");
    let start = v.ed.buffer().line_start(150_000).unwrap();
    let mut src = BufLines::new(v.snap());
    src.known = Some((150_000, start));
    SNAP_READ.with(|n| n.set(0));
    let lines = src.read_lines(150_000, 10).unwrap();
    let read = SNAP_READ.with(std::cell::Cell::get);
    assert_eq!(lines[0].text, rust(150_001).lines().last().unwrap());
    assert!(
        read <= 10 * 2 * 1024,
        "{read} bytes read for 10 short lines"
    );
    let mut src = BufLines::new(v.snap());
    src.cancel = Some(Arc::new(|| true));
    let e = src.read_lines(150_000, 10).expect_err("stopped");
    assert!(e.is::<Superseded>(), "{e:#}");
}

/// R1 MUST 2: edits made while the worker is held keep one ask waiting, never a queue of
/// snapshots; once let go it answers for the newest.
#[test]
fn edits_behind_a_held_worker_keep_one_ask() {
    let mut v = view(&rust(400), "held.rs");
    let (hook, hold, reached, go) = holding_hook("item 1\"");
    v.hook = Some(hook);
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    hold.store(true, Ordering::SeqCst);
    go_to_line(&mut v, 300);
    v.press(&Press::Text("z".into()));
    drv.idle(&mut v, 1);
    reached
        .recv_timeout(Duration::from_secs(30))
        .expect("the worker held on line 2");
    let done = watchdog(&go, 30);
    for _ in 0..30 {
        v.press(&Press::Text("z".into()));
        drv.idle(&mut v, 1);
    }
    let waiting = v.hl.worker.as_ref().unwrap().pending();
    let _ = done.send(());
    hold.store(false, Ordering::SeqCst);
    let _ = go.send(());
    assert!(waiting <= 1, "{waiting} asks waiting");
    drv.until(&mut v, 30, |v| v.ready());
}

/// R1 MUST 2: a worker waiting for an ask ends when it is dropped, giving its pool slot back.
#[test]
fn a_dropped_worker_gives_its_slot_back() {
    let pool = Pool::new(1);
    let syntax = view("fn x() {}\n", "x.rs").syntax.expect("Rust");
    let hl = Highlight::new(syntax, Color32::WHITE, u64::MAX).unwrap();
    let worker = Worker::start(hl, egui::Context::default(), &pool).expect("a slot");
    assert_eq!(pool.live(), 1);
    std::thread::sleep(Duration::from_millis(50));
    drop(worker);
    let deadline = Instant::now() + Duration::from_secs(10);
    while pool.live() > 0 {
        assert!(Instant::now() < deadline, "the worker never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// R1 MUST 3 (made deterministic in round 2): a reply the worker made for the document before
/// an edit that moved lines (Enter) is never laid on the document after it — the reply is the
/// test's own, sent once the edit is made — and the rows keep their own colours, moved with them.
#[test]
fn a_reply_for_an_older_document_is_never_painted() {
    let text: Vec<String> = (0..300)
        .map(|i| match i % 2 {
            0 => format!("fn f{i}() {{}}"),
            _ => format!("// note {i}"),
        })
        .collect();
    let mut v = view(&text.join("\n"), "stale.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    // The page a worker answers for this document, made here.
    let mut hl = Highlight::new(v.syntax.unwrap(), PALETTE.text, u64::MAX).unwrap();
    let rows = v.rows as usize;
    let runs = (hl.page(&mut BufLines::new(v.snap()), 0, rows, &|| false))
        .unwrap()
        .unwrap();
    // A worker of the test's: its replies are the test's to send.
    let (tx, done) = mpsc::channel();
    v.hl.worker = Some(Worker {
        mail: Arc::default(),
        done,
        latest: Arc::default(),
        stop: Arc::default(),
        parsed: Arc::default(),
    });
    // An ask of this document, the page at line 1: an edit far below it.
    go_to_line(&mut v, 250);
    v.press(&Press::Text("z".into()));
    v.ed.place(0, false);
    v.follow = true;
    drv.idle(&mut v, 1);
    let asked = v.hl.gen;
    // Enter at line 1 before any pass: every line moves one down; then that ask's reply arrives.
    v.press(&Press::key(Key::Enter, Modifiers::NONE));
    tx.send(old_reply(asked, rows, runs)).unwrap();
    drv.idle(&mut v, 1);
    // (Line 2, the text Enter moved off line 1, is plain until the worker answers: R1 S8.)
    let l = screen(&drv);
    assert!(text_of(&l, 6, 5).starts_with("fn f2() {}"));
    assert_eq!(
        glyph_colors(&l, 6, 5)[0],
        cga::YELLOW,
        "line 4 is a function's"
    );
    let comment = glyph_colors(&l, 6, 4);
    assert!(
        comment.iter().all(|&c| c == cga::CYAN),
        "line 3: {comment:?}"
    );
}

/// The reply a worker sends for the page of `rows` lines at line 1 asked under `gen`.
fn old_reply(gen: u64, rows: usize, runs: Vec<Runs>) -> Done {
    Done::Page {
        gen,
        top: 0,
        count: rows,
        runs,
        stop: u64::MAX,
    }
}

/// R1 MUST 4: the caret on a wide glyph at the right edge brings the whole glyph into view, and
/// so does a wide preedit at the last column.
#[test]
fn a_wide_glyph_or_preedit_at_the_right_edge_comes_into_view() {
    let mut v = view(&format!("{}\n", "가".repeat(60)), "wide.txt");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    for _ in 0..36 {
        v.press(&Press::key(Key::ArrowRight, Modifiers::NONE));
    }
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let caret = l.caret.expect("the caret on screen");
    let under = &l.seen()[usize::from(caret.y)][usize::from(caret.x)];
    assert_eq!(under.symbol, "가", "the caret's character under it");
    let mut v = view(&format!("{}\n", "a".repeat(72)), "edge.txt");
    v.press(&Press::key(Key::End, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let preedit = Event::Ime(ImeEvent::Preedit {
        text: "하".into(),
        active_range_chars: None,
    });
    drv.frame(&mut v, vec![preedit], Modifiers::NONE);
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    assert!(text_of(&l, 6, 2).contains('하'), "{}", row(&l, 2));
}

/// R1 MUST 5: a resize brings the caret back into view; the wheel's scrolling on a layout that
/// did not change stays where it put the text.
#[test]
fn a_resize_brings_the_caret_back_into_view() {
    let mut v = view(&format!("{}\n", "a".repeat(100)), "resize.txt");
    let mut drv = Driver::new(640.0, 400.0);
    v.ed.place(60, false);
    v.follow = true;
    drv.idle(&mut v, 1);
    assert_eq!(screen(&drv).caret, Some(Position::new(66, 2)));
    drv.resize(320.0, 400.0);
    drv.idle(&mut v, 1);
    let caret = screen(&drv)
        .caret
        .expect("the caret in view after the resize");
    assert!(caret.x < 39, "{caret:?}");
    let mut v = view(&rust(200), "wheel.rs");
    drv.idle(&mut v, 1);
    drv.wheel(&mut v, Pos2::new(100.0, 100.0), -10.0);
    drv.idle(&mut v, 2);
    assert_eq!(v.top, 30, "the wheel's rows stay");
}

/// R1 MUST 6: a run of combining marks costs a bounded frame: a cell takes a few of them, its
/// symbol made once, and a row decodes a bounded stretch however long the run.
#[test]
fn combining_marks_cost_a_bounded_frame() {
    let text = format!("a{}\nnext\n", "\u{301}".repeat(300_000));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut v = view(&text, "marks.txt");
        let mut drv = Driver::new(640.0, 400.0);
        drv.idle(&mut v, 1);
        DECODED.with(|n| n.set(0));
        drv.idle(&mut v, 1);
        let decoded = DECODED.with(std::cell::Cell::get);
        let l = screen(&drv);
        let first = l.seen()[2][6].symbol.clone();
        let _ = tx.send((decoded, first, text_of(&l, 6, 3)));
    });
    let (decoded, first, next) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("two frames over 300,000 marks within 30 s");
    assert!(
        first.starts_with('a') && first.chars().count() <= 9,
        "{} chars",
        first.chars().count()
    );
    assert!(decoded <= 4 * STRIDE, "{decoded} bytes decoded in a frame");
    assert!(next.starts_with("next"));
}

/// R1 MUST 7: the text viewer's walk is 01751df's: a line its index did not see cut (the file
/// changed under it) is parsed as read, and the lines after it are coloured.
#[test]
fn the_viewers_walk_parses_a_line_its_index_did_not_see_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let line = |i: usize| format!("{:<99}\n", format!("fn f{i}() {{}} //"));
    let body: String = (0..300).map(line).collect();
    let path = tmp.path().join("changed.rs");
    std::fs::write(&path, &body).unwrap();
    let mut doc = text::TextDoc::open(&path).unwrap();
    // The file changes under its index, its size kept: lines 11 to 60 run together.
    let mut changed = body.into_bytes();
    for i in 10..60 {
        changed[i * 100 + 99] = b' ';
    }
    std::fs::write(&path, &changed).unwrap();
    let syntax = text::syntax_of("changed.rs", &mut doc).unwrap().unwrap();
    let mut hl = Highlight::new(syntax, Color32::WHITE, u64::MAX).unwrap();
    let runs = hl.page(&mut doc, 0, 20, &|| false).unwrap().unwrap();
    assert!(runs[10].len() > 1, "the line read cut is parsed");
    assert!(
        runs[11..].iter().all(|r| r.len() > 1),
        "the lines after it are coloured"
    );
}

/// R1 MUST 8: typing at the end of a long line decodes at most a few strides a frame — the
/// checkpoints before the edit stay.
#[test]
fn typing_at_the_end_of_a_long_line_decodes_a_bounded_stretch() {
    let mut v = view(&format!("{}\nb\n", "a".repeat(1 << 20)), "line.txt");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::key(Key::End, Modifiers::NONE));
    drv.idle(&mut v, 1);
    for _ in 0..3 {
        v.press(&Press::Text("z".into()));
        DECODED.with(|n| n.set(0));
        drv.idle(&mut v, 1);
        let decoded = DECODED.with(std::cell::Cell::get);
        assert!(decoded <= 4 * STRIDE, "{decoded} bytes decoded for a key");
    }
}

/// R1 MUST 9: the input method stays on while the editor has the keyboard with its caret
/// scrolled out of view, its rectangle inside the text.
#[test]
fn the_input_method_is_on_with_the_caret_scrolled_away() {
    let mut w = Ime(view(&rust(200), "far.rs"), None);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut w, 1);
    drv.wheel(&mut w, Pos2::new(100.0, 100.0), -20.0);
    drv.idle(&mut w, 1);
    let l = screen(&drv);
    assert_eq!(l.caret, None, "scrolled away");
    let ime = w.1.expect("the input method on");
    let text = l.rect(Cells::new(6, 2, 73, 20));
    assert!(text.contains_rect(ime.cursor_rect), "{:?}", ime.cursor_rect);
}

/// R1 S1: a highlighter that fails leaves every row plain — no old colours laid around edits.
#[test]
fn a_failed_highlighter_leaves_every_row_plain() {
    let mut v = view(&rust(100), "boom.rs");
    let boom = Arc::new(AtomicBool::new(false));
    let armed = boom.clone();
    v.hook = Some(Arc::new(move |line: &str| {
        if armed.load(Ordering::SeqCst) && line.contains("item 1\"") {
            panic!("a test's parser failing on purpose");
        }
    }));
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    boom.store(true, Ordering::SeqCst);
    go_to_line(&mut v, 50);
    v.press(&Press::Text("x".into()));
    v.ed.place(0, false);
    v.follow = true;
    drv.until(&mut v, 30, |v| v.hl.worker.is_none());
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    for y in 2..22 {
        let colors = glyph_colors(&l, 6, y);
        assert!(
            colors.iter().all(|&c| c == PALETTE.text),
            "row {y}: {colors:?}"
        );
    }
}

/// A Rust file past 16 MiB that the viewer stops short of 16 MiB: `/*`, then 4,300 lines of
/// 4,000 `x` (all comment, cheap to parse; line k starts at 3 + 4,001 (k − 1)); and the line of
/// the viewer's last index mark at or below 16 MiB — a mark every 256 lines, or at the first
/// line start a span (64 KiB) past the last.
fn past_16_mib() -> (String, u64) {
    let body = format!("/*\n{}", format!("{}\n", "x".repeat(4000)).repeat(4_300));
    let start = |k: u64| if k == 0 { 0 } else { 3 + 4001 * (k - 1) };
    let (mut last, mut k) = (0u64, 0u64);
    let stop = loop {
        k += 1;
        if k - last >= 256 || start(k) - start(last) >= 64 * 1024 {
            if start(k) > HIGHLIGHT_MAX {
                break last;
            }
            last = k;
        }
    };
    assert!(
        stop + 5 < (HIGHLIGHT_MAX - 3) / 4001,
        "the viewer stops short of 16 MiB"
    );
    (body, stop)
}

/// Whether line `line` of `v` is drawn all in `want` (and draws something), once highlighting
/// has answered with the caret on it.
fn line_is(drv: &mut Driver, v: &mut EditView, line: u64, want: Color32) -> Result<(), String> {
    go_to_line(v, line);
    drv.until(v, 60, |v| v.ready());
    let (y, x) = place_of(v, line);
    let colors = glyph_colors(&screen(drv), x, y);
    match !colors.is_empty() && colors.iter().all(|&c| c == want) {
        true => Ok(()),
        false => Err(format!("line {line}: {colors:?}")),
    }
}

/// R1 S5: highlighting stops where the viewer's does — at the last of its index marks at or below
/// 16 MiB — not at the last line starting below 16 MiB.
#[test]
fn highlighting_stops_where_the_viewers_does() {
    let (body, stop) = past_16_mib();
    let mut v = view(&body, "after.rs");
    let mut drv = Driver::new(640.0, 400.0);
    line_is(&mut drv, &mut v, stop - 1, cga::CYAN).unwrap();
    line_is(&mut drv, &mut v, stop + 3, PALETTE.text).unwrap();
}

/// R1 S6: in an ambiguous-wide terminal a control character's glyph is the viewer's stand-in.
#[test]
fn control_glyphs_take_the_viewers_stand_in() {
    // BEL's glyph is `•`, of ambiguous width.
    assert_eq!(chrome::shown_char('\u{7}'), '•');
    assert!(crate::gui::term::ambiguous('•'));
    let mut v = view("\u{7}x\n", "ctl.txt");
    let mut drv = Driver::new(640.0, 400.0);
    cells::set_ambiguous_wide(&drv.ctx, true);
    drv.idle(&mut v, 1);
    let want = crate::gui::term::narrow("•").unwrap_or(".");
    assert_eq!(screen(&drv).seen()[2][6].symbol, want);
}

/// R1 S7: the wheel's part of a row is kept for the next event, as the viewer's is.
#[test]
fn the_wheels_part_rows_add_up() {
    let mut v = view(&rust(200), "wheel.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    let event = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::Vec2::new(0.0, -6.4),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    drv.move_to(&mut v, Pos2::new(100.0, 100.0));
    for _ in 0..3 {
        drv.frame(&mut v, vec![event.clone()], Modifiers::NONE);
    }
    assert_eq!(v.top, 1, "0.4 + 0.4 + 0.4 rows");
}

/// R1 S9: Overwrite's caret on a wide glyph is both its cells in swapped colours, with no
/// Insert foot drawn over them.
#[test]
fn the_overwrite_caret_covers_a_wide_glyph() {
    let mut v = view("가나\n", "wide.txt");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::key(Key::Insert, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let swapped = (Some(PALETTE.back), Some(PALETTE.text));
    let seen = l.seen();
    assert_eq!((seen[2][6].fg, seen[2][6].bg), swapped);
    assert_eq!((seen[2][7].fg, seen[2][7].bg), swapped);
    // The foot cells::paint draws at the caret is covered in the block's colour.
    let cells = l.rect(Cells::new(6, 2, 2, 1));
    let foot = egui::Rect::from_min_max(egui::pos2(cells.left(), cells.bottom() - 2.0), cells.max);
    let rects = drv.rects();
    let last = rects
        .iter()
        .rposition(|(r, _, _)| r.intersects(foot))
        .expect("the foot");
    assert_eq!(
        rects[last].1, PALETTE.text,
        "the last paint over the foot is the block's"
    );
    assert!(rects[last].0.contains_rect(foot));
}

/// R1 S10: the `^S` prompt shortens the file's name, never its tail.
#[test]
fn the_save_prompt_shortens_the_name() {
    let name = format!("{}.rs", "a_very_long_name_".repeat(4));
    let mut v = view_in("fn main() {}\n", &name, Backend::Terminal);
    let mut drv = Driver::new(1000.0, 400.0);
    v.press(&ctrl(Key::S));
    drv.idle(&mut v, 1);
    let status = row(&screen(&drv), 23);
    let status = status.trim();
    assert!(status.starts_with("Save over …"), "{status}");
    // At 80 columns the words alone are wider than the row: the name keeps its end, then the
    // words' end gives way — `[Save]` stays.
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    let narrow = row(&screen(&drv), 23);
    assert!(
        narrow
            .trim()
            .starts_with("Save over …ng_name_.rs? F2 or a click on [Save] saves"),
        "{narrow}"
    );
    assert!(
        status.ends_with(
            "name_.rs? F2 or a click on [Save] saves · C saves a copy · any other key cancels"
        ),
        "{status}"
    );
}

// ---------------------------------------------------------------------------------------------
// Review round 2 (e1d9c7c).

/// R2 1: a reply that ends at a new stop takes its whole page: text over `LINE_MAX` typed into
/// the first line, and every row after it goes plain (EH-2) — none keeps its old colours.
#[test]
fn rows_past_a_new_stop_go_plain() {
    let mut v = view(&rust(100), "stop.rs");
    let mut drv = Driver::new(640.0, 400.0);
    drv.until(&mut v, 30, |v| v.ready());
    let plain = PALETTE.text;
    assert!(
        glyph_colors(&screen(&drv), 6, 3).contains(&cga::LIGHT_CYAN),
        "line 2's string"
    );
    v.ed.place(0, false);
    v.press(&Press::Text("x".repeat(LINE_MAX + 100)));
    v.ed.place(0, false);
    v.follow = true;
    drv.until(&mut v, 30, |v| v.ready());
    let l = screen(&drv);
    let rows: Vec<Vec<Color32>> = (3..22)
        .map(|y| glyph_colors(&l, 6, y))
        .filter(|c| !c.is_empty())
        .collect();
    assert!(rows.len() >= 15, "{} rows with text", rows.len());
    for colors in rows {
        assert!(colors.iter().all(|&c| c == plain), "{colors:?}");
    }
}

/// The cells of rows `rows` of `l`: symbol and colours, and the caret.
fn drawn(
    l: &cells::Layer,
    rows: std::ops::Range<usize>,
) -> (Vec<Vec<cells::Seen>>, Option<Position>) {
    (l.seen()[rows].to_vec(), l.caret)
}

/// What a fresh view of `v`'s document draws — no checkpoints — with `v`'s caret, top and
/// column: its text rows' cells and its caret.
fn fresh(drv: &mut Driver, v: &EditView) -> (Vec<Vec<cells::Seen>>, Option<Position>) {
    let mut bytes = Vec::new();
    v.ed.buffer().read(0..v.ed.buffer().len(), &mut bytes);
    let mut f = view(&String::from_utf8(bytes).unwrap(), &v.name);
    f.ed.place(v.ed.caret(), false);
    (f.top, f.left, f.rows, f.cols, f.follow) = (v.top, v.left, v.rows, v.cols, false);
    drv.idle(&mut f, 1);
    drawn(&screen(drv), 2..22)
}

/// R2 3: the checkpoints kept across an edit earlier on a long line already walked, and across
/// a block move that keeps the length and the line count, draw what a fresh view draws.
#[test]
fn kept_checkpoints_draw_what_a_fresh_view_draws() {
    // Mixed widths — a wide glyph among letters, no tab (a tab would set a stale column right
    // again at the next multiple of 4) — so a stale checkpoint shows elsewhere.
    let unit = "ab한cd";
    let mut v = view(&format!("{}\nend\n", unit.repeat(40_000)), "mixed.txt");
    let mut drv = Driver::new(640.0, 400.0);
    let end = Press::key(Key::End, Modifiers::NONE);
    v.press(&end);
    drv.idle(&mut v, 1);
    assert!(!v.walked.0.is_empty(), "the line walked keeps checkpoints");
    // An edit early on it, the caret back at its end.
    v.ed.place(98, false);
    v.press(&Press::Text("한한".into()));
    v.press(&end);
    drv.idle(&mut v, 1);
    let kept = drawn(&screen(&drv), 2..22);
    assert_eq!(kept, fresh(&mut drv, &v), "after an edit at byte 98");
    // A block move far along the line: the length and the line count stay.
    let (len, lines) = (v.ed.buffer().len(), v.ed.buffer().lines());
    v.ed.place(98, false);
    v.press(&ctrl(Key::K));
    v.press(&Press::key(Key::B, Modifiers::NONE));
    v.ed.place(104, false);
    v.press(&ctrl(Key::K));
    v.press(&Press::key(Key::K, Modifiers::NONE));
    v.ed.place(104 + 7 * 28_571, false);
    v.press(&ctrl(Key::K));
    v.press(&Press::key(Key::V, Modifiers::NONE));
    assert_eq!((v.ed.buffer().len(), v.ed.buffer().lines()), (len, lines));
    let mut moved = Vec::new();
    v.ed.buffer().read(98..104, &mut moved);
    assert_eq!(moved, "ab한c".as_bytes(), "the pair moved off byte 98");
    // The caret between the block's two places, where every column moved by the pair's four.
    v.ed.place(98 + 7 * 21_428, false);
    v.follow = true;
    drv.idle(&mut v, 1);
    let kept = drawn(&screen(&drv), 2..22);
    assert_eq!(kept, fresh(&mut drv, &v), "after the block move");
}

/// R2 4: an edit that keeps the caret still brings its character into view — Del pulls `가`
/// onto the last column — and Overwrite's cover stays inside the text, never over the scroll bar.
#[test]
fn an_edit_that_keeps_the_caret_follows_it() {
    let mut v = view(&format!("{}x가\n", "a".repeat(72)), "del.txt");
    let mut drv = Driver::new(640.0, 400.0);
    v.ed.place(72, false);
    v.follow = true;
    drv.idle(&mut v, 1);
    assert_eq!(screen(&drv).caret, Some(Position::new(78, 2)));
    v.press(&Press::key(Key::Delete, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let caret = l.caret.expect("the caret on screen");
    let under = &l.seen()[usize::from(caret.y)][usize::from(caret.x)];
    assert_eq!(under.symbol, "가", "the caret's character under it");
    // `가` at the last column, cut there (the text put so by hand), in Overwrite.
    (v.left, v.follow) = (0, false);
    v.press(&Press::key(Key::Insert, Modifiers::NONE));
    drv.idle(&mut v, 1);
    let l = screen(&drv);
    let bar = l.rect(Cells::new(79, 2, 1, 1));
    let foot = egui::Rect::from_min_max(egui::pos2(bar.left(), bar.bottom() - 2.0), bar.max);
    let rects = drv.rects();
    let over = |r: &egui::Rect| r.intersect(foot).area() > 0.0;
    let last = rects
        .iter()
        .rposition(|(r, _, _)| over(r))
        .expect("the bar");
    assert_eq!(
        rects[last].1, PALETTE.track_back,
        "the scroll bar's own colour on top"
    );
}

/// R2 5: combining marks are counted per row: glyphs each followed by just under a stride of
/// marks still cost a row about a stride.
#[test]
fn marks_are_counted_per_row() {
    let run = "\u{301}".repeat(30_000);
    let line: String = (0..10)
        .map(|i| format!("{}{run}", char::from(b'a' + i)))
        .collect();
    let mut v = view(&format!("{line}\nnext\n"), "runs.txt");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut v, 1);
    DECODED.with(|n| n.set(0));
    drv.idle(&mut v, 1);
    let decoded = DECODED.with(std::cell::Cell::get);
    assert!(decoded <= 4 * STRIDE, "{decoded} bytes decoded in a frame");
}

/// R2 6: an edit past the stop line finds the stop again: cutting the file below 16 MiB colours
/// the lines past the viewer's old stop.
#[test]
fn an_edit_past_the_stop_finds_it_again() {
    let (body, stop) = past_16_mib();
    let mut v = view(&body, "cut.rs");
    let mut drv = Driver::new(640.0, 400.0);
    line_is(&mut drv, &mut v, stop + 3, PALETTE.text).unwrap();
    let from = v.ed.buffer().line_start(stop + 7).unwrap();
    v.ed.place(from, false);
    v.press(&Press::key(Key::End, Modifiers::COMMAND | Modifiers::SHIFT));
    v.press(&Press::key(Key::Delete, Modifiers::NONE));
    assert!(v.ed.buffer().len() <= HIGHLIGHT_MAX);
    line_is(&mut drv, &mut v, stop + 3, cga::CYAN).unwrap();
}
