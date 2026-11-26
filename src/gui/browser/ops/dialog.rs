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

use crate::gui::cells::{self, Buffer, Cells, Grid, Pen, Position};
use crate::gui::chrome::{self, cga, CELL};
use eframe::egui::{self, Color32, Sense};
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
    }
}

/// The fewest cells a field has.
const FIELD: usize = 12;

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
    let fixed = |texts: &[Vec<String>]| -> usize {
        (parts.iter().zip(texts))
            .map(|(part, rows)| match part {
                Part::Text(..) | Part::Held(..) => rows.len(),
                Part::Scroll { .. } => 0,
                _ => 1,
            })
            .sum()
    };
    // The box is its rows, two border rows and two margin rows; its shadow one more.
    let fits = usize::from(grid.rows).saturating_sub(5);
    while fixed(&texts) + shown > fits && shown > 1 {
        shown -= 1;
    }
    while fixed(&texts) + shown > fits {
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
    let inside = fixed(&texts) + shown;
    let frame = centred(grid, width as u16 + 4, inside as u16 + 4);
    let (x0, mut y) = (frame.x + 2, frame.y + 2);
    let width = width as u16;
    let mut placed = vec![];
    for (part, rows) in parts.iter().zip(texts) {
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
/// row its rows that scroll show.
#[derive(Default)]
pub struct Inputs<'a> {
    pub field: Option<&'a mut String>,
    pub check: Option<&'a mut bool>,
    pub top: Option<&'a mut usize>,
}

/// What a pass of a dialog did.
pub struct Shown {
    pub modal: egui::ModalResponse<()>,
    /// The buttons and the check box clicked — Enter or Space on the focused one too.
    clicked: Vec<egui::Id>,
    /// Its controls (the field, the check box, the buttons): the ones that may hold the focus.
    pub controls: Vec<egui::Id>,
}

impl Shown {
    pub fn clicked(&self, id: egui::Id) -> bool {
        self.clicked.contains(&id)
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

/// Shows `layout` as a modal `id` at its cells for one pass: one buffer of cells — the shadow,
/// the frame, the texts — and each control an egui interaction at its cells, in the order of the
/// parts (Tab's order); the field's engine at its cells; the wheel over the box and the scroll
/// bar ([`scroll_bar_input`]) scroll its rows.
/// Painted once every control has had its say, so the focus it shows is this pass's.
pub fn show(ctx: &egui::Context, id: egui::Id, layout: &Layout, inputs: Inputs) -> Shown {
    let Inputs {
        mut field,
        mut check,
        mut top,
    } = inputs;
    let grid = layout.grid;
    // The box, cut to the grid: on a grid too small for it, nothing of it stands outside.
    let body = grid.rect(layout.frame.intersection(grid.all()));
    let at = body.min - ctx.content_rect().min;
    let area = egui::Modal::default_area(id)
        .anchor(egui::Align2::LEFT_TOP, at)
        .fade_in(false)
        .constrain(false);
    let mut clicked = vec![];
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
            let mut field_at = None;
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
                        if let (Some(text), Some(cells)) = (field.as_deref_mut(), visible(cells)) {
                            self::field(ui, &grid, cells, *id, text, |e| e);
                            controls.push(*id);
                            field_at = Some((cells, *id));
                        }
                    }
                    Placed::Check(cells, _, id) => {
                        let Some(cells) = visible(cells) else {
                            continue;
                        };
                        if ui.interact(grid.rect(cells), *id, Sense::click()).clicked() {
                            clicked.push(*id);
                            if let Some(on) = check.as_deref_mut() {
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
                }
            }
            // What the controls show, now that the focus is this pass's.
            let ctx = ui.ctx().clone();
            for placed in &layout.placed {
                match placed {
                    Placed::Check(cells, label, id) => {
                        let on = check.as_deref().copied().unwrap_or(false);
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
                        if ui.rect_contains_pointer(body) {
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
                    _ => {}
                }
            }
            let caret = match (field_at, field.as_deref()) {
                (Some((cells, id)), Some(text)) => {
                    let focused = ctx.memory(|m| m.has_focus(id));
                    field_cells(&ctx, &mut buf, cells, id, text, focused)
                }
                _ => None,
            };
            let painter = ctx.layer_painter(ui.layer_id());
            let painter = painter.with_clip_rect(grid.rect(grid.all()));
            cells::paint(&painter, grid.origin, &buf, caret);
        });
    Shown {
        modal,
        clicked,
        controls,
    }
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
