//! The text viewer on the cell grid (10 §4–§6, X3-text): every part of its screen — the bar, the
//! frame and its scroll bar, the gutter, the rows, the notes, the status row and Go to's field, a
//! preview's last row — is cells, a glyph per cell at the cell's corner, so the terminal shows it
//! where a hand-off box stood. Widths are unicode-width's: Hangul and an emoji two cells, a
//! combining mark in its base's cell. The window's `SOPEN_CELLS` and the terminal's are the same
//! bytes for a UTF-16 Hangul file in view 1, and with F8's list open (TS-4 itself, through the
//! terminal's bytes, is `term::tests::text`); TK-3's `W` through the translator.

use super::*;
use crate::gui::browser::{Browser, Connect, Launch, Start};
use crate::gui::cells;
use crate::gui::chrome::CELL;
use crate::gui::term::{self, Backend, End, Tty};
use crate::gui::testkit::command;
use crate::remote::{LocalFs, RemoteFs};
use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;

/// The grid of the last pass: the whole cells of the window.
fn grid_of(drv: &Driver) -> (u16, u16) {
    let r = drv.ctx.content_rect();
    ((r.width() / CELL.x) as u16, (r.height() / CELL.y) as u16)
}

/// Nothing of the last pass is egui's: laid as the terminal lays it, no cell is left to a
/// hand-off box, and every text egui drew is one glyph at a cell's corner (the cell painter's).
fn all_cells(drv: &Driver, what: &str) {
    let (cols, rows) = grid_of(drv);
    let screen = term::compose(&drv.ctx, &cells::layers(&drv.ctx), cols, rows);
    assert!(
        screen.hand_offs.is_empty(),
        "{what}: cells no layer covers: {:?}",
        screen.hand_offs
    );
    for (at, galley) in drv.galleys() {
        let text = galley.text();
        assert!(
            text.graphemes(true).count() <= 1 && at.x % CELL.x == 0.0 && at.y % CELL.y == 0.0,
            "{what}: egui laid out {text:?} at {at:?}"
        );
    }
}

/// Every part of the viewer is cells: the viewer, a preview, notes, Go to's field open, and
/// grids too small for its rows (nothing panics, nothing is left to egui).
#[test]
fn every_part_of_the_viewer_is_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "main.rs", RUST);
    let mut drv = Driver::new(800.0, 480.0);
    drv.until(&mut win, 5, |w| w.ready());
    all_cells(&drv, "the viewer");
    drv.key(&mut win, Key::G, command(false));
    drv.frame(
        &mut win,
        vec![egui::Event::Text("3".into())],
        Modifiers::NONE,
    );
    all_cells(&drv, "Go to open");
    let mut preview = viewer(tmp.path(), "notes.txt", "a preview\n");
    Window::set_preview(&mut preview, true);
    let mut drv = Driver::new(400.0, 320.0);
    drv.until(&mut preview, 5, |w| w.ready());
    all_cells(&drv, "a preview");
    let path = write(tmp.path(), "noted.txt", b"one\ntwo\n");
    let notice = "the edits an earlier copy kept were not saved: they are in the journal, which \
                  this sentence names whole however long it is";
    let mut noted = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:noted".into(),
        "noted.txt",
        vec![notice.to_string()],
    );
    let mut drv = Driver::new(400.0, 320.0);
    drv.until(&mut noted, 5, |w| w.ready());
    all_cells(&drv, "notes");
    for (w, h) in [(80.0, 64.0), (24.0, 32.0), (8.0, 16.0), (4.0, 4.0)] {
        let mut win = viewer(tmp.path(), "tiny.rs", RUST);
        let mut drv = Driver::new(w, h);
        drv.idle(&mut win, 3);
        all_cells(&drv, &format!("{w}×{h}"));
    }
}

/// The cells of the layer that holds `text` in one row: that row's cells from where `text`
/// starts, each its symbol (a wide glyph's second cell empty), as many as `n`.
fn cells_from(drv: &Driver, text: &str, n: usize) -> Vec<String> {
    let layers = cells::layers(&drv.ctx);
    let hit = cells::find(&layers, text)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{text:?} is not in cells"));
    let l = &layers[hit.layer];
    let a = l.buf.area;
    let row = &l.seen()[usize::from(hit.cells.y - a.y)];
    let from = usize::from(hit.cells.x - a.x);
    row[from..(from + n).min(row.len())]
        .iter()
        .map(|s| s.symbol.clone())
        .collect()
}

/// Hangul and an emoji take two cells a glyph, a combining mark stays in its base's cell —
/// wrapped and not — and the cells hold the row's characters in order.
#[test]
fn hangul_emoji_and_marks_take_their_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "ko.txt", "한글 😀 e\u{301}x end\n");
    let mut drv = Driver::new(640.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    let want = [
        "한", "", "글", "", " ", "😀", "", " ", "e\u{301}", "x", " ", "e",
    ];
    for mode in ["wrapped", "not wrapped"] {
        assert_eq!(cells_from(&drv, "한글", want.len()), want, "{mode}");
        drv.key(&mut win, Key::W, Modifiers::NONE);
        drv.until(&mut win, 5, |w| w.ready());
    }
}

/// The column of the frame's scroll bar (its `↑` in the bar's colours) and the first row of text
/// cells: the rows' room ends there.
fn bar_column(drv: &Driver) -> u16 {
    let p = chrome::PALETTE;
    for l in cells::layers(&drv.ctx) {
        for (y, row) in l.seen().iter().enumerate() {
            for (x, s) in row.iter().enumerate() {
                if s.symbol == "↑" && (s.fg, s.bg) == (Some(p.track), Some(p.track_back)) {
                    let _ = y;
                    return l.buf.area.x + x as u16;
                }
            }
        }
    }
    panic!("no scroll bar in cells");
}

/// Wrapped, every row of a Hangul line fits the cells between the text's column and the scroll
/// bar — a syllable two cells — and is as full as the next glyph allows; the rows are the line.
#[test]
fn hangul_rows_wrap_by_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..60).map(|i| format!("가나다라{i:02} ")).collect();
    let mut win = viewer(tmp.path(), "ko.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(400.0, 480.0);
    drv.until(&mut win, 5, |w| w.ready());
    let start = cells::find(&cells::layers(&drv.ctx), "가나다라00")
        .first()
        .expect("the first row in cells")
        .cells
        .x;
    let room = usize::from(bar_column(&drv) - start);
    let rows: Vec<String> = win
        .page
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.cut.text())
        .collect();
    let of_line = rows.iter().take_while(|r| r.as_str() != "next");
    let mut joined = String::new();
    for row in of_line {
        assert!(
            cells::width(row) <= room,
            "{row:?} is wider than {room} cells"
        );
        assert!(
            cells::width(row) + 11 > room,
            "{row:?} leaves room for the next word in {room} cells"
        );
        assert!(cells::find(&cells::layers(&drv.ctx), row.trim_end()).len() == 1);
        joined.push_str(row);
    }
    assert!(line.starts_with(&joined), "{joined:?}");
}

/// Not wrapped, → scrolls by bytes to a character's start, and what the row shows is laid by
/// cells: no U+FFFD, the text from that character on, and a syllable that would not fit at the
/// right edge is not cut in half there.
#[test]
fn sideways_through_hangul_by_cells() {
    let tmp = tempfile::tempdir().unwrap();
    let line: String = (0..80).map(|i| format!("한{i:02} ")).collect();
    let mut win = viewer(tmp.path(), "ko.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(400.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.key(&mut win, Key::ArrowRight, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    // Byte 8 is inside 한 of "한01 " (bytes 6–8): the row shows from the character after it.
    let row = win.page.as_ref().unwrap().rows[0].cut.text();
    assert!(row.starts_with("01 한02 "), "{row:?}");
    let room = usize::from(bar_column(&drv)) - {
        let hit = cells::find(&cells::layers(&drv.ctx), "01 한02");
        hit.first().expect("the row in cells").cells.x as usize
    };
    let shown = cells_from(&drv, "01 한02", room);
    let text: String = shown.concat();
    assert!(!text.contains('\u{FFFD}'));
    assert!(row.starts_with(text.trim_end()), "{text:?} of {row:?}");
    let laid = cells::width(text.trim_end());
    let next = row[text.trim_end().len()..].chars().next().unwrap();
    assert!(
        laid + cells::width(&next.to_string()) > room || next == ' ',
        "{laid} of {room} cells shown, {next:?} would fit"
    );
}

/// The bar (08 §7.5, VM-3): the encoding whole at its right end, the stats before it — cut at
/// their end where they do not fit — and the title in what is left, cut from the left.
#[test]
fn the_bar_keeps_the_encoding_and_cuts_the_title_from_the_left() {
    let tmp = tempfile::tempdir().unwrap();
    let text = "2026-11-25 10:00:00 INFO user=0001 사용자가 로그인\r\n".repeat(30);
    let path = write(tmp.path(), "notes.dat", &encoded(&text, Utf16Le, false));
    let title = format!("box:/{}/notes.dat", "a-long-folder-name".repeat(8));
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        title.clone(),
        "notes.dat",
        vec![],
    );
    let bar = |drv: &Driver| -> String {
        let layers = cells::layers(&drv.ctx);
        let hit = cells::find(&layers, "UTF-16LE (guessed)");
        let hit = hit.first().expect("the encoding in cells");
        cells::row_text(&layers[hit.layer].buf, hit.cells.y).0
    };
    let stats = |win: &TextWindow| {
        format!(
            "{} lines · {} · plain text ·",
            win.doc.line_count(),
            crate::human(win.doc.size())
        )
    };
    // 70 cells: the title cut from the left, two cells, the stats whole, a cell, the encoding
    // whole, a cell.
    let mut drv = Driver::new(560.0, 320.0);
    drv.until(&mut win, 10, |w| w.ready());
    let row = bar(&drv);
    assert_eq!(cells::width(&row), 70, "{row:?}");
    let tail = format!("  {} UTF-16LE (guessed) ", stats(&win));
    assert!(row.ends_with(&tail), "{row:?}");
    let shown = row[1..row.len() - tail.len()].to_string();
    assert!(
        shown.starts_with('…'),
        "the title cut from the left: {row:?}"
    );
    assert!(
        title.ends_with(&shown['…'.len_utf8()..]),
        "{shown:?} of {title:?}"
    );
    assert!(shown.ends_with("/notes.dat"), "{shown:?}");
    // 30 cells: the stats cut at their end, the encoding whole; no room for the title.
    let mut drv = Driver::new(240.0, 320.0);
    drv.idle(&mut win, 3);
    let row = bar(&drv);
    assert_eq!(row, " 30 lines… UTF-16LE (guessed) ");
    // Wide enough, all of it.
    let mut drv = Driver::new(2400.0, 320.0);
    drv.idle(&mut win, 3);
    let row = bar(&drv);
    assert!(row.starts_with(&format!(" {title}  ")), "{row:?}");
}

/// The bar never cuts the encoding while the grid holds it (08 §7.5, VM-3): its blank cells at
/// either end give way first — at a width of the label and of the label and a cell it is whole,
/// flush right; with its two cells it has its margin back.
#[test]
fn a_bar_as_narrow_as_the_encoding_keeps_it_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "bom.txt", &encoded("one\ntwo\n", Utf16Le, true));
    let mut win = TextWindow::new(TextDoc::open(&path).unwrap(), "box:b".into(), "b", vec![]);
    let label = "UTF-16LE (BOM)";
    let w = cells::width(label);
    for (cols, row) in [
        (w, label.to_string()),
        (w + 1, format!(" {label}")),
        (w + 2, format!(" {label} ")),
    ] {
        let mut drv = Driver::new(cols as f32 * CELL.x, 160.0);
        drv.until(&mut win, 10, |w| w.ready());
        drv.idle(&mut win, 1);
        let layers = cells::layers(&drv.ctx);
        let viewer = cells::find(&layers, "line");
        let l = &layers[viewer.first().expect("the status row").layer];
        assert_eq!(cells::row_text(&l.buf, 0).0, row, "{cols} cells");
    }
}

/// Go to (Cmd/Ctrl+G) is a cell field on the status row: `Go to`, then black cells with what is
/// typed in white and the caret after it; Enter goes there and the field goes.
#[test]
fn go_to_is_a_cell_field() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=200).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "rows.txt", &body);
    let mut drv = Driver::new(640.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::G, command(false));
    drv.frame(
        &mut win,
        vec![egui::Event::Text("150".into())],
        Modifiers::NONE,
    );
    drv.idle(&mut win, 1);
    let layers = cells::layers(&drv.ctx);
    let label = cells::find(&layers, "Go to")
        .into_iter()
        .next()
        .expect("Go to in cells");
    let l = &layers[label.layer];
    let row = &l.seen()[usize::from(label.cells.y - l.buf.area.y)];
    let field_at = usize::from(label.cells.right() - l.buf.area.x) + 1;
    let field: Vec<&cells::Seen> = row[field_at..field_at + 4].iter().collect();
    let black = crate::gui::chrome::cga::BLACK;
    assert!(field.iter().all(|s| s.bg == Some(black)), "{field:?}");
    let typed: String = field[..3].iter().map(|s| s.symbol.as_str()).collect();
    assert_eq!(typed, "150");
    let caret = l.caret.expect("the field's caret");
    assert_eq!(
        (caret.x, caret.y),
        (label.cells.right() + 1 + 3, label.cells.y),
        "after what is typed"
    );
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    drv.idle(&mut win, 1);
    assert!(cells::find(&cells::layers(&drv.ctx), "Go to").is_empty());
    assert_eq!(win.line, Some(149));
}

/// Not wrapped, a right-to-left line longer than the window shows its start: the cells that fit
/// are taken in the line's own order, then reversed into the cells' — its first letter at the
/// row's right end, nothing of what follows past the edge.
#[test]
fn an_unwrapped_right_to_left_line_shows_its_start() {
    let tmp = tempfile::tempdir().unwrap();
    let word = "\u{05E9}\u{05DC}\u{05D5}\u{05DD} ";
    let line = format!("\u{05D0}{}", word.repeat(80));
    let mut win = viewer(tmp.path(), "he.txt", &format!("{line}\nnext\n"));
    let mut drv = Driver::new(400.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    drv.key(&mut win, Key::W, Modifiers::NONE);
    drv.until(&mut win, 5, |w| w.ready());
    let room = win.room;
    let start: String = line.chars().take(room).collect();
    let visual: String = start.trim_end().chars().rev().collect();
    let layers = cells::layers(&drv.ctx);
    let hit = cells::find(&layers, &visual).into_iter().next();
    let hit = hit.unwrap_or_else(|| panic!("{visual:?} not in cells"));
    assert_eq!(
        hit.cells.right(),
        bar_column(&drv) - u16::from(start.ends_with(' '))
    );
}

/// The scroll bar's ↑ and ↓ step a row, as the DOS bar does; a click on its track is a jump
/// there (06 §7.2).
#[test]
fn the_bars_arrows_step_a_row_and_its_track_jumps() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (1..=300).map(|i| format!("row {i}\n")).collect();
    let mut win = viewer(tmp.path(), "rows.txt", &body);
    let mut drv = Driver::new(640.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    let (l, x, rows) = {
        let layers = cells::layers(&drv.ctx);
        let p = chrome::PALETTE;
        let found = layers.iter().find_map(|l| {
            let seen = l.seen();
            seen.iter().enumerate().find_map(|(dy, row)| {
                let dx = row.iter().position(|s| {
                    s.symbol == "↑" && (s.fg, s.bg) == (Some(p.track), Some(p.track_back))
                })?;
                let down = (dy..seen.len()).find(|&k| seen[k][dx].symbol == "↓")?;
                Some((l.clone(), dx as u16, dy as u16..down as u16 + 1))
            })
        });
        found.expect("the scroll bar")
    };
    let centre = |y: u16| l.rect(cells::Cells::new(x, y, 1, 1)).center();
    drv.click(&mut win, centre(rows.end - 1));
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(win.line, Some(1), "↓: a row down");
    drv.click(&mut win, centre(rows.end - 1));
    drv.click(&mut win, centre(rows.start));
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(win.line, Some(1), "↓ then ↑: back a row");
    drv.click(&mut win, centre(rows.end - 2));
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(
        win.line.map(|l| l + win.visible),
        Some(300),
        "the track's last cell: the end"
    );
}

/// A row's control characters are recorded as sopen's own glyphs, wrapped or not — C0 and DEL as
/// their code page 437 glyphs, a C1 control as `·` — and only those: laid for an ambiguous-wide
/// terminal each of width A is its stand-in (`•` `.`, `↑` `^`, `·` `.`) while the same glyphs
/// written in the file stay content; laid for any other, the glyphs.
#[test]
fn a_rows_control_glyphs_and_only_they_get_stand_ins() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(
        tmp.path(),
        "c.txt",
        "a\u{7}b\u{18}c\u{85}d\u{1}e\u{7F} ♥•↑·\n".as_bytes(),
    );
    let mut win = TextWindow::new(TextDoc::open(&path).unwrap(), "box:c".into(), "c", vec![]);
    let mut drv = Driver::new(400.0, 160.0);
    drv.until(&mut win, 5, |w| w.ready());
    for mode in ["wrapped", "not wrapped"] {
        let row = &win.page.as_ref().unwrap().rows[0];
        assert_eq!(row.cut.text(), "a•b↑c·d☺e⌂ ♥•↑·", "{mode}");
        assert_eq!(row.cut.own, [1, 3, 5, 7, 9], "{mode}");
        let laid = |wide: bool| -> String {
            let chars = row_chars(row, None, Color32::WHITE, Color32::GRAY, 64, wide);
            chars.into_iter().map(|(c, _)| c).collect()
        };
        // (`☺` and `⌂` are not of width A: one cell in every terminal.)
        assert_eq!(laid(true), "a.b^c.d☺e⌂ ♥•↑·", "{mode}");
        assert_eq!(laid(false), "a•b↑c·d☺e⌂ ♥•↑·", "{mode}");
        drv.key(&mut win, Key::W, Modifiers::NONE);
        drv.until(&mut win, 5, |w| w.ready());
    }
}

/// The notes (what must be read whole) are cells on the rows right above the status row, in the
/// error colour, wrapped by cells, the whole of each.
#[test]
fn the_notes_stand_above_the_status_row() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "noted.txt", b"one\ntwo\n");
    let notice = "first word, then a sentence long enough to wrap in a window of fifty cells, \
                  ending with the last";
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:noted".into(),
        "noted.txt",
        vec![notice.to_string()],
    );
    let mut drv = Driver::new(400.0, 320.0);
    drv.until(&mut win, 5, |w| w.ready());
    let layers = cells::layers(&drv.ctx);
    let at = |text: &str| {
        let hit = cells::find(&layers, text).into_iter().next();
        hit.unwrap_or_else(|| panic!("{text:?} not in cells"))
    };
    let (first, last, status) = (at("first word"), at("the last"), at("lines 1–2 of 2"));
    assert!(first.cells.y < last.cells.y, "wrapped");
    assert_eq!(
        last.cells.y + 1,
        status.cells.y,
        "right above the status row"
    );
    let red = chrome::PALETTE.error;
    for hit in [first, last] {
        assert_eq!(cells::colors(&layers[hit.layer], hit.cells), [red]);
    }
}

/// Notes that do not all fit are never dropped unsaid: the last row they have shows the start of
/// the next and how many rows are not shown — on a grid of six rows too, where the frame has no
/// row to spare.
#[test]
fn notes_cut_short_say_how_many_rows_are_not_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write(tmp.path(), "noted.txt", b"one\ntwo\n");
    let notice = "first word, then a sentence long enough to wrap in a window of fifty cells \
                  over and over again, ending with the last";
    let mut win = TextWindow::new(
        TextDoc::open(&path).unwrap(),
        "box:noted".into(),
        "noted.txt",
        vec![notice.to_string()],
    );
    let rows = crate::gui::browser::ops::dialog::wrap(notice, 48);
    assert_eq!(rows.len(), 3, "{rows:?}");
    for (h, shown) in [(6u16, 1usize), (7, 2), (8, 3)] {
        let mut drv = Driver::new(400.0, f32::from(h) * CELL.y);
        drv.until(&mut win, 5, |w| w.ready());
        drv.idle(&mut win, 1);
        let layers = cells::layers(&drv.ctx);
        let hit = cells::find(&layers, "first word");
        let hit = hit.first().unwrap_or_else(|| panic!("{h} rows: the notes"));
        let buf = &layers[hit.layer].buf;
        let row = |y: u16| cells::row_text(buf, y).0.trim().to_string();
        let first = h - 1 - shown as u16;
        assert_eq!(hit.cells.y, first, "{h} rows: right above the status row");
        for (k, want) in rows.iter().enumerate().take(shown - 1) {
            assert_eq!(&row(first + k as u16), want, "{h} rows");
        }
        let last = row(h - 2);
        let hidden = rows.len() - shown;
        if hidden == 0 {
            assert_eq!(last, rows[2], "{h} rows: all of them");
        } else {
            let start: String = rows[shown - 1].chars().take(8).collect();
            assert!(last.starts_with(&start), "{h} rows: {last:?}");
            assert!(
                last.ends_with(&format!("… {hidden} more")),
                "{h} rows: {last:?}"
            );
        }
    }
}

/// A row full just before a tab: the tab is the space the row ends at — taken, not shown — and
/// the next row starts with the next word, as with a space (a tab is a space to line breaking).
#[test]
fn a_row_full_before_a_tab_ends_there() {
    let cols = columns(600.0, 400.0);
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(
        tmp.path(),
        "tab.txt",
        &format!("{}\tz end\n", "y".repeat(cols)),
    );
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(rows_of(&win)[..2], ["y".repeat(cols), "z end".to_string()]);
}

/// A space with a mark on it is no row-end space: at a full row's end it starts the next row,
/// its mark with it — never swallowed as a space the row broke at is.
#[test]
fn a_space_with_a_mark_is_not_swallowed_at_a_rows_end() {
    let cols = columns(600.0, 400.0);
    let tmp = tempfile::tempdir().unwrap();
    let body = format!("{} \u{301}z end\n", "y".repeat(cols));
    let mut win = viewer(tmp.path(), "mark.txt", &body);
    let mut drv = Driver::new(600.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    assert_eq!(
        rows_of(&win)[..2],
        ["y".repeat(cols), " \u{301}z end".to_string()]
    );
}

/// In an ambiguous-wide terminal the status row's own words get their stand-ins — the cut's
/// `…` — but a message's own text (a path with `·` and `…` in it) is the file system's, as it
/// is: content (T38).
#[test]
fn a_status_message_keeps_its_own_text_in_a_wide_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut win = viewer(tmp.path(), "s.txt", "one\ntwo\n");
    let mut drv = Driver::new(640.0, 160.0);
    crate::gui::cells::set_ambiguous_wide(&drv.ctx, true);
    drv.until(&mut win, 5, |w| w.ready());
    win.status = Status::error("cannot read /srv/a·b…c.txt");
    drv.idle(&mut win, 1);
    let row = |drv: &Driver| -> String {
        let layers = cells::layers(&drv.ctx);
        let hit = cells::find(&layers, "lines 1");
        let hit = hit.first().expect("the status row");
        cells::row_text(&layers[hit.layer].buf, hit.cells.y).0
    };
    assert!(
        row(&drv).contains("cannot read /srv/a·b…c.txt"),
        "{:?}",
        row(&drv)
    );
    // Cut: the `…` the cut adds is sopen's.
    win.status = Status::error(format!("cannot read /srv/a·b…c.txt {}", "z".repeat(80)));
    drv.idle(&mut win, 1);
    let cut = row(&drv);
    assert!(
        cut.contains("/srv/a·b…c.txt zz") && cut.trim_end().ends_with("z~"),
        "{cut:?}"
    );
}

/// A right-to-left run with digits in it, as UAX #9 lays it in a left-to-right row: the run is
/// reversed whole, its digits kept in their order as a unit — and digits after a Latin letter
/// are that word's (`L`).
#[test]
fn digits_in_a_right_to_left_run_keep_their_order() {
    for (s, laid) in [
        ("שלום 123 עולם", "םלוע 123 םולש"),
        ("abc 123 def", "abc 123 def"),
        ("שלום 123 abc", "123 םולש abc"),
        ("x שלום 1.5 עולם 50% y", "x 50% םלוע 1.5 םולש y"),
        ("abc שלום, עולם. def", "abc םלוע ,םולש. def"),
    ] {
        let chars: Vec<(char, Color32)> = s.chars().map(|c| (c, Color32::WHITE)).collect();
        let order: String = in_cell_order(&chars).into_iter().map(|(g, _)| g).collect();
        assert_eq!(order, laid, "{s:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// TS-4 and TK-3: the browser's viewer in the window and in the terminal.

/// A folder holding a UTF-16LE Hangul file (with its BOM), and a browser on it.
struct Fx {
    _tmp: tempfile::TempDir,
    root: String,
}

fn hangul_folder() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("r");
    std::fs::create_dir_all(&root).unwrap();
    let lines = hangul_lines(60);
    let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
    std::fs::write(root.join("korean.txt"), encoded(&text, Utf16Le, true)).unwrap();
    let root = root.to_str().unwrap().to_string();
    Fx { _tmp: tmp, root }
}

fn browser_on(fx: &Fx) -> Rc<RefCell<Browser>> {
    let connect: Connect = Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
    let launch: Launch = Arc::new(|abs: &str, _| Ok(format!("opened {abs}")));
    let cache = std::path::Path::new(&fx.root).with_file_name("cache");
    Rc::new(RefCell::new(Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect,
        label: None,
        destination: None,
        abs: fx.root.clone(),
        cache: cache.clone(),
        state_dir: cache.with_file_name("state"),
        launch,
        git: None,
    })))
}

/// The browser as a window, shared with the test: the window's pass kept for its receipt — a
/// window the switch opened (TW-1), whose title row holds the mode button `[TUI]` where the
/// terminal's has `[GUI]`, so the two rows are laid alike and the receipts blank it (10 §5). In
/// the terminal (`.2`, its tape) it counts the passes it has been ready in since the tape's last
/// event and asks for the next at once until there are three, as `SOPEN_CELLS` waits; then the
/// tape stops the run.
struct Shared(Rc<RefCell<Browser>>, Option<term::WindowPass>, Option<Tape>);

impl Window for Shared {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.2.is_none() {
            chrome::set_mode_button(ui.ctx(), "[TUI]");
        }
        self.0.borrow_mut().ui(ui);
        self.1 = Some(term::keep_window_pass(ui.ctx()));
        if let Some(tape) = &self.2 {
            let ready = tape.0.borrow().events.is_empty() && self.0.borrow().ready();
            let mut t = tape.0.borrow_mut();
            t.settled = if ready { t.settled + 1 } else { 0 };
            if t.settled < 3 {
                ui.ctx().request_repaint();
            }
        }
    }

    fn ready(&self) -> bool {
        self.0.borrow().ready()
    }

    fn close_requested(&mut self) -> bool {
        self.0.borrow_mut().close_requested()
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        self.0.borrow().keys()
    }
}

/// A terminal's script: its size, the events it gives one at a time once the browser has
/// settled, and the passes ready in a row since the last (the run stops at three).
struct Script {
    size: (u16, u16),
    events: VecDeque<CEvent>,
    browser: Rc<RefCell<Browser>>,
    settled: usize,
}

/// A terminal that is never ambiguous-wide or kitty, read through the translator as typed.
#[derive(Clone)]
struct Tape(Rc<RefCell<Script>>);

impl Tty for Tape {
    fn raw(&mut self, _on: bool) -> io::Result<()> {
        Ok(())
    }

    fn size(&mut self) -> io::Result<(u16, u16)> {
        Ok(self.0.borrow().size)
    }

    fn probe(&mut self) -> term::Probed {
        term::Probed {
            kitty: false,
            ambiguous: false,
        }
    }

    fn drop_input(&mut self) {}

    fn next(&mut self, _wait: Duration) -> io::Result<Option<CEvent>> {
        let ready = self.0.borrow().browser.borrow().ready();
        if ready {
            if let Some(ev) = self.0.borrow_mut().events.pop_front() {
                return Ok(Some(ev));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
        Ok(None)
    }

    fn pending(&mut self) -> io::Result<bool> {
        Ok(false)
    }

    fn stopped(&self) -> Option<i32> {
        (self.0.borrow().settled >= 3).then_some(libc::SIGTERM)
    }
}

fn press(code: KeyCode) -> CEvent {
    CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// The terminal's `SOPEN_CELLS` receipt (`term::cells_text` of its screen, its mode button left
/// out) of the browser on `fx`, `cols` × `rows`, once `events` have been played into it through
/// the translator — keys a receipt run itself would not read: there they are `SOPEN_KEYS`'.
fn terminal_receipt(fx: &Fx, cols: u16, rows: u16, events: Vec<CEvent>) -> String {
    chrome::set_test_now(Some(crate::gui::testkit::START_OF_A_MINUTE));
    let b = browser_on(fx);
    let tape = Tape(Rc::new(RefCell::new(Script {
        size: (cols, rows),
        events: events.into(),
        browser: b.clone(),
        settled: 0,
    })));
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut backend = Backend::new(tape.clone(), io::sink(), ctx.clone(), true);
    let mut win = Shared(b, None, Some(tape.clone()));
    let mut no_switch = |_: &mut Backend<Tape, io::Sink>| -> io::Result<Option<String>> {
        panic!("no switch here")
    };
    let end = backend.run(&mut win, &mut no_switch).unwrap();
    assert_eq!(end, End::Signal(libc::SIGTERM), "settled");
    let mode = chrome::mode_button_rect(&ctx).map(|r| {
        let (x, y) = ((r.min.x / CELL.x) as u16, (r.min.y / CELL.y) as u16);
        cells::Cells::new(x, y, (r.width() / CELL.x).round() as u16, 1)
    });
    assert!(mode.is_some(), "the terminal's [GUI]");
    term::cells_text(&backend.screen(), mode)
}

/// The window's receipt of the browser on `fx`, `cols` × `rows`, after `keys`.
fn window_receipt(fx: &Fx, cols: u16, rows: u16, keys: &[Key]) -> String {
    let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
    let mut win = Shared(browser_on(fx), None, None);
    drv.until(&mut win, 20, |w| w.ready());
    for &key in keys {
        drv.key(&mut win, key, Modifiers::NONE);
        drv.until(&mut win, 20, |w| w.ready());
    }
    drv.idle(&mut win, 2);
    let pass = win.1.take().expect("a pass");
    term::window_receipt(&drv.ctx, &pass)
}

/// `receipt` with the list's free space — the disk's, which changes between two runs — as `#`s.
fn steady(receipt: &str) -> String {
    let lines = receipt.lines().map(|line| match line.find(" Bytes Free") {
        Some(end) => {
            let start = line[..end]
                .rfind(|c: char| !c.is_ascii_digit() && c != ',')
                .map_or(0, |i| i + 1);
            format!(
                "{}{}{}",
                &line[..start],
                "#".repeat(end - start),
                &line[end..]
            )
        }
        None => line.to_string(),
    });
    lines.collect::<Vec<_>>().join("\n")
}

/// `SOPEN_CELLS` (10 §15) of the text viewer: a UTF-16 Hangul file in view 1 is the same
/// receipt from the window and from the terminal's run — its rows there, no hand-off box — and
/// so it is with F8's list of encodings open over it, and as the preview beside the list (its
/// rows; its host's frame is X3-preview's). The window is one the switch opened (`[TUI]`): its
/// title row is laid as the terminal's. (TS-4 through the terminal's bytes:
/// `term::tests::text`.)
#[test]
fn the_cells_receipts_of_both_backends_agree_on_the_text_viewer() {
    let fx = hangul_folder();
    let (cols, rows) = (160, 30);
    for (what, keys, events) in [
        (
            "view 1",
            vec![Key::ArrowDown, Key::Enter],
            vec![press(KeyCode::Down), press(KeyCode::Enter)],
        ),
        (
            "F8's list",
            vec![Key::ArrowDown, Key::Enter, Key::F8],
            vec![
                press(KeyCode::Down),
                press(KeyCode::Enter),
                press(KeyCode::F(8)),
            ],
        ),
        (
            "the preview",
            vec![Key::ArrowDown],
            vec![press(KeyCode::Down)],
        ),
    ] {
        let window = window_receipt(&fx, cols, rows, &keys);
        let terminal = terminal_receipt(&fx, cols, rows, events);
        let shown: &[&str] = if what == "the preview" {
            // The preview's rows and frame in cells; its host's ring is X3-preview's.
            &["0001 한글 줄 text", "Text Viewer"]
        } else {
            &["0001 한글 줄 text", "UTF-16LE (BOM)", "Text Viewer"]
        };
        for shown in shown {
            assert!(window.contains(shown), "{what}: {shown:?} in:\n{window}");
        }
        assert!(!window.contains(term::HAND_OFF), "{what}:\n{window}");
        if what == "F8's list" {
            assert!(window.contains("Encoding"), "{window}");
        }
        assert_eq!(steady(&terminal), steady(&window), "{what}: the same bytes");
    }
}

/// TK-3 (10 §14): in the terminal `w` — through the translator, a key and its text — unwraps the
/// text viewer's lines (the F-key row then offers Wrap), and `w` again wraps them.
#[test]
fn tk3_w_wraps_and_unwraps_in_the_terminal() {
    let fx = hangul_folder();
    let open = || vec![press(KeyCode::Down), press(KeyCode::Enter)];
    let mut once = open();
    once.push(press(KeyCode::Char('w')));
    let unwrapped = terminal_receipt(&fx, 160, 30, once);
    let keys = unwrapped.lines().nth(30).unwrap_or_default().to_string();
    assert!(keys.contains("2Wrap"), "{keys}\n{unwrapped}");
    let mut twice = open();
    twice.extend([press(KeyCode::Char('w')), press(KeyCode::Char('w'))]);
    let wrapped = terminal_receipt(&fx, 160, 30, twice);
    assert!(wrapped.contains("2Unwrap"), "{wrapped}");
    assert!(wrapped.contains("0001 한글 줄 text"), "{wrapped}");
}

/// With F8's list over the viewer, the viewer's own cells under the list and its shadow are
/// blank — no wide glyph is left half under its edge, as a terminal erases one — and once the
/// list closes, the next pass shows the rows there again.
#[test]
fn the_viewer_blanks_its_cells_under_f8s_list() {
    let fx = hangul_folder();
    let mut drv = Driver::new(1280.0, 480.0);
    let mut win = Shared(browser_on(&fx), None, None);
    drv.until(&mut win, 20, |w| w.ready());
    for key in [Key::ArrowDown, Key::Enter] {
        drv.key(&mut win, key, Modifiers::NONE);
        drv.until(&mut win, 20, |w| w.ready());
    }
    drv.idle(&mut win, 1);
    let viewer_of = |layers: &[std::sync::Arc<cells::Layer>]| {
        let hit = cells::find(layers, "Text Viewer");
        layers[hit.first().expect("the viewer").layer].buf.clone()
    };
    let before = viewer_of(&cells::layers(&drv.ctx));
    drv.key(&mut win, Key::F8, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 1);
    let layers = cells::layers(&drv.ctx);
    let of = |text: &str| cells::find(&layers, text).first().expect(text).layer;
    let (viewer, list) = (&layers[of("Text Viewer")], &layers[of("Encoding")]);
    assert_eq!(viewer.origin, list.origin, "one grid");
    let a = list.buf.area;
    let covered = |x: u16, y: u16| {
        a.contains(cells::Position::new(x, y))
            && matches!(list.buf[(x, y)].bg, ratatui_core::style::Color::Rgb(..))
    };
    let mut under = 0;
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            let symbol = viewer.buf[(x, y)].symbol();
            if covered(x, y) {
                under += 1;
                assert!(
                    symbol.trim().is_empty(),
                    "({x},{y}) {symbol:?} under the list"
                );
            } else if cells::width(symbol) == 2 {
                assert!(
                    !covered(x + 1, y),
                    "({x},{y}) {symbol:?} half under the list"
                );
            }
        }
    }
    assert!(
        under > 100,
        "the list stands over the viewer: {under} cells"
    );
    // Closed by Esc's press: the very next pass (its release's) shows the rows where it stood
    // — egui's top modal layer is still the list's then.
    let esc = |pressed| egui::Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    drv.frame(&mut win, vec![esc(true)], Modifiers::NONE);
    // (The list was shown in that pass: the next one begins with it as egui's top modal layer.)
    assert!(
        drv.ctx.memory(|m| m.top_modal_layer()).is_some(),
        "the case"
    );
    drv.frame(&mut win, vec![esc(false)], Modifiers::NONE);
    let layers = cells::layers(&drv.ctx);
    assert!(
        cells::find(&layers, "Encoding").is_empty(),
        "the list closed"
    );
    assert!(
        viewer_of(&layers) == before,
        "the viewer's cells as before F8"
    );
}
