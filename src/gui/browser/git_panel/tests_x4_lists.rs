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

/// The panel's layer of cells: the one over all of its side.
fn panel(fx: &Fx) -> Arc<Layer> {
    let grid = grid_of(fx);
    let side = fx.host.side.unwrap_or(grid.all());
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(|l| l.buf.area == side && l.origin == grid.origin)
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
                self.ends
                    .extend(backups::draw(ui, &grid, grid.all(), &mut buf, view, true));
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
