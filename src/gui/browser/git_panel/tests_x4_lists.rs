//! X4-lists' tests (10 §17): the command log and the Backups list in cells, in the panel's body
//! (03 §3.7, §8.2), and the log's entry shown whole as an `ops::dialog` dialog.

use super::backups::{self, BackupsEnd, BackupsView};
use super::log_view::{self, LogView};
use super::tests::{fixture_view, Fx};
use super::tests_s3::{entry, records, repo};
use super::*;
use crate::git::actions::Action;
use crate::git::backup::{Manifest, Record};
use crate::git::model::{Backup, BackupKind, BackupRef, Oid};
use crate::git::runner::{Ending, LogEntry};
use crate::gui::cells::{Layer, Seen};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::PALETTE;
use crate::gui::Window;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Reading the panel's cells.

/// The panel's grid: the one its host laid over the window's central area.
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The panel's layer of cells: the one over all of its side (the host's side of the window's
/// grid, or all of the panel's grid).
fn panel(fx: &Fx) -> Arc<Layer> {
    let grid = grid_of(fx);
    let mine = |l: &Arc<Layer>| match fx.host.side {
        Some(side) => l.buf.area == side,
        None => l.buf.area == grid.all() && l.origin == grid.origin,
    };
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(mine)
        .expect("the panel's cells over its side")
}

/// The cell at column `x` of row `y` of the panel.
fn cell(fx: &Fx, x: u16, y: u16) -> Seen {
    let l = panel(fx);
    let a = l.buf.area;
    l.seen()[usize::from(y - a.y)][usize::from(x - a.x)].clone()
}

/// Row `y` of the panel from column `from` to `to` (exclusive), as text.
fn text(fx: &Fx, y: u16, from: u16, to: u16) -> String {
    (from..to).map(|x| cell(fx, x, y).symbol).collect()
}

/// Where `text` first stands on row `y` of the panel: its first cell's column.
fn at(fx: &Fx, y: u16, text: &str) -> Option<u16> {
    let l = panel(fx);
    let (s, cols) = cells::row_text(&l.buf, y);
    let start = s.find(text)?;
    Some(cols[s[..start].chars().count()])
}

/// The centre of cell (`x`, `y`) of the panel's grid, in points.
fn centre(fx: &Fx, x: u16, y: u16) -> Pos2 {
    grid_of(fx).rect(Cells::new(x, y, 1, 1)).center()
}

/// A press and a release of the primary button at `p`, as one pass's events.
fn click_events(p: Pos2) -> Vec<Event> {
    let button_at = |pressed| Event::PointerButton {
        pos: p,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(p), button_at(true), button_at(false)]
}

/// A key's press and release with `modifiers`, as one pass's events.
fn key_events(key: Key, modifiers: Modifiers) -> Vec<Event> {
    [true, false]
        .map(|pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        })
        .to_vec()
}

/// A wheel's turn of `dy` points over `p`, as one pass's events.
fn wheel_events(p: Pos2, dy: f32) -> Vec<Event> {
    vec![
        Event::PointerMoved(p),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, dy),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        },
    ]
}

/// The hand-off boxes a terminal would show inside the panel's grid: cells no layer covers.
fn boxes(fx: &Fx) -> Vec<Cells> {
    let g = grid_of(fx);
    let layers = cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows);
    (screen.hand_offs.into_iter())
        .filter(|r| r.y < g.rows)
        .collect()
}

/// Whether the dialog on top is drawn in cells: its modal's layer holds a layer of cells (a
/// dialog egui draws is a hand-off box in a terminal).
fn top_modal_in_cells(fx: &Fx) -> bool {
    let modal = fx.drv.ctx.memory(|m| m.top_modal_layer());
    let layers = cells::layers(&fx.drv.ctx);
    modal.is_some_and(|m| layers.iter().any(|l| l.layer == m))
}

/// Cmd/Ctrl, as the test kit sends it.
fn command() -> Modifiers {
    crate::gui::testkit::command(false)
}

// ---------------------------------------------------------------------------------------------
// Fixtures.

/// `n` log entries, newest first, a second apart; the second failed.
fn entries(n: usize) -> Vec<LogEntry> {
    (0..n)
        .map(|k| {
            let (ending, stderr) = match k {
                1 => (
                    Ending::Exit(128),
                    "fatal: 'origin' does not appear to be a git repository",
                ),
                _ => (Ending::Exit(0), ""),
            };
            let line = format!("git fetch --progress origin b{k}");
            entry(&line, ending, stderr, (n - k) as u64)
        })
        .collect()
}

/// The fixture's panel with the log of `log` open, drawn once in a `w`×`h` window.
fn log_fx(w: f32, h: f32, log: Vec<LogEntry>) -> Fx {
    let mut fx = Fx::new(w, h, fixture_view());
    fx.view_mut().open_log(log);
    fx.idle();
    fx
}

/// The fixture's panel with the Backups list of `records` open, drawn once (its first pass
/// painted) in a `w`×`h` window.
fn backups_fx(w: f32, h: f32, records: Vec<Record>) -> Fx {
    let mut fx = Fx::new(w, h, fixture_view());
    fx.view_mut().set_repo(repo());
    fx.view_mut().open_backups(records);
    fx.idle();
    fx
}

/// An object id of `c` repeated.
fn oid(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).unwrap()
}

/// A group `stamp` with `message`, holding `refs` (kind, name under the stamp, commit, subject).
fn group(stamp: &str, message: &str, refs: Vec<(BackupKind, &str, char, &str)>) -> Record {
    Record {
        group: Backup {
            stamp: stamp.into(),
            message: message.into(),
            refs: (refs.into_iter())
                .map(|(kind, name, at, subject)| BackupRef {
                    kind,
                    refname: format!("refs/sopen-backup/{stamp}/{name}"),
                    oid: oid(at),
                    subject: subject.into(),
                })
                .collect(),
        },
        manifest: Manifest::default(),
    }
}

/// `n` groups of one branch each, newest first.
fn many_groups(n: usize) -> Vec<Record> {
    (0..n)
        .map(|k| {
            let stamp = format!("20261123T10{:02}{:02}Z", k / 60, k % 60);
            let message = format!("sopen: delete branch b{k}");
            let name = format!("heads/b{k}");
            let head = BackupKind::Head(format!("b{k}"));
            group(&stamp, &message, vec![(head, &name, 'a', "first")])
        })
        .collect()
}

/// The panel's rows of a list: its header in the heads' row, its lines from the next.
const HEADER_Y: u16 = 1;
const FIRST_Y: u16 = 2;

/// The Backups list's foot row in a grid `rows` tall: the last of its cells, over the rule.
fn foot_y(rows: u16) -> u16 {
    rows - 4
}

/// The column of `[ <word> ]` on the Backups list's foot.
fn button_x(fx: &Fx, word: &str) -> u16 {
    let y = foot_y(grid_of(fx).rows);
    at(fx, y, &format!("[ {word} ]")).unwrap_or_else(|| panic!("no [ {word} ]"))
}

/// What the dialog the panel opened is for, if one is up.
fn dialog_action(fx: &Fx) -> Option<Action> {
    fx.view().dialog.as_ref().map(|d| d.draft().clone())
}

// ---------------------------------------------------------------------------------------------
// The log in cells (03 §3.7).

/// The log is cells in the panel's buffer: its header dim in the heads' row, an entry a row from
/// the next — newest first, the bar on the newest in the bar's colours, a failed entry in the
/// error colour — its scroll bar on the frame's right border beside the rows, and no hand-off box.
#[test]
fn the_log_is_cells_with_its_header_rows_bar_and_scroll_bar() {
    let log = entries(30);
    let fx = log_fx(1000.0, 300.0, log.clone());
    let g = grid_of(&fx);
    const { assert!(log_view::PAINTS_CELLS) };
    assert_eq!(boxes(&fx), Vec::<Cells>::new(), "no box where the log is");
    assert_eq!(at(&fx, HEADER_Y, log_view::HEADER), Some(1));
    assert_eq!(cell(&fx, 1, HEADER_Y).fg, Some(LOOK.dim));
    for (k, e) in log.iter().take(3).enumerate() {
        let y = FIRST_Y + k as u16;
        assert_eq!(at(&fx, y, &log_view::row_text(e)), Some(1), "entry {k}");
    }
    let bar = cell(&fx, 1, FIRST_Y);
    assert_eq!((bar.fg, bar.bg), (Some(LOOK.on_bar), Some(PALETTE.bar)));
    let failed = cell(&fx, 1, FIRST_Y + 1);
    assert_eq!(
        (failed.fg, failed.bg),
        (Some(LOOK.error), Some(PALETTE.back))
    );
    assert_eq!(cell(&fx, 1, FIRST_Y + 2).fg, Some(LOOK.text));
    // The scroll bar on the border, from the first row to the last over the rule.
    let border = g.cols - 1;
    assert_eq!(cell(&fx, border, FIRST_Y).symbol, "↑");
    assert_eq!(cell(&fx, border, g.rows - 4).symbol, "↓");
    assert_eq!(cell(&fx, border, HEADER_Y).symbol, "║", "the heads' row");
    assert_eq!(cell(&fx, 0, FIRST_Y).symbol, "║");
}

/// A row longer than the log is cut by cells with `…`, a Hangul glyph whole or not at all; an
/// empty log says so.
#[test]
fn a_long_entry_is_cut_by_cells_and_an_empty_log_says_so() {
    let line = format!("git commit -m {}", "한글".repeat(80));
    let fx = log_fx(1000.0, 300.0, vec![entry(&line, Ending::Exit(0), "", 0)]);
    let g = grid_of(&fx);
    let row = text(&fx, FIRST_Y, 1, g.cols - 1);
    assert!(row.trim_end().ends_with('…'), "{row:?}");
    assert!(cells::width(row.trim_end()) <= usize::from(g.cols - 2));
    let fx = log_fx(1000.0, 300.0, Vec::new());
    assert_eq!(at(&fx, FIRST_Y, "no command has run"), Some(1));
}

/// The wheel scrolls the log by cells (a row each 16 points) and the bar stays; a click puts the
/// bar on an entry, a second click on it shows it whole.
#[test]
fn the_logs_wheel_and_clicks_go_by_cells() {
    let log = entries(40);
    let mut fx = log_fx(1000.0, 300.0, log.clone());
    let over = centre(&fx, 20, FIRST_Y + 2);
    fx.drv
        .frame(&mut fx.host, wheel_events(over, -48.0), Modifiers::NONE);
    fx.idle();
    assert_eq!(
        at(&fx, FIRST_Y, &log_view::row_text(&log[3])),
        Some(1),
        "3 rows"
    );
    // A click on the row showing entry 5: the bar there; again: shown whole.
    let p = centre(&fx, 20, FIRST_Y + 2);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert_eq!(cell(&fx, 1, FIRST_Y + 2).bg, Some(PALETTE.bar));
    assert!(!fx.view().dialog_up());
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    let whole = fx.view().log.as_ref().and_then(LogView::whole).cloned();
    assert_eq!(whole.as_ref(), Some(&log[5]), "the clicked entry, whole");
}

/// Several keys in one pass: two Downs move the bar two entries and that pass's cells have it
/// there; Enter then Esc in one pass show the entry whole and close it again, the log still open.
#[test]
fn several_keys_in_one_pass_on_the_log() {
    let mut fx = log_fx(1000.0, 300.0, entries(10));
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        cell(&fx, 1, FIRST_Y + 2).bg,
        Some(PALETTE.bar),
        "in that pass"
    );
    assert_eq!(cell(&fx, 1, FIRST_Y).bg, Some(PALETTE.back));
    let mut events = key_events(Key::Enter, Modifiers::NONE);
    events.extend(key_events(Key::Escape, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(!fx.view().dialog_up(), "shown and closed in one pass");
    assert!(fx.view().log_newest().is_some(), "the log stays");
}

// ---------------------------------------------------------------------------------------------
// The entry shown whole (03 §3.7): an `ops::dialog` dialog.

/// Enter shows the entry whole as a dialog in cells on the panels' grid: each field a text after
/// its name, stderr a row a line (a tab to the next eighth cell), `[ OK ]` and the hint; the
/// panel blanks what it covers, which it says (`GitView::covered`). Esc closes it, and so does OK.
#[test]
fn an_entry_shown_whole_is_a_dialog_in_cells() {
    let mut e = entry(
        "git push --progress origin main",
        Ending::Exit(1),
        "error: failed to push some refs\n\thint: fetch first\n",
        0,
    );
    e.stdin = Some("message, 3 lines".into());
    let mut fx = log_fx(1000.0, 500.0, vec![e]);
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up());
    assert!(top_modal_in_cells(&fx), "cells, not a hand-off box");
    for shown in [
        "Command",
        "git push --progress origin main",
        "/srv/sopen",
        "message, 3 lines",
        "exit 1",
        "1.2 s",
        "0 bytes",
        "Stderr",
        "error: failed to push some refs",
        "hint: fetch first",
        "[ OK ]",
        "Enter or Esc closes",
    ] {
        assert!(fx.shows(shown), "{shown:?} in {:#?}", fx.texts());
    }
    let error = fx.drv.find_text("error: failed to push some refs").unwrap();
    let hint = fx.drv.find_text("hint: fetch first").unwrap();
    assert_eq!(hint.left() - error.left(), 8.0 * chrome::CELL.x, "the tab");
    assert_eq!(hint.top() - error.top(), chrome::CELL.y, "a row a line");
    // What it covers: blank in the panel's cells.
    let g = grid_of(&fx);
    let covered = fx.view().covered(&g);
    assert!(!covered.is_empty());
    for r in &covered {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                let s = cell(&fx, x, y);
                let blank = (s.symbol.as_str(), s.bg);
                assert_eq!(blank, (" ", Some(PALETTE.back)), "({x}, {y})");
            }
        }
    }
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up(), "Esc closed it");
    assert!(fx.view().covered(&g).is_empty());
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up());
    fx.drv.click_text_on_top(&mut fx.host, "[ OK ]");
    fx.idle();
    assert!(!fx.view().dialog_up(), "OK closed it");
    assert!(fx.view().log_newest().is_some(), "the log stays");
}

/// A long stderr scrolls in the dialog: at most half the grid's rows on screen (six at least),
/// the wheel over the box scrolling them.
#[test]
fn a_long_stderr_scrolls_in_the_dialog() {
    let stderr: String = (0..40).map(|k| format!("remote: line {k}\n")).collect();
    let log = vec![entry("git fetch", Ending::Exit(1), &stderr, 0)];
    let mut fx = log_fx(1000.0, 400.0, log);
    fx.press(Key::Enter);
    let g = grid_of(&fx);
    let most = usize::from(g.rows / 2).max(6);
    let shown = (0..40)
        .filter(|k| fx.shows(&format!("remote: line {k}")))
        .count();
    assert!(shown > 0 && shown <= most, "{shown} of {most}");
    assert!(fx.shows("remote: line 0") && !fx.shows("remote: line 39"));
    let over = fx.drv.find_text("remote: line 0").unwrap().center();
    fx.drv
        .frame(&mut fx.host, wheel_events(over, -48.0), Modifiers::NONE);
    fx.idle();
    assert!(!fx.shows("remote: line 0"), "3 rows on");
    assert!(fx.shows("remote: line 3"));
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up());
}

/// The entry's dialog is laid out once for its grid: passes that change nothing lay out none of
/// it again, however long its stderr (03 §3.7's last 64 KiB, here in one line); another grid lays
/// it out again.
#[test]
fn the_entry_is_laid_out_once_for_its_grid() {
    let line = "x".repeat(64 * 1024);
    let log = vec![entry("git fetch", Ending::Exit(1), &line, 0)];
    let mut fx = log_fx(1000.0, 500.0, log);
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up());
    let laid = || log_view::LAID.with(|c| c.get());
    let before = laid();
    fx.idle();
    fx.idle();
    assert_eq!(laid(), before, "no pass laid it out again");
    fx.drv.resize(900.0, 500.0);
    fx.idle();
    assert!(laid() > before, "laid out again on another grid");
}

/// Stderr's rows are cut as the kit's `wrap` breaks a text — at the last blank that fits, else
/// after the last whole glyph, a Hangul glyph never split — each row fitting its room, so the
/// kit keeps each whole. (In a room narrower than a glyph the two may differ: the kit keeps a
/// blank after a glyph alone on its row as a row of its own; the grid less eight cells is never
/// so narrow.)
#[test]
fn stderr_is_cut_as_the_kit_wraps() {
    let texts = [
        "remote: Enumerating objects: 5, done.",
        "error: failed to push some refs to 'https://example.com/a/very/long/path/repo.git'",
        &"x".repeat(200),
        "한글 오류가 아주 길게 이어지는 줄입니다 한글 오류가 아주 길게 이어지는 줄입니다",
        &"한".repeat(50),
        "a  b   c    d",
        "",
        "   ",
        // A run of blanks longer than the room after a break (review round 1, S1).
        "a          ",
        "abc            ",
        "a          b",
        "fatal: x                                                                              ",
    ];
    for text in texts {
        for room in [1, 2, 5, 9, 20, 76] {
            let rows = log_view::cut(text, room);
            let widest = text.chars().map(|c| cells::width(&c.to_string())).max();
            if room >= widest.unwrap_or(0) {
                let kit = crate::gui::browser::ops::dialog::wrap(text, room);
                assert_eq!(rows, kit, "{text:?} at {room}");
            }
            for row in &rows {
                let wide_alone = row.chars().count() == 1;
                assert!(cells::width(row) <= room || wide_alone, "{row:?} at {room}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The Backups list in cells (03 §8.2).

/// The Backups list is cells: its header dim in the heads' row; a group's stamp and message, its
/// refs two cells in — the kind dim, the short hash, the subject — each column at its cell; the
/// bar on the first line; `[ Restore ]  [ Show ]  [ Delete ]` at the foot in the dialog's colours,
/// two blank cells apart, then the keys' hint dim; the scroll bar on the border; no hand-off box.
#[test]
fn the_backups_list_is_cells_with_its_columns_bar_buttons_and_hint() {
    let fx = backups_fx(1000.0, 400.0, records());
    let g = grid_of(&fx);
    const { assert!(backups::PAINTS_CELLS) };
    assert_eq!(boxes(&fx), Vec::<Cells>::new(), "no box where the list is");
    let head = backups::HEADER.split(" ·").next().unwrap();
    assert_eq!(at(&fx, HEADER_Y, head), Some(1));
    assert_eq!(cell(&fx, 1, HEADER_Y).fg, Some(LOOK.dim));
    let y = FIRST_Y;
    assert_eq!(at(&fx, y, "2026-11-23 10:24:05 UTC"), Some(1));
    assert_eq!(at(&fx, y, "sopen: reset main to aaaaaaaa"), Some(1 + 28));
    let bar = cell(&fx, 1, y);
    assert_eq!((bar.fg, bar.bg), (Some(LOOK.on_bar), Some(PALETTE.bar)));
    assert_eq!(at(&fx, y + 1, "heads/main"), Some(1 + 2));
    assert_eq!(cell(&fx, 3, y + 1).fg, Some(LOOK.dim), "the kind dim");
    assert_eq!(at(&fx, y + 1, "aaaaaaaa"), Some(1 + 28));
    assert_eq!(cell(&fx, 29, y + 1).fg, Some(LOOK.text));
    assert_eq!(at(&fx, y + 1, "first"), Some(1 + 38));
    assert_eq!(at(&fx, y + 2, "manifest"), Some(1 + 2));
    assert_eq!(at(&fx, y + 3, "2026-11-22 08:00:00 UTC -2"), Some(1));
    assert_eq!(at(&fx, y + 3, "sopen: drop stash@{0}"), Some(1 + 28));
    assert_eq!(at(&fx, y + 4, "stash"), Some(1 + 2));
    assert_eq!(at(&fx, y + 4, "WIP on main: first"), Some(1 + 38));
    // The foot.
    let foot = foot_y(g.rows);
    let restore = button_x(&fx, "Restore");
    let show = button_x(&fx, "Show");
    let delete = button_x(&fx, "Delete");
    assert_eq!((restore, show, delete), (1, 1 + 11 + 2, 1 + 11 + 2 + 8 + 2));
    for x in [restore, show, delete] {
        let c = cell(&fx, x, foot);
        let dialog = (Some(PALETTE.dialog_text), Some(PALETTE.dialog_back));
        assert_eq!((c.fg, c.bg), dialog, "cell {x}");
    }
    let hint = at(&fx, foot, "Enter Show").expect("the hint");
    assert_eq!(hint, delete + 10 + 2);
    assert_eq!(cell(&fx, hint, foot).fg, Some(LOOK.dim));
    // The scroll bar on the border, beside the lines.
    let border = g.cols - 1;
    assert_eq!(cell(&fx, border, FIRST_Y).symbol, "↑");
    assert_eq!(cell(&fx, border, foot - 1).symbol, "↓");
    assert_eq!(cell(&fx, border, foot).symbol, "║", "the foot's row");
}

/// Hangul names and subjects are cut by cells, each column a blank before the next: a kind too
/// long for its column ends in `…` and never splits a glyph, and the hash after it stands at its
/// cell; an empty list says so.
#[test]
fn hangul_lines_are_cut_by_cells_at_their_columns() {
    let kind = "기능-한글-브랜치-이름이-아주-길다";
    let name = format!("heads/{kind}");
    let subject = "한글 제목 ".repeat(20);
    let message = format!("sopen: delete branch {kind}");
    let head = BackupKind::Head(kind.into());
    let rec = group(
        "20261128T010203Z",
        &message,
        vec![(head, &name, 'c', &subject)],
    );
    let fx = backups_fx(1000.0, 400.0, vec![rec]);
    let y = FIRST_Y + 1;
    let column = text(&fx, y, 1 + 2, 1 + 27);
    assert!(column.trim_end().ends_with('…'), "{column:?}");
    assert!(cells::width(column.trim_end()) <= 25, "{column:?}");
    assert_eq!(cell(&fx, 1 + 27, y).symbol, " ", "a blank before the hash");
    assert_eq!(at(&fx, y, "cccccccc"), Some(1 + 28));
    let g = grid_of(&fx);
    let shown = text(&fx, y, 1 + 38, g.cols - 1);
    assert!(shown.trim_end().ends_with('…'), "{shown:?}");
    let fx = backups_fx(1000.0, 400.0, Vec::new());
    assert_eq!(at(&fx, FIRST_Y, "no backups"), Some(1));
}

/// The wheel scrolls the lines by cells and the bar stays; a click puts the bar on a line, a
/// second click on it Shows it.
#[test]
fn the_lists_wheel_and_clicks_go_by_cells() {
    let mut fx = backups_fx(1000.0, 300.0, many_groups(30));
    let over = centre(&fx, 20, FIRST_Y + 2);
    fx.drv
        .frame(&mut fx.host, wheel_events(over, -48.0), Modifiers::NONE);
    fx.idle();
    // Three rows on: group 1's line (each group a line and its ref's).
    assert_eq!(at(&fx, FIRST_Y + 1, "sopen: delete branch b2"), Some(29));
    let y = (FIRST_Y..foot_y(grid_of(&fx).rows))
        .find(|&y| at(&fx, y, "sopen: delete branch b3").is_some())
        .unwrap();
    let p = centre(&fx, 5, y);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert_eq!(cell(&fx, 1, y).bg, Some(PALETTE.bar));
    assert!(fx.take().is_empty());
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    let refs = many_groups(30)[3].group.refs.clone();
    assert_eq!(fx.take(), [GitAction::ShowBackup(refs)]);
}

/// A click on a button asks for its action; one on the blank between two buttons, beside them or
/// on the hint presses nothing (egui gives a click near a widget to it when nothing is under the
/// pointer).
#[test]
fn a_click_between_the_buttons_presses_nothing() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    let foot = foot_y(grid_of(&fx).rows);
    let show = button_x(&fx, "Show");
    let delete = button_x(&fx, "Delete");
    for x in [
        show - 2,
        show - 1,
        delete - 2,
        delete - 1,
        delete + 10,
        delete + 11,
        delete + 20,
    ] {
        let p = centre(&fx, x, foot);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        assert!(fx.take().is_empty(), "cell {x}");
        assert!(dialog_action(&fx).is_none(), "cell {x}");
        assert!(fx.view().backups().is_some(), "cell {x}");
    }
    // Just above a button, on a blank row of the list, and just below it, on the rule: 2 points
    // off its cells.
    let at = centre(&fx, delete + 3, foot);
    for dy in [-10.0, 10.0] {
        let p = at + Vec2::new(0.0, dy);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        assert!(fx.take().is_empty(), "{dy} points off");
        assert!(dialog_action(&fx).is_none(), "{dy} points off");
    }
    let p = centre(&fx, show + 3, foot);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    let refs = vec![records()[0].group.refs[0].clone()];
    assert_eq!(fx.take(), [GitAction::ShowBackup(refs)]);
}

/// Two clicks on a button in one pass ask for its action once (egui reports one click a pass).
#[test]
fn two_clicks_on_a_button_in_one_pass_ask_once() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    let p = centre(&fx, button_x(&fx, "Restore") + 2, foot_y(grid_of(&fx).rows));
    let mut events = click_events(p);
    events.extend(click_events(p));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    let restore = GitAction::Restore {
        stamp: "20261123T102405Z".into(),
        only: None,
    };
    assert_eq!(fx.take(), [restore]);
}

// ---------------------------------------------------------------------------------------------
// What runs is what was shown (the X4 brief's lessons).

/// A re-read that lands in the pass of an F8 — here the bar's group deleted, the bar on the next
/// group — deletes nothing: the owner saw the other group under the bar. The next F8 deletes the
/// group the list now shows under it.
#[test]
fn a_reread_and_f8_in_one_pass_delete_nothing() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    fx.view_mut().open_backups(records()[1..].to_vec());
    let f8 = key_events(Key::F8, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, f8, Modifiers::NONE);
    fx.idle();
    assert_eq!(dialog_action(&fx), None, "nothing under the bar was seen");
    fx.press(Key::F8);
    let stamp = "20261122T080000Z-2".to_string();
    assert_eq!(dialog_action(&fx), Some(Action::DeleteBackup { stamp }));
}

/// A delete that lands in the pass of a click on `[ Delete ]` deletes nothing either; the next
/// click deletes the group now under the bar.
#[test]
fn a_delete_landing_and_a_click_on_delete_in_one_pass_delete_nothing() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    let p = centre(&fx, button_x(&fx, "Delete") + 2, foot_y(grid_of(&fx).rows));
    fx.view_mut().open_backups(records()[1..].to_vec());
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert_eq!(dialog_action(&fx), None);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    let stamp = "20261122T080000Z-2".to_string();
    assert_eq!(dialog_action(&fx), Some(Action::DeleteBackup { stamp }));
}

/// A re-read that changes nothing keeps the press: F8 in its pass deletes the group shown.
#[test]
fn an_identical_reread_keeps_the_press() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    fx.view_mut().open_backups(records());
    let f8 = key_events(Key::F8, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, f8, Modifiers::NONE);
    fx.idle();
    let stamp = "20261123T102405Z".to_string();
    assert_eq!(dialog_action(&fx), Some(Action::DeleteBackup { stamp }));
}

/// ↓ then Cmd/Ctrl+Z in one pass restores nothing — the bar moved to a line the owner had not
/// seen under it — and that pass shows the bar where ↓ put it; the next Cmd/Ctrl+Z restores that
/// ref. Two ↓ in one pass move the bar two lines.
#[test]
fn a_key_after_a_move_in_one_pass_acts_on_nothing() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::Z, command()));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        cell(&fx, 1, FIRST_Y + 1).bg,
        Some(PALETTE.bar),
        "the bar moved"
    );
    fx.idle();
    assert!(fx.take().is_empty(), "nothing restored");
    let asked = fx.key(Key::Z, command());
    let only = Some(records()[0].group.refs[0].refname.clone());
    let stamp = "20261123T102405Z".to_string();
    assert_eq!(asked, [GitAction::Restore { stamp, only }]);
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        cell(&fx, 1, FIRST_Y + 3).bg,
        Some(PALETTE.bar),
        "two lines on"
    );
}

/// A move then a key in one pass acts where the line the bar lands on asks for what the painted
/// line asked for: ↓ from a group's line onto its ref, then F8, deletes that group (a ref's Delete
/// is its group's); ↓ from a group's last ref onto the next group, then F8, deletes nothing.
#[test]
fn a_key_after_a_move_acts_where_the_line_asks_the_same() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::F8, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events.clone(), Modifiers::NONE);
    fx.idle();
    let stamp = "20261123T102405Z".to_string();
    assert_eq!(dialog_action(&fx), Some(Action::DeleteBackup { stamp }));
    let mut fx = backups_fx(1000.0, 400.0, records());
    fx.press(Key::ArrowDown);
    fx.press(Key::ArrowDown);
    let manifest = cell(&fx, 1, FIRST_Y + 2);
    assert_eq!(manifest.bg, Some(PALETTE.bar), "the manifest");
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        dialog_action(&fx),
        None,
        "the next group was not under the bar"
    );
    assert_eq!(cell(&fx, 1, FIRST_Y + 3).bg, Some(PALETTE.bar));
}

/// The list's first pass painted nothing the owner saw: an F8 in the pass it opens in deletes
/// nothing, the next one does.
#[test]
fn the_lists_first_pass_takes_no_action() {
    let mut fx = Fx::new(1000.0, 400.0, fixture_view());
    fx.view_mut().set_repo(repo());
    fx.view_mut().open_backups(records());
    let f8 = key_events(Key::F8, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, f8, Modifiers::NONE);
    fx.idle();
    assert_eq!(dialog_action(&fx), None);
    fx.press(Key::F8);
    assert!(dialog_action(&fx).is_some());
}

/// F-git-covered: the Backups list under a dialog of its own panel — a result over it, its box
/// clear of the bar's line — does not have the keyboard, so a pass it stood under the result in
/// is not what the screen showed of it: Esc closes the result in a pass the terminal does not
/// print, then F8 in the next asks nothing; the next F8, the list printed with the keyboard,
/// asks to delete the group under the bar.
#[test]
fn f_git_covered_the_list_under_its_panels_result_takes_no_key() {
    let mut fx = backups_fx(1000.0, 400.0, records());
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    fx.idle();
    fx.idle();
    let grid = grid_of(&fx);
    let bar_line = Cells::new(0, FIRST_Y, grid.cols, 1);
    let covered = fx.view().covered(&grid);
    assert!(!covered.is_empty(), "the result is up");
    assert!(
        covered.iter().all(|c| !c.intersects(bar_line)),
        "the bar's line in sight: {covered:?}"
    );
    let esc = key_events(Key::Escape, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, esc, Modifiers::NONE);
    cells::set_presented(&fx.drv.ctx, false);
    assert!(!fx.view().dialog_up(), "Esc closed the result");
    let f8 = key_events(Key::F8, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, f8, Modifiers::NONE);
    cells::set_presented(&fx.drv.ctx, true);
    fx.idle();
    assert_eq!(dialog_action(&fx), None, "shown covered: nothing asked");
    fx.press(Key::F8);
    let stamp = "20261123T102405Z".to_string();
    assert_eq!(dialog_action(&fx), Some(Action::DeleteBackup { stamp }));
}

/// F-git-covered review 1: the log's entry shown whole under a result of its panel's (a run's
/// failure the integration opened over it) reads no key: Esc closes the result, and the entry
/// stays; the next Esc closes the entry.
#[test]
fn f_git_covered_the_logs_entry_under_a_result_reads_no_key() {
    let e = entry("git fetch origin", Ending::Exit(1), "fatal: hung up\n", 0);
    let mut fx = log_fx(1000.0, 500.0, vec![e]);
    fx.press(Key::Enter);
    assert!(
        fx.view().log.as_ref().is_some_and(|l| l.whole().is_some()),
        "the entry whole"
    );
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    fx.idle();
    fx.idle();
    let esc = key_events(Key::Escape, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, esc, Modifiers::NONE);
    fx.idle();
    assert!(fx.view().result.is_none(), "the Esc closed the result");
    assert!(
        fx.view().log.as_ref().is_some_and(|l| l.whole().is_some()),
        "and left the entry under it"
    );
    fx.press(Key::Escape);
    assert!(
        fx.view().log.as_ref().is_some_and(|l| l.whole().is_none()),
        "the next Esc closes the entry"
    );
}

/// The bar scrolled off the lines by the wheel is under no line the owner sees: Enter Shows
/// nothing; a ↓ brings it back into view and the next Enter Shows its line.
#[test]
fn the_bar_scrolled_off_takes_no_action() {
    let mut fx = backups_fx(1000.0, 300.0, many_groups(30));
    let over = centre(&fx, 20, FIRST_Y + 2);
    fx.drv
        .frame(&mut fx.host, wheel_events(over, -160.0), Modifiers::NONE);
    fx.idle();
    assert!(at(&fx, FIRST_Y, "sopen: delete branch b0").is_none());
    assert!(fx.press(Key::Enter).is_empty(), "nothing shown");
    assert!(fx.view().backups().is_some());
    fx.press(Key::ArrowDown);
    let refs = many_groups(30)[0].group.refs.clone();
    assert_eq!(fx.press(Key::Enter), [GitAction::ShowBackup(refs)]);
}

/// The header, the keys' hint and each column of a line are written over blanks in their own
/// colours, so the blanks after their last glyph are of their run (`cells::runs`): an
/// ambiguous-wide terminal lays the lists' own `·` in two cells by taking a blank that ends its
/// run's word (`term::lay_run`, 10 §10 as built), and a run whose last cell is a glyph has none —
/// it would be cut with `~` there.
#[test]
fn the_header_hint_and_columns_end_in_blanks_of_their_own_colours() {
    let dim = (Some(LOOK.dim), Some(PALETTE.back));
    let pen = |fx: &Fx, x: u16, y: u16| {
        let c = cell(fx, x, y);
        assert_eq!(c.symbol, " ", "({x}, {y}) a blank");
        (c.fg, c.bg)
    };
    let fx = log_fx(1000.0, 300.0, entries(3));
    let end = 1 + cells::width(log_view::HEADER) as u16;
    assert_eq!(pen(&fx, end, HEADER_Y), dim, "the log's header");
    assert_eq!(pen(&fx, end + 1, HEADER_Y), dim);
    let fx = backups_fx(1000.0, 400.0, records());
    let end = 1 + cells::width(backups::HEADER) as u16;
    assert_eq!(pen(&fx, end, HEADER_Y), dim, "the list's header");
    let foot = foot_y(grid_of(&fx).rows);
    let hint = at(&fx, foot, "Enter Show").unwrap();
    let text: String = (hint..grid_of(&fx).cols - 1)
        .map(|x| cell(&fx, x, foot).symbol)
        .collect();
    let end = hint + cells::width(text.trim_end()) as u16;
    assert_eq!(pen(&fx, end, foot), dim, "the hint");
    // A ref's kind, dim, ends in dim blanks too.
    let kind_end = 1 + 2 + cells::width("heads/main") as u16;
    assert_eq!(pen(&fx, kind_end, FIRST_Y + 1), dim, "the kind");
}

// ---------------------------------------------------------------------------------------------
// Pointer-only controls and ids per panel (the X4 brief's lessons).

/// The Backups list drawn alone, its keys read by no one: Tab and Space reach egui as they would
/// in a pass whose keys nothing takes.
struct Bare {
    view: BackupsView,
    ends: Vec<BackupsEnd>,
}

impl Window for Bare {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let mut buf = Buffer::empty(grid.all());
                let view = &mut self.view;
                self.ends.extend(backups::draw(
                    ui,
                    &grid,
                    grid.all(),
                    &mut buf,
                    view,
                    true,
                    &[],
                ));
                let painter = ui.painter_at(grid.rect(grid.all()));
                cells::paint(&painter, grid.origin, &buf, None);
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The lines and buttons sense clicks alone: Tab moves egui's keyboard to none of them, so Tab
/// then Space — each in its own pass, or both in one — never presses `[ Restore ]`, `[ Show ]`
/// or `[ Delete ]`.
#[test]
fn tab_then_space_presses_no_button() {
    let mut drv = crate::gui::testkit::Driver::new(1000.0, 400.0);
    let mut bare = Bare {
        view: BackupsView::new(records()),
        ends: Vec::new(),
    };
    drv.idle(&mut bare, 2);
    for _ in 0..8 {
        drv.key(&mut bare, Key::Tab, Modifiers::NONE);
        drv.key(&mut bare, Key::Space, Modifiers::NONE);
        let mut events = key_events(Key::Tab, Modifiers::NONE);
        events.extend(key_events(Key::Space, Modifiers::NONE));
        drv.frame(&mut bare, events, Modifiers::NONE);
        drv.idle(&mut bare, 1);
    }
    assert_eq!(bare.ends, Vec::<BackupsEnd>::new());
}

/// Two repositories' panels side by side in one `ui`, each with its Backups list open.
struct Two {
    views: [GitView; 2],
    actions: [Vec<GitAction>; 2],
}

impl Window for Two {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let sides = cells::halves(grid.all());
                for (i, side) in sides.into_iter().enumerate() {
                    let asked = draw(ui, &grid, side, &mut self.views[i], i == 0, &[]);
                    self.actions[i].extend(asked);
                }
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Each panel's buttons are its own: a click on the left list's `[ Show ]` asks the left panel
/// alone, one on the right list's the right one alone (one id for both would leave egui one rect
/// for both).
#[test]
fn two_panels_side_by_side_each_take_their_own_clicks() {
    let view = |records: Vec<Record>| {
        let mut v = fixture_view();
        v.set_repo(repo());
        v.open_backups(records);
        v
    };
    let mut drv = crate::gui::testkit::Driver::new(1600.0, 400.0);
    let mut two = Two {
        views: [view(records()), view(many_groups(3))],
        actions: [Vec::new(), Vec::new()],
    };
    drv.idle(&mut two, 2);
    let window = egui::Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 400.0));
    let grid = Grid::over(window);
    let sides = cells::halves(grid.all());
    let foot = foot_y(grid.rows);
    // `[ Show ]` is the second button: 11 cells and two blanks in, inside each frame.
    for (i, side) in sides.into_iter().enumerate() {
        let p = grid
            .rect(Cells::new(side.x + 1 + 13 + 2, foot, 1, 1))
            .center();
        drv.click(&mut two, p);
        drv.idle(&mut two, 1);
        let wanted = match i {
            0 => records()[0].group.refs[..1].to_vec(),
            _ => many_groups(3)[0].group.refs.clone(),
        };
        assert_eq!(two.actions[i], [GitAction::ShowBackup(wanted)], "panel {i}");
        assert!(two.actions[1 - i].is_empty(), "panel {i}: not the other's");
        two.actions = [Vec::new(), Vec::new()];
    }
    // The logs' rows too: a double click on a row of one panel's log shows its entry whole there
    // alone.
    let log = entries(5);
    for (i, side) in sides.into_iter().enumerate() {
        for v in &mut two.views {
            v.open_log(log.clone());
        }
        drv.idle(&mut two, 2);
        let p = grid
            .rect(Cells::new(side.x + 5, FIRST_Y + 3, 1, 1))
            .center();
        drv.click(&mut two, p);
        drv.idle(&mut two, 1);
        drv.click(&mut two, p);
        drv.idle(&mut two, 1);
        let whole = |v: &GitView| v.log.as_ref().and_then(LogView::whole).cloned();
        assert_eq!(whole(&two.views[i]).as_ref(), Some(&log[3]), "panel {i}");
        assert_eq!(whole(&two.views[1 - i]), None, "panel {i}: not the other's");
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// `f`'s count on the counter `key` of `log_view` (its test-only work counters).
fn counted(key: &'static std::thread::LocalKey<std::cell::Cell<usize>>, f: impl FnOnce()) -> usize {
    key.with(|c| c.set(0));
    f();
    key.with(|c| c.get())
}

/// A command line of `bytes` bytes, as the files backup's batches of paths make them.
fn long_line(bytes: usize) -> String {
    let mut line = String::from("git add --");
    let mut k = 0;
    while line.len() < bytes {
        line.push_str(&format!(" dir/file-{k:05}.txt"));
        k += 1;
    }
    line.truncate(bytes);
    line
}

/// M2: the entry's fields reach the kit — which wraps them by `dialog::wrap`, its work growing
/// with the square of a text — cut to the cells the box can show: a 64 KiB command line hands it
/// a few screens of text, not 64 KiB.
#[test]
fn a_64_kib_command_line_reaches_the_kit_cut_to_the_box() {
    let line = long_line(64 * 1024);
    let mut fx = log_fx(1000.0, 500.0, vec![entry(&line, Ending::Exit(0), "", 0)]);
    let handed = counted(&log_view::HANDED, || {
        fx.press(Key::Enter);
    });
    assert!(fx.view().dialog_up());
    let g = grid_of(&fx);
    let width = crate::gui::browser::ops::dialog::inside_width(g.cols, 88, 0);
    let screens = (usize::from(g.rows) + 2) * (width + 1);
    assert!(
        handed <= 2 * screens + 1024,
        "{handed} bytes handed to the kit"
    );
    let shown = fx
        .drv
        .text_starting_with("git add -- dir/file-00000.txt dir/file-00001.txt");
    assert!(shown.is_some(), "its first row");
}

/// M2: what the box shows of a value cut to its cells is what it shows of the whole value — the
/// same layout, row for row — on grids short and tall, for a long Latin, Hangul or blank-run
/// command line, and one that fits.
#[test]
fn a_value_cut_to_the_box_lays_out_as_the_whole_value() {
    // Longer than the cut on the short grids, and the first than the 125×60 grid's too (their
    // whole layouts are slow: the quadratic wrap).
    let lines = [
        long_line(6 * 1024),
        "커밋 메시지가 아주 긴 한글 경로/".repeat(100),
        format!("git add -- a{}b", " ".repeat(2000)),
        "git fetch --progress origin".to_string(),
    ];
    for line in lines {
        let e = entry(&line, Ending::Exit(0), "fatal: x", 0);
        // The narrow ones lay each value under its name (`dialog::named_room`).
        for (cols, rows) in [(125, 18), (125, 60), (60, 12), (20, 7), (20, 40), (21, 24)] {
            let grid = Grid {
                origin: Pos2::ZERO,
                cols,
                rows,
            };
            let cut = log_view::entry_layout(grid, &e, 0);
            let whole = log_view::entry_layout_in(grid, &e, 0, usize::MAX);
            let at = format!("{} bytes at {cols}x{rows}", line.len());
            assert_eq!(format!("{cut:?}"), format!("{whole:?}"), "{at}");
        }
    }
}

// S1: a run of blanks longer than the room after a break makes no rows of blanks — see
// `stderr_is_cut_as_the_kit_wraps`, whose inputs hold such runs.

/// S3: a pass walks an entry's row up to its room and a cell more, however long its command line
/// (64 KiB); the row it draws is the one `cells::fit_start` cuts.
#[test]
fn a_rows_work_is_its_room() {
    let log: Vec<LogEntry> = (0..30)
        .map(|k| entry(&long_line(64 * 1024 + k), Ending::Exit(0), "", k as u64))
        .collect();
    let mut fx = log_fx(1000.0, 300.0, log.clone());
    let walked = counted(&log_view::ROWS_WALKED, || fx.idle());
    let g = grid_of(&fx);
    let rows = usize::from(g.rows - 5);
    let width = usize::from(g.cols - 2);
    // Each row walked to its room and a cell past it — not nothing, nor the whole line.
    assert!(
        rows * width <= walked && walked <= rows * (width + 2),
        "{walked} for {rows} rows of {width}"
    );
    let row = text(&fx, FIRST_Y, 1, g.cols - 1);
    let want = cells::fit_start(&graph_view::printable(&log_view::row_text(&log[0])), width);
    assert_eq!(row.trim_end(), want.trim_end());
}

/// S4: a double click is on the entry itself, never on its place: a click, an entry logged that
/// moves the rows down one, a second click on the same cells — the other entry there now — shows
/// nothing whole and puts the bar there. The control: the same entries again, and the second click
/// shows the clicked entry whole.
#[test]
fn a_double_click_across_a_reread_of_the_log_is_its_entrys_own() {
    let log = entries(10);
    for moved in [true, false] {
        let mut fx = log_fx(1000.0, 300.0, log.clone());
        let p = centre(&fx, 20, FIRST_Y + 3);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        let mut again = log.clone();
        if moved {
            let new = entry("git fetch --progress origin new", Ending::Exit(0), "", 99);
            again.insert(0, new);
        }
        fx.view_mut().set_log(again);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        let whole = fx.view().log.as_ref().and_then(LogView::whole).cloned();
        match moved {
            true => {
                assert_eq!(whole, None, "another entry under the second click");
                assert_eq!(cell(&fx, 1, FIRST_Y + 3).bg, Some(PALETTE.bar));
            }
            false => assert_eq!(whole.as_ref(), Some(&log[3]), "the same entry"),
        }
    }
}

/// S4: the same for the Backups list's lines: a click on a group's line, a re-read that moves the
/// lines up one — a group above gone — and a second click on the same cells, now the group's own
/// ref (which would Show the same ref), Shows nothing. The control: the same records again, and
/// the second click Shows the group.
#[test]
fn a_double_click_across_a_reread_of_the_list_is_its_lines_own() {
    let gone = group("20261124T000000Z", "sopen: nothing left", vec![]);
    for moved in [true, false] {
        let mut before = vec![gone.clone()];
        before.extend(records());
        let mut fx = backups_fx(1000.0, 400.0, before.clone());
        let p = centre(&fx, 5, FIRST_Y + 1);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        assert!(fx.take().is_empty());
        let again = if moved { records() } else { before };
        fx.view_mut().open_backups(again);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        let asked = fx.take();
        match moved {
            true => assert!(asked.is_empty(), "{asked:?}"),
            false => {
                let refs = vec![records()[0].group.refs[0].clone()];
                assert_eq!(asked, [GitAction::ShowBackup(refs)]);
            }
        }
    }
}

/// S5: stderr is cut in one walk of its graphemes — `log_view::cut`, never `dialog::wrap`, which
/// measures the rest of a line again for each row.
#[test]
fn stderr_is_walked_once() {
    let stderr = "y".repeat(64 * 1024);
    let mut fx = log_fx(
        1000.0,
        500.0,
        vec![entry("git fetch", Ending::Exit(1), &stderr, 0)],
    );
    let walked = counted(&log_view::CUT, || {
        fx.press(Key::Enter);
    });
    assert!(fx.view().dialog_up());
    assert_eq!(walked, stderr.len(), "each grapheme once");
}

/// S7: the entry's field names and `Stderr` in the dialog's weak colour, as egui had them, the
/// values in its text colour.
#[test]
fn the_entrys_names_are_weak_and_its_values_black() {
    let e = entry("git fetch", Ending::Exit(1), "fatal: no", 0);
    let mut fx = log_fx(1000.0, 500.0, vec![e]);
    fx.press(Key::Enter);
    let weak = Some(vec![PALETTE.dialog_weak]);
    let text = Some(vec![PALETTE.dialog_text]);
    // `Command` is the title's word too: the names but that one.
    for name in [
        "Time",
        "Host",
        "Repository",
        "Stdin",
        "Ending",
        "Duration",
        "Stdout",
        "Stderr",
    ] {
        assert_eq!(fx.drv.colors_of(name), weak, "{name}");
    }
    for value in ["/srv/sopen", "exit 1", "fatal: no"] {
        assert_eq!(fx.drv.colors_of(value), text, "{value}");
    }
    // A value too long for its row goes on under its first row's start (`Part::Named`).
    let mut e = entry("git fetch", Ending::Exit(0), "", 0);
    e.repo = format!("/srv/{}/end", "a".repeat(120));
    let mut fx = log_fx(1000.0, 500.0, vec![e]);
    fx.press(Key::Enter);
    let row = |prefix: &str| {
        let text = fx.drv.text_starting_with(prefix).expect(prefix);
        fx.drv.find_text(&text).expect(prefix)
    };
    let (start, rest) = (row("/srv/aaaa"), row("aaaa"));
    assert!(
        fx.shows(&format!("{}/end", "a".repeat(120 - 71))),
        "the second row"
    );
    assert_eq!(rest.left(), start.left(), "under the first");
    assert_eq!(rest.top() - start.top(), chrome::CELL.y);
}

/// S7: the keys' hint stands as many of its keys whole as fit after the buttons — on an 80-column
/// terminal's side none, never `En…`; on a 1000-point window's side `Enter Show`.
#[test]
fn the_hint_shows_its_keys_whole_or_not_at_all() {
    for (w, want) in [(640.0, ""), (1000.0, "Enter Show")] {
        let mut fx = Fx::new(w, 400.0, fixture_view());
        let g = Grid::over(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(w, 400.0 - 16.0),
        ));
        let [_, side] = cells::halves(g.all());
        fx.host.side = Some(side);
        fx.view_mut().set_repo(repo());
        fx.view_mut().open_backups(records());
        fx.idle();
        fx.idle();
        let foot = foot_y(g.rows);
        let delete = at(&fx, foot, "[ Delete ]").expect("the buttons");
        let after = text(&fx, foot, delete + 10, side.right() - 1);
        assert_eq!(after.trim(), want, "{w}");
    }
}

/// GPT: in the panel at the right side, a click a cell or two left of the buttons — the frame's
/// border, the other side — or right of them, on the frame's border at the foot, or on the rule
/// under them presses nothing, and the list stays.
#[test]
fn a_click_beside_the_buttons_in_the_panel_presses_nothing() {
    let mut fx = Fx::new(1000.0, 400.0, fixture_view());
    let g = Grid::over(egui::Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(1000.0, 384.0),
    ));
    let [_, side] = cells::halves(g.all());
    fx.host.side = Some(side);
    fx.view_mut().set_repo(repo());
    fx.view_mut().open_backups(records());
    fx.idle();
    fx.idle();
    let foot = foot_y(g.rows);
    let restore = at(&fx, foot, "[ Restore ]").expect("the buttons");
    let delete = at(&fx, foot, "[ Delete ]").expect("the buttons");
    let cells_at = [
        (restore - 1, foot),
        (restore - 2, foot),
        (delete + 10, foot),
        (delete + 11, foot),
        (side.right() - 1, foot),
        (restore + 3, foot + 1),
        (restore - 1, foot + 1),
    ];
    for (x, y) in cells_at {
        let p = g.rect(Cells::new(x, y, 1, 1)).center();
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        assert!(fx.take().is_empty(), "({x}, {y})");
        assert!(dialog_action(&fx).is_none(), "({x}, {y})");
        assert!(fx.view().backups().is_some(), "({x}, {y})");
    }
}

/// GPT: ten thousand entries draw a screen of rows, a pass walking no more than their room.
#[test]
fn ten_thousand_entries_draw_a_screen_of_rows() {
    let mut fx = log_fx(1000.0, 300.0, entries(10_000));
    let walked = counted(&log_view::ROWS_WALKED, || fx.idle());
    let g = grid_of(&fx);
    let rows = usize::from(g.rows - 5);
    assert!(walked <= rows * usize::from(g.cols), "{walked}");
    fx.press(Key::End);
    assert_eq!(
        at(&fx, g.rows - 4, "git fetch --progress origin b9999"),
        Some(16)
    );
}

/// GPT: a MiB of stderr is cut once and laid out once; the passes after it do neither again.
#[test]
fn a_mebibyte_of_stderr_is_cut_and_laid_out_once() {
    let stderr: String = (0..13_274).map(|k| format!("remote: {k:070}\n")).collect();
    let mut fx = log_fx(
        1000.0,
        500.0,
        vec![entry("git fetch", Ending::Exit(1), &stderr, 0)],
    );
    let walked = counted(&log_view::CUT, || {
        fx.press(Key::Enter);
    });
    assert!(stderr.len() >= 1024 * 1024);
    assert_eq!(
        walked,
        stderr.len() - 13_274,
        "each grapheme once, the newlines apart"
    );
    assert!(fx.shows(&format!("remote: {:070}", 0)));
    let laid = log_view::LAID.with(|c| c.get());
    let again = counted(&log_view::CUT, || {
        fx.idle();
        fx.idle();
    });
    assert_eq!((again, log_view::LAID.with(|c| c.get())), (0, laid));
}

/// The log and the Backups list drawn alone at `area` of the window's grid.
struct Alone {
    log: LogView,
    list: BackupsView,
    area: Cells,
}

impl Window for Alone {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let mut buf = Buffer::empty(grid.all());
                let area = self.area.intersection(grid.all());
                log_view::draw(ui, &grid, area, &mut buf, &mut self.log, true);
                let _ = backups::draw(ui, &grid, area, &mut buf, &mut self.list, true, &[]);
                let painter = ui.painter_at(grid.rect(grid.all()));
                cells::paint(&painter, grid.origin, &buf, None);
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// GPT: no cells to draw in — no width, no height, one, two or three rows — draws what fits and
/// never panics; nor does the entry shown whole on a grid of one row.
#[test]
fn no_width_and_a_row_of_grid_never_panic() {
    for (w, h) in [
        (0, 10),
        (10, 0),
        (0, 0),
        (1, 1),
        (40, 1),
        (40, 2),
        (40, 3),
        (2, 5),
    ] {
        let mut drv = crate::gui::testkit::Driver::new(800.0, 400.0);
        let mut alone = Alone {
            log: LogView::new(entries(5)),
            list: BackupsView::new(records()),
            area: Cells::new(3, 2, w, h),
        };
        drv.idle(&mut alone, 2);
        drv.key(&mut alone, Key::ArrowDown, Modifiers::NONE);
    }
    let mut fx = log_fx(1000.0, 32.0, entries(3));
    assert_eq!(grid_of(&fx).rows, 1);
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up(), "shown whole on one row");
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up());
}

// ---------------------------------------------------------------------------------------------
// Review round 1, M1: what the list acts on is what reached the screen.

/// A group of two branches and its manifest: its Show is both refs, each ref's line its own.
fn two_branches() -> Vec<Record> {
    vec![group(
        "20261128T010203Z",
        "sopen: rebase main",
        vec![
            (BackupKind::Head("main".into()), "heads/main", 'a', "first"),
            (BackupKind::Head("b".into()), "heads/b", 'b', "second"),
            (BackupKind::Manifest, "manifest", 'e', "sopen: rebase main"),
        ],
    )]
}

/// A terminal that sends its bursts of events back to back — each waiting while the next is
/// unread, so the backend prints only the last pass of a burst — one burst once the backend has
/// asked three times since the last (its passes done and printed), then ends the run (X4-dialogs'
/// `Bursts`, without its counts).
struct Bursts {
    bursts: std::collections::VecDeque<Vec<crossterm::event::Event>>,
    now: std::collections::VecDeque<crossterm::event::Event>,
    asked: usize,
}

impl crate::gui::term::Tty for Bursts {
    fn raw(&mut self, _on: bool) -> std::io::Result<()> {
        Ok(())
    }
    fn size(&mut self) -> std::io::Result<(u16, u16)> {
        Ok((100, 30))
    }
    fn probe(&mut self) -> crate::gui::term::Probed {
        crate::gui::term::Probed {
            kitty: false,
            ambiguous: false,
        }
    }
    fn drop_input(&mut self) {}
    fn next(
        &mut self,
        _wait: std::time::Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        if let Some(ev) = self.now.pop_front() {
            return Ok(Some(ev));
        }
        self.asked += 1;
        assert!(self.asked < 20_000, "the script ran out without an end");
        if self.asked >= 3 {
            if let Some(burst) = self.bursts.pop_front() {
                self.asked = 0;
                self.now.extend(burst);
                return Ok(self.now.pop_front());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        Ok(None)
    }
    fn pending(&mut self) -> std::io::Result<bool> {
        Ok(!self.now.is_empty())
    }
    fn stopped(&self) -> Option<i32> {
        (self.bursts.is_empty() && self.now.is_empty() && self.asked >= 3).then_some(libc::SIGTERM)
    }
}

/// What the panel asked for in `sopen --tui` (a 100×30 terminal), the Backups list of
/// [`two_branches`] open, after `bursts`.
fn in_terminal(bursts: Vec<Vec<crossterm::event::Event>>) -> Vec<GitAction> {
    use crate::gui::term::{Backend, End};
    let mut view = fixture_view();
    view.set_repo(repo());
    view.open_backups(two_branches());
    let mut host = super::tests::Host {
        view,
        active: true,
        actions: Vec::new(),
        rect: egui::Rect::NOTHING,
        under: Vec::new(),
        side: None,
    };
    let mut all = vec![
        vec![crossterm::event::Event::FocusGained],
        vec![crossterm::event::Event::FocusGained],
    ];
    all.extend(bursts);
    let tty = Bursts {
        bursts: all.into(),
        now: Default::default(),
        asked: 0,
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = Backend::new(tty, std::io::sink(), ctx, true);
    let mut no_switch = |_: &mut Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut host, &mut no_switch).expect("the run");
    assert_eq!(end, End::Signal(libc::SIGTERM));
    host.actions
}

/// A key's press, as the terminal sends it.
fn tui_key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    use crossterm::event::{KeyEvent, KeyModifiers};
    crossterm::event::Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// M1: in `sopen --tui`, ↓ and Enter read together: the ↓'s pass is not printed (input waits),
/// so the screen still has the group under the bar, and the Enter Shows nothing — not the ref the
/// unprinted pass put the bar on.
#[test]
fn m1_down_and_enter_in_one_read_of_the_terminal_show_nothing() {
    use crossterm::event::KeyCode;
    let asked = in_terminal(vec![vec![tui_key(KeyCode::Down), tui_key(KeyCode::Enter)]]);
    assert_eq!(asked, Vec::<GitAction>::new());
}

/// M1, the control: ↓ printed, then Enter Shows the ref the screen has under the bar.
#[test]
fn m1_down_printed_then_enter_shows_its_ref() {
    use crossterm::event::KeyCode;
    let asked = in_terminal(vec![
        vec![tui_key(KeyCode::Down)],
        vec![tui_key(KeyCode::Enter)],
    ]);
    let main = two_branches()[0].group.refs[0].clone();
    assert_eq!(asked, [GitAction::ShowBackup(vec![main])]);
}

/// M1: a click in `sopen --tui` is two passes (the press and the release, `term_input`); read
/// together, the press's pass is not printed, and the click on `[ Show ]` still Shows — the screen
/// still has the group under the bar, which is what the pass before showed (the unprinted pass's
/// painting does not replace it, nor is it dropped).
#[test]
fn m1_a_click_in_one_read_of_the_terminal_shows() {
    use crossterm::event::{
        Event as CEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    // The panel over the terminal's grid but its F-key row (29 rows): the foot on row 25,
    // `[ Show ]` 11 cells and two blanks in, inside the frame.
    let mouse = |kind| {
        CEvent::Mouse(MouseEvent {
            kind,
            column: 1 + 13 + 2,
            row: foot_y(29),
            modifiers: KeyModifiers::NONE,
        })
    };
    let asked = in_terminal(vec![vec![
        mouse(MouseEventKind::Down(MouseButton::Left)),
        mouse(MouseEventKind::Up(MouseButton::Left)),
    ]]);
    let refs = two_branches()[0].group.refs[..2].to_vec();
    assert_eq!(asked, [GitAction::ShowBackup(refs)]);
}

/// M1: a double click on a line in `sopen --tui` — two clicks, each its press and release read
/// together, the second read after the first's passes — Shows that line: the first click's bar is
/// printed before the second comes. (Both clicks in one read Show nothing: the screen never had
/// the bar there, the guard's reading of a terminal that lags.)
#[test]
fn m1_a_double_click_in_the_terminal_shows_its_line() {
    use crossterm::event::{
        Event as CEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    // The `heads/b` line: the third of the list, on row 4 of the terminal.
    let mouse = |kind| {
        CEvent::Mouse(MouseEvent {
            kind,
            column: 10,
            row: FIRST_Y + 2,
            modifiers: KeyModifiers::NONE,
        })
    };
    let click = || {
        vec![
            mouse(MouseEventKind::Down(MouseButton::Left)),
            mouse(MouseEventKind::Up(MouseButton::Left)),
        ]
    };
    let asked = in_terminal(vec![click(), click()]);
    let b = two_branches()[0].group.refs[1].clone();
    assert_eq!(asked, [GitAction::ShowBackup(vec![b])]);
}

/// M1: a pass that drew no list leaves nothing under the bar on screen: in a panel too short for
/// its parts the list is not drawn, and Enter or F8 there acts on nothing.
#[test]
fn m1_a_short_panels_enter_acts_on_nothing() {
    let mut fx = backups_fx(1000.0, 400.0, two_branches());
    fx.idle();
    fx.drv.resize(1000.0, 80.0);
    fx.idle();
    fx.idle();
    assert_eq!(grid_of(&fx).rows, 4);
    assert!(fx.press(Key::Enter).is_empty(), "Enter");
    fx.press(Key::F8);
    assert_eq!(dialog_action(&fx), None, "F8");
    assert!(fx.view().backups().is_some());
}

// ---------------------------------------------------------------------------------------------
// The fix check.

/// MUST 1: on a grid whose dialog leaves no cell after a field's name (`at` 12, the inside 11 to
/// 14 cells), no cell of a value stands outside the box's content — the frame's border and the
/// blank margin inside it untouched — and a value with no room beside its name goes on its own
/// rows under it, from the content's left.
#[test]
fn a_value_never_stands_past_the_content_on_a_narrow_grid() {
    for cols in [19u16, 20, 21, 22] {
        let mut fx = log_fx(f32::from(cols) * 8.0, 500.0, entries(1));
        fx.press(Key::Enter);
        assert!(fx.view().dialog_up(), "{cols}");
        let g = grid_of(&fx);
        let frame = fx.view().covered(&g)[0];
        let modal = fx
            .drv
            .ctx
            .memory(|m| m.top_modal_layer())
            .expect("the dialog");
        let layer = (cells::layers(&fx.drv.ctx).into_iter())
            .find(|l| l.layer == modal)
            .expect("the dialog's cells");
        let seen = layer.seen();
        let a = layer.buf.area;
        let at = |x: u16, y: u16| seen[usize::from(y - a.y)][usize::from(x - a.x)].clone();
        let (left, right) = (frame.x, frame.right() - 1);
        let (top, bottom) = (frame.y, frame.bottom() - 1);
        for y in top + 1..bottom {
            assert_eq!(at(left, y).symbol, "║", "{cols}: ({left}, {y})");
            assert_eq!(at(right, y).symbol, "║", "{cols}: ({right}, {y})");
            for x in [left + 1, right - 1] {
                assert_eq!(at(x, y).symbol, " ", "{cols}: the margin at ({x}, {y})");
            }
        }
        for x in left + 1..right {
            for y in [top + 1, bottom - 1] {
                assert_eq!(at(x, y).symbol, " ", "{cols}: the margin at ({x}, {y})");
            }
        }
        // Where a text stands in the box: its row and first cell.
        let place = |text: &str| {
            (top..=bottom).find_map(|y| {
                let row: String = (left..=right).map(|x| at(x, y).symbol).collect();
                let i = row.find(text)?;
                Some((y, left + row[..i].chars().count() as u16))
            })
        };
        let (name_y, name_x) = place("Host").expect("the name");
        let width = cols - 8;
        if width <= 12 {
            let value = place("box").expect("the host's value");
            assert_eq!(value, (name_y + 1, name_x), "{cols}: under its name");
        } else {
            // Beside it, from cell 12, in the cell or two left there a row.
            let first = at(name_x + 12, name_y).symbol;
            assert_eq!(first, "b", "{cols}: beside its name");
        }
    }
}

/// SHOULD 3: in `sopen --tui`, the key help over the list (F1), then Esc and F8 read together:
/// the screen had the help over the bar's line, so the F8 deletes nothing — the Esc's pass, which
/// shows the list again, is not printed.
#[test]
fn f8_after_the_help_over_the_list_in_one_read_deletes_nothing() {
    use crossterm::event::KeyCode;
    let (asked, dialog) = in_terminal_with_help(vec![
        vec![tui_key(KeyCode::F(1))],
        vec![tui_key(KeyCode::Esc), tui_key(KeyCode::F(8))],
    ]);
    assert_eq!(asked, Vec::<GitAction>::new());
    assert_eq!(dialog, None, "no delete asked");
}

/// SHOULD 3, the control: the help closed and printed, F8 deletes the group under the bar.
#[test]
fn f8_after_the_help_closed_deletes_the_group() {
    use crossterm::event::KeyCode;
    let (_, dialog) = in_terminal_with_help(vec![
        vec![tui_key(KeyCode::F(1))],
        vec![tui_key(KeyCode::Esc)],
        vec![tui_key(KeyCode::F(8))],
    ]);
    let stamp = "20261128T010203Z".to_string();
    assert_eq!(dialog, Some(Action::DeleteBackup { stamp }));
}

/// The panel's host in a terminal, opening the key help on the Help it asks for, as the browser
/// does.
struct WithHelp(super::tests::Host);

impl Window for WithHelp {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.0.ui(ui);
        let ctx = ui.ctx().clone();
        self.0.actions.retain(|a| {
            let help = *a == GitAction::Help;
            if help {
                let page = crate::gui::help::for_context(keys::Context::Git, Mode::Preview, &[]);
                crate::gui::help::open(&ctx, page);
            }
            !help
        });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// What the panel asked for and the dialog it opened in `sopen --tui`, the key help working as the
/// browser's, the Backups list of [`two_branches`] open, after `bursts`.
fn in_terminal_with_help(
    bursts: Vec<Vec<crossterm::event::Event>>,
) -> (Vec<GitAction>, Option<Action>) {
    use crate::gui::term::{Backend, End};
    let mut view = fixture_view();
    view.set_repo(repo());
    view.open_backups(two_branches());
    let mut win = WithHelp(super::tests::Host {
        view,
        active: true,
        actions: Vec::new(),
        rect: egui::Rect::NOTHING,
        under: Vec::new(),
        side: None,
    });
    let mut all = vec![
        vec![crossterm::event::Event::FocusGained],
        vec![crossterm::event::Event::FocusGained],
    ];
    all.extend(bursts);
    let tty = Bursts {
        bursts: all.into(),
        now: Default::default(),
        asked: 0,
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = Backend::new(tty, std::io::sink(), ctx, true);
    let mut no_switch = |_: &mut Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, End::Signal(libc::SIGTERM));
    let dialog = win.0.view.dialog.as_ref().map(|d| d.draft().clone());
    (win.0.actions, dialog)
}

/// The panel's host, its pass run twice in a frame when asked (egui's discarded pass: a Grid
/// sizing itself), as a terminal's `run_ui` can.
struct Twice {
    host: super::tests::Host,
    discard: bool,
}

impl Window for Twice {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.discard && ui.ctx().current_pass_index() == 0 {
            ui.ctx().request_discard("the test's second pass");
        }
        self.host.ui(ui);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// SHOULD 5: what the screen shows is a frame's — `cells::set_presented` describes the frame, and
/// a frame can run two passes. A frame shows the group under the bar; the next, run twice (↓ on
/// its first pass), is not printed; then Enter Shows nothing — the screen still has the group,
/// not the ref the unprinted frame's passes put the bar on.
#[test]
fn a_frame_of_two_passes_unprinted_is_not_on_screen() {
    let mut view = fixture_view();
    view.set_repo(repo());
    view.open_backups(two_branches());
    let mut win = Twice {
        host: super::tests::Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        discard: false,
    };
    let mut drv = crate::gui::testkit::Driver::new(1000.0, 400.0);
    drv.idle(&mut win, 3);
    win.discard = true;
    drv.frame(
        &mut win,
        key_events(Key::ArrowDown, Modifiers::NONE),
        Modifiers::NONE,
    );
    win.discard = false;
    cells::set_presented(&drv.ctx, false);
    drv.frame(
        &mut win,
        key_events(Key::Enter, Modifiers::NONE),
        Modifiers::NONE,
    );
    assert_eq!(win.host.actions, Vec::<GitAction>::new());
}

/// SHOULD 5: a switch between the window and the terminal moves the view to another context,
/// whose frames count apart: the first frame there has shown nothing of the list yet, so Enter
/// in it Shows nothing, whatever its frame's number; the next Enter Shows.
#[test]
fn another_context_has_shown_nothing_yet() {
    let mut a = backups_fx(1000.0, 400.0, two_branches());
    a.idle();
    let mut b = Fx::new(1000.0, 400.0, fixture_view());
    // B's next frame is the one after A's last: the same count, another context.
    let next = a.drv.ctx.cumulative_frame_nr();
    while b.drv.ctx.cumulative_frame_nr() < next {
        b.idle();
    }
    assert_eq!(b.drv.ctx.cumulative_frame_nr(), next);
    std::mem::swap(&mut a.host.view, &mut b.host.view);
    assert!(b.press(Key::Enter).is_empty(), "nothing on B's screen yet");
    let both = two_branches()[0].group.refs[..2].to_vec();
    assert_eq!(b.press(Key::Enter), [GitAction::ShowBackup(both)]);
}

// ---------------------------------------------------------------------------------------------
// Receipts.

/// The panel's release frame cost at 1000×700 with the log open — the runner's 500 entries
/// (`LOG_ENTRIES`), Hangul in every command line, every tenth failed — the panel over the whole
/// window, run alone: `cargo test --release --lib -- --ignored --exact
/// gui::browser::git_panel::tests_x4_lists::frame_cost_with_the_log_at_1000_by_700 --nocapture`.
/// Printed, no budget set.
#[test]
#[ignore]
fn frame_cost_with_the_log_at_1000_by_700() {
    use std::time::Instant;
    let log: Vec<LogEntry> = (0..500)
        .map(|k| {
            let ending = match k % 10 {
                3 => Ending::Exit(1),
                _ => Ending::Exit(0),
            };
            let line = format!("git commit -m 커밋-{k}-한글-제목 --allow-empty -- 경로/{k}");
            entry(&line, ending, "fatal: 한글 오류", k as u64)
        })
        .collect();
    let mut fx = Fx::new(1000.0, 700.0, fixture_view());
    fx.view_mut().open_log(log);
    fx.drv.idle(&mut fx.host, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the git panel's frame at 1000x700 with the log: {:?}",
        started.elapsed() / n
    );
}

/// TS-4 (10 §14): the log, an entry shown whole and the Backups list — the same cells in both
/// backends, one buffer each, in one test (the log's entries carry each run's times, so its 1:1
/// is this harness's, 10 §17).
mod ts4 {
    use super::super::tests::{history, refs, rows_of, Host};
    use super::*;
    use crate::gui::chrome::CELL;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
    use crate::gui::testkit::Driver;
    use eframe::egui::Rect;
    use std::io;
    use std::time::Duration;

    /// A terminal that sends nothing, `cols` × `rows`, never ambiguous-wide: a receipt run's.
    struct Still((u16, u16));

    impl Tty for Still {
        fn raw(&mut self, _on: bool) -> io::Result<()> {
            Ok(())
        }
        fn size(&mut self) -> io::Result<(u16, u16)> {
            Ok(self.0)
        }
        fn probe(&mut self) -> Probed {
            Probed {
                kitty: false,
                ambiguous: false,
            }
        }
        fn drop_input(&mut self) {}
        fn next(&mut self, wait: Duration) -> io::Result<Option<crossterm::event::Event>> {
            std::thread::sleep(wait.min(Duration::from_millis(1)));
            Ok(None)
        }
        fn pending(&mut self) -> io::Result<bool> {
            Ok(false)
        }
        fn stopped(&self) -> Option<i32> {
            None
        }
    }

    /// The terminal's `SOPEN_CELLS` receipt of `win` on a `cols` × `rows` terminal, and the time
    /// of its last pass.
    fn terminal_run<W: Window>(win: &mut W, cols: u16, rows: u16) -> (String, f64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cells.txt");
        let ctx = egui::Context::default();
        chrome::install(&ctx);
        let mut b = Backend::new(Still((cols, rows)), io::sink(), ctx.clone(), true);
        b.offset = 1_000.0;
        b.receipt = Some(Receipt::new(path.clone(), Duration::from_secs(30)));
        let mut no_switch = |_: &mut Backend<Still, io::Sink>| -> io::Result<Option<String>> {
            panic!("no switch in this test")
        };
        let end = b.run(win, &mut no_switch).expect("the receipt");
        assert_eq!(end, End::Closed);
        let when = ctx.input(|i| i.time);
        (std::fs::read_to_string(&path).unwrap(), when)
    }

    /// The window's `SOPEN_CELLS` receipt of one more pass of `win` in `drv`'s context at `time`
    /// seconds, as the shell writes it (`term::window_receipt`).
    fn window_receipt<W: Window>(
        drv: &Driver,
        win: &mut W,
        cols: u16,
        rows: u16,
        time: f64,
    ) -> String {
        let size = egui::vec2(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            max_texture_side: Some(8192),
            focused: true,
            ..Default::default()
        };
        let mut pass = None;
        let mut out = drv.ctx.run_ui(raw, |ui| {
            cells::clear(ui.ctx());
            win.ui(ui);
            pass = Some(term::keep_window_pass(ui.ctx()));
        });
        out.textures_delta.clear();
        term::window_receipt(&drv.ctx, &pass.expect("the pass ran"))
    }

    /// What the host shows over the panel's body.
    #[derive(Clone, Copy, Debug)]
    enum Open {
        Log,
        Whole,
        Backups,
    }

    /// The graph host with `open` over its body: the log of three entries (one failed, Hangul in
    /// command lines and stderr), the failed one shown whole, or the Backups list with a Hangul
    /// branch's group above the S3 fixture's.
    fn host(open: Open) -> Host {
        let mut view = GitView::new("box:/srv/repo");
        view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
        view.set_rows(rows_of(history(), true), false, 2);
        view.set_paging(Paging::AllLoaded);
        view.set_repo(repo());
        let mut failed = entry(
            "git push --progress origin 기능",
            Ending::Exit(1),
            "error: failed to push some refs to 'origin'\n\thint: 한글 힌트\n",
            60,
        );
        failed.stdin = Some("message, 3 lines".into());
        let log = vec![
            entry("git fetch --progress --all", Ending::Exit(0), "", 120),
            failed,
            entry("git branch -d 기능", Ending::Exit(0), "", 0),
        ];
        match open {
            Open::Log => view.open_log(log),
            Open::Whole => {
                view.open_log(log);
                let log = view.log.as_mut().unwrap();
                log.run(Command::Bar(Step::Down));
                log.run(Command::Details);
            }
            Open::Backups => {
                let mut all = vec![group(
                    "20261128T010203Z",
                    "sopen: delete branch 기능",
                    vec![
                        (
                            BackupKind::Head("기능".into()),
                            "heads/기능",
                            'c',
                            "기능: 첫 커밋",
                        ),
                        (
                            BackupKind::Manifest,
                            "manifest",
                            'e',
                            "sopen: delete branch 기능",
                        ),
                    ],
                )];
                all.extend(records());
                view.open_backups(all);
            }
        }
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    /// TS-4's lists: the terminal's receipt and the window's are the same bytes for the log, its
    /// entry shown whole and the Backups list, at 100×30 and at 60×20, each with what it shows
    /// and no hand-off box.
    #[test]
    fn ts4_the_log_the_entry_and_the_backups_list_are_the_same_cells_in_both_backends() {
        for open in [Open::Log, Open::Whole, Open::Backups] {
            for (cols, rows) in [(100, 30), (60, 20)] {
                let mut tui = host(open);
                let (terminal, when) = terminal_run(&mut tui, cols, rows);
                let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
                let mut win = host(open);
                drv.idle(&mut win, 2);
                let in_window = window_receipt(&drv, &mut win, cols, rows, when);
                assert_eq!(terminal, in_window, "{open:?} {cols}x{rows}");
                let wanted: &[&str] = match open {
                    Open::Log => &["Command log", "git fetch --progress --all", "기능"],
                    Open::Whole => &["Command", "message, 3 lines", "hint: 한글 힌트", "[ OK ]"],
                    Open::Backups => &["Backups under", "heads/기능", "기능: 첫", "[ Restore ]"],
                };
                for text in wanted {
                    let at = format!("{open:?} {cols}x{rows}: {text}");
                    assert!(terminal.contains(text), "{at}\n{terminal}");
                }
                let at = format!("{open:?} {cols}x{rows}: no hand-off box");
                assert!(!terminal.contains("F12"), "{at}\n{terminal}");
            }
        }
    }
}
