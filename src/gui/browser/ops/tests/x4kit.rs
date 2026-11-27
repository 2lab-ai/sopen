//! X4-kit (10 §17): the dialog parts the git panel's dialogs need, in `ops::dialog` — a row of
//! choices, one of several or any of several, wrapped by cells; a message field of several rows,
//! Shift+Enter its new line and a plain Enter the dialog's, scrolling with its caret; a list
//! with a bar, its rows' keys at the right, scrolled with the bar; several fields and check
//! boxes in one dialog, Tab through them. Each in a stand-in host that lays a screen of cells
//! under the dialog, so a terminal (`term::compose` of the pass's layers) shows the window's
//! cells exactly; keys and clicks through the test kit, several in one pass where it matters.

use super::*;
use crate::gui::browser::keys::{self, Mods};
use crate::gui::cells::{Layer, Pen};
use crate::gui::term::{self, Screen};
use dialog::{choice_id, work, Bar, Input, Row, Rows};
use std::sync::Arc;

const CELL: Vec2 = chrome::CELL;

fn id(name: &str) -> egui::Id {
    egui::Id::unique(("sopen-x4kit", name))
}

/// One dialog over a screen of cells, and its controls' state.
struct Host {
    title: String,
    nominal: usize,
    parts: Vec<Part>,
    a: String,
    b: String,
    message: String,
    on: bool,
    also: bool,
    one: Option<usize>,
    any: Vec<bool>,
    bar: Bar,
    /// The first field and check box through `Inputs::field` and `check`, as the list's dialogs
    /// give theirs, instead of by their ids.
    legacy: bool,
    /// The host takes a plain Enter as the dialog's, as the git panel's dialogs do.
    takes_enter: bool,
    enters: usize,
    clicked: Vec<egui::Id>,
    chosen: Vec<usize>,
    controls: Vec<egui::Id>,
    covers: Vec<Cells>,
    /// The modal's rect, as egui keeps it.
    modal: Rect,
    /// The dialog is not shown (closed for a while).
    hidden: bool,
    /// The text the dialog left in each pass's input, for what is behind it.
    left: Vec<String>,
    /// The first row a report's rows that scroll show.
    report_top: usize,
}

impl Host {
    fn new(title: &str, nominal: usize, parts: Vec<Part>) -> Self {
        Self {
            title: title.to_string(),
            nominal,
            parts,
            a: String::new(),
            b: String::new(),
            message: String::new(),
            on: false,
            also: false,
            one: None,
            any: vec![false; 8],
            bar: Bar::default(),
            legacy: false,
            takes_enter: true,
            enters: 0,
            clicked: vec![],
            chosen: vec![],
            controls: vec![],
            covers: vec![],
            modal: Rect::NOTHING,
            hidden: false,
            left: vec![],
            report_top: 0,
        }
    }

    fn take_clicked(&mut self) -> Vec<egui::Id> {
        std::mem::take(&mut self.clicked)
    }
}

/// Every id a pass may say was clicked: the buttons, the check boxes, the choices, the list.
fn clickable() -> Vec<egui::Id> {
    let mut all = vec![id("ok"), id("cancel"), id("on"), id("also"), id("list")];
    for row in ["one", "any"] {
        all.extend((0..8).map(|k| choice_id(id(row), k)));
    }
    all
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.hidden {
            return;
        }
        let ctx = ui.ctx().clone();
        // The dialog's Enter, taken exactly, without Shift (the git panel's `keys::take`).
        if self.takes_enter && ctx.input_mut(|i| keys::take(i, Key::Enter, Mods::None)) {
            self.enters += 1;
        }
        let grid = dialog::grid(&ctx);
        let layout = dialog::layout(grid, &self.title, self.nominal, &self.parts);
        // The screen under it, the cells it covers blanked first (cells.rs's rule).
        let body = Pen::new(chrome::PALETTE.text, chrome::PALETTE.back);
        let mut under = Buffer::empty(grid.all());
        cells::fill(&mut under, grid.all(), body);
        let words = "한글 under ".repeat(20);
        for y in 0..grid.rows {
            cells::put(&mut under, 0, y, &words, usize::from(grid.cols), body);
        }
        self.covers = layout.covers();
        for c in &self.covers {
            cells::fill(&mut under, *c, body);
        }
        cells::paint(ui.painter(), grid.origin, &under, None);
        let mut by_id = vec![
            (id("b"), Input::Text(&mut self.b)),
            (id("message"), Input::Text(&mut self.message)),
            (id("also"), Input::Check(&mut self.also)),
            (id("one"), Input::One(&mut self.one)),
            (id("any"), Input::Any(&mut self.any)),
            (id("list"), Input::List(&mut self.bar)),
        ];
        let (mut field, mut check) = (None, None);
        if self.legacy {
            field = Some(&mut self.a);
            check = Some(&mut self.on);
        } else {
            by_id.push((id("a"), Input::Text(&mut self.a)));
            by_id.push((id("on"), Input::Check(&mut self.on)));
        }
        let inputs = Inputs {
            field,
            check,
            top: Some(&mut self.report_top),
            by_id,
        };
        let shown = dialog::show(&ctx, id("dialog"), &layout, inputs);
        self.clicked
            .extend(clickable().into_iter().filter(|c| shown.clicked(*c)));
        self.chosen.extend(shown.chosen(id("list")));
        self.controls = shown.controls;
        self.modal = shown.modal.response.rect;
        self.left.extend(ctx.input(|i| {
            (i.events.iter())
                .filter_map(|e| match e {
                    Event::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        }));
    }

    fn ready(&self) -> bool {
        true
    }
}

fn buttons() -> Part {
    Part::Buttons(vec![
        Button::new("Run", id("ok")),
        Button::new("Cancel", id("cancel")),
    ])
}

fn one_of(label: &str, choices: &[&str]) -> Part {
    Part::Choices {
        label: label.to_string(),
        id: id("one"),
        choices: choices.iter().map(|c| c.to_string()).collect(),
        many: false,
    }
}

fn any_of(label: &str, choices: &[&str]) -> Part {
    Part::Choices {
        label: label.to_string(),
        id: id("any"),
        choices: choices.iter().map(|c| c.to_string()).collect(),
        many: true,
    }
}

fn message(label: &str, rows: u16) -> Part {
    Part::Message {
        label: label.to_string(),
        id: id("message"),
        rows,
    }
}

fn list(rows: &[(&str, &str)], most: u16) -> Part {
    Part::List {
        rows: Rows::new(rows.iter().map(|(l, k)| Row::new(l, k)).collect()),
        id: id("list"),
        most,
    }
}

fn field(label: &str, name: &str) -> Part {
    Part::Field {
        label: label.to_string(),
        id: id(name),
    }
}

fn check(label: &str, name: &str) -> Part {
    Part::Check {
        label: label.to_string(),
        id: id(name),
    }
}

/// A key pressed, with its physical key, and no release.
fn down(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn focus(drv: &mut Driver, h: &mut Host, on: egui::Id) {
    drv.ctx.memory_mut(|m| m.request_focus(on));
    drv.idle(h, 2);
}

fn focused(drv: &Driver) -> Option<egui::Id> {
    drv.ctx.memory(|m| m.focused())
}

/// The window's grid in cells.
fn size(drv: &Driver) -> (u16, u16) {
    let r = drv.ctx.content_rect();
    ((r.width() / CELL.x) as u16, (r.height() / CELL.y) as u16)
}

/// The dialog's layer: the one whose rows hold its title.
fn boxed(drv: &Driver, title: &str) -> Arc<Layer> {
    let layers = cells::layers(&drv.ctx);
    (layers.into_iter())
        .find(|l| {
            let a = l.buf.area;
            (a.top()..a.bottom()).any(|y| cells::row_text(&l.buf, y).0.contains(title))
        })
        .unwrap_or_else(|| panic!("no layer titled {title:?}"))
}

/// Row `y` of the dialog's layer, from the box's left border.
fn row(l: &Layer, y: u16) -> String {
    cells::row_text(&l.buf, y).0
}

fn rows(l: &Layer) -> Vec<String> {
    let a = l.buf.area;
    (a.top()..a.bottom()).map(|y| row(l, y)).collect()
}

/// Where `text` stands in the dialog's layer (its first place).
fn place(l: &Arc<Layer>, text: &str) -> Cells {
    let hits = cells::find(std::slice::from_ref(l), text);
    hits.first()
        .unwrap_or_else(|| panic!("{text:?} not in {:#?}", rows(l)))
        .cells
}

/// The symbol of cell (`x`, `y`) of the layer (a wide glyph's second cell: none).
fn symbol_at(l: &Layer, x: u16, y: u16) -> String {
    let a = l.buf.area;
    l.seen()[usize::from(y - a.y)][usize::from(x - a.x)]
        .symbol
        .clone()
}

/// The colours of cell (`x`, `y`) of the layer.
fn pen_at(l: &Layer, x: u16, y: u16) -> (Option<Color32>, Option<Color32>) {
    let a = l.buf.area;
    let seen = &l.seen()[usize::from(y - a.y)][usize::from(x - a.x)];
    (seen.fg, seen.bg)
}

fn bar_pen() -> (Option<Color32>, Option<Color32>) {
    let p = chrome::PALETTE;
    (Some(p.bar_text), Some(p.bar))
}

fn field_pen() -> (Option<Color32>, Option<Color32>) {
    (Some(chrome::PALETTE.field_text), Some(chrome::cga::BLACK))
}

fn text_pen() -> (Option<Color32>, Option<Color32>) {
    let p = chrome::PALETTE;
    (Some(p.dialog_text), Some(p.dialog_back))
}

/// The middle of cell (`x`, `y`) in points.
fn at_cell(x: u16, y: u16) -> Pos2 {
    Pos2::new((f32::from(x) + 0.5) * CELL.x, (f32::from(y) + 0.5) * CELL.y)
}

/// What a terminal shows of the pass (`term::compose`), every cell the window's: per cell the
/// last layer painted there (one whose cell has no colour of its own lets the one under show),
/// the same character, foreground and background, and the same caret; no hand-off box.
fn same_in_a_terminal(drv: &Driver) -> Screen {
    let layers = cells::layers(&drv.ctx);
    let (cols, rows) = size(drv);
    let screen = term::compose(&drv.ctx, &layers, cols, rows);
    assert_eq!(screen.hand_offs, vec![], "every cell is the window's");
    for y in 0..rows {
        for x in 0..cols {
            let at = Position::new(x, y);
            let want = (layers.iter().rev())
                .find(|l| {
                    l.buf.area.contains(at)
                        && !matches!(l.buf[at].bg, ratatui_core::style::Color::Reset)
                })
                .map(|l| &l.buf[at])
                .expect("a layer under every cell");
            let got = &screen.buf[at];
            assert_eq!(
                (want.symbol(), want.fg, want.bg),
                (got.symbol(), got.fg, got.bg),
                "cell {x},{y}"
            );
        }
    }
    let caret = layers.iter().rev().find_map(|l| l.caret);
    assert_eq!(screen.caret, caret, "the caret's cell");
    screen
}

fn screen_rows(s: &Screen) -> Vec<String> {
    let a = s.buf.area;
    (a.top()..a.bottom())
        .map(|y| cells::row_text(&s.buf, y).0)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Every part in both backends.

/// Every part of the kit in one dialog: two fields, two check boxes, a row of one of several
/// and one of any of several, a message field and a list — Hangul in labels, choices, the text
/// and the rows.
fn every_part() -> Host {
    let parts = vec![
        field("이름", "a"),
        field("Email", "b"),
        check("Sign off", "on"),
        check("한글 확인", "also"),
        one_of("브랜치", &["메인", "dev", "release"]),
        any_of("Show", &["Tags", "Stashes", "Remotes"]),
        message("Message", 3),
        list(
            &[
                ("Check out…", "F5"),
                ("한글 항목", "F6"),
                ("Reset…", ""),
                ("Tag…", "F7"),
                ("Branch…", "F8"),
            ],
            3,
        ),
        buttons(),
    ];
    let mut h = Host::new("Commit", 50, parts);
    h.a = "홍길동".into();
    h.b = "z@example.org".into();
    h.on = true;
    h.one = Some(0);
    h.any[0] = true;
    h.any[2] = true;
    h.message = "제목 줄\n\n본문 한글".into();
    h.bar = Bar { at: 1, top: 0 };
    h
}

/// Each part is cells in the window and the same cells in a terminal: the choices with their
/// marks — `(•)` the chosen one of several, `[X]` each on of any of several — the message's
/// rows in white on black, the list's rows with their keys at the right and the bar's row in
/// the bar's colours, the focused field's caret.
#[test]
fn x4kit_every_part_is_the_same_cells_in_a_terminal() {
    let mut h = every_part();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("a"));
    let l = boxed(&drv, "Commit");
    let all = rows(&l).join("\n");
    for text in [
        "(•) 메인",
        "( ) dev",
        "( ) release",
        "[X] Tags",
        "[ ] Stashes",
        "[X] Remotes",
        "[X] Sign off",
        "[ ] 한글 확인",
        "Message",
        "제목 줄",
        "본문 한글",
        "Check out…",
        "한글 항목",
        "Reset…",
    ] {
        assert!(all.contains(text), "{text:?} not in the dialog: {all}");
    }
    assert!(!all.contains("Tag…"), "the list shows 3 rows: {all}");
    // The list's keys at its right edge, a blank after them; the bar's row in its colours.
    let first = place(&l, "Check out…");
    let key = place(&l, "F5");
    let inside_right = first.x - 1 + 50;
    assert_eq!(key.right(), inside_right - 1, "{all}");
    let bar = place(&l, "한글 항목");
    assert_eq!(pen_at(&l, bar.x, bar.y), bar_pen());
    assert_eq!(pen_at(&l, bar.x - 1, bar.y), bar_pen(), "the bar's blank");
    assert_eq!(pen_at(&l, first.x, first.y), text_pen());
    // The message's rows: white on black from the box's inside left.
    let subject = place(&l, "제목 줄");
    assert_eq!(pen_at(&l, subject.x, subject.y), field_pen());
    assert_eq!(
        pen_at(&l, subject.x, subject.y + 1),
        field_pen(),
        "an empty row"
    );
    let screen = same_in_a_terminal(&drv);
    assert!(screen.caret.is_some(), "the field's caret");
    let shown = screen_rows(&screen).join("\n");
    assert!(
        shown.contains("(•) 메인") && shown.contains("본문 한글"),
        "{shown}"
    );
}

/// In an ambiguous-wide terminal the chosen choice's `•` is §10's `.`, written at the source,
/// and the cells are still the window's.
#[test]
fn x4kit_an_ambiguous_wide_terminal_reads_the_chosen_choice_as_a_dot() {
    let mut h = every_part();
    let mut drv = Driver::new(640.0, 400.0);
    cells::set_ambiguous_wide(&drv.ctx, true);
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Commit");
    let all = rows(&l).join("\n");
    assert!(all.contains("(.) 메인"), "{all}");
    assert!(!all.contains('•'), "{all}");
    same_in_a_terminal(&drv);
}

/// A row of choices wraps by cells: on a narrow window a choice that does not fit goes to the
/// next row — under the first choice when the label leaves room for the widest there, else at
/// the row's start; a Hangul choice wraps whole.
#[test]
fn x4kit_a_row_of_choices_wraps_by_cells() {
    let parts = vec![
        one_of(
            "On conflicts prefer",
            &["nothing", "ours", "theirs", "한글한글한글"],
        ),
        any_of("Parent", &["1", "2", "3", "4", "5", "6"]),
    ];
    let mut h = Host::new("Merge", 20, parts);
    let mut drv = Driver::new(320.0, 400.0);
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Merge");
    let head = place(&l, "On conflicts prefer");
    let x0 = head.x;
    // 40 columns: 32 inside. The label takes 20 cells, the widest choice 16: no room for it
    // after the label, so the rows after the first start at the row's start.
    assert_eq!(place(&l, "( ) nothing").x, x0 + 20);
    let ours = place(&l, "( ) ours");
    assert_eq!((ours.x, ours.y), (x0, head.y + 1));
    let theirs = place(&l, "( ) theirs");
    assert_eq!((theirs.x, theirs.y), (x0 + 10, head.y + 1));
    let hangul = place(&l, "( ) 한글한글한글");
    assert_eq!((hangul.x, hangul.y, hangul.width), (x0, head.y + 2, 16));
    // "Parent " leaves room for every choice: the next row starts under the first one.
    let parent = place(&l, "Parent");
    assert_eq!(parent.y, head.y + 3);
    assert_eq!(place(&l, "[ ] 1").x, x0 + 7);
    let four = place(&l, "[ ] 4");
    assert_eq!((four.x, four.y), (x0 + 7, parent.y + 1));
    same_in_a_terminal(&drv);
}

// ---------------------------------------------------------------------------------------------
// The keyboard.

fn tab_dialog() -> Host {
    let parts = vec![
        field("Name", "a"),
        field("Email", "b"),
        check("Sign off", "on"),
        one_of("How", &["merge", "rebase", "only"]),
        message("Message", 3),
        list(&[("one", ""), ("two", ""), ("three", "")], 3),
        buttons(),
    ];
    Host::new("Tab", 50, parts)
}

/// Tab goes through every control in the order of the parts — the fields, the check box, each
/// choice, the message field, the list, the buttons — and Shift+Tab back; the pass says the
/// same order of its controls.
#[test]
fn x4kit_tab_goes_through_every_control_in_order() {
    let mut h = tab_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let order = vec![
        id("a"),
        id("b"),
        id("on"),
        choice_id(id("one"), 0),
        choice_id(id("one"), 1),
        choice_id(id("one"), 2),
        id("message"),
        id("list"),
        id("ok"),
        id("cancel"),
    ];
    assert_eq!(h.controls, order);
    focus(&mut drv, &mut h, id("a"));
    for want in &order[1..] {
        drv.key(&mut h, Key::Tab, Modifiers::NONE);
        assert_eq!(focused(&drv), Some(*want));
    }
    drv.key(&mut h, Key::Tab, Modifiers::SHIFT);
    assert_eq!(focused(&drv), Some(id("ok")));
    // Typed into each field it reaches, the text goes there alone.
    focus(&mut drv, &mut h, id("a"));
    drv.frame(&mut h, vec![Event::Text("가".into())], Modifiers::NONE);
    drv.key(&mut h, Key::Tab, Modifiers::NONE);
    drv.frame(&mut h, vec![Event::Text("나".into())], Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert_eq!((h.a.as_str(), h.b.as_str()), ("가", "나"));
}

/// Space chooses the focused choice of one of several, as Enter does where the dialog leaves
/// Enter; where the dialog takes Enter (the git panel's) it is the dialog's and chooses
/// nothing. A click chooses and takes the keyboard.
#[test]
fn x4kit_space_and_enter_choose_one_of_several() {
    let mut h = tab_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, choice_id(id("one"), 1));
    drv.key(&mut h, Key::Space, Modifiers::NONE);
    assert_eq!(h.one, Some(1));
    assert_eq!(h.take_clicked(), vec![choice_id(id("one"), 1)]);
    let l = boxed(&drv, "Tab");
    assert!(rows(&l).join("\n").contains("( ) merge  (•) rebase"));
    let rebase = place(&l, "(•) rebase");
    assert_eq!(
        pen_at(&l, rebase.x, rebase.y),
        bar_pen(),
        "the focus's colours"
    );
    let merge = place(&l, "( ) merge");
    assert_eq!(pen_at(&l, merge.x, merge.y), text_pen());
    // Enter the dialog takes: the dialog's, not the choice's.
    focus(&mut drv, &mut h, choice_id(id("one"), 2));
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert_eq!((h.one, h.enters), (Some(1), 1));
    // Enter the dialog leaves: the focused choice's.
    h.takes_enter = false;
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert_eq!((h.one, h.enters), (Some(2), 1));
    // A click: chosen, and the keyboard there.
    let l = boxed(&drv, "Tab");
    let merge = place(&l, "( ) merge");
    drv.click(&mut h, at_cell(merge.x + 1, merge.y));
    assert_eq!(h.one, Some(0));
    assert_eq!(focused(&drv), Some(choice_id(id("one"), 0)));
}

fn any_dialog() -> Host {
    let parts = vec![
        field("Name", "a"),
        any_of("Show", &["Tags", "Stashes", "Remotes"]),
        buttons(),
    ];
    Host::new("Any", 40, parts)
}

/// Any of several: Space turns the focused choice on or off, once a press — two presses in one
/// pass turn it twice; a Space before a Tab in one pass is the choice's that had the keyboard,
/// one after it the choice's the Tab gives it to.
#[test]
fn x4kit_space_turns_a_choice_of_any_once_a_press() {
    let mut h = any_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let tags = choice_id(id("any"), 0);
    focus(&mut drv, &mut h, tags);
    drv.key(&mut h, Key::Space, Modifiers::NONE);
    assert_eq!(&h.any[..3], [true, false, false]);
    assert!(rows(&boxed(&drv, "Any"))
        .join("\n")
        .contains("[X] Tags  [ ] Stashes"));
    let space = || down(Key::Space, Modifiers::NONE);
    drv.frame(&mut h, vec![space(), space()], Modifiers::NONE);
    assert_eq!(&h.any[..3], [true, false, false], "on and off again");
    drv.frame(&mut h, vec![space()], Modifiers::NONE);
    assert_eq!(&h.any[..3], [false, false, false]);
    // Space, then Tab: Tags turns; the keyboard goes on to Stashes.
    drv.frame(
        &mut h,
        vec![space(), down(Key::Tab, Modifiers::NONE)],
        Modifiers::NONE,
    );
    drv.idle(&mut h, 1);
    assert_eq!(&h.any[..3], [true, false, false]);
    assert_eq!(focused(&drv), Some(choice_id(id("any"), 1)));
    // Tab, then Space: the keyboard goes on to Remotes, which turns.
    drv.frame(
        &mut h,
        vec![down(Key::Tab, Modifiers::NONE), space()],
        Modifiers::NONE,
    );
    drv.idle(&mut h, 1);
    assert_eq!(focused(&drv), Some(choice_id(id("any"), 2)));
    assert_eq!(&h.any[..3], [true, false, true]);
}

/// Space as the window sends it — the key and its text in one pass — is one press, and its
/// text is taken with it: nothing behind the dialog sees it, in a pass alone or with a Tab after
/// it.
#[test]
fn x4kit_space_with_its_text_is_one_press_and_types_nothing() {
    let mut h = tab_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, choice_id(id("one"), 1));
    drv.press_typed(&mut h, Key::Space, " ");
    assert_eq!(h.one, Some(1));
    assert_eq!(std::mem::take(&mut h.left), Vec::<String>::new());
    focus(&mut drv, &mut h, choice_id(id("one"), 2));
    let pass = vec![
        down(Key::Space, Modifiers::NONE),
        Event::Text(" ".into()),
        down(Key::Tab, Modifiers::NONE),
    ];
    drv.frame(&mut h, pass, Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!((h.one, focused(&drv)), (Some(2), Some(id("message"))));
    assert_eq!(std::mem::take(&mut h.left), Vec::<String>::new());
    // The list: one choice of its bar's row.
    focus(&mut drv, &mut h, id("list"));
    drv.press_typed(&mut h, Key::Space, " ");
    assert_eq!(h.chosen, vec![0]);
    assert_eq!(h.left, Vec::<String>::new());
}

/// A Space with modifiers is not a choice's: Shift+Space or Ctrl+Space on a focused choice
/// chooses nothing (the kit's keys are taken with exactly no modifiers, as `keys::take`'s
/// `Mods::None`).
#[test]
fn x4kit_a_space_with_modifiers_chooses_nothing() {
    let mut h = tab_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, choice_id(id("one"), 1));
    for mods in [Modifiers::SHIFT, Modifiers::COMMAND] {
        let pass = vec![down(Key::Space, mods), Event::Text(" ".into())];
        drv.frame(&mut h, pass, mods);
        drv.idle(&mut h, 1);
        assert_eq!(h.one, None, "{mods:?}");
    }
    drv.press_typed(&mut h, Key::Space, " ");
    assert_eq!(h.one, Some(1));
}

/// One Space in the pass a Tab leaves a field or a message field for a choice or a list acts
/// once: the field that had the keyboard read the pass and typed it, so the choice it reaches
/// neither turns nor is chosen, nor the list's row (`[Tab, Space, its text]`).
#[test]
fn x4kit_one_space_after_a_tab_from_a_field_acts_once() {
    let tab_space = || {
        vec![
            down(Key::Tab, Modifiers::NONE),
            down(Key::Space, Modifiers::NONE),
            Event::Text(" ".into()),
        ]
    };
    let mut h = any_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("a"));
    drv.frame(&mut h, tab_space(), Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!(focused(&drv), Some(choice_id(id("any"), 0)));
    assert_eq!((h.a.as_str(), &h.any[..3]), (" ", &[false; 3][..]));
    // A field before a list.
    let mut h = list_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("a"));
    drv.frame(&mut h, tab_space(), Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!(focused(&drv), Some(id("list")));
    assert_eq!((h.a.as_str(), h.chosen.as_slice()), (" ", &[][..]));
    // A message field before a list.
    let mut h = tab_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("message"));
    drv.frame(&mut h, tab_space(), Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!(focused(&drv), Some(id("list")));
    assert_eq!((h.message.as_str(), h.chosen.as_slice()), (" ", &[][..]));
}

/// Space then Tab in one pass on check boxes given by id turns the one that had the keyboard,
/// and the keyboard goes on: neither the next box nor the next button acts.
#[test]
fn x4kit_space_then_tab_turns_the_check_box_that_had_the_keyboard() {
    let parts = vec![
        check("Sign off", "on"),
        check("한글 확인", "also"),
        buttons(),
    ];
    let mut h = Host::new("Checks", 40, parts);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("on"));
    let space_tab = || {
        vec![
            down(Key::Space, Modifiers::NONE),
            Event::Text(" ".into()),
            down(Key::Tab, Modifiers::NONE),
        ]
    };
    drv.frame(&mut h, space_tab(), Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!((h.on, h.also), (true, false));
    assert_eq!(focused(&drv), Some(id("also")));
    assert_eq!(h.take_clicked(), vec![id("on")]);
    drv.frame(&mut h, space_tab(), Modifiers::NONE);
    drv.idle(&mut h, 2);
    assert_eq!((h.on, h.also), (true, true));
    assert_eq!(focused(&drv), Some(id("ok")));
    assert_eq!(h.take_clicked(), vec![id("also")], "Run is not clicked");
    // Space, Space in one pass: twice.
    focus(&mut drv, &mut h, id("on"));
    let space = || down(Key::Space, Modifiers::NONE);
    drv.frame(&mut h, vec![space(), space()], Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert_eq!((h.on, h.also), (true, true));
    // A click still turns it.
    let l = boxed(&drv, "Checks");
    let on = place(&l, "[X] Sign off");
    drv.click(&mut h, at_cell(on.x + 1, on.y));
    assert!(!h.on, "a click turns it off");
}

// ---------------------------------------------------------------------------------------------
// The message field.

fn message_dialog(rows: u16, nominal: usize) -> Host {
    let parts = vec![
        Part::Text("Commit the staged changes.".into(), Ink::Text),
        message("Message", rows),
        buttons(),
    ];
    Host::new("Commit", nominal, parts)
}

fn shift_enter() -> Event {
    down(Key::Enter, Modifiers::SHIFT)
}

/// Shift+Enter is a new line; a plain Enter is never typed — the dialog's where it takes it,
/// nothing where it does not. Text, Shift+Enter and text in one pass are one edit each, in
/// order.
#[test]
fn x4kit_shift_enter_is_a_new_line_and_enter_is_the_dialogs() {
    let mut h = message_dialog(3, 40);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("message"));
    let typed = |t: &str| Event::Text(t.into());
    drv.frame(
        &mut h,
        vec![typed("가"), shift_enter(), typed("나")],
        Modifiers::SHIFT,
    );
    drv.idle(&mut h, 1);
    assert_eq!(h.message, "가\n나");
    assert_eq!(h.enters, 0);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert_eq!((h.message.as_str(), h.enters), ("가\n나", 1));
    h.takes_enter = false;
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert_eq!(h.message, "가\n나", "a plain Enter is never typed");
    drv.key(&mut h, Key::Enter, Modifiers::SHIFT);
    assert_eq!(h.message, "가\n나\n");
    // The rows in cells, the caret on the new empty row's first cell.
    let l = boxed(&drv, "Commit");
    let ga = place(&l, "가");
    let na = place(&l, "나");
    assert_eq!((na.x, na.y), (ga.x, ga.y + 1));
    assert_eq!(pen_at(&l, ga.x, ga.y), field_pen());
    assert_eq!(l.caret, Some(Position::new(ga.x, ga.y + 2)));
    same_in_a_terminal(&drv);
}

/// The text wraps by cells, its room a cell narrower than the field (the caret's cell after a
/// full row): at its last blank that fits, the blank ending the row above, else after its last
/// whole glyph — a Hangul glyph never split, on an odd room too.
#[test]
fn x4kit_the_message_wraps_by_cells() {
    // A box of 12 cells inside: an 11-cell room.
    let parts = vec![message("", 3)];
    let mut h = Host::new("M", 0, parts);
    let mut drv = Driver::new(640.0, 400.0);
    h.message = "가나다라마바사".into();
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "M");
    let first = place(&l, "가나다라마");
    let rest = place(&l, "바사");
    assert_eq!((rest.x, rest.y), (first.x, first.y + 1), "{:#?}", rows(&l));
    // Typed: broken after the blank that fits; the caret after "끝" on the third row.
    h.message.clear();
    focus(&mut drv, &mut h, id("message"));
    let typed = Event::Text("가나 다라마바사 끝".into());
    drv.frame(&mut h, vec![typed], Modifiers::NONE);
    drv.idle(&mut h, 1);
    let l = boxed(&drv, "M");
    let all = rows(&l);
    let at = place(&l, "가나");
    assert_eq!(place(&l, "다라마바사").y, at.y + 1, "{all:#?}");
    assert_eq!(place(&l, "끝").y, at.y + 2, "{all:#?}");
    assert_eq!(l.caret, Some(Position::new(at.x + 2, at.y + 2)));
    // A Hangul glyph typed at the end of a full row goes whole to the next.
    h.message.clear();
    drv.frame(
        &mut h,
        vec![Event::Text("가나다라마".into())],
        Modifiers::NONE,
    );
    drv.frame(&mut h, vec![Event::Text("바".into())], Modifiers::NONE);
    drv.idle(&mut h, 1);
    let l = boxed(&drv, "M");
    assert_eq!(place(&l, "바").y, place(&l, "가나다라마").y + 1);
    same_in_a_terminal(&drv);
}

/// Past its rows the field scrolls with the caret: the last rows typed show, ↑ ░ ↓ on the box's
/// right border; the wheel scrolls it without moving the caret (hidden while out of sight); a
/// click on a row it shows puts the caret there, one on the text above it does not; the box
/// stays as it was; the caret is the same cell in a terminal.
#[test]
fn x4kit_the_message_scrolls_with_its_caret() {
    let mut h = message_dialog(3, 40);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let (covers, modal) = (h.covers.clone(), h.modal);
    focus(&mut drv, &mut h, id("message"));
    let mut typed = vec![];
    for (k, word) in ["one", "two", "three", "four", "five", "six"]
        .iter()
        .enumerate()
    {
        if k > 0 {
            typed.push(shift_enter());
        }
        typed.push(Event::Text(word.to_string()));
    }
    drv.frame(&mut h, typed, Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert_eq!(h.message, "one\ntwo\nthree\nfour\nfive\nsix");
    assert_eq!(
        (&h.covers, h.modal),
        (&covers, modal),
        "the box keeps its size"
    );
    let l = boxed(&drv, "Commit");
    let all = rows(&l).join("\n");
    assert!(!all.contains("three") && all.contains("four"), "{all}");
    let six = place(&l, "six");
    assert_eq!(place(&l, "four").y + 2, six.y);
    assert_eq!(l.caret, Some(Position::new(six.x + 3, six.y)));
    // The scroll bar on the right border beside the field's rows, its thumb at the end.
    let border = six.x - 2 + 40 + 3;
    assert_eq!(
        (
            symbol_at(&l, border, six.y - 2),
            symbol_at(&l, border, six.y)
        ),
        ("↑".to_string(), "↓".to_string()),
        "{all}"
    );
    same_in_a_terminal(&drv);
    // A click on the dialog's text above the field, where the rows scrolled out of it would
    // stand: the caret stays at the end, the rows as they were.
    let end = h.message.chars().count();
    let caret = |drv: &Driver| {
        let state = egui::text_edit::TextEditState::load(&drv.ctx, id("message")).unwrap();
        state.cursor.char_range().unwrap().primary.index.0
    };
    let head = place(&l, "Commit the staged changes.");
    drv.click(&mut h, at_cell(head.x + 2, head.y));
    assert_eq!(caret(&drv), end);
    assert_eq!(place(&boxed(&drv, "Commit"), "six").y, six.y);
    // The wheel: up to the top; the caret out of sight, not drawn.
    focus(&mut drv, &mut h, id("message"));
    drv.wheel(&mut h, at_cell(six.x + 1, six.y), 3.0);
    let l = boxed(&drv, "Commit");
    let one = place(&l, "one");
    assert_eq!(place(&l, "three").y, one.y + 2);
    assert_eq!(l.caret, None, "{:#?}", rows(&l));
    let screen = same_in_a_terminal(&drv);
    assert_eq!(screen.caret, None);
    assert_eq!((&h.covers, h.modal), (&covers, modal));
    // A click on "two"'s row after its text: the caret there, on the second row.
    drv.click(&mut h, at_cell(one.x + 6, one.y + 1));
    assert_eq!(caret(&drv), "one\ntwo".chars().count());
    let l = boxed(&drv, "Commit");
    assert_eq!(l.caret, Some(Position::new(one.x + 3, one.y + 1)));
    same_in_a_terminal(&drv);
    // Typing there: the text where the caret is.
    drv.frame(&mut h, vec![Event::Text("!".into())], Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert!(h.message.starts_with("one\ntwo!\nthree"), "{:?}", h.message);
    assert_eq!(h.covers, covers);
}

/// A message longer than its rows opens at its top, and once it has the keyboard its caret is
/// on the first cell (egui's own field would put it at the end).
#[test]
fn x4kit_a_long_message_opens_at_its_top() {
    let mut h = message_dialog(3, 40);
    h.message = "subject\n\nline 3\nline 4\nline 5\nline 6".into();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Commit");
    let all = rows(&l).join("\n");
    assert!(all.contains("subject") && all.contains("line 3"), "{all}");
    assert!(!all.contains("line 4"), "{all}");
    focus(&mut drv, &mut h, id("message"));
    let l = boxed(&drv, "Commit");
    let subject = place(&l, "subject");
    assert_eq!(l.caret, Some(Position::new(subject.x, subject.y)));
    assert!(rows(&l).join("\n").contains("subject"));
}

/// A message field shown again after its dialog closed — the same id, another text — starts
/// afresh: at its top, unfocused; once it has the keyboard, its caret at its start.
#[test]
fn x4kit_a_message_shown_again_starts_afresh() {
    let mut h = message_dialog(3, 40);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("message"));
    let mut typed = vec![];
    for (k, word) in ["one", "two", "three", "four", "five"].iter().enumerate() {
        if k > 0 {
            typed.push(shift_enter());
        }
        typed.push(Event::Text(word.to_string()));
    }
    drv.frame(&mut h, typed, Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert!(!rows(&boxed(&drv, "Commit")).join("\n").contains("one"));
    h.hidden = true;
    drv.idle(&mut h, 2);
    h.hidden = false;
    h.message = "first\nsecond\nthird\nfourth\nfifth".into();
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Commit");
    let all = rows(&l).join("\n");
    assert!(all.contains("first") && !all.contains("fourth"), "{all}");
    assert_eq!(l.caret, None);
    focus(&mut drv, &mut h, id("message"));
    let l = boxed(&drv, "Commit");
    let first = place(&l, "first");
    assert_eq!(l.caret, Some(Position::new(first.x, first.y)));
}

/// On a window too short for the dialog, a message field's rows and a list's give way first,
/// the most first, down to what fits; the box and its shadow stay inside the window.
#[test]
fn x4kit_a_short_window_cuts_the_message_and_the_list_first() {
    let items: Vec<String> = (0..10).map(|k| format!("item {k}")).collect();
    let refs: Vec<(&str, &str)> = items.iter().map(|i| (i.as_str(), "")).collect();
    let parts = vec![
        Part::Text("Pick one.".into(), Ink::Text),
        message("Message", 8),
        list(&refs, 8),
        buttons(),
    ];
    for (w, h_px, want) in [(640.0, 400.0, 8), (480.0, 320.0, 6)] {
        let mut h = Host::new("Short", 40, parts.clone());
        h.message = (0..10)
            .map(|k| format!("line {k}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut drv = Driver::new(w, h_px);
        drv.idle(&mut h, 2);
        let l = boxed(&drv, "Short");
        let shown = |name: &str| {
            (0..10)
                .filter(|k| {
                    !cells::find(std::slice::from_ref(&l), &format!("{name} {k}")).is_empty()
                })
                .count()
        };
        assert_eq!(
            (shown("line"), shown("item")),
            (want, want),
            "{:#?}",
            rows(&l)
        );
        let (cols, rows_n) = size(&drv);
        for c in &h.covers {
            assert!(
                c.right() <= cols && c.bottom() <= rows_n,
                "{c:?} inside {cols}x{rows_n}"
            );
        }
        same_in_a_terminal(&drv);
    }
}

/// The scroll bars on the box's border beside a message field and a list take the pointer as
/// the report's does: ↓ a row, a click on the track past the thumb a page; the list's bar stays
/// where it was.
#[test]
fn x4kit_the_scroll_bars_of_a_message_and_a_list_take_clicks() {
    let items: Vec<String> = (0..10).map(|k| format!("item {k}")).collect();
    let refs: Vec<(&str, &str)> = items.iter().map(|i| (i.as_str(), "")).collect();
    let parts = vec![message("Message", 5), list(&refs, 4), buttons()];
    let mut h = Host::new("Bars", 40, parts);
    h.message = (0..10)
        .map(|k| format!("line {k}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Bars");
    let top = place(&l, "line 0");
    let border = top.x + 40 + 1;
    assert_eq!(symbol_at(&l, border, top.y + 4), "↓");
    drv.click(&mut h, at_cell(border, top.y + 4));
    let l = boxed(&drv, "Bars");
    assert!(cells::find(std::slice::from_ref(&l), "line 0").is_empty());
    assert_eq!(place(&l, "line 1").y, top.y);
    // The track's last cell, past the thumb: a page (five rows), to the end.
    drv.click(&mut h, at_cell(border, top.y + 3));
    let l = boxed(&drv, "Bars");
    assert_eq!(place(&l, "line 5").y, top.y, "{:#?}", rows(&l));
    // The list: ↓ on its bar scrolls its rows, not its bar.
    let item0 = place(&l, "item 0");
    assert_eq!(symbol_at(&l, border, item0.y + 3), "↓");
    drv.click(&mut h, at_cell(border, item0.y + 3));
    assert_eq!((h.bar.at, h.bar.top), (0, 1));
    let l = boxed(&drv, "Bars");
    assert_eq!(place(&l, "item 1").y, item0.y);
    same_in_a_terminal(&drv);
}

/// A selection in a message field — over every row (Ctrl/⌘+A), or a part of one row (Shift+→)
/// — shows in the bar's colours where it is, and the field goes on editing: typing replaces it.
#[test]
fn x4kit_a_selection_over_rows_of_a_message_shows_and_types_over() {
    let mut h = message_dialog(3, 40);
    h.message = "one\ntwo\nthree".into();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("message"));
    drv.key(&mut h, Key::A, Modifiers::COMMAND);
    let l = boxed(&drv, "Commit");
    for word in ["one", "two", "three"] {
        let at = place(&l, word);
        assert_eq!(pen_at(&l, at.x, at.y), bar_pen(), "{word}");
    }
    same_in_a_terminal(&drv);
    // Cmd/Ctrl+↑: the text's start (egui's Home is the row's).
    drv.key(&mut h, Key::ArrowUp, Modifiers::COMMAND);
    for _ in 0..2 {
        drv.key(&mut h, Key::ArrowRight, Modifiers::SHIFT);
    }
    let l = boxed(&drv, "Commit");
    let one = place(&l, "one");
    assert_eq!(pen_at(&l, one.x + 1, one.y), bar_pen());
    assert_eq!(pen_at(&l, one.x + 2, one.y), field_pen());
    drv.frame(&mut h, vec![Event::Text("새".into())], Modifiers::NONE);
    drv.idle(&mut h, 1);
    assert_eq!(h.message, "새e\ntwo\nthree");
}

/// A message of 200 KB on one line in a narrow field of three rows: a pass without input cuts
/// only the rows it shows and lays no glyph; a typed character lays the text once again, in
/// work linear in its length; the rows it shows follow the caret to the end.
#[test]
fn x4kit_a_long_message_costs_its_rows_not_its_length_a_pass() {
    let mut h = Host::new("Long", 16, vec![message("", 3)]);
    h.message = "word ".repeat(40_960);
    let len = h.message.len();
    let mut drv = Driver::new(640.0, 400.0);
    work::take();
    work::budget(Some(3 * len));
    drv.idle(&mut h, 2);
    let idle = |drv: &mut Driver, h: &mut Host| {
        work::take();
        work::budget(Some(200));
        drv.idle(h, 1);
        let done = work::take();
        assert_eq!(done.glyphs, 0, "{done:?}");
        assert!(done.cut <= 3 * 16, "{done:?}");
    };
    idle(&mut drv, &mut h);
    focus(&mut drv, &mut h, id("message"));
    idle(&mut drv, &mut h);
    work::budget(Some(3 * len));
    drv.key(&mut h, Key::ArrowDown, Modifiers::COMMAND);
    drv.frame(&mut h, vec![Event::Text("!".into())], Modifiers::NONE);
    let done = work::take();
    assert!(done.glyphs <= 2 * len && done.cut <= 3 * len, "{done:?}");
    idle(&mut drv, &mut h);
    work::budget(None);
    assert!(h.message.ends_with("word !"));
    let l = boxed(&drv, "Long");
    assert!(rows(&l).join("\n").contains("word !"), "{:#?}", rows(&l));
}

/// The wheel over a message field scrolls its rows and leaves a report's rows that scroll in
/// the same dialog where they were; over the report's rows it scrolls those.
#[test]
fn x4kit_the_wheel_over_a_message_leaves_the_reports_rows() {
    let report: Vec<(String, Ink)> = (0..30).map(|k| (format!("step {k}"), Ink::Text)).collect();
    let parts = vec![
        Part::Scroll {
            rows: report,
            most: 4,
            top: 0,
        },
        message("Message", 3),
        buttons(),
    ];
    let mut h = Host::new("Report", 40, parts);
    h.message = (0..10)
        .map(|k| format!("line {k}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    let l = boxed(&drv, "Report");
    let line0 = place(&l, "line 0");
    drv.wheel(&mut h, at_cell(line0.x + 2, line0.y), -1.0);
    assert_eq!(h.report_top, 0, "the report's rows stay");
    let l = boxed(&drv, "Report");
    assert_eq!(
        place(&l, "line 3").y,
        line0.y,
        "the message's rows scrolled"
    );
    let step0 = place(&l, "step 0");
    drv.wheel(&mut h, at_cell(step0.x + 2, step0.y), -1.0);
    assert_eq!(h.report_top, 3);
    assert_eq!(place(&boxed(&drv, "Report"), "line 3").y, line0.y);
}

// ---------------------------------------------------------------------------------------------
// The list.

fn list_dialog() -> Host {
    let names: Vec<(String, String)> = (0..10)
        .map(|k| {
            let label = if k == 3 {
                "한글 셋".to_string()
            } else {
                format!("row {k}")
            };
            let key = if k % 3 == 2 {
                String::new()
            } else {
                format!("F{k}")
            };
            (label, key)
        })
        .collect();
    let refs: Vec<(&str, &str)> = names
        .iter()
        .map(|(l, k)| (l.as_str(), k.as_str()))
        .collect();
    let parts = vec![field("Find", "a"), list(&refs, 4), buttons()];
    Host::new("Menu", 40, parts)
}

/// What the list shows: its rows' labels, top to bottom.
fn list_rows(drv: &Driver) -> Vec<String> {
    let l = boxed(drv, "Menu");
    (0..10)
        .map(|k| match k {
            3 => "한글 셋".to_string(),
            k => format!("row {k}"),
        })
        .filter(|name| !cells::find(std::slice::from_ref(&l), name).is_empty())
        .collect()
}

/// ↑ ↓ PgUp PgDn Home End move the bar while the list has the keyboard, a page its rows less
/// one; the rows it shows follow the bar; its keys stand at the right; two ↓ in one pass are
/// two rows.
#[test]
fn x4kit_the_lists_keys_move_its_bar_and_it_scrolls_with_it() {
    let mut h = list_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    assert_eq!(list_rows(&drv), ["row 0", "row 1", "row 2", "한글 셋"]);
    focus(&mut drv, &mut h, id("list"));
    let step = |drv: &mut Driver, h: &mut Host, key: Key| {
        drv.key(h, key, Modifiers::NONE);
        (h.bar.at, h.bar.top)
    };
    assert_eq!(step(&mut drv, &mut h, Key::ArrowDown), (1, 0));
    assert_eq!(step(&mut drv, &mut h, Key::ArrowDown), (2, 0));
    assert_eq!(step(&mut drv, &mut h, Key::ArrowDown), (3, 0));
    assert_eq!(step(&mut drv, &mut h, Key::ArrowDown), (4, 1));
    assert_eq!(list_rows(&drv), ["row 1", "row 2", "한글 셋", "row 4"]);
    assert_eq!(step(&mut drv, &mut h, Key::PageDown), (7, 4));
    assert_eq!(step(&mut drv, &mut h, Key::PageDown), (9, 6));
    assert_eq!(list_rows(&drv), ["row 6", "row 7", "row 8", "row 9"]);
    assert_eq!(step(&mut drv, &mut h, Key::PageUp), (6, 6));
    assert_eq!(step(&mut drv, &mut h, Key::ArrowUp), (5, 5));
    assert_eq!(step(&mut drv, &mut h, Key::Home), (0, 0));
    assert_eq!(step(&mut drv, &mut h, Key::End), (9, 6));
    assert_eq!(step(&mut drv, &mut h, Key::Home), (0, 0));
    assert_eq!(
        focused(&drv),
        Some(id("list")),
        "the keys never moved the keyboard"
    );
    // Two ↓ in one pass.
    let arrow = || down(Key::ArrowDown, Modifiers::NONE);
    drv.frame(&mut h, vec![arrow(), arrow()], Modifiers::NONE);
    assert_eq!(h.bar.at, 2);
    drv.idle(&mut h, 1);
    // The bar's row in the bar's colours across the list, its key at the right.
    let l = boxed(&drv, "Menu");
    let r1 = place(&l, "row 1");
    let r2 = place(&l, "row 2");
    assert_eq!(pen_at(&l, r2.x, r2.y), bar_pen());
    assert_eq!(pen_at(&l, r2.x + 20, r2.y), bar_pen());
    assert_eq!(pen_at(&l, r1.x, r1.y), text_pen());
    let f1 = place(&l, "F1");
    assert_eq!((f1.y, f1.right()), (r1.y, r1.x - 1 + 40 - 1));
    assert!(
        cells::find(std::slice::from_ref(&l), "F2").is_empty(),
        "row 2 has no key"
    );
    // The scroll bar on the border beside the list's rows.
    let border = r1.x - 3 + 40 + 3;
    let edge = |y: u16| symbol_at(&l, border, y);
    assert_eq!((edge(r1.y - 1), edge(r1.y + 2)), ("↑".into(), "↓".into()));
    same_in_a_terminal(&drv);
}

/// Space or Enter chooses the bar's row: in one pass with ↓, the one the bar was on at its
/// press. A click on a row chooses it and moves the bar there. The wheel scrolls the rows and
/// leaves the bar.
#[test]
fn x4kit_the_list_chooses_its_bars_row() {
    let mut h = list_dialog();
    h.takes_enter = false;
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("list"));
    let key = |k: Key| down(k, Modifiers::NONE);
    drv.frame(
        &mut h,
        vec![key(Key::ArrowDown), key(Key::Space)],
        Modifiers::NONE,
    );
    assert_eq!((std::mem::take(&mut h.chosen), h.bar.at), (vec![1], 1));
    drv.frame(
        &mut h,
        vec![key(Key::Space), key(Key::ArrowDown)],
        Modifiers::NONE,
    );
    assert_eq!((std::mem::take(&mut h.chosen), h.bar.at), (vec![1], 2));
    drv.frame(&mut h, vec![key(Key::Enter)], Modifiers::NONE);
    assert_eq!(
        std::mem::take(&mut h.chosen),
        vec![2],
        "Enter once is one choice"
    );
    drv.idle(&mut h, 1);
    assert!(h.take_clicked().contains(&id("list")));
    let l = boxed(&drv, "Menu");
    let three = place(&l, "한글 셋");
    drv.click(&mut h, at_cell(three.x + 12, three.y));
    assert_eq!((std::mem::take(&mut h.chosen), h.bar.at), (vec![3], 3));
    drv.wheel(&mut h, at_cell(three.x, three.y), -1.0);
    assert_eq!((h.bar.at, h.bar.top), (3, 3), "{:?}", list_rows(&drv));
    assert_eq!(list_rows(&drv), ["한글 셋", "row 4", "row 5", "row 6"]);
    assert!(h.chosen.is_empty());
}

/// ↓ in the first pass after Tab gave the list the keyboard moves its bar and keeps the
/// keyboard there, though egui read that ↓ as a move of the focus before the list could say it
/// keeps its arrows.
#[test]
fn x4kit_the_lists_first_arrow_after_tab_is_its_own() {
    let mut h = list_dialog();
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("a"));
    drv.frame(
        &mut h,
        vec![down(Key::Tab, Modifiers::NONE)],
        Modifiers::NONE,
    );
    assert_eq!(focused(&drv), Some(id("list")));
    drv.frame(
        &mut h,
        vec![down(Key::ArrowDown, Modifiers::NONE)],
        Modifiers::NONE,
    );
    drv.idle(&mut h, 2);
    assert_eq!((focused(&drv), h.bar.at), (Some(id("list")), 1));
}

/// A list of 20,000 rows (the branch chooser's): a pass without input, and one that moves the
/// bar, measures none of its rows and draws only the rows it shows; its part is the same rows,
/// shared, pass after pass (nothing copies them).
#[test]
fn x4kit_a_long_list_costs_its_view_not_its_length_a_pass() {
    let names: Vec<String> = (0..20_000).map(|k| format!("branch-{k}")).collect();
    let refs: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "")).collect();
    let mut h = Host::new("Branches", 40, vec![list(&refs, 8), buttons()]);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut h, 2);
    focus(&mut drv, &mut h, id("list"));
    let pass = |drv: &mut Driver, h: &mut Host, events: Vec<Event>| {
        work::take();
        work::budget(Some(100));
        drv.frame(h, events, Modifiers::NONE);
        let done = work::take();
        work::budget(None);
        assert_eq!(done.measured, 0, "{done:?}");
        assert!(done.drawn <= 8, "{done:?}");
    };
    pass(&mut drv, &mut h, vec![]);
    pass(&mut drv, &mut h, vec![down(Key::End, Modifiers::NONE)]);
    assert_eq!((h.bar.at, h.bar.top), (19_999, 19_992));
    let l = boxed(&drv, "Branches");
    assert!(rows(&l).join("\n").contains("branch-19999"));
    let Part::List { rows: shared, .. } = &h.parts[0] else {
        unreachable!()
    };
    assert_eq!(Arc::strong_count(shared), 1, "no pass keeps a copy");
}

// ---------------------------------------------------------------------------------------------
// Several fields and check boxes.

/// Two check boxes, each its own: a click turns the one clicked. The first field and check box
/// still take `Inputs::field` and `check` when the dialog gives them so (the list's dialogs),
/// a second field its text by its id.
#[test]
fn x4kit_several_fields_and_check_boxes_each_hold_their_own() {
    let parts = vec![
        field("Name", "a"),
        field("Email", "b"),
        check("Sign off", "on"),
        check("한글 확인", "also"),
        buttons(),
    ];
    for legacy in [false, true] {
        let mut h = Host::new("Fields", 40, parts.clone());
        h.legacy = legacy;
        let mut drv = Driver::new(640.0, 400.0);
        drv.idle(&mut h, 2);
        let l = boxed(&drv, "Fields");
        let also = place(&l, "[ ] 한글 확인");
        drv.click(&mut h, at_cell(also.x + 1, also.y));
        assert_eq!((h.on, h.also), (false, true), "legacy {legacy}");
        let on = place(&l, "[ ] Sign off");
        drv.click(&mut h, at_cell(on.x + 1, on.y));
        assert_eq!((h.on, h.also), (true, true), "legacy {legacy}");
        assert!(rows(&boxed(&drv, "Fields"))
            .join("\n")
            .contains("[X] 한글 확인"));
        focus(&mut drv, &mut h, id("a"));
        drv.frame(&mut h, vec![Event::Text("홍".into())], Modifiers::NONE);
        focus(&mut drv, &mut h, id("b"));
        drv.frame(&mut h, vec![Event::Text("z@".into())], Modifiers::NONE);
        drv.idle(&mut h, 1);
        assert_eq!(
            (h.a.as_str(), h.b.as_str()),
            ("홍", "z@"),
            "legacy {legacy}"
        );
        let l = boxed(&drv, "Fields");
        let b = place(&l, "z@");
        assert_eq!(pen_at(&l, b.x, b.y), field_pen());
        assert_eq!(
            l.caret,
            Some(Position::new(b.x + 2, b.y)),
            "the focused field's caret"
        );
        same_in_a_terminal(&drv);
    }
}
