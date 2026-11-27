//! The address window (09 §7, WV-E2): `sopen URL` shows the address in an xtools window of its
//! own, on every route — here, as HOST's half of a tunnel, and as HOST's sopen given the address
//! as written — and returns when it is closed. `--browser` keeps the system browser, as does a
//! machine with no display (§4, §8), and so, after §4's line, does a process with no web engine
//! ([`way`]: the engine is picked before the window, so none flashes up for nothing).
//!
//! - **The view** ([`Web`], `Content::Address`): made at the window's first pass, on its origin
//!   — the address up to its path, as written ([`split_origin`]) — and loaded with the rest;
//!   scripts on (§7, decision 10), F2 turns them off and on. The engine keeps the main frame in
//!   the origin (`mac.rs`'s scope); a view the engine refuses even so closes the window, and the
//!   run takes §4's fallback ([`Shown::Unavailable`]).
//! - **The cues** ([`Cue`], [`speak`]): once the view is made, [`url::TUNNEL_WINDOW`] (HOST's
//!   half of a tunnel) or the opened line (any other route); once its load is asked for,
//!   [`url::TUNNEL_OPEN`] — off the load, not the engine's word that it started, which might
//!   never come; once the owner closed the window and it is gone, [`url::TUNNEL_CLOSED`]
//!   ([`Report::ended`]) — never after the input's end, which closes the window for the other
//!   side, gone already. All written ignoring errors: the session may be gone.
//! - **The screen**, cells on the grid (10 §4): the title row (`xtools`, the address), the
//!   page's frame — single-lined while sopen has the keyboard, double while the page has it
//!   (§2.4) — with the bar (the document on screen; `scripts ON|off`, loading) and one line
//!   under it, the page at the rest of its cells, and the F-key row `1Help 2JS off 3Exit`. Every
//!   address it shows goes through [`url::redacted`].
//! - **Navigation**: a link the owner clicked out of the origin goes to the system browser
//!   ([`Opener`], on a thread of its own: `url::open` waits for the opener), one at a time — only
//!   an `http` or `https` one, as [`url::parse`] reads it; anything else is said and not opened,
//!   since the system's opener would start an application for a `file:` link. Any other navigation out
//!   was refused by the engine: a line says where it went.
//! - **Keys**: Esc while the page has the keyboard takes it back (the component, or the macOS
//!   key monitor); one that reaches the window closes it. Cmd/Ctrl+R reloads. Before anything
//!   was on screen — the first load failed, a dev server not up yet — Cmd/Ctrl+R and F2 load the
//!   address again: there is nothing to reload.
//! - **The input** ([`Input`], HOST's half of a tunnel): read to its end on a thread of its own;
//!   its end — the other side's sopen gone — closes the window. A scripted run ends by its hook.

#[cfg(test)]
mod tests;

use super::{Content, Event, Keyboard, Settings, Unavailable, Web};
use crate::gui::browser::keys::{self, Mods};
use crate::gui::cells::{self, Cells, Grid, Pen};
use crate::gui::{chrome, Hooks, Window};
use crate::url::{self, Url};
use eframe::egui::{self, Key};
use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The page's frame title.
pub(crate) const TITLE: &str = "Web Page";

/// How an address opens where it shows (09 §7, §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Way {
    /// The system browser, as before 09: `--browser`, or no display here (§4, §8).
    Browser,
    /// The system browser after §4's line ([`fallback_line`]): this process has no web engine.
    Fallback(Unavailable),
    /// The address window.
    Window,
}

/// How an address opens with `--browser` (`browser`) on a machine with or without a display:
/// the engine (`engine`, [`engine_here`]) is asked for only when the window is the way. The
/// same on every route: HOST's half of a tunnel and HOST's sopen given the address decide here.
pub fn way(browser: bool, display: bool, engine: impl FnOnce() -> Result<(), Unavailable>) -> Way {
    if browser || !display {
        return Way::Browser;
    }
    match engine() {
        Ok(()) => Way::Window,
        Err(why) => Way::Fallback(why),
    }
}

/// Whether this process can show an address (§4): its engine, picked now if it was not yet. A
/// backend that does not contain file views shows addresses all the same.
pub fn engine_here() -> Result<(), Unavailable> {
    super::engine().map(drop)
}

/// §4's line before an address goes to the system browser for want of an engine (after
/// `sopen: `): `no web engine here (<reason>); opening <shown> in the system browser`.
pub fn fallback_line(why: &Unavailable, shown: &str) -> String {
    format!("{}; opening {shown} in the system browser", why.line())
}

/// The line `sopen URL` prints once its window is up (route 1, and HOST's for route 3).
pub fn opened_line(shown: &str) -> String {
    format!("opened {shown} in xtools")
}

/// The window's title: `xtools — <shown>`.
pub fn title(shown: &str) -> String {
    format!("{} — {shown}", chrome::PROGRAM)
}

/// What the address window says on its way (09 §7), in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// The view is made (`Web::new` succeeded).
    Window,
    /// Its load was asked for.
    Open,
    /// The owner closed the window, and it is gone.
    Closed,
}

/// Writes `cue` on `out`: as HOST's half of a tunnel (`hold`) its fixed line; on any other route
/// the opened line for [`Cue::Window`] and nothing else. Errors ignored, as [`url::write_cue`]
/// ignores them: the session may be gone.
pub fn speak(out: &mut dyn std::io::Write, hold: bool, shown: &str, cue: Cue) {
    match (hold, cue) {
        (true, Cue::Window) => url::write_cue(out, url::TUNNEL_WINDOW),
        (true, Cue::Open) => url::write_cue(out, url::TUNNEL_OPEN),
        (true, Cue::Closed) => url::write_cue(out, url::TUNNEL_CLOSED),
        (false, Cue::Window) => url::write_cue(out, &opened_line(shown)),
        (false, Cue::Open | Cue::Closed) => {}
    }
}

/// `text` (an address as [`url::parse`] took it) as its origin — up to its path, query or
/// fragment, a user and password kept: the engine gets the address as the browser would — and
/// the rest, as written.
pub(crate) fn split_origin(text: &str) -> (&str, &str) {
    let start = text.find("://").map_or(0, |i| i + 3);
    let end = text[start..]
        .find(['/', '?', '#'])
        .map_or(text.len(), |i| start + i);
    text.split_at(end)
}

/// What a failed load was, from the engine's error (09 §7 "Failures").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// Anything else: the page did not load (the dev server is not up yet).
    Load,
    /// A certificate the system does not trust: never accepted.
    Untrusted,
    /// HTTP authentication: this window gives no login.
    Login,
    /// A client certificate asked for: this window gives none.
    ClientCertificate,
}

/// What `why` — the engine's failure, macOS's `<description> (<domain> <code>)` — was: by its
/// `NSURLErrorDomain` code (-1201…-1204 the server's certificate, -1205 and -1206 a client
/// certificate, -1012 and -1013 a login), or a 401 or 407 with nothing to show (the engine's
/// `the page answered <status>`). A 401 whose body the engine shows is the page, not a failure.
pub(crate) fn failure(why: &str) -> Failure {
    match why {
        "the page answered 401" | "the page answered 407" => return Failure::Login,
        _ => {}
    }
    let code = why
        .rsplit_once("(NSURLErrorDomain ")
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .and_then(|code| code.parse::<i64>().ok());
    match code {
        Some(-1204..=-1201) => Failure::Untrusted,
        Some(-1206 | -1205) => Failure::ClientCertificate,
        Some(-1013 | -1012) => Failure::Login,
        _ => Failure::Load,
    }
}

/// Opens a link the owner clicked out of the origin: the system browser ([`url::open`]); why
/// not, as one line. Called on a thread of its own.
pub type Opener = Arc<dyn Fn(&Url) -> Result<(), String> + Send + Sync>;

/// The system browser's opener.
fn system_browser() -> Opener {
    Arc::new(|url: &Url| url::open(url).map_err(|e| format!("{e:#}").replace('\n', " ")))
}

/// HOST's half of a tunnel: its input, read to its end, and the window's context, woken then.
#[derive(Clone, Default)]
pub(crate) struct Input {
    ended: Arc<AtomicBool>,
    wake: Arc<OnceLock<egui::Context>>,
}

impl Input {
    /// Reads `from` to its end — or an error reading it: either way the other side is gone — on
    /// a thread of its own, then [`Input::end`].
    fn watch(&self, from: impl Read + Send + 'static) -> std::io::Result<()> {
        let input = self.clone();
        std::thread::Builder::new()
            .name("sopen-address-input".into())
            .spawn(move || {
                let mut from = from;
                let _ = std::io::copy(&mut from, &mut std::io::sink());
                input.end();
            })
            .map(drop)
    }

    /// The input ended: the window hears it on its next pass, which this asks for.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        if let Some(ctx) = self.wake.get() {
            ctx.request_repaint();
        }
    }

    fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    fn bind(&self, ctx: &egui::Context) {
        let _ = self.wake.set(ctx.clone());
    }
}

/// How the window went, for [`run`] once it is gone.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// The view was made.
    made: bool,
    /// The engine refused the view (§4): why.
    failed: Option<Unavailable>,
    /// The input's end closed it.
    by_input: bool,
}

impl Report {
    /// What the window's end says: [`Cue::Closed`] once the owner closed a view that was made.
    pub(crate) fn ended(&self) -> Option<Cue> {
        (self.made && !self.by_input).then_some(Cue::Closed)
    }

    /// What the window's end writes on `out` ([`run`]'s tail): [`Report::ended`]'s cue, as
    /// [`speak`] writes it on `hold`'s route — the closed cue on HOST's half of a tunnel, nothing
    /// on another route.
    pub(crate) fn finish(&self, out: &mut dyn std::io::Write, hold: bool, shown: &str) {
        if let Some(cue) = self.ended() {
            speak(out, hold, shown, cue);
        }
    }
}

/// How [`run`] ended.
pub enum Shown {
    /// The window was up, and is closed.
    Closed,
    /// The engine refused the view (§4): the caller opens the system browser after the line.
    Unavailable(Unavailable),
}

/// The system browser's answer for a clicked link: the link shown, or why it was not opened.
type Answer = Result<String, String>;

/// What the line under the bar says.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// A failed load, by its kind, with the engine's reason.
    Failed(Failure, String),
    /// The web process ended.
    Ended,
    /// A navigation out of the origin, refused: where it went.
    Refused(String),
    /// A clicked link out, on its way to the system browser.
    Opening(String),
    /// The system browser took it.
    Opened(String),
    /// Not opened: the whole line.
    NotOpened(String),
}

/// The address window (see the module's header).
pub struct AddressWindow {
    /// The address as the owner gave it, shown.
    shown: String,
    origin: String,
    /// The rest of the address (path, query, fragment), as written.
    path: String,
    host: String,
    web: Option<Web>,
    /// The view was asked for (once).
    asked: bool,
    say: Box<dyn FnMut(Cue)>,
    opener: Opener,
    /// The system browser's answers, as they come: the link shown, or why not.
    answered: Sender<Answer>,
    answers: Receiver<Answer>,
    /// Links on their way to the system browser: one at a time.
    opening: usize,
    input: Option<Input>,
    scripted: bool,
    /// The input's end asked the window to close.
    ending: bool,
    report: Rc<RefCell<Report>>,
    loading: bool,
    /// The main frame finished or failed since the last load.
    loaded: bool,
    /// A document has been on screen: there is something to reload.
    committed: bool,
    /// The document on screen, as the engine said it (shown redacted).
    document: Option<String>,
    line: Option<Line>,
    painted: bool,
}

impl AddressWindow {
    /// The window for `url`: its cues to `say`, clicked links out to `opener`; `input` the tunnel
    /// half's input (its end closes the window, unless `scripted`); how it went in `report`.
    pub(crate) fn new(
        url: Url,
        say: Box<dyn FnMut(Cue)>,
        opener: Opener,
        input: Option<Input>,
        scripted: bool,
        report: Rc<RefCell<Report>>,
    ) -> Self {
        let (origin, path) = split_origin(url.as_str());
        let (answered, answers) = mpsc::channel();
        AddressWindow {
            shown: url.shown(),
            origin: origin.to_string(),
            path: path.to_string(),
            host: url.host().to_string(),
            web: None,
            asked: false,
            say,
            opener,
            answered,
            answers,
            opening: 0,
            input,
            scripted,
            ending: false,
            report,
            loading: false,
            loaded: false,
            committed: false,
            document: None,
            line: None,
            painted: false,
        }
    }

    /// The view, at the first pass (the windows' thread's): the window cue, its load, the open
    /// cue. One the engine refuses closes the window, with no cue. None once the input has ended
    /// (the other side gone before the first pass): nothing to load toward a port the tunnel no
    /// longer forwards, nothing to say.
    fn make(&mut self, ctx: &egui::Context) {
        if self.ending || std::mem::replace(&mut self.asked, true) {
            return;
        }
        let content = Content::Address {
            origin: self.origin.clone(),
        };
        match Web::new(content, Settings::default()) {
            Ok(mut web) => {
                self.report.borrow_mut().made = true;
                (self.say)(Cue::Window);
                web.load(&self.path);
                self.loaded = false;
                (self.say)(Cue::Open);
                self.web = Some(web);
            }
            Err(why) => {
                self.report.borrow_mut().failed = Some(why);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// The input's end closes the window (HOST's half of a tunnel; not a scripted run's).
    fn heed_the_input(&mut self, ctx: &egui::Context) {
        let Some(input) = &self.input else {
            return;
        };
        input.bind(ctx);
        if self.scripted || self.ending || !input.ended() {
            return;
        }
        self.ending = true;
        self.report.borrow_mut().by_input = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn scripts(&self) -> bool {
        self.web.as_ref().is_some_and(Web::scripts)
    }

    fn keyboard(&self) -> Keyboard {
        self.web.as_ref().map_or(Keyboard::Sopen, Web::keyboard)
    }

    /// What the view said since the last pass.
    fn events(&mut self, ctx: &egui::Context) {
        let Some(web) = self.web.as_mut() else {
            return;
        };
        for event in web.events() {
            match event {
                Event::Loading => {
                    (self.loading, self.loaded) = (true, false);
                    if matches!(self.line, Some(Line::Failed(..) | Line::Ended)) {
                        self.line = None;
                    }
                }
                Event::Finished => (self.loading, self.loaded) = (false, true),
                Event::Failed(why) => {
                    (self.loading, self.loaded) = (false, true);
                    self.line = Some(Line::Failed(failure(&why), why));
                }
                Event::Document(at) => {
                    self.document = Some(at);
                    self.committed = true;
                }
                Event::Refused {
                    target,
                    clicked: true,
                } => self.outside(ctx, &target),
                Event::Refused {
                    target,
                    clicked: false,
                } => self.line = Some(Line::Refused(target)),
                // An address view has no rule list: nothing of it is counted.
                Event::Blocked(_) => {}
                Event::ProcessEnded => {
                    (self.loading, self.loaded) = (false, true);
                    self.line = Some(Line::Ended);
                }
            }
        }
    }

    /// A link the owner clicked out of the origin: an `http` or `https` one to the system browser
    /// on a thread of its own — one at a time: another while one is opening (a script's
    /// `a.click()` may count as a click) is said and not opened; anything else said, not opened.
    fn outside(&mut self, ctx: &egui::Context, target: &str) {
        let shown = url::redacted(target);
        if self.opening > 0 {
            self.line = Some(Line::NotOpened(format!(
                "{shown}: not opened — a link is still opening in the system browser"
            )));
            return;
        }
        let link = match url::parse(target) {
            Some(Ok(link)) => link,
            Some(Err(why)) => {
                self.line = Some(Line::NotOpened(format!("not opened: {why}")));
                return;
            }
            None => {
                self.line = Some(Line::NotOpened(format!(
                    "{shown}: not opened — only an http:// or https:// link goes to the system \
                     browser"
                )));
                return;
            }
        };
        let (opener, tx, ctx) = (self.opener.clone(), self.answered.clone(), ctx.clone());
        let to = shown.clone();
        let spawned = std::thread::Builder::new()
            .name("sopen-address-link".into())
            .spawn(move || {
                // An opener that panics still answers: `opening` never sticks.
                let opened =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| opener(&link)))
                        .unwrap_or_else(|_| Err("the opener failed".to_string()));
                let _ = tx.send(opened.map(|()| to));
                ctx.request_repaint();
            });
        self.line = Some(match spawned {
            Ok(_) => {
                self.opening += 1;
                Line::Opening(shown)
            }
            Err(e) => Line::NotOpened(format!("{shown}: not opened: {e}")),
        });
    }

    /// The system browser's answers since the last pass.
    fn take_answers(&mut self) {
        while let Ok(answer) = self.answers.try_recv() {
            self.opening = self.opening.saturating_sub(1);
            self.line = Some(match answer {
                Ok(shown) => Line::Opened(shown),
                Err(why) => Line::NotOpened(why),
            });
        }
    }

    /// The line's words.
    fn line_text(&self, line: &Line) -> String {
        let shown = &self.shown;
        let browser = "--browser opens it in the system browser";
        match line {
            // The engine's words can carry an address (macOS: one it could not make a URL of).
            Line::Failed(Failure::Load, why) => format!(
                "cannot load {shown}: {} — Cmd/Ctrl+R tries again",
                url::redacted(why)
            ),
            Line::Failed(Failure::Untrusted, _) => {
                format!(
                    "the certificate of {} is not trusted — {browser}",
                    self.host
                )
            }
            Line::Failed(Failure::Login, _) => {
                format!("{shown} asks for a login, which this window does not give — {browser}")
            }
            Line::Failed(Failure::ClientCertificate, _) => format!(
                "{shown} asks for a client certificate, which this window does not give — \
                 {browser}"
            ),
            Line::Ended => "the page's process ended — Cmd/Ctrl+R reloads".to_string(),
            Line::Refused(to) => format!(
                "{}: a navigation away from {}, refused",
                url::redacted(to),
                url::redacted(&self.origin)
            ),
            Line::Opening(to) => format!("{to}: opening in the system browser…"),
            Line::Opened(to) => format!("{to}: opened in the system browser"),
            Line::NotOpened(words) => words.clone(),
        }
    }

    /// Loads the address again (nothing on screen yet: nothing to reload), else reloads.
    fn again(&mut self) {
        let (committed, path) = (self.committed, self.path.clone());
        if let Some(web) = self.web.as_mut() {
            if committed {
                web.reload();
            } else {
                web.load(&path);
            }
            self.loaded = false;
        }
    }

    /// The window's own keys, after the page took its own: F2, Cmd/Ctrl+R, Esc — each press of
    /// the pass (several may come in one).
    fn take_keys(&mut self, ctx: &egui::Context) {
        let count = |i: &mut egui::InputState, key: Key, mods: Mods| {
            std::iter::from_fn(|| keys::take(i, key, mods).then_some(())).count()
        };
        let (f2, reload, esc) = ctx.input_mut(|i| {
            (
                count(i, Key::F2, Mods::None),
                count(i, Key::R, Mods::Command),
                count(i, Key::Escape, Mods::None),
            )
        });
        for _ in 0..f2 {
            let on = !self.scripts();
            let changed = self
                .web
                .as_mut()
                .is_some_and(|web| web.set_scripts(on) == Ok(true));
            if changed {
                self.loaded = false;
                if !self.committed {
                    // The component reloaded; with nothing on screen that does nothing.
                    let path = self.path.clone();
                    if let Some(web) = self.web.as_mut() {
                        web.load(&path);
                    }
                }
            }
        }
        for _ in 0..reload {
            self.again();
        }
        if esc > 0 {
            crate::gui::request_close(ctx);
        }
    }

    /// The F-keys the row lists besides F1 and F3: F2 names what it does.
    fn fkeys(&self) -> &'static [chrome::FKey] {
        match self.scripts() {
            true => chrome::PAGE_KEYS_ON,
            false => chrome::PAGE_KEYS,
        }
    }

    /// The bar: the document on screen, then — kept whole, at the right — scripts and loading.
    fn bar(&self) -> (String, String) {
        let shown = self
            .document
            .as_deref()
            .map_or_else(|| self.shown.clone(), url::redacted);
        let mut right = vec![match self.scripts() {
            true => "scripts ON",
            false => "scripts off",
        }];
        if self.loading {
            right.push("loading…");
        }
        (shown, right.join(" · "))
    }

    /// The page's frame, its bar and line, on `area`'s grid: the page's rect.
    fn draw(&self, ui: &egui::Ui, area: egui::Rect) -> egui::Rect {
        let p = chrome::PALETTE;
        let grid = Grid::over(area);
        let mut buf = cells::Buffer::empty(grid.all());
        // Double-lined while the page has the keyboard (§2.4), single while sopen has it.
        let active = self.keyboard() == Keyboard::Page;
        let mut page = cells::frame(&mut buf, grid.all(), TITLE, active);
        let text = Pen::new(p.text, p.back);
        let row = |buf: &mut cells::Buffer, page: &mut Cells| -> Option<u16> {
            (page.height > 1).then(|| {
                let y = page.y;
                page.y += 1;
                page.height -= 1;
                cells::fill(buf, Cells::new(page.x, y, page.width, 1), text);
                y
            })
        };
        if let Some(y) = row(&mut buf, &mut page) {
            let (shown, right) = self.bar();
            let room = usize::from(page.width);
            let right_w = cells::width(&right).min(room);
            let end = page.x + page.width;
            let right_pen = Pen::new(if self.scripts() { p.warn } else { p.dim }, p.back);
            cells::put_right(&mut buf, end, y, &cells::fit_start(&right, room), right_pen);
            let left_room = room.saturating_sub(right_w + 2);
            let left = cells::fit_left(&shown, left_room);
            cells::put(&mut buf, page.x, y, &left, left_room, text);
        }
        if let Some(line) = &self.line {
            if let Some(y) = row(&mut buf, &mut page) {
                let words = self.line_text(line);
                let room = usize::from(page.width);
                let pen = match line {
                    Line::Opening(_) | Line::Opened(_) => text,
                    _ => Pen::new(p.error, p.back),
                };
                let words = cells::fit_start(&words, room);
                cells::put(&mut buf, page.x, y, &words, room, pen);
            }
        }
        // The page's own cells are not the frame's: the view is there.
        for y in page.top()..page.bottom() {
            for x in page.left()..page.right() {
                buf[(x, y)].reset();
            }
        }
        let rect = grid.rect(page);
        let painter = ui.painter_at(area);
        // What shows where the page is while it is hidden.
        painter.rect_filled(rect, 0.0, p.back);
        cells::paint(&painter, grid.origin, &buf, None);
        rect
    }
}

impl Window for AddressWindow {
    fn logic(&mut self, ctx: &egui::Context) {
        self.heed_the_input(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Before the pass, should the shell's `logic` not have run first.
        self.heed_the_input(&ctx);
        self.make(&ctx);
        // The last link's answer first: a click out in this pass's events is then not refused
        // for a link that has opened already.
        self.take_answers();
        self.events(&ctx);
        chrome::title_panel(ui, "sopen-address-title", &self.shown);
        crate::gui::own_keys(ui, self.fkeys());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let rect = self.draw(ui, ui.max_rect());
                // Before the frame's keys are read: while the page has the keyboard its keys
                // are its own.
                if let Some(web) = self.web.as_mut() {
                    web.ui(ui, rect);
                }
            });
        self.take_keys(&ctx);
        if self.loading || self.opening > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.painted = true;
    }

    /// On screen once its main frame finished or failed since the last load (§11); a view the
    /// engine refused, at once.
    fn ready(&self) -> bool {
        self.painted && (self.loaded || self.report.borrow().failed.is_some())
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        self.fkeys()
    }
}

/// Shows `url` in the address window until it is closed (09 §7): with `hold`, HOST's half of a
/// tunnel — the cues on the standard output, the input read to its end, which closes the window
/// — else the opened line once it is up. [`Shown::Unavailable`] when the engine refused the view
/// after all: nothing was said, and the caller takes §4's fallback.
pub fn run(url: &Url, hold: bool, hooks: &Hooks) -> anyhow::Result<Shown> {
    let shown = url.shown();
    let input = match hold {
        true => {
            let input = Input::default();
            input.watch(std::io::stdin())?;
            Some(input)
        }
        false => None,
    };
    let said = shown.clone();
    let say = Box::new(move |cue| speak(&mut std::io::stdout(), hold, &said, cue));
    let report: Rc<RefCell<Report>> = Rc::default();
    let window = AddressWindow::new(
        url.clone(),
        say,
        system_browser(),
        input,
        hooks.scripted(),
        report.clone(),
    );
    let ran = crate::gui::run_window(&title(&shown), crate::gui::browser::WINDOW, window, hooks);
    let report = report.borrow();
    if let Some(why) = &report.failed {
        return Ok(Shown::Unavailable(why.clone()));
    }
    ran?;
    report.finish(&mut std::io::stdout(), hold, &shown);
    Ok(Shown::Closed)
}
