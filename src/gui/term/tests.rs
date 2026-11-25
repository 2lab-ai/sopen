//! The terminal backend against a scripted terminal (10 §15): its bytes go into a strict model
//! of a VT screen ([`Vt`]: every sequence the backend may send, anything else a failure), its
//! input is crossterm's events as the translator receives them (the decode itself is
//! `term_input`'s tests'), and the window it switches to is the test kit's driver. No tty.
//!
//! - TS-1 (10 §14): the window's cells — what a window pass painted in cells, laid in the order it
//!   painted them — equal the terminal's, cell by cell (character, foreground, background) and
//!   the caret, for the list, the F-key row and F7's dialog with `새 폴더 2026` typed, Korean names
//!   and U+1F3B5, at 100×30; under truecolor and through the 256-colour table; and again with
//!   F1's help open. TS-5: the same at 80×24 and 160×50 after a resize.
//! - Keys and the mouse through the backend: Alt+digits, Alt+G, F12, typed Hangul, a click on a
//!   row, the wheel, Ctrl+Q; F1's help and its keys; OSC 52 for a copy; a hand-off box and a
//!   click on it.
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
    /// The signal comes with the last event (during its pass), not after it.
    stop_with_last: bool,
    /// The signal comes at this time, whatever the events.
    stop_at: Option<Instant>,
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
                if s.events.is_empty() && s.stop_with_last {
                    s.stopped = true;
                }
                return Ok(Some(ev));
            }
            // A signal at a set time comes then, not when the events run out.
            if s.stop.is_some() && s.stop_at.is_none() {
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
        let at = s.stop_at.is_some_and(|at| Instant::now() >= at);
        s.stop.filter(|_| s.stopped || at)
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

/// TS-1 again with F1's help open (10 §14): its box over the list — the list's Korean names
/// under its edges among them — is the same cells in the terminal as in the window, under
/// truecolor, through the 256-colour table and in tmux; the help's rows that scroll included.
#[test]
fn ts1_with_f1s_help_open_the_terminal_shows_the_windows_cells() {
    let fx = fixture(0);
    let (mut drv, mut win) = window(&fx, 100, 30);
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    drv.key(&mut win, Key::F1, Modifiers::NONE);
    drv.idle(&mut win, 2);
    assert!(crate::gui::help::is_open(&drv.ctx));
    let mut sw = switched(&fx);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, 30), 100, 30);
    assert!(painted.0.iter().all(|c| c.covered), "every cell is painted");
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, true);
    let text = vt.text();
    for shown in [
        "Keys in the file list",
        "Rename the entry under the bar, in place",
        "Esc, F1 or F3: back",
        "7MkDir",
    ] {
        assert!(text.contains(shown), "{shown:?} on screen:\n{text}");
    }
    assert!(vt.caret().is_none(), "no caret: the help has no field");
    let d = differences(&painted, &vt, Ink::Rgb);
    only_the_mode_button(&d, &vt, "truecolor");
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, false);
    let d = differences(&painted, &vt, |c| Ink::Index(cells::index256(c)));
    only_the_mode_button(&d, &vt, "256 colours");
    // Scrolled to its end: the alias lines, the scroll bar's thumb at the foot.
    drv.key(&mut win, Key::End, Modifiers::NONE);
    drv.idle(&mut win, 2);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, 30), 100, 30);
    let vt = terminal_of(&drv.ctx, &mut win, 100, 30, true);
    assert!(vt.text().contains("Ctrl/⌘+1…0 · Alt+1…0"), "{}", vt.text());
    only_the_mode_button(&differences(&painted, &vt, Ink::Rgb), &vt, "at the end");
    // TS-1 (b): the same bytes in tmux.
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
    assert!(vt.text().contains("Keys in the file list"), "{}", vt.text());
    only_the_mode_button(&differences(&painted, &vt, Ink::Rgb), &vt, "tmux");
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
    // Alt+G with the caret after `abc` and nothing selected: a `g` it typed would show.
    events.push(key(KeyCode::Char('g'), KeyModifiers::ALT));
    // Ctrl+A selects the field's text, Ctrl+C copies it (Ctrl is egui's command, T26).
    events.push(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    events.push(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    events.push(plain(KeyCode::F(12)));
    events.push(plain(KeyCode::Esc));
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let asked = Rc::new(RefCell::new(0));
    let count = asked.clone();
    let first = Rc::new(RefCell::new(String::new()));
    let seen = first.clone();
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        *count.borrow_mut() += 1;
        if *count.borrow() == 1 {
            // The screen of the pass Alt+G was in.
            let screen = b.screen();
            let a = screen.buf.area;
            *seen.borrow_mut() = (a.top()..a.bottom())
                .map(|y| cells::row_text(&screen.buf, y).0)
                .collect::<Vec<_>>()
                .join("\n");
        }
        Ok(Some("not now".into()))
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut switch).unwrap(), End::Closed);
    assert_eq!(*asked.borrow(), 2, "Alt+G and F12, each once");
    let at_alt_g = first.borrow();
    assert!(
        at_alt_g.contains("name abc"),
        "the field after Alt+G:\n{at_alt_g}"
    );
    assert!(
        !at_alt_g.contains("abcg"),
        "Alt+G typed nothing:\n{at_alt_g}"
    );
    let vt = bytes.vt(100, 30);
    assert_eq!(vt.copied, ["abc"], "OSC 52");
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

/// F1's help open in the terminal crosses the switch as it is — open, scrolled to where it was —
/// and comes back as the window left it: scrolled back to its top there, or closed by Esc.
#[test]
fn the_help_crosses_the_switch_as_it_is() {
    for close_there in [false, true] {
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let events = [
            plain(KeyCode::F(1)),
            plain(KeyCode::End),
            plain(KeyCode::F(12)),
        ];
        tape.0.borrow_mut().events.extend(events);
        tape.0.borrow_mut().stop = Some(libc::SIGTERM);
        let browser = fx.b.clone();
        let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
            to_window(b, &browser, Ok(Place::Here), |mut switched| {
                let mut drv = Driver::new(800.0, 480.0);
                drv.as_shell();
                drv.until(&mut switched, 20, |w| w.ready());
                drv.idle(&mut switched, 2);
                assert!(crate::gui::help::is_open(&drv.ctx), "open in the window");
                let layers = cells::layers(&drv.ctx);
                let at_end = cells::find(&layers, "Ctrl/⌘+1…0 · Alt+1…0");
                assert!(
                    !at_end.is_empty(),
                    "scrolled to its end, as in the terminal"
                );
                let key = if close_there { Key::Escape } else { Key::Home };
                drv.key(&mut switched, key, Modifiers::NONE);
                drv.key(&mut switched, Key::F12, Modifiers::NONE);
                drv.idle(&mut switched, 1);
                assert!(drv.window_closed, "F12 there closes the window");
                Ok(())
            })
        };
        let mut win = Shared(fx.b.clone());
        let end = b.run(&mut win, &mut switch).unwrap();
        assert_eq!(end, End::Signal(libc::SIGTERM));
        assert_eq!(crate::gui::help::is_open(&b.ctx), !close_there);
        let screen = bytes.last_screen(100, 30).text();
        if close_there {
            assert!(!screen.contains("Keys in the file list"), "{screen}");
        } else {
            assert!(screen.contains("This screen"), "at its top:\n{screen}");
            assert!(!screen.contains("Ctrl/⌘+1…0 · Alt+1…0"), "{screen}");
        }
    }
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
// The key help, and what the terminal cannot show yet (10 §11, T17).

/// F1 (and Alt+1, its alias) in the terminal opens the key help, in cells: its box on screen,
/// its title, a row and the alias line, nothing on the status row. While it is open the keys are
/// its own — ↓ scrolls it, the list's bar stays — Esc closes it, and the next ↓ moves the bar.
#[test]
fn f1_in_the_terminal_opens_the_help_in_cells_and_its_keys_are_its_own() {
    for f1 in [
        plain(KeyCode::F(1)),
        key(KeyCode::Char('1'), KeyModifiers::ALT),
    ] {
        // The run ends with the help open: the last screen is the help's.
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        tape.0
            .borrow_mut()
            .events
            .extend([f1.clone(), plain(KeyCode::End)]);
        tape.0.borrow_mut().stop = Some(libc::SIGTERM);
        let mut win = Shared(fx.b.clone());
        let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
            panic!("no switch in this test")
        };
        let end = b.run(&mut win, &mut no_switch).unwrap();
        assert_eq!(end, End::Signal(libc::SIGTERM));
        assert!(
            crate::gui::help::is_open(&b.ctx),
            "{f1:?}: the help is open"
        );
        let vt = bytes.vt(100, 30);
        let screen = vt.text();
        for shown in [
            "Keys in the file list",
            "Ctrl/⌘+1…0 · Alt+1…0",
            "Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more",
        ] {
            assert!(
                screen.contains(shown),
                "{f1:?}: {shown:?} on screen:\n{screen}"
            );
        }
        // No hand-off box, and no line on the status row saying the help is the window's.
        assert!(!screen.contains(HAND_OFF), "{f1:?}:\n{screen}");
        // Its keys are its own; Esc gives them back.
        let fx = fixture(0);
        let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
        let events = vec![
            f1.clone(),
            plain(KeyCode::Down),
            plain(KeyCode::Esc),
            plain(KeyCode::Down),
            ctrl_q(),
        ];
        assert_eq!(play(&mut b, &tape, &fx, events), End::Closed);
        let vt = bytes.vt(100, 30);
        let bar = Ink::Rgb(chrome::PALETTE.bar);
        assert_eq!(
            (vt.cell(3, 3).bg, vt.cell(3, 4).bg, vt.cell(3, 5).bg),
            (
                Ink::Rgb(chrome::PALETTE.back),
                bar,
                Ink::Rgb(chrome::PALETTE.back)
            ),
            "{f1:?}: one Down moved the bar, the help's did not:\n{}",
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

/// F1's help opened over a dialog egui draws: the dialog's hand-off box stands under the help —
/// every cell of the help is on screen as the help painted it, none of the box over it.
#[test]
fn the_help_stays_over_a_dialogs_hand_off_box() {
    struct AsksThenHelp(Asks);
    impl super::Window for AsksThenHelp {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if ui.input(|i| i.key_pressed(Key::H)) {
                crate::gui::help::open(ui.ctx(), crate::gui::help::for_window(&[]));
            }
            self.0.ui(ui);
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, 30);
        s.events.extend(typed("dh"));
        s.stop = Some(libc::SIGTERM);
    }
    let bytes = Bytes::default();
    let mut b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let mut win = AsksThenHelp(Asks { open: false });
    let end = b.run(&mut win, &mut no_switch).unwrap();
    assert_eq!(end, End::Signal(libc::SIGTERM));
    assert!(win.0.open && crate::gui::help::is_open(&b.ctx));
    let screen = b.screen();
    assert!(!screen.hand_offs.is_empty(), "the dialog's box");
    let layers = cells::layers(&b.ctx);
    let help = (layers.iter())
        .find(|l| l.layer.order == egui::Order::Tooltip)
        .expect("the help's cells");
    let (ox, oy) = (
        (help.origin.x / CELL.x) as u16,
        (help.origin.y / CELL.y) as u16,
    );
    let a = help.buf.area;
    let mut differ = vec![];
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            let (want, got) = (&help.buf[(x, y)], &screen.buf[(ox + x, oy + y)]);
            // The corners neither the box nor its shadow takes are left as they were.
            let untouched = *want == ratatui_core::buffer::Cell::default();
            if !untouched && (want.symbol() != got.symbol() || want.bg != got.bg) {
                differ.push(format!("({},{}) {:?}", ox + x, oy + y, got.symbol()));
            }
        }
    }
    assert!(differ.is_empty(), "the help covered: {differ:?}");
}

/// A layer of cells in the egui dialog's own order (Foreground) that egui paints after it —
/// over it in the window — stays over the dialog's hand-off box in the terminal: "above the
/// dialog" is egui's painting order, not its order alone.
#[test]
fn a_later_overlay_in_the_dialogs_order_stays_over_its_hand_off_box() {
    struct Overlaid(Asks);
    impl super::Window for Overlaid {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.0.ui(ui);
            if self.0.open {
                let ctx = ui.ctx().clone();
                let grid = cells::Grid::over(ctx.content_rect());
                egui::Area::new(egui::Id::unique("sopen-term-test-overlay"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(Pos2::ZERO)
                    .show(&ctx, |ui| {
                        let at = Cells::new(40, 14, 20, 1);
                        let mut buf = Buffer::empty(at);
                        let pen = Pen::new(chrome::PALETTE.text, chrome::PALETTE.back);
                        cells::fill(&mut buf, at, pen);
                        cells::put(&mut buf, 42, 14, "OVER THE DIALOG", 16, pen);
                        let painter = ui.painter().with_clip_rect(grid.rect(grid.all()));
                        cells::paint(&painter, grid.origin, &buf, None);
                    });
            }
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, 30);
        s.events.extend(typed("d"));
        s.stop = Some(libc::SIGTERM);
    }
    let bytes = Bytes::default();
    let mut b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let mut win = Overlaid(Asks { open: false });
    let end = b.run(&mut win, &mut no_switch).unwrap();
    assert_eq!(end, End::Signal(libc::SIGTERM));
    assert!(win.0.open);
    // In the window: the overlay's area after the dialog's, in its order.
    let modal = b.ctx.memory(|m| m.top_modal_layer()).expect("the dialog");
    let ids: Vec<egui::LayerId> = b.ctx.memory(|m| m.layer_ids().collect());
    let overlay = (cells::layers(&b.ctx).iter())
        .find(|l| l.layer.order == egui::Order::Foreground)
        .map(|l| l.layer)
        .expect("the overlay's cells");
    let place = |l: egui::LayerId| ids.iter().position(|i| *i == l);
    assert_eq!(modal.order, overlay.order);
    assert!(
        place(overlay) > place(modal),
        "the control: painted after the dialog"
    );
    let screen = b.screen();
    let r = *screen.hand_offs.last().expect("the dialog's box");
    assert!(
        r.contains(Position::new(45, 14)),
        "under the overlay: {r:?}"
    );
    let row = cells::row_text(&screen.buf, 14).0;
    assert!(row.contains("OVER THE DIALOG"), "{row:?}");
}

/// Whether the list's bar is on the row `name` stands in (its cells in the bar's colour).
fn bar_on(drv: &Driver, name: &str) -> bool {
    let layers = cells::layers(&drv.ctx);
    cells::find(&layers, name).into_iter().any(|hit| {
        let l = &layers[hit.layer];
        let (a, at) = (l.buf.area, hit.cells);
        let seen = l.seen();
        seen[usize::from(at.y - a.y)][usize::from(at.x - a.x)].bg == Some(chrome::PALETTE.bar)
    })
}

/// A click on the Help slot closes the help, and a key or typed text that came in the same
/// batch of input as the click — before its release — is the help's too: the list does not
/// get it (Enter would open the folder under the bar, `b` would jump to `beta`).
#[test]
fn a_key_with_the_click_that_closes_the_help_is_the_helps() {
    let enter = [
        egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        },
        egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        },
    ];
    let typed = [egui::Event::Text("b".into())];
    for (what, between) in [("Enter", enter.to_vec()), ("typed b", typed.to_vec())] {
        let fx = fixture(0);
        let (mut drv, mut win) = window(&fx, 100, 30);
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
        drv.until(&mut win, 20, |w| w.ready());
        drv.idle(&mut win, 2);
        assert!(bar_on(&drv, "alpha"), "the control: the bar on alpha");
        drv.key(&mut win, Key::F1, Modifiers::NONE);
        drv.idle(&mut win, 2);
        assert!(crate::gui::help::is_open(&drv.ctx));
        // Slot 1: the last row's first cell.
        let slot = Pos2::new(4.0, 29.5 * CELL.y);
        let button = |pressed| egui::Event::PointerButton {
            pos: slot,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut batch = vec![egui::Event::PointerMoved(slot), button(true)];
        batch.extend(between);
        batch.push(button(false));
        drv.frame(&mut win, batch, Modifiers::NONE);
        drv.until(&mut win, 20, |w| w.ready());
        drv.idle(&mut win, 2);
        assert!(
            !crate::gui::help::is_open(&drv.ctx),
            "{what}: the click closed it"
        );
        assert!(drv.find_text("beta").is_some(), "{what}: still the folder");
        assert!(bar_on(&drv, "alpha"), "{what}: the bar where it was");
    }
}

/// Under F1's help the browser's F-key row is in sight but its slots are the help's: a click on
/// Copy opens no Copy dialog and leaves the help; the control: without the help it does.
#[test]
fn under_the_help_the_copy_slot_does_nothing() {
    let fx = fixture(0);
    let (mut drv, mut win) = window(&fx, 100, 30);
    drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    drv.key(&mut win, Key::F1, Modifiers::NONE);
    drv.idle(&mut win, 2);
    drv.click_text(&mut win, "Copy");
    drv.idle(&mut win, 2);
    assert!(crate::gui::help::is_open(&drv.ctx), "the help stays");
    assert!(drv.find_text("[ Copy ]").is_none(), "no Copy dialog");
    drv.key(&mut win, Key::Escape, Modifiers::NONE);
    drv.click_text(&mut win, "Copy");
    drv.idle(&mut win, 2);
    assert!(
        drv.find_text("[ Copy ]").is_some(),
        "the control: the slot is F5"
    );
}

/// While F1's help is open a click on a hand-off box beside it (the preview of the file under
/// the bar) is the help's, as every click is: no switch, the help stays.
#[test]
fn a_click_on_a_hand_off_box_beside_the_help_does_not_switch() {
    let fx = fixture(0);
    let (mut b, tape, _bytes) = backend(&fx, 100, 30, false);
    let mut events = vec![plain(KeyCode::End), plain(KeyCode::F(1))];
    events.extend(click(96, 10));
    tape.0.borrow_mut().events.extend(events);
    tape.0.borrow_mut().stop = Some(libc::SIGTERM);
    let switched = Rc::new(RefCell::new(0));
    let count = switched.clone();
    let mut switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        *count.borrow_mut() += 1;
        Ok(None)
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(
        b.run(&mut win, &mut switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    let screen = b.screen();
    assert!(
        screen
            .hand_offs
            .iter()
            .any(|r| r.contains(Position::new(96, 10))),
        "the control: a hand-off box is there: {:?}",
        screen.hand_offs
    );
    assert_eq!(*switched.borrow(), 0, "no switch");
    assert!(crate::gui::help::is_open(&b.ctx), "the help stays");
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

// ---------------------------------------------------------------------------------------------
// Review round (2026-11-25).

/// `[GUI]` clicked while a dialog in cells is open — an egui modal, whose backdrop takes a click
/// outside it — is the switch and leaves the dialog and its text as they were; `[TUI]` clicked in
/// the switched window the same way back (§9: the button always works).
#[test]
fn the_mode_button_works_over_an_open_dialog_both_ways() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let mut events = vec![plain(KeyCode::F(7))];
    events.extend(typed("abc"));
    events.extend(click(87, 0));
    {
        let mut s = tape.0.borrow_mut();
        s.events.extend(events);
        s.stop = Some(libc::SIGTERM);
    }
    let browser = fx.b.clone();
    let mut in_window = 0;
    let mut switch = |b: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        in_window += 1;
        let screen = b.screen();
        let text: String = (0..30).map(|y| cells::row_text(&screen.buf, y).0).collect();
        assert!(
            text.contains("name abc"),
            "the dialog open when [GUI] switched:\n{text}"
        );
        to_window(b, &browser, Ok(Place::Here), |mut switched| {
            let mut drv = Driver::new(800.0, 480.0);
            drv.as_shell();
            drv.until(&mut switched, 20, |w| w.ready());
            drv.idle(&mut switched, 2);
            let layers = cells::layers(&drv.ctx);
            assert!(
                !cells::find(&layers, "name abc").is_empty(),
                "the dialog in the window"
            );
            let hit = cells::find(&layers, "[TUI]");
            let hit = hit.first().expect("the switched window's [TUI]");
            let at = layers[hit.layer].rect(hit.cells).center();
            drv.click(&mut switched, at);
            drv.idle(&mut switched, 1);
            assert!(
                drv.window_closed,
                "a click on [TUI] over the dialog brings the terminal back"
            );
            Ok(())
        })
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(
        b.run(&mut win, &mut switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    assert_eq!(
        in_window, 1,
        "a click on [GUI] over the dialog is the switch"
    );
    let screen = bytes.last_screen(100, 30).text();
    assert!(
        screen.contains("Make folder") && screen.contains("name abc"),
        "kept:\n{screen}"
    );
}

/// A screen that counts its passes.
struct Counted(Arc<std::sync::atomic::AtomicUsize>);

impl super::Window for Counted {
    fn ui(&mut self, _ui: &mut egui::Ui) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn ready(&self) -> bool {
        true
    }
}

/// A repaint asked for from another thread while the terminal is idle (a worker done) is a pass
/// within a tick, no key needed.
#[test]
fn a_repaint_asked_for_while_idle_is_a_pass_within_a_tick() {
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (40, 10);
        s.stop = Some(libc::SIGTERM);
        s.stop_at = Some(Instant::now() + Duration::from_millis(900));
    }
    let bytes = Bytes::default();
    let mut b = Backend::new(tape.clone(), bytes, skin(), true);
    let passes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ctx = b.ctx.clone();
    let count = passes.clone();
    let asked = std::thread::spawn(move || {
        // Idle first: no pass between these two looks.
        std::thread::sleep(Duration::from_millis(200));
        let idle = count.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        let before = count.load(Ordering::SeqCst);
        ctx.request_repaint();
        std::thread::sleep(TICK * 3);
        (idle, before, count.load(Ordering::SeqCst))
    });
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let mut win = Counted(passes);
    assert_eq!(
        b.run(&mut win, &mut no_switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    let (idle, before, after) = asked.join().unwrap();
    assert!(before > 0, "the first passes ran");
    assert_eq!(idle, before, "idle before the request");
    assert!(
        after > before,
        "a pass after the request ({before} → {after})"
    );
}

/// Where the keyboard is moves with the browser both ways — its absence too: a field that had it
/// in the terminal, none in the window, none back in the terminal.
#[test]
fn no_focus_in_one_context_is_no_focus_in_the_other() {
    let field = egui::Id::unique("sopen-term-test-field");
    let terminal = skin();
    terminal.memory_mut(|m| m.request_focus(field));
    assert_eq!(terminal.memory(|m| m.focused()), Some(field));
    let window = skin();
    Carry::of(&terminal).apply(&window);
    assert_eq!(window.memory(|m| m.focused()), Some(field), "Some across");
    window.memory_mut(|m| m.surrender_focus(field));
    assert_eq!(window.memory(|m| m.focused()), None);
    Carry::of(&window).apply(&terminal);
    assert_eq!(terminal.memory(|m| m.focused()), None, "None back across");
}

/// A signal that comes during the pass that asks for the switch ends the run: the switch is not
/// made, and the signal is not lost.
#[test]
fn a_signal_during_the_switch_pass_ends_the_run_without_switching() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    {
        let mut s = tape.0.borrow_mut();
        s.events.push_back(plain(KeyCode::F(12)));
        s.stop = Some(libc::SIGTERM);
        s.stop_with_last = true;
    }
    let mut switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> {
        panic!("switched after the signal came")
    };
    let mut win = Shared(fx.b.clone());
    assert_eq!(
        b.run(&mut win, &mut switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    left_as_found(&bytes, &tape);
}

/// Taking the terminal again (after a switch) keeps a signal that came meanwhile: it is the run's
/// to end, set once per run.
#[test]
fn watching_the_signals_again_keeps_one_that_came() {
    STOP.store(libc::SIGTERM, Ordering::SeqCst);
    let watch = Signals::watch();
    let kept = STOP.load(Ordering::SeqCst);
    drop(watch);
    STOP.store(0, Ordering::SeqCst);
    assert_eq!(kept, libc::SIGTERM);
}

/// A wide glyph whose second cell is outside its layer's clip is not written past the clip: its
/// first cell a blank in its colours, the second what the layer below shows there.
#[test]
fn a_wide_glyph_half_outside_its_clip_is_a_blank_not_written_past_it() {
    let ctx = skin();
    cells::set_recording(&ctx, true);
    let p = chrome::PALETTE;
    let mut below = Buffer::empty(Cells::new(0, 0, 4, 1));
    cells::put(&mut below, 0, 0, "abcd", 4, Pen::new(p.text, p.back));
    let mut above = Buffer::empty(Cells::new(0, 0, 4, 1));
    let pen = Pen::new(p.bar_text, p.bar);
    cells::put(&mut above, 1, 0, "한", 2, pen);
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(80.0, 32.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        cells::paint(ui.painter(), Pos2::ZERO, &below, None);
        // The clip ends between 한's two cells (x 16 points).
        let over = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("over"));
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(16.0, 16.0));
        let painter = ui.ctx().layer_painter(over).with_clip_rect(clip);
        cells::paint(&painter, Pos2::ZERO, &above, None);
    });
    out.textures_delta.clear();
    let screen = compose(&ctx, &cells::layers(&ctx), 10, 2);
    let row: Vec<&str> = (0..4).map(|x| screen.buf[(x, 0)].symbol()).collect();
    assert_eq!(row, ["a", " ", "c", "d"], "{row:?}");
    assert_eq!(
        screen.buf[(1, 0)].bg,
        Color::Rgb(p.bar.r(), p.bar.g(), p.bar.b())
    );
    assert_eq!(
        screen.buf[(2, 0)].bg,
        Color::Rgb(p.back.r(), p.back.g(), p.back.b())
    );
}

/// A screen that copies `0` bytes of text to the clipboard on C.
struct Copies(usize);

impl super::Window for Copies {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let p = chrome::PALETTE;
        let grid = cells::Grid::over(ui.max_rect());
        let mut buf = Buffer::empty(grid.all());
        cells::fill(&mut buf, grid.all(), Pen::new(p.text, p.back));
        cells::paint(ui.painter(), grid.origin, &buf, None);
        if ui.input(|i| i.key_pressed(Key::C)) {
            ui.ctx().copy_text("x".repeat(self.0));
        }
    }
    fn ready(&self) -> bool {
        true
    }
}

/// A copy above 256 KiB is not sent through the terminal (OSC 52 there is refused or cut by
/// many): the status row says so; one at the limit is sent.
#[test]
fn a_copy_too_large_for_osc_52_is_said_not_sent() {
    for (size, sent) in [(256 * 1024, true), (256 * 1024 + 1, false)] {
        let tape = Tape::default();
        {
            let mut s = tape.0.borrow_mut();
            s.size = (100, 30);
            s.events.extend(typed("c"));
            s.stop = Some(libc::SIGTERM);
        }
        let bytes = Bytes::default();
        let mut b = Backend::new(tape.clone(), bytes.clone(), skin(), true);
        let mut no_switch =
            |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
        let end = b.run(&mut Copies(size), &mut no_switch).unwrap();
        assert_eq!(end, End::Signal(libc::SIGTERM));
        let vt = bytes.vt(100, 30);
        assert_eq!(vt.copied.len(), usize::from(sent), "{size} bytes");
        if !sent {
            let line = format!(
                "too large to copy through the terminal ({} KiB)",
                size.div_ceil(1024)
            );
            assert_eq!(vt.row(28).trim(), line, "{}", vt.text());
        }
    }
}

/// A status line with control characters in it (a file name, a host's message) prints none of
/// them: no escape, BEL or C1 reaches the terminal.
#[test]
fn a_status_line_prints_no_control_characters() {
    let tape = Tape::default();
    tape.0.borrow_mut().size = (60, 5);
    let bytes = Bytes::default();
    let mut b = Backend::new(tape, bytes.clone(), skin(), true);
    b.said = Some("bad\x1b[31mred\x07bell\u{9b}2Jc1\x1b]52;c;eA==\x07end".to_string());
    b.enter().unwrap();
    b.pass(&mut Copies(0), vec![]).unwrap();
    b.draw().unwrap();
    // The strict model panics on any escape or control character the backend does not send.
    let vt = bytes.vt(60, 5);
    let row = vt.row(3);
    assert!(row.contains("bad") && row.contains("end"), "{row:?}");
    assert!(!row.chars().any(char::is_control), "{row:?}");
    assert!(vt.copied.is_empty(), "no OSC 52 smuggled in");
}

/// A hand-off box says what is there: the pane's own title (the preview's path).
#[test]
fn a_hand_off_box_has_the_panes_title() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    let mut events: Vec<CEvent> = (0..4).map(|_| plain(KeyCode::Down)).collect();
    events.push(ctrl_q());
    tape.0.borrow_mut().events.extend(events);
    let mut no_switch = |_: &mut Backend<Tape, Bytes>| -> io::Result<Option<String>> { Ok(None) };
    let mut win = Shared(fx.b.clone());
    assert_eq!(b.run(&mut win, &mut no_switch).unwrap(), End::Closed);
    let vt = bytes.vt(100, 30);
    assert!(
        vt.part(1, 50, 100).contains("readme.md"),
        "the box's title:\n{}",
        vt.text()
    );
}

/// What is written to the standard error while the terminal is held does not land on its
/// screen: it is kept, and written when the terminal is let go.
/// Set in the child [`stderr_child`] runs.
const STDERR_CHILD: &str = "SOPEN_TEST_TERM_STDERR_CHILD";
const STDERR_CHILD_TEST: &str = "gui::term::tests::stderr_child";
/// How much the child writes while its standard error is held: past [`STDERR_KEPT`].
const STDERR_WRITTEN: usize = STDERR_KEPT + 300_000;

/// Not a test of its own (it returns at once): [`held_stderr_is_kept_bounded_and_written_after`]'s
/// child — fd 2 redirected in a process of its own, never this test binary's. A line before the
/// hold goes straight out; past the bound while held; a line after the release straight out.
#[test]
fn stderr_child() {
    if std::env::var_os(STDERR_CHILD).is_none() {
        return;
    }
    let write = |bytes: &[u8]| {
        let mut at = 0;
        while at < bytes.len() {
            // SAFETY: a plain write of a live buffer to fd 2.
            let n = unsafe { libc::write(2, bytes[at..].as_ptr().cast(), bytes.len() - at) };
            assert!(n > 0, "write to fd 2");
            at += n as usize;
        }
    };
    write(b"BEFORE THE HOLD\n");
    let held = Stderr::hold().expect("stderr held");
    let mut bytes = b"FIRST HELD\n".to_vec();
    let tail = b"\nLAST HELD\n";
    bytes.resize(STDERR_WRITTEN - tail.len(), b'x');
    bytes.extend_from_slice(tail);
    assert_eq!(bytes.len(), STDERR_WRITTEN);
    write(&bytes);
    let kept = held.release();
    write(b"AFTER THE RELEASE\n");
    assert!(kept.ends_with(b"LAST HELD\n"));
}

/// What is written to the standard error while the terminal is held does not land on its
/// screen: it is kept — the last [`STDERR_KEPT`] bytes, a line saying how many went before them —
/// and written when the terminal is let go, between what came before and after.
#[test]
fn held_stderr_is_kept_bounded_and_written_after() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            STDERR_CHILD_TEST,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(STDERR_CHILD, "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "the child: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    let dropped = STDERR_WRITTEN - STDERR_KEPT;
    let said = format!("… {dropped} bytes of stderr dropped\n");
    let at = |what: &str| {
        err.find(what)
            .unwrap_or_else(|| panic!("{what:?} in the child's stderr"))
    };
    let (before, line, last, after) = (
        at("BEFORE THE HOLD"),
        at(&said),
        at("LAST HELD"),
        at("AFTER THE RELEASE"),
    );
    assert!(before < line && line < last && last < after, "in order");
    assert!(!err.contains("FIRST HELD"), "the start went past the bound");
    let kept = &err.as_bytes()[line + said.len()..after];
    assert_eq!(kept.len(), STDERR_KEPT, "the last {STDERR_KEPT} bytes");
}

/// A repaint asked for after a delay longer than an `Instant` can reach (on every OS; short of
/// `Duration::MAX`, which egui never passes on) is no deadline, not a panic in egui's callback.
#[test]
fn a_repaint_asked_for_ages_away_is_no_deadline() {
    let tape = Tape::default();
    tape.0.borrow_mut().size = (40, 10);
    let b = Backend::new(tape, Bytes::default(), skin(), true);
    b.ctx.request_repaint_after(Duration::from_secs(u64::MAX));
    assert_eq!(*b.asked.lock().unwrap(), None);
}

/// The terminal's input on a pty this test owns: quiet, then ready with a key, then — the
/// terminal's side closed, as a hang-up — an error at once, never a wait or a spin (crossterm's
/// own reader loops on a hung-up terminal's empty reads without returning).
#[test]
fn a_terminal_that_hung_up_is_an_error_at_once() {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: two out-pointers to live ints; no name, termios or size asked for.
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(made, 0, "openpty");
    let quiet = wait_input(slave, Duration::from_millis(50)).unwrap();
    assert_eq!(quiet, Input::Quiet);
    // SAFETY: writes from a live buffer to the master this test opened (a line: the pty reads
    // in lines until raw mode).
    unsafe { libc::write(master, b"x\n".as_ptr().cast(), 2) };
    assert_eq!(
        wait_input(slave, Duration::from_secs(5)).unwrap(),
        Input::Ready
    );
    let mut byte = [0u8; 8];
    // SAFETY: reads into a live buffer from the slave this test opened.
    unsafe { libc::read(slave, byte.as_mut_ptr().cast(), byte.len()) };
    // SAFETY: closes the master this test opened: the terminal's side is gone.
    unsafe { libc::close(master) };
    let at = Instant::now();
    let gone = wait_input(slave, Duration::from_secs(5));
    // SAFETY: closes the slave this test opened.
    unsafe { libc::close(slave) };
    assert!(
        gone.as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::BrokenPipe),
        "{gone:?}"
    );
    assert!(
        at.elapsed() < Duration::from_secs(1),
        "at once: {:?}",
        at.elapsed()
    );
}

// ---------------------------------------------------------------------------------------------
// Fix-check (2026-11-25): a press on the mode button whose release the switch swallowed.

/// `[GUI]` pressed: the switch runs in the press's pass; the release, sent by the terminal
/// meanwhile, is dropped with the rest of its unread input when the terminal is taken again
/// (mouse reporting was off). Back in the terminal the button's gesture is over: the next click's
/// release is not taken for it.
fn gui_press_then_back(
    fx: &Fx,
    tape: &Tape,
    also: impl Fn(&mut Backend<Tape, Bytes>) + 'static,
) -> impl FnMut(&mut Backend<Tape, Bytes>) -> io::Result<Option<String>> {
    let browser = fx.b.clone();
    let typing = tape.clone();
    move |b: &mut Backend<Tape, Bytes>| {
        let ran = to_window(b, &browser, Ok(Place::Here), |mut switched| {
            // The release of the press on [GUI], sent while the window runs.
            let up = mouse(MouseEventKind::Up(MouseButton::Left), 87, 0);
            typing.0.borrow_mut().unread.push_back(up);
            let mut drv = Driver::new(800.0, 480.0);
            drv.as_shell();
            drv.until(&mut switched, 20, |w| w.ready());
            drv.key(&mut switched, Key::F12, Modifiers::NONE);
            drv.idle(&mut switched, 1);
            assert!(drv.window_closed, "F12 there");
            Ok(())
        });
        also(b);
        ran
    }
}

#[test]
fn after_the_mode_button_switched_a_click_on_a_row_moves_the_bar() {
    let fx = fixture(0);
    let (mut b, tape, bytes) = backend(&fx, 100, 30, false);
    {
        let mut s = tape.0.borrow_mut();
        // The press alone: its release comes during the window.
        s.events
            .push_back(mouse(MouseEventKind::Down(MouseButton::Left), 87, 0));
        // Back: one click on `beta` (title 0, frame 1, heads 2, `..` 3, alpha 4).
        s.events.extend(click(5, 5));
        s.stop = Some(libc::SIGTERM);
    }
    let mut switch = gui_press_then_back(&fx, &tape, |_| {});
    let mut win = Shared(fx.b.clone());
    assert_eq!(
        b.run(&mut win, &mut switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    let vt = bytes.last_screen(100, 30);
    let bar = Ink::Rgb(chrome::PALETTE.bar);
    assert_eq!(
        vt.cell(3, 5).bg,
        bar,
        "the bar on the clicked row:\n{}",
        vt.text()
    );
}

#[test]
fn after_the_mode_button_switched_a_click_on_make_works() {
    let fx = fixture(0);
    let (mut b, tape, _bytes) = backend(&fx, 100, 30, false);
    {
        let mut s = tape.0.borrow_mut();
        s.events.push_back(plain(KeyCode::F(7)));
        s.events.extend(typed("made"));
        s.events
            .push_back(mouse(MouseEventKind::Down(MouseButton::Left), 87, 0));
        s.stop = Some(libc::SIGTERM);
    }
    // Back in the terminal: a click on the dialog's `[ Make ]`, where its last screen had it.
    let clicks = tape.clone();
    let mut switch = gui_press_then_back(&fx, &tape, move |b| {
        let screen = b.screen();
        let a = screen.buf.area;
        let found = (a.top()..a.bottom()).find_map(|y| {
            let (text, at) = cells::row_text(&screen.buf, y);
            let i = text.find("[ Make ]")?;
            Some((at[text[..i].chars().count()] + 3, y))
        });
        let (x, y) = found.expect("[ Make ] on the terminal's screen");
        clicks.0.borrow_mut().events.extend(click(x, y));
    });
    let mut win = Shared(fx.b.clone());
    assert_eq!(
        b.run(&mut win, &mut switch).unwrap(),
        End::Signal(libc::SIGTERM)
    );
    assert!(
        std::path::Path::new(&fx.root).join("made").is_dir(),
        "the click on [ Make ] made the folder"
    );
}

/// The mode button's gesture on its own: a press on it is taken and its release with it; a
/// primary press elsewhere, or the focus lost, ends the gesture, so the next release is the
/// screen's.
#[test]
fn the_mode_buttons_gesture_ends_with_a_press_elsewhere_or_the_focus_lost() {
    let at = Some(Rect::from_min_size(
        Pos2::new(680.0, 0.0),
        Vec2::new(40.0, 16.0),
    ));
    let press = |x: f32, pressed: bool| egui::Event::PointerButton {
        pos: Pos2::new(x, 8.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    for ender in [press(10.0, true), egui::Event::WindowFocused(false)] {
        let mut down = false;
        let mut events = vec![press(700.0, true)];
        assert!(
            crate::gui::mode_button(&mut events, &mut down, at),
            "the switch"
        );
        assert!(events.is_empty() && down, "the press taken");
        // The release never came (it went to the other backend); then:
        let mut events = vec![ender.clone(), press(10.0, false)];
        assert!(!crate::gui::mode_button(&mut events, &mut down, at));
        assert_eq!(
            events,
            [ender.clone(), press(10.0, false)],
            "{ender:?}: the screen's"
        );
        assert!(!down);
    }
}

/// A switch ends the gesture in the backend it left and in the one it comes back to: a release
/// after it is the screen's, though no press came in between.
#[test]
fn a_switch_ends_the_mode_buttons_gesture() {
    let ctx = skin();
    let pass = |events: Vec<egui::Event>| {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 480.0))),
            events,
            focused: true,
            ..Default::default()
        };
        let mut released = false;
        let mut out = ctx.run_ui(raw, |ui| {
            chrome::set_mode_button(ui.ctx(), "[GUI]");
            chrome::title_panel(ui, "sopen-test-title", "/somewhere");
            released |= ui.input(|i| i.pointer.any_released());
        });
        out.textures_delta.clear();
        released
    };
    pass(vec![]);
    let button = chrome::mode_button_rect(&ctx).expect("drawn");
    let primary = |pressed: bool| egui::Event::PointerButton {
        pos: button.center(),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    pass(vec![
        egui::Event::PointerMoved(button.center()),
        primary(true),
    ]);
    assert!(crate::gui::switch_asked(&ctx), "the press switched");
    crate::gui::gestures_ended(&ctx);
    assert!(
        pass(vec![primary(false)]),
        "a release after the switch reaches the screen"
    );
}
