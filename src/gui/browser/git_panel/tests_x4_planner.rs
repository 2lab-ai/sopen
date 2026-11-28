//! X4-planner's tests (10 §17): the planner's pane in cells over the panel's body (03 §7.8.2) —
//! its rows, the bar, the scroll bar, the note and the keys' hint at its foot; its clicks, wheel
//! and keys by cells, several in one pass; what the panel blanks over it; the same cells in both
//! backends (TS-4).

use super::tests::{fixture_view, Fx};
use super::*;
use crate::git::model::{CommitRow, Oid};
use crate::git::rebase::{Author, Base, Commit, CommitterDate, Date, Model, Verb};
use crate::gui::cells::{Layer, Seen};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::PALETTE;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Fixtures.

/// Commit `k`'s id: its number in eight hex digits, five times — so its short form is the number.
fn id(k: u32) -> Oid {
    Oid::parse(&format!("{k:08x}").repeat(5)).expect("hex")
}

/// Commit `k`'s short form.
fn short(k: u32) -> String {
    format!("{k:08x}")
}

/// A date of the fixture's zone, `+0900`.
fn date(seconds: i64) -> Date {
    Date {
        seconds,
        offset_minutes: 540,
    }
}

/// The author date of commit `k`: a day apart.
fn when(k: u32) -> i64 {
    1_790_000_000 + i64::from(k) * 86_400
}

/// Commit `k` of a range as the planner reads it, by `author` at [`when`], its parent `k - 1`, its
/// message `<subject>`, a blank line and a body.
fn commit(k: u32, author: &str, subject: &str) -> Commit {
    Commit {
        row: CommitRow {
            oid: id(k),
            parents: vec![id(k - 1)],
            author_name: author.into(),
            author_email: "dev@example.com".into(),
            author_time: when(k),
            commit_time: when(k),
            subject: subject.into(),
        },
        message: format!("{subject}\n\nbody of {k}\n"),
        message_lossy: false,
        author: Author {
            name: author.into(),
            email: "dev@example.com".into(),
            date: date(when(k)),
            committer: CommitterDate::Keep,
        },
        committed: date(when(k)),
    }
}

/// A range of `n` commits above commit 1 as it is read (every row a pick), the newest first —
/// commit `n + 1` down to 2: Hangul and Latin subjects and authors.
fn plain(n: u32) -> Model {
    let rows = (2..n + 2)
        .rev()
        .map(|k| {
            let author = if k % 2 == 0 { "김철수" } else { "Ann Lee" };
            commit(
                k,
                author,
                &format!("커밋 {k}: 한글 제목과 an English subject"),
            )
        })
        .collect();
    Model::new(Base::Commit(id(1)), rows)
}

/// [`plain`]'s range with a verb other than pick on every seventh row from the top.
fn range(n: u32) -> Model {
    let mut m = plain(n);
    let verbs = [Verb::Reword, Verb::Edit, Verb::Fixup, Verb::Drop];
    let len = m.rows.len();
    for (j, k) in (0..len).step_by(7).enumerate() {
        // A squash or fixup keeps a kept row below it: never the last.
        if k + 1 < len {
            m.rows[k].verb = verbs[j % verbs.len()];
        }
    }
    m
}

/// A Hangul subject wider than the subject's room at 1000 points.
fn long_hangul() -> String {
    "한글 제목이 아주 길다 ".repeat(8)
}

/// Commits 9 (the newest) to 2: a drop row, an author changed (`Grace`, 2023-11-14), a Hangul
/// subject longer than its room and every verb, with a kept row below the squash and the fixup.
fn every_verb() -> Model {
    let rows = vec![
        commit(9, "Ada", "ninth"),
        commit(8, "Ada", "eighth"),
        commit(7, "김철수", &long_hangul()),
        commit(6, "Ada", "sixth"),
        commit(5, "Ada", "fifth"),
        commit(4, "Ada", "fourth"),
        commit(3, "Ada", "third"),
        commit(2, "Ada", "second"),
    ];
    let mut m = Model::new(Base::Commit(id(1)), rows);
    let verbs = [
        Verb::Pick,
        Verb::Drop,
        Verb::Reword,
        Verb::Edit,
        Verb::Squash,
        Verb::Fixup,
        Verb::Pick,
        Verb::Pick,
    ];
    for (row, verb) in m.rows.iter_mut().zip(verbs) {
        row.verb = verb;
    }
    m.rows[2].author = Some(Author {
        name: "Grace".into(),
        email: "grace@example.com".into(),
        date: date(1_700_000_000),
        committer: CommitterDate::Keep,
    });
    m
}

/// The fixture's panel with the planner open on `model`, in a `w`×`h` window.
fn with_planner(w: f32, h: f32, model: Model) -> Fx {
    let mut fx = Fx::new(w, h, fixture_view());
    fx.view_mut().set_repo(super::tests_s4::repo());
    fx.view_mut().open_planner(model);
    fx.idle();
    fx.idle();
    fx
}

/// The open planner.
fn pane(fx: &Fx) -> &planner::PlannerView {
    fx.view().planner.as_ref().expect("the planner is open")
}

/// The open planner's row `i`, and its bar's row.
fn verb_of(fx: &Fx, i: usize) -> Verb {
    pane(fx).model().rows[i].verb
}

fn bar_of(fx: &Fx) -> usize {
    let bar = pane(fx).bar().expect("a bar");
    (pane(fx).model().rows.iter())
        .position(|r| &r.commit.row.oid == bar)
        .expect("the bar's row")
}

// ---------------------------------------------------------------------------------------------
// Reading the panel's cells.

/// The panel's grid: the one its host laid over the window's central area.
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The panel's layer of cells: the one over all of its side.
fn panel(fx: &Fx) -> Arc<Layer> {
    let grid = grid_of(fx);
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(|l| l.buf.area == grid.all() && l.origin == grid.origin)
        .expect("the panel's cells over its side")
}

/// Every cell of the panel, a row at a time (a wide glyph's second cell shows nothing).
fn seen(fx: &Fx) -> Vec<Vec<Seen>> {
    panel(fx).seen()
}

/// Row `y` of the panel from column `from` to `to` (exclusive), as text.
fn text(fx: &Fx, y: u16, from: u16, to: u16) -> String {
    seen(fx)[usize::from(y)][usize::from(from)..usize::from(to)]
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

/// Row `y` of the panel, whole.
fn line(fx: &Fx, y: u16) -> String {
    text(fx, y, 0, grid_of(fx).cols)
}

/// The cell at column `x` of row `y`.
fn cell(fx: &Fx, x: u16, y: u16) -> Seen {
    seen(fx)[usize::from(y)][usize::from(x)].clone()
}

/// Where `text` first stands on row `y`: its first cell's column.
fn at(fx: &Fx, y: u16, text: &str) -> Option<u16> {
    let row = &seen(fx)[usize::from(y)];
    let (mut s, mut cols) = (String::new(), vec![]);
    for (x, c) in row.iter().enumerate() {
        for ch in c.symbol.chars() {
            s.push(ch);
            cols.push(x as u16);
        }
    }
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

/// The cells the composed screen leaves to hand-off boxes, inside the panel's grid.
fn boxes(fx: &Fx) -> Vec<Cells> {
    let g = grid_of(fx);
    let layers = cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows);
    (screen.hand_offs.into_iter())
        .filter(|r| r.y < g.rows)
        .collect()
}

// In a 1000×500 window the test host's grid is 125×30: the frame inside is 123×28, the heads'
// row is row 1, the pane's rows start at row 2, its foot ends at row 26 above the rule (27) and
// the paging line (28). The pane's columns (10 §17, X4-planner's decisions), two blanks apart,
// in the grid's columns: the verb at 1, the `A` mark at 9, the hash at 12, the subject at 22, the
// date in the last 16 of the pane's 123 cells (108) and the author 2 and 16 before it (90).
const VERB_AT: u16 = 1;
const MARK_AT: u16 = 9;
const HASH_AT: u16 = 12;
const SUBJECT_AT: u16 = 22;
const AUTHOR_AT: u16 = 90;
const DATE_AT: u16 = 108;
/// The subject's room: up to two cells before the author.
const SUBJECT_ROOM: u16 = AUTHOR_AT - 2 - SUBJECT_AT;

// ---------------------------------------------------------------------------------------------
// The pane (03 §7.8.2).

/// The pane is cells over the panel's body: the heads dim; each row's verb, `A` mark, hash,
/// subject (a Hangul one cut by cells, never half a glyph, `…` where it stops), author and date
/// at their columns, each a text of its own (two blanks between); a dropped row dim; the bar in
/// the bar's colours; the scroll bar on the border beside the rows, the frame's `║` beside the
/// heads and the foot; no hand-off box anywhere.
#[test]
fn the_pane_is_cells_over_the_body() {
    let mut fx = with_planner(1000.0, 500.0, every_verb());
    let g = grid_of(&fx);
    assert_eq!((g.cols, g.rows), (125, 30));
    for (head, x) in [
        ("Verb", VERB_AT),
        ("Commit", HASH_AT),
        ("Subject", SUBJECT_AT),
        ("Author", AUTHOR_AT),
        ("Date", DATE_AT),
    ] {
        assert_eq!(at(&fx, 1, head), Some(x), "{head}: {:?}", line(&fx, 1));
        assert_eq!(cell(&fx, x, 1).fg, Some(LOOK.dim), "{head} dim");
    }
    let words = ["pick", "drop", "reword", "edit", "squash", "fixup", "pick"];
    for (i, word) in words.iter().enumerate() {
        let y = 2 + i as u16;
        let k = 9 - i as u32;
        let row = line(&fx, y);
        assert_eq!(
            text(&fx, y, VERB_AT, VERB_AT + 6).trim_end(),
            *word,
            "{row:?}"
        );
        assert_eq!(at(&fx, y, &short(k)), Some(HASH_AT), "{row:?}");
        assert_eq!(
            text(&fx, y, HASH_AT + 8, HASH_AT + 10),
            "  ",
            "two blanks after the hash"
        );
    }
    assert_eq!(at(&fx, 2, "ninth"), Some(SUBJECT_AT));
    assert_eq!(at(&fx, 2, "Ada"), Some(AUTHOR_AT));
    assert_eq!(at(&fx, 2, &graph_view::date_text(when(9))), Some(DATE_AT));
    // The changed author's row: `A`, its new name and date; the Hangul subject cut by cells.
    let y = 4;
    assert_eq!(text(&fx, y, MARK_AT, MARK_AT + 1), "A");
    assert_eq!(
        text(&fx, y, MARK_AT - 2, MARK_AT),
        "  ",
        "two blanks before A"
    );
    assert_eq!(text(&fx, y, MARK_AT + 1, MARK_AT + 3), "  ", "two after");
    assert_eq!(at(&fx, y, "Grace"), Some(AUTHOR_AT));
    assert_eq!(
        at(&fx, y, &graph_view::date_text(1_700_000_000)),
        Some(DATE_AT)
    );
    let cut = cells::fit_start(&long_hangul(), usize::from(SUBJECT_ROOM));
    assert!(cut.ends_with('…') && cells::width(&cut) <= usize::from(SUBJECT_ROOM));
    assert_eq!(at(&fx, y, &cut), Some(SUBJECT_AT), "{:?}", line(&fx, y));
    for y in 2..10 {
        let mark = if y == 4 { "A" } else { " " };
        assert_eq!(
            text(&fx, y, MARK_AT, MARK_AT + 1),
            mark,
            "the mark on row {y}"
        );
    }
    // The bar on the newest, the panel having the keyboard; a dropped row dim.
    let bar = cell(&fx, SUBJECT_AT, 2);
    assert_eq!((bar.fg, bar.bg), (Some(LOOK.on_bar), Some(PALETTE.bar)));
    assert_eq!(cell(&fx, 60, 2).bg, Some(PALETTE.bar), "across the row");
    let dropped = cell(&fx, VERB_AT, 3);
    assert_eq!(
        (dropped.fg, dropped.bg),
        (Some(LOOK.dim), Some(PALETTE.back))
    );
    let plain = cell(&fx, VERB_AT, 5);
    assert_eq!((plain.fg, plain.bg), (Some(LOOK.text), Some(PALETTE.back)));
    // The scroll bar on the border beside the rows (2 to 25), the frame's side elsewhere.
    let right = g.cols - 1;
    assert_eq!(cell(&fx, right, 2).symbol, "↑");
    assert_eq!(cell(&fx, right, 25).symbol, "↓");
    // Nothing to scroll: the thumb at the top of the track, the track below it.
    assert_eq!(cell(&fx, right, 3).bg, Some(PALETTE.thumb));
    assert_eq!(cell(&fx, right, 4).bg, Some(PALETTE.track_back));
    for y in [1, 26] {
        assert_eq!(cell(&fx, right, y).symbol, "║", "row {y}");
    }
    for y in 1..27 {
        assert_eq!(
            cell(&fx, 0, y).symbol,
            "║",
            "the frame's left side, row {y}"
        );
    }
    assert_eq!(boxes(&fx), Vec::<Cells>::new(), "no hand-off box");
    // Not the keyboard's: the bar idle.
    fx.host.active = false;
    fx.idle();
    let bar = cell(&fx, SUBJECT_AT, 2);
    assert_eq!(bar.bg, Some(PALETTE.bar_idle));
    assert_eq!(bar.fg, Some(LOOK.text));
}

/// A narrow pane drops the author and the date, the subject keeping what is left up to the
/// frame's border, where the scroll bar stands beside the rows.
#[test]
fn a_narrow_pane_drops_the_author_and_the_date() {
    let fx = with_planner(60.0 * 8.0, 500.0, every_verb());
    let g = grid_of(&fx);
    assert_eq!(g.cols, 60);
    let heads = line(&fx, 1);
    assert!(
        !heads.contains("Author") && !heads.contains("Date"),
        "{heads:?}"
    );
    assert_eq!(at(&fx, 1, "Subject"), Some(SUBJECT_AT));
    let row = line(&fx, 4);
    let room = usize::from(g.cols - 1 - SUBJECT_AT);
    let cut = cells::fit_start(&long_hangul(), room);
    assert_eq!(at(&fx, 4, &cut), Some(SUBJECT_AT), "{row:?}");
    assert_eq!(
        cell(&fx, g.cols - 1, 4).bg,
        Some(PALETTE.track_back),
        "{row:?}"
    );
    assert_eq!(cell(&fx, g.cols - 1, 1).symbol, "║");
    assert!(!row.contains("Grace"), "{row:?}");
}

/// The foot (03 §7.8.2): the checks that fail and an op's refusal in the error colour, a key's
/// hint dim, each a row; then `[ Run… ]` and `[ Close ]` — black on cyan, as egui's buttons were —
/// and the keys' hint, two blanks apart. A click on a button's cells is its press.
#[test]
fn the_foot_says_the_checks_the_note_and_the_keys() {
    let mut fx = with_planner(1000.0, 500.0, every_verb());
    // The buttons and the hint on the foot's last row.
    let y = 26;
    assert_eq!(at(&fx, y, "[ Run… ]"), Some(1), "{:?}", line(&fx, y));
    assert_eq!(at(&fx, y, "[ Close ]"), Some(11));
    let cmd = keys::Mods::Command.prefix();
    let hint = format!("P R E S F D verb · A author and dates · {cmd}↑↓ move");
    assert_eq!(at(&fx, y, &hint), Some(22), "{:?}", line(&fx, y));
    assert_eq!(cell(&fx, 22, y).fg, Some(LOOK.dim));
    let button = cell(&fx, 3, y);
    assert_eq!(
        (button.fg, button.bg),
        (Some(PALETTE.dialog_text), Some(PALETTE.dialog_back))
    );
    assert_eq!(cell(&fx, 9, y).bg, Some(PALETTE.back), "two blanks between");
    // A refusal: Cmd/Ctrl+↑ on the top row.
    fx.key(Key::ArrowUp, egui::Modifiers::COMMAND);
    let why = format!("{} is at the top: it cannot move up", short(9));
    assert_eq!(at(&fx, 25, &why), Some(1), "{:?}", line(&fx, 25));
    assert_eq!(cell(&fx, 1, 25).fg, Some(LOOK.error));
    // A key's hint: Enter on a pick row.
    fx.press(Key::Enter);
    assert_eq!(at(&fx, 25, planner::ENTER_HINT), Some(1));
    assert_eq!(cell(&fx, 1, 25).fg, Some(LOOK.dim));
    // A check that fails: the bottom row a squash, nothing below it.
    fx.press(Key::End);
    fx.typed("s");
    let problem = format!(
        "{} is a squash or fixup with no kept commit below it to go into",
        short(2)
    );
    assert_eq!(at(&fx, 25, &problem), Some(1), "{:?}", line(&fx, 25));
    assert_eq!(cell(&fx, 1, 25).fg, Some(LOOK.error));
    assert_eq!(at(&fx, 26, "[ Run… ]"), Some(1), "the buttons below it");
    // A click on `[ Run… ]`'s cells: the run's confirmation.
    let p = centre(&fx, 4, 26);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert!(pane(&fx).dialog_up(), "the run's confirmation");
    fx.press(Key::Escape);
    assert!(!pane(&fx).dialog_up());
    // A click on `[ Close ]`'s cells with nothing changed: the planner closes.
    let mut fx = with_planner(1000.0, 500.0, plain(5));
    let p = centre(&fx, 15, 26);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert!(fx.view().planner_model().is_none(), "closed");
}

// ---------------------------------------------------------------------------------------------
// The pointer and the keys by cells, several events in one pass.

/// Keys by cells, several in one pass: two ↓ move the bar two rows, and that pass's cells have
/// it there; a letter typed, then ↓, sets the verb on the row it was typed on before the bar
/// moves; two Cmd/Ctrl+↓ move the row two places.
#[test]
fn several_keys_in_one_pass_act_in_their_order() {
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(bar_of(&fx), 2, "two rows");
    assert_eq!(
        cell(&fx, SUBJECT_AT, 4).bg,
        Some(PALETTE.bar),
        "in that pass"
    );
    assert_eq!(cell(&fx, SUBJECT_AT, 2).bg, Some(PALETTE.back));
    // `r` then ↓: the reword on row 2, the bar on row 3.
    let mut events = vec![Event::Text("r".into())];
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        (verb_of(&fx, 2), verb_of(&fx, 3)),
        (Verb::Reword, Verb::Pick)
    );
    assert_eq!(bar_of(&fx), 3);
    assert_eq!(text(&fx, 4, VERB_AT, VERB_AT + 6), "reword", "in that pass");
    assert_eq!(cell(&fx, SUBJECT_AT, 5).bg, Some(PALETTE.bar));
    // Two Cmd/Ctrl+↓: row 3 (commit 4) two places down, the bar with it.
    let moved = pane(&fx).model().rows[3].commit.row.oid.clone();
    let mut events = key_events(Key::ArrowDown, Modifiers::COMMAND);
    events.extend(key_events(Key::ArrowDown, Modifiers::COMMAND));
    fx.drv.frame(&mut fx.host, events, Modifiers::COMMAND);
    assert_eq!(pane(&fx).model().rows[5].commit.row.oid, moved);
    assert_eq!(bar_of(&fx), 5);
    assert_eq!(at(&fx, 7, &short(4)), Some(HASH_AT), "in that pass");
}

/// Clicks by cells: a click on a row's cells puts the bar there; two in one pass, the second's
/// row; a letter typed and a click in one pass — the letter sets the bar's row's verb, then the
/// bar goes to the clicked row. A click on the heads, the foot or the frame moves nothing.
#[test]
fn clicks_go_by_cells_with_keys_in_the_same_pass() {
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let p = centre(&fx, 40, 5);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert_eq!(bar_of(&fx), 3);
    assert_eq!(cell(&fx, 40, 5).bg, Some(PALETTE.bar), "the bar's cells");
    // Two clicks in one pass: egui keeps the last press's place, row 4 (cell row 6).
    let mut events = click_events(centre(&fx, 40, 3));
    events.extend(click_events(centre(&fx, 40, 6)));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(bar_of(&fx), 4, "the second click's row");
    // `d` and a click on row 0 in one pass: row 4 dropped, the bar on row 0.
    let mut events = vec![Event::Text("d".into())];
    events.extend(click_events(centre(&fx, 40, 2)));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(verb_of(&fx, 4), Verb::Drop);
    assert_eq!(verb_of(&fx, 0), Verb::Pick);
    assert_eq!(bar_of(&fx), 0);
    assert_eq!(cell(&fx, 40, 2).bg, Some(PALETTE.bar));
    // The heads, the empty rows below the range, the frame's title: nothing (egui takes a click
    // up to 5 points off a widget for it, so the frame's sides are not cells of their own here).
    for (x, y) in [(40, 1), (40, 12), (40, 0)] {
        let p = centre(&fx, x, y);
        fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        fx.idle();
        assert_eq!(bar_of(&fx), 0, "a click at ({x}, {y})");
    }
}

/// The wheel by cells over the rows: a point delta a row each 16 points, a line three rows —
/// two wheel events in one pass add up — and the bar stays.
#[test]
fn the_wheel_goes_by_cells() {
    let mut fx = with_planner(1000.0, 500.0, plain(100));
    let right = grid_of(&fx).cols - 1;
    assert_eq!(
        cell(&fx, right, 3).bg,
        Some(PALETTE.thumb),
        "the control: the thumb at the top"
    );
    let wheel = |unit, dy: f32| Event::MouseWheel {
        unit,
        delta: Vec2::new(0.0, dy),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    let events = vec![
        Event::PointerMoved(centre(&fx, 40, 6)),
        wheel(egui::MouseWheelUnit::Point, -32.0),
        wheel(egui::MouseWheelUnit::Line, -1.0),
    ];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    // Row 5 at the top: commit 101 - 5.
    assert_eq!(at(&fx, 2, &short(96)), Some(HASH_AT), "{:?}", line(&fx, 2));
    assert_eq!(bar_of(&fx), 0, "the bar stays");
    // The thumb: a cell of the 21 between the arrows' first and last, at 5 of 76 (100 rows, 24
    // shown) — one cell down.
    assert_eq!(
        cell(&fx, right, 4).bg,
        Some(PALETTE.thumb),
        "the thumb moved"
    );
    assert_eq!(cell(&fx, right, 3).bg, Some(PALETTE.track_back));
}

// ---------------------------------------------------------------------------------------------
// What the panel blanks over the pane, a short panel, an ambiguous-wide terminal.

/// The cells under a dialog of the list's (`under`) and under the key help are blank in the
/// pane's cells — a Hangul glyph half under a dialog's left edge goes whole, as a terminal erases
/// it, the one before it stays.
#[test]
fn the_panel_blanks_the_pane_under_the_dialogs_and_the_help() {
    let mut fx = with_planner(1000.0, 500.0, every_verb());
    let y = 4;
    // The cut subject's glyphs from the subject's column: 한 글 (blank) 제 …, two cells each.
    let edge = SUBJECT_AT + 3;
    assert_eq!(
        cell(&fx, edge, y).symbol,
        "",
        "the control: 글's second cell"
    );
    fx.host.under = vec![Cells::new(edge, y, 10, 2)];
    fx.idle();
    for c in edge..edge + 10 {
        let s = cell(&fx, c, y);
        assert_eq!(
            (s.symbol.as_str(), s.bg),
            (" ", Some(PALETTE.back)),
            "cell {c}"
        );
    }
    assert_eq!(
        cell(&fx, edge - 1, y).symbol,
        " ",
        "the glyph half under it, gone"
    );
    assert_eq!(
        cell(&fx, SUBJECT_AT, y).symbol,
        "한",
        "the one before stays"
    );
    fx.host.under.clear();
    let page = crate::gui::help::for_context(keys::Context::Git, Mode::Preview, &[]);
    crate::gui::help::open(&fx.drv.ctx, page);
    fx.idle();
    let covered = crate::gui::help::covered(&fx.drv.ctx, &grid_of(&fx));
    assert!(!covered.is_empty());
    let rows = seen(&fx);
    for r in covered {
        for yy in r.top()..r.bottom() {
            for xx in r.left()..r.right() {
                let s = &rows[usize::from(yy)][usize::from(xx)];
                assert_eq!(
                    (s.symbol.as_str(), s.bg),
                    (" ", Some(PALETTE.back)),
                    "({xx}, {yy})"
                );
            }
        }
    }
}

/// A panel too short for its parts (four rows: its frame and two inside) still runs the planner
/// at no cells: a typed letter sets the bar's verb, Esc asks since something changed; nothing of
/// the pane is painted, and a click anywhere moves nothing.
#[test]
fn a_short_panel_still_takes_the_planners_keys() {
    let mut fx = with_planner(1000.0, 80.0, plain(6));
    assert_eq!(grid_of(&fx).rows, 4);
    let frame: Vec<String> = (0..4).map(|y| line(&fx, y)).collect();
    fx.typed("d");
    assert_eq!(verb_of(&fx, 0), Verb::Drop, "the verb set");
    let now: Vec<String> = (0..4).map(|y| line(&fx, y)).collect();
    assert_eq!(now, frame, "nothing of the pane painted");
    for word in ["drop", "pick", "Verb", "Run"] {
        assert!(!now.concat().contains(word), "{word}: {now:?}");
    }
    let g = grid_of(&fx);
    for y in 0..g.rows {
        for x in (0..g.cols).step_by(4) {
            let p = centre(&fx, x, y);
            fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
        }
    }
    fx.idle();
    assert_eq!(bar_of(&fx), 0, "no click reached a row");
    assert!(!pane(&fx).dialog_up(), "no click reached a button");
    fx.press(Key::Escape);
    assert!(pane(&fx).dialog_up(), "Esc asks: the plan changed");
}

/// 10 §10, T38: in an ambiguous-wide terminal the hint's own `↑↓` and `·` are §10's stand-ins
/// at the source (`^v`, `.`) — the hint is cut to its row and leaves the printer no blanks to
/// move into; `⌘` is narrow and stays; a subject's `·` is content and stays.
#[test]
fn an_ambiguous_wide_terminal_gets_the_hints_stand_ins() {
    let mut m = plain(3);
    m.rows[0].commit.row.subject = "a·b".into();
    let mut fx = with_planner(1000.0, 500.0, m);
    let cmd = keys::Mods::Command.prefix();
    assert!(
        line(&fx, 26).contains(&format!("verb · A author and dates · {cmd}↑↓ move")),
        "the control: {:?}",
        line(&fx, 26)
    );
    cells::set_ambiguous_wide(&fx.drv.ctx, true);
    fx.idle();
    let hint = text(&fx, 26, 22, grid_of(&fx).cols - 1);
    assert!(
        hint.starts_with(&format!(
            "P R E S F D verb . A author and dates . {cmd}^v move . Enter message"
        )),
        "{hint:?}"
    );
    assert!(!hint.chars().any(crate::gui::term::ambiguous), "{hint:?}");
    assert_eq!(at(&fx, 2, "a·b"), Some(SUBJECT_AT), "a subject's · stays");
    cells::set_ambiguous_wide(&fx.drv.ctx, false);
}

/// A key that opens a dialog of the pane's ends the pane's keys for the pass: the rest of the
/// pass is the dialog's. `a` then ↓ in one pass: the author dialog of the row `a` was typed on,
/// the bar staying there; `r`, `x` and ↓: the reword, nothing, then the bar down.
#[test]
fn a_key_that_opens_a_dialog_leaves_the_rest_of_the_pass_to_it() {
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let mut events = vec![Event::Text("a".into())];
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(pane(&fx).dialog_up(), "the author dialog");
    let title = format!("{} ", short(7));
    let titled = (fx.drv.texts().into_iter())
        .any(|(_, _, t)| t.starts_with(&title) && t.ends_with(": author and dates"));
    assert!(titled, "row 0's: {:#?}", fx.drv.texts());
    assert_eq!(bar_of(&fx), 0, "the bar stays");
    fx.press(Key::Escape);
    assert!(!pane(&fx).dialog_up());
    let mut events = vec![Event::Text("r".into()), Event::Text("x".into())];
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        (verb_of(&fx, 0), verb_of(&fx, 1)),
        (Verb::Reword, Verb::Pick)
    );
    assert_eq!(bar_of(&fx), 1);
}

/// What a pass holds after a key that opens a dialog of the pane's is the dialog's, as if typed
/// once it was open (10 §17, X4-planner): on a reword row, Enter and a typed message in one pass
/// — the message dialog's field, whose caret egui puts at the end of the text, holds the
/// message with the typed text after it, as when the two come in two passes, and Enter keeps it;
/// `a` and `x`, as two texts or as one, put the `x` at the end of the author's name, its field
/// having the typing, and Enter keeps that name.
#[test]
fn text_typed_after_a_key_that_opens_a_dialog_reaches_it() {
    let message = format!(
        "{}\n\nbody of 7\n",
        "커밋 7: 한글 제목과 an English subject"
    );
    let typed = format!("{message}new message");
    let shown = |fx: &Fx, text: &str| (fx.drv.texts().into_iter()).any(|(_, _, t)| t == text);
    for split in [false, true] {
        let mut fx = with_planner(1000.0, 500.0, plain(6));
        fx.typed("r");
        assert_eq!(verb_of(&fx, 0), Verb::Reword);
        let text = vec![Event::Text("new message".into())];
        if split {
            fx.drv.frame(
                &mut fx.host,
                key_events(Key::Enter, Modifiers::NONE),
                Modifiers::NONE,
            );
            fx.drv.frame(&mut fx.host, text, Modifiers::NONE);
        } else {
            let mut events = key_events(Key::Enter, Modifiers::NONE);
            events.extend(text);
            fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
        }
        fx.idle();
        assert!(pane(&fx).dialog_up(), "split {split}: the message dialog");
        // The box's rows in order: under the field's label each line of the message a row.
        let titled = |l: &Arc<Layer>| {
            let a = l.buf.area;
            (a.top()..a.bottom()).any(|y| cells::row_text(&l.buf, y).0.contains(": its message"))
        };
        let boxed = (cells::layers(&fx.drv.ctx).into_iter())
            .find(titled)
            .expect("the message dialog's box");
        let a = boxed.buf.area;
        let rows: Vec<String> = (a.top()..a.bottom())
            .map(|y| cells::row_text(&boxed.buf, y).0)
            .collect();
        let mut from = rows
            .iter()
            .position(|r| r.contains("Message"))
            .expect("its label")
            + 1;
        for line in typed.lines().filter(|l| !l.is_empty()) {
            let k = rows[from..].iter().position(|r| r.contains(line));
            from += k.unwrap_or_else(|| panic!("split {split}: {line:?} in {rows:#?}")) + 1;
        }
        fx.press(Key::Enter);
        assert!(!pane(&fx).dialog_up(), "split {split}: Enter keeps it");
        let row = &pane(&fx).model().rows[0];
        assert_eq!(
            row.message.as_deref(),
            Some(typed.as_str()),
            "split {split}"
        );
    }
    // The rest of a text put back where it stood, before what came after it; a dialog-opening
    // letter after a Hangul one, and Hangul after it (`char_indices` on a multi-byte boundary).
    let text_event = |t: &str| Event::Text(t.into());
    for (events, name) in [
        (vec![text_event("a"), text_event("x")], "Ann Leex"),
        (vec![text_event("ax")], "Ann Leex"),
        (vec![text_event("ax"), text_event("y")], "Ann Leexy"),
        (vec![text_event("a한x")], "Ann Lee한x"),
        (vec![text_event("한ax")], "Ann Leex"),
    ] {
        let mut fx = with_planner(1000.0, 500.0, plain(6));
        fx.drv.frame(&mut fx.host, events.clone(), Modifiers::NONE);
        fx.idle();
        assert!(pane(&fx).dialog_up(), "{events:?}: the author dialog");
        assert!(shown(&fx, name), "{events:?}: {:#?}", fx.drv.texts());
        fx.press(Key::Enter);
        assert!(!pane(&fx).dialog_up(), "{events:?}");
        let author = pane(&fx).model().rows[0]
            .author
            .as_ref()
            .map(|a| a.name.clone());
        assert_eq!(author.as_deref(), Some(name), "{events:?}");
        assert_eq!(
            text(&fx, 2, MARK_AT, MARK_AT + 1),
            "A",
            "{events:?}: the mark"
        );
    }
}

/// Keys with modifiers and repeats in one pass, in their order: ↓ then Cmd/Ctrl+↓ moves the bar,
/// then its row; Shift+↓ is none of the pane's and moves nothing, the ↓ after it one row; a
/// press and two repeats of ↓ three rows.
#[test]
fn keys_with_modifiers_and_repeats_in_one_pass() {
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::COMMAND));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(bar_of(&fx), 2, "the bar with its row");
    assert_eq!(
        pane(&fx).model().rows[2].commit.row.oid,
        id(6),
        "row 1 moved down"
    );
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let mut events = key_events(Key::ArrowDown, Modifiers::SHIFT);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(bar_of(&fx), 1, "Shift+↓ is not the pane's");
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let down = |repeat| Event::Key {
        key: Key::ArrowDown,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers: Modifiers::NONE,
    };
    let events = vec![down(false), down(true), down(true)];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(bar_of(&fx), 3, "a press and two repeats");
}

/// The author and the date stand where the subject keeps 10 cells: in a pane of 67 cells (a grid
/// of 69 columns) the author at the pane's cell 33 and the date at 51, the subject's room 10; a
/// cell narrower, neither.
#[test]
fn the_author_and_the_date_need_ten_cells_of_subject() {
    let fx = with_planner(69.0 * 8.0, 500.0, every_verb());
    let heads = line(&fx, 1);
    assert_eq!(at(&fx, 1, "Author"), Some(1 + 33), "{heads:?}");
    assert_eq!(at(&fx, 1, "Date"), Some(1 + 51), "{heads:?}");
    let cut = cells::fit_start(&long_hangul(), 10);
    assert_eq!(at(&fx, 4, &cut), Some(SUBJECT_AT), "{:?}", line(&fx, 4));
    assert_eq!(text(&fx, 4, 32, 34), "  ", "two blanks before the author");
    let fx = with_planner(68.0 * 8.0, 500.0, every_verb());
    let heads = line(&fx, 1);
    assert!(
        !heads.contains("Author") && !heads.contains("Date"),
        "{heads:?}"
    );
}

/// A button stands, and is pressed, only where it fits whole: in a pane of 18 cells `[ Run… ]`
/// (8) is drawn and `[ Close ]` (9 more after two blanks) is not, and a click where it would
/// stand closes nothing; a click on `[ Run… ]` opens the run's confirmation.
#[test]
fn a_button_stands_only_where_it_fits_whole() {
    let mut fx = with_planner(20.0 * 8.0, 500.0, plain(6));
    let y = 26;
    let foot = line(&fx, y);
    assert_eq!(at(&fx, y, "[ Run… ]"), Some(1), "{foot:?}");
    assert!(!foot.contains("Close") && !foot.contains("[ C"), "{foot:?}");
    let p = centre(&fx, 1 + 10 + 4, y);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert!(fx.view().planner_model().is_some(), "nothing closed");
    assert!(!pane(&fx).dialog_up());
    let p = centre(&fx, 4, y);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert!(pane(&fx).dialog_up(), "the control: Run…");
}

/// A button that fits exactly is drawn and pressed: in a pane of 19 cells `[ Close ]` ends on
/// the pane's last cell, and a click on it closes the planner (nothing changed).
#[test]
fn a_button_that_fits_exactly_stands() {
    let mut fx = with_planner(21.0 * 8.0, 500.0, plain(6));
    let y = 26;
    assert_eq!(at(&fx, y, "[ Close ]"), Some(11), "{:?}", line(&fx, y));
    assert_eq!(cell(&fx, 20, y).symbol, "║", "the frame right after it");
    let p = centre(&fx, 15, y);
    fx.drv.frame(&mut fx.host, click_events(p), Modifiers::NONE);
    fx.idle();
    assert!(fx.view().planner_model().is_none(), "closed");
}

/// Enter, a typed `x` and Enter in one pass on a reword row: the message dialog opens, takes the
/// second Enter as its own and its field the `x` in the same pass, so the message kept is the
/// commit's with the `x` after it.
#[test]
fn enter_text_and_enter_in_one_pass_keep_the_text() {
    let message = format!(
        "{}\n\nbody of 7\n",
        "커밋 7: 한글 제목과 an English subject"
    );
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    fx.typed("r");
    let mut events = key_events(Key::Enter, Modifiers::NONE);
    events.push(Event::Text("x".into()));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(!pane(&fx).dialog_up(), "kept and closed");
    let kept = pane(&fx).model().rows[0].message.clone();
    assert_eq!(kept, Some(format!("{message}x")));
}

/// The pane's dialogs read the pass's keys in its order (X4-planner2): a dialog reads what came
/// after the key that opened it up to its first Enter or Esc, and once that key closed it what
/// follows is the pane's — Enter, Enter and `d`: the message kept as it was, then the row dropped;
/// Enter, Esc and `d`: nothing kept, the row dropped; `a`, Enter and `d`: the author kept as it was
/// (no mark), the row dropped. What came before the closing key is the dialog's and only its:
/// Enter, `d` and Enter keep the message with the `d`, the row still a reword.
#[test]
fn the_dialogs_read_their_keys_in_the_passs_order() {
    let message = format!(
        "{}\n\nbody of 7\n",
        "커밋 7: 한글 제목과 an English subject"
    );
    let pass = |fx: &mut Fx, events: Vec<Event>| {
        fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
        fx.idle();
    };
    let keyed = |key: Key| key_events(key, Modifiers::NONE);
    for (second, kept) in [(Key::Enter, Some(message.clone())), (Key::Escape, None)] {
        let mut fx = with_planner(1000.0, 500.0, plain(6));
        fx.typed("r");
        let mut events = keyed(Key::Enter);
        events.extend(keyed(second));
        events.push(Event::Text("d".into()));
        pass(&mut fx, events);
        assert!(!pane(&fx).dialog_up(), "{second:?}");
        assert_eq!(pane(&fx).model().rows[0].message, kept, "{second:?}");
        assert_eq!(verb_of(&fx, 0), Verb::Drop, "{second:?}: the pane's d");
    }
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    let mut events = vec![Event::Text("a".into())];
    events.extend(keyed(Key::Enter));
    events.push(Event::Text("d".into()));
    pass(&mut fx, events);
    assert!(!pane(&fx).dialog_up());
    assert_eq!(pane(&fx).model().rows[0].author, None, "kept as it was");
    assert_eq!(verb_of(&fx, 0), Verb::Drop, "the pane's d");
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    fx.typed("r");
    let mut events = keyed(Key::Enter);
    events.push(Event::Text("d".into()));
    events.extend(keyed(Key::Enter));
    pass(&mut fx, events);
    assert!(!pane(&fx).dialog_up());
    let kept = pane(&fx).model().rows[0].message.clone();
    assert_eq!(kept, Some(format!("{message}d")));
    assert_eq!(
        verb_of(&fx, 0),
        Verb::Reword,
        "the d was the message's alone"
    );
}

/// A dialog of the pane's opens with each field's caret at the end of its text, whatever caret
/// the same field had in the row edited before (egui keeps it by the field's id, one for every
/// row): the caret moved to the top of one row's message, that message kept, then on the next
/// row's a typed `x` goes at its end.
#[test]
fn a_dialog_opens_with_its_carets_at_the_end() {
    let message = |k: u32| format!("커밋 {k}: 한글 제목과 an English subject\n\nbody of {k}\n");
    let mut fx = with_planner(1000.0, 500.0, plain(6));
    fx.typed("r");
    fx.press(Key::Enter);
    assert!(pane(&fx).dialog_up());
    for _ in 0..4 {
        fx.press(Key::ArrowUp);
    }
    fx.press(Key::Enter);
    assert!(!pane(&fx).dialog_up());
    assert_eq!(
        pane(&fx).model().rows[0].message,
        Some(message(7)),
        "kept as it was"
    );
    fx.press(Key::ArrowDown);
    fx.typed("r");
    fx.press(Key::Enter);
    assert!(pane(&fx).dialog_up(), "row 1's message");
    fx.typed("x");
    fx.press(Key::Enter);
    let kept = pane(&fx).model().rows[1].message.clone();
    assert_eq!(kept, Some(format!("{}x", message(6))));
}

/// The pane's buttons never take the keyboard (F-git-ids): its keys are its own (Cmd/Ctrl+Enter
/// runs, Esc closes) and Tab is the git panel's way back to the list, so Tab moves egui's focus
/// to no row and no button — `[ Run… ]` and `[ Close ]` stay black on cyan however many come.
#[test]
fn tab_gives_no_button_the_focus() {
    let mut fx = with_planner(1000.0, 500.0, plain(3));
    let y = 26;
    let colours = |fx: &Fx, x: u16| {
        let c = cell(fx, x, y);
        (c.fg, c.bg)
    };
    let idle = (Some(PALETTE.dialog_text), Some(PALETTE.dialog_back));
    for tabs in 0..12 {
        assert_eq!(colours(&fx, 3), idle, "Run after {tabs} tabs");
        assert_eq!(colours(&fx, 13), idle, "Close after {tabs} tabs");
        assert_eq!(fx.drv.ctx.memory(|m| m.focused()), None, "{tabs} tabs");
        fx.press(Key::Tab);
    }
}

/// The S4 stand-in host (its fake seams): two ↓ in one pass move the bar two rows and leave no ↓
/// for the panel's own key lookup.
#[test]
fn two_keys_in_one_pass_leave_nothing_for_the_panel() {
    let mut fx = super::tests_s4::with_planner(super::tests_s4::model(), &super::tests_s4::repo());
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    let third = fx.pane().model().rows[2].commit.row.oid.clone();
    assert_eq!(fx.pane().bar(), Some(&third), "two rows");
    assert!(fx.host.leaked.is_empty(), "leaked: {:?}", fx.host.leaked);
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): the planner — every verb, the `A` mark, a Hangul subject — in both backends.

mod ts4 {
    use super::super::tests::Host;
    use super::*;
    use crate::gui::chrome::CELL;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
    use crate::gui::testkit::Driver;
    use crate::gui::Window;
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
    /// of its last pass (X4-graph's `ts4`).
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

    /// The fixture's panel with the planner open on [`every_verb`].
    fn host() -> Host {
        let mut view = fixture_view();
        view.set_repo(super::super::tests_s4::repo());
        view.open_planner(every_verb());
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    /// TS-4's planner: the terminal's receipt and the window's are the same bytes — at 100×30,
    /// and at 50×20, where the author and the date are dropped; every verb, the `A` mark and the
    /// Hangul subject among them.
    #[test]
    fn ts4_the_planner_is_the_same_cells_in_both_backends() {
        for (cols, rows) in [(100, 30), (50, 20)] {
            let mut tui = host();
            let (terminal, when) = terminal_run(&mut tui, cols, rows);
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = host();
            drv.idle(&mut win, 2);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "{cols}x{rows}");
            let marked = format!("reword  A  {}", short(7));
            for word in [
                "pick",
                "drop",
                "reword",
                "edit",
                "squash",
                "fixup",
                &marked,
                "한글",
                "[ Run… ]",
            ] {
                assert!(terminal.contains(word), "{cols}x{rows}: {word}\n{terminal}");
            }
            let wide = cols == 100;
            assert_eq!(terminal.contains("Grace"), wide, "{terminal}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The release frame cost (10 §17's X4 intro: each screen unit records its own).

/// The planner's release frame cost at 1000×700 — 300 rows with Hangul subjects and authors and
/// a verb of each kind, the pane over the panel's body — run alone: `cargo test --release --lib
/// --no-default-features -- --ignored --exact
/// gui::browser::git_panel::tests_x4_planner::frame_cost_at_1000_by_700 --nocapture`. Printed,
/// no budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    use std::time::Instant;
    let mut fx = with_planner(1000.0, 700.0, range(300));
    assert!(fx.view().planner_model().is_some(), "the planner is open");
    fx.drv.idle(&mut fx.host, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the planner's frame at 1000x700: {:?}",
        started.elapsed() / n
    );
}
