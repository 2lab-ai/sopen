//! The editor's screen (12 §4, §8; ED-screen): its cells read back as both backends show them,
//! highlighting through the buffer's line source (EH-1, EH-2, the kept states' spacing), the
//! input method's output (E10, E20) and the text document's Bulk reader.

use super::*;
use crate::edit::ops::Doubt;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::Driver;
use eframe::egui::{Event, ImeEvent, Key, Modifiers};
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
    drv.idle(&mut v, 30);
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

/// E19: the editor's kept states stand every 64 lines — one per 64 lines up to the last.
#[test]
fn kept_states_stand_every_64_lines() {
    let mut kept = KeptStates::new(rust(1_000).into_bytes(), "spaced.rs");
    kept.fill();
    assert_eq!(STATE_LINES, 64);
    assert_eq!(kept.count(), 999 / 64 + 1);
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
    let doc = text::TextDoc::streamed(source);
    let mut reader = doc.bulk_reader().expect("a stream's document has one");
    let mut got = vec![0; bytes.len()];
    let n =
        crate::stream::Bytes::read_at(&mut reader, 0, &mut got, &AtomicBool::new(false)).unwrap();
    assert_eq!((n, got), (bytes.len(), bytes));
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
