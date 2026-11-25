//! The terminal backend against a scripted terminal (10 §15): its bytes go into a strict model
//! of a VT screen ([`Vt`]: every sequence the backend may send, anything else a failure), its
//! input is crossterm's events as the translator receives them (the decode itself is
//! `term_input`'s tests'), and the window it switches to is the test kit's driver. No tty.
//!
//! - TS-1 (10 §14): the window's cells — what a window pass painted in cells, laid in the order it
//!   painted them — equal the terminal's, cell by cell (character, foreground, background) and
//!   the caret, for the list, the F-key row and F7's dialog with `새 폴더 2026` typed, Korean names
//!   and U+1F3B5, at 100×30; under truecolor and through the 256-colour table. TS-5: the same
//!   at 80×24 and 160×50 after a resize.
//! - Keys and the mouse through the backend: Alt+digits, Alt+G, F12, typed Hangul, a click on a
//!   row, the wheel, Ctrl+Q; OSC 52 for a copy; a hand-off box and a click on it.
//! - Leaving (TL-1): on a close, a panic in a pass, a signal — every mode off, the kitty flag
//!   popped only where it was pushed, raw mode off.
//! - The switch (TW-1, TW-5): the browser in a window and back with its dialog, its typed text
//!   and caret; the keys typed meanwhile dropped; a HOST, no display and a window that does not
//!   open are lines on the status row.

use super::*;
use crate::gui::browser::{Connect, Launch, Start};
use crate::gui::testkit::Driver;
use crate::remote::{LocalFs, RemoteFs};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use eframe::egui::epaint::ClippedShape;
use eframe::egui::{Color32, Key, Modifiers};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------------------------
// A VT screen, strictly.

/// A colour as the terminal was told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Default,
    Rgb(Color32),
    Index(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VtCell {
    /// The grapheme; empty in a wide glyph's second cell.
    symbol: String,
    fg: Ink,
    bg: Ink,
}

impl Default for VtCell {
    fn default() -> Self {
        VtCell {
            symbol: " ".into(),
            fg: Ink::Default,
            bg: Ink::Default,
        }
    }
}

/// A terminal's screen as the bytes leave it: the alternate screen's cells (kept once it is
/// left, to be read), the cursor and whether it shows, the private modes set, the kitty flags'
/// stack, what OSC 52 copied, and what was printed on the main screen. A sequence it does not
/// know fails the test: the backend sends nothing else.
#[derive(Debug, Clone)]
struct Vt {
    cols: u16,
    rows: u16,
    cells: Vec<VtCell>,
    x: u16,
    y: u16,
    fg: Ink,
    bg: Ink,
    cursor_shown: bool,
    alt: bool,
    modes: std::collections::BTreeSet<u16>,
    kitty: Vec<u16>,
    copied: Vec<String>,
    main: String,
}

impl Vt {
    fn new(cols: u16, rows: u16) -> Vt {
        Vt {
            cols,
            rows,
            cells: vec![VtCell::default(); usize::from(cols) * usize::from(rows)],
            x: 0,
            y: 0,
            fg: Ink::Default,
            bg: Ink::Default,
            cursor_shown: true,
            alt: false,
            modes: Default::default(),
            kitty: vec![],
            copied: vec![],
            main: String::new(),
        }
    }

    /// The screen is `cols` × `rows` from now on (a resize), cleared.
    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.clear();
    }

    fn clear(&mut self) {
        self.cells = vec![VtCell::default(); usize::from(self.cols) * usize::from(self.rows)];
    }

    fn cell(&self, x: u16, y: u16) -> &VtCell {
        &self.cells[usize::from(y) * usize::from(self.cols) + usize::from(x)]
    }

    /// The cursor's cell while it shows.
    fn caret(&self) -> Option<(u16, u16)> {
        self.cursor_shown.then_some((self.x, self.y))
    }

    /// Cells `from..to` of row `y` as text (a wide glyph's second cell adds nothing).
    fn part(&self, y: u16, from: u16, to: u16) -> String {
        (from..to)
            .map(|x| self.cell(x, y).symbol.as_str())
            .collect()
    }

    fn row(&self, y: u16) -> String {
        self.part(y, 0, self.cols)
    }

    fn text(&self) -> String {
        (0..self.rows)
            .map(|y| self.row(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn feed(&mut self, bytes: &[u8]) {
        let text = std::str::from_utf8(bytes).expect("the backend writes UTF-8");
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("\x1b[") {
                let end = after
                    .find(|c: char| ('\x40'..='\x7e').contains(&c))
                    .unwrap_or_else(|| panic!("an unended CSI: {after:?}"));
                self.csi(&after[..end], after.as_bytes()[end] as char);
                rest = &after[end + 1..];
            } else if let Some(after) = rest.strip_prefix("\x1b]") {
                let end = after.find('\x07').expect("an OSC ended by BEL");
                self.osc(&after[..end]);
                rest = &after[end + 1..];
            } else if rest.starts_with('\x1b') {
                let shown: String = rest.chars().take(8).collect();
                panic!("an escape the backend does not send: {shown:?}");
            } else {
                let end = rest.find('\x1b').unwrap_or(rest.len());
                self.print(&rest[..end]);
                rest = &rest[end..];
            }
        }
    }

    fn csi(&mut self, params: &str, last: char) {
        let numbers = |s: &str| -> Vec<u16> {
            s.split(';')
                .map(|n| n.parse().unwrap_or_else(|_| panic!("CSI {params:?}{last}")))
                .collect()
        };
        match (params.chars().next(), last) {
            (Some('?'), 'h' | 'l') => {
                for mode in numbers(&params[1..]) {
                    let on = last == 'h';
                    if on {
                        self.modes.insert(mode);
                    } else {
                        self.modes.remove(&mode);
                    }
                    match mode {
                        25 => self.cursor_shown = on,
                        1049 => {
                            self.alt = on;
                            if on {
                                self.clear();
                            }
                        }
                        1000 | 1002 | 1003 | 1004 | 1006 | 1015 | 2004 => {}
                        other => panic!("a private mode the backend does not use: {other}"),
                    }
                }
                self.modes.remove(&25);
            }
            (Some('>'), 'u') => self.kitty.push(numbers(&params[1..])[0]),
            (Some('<'), 'u') => {
                for _ in 0..numbers(&params[1..])[0] {
                    self.kitty.pop();
                }
            }
            (_, 'J') if params == "2" => self.clear(),
            (_, 'H') => {
                let n = numbers(params);
                assert!(n[0] >= 1 && n[0] <= self.rows && n[1] >= 1 && n[1] <= self.cols);
                (self.y, self.x) = (n[0] - 1, n[1] - 1);
            }
            (_, 'm') => self.sgr(&numbers(params)),
            _ => panic!("a CSI the backend does not send: {params:?}{last}"),
        }
    }

    fn sgr(&mut self, n: &[u16]) {
        let mut i = 0;
        while i < n.len() {
            let ground = n[i];
            let ink = match (ground, n.get(i + 1)) {
                (0, _) => {
                    (self.fg, self.bg) = (Ink::Default, Ink::Default);
                    i += 1;
                    continue;
                }
                (39 | 49, _) => {
                    i += 1;
                    Ink::Default
                }
                (38 | 48, Some(2)) => {
                    let c = |k: usize| n[i + k] as u8;
                    let ink = Ink::Rgb(Color32::from_rgb(c(2), c(3), c(4)));
                    i += 5;
                    ink
                }
                (38 | 48, Some(5)) => {
                    let ink = Ink::Index(n[i + 2] as u8);
                    i += 3;
                    ink
                }
                _ => panic!("an SGR the backend does not send: {n:?}"),
            };
            if matches!(ground, 38 | 39) {
                self.fg = ink;
            } else {
                self.bg = ink;
            }
        }
    }

    fn osc(&mut self, body: &str) {
        let encoded = body
            .strip_prefix("52;c;")
            .unwrap_or_else(|| panic!("an OSC the backend does not send: {body:?}"));
        self.copied
            .push(String::from_utf8(unbase64(encoded)).unwrap());
    }

    fn print(&mut self, text: &str) {
        if !self.alt {
            self.main.push_str(text);
            return;
        }
        for g in text.graphemes(true) {
            assert!(
                !g.chars().any(char::is_control),
                "a control character on the alternate screen: {g:?}"
            );
            let w = g.width() as u16;
            assert!(
                (1..=2).contains(&w) && self.x + w <= self.cols,
                "{g:?} ({w} cells) at {},{}",
                self.x,
                self.y
            );
            let i = usize::from(self.y) * usize::from(self.cols) + usize::from(self.x);
            self.cells[i] = VtCell {
                symbol: g.to_string(),
                fg: self.fg,
                bg: self.bg,
            };
            if w == 2 {
                self.cells[i + 1] = VtCell {
                    symbol: String::new(),
                    fg: self.fg,
                    bg: self.bg,
                };
            }
            // At the right edge the cursor stays on the last cell (the backend moves it before
            // the next row).
            self.x = (self.x + w).min(self.cols - 1);
        }
    }
}

fn unbase64(s: &str) -> Vec<u8> {
    const ABC: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = vec![];
    for c in s.chars().filter(|&c| c != '=') {
        bits = (bits << 6) | ABC.find(c).expect("base64") as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The window's cells, as it painted them.

/// One cell as the window painted it: the grapheme ("" in a wide glyph's second cell) and its
/// colours; whether a shape painted it at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Painted {
    symbol: String,
    fg: Color32,
    bg: Color32,
    covered: bool,
}

/// The window's cells and its caret's cell.
type WindowCells = (Vec<Painted>, Option<(u16, u16)>);

/// A pass's clock for the tests' own passes, later each time (and later than the driver's).
fn later() -> f64 {
    thread_local!(static NOW: std::cell::Cell<f64> = const { std::cell::Cell::new(10_000.0) });
    NOW.with(|n| {
        n.set(n.get() + 10.0);
        n.get()
    })
}

/// What one idle window pass of `win` in `ctx` painted, `cols` × `rows` cells: its shapes.
fn window_pass(
    ctx: &egui::Context,
    win: &mut impl super::Window,
    cols: u16,
    rows: u16,
) -> Vec<ClippedShape> {
    let size = Vec2::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(later()),
        max_texture_side: Some(8192),
        focused: true,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        win.ui(ui);
    });
    out.textures_delta.clear();
    out.shapes
}

/// The window's shapes laid back onto cells (10 §5: what it painted, not the buffers it was
/// given), in painting order: an opaque filled rectangle is a background — on the cells whose
/// centres it covers inside its clip, their glyphs gone — except the caret's two scan lines at a
/// cell's foot; a text is a glyph at the cell its corner is on, a wide one taking the next
/// cell too. A text off the grid fails: a terminal would not show it.
fn window_cells(shapes: &[ClippedShape], cols: u16, rows: u16) -> WindowCells {
    let p = chrome::PALETTE;
    let body = Painted {
        symbol: " ".into(),
        fg: p.text,
        bg: p.back,
        covered: false,
    };
    let mut out = vec![body; usize::from(cols) * usize::from(rows)];
    let mut caret = None;
    let at = |x: f32, y: f32| -> Option<usize> {
        let (x, y) = ((x / CELL.x).floor(), (y / CELL.y).floor());
        let inside = x >= 0.0 && y >= 0.0 && x < f32::from(cols) && y < f32::from(rows);
        inside.then(|| y as usize * usize::from(cols) + x as usize)
    };
    fn flat<'a>(shape: &'a egui::Shape, all: &mut Vec<&'a egui::Shape>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| flat(s, all)),
            other => all.push(other),
        }
    }
    for c in shapes {
        let mut all = vec![];
        flat(&c.shape, &mut all);
        for shape in all {
            match shape {
                egui::Shape::Rect(r) if r.fill.a() == 255 => {
                    let rect = r.rect;
                    if rect.width() == CELL.x && rect.height() == 2.0 {
                        caret = at(rect.center().x, rect.top() - 1.0).map(|i| {
                            (
                                (i % usize::from(cols)) as u16,
                                (i / usize::from(cols)) as u16,
                            )
                        });
                        continue;
                    }
                    let shown = rect.intersect(c.clip_rect);
                    for y in 0..rows {
                        for x in 0..cols {
                            let centre = Pos2::new(
                                (f32::from(x) + 0.5) * CELL.x,
                                (f32::from(y) + 0.5) * CELL.y,
                            );
                            if shown.contains(centre) {
                                let cell =
                                    &mut out[usize::from(y) * usize::from(cols) + usize::from(x)];
                                cell.symbol = " ".into();
                                cell.bg = r.fill;
                                cell.covered = true;
                            }
                        }
                    }
                }
                egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => {
                    let text = t.galley.text();
                    assert!(
                        t.pos.x % CELL.x == 0.0 && t.pos.y % CELL.y == 0.0,
                        "a text off the grid at {:?}: {text:?}",
                        t.pos
                    );
                    let centre = t.pos + CELL / 2.0;
                    let Some(i) = at(centre.x, centre.y).filter(|_| c.clip_rect.contains(centre))
                    else {
                        continue;
                    };
                    let fg = (t.galley.job.sections.first())
                        .map_or(t.fallback_color, |s| s.format.color);
                    let fg = t.override_text_color.unwrap_or(fg);
                    out[i].symbol = text.to_string();
                    out[i].fg = fg;
                    out[i].covered = true;
                    if text.width() == 2 && (i + 1) % usize::from(cols) != 0 {
                        out[i + 1].symbol = String::new();
                        out[i + 1].fg = fg;
                    }
                }
                _ => {}
            }
        }
    }
    (out, caret)
}

/// Every cell of `vt` against the window's, through `ink` (the colour the window's colour is
/// sent as): character, background, and the foreground where a glyph is painted (a blank shows
/// none, 10 §5's measure); then the caret. The differences, at most 20.
fn differences(window: &WindowCells, vt: &Vt, ink: impl Fn(Color32) -> Ink) -> Vec<String> {
    let mut out = vec![];
    for y in 0..vt.rows {
        for x in 0..vt.cols {
            let w = &window.0[usize::from(y) * usize::from(vt.cols) + usize::from(x)];
            let t = vt.cell(x, y);
            let blank = !w.symbol.is_empty() && w.symbol.trim().is_empty();
            let want = VtCell {
                symbol: w.symbol.clone(),
                fg: if blank { t.fg } else { ink(w.fg) },
                bg: ink(w.bg),
            };
            if *t != want && out.len() < 20 {
                out.push(format!("({x},{y}): terminal {t:?}, window {want:?}"));
            }
        }
    }
    if vt.caret() != window.1 {
        out.push(format!(
            "caret: terminal {:?}, window {:?}",
            vt.caret(),
            window.1
        ));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// A scripted terminal.

#[derive(Default)]
struct Script {
    size: (u16, u16),
    kitty: bool,
    /// Raw mode now, and every switch of it.
    raw: bool,
    raws: Vec<bool>,
    drops: usize,
    /// Typed into the terminal and not read yet: what `drop_input` drops.
    unread: VecDeque<CEvent>,
    /// The test's events, each given once `gate` says the browser has settled.
    events: VecDeque<CEvent>,
    gate: Option<Box<dyn Fn() -> bool>>,
    /// The signal that comes once the events are all given.
    stop: Option<i32>,
    stopped: bool,
    /// Waits for input since the last event, to fail a script that never ends.
    idle: usize,
}

#[derive(Clone, Default)]
struct Tape(Rc<RefCell<Script>>);

impl Tty for Tape {
    fn raw(&mut self, on: bool) -> io::Result<()> {
        let mut s = self.0.borrow_mut();
        s.raw = on;
        s.raws.push(on);
        Ok(())
    }

    fn size(&mut self) -> io::Result<(u16, u16)> {
        Ok(self.0.borrow().size)
    }

    fn kitty(&mut self) -> bool {
        self.0.borrow().kitty
    }

    fn drop_input(&mut self) {
        let mut s = self.0.borrow_mut();
        s.drops += 1;
        s.unread.clear();
    }

    fn next(&mut self, _wait: Duration) -> io::Result<Option<CEvent>> {
        let open = self.0.borrow().gate.as_ref().is_none_or(|g| g());
        let mut s = self.0.borrow_mut();
        if let Some(ev) = s.unread.pop_front() {
            return Ok(Some(ev));
        }
        if open {
            if let Some(ev) = s.events.pop_front() {
                s.idle = 0;
                return Ok(Some(ev));
            }
            if s.stop.is_some() {
                s.stopped = true;
            }
        }
        s.idle += 1;
        assert!(s.idle < 20_000, "the script ran out without an end");
        drop(s);
        std::thread::sleep(Duration::from_millis(1));
        Ok(None)
    }

    fn pending(&mut self) -> io::Result<bool> {
        Ok(!self.0.borrow().unread.is_empty())
    }

    fn stopped(&self) -> Option<i32> {
        let s = self.0.borrow();
        s.stop.filter(|_| s.stopped)
    }
}

/// The bytes the backend wrote.
#[derive(Clone, Default)]
struct Bytes(Rc<RefCell<Vec<u8>>>);

impl Write for Bytes {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Bytes {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }

    fn vt(&self, cols: u16, rows: u16) -> Vt {
        let mut vt = Vt::new(cols, rows);
        vt.feed(&self.0.borrow());
        vt
    }

    /// The screen since the terminal was last entered.
    fn last_screen(&self, cols: u16, rows: u16) -> Vt {
        let text = self.text();
        let mut vt = Vt::new(cols, rows);
        vt.feed(&text.as_bytes()[text.rfind(ENTER).expect("entered")..]);
        vt
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> CEvent {
    CEvent::Key(KeyEvent::new(code, modifiers))
}

fn plain(code: KeyCode) -> CEvent {
    key(code, KeyModifiers::NONE)
}

fn typed(text: &str) -> Vec<CEvent> {
    text.chars().map(|c| plain(KeyCode::Char(c))).collect()
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> CEvent {
    CEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn click(column: u16, row: u16) -> [CEvent; 2] {
    [
        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        mouse(MouseEventKind::Up(MouseButton::Left), column, row),
    ]
}

fn ctrl_q() -> CEvent {
    key(KeyCode::Char('q'), KeyModifiers::CONTROL)
}

// ---------------------------------------------------------------------------------------------
// The browser.

struct Fx {
    _tmp: tempfile::TempDir,
    root: String,
    b: Rc<RefCell<Browser>>,
}

/// The browser on a folder of three folders (one Korean), and files with a Korean name, an
/// emoji (U+1F3B5) and a plain one; `more` files besides (a list that scrolls).
fn fixture(more: usize) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    for dir in ["alpha", "beta", "한글 폴더"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in ["한글 문서 초안.txt", "\u{1F3B5} song.mp3", "readme.md"] {
        std::fs::write(root.join(file), "x\n").unwrap();
    }
    for n in 0..more {
        std::fs::write(root.join(format!("more{n:03}.txt")), "x\n").unwrap();
    }
    let root = root.to_str().unwrap().to_string();
    let connect: Connect = Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
    let launch: Launch = Arc::new(|abs: &str, _| Ok(format!("opened {abs}")));
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect,
        label: None,
        destination: None,
        abs: root.clone(),
        cache: tmp.path().join("cache"),
        state_dir: tmp.path().join("state"),
        launch,
        git: None,
    });
    Fx {
        _tmp: tmp,
        root,
        b: Rc::new(RefCell::new(b)),
    }
}

/// A context in the skin, as `here` makes it, the title row's clock pinned as the test kit
/// pins it.
fn skin() -> egui::Context {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    chrome::set_test_now(Some(crate::gui::testkit::START_OF_A_MINUTE));
    ctx
}

/// A backend on a scripted `cols` × `rows` terminal (`kitty`: it answers the keyboard query)
/// whose events wait for `fx`'s browser to settle.
fn backend(fx: &Fx, cols: u16, rows: u16, kitty: bool) -> (Backend<Tape, Bytes>, Tape, Bytes) {
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (cols, rows);
        s.kitty = kitty;
        let b = fx.b.clone();
        s.gate = Some(Box::new(move || b.borrow().ready()));
    }
    let bytes = Bytes::default();
    let b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
    (b, tape, bytes)
}

/// Runs the browser in `b` with `events` played, one each time it has settled.
fn play(b: &mut Backend<Tape, Bytes>, tape: &Tape, fx: &Fx, events: Vec<CEvent>) -> End {
    tape.0.borrow_mut().events.extend(events);
    let mut win = Shared(fx.b.clone());
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        panic!("no switch in this test")
    };
    b.run(&mut win, &mut no_switch).unwrap()
}

/// The browser in a window (the test kit's driver, `cols` × `rows` cells) until it has settled.
fn window(fx: &Fx, cols: u16, rows: u16) -> (Driver, Shared) {
    let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
    let mut win = Shared(fx.b.clone());
    drv.until(&mut win, 20, |w| w.ready());
    (drv, win)
}

// ---------------------------------------------------------------------------------------------
// TS-1, TS-5.

/// The terminal's screen of one idle pass on the window's own context (the same focus and edit
/// state: the caret is comparable), `cols` × `rows`, truecolor or not.
fn terminal_of(ctx: &egui::Context, win: &mut Shared, cols: u16, rows: u16, rgb: bool) -> Vt {
    let tape = Tape::default();
    tape.0.borrow_mut().size = (cols, rows);
    let bytes = Bytes::default();
    let mut b = Backend::new(tape, bytes.clone(), ctx.clone(), rgb);
    b.offset = later();
    b.enter().unwrap();
    b.pass(win, vec![]).unwrap();
    b.draw().unwrap();
    bytes.vt(cols, rows)
}

/// The browser as a window a switch opened shows it (its title row's `[TUI]`).
fn switched(fx: &Fx) -> Switched {
    Switched {
        browser: fx.b.clone(),
        carry: Rc::default(),
        first: false,
    }
}

/// TS-1's one difference by design (10 §5, *tui* §2): the mode button's label, `[GUI]` in the
/// terminal and `[TUI]` in the window — one cell, nothing else.
fn only_the_mode_button(d: &[String], vt: &Vt, what: &str) {
    let x = (0..vt.cols.saturating_sub(5)).find(|&x| vt.part(0, x, x + 5) == "[GUI]");
    let x = x.unwrap_or_else(|| panic!("{what}: no [GUI] on the title row: {}", vt.row(0)));
    let cell = format!("({},0): ", x + 1);
    assert!(
        d.len() == 1 && d[0].starts_with(&cell),
        "{what}: the mode button's cell {cell}only:\n{}",
        d.join("\n")
    );
}

#[test]
fn ts1_the_terminal_shows_the_windows_cells_under_truecolor_and_256_colours() {
    let fx = fixture(0);
    let (mut drv, mut win) = window(&fx, 100, 30);
    // Down Down F7 and `새 폴더 2026` typed, as 10 §5's measurement.
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    drv.key(&mut win, Key::F7, Modifiers::NONE);
    drv.idle(&mut win, 2);
    let typing = egui::Event::Text("새 폴더 2026".into());
    drv.frame(&mut win, vec![typing], Modifiers::NONE);
    drv.idle(&mut win, 2);
    let mut sw = switched(&fx);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, 30), 100, 30);
    assert!(painted.0.iter().all(|c| c.covered), "every cell is painted");
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, true);
    let text = vt.text();
    for shown in [
        "Make folder",
        "새 폴더 2026",
        "한글 폴더",
        "\u{1F3B5} song",
        "7MkDir",
    ] {
        assert!(text.contains(shown), "{shown:?} on screen:\n{text}");
    }
    assert!(vt.caret().is_some(), "the field's caret is the cursor");
    let d = differences(&painted, &vt, Ink::Rgb);
    only_the_mode_button(&d, &vt, "truecolor");
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, false);
    let d = differences(&painted, &vt, |c| Ink::Index(cells::index256(c)));
    only_the_mode_button(&d, &vt, "256 colours");
    // The negative control: one cell of the window's changed is one difference more.
    let mut changed = painted.clone();
    changed.0[0].symbol = "X".into();
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, true);
    assert_eq!(differences(&changed, &vt, Ink::Rgb).len(), 2);
}

/// Skips the tmux test with `why` — or, under `SOPEN_REQUIRE_TMUX=1` (as the other
/// `SOPEN_REQUIRE_*`), fails it.
fn tmux_skip(why: &str) -> Option<Vt> {
    assert!(
        std::env::var("SOPEN_REQUIRE_TMUX").as_deref() != Ok("1"),
        "SOPEN_REQUIRE_TMUX=1: {why}"
    );
    eprintln!("skipped: {why}");
    None
}

/// The screen a private tmux server shows after `bytes` (`capture-pane -e`, its SGR read here),
/// `cols` × `rows`, and its cursor while it shows (10 §15's second emulator); `None` — skipped —
/// without tmux, or with one whose output this reader does not know.
fn tmux_screen(bytes: &[u8], cols: u16, rows: u16) -> Option<Vt> {
    let found = std::process::Command::new("tmux").arg("-V").output();
    if !found.is_ok_and(|o| o.status.success()) {
        return tmux_skip("no tmux on PATH");
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("screen.bin");
    std::fs::write(&file, bytes).unwrap();
    let server = format!("sopen-term-test-{}", std::process::id());
    let tmux = |args: &[&str]| {
        let out = std::process::Command::new("tmux")
            .args(["-L", &server, "-f", "/dev/null"])
            .args(args)
            .env("TERM", "xterm-256color")
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let shell = format!("cat '{}'; sleep 60", file.display());
    let (w, h) = (cols.to_string(), rows.to_string());
    tmux(&["new-session", "-d", "-x", &w, "-y", &h, "-s", "t", &shell]);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !tmux(&["capture-pane", "-t", "t", "-p"]).contains("1Help") {
        std::thread::sleep(Duration::from_millis(50));
    }
    let screen = tmux(&["capture-pane", "-t", "t", "-p", "-e", "-N"]);
    let format = "#{cursor_x},#{cursor_y},#{cursor_flag}";
    let cursor = tmux(&["display", "-p", "-t", "t", format]);
    tmux(&["kill-server"]);
    // The reader is strict (any sequence it does not know panics): a tmux that writes another
    // one is a skip, not a failure, unless tmux is required.
    let read = std::panic::catch_unwind(|| {
        let mut vt = Vt::new(cols, rows);
        vt.alt = true;
        for (y, line) in screen.lines().take(usize::from(rows)).enumerate() {
            vt.feed(format!("\x1b[{};1H{line}", y + 1).as_bytes());
        }
        let n: Vec<u16> = (cursor.trim().split(','))
            .map(|n| n.parse().unwrap())
            .collect();
        (vt.x, vt.y, vt.cursor_shown) = (n[0], n[1], n[2] == 1);
        vt
    });
    match read {
        Ok(vt) => Some(vt),
        Err(_) => tmux_skip("this tmux's capture-pane -e has sequences the reader does not know"),
    }
}

/// TS-1 (b): the same bytes in a private tmux server — an emulator not written here — show the
/// window's cells: character, colours, the caret.
#[test]
fn ts1_tmux_shows_the_windows_cells_from_the_backends_bytes() {
    let fx = fixture(0);
    let (mut drv, mut win) = window(&fx, 100, 30);
    drv.key(&mut win, Key::F7, Modifiers::NONE);
    drv.idle(&mut win, 2);
    let typing = egui::Event::Text("새 폴더 2026".into());
    drv.frame(&mut win, vec![typing], Modifiers::NONE);
    drv.idle(&mut win, 2);
    let mut sw = switched(&fx);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, 30), 100, 30);
    let tape = Tape::default();
    tape.0.borrow_mut().size = (100, 30);
    let bytes = Bytes::default();
    let mut b = Backend::new(tape, bytes.clone(), drv.ctx.clone(), true);
    b.offset = later();
    b.enter().unwrap();
    b.pass(&mut win, vec![]).unwrap();
    b.draw().unwrap();
    let Some(vt) = tmux_screen(&bytes.0.borrow(), 100, 30) else {
        return;
    };
    assert!(vt.text().contains("새 폴더 2026"), "{}", vt.text());
    let d = differences(&painted, &vt, Ink::Rgb);
    only_the_mode_button(&d, &vt, "tmux");
}

#[test]
fn ts5_after_a_resize_the_terminal_shows_the_windows_cells_at_80x24_and_160x50() {
    let fx = fixture(0);
    let (mut drv, mut win) = window(&fx, 100, 30);
    for (cols, rows) in [(80, 24), (160, 50)] {
        drv.resize(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        drv.until(&mut win, 20, |w| w.ready());
        drv.idle(&mut win, 2);
        let mut sw = switched(&fx);
        let painted = window_cells(&window_pass(&drv.ctx, &mut sw, cols, rows), cols, rows);
        let vt = terminal_of(&drv.ctx, &mut win, cols, rows, true);
        let d = differences(&painted, &vt, Ink::Rgb);
        only_the_mode_button(&d, &vt, &format!("{cols}x{rows}"));
        assert!(
            vt.row(rows - 1).starts_with("1Help"),
            "the F-key row at the foot"
        );
    }
}

/// A resize through the loop: the browser is laid out again at the new size and the whole
/// screen printed — the right side's frame ends on the new last column.
#[test]
fn a_resize_reflows_the_screen() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let end = play(&mut b, &tape, &fx, vec![CEvent::Resize(80, 24), ctrl_q()]);
    assert_eq!(end, End::Closed);
    let text = bytes.text();
    let resized = text
        .rfind("\x1b[2J")
        .expect("printed whole after the resize");
    let mut vt = Vt::new(100, 30);
    vt.feed(&text.as_bytes()[..resized]);
    assert_eq!(vt.cell(99, 1).symbol, "┐", "at 100 columns first");
    vt.resize(80, 24);
    vt.feed(&text.as_bytes()[resized..]);
    assert!(vt.row(23).starts_with("1Help"), "{}", vt.text());
    assert_eq!(vt.cell(79, 1).symbol, "┐", "at 80 columns:\n{}", vt.text());
}

/// A screen printed whole (entered, resized) hides the cursor first: one that showed at a caret
/// the new screen does not have is not left on it.
#[test]
fn a_screen_printed_whole_hides_a_cursor_it_has_no_caret_for() {
    let p = chrome::PALETTE;
    let area = Cells::new(0, 0, 4, 2);
    let mut buf = Buffer::empty(area);
    cells::fill(&mut buf, area, Pen::new(p.text, p.back));
    let bytes = Bytes::default();
    let mut term = Term::new(bytes.clone(), true);
    let with = Screen {
        buf: buf.clone(),
        caret: Some(Position::new(1, 1)),
        hand_offs: vec![],
    };
    term.draw(&with).unwrap();
    let mut vt = Vt::new(4, 2);
    vt.alt = true;
    vt.feed(&bytes.0.borrow());
    assert_eq!(vt.caret(), Some((1, 1)));
    term.forget();
    let without = Screen {
        buf,
        caret: None,
        hand_offs: vec![],
    };
    term.draw(&without).unwrap();
    let mut vt = Vt::new(4, 2);
    vt.alt = true;
    vt.feed(&bytes.0.borrow());
    assert_eq!(
        vt.caret(),
        None,
        "hidden after the screen was printed whole"
    );
}

// ---------------------------------------------------------------------------------------------
// Keys and the mouse.

#[test]
fn keys_and_the_mouse_reach_the_browser_through_the_backend() {
    let fx = fixture(60);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, true);
    let mut events = vec![plain(KeyCode::Down), plain(KeyCode::Down)];
    // Alt+7 is F7 in a terminal (T7): the make-folder dialog; a Hangul name typed into it.
    events.push(key(KeyCode::Char('7'), KeyModifiers::ALT));
    events.extend(typed("새폴더"));
    events.push(plain(KeyCode::Enter));
    // A click on the left side's row of `beta` (title 0, frame 1, heads 2, `..` 3, alpha 4).
    events.extend(click(5, 5));
    // The wheel down a notch over the right side, which does not follow a bar: three rows.
    events.push(mouse(MouseEventKind::ScrollDown, 70, 10));
    events.push(ctrl_q());
    let end = play(&mut b, &tape, &fx, events);
    assert_eq!(end, End::Closed, "Ctrl+Q closes it");
    assert!(
        std::path::Path::new(&fx.root).join("새폴더").is_dir(),
        "Alt+7, typed Hangul and Enter made the folder"
    );
    let vt = bytes.vt(100, 30);
    let screen = vt.text();
    let bar = Ink::Rgb(chrome::PALETTE.bar);
    assert_eq!(
        vt.cell(3, 5).bg,
        bar,
        "the bar on the clicked row:\n{screen}"
    );
    assert!(vt.part(5, 0, 50).starts_with("║beta"), "{screen}");
    // `..`, alpha, beta scrolled away: the new folder on top.
    assert!(vt.part(3, 50, 100).starts_with("│새폴더"), "{screen}");
    // The kitty flag was pushed (the terminal answered) and popped.
    assert!(bytes.text().contains(KITTY_PUSH));
    assert!(vt.kitty.is_empty());
}

#[test]
fn f12_and_alt_g_ask_for_the_switch_and_a_copy_goes_out_as_osc_52() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let mut events = vec![plain(KeyCode::F(7))];
    events.extend(typed("abc"));
    // Ctrl+A selects the field's text, Ctrl+C copies it (Ctrl is egui's command, T26).
    events.push(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    events.push(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    events.push(plain(KeyCode::F(12)));
    events.push(key(KeyCode::Char('g'), KeyModifiers::ALT));
    events.push(plain(KeyCode::Esc));
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let asked = Rc::new(RefCell::new(0));
    let count = asked.clone();
    let mut switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        *count.borrow_mut() += 1;
        Ok(Some("not now".into()))
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut switch).unwrap(), End::Closed);
    assert_eq!(*asked.borrow(), 2, "F12 and Alt+G, each once");
    let vt = bytes.vt(100, 30);
    assert_eq!(vt.copied, ["abc"], "OSC 52");
    assert!(!vt.text().contains("abcg"), "Alt+G typed nothing");
}

// ---------------------------------------------------------------------------------------------
// Hand-off boxes.

#[test]
fn a_pane_the_window_draws_in_egui_is_a_hand_off_box_and_a_click_on_it_switches() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    // The bar on `readme.md` (`..`, three folders, then it): the other side previews it.
    let mut events: Vec<CEvent> = (0..4).map(|_| plain(KeyCode::Down)).collect();
    events.extend(click(75, 15));
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let asked = Rc::new(RefCell::new(0));
    let count = asked.clone();
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        *count.borrow_mut() += 1;
        assert!(!b.hand_offs.is_empty());
        Ok(None)
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut switch).unwrap(), End::Closed);
    assert_eq!(*asked.borrow(), 1, "the click on the box is the switch");
    let vt = bytes.vt(100, 30);
    let text = vt.text();
    let at = (0..vt.rows).find(|&y| vt.part(y, 50, 100).contains(HAND_OFF));
    assert!(at.is_some(), "a hand-off box on the right side:\n{text}");
    assert_eq!(
        vt.cell(50, 1).symbol,
        "┌",
        "its frame where the side starts:\n{text}"
    );
}

// ---------------------------------------------------------------------------------------------
// Leaving (TL-1).

/// The bytes leave the terminal as they found it: every mode the entering set is off, the kitty
/// stack as it was, the cursor shown, the main screen back — and raw mode off.
fn left_as_found(bytes: &Bytes, tape: &Tape) {
    let vt = bytes.vt(100, 30);
    assert!(vt.modes.is_empty(), "modes left on: {:?}", vt.modes);
    assert!(vt.kitty.is_empty(), "kitty flags left: {:?}", vt.kitty);
    assert!(vt.cursor_shown && !vt.alt);
    assert!(bytes.text().ends_with(LEAVE), "the last bytes leave");
    let s = tape.0.borrow();
    assert!(!s.raw, "raw mode off");
    assert_eq!(s.raws.first(), Some(&true));
}

#[test]
fn leaving_on_a_close_restores_the_terminal_with_and_without_the_kitty_flag() {
    for kitty in [false, true] {
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, kitty);
        assert_eq!(play(&mut b, &tape, &fx, vec![ctrl_q()]), End::Closed);
        left_as_found(&bytes, &tape);
        assert_eq!(
            bytes.text().contains(KITTY_POP),
            kitty,
            "popped only if pushed"
        );
        // Leaving again (the backend dropped) writes nothing more.
        let len = bytes.text().len();
        drop(b);
        assert_eq!(bytes.text().len(), len);
    }
}

#[test]
fn a_signal_ends_the_run_and_restores_the_terminal() {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, true);
        tape.0.borrow_mut().stop = Some(signal);
        assert_eq!(play(&mut b, &tape, &fx, vec![]), End::Signal(signal));
        left_as_found(&bytes, &tape);
        assert_eq!(Signalled(signal).exit_code(), 128 + signal as u8);
    }
}

/// A window whose pass panics when P is pressed (a test hook in the loop).
struct Bomb;

impl super::Window for Bomb {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if ui.input(|i| i.key_pressed(Key::P)) {
            panic!("a test hook's panic in the loop");
        }
    }
    fn ready(&self) -> bool {
        true
    }
}

#[test]
fn a_panic_in_a_pass_restores_the_terminal_then_goes_on() {
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, 30);
        s.kitty = true;
        s.events.push_back(plain(KeyCode::Char('p')));
    }
    let bytes = Bytes::default();
    let mut b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        b.run(&mut Bomb, &mut no_switch)
    }));
    let panic = ran.expect_err("the panic goes on");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"a test hook's panic in the loop")
    );
    left_as_found(&bytes, &tape);
    assert!(bytes.text().contains(KITTY_POP));
}

/// The process's hook keeps a panic on the terminal's thread — printed by `here` once the
/// terminal is back, not on the alternate screen that goes with it.
#[test]
fn a_panic_on_the_terminals_thread_is_kept_for_after_the_terminal_is_back() {
    keep_panics();
    *HELD_BY.lock().unwrap() = Some((std::thread::current().id(), None));
    let _ = std::panic::catch_unwind(|| panic!("kept until the terminal is back"));
    let kept = HELD_BY.lock().unwrap().take().and_then(|(_, kept)| kept);
    assert!(
        kept.as_deref()
            .is_some_and(|m| m.contains("kept until the terminal is back")),
        "{kept:?}"
    );
}

#[test]
fn base64_is_rfc_4648() {
    for (plain, coded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foobar", "Zm9vYmFy"),
        ("새", "7IOI"),
    ] {
        assert_eq!(base64(plain.as_bytes()), coded);
        assert_eq!(unbase64(coded), plain.as_bytes());
    }
}

#[test]
fn colorterm_says_truecolor() {
    assert!(truecolor(Some("truecolor")) && truecolor(Some("24bit")));
    assert!(!truecolor(None) && !truecolor(Some("")) && !truecolor(Some("yes")));
}

// ---------------------------------------------------------------------------------------------
// Compose.

/// A layer egui paints above another wins whatever order the calls came in: a buffer on a
/// foreground layer painted first, one on the background after it.
#[test]
fn layers_are_laid_in_egui_s_order_not_the_calls() {
    let ctx = skin();
    cells::set_recording(&ctx, true);
    let p = chrome::PALETTE;
    let mut top = Buffer::empty(Cells::new(0, 0, 3, 1));
    cells::put(&mut top, 0, 0, "TOP", 3, Pen::new(p.text, p.back));
    let mut low = Buffer::empty(Cells::new(0, 0, 3, 1));
    cells::put(&mut low, 0, 0, "low", 3, Pen::new(p.text, p.back));
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(80.0, 32.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        let over = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("over"));
        cells::paint(&ui.ctx().layer_painter(over), Pos2::ZERO, &top, None);
        cells::paint(ui.painter(), Pos2::ZERO, &low, None);
    });
    out.textures_delta.clear();
    let layers = cells::layers(&ctx);
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[1].layer, egui::LayerId::background(), "called last");
    let screen = compose(&ctx, &layers, 10, 2);
    let row: String = (0..3).map(|x| screen.buf[(x, 0)].symbol()).collect();
    assert_eq!(row, "TOP");
    // The rest is not a layer's: hand-off boxes there.
    assert_eq!(screen.hand_offs.len(), 2);
}

/// A glyph laid over half of a wide one erases the rest of it, as a terminal does.
#[test]
fn a_cell_over_half_a_wide_glyph_erases_its_other_half() {
    let p = chrome::PALETTE;
    let pen = Pen::new(p.text, p.back);
    let mut buf = Buffer::empty(Cells::new(0, 0, 4, 1));
    let area = buf.area;
    cells::fill(&mut buf, area, pen);
    let mut wide = Buffer::empty(Cells::new(0, 0, 4, 1));
    cells::put(&mut wide, 0, 0, "한", 4, pen);
    assert!(lay(&mut buf, 0, 0, &wide[(0, 0)], 2));
    assert_eq!(buf[(0, 0)].symbol(), "한");
    let mut narrow = Buffer::empty(Cells::new(0, 0, 4, 1));
    cells::put(&mut narrow, 0, 0, "x", 4, pen);
    assert!(lay(&mut buf, 1, 0, &narrow[(0, 0)], 1));
    assert_eq!([buf[(0, 0)].symbol(), buf[(1, 0)].symbol()], [" ", "x"]);
}

// ---------------------------------------------------------------------------------------------
// The switch (TW-1, TW-5).

/// F7 typed into in the terminal, the caret moved back a character, F12: the browser in a window
/// with its dialog, the typed text and the caret where it was (a `d` typed there goes before the
/// `c`); Enter makes the folder, F12 brings it back — listed in the terminal. A key typed into
/// the terminal meanwhile is dropped, never acted on.
#[test]
fn the_switch_runs_the_browser_in_a_window_and_back_with_its_state() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let mut events = vec![plain(KeyCode::F(7))];
    events.extend(typed("abc"));
    events.push(plain(KeyCode::Left));
    events.push(plain(KeyCode::F(12)));
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let browser = fx.b.clone();
    let typing = tape.clone();
    let mut in_window = 0;
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        in_window += 1;
        assert_eq!(
            b.points(),
            [800.0, 480.0],
            "the window gets the grid's size (T16)"
        );
        to_window(b, &browser, Ok(Place::Here), |mut switched| {
            assert!(
                !typing.0.borrow().raw,
                "the terminal is left while the window runs"
            );
            // Typed into the terminal while the window is up: F8 (the delete dialog).
            typing.0.borrow_mut().unread.push_back(plain(KeyCode::F(8)));
            let mut drv = Driver::new(800.0, 480.0);
            drv.as_shell();
            drv.until(&mut switched, 20, |w| w.ready());
            drv.idle(&mut switched, 2);
            let shown = cells::find(&cells::layers(&drv.ctx), "abc");
            assert!(!shown.is_empty(), "the dialog's typed text in the window");
            let more = egui::Event::Text("d".into());
            drv.frame(&mut switched, vec![more], Modifiers::NONE);
            drv.idle(&mut switched, 1);
            assert!(
                !cells::find(&cells::layers(&drv.ctx), "abdc").is_empty(),
                "typed at the carried caret"
            );
            drv.key(&mut switched, Key::Enter, Modifiers::NONE);
            drv.until(&mut switched, 20, |w| w.ready());
            drv.key(&mut switched, Key::F12, Modifiers::NONE);
            drv.idle(&mut switched, 1);
            assert!(drv.window_closed, "F12 there closes the window");
            Ok(())
        })
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut switch).unwrap(), End::Closed);
    assert_eq!(in_window, 1);
    assert!(std::path::Path::new(&fx.root).join("abdc").is_dir());
    let text = bytes.text();
    let line = text.find(IN_A_WINDOW).expect("the line on the main screen");
    assert!(
        text[..line].ends_with(LEAVE),
        "said after the terminal is left"
    );
    assert!(text[line..].contains(ENTER), "the terminal taken again");
    let s = tape.0.borrow();
    assert_eq!(s.raws, [true, false, true, false]);
    assert_eq!(
        s.drops, 2,
        "its unread input dropped on entering, each time"
    );
    drop(s);
    // Back in the terminal: the new folder listed, no delete dialog (F8 was dropped).
    let vt = bytes.last_screen(100, 30);
    let screen = vt.text();
    assert!(screen.contains("abdc"), "{screen}");
    let rows_above_the_keys = (0..29).map(|y| vt.row(y));
    assert!(
        rows_above_the_keys
            .into_iter()
            .all(|r| !r.contains("Delete")),
        "{screen}"
    );
}

#[test]
fn a_switch_to_a_host_or_without_a_display_or_a_window_that_does_not_open_says_why() {
    let cases: [(Place, &str); 3] = [
        (
            Place::Host("box".into()),
            "switching to box comes with the remote switch; --here switches on this machine",
        ),
        (Place::NoDisplay, NO_DISPLAY),
        (
            Place::Here,
            "the window did not open: no backend — check the display, or set a target",
        ),
    ];
    for (place, line) in cases {
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let mut events = vec![plain(KeyCode::F(7))];
        events.extend(typed("kept"));
        events.push(plain(KeyCode::F(12)));
        tape.0.borrow_mut().events.extend(events);
        tape.0.borrow_mut().stop = Some(libc::SIGTERM);
        let browser = fx.b.clone();
        let mut place = Some(place);
        let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
            to_window(b, &browser, Ok(place.take().unwrap()), |_| {
                Err(crate::gui::NotStarted {
                    reason: "no backend".into(),
                }
                .into())
            })
        };
        let mut win = Shared(fx.b.clone());
        let end = b.run(&mut win, &mut switch).unwrap();
        assert_eq!(end, End::Signal(libc::SIGTERM));
        let vt = bytes.last_screen(100, 30);
        assert_eq!(vt.row(28).trim(), line, "on the status row:\n{}", vt.text());
        assert_eq!(vt.cell(1, 28).fg, Ink::Rgb(chrome::PALETTE.error));
        assert!(
            vt.text().contains("kept"),
            "the dialog as it was:\n{}",
            vt.text()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The mode button (10 §7, T4).

/// The title row's `[GUI]` in the terminal is the switch, and the switched window's `[TUI]` the
/// way back: a click on either, no key.
#[test]
fn the_mode_button_switches_from_the_terminal_and_back_from_the_window() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    // `[GUI]` before the clock: at 85–89 of a 100-cell title row (checked below).
    let mut events = click(87, 0).to_vec();
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let browser = fx.b.clone();
    let mut in_window = 0;
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        in_window += 1;
        to_window(b, &browser, Ok(Place::Here), |mut switched| {
            let mut drv = Driver::new(800.0, 480.0);
            drv.as_shell();
            drv.until(&mut switched, 20, |w| w.ready());
            drv.idle(&mut switched, 2);
            let layers = cells::layers(&drv.ctx);
            let hit = cells::find(&layers, "[TUI]");
            let hit = hit.first().expect("the switched window's [TUI]");
            let at = layers[hit.layer].rect(hit.cells).center();
            drv.click(&mut switched, at);
            drv.idle(&mut switched, 1);
            assert!(
                drv.window_closed,
                "a click on [TUI] brings the terminal back"
            );
            Ok(())
        })
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut switch).unwrap(), End::Closed);
    assert_eq!(in_window, 1, "a click on [GUI] is the switch");
    let vt = bytes.last_screen(100, 30);
    assert_eq!(vt.part(0, 85, 90), "[GUI]", "{}", vt.row(0));
}

// ---------------------------------------------------------------------------------------------
// What the terminal cannot show yet (10 §11, T17).

/// F1 (and Alt+1, its alias) in the terminal opens no help it could not show — it would take
/// every key, unseen: the status row says where the help is, and the next key reaches the list.
#[test]
fn f1_in_the_terminal_opens_no_help_and_says_where_it_is() {
    for f1 in [
        plain(KeyCode::F(1)),
        key(KeyCode::Char('1'), KeyModifiers::ALT),
    ] {
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let help_seen = Rc::new(RefCell::new(None));
        let seen = help_seen.clone();
        let ctx = b.ctx.clone();
        // Down once F1 has been read: is the help open then?
        let mut events = vec![f1.clone()];
        events.push(plain(KeyCode::Down));
        events.push(ctrl_q());
        tape.0.borrow_mut().events.extend(events);
        let gate_ctx = ctx.clone();
        let b_gate = fx.b.clone();
        tape.0.borrow_mut().gate = Some(Box::new(move || {
            if seen.borrow().is_none() {
                *seen.borrow_mut() = Some(false);
            }
            if crate::gui::help::is_open(&gate_ctx) {
                *seen.borrow_mut() = Some(true);
            }
            b_gate.borrow().ready()
        }));
        assert_eq!(play(&mut b, &tape, &fx, vec![]), End::Closed);
        assert_eq!(*help_seen.borrow(), Some(false), "{f1:?}: no help opened");
        let text = bytes.text();
        assert!(text.contains(F1_LINE), "{f1:?}: the line on the status row");
        let vt = bytes.vt(100, 30);
        assert_eq!(
            vt.cell(3, 4).bg,
            Ink::Rgb(chrome::PALETTE.bar),
            "{f1:?}: Down after it moved the bar:\n{}",
            vt.text()
        );
    }
}

/// A screen all in cells with a dialog egui draws (as the viewers' and the git panel's prompts
/// still are): `D` opens it, Esc closes it.
struct Asks {
    open: bool,
}

impl super::Window for Asks {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let p = chrome::PALETTE;
        let ctx = ui.ctx().clone();
        let grid = cells::Grid::over(ui.max_rect());
        let mut buf = Buffer::empty(grid.all());
        cells::fill(&mut buf, grid.all(), Pen::new(p.text, p.back));
        cells::paint(ui.painter(), grid.origin, &buf, None);
        if ui.input(|i| i.key_pressed(Key::D)) {
            self.open = true;
        }
        if self.open {
            let id = egui::Id::unique("sopen-term-test-asks");
            let shown = chrome::dialog(&ctx, id, "Asks", |ui| {
                ui.label("a question the terminal cannot draw yet");
            });
            if shown.should_close() {
                self.open = false;
            }
        }
    }
    fn ready(&self) -> bool {
        true
    }
}

/// An egui dialog open in the terminal is a hand-off box where it stands, and the status row
/// says so; Esc closes it as in the window, and the box and the line go.
#[test]
fn a_dialog_egui_draws_is_a_hand_off_box_with_its_line() {
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, 30);
        s.events.extend(typed("d"));
        s.events.push_back(plain(KeyCode::Esc));
        // A key Asks does nothing with: the pass after the one Esc closed it in.
        s.events.extend(typed("x"));
        s.stop = Some(libc::SIGTERM);
    }
    let bytes = Bytes::default();
    let mut b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let mut asks = Asks { open: false };
    let end = b.run(&mut asks, &mut no_switch).unwrap();
    assert_eq!(end, End::Signal(libc::SIGTERM));
    assert!(!asks.open, "Esc closed it");
    // While it was open: the box where it stood, the line on the status row.
    let text = bytes.text();
    let open_at = text
        .find(MODAL_LINE)
        .expect("the dialog's line while it was open");
    let mut vt = Vt::new(100, 30);
    vt.feed(&text.as_bytes()[..open_at + MODAL_LINE.len()]);
    let screen = vt.text();
    let boxed = (0..vt.rows).find(|&y| vt.row(y).contains(HAND_OFF));
    let boxed = boxed.unwrap_or_else(|| panic!("a hand-off box:\n{screen}"));
    assert!(
        boxed > 1 && boxed < 28,
        "where the dialog stands, mid-screen:\n{screen}"
    );
    // Closed: neither is left.
    let vt = bytes.vt(100, 30);
    assert!(!vt.text().contains(HAND_OFF), "{}", vt.text());
    assert!(!vt.text().contains(MODAL_LINE), "{}", vt.text());
}

// ---------------------------------------------------------------------------------------------
// Pictures across the switch (10 §7.2).

/// The texture ids `shapes` paint with, the font's left out.
fn textures(shapes: &[ClippedShape]) -> Vec<egui::TextureId> {
    fn walk(s: &egui::Shape, out: &mut Vec<egui::TextureId>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Rect(r) => out.extend(r.brush.as_ref().map(|b| b.fill_texture_id)),
            egui::Shape::Mesh(m) => out.push(m.texture_id),
            _ => {}
        }
    }
    let mut out = vec![];
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out.retain(|id| *id != egui::TextureId::default());
    out
}

/// Whether every texture `shapes` paint with is one `ctx` has.
fn all_known(ctx: &egui::Context, shapes: &[ClippedShape]) -> (usize, bool) {
    let ids = textures(shapes);
    let known = (ids.iter()).all(|id| ctx.tex_manager().read().meta(*id).is_some());
    (ids.len(), known)
}

/// A picture previewed in one context (the terminal's), then painted in another (a window's
/// after the switch) and back: each paints a texture of its own, never the other's.
#[test]
fn a_picture_made_in_one_context_is_made_again_in_the_other() {
    let fx = fixture(0);
    image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 90]))
        .save(std::path::Path::new(&fx.root).join("alpha/photo.png"))
        .unwrap();
    // Into alpha (listed now, the picture with it), the bar on photo.png: its preview.
    let (mut a, mut win) = window(&fx, 100, 30);
    a.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    a.key(&mut win, Key::Enter, Modifiers::NONE);
    a.until(&mut win, 20, |w| w.ready());
    a.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    a.until(&mut win, 20, |w| w.ready());
    a.idle(&mut win, 20);
    a.until(&mut win, 20, |w| w.ready());
    let (n, known) = all_known(&a.ctx, &window_pass(&a.ctx, &mut win, 100, 30));
    assert!(
        n > 0 && known,
        "the preview's picture, in the first context ({n})"
    );
    let (b, _) = window(&fx, 100, 30);
    let (n, known) = all_known(&b.ctx, &window_pass(&b.ctx, &mut win, 100, 30));
    assert!(
        n > 0 && known,
        "after the switch: the second context's own texture ({n})"
    );
    let (n, known) = all_known(&a.ctx, &window_pass(&a.ctx, &mut win, 100, 30));
    assert!(n > 0 && known, "and back: the first context's again ({n})");
}

/// A video's preview frame, decoded in one context, then painted in another and back (the media
/// window keeps its frame's pixels to make it again). Needs ffmpeg (skips without it, unless
/// `SOPEN_REQUIRE_FFMPEG` is set).
#[test]
fn a_video_frame_made_in_one_context_is_made_again_in_the_other() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let fx = fixture(0);
    let clip = format!("{}/alpha/clip.mp4", fx.root);
    let made = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=1",
        ])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", &clip])
        .status()
        .unwrap();
    assert!(made.success(), "fixture");
    let (mut a, mut win) = window(&fx, 100, 30);
    a.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    a.key(&mut win, Key::Enter, Modifiers::NONE);
    a.until(&mut win, 20, |w| w.ready());
    a.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    a.until(&mut win, 30, |w| w.ready());
    a.idle(&mut win, 20);
    a.until(&mut win, 30, |w| w.ready());
    let (n, known) = all_known(&a.ctx, &window_pass(&a.ctx, &mut win, 100, 30));
    assert!(
        n > 0 && known,
        "the preview's frame, in the first context ({n})"
    );
    let (b, _) = window(&fx, 100, 30);
    let (n, known) = all_known(&b.ctx, &window_pass(&b.ctx, &mut win, 100, 30));
    assert!(
        n > 0 && known,
        "after the switch: the second context's own texture ({n})"
    );
    let (n, known) = all_known(&a.ctx, &window_pass(&a.ctx, &mut win, 100, 30));
    assert!(n > 0 && known, "and back: the first context's again ({n})");
}
