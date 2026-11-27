//! X4-find's tests (10 §17): Find's field in the heads' row and the history search's dialog in
//! cells (03 §4.7), through `ops::dialog`'s field and the kit's parts.

use super::tests::{commit, day, fixture_view, history, oid, refs, rows_of, Fx, Host};
use super::*;
use crate::gui::cells::{Layer, Position, Seen};
use crate::gui::chrome::{cga, git::LOOK, PALETTE};
use crate::gui::testkit::Driver;
use crate::gui::Window;
use eframe::egui::{Event, ImeEvent, Modifiers, PointerButton, Pos2, Rect};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Reading the cells.

/// The panel's grid: the one its host laid over the window's central area (the panel over all of
/// it, `side` unset).
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The layer of cells over `area` of the window's grid: the panel's (its side).
fn layer_over(ctx: &egui::Context, area: Cells) -> Arc<Layer> {
    cells::layers(ctx)
        .into_iter()
        .find(|l| l.buf.area == area)
        .unwrap_or_else(|| panic!("no layer over {area:?}"))
}

/// The panel's layer, over all of the grid.
fn panel(fx: &Fx) -> Arc<Layer> {
    layer_over(&fx.drv.ctx, grid_of(fx).all())
}

/// Row `y` of `l` from column `from` to `to` (exclusive, in the layer's grid), as text.
fn text(l: &Layer, y: u16, from: u16, to: u16) -> String {
    let a = l.buf.area;
    let row = &l.seen()[usize::from(y - a.y)];
    row[usize::from(from - a.x)..usize::from(to - a.x)]
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

/// Row `y` of `l`, whole.
fn line(l: &Layer, y: u16) -> String {
    let a = l.buf.area;
    text(l, y, a.x, a.right())
}

/// The cell at (`x`, `y`) of `l`.
fn cell(l: &Layer, x: u16, y: u16) -> Seen {
    let a = l.buf.area;
    l.seen()[usize::from(y - a.y)][usize::from(x - a.x)].clone()
}

/// The centre of cell (`x`, `y`) of `grid`, in points.
fn centre(grid: &Grid, x: u16, y: u16) -> Pos2 {
    grid.rect(Cells::new(x, y, 1, 1)).center()
}

/// A press and a release of the primary button at `p`, as one pass's events.
fn click_events(p: Pos2) -> Vec<Event> {
    let button = |pressed| Event::PointerButton {
        pos: p,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(p), button(true), button(false)]
}

/// A key's press and release with `mods`, as one pass's events.
fn key_events(key: Key, mods: Modifiers) -> Vec<Event> {
    [true, false]
        .map(|pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: mods,
        })
        .to_vec()
}

/// A press of Space as egui-winit sends it: the key and its text in one pass, then its release.
fn space_events() -> Vec<Event> {
    let mut events = key_events(Key::Space, Modifiers::NONE);
    events.insert(1, Event::Text(" ".into()));
    events
}

/// An input method's composition so far.
fn preedit(text: &str) -> Event {
    Event::Ime(ImeEvent::Preedit {
        text: text.into(),
        active_range_chars: None,
    })
}

/// The cells a terminal would show as hand-off boxes inside the grid (none: all is cells).
fn boxes(fx: &Fx) -> Vec<Cells> {
    let g = grid_of(fx);
    let layers = cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows);
    (screen.hand_offs.into_iter())
        .filter(|r| r.y < g.rows)
        .collect()
}

/// The fixture with a Hangul subject and a Hangul author: `x: 한글 하나` by 김철수.
fn hangul_view() -> GitView {
    let mut commits = history();
    commits[5].subject = "x: 한글 하나".into();
    commits[5].author_name = "김철수".into();
    let mut view = GitView::new("box:/srv/repo");
    view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
    view.set_rows(rows_of(commits, true), false, 2);
    view.set_paging(Paging::AllLoaded);
    view
}

/// The graph's row whose subject is `subject`.
fn index_of(fx: &Fx, subject: &str) -> usize {
    (0..fx.view().rows().len())
        .find(|&i| fx.view().line(i).is_some_and(|l| l.subject == subject))
        .unwrap_or_else(|| panic!("no row {subject:?}"))
}

/// Find's case toggle, while Find is open.
fn case(view: &GitView) -> Option<bool> {
    view.find.as_ref().map(|f| f.case)
}

// ---------------------------------------------------------------------------------------------
// Find's field in the heads' row.

/// The heads' row of a panel over all of the grid (no banner): the frame's second row.
const HEAD: u16 = 1;

/// A letter typed in the graph: Find's field in cells in the heads' row — `Find` dim, the field's
/// black cells with the letter in white from the sixth cell, the caret after it, `[ ] Aa` at the
/// right end — and no hand-off box anywhere (X4-graph left one there).
#[test]
fn a_typed_letter_opens_finds_field_in_cells() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    assert!(boxes(&fx).is_empty(), "the control: all cells");
    fx.typed("m");
    assert_eq!(fx.view().find_text(), Some("m"));
    let (l, g) = (panel(&fx), grid_of(&fx));
    assert_eq!(text(&l, HEAD, 1, 5), "Find", "{}", line(&l, HEAD));
    assert_eq!(
        (cell(&l, 1, HEAD).fg, cell(&l, 1, HEAD).bg),
        (Some(LOOK.dim), Some(PALETTE.back))
    );
    let m = cell(&l, 6, HEAD);
    assert_eq!(
        (m.symbol.as_str(), m.fg, m.bg),
        ("m", Some(PALETTE.field_text), Some(cga::BLACK)),
        "the field's first cell"
    );
    // The field runs to the blank before the toggle, black.
    let end = g.cols - 1 - 7;
    assert_eq!(cell(&l, end - 1, HEAD).bg, Some(cga::BLACK));
    assert_eq!(cell(&l, end, HEAD).bg, Some(PALETTE.back), "the blank");
    assert_eq!(text(&l, HEAD, end + 1, g.cols - 1), "[ ] Aa");
    assert_eq!(cell(&l, g.cols - 1, HEAD).symbol, "║", "the frame's side");
    assert_eq!(l.caret, Some(Position::new(7, HEAD)), "after the m");
    assert!(boxes(&fx).is_empty(), "no box where Find stands");
    const { assert!(find::PAINTS_CELLS) };
    // Esc closes it: the column heads again.
    fx.press(Key::Escape);
    let l = panel(&fx);
    assert!(line(&l, HEAD).contains("Description"), "{}", line(&l, HEAD));
    assert_eq!(l.caret, None);
}

/// Hangul typed into Find lands in its cells as typed — each syllable two cells, the caret after
/// them — and the bar jumps to the Hangul subject, then to the Hangul author.
#[test]
fn hangul_typed_into_find_lands_in_its_cells() {
    let mut fx = Fx::new(1000.0, 500.0, hangul_view());
    fx.key(Key::F, Modifiers::COMMAND);
    fx.typed("한글");
    let l = panel(&fx);
    let shown: Vec<String> = (0..4).map(|k| cell(&l, 6 + k, HEAD).symbol).collect();
    assert_eq!(shown, ["한", "", "글", ""], "{}", line(&l, HEAD));
    assert_eq!(
        cell(&l, 7, HEAD).bg,
        Some(cga::BLACK),
        "a wide glyph's second cell"
    );
    assert_eq!(l.caret, Some(Position::new(10, HEAD)));
    let hangul = index_of(&fx, "x: 한글 하나");
    assert_eq!(fx.view().bar_place().0, hangul, "the bar on the match");
    fx.press(Key::Escape);
    fx.press(Key::Home);
    fx.typed("김철수");
    assert_eq!(fx.view().find_text(), Some("김철수"));
    assert_eq!(fx.view().bar_place().0, hangul, "the Hangul author's row");
    assert_eq!(panel(&fx).caret, Some(Position::new(12, HEAD)));
}

/// An input method's composition lands in Find's cells as typed — a pass per step and several
/// steps in one pass, in order — the caret after it; the committed text is Find's, once.
#[test]
fn an_input_methods_composition_lands_in_finds_cells() {
    for one_pass in [false, true] {
        let mut fx = Fx::new(1000.0, 500.0, hangul_view());
        fx.key(Key::F, Modifiers::COMMAND);
        let steps = vec![preedit("ㅎ"), preedit("하"), preedit("한")];
        if one_pass {
            fx.drv.frame(&mut fx.host, steps, Modifiers::NONE);
        } else {
            for (step, shows) in steps.into_iter().zip(["ㅎ", "하", "한"]) {
                fx.drv.frame(&mut fx.host, vec![step], Modifiers::NONE);
                let l = panel(&fx);
                assert_eq!(cell(&l, 6, HEAD).symbol, shows, "{}", line(&l, HEAD));
                assert_eq!(l.caret, Some(Position::new(8, HEAD)), "after {shows}");
            }
        }
        let l = panel(&fx);
        assert_eq!(cell(&l, 6, HEAD).symbol, "한", "one pass: {one_pass}");
        assert_eq!(l.caret, Some(Position::new(8, HEAD)));
        // The syllable being composed is marked as egui's engine holds it, a selection: the
        // bar's colours (the kit's field), as a Korean input method marks its syllable.
        let composing = cell(&l, 6, HEAD);
        assert_eq!(
            (composing.fg, composing.bg),
            (Some(PALETTE.bar_text), Some(PALETTE.bar))
        );
        // The commit, and the next syllable's composition, in one pass.
        let events = vec![
            Event::Ime(ImeEvent::Commit("한".into())),
            preedit("ㄱ"),
            preedit("글"),
        ];
        fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
        let l = panel(&fx);
        assert_eq!(text(&l, HEAD, 6, 10), "한글", "{}", line(&l, HEAD));
        assert_eq!(l.caret, Some(Position::new(10, HEAD)));
        fx.drv.frame(
            &mut fx.host,
            vec![Event::Ime(ImeEvent::Commit("글".into()))],
            Modifiers::NONE,
        );
        fx.idle();
        assert_eq!(fx.view().find_text(), Some("한글"), "one pass: {one_pass}");
        let l = panel(&fx);
        assert_eq!(text(&l, HEAD, 6, 10), "한글");
        let typed = cell(&l, 8, HEAD);
        assert_eq!(
            (typed.fg, typed.bg),
            (Some(PALETTE.field_text), Some(cga::BLACK)),
            "committed: the field's own colours"
        );
        assert_eq!(
            fx.view().bar_place().0,
            index_of(&fx, "x: 한글 하나"),
            "the bar on the match"
        );
    }
}

/// Several letters in one pass go in order: the first opens Find, the rest are typed after it —
/// as typed text, a Hangul commit among them — and a Cmd/Ctrl+F in that pass opens it empty with
/// the rest typed into it; that pass's cells show it.
#[test]
fn several_letters_in_one_pass_go_into_find_in_order() {
    let mut fx = Fx::new(1000.0, 500.0, hangul_view());
    let events = vec![
        Event::Text("x".into()),
        Event::Text(":".into()),
        Event::Text(" ".into()),
        Event::Ime(ImeEvent::Commit("한".into())),
        Event::Text("글".into()),
    ];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("x: 한글"));
    assert_eq!(text(&panel(&fx), HEAD, 6, 13), "x: 한글", "in that pass");
    assert_eq!(fx.view().bar_place().0, index_of(&fx, "x: 한글 하나"));
    fx.press(Key::Escape);
    let mut events = key_events(Key::F, Modifiers::COMMAND);
    events.extend([Event::Text("y".into()), Event::Text(":".into())]);
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("y:"));
    assert_eq!(fx.view().bar_place().0, index_of(&fx, "y: one"));
}

/// The `Aa` toggle: its mark says it, never a colour — `[ ] Aa`, then `[X] Aa` after a click on
/// it, Find matching by case from then (03 §4.7); a click on the blank before it, on `Find` or on
/// the field turns nothing; Tab and Space never press it (it is the pointer's: Tab stays in the
/// field, which types the blank). What it does is the status row's (X4-find's decision).
#[test]
fn the_aa_toggle_is_its_mark_and_the_pointers() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("x: one");
    let g = grid_of(&fx);
    let toggle = g.cols - 1 - 6;
    assert_eq!(fx.view().bar_place().0, index_of(&fx, "x: one"));
    assert_eq!(fx.view().status_line().text, find::FIND_HINT);
    assert!(
        find::FIND_HINT.ends_with("Aa matches case"),
        "the hint says it"
    );
    // Tab, then Space, in one pass and in two: the field keeps the keyboard and types the blank.
    let mut events = key_events(Key::Tab, Modifiers::NONE);
    events.extend(space_events());
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    let tab = key_events(Key::Tab, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, tab, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, space_events(), Modifiers::NONE);
    fx.idle();
    assert_eq!(fx.view().find_text(), Some("x: one  "), "both blanks typed");
    assert_eq!(case(fx.view()), Some(false));
    assert_eq!(text(&panel(&fx), HEAD, toggle, g.cols - 1), "[ ] Aa");
    // Clicks beside it: the blank before it, the word, the field.
    for x in [toggle - 1, 2, 20] {
        let click = click_events(centre(&g, x, HEAD));
        fx.drv.frame(&mut fx.host, click, Modifiers::NONE);
        fx.idle();
        assert_eq!(case(fx.view()), Some(false), "x {x}");
    }
    // On it, its first and last cell: each click turns it.
    for x in [toggle, toggle + 5, toggle] {
        let before = case(fx.view());
        let click = click_events(centre(&g, x, HEAD));
        fx.drv.frame(&mut fx.host, click, Modifiers::NONE);
        fx.idle();
        assert_ne!(case(fx.view()), before, "x {x}");
    }
    let l = panel(&fx);
    assert_eq!(text(&l, HEAD, toggle, g.cols - 1), "[X] Aa");
    let on = cell(&l, toggle + 1, HEAD);
    assert_eq!(
        (on.fg, on.bg),
        (Some(PALETTE.text), Some(PALETTE.back)),
        "no colour of its own"
    );
    assert_eq!(
        fx.view().find_text(),
        Some("x: one  "),
        "the clicks typed nothing"
    );
    for _ in 0..8 {
        fx.press(Key::Backspace);
    }
    fx.typed("x: ONE");
    assert!(fx.shows("no match"), "case-sensitive: {:?}", fx.texts());
    assert_eq!(case(fx.view()), Some(true));
}

/// A click inside Find's field puts its caret there (the row's own rect under it takes no click
/// from it), and what is typed next lands there.
#[test]
fn a_click_in_finds_field_puts_its_caret_there() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("x: one");
    assert_eq!(panel(&fx).caret, Some(Position::new(12, HEAD)));
    let g = grid_of(&fx);
    // The left half of the `:`'s cell: the caret before it.
    let at = g.rect(Cells::new(7, HEAD, 1, 1)).left_center() + egui::vec2(2.0, 0.0);
    fx.drv
        .frame(&mut fx.host, click_events(at), Modifiers::NONE);
    fx.idle();
    assert_eq!(
        panel(&fx).caret,
        Some(Position::new(7, HEAD)),
        "between x and :"
    );
    fx.typed("y");
    assert_eq!(fx.view().find_text(), Some("xy: one"));
}

/// A narrow heads' row gives up `Find` first and then the toggle, the field keeping eight cells
/// while either stands; at the last the field is the whole row (03 §4.2's narrow side, 12's
/// 24-cell Git tab). Typing and the caret follow the field wherever it starts.
#[test]
fn a_narrow_row_gives_up_the_word_then_the_toggle() {
    let rows = [
        (20, true, true, 8),
        (19, false, true, 12),
        (15, false, true, 8),
        (14, false, false, 14),
    ];
    for (inside, word, toggle, width) in rows {
        let mut fx = Fx::new(1000.0, 500.0, fixture_view());
        let side = Cells::new(40, 0, inside + 2, grid_of(&fx).rows);
        fx.host.side = Some(side);
        fx.idle();
        fx.typed("y");
        let l = layer_over(&fx.drv.ctx, side);
        let row = line(&l, HEAD);
        assert_eq!(row.contains("Find"), word, "{inside}: {row:?}");
        assert_eq!(row.contains("[ ] Aa"), toggle, "{inside}: {row:?}");
        let first = if word { 46 } else { 41 };
        assert_eq!(cell(&l, first, HEAD).symbol, "y", "{inside}: {row:?}");
        assert_eq!(cell(&l, first, HEAD).bg, Some(cga::BLACK));
        assert_eq!(cell(&l, first + width - 1, HEAD).bg, Some(cga::BLACK));
        assert_ne!(
            cell(&l, first + width, HEAD).bg,
            Some(cga::BLACK),
            "{inside}"
        );
        assert_eq!(l.caret, Some(Position::new(first + 1, HEAD)), "{inside}");
        let placed = find::Placed::of(Cells::new(41, HEAD, inside, 1));
        assert_eq!(placed.field, Cells::new(first, HEAD, width, 1), "{inside}");
    }
}

/// Two panels side by side in one `ui` (a pin beside another repository's panel): each Find's
/// field and toggle are its own — a click on the left toggle turns the left panel's case only,
/// then one on the right the right's (with one id the click went to the panel laid last) — each
/// field shows its own text, and each keeps its own caret as the keyboard goes from one to the
/// other (with one field id the right would type at the left's caret).
#[test]
fn two_panels_finds_are_their_own() {
    let mut drv = Driver::new(1000.0, 500.0);
    let mut pair = Pair::new();
    drv.idle(&mut pair, 1);
    pair.views[0].run(Command::FindWith('x'));
    pair.views[1].run(Command::FindWith('y'));
    drv.idle(&mut pair, 2);
    let g = pair.grid;
    let [left, right] = cells::halves(g.all());
    let toggle = |side: Cells| centre(&g, side.right() - 1 - 6, HEAD);
    let cases = |pair: &Pair| pair.views.each_ref().map(case);
    drv.frame(&mut pair, click_events(toggle(left)), Modifiers::NONE);
    drv.idle(&mut pair, 1);
    assert_eq!(cases(&pair), [Some(true), Some(false)], "the left one");
    drv.frame(&mut pair, click_events(toggle(right)), Modifiers::NONE);
    drv.idle(&mut pair, 1);
    assert_eq!(cases(&pair), [Some(true), Some(true)], "then the right one");
    let shows = |side: Cells| {
        let l = layer_over(&drv.ctx, side);
        (cell(&l, side.x + 6, HEAD).symbol, line(&l, HEAD))
    };
    assert_eq!(shows(left).0, "x", "{}", shows(left).1);
    assert_eq!(shows(right).0, "y", "{}", shows(right).1);
    assert!(shows(left).1.contains("[X] Aa") && shows(right).1.contains("[X] Aa"));
    // Each field's caret is its own (review round 1): the left's moved, the right's at its end;
    // with one id for both the right would type where the left's caret stands.
    for (view, text) in pair.views.iter_mut().zip(["abcd", "wxyz"]) {
        let find = view.find.as_mut().unwrap();
        (find.text, find.fresh) = (text.into(), true);
    }
    pair.actives = [true, false];
    drv.idle(&mut pair, 1);
    for _ in 0..3 {
        drv.key(&mut pair, Key::ArrowLeft, Modifiers::NONE);
    }
    pair.actives = [false, true];
    drv.idle(&mut pair, 2);
    drv.frame(&mut pair, vec![Event::Text("!".into())], Modifiers::NONE);
    pair.actives = [true, false];
    drv.idle(&mut pair, 2);
    drv.frame(&mut pair, vec![Event::Text("?".into())], Modifiers::NONE);
    drv.idle(&mut pair, 1);
    let texts = pair
        .views
        .each_ref()
        .map(|v| v.find_text().map(str::to_string));
    assert_eq!(texts, [Some("a?bcd".into()), Some("wxyz!".into())]);
}

/// Two git panels side by side, as the browser lays a pin beside another repository's panel, each
/// active or not (the one with the keyboard).
struct Pair {
    views: [GitView; 2],
    actives: [bool; 2],
    grid: Grid,
}

impl Pair {
    fn new() -> Self {
        Pair {
            views: [fixture_view(), fixture_view()],
            actives: [true, true],
            grid: Grid::over(Rect::NOTHING),
        }
    }
}

impl Window for Pair {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.grid = Grid::over(ui.max_rect());
                let sides = cells::halves(self.grid.all());
                for ((view, side), active) in self.views.iter_mut().zip(sides).zip(self.actives) {
                    draw(ui, &self.grid, side, view, active, &[]);
                }
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// A panel too short for its parts runs Find's field on no cells, in a child `ui` of its own: its
/// id is the panel's own key, not that `ui`'s scope, so its caret and text stay as they were
/// across the resize — a letter typed there lands where the caret was — and the field comes back
/// with them.
#[test]
fn finds_field_keeps_its_caret_across_a_short_panel() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("ab");
    fx.press(Key::ArrowLeft);
    assert_eq!(
        panel(&fx).caret,
        Some(Position::new(7, HEAD)),
        "between a and b"
    );
    fx.drv.resize(1000.0, 16.0 + 3.0 * 16.0);
    fx.idle();
    fx.idle();
    assert!(fx.view().geometry().is_none(), "too short for the graph");
    fx.typed("x");
    assert_eq!(fx.view().find_text(), Some("axb"), "typed at the caret");
    fx.drv.resize(1000.0, 500.0);
    fx.idle();
    fx.idle();
    let l = panel(&fx);
    assert_eq!(text(&l, HEAD, 6, 9), "axb");
    assert_eq!(l.caret, Some(Position::new(8, HEAD)), "after the x");
}

/// The key help over Find's field: the panel blanks its cells and gives no caret there, so a
/// terminal's cursor is never on the help.
#[test]
fn the_key_help_over_finds_field_hides_its_caret() {
    let mut fx = Fx::new(1000.0, 120.0, fixture_view());
    fx.typed(&"x".repeat(30));
    let caret = Position::new(36, HEAD);
    assert_eq!(panel(&fx).caret, Some(caret), "the control");
    let page = crate::gui::help::for_context(keys::Context::Git, Mode::Preview, &[]);
    crate::gui::help::open(&fx.drv.ctx, page);
    fx.idle();
    let covered = crate::gui::help::covered(&fx.drv.ctx, &grid_of(&fx));
    assert!(
        covered.iter().any(|r| r.contains(caret)),
        "the help stands over the caret's cell: {covered:?}"
    );
    assert_eq!(panel(&fx).caret, None);
    assert_eq!(
        text(&panel(&fx), HEAD, 30, 40).trim(),
        "",
        "blanked under it"
    );
}

// ---------------------------------------------------------------------------------------------
// The history search's dialog.

/// The dialog's layer: the one titled `History search`.
fn search_layer(fx: &Fx) -> Arc<Layer> {
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(|l| {
            let a = l.buf.area;
            (a.top()..a.bottom()).any(|y| line(l, y).contains("History search"))
        })
        .expect("the history search's cells")
}

/// The dialog's rows as text, top to bottom, the shadow's cells cut off.
fn search_rows(fx: &Fx) -> Vec<String> {
    let l = search_layer(fx);
    let a = l.buf.area;
    (a.top()..a.bottom() - 1)
        .map(|y| text(&l, y, a.x, a.right() - 2))
        .collect()
}

/// Where `what` first stands in the dialog: its first cell's column and its row.
fn search_at(fx: &Fx, what: &str) -> (u16, u16) {
    let l = search_layer(fx);
    let a = l.buf.area;
    for y in a.top()..a.bottom() {
        let (s, at) = cells::row_text(&l.buf, y);
        if let Some(k) = s.find(what) {
            return (at[s[..k].chars().count()], y);
        }
    }
    panic!("{what:?} is not in the dialog: {:#?}", search_rows(fx))
}

/// The panel with the history search open (Cmd/Ctrl+Shift+F) and settled.
fn with_search(w: f32, h: f32) -> Fx {
    let mut fx = Fx::new(w, h, hangul_view());
    fx.key(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    fx.idle();
    fx
}

/// The query the open dialog holds.
fn query(fx: &Fx) -> SearchQuery {
    fx.view()
        .search_dialog
        .as_ref()
        .expect("the dialog")
        .query
        .clone()
}

/// Cmd/Ctrl+Shift+F: the history search's dialog in cells — its frame titled `History search`,
/// the six fields under labels of one width, black cells, `[ ] Match case`, the buttons and the
/// note — over the panel, which blanks the cells under the box and its shadow (`covers`); no
/// hand-off box anywhere; the keyboard on Message, the caret in its first cell.
#[test]
fn the_history_search_is_cells() {
    let fx = with_search(1000.0, 500.0);
    let rows = search_rows(&fx);
    assert!(
        rows[0].starts_with('╔') && rows[0].contains(" History search "),
        "{rows:#?}"
    );
    let labels = [
        "Message              ",
        "Author               ",
        "Text added or removed",
        "Changed path         ",
        "Since                ",
        "Until                ",
    ];
    let (x0, y0) = search_at(&fx, "Message");
    let l = search_layer(&fx);
    for (k, label) in labels.iter().enumerate() {
        let y = y0 + k as u16;
        assert_eq!(text(&l, y, x0, x0 + 21), *label, "{rows:#?}");
        let first = cell(&l, x0 + 22, y);
        assert_eq!(
            (first.symbol.as_str(), first.bg),
            (" ", Some(cga::BLACK)),
            "{label}"
        );
    }
    assert_eq!(search_at(&fx, "[ ] Match case"), (x0, y0 + 6));
    assert_eq!(search_at(&fx, "[ Search ]  [ Cancel ]"), (x0, y0 + 8));
    assert_eq!(search_at(&fx, "Enter searches · Esc cancels"), (x0, y0 + 9));
    assert_eq!(l.caret, Some(Position::new(x0 + 22, y0)), "on Message");
    let covers = fx.view().covered(&grid_of(&fx));
    assert_eq!(covers.len(), 3, "the box and its shadow: {covers:?}");
    assert_eq!(covers[0].union(covers[1]).union(covers[2]), l.buf.area);
    let p = panel(&fx);
    for r in &covers {
        for y in r.top()..r.bottom() {
            let under = text(&p, y, r.x, r.right());
            assert!(
                under.trim().is_empty(),
                "blank under it, row {y}: {under:?}"
            );
        }
    }
    assert!(boxes(&fx).is_empty(), "no box: {:?}", boxes(&fx));
}

/// The dialog filled through its fields — Hangul typed in Message and Author, Tab between them,
/// Space on Match case — and Enter searches for exactly that.
#[test]
fn the_history_search_takes_what_is_typed_and_enter_searches() {
    let mut fx = with_search(1000.0, 500.0);
    fx.typed("한글");
    let (x0, y0) = search_at(&fx, "Message");
    let l = search_layer(&fx);
    assert_eq!(
        text(&l, y0, x0 + 22, x0 + 26),
        "한글",
        "{:#?}",
        search_rows(&fx)
    );
    assert_eq!(l.caret, Some(Position::new(x0 + 26, y0)));
    fx.press(Key::Tab);
    fx.typed("김철수");
    for _ in 0..4 {
        fx.press(Key::Tab);
    }
    fx.typed("2026-11-01");
    fx.press(Key::Tab);
    fx.drv.frame(&mut fx.host, space_events(), Modifiers::NONE);
    fx.idle();
    assert_eq!(
        search_at(&fx, "[X] Match case").1,
        y0 + 6,
        "turned by Space"
    );
    let rows = search_rows(&fx);
    assert!(rows.iter().any(|r| r.contains("김철수")), "{rows:#?}");
    let asked = fx.press(Key::Enter);
    let wanted = SearchQuery {
        message: "한글".into(),
        author: "김철수".into(),
        until: "2026-11-01".into(),
        case: true,
        ..Default::default()
    };
    assert_eq!(asked, [GitAction::Search(wanted)]);
    assert!(fx.view().search_dialog.is_none(), "closed");
}

/// The pass that opens the dialog: what is typed after Cmd/Ctrl+Shift+F in it lands in Message,
/// in order, though egui lays the new box out unseen in that pass; an Enter there runs nothing
/// (no box was seen yet).
#[test]
fn what_is_typed_in_the_pass_that_opens_the_search_lands_in_it() {
    let mut fx = Fx::new(1000.0, 500.0, hangul_view());
    let mut events = key_events(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    events.extend([
        Event::Text("ab".into()),
        Event::Ime(ImeEvent::Commit("한".into())),
        Event::Text("c".into()),
    ]);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(query(&fx).message, "ab한c");
    assert!(fx.take().is_empty(), "the Enter ran nothing");
    assert!(fx.view().search_dialog.is_some());
}

/// Only what the screen showed runs: a letter and Enter in one pass search for nothing and say
/// the search changed; the next Enter searches for what then shows. The first pass the box is
/// seen in takes no Enter either.
#[test]
fn an_enter_runs_only_the_search_on_screen() {
    let mut fx = with_search(1000.0, 500.0);
    fx.typed("one");
    let mut events = vec![Event::Text("x".into())];
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(fx.take().is_empty(), "nothing ran");
    assert_eq!(query(&fx).message, "onex");
    let (x, y) = search_at(&fx, "the search changed: Enter searches for it");
    assert_eq!(cell(&search_layer(&fx), x, y).fg, Some(PALETTE.dialog_text));
    let asked = fx.press(Key::Enter);
    let wanted = SearchQuery {
        message: "onex".into(),
        ..Default::default()
    };
    assert_eq!(asked, [GitAction::Search(wanted)]);
    // The first pass seen: the box laid out unseen in the pass before.
    let mut fx = Fx::new(1000.0, 500.0, hangul_view());
    fx.view_mut().run(Command::HistorySearch);
    fx.view_mut().search_dialog.as_mut().unwrap().query.message = "one".into();
    fx.drv.frame(&mut fx.host, vec![], Modifiers::NONE);
    let enter = || key_events(Key::Enter, Modifiers::NONE);
    fx.drv.frame(&mut fx.host, enter(), Modifiers::NONE);
    assert!(fx.take().is_empty(), "the first pass seen takes no Enter");
    fx.drv.frame(&mut fx.host, enter(), Modifiers::NONE);
    assert_eq!(fx.take().len(), 1, "the next one searches");
}

/// Enter with every field empty says so in the error colour, the box where it was; Esc, Cancel
/// (clicked, or Enter on it) and a click outside cancel.
#[test]
fn an_empty_search_is_refused_and_esc_cancel_and_outside_cancel() {
    let mut fx = with_search(1000.0, 500.0);
    let before = fx.view().covered(&grid_of(&fx));
    assert!(fx.press(Key::Enter).is_empty());
    let (x, y) = search_at(&fx, "type what to search for");
    assert_eq!(
        cell(&search_layer(&fx), x, y).fg,
        Some(PALETTE.dialog_error)
    );
    assert_eq!(fx.view().covered(&grid_of(&fx)), before, "the same box");
    assert!(fx.view().search_dialog.is_some());
    fx.press(Key::Escape);
    assert!(fx.view().search_dialog.is_none(), "Esc");
    for how in ["click", "enter", "outside"] {
        let mut fx = with_search(1000.0, 500.0);
        let g = grid_of(&fx);
        fx.typed("one");
        let mut asked = Vec::new();
        match how {
            "click" => {
                let (x, y) = search_at(&fx, "[ Cancel ]");
                fx.drv.click(&mut fx.host, centre(&g, x + 2, y));
            }
            "enter" => {
                for _ in 0..8 {
                    asked.extend(fx.press(Key::Tab));
                }
                assert_eq!(
                    fx.drv.ctx.memory(|m| m.focused()),
                    Some(find::cancel_button_id()),
                    "on Cancel"
                );
                asked.extend(fx.press(Key::Enter));
            }
            _ => fx.drv.click(&mut fx.host, centre(&g, 1, 1)),
        }
        fx.idle();
        asked.extend(fx.take());
        assert!(asked.is_empty(), "{how}: no search: {asked:?}");
        assert!(fx.view().search_dialog.is_none(), "{how}: cancelled");
    }
}

/// Presses in one pass, in order: `[Tab, Space]` from Message types the blank there and nowhere
/// else (the field read the whole pass); on Match case `[Space, Tab]` turns it once and presses
/// no Search; an Enter after a Shift+Tab in the same pass is dropped (egui moves the keyboard
/// later), a Space with it; `[Space, Tab]` on Search searches, never pressing the Cancel the Tab
/// reaches (the buttons take the kit's presses, F-kit-tab).
#[test]
fn presses_in_one_pass_go_where_the_keyboard_was() {
    let mut fx = with_search(1000.0, 500.0);
    fx.typed("one");
    let mut events = key_events(Key::Tab, Modifiers::NONE);
    events.extend(space_events());
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        (query(&fx).message.as_str(), query(&fx).author.as_str()),
        ("one ", ""),
        "the field read the blank"
    );
    assert!(!query(&fx).case);
    for _ in 0..5 {
        fx.press(Key::Tab);
    }
    // Now on Match case.
    let mut events = space_events();
    events.extend(key_events(Key::Tab, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(query(&fx).case, "turned once");
    assert!(fx.take().is_empty(), "no search");
    // On Search: Shift+Tab then Enter and Space in one pass — all dropped.
    let mut events = key_events(Key::Tab, Modifiers::SHIFT);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    events.extend(space_events());
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(fx.take().is_empty(), "dropped");
    assert!(query(&fx).case, "the Space did not reach Match case");
    // Back on Search: Space, then Tab to Cancel, in one pass — Search's.
    fx.press(Key::Tab);
    let mut events = space_events();
    events.extend(key_events(Key::Tab, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    let wanted = SearchQuery {
        message: "one ".into(),
        case: true,
        ..Default::default()
    };
    assert_eq!(fx.take(), [GitAction::Search(wanted)]);
}

/// A search run from the dialog shows its results in place of the graph, in cells (X4-graph's
/// flat list), and Esc goes back to the graph on the result's commit (03 §4.7).
#[test]
fn a_search_from_the_dialog_shows_its_results_in_cells() {
    let mut fx = with_search(1000.0, 500.0);
    fx.typed("prepare");
    let asked = fx.press(Key::Enter);
    let wanted = SearchQuery {
        message: "prepare".into(),
        ..Default::default()
    };
    assert_eq!(asked, [GitAction::Search(wanted.clone())]);
    fx.view_mut().set_search_results(
        &wanted,
        vec![commit(5, &[1], "Ann", day(5), "main: prepare to merge")],
        Paging::AllLoaded,
    );
    fx.idle();
    let l = panel(&fx);
    assert!(
        line(&l, 2).contains("main: prepare to merge"),
        "{}",
        line(&l, 2)
    );
    assert!(boxes(&fx).is_empty());
    fx.press(Key::Escape);
    assert_eq!(fx.bar(), RowKey::Commit(oid(5)));
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// Only the active panel's Find types (MUST 1): two panels side by side, both with Find open, the
/// left with the keyboard — a letter typed lands in the left's field alone, and only the left
/// shows a caret (one cursor in a terminal); an inactive panel's field gives the keyboard up.
#[test]
fn only_the_active_panels_find_types() {
    let mut drv = Driver::new(1000.0, 500.0);
    let mut pair = Pair::new();
    pair.actives = [true, false];
    drv.idle(&mut pair, 1);
    pair.views[0].run(Command::FindWith('x'));
    pair.views[1].run(Command::FindWith('y'));
    drv.idle(&mut pair, 2);
    drv.frame(&mut pair, vec![Event::Text("z".into())], Modifiers::NONE);
    drv.idle(&mut pair, 1);
    let texts = pair
        .views
        .each_ref()
        .map(|v| v.find_text().map(str::to_string));
    assert_eq!(texts, [Some("xz".into()), Some("y".into())]);
    let [left, right] = cells::halves(pair.grid.all());
    let carets = [left, right].map(|side| layer_over(&drv.ctx, side).caret);
    assert!(carets[0].is_some() && carets[1].is_none(), "{carets:?}");
    // The keyboard goes to the other side: its Find takes the typing, the left's none.
    pair.actives = [false, true];
    drv.idle(&mut pair, 2);
    drv.frame(&mut pair, vec![Event::Text("w".into())], Modifiers::NONE);
    drv.idle(&mut pair, 1);
    let texts = pair
        .views
        .each_ref()
        .map(|v| v.find_text().map(str::to_string));
    assert_eq!(texts, [Some("xz".into()), Some("yw".into())]);
    // The keyboard elsewhere (the list's): neither Find keeps it, neither types.
    pair.actives = [false, false];
    drv.idle(&mut pair, 2);
    assert_eq!(drv.ctx.memory(|m| m.focused()), None, "given up");
    drv.frame(&mut pair, vec![Event::Text("q".into())], Modifiers::NONE);
    drv.idle(&mut pair, 1);
    let texts = pair
        .views
        .each_ref()
        .map(|v| v.find_text().map(str::to_string));
    assert_eq!(
        texts,
        [Some("xz".into()), Some("yw".into())],
        "nobody typed"
    );
}

/// Find's field is the panel's own whatever column it stands in (MUST 2): with the caret between
/// two letters, the panel's column moves (the right side as the window's width changes) and a
/// letter typed lands at the caret; an input method's composition goes on across the move.
#[test]
fn finds_field_keeps_its_state_when_its_column_moves() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.host.side = Some(Cells::new(40, 0, 60, grid_of(&fx).rows));
    fx.idle();
    fx.typed("ab");
    fx.press(Key::ArrowLeft);
    fx.drv
        .frame(&mut fx.host, vec![preedit("하")], Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("a하b"));
    fx.host.side = Some(Cells::new(43, 0, 60, grid_of(&fx).rows));
    fx.idle();
    let events = vec![preedit("한"), Event::Ime(ImeEvent::Commit("한".into()))];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        fx.view().find_text(),
        Some("a한b"),
        "the composition went on"
    );
    fx.typed("x");
    assert_eq!(fx.view().find_text(), Some("a한xb"), "typed at the caret");
}

/// A blank typed after a Shift+Tab in the same pass, in a field, stays typed (MUST 3): the field
/// read the whole pass, and the kit keeps the control the keyboard goes to from taking it.
#[test]
fn a_blank_after_a_shift_tab_in_a_field_is_typed() {
    let mut fx = with_search(1000.0, 500.0);
    fx.press(Key::Tab);
    assert_eq!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(find::search_field_id(1)),
        "on Author"
    );
    let mut events = key_events(Key::Tab, Modifiers::SHIFT);
    events.push(Event::Text("a".into()));
    events.extend(space_events());
    events.push(Event::Text("b".into()));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(query(&fx).author, "a b");
}

/// A field that is not on screen is never searched (MUST 4): on a grid too short for the box, a
/// filled Until below its foot keeps Search off and says so at the box's top; Enter searches
/// nothing. A taller grid searches it.
#[test]
fn a_field_off_the_grid_is_never_searched() {
    let mut fx = with_search(1000.0, 16.0 + 7.0 * 16.0);
    fx.view_mut().search_dialog.as_mut().unwrap().query.until = "2026-11-01".into();
    fx.typed("one");
    fx.idle();
    let asked = fx.press(Key::Enter);
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.view().search_dialog.is_some());
    let (x, y) = search_at(&fx, find::TOO_SMALL);
    assert_eq!(
        cell(&search_layer(&fx), x, y).fg,
        Some(PALETTE.dialog_error)
    );
    assert!(
        !search_rows(&fx).iter().any(|r| r.contains("Until")),
        "Until is off the grid: {:#?}",
        search_rows(&fx)
    );
    // Taller: every field on screen, the search runs.
    fx.drv.resize(1000.0, 500.0);
    fx.idle();
    fx.idle();
    let asked = fx.press(Key::Enter);
    let wanted = SearchQuery {
        message: "one".into(),
        until: "2026-11-01".into(),
        ..Default::default()
    };
    assert_eq!(asked, [GitAction::Search(wanted)]);
}

/// Two moves of the keyboard in one pass (MUST 5): egui moves it once, so an Enter or a Space
/// after the second is dropped — `[Tab, Tab, Enter]` from Match case neither searches nor
/// cancels.
#[test]
fn presses_after_a_second_move_in_one_pass_are_dropped() {
    let mut fx = with_search(1000.0, 500.0);
    fx.typed("one");
    for _ in 0..6 {
        fx.press(Key::Tab);
    }
    assert_eq!(
        fx.drv.ctx.memory(|m| m.focused()),
        Some(find::match_case_id()),
        "on Match case"
    );
    for last in [Key::Enter, Key::Space] {
        let mut events = key_events(Key::Tab, Modifiers::NONE);
        events.extend(key_events(Key::Tab, Modifiers::NONE));
        events.extend(key_events(last, Modifiers::NONE));
        fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
        fx.idle();
        assert!(fx.take().is_empty(), "{last:?}: nothing pressed");
        assert!(fx.view().search_dialog.is_some(), "{last:?}: not cancelled");
        assert!(!query(&fx).case);
        fx.drv
            .ctx
            .memory_mut(|m| m.request_focus(find::match_case_id()));
        fx.idle();
    }
}

/// Find's Enter acts after what the pass typed before it (item 6): `[merge, Enter]` in one pass
/// goes to the first match of `merge`, then the next; Shift+Enter after typing, the previous.
#[test]
fn finds_enter_acts_after_what_was_typed() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.key(Key::F, Modifiers::COMMAND);
    let mut events = vec![Event::Text("merge".into())];
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("merge"));
    assert_eq!(
        fx.bar(),
        RowKey::Commit(oid(7)),
        "the first match, then the next"
    );
    fx.idle();
    assert!(fx.shows("2 of 3"));
    // Shift+Enter after typing: the previous.
    let mut events = vec![Event::Text("".into())];
    events.extend(key_events(Key::Enter, Modifiers::SHIFT));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.bar(), RowKey::Commit(oid(9)));
    fx.press(Key::Escape);
    // Find closed, `[m, Enter]`: the m opens it, the bar jumps to its first match, then the next.
    fx.press(Key::Home);
    let mut events = vec![Event::Text("m".into())];
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("m"));
    // The first match of `m` is the merge (9), the next the octopus merge (7).
    assert_eq!(
        fx.bar(),
        RowKey::Commit(oid(7)),
        "the match after the first"
    );
}

/// Find's keys are the keyboard's only (round 1): with a dialog of the panel's up — the history
/// search over an open Find — Enter searches and Esc cancels the dialog, Find's bar staying; and
/// an inactive panel's open Find never steps on the pass's Enter.
#[test]
fn finds_keys_are_the_keyboards_only() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("x");
    let bar = fx.view().bar_place().0;
    fx.view_mut().run(Command::HistorySearch);
    fx.idle();
    fx.idle();
    fx.typed("one");
    let asked = fx.press(Key::Enter);
    assert!(
        matches!(&asked[..], [GitAction::Search(q)] if q.message == "one"),
        "{asked:?}"
    );
    assert_eq!(fx.view().bar_place().0, bar, "Find did not step");
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.typed("x");
    fx.view_mut().run(Command::HistorySearch);
    fx.idle();
    fx.idle();
    fx.press(Key::Escape);
    assert!(
        fx.view().search_dialog.is_none(),
        "Esc cancelled the dialog"
    );
    assert_eq!(fx.view().find_text(), Some("x"), "and left Find open");
    // Two panels, both Finds on `m`: the Enter steps the active one's alone.
    let mut drv = Driver::new(1000.0, 500.0);
    let mut pair = Pair::new();
    pair.actives = [true, false];
    drv.idle(&mut pair, 1);
    for view in &mut pair.views {
        view.run(Command::FindWith('m'));
    }
    drv.idle(&mut pair, 2);
    let bars = |pair: &Pair| pair.views.each_ref().map(|v| v.bar_place().0);
    let before = bars(&pair);
    assert_eq!(before[0], before[1], "both on the first match");
    drv.frame(
        &mut pair,
        key_events(Key::Enter, Modifiers::NONE),
        Modifiers::NONE,
    );
    drv.idle(&mut pair, 1);
    let after = bars(&pair);
    assert!(after[0] > before[0], "the active one stepped: {after:?}");
    assert_eq!(after[1], before[1], "the inactive one did not");
}

/// Find's Esc acts after what the pass typed before it (item 6): `[x, Esc]` — Find open, then
/// closed — types the x (or opens Find with it), the bar jumping to its first match, then closes
/// Find, never opening it again with the x, and that pass's cells show the heads; a Down after
/// the Esc moves the bar; `[Esc, y]` closes it, then the y opens it anew.
#[test]
fn an_esc_after_typing_closes_find_for_good() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    for open in [true, false] {
        fx.press(Key::Home);
        if open {
            fx.key(Key::F, Modifiers::COMMAND);
        }
        let mut events = vec![Event::Text("x".into())];
        events.extend(key_events(Key::Escape, Modifiers::NONE));
        fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
        assert_eq!(fx.view().find_text(), None, "open {open}: closed");
        let bar = fx.view().bar_place().0;
        assert_eq!(
            fx.view().line(bar).unwrap().subject,
            "Octopus merge of x and y",
            "open {open}: the x's first match"
        );
        let l = panel(&fx);
        assert!(
            line(&l, HEAD).contains("Description"),
            "open {open}, in that pass: {}",
            line(&l, HEAD)
        );
        assert_eq!(l.caret, None);
    }
    // `[x, Esc, ↓]`: what comes after the Esc is the graph's — the bar a row below the match.
    fx.press(Key::Home);
    fx.key(Key::F, Modifiers::COMMAND);
    let mut events = vec![Event::Text("x".into())];
    events.extend(key_events(Key::Escape, Modifiers::NONE));
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), None);
    let octopus = index_of(&fx, "Octopus merge of x and y");
    assert_eq!(
        fx.view().bar_place().0,
        octopus + 1,
        "the Down after the Esc"
    );
    // `[Esc, y]` with Find open: closed, then opened again by the y.
    fx.typed("w");
    let mut events = key_events(Key::Escape, Modifiers::NONE);
    events.push(Event::Text("y".into()));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("y"));
}

/// A key the graph takes takes its text with it (item 7): with Find closed, Space then a letter
/// in one pass marks the bar's row and opens Find with the letter alone.
#[test]
fn a_space_the_graph_takes_never_reaches_find() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let mut events = space_events();
    events.push(Event::Text("a".into()));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(fx.view().find_text(), Some("a"));
    assert_eq!(fx.view().marks.len(), 1, "the row marked");
}

/// The log and the Backups list close Find (item 8), as the filters and the planner do: Enter and
/// Esc are theirs then.
#[test]
fn the_log_and_the_backups_list_close_find() {
    for open in ["log", "backups"] {
        let mut fx = Fx::new(1000.0, 500.0, fixture_view());
        fx.typed("x");
        let v = fx.view_mut();
        match open {
            "log" => v.open_log(vec![super::tests_s3::entry(
                "git fetch",
                crate::git::runner::Ending::Exit(0),
                "",
                0,
            )]),
            _ => v.open_backups(super::tests_s3::records()),
        }
        fx.idle();
        assert_eq!(fx.view().find_text(), None, "{open}");
        fx.press(Key::Escape);
        let closed = match open {
            "log" => fx.view().log_newest().is_none(),
            _ => fx.view().backups().is_none(),
        };
        assert!(closed, "{open}: Esc is its");
    }
}

/// On a grid too narrow for the labels at one width (SHOULD 9) the fields follow their own
/// labels, so each keeps cells and takes the typing.
#[test]
fn a_narrow_search_keeps_its_fields() {
    let mut fx = with_search(30.0 * 8.0, 500.0);
    fx.typed("abc");
    let (x, y) = search_at(&fx, "Message");
    assert_eq!(query(&fx).message, "abc");
    let l = search_layer(&fx);
    assert_eq!(text(&l, y, x + 8, x + 11), "abc", "{:#?}", search_rows(&fx));
    assert_eq!(cell(&l, x + 8, y).bg, Some(cga::BLACK));
}

/// The note row's change asks for a pass (SHOULD 10), so the note shows without waiting for input.
#[test]
fn a_changed_note_asks_for_a_pass() {
    // The passes' repaints asked from `find.rs`: egui says, a pass later, who asked (its file).
    let from_find = |fx: &Fx| {
        (fx.drv.ctx.repaint_causes().iter()).any(|c| c.file.ends_with("git_panel/find.rs"))
    };
    let mut fx = with_search(1000.0, 500.0);
    for _ in 0..7 {
        fx.press(Key::Tab);
    }
    fx.idle();
    fx.idle();
    assert!(!from_find(&fx), "the control: nothing asked");
    // Space on Search with nothing typed: the note says so, and asks for the pass that shows it.
    fx.drv.frame(&mut fx.host, space_events(), Modifiers::NONE);
    fx.idle();
    assert!(fx.shows("type what to search for"));
    assert!(
        from_find(&fx),
        "the note's change: {:?}",
        fx.drv.ctx.repaint_causes()
    );
}

/// Shift+Enter and Alt+Enter search as Enter does, as the egui dialog's did (SHOULD 11);
/// Cmd/Ctrl+Enter does not.
#[test]
fn shift_and_alt_enter_search() {
    for (mods, searches) in [
        (Modifiers::SHIFT, true),
        (Modifiers::ALT, true),
        (Modifiers::COMMAND, false),
    ] {
        let mut fx = with_search(1000.0, 500.0);
        fx.typed("one");
        let asked = fx.key(Key::Enter, mods);
        assert_eq!(!asked.is_empty(), searches, "{mods:?}: {asked:?}");
    }
}

/// Find's field at no, one and three cells (SHOULD 12): it never panics, typing lands, and the
/// caret stays in its cells.
#[test]
fn finds_field_on_tiny_rows() {
    for inside in [0u16, 1, 3] {
        let placed = find::Placed::of(Cells::new(5, 1, inside, 1));
        assert_eq!(placed.field, Cells::new(5, 1, inside, 1), "{inside}");
        assert!(placed.word.is_none() && placed.toggle.is_none());
        if inside == 0 {
            continue;
        }
        let mut fx = Fx::new(1000.0, 500.0, fixture_view());
        let side = Cells::new(40, 0, inside + 2, grid_of(&fx).rows);
        fx.host.side = Some(side);
        fx.idle();
        fx.typed("가나다");
        assert_eq!(fx.view().find_text(), Some("가나다"), "{inside}");
        let l = layer_over(&fx.drv.ctx, side);
        assert_eq!(
            l.caret,
            Some(Position::new(41 + inside - 1, HEAD)),
            "{inside}"
        );
        assert_eq!(
            cell(&l, side.right() - 1, HEAD).symbol,
            "║",
            "{inside}: the frame stays"
        );
    }
}

/// A long mixed query (SHOULD 12): 500 characters, Hangul and Latin, the field showing the end
/// with the caret on its last cell, never half a wide glyph at its left edge; Home shows the
/// start, the caret on the first cell.
#[test]
fn a_long_mixed_query_scrolls_by_whole_glyphs() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let query: String = "가a".repeat(250);
    fx.key(Key::F, Modifiers::COMMAND);
    fx.typed(&query);
    assert_eq!(fx.view().find_text(), Some(query.as_str()));
    let g = grid_of(&fx);
    let end = g.cols - 1 - 7;
    let l = panel(&fx);
    let caret = l.caret.expect("a caret");
    assert!(caret.x >= end - 2 && caret.x < end, "{caret:?}");
    let first = cell(&l, 6, HEAD);
    assert_ne!(first.symbol, "", "never a wide glyph's second half first");
    let shown = text(&l, HEAD, 6, end);
    assert!(query.contains(shown.trim()), "{shown:?}");
    fx.press(Key::Home);
    let l = panel(&fx);
    assert_eq!(l.caret, Some(Position::new(6, HEAD)));
    assert!(text(&l, HEAD, 6, end).starts_with("가a가a"));
}

/// The panel's release frame cost at 1000×700 with Find open on a Hangul word — 3,000 commits on
/// one line, Hangul subjects and authors, a match every third row — run alone: `cargo test
/// --release --lib -- --ignored --exact
/// gui::browser::git_panel::tests_x4_find::frame_cost_with_find_at_1000_by_700 --nocapture`.
/// Printed, no budget set. It builds against main's code as it is, for the comparison.
#[test]
#[ignore]
fn frame_cost_with_find_at_1000_by_700() {
    use crate::git::graph::RowStream;
    use crate::git::model::{CommitRow, Oid};
    use std::time::Instant;
    let id = |k: u32| Oid::parse(&format!("{k:040x}")).unwrap();
    let n = 3_000u32;
    let commits: Vec<CommitRow> = (1..=n)
        .rev()
        .map(|k| CommitRow {
            oid: id(k),
            parents: if k > 1 { vec![id(k - 1)] } else { vec![] },
            author_name: if k % 2 == 0 { "김철수" } else { "Ann" }.into(),
            author_email: "dev@example.com".into(),
            author_time: 1_790_000_000 + i64::from(k) * 600,
            commit_time: 1_790_000_000 + i64::from(k) * 600,
            subject: match k % 3 {
                0 => format!("한글 제목 {k}: 검색되는 줄"),
                _ => format!("commit {k}: an English subject"),
            },
        })
        .collect();
    let mut stream = RowStream::new(Some(id(n)), false, Vec::new(), false);
    let mut view = GitView::new("box:/srv/repo");
    view.set_refs(Vec::new(), HeadRef::Branch("refs/heads/main".into()));
    view.set_rows(stream.page(commits), false, 0);
    view.set_paging(Paging::AllLoaded);
    let mut fx = Fx::new(1000.0, 700.0, view);
    fx.key(Key::F, Modifiers::COMMAND);
    fx.typed("한글 제목");
    assert_eq!(fx.view().find_text(), Some("한글 제목"));
    fx.drv.idle(&mut fx.host, 20);
    let frames = 400;
    let started = Instant::now();
    for _ in 0..frames {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the git panel's frame with Find open at 1000x700: {:?}",
        started.elapsed() / frames
    );
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): Find and the history search, the same cells in both backends.

mod ts4 {
    use super::*;
    use crate::gui::chrome::CELL;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
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

    /// The Hangul fixture with Find open on `김철` (as Cmd/Ctrl+F and the word typed leave it),
    /// or with the history search filled.
    fn host(search: bool) -> Host {
        let mut view = hangul_view();
        if search {
            view.run(Command::HistorySearch);
            let d = view.search_dialog.as_mut().unwrap();
            d.query.message = "한글".into();
            d.query.author = "김철수".into();
            d.query.case = true;
        } else {
            view.run(Command::Find);
            view.find.as_mut().unwrap().text = "김철".into();
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

    /// The terminal's receipt and the window's are the same bytes, the caret's line among them:
    /// Find with a Hangul word typed, and the history search filled.
    #[test]
    fn ts4_find_and_the_search_are_the_same_cells_in_both_backends() {
        for search in [false, true] {
            let (cols, rows) = (100, 30);
            let mut tui = host(search);
            let (terminal, when) = terminal_run(&mut tui, cols, rows);
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = host(search);
            drv.idle(&mut win, 3);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "search: {search}");
            let wanted = if search { "[X] Match case" } else { "[ ] Aa" };
            assert!(terminal.contains(wanted), "{terminal}");
            assert!(terminal.contains("김철"), "{terminal}");
            assert!(!terminal.contains("caret none"), "{terminal}");
            assert!(
                !terminal.contains("F12 shows it"),
                "no hand-off box: {terminal}"
            );
        }
    }
}
