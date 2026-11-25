//! The terminal backend (10 §4, X1b): `sopen --tui` runs the file browser in this terminal. Each
//! pass is the window's own — a headless `egui::Context` in the skin, the browser's `ui`, the
//! shell's input hook ([`super::FKeyInput`]) — fed by the translator ([`TermInput`]) from
//! crossterm's events, and what it paints in cells ([`cells`]) is laid onto one grid the size of
//! the terminal ([`compose`]) and printed, only the cells that changed ([`Term`]). So the
//! terminal and the window show the same cells (10 §5, TS-1).
//!
//! - **Entering** (T7, T28): raw mode, the alternate screen, mouse reporting (presses, releases
//!   and drags, SGR-encoded), bracketed paste, focus reports, the cursor hidden; the kitty
//!   keyboard protocol's flag 1 pushed when the terminal answers `CSI ? u`, so Ctrl+digits and
//!   the Ctrl+Shift chords arrive whole, else the legacy encoding and its losses (§9); then the
//!   terminal's unread input is dropped — keys typed while it was away were meant for the window.
//! - **Leaving** (T19): every mode off, the flags popped only if pushed, the cursor shown, the
//!   main screen back, raw mode off — once, whatever ends the run: a close, a panic in a pass
//!   (the message printed after the terminal is back), SIGTERM, SIGHUP or SIGINT (exit 128 + n).
//!   A write that fails (a terminal gone with SIGHUP) is ignored there.
//! - **The grid** is the terminal's size in cells, each 8×16 points as in the window; a resize
//!   lays the browser out again at the new size and prints the whole screen.
//! - **Layers** are laid in egui's order — its layer order, then the order of the calls — each
//!   inside its clip, a cell with no RGB background and no glyph leaving what is under it, as the
//!   window paints them. The cursor stands at the caret's cell (a field's: the terminal draws a
//!   Hangul composition there, 10 §4), else it is hidden.
//! - **Hand-off boxes** (T17, 10 §11): cells no layer covers are a pane the window draws in egui
//!   (the preview, a viewer, the tree, the git panel) — in the terminal a frame saying `F12 shows
//!   it in the window`; a click on it is the switch.
//! - **Colours** are sent as RGB when `COLORTERM` says `truecolor` or `24bit`, else through
//!   [`cells::index256`]; never the 16 indexes a theme repaints (T15).
//! - **The clipboard** (§4): egui's `CopyText` is OSC 52; the terminal's paste arrives bracketed.
//! - **The switch** (§7.1–§7.2, T4): F12, Alt+G (the shell's hook, in the terminal only) or a
//!   click on a hand-off box. Where the window would open is read then, as 01 §4a resolves a run;
//!   a HOST says it comes with the remote switch (X2) and no display says how to get one — on the
//!   status row, until the next key. Else the terminal is left, the browser runs in a window of
//!   the terminal's grid size (T16) and comes back with its state when F12 is pressed there or the
//!   window closes (Cmd/Ctrl+Q and +W too); the focused field's edit state goes and comes back
//!   with it. A window that does not open brings the terminal back with the reason (T31).

use super::browser::{self, Browser};
use super::cells::{self, Buffer, Cells, Layer, Pen, Position};
use super::chrome::{self, FKey, CELL};
use super::term_input::TermInput;
use super::{Hooks, Outcome, Window};
use anyhow::Result;
use crossterm::event::{Event as CEvent, KeyEventKind, MouseButton, MouseEventKind};
use eframe::egui::{self, Pos2, Rect, Vec2};
use ratatui_core::style::Color;
use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Entering: the alternate screen, mouse reporting (1000 presses and releases, 1002 drags, 1006
/// SGR coordinates), bracketed paste, focus reports, the cursor hidden, the screen cleared.
pub(crate) const ENTER: &str =
    "\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[2J";
/// The kitty keyboard protocol's flag 1 (DISAMBIGUATE_ESCAPE_CODES), pushed and popped.
pub(crate) const KITTY_PUSH: &str = "\x1b[>1u";
pub(crate) const KITTY_POP: &str = "\x1b[<1u";
/// Leaving: focus reports, bracketed paste and every mouse mode off (1015 and 1003 too, as
/// crossterm turns them off), the colours reset, the cursor shown, the main screen back.
pub(crate) const LEAVE: &str = "\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1015l\x1b[?1003l\
     \x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

/// The line printed on the main screen while the browser is in a window (10 §7.2).
pub(crate) const IN_A_WINDOW: &str =
    "xtools is in a window now — close it, or press F12 there, to come back here.";
/// What a hand-off box says (10 §11).
pub(crate) const HAND_OFF: &str = "F12 shows it in the window";
/// The status line for F1 in the terminal: its help is egui's, not yet cells.
pub(crate) const F1_LINE: &str = "F1's help: F12 shows it in the window";
/// The status line while a dialog egui draws (not yet cells) is open.
pub(crate) const MODAL_LINE: &str =
    "a dialog the terminal does not draw yet: F12 shows it in the window, Esc closes it";
/// The status line when there is no display for the window (10 §7.1).
pub(crate) const NO_DISPLAY: &str =
    "no display here — sopen config target <host>, or --target, opens the window on another machine";

/// The longest a wait for input lasts before a signal is looked for again.
const TICK: Duration = Duration::from_millis(100);
/// The shortest time between two passes no input asked for (an animation asking for every
/// frame): the terminal is not a display with a refresh rate.
const IDLE_FLOOR: Duration = Duration::from_millis(8);

// ---------------------------------------------------------------------------------------------
// The terminal.

/// What the backend asks of its terminal besides the bytes it writes: crossterm's and the
/// system's in a real one ([`Real`]), a script in the tests.
pub(crate) trait Tty {
    /// Raw mode on or off — for the real terminal also the signals that end the run.
    fn raw(&mut self, on: bool) -> io::Result<()>;
    /// The terminal's size in cells.
    fn size(&mut self) -> io::Result<(u16, u16)>;
    /// Whether the terminal answers `CSI ? u` (the kitty keyboard protocol); asked in raw mode.
    fn kitty(&mut self) -> bool;
    /// Drops what the terminal sent that nobody read (10 §4, T28).
    fn drop_input(&mut self);
    /// The next event, waiting at most `wait` for it.
    fn next(&mut self, wait: Duration) -> io::Result<Option<CEvent>>;
    /// Whether an event is waiting.
    fn pending(&mut self) -> io::Result<bool>;
    /// The signal that ends the run, once one came.
    fn stopped(&self) -> Option<i32>;
}

/// The terminal sopen runs in: crossterm's raw mode, size, keyboard query and events; the
/// system's input queue; SIGTERM, SIGHUP and SIGINT caught while raw mode is on ([`Signals`]).
#[derive(Default)]
struct Real {
    signals: Option<Signals>,
}

impl Tty for Real {
    fn raw(&mut self, on: bool) -> io::Result<()> {
        if on {
            crossterm::terminal::enable_raw_mode()?;
            self.signals = Some(Signals::watch());
        } else {
            self.signals = None;
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    fn size(&mut self) -> io::Result<(u16, u16)> {
        crossterm::terminal::size()
    }

    fn kitty(&mut self) -> bool {
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    }

    fn drop_input(&mut self) {
        // SAFETY: a plain call on the standard input, a terminal (`--tui` asks for one).
        unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
        // And what crossterm already read: bounded, as a key held down keeps sending.
        for _ in 0..4096 {
            match crossterm::event::poll(Duration::ZERO) {
                Ok(true) if crossterm::event::read().is_ok() => {}
                _ => break,
            }
        }
    }

    fn next(&mut self, wait: Duration) -> io::Result<Option<CEvent>> {
        if crossterm::event::poll(wait)? {
            crossterm::event::read().map(Some)
        } else {
            Ok(None)
        }
    }

    fn pending(&mut self) -> io::Result<bool> {
        crossterm::event::poll(Duration::ZERO)
    }

    fn stopped(&self) -> Option<i32> {
        let signal = STOP.load(Ordering::SeqCst);
        (signal != 0).then_some(signal)
    }
}

/// The signal that ended the run (0: none yet).
static STOP: AtomicI32 = AtomicI32::new(0);

const STOPS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// The handler: one atomic store, async-signal-safe.
extern "C" fn stop(signal: libc::c_int) {
    let _ = STOP.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

/// SIGTERM, SIGHUP and SIGINT end the run while the terminal is held — the loop sees it within
/// [`TICK`] and leaves the terminal as it found it (T19) — and are put back as they were when it
/// is let go (a switched window ends by them as any window does). A signal that was ignored
/// stays ignored.
struct Signals {
    before: [Option<libc::sigaction>; 3],
}

impl Signals {
    fn watch() -> Signals {
        STOP.store(0, Ordering::SeqCst);
        let mut before = [None; 3];
        for (signal, before) in STOPS.into_iter().zip(&mut before) {
            // SAFETY: whole `sigaction`s; the handler only stores into an atomic.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                new.sa_flags = libc::SA_RESTART;
                if libc::sigaction(signal, &new, std::ptr::null_mut()) == 0 {
                    *before = Some(old);
                }
            }
        }
        Signals { before }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, before) in STOPS.into_iter().zip(&self.before) {
            if let Some(before) = before {
                // SAFETY: the disposition found when the watch started.
                unsafe { libc::sigaction(signal, before, std::ptr::null_mut()) };
            }
        }
    }
}

/// A run of the terminal ended by a signal (exit 128 + the signal, as a shell reports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signalled(pub i32);

impl Signalled {
    pub fn exit_code(&self) -> u8 {
        128u8.saturating_add(u8::try_from(self.0).unwrap_or(0))
    }
}

impl std::fmt::Display for Signalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            libc::SIGHUP => f.write_str("the terminal hung up"),
            libc::SIGINT => f.write_str("interrupted (SIGINT)"),
            libc::SIGTERM => f.write_str("terminated (SIGTERM)"),
            n => write!(f, "ended by signal {n}"),
        }
    }
}

impl std::error::Error for Signalled {}

/// Whether `COLORTERM` (its value) says the terminal takes RGB colours (10 §4).
pub(crate) fn truecolor(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor" | "24bit"))
}

// ---------------------------------------------------------------------------------------------
// The screen.

/// What the terminal shows: the grid as the layers lay it, the cursor's cell, and the hand-off
/// boxes (a click on one is the switch).
#[derive(Debug, Clone)]
pub(crate) struct Screen {
    pub buf: Buffer,
    pub caret: Option<Position>,
    pub hand_offs: Vec<Cells>,
}

/// The cell under `p` (points) on a grid whose cell (0, 0) is at the screen's corner.
fn cell_at(p: Pos2) -> (i32, i32) {
    ((p.x / CELL.x).round() as i32, (p.y / CELL.y).round() as i32)
}

/// The grid of `cols` × `rows` as `layers` (one pass's, [`cells::layers`]) lay it: the skin's
/// body under everything; then each layer in egui's painting order — its layer order (the
/// areas' order of `ctx`, back to front), then the order of the calls — each cell whose centre
/// is inside the layer's clip. A cell with no RGB background and a blank keeps what is under it
/// (the window paints nothing there); no RGB foreground is the skin's text colour. A wide glyph
/// takes its second cell too, and a glyph written over half of one erases the rest of it, as a
/// terminal does. Cells no layer covers are hand-off boxes.
pub(crate) fn compose(ctx: &egui::Context, layers: &[Arc<Layer>], cols: u16, rows: u16) -> Screen {
    let p = chrome::PALETTE;
    let all = Cells::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(all);
    cells::fill(&mut buf, all, Pen::new(p.text, p.back));
    let mut covered = vec![false; usize::from(cols) * usize::from(rows)];
    let order: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    let mut sorted: Vec<(usize, &Arc<Layer>)> = layers.iter().enumerate().collect();
    sorted.sort_by_key(|&(call, l)| {
        let area = order.iter().position(|o| *o == l.layer);
        (l.layer.order, area.unwrap_or(usize::MAX), call)
    });
    let screen = Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y),
    );
    let mut caret = None;
    for (_, l) in sorted {
        let visible = l.clip.intersect(screen);
        let (ox, oy) = cell_at(l.origin);
        let on_screen = |x: u16, y: u16| {
            let (sx, sy) = (ox + i32::from(x), oy + i32::from(y));
            let inside = (0..i32::from(cols)).contains(&sx) && (0..i32::from(rows)).contains(&sy);
            let seen = visible.contains(l.rect(Cells::new(x, y, 1, 1)).center());
            (inside && seen).then_some((sx as u16, sy as u16))
        };
        let a = l.buf.area;
        for y in a.top()..a.bottom() {
            let mut x = a.left();
            while x < a.right() {
                let cell = &l.buf[(x, y)];
                let width = (cells::width(cell.symbol()) as u16).clamp(1, 2);
                let width = width.min(a.right() - x);
                if let Some((sx, sy)) = on_screen(x, y) {
                    if lay(&mut buf, sx, sy, cell, width) {
                        for k in 0..width.min(cols - sx) {
                            covered[usize::from(sy) * usize::from(cols) + usize::from(sx + k)] =
                                true;
                        }
                    }
                }
                x += width;
            }
        }
        if let Some((x, y)) = l.caret.and_then(|c| on_screen(c.x, c.y)) {
            caret = Some(Position::new(x, y));
        }
    }
    let hand_offs = uncovered(&covered, cols, rows);
    for &r in &hand_offs {
        hand_off(&mut buf, r);
    }
    Screen {
        buf,
        caret,
        hand_offs,
    }
}

/// Lays `cell` of a layer, `width` cells wide, at (`x`, `y`) of `buf`; false when it leaves
/// what is under it (no background, no glyph).
fn lay(buf: &mut Buffer, x: u16, y: u16, cell: &ratatui_core::buffer::Cell, width: u16) -> bool {
    let symbol = cell.symbol();
    let blank = symbol.trim().is_empty();
    let bg = match cell.bg {
        Color::Rgb(..) => cell.bg,
        _ if blank => return false,
        _ => buf[(x, y)].bg,
    };
    let fg = match cell.fg {
        Color::Rgb(..) => cell.fg,
        _ => rgb(chrome::PALETTE.text),
    };
    // The cells it takes here; a glyph cut by the screen's edge, a control character (it would
    // move the terminal's cursor) and a wide blank are blanks.
    let span = width.min(buf.area.right() - x);
    let printable = !symbol.chars().any(char::is_control);
    let shown = if printable && !blank && cells::width(symbol) == usize::from(span) {
        symbol
    } else {
        " "
    };
    // Half of a wide glyph to the left written over: the rest of it goes.
    if x > buf.area.left() && cells::width(buf[(x - 1, y)].symbol()) > 1 {
        buf[(x - 1, y)].set_symbol(" ");
    }
    for k in 0..span {
        let c = &mut buf[(x + k, y)];
        c.set_symbol(if k == 0 { shown } else { " " });
        c.fg = fg;
        c.bg = bg;
        c.modifier = ratatui_core::style::Modifier::empty();
    }
    true
}

fn rgb(c: egui::Color32) -> Color {
    Color::Rgb(c.r(), c.g(), c.b())
}

/// The rectangles of cells no layer covers (`covered`, row by row): from each uncovered cell
/// not taken yet, as far right as the row goes uncovered, then as far down as those cells all
/// do — a pane is a rectangle, so it is found whole.
fn uncovered(covered: &[bool], cols: u16, rows: u16) -> Vec<Cells> {
    let (w, h) = (usize::from(cols), usize::from(rows));
    let mut taken = covered.to_vec();
    let mut out = vec![];
    for y in 0..h {
        for x in 0..w {
            if taken[y * w + x] {
                continue;
            }
            let right = (x..w).take_while(|&k| !taken[y * w + k]).count();
            let down = (y..h)
                .take_while(|&r| (x..x + right).all(|k| !taken[r * w + k]))
                .count();
            for r in y..y + down {
                taken[r * w + x..r * w + x + right].fill(true);
            }
            out.push(Cells::new(x as u16, y as u16, right as u16, down as u16));
        }
    }
    out
}

/// A hand-off box in `r` (10 §11): a single-line frame, and in its middle [`HAND_OFF`].
fn hand_off(buf: &mut Buffer, r: Cells) {
    let p = chrome::PALETTE;
    let inner = cells::frame(buf, r, "", false);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let shown = cells::fit_start(HAND_OFF, usize::from(inner.width));
    let w = cells::width(&shown) as u16;
    let x = inner.x + (inner.width - w) / 2;
    let y = inner.y + (inner.height - 1) / 2;
    cells::put(buf, x, y, &shown, w.into(), Pen::new(p.text, p.back));
}

/// `line` on the status row — the row above the F-key row — in the error colour, from its
/// second cell, cut with "…" to the row less a cell at either end (as `chrome::status_cells`).
fn say(buf: &mut Buffer, line: &str) {
    let a = buf.area;
    if a.height < 2 || a.width < 3 {
        return;
    }
    let p = chrome::PALETTE;
    let y = a.bottom() - 2;
    cells::fill(
        buf,
        Cells::new(a.x, y, a.width, 1),
        Pen::new(p.text, p.back),
    );
    let room = usize::from(a.width) - 2;
    let shown = cells::fit_start(line, room);
    cells::put(buf, a.x + 1, y, &shown, room, Pen::new(p.error, p.back));
}

// ---------------------------------------------------------------------------------------------
// Printing.

/// What has been printed into a terminal, and what prints the next screen there.
pub(crate) struct Term<O: Write> {
    pub out: O,
    truecolor: bool,
    /// The screen as the terminal shows it; `None`: unknown, the next is printed whole.
    shown: Option<Buffer>,
    /// Where the terminal's cursor is shown (`None`: hidden).
    cursor: Option<Position>,
}

impl<O: Write> Term<O> {
    pub fn new(out: O, truecolor: bool) -> Self {
        Term {
            out,
            truecolor,
            shown: None,
            cursor: None,
        }
    }

    /// The terminal's screen is unknown (entered, resized): the next is printed whole.
    fn forget(&mut self) {
        self.shown = None;
        self.cursor = None;
    }

    /// An SGR colour: RGB, or its 256-colour index (10 §5).
    fn colour(&self, c: Color, ground: u8) -> String {
        match c {
            Color::Rgb(r, g, b) if self.truecolor => format!("{ground}8;2;{r};{g};{b}"),
            Color::Rgb(r, g, b) => {
                let i = cells::index256(egui::Color32::from_rgb(r, g, b));
                format!("{ground}8;5;{i}")
            }
            _ => format!("{ground}9"),
        }
    }

    /// Prints `next`: the cells that differ from what is shown (all of them when that is not
    /// known), each run from where the cursor stands, a colour change only where the colours
    /// change; then the cursor at the caret, or hidden.
    pub fn draw(&mut self, next: &Screen) -> io::Result<()> {
        let mut bytes = String::new();
        let whole;
        let updates = match &self.shown {
            Some(shown) if shown.area == next.buf.area => shown.diff(&next.buf),
            _ => {
                // The cursor hidden first: where the terminal shows it is not known (a resize),
                // and it stays hidden unless the screen has a caret.
                bytes.push_str("\x1b[?25l\x1b[2J");
                self.cursor = None;
                whole = every_cell(&next.buf);
                whole
                    .iter()
                    .map(|&(x, y)| (x, y, &next.buf[(x, y)]))
                    .collect()
            }
        };
        let moved = !updates.is_empty();
        let mut at: Option<(u16, u16)> = None;
        let mut pen: Option<(Color, Color)> = None;
        for (x, y, cell) in updates {
            if at != Some((x, y)) {
                bytes.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            }
            if pen != Some((cell.fg, cell.bg)) {
                let fg = self.colour(cell.fg, 3);
                let bg = self.colour(cell.bg, 4);
                bytes.push_str(&format!("\x1b[{fg};{bg}m"));
                pen = Some((cell.fg, cell.bg));
            }
            let symbol = cell.symbol();
            bytes.push_str(if symbol.is_empty() { " " } else { symbol });
            at = Some((x + (cells::width(symbol) as u16).max(1), y));
        }
        match next.caret {
            Some(c) if moved || self.cursor != Some(c) => {
                bytes.push_str(&format!("\x1b[{};{}H", c.y + 1, c.x + 1));
                if self.cursor.is_none() {
                    bytes.push_str("\x1b[?25h");
                }
            }
            None if self.cursor.is_some() => bytes.push_str("\x1b[?25l"),
            _ => {}
        }
        self.out.write_all(bytes.as_bytes())?;
        self.out.flush()?;
        self.shown = Some(next.buf.clone());
        self.cursor = next.caret;
        Ok(())
    }

    /// `text` to the terminal's clipboard: OSC 52 (10 §4).
    pub fn copy(&mut self, text: &str) -> io::Result<()> {
        write!(self.out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
        self.out.flush()
    }
}

/// Every cell of `buf` that starts a glyph, row by row — a wide glyph's second cell left out.
fn every_cell(buf: &Buffer) -> Vec<(u16, u16)> {
    let a = buf.area;
    let mut out = vec![];
    for y in a.top()..a.bottom() {
        let mut x = a.left();
        while x < a.right() {
            out.push((x, y));
            x += (cells::width(buf[(x, y)].symbol()) as u16).clamp(1, 2);
        }
    }
    out
}

/// `bytes` in base64 (RFC 4648, padded), as OSC 52 carries them.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The loop.

/// How a run of the terminal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The browser closed (Cmd/Ctrl+Q, its own close), as a window closes.
    Closed,
    /// A signal ([`Tty::stopped`]).
    Signal(i32),
}

/// One pass's word to the loop.
struct Passed {
    /// A close went through: the browser ends.
    closed: bool,
    /// The switch was asked for (F12, Alt+G).
    switch: bool,
    /// When egui wants the next pass.
    repaint: Duration,
}

/// The terminal backend: a terminal, the bytes going to it, the context its passes run in.
pub(crate) struct Backend<T: Tty, O: Write> {
    pub tty: T,
    pub term: Term<O>,
    pub ctx: egui::Context,
    input: TermInput,
    /// The grid: the terminal's size in cells.
    pub size: (u16, u16),
    /// Whether the terminal answered the kitty keyboard query (asked once).
    kitty: Option<bool>,
    /// The kitty flag is pushed now.
    pushed: bool,
    /// The terminal is held (raw mode, the alternate screen, the modes).
    held: bool,
    /// A close the last pass asked for, delivered with the next one.
    close_pending: bool,
    /// The backend's own line on the status row (a switch that did not happen), until the next
    /// key or click.
    said: Option<String>,
    /// The hand-off boxes the last screen showed.
    hand_offs: Vec<Cells>,
    /// The passes' clock: `offset` + the time since the backend was made.
    started: Instant,
    pub offset: f64,
}

impl<T: Tty, O: Write> Backend<T, O> {
    /// The backend on `tty` writing `out`, its passes in `ctx` (in the skin already:
    /// [`chrome::install`]), which it marks as the terminal's (Alt+digits and Alt+G, §9) and
    /// records cells in.
    pub fn new(tty: T, out: O, ctx: egui::Context, truecolor: bool) -> Self {
        cells::set_recording(&ctx, true);
        super::mark_terminal(&ctx);
        // After the shell's own hook (added by `chrome::install`): it sees F1 after the aliases.
        ctx.add_plugin(TermKeys);
        Backend {
            tty,
            term: Term::new(out, truecolor),
            ctx,
            input: TermInput::new(cfg!(target_os = "macos")),
            size: (80, 24),
            kitty: None,
            pushed: false,
            held: false,
            close_pending: false,
            said: None,
            hand_offs: vec![],
            started: Instant::now(),
            offset: 0.0,
        }
    }

    /// The grid in points: the size a window opened by the switch gets (T16).
    pub fn points(&self) -> [f32; 2] {
        [
            f32::from(self.size.0) * CELL.x,
            f32::from(self.size.1) * CELL.y,
        ]
    }

    /// Takes the terminal (see the module's header): raw mode, the modes, the kitty flag where
    /// the terminal answers, the size, its unread input dropped.
    pub fn enter(&mut self) -> io::Result<()> {
        self.tty.raw(true)?;
        self.held = true;
        let kitty = match self.kitty {
            Some(kitty) => kitty,
            None => *self.kitty.insert(self.tty.kitty()),
        };
        if let Ok((cols, rows)) = self.tty.size() {
            if cols > 0 && rows > 0 {
                self.size = (cols, rows);
            }
        }
        self.term.out.write_all(ENTER.as_bytes())?;
        if kitty {
            self.term.out.write_all(KITTY_PUSH.as_bytes())?;
            self.pushed = true;
        }
        self.term.out.flush()?;
        self.tty.drop_input();
        self.term.forget();
        Ok(())
    }

    /// Gives the terminal back as it was (T19): once, every write's error ignored.
    pub fn leave(&mut self) {
        if !std::mem::take(&mut self.held) {
            return;
        }
        let mut bytes = String::new();
        if std::mem::take(&mut self.pushed) {
            bytes.push_str(KITTY_POP);
        }
        bytes.push_str(LEAVE);
        let _ = self.term.out.write_all(bytes.as_bytes());
        let _ = self.term.out.flush();
        let _ = self.tty.raw(false);
        self.term.forget();
    }

    fn now(&self) -> f64 {
        self.offset + self.started.elapsed().as_secs_f64()
    }

    /// One pass of `win` with `events`, as the shell runs a window's (and the test kit's driver
    /// in its `as_shell` mode): a close asked for in one pass is delivered with the next, the
    /// shell's part of closing runs first ([`super::closing`]); what egui asked of the platform
    /// is done — a copy goes to the terminal's clipboard.
    fn pass<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        events: Vec<egui::Event>,
    ) -> io::Result<Passed> {
        let (cols, rows) = self.size;
        let size = Vec2::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        let modifiers = self.input.modifiers();
        let mut all = vec![egui::Event::ModifiersChanged(modifiers)];
        all.extend(events);
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(self.now()),
            max_texture_side: Some(8192),
            events: all,
            focused: self.input.focused(),
            ..Default::default()
        };
        let delivered = std::mem::take(&mut self.close_pending);
        if delivered {
            (raw.viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events)
                .push(egui::ViewportEvent::Close);
        }
        chrome::set_mode_button(&self.ctx, "[GUI]");
        let mut out = self.ctx.run_ui(raw, |ui| {
            // Only the cells of the pass whose shapes come out: egui runs this again after a
            // pass it discards.
            cells::clear(ui.ctx());
            super::closing(ui.ctx(), win, false);
            win.ui(ui);
        });
        if self
            .ctx
            .data_mut(|d| d.remove_temp::<HelpAsked>(help_asked_id()))
            .is_some()
        {
            self.said = Some(F1_LINE.to_string());
        }
        // No renderer: the textures stay in the context.
        out.textures_delta.clear();
        for command in std::mem::take(&mut out.platform_output.commands) {
            if let egui::OutputCommand::CopyText(text) = command {
                self.term.copy(&text)?;
            }
        }
        let mut sent = vec![];
        let mut repaint = Duration::MAX;
        for (id, viewport) in std::mem::take(&mut out.viewport_output) {
            if id == egui::ViewportId::ROOT {
                repaint = viewport.repaint_delay;
            }
            sent.extend(viewport.commands);
        }
        let cancelled = sent.contains(&egui::ViewportCommand::CancelClose);
        self.close_pending = sent.contains(&egui::ViewportCommand::Close);
        Ok(Passed {
            closed: delivered && !cancelled,
            switch: super::switch_asked(&self.ctx) || chrome::take_mode_click(&self.ctx),
            repaint,
        })
    }

    /// The screen of the last pass: its layers laid out; a dialog egui draws, not yet in cells,
    /// a hand-off box where it stands (its keys still go to it, Esc closing it as in the
    /// window); the backend's line, or that dialog's, on the status row.
    pub fn screen(&self) -> Screen {
        let (cols, rows) = self.size;
        let layers = cells::layers(&self.ctx);
        let mut screen = compose(&self.ctx, &layers, cols, rows);
        let modal = self.ctx.memory(|m| m.top_modal_layer());
        let egui_only = modal.filter(|m| !layers.iter().any(|l| l.layer == *m));
        let stands = egui_only.and_then(|m| self.ctx.memory(|mem| mem.area_rect(m.id)));
        if let Some(r) = stands.and_then(|rect| cells_over(rect, cols, rows)) {
            hand_off(&mut screen.buf, r);
            screen.hand_offs.push(r);
            screen.caret = screen.caret.filter(|c| !r.contains(*c));
        }
        let line = self.said.as_deref().or(stands.map(|_| MODAL_LINE));
        if let Some(line) = line {
            say(&mut screen.buf, line);
        }
        screen
    }

    /// Prints the last pass's screen.
    pub fn draw(&mut self) -> io::Result<()> {
        let screen = self.screen();
        self.hand_offs = screen.hand_offs.clone();
        self.term.draw(&screen)
    }

    /// One crossterm event: egui's events of it, and whether it is a click on a hand-off box
    /// (the switch). A key or a click puts away the backend's line; a resize lays out anew.
    fn take(&mut self, ev: &CEvent) -> (Vec<egui::Event>, bool) {
        match ev {
            CEvent::Resize(cols, rows) if *cols > 0 && *rows > 0 => {
                self.size = (*cols, *rows);
                self.term.forget();
            }
            CEvent::Key(k) if k.kind != KeyEventKind::Release => self.said = None,
            CEvent::Mouse(m) if matches!(m.kind, MouseEventKind::Down(_)) => {
                self.said = None;
                let at = Position::new(m.column, m.row);
                let left = m.kind == MouseEventKind::Down(MouseButton::Left);
                if left && self.hand_offs.iter().any(|r| r.contains(at)) {
                    return (vec![], true);
                }
            }
            _ => {}
        }
        (self.input.translate(ev), false)
    }

    /// Runs `win` in the terminal until it closes or a signal comes: one pass per event and
    /// whenever egui asks for one, the screen printed once no event is waiting. `switch` is the
    /// switch to the window (F12, Alt+G, a hand-off box): its answer is a line for the status
    /// row (it did not happen) or none. The terminal is taken first and given back whatever
    /// ends the run — a panic in a pass too, which then goes on.
    pub fn run<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        switch: &mut dyn FnMut(&mut Self) -> io::Result<Option<String>>,
    ) -> io::Result<End> {
        self.enter()?;
        let ran =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.serve(win, switch)));
        self.leave();
        match ran {
            // A terminal gone with its hang-up fails a read or a write: the signal says why.
            Ok(Err(e)) => self.tty.stopped().map(End::Signal).ok_or(e),
            Ok(end) => end,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn serve<W: Window + ?Sized>(
        &mut self,
        win: &mut W,
        switch: &mut dyn FnMut(&mut Self) -> io::Result<Option<String>>,
    ) -> io::Result<End> {
        let mut due = Some(Instant::now());
        loop {
            if let Some(signal) = self.tty.stopped() {
                return Ok(End::Signal(signal));
            }
            let wait = due.map_or(TICK, |d| d.saturating_duration_since(Instant::now()));
            let (events, hand_off) = match self.tty.next(wait.min(TICK))? {
                Some(ev) => self.take(&ev),
                None if due.is_some_and(|d| Instant::now() >= d) => (vec![], false),
                None => continue,
            };
            let passed = self.pass(win, events)?;
            if passed.closed {
                return Ok(End::Closed);
            }
            if passed.switch || hand_off {
                self.said = switch(self)?;
                due = Some(Instant::now());
                continue;
            }
            if !self.tty.pending()? {
                self.draw()?;
            }
            // A close asked for is delivered with the next pass, at once (as eframe does).
            let repaint = if self.close_pending {
                Duration::ZERO
            } else {
                passed.repaint.max(IDLE_FLOOR)
            };
            due = Instant::now().checked_add(repaint);
        }
    }
}

impl<T: Tty, O: Write> Drop for Backend<T, O> {
    fn drop(&mut self) {
        self.leave();
    }
}

/// The terminal's own input hook, after the shell's ([`super::FKeyInput`], whose aliases it
/// sees): F1 — the key, Alt+1, a click on the F-key row's slot 1 — opens no help here, which is
/// egui's and not yet cells (it would take every key, unseen); the pass says so instead
/// ([`F1_LINE`]).
struct TermKeys;

/// F1 was pressed in this pass.
#[derive(Debug, Clone, Copy, Default)]
struct HelpAsked;

fn help_asked_id() -> egui::Id {
    egui::Id::unique("sopen-term-help-asked")
}

impl egui::Plugin for TermKeys {
    fn debug_name(&self) -> &'static str {
        "sopen-term-keys"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if !super::in_terminal(ctx) {
            return;
        }
        let mut asked = false;
        input.events.retain(|e| match e {
            egui::Event::Key {
                key: egui::Key::F1,
                pressed,
                repeat,
                ..
            } => {
                asked |= *pressed && !*repeat;
                false
            }
            _ => true,
        });
        if asked {
            ctx.data_mut(|d| d.insert_temp(help_asked_id(), HelpAsked));
        }
    }
}

/// The cells `rect` (points) touches on a `cols` × `rows` grid; `None` when it is off it.
fn cells_over(rect: Rect, cols: u16, rows: u16) -> Option<Cells> {
    let clamp = |v: f32, cell: f32, n: u16| (v / cell).clamp(0.0, f32::from(n));
    let (x0, y0) = (
        clamp(rect.min.x, CELL.x, cols).floor(),
        clamp(rect.min.y, CELL.y, rows).floor(),
    );
    let (x1, y1) = (
        clamp(rect.max.x, CELL.x, cols).ceil(),
        clamp(rect.max.y, CELL.y, rows).ceil(),
    );
    (x1 > x0 && y1 > y0)
        .then(|| Cells::new(x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16))
}

// ---------------------------------------------------------------------------------------------
// The switch (10 §7.1–§7.2).

/// Where the switch opens the window (10 §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Place {
    /// This machine, which has a display: in this process.
    Here,
    /// Another machine (X2's remote switch).
    Host(String),
    /// This machine, which has no display.
    NoDisplay,
}

/// The run's `--here` and `--target`: with `SOPEN_TARGET` and the config's `target`, read when
/// the owner switches, they say where the window opens (10 §7.1, 01 §4a).
#[derive(Debug, Clone, Default)]
pub struct Destination {
    pub here: bool,
    pub target: Option<String>,
}

impl Destination {
    fn place(&self) -> Result<Place> {
        let chosen = crate::forward::choose(
            self.here,
            self.target.as_deref(),
            crate::forward::target_env,
            || Ok(crate::config::load(&crate::config::path()?)?.target),
        )?;
        let hostname = crate::forward::hostname();
        Ok(match chosen {
            Some(host) if !crate::forward::is_here(&host, hostname.as_deref()) => Place::Host(host),
            _ if !super::display_available() => Place::NoDisplay,
            _ => Place::Here,
        })
    }
}

/// What moves with the browser between the terminal's context and the window's: what has the
/// keyboard, and the edit state (cursor, selection) of the field that has it — a context's own,
/// so a dialog's caret would be lost on the way.
#[derive(Clone, Default)]
pub(crate) struct Carry {
    focus: Option<egui::Id>,
    edit: Option<egui::text_edit::TextEditState>,
}

impl Carry {
    pub fn of(ctx: &egui::Context) -> Carry {
        let focus = ctx.memory(|m| m.focused());
        Carry {
            focus,
            edit: focus.and_then(|id| egui::text_edit::TextEditState::load(ctx, id)),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        if let Some(id) = self.focus {
            if let Some(edit) = self.edit.clone() {
                edit.store(ctx, id);
            }
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }
}

/// The browser in the terminal: shared with the window a switch opens.
struct Shared(Rc<RefCell<Browser>>);

impl Window for Shared {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.0.borrow_mut().ui(ui);
    }

    fn ready(&self) -> bool {
        self.0.borrow().ready()
    }

    fn close_requested(&mut self) -> bool {
        self.0.borrow_mut().close_requested()
    }

    fn keys(&self) -> &'static [FKey] {
        self.0.borrow().keys()
    }
}

/// The browser in a window the switch opened (10 §7.2): F12 there, its title row's `[TUI]`, or
/// any close — the close button, Cmd/Ctrl+Q and +W — ends the window and brings the browser back
/// to the terminal as it is, nothing asked (unsaved edits cross unsaved: one process holds
/// them).
pub(crate) struct Switched {
    browser: Rc<RefCell<Browser>>,
    carry: Rc<RefCell<Carry>>,
    first: bool,
}

impl Window for Switched {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.first) {
            self.carry.borrow().apply(&ctx);
        }
        chrome::set_mode_button(&ctx, "[TUI]");
        self.browser.borrow_mut().ui(ui);
        *self.carry.borrow_mut() = Carry::of(&ctx);
        if super::switch_asked(&ctx) || chrome::take_mode_click(&ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ready(&self) -> bool {
        self.browser.borrow().ready()
    }

    fn close_requested(&mut self) -> bool {
        true
    }

    fn keys(&self) -> &'static [FKey] {
        self.browser.borrow().keys()
    }
}

/// The switch from the terminal (10 §7.1–§7.2) to `place`: a HOST or no display is a line for
/// the status row and nothing else; this machine leaves the terminal, says [`IN_A_WINDOW`],
/// runs the browser in `window` and takes the terminal again — its unread input dropped — with
/// the browser as the window left it. A window that did not open is a line (T31).
pub(crate) fn to_window<T: Tty, O: Write>(
    b: &mut Backend<T, O>,
    browser: &Rc<RefCell<Browser>>,
    place: Result<Place>,
    window: impl FnOnce(Switched) -> Result<()>,
) -> io::Result<Option<String>> {
    match place {
        Err(e) => return Ok(Some(format!("{e:#}"))),
        Ok(Place::Host(host)) => {
            return Ok(Some(format!(
                "switching to {host} comes with the remote switch; --here switches on this \
                 machine"
            )))
        }
        Ok(Place::NoDisplay) => return Ok(Some(NO_DISPLAY.to_string())),
        Ok(Place::Here) => {}
    }
    let carry = Rc::new(RefCell::new(Carry::of(&b.ctx)));
    b.leave();
    let _ = writeln!(b.term.out, "{IN_A_WINDOW}");
    let _ = b.term.out.flush();
    let ran = window(Switched {
        browser: browser.clone(),
        carry: carry.clone(),
        first: true,
    });
    b.enter()?;
    carry.borrow().apply(&b.ctx);
    Ok(ran
        .err()
        .map(|e| match e.downcast_ref::<super::NotStarted>() {
            Some(not) => format!(
                "the window did not open: {} — check the display, or set a target",
                not.reason
            ),
            None => format!("the window ended: {e:#}"),
        }))
}

// ---------------------------------------------------------------------------------------------
// The run.

/// A panic on the terminal's thread while it is held: its message, kept by the panic hook to be
/// printed once the terminal is back (on the alternate screen it would go with it).
static HELD_BY: Mutex<Option<(std::thread::ThreadId, Option<String>)>> = Mutex::new(None);

/// The process's panic hook, once: a panic on the thread [`HELD_BY`] names is kept there;
/// every other goes to the hook that was there.
fn keep_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let before = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mut held = HELD_BY.lock().unwrap_or_else(|p| p.into_inner());
            match held.as_mut() {
                Some((id, kept)) if *id == std::thread::current().id() => {
                    *kept = Some(info.to_string());
                }
                _ => {
                    drop(held);
                    before(info);
                }
            }
        }));
    });
}

/// Shows the browser `start` opens in this terminal until it closes (10 §11 row 3), the switch
/// to `to` (§7) on F12, Alt+G or a hand-off box; then waits for what is still writing, as the
/// window's [`browser::open`] does. A signal that ended it is the error ([`Signalled`]).
pub fn open(start: browser::Start, hooks: &Hooks, to: Destination) -> Result<Outcome> {
    let mut signal = None;
    let shot = hooks.screenshot.clone();
    let outcome = browser::open_on(start, hooks, |title, browser| {
        signal = here(title, browser, &to, shot)?;
        Ok(())
    })?;
    match signal {
        Some(n) => Err(Signalled(n).into()),
        None => Ok(outcome),
    }
}

/// The browser in this terminal; the signal that ended it, if one did. `shot` (the
/// `SOPEN_SCREENSHOT` receipt): a window the switch opens is shot, then closes.
fn here(
    title: &str,
    browser: Browser,
    to: &Destination,
    shot: Option<std::path::PathBuf>,
) -> Result<Option<i32>> {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let shared = Rc::new(RefCell::new(browser));
    let truecolor = truecolor(std::env::var("COLORTERM").ok().as_deref());
    let out = io::BufWriter::with_capacity(1 << 16, io::stdout());
    let mut backend = Backend::new(Real::default(), out, ctx, truecolor);
    let mut switch = |b: &mut Backend<Real, _>| {
        let size = b.points();
        let hooks = Hooks {
            window_size: Some(size),
            screenshot: shot.clone(),
            ..Hooks::default()
        };
        to_window(b, &shared, to.place(), |switched| {
            super::run_window(title, size, switched, &hooks)
        })
    };
    keep_panics();
    *HELD_BY.lock().unwrap_or_else(|p| p.into_inner()) = Some((std::thread::current().id(), None));
    let mut tui = Shared(shared.clone());
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        backend.run(&mut tui, &mut switch)
    }));
    let kept = HELD_BY.lock().unwrap_or_else(|p| p.into_inner()).take();
    drop(backend);
    match ran {
        Ok(end) => Ok(match end? {
            End::Closed => None,
            End::Signal(n) => Some(n),
        }),
        Err(panic) => {
            if let Some((_, Some(message))) = kept {
                eprintln!("{message}");
            }
            std::panic::resume_unwind(panic)
        }
    }
}

#[cfg(test)]
mod tests;
