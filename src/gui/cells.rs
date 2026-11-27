//! The cell core (10 §4): a screen on the grid is drawn into a ratatui-core [`Buffer`] — a
//! grapheme, a foreground and a background per cell, never a modifier (T37) — and the window
//! paints it here, each cell at (col × 8, row × 16) pt: its background, then its glyph at its own
//! cell in the skin's font. A glyph is never laid out with its neighbours, so a Hangul name stays
//! on the grid however wide the installed font draws it (T6, 10 §10). A wide glyph takes two
//! cells, and its second cell is painted in the glyph's colours, as a terminal paints it (10 §5).
//!
//! - The grid ([`Grid`]): the whole cells over a window area, and their rects in points.
//! - The painter ([`paint`]) and the layer record ([`set_recording`], [`layers`]): what each paint
//!   drew, kept for the pass when recording is on, for the test kit and the terminal.
//! - The cell API ([`put`], [`put_right`], [`fill`], [`frame`], [`rule`], [`scroll_bar`]): colours
//!   as a [`Pen`] of the skin's opaque RGB (T15); widths as the buffer lays text out
//!   (unicode-width, by grapheme).
//! - The read-back ([`Layer::seen`], [`row_text`], [`find`], [`colors`], [`runs`]): character,
//!   colours and caret per cell.
//! - [`index256`]: a skin colour in a 256-colour terminal (10 §5).
//! - [`set_ambiguous_wide`], [`ambiguous_wide`]: whether the passes are shown in a terminal that
//!   draws a glyph of ambiguous width in two cells (10 §10).
//!
//! A buffer is one layer, and a wide glyph half written over is erased (see [`put`]) inside it
//! only. A later layer whose cells cover half of a wide glyph in an earlier one leaves the glyph's
//! other half showing in the window, where a terminal erases the whole glyph: a screen that lays
//! one buffer over another (a dialog over the list) first [`fill`]s, in the lower buffer, the
//! cells the upper one covers.

use super::chrome::{self, CELL};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
pub use ratatui_core::buffer::Buffer;
use ratatui_core::buffer::{Cell, CellWidth};
pub use ratatui_core::layout::{Position, Rect as Cells};
pub use ratatui_core::style::Modifier;
use ratatui_core::style::{Color, Style};
use ratatui_core::text::Span;
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// The grid.

/// A grid laid over a window area: its top-left corner and how many whole cells fit from there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub origin: Pos2,
    pub cols: u16,
    pub rows: u16,
}

impl Grid {
    /// The whole cells that fit in `area` from its top-left corner (rounded to a point); what is
    /// left at the right and the bottom, less than a cell, stays the area's background.
    pub fn over(area: Rect) -> Grid {
        let origin = area.min.round();
        // `as` saturates: an empty or upside-down area has no cells.
        let whole = |room: f32, cell: f32| (room / cell).floor().max(0.0) as u16;
        Grid {
            origin,
            cols: whole(area.max.x - origin.x, CELL.x),
            rows: whole(area.max.y - origin.y, CELL.y),
        }
    }

    /// All of it, in cells.
    pub fn all(&self) -> Cells {
        Cells::new(0, 0, self.cols, self.rows)
    }

    /// Cells `r` in points.
    pub fn rect(&self, r: Cells) -> Rect {
        rect_at(self.origin, r)
    }

    /// The cell under `p`, if it is on the grid.
    pub fn at(&self, p: Pos2) -> Option<Position> {
        let d = p - self.origin;
        let (x, y) = ((d.x / CELL.x).floor(), (d.y / CELL.y).floor());
        let inside = x >= 0.0 && y >= 0.0 && x < f32::from(self.cols) && y < f32::from(self.rows);
        inside.then(|| Position::new(x as u16, y as u16))
    }
}

/// `r` as two sides next to each other: the left one half its columns, rounded down, the right
/// one the rest — the odd column is the right side's.
pub fn halves(r: Cells) -> [Cells; 2] {
    let left = r.width / 2;
    [
        Cells::new(r.x, r.y, left, r.height),
        Cells::new(r.x + left, r.y, r.width - left, r.height),
    ]
}

/// The rect in points of cells `r` on a grid whose cell (0, 0) is at `origin`.
fn rect_at(origin: Pos2, r: Cells) -> Rect {
    Rect::from_min_size(
        origin + Vec2::new(f32::from(r.x) * CELL.x, f32::from(r.y) * CELL.y),
        Vec2::new(f32::from(r.width) * CELL.x, f32::from(r.height) * CELL.y),
    )
}

// ---------------------------------------------------------------------------------------------
// Colours.

/// The two colours a cell is written in, opaque RGB (the skin's, T15). The cell API takes
/// nothing else, so a cell it writes never carries a modifier (T37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pen {
    pub fg: Color32,
    pub bg: Color32,
}

impl Pen {
    pub const fn new(fg: Color32, bg: Color32) -> Pen {
        Pen { fg, bg }
    }

    fn style(self) -> Style {
        Style::new().fg(rgb(self.fg)).bg(rgb(self.bg))
    }
}

fn rgb(c: Color32) -> Color {
    debug_assert!(c.is_opaque(), "a cell's colour is opaque: {c:?}");
    Color::Rgb(c.r(), c.g(), c.b())
}

fn c32(c: Color) -> Option<Color32> {
    match c {
        Color::Rgb(r, g, b) => Some(Color32::from_rgb(r, g, b)),
        _ => None,
    }
}

/// The index a 256-colour terminal is sent for `c` (10 §5, T37): the nearest by RGB distance
/// among 16–255, xterm's 6×6×6 cube and its grey ramp, the lowest index of equals. 0–15 are never
/// sent: a terminal's theme repaints them.
pub fn index256(c: Color32) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let rgb = |i: u8| match i {
        16..=231 => {
            let n = usize::from(i - 16);
            [LEVELS[n / 36], LEVELS[n / 6 % 6], LEVELS[n % 6]]
        }
        _ => [8 + 10 * (i - 232); 3],
    };
    let distance = |i: u8| -> i32 {
        rgb(i)
            .into_iter()
            .zip([c.r(), c.g(), c.b()])
            .map(|(a, b)| (i32::from(a) - i32::from(b)).pow(2))
            .sum()
    };
    (16..=255).min_by_key(|&i| distance(i)).unwrap_or(16)
}

// ---------------------------------------------------------------------------------------------
// An ambiguous-wide terminal (10 §10, TS-6).

/// The context's passes are shown in a terminal that draws a glyph of ambiguous width in two
/// cells ([`set_ambiguous_wide`]).
#[derive(Debug, Clone, Copy, Default)]
struct AmbiguousWide;

fn ambiguous_wide_id() -> egui::Id {
    egui::Id::unique("sopen-ambiguous-wide")
}

/// Says that the passes of `ctx` are shown in a terminal that draws a glyph of ambiguous width
/// in two cells (`wide`), or not: the terminal backend says it once its probe has answered
/// (`term::Backend::enter`). A screen that knows a cell holds a glyph of sopen's own tables (the
/// hex view's CP437 column) substitutes 10 §10's stand-in at the source, where the printer could
/// not tell it from a name's letter.
pub fn set_ambiguous_wide(ctx: &egui::Context, wide: bool) {
    ctx.data_mut(|d| {
        if wide {
            d.insert_temp(ambiguous_wide_id(), AmbiguousWide);
        } else {
            d.remove_temp::<AmbiguousWide>(ambiguous_wide_id());
        }
    });
}

/// Whether `ctx`'s passes are shown in an ambiguous-wide terminal ([`set_ambiguous_wide`]); a
/// window's never are.
pub fn ambiguous_wide(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<AmbiguousWide>(ambiguous_wide_id()).is_some())
}

// ---------------------------------------------------------------------------------------------
// What reached the screen.

/// The context's last pass was not put on screen ([`set_presented`]).
#[derive(Debug, Clone, Copy, Default)]
struct Unpresented;

fn unpresented_id() -> egui::Id {
    egui::Id::unique("sopen-unpresented")
}

/// Says whether the pass just run in `ctx` reached the screen: the terminal backend does after
/// each pass, which it does not print while input is waiting (`term::Backend::serve`); a window
/// shows every pass and never says.
pub fn set_presented(ctx: &egui::Context, presented: bool) {
    ctx.data_mut(|d| {
        if presented {
            d.remove_temp::<Unpresented>(unpresented_id());
        } else {
            d.insert_temp(unpresented_id(), Unpresented);
        }
    });
}

/// Whether the pass before this one reached the screen ([`set_presented`]). A screen that acts
/// only on what was on screen keeps its paintings in an [`OnScreen`].
pub fn presented(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Unpresented>(unpresented_id()).is_none())
}

/// What a screen shows the owner (an action's dialog: what Run would run): the painting of the
/// last pass that reached the screen ([`presented`]), and that of the pass just run, until the
/// next pass knows whether it did. A Run or an Enter acts only on what [`OnScreen::start`] gives.
#[derive(Debug, Clone)]
pub struct OnScreen<T> {
    shown: Option<T>,
    last: Option<T>,
}

impl<T> Default for OnScreen<T> {
    fn default() -> Self {
        Self {
            shown: None,
            last: None,
        }
    }
}

impl<T> OnScreen<T> {
    /// At the start of a pass: what the screen shows now, the owner's view as they pressed this
    /// pass's keys. The pass before reached the screen: its painting, none if it painted nothing
    /// (a new modal's unseen first layout); it did not: what was shown before stays.
    pub fn start(&mut self, ctx: &egui::Context) -> Option<&T> {
        let last = self.last.take();
        if presented(ctx) {
            self.shown = last;
        }
        self.shown.as_ref()
    }

    /// What this pass painted.
    pub fn painted(&mut self, painting: T) {
        self.last = Some(painting);
    }
}

// ---------------------------------------------------------------------------------------------
// Widths.

/// The graphemes of `span` as [`Buffer::set_stringn`] lays them out — those with a control
/// character or no width left out — each with the cells it takes.
fn graphemes<'a>(span: &'a Span<'a>) -> impl Iterator<Item = (&'a str, usize)> + 'a {
    span.styled_graphemes(Style::new())
        .map(|g| (g.symbol, usize::from(g.symbol.cell_width())))
        .filter(|&(_, w)| w > 0)
}

/// How many cells `text` takes when it is put into a buffer.
pub fn width(text: &str) -> usize {
    graphemes(&Span::raw(text)).map(|(_, w)| w).sum()
}

/// The cells a symbol already in a buffer takes: ratatui-core's measure, without its debug
/// assertion on a control byte (a cell set directly may hold one).
fn cells_of(symbol: &str) -> u16 {
    if symbol.len() == 1 {
        1
    } else {
        symbol.cell_width()
    }
}

// ---------------------------------------------------------------------------------------------
// The painter.

/// One cell of a row as the window paints it and a terminal shows it: a wide glyph's second
/// cell has no symbol of its own and the glyph's colours, whatever the buffer holds there.
#[derive(Clone, Copy)]
struct Slot<'a> {
    x: u16,
    symbol: &'a str,
    /// The cells the symbol takes from here; 0 in a wide glyph's second cell.
    width: u16,
    fg: Option<Color32>,
    bg: Option<Color32>,
    modifier: Modifier,
}

fn slots(buf: &Buffer, y: u16) -> Vec<Slot<'_>> {
    let a = buf.area;
    let mut out = Vec::with_capacity(usize::from(a.width));
    let mut x = a.left();
    while x < a.right() {
        let cell = &buf[(x, y)];
        let width = cells_of(cell.symbol()).clamp(1, a.right() - x);
        let lead = Slot {
            x,
            symbol: cell.symbol(),
            width,
            fg: c32(cell.fg),
            bg: c32(cell.bg),
            modifier: cell.modifier,
        };
        out.push(lead);
        out.extend((1..width).map(|k| Slot {
            x: x + k,
            symbol: "",
            width: 0,
            ..lead
        }));
        x += width;
    }
    out
}

/// Paints `buf` with its cell (0, 0) at `origin`: per row one rectangle per run of a background,
/// then each glyph at its own cell's corner in the skin's font, and the caret as the two scan
/// lines at its cell's foot in the cell's foreground. A glyph wider than the cells it takes (an
/// emoji in a font that draws it over two, 10 §10) is cut to them; only such a glyph gets a clip
/// of its own, as each clip is a draw call of its own and the rest of the screen stays one. A cell
/// with no RGB background is not painted (what is under it shows); a glyph with no RGB
/// foreground is in the skin's text colour. Recorded as a [`Layer`] when recording is on.
pub fn paint(painter: &egui::Painter, origin: Pos2, buf: &Buffer, caret: Option<Position>) {
    let a = buf.area;
    let font = chrome::font();
    let fg_of = |s: &Slot| s.fg.unwrap_or(chrome::PALETTE.text);
    for y in a.top()..a.bottom() {
        let row = slots(buf, y);
        let mut i = 0;
        while i < row.len() {
            let bg = row[i].bg;
            let e = i + row[i..].iter().take_while(|s| s.bg == bg).count();
            if let Some(fill) = bg {
                let run = Cells::new(row[i].x, y, (e - i) as u16, 1);
                painter.rect_filled(rect_at(origin, run), 0.0, fill);
            }
            i = e;
        }
        for s in row
            .iter()
            .filter(|s| s.width > 0 && !s.symbol.trim().is_empty())
        {
            let cells = rect_at(origin, Cells::new(s.x, y, s.width, 1));
            let galley = painter.layout_no_wrap(s.symbol.to_string(), font.clone(), fg_of(s));
            if cells.contains_rect(Rect::from_min_size(cells.min, galley.size())) {
                painter.galley(cells.min, galley, fg_of(s));
            } else {
                painter
                    .with_clip_rect(cells)
                    .galley(cells.min, galley, fg_of(s));
            }
        }
    }
    if let Some(at) = caret.filter(|&p| a.contains(p)) {
        let cell = rect_at(origin, Cells::new(at.x, at.y, 1, 1));
        let foot = Rect::from_min_max(Pos2::new(cell.left(), cell.bottom() - 2.0), cell.max);
        let fg = slots(buf, at.y)
            .into_iter()
            .find(|s| s.x == at.x)
            .map_or(chrome::PALETTE.text, |s| fg_of(&s));
        painter.rect_filled(foot, 0.0, fg);
    }
    record(painter, origin, buf, caret);
}

// ---------------------------------------------------------------------------------------------
// The pass's record of what was painted in cells.

/// What one [`paint`] drew: where its cell (0, 0) is, the egui layer and the clip it was painted
/// under, the cells and the caret's cell. The record keeps the order of the calls; what is on top
/// on screen is by egui's order of `layer` (a modal's area over the central panel) first.
#[derive(Debug, Clone)]
pub struct Layer {
    pub origin: Pos2,
    pub layer: egui::LayerId,
    pub clip: Rect,
    pub buf: Buffer,
    pub caret: Option<Position>,
}

#[derive(Clone, Default)]
struct Record {
    on: bool,
    layers: Vec<Arc<Layer>>,
}

fn record_id() -> egui::Id {
    egui::Id::unique("sopen-cell-layers")
}

/// Recording on (the test kit, the terminal) or off (the default: nothing is kept).
pub fn set_recording(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(record_id(), Record { on, layers: vec![] }));
}

/// Forgets the layers recorded so far (before a pass).
pub fn clear(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Record>(record_id())
            .layers
            .clear()
    });
}

/// The layers painted since [`clear`], in the order [`paint`] was called.
pub fn layers(ctx: &egui::Context) -> Vec<Arc<Layer>> {
    ctx.data(|d| d.get_temp::<Record>(record_id()))
        .map(|r| r.layers)
        .unwrap_or_default()
}

fn record(painter: &egui::Painter, origin: Pos2, buf: &Buffer, caret: Option<Position>) {
    painter.ctx().data_mut(|d| {
        let r = d.get_temp_mut_or_default::<Record>(record_id());
        if r.on {
            r.layers.push(Arc::new(Layer {
                origin,
                layer: painter.layer_id(),
                clip: painter.clip_rect(),
                buf: buf.clone(),
                caret,
            }));
        }
    });
}

// ---------------------------------------------------------------------------------------------
// The cell API: colour only, no modifiers (T37).

/// The colours of `pen` on `cell`, and no modifier.
fn ink(cell: &mut Cell, pen: Pen) {
    cell.fg = rgb(pen.fg);
    cell.bg = rgb(pen.bg);
    cell.modifier = Modifier::empty();
}

/// Cell (`x`, `y`) is about to be written: a wide glyph to its left that covers it becomes a
/// space in its colours, as a terminal erases a wide glyph half of which is overwritten (else
/// the glyph would still hide what is written here).
fn unsplit(buf: &mut Buffer, x: u16, y: u16) {
    for back in 1..=x.saturating_sub(buf.area.left()).min(3) {
        let cell = &mut buf[(x - back, y)];
        if cells_of(cell.symbol()) > back {
            cell.set_symbol(" ");
            return;
        }
    }
}

/// One cell's `symbol` (one cell wide) in `pen`, where the buffer has the cell.
fn set(buf: &mut Buffer, x: u16, y: u16, symbol: &str, pen: Pen) {
    if buf.area.contains(Position::new(x, y)) {
        unsplit(buf, x, y);
        let cell = &mut buf[(x, y)];
        cell.set_symbol(symbol);
        ink(cell, pen);
    }
}

/// `r` (as much of it as the buffer has) filled with spaces in `pen`.
pub fn fill(buf: &mut Buffer, r: Cells, pen: Pen) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        if r.width > 0 {
            unsplit(buf, r.left(), y);
        }
        for x in r.left()..r.right() {
            let cell = &mut buf[(x, y)];
            cell.reset();
            ink(cell, pen);
        }
    }
}

/// `text` from cell (`x`, `y`) in at most `max` cells and never half a wide glyph; the cells it
/// took. A wide glyph's second cell gets the glyph's colours: ratatui-core resets it, and the
/// window paints it, where a terminal paints the glyph's background across both (10 §4).
pub fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, max: usize, pen: Pen) -> usize {
    if !buf.area.contains(Position::new(x, y)) {
        return 0;
    }
    let (end, _) = buf.set_stringn(x, y, text, max, pen.style());
    if end > x {
        unsplit(buf, x, y);
    }
    for cx in x..end {
        ink(&mut buf[(cx, y)], pen);
    }
    usize::from(end - x)
}

/// `text` ending at cell `end` (exclusive), right-aligned; the cells it took. A text wider than
/// the room before `end` shows its start from cell 0.
pub fn put_right(buf: &mut Buffer, end: u16, y: u16, text: &str, pen: Pen) -> usize {
    let w = width(text).min(usize::from(end)) as u16;
    put(buf, end - w, y, text, usize::from(w), pen)
}

/// `text` as it fits in `room` cells: whole, or its longest start before "…" — a wide glyph that
/// does not fit ends it a cell earlier, never split (01 §3a).
pub fn fit_start(text: &str, room: usize) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let span = Span::raw(text);
    let mut out = String::new();
    let mut used = 1;
    for (g, w) in graphemes(&span) {
        if used + w > room {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// `text` as it fits in `room` cells: whole, or its longest end after "…" — a path keeps the
/// folder it ends in.
pub fn fit_left(text: &str, room: usize) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let span = Span::raw(text);
    let all: Vec<(&str, usize)> = graphemes(&span).collect();
    let mut used = 1;
    let mut kept = 0;
    for &(_, w) in all.iter().rev() {
        if used + w > room {
            break;
        }
        used += w;
        kept += 1;
    }
    std::iter::once("…")
        .chain(all[all.len() - kept..].iter().map(|&(g, _)| g))
        .collect()
}

/// A window's frame in `r`: filled with the body colour, its border a double line (`active`) or a
/// single one, `title` centred in the top border with a blank cell either side — cut to the
/// width less six cells, as `chrome::frame_title` cuts it. Returns the cells inside the border.
pub fn frame(buf: &mut Buffer, r: Cells, title: &str, active: bool) -> Cells {
    let p = chrome::PALETTE;
    fill(buf, r, Pen::new(p.text, p.back));
    if r.width < 2 || r.height < 2 {
        return Cells::new(r.x, r.y, 0, 0);
    }
    let pen = line_pen(active);
    let [tl, tr, bl, br, h, v] = if active {
        ["╔", "╗", "╚", "╝", "═", "║"]
    } else {
        ["┌", "┐", "└", "┘", "─", "│"]
    };
    let (l, rt, t, b) = (r.left(), r.right() - 1, r.top(), r.bottom() - 1);
    for x in l + 1..rt {
        set(buf, x, t, h, pen);
        set(buf, x, b, h, pen);
    }
    for y in t + 1..b {
        set(buf, l, y, v, pen);
        set(buf, rt, y, v, pen);
    }
    for (x, y, corner) in [(l, t, tl), (rt, t, tr), (l, b, bl), (rt, b, br)] {
        set(buf, x, y, corner, pen);
    }
    let shown = fit_left(title, usize::from(r.width).saturating_sub(6));
    if !shown.is_empty() {
        let w = width(&shown) as u16;
        let x = l + (r.width - w) / 2;
        fill(buf, Cells::new(x - 1, t, w + 2, 1), pen);
        put(buf, x, t, &shown, w.into(), pen);
    }
    Cells::new(l + 1, t + 1, r.width - 2, r.height - 2)
}

/// The frame's line colour on the body: white when `active`, light grey otherwise.
fn line_pen(active: bool) -> Pen {
    let p = chrome::PALETTE;
    let line = if active { p.frame_active } else { p.frame_idle };
    Pen::new(line.color, p.back)
}

/// The rule across a frame `r` at row `y`, joined to its sides: `╟─╢` (`active`) or `├─┤`.
pub fn rule(buf: &mut Buffer, r: Cells, y: u16, active: bool) {
    if r.width < 2 {
        return;
    }
    let pen = line_pen(active);
    let (l, rt) = (r.left(), r.right() - 1);
    for x in l + 1..rt {
        set(buf, x, y, "─", pen);
    }
    let (jl, jr) = if active {
        ("╟", "╢")
    } else {
        ("├", "┤")
    };
    set(buf, l, y, jl, pen);
    set(buf, rt, y, jr, pen);
}

/// The scroll bar in column `x` from row `top` for `rows` rows: ↑, the ░ track, ↓, and the red
/// thumb one cell tall where `at` (0 the top … 1 the end) puts it — at the top when there is
/// nothing to scroll (`None`), and nowhere when no track cell fits between the arrows.
pub fn scroll_bar(buf: &mut Buffer, x: u16, top: u16, rows: u16, at: Option<f32>) {
    let p = chrome::PALETTE;
    let pen = Pen::new(p.track, p.track_back);
    fill(buf, Cells::new(x, top, 1, rows), pen);
    if rows < 2 {
        return;
    }
    let bottom = top.saturating_add(rows - 1);
    set(buf, x, top, "↑", pen);
    for y in top + 1..bottom {
        set(buf, x, y, "░", pen);
    }
    set(buf, x, bottom, "↓", pen);
    if rows < 3 {
        return;
    }
    let room = f32::from(rows - 3);
    let y = top + 1 + (room * at.unwrap_or(0.0).clamp(0.0, 1.0)).round() as u16;
    fill(buf, Cells::new(x, y, 1, 1), Pen::new(p.track, p.thumb));
}

// ---------------------------------------------------------------------------------------------
// Reading cells back (the test kit's cell queries, the parity oracle).

/// What one cell shows, as both backends must show it (10 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The cell's grapheme; empty in a wide glyph's second cell.
    pub symbol: String,
    /// `None` where the buffer holds no RGB colour: no background is painted there, and its
    /// glyph is in the skin's text colour.
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub caret: bool,
    /// Always empty from the cell API (T37); the oracle checks it.
    pub modifier: Modifier,
}

impl Layer {
    /// Cells `r` in points.
    pub fn rect(&self, r: Cells) -> Rect {
        rect_at(self.origin, r)
    }

    /// Every cell of the buffer's area, a row at a time from the top.
    pub fn seen(&self) -> Vec<Vec<Seen>> {
        let a = self.buf.area;
        (a.top()..a.bottom())
            .map(|y| {
                slots(&self.buf, y)
                    .into_iter()
                    .map(|s| Seen {
                        symbol: s.symbol.to_string(),
                        fg: s.fg,
                        bg: s.bg,
                        caret: self.caret == Some(Position::new(s.x, y)),
                        modifier: s.modifier,
                    })
                    .collect()
            })
            .collect()
    }
}

/// Row `y` of `buf` as text, with the cell each character is in (a wide glyph's second cell
/// adds nothing).
pub fn row_text(buf: &Buffer, y: u16) -> (String, Vec<u16>) {
    let (mut s, mut at) = (String::new(), Vec::new());
    for slot in slots(buf, y).into_iter().filter(|s| s.width > 0) {
        s.push_str(slot.symbol);
        at.extend(slot.symbol.chars().map(|_| slot.x));
    }
    (s, at)
}

/// Where a text stands in cells: the layer (its index in painting order) and the cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub layer: usize,
    pub cells: Cells,
}

/// Every place `text` stands within one row of a layer, in painting order.
pub fn find(layers: &[Arc<Layer>], text: &str) -> Vec<Hit> {
    let mut out = vec![];
    if text.is_empty() {
        return out;
    }
    let n = text.chars().count();
    for (layer, l) in layers.iter().enumerate() {
        let a = l.buf.area;
        for y in a.top()..a.bottom() {
            let (s, at) = row_text(&l.buf, y);
            for (start, _) in s.match_indices(text) {
                let ci = s[..start].chars().count();
                let (first, last) = (at[ci], at[ci + n - 1]);
                let end = last + cells_of(l.buf[(last, y)].symbol()).clamp(1, a.right() - last);
                out.push(Hit {
                    layer,
                    cells: Cells::new(first, y, end - first, 1),
                });
            }
        }
    }
    out
}

/// The foreground colours of `cells` (one row) in `l` as the window paints them, consecutive
/// repeats merged, as a galley's sections are.
pub fn colors(l: &Layer, cells: Cells) -> Vec<Color32> {
    let mut out: Vec<Color32> = vec![];
    let row = slots(&l.buf, cells.y);
    for s in row
        .iter()
        .filter(|s| s.x >= cells.left() && s.x < cells.right())
    {
        let c = s.fg.unwrap_or(chrome::PALETTE.text);
        if out.last() != Some(&c) {
            out.push(c);
        }
    }
    out
}

/// A frame's or a scroll bar's glyph (box drawing, blocks and shades): never part of a text.
pub fn frame_glyph(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}')
}

/// Whether a cell showing `symbol` in `fg` on `bg` is a frame's or a scroll bar's, never part of
/// a text: a [`frame_glyph`], or an arrow in the scroll bar's colours ([`scroll_bar`]) — an arrow
/// elsewhere (a status row's "↑ ↓ choose") is text.
pub fn frame_cell(symbol: &str, fg: Option<Color32>, bg: Option<Color32>) -> bool {
    let p = chrome::PALETTE;
    let bar = (fg, bg) == (Some(p.track), Some(p.track_back));
    (symbol.chars()).any(|c| frame_glyph(c) || (bar && matches!(c, '↑' | '↓')))
}

/// A run of text in a row: its cells and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub cells: Cells,
    pub text: String,
}

/// Every run of text in `buf`: per row, the stretches of one pair of colours with no frame's cell
/// ([`frame_cell`]) and no two spaces in a row, blanks at either end cut, blank ones left out.
pub fn runs(buf: &Buffer) -> Vec<Run> {
    let blank = |s: &Slot| s.symbol.trim().is_empty();
    let framed = |s: &Slot| frame_cell(s.symbol, s.fg, s.bg);
    let mut out = vec![];
    let a = buf.area;
    for y in a.top()..a.bottom() {
        let row: Vec<Slot> = slots(buf, y).into_iter().filter(|s| s.width > 0).collect();
        let mut i = 0;
        while i < row.len() {
            let colours = (row[i].fg, row[i].bg);
            let mut e = i;
            while e < row.len()
                && (row[e].fg, row[e].bg) == colours
                && !framed(&row[e])
                && !(blank(&row[e]) && row.get(e + 1).is_some_and(blank))
            {
                e += 1;
            }
            let run = &row[i..e];
            let first = run.iter().position(|s| !blank(s));
            let last = run.iter().rposition(|s| !blank(s));
            if let (Some(f), Some(l)) = (first, last) {
                let (from, to) = (&run[f], &run[l]);
                out.push(Run {
                    cells: Cells::new(from.x, y, to.x + to.width - from.x, 1),
                    text: run[f..=l].iter().map(|s| s.symbol).collect(),
                });
            }
            i = e.max(i + 1);
        }
    }
    out
}

#[cfg(test)]
mod tests;
