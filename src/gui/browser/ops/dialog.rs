//! The list's dialogs on the cell grid (10 §4–§6, X1b): a box of whole cells on the window's
//! grid, centred with its shadow, black on cyan; its frame a double line in box glyphs with the
//! title in its top line, one blank cell inside it (10 §6's margin), the DOS shadow two cells
//! right and one down — and inside, top to bottom, its [`Part`]s: texts wrapped at its width in
//! cells, its field, a check box, a row of `[ buttons ]`, rows that scroll. It is laid out from
//! what it says and the grid alone ([`layout`]), so where it stands is known before it is drawn
//! (the list blanks the cells under it first, cells.rs's rule for one buffer over another);
//! [`show`] draws it as one buffer of cells in an egui modal at its cells. The modal keeps what
//! it did for the egui dialogs — its layer over the list, a click outside it, the keyboard kept
//! in it — and each control is an egui interaction at its cells, so focus, Tab, and Enter or
//! Space on a focused button work as egui's widgets did.
//!
//! A field ([`field`], [`field_cells`]) is egui's `TextEdit` as the editing engine only —
//! typing, the cursor, the selection, the clipboard, IME — laid at the field's cells, where it
//! paints nothing (each of its shapes is a `Noop`); the cells show what it holds: black cells,
//! the text in white, the selection in the bar's colours while it has the focus, the caret the
//! cell it stands in (10 §5, §6; in a terminal the cursor goes there). F2's field in the list's
//! row is the same field.
//!
//! The shadow's cells are black: 10 §6 has them show the cells under them in dark grey, which
//! needs the layers below, and the window keeps none outside the tests (cells.rs's recording) —
//! black is the same in both backends.
//!
//! The parts the git panel's dialogs need (X4-kit, 10 §17): a row of choices — one of several,
//! `(•) word`, or any of several, `[X] word` — wrapped by cells, each choice a control of its
//! own; a message field of several rows ([`message`], [`message_cells`]), its text wrapped by
//! cells, Shift+Enter its new line and a plain Enter the dialog's; a list with a bar, its rows'
//! keys at the right, scrolled with the bar; and several fields and check boxes in one dialog,
//! Tab going through every control in the order of the parts. Their state is the caller's,
//! given by the id of each part ([`Inputs::by_id`], [`Input`]). A chosen choice reads as its
//! mark, never as a colour: the bar's colours are the focus's (10 §6), so the two never look
//! alike, and `SOPEN_CELLS`' text rows say which is chosen; `•` is of ambiguous width, so an
//! ambiguous-wide terminal gets §10's `.` there, at the source (T38).

use crate::gui::browser::keys::Mods;
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen, Position};
use crate::gui::chrome::{self, cga, CELL};
use eframe::egui::{self, Color32, Key, Sense};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

/// The colour a text of a dialog is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// Black on cyan.
    Text,
    /// A hint: the dialog's weak colour.
    Weak,
    /// A refusal, a failure, a permanent delete: the dialog's error colour.
    Error,
}

impl Ink {
    fn pen(self) -> Pen {
        let p = chrome::PALETTE;
        let fg = match self {
            Ink::Text => p.dialog_text,
            Ink::Weak => p.dialog_weak,
            Ink::Error => p.dialog_error,
        };
        Pen::new(fg, p.dialog_back)
    }
}

/// A `[ label ]` of a dialog's row of buttons; one that is not `on` takes no focus and no click.
#[derive(Debug, Clone)]
pub struct Button {
    pub label: String,
    pub id: egui::Id,
    pub on: bool,
}

impl Button {
    pub fn new(label: &str, id: egui::Id) -> Self {
        Self {
            label: label.to_string(),
            id,
            on: true,
        }
    }
}

/// What a dialog's inside holds, top to bottom, before it is laid out.
#[derive(Debug, Clone)]
pub enum Part {
    /// A text in its ink, wrapped at the dialog's width.
    Text(String, Ink),
    /// A text that holds the keyboard for a dialog without a field (the delete dialog's
    /// question): an egui focus holder over its cells.
    Held(String, Ink, egui::Id),
    Blank,
    /// The dialog's field, `label` before it (empty: the field takes the row).
    Field {
        label: String,
        id: egui::Id,
    },
    /// A check box, `[X] label` or `[ ] label`.
    Check {
        label: String,
        id: egui::Id,
    },
    /// Buttons, two blank cells apart.
    Buttons(Vec<Button>),
    /// Rows that scroll (the report's): at most `most` on screen, from row `top`; each wrapped.
    Scroll {
        rows: Vec<(String, Ink)>,
        most: u16,
        top: usize,
    },
    /// A row of choices after `label` (empty: none), two blank cells apart and wrapped by cells
    /// under the first: one of several (`(•) word`, its [`Input::One`]) or, `many`, any of
    /// several (`[X] word`, its [`Input::Any`]). Each choice is a control of its own
    /// ([`choice_id`]): a click, or Space or Enter while it has the keyboard, chooses it — or
    /// turns it on or off — once a press.
    #[cfg_attr(not(test), allow(dead_code))] // X4-dialogs uses it; the tests till then.
    Choices {
        label: String,
        id: egui::Id,
        choices: Vec<String>,
        many: bool,
    },
    /// A message field ([`Input::Text`]): `label` on a row of its own (empty: none), then `rows`
    /// rows of black cells, its text wrapped by cells; past them it scrolls with the caret,
    /// ↑ ░ ↓ on the box's right border. Shift+Enter is a new line; a plain Enter is never
    /// typed, it is the dialog's — which takes it with exactly no modifiers (`keys::take`'s
    /// `Mods::None`): egui's `consume_key(Modifiers::NONE, …)` would take Shift+Enter too.
    #[cfg_attr(not(test), allow(dead_code))] // X4-dialogs uses it; the tests till then.
    Message {
        label: String,
        id: egui::Id,
        rows: u16,
    },
    /// A list with a bar ([`Input::List`]): at most `most` rows on screen, each its label at the
    /// left and its key at the right, a blank cell at either end; one control. ↑ ↓ PgUp PgDn
    /// Home End move the bar while it has the keyboard, the rows it shows following the bar;
    /// a click on a row, or Space or Enter, chooses the bar's row ([`Shown::chosen`]).
    #[cfg_attr(not(test), allow(dead_code))] // X4-menu uses it; the tests till then.
    List {
        rows: Vec<Row>,
        id: egui::Id,
        most: u16,
    },
}

/// A row of a [`Part::List`]: its label, and its key at the right (empty: none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub key: String,
}

impl Row {
    #[cfg_attr(not(test), allow(dead_code))] // X4-menu uses it; the tests till then.
    pub fn new(label: &str, key: &str) -> Self {
        Self {
            label: label.to_string(),
            key: key.to_string(),
        }
    }
}

/// Where a list stands: the row its bar is on and the first row it shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bar {
    pub at: usize,
    pub top: usize,
}

/// The id of choice `k` of the row of choices `row` (its focus and its click).
pub fn choice_id(row: egui::Id, k: usize) -> egui::Id {
    row.with(("sopen-choice", k))
}

/// A part at its cells.
#[derive(Debug, Clone)]
enum Placed {
    Text(Position, String, Pen),
    Held(Cells, egui::Id),
    Field(Cells, egui::Id),
    Check(Cells, String, egui::Id),
    Button(Cells, Button),
    Scroll {
        cells: Cells,
        rows: Vec<(String, Pen)>,
        top: usize,
    },
    /// Choice `k` of the row of choices `row`: its cells and its word.
    Choice {
        cells: Cells,
        word: String,
        row: egui::Id,
        k: usize,
        many: bool,
    },
    Message(Cells, egui::Id),
    List {
        cells: Cells,
        rows: Vec<Row>,
        id: egui::Id,
    },
}

/// A dialog laid out on the window's grid: its box, its title and its parts at their cells.
#[derive(Debug, Clone)]
pub struct Layout {
    grid: Grid,
    frame: Cells,
    title: String,
    placed: Vec<Placed>,
}

/// The whole window's grid, for a dialog shown with no title or status row around it (the
/// tests' hosts); the browser gives the area between them ([`super::Ops::dialogs`]).
pub fn grid(ctx: &egui::Context) -> Grid {
    Grid::over(ctx.content_rect())
}

/// `text` in rows of at most `room` cells: whole when it fits; else broken at its last blank that
/// fits (the blank dropped), or — a word longer than the row — after its last whole glyph.
pub fn wrap(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut rows = vec![];
    let mut rest = text;
    // After a break, blanks alone — the text's own trailing ones — make no row, however wide.
    while cells::width(rest) > room && (rows.is_empty() || !rest.trim().is_empty()) {
        let (mut used, mut cut, mut blank) = (0, 0, None);
        for (i, g) in rest.grapheme_indices(true) {
            let w = cells::width(g);
            if g == " " && i > 0 {
                blank = Some(i);
            }
            if used + w > room {
                break;
            }
            used += w;
            cut = i + g.len();
        }
        let (row, next) = match blank {
            Some(b) if b <= cut => (&rest[..b], &rest[b + 1..]),
            // A glyph wider than the row alone: it goes on a row of its own, cut there.
            _ if cut == 0 => {
                let first = rest.graphemes(true).next().map_or(rest.len(), str::len);
                (&rest[..first], &rest[first..])
            }
            _ => (&rest[..cut], &rest[cut..]),
        };
        rows.push(row.to_string());
        rest = next;
    }
    if !rest.trim().is_empty() || rows.is_empty() {
        rows.push(rest.to_string());
    }
    rows
}

/// How many cells a part wants in a row to stand whole.
fn wants(part: &Part) -> usize {
    let w = cells::width;
    match part {
        Part::Text(text, _) | Part::Held(text, _, _) => w(text),
        Part::Blank => 0,
        Part::Field { label, .. } if label.is_empty() => FIELD,
        Part::Field { label, .. } => w(label) + 1 + FIELD,
        Part::Check { label, .. } => w(label) + 4,
        Part::Buttons(all) => {
            let each: usize = all.iter().map(|b| w(&b.label) + 4).sum();
            each + 2 * all.len().saturating_sub(1)
        }
        Part::Scroll { rows, .. } => rows.iter().map(|(t, _)| w(t)).max().unwrap_or(0),
        Part::Choices { label, choices, .. } => {
            let each: usize = choices.iter().map(|c| w(c) + 4).sum();
            lead(label) + each + 2 * choices.len().saturating_sub(1)
        }
        Part::Message { label, .. } => w(label).max(FIELD),
        Part::List { rows, .. } => rows.iter().map(row_wants).max().unwrap_or(0),
    }
}

/// The fewest cells a field has.
const FIELD: usize = 12;

/// The cells a row of choices' label takes before its first choice: the label and a blank.
fn lead(label: &str) -> usize {
    match cells::width(label) {
        0 => 0,
        w => w + 1,
    }
}

/// How many cells a list's row wants: a blank, its label, two blanks and its key, a blank.
fn row_wants(row: &Row) -> usize {
    let w = cells::width;
    match w(&row.key) {
        0 => w(&row.label) + 2,
        key => w(&row.label) + 2 + key + 2,
    }
}

/// Where each choice of a row after `label` stands in a dialog `width` cells wide: its row, its
/// first cell and its cells (`(•) ` and its word). Two blank cells apart; one that does not fit
/// in what is left of a row starts the next, under the first choice — or at the row's start,
/// where the label leaves no room there for the widest. A choice wider than a row is cut.
fn choice_places(label: &str, choices: &[String], width: usize) -> Vec<(usize, usize, usize)> {
    let lead = lead(label);
    let wants = |c: &String| cells::width(c) + 4;
    let widest = choices.iter().map(wants).max().unwrap_or(0);
    let under = if lead + widest <= width { lead } else { 0 };
    let (mut row, mut x) = (0, lead);
    let mut out = vec![];
    for c in choices {
        let w = wants(c);
        // Something stands before it in its row (the label, a choice): it may go to the next.
        let taken = if row == 0 { x > 0 } else { x > under };
        if x + w > width && taken {
            row += 1;
            x = under;
        }
        out.push((row, x, w.min(width.saturating_sub(x))));
        x += w + 2;
    }
    out
}

/// `row` cut to end in "…" within `room` cells: a text with rows below it that do not fit.
fn cut_short(row: &str, room: usize) -> String {
    let mut s = cells::fit_start(row, room.saturating_sub(1));
    if !s.ends_with('…') {
        s.push('…');
    }
    s
}

/// The most cells a dialog's inside takes on a wide window, unless its nominal width is more: a
/// longer text wraps by cells — a box as wide as the window buys nothing where a path still
/// wraps, and looks loose.
const WIDEST: usize = 76;

/// How many cells wide a dialog's inside is on a grid `cols` wide when its texts want `wanted`:
/// `nominal` at least, at most [`WIDEST`] or `nominal` (the larger), and never more than the grid
/// less eight (the box's border and margin, its shadow, a cell clear at either side). The key
/// help's box is as wide by the same rule (`gui::help`).
pub fn inside_width(cols: u16, nominal: usize, wanted: usize) -> usize {
    let room = usize::from(cols.saturating_sub(8)).max(1);
    wanted
        .max(nominal)
        .min(nominal.max(WIDEST.min(room)))
        .min(room)
}

/// A box `w` × `h` cells centred on `grid` with its shadow (two cells right, one down).
pub fn centred(grid: Grid, w: u16, h: u16) -> Cells {
    let x = grid.cols.saturating_sub(w.saturating_add(2)) / 2;
    let y = grid.rows.saturating_sub(h.saturating_add(1)) / 2;
    Cells::new(x, y, w, h)
}

/// The two strips of the shadow of the box `frame`: right of it from its second row, under it
/// from its third column.
pub fn shadow_of(frame: Cells) -> [Cells; 2] {
    let f = frame;
    [
        Cells::new(f.right(), f.y + 1, 2, f.height),
        Cells::new(f.x + 2, f.bottom(), f.width, 1),
    ]
}

/// The shadow of the box `frame` in `buf`: black cells (see the module's header).
pub fn shadow(buf: &mut Buffer, frame: Cells) {
    let pen = Pen::new(cga::DARK_GRAY, cga::BLACK);
    for strip in shadow_of(frame) {
        cells::fill(buf, strip, pen);
    }
}

/// The dialog titled `title` with `parts`, on `grid`: as wide as its widest part wants, by
/// [`inside_width`]'s rule, and as tall as its parts; where the grid is too short, the rows that
/// scroll give way first, then the tallest text loses its last rows ("…"). Centred with its
/// shadow ([`centred`]).
pub fn layout(grid: Grid, title: &str, nominal: usize, parts: &[Part]) -> Layout {
    let wanted = (parts.iter().map(wants))
        .chain([nominal, cells::width(title) + 2])
        .max()
        .unwrap_or(nominal);
    let width = inside_width(grid.cols, nominal, wanted);
    // Each part's rows: texts wrapped, rows that scroll as many as they may show.
    let mut texts: Vec<Vec<String>> = (parts.iter())
        .map(|part| match part {
            Part::Text(text, _) | Part::Held(text, _, _) => wrap(text, width),
            _ => vec![],
        })
        .collect();
    let scrolled: Vec<(String, Ink)> = (parts.iter())
        .filter_map(|part| match part {
            Part::Scroll { rows, .. } => Some(rows),
            _ => None,
        })
        .flatten()
        .flat_map(|(text, ink)| wrap(text, width).into_iter().map(|row| (row, *ink)))
        .collect();
    let most = (parts.iter())
        .find_map(|part| match part {
            Part::Scroll { most, .. } => Some(usize::from(*most)),
            _ => None,
        })
        .unwrap_or(0);
    let mut shown = scrolled.len().min(most);
    let places: Vec<Vec<(usize, usize, usize)>> = (parts.iter())
        .map(|part| match part {
            Part::Choices { label, choices, .. } => choice_places(label, choices, width),
            _ => vec![],
        })
        .collect();
    // The rows each list and message field shows; they give way with the rows that scroll.
    let mut give: Vec<usize> = (parts.iter())
        .map(|part| match part {
            Part::List { rows, most, .. } => rows.len().min(usize::from(*most)).max(1),
            Part::Message { rows, .. } => usize::from(*rows).max(1),
            _ => 0,
        })
        .collect();
    let fixed = |texts: &[Vec<String>]| -> usize {
        (parts.iter().zip(texts).zip(&places))
            .map(|((part, rows), places)| match part {
                Part::Text(..) | Part::Held(..) => rows.len(),
                Part::Scroll { .. } | Part::List { .. } => 0,
                Part::Message { label, .. } => usize::from(!label.is_empty()),
                Part::Choices { .. } => places.last().map_or(1, |p| p.0 + 1),
                _ => 1,
            })
            .sum()
    };
    // The box is its rows, two border rows and two margin rows; its shadow one more.
    let fits = usize::from(grid.rows).saturating_sub(5);
    // The rows that scroll, a list's and a message field's give way first, the most first,
    // down to one each.
    loop {
        if fixed(&texts) + shown + give.iter().sum::<usize>() <= fits {
            break;
        }
        let big = (0..give.len())
            .filter(|&i| give[i] > 1)
            .max_by_key(|&i| give[i]);
        match big {
            Some(i) if give[i] > shown || shown <= 1 => give[i] -= 1,
            _ if shown > 1 => shown -= 1,
            _ => break,
        }
    }
    let shown_with = |shown: usize| shown + give.iter().sum::<usize>();
    while fixed(&texts) + shown_with(shown) > fits {
        let Some(tallest) = (texts.iter_mut())
            .filter(|rows| rows.len() > 1)
            .max_by_key(|rows| rows.len())
        else {
            break;
        };
        tallest.pop();
        if let Some(last) = tallest.last_mut() {
            *last = cut_short(last, width);
        }
    }
    let inside = fixed(&texts) + shown_with(shown);
    let frame = centred(grid, width as u16 + 4, inside as u16 + 4);
    let (x0, mut y) = (frame.x + 2, frame.y + 2);
    let width = width as u16;
    let mut placed = vec![];
    let label_at = |placed: &mut Vec<Placed>, y: u16, label: &str| {
        let label = cells::fit_start(label, usize::from(width));
        placed.push(Placed::Text(Position::new(x0, y), label, Ink::Text.pen()));
    };
    for (i, (part, rows)) in parts.iter().zip(texts).enumerate() {
        match part {
            Part::Text(_, ink) | Part::Held(_, ink, _) => {
                if let Part::Held(_, _, id) = part {
                    placed.push(Placed::Held(
                        Cells::new(x0, y, width, rows.len() as u16),
                        *id,
                    ));
                }
                for row in rows {
                    placed.push(Placed::Text(Position::new(x0, y), row, ink.pen()));
                    y += 1;
                }
                continue;
            }
            Part::Blank => {}
            Part::Field { label, id } => {
                let mut at = x0;
                if !label.is_empty() {
                    let label = cells::fit_start(label, usize::from(width));
                    at += cells::width(&label) as u16 + 1;
                    placed.push(Placed::Text(Position::new(x0, y), label, Ink::Text.pen()));
                }
                let end = x0 + width;
                placed.push(Placed::Field(
                    Cells::new(at.min(end), y, end.saturating_sub(at), 1),
                    *id,
                ));
            }
            Part::Check { label, id } => {
                let w = (cells::width(label) as u16 + 4).min(width);
                placed.push(Placed::Check(Cells::new(x0, y, w, 1), label.clone(), *id));
            }
            Part::Buttons(all) => {
                let mut at = x0;
                for b in all {
                    let w = cells::width(&b.label) as u16 + 4;
                    let w = w.min((x0 + width).saturating_sub(at));
                    placed.push(Placed::Button(Cells::new(at, y, w, 1), b.clone()));
                    at = at.saturating_add(w + 2);
                }
            }
            Part::Scroll { top, .. } => {
                let max_top = scrolled.len().saturating_sub(shown);
                placed.push(Placed::Scroll {
                    cells: Cells::new(x0, y, width, shown as u16),
                    rows: scrolled
                        .iter()
                        .map(|(t, ink)| (t.clone(), ink.pen()))
                        .collect(),
                    top: (*top).min(max_top),
                });
                y += shown as u16;
                continue;
            }
            Part::Choices {
                label,
                id,
                choices,
                many,
            } => {
                if !label.is_empty() {
                    label_at(&mut placed, y, label);
                }
                for (k, (word, &(row, x, w))) in choices.iter().zip(&places[i]).enumerate() {
                    placed.push(Placed::Choice {
                        cells: Cells::new(x0 + x as u16, y + row as u16, w as u16, 1),
                        word: word.clone(),
                        row: *id,
                        k,
                        many: *many,
                    });
                }
                y += places[i].last().map_or(1, |p| p.0 as u16 + 1);
                continue;
            }
            Part::Message { label, id, .. } => {
                if !label.is_empty() {
                    label_at(&mut placed, y, label);
                    y += 1;
                }
                placed.push(Placed::Message(
                    Cells::new(x0, y, width, give[i] as u16),
                    *id,
                ));
                y += give[i] as u16;
                continue;
            }
            Part::List { rows, id, .. } => {
                placed.push(Placed::List {
                    cells: Cells::new(x0, y, width, give[i] as u16),
                    rows: rows.clone(),
                    id: *id,
                });
                y += give[i] as u16;
                continue;
            }
        }
        y += 1;
    }
    Layout {
        grid,
        frame,
        title: title.to_string(),
        placed,
    }
}

impl Layout {
    /// The box, and its shadow's two strips: right of it from its second row, under it from its
    /// third column.
    fn rects(&self) -> [Cells; 3] {
        let [right, under] = shadow_of(self.frame);
        [self.frame, right, under]
    }

    /// The box and its shadow, cut to the grid.
    pub fn covers(&self) -> Vec<Cells> {
        let all = self.grid.all();
        (self.rects().into_iter())
            .map(|r| r.intersection(all))
            .collect()
    }
}

/// What a dialog's controls hold while it is shown: its field's text, its check box, the first
/// row its rows that scroll show — and every other control's, by the id of its part.
#[derive(Default)]
pub struct Inputs<'a> {
    /// The first field's text, unless [`Inputs::by_id`] names it.
    pub field: Option<&'a mut String>,
    /// The first check box, unless [`Inputs::by_id`] names it.
    pub check: Option<&'a mut bool>,
    pub top: Option<&'a mut usize>,
    /// Each control's state by the id of its part: several fields and check boxes in one
    /// dialog, its rows of choices, its message fields and its lists (X4-kit).
    pub by_id: Vec<(egui::Id, Input<'a>)>,
}

/// A control's state while its dialog is shown ([`Inputs::by_id`]).
#[cfg_attr(not(test), allow(dead_code))] // X4-dialogs uses it; the tests till then.
pub enum Input<'a> {
    /// A field's or a message field's text.
    Text(&'a mut String),
    /// A check box.
    Check(&'a mut bool),
    /// A row of one of several: the chosen choice, if one is.
    One(&'a mut Option<usize>),
    /// A row of any of several: each choice on or off.
    Any(&'a mut [bool]),
    /// A list: where it stands.
    List(&'a mut Bar),
}

/// The text `by_id` gives the part `id`.
fn text_of<'b>(by_id: &'b mut [(egui::Id, Input<'_>)], id: egui::Id) -> Option<&'b mut String> {
    match by_id.iter_mut().find(|(k, _)| *k == id) {
        Some((_, Input::Text(text))) => Some(&mut **text),
        _ => None,
    }
}

/// The check box `by_id` gives the part `id`.
fn check_of<'b>(by_id: &'b mut [(egui::Id, Input<'_>)], id: egui::Id) -> Option<&'b mut bool> {
    match by_id.iter_mut().find(|(k, _)| *k == id) {
        Some((_, Input::Check(on))) => Some(&mut **on),
        _ => None,
    }
}

/// What a pass of a dialog did.
pub struct Shown {
    pub modal: egui::ModalResponse<()>,
    /// The buttons and the check box clicked — Enter or Space on the focused one too; each
    /// choice chosen, each list a row of which was.
    clicked: Vec<egui::Id>,
    /// Each list's rows chosen, in order.
    chosen: Vec<(egui::Id, usize)>,
    /// Its controls (the fields, the check boxes, the choices, the message fields, the lists,
    /// the buttons): the ones that may hold the focus.
    pub controls: Vec<egui::Id>,
}

impl Shown {
    pub fn clicked(&self, id: egui::Id) -> bool {
        self.clicked.contains(&id)
    }

    /// The rows of the list `id` chosen this pass, in order: a click on one, or Space or Enter
    /// on its bar — each the row the bar was on then.
    #[cfg_attr(not(test), allow(dead_code))] // X4-menu uses it; the tests till then.
    pub fn chosen(&self, id: egui::Id) -> Vec<usize> {
        (self.chosen.iter())
            .filter(|(list, _)| *list == id)
            .map(|&(_, row)| row)
            .collect()
    }
}

/// The dialog's frame in `r`: filled black on cyan, its border a double line, `title` centred in
/// the top line with a blank either side — cut as [`cells::frame`] cuts a title.
fn frame(buf: &mut Buffer, r: Cells, title: &str) {
    let pen = Ink::Text.pen();
    cells::fill(buf, r, pen);
    if r.width < 2 || r.height < 2 {
        return;
    }
    let (l, rt, t, b) = (r.left(), r.right() - 1, r.top(), r.bottom() - 1);
    let glyph = |buf: &mut Buffer, x: u16, y: u16, g: &str| {
        cells::put(buf, x, y, g, 1, pen);
    };
    for x in l + 1..rt {
        glyph(buf, x, t, "═");
        glyph(buf, x, b, "═");
    }
    for y in t + 1..b {
        glyph(buf, l, y, "║");
        glyph(buf, rt, y, "║");
    }
    for (x, y, corner) in [(l, t, "╔"), (rt, t, "╗"), (l, b, "╚"), (rt, b, "╝")] {
        glyph(buf, x, y, corner);
    }
    let shown = cells::fit_left(title, usize::from(r.width).saturating_sub(6));
    if !shown.is_empty() {
        let w = cells::width(&shown) as u16;
        let x = l + (r.width - w) / 2;
        cells::fill(buf, Cells::new(x - 1, t, w + 2, 1), pen);
        cells::put(buf, x, t, &shown, w.into(), pen);
    }
}

/// A button's or a check box's pen: the bar's when it has the focus, the weak colour when off.
fn control_pen(ctx: &egui::Context, id: egui::Id, on: bool) -> Pen {
    let p = chrome::PALETTE;
    if !on {
        Ink::Weak.pen()
    } else if ctx.memory(|m| m.has_focus(id)) {
        Pen::new(p.bar_text, p.bar)
    } else {
        Ink::Text.pen()
    }
}

/// The wheel's rows this pass: three a notch, as the list scrolls (list_cells.rs).
fn wheel_rows(ctx: &egui::Context) -> f32 {
    ctx.input(|i| {
        (i.events.iter())
            .map(|e| match e {
                egui::Event::MouseWheel { unit, delta, .. } => match unit {
                    egui::MouseWheelUnit::Point => -delta.y / CELL.y,
                    _ => -delta.y * 3.0,
                },
                _ => 0.0,
            })
            .sum()
    })
}

/// The scroll bar at cells `bar` (a column: ↑, the track, ↓) of rows that show from `first` of
/// `max_top + 1` places, as egui's scroll area took the pointer: ↑ and ↓ a row, a click on the
/// track a page towards it, the track dragged — the thumb, or anywhere on it — the place under
/// the pointer. Never focusable: Tab's order is the dialog's controls'.
fn scroll_bar_input(
    ui: &mut egui::Ui,
    grid: &Grid,
    bar: Cells,
    id: egui::Id,
    first: usize,
    max_top: usize,
) -> usize {
    let one = |y: u16| grid.rect(Cells::new(bar.x, y, 1, 1));
    let (top_y, end_y) = (bar.y, bar.bottom() - 1);
    let mut first = first;
    if bar.height < 3 {
        return first;
    }
    let up = ui.interact(one(top_y), id.with("scroll-up"), Sense::CLICK);
    let down = ui.interact(one(end_y), id.with("scroll-down"), Sense::CLICK);
    let track_cells = Cells::new(bar.x, top_y + 1, 1, bar.height - 2);
    let track = grid.rect(track_cells);
    let sense = Sense::CLICK | Sense::DRAG;
    let on_track = ui.interact(track, id.with("scroll-track"), sense);
    if up.clicked() {
        first = first.saturating_sub(1);
    }
    if down.clicked() {
        first = (first + 1).min(max_top);
    }
    // The thumb's cell on the track, as `cells::scroll_bar` draws it.
    let room = f32::from(bar.height - 3);
    let thumb = |first: usize| (room * first as f32 / max_top as f32).round();
    let row_of = |p: egui::Pos2| ((p.y - track.top()) / CELL.y).floor().clamp(0.0, room);
    let page = usize::from(bar.height);
    if on_track.dragged() {
        if let Some(p) = on_track.interact_pointer_pos() {
            first = (row_of(p) / room.max(1.0) * max_top as f32).round() as usize;
        }
    } else if on_track.clicked() {
        if let Some(p) = on_track.interact_pointer_pos() {
            let (row, at) = (row_of(p), thumb(first));
            if row < at {
                first = first.saturating_sub(page);
            } else if row > at {
                first = (first + page).min(max_top);
            }
        }
    }
    first.min(max_top)
}

/// The keys a list takes while it has the keyboard.
const LIST_KEYS: [Key; 8] = [
    Key::ArrowUp,
    Key::ArrowDown,
    Key::PageUp,
    Key::PageDown,
    Key::Home,
    Key::End,
    Key::Space,
    Key::Enter,
];

/// The presses of `wanted` (with no modifiers) before the pass's first Tab — or, `after`, after
/// it — taken out of its input, in order; a Space's text with it (egui-winit sends the key and
/// its text in one pass), so no field the keyboard reaches in the pass types it.
fn take_presses(ctx: &egui::Context, wanted: &[Key], after: bool) -> Vec<Key> {
    ctx.input_mut(|i| {
        let (mut out, mut tab, mut typed) = (vec![], false, false);
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (egui::Event::Text(text), true) if text == " " => false,
            (e, _) => match e {
                egui::Event::Key {
                    key: Key::Tab,
                    pressed: true,
                    ..
                } => {
                    tab = true;
                    true
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if tab == after && wanted.contains(key) && Mods::None.matches(*modifiers) => {
                    out.push(*key);
                    typed = *key == Key::Space;
                    false
                }
                _ => true,
            },
        });
        out
    })
}

/// Whether the pass has a Tab pressed.
fn tab_pressed(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        (i.events.iter()).any(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: Key::Tab,
                    pressed: true,
                    ..
                }
            )
        })
    })
}

/// The presses of `wanted` this pass that are the control `id`'s, in order, and its egui
/// interaction (`interact`, which moves the keyboard on a Tab): while it has the keyboard as its
/// turn comes, those before the pass's first Tab; once that Tab gives it the keyboard, those
/// after. Taken out of the input before egui's own Space and Enter click could see them (one
/// flag a pass, whatever their order and number), so a click the pass reports is the pointer's.
fn presses(
    ctx: &egui::Context,
    id: egui::Id,
    wanted: &[Key],
    interact: impl FnOnce() -> egui::Response,
) -> (Vec<Key>, egui::Response) {
    let had = ctx.memory(|m| m.has_focus(id));
    let mut keys = if had {
        take_presses(ctx, wanted, false)
    } else {
        vec![]
    };
    let response = interact();
    if !had && ctx.memory(|m| m.has_focus(id)) {
        keys.extend(take_presses(ctx, wanted, true));
    }
    (keys, response)
}

/// The text `by_id` gives the part `id`, to read.
fn text_value<'b>(by_id: &'b [(egui::Id, Input<'_>)], id: egui::Id) -> Option<&'b str> {
    match by_id.iter().find(|(k, _)| *k == id) {
        Some((_, Input::Text(text))) => Some(text.as_str()),
        _ => None,
    }
}

/// The check box `by_id` gives the part `id`, to read.
fn check_value(by_id: &[(egui::Id, Input<'_>)], id: egui::Id) -> Option<bool> {
    match by_id.iter().find(|(k, _)| *k == id) {
        Some((_, Input::Check(on))) => Some(**on),
        _ => None,
    }
}

/// The list's rows at `cells` of `buf`, from the bar's `top`: a blank, the label, the key at the
/// right and a blank; the bar's row in the bar's colours; ↑ ░ ↓ in column `border` beside them
/// when there are more.
fn list_cells(buf: &mut Buffer, cells: Cells, rows: &[Row], bar: Bar, border: u16) {
    let p = chrome::PALETTE;
    let width = usize::from(cells.width);
    let shown = usize::from(cells.height);
    for (k, row) in rows.iter().enumerate().skip(bar.top).take(shown) {
        let y = cells.y + (k - bar.top) as u16;
        let pen = match k == bar.at {
            true => Pen::new(p.bar_text, p.bar),
            false => Ink::Text.pen(),
        };
        cells::fill(buf, Cells::new(cells.x, y, cells.width, 1), pen);
        let key = cells::width(&row.key);
        let room = match key {
            0 => width.saturating_sub(2),
            _ => width.saturating_sub(key + 4),
        };
        let label = cells::fit_start(&row.label, room);
        cells::put(buf, cells.x + 1, y, &label, room, pen);
        if key > 0 && key + 2 <= width {
            cells::put_right(buf, cells.right() - 1, y, &row.key, pen);
        }
    }
    let max_top = rows.len().saturating_sub(shown);
    if max_top > 0 {
        let at = bar.top as f32 / max_top as f32;
        cells::scroll_bar(buf, border, cells.y, cells.height, Some(at));
    }
}

/// One pass of the list `list` of `n` rows at cells `at.0` of `grid`, where it stands `bar`: its
/// keys while it has the keyboard ([`presses`]; a page its rows less one, as the tree's), a click
/// on a row, the wheel over it and its scroll bar at `at.1` (ids under the dialog's `salt`), the
/// rows it shows following the bar when the bar moved. The rows chosen, in order.
fn list_pass(
    ui: &mut egui::Ui,
    grid: &Grid,
    at: (Cells, Cells),
    list: egui::Id,
    n: usize,
    bar: &mut Bar,
    salt: egui::Id,
) -> Vec<usize> {
    let ctx = ui.ctx().clone();
    let (cells, column) = at;
    let shown = usize::from(cells.height);
    let rect = grid.rect(cells);
    // While it has the keyboard, its arrows are its own: egui moves the focus by them otherwise.
    let arrows = egui::EventFilter {
        vertical_arrows: true,
        ..Default::default()
    };
    ctx.memory_mut(|m| m.set_focus_lock_filter(list, arrows));
    let interact = || ui.interact(rect, list, Sense::click());
    let (keys, response) = presses(&ctx, list, &LIST_KEYS, interact);
    let (last, page) = (n.saturating_sub(1), shown.saturating_sub(1).max(1));
    bar.at = bar.at.min(last);
    let (mut chosen, mut moved, mut arrow) = (vec![], false, false);
    for key in keys {
        let to = match key {
            Key::ArrowUp => bar.at.saturating_sub(1),
            Key::ArrowDown => (bar.at + 1).min(last),
            Key::PageUp => bar.at.saturating_sub(page),
            Key::PageDown => (bar.at + page).min(last),
            Key::Home => 0,
            Key::End => last,
            // Space, Enter.
            _ => {
                if n > 0 {
                    chosen.push(bar.at);
                }
                continue;
            }
        };
        arrow |= matches!(key, Key::ArrowUp | Key::ArrowDown);
        (bar.at, moved) = (to, true);
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        let row = (response.interact_pointer_pos())
            .map(|p| ((p.y - rect.top()) / CELL.y).floor().max(0.0) as usize);
        if let Some(k) = row.map(|r| bar.top + r).filter(|k| *k < n) {
            (bar.at, moved) = (k, true);
            chosen.push(k);
        }
        response.request_focus();
    }
    // egui read this pass's arrow as a move of the focus before the list could keep it (its
    // first pass with the keyboard): no move. A Tab's is egui's.
    if arrow && !tab_pressed(&ctx) {
        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
    let max_top = n.saturating_sub(shown);
    if moved && bar.at < bar.top {
        bar.top = bar.at;
    } else if moved && bar.at >= bar.top + shown {
        bar.top = bar.at + 1 - shown;
    }
    if ui.rect_contains_pointer(rect) {
        let moved = (bar.top as f32 + wheel_rows(&ctx)).round();
        bar.top = moved.clamp(0.0, max_top as f32) as usize;
    }
    if max_top > 0 {
        bar.top = scroll_bar_input(ui, grid, column, salt.with(list), bar.top, max_top);
    }
    bar.top = bar.top.min(max_top);
    chosen
}

/// Shows `layout` as a modal `id` at its cells for one pass: one buffer of cells — the shadow,
/// the frame, the texts — and each control an egui interaction at its cells, in the order of the
/// parts (Tab's order); the fields' engines at their cells; the wheel over the box and the scroll
/// bar ([`scroll_bar_input`]) scroll its rows — over a list or a message field, those.
/// Painted once every control has had its say, so the focus it shows is this pass's.
pub fn show(ctx: &egui::Context, id: egui::Id, layout: &Layout, inputs: Inputs) -> Shown {
    let Inputs {
        mut field,
        mut check,
        mut top,
        mut by_id,
    } = inputs;
    // The first field and check box `by_id` does not name take `field` and `check`.
    let named = |id: egui::Id| by_id.iter().any(|(k, _)| *k == id);
    let first_field = (layout.placed.iter()).find_map(|p| match p {
        Placed::Field(_, id) if !named(*id) => Some(*id),
        _ => None,
    });
    let first_check = (layout.placed.iter()).find_map(|p| match p {
        Placed::Check(_, _, id) if !named(*id) => Some(*id),
        _ => None,
    });
    // The lists' and message fields' cells: the wheel over them is theirs.
    let own_wheel: Vec<Cells> = (layout.placed.iter())
        .filter_map(|p| match p {
            Placed::Message(cells, _) | Placed::List { cells, .. } => Some(*cells),
            _ => None,
        })
        .collect();
    let grid = layout.grid;
    // The box, cut to the grid: on a grid too small for it, nothing of it stands outside.
    let body = grid.rect(layout.frame.intersection(grid.all()));
    let at = body.min - ctx.content_rect().min;
    let area = egui::Modal::default_area(id)
        .anchor(egui::Align2::LEFT_TOP, at)
        .fade_in(false)
        .constrain(false);
    let mut clicked = vec![];
    let mut chosen = vec![];
    let mut controls = vec![];
    let modal = egui::Modal::new(id)
        .area(area)
        .frame(egui::Frame::NONE)
        .backdrop_color(Color32::TRANSPARENT)
        .show(ctx, |ui| {
            ui.allocate_rect(body, Sense::hover());
            let [frame_cells, right, under] = layout.rects();
            let area = frame_cells.union(right).union(under);
            let mut buf = Buffer::empty(area.intersection(grid.all()));
            shadow(&mut buf, frame_cells);
            frame(&mut buf, frame_cells, &layout.title);
            // The box's right border: a list's or a message field's scroll bar stands there.
            let border = frame_cells.right() - 1;
            let mut fields_at = vec![];
            let mut messages_at = vec![];
            let mut lists_at = vec![];
            // A control's cells on the grid: one with none there (a grid smaller than the dialog)
            // is not there to click, nor its field to hold the keyboard.
            let visible = |c: &Cells| Some(c.intersection(grid.all())).filter(|v| !v.is_empty());
            for placed in &layout.placed {
                match placed {
                    Placed::Text(at, text, pen) => {
                        let room = usize::from(frame_cells.right().saturating_sub(at.x + 2));
                        // The blanks after it in its pen too: one run, which a terminal that
                        // lays a glyph of ambiguous width in two cells moves into them (10 §10)
                        // where the frame's colours would have cut it (a hint's `·`, a note's
                        // `—`).
                        let row = Cells::new(at.x, at.y, room as u16, 1);
                        cells::fill(&mut buf, row, *pen);
                        cells::put(&mut buf, at.x, at.y, text, room, *pen);
                    }
                    Placed::Held(cells, id) => {
                        if let Some(cells) = visible(cells) {
                            let sense = Sense::focusable_noninteractive();
                            ui.interact(grid.rect(cells), *id, sense);
                        }
                    }
                    Placed::Field(cells, id) => {
                        let text = match Some(*id) == first_field {
                            true => field.as_deref_mut(),
                            false => text_of(&mut by_id, *id),
                        };
                        if let (Some(text), Some(cells)) = (text, visible(cells)) {
                            self::field(ui, &grid, cells, *id, text, |e| e);
                            controls.push(*id);
                            fields_at.push((cells, *id));
                        }
                    }
                    Placed::Check(cells, _, id) => {
                        let Some(cells) = visible(cells) else {
                            continue;
                        };
                        if ui.interact(grid.rect(cells), *id, Sense::click()).clicked() {
                            clicked.push(*id);
                            let on = match Some(*id) == first_check {
                                true => check.as_deref_mut(),
                                false => check_of(&mut by_id, *id),
                            };
                            if let Some(on) = on {
                                *on = !*on;
                            }
                        }
                        controls.push(*id);
                    }
                    Placed::Button(cells, b) => {
                        let Some(cells) = visible(cells) else {
                            continue;
                        };
                        let sense = if b.on { Sense::click() } else { Sense::hover() };
                        if ui.interact(grid.rect(cells), b.id, sense).clicked() {
                            clicked.push(b.id);
                        }
                        controls.push(b.id);
                    }
                    Placed::Scroll { .. } => {}
                    Placed::Choice {
                        cells,
                        row,
                        k,
                        many,
                        ..
                    } => {
                        let Some(cells) = visible(cells) else {
                            continue;
                        };
                        let cid = choice_id(*row, *k);
                        let rect = grid.rect(cells);
                        let keys = [Key::Space, Key::Enter];
                        let (keys, response) =
                            presses(ctx, cid, &keys, || ui.interact(rect, cid, Sense::click()));
                        let mut times = keys.len();
                        if response.clicked_by(egui::PointerButton::Primary) {
                            times += 1;
                            response.request_focus();
                        }
                        for _ in 0..times {
                            clicked.push(cid);
                            match by_id.iter_mut().find(|(i, _)| i == row) {
                                Some((_, Input::One(which))) if !*many => **which = Some(*k),
                                Some((_, Input::Any(on))) if *many => {
                                    if let Some(on) = on.get_mut(*k) {
                                        *on = !*on;
                                    }
                                }
                                _ => {}
                            }
                        }
                        controls.push(cid);
                    }
                    Placed::Message(cells, mid) => {
                        let (Some(text), Some(cells)) = (text_of(&mut by_id, *mid), visible(cells))
                        else {
                            continue;
                        };
                        message(ui, &grid, cells, *mid, text);
                        let mut view = MessageView::load(ctx, *mid);
                        let max_top = view.rows.saturating_sub(usize::from(cells.height));
                        if ui.rect_contains_pointer(grid.rect(cells)) {
                            let moved = (view.top as f32 + wheel_rows(ctx)).round();
                            view.top = moved.clamp(0.0, max_top as f32) as usize;
                        }
                        if max_top > 0 {
                            let bar = Cells::new(border, cells.y, 1, cells.height);
                            let first = view.top;
                            view.top =
                                scroll_bar_input(ui, &grid, bar, id.with(*mid), first, max_top);
                        }
                        view.top = view.top.min(max_top);
                        view.store(ctx, *mid);
                        controls.push(*mid);
                        messages_at.push((cells, *mid));
                    }
                    Placed::List {
                        cells,
                        rows,
                        id: list,
                    } => {
                        let Some(cells) = visible(cells) else {
                            continue;
                        };
                        let mut own = Bar::default();
                        let bar = match by_id.iter_mut().find(|(k, _)| k == list) {
                            Some((_, Input::List(bar))) => &mut **bar,
                            _ => &mut own,
                        };
                        let column = Cells::new(border, cells.y, 1, cells.height);
                        let at = (cells, column);
                        for row in list_pass(ui, &grid, at, *list, rows.len(), bar, id) {
                            chosen.push((*list, row));
                            clicked.push(*list);
                        }
                        controls.push(*list);
                        lists_at.push((cells, rows, *bar));
                    }
                }
            }
            // What the controls show, now that the focus is this pass's.
            let ctx = ui.ctx().clone();
            for placed in &layout.placed {
                match placed {
                    Placed::Check(cells, label, id) => {
                        let on = match Some(*id) == first_check {
                            true => check.as_deref().copied(),
                            false => check_value(&by_id, *id),
                        };
                        let on = on.unwrap_or(false);
                        let shown = format!("[{}] {label}", if on { "X" } else { " " });
                        let pen = control_pen(&ctx, *id, true);
                        cells::put(&mut buf, cells.x, cells.y, &shown, cells.width.into(), pen);
                    }
                    Placed::Button(cells, b) => {
                        let pen = control_pen(&ctx, b.id, b.on);
                        let shown = format!("[ {} ]", b.label);
                        cells::put(&mut buf, cells.x, cells.y, &shown, cells.width.into(), pen);
                    }
                    Placed::Scroll {
                        cells,
                        rows,
                        top: laid,
                    } => {
                        let max_top = rows.len().saturating_sub(usize::from(cells.height));
                        let mut first = top.as_deref().copied().unwrap_or(*laid).min(max_top);
                        let theirs =
                            (own_wheel.iter()).any(|c| ui.rect_contains_pointer(grid.rect(*c)));
                        if ui.rect_contains_pointer(body) && !theirs {
                            let moved = (first as f32 + wheel_rows(&ctx)).round();
                            first = moved.clamp(0.0, max_top as f32) as usize;
                        }
                        if max_top > 0 {
                            let x = frame_cells.right() - 1;
                            let bar = Cells::new(x, cells.y, 1, cells.height);
                            first = scroll_bar_input(ui, &grid, bar, id, first, max_top);
                        }
                        if let Some(top) = top.as_deref_mut() {
                            *top = first;
                        }
                        let shown = rows.iter().skip(first).take(cells.height.into());
                        for (k, (text, pen)) in shown.enumerate() {
                            let y = cells.y + k as u16;
                            cells::put(&mut buf, cells.x, y, text, cells.width.into(), *pen);
                        }
                        if max_top > 0 {
                            let at = first as f32 / max_top as f32;
                            let x = frame_cells.right() - 1;
                            cells::scroll_bar(&mut buf, x, cells.y, cells.height, Some(at));
                        }
                    }
                    Placed::Choice {
                        cells,
                        word,
                        row,
                        k,
                        many,
                    } => {
                        let on = match by_id.iter().find(|(i, _)| i == row) {
                            Some((_, Input::One(which))) => **which == Some(*k),
                            Some((_, Input::Any(on))) => on.get(*k).copied().unwrap_or(false),
                            _ => false,
                        };
                        // `•` is of ambiguous width: §10's `.` where a terminal draws it in two.
                        let dot = if cells::ambiguous_wide(&ctx) {
                            "."
                        } else {
                            "•"
                        };
                        let shown = match (*many, on) {
                            (true, true) => format!("[X] {word}"),
                            (true, false) => format!("[ ] {word}"),
                            (false, true) => format!("({dot}) {word}"),
                            (false, false) => format!("( ) {word}"),
                        };
                        let pen = control_pen(&ctx, choice_id(*row, *k), true);
                        cells::put(&mut buf, cells.x, cells.y, &shown, cells.width.into(), pen);
                    }
                    _ => {}
                }
            }
            for (cells, rows, bar) in lists_at {
                list_cells(&mut buf, cells, rows, bar, border);
            }
            let mut caret = None;
            for (cells, fid) in fields_at {
                let text = match Some(fid) == first_field {
                    true => field.as_deref().map(String::as_str),
                    false => text_value(&by_id, fid),
                };
                if let Some(text) = text {
                    let focused = ctx.memory(|m| m.has_focus(fid));
                    caret = field_cells(&ctx, &mut buf, cells, fid, text, focused).or(caret);
                }
            }
            for (cells, mid) in messages_at {
                let Some(text) = text_value(&by_id, mid) else {
                    continue;
                };
                let focused = ctx.memory(|m| m.has_focus(mid));
                caret = message_cells(&ctx, &mut buf, cells, mid, text, focused).or(caret);
                let view = MessageView::load(&ctx, mid);
                let max_top = view.rows.saturating_sub(usize::from(cells.height));
                if max_top > 0 {
                    let at = view.top as f32 / max_top as f32;
                    cells::scroll_bar(&mut buf, border, cells.y, cells.height, Some(at));
                }
            }
            let painter = ctx.layer_painter(ui.layer_id());
            let painter = painter.with_clip_rect(grid.rect(grid.all()));
            cells::paint(&painter, grid.origin, &buf, caret);
        });
    Shown {
        modal,
        clicked,
        chosen,
        controls,
    }
}

/// What a message field keeps between passes: the first row it shows, how many rows its text
/// takes, its caret's row and cell as egui's galley puts it (while it has the keyboard), the
/// caret's character and the text's length when last seen — a change scrolls to the caret —
/// and the pass it was last laid in.
#[derive(Debug, Clone, Copy, Default)]
struct MessageView {
    top: usize,
    rows: usize,
    caret: Option<(usize, usize)>,
    seen: Option<(usize, usize)>,
    laid: Option<u64>,
}

impl MessageView {
    fn id(field: egui::Id) -> egui::Id {
        field.with("sopen-message-view")
    }

    fn load(ctx: &egui::Context, field: egui::Id) -> Self {
        ctx.data(|d| d.get_temp(Self::id(field)))
            .unwrap_or_default()
    }

    fn store(self, ctx: &egui::Context, field: egui::Id) {
        ctx.data_mut(|d| d.insert_temp(Self::id(field), self));
    }
}

/// The cells a message field at `at` lays its text in a row: a cell fewer than it has, so the
/// caret after a full row is still on it.
fn room_of(at: Cells) -> usize {
    usize::from(at.width).saturating_sub(1).max(1)
}

/// A message field's engine: egui's multi-line `TextEdit` for `text` at cells `at` of `grid`, as
/// [`field`] is a one-line field's — painting nothing while it edits, selects, takes the
/// clipboard and IME; Shift+Enter its new line, a plain Enter never typed, Tab moving on. Its
/// galley is laid on the cells ([`message_galley`]): each row of the text at a row of cells,
/// every character at its cell. It is laid as many rows higher as the field has scrolled, under
/// a clip of the field's cells, so a click on a row the field shows lands on that row and one
/// outside the field never reaches it. When its caret moves or its text changes, the rows it
/// shows follow the caret. A field not laid in the pass before — a dialog opened again under
/// the same id too — starts afresh: at its top, its caret at its start (egui's would be at its
/// end), nothing to undo from before, as an editor opens a file; a caller that wants another
/// caret sets it once the field has been laid.
pub fn message(ui: &mut egui::Ui, grid: &Grid, at: Cells, id: egui::Id, text: &mut String) {
    let ctx = ui.ctx().clone();
    let room = room_of(at);
    let shown = usize::from(at.height).max(1);
    let mut view = MessageView::load(&ctx, id);
    let now = ctx.cumulative_pass_nr();
    if view.laid.is_none_or(|pass| pass + 1 < now) {
        view = MessageView::default();
        egui::text_edit::TextEditState::default().store(&ctx, id);
    }
    view.laid = Some(now);
    let field = grid.rect(at);
    let above = view.top as f32 * CELL.y;
    let tall = (view.rows.max(shown) as f32 * CELL.y).max(field.height() + above);
    let rect = egui::Rect::from_min_size(
        field.min - egui::vec2(0.0, above),
        egui::vec2(field.width(), tall),
    );
    let mut engine = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    engine.shrink_clip_rect(field);
    engine.set_opacity(0.0);
    let inside = egui::Frame::NONE.inner_margin(egui::Margin {
        right: CELL.x as i8,
        ..egui::Margin::ZERO
    });
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        message_galley(ui, buf.as_str(), room)
    };
    let shift_enter = egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, Key::Enter);
    let out = egui::TextEdit::multiline(text)
        .id(id)
        .frame(inside)
        .desired_width(rect.width())
        .desired_rows(shown)
        .cursor_at_end(false)
        .return_key(shift_enter)
        .layouter(&mut layouter)
        .show(&mut engine);
    view.rows = out.galley.rows.len();
    view.caret = out.cursor_range.map(|r| {
        let p = out.galley.pos_from_cursor(r.primary);
        let cell = |v: f32, size: f32| (v / size).round().max(0.0) as usize;
        (cell(p.min.y, CELL.y), cell(p.min.x, CELL.x))
    });
    let seen = (out.cursor_range).map(|r| (r.primary.index.0, text.chars().count()));
    if let (Some((row, _)), true) = (view.caret, seen != view.seen) {
        if row < view.top {
            view.top = row;
        } else if row >= view.top + shown {
            view.top = row + 1 - shown;
        }
    }
    view.seen = seen;
    view.top = view.top.min(view.rows.saturating_sub(shown));
    view.store(&ctx, id);
}

/// A row of a message field's text: the line it is in, its characters in that line, its glyphs
/// (the grapheme, the characters before it in the text, its cell in the row, its cells) and
/// whether a `\n` ends it.
struct TextRow<'a> {
    line: usize,
    chars: std::ops::Range<usize>,
    glyphs: Vec<(&'a str, usize, usize, usize)>,
    newline: bool,
}

/// The rows a message field lays `text` in, `room` cells wide: each of its lines broken by cells
/// as [`wrap`] breaks a text — after its last blank that fits, else after its last whole glyph —
/// with nothing dropped: the blanks at a break end the row above, past `room` if they must (no
/// cell shows them there), so every character is on a row.
fn message_rows(text: &str, room: usize) -> Vec<TextRow<'_>> {
    let room = room.max(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = vec![];
    let mut before = 0;
    for (n, line) in lines.iter().enumerate() {
        let all = glyphs(line);
        let count = line.chars().count();
        let mut start = 0;
        loop {
            let x0 = all.get(start).map_or(0, |g| g.2);
            let (mut end, mut after_blank) = (start, None);
            while let Some(&(g, _, x, w)) = all.get(end) {
                if g == " " {
                    end += 1;
                    after_blank = Some(end);
                    continue;
                }
                if x + w - x0 > room {
                    break;
                }
                end += 1;
            }
            if end < all.len() {
                end = match after_blank {
                    Some(b) => b,
                    // A glyph wider than the row: on a row of its own.
                    None if end == start => start + 1,
                    None => end,
                };
            }
            let char_at = |i: usize| all.get(i).map_or(count, |g| g.1);
            let last = end >= all.len();
            out.push(TextRow {
                line: n,
                chars: char_at(start)..char_at(end),
                glyphs: (all[start..end].iter())
                    .map(|&(g, c, x, w)| (g, before + c, x - x0, w))
                    .collect(),
                newline: last && n + 1 < lines.len(),
            });
            if last {
                break;
            }
            start = end;
        }
        before += count + 1;
    }
    out
}

/// `text` laid for a message field `room` cells wide: egui's galley of its lines, unwrapped, cut
/// into [`message_rows`]' rows, each a row of cells, every character at its cell as [`on_cells`]
/// places a field's. Only egui's interaction reads it — the engine paints nothing. A galley that
/// is not a row a line and a glyph a character is left as egui laid it.
fn message_galley(ui: &egui::Ui, text: &str, room: usize) -> Arc<egui::Galley> {
    let font = egui::FontSelection::Default.resolve(ui.style());
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, cga::WHITE, f32::INFINITY);
    job.keep_trailing_whitespace = true;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let lines: Vec<usize> = text.split('\n').map(|l| l.chars().count()).collect();
    let fits = galley.rows.len() == lines.len()
        && (galley.rows.iter().zip(&lines)).all(|(r, n)| r.row.glyphs.len() == *n);
    if !fits {
        return galley;
    }
    let mut laid = (*galley).clone();
    let mut placed = vec![];
    let mut right = 0.0f32;
    for (k, r) in message_rows(text, room).iter().enumerate() {
        let line = &galley.rows[r.line];
        let mut row = (*line.row).clone();
        row.visuals = Default::default();
        let mut source = line.row.glyphs[r.chars.clone()].iter();
        row.glyphs.clear();
        for &(g, _, x, w) in &r.glyphs {
            for (c, _) in g.chars().enumerate() {
                let Some(glyph) = source.next() else {
                    break;
                };
                let mut glyph = *glyph;
                (glyph.pos.x, glyph.advance_width) = match c {
                    0 => (x as f32 * CELL.x, w as f32 * CELL.x),
                    _ => ((x + w) as f32 * CELL.x, 0.0),
                };
                row.glyphs.push(glyph);
            }
        }
        row.size = egui::vec2(row.glyphs.last().map_or(0.0, |g| g.max_x()), CELL.y);
        right = right.max(row.size.x);
        placed.push(egui::epaint::text::PlacedRow {
            pos: egui::pos2(0.0, k as f32 * CELL.y),
            row: Arc::new(row),
            ends_with_newline: r.newline,
        });
    }
    laid.rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(right, placed.len() as f32 * CELL.y),
    );
    laid.mesh_bounds = laid.rect;
    (laid.num_vertices, laid.num_indices) = (0, 0);
    laid.rows = placed;
    Arc::new(laid)
}

/// The message field at cells `at` of `buf`, as its engine `id` holds `text`: black cells, the
/// rows it shows of the text in white ([`message_rows`], from the first it keeps), the selection
/// in the bar's colours while `focused`; while `focused`, the caret's cell when its row shows.
pub fn message_cells(
    ctx: &egui::Context,
    buf: &mut Buffer,
    at: Cells,
    id: egui::Id,
    text: &str,
    focused: bool,
) -> Option<Position> {
    let p = chrome::PALETTE;
    let plain = Pen::new(p.field_text, cga::BLACK);
    let marked = Pen::new(p.bar_text, p.bar);
    cells::fill(buf, at, plain);
    let view = MessageView::load(ctx, id);
    let range = (egui::text_edit::TextEditState::load(ctx, id))
        .and_then(|s| s.cursor.char_range())
        .map(|r| (r.primary.index.0, r.secondary.index.0));
    let (lo, hi) = match range {
        Some((a, b)) if focused => (a.min(b), a.max(b)),
        _ => (0, 0),
    };
    let (width, shown) = (usize::from(at.width), usize::from(at.height));
    let rows = message_rows(text, room_of(at));
    for (k, row) in rows.iter().enumerate().skip(view.top).take(shown) {
        let y = at.y + (k - view.top) as u16;
        for &(g, first, x, w) in &row.glyphs {
            if w == 0 || x + w > width {
                continue;
            }
            let pen = if first >= lo && first < hi {
                marked
            } else {
                plain
            };
            cells::put(buf, at.x + x as u16, y, g, w, pen);
        }
    }
    let (row, col) = view.caret.filter(|_| focused)?;
    let on = row >= view.top && row < view.top + shown;
    on.then(|| {
        let col = col.min(width.saturating_sub(1));
        Position::new(at.x + col as u16, at.y + (row - view.top) as u16)
    })
}

/// The field's engine: egui's `TextEdit` for `text`, `tweak`ed, at cells `at` of `grid`, in a ui
/// that paints nothing — each of its shapes a `Noop` — while it edits, selects, takes the
/// clipboard and IME. Its galley is laid on the cells ([`on_cells`]): every character at the x
/// of its cell, a glyph as wide as its cells, so egui's click, drag, scroll and IME rectangle
/// agree with what the cells show; its text area is a cell narrower than the field, so the caret
/// after the last character it shows is on the field's last cell. How far egui scrolled its
/// galley is what the cells then show ([`field_cells`]): the two views are one.
pub fn field(
    ui: &mut egui::Ui,
    grid: &Grid,
    at: Cells,
    id: egui::Id,
    text: &mut String,
    tweak: impl FnOnce(egui::TextEdit) -> egui::TextEdit,
) {
    let rect = grid.rect(at);
    let mut engine = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    engine.set_opacity(0.0);
    let inside = egui::Frame::NONE.inner_margin(egui::Margin {
        right: CELL.x as i8,
        ..egui::Margin::ZERO
    });
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        let font = egui::FontSelection::Default.resolve(ui.style());
        let mut job =
            egui::text::LayoutJob::simple_singleline(buf.as_str().to_string(), font, cga::WHITE);
        job.keep_trailing_whitespace = true;
        on_cells(ui.fonts_mut(|f| f.layout_job(job)))
    };
    let edit = egui::TextEdit::singleline(text)
        .id(id)
        .frame(inside)
        .desired_width(rect.width())
        .layouter(&mut layouter);
    let out = tweak(edit).show(&mut engine);
    let scrolled = (out.text_clip_rect.left() - out.galley_pos.x) / CELL.x;
    let first = scrolled.round().max(0.0) as usize;
    ui.ctx()
        .data_mut(|d| d.insert_temp(first_shown_id(id), first));
}

/// `galley` (one row) with each character at its cell: a glyph at the x of the cells before it,
/// as wide as its cells; a character after the first of a glyph (a combining mark) at the glyph's
/// end and of no width. Only egui's interaction reads it — the field's engine paints nothing.
fn on_cells(galley: std::sync::Arc<egui::Galley>) -> std::sync::Arc<egui::Galley> {
    let mut laid = (*galley).clone();
    let text = laid.job.text.clone();
    let mut places = glyphs(&text).into_iter().flat_map(|(g, _, x, w)| {
        (g.chars().enumerate()).map(move |(k, _)| if k == 0 { (x, w) } else { (x + w, 0) })
    });
    let mut right = 0.0f32;
    for placed in &mut laid.rows {
        let row = std::sync::Arc::make_mut(&mut placed.row);
        for glyph in &mut row.glyphs {
            if let Some((x, w)) = places.next() {
                glyph.pos.x = x as f32 * CELL.x;
                glyph.advance_width = w as f32 * CELL.x;
            }
        }
        row.size.x = row.glyphs.last().map_or(0.0, |g| g.max_x());
        right = right.max(placed.pos.x + row.size.x);
    }
    laid.rect.max.x = laid.rect.min.x + right;
    std::sync::Arc::new(laid)
}

/// Each glyph of `text`: itself, the characters before it, the cells before it, its cells.
fn glyphs(text: &str) -> Vec<(&str, usize, usize, usize)> {
    let (mut chars, mut x) = (0, 0);
    let mut out = vec![];
    for g in text.graphemes(true) {
        let w = cells::width(g);
        out.push((g, chars, x, w));
        chars += g.chars().count();
        x += w;
    }
    out
}

/// The cells of `text` before character `at` (a glyph that starts before it counts whole).
fn cells_before(text: &str, at: usize) -> usize {
    (glyphs(text).into_iter())
        .filter(|&(_, first, _, _)| first < at)
        .map(|(_, _, x, w)| x + w)
        .next_back()
        .unwrap_or(0)
}

/// Where the field `id` keeps the first cell of its text it shows: how far its engine scrolled.
fn first_shown_id(id: egui::Id) -> egui::Id {
    id.with("sopen-field-first-shown")
}

/// The field at cells `at` of `buf`, as its engine `id` holds `text`: black cells, the text in
/// white from the first cell its engine shows ([`field`]), the selection in the bar's
/// colours, a wide glyph never cut in half; while `focused`, the selection shows and the caret's
/// cell is returned.
pub fn field_cells(
    ctx: &egui::Context,
    buf: &mut Buffer,
    at: Cells,
    id: egui::Id,
    text: &str,
    focused: bool,
) -> Option<Position> {
    let p = chrome::PALETTE;
    let plain = Pen::new(p.field_text, cga::BLACK);
    let marked = Pen::new(p.bar_text, p.bar);
    cells::fill(buf, at, plain);
    let range = (egui::text_edit::TextEditState::load(ctx, id))
        .and_then(|s| s.cursor.char_range())
        .map(|r| (r.primary.index.0, r.secondary.index.0));
    let (lo, hi) = match range {
        Some((a, b)) if focused => (a.min(b), a.max(b)),
        _ => (0, 0),
    };
    let room = usize::from(at.width);
    let start = ctx
        .data(|d| d.get_temp::<usize>(first_shown_id(id)))
        .unwrap_or(0);
    for (g, first, x, w) in glyphs(text) {
        if w == 0 || x < start || x + w - start > room {
            continue;
        }
        let pen = if first >= lo && first < hi {
            marked
        } else {
            plain
        };
        let cx = at.x + (x - start) as u16;
        cells::put(buf, cx, at.y, g, w, pen);
    }
    let caret = range.map_or(0, |(primary, _)| cells_before(text, primary));
    let caret = caret.saturating_sub(start).min(room.saturating_sub(1));
    focused.then(|| Position::new(at.x + caret as u16, at.y))
}
