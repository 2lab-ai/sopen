//! F1's key help (04 §2 slot 1, §6.1): a read-only screen listing the keys of where the keyboard
//! is — the list, a viewer, the git panel, the tree — in every xtools window, in the skin of
//! [`super::chrome`].
//!
//! - **What it lists** comes from the browser's one key table ([`for_context`]: its landed rows
//!   of that context, the F-key slots a hosted viewer adds) or, in a window of its own (image,
//!   media, hex, the binary dialog), from that window's F-key row ([`for_window`]). Both end with
//!   the F-key aliases and the clickable row (04 §6.1), then where ⌘ and Ctrl apply.
//! - **One text in both backends** (10 T27): keys are named by [`Mods::prefix`], the same on
//!   every machine (`Ctrl/⌘+R`, `⌘+Backspace`), so the window and the terminal on one machine
//!   read alike; it lists the rows this machine can press ([`keys::rows`]: no ⌘⌫ off a Mac).
//! - **Who opens it**: a window of its own on F1 ([`super::own_keys`]); the browser on
//!   `keys::Command::Help` with its context's page (S1-E). [`open`] puts it on screen.
//! - **It is cells** (10 §4–§6), the same in the window and the terminal (TS-1): a box on the
//!   window's grid between the title row and the status row (the window's first row and its last
//!   two), centred with its shadow of black cells, laid out as a dialog is (`ops::dialog`'s
//!   width, centring and shadow; at most 76 cells inside) — in the window's body colours, its
//!   frame a double line with the page's title in it, a column of keys in the key colour and one
//!   of words beside it (under the keys, two cells in, on a box that leaves the words fewer than
//!   12 cells), each wrapped by cells inside the box; then a blank row and how to leave it.
//!   Lines that do not all fit scroll inside the box, the scroll bar on its frame's right line.
//!   Where the area is too short the box is cut to it, never drawn over the rows around it. The
//!   list blanks its cells under the box first ([`covered`]), so a wide glyph half under its
//!   edge goes in the window as a terminal erases it.
//! - **While it is open** it takes the keyboard and the pointer of the whole window: Esc, F1 or
//!   F3 close it, ↑ ↓ PgUp PgDn Home End and the wheel scroll it, a row at a time; on its scroll
//!   bar ↑ and ↓ a row, a click on the track a page towards it, the thumb dragged (10 §6); a
//!   click on the F-key row's slot 1 or 3 closes it as F1 or F3 does, another slot does nothing.
//!   The window's pass sees no key, typed text, wheel or pointer (Cmd/Ctrl+Q and +W close the
//!   help and go on to close the window, through its unsaved-changes prompt) — egui gets no
//!   pointer event at all, so nothing under it, a modal's backdrop included, is clicked — and a
//!   page view's native view hides under it anywhere (its layer covers the window, as a modal's
//!   backdrop). The switch carries it to the other backend as it is ([`carried`]).
//!   The shell's input plugin (`super::FKeyInput`) runs [`input_hook`] on the raw input (the
//!   pointer), [`begin_pass`] before every window's pass (the keys) and [`end_pass`] after it.
//!   The screen is painted, not laid out: a plugin's pass must not make widgets (egui's debug
//!   build asks every plugin about the widget under the pointer, the one running included, and
//!   waits on its lock for ever) — so its scroll bar and the slots read the pointer's events
//!   themselves.

use super::browser::keys::{self, Context, Mods, Trigger};
use super::browser::ops::dialog;
use super::browser::view::Mode;
use super::cells::{self, Buffer, Cells, Grid, Pen};
use super::chrome::{self, FKey, CELL};
use eframe::egui::{self, Color32, Event, Key, Pos2, Rect, Vec2};

/// One line of the screen: the keys (alternatives joined by ", ") and what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub keys: String,
    pub what: String,
}

/// What the screen shows: its title (in the frame's top border) and its lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Page {
    pub title: String,
    pub lines: Vec<Line>,
}

/// The browser's keys in `context` (its landed rows, 02 §7), then the F-key slots of `own` that
/// the table has no row for there (a hosted viewer's own keys: the hex window's 5 Save, …), then
/// the aliases. F9's words name the mode it switches to in `mode`. The browser opens it on
/// `keys::Command::Help`.
pub(crate) fn for_context(context: Context, mode: Mode, own: &[FKey]) -> Page {
    page_upto(context, mode, own, keys::LANDED_SLICES)
}

/// [`for_context`] with the rows of the slices in `upto` — the set of landed slices
/// ([`keys::LANDED_SLICES`]; a slice's integration WU adds its number), or every slice in the
/// tests — that this machine can press.
fn page_upto(context: Context, mode: Mode, own: &[FKey], upto: keys::Slices) -> Page {
    page_of(context, mode, own, keys::rows(context, upto))
}

/// The page of `rows`, rows of `context` in the table's order (the tests give a Mac's and
/// another machine's).
fn page_of<'a>(
    context: Context,
    mode: Mode,
    own: &[FKey],
    rows: impl Iterator<Item = &'a keys::Row>,
) -> Page {
    // One line per wording, in the table's order; its keys plain ones first.
    let mut lines: Vec<(Vec<(bool, String)>, String)> = Vec::new();
    let mut slots = Vec::new();
    for row in rows {
        let plain = match row.trigger {
            Trigger::Key(key, Mods::None) => {
                slots.extend(chrome::fkey_number(key));
                true
            }
            Trigger::Key(..) => false,
            Trigger::Char(_) | Trigger::Letter => true,
        };
        let what = if row.command == keys::Command::SwitchMode {
            switch_what(mode).to_string()
        } else {
            row.what.to_string()
        };
        let name = (plain, row.trigger.name());
        match lines.iter_mut().find(|(_, w)| *w == what) {
            Some((names, _)) => names.push(name),
            None => lines.push((vec![name], what)),
        }
    }
    let mut out: Vec<Line> = lines
        .into_iter()
        .map(|(mut names, what)| {
            names.sort_by_key(|(plain, _)| !plain);
            let names: Vec<String> = names.into_iter().map(|(_, name)| name).collect();
            Line {
                keys: names.join(", "),
                what,
            }
        })
        .collect();
    // The slots a hosted window adds, where the table has no F-key of its own.
    let mut own: Vec<FKey> = own
        .iter()
        .filter(|k| !slots.contains(&k.n))
        .copied()
        .collect();
    own.sort_by_key(|k| k.n);
    own.dedup_by_key(|k| k.n);
    out.extend(own.into_iter().map(own_line));
    out.extend(aliases());
    Page {
        title: format!("Keys in {}", context.name()),
        lines: out,
    }
}

/// What F9 does in `mode` (the mode it switches to, as its label says).
fn switch_what(mode: Mode) -> &'static str {
    match mode {
        Mode::Preview => "Switch to dual mode: two lists",
        Mode::Dual => "Switch to preview mode: the other side follows the bar",
    }
}

/// A window of its own: its F-key row's slots (`row`, as [`super::own_keys`] draws it), then
/// the aliases.
pub(crate) fn for_window(row: &[FKey]) -> Page {
    let mut row = row.to_vec();
    row.sort_by_key(|k| k.n);
    let mut lines: Vec<Line> = row.into_iter().map(own_line).collect();
    lines.extend(aliases());
    Page {
        title: "Keys".to_string(),
        lines,
    }
}

/// The line of a window's own F-key.
fn own_line(key: FKey) -> Line {
    Line {
        keys: format!("F{}", key.n),
        what: describe(key),
    }
}

/// The last lines of every page (10 T27, §9): the aliases, each with where it works —
/// Cmd/Ctrl+digits and the row's slots everywhere (04 §6.1), Alt+digits in a terminal; the
/// switch's F12, Alt+G in a terminal (T33) and the mode button — then which of ⌘ and Ctrl a
/// `Ctrl/⌘` key is.
fn aliases() -> [Line; 3] {
    let (cmd, alt) = (Mods::Command.prefix(), Mods::Alt.prefix());
    [
        Line {
            keys: format!("{cmd}1…0 · {alt}1…0"),
            what: format!("F1…F10 ({alt}1…0 in a terminal); so does a click on a slot"),
        },
        Line {
            keys: format!("F12 · {alt}G"),
            what: format!(
                "Between the terminal and a window, under sopen --tui ({alt}G in a terminal); so \
                 does a click on [GUI] or [TUI]"
            ),
        },
        Line {
            keys: cmd.trim_end_matches('+').to_string(),
            what: "⌘ applies in a Mac window; Ctrl everywhere else (in a terminal on every OS)"
                .to_string(),
        },
    ]
}

/// What a window's own F-key does, from its row's label (the viewers' keys, 01 §3a).
fn describe(key: FKey) -> String {
    let cmd = Mods::Command.prefix();
    match (key.n, key.label) {
        (1, "Help") => "This screen".to_string(),
        (3, "Exit") => format!("Close the window (as {cmd}W)"),
        (5, "Save") => format!("Save (as {cmd}S)"),
        (6, "Goto") => format!("Go to an offset (as {cmd}G)"),
        (7, "SaveAs") => format!("Save As: a new local file with your edits (as {cmd}Alt+S)"),
        (8, "Asc/hx") => "Type in the ASCII or the hex column (as Tab)".to_string(),
        (2, "Unwrap") => "One row per line (as W)".to_string(),
        (2, "Wrap") => "Wrap the lines at the window's width (as W)".to_string(),
        (_, label) => label.to_string(),
    }
}

/// The widest the key column gets; longer keys wrap in it.
const KEYS: usize = 28;
/// The fewest cells the words column takes beside the keys; on a box narrower than that, each
/// line's words go under its keys.
const WORDS: usize = 12;
/// The last row: how to leave the screen; with more rows than fit, how to see them.
const BACK: &str = "Esc, F1 or F3: back";
const MORE: &str = "Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more";

/// The screen laid out on a window's grid (see the module's header).
#[derive(Debug, Clone)]
struct Laid {
    /// The area between the title row and the status row: the window's grid less its first row
    /// and its last two.
    area: Grid,
    /// The box, in the area's cells.
    frame: Cells,
    /// The box's inside, in cells: the rows' width.
    width: usize,
    /// The cells of the key column; where the words column starts (two blank cells after the
    /// keys, or two in from the left under them) and its cells.
    keys: usize,
    words_x: usize,
    words: usize,
    /// Every row: a line's keys and its words, each wrapped by cells in its column — beside
    /// each other a line takes as many rows as the taller of the two; under each other, its
    /// keys' rows then its words'.
    rows: Vec<(String, String)>,
    /// How many rows show at once.
    shown: usize,
    /// How to leave, wrapped.
    foot: Vec<String>,
}

impl Laid {
    /// The rows' cells: the box's inside less its margin, as tall as the rows shown.
    fn list(&self) -> Cells {
        let (x, y) = (self.frame.x + 2, self.frame.y + 2);
        Cells::new(x, y, self.width as u16, self.shown as u16)
    }

    /// The first row shown at the end.
    fn last(&self) -> usize {
        self.rows.len().saturating_sub(self.shown)
    }

    /// The scroll bar on the frame's right line beside the rows, when they do not all fit.
    fn bar(&self) -> Option<Cells> {
        let list = self.list();
        let x = self.frame.right().saturating_sub(1);
        (self.rows.len() > self.shown).then(|| Cells::new(x, list.y, 1, list.height))
    }
}

/// `page` on `screen`, a window's whole grid: its rows in a box as wide as they want by a
/// dialog's rule ([`dialog::inside_width`]) — the key column as wide as the widest keys, at most
/// [`KEYS`] and half the box, the words beside it when that leaves them [`WORDS`] cells, else
/// under the keys two cells in, every column inside the box — and as tall as they are; on a
/// short area as many rows as fit, one at least. Centred in the area with its shadow
/// ([`dialog::centred`]).
fn lay(page: &Page, screen: Grid) -> Laid {
    let area = Grid {
        origin: screen.origin + Vec2::new(0.0, CELL.y),
        cols: screen.cols,
        rows: screen.rows.saturating_sub(3),
    };
    let w = cells::width;
    let widest = (page.lines.iter().map(|l| w(&l.keys)))
        .max()
        .unwrap_or(0)
        .min(KEYS);
    let wanted = (page.lines.iter().map(|l| widest + 2 + w(&l.what)))
        .chain([w(MORE), w(&page.title) + 2])
        .max()
        .unwrap_or(0);
    // At least a cell: the grid less eight cells, never below one.
    let width = dialog::inside_width(area.cols, 0, wanted).max(1);
    let keys = widest.min(width.saturating_sub(2) / 2).max(1);
    let beside = width >= keys + 2 + WORDS;
    let (keys, words_x) = match beside {
        true => (keys, keys + 2),
        false => (width, 2.min(width - 1)),
    };
    let words = width - words_x;
    let mut rows: Vec<(String, String)> = vec![];
    for l in &page.lines {
        let (k, said) = (dialog::wrap(&l.keys, keys), dialog::wrap(&l.what, words));
        if beside {
            let n = k.len().max(said.len());
            let row = |v: &[String], i: usize| v.get(i).cloned().unwrap_or_default();
            rows.extend((0..n).map(|i| (row(&k, i), row(&said, i))));
        } else {
            rows.extend(k.into_iter().map(|k| (k, String::new())));
            rows.extend(said.into_iter().map(|w| (String::new(), w)));
        }
    }
    // The box: its rows, a blank, the foot, two border rows and two margin rows; its shadow one
    // more.
    let fits = usize::from(area.rows).saturating_sub(5);
    let room = |foot: &[String]| fits.saturating_sub(1 + foot.len()).max(1);
    let mut foot = dialog::wrap(BACK, width);
    if rows.len() > room(&foot) {
        foot = dialog::wrap(MORE, width);
    }
    let shown = rows.len().min(room(&foot));
    let height = shown + 1 + foot.len() + 4;
    let frame = dialog::centred(area, width as u16 + 4, height as u16);
    Laid {
        area,
        frame,
        width,
        keys,
        words_x,
        words,
        rows,
        shown,
        foot,
    }
}

/// The screen's cells with row `top` first: the shadow, the frame with `title` in it, the rows,
/// the scroll bar, the foot — cut to the area.
fn draw(laid: &Laid, title: &str, top: usize) -> Buffer {
    let p = chrome::PALETTE;
    let f = laid.frame;
    let [right, under] = dialog::shadow_of(f);
    let mut buf = Buffer::empty(f.union(right).union(under).intersection(laid.area.all()));
    dialog::shadow(&mut buf, f);
    cells::frame(&mut buf, f, title, true);
    let list = laid.list();
    let (key_pen, word_pen) = (Pen::new(p.help_key, p.back), Pen::new(p.text, p.back));
    let words_x = list.x + laid.words_x as u16;
    for (k, (keys, words)) in laid.rows.iter().skip(top).take(laid.shown).enumerate() {
        let y = list.y + k as u16;
        cells::put(&mut buf, list.x, y, keys, laid.keys, key_pen);
        cells::put(&mut buf, words_x, y, words, laid.words, word_pen);
    }
    if let Some(bar) = laid.bar() {
        let at = top as f32 / laid.last().max(1) as f32;
        cells::scroll_bar(&mut buf, bar.x, bar.y, bar.height, Some(at));
    }
    let foot_y = list.bottom() + 1;
    for (k, row) in laid.foot.iter().enumerate() {
        let pen = Pen::new(p.dim, p.back);
        cells::put(
            &mut buf,
            list.x,
            foot_y + k as u16,
            row,
            list.width.into(),
            pen,
        );
    }
    buf
}

/// The screen while it is open: its page, the first row shown, and what the last pass drew.
#[derive(Debug, Clone, Default)]
struct Open {
    page: Page,
    top: usize,
    /// Rows that fit (a page for PgUp / PgDn is one less) and all the rows.
    rows: usize,
    total: usize,
    /// The scroll bar's column, in points, while the rows do not all fit.
    bar: Option<Rect>,
    /// The scroll bar's thumb is being dragged.
    drag: bool,
    /// The F-key row's slot 1 or 3 pressed, waiting for its release there.
    slot: Option<u8>,
}

fn open_id() -> egui::Id {
    egui::Id::unique("sopen-key-help")
}

/// Shows `page` over the window, from the next pass on (and this one's end).
pub(crate) fn open(ctx: &egui::Context, page: Page) {
    ctx.data_mut(|d| {
        d.insert_temp(
            open_id(),
            Open {
                page,
                ..Open::default()
            },
        )
    });
    ctx.request_repaint();
}

/// Whether the help screen is on screen (the viewer's view keys wait for it: 08 §4's Question).
pub(crate) fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Open>(open_id()).is_some())
}

/// The help as it stands — its page and the row at its top — for the switch to carry into the
/// other backend's context (10 §7.2, `term::Carry`); none while it is closed.
#[derive(Debug, Clone, Default)]
pub(crate) struct Carried(Option<Open>);

pub(crate) fn carried(ctx: &egui::Context) -> Carried {
    let open = ctx.data(|d| d.get_temp::<Open>(open_id()));
    Carried(open.map(|open| Open {
        drag: false,
        slot: None,
        ..open
    }))
}

/// `carried` into `ctx`: the help open as it was where it was carried from, or closed.
pub(crate) fn carry_into(ctx: &egui::Context, carried: &Carried) {
    ctx.data_mut(|d| {
        if let Some(open) = carried.0.clone() {
            d.insert_temp(open_id(), open);
        } else {
            d.remove_temp::<Open>(open_id());
        }
    });
}

/// The cells of `on` — a grid on the window, as the list's stands — that the open help covers,
/// its box and its shadow, for the list to blank before it is painted: one buffer of cells laid
/// over another (cells.rs), so a wide glyph half under the box's edge goes from the list too, as
/// a terminal erases it. None while it is closed.
pub(crate) fn covered(ctx: &egui::Context, on: &Grid) -> Vec<Cells> {
    let Some(open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return vec![];
    };
    let laid = lay(&open.page, Grid::over(ctx.content_rect()));
    let [right, under] = dialog::shadow_of(laid.frame);
    ([laid.frame, right, under].into_iter())
        .map(|c| cells_on(on, laid.area.rect(c.intersection(laid.area.all()))))
        .filter(|c| !c.is_empty())
        .collect()
}

/// The cells of `on` that `r` (points, on the same 8 × 16 lattice) stands on, cut to it.
fn cells_on(on: &Grid, r: Rect) -> Cells {
    let at = |v: f32, cell: f32, n: u16| (v / cell).round().clamp(0.0, f32::from(n)) as u16;
    let (from, to) = (r.min - on.origin, r.max - on.origin);
    let (x0, y0) = (at(from.x, CELL.x, on.cols), at(from.y, CELL.y, on.rows));
    let (x1, y1) = (at(to.x, CELL.x, on.cols), at(to.y, CELL.y, on.rows));
    Cells::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

/// A press on the scroll bar: the first row it puts at the top, or the thumb taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    To(usize),
    Drag,
}

/// A press at `pos` on the scroll bar `bar` (its column in points: ↑, the track, ↓) of rows
/// shown from `top` of `last + 1` places, as a dialog's scroll bar takes it (10 §6): ↑ and ↓ a
/// row, the track a `page` towards the press, the thumb taken for a drag. `None` off the bar.
fn on_bar(bar: Rect, pos: Pos2, top: usize, last: usize, page: usize) -> Option<Press> {
    let rows = (bar.height() / CELL.y).round() as usize;
    if !bar.contains(pos) || rows < 3 {
        return None;
    }
    let k = ((pos.y - bar.top()) / CELL.y).floor() as usize;
    if k == 0 {
        return Some(Press::To(top.saturating_sub(1)));
    }
    if k + 1 >= rows {
        return Some(Press::To((top + 1).min(last)));
    }
    // The thumb's cell on the track, as `cells::scroll_bar` draws it.
    let room = (rows - 3) as f32;
    let thumb = (room * top as f32 / last.max(1) as f32).round() as usize;
    Some(match k - 1 {
        r if r < thumb => Press::To(top.saturating_sub(page)),
        r if r > thumb => Press::To((top + page).min(last)),
        _ => Press::Drag,
    })
}

/// The first row the thumb dragged to `pos` puts at the top: the place on the track under it.
fn dragged(bar: Rect, pos: Pos2, last: usize) -> usize {
    let room = ((bar.height() / CELL.y).round() - 3.0).max(1.0);
    let r = ((pos.y - bar.top() - CELL.y) / CELL.y)
        .floor()
        .clamp(0.0, room);
    (r / room * last as f32).round() as usize
}

/// The F-key slot (1–10) of the window's last row under `p`, as the F-key row lays its slots
/// on whole cells ([`chrome::key_cells`]); none off that row.
fn slot_at(screen: Rect, p: Pos2) -> Option<u8> {
    if !screen.contains(p) || p.y < screen.bottom() - CELL.y {
        return None;
    }
    let cols = (screen.width() / CELL.x).floor() as u16;
    let x = ((p.x - screen.left()) / CELL.x).floor() as u16;
    (1..=10u8).find(|&n| {
        let (digit, label) = chrome::key_cells(cols, n);
        (digit.x..label.right().max(digit.right())).contains(&x)
    })
}

/// The shell's input hook (`super::FKeyInput`), after the F-key aliases and the mode button
/// took theirs: while the help is open the pointer is its own, before egui sees any of it — so
/// nothing under it, a modal's backdrop included, is hovered, pressed or clicked. Its scroll bar
/// ([`on_bar`], [`dragged`]), the wheel anywhere, and the F-key row's slots 1 and 3, pressed and
/// let go there, which close it as F1 and F3 do (another slot does nothing); every pointer event
/// then goes.
pub(crate) fn input_hook(ctx: &egui::Context, events: &mut Vec<Event>) {
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return;
    };
    let page = open.rows.saturating_sub(1).max(1);
    let last = open.total.saturating_sub(open.rows);
    let screen = ctx.content_rect();
    let mut closed = false;
    let mut lines = 0.0;
    events.retain(|e| match e {
        Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            ..
        } => {
            open.drag = false;
            let slot = slot_at(screen, *pos).filter(|n| matches!(n, 1 | 3));
            if *pressed {
                open.slot = slot;
                match open
                    .bar
                    .and_then(|bar| on_bar(bar, *pos, open.top, last, page))
                {
                    Some(Press::To(top)) => open.top = top,
                    Some(Press::Drag) => open.drag = true,
                    None => {}
                }
            } else {
                closed |= open.slot.is_some() && open.slot == slot;
                open.slot = None;
            }
            false
        }
        Event::PointerMoved(pos) => {
            if let Some(bar) = open.bar.filter(|_| open.drag) {
                open.top = dragged(bar, *pos, last);
            }
            false
        }
        Event::MouseWheel { unit, delta, .. } => {
            lines += match unit {
                egui::MouseWheelUnit::Point => -delta.y / CELL.y,
                _ => -delta.y * 3.0,
            };
            false
        }
        Event::PointerGone => {
            (open.drag, open.slot) = (false, None);
            false
        }
        Event::WindowFocused(false) => {
            (open.drag, open.slot) = (false, None);
            true
        }
        Event::PointerButton { .. }
        | Event::MouseMoved(_)
        | Event::Touch { .. }
        | Event::Zoom(_)
        | Event::Rotate(_) => false,
        _ => true,
    });
    open.top = (open.top as f32 + lines).round().clamp(0.0, last as f32) as usize;
    ctx.data_mut(|d| {
        if closed {
            d.remove_temp::<Open>(open_id());
        } else {
            d.insert_temp(open_id(), open);
        }
    });
}

/// Before the window's pass: the screen's own keys, then the rest of the frame's input withheld
/// from the window — every key but Cmd/Ctrl+Q and +W (they close it as ever), the typed text —
/// and the pointer's state cleared (its events went in [`input_hook`]; egui still holds where
/// it last was, so nothing under the screen is hovered).
pub(crate) fn begin_pass(ctx: &egui::Context) {
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return;
    };
    let page = open.rows.saturating_sub(1).max(1);
    let last = open.total.saturating_sub(open.rows);
    let closed = ctx.input_mut(|i| {
        let mut closed = false;
        for key in [Key::Escape, Key::F1, Key::F3] {
            closed |= keys::take(i, key, Mods::None);
        }
        // Cmd/Ctrl+Q or +W goes on to the window, which may ask about unsaved changes: the
        // help goes first, so its question is on screen.
        closed |= i.events.iter().any(|e| {
            matches!(e, Event::Key { key: Key::Q | Key::W, pressed: true, modifiers, .. }
                if Mods::Command.matches(*modifiers))
        });
        let steps: [(Key, isize); 4] = [
            (Key::ArrowDown, 1),
            (Key::ArrowUp, -1),
            (Key::PageDown, page as isize),
            (Key::PageUp, -(page as isize)),
        ];
        for (key, by) in steps {
            while keys::take(i, key, Mods::None) {
                open.top = open.top.saturating_add_signed(by).min(last);
            }
        }
        if keys::take(i, Key::Home, Mods::None) {
            open.top = 0;
        }
        if keys::take(i, Key::End, Mods::None) {
            open.top = last;
        }
        i.events.retain(|e| match e {
            Event::Key { key, modifiers, .. } => {
                matches!(key, Key::Q | Key::W) && Mods::Command.matches(*modifiers)
            }
            Event::ModifiersChanged(_) | Event::WindowFocused(_) => true,
            _ => false,
        });
        i.pointer = Default::default();
        i.smooth_scroll_delta = Vec2::ZERO;
        closed
    });
    ctx.data_mut(|d| {
        if closed {
            d.remove_temp::<Open>(open_id());
        } else {
            d.insert_temp(open_id(), open);
        }
    });
}

/// After the window's pass: the screen ([`lay`], [`draw`]) in cells above everything the
/// window drew, its dialogs included, and over the whole window a layer nothing shows through
/// to the pointer (as a modal's backdrop: a page view's native view hides under it).
pub(crate) fn end_pass(ctx: &egui::Context) {
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return;
    };
    let screen = ctx.content_rect();
    let laid = lay(&open.page, Grid::over(screen));
    open.rows = laid.shown;
    open.total = laid.rows.len();
    open.top = open.top.min(laid.last());
    open.bar = laid.bar().map(|bar| laid.area.rect(bar));
    let buf = draw(&laid, &open.page.title, open.top);
    let painter = ctx
        .layer_painter(egui::LayerId::new(egui::Order::Tooltip, open_id()))
        .with_clip_rect(screen);
    painter.rect_filled(screen, 0.0, Color32::TRANSPARENT);
    cells::paint(&painter, laid.area.origin, &buf, None);
    ctx.data_mut(|d| d.insert_temp(open_id(), open));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::browser::keys::{rows, Row, LANDED_SLICES};
    use crate::gui::cells;
    use crate::gui::testkit::Driver;
    use crate::gui::Window;

    fn line<'a>(page: &'a Page, keys: &str) -> Option<&'a Line> {
        page.lines.iter().find(|l| l.keys == keys)
    }

    /// F1 in the list (K-R1's help): every landed row of the list is on one line, the
    /// alternatives of one action together (plain key first), in the table's order — S6's F7 and
    /// F8 among them; the aliases last.
    #[test]
    fn the_list_page_lists_the_tables_rows() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the file list");
        for (keys, what) in [
            ("F1", "This screen"),
            ("F2, Shift+F6", "Rename the entry under the bar, in place"),
            ("F3", "The file under the bar in its viewer"),
            ("F5", "Copy to the other panel's folder (asks first)"),
            ("F6", "Move to the other panel's folder (asks first)"),
            ("Tab, Shift+Tab", "The keyboard to the other side"),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            (
                "Space, Ins",
                "Select or unselect the entry under the bar, then step down",
            ),
            ("+", "Select the entries matching a pattern"),
            ("*", "Invert the selection"),
            ("a letter", "The next name starting with it"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert!(line(&page, "Backspace, Ctrl/⌘+PgUp").is_some(), "{page:#?}");
        assert!(line(&page, "Ctrl/⌘+A").is_some());
        assert!(line(&page, "F9")
            .unwrap()
            .what
            .starts_with("Switch to dual mode"));
        let dual = for_context(Context::List, Mode::Dual, &[]);
        assert!(line(&dual, "F9")
            .unwrap()
            .what
            .starts_with("Switch to preview mode"));
        // Each landed row this machine can press is on exactly one line.
        let all: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
        for row in rows(Context::List, LANDED_SLICES) {
            let name = row.trigger.name();
            let hits = all
                .iter()
                .filter(|keys| keys.split(", ").any(|k| k == name))
                .count();
            assert_eq!(hits, 1, "{name} in {all:?}");
        }
        // S5 has landed: F10 and Alt+F10, the tree, on one line.
        assert_eq!(
            line(&page, "F10, Alt+F10").map(|l| l.what.as_str()),
            Some("The directory tree in place of this list"),
            "{page:#?}"
        );
        // S2 has landed: Cmd/Ctrl+G pins the git panel.
        assert_eq!(
            line(&page, "Ctrl/⌘+G").map(|l| l.what.as_str()),
            Some("Pin the git panel to the other side; again: unpin"),
            "{page:#?}"
        );
        // S6 has landed: F7 makes a folder; F8, Del (and ⌘⌫ on macOS) delete to the Trash,
        // Shift+F8 and Shift+Del without it — each action on one line.
        assert_eq!(
            line(&page, "F7").map(|l| l.what.as_str()),
            Some("Make a folder (asks its name)"),
            "{page:#?}"
        );
        let delete = match cfg!(target_os = "macos") {
            true => "F8, Del, ⌘+Backspace",
            false => "F8, Del",
        };
        assert_eq!(
            line(&page, delete).map(|l| l.what.as_str()),
            Some("Delete the selection or the entry (asks; locally to the Trash)"),
            "{page:#?}"
        );
        assert_eq!(
            line(&page, "Shift+F8, Shift+Del").map(|l| l.what.as_str()),
            Some("Delete without the Trash (asks)"),
            "{page:#?}"
        );
        assert!(page.lines.ends_with(&aliases()), "{page:#?}");
        assert_eq!(page.lines.first().map(|l| l.keys.as_str()), Some("F1"));
    }

    /// The rows of `context` of the slices in `upto` that a Mac (`mac`) or another machine can
    /// press — [`keys::rows`] on such a build (⌘ is a Mac's).
    fn rows_on(
        mac: bool,
        context: Context,
        upto: keys::Slices,
    ) -> impl Iterator<Item = &'static Row> {
        keys::TABLE.iter().filter(move |r| {
            r.context == context
                && upto.has(r.slice)
                && (mac || !matches!(r.trigger, Trigger::Key(_, Mods::MacCmd)))
        })
    }

    /// The help is one text in both backends (10 T27): its key names come from no per-machine
    /// input — a command row reads `Ctrl/⌘+R`, never this machine's `Cmd+R` or `Ctrl+R` — and
    /// the last lines name each alias with where it works, then where ⌘ and Ctrl apply. It
    /// lists what the machine can press: a Mac's page and another machine's, built from each
    /// one's rows, differ only by ⌘⌫ on the delete line, and this build's page is its own.
    #[test]
    fn the_help_is_one_text_on_every_machine() {
        let mac_here = cfg!(target_os = "macos");
        assert_eq!(Mods::MacCmd.here(), mac_here);
        for context in Context::ALL {
            let all = keys::ALL_SLICES;
            let page_on = |mac| page_of(context, Mode::Preview, &[], rows_on(mac, context, all));
            let (mac, other) = (page_on(true), page_on(false));
            let here = page_upto(context, Mode::Preview, &[], all);
            assert_eq!(&here, if mac_here { &mac } else { &other }, "{context:?}");
            assert_eq!(mac.title, other.title);
            assert_eq!(mac.lines.len(), other.lines.len(), "{context:?}");
            let differ: Vec<(&Line, &Line)> = mac
                .lines
                .iter()
                .zip(&other.lines)
                .filter(|(m, o)| m != o)
                .collect();
            let delete = "Delete the selection or the entry (asks; locally to the Trash)";
            match context {
                Context::List => assert_eq!(
                    differ,
                    [(
                        &Line {
                            keys: "F8, Del, ⌘+Backspace".into(),
                            what: delete.into()
                        },
                        &Line {
                            keys: "F8, Del".into(),
                            what: delete.into()
                        }
                    )],
                    "{context:?}"
                ),
                _ => assert_eq!(differ, [], "{context:?}"),
            }
            for name in mac.lines.iter().flat_map(|l| l.keys.split(", ")) {
                assert!(
                    !name.starts_with("Cmd+") && !name.starts_with("Ctrl+"),
                    "a machine's own name: {name} in {context:?}"
                );
            }
        }
        let page = page_upto(Context::List, Mode::Preview, &[], keys::ALL_SLICES);
        let keys: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
        assert!(keys.contains(&"Ctrl/⌘+R"), "{keys:?}");
        assert!(keys.contains(&"Ctrl/⌘+U"), "{keys:?}");
        // The aliases with where each works (T27, 10 §9): the F-keys' digits, the switch.
        assert_eq!(
            aliases().as_slice(),
            [
                Line {
                    keys: "Ctrl/⌘+1…0 · Alt+1…0".into(),
                    what: "F1…F10 (Alt+1…0 in a terminal); so does a click on a slot".into(),
                },
                Line {
                    keys: "F12 · Alt+G".into(),
                    what: "Between the terminal and a window, under sopen --tui (Alt+G in a \
                           terminal); so does a click on [GUI] or [TUI]"
                        .into(),
                },
                Line {
                    keys: "Ctrl/⌘".into(),
                    what: "⌘ applies in a Mac window; Ctrl everywhere else (in a terminal on \
                           every OS)"
                        .into(),
                },
            ]
        );
        // Every context's page and a window's page end with those lines, one of each.
        let window = for_window(&[chrome::HELP_KEY, chrome::EXIT_KEY]);
        for page in Context::ALL
            .iter()
            .map(|&c| page_upto(c, Mode::Preview, &[], keys::ALL_SLICES))
            .chain([window])
        {
            assert!(page.lines.ends_with(&aliases()), "{page:#?}");
            for alias in aliases() {
                assert_eq!(
                    page.lines.iter().filter(|l| **l == alias).count(),
                    1,
                    "{page:#?}"
                );
            }
        }
        // The rows that tell, per modifier: Shift, Alt, Cmd/Ctrl, Cmd/Ctrl+Shift, ⌘.
        let git = page_upto(Context::Git, Mode::Preview, &[], keys::ALL_SLICES);
        let git: Vec<&str> = git.lines.iter().map(|l| l.keys.as_str()).collect();
        assert!(git.contains(&"Ctrl/⌘+Shift+F"), "{git:?}");
        assert!(git.contains(&"Shift+F3"), "{git:?}");
        assert!(keys.contains(&"F10, Alt+F10"), "{keys:?}");
    }

    /// F1 in the tree (05 §1, landed with S5): every key of the tree's context, the aliases last.
    #[test]
    fn the_tree_page_lists_the_trees_keys() {
        let page = for_context(Context::Tree, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the directory tree");
        for (keys, what) in [
            ("F1", "This screen"),
            ("F10, Esc, Alt+F10", "Close the tree, the list as it was"),
            ("Enter", "List that folder in this panel"),
            ("→", "Open the folder (listed if it never was)"),
            ("←", "Close the folder; on a closed one: its parent"),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            ("a letter", "The next folder starting with it"),
            ("Space", "Size the folder now"),
            ("Ctrl/⌘+R", "List the folder again"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert!(page.lines.ends_with(&aliases()), "{page:#?}");
    }

    /// F1 in the git panel (03 §9; S2's keys, S3's and S4's since they landed), each on one
    /// line: the details, a file at a revision, the editor, the mode, the bar, the label cursor,
    /// marks and staging, Tab back, Esc, Find, history search, refresh, the filters, HEAD, wide,
    /// the pin; S3's slots (Pick, Rebase, Branch / Commit, the menu), Fetch all, undo, the
    /// Backups list, the command log and the banner's Cmd/Ctrl+Shift+C, S, A, B; S4's F2 (a
    /// commit's reword, a label's rename) and F8 (a commit's drop among the deletes), the
    /// planner's Cmd/Ctrl+E and an edit stop's Cmd/Ctrl+Shift+M and P (03 §9.2).
    #[test]
    fn the_git_page_lists_the_git_panels_keys() {
        let page = for_context(Context::Git, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the git panel");
        for (keys, what) in [
            ("F1", "This screen"),
            ("Shift+F3", "The file at this revision"),
            (
                "F3, Enter",
                "The row's details (with one marked row: compare the two)",
            ),
            (
                "F4",
                "The repository (uncommitted row: the file) in the editor",
            ),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            ("←", "The label cursor to the previous label"),
            ("→", "The label cursor to the next label"),
            (
                "Space, Ins",
                "Mark or unmark the row, then step down (a changed file: stage or unstage it)",
            ),
            ("Tab, Shift+Tab", "Back to the list"),
            ("Esc", "Close the innermost open thing; with none, unpin"),
            ("a letter", "Find, starting with it"),
            ("Ctrl/⌘+Shift+F", "History search"),
            ("Ctrl/⌘+F", "Find"),
            ("Ctrl/⌘+R", "Refresh (hard)"),
            ("Ctrl/⌘+J", "The bar to HEAD"),
            ("Ctrl/⌘+Shift+G", "The git panel over both sides, and back"),
            ("Ctrl/⌘+G", "Pin this git panel; again: unpin"),
            ("Ctrl/⌘+K", "The filters"),
            // S3, and S4's F2.
            ("F2", "A commit: reword it; a branch's label: rename it"),
            ("F5", "Cherry-pick it or the marked ones; a stash: apply it"),
            ("F6", "Rebase the current branch onto it"),
            ("F7", "A new branch here (uncommitted row: commit)"),
            ("F8", "Delete, drop or discard what is under the bar (asks)"),
            ("F10, Shift+F10", "The menu of what is under the bar"),
            ("Ctrl/⌘+Shift+R", "Fetch all"),
            ("Ctrl/⌘+Z", "Undo the newest backup group"),
            ("Ctrl/⌘+B", "The backups"),
            ("Ctrl/⌘+L", "The command log"),
            (
                "Ctrl/⌘+Shift+C",
                "The operation in progress: Continue (asks)",
            ),
            ("Ctrl/⌘+Shift+S", "The operation in progress: Skip (asks)"),
            ("Ctrl/⌘+Shift+A", "The operation in progress: Abort (asks)"),
            ("Ctrl/⌘+Shift+B", "Bisect in progress: Reset (asks)"),
            // S4.
            (
                "Ctrl/⌘+E",
                "Plan a history edit from the commit under the bar",
            ),
            ("Ctrl/⌘+Shift+M", "An edit stop: Amend (asks)"),
            ("Ctrl/⌘+Shift+P", "An edit stop: Split (asks)"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert!(line(&page, "F9")
            .unwrap()
            .what
            .starts_with("Switch to dual mode"));
        assert!(page.lines.ends_with(&aliases()), "{page:#?}");
    }

    /// A viewer in the browser: the table's viewer rows, then the viewer's own slots the table
    /// has no row for (the hex window's), described.
    #[test]
    fn the_viewer_page_adds_the_viewers_own_keys() {
        let mut own = vec![chrome::EXIT_KEY];
        own.extend_from_slice(chrome::HEX_KEYS);
        let page = for_context(Context::Viewer, Mode::Preview, &own);
        let keys: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
        let aliases = aliases();
        let want: Vec<&str> = [
            "F1", "F3, Esc", "F4", "1, 2, 3", "F9", "F5", "F6", "F7", "F8",
        ]
        .into_iter()
        .chain(aliases.iter().map(|l| l.keys.as_str()))
        .collect();
        assert_eq!(keys, want);
        assert_eq!(line(&page, "F5").unwrap().what, "Save (as Ctrl/⌘+S)");
        assert_eq!(line(&page, "F3, Esc").unwrap().what, "Back to the list");
        assert_eq!(
            line(&page, "F6").unwrap().what,
            "Go to an offset (as Ctrl/⌘+G)"
        );
        assert_eq!(
            line(&page, "F7").unwrap().what,
            "Save As: a new local file with your edits (as Ctrl/⌘+Alt+S)"
        );
    }

    /// A window of its own lists its row: Help, Exit (as Cmd/Ctrl+W) and its own keys.
    #[test]
    fn a_window_page_lists_its_row() {
        let row = chrome::merged(&[chrome::HELP_KEY, chrome::EXIT_KEY], chrome::IMAGE_KEYS);
        let page = for_window(&row);
        assert_eq!(page.title, "Keys");
        let own = [
            Line {
                keys: "F1".into(),
                what: "This screen".into(),
            },
            Line {
                keys: "F3".into(),
                what: "Close the window (as Ctrl/⌘+W)".into(),
            },
            Line {
                keys: "F5".into(),
                what: "Save (as Ctrl/⌘+S)".into(),
            },
            Line {
                keys: "F7".into(),
                what: "Save As: a new local file with your edits (as Ctrl/⌘+Alt+S)".into(),
            },
        ];
        let want: Vec<Line> = own.into_iter().chain(aliases()).collect();
        assert_eq!(page.lines, want);
    }

    /// Every context's page, with every slice landed, holds every row this machine can press
    /// once (S2–S5 open their contexts' help with this), and no row it cannot (⌘⌫ off a Mac).
    #[test]
    fn every_contexts_page_covers_its_rows() {
        for context in Context::ALL {
            let page = page_upto(context, Mode::Preview, &[], keys::ALL_SLICES);
            let all: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
            for row in rows_on(true, context, keys::ALL_SLICES) {
                let name = row.trigger.name();
                let hits = all
                    .iter()
                    .filter(|keys| keys.split(", ").any(|k| k == name))
                    .count();
                let here = match row.trigger {
                    Trigger::Key(_, mods) => mods.here(),
                    _ => true,
                };
                assert_eq!(hits, usize::from(here), "{context:?}: {name} in {all:?}");
            }
            if rows(context, keys::ALL_SLICES).any(|r| r.trigger == Trigger::Letter) {
                assert!(all.contains(&"a letter"), "{context:?}");
            }
            assert!(page.lines.ends_with(&aliases()), "{context:?}");
        }
    }

    /// Opens `page` on its first pass, over a label.
    struct Shows {
        page: Option<Page>,
    }

    impl Window for Shows {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if let Some(page) = self.page.take() {
                open(ui.ctx(), page);
            }
            ui.label("under the help");
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// Where `text` stands in the cells the last pass painted (none: it is not in cells), and
    /// the layer that holds it.
    fn in_cells(drv: &Driver, text: &str) -> Option<(cells::Hit, std::sync::Arc<cells::Layer>)> {
        let layers = cells::layers(&drv.ctx);
        let hit = cells::find(&layers, text).into_iter().last()?;
        Some((hit, layers[hit.layer].clone()))
    }

    /// The help is drawn in cells (10 §4–§6, TS-1 with F1's help open): its title in the top
    /// line of a double frame, a line's keys in the key colour and its words in the text colour
    /// on the body, the alias line last — all cells of one layer, a box with its shadow of black
    /// cells, at most 76 cells inside, between the title row and the status row (the rows of a
    /// window's top and foot stay as they are).
    #[test]
    fn the_help_is_drawn_in_cells() {
        let p = chrome::PALETTE;
        let page = for_context(Context::List, Mode::Preview, &[]);
        // 100 × 30 cells.
        let mut drv = Driver::new(800.0, 480.0);
        let mut win = Shows {
            page: Some(page.clone()),
        };
        drv.idle(&mut win, 2);
        let (title, layer) = in_cells(&drv, "Keys in the file list").expect("the title in cells");
        let row = |l: &cells::Layer, y: u16| cells::row_text(&l.buf, y).0;
        // The row of the box's top line (the shadow's columns after it blank there).
        let top = row(&layer, title.cells.y);
        assert!(
            top.starts_with('╔') && top.trim_end().ends_with('╗'),
            "{top:?}"
        );
        let (keys, _) = in_cells(&drv, "F2, Shift+F6").expect("the rename line's keys");
        let (words, _) =
            in_cells(&drv, "Rename the entry under the bar, in place").expect("its words");
        assert_eq!(keys.cells.y, words.cells.y, "one row");
        assert!(keys.cells.right() < words.cells.x);
        assert_eq!(cells::colors(&layer, keys.cells), [p.help_key]);
        assert_eq!(cells::colors(&layer, words.cells), [p.text]);
        let seen = layer.seen();
        let at = |x: u16, y: u16| {
            &seen[usize::from(y - layer.buf.area.y)][usize::from(x - layer.buf.area.x)]
        };
        assert_eq!(at(words.cells.x, words.cells.y).bg, Some(p.back));
        // The box: its frame's corners, its width, its place between the title and status rows,
        // its shadow two cells right and one down, black.
        let a = layer.buf.area;
        let (w, h) = (a.width - 2, a.height - 1);
        assert!(w - 4 <= 76, "at most 76 cells inside: {w}");
        assert_eq!(at(a.x, a.y + h - 1).symbol, "╚");
        assert_eq!(at(a.x + w - 1, a.y).symbol, "╗");
        // The layer's grid is the area's: its row 0 the window's row 1.
        let row0 = (layer.origin.y / CELL.y) as u16;
        assert_eq!(
            (layer.origin.x, row0),
            (0.0, 1),
            "the area under the title row"
        );
        assert!(row0 + a.bottom() <= 30 - 2, "above the status row: {a:?}");
        for (x, y) in [
            (a.x + w, a.y + 1),
            (a.x + w + 1, a.y + h - 1),
            (a.x + 2, a.y + h),
        ] {
            assert_eq!(
                at(x, y).bg,
                Some(chrome::cga::BLACK),
                "the shadow at ({x},{y})"
            );
        }
        // The rows that do not fit: the scroll bar on the frame's right line, then the rest.
        assert!(in_cells(&drv, "Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more").is_some());
        assert!(
            (0..h).any(|y| at(a.x + w - 1, a.y + y).symbol == "↑"),
            "the scroll bar"
        );
        drv.key(&mut win, Key::End, egui::Modifiers::NONE);
        let digits = aliases()[0].clone();
        let (alias, layer) = in_cells(&drv, &digits.keys).expect("the alias line in cells");
        // Its words start beside it (they wrap by cells: see below).
        let (what, _) = in_cells(&drv, "F1…F10 (Alt+1…0 in a terminal);").expect("its words");
        assert_eq!(alias.cells.y, what.cells.y);
        assert_eq!(cells::colors(&layer, alias.cells), [p.help_key]);
        assert!(
            in_cells(&drv, "⌘ applies in a Mac window;").is_some(),
            "the last line"
        );
    }

    /// The screen: the page's title in the frame, each line's keys in the key colour with its
    /// words beside them on the same row; in a short window the rest comes with End, PgDn and
    /// ↓, and goes back with Home.
    #[test]
    fn the_screen_draws_the_page_and_scrolls() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        let mut drv = Driver::new(640.0, 240.0);
        let mut win = Shows {
            page: Some(page.clone()),
        };
        drv.idle(&mut win, 2);
        assert!(is_open(&drv.ctx));
        assert!(
            drv.find_text("Keys in the file list").is_some(),
            "the title"
        );
        let keys = drv.find_text("F2, Shift+F6").expect("the rename line");
        let words = drv
            .find_text("Rename the entry under the bar, in place")
            .expect("its words");
        assert_eq!(keys.top(), words.top(), "one row");
        assert!(keys.right() < words.left());
        assert_eq!(
            drv.colors_of("F2, Shift+F6"),
            Some(vec![chrome::PALETTE.help_key])
        );
        let [digits, switch, command] = aliases();
        assert!(drv.find_text(&digits.keys).is_none(), "below the screen");
        assert!(drv
            .find_text("Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more")
            .is_some());
        drv.key(&mut win, Key::End, egui::Modifiers::NONE);
        // Whole, not cut: the alias keys fit the key column.
        assert!(drv.find_text(&switch.keys).is_some(), "End: the aliases");
        assert!(drv.find_text(&command.keys).is_some(), "End: the last line");
        drv.key(&mut win, Key::ArrowUp, egui::Modifiers::NONE);
        drv.key(&mut win, Key::ArrowUp, egui::Modifiers::NONE);
        assert!(
            drv.find_text(&digits.keys).is_some(),
            "↑ ↑: the digits' alias"
        );
        assert!(drv.find_text("This screen").is_none());
        drv.key(&mut win, Key::Home, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_some());
        drv.key(&mut win, Key::ArrowDown, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_none(), "↓: a line on");
        drv.key(&mut win, Key::PageUp, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_some());
        drv.key(&mut win, Key::Escape, egui::Modifiers::NONE);
        assert!(!is_open(&drv.ctx));
        assert!(drv.find_text("Keys in the file list").is_none());
        assert!(drv.find_text("under the help").is_some());
    }

    /// A window of its own: its F-key row ([`crate::gui::own_keys`]) under a label.
    /// …with a button that counts its clicks, and F5 (Save) on its row, counted too.
    #[derive(Default)]
    struct WithRow {
        clicks: usize,
        saves: usize,
    }

    impl Window for WithRow {
        fn ui(&mut self, ui: &mut egui::Ui) {
            crate::gui::own_keys(
                ui,
                &[chrome::FKey {
                    n: 5,
                    label: "Save",
                }],
            );
            self.saves +=
                usize::from(ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F5)));
            if ui.button("under the help").clicked() {
                self.clicks += 1;
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// The F-key row stays in sight under the help, and its Help and Exit slots close it (as F1
    /// and F3 do): a click on the Help slot closes it, on the Exit slot too — the help, not the
    /// window. The window gets no click meanwhile: neither its button nor another slot (Save)
    /// acts, and the help stays.
    #[test]
    fn the_f_key_rows_help_and_exit_slots_close_the_help_and_nothing_else_acts() {
        let mut drv = Driver::new(800.0, 480.0);
        let mut win = WithRow::default();
        drv.idle(&mut win, 2);
        let button = drv
            .find_text("under the help")
            .expect("the button")
            .center();
        drv.click(&mut win, button);
        assert_eq!(win.clicks, 1, "the control: the button counts");
        drv.key(&mut win, Key::F1, egui::Modifiers::NONE);
        assert!(is_open(&drv.ctx));
        drv.click_text(&mut win, "Help   ");
        drv.idle(&mut win, 1);
        assert!(!is_open(&drv.ctx), "the Help slot: F1");
        drv.key(&mut win, Key::F1, egui::Modifiers::NONE);
        assert!(is_open(&drv.ctx));
        drv.click(&mut win, button);
        drv.click_text(&mut win, "Save   ");
        drv.idle(&mut win, 2);
        assert_eq!(
            (win.clicks, win.saves),
            (1, 0),
            "no click reached the window"
        );
        assert!(is_open(&drv.ctx), "a click elsewhere: nothing");
        drv.click_text(&mut win, "Exit   ");
        drv.idle(&mut win, 1);
        assert!(!is_open(&drv.ctx), "the Exit slot: F3 closes the help");
        assert!(!drv.closed(), "not the window");
        drv.click_text(&mut win, "Save   ");
        drv.idle(&mut win, 2);
        assert_eq!(
            win.saves, 1,
            "the control: the Save slot is F5 without the help"
        );
    }

    /// On narrow and tiny grids (10 §6: the box less the grid's eight cells) the help writes
    /// nothing outside its box's inside but its frame, its scroll bar and its shadow: the words
    /// never over the frame's right line or into the shadow — under 12 cells beside the keys
    /// they go under them — and the margin cells stay blank; no grid panics, 0 × 0 included.
    #[test]
    fn on_narrow_grids_the_help_stays_inside_its_box() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        let p = chrome::PALETTE;
        for (cols, rows) in [
            (0u16, 0u16),
            (1, 1),
            (9, 4),
            (9, 30),
            (10, 30),
            (20, 30),
            (34, 30),
        ] {
            let screen = Grid {
                origin: Pos2::ZERO,
                cols,
                rows,
            };
            let laid = lay(&page, screen);
            let buf = draw(&laid, &page.title, laid.last());
            let f = laid.frame;
            let [right, under] = dialog::shadow_of(f);
            let bar = laid.bar();
            let a = buf.area;
            for y in a.top()..a.bottom() {
                for x in a.left()..a.right() {
                    let at = cells::Position::new(x, y);
                    let c = &buf[(x, y)];
                    let what = format!("{cols}x{rows} ({x},{y}) {:?} in {f:?}", c.symbol());
                    let shown = c.symbol().trim().to_string();
                    if !f.contains(at) && (right.contains(at) || under.contains(at)) {
                        assert_eq!(c.bg, ratatui_core::style::Color::Rgb(0, 0, 0), "{what}");
                        assert_eq!(shown, "", "the shadow: {what}");
                    } else if !f.contains(at) {
                        assert_eq!(
                            *c,
                            ratatui_core::buffer::Cell::default(),
                            "untouched: {what}"
                        );
                    } else if y == f.top() {
                        // The top line: the frame's and the title's.
                    } else if bar.is_some_and(|b| b.contains(at)) {
                        assert!(
                            shown.is_empty()
                                || cells::frame_cell(c.symbol(), Some(p.track), Some(p.track_back)),
                            "the scroll bar: {what}"
                        );
                    } else if x == f.left() || x + 1 == f.right() || y + 1 == f.bottom() {
                        assert!(
                            shown.chars().all(cells::frame_glyph) && !shown.is_empty(),
                            "the frame: {what}"
                        );
                    } else if x == f.left() + 1 || x + 2 == f.right() || y == f.top() + 1 {
                        assert_eq!(shown, "", "the margin: {what}");
                    }
                }
            }
            // Each row shown holds its keys and its words, none drawn over.
            let list = laid.list();
            let shown = laid.rows.iter().skip(laid.last()).take(laid.shown);
            for (k, (keys, words)) in shown.enumerate() {
                let y = list.y + k as u16;
                if !a.contains(cells::Position::new(list.x, y)) {
                    continue;
                }
                let row = cells::row_text(&buf, y).0;
                for text in [keys, words] {
                    assert!(
                        row.contains(text.trim()),
                        "{cols}x{rows}: {text:?} in {row:?}"
                    );
                }
            }
            // In a window too: no panic, and the keys work.
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = Shows {
                page: Some(page.clone()),
            };
            drv.idle(&mut win, 2);
            drv.key(&mut win, Key::End, egui::Modifiers::NONE);
            drv.key(&mut win, Key::Escape, egui::Modifiers::NONE);
            assert!(!is_open(&drv.ctx));
        }
        // Beside the keys from 12 cells of words on; under them below.
        let at = |cols| {
            let grid = Grid {
                origin: Pos2::ZERO,
                cols,
                rows: 30,
            };
            lay(&page, grid).rows[..2].to_vec()
        };
        let (f1, words) = ("F1".to_string(), "This screen".to_string());
        assert_eq!(
            at(80),
            [
                (f1.clone(), words.clone()),
                (
                    "F2, Shift+F6".into(),
                    "Rename the entry under the bar, in place".into()
                )
            ]
        );
        // 12 cells inside: the words under the keys, two cells in, wrapped at the other ten.
        let first = words.split(' ').next().unwrap_or_default().to_string();
        assert_eq!(
            at(20),
            [(f1, String::new()), (String::new(), first)],
            "under the keys"
        );
    }

    /// The screen's state as the last pass left it.
    fn state(drv: &Driver) -> Open {
        drv.ctx
            .data(|d| d.get_temp::<Open>(open_id()))
            .expect("the help is open")
    }

    /// A long help's scroll bar, as a dialog's (10 §6): ↓ and ↑ a row, a click on the track
    /// below the thumb a page, the thumb dragged to the track's end the end, back to the top;
    /// the wheel three rows a notch — and the thumb drawn where the rows are.
    #[test]
    fn a_long_help_scrolls_by_its_scroll_bar_and_the_wheel() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        // 80 × 15 cells: a few rows at once.
        let mut drv = Driver::new(640.0, 240.0);
        let mut win = Shows { page: Some(page) };
        drv.idle(&mut win, 2);
        let first = state(&drv);
        let bar = first.bar.expect("a scroll bar: the rows do not all fit");
        let (shown, last) = (first.rows, first.total - first.rows);
        let height = (bar.height() / CELL.y).round() as usize;
        assert_eq!(height, shown, "beside the rows");
        assert!(shown >= 4 && last > shown, "{shown} of {}", first.total);
        let at = |k: usize| Pos2::new(bar.center().x, bar.top() + CELL.y * (k as f32 + 0.5));
        let top = |drv: &Driver| state(drv).top;
        drv.click(&mut win, at(height - 1));
        assert_eq!(top(&drv), 1, "↓: a row");
        drv.click(&mut win, at(0));
        assert_eq!(top(&drv), 0, "↑: back");
        drv.click(&mut win, at(height - 2));
        assert_eq!(top(&drv), shown - 1, "the track below the thumb: a page");
        // The thumb's cell, as `cells::scroll_bar` puts it; dragged to the track's end.
        let room = (height - 3) as f32;
        let thumb = 1 + (room * top(&drv) as f32 / last as f32).round() as usize;
        drv.drag(
            &mut win,
            egui::PointerButton::Primary,
            at(thumb),
            at(height - 2),
        );
        assert_eq!(top(&drv), last, "the thumb dragged to the end");
        let layers = cells::layers(&drv.ctx);
        let help = &layers.last().expect("the help's cells").buf;
        let bar_x = (bar.left() / CELL.x) as u16;
        let bar_y = ((bar.top() - CELL.y) / CELL.y) as u16;
        let thumb_bg = help[(bar_x, bar_y + height as u16 - 2)].bg;
        assert_eq!(
            thumb_bg,
            ratatui_core::style::Color::Rgb(0xaa, 0, 0),
            "the thumb at the end"
        );
        drv.drag(
            &mut win,
            egui::PointerButton::Primary,
            at(height - 2),
            at(1),
        );
        assert_eq!(top(&drv), 0, "and back to the top");
        // A notch of the wheel, off the box as on it: three rows.
        drv.wheel(&mut win, Pos2::new(4.0, 20.0), -1.0);
        assert_eq!(top(&drv), 3);
        drv.wheel(&mut win, at(0), 1.0);
        assert_eq!(top(&drv), 0);
    }

    /// A line longer than the words column wraps by cells under the words column (its keys on
    /// its first row alone), every word kept; on an area too short for the box the box is cut to
    /// it — never over the title row or the status and F-key rows.
    #[test]
    fn the_help_wraps_by_cells_and_stays_between_the_rows() {
        let words = "Rename the entry under the bar, in place, and then — since the row is \
                     long — say a good deal more than one row of the box can hold, by cells";
        let page = Page {
            title: "Keys".into(),
            lines: vec![
                Line {
                    keys: "F2".into(),
                    what: words.into(),
                },
                Line {
                    keys: "F3".into(),
                    what: "View".into(),
                },
            ],
        };
        let mut drv = Driver::new(400.0, 240.0);
        let mut win = Shows {
            page: Some(page.clone()),
        };
        drv.idle(&mut win, 2);
        let (keys, layer) = in_cells(&drv, "F2").expect("the keys");
        let (first, _) = in_cells(&drv, "Rename the entry").expect("its words");
        assert_eq!(first.cells.y, keys.cells.y);
        let laid = lay(&page, Grid::over(drv.ctx.content_rect()));
        let (rest, _) = in_cells(&drv, &laid.rows[1].1).expect("its second row");
        assert_eq!(
            (rest.cells.x, rest.cells.y),
            (first.cells.x, first.cells.y + 1),
            "the rest under the words: {:?}",
            laid.rows
        );
        assert_eq!(laid.rows[1].0, "", "the keys on the first row alone");
        assert!(cells::colors(&layer, keys.cells) == [chrome::PALETTE.help_key]);
        // Every word, in order, none lost at a break.
        let all: Vec<&str> = (laid.rows.iter())
            .filter(|(k, _)| k != "F3")
            .flat_map(|(_, w)| w.split_whitespace())
            .collect();
        assert_eq!(all, words.split_whitespace().collect::<Vec<_>>());
        assert!(laid.rows.len() > 3, "wrapped: {:?}", laid.rows);
        assert!(laid.rows.iter().all(|(_, w)| cells::width(w) <= laid.words));
        // A short window (50 × 8 cells): one row of five between the rows, the box cut to it.
        let mut drv = Driver::new(400.0, 128.0);
        let mut win = Shows { page: Some(page) };
        drv.idle(&mut win, 2);
        let layers = cells::layers(&drv.ctx);
        let help = layers.last().expect("the help's cells");
        assert_eq!(help.origin.y, CELL.y, "under the title row");
        assert!(
            help.buf.area.bottom() <= 5,
            "above the status row: {:?}",
            help.buf.area
        );
        assert!(in_cells(&drv, "F2").is_some(), "a row shows");
    }
}
