//! The page view in the file browser (09 §5–§6, WV-D): a page file shown by its [`Web`] in the
//! preview and in the Enter viewer, its requests answered from its root by a page server of its
//! own (`stream::page`) — on this machine in place, a remote one over the opening's connection.
//!
//! - **Served on a worker, viewed on the frame** ([`Served`], [`embed`]): the preparation resolves
//!   the root (§3.1) and starts the server; the view — `Web::new`, the windows' thread's — is
//!   made at the page's first pass in a window, and made again, on a server of its own, in the
//!   next window after the one it was in went (the terminal's switch takes every view of its
//!   window down, 10 §7). In a terminal no view is made and no engine asked: the page's cells
//!   are left to the terminal's hand-off box (10 §11); its frame and rows are cells either way.
//! - **Rows** (§6): the document on screen (`host:/abs`), `scripts off|ON`, loading, the
//!   requests refused for reaching outside the root and the outside requests blocked; under
//!   them a link not followed (F5 copies it, §3.4), a navigation out cancelled, F2's refusal,
//!   a failure. A preview has none (02 §4.2).
//! - **Keys** (the viewer's): F2 scripts on and off where the engine allows them (§3.3, R1,
//!   R11), F5 the link, Cmd/Ctrl+R reload; Esc and the browser's own keys stay the browser's.
//!   While a texture page has the keyboard it declares a field to the view keys, so its digits
//!   are typed into the page. A preview takes no key (02 §4.4).
//! - **The hand-over** (§6): the shield goes, the preview's budget lifts — a page it cut reloads
//!   once — and the connection stays with the viewer, given back to it, not to the window's
//!   preview session, when its server ends.
//! - **A remote page served again** (its window went): on the connection its last server gave
//!   back; a preview on the window's preview session (06 §13, the one it lent); else — the
//!   connection lost with its server's link, or the preview session's now — connected again on
//!   a worker, a preview's becoming the preview session, a viewer's its own. Only a connection
//!   that cannot be made again leaves the closed line.
//! - **A view that cannot be made** (§4: the engine refused it) says why on its row and to its
//!   host — the browser for the viewer ([`take_failure`]), the preview host for a preview
//!   ([`take_preview_failure`]) — which shows the file's source instead.

use super::{Content, Event, Keyboard, Settings, Unavailable, Web, WindowKey};
use crate::gui::browser::keys::{self, Mods};
use crate::gui::browser::Connect;
use crate::gui::cells::{self, Cells, Grid, Pen};
use crate::gui::{chrome, Embedded, Outcome, Window};
use crate::remote::RemoteFs;
use crate::stream::page::{self as server, Control, Options, Root};
use eframe::egui::{self, Key};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

/// The page viewer's frame title.
const TITLE: &str = "Page Viewer";

/// A connection on its way back.
type Conn = Box<dyn RemoteFs + Send>;

/// Where a remote page's connection is (§3.1 "Channels", 06 §13).
enum Held {
    /// Lent to the server running now. When that ends it goes to the window's preview session
    /// (`Some`: the page is a preview), else back here.
    Serving(Option<Sender<Conn>>),
    /// Back from the server that ended: the next one runs on it.
    Here(Conn),
    /// Not here: lost with its server's link, or the preview session's now.
    Gone,
}

/// A remote page's connection, shared with the give-back of its server ([`options`]).
type Session = Arc<Mutex<Held>>;

fn held(session: &Session) -> std::sync::MutexGuard<'_, Held> {
    session.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The window's preview session as the preview host keeps it (02 §4.4, 06 §13): idle in `conn`,
/// or on its way back from what borrowed it through `back`.
#[derive(Clone)]
pub struct PreviewSession {
    pub conn: Arc<Mutex<Option<Conn>>>,
    pub back: Arc<Mutex<Option<Receiver<Conn>>>>,
}

/// What [`PreviewSession::take`] found.
enum Take {
    Conn(Conn),
    /// On its way back, or the host has it now: look again.
    Wait,
    /// Not there (lost).
    Nothing,
}

impl PreviewSession {
    /// The session, if it is back: through `back` (what the last server gave), else idle in
    /// `conn`. Never waits: the frame looks again.
    fn take(&self) -> Take {
        let mut back = self.back.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rx) = back.take() {
            match rx.try_recv() {
                Ok(conn) => return Take::Conn(conn),
                Err(TryRecvError::Empty) => {
                    *back = Some(rx);
                    return Take::Wait;
                }
                Err(TryRecvError::Disconnected) => {}
            }
        }
        drop(back);
        match self.conn.try_lock() {
            Ok(mut conn) => conn.take().map_or(Take::Nothing, Take::Conn),
            Err(TryLockError::Poisoned(p)) => {
                p.into_inner().take().map_or(Take::Nothing, Take::Conn)
            }
            Err(TryLockError::WouldBlock) => Take::Wait,
        }
    }

    /// Lent to a page's server: it comes back through `back` when that ends.
    fn lend(&self) -> Sender<Conn> {
        let (tx, rx) = mpsc::channel();
        *self.back.lock().unwrap_or_else(PoisonError::into_inner) = Some(rx);
        tx
    }
}

/// Why file pages show their source in `ctx`'s window, if they do (09 §4): no engine on this
/// thread, or one that does not contain a file view with scripts off. In a terminal no engine
/// is asked (it runs only inside a window): there the page is a hand-off box.
pub(crate) fn pages_here(ctx: &egui::Context) -> Result<(), Unavailable> {
    if crate::gui::in_terminal(ctx) {
        return Ok(());
    }
    match super::engine()?.uncontained(false) {
        Some(channel) => Err(Unavailable::NotContained(channel)),
        None => Ok(()),
    }
}

/// The first row of a page file shown as its source (§4): `<why>: showing the source`.
pub fn source_line(why: &Unavailable) -> String {
    format!("{}: showing the source", why.line())
}

fn failure_id() -> egui::Id {
    egui::Id::unique("sopen-page-failed")
}

fn preview_failure_id() -> egui::Id {
    egui::Id::unique("sopen-page-preview-failed")
}

/// The preview's page could not be made this pass, and why (§4): the preview host shows the
/// source in its place. Taken once per pass.
pub(crate) fn take_preview_failure(ctx: &egui::Context) -> Option<Unavailable> {
    ctx.data_mut(|d| {
        let why = d.get_temp::<Unavailable>(preview_failure_id());
        d.remove::<Unavailable>(preview_failure_id());
        why
    })
}

/// The page on screen in the viewer could not be made this pass, and why (§4): the browser
/// shows the source in its place. Taken once per pass.
pub(crate) fn take_failure(ctx: &egui::Context) -> Option<Unavailable> {
    ctx.data_mut(|d| {
        let why = d.get_temp::<Unavailable>(failure_id());
        d.remove::<Unavailable>(failure_id());
        why
    })
}

/// A page made ready on a worker: its server running, its root, where the view starts.
pub struct Served {
    server: server::Page,
    control: Control,
    root: Root,
    /// `[user@]host`; `None` on this machine.
    label: Option<String>,
    session: Session,
    /// How a remote page connects again; a preview's session.
    connect: Option<Connect>,
    preview_session: Option<PreviewSession>,
    preview: bool,
}

impl Served {
    /// A page on this machine (§3.1 "Transport": read in place); `root` as [`server::root`] gave
    /// it. A preview's has the preview budget.
    pub fn local(root: Root, preview: bool) -> Served {
        let (server, control) = server::serve_local(&root.root, options(preview, None));
        Served {
            server,
            control,
            root,
            label: None,
            session: Arc::new(Mutex::new(Held::Gone)),
            connect: None,
            preview_session: None,
            preview,
        }
    }

    /// A remote page over `conn` — the connection that resolved, stat'ed and sniffed the file
    /// (the viewer's, §3.1 "Channels") or, with `preview`, the window's preview session, lent:
    /// it goes back there when the preview's server ends. `connect` connects again (a viewer's
    /// own session, a preview's lost one).
    pub fn remote(
        conn: Conn,
        root: Root,
        label: &str,
        connect: Connect,
        preview: Option<PreviewSession>,
    ) -> Served {
        let to = preview.as_ref().map(PreviewSession::lend);
        let session = Arc::new(Mutex::new(Held::Serving(to)));
        let options = options(preview.is_some(), Some(&session));
        let (server, control) = server::serve(conn, &root.root, options);
        Served {
            server,
            control,
            root,
            label: Some(label.to_string()),
            session,
            connect: Some(connect),
            preview: preview.is_some(),
            preview_session: preview,
        }
    }
}

/// The give-back of a remote page's server: the connection to where [`Held::Serving`] says.
/// Dropped without being called — the server ended without its connection, lost with its link —
/// it marks the connection gone, so the next server connects again.
struct Giving {
    session: Session,
    given: bool,
}

impl Giving {
    fn give(mut self, conn: Conn) {
        self.given = true;
        let mut held = held(&self.session);
        match std::mem::replace(&mut *held, Held::Gone) {
            // The window's preview session: back to it (gone from here).
            Held::Serving(Some(to)) => {
                let _ = to.send(conn);
            }
            Held::Serving(None) | Held::Here(_) | Held::Gone => *held = Held::Here(conn),
        }
    }
}

impl Drop for Giving {
    fn drop(&mut self) {
        if !self.given {
            let mut held = held(&self.session);
            if matches!(*held, Held::Serving(_)) {
                // A preview's lent session is lost: the window's next preview connects again.
                *held = Held::Gone;
            }
        }
    }
}

/// A server's options: the preview's budget or none, the throttle hook, and — remote — the
/// connection given back as `session` says ([`Giving`]).
fn options(preview: bool, session: Option<&Session>) -> Options {
    let mut options = if preview {
        Options::preview()
    } else {
        Options::viewer()
    };
    if let Some(session) = session.cloned() {
        let giving = Giving {
            session,
            given: false,
        };
        options.give_back = Some(Box::new(move |conn| giving.give(conn)));
    }
    options
}

/// The page's window, for the viewer or the preview ([`Window::set_preview`]).
pub fn embed(served: Served) -> Embedded {
    Embedded {
        window: Box::new(PageWindow::new(served)),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

/// What the row under the bar says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// A clicked link out, not followed (§3.4): F5 copies the whole of it.
    Link(String),
    /// F5 copied it.
    Copied,
    /// Another navigation out, cancelled: where it went.
    Went(String),
    /// The main frame failed.
    Failed(String),
    /// The web process ended.
    Ended,
    /// F2's refusal (§3.3).
    Said(String),
    /// The view could not be made (§4).
    Unmade(String),
    /// The page's view closed with its window and its connection could not be made again.
    Closed(String),
}

impl Line {
    fn text(&self) -> String {
        match self {
            Line::Link(url) => format!(
                "{}: link not followed from a file view — F5 copies it",
                crate::url::redacted(url)
            ),
            Line::Copied => "copied — it may carry what the page read".to_string(),
            Line::Went(url) => format!(
                "{}: a navigation out of the page, cancelled",
                crate::url::redacted(url)
            ),
            Line::Failed(why) => format!("the page did not load: {why} — Cmd/Ctrl+R reloads"),
            Line::Ended => "the page's process ended — Cmd/Ctrl+R reloads".to_string(),
            Line::Said(words) | Line::Unmade(words) => words.clone(),
            Line::Closed(why) => format!(
                "the page closed with its window and cannot connect again: {why} — Esc, then Enter opens it"
            ),
        }
    }
}

/// A page in the browser: its view, its server's side, its rows.
pub struct PageWindow {
    /// A server ready for the next view: the one the preparation started, until it is made.
    served: Option<server::Page>,
    /// The current server's side: the refusals and the budget.
    control: Control,
    root: Root,
    label: Option<String>,
    session: Session,
    connect: Option<Connect>,
    preview_session: Option<PreviewSession>,
    /// A connection being made on a worker, for the next server.
    dialing: Option<Receiver<anyhow::Result<Conn>>>,
    /// The view and the window it is in.
    web: Option<(Web, WindowKey)>,
    /// `Web::new` did not make it: why (§4).
    failed: Option<Unavailable>,
    /// The browser was told it since the page was last shown ([`take_failure`]).
    told: bool,
    preview: bool,
    /// The last pass was a terminal's: no view, the page a hand-off box.
    terminal: bool,
    painted: bool,
    /// The main frame finished or failed since the last load.
    loaded: bool,
    loading: bool,
    /// The document on screen, its path under the root.
    document: Option<String>,
    blocked: u32,
    line: Option<Line>,
}

impl PageWindow {
    fn new(served: Served) -> Self {
        PageWindow {
            served: Some(served.server),
            control: served.control,
            root: served.root,
            label: served.label,
            session: served.session,
            connect: served.connect,
            preview_session: served.preview_session,
            dialing: None,
            web: None,
            failed: None,
            told: false,
            preview: served.preview,
            terminal: false,
            painted: false,
            loaded: false,
            loading: false,
            document: None,
            blocked: 0,
            line: None,
        }
    }

    /// The view for `ctx`'s window: none in a terminal; made at the first pass in a window, on a
    /// server of its own, and made again in a window other than the one it was made in (that one
    /// went, and took it down).
    fn view_for(&mut self, ctx: &egui::Context) {
        self.terminal = crate::gui::in_terminal(ctx);
        let key = (!self.terminal).then(|| super::window(ctx));
        if self.web.as_ref().is_some_and(|(_, k)| Some(*k) != key) {
            // Its window went: the view is closed already, and what it said with it.
            self.web = None;
            (self.loaded, self.loading) = (false, false);
            (self.document, self.blocked) = (None, 0);
        }
        let Some(key) = key else {
            return;
        };
        if self.web.is_some() || self.failed.is_some() || self.closed() {
            return;
        }
        let Some(server) = self.served.take().or_else(|| self.serve_again()) else {
            return;
        };
        let settings = Settings {
            preview: self.preview,
        };
        match Web::new(
            Content::Page {
                server: Box::new(server),
            },
            settings,
        ) {
            Ok(mut web) => {
                web.load(&self.root.path);
                self.loaded = false;
                self.web = Some((web, key));
            }
            Err(why) => {
                self.line = Some(Line::Unmade(source_line(&why)));
                self.failed = Some(why);
            }
        }
    }

    /// A server for a view made again: in place on this machine; a remote one once its
    /// connection is here ([`PageWindow::connection`]).
    fn serve_again(&mut self) -> Option<server::Page> {
        if self.label.is_none() {
            let (server, control) =
                server::serve_local(&self.root.root, options(self.preview, None));
            self.control = control;
            return Some(server);
        }
        let conn = self.connection()?;
        let to = match (&self.preview_session, self.preview) {
            (Some(session), true) => Some(session.lend()),
            _ => None,
        };
        *held(&self.session) = Held::Serving(to);
        let options = options(self.preview, Some(&self.session));
        let (server, control) = server::serve(conn, &self.root.root, options);
        self.control = control;
        Some(server)
    }

    /// The connection for a remote page's next server, when it is here: given back by the last
    /// one; a preview's, the window's preview session; else made again on a worker (looked at on
    /// the next passes). `None` while it is on its way, and for good when it cannot be made (the
    /// closed line).
    fn connection(&mut self) -> Option<Conn> {
        if let Some(dialing) = &self.dialing {
            let made = match dialing.try_recv() {
                Ok(made) => made,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("its worker stopped")),
            };
            self.dialing = None;
            return match made {
                Ok(conn) => Some(conn),
                Err(e) => {
                    self.line = Some(Line::Closed(crate::stream::explain(&e)));
                    None
                }
            };
        }
        {
            let mut held = held(&self.session);
            match std::mem::replace(&mut *held, Held::Gone) {
                Held::Here(conn) => return Some(conn),
                // The last server has not ended yet: it gives the connection back.
                serving @ Held::Serving(_) => {
                    *held = serving;
                    return None;
                }
                Held::Gone => {}
            }
        }
        if self.preview {
            match self.preview_session.as_ref().map(PreviewSession::take) {
                Some(Take::Conn(conn)) => return Some(conn),
                Some(Take::Wait) => return None,
                Some(Take::Nothing) | None => {}
            }
        }
        let Some(connect) = self.connect.clone() else {
            self.line = Some(Line::Closed("no way to connect".to_string()));
            return None;
        };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sopen-page-connect".into())
            .spawn(move || {
                let _ = tx.send(connect());
            });
        match spawned {
            Ok(_) => self.dialing = Some(rx),
            Err(e) => self.line = Some(Line::Closed(e.to_string())),
        }
        None
    }

    fn closed(&self) -> bool {
        matches!(self.line, Some(Line::Closed(_)))
    }

    fn web(&mut self) -> Option<&mut Web> {
        self.web.as_mut().map(|(web, _)| web)
    }

    fn scripts(&self) -> bool {
        self.web.as_ref().is_some_and(|(web, _)| web.scripts())
    }

    fn keyboard(&self) -> Keyboard {
        self.web
            .as_ref()
            .map_or(Keyboard::Sopen, |(web, _)| web.keyboard())
    }

    /// What the view said since the last pass.
    fn events(&mut self) {
        let Some(web) = self.web() else {
            return;
        };
        for event in web.events() {
            match event {
                Event::Loading => {
                    (self.loading, self.loaded) = (true, false);
                    if matches!(self.line, Some(Line::Failed(_) | Line::Ended)) {
                        self.line = None;
                    }
                }
                Event::Finished => (self.loading, self.loaded) = (false, true),
                Event::Failed(why) => {
                    (self.loading, self.loaded) = (false, true);
                    self.line = Some(Line::Failed(why));
                }
                Event::Document(path) => self.document = Some(path),
                Event::Refused { target, clicked } => {
                    self.line = Some(if clicked {
                        Line::Link(target)
                    } else {
                        Line::Went(target)
                    });
                }
                Event::Blocked(n) => self.blocked = n,
                Event::ProcessEnded => {
                    (self.loading, self.loaded) = (false, true);
                    self.line = Some(Line::Ended);
                }
            }
        }
    }

    /// The viewer's own keys: F2, F5, Cmd/Ctrl+R.
    fn keys(&mut self, ctx: &egui::Context) {
        let (f2, f5, reload) = ctx.input_mut(|i| {
            (
                keys::take(i, Key::F2, Mods::None),
                keys::take(i, Key::F5, Mods::None),
                keys::take(i, Key::R, Mods::Command),
            )
        });
        if f2 {
            let on = !self.scripts();
            if let Some(web) = self.web() {
                match web.set_scripts(on) {
                    Ok(true) => self.loaded = false,
                    Ok(false) => {}
                    Err(words) => self.line = Some(Line::Said(words)),
                }
            }
        }
        if f5 {
            if let Some(Line::Link(url)) = &self.line {
                ctx.copy_text(url.clone());
                self.line = Some(Line::Copied);
            }
        }
        if reload {
            if let Some(web) = self.web() {
                web.reload();
                self.loaded = false;
            }
        }
    }

    /// The bar: the document on screen, then — kept whole, at the right — scripts, loading and
    /// the counts.
    fn bar(&self) -> (String, String) {
        let path = self.document.as_deref().unwrap_or(&self.root.path);
        let abs = match self.root.root.as_str() {
            "/" => format!("/{path}"),
            root => format!("{root}/{path}"),
        };
        let shown = match &self.label {
            Some(label) => format!("{label}:{abs}"),
            None => abs,
        };
        let mut right = vec![if self.scripts() {
            "scripts ON".to_string()
        } else {
            "scripts off".to_string()
        }];
        if self.loading {
            right.push("loading…".to_string());
        }
        let refused = self.control.refused();
        if refused > 0 {
            right.push(format!("{refused} refused"));
        }
        if self.blocked > 0 {
            let s = if self.blocked == 1 { "" } else { "s" };
            right.push(format!("{} outside request{s} blocked", self.blocked));
        }
        (shown, right.join(" · "))
    }

    /// The frame, the rows and the page's cells, laid out on `area`'s grid: the page's rect.
    fn draw(&self, ui: &egui::Ui, area: egui::Rect) -> egui::Rect {
        let p = chrome::PALETTE;
        let grid = Grid::over(area);
        let mut buf = cells::Buffer::empty(grid.all());
        // Double-lined while the page has the keyboard (§2.4), single while sopen has it.
        let active = !self.preview && self.keyboard() == Keyboard::Page;
        let inside = cells::frame(&mut buf, grid.all(), TITLE, active);
        let mut page = inside;
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
        if !self.preview {
            if let Some(y) = row(&mut buf, &mut page) {
                let (shown, right) = self.bar();
                let room = usize::from(page.width);
                let right_w = cells::width(&right).min(room);
                let end = page.x + page.width;
                let warn = self.scripts();
                let right_pen = Pen::new(if warn { p.warn } else { p.dim }, p.back);
                cells::put_right(&mut buf, end, y, &cells::fit_start(&right, room), right_pen);
                let left_room = room.saturating_sub(right_w + 2);
                let left = cells::fit_left(&shown, left_room);
                cells::put(&mut buf, page.x, y, &left, left_room, text);
            }
        }
        if let Some(line) = &self.line {
            if let Some(y) = row(&mut buf, &mut page) {
                let words = line.text();
                let room = usize::from(page.width);
                let pen = match line {
                    Line::Copied => text,
                    _ => Pen::new(p.error, p.back),
                };
                cells::put(
                    &mut buf,
                    page.x,
                    y,
                    &cells::fit_start(&words, room),
                    room,
                    pen,
                );
            }
        }
        // The page's own cells are not the frame's: a terminal shows its hand-off box there.
        for y in page.top()..page.bottom() {
            for x in page.left()..page.right() {
                buf[(x, y)].reset();
            }
        }
        let rect = grid.rect(page);
        let painter = ui.painter_at(area);
        // What shows where the page is while it is hidden, or not made.
        painter.rect_filled(rect, 0.0, p.back);
        cells::paint(&painter, grid.origin, &buf, None);
        rect
    }
}

impl Window for PageWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.view_for(&ctx);
        self.events();
        let area = ui.max_rect();
        let rect = self.draw(ui, area);
        // Before the frame's keys are read: while the page has the keyboard its keys are its own.
        if let Some(web) = self.web() {
            web.ui(ui, rect);
        }
        if !self.preview {
            self.keys(&ctx);
        }
        if let Some(why) = self.failed.as_ref().filter(|_| !self.told) {
            // Its host shows the source in its place: the browser's viewer, the preview host.
            let id = match self.preview {
                false => failure_id(),
                true => preview_failure_id(),
            };
            ctx.data_mut(|d| d.insert_temp(id, why.clone()));
            self.told = true;
        }
        let waiting = self.web.is_none() && self.failed.is_none() && !self.terminal;
        if self.loading || (waiting && !self.closed()) {
            // Its rows follow the load; a view waiting for its connection looks again.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.painted = true;
    }

    /// On screen once its main frame finished or failed since the last load (§11); in a
    /// terminal, or when it could not be made, once drawn.
    fn ready(&self) -> bool {
        self.painted
            && match &self.web {
                Some(_) => self.loaded,
                None => self.terminal || self.failed.is_some() || self.closed(),
            }
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        match (self.scripts(), matches!(self.line, Some(Line::Link(_)))) {
            (false, false) => chrome::PAGE_KEYS,
            (true, false) => chrome::PAGE_KEYS_ON,
            (false, true) => chrome::PAGE_LINK_KEYS,
            (true, true) => chrome::PAGE_LINK_KEYS_ON,
        }
    }

    /// The preview, or the hand-over (§6, §3.1): the shield goes, the budget lifts — a page it
    /// cut loads again, whole — and a remote page's connection is the viewer's from now on.
    fn set_preview(&mut self, on: bool) {
        let handed_over = std::mem::replace(&mut self.preview, on) && !on;
        if handed_over {
            let cut = self.control.lift();
            // The connection is the viewer's from now on: its server gives it back here. One the
            // preview session has back already is gone from here — connected again, if needed.
            let mut held = held(&self.session);
            if let Held::Serving(to) = &mut *held {
                *to = None;
            }
            drop(held);
            if let Some(web) = self.web() {
                web.set_preview(false);
                if cut {
                    web.reload();
                    self.loaded = false;
                }
            }
        } else if let Some(web) = self.web() {
            web.set_preview(on);
        }
    }

    /// Hidden among the viewer's views (08 §5): shows nothing, takes no input, plays nothing.
    fn set_shown(&mut self, shown: bool) {
        // Shown again, a view that could not be made says so again.
        self.told &= !shown;
        if let Some(web) = self.web() {
            web.set_shown(shown);
        }
    }

    /// While a page drawn as a texture has the keyboard, digits are typed into it (08 §4).
    fn view_state(&mut self) -> crate::view_keys::State {
        match self.keyboard() {
            Keyboard::Page => crate::view_keys::State::Field,
            Keyboard::Sopen => crate::view_keys::State::Free,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fix-check of WV-D (review MF1 c): a server that ends without its connection — lost
    /// with its link — drops its give-back unused, and the connection is gone, so the next
    /// server connects again; one called gives it back here, or a preview's to the window's
    /// preview session.
    #[test]
    fn wv_d_a_lost_link_marks_the_connection_gone() {
        let conn = || Box::new(crate::remote::LocalFs::new()) as Conn;
        let session: Session = Arc::new(Mutex::new(Held::Serving(None)));
        drop(options(false, Some(&session)));
        assert!(matches!(*held(&session), Held::Gone), "lost");
        *held(&session) = Held::Serving(None);
        let give = options(false, Some(&session)).give_back.expect("remote");
        give(conn());
        assert!(matches!(*held(&session), Held::Here(_)), "back here");
        let (tx, rx) = mpsc::channel();
        *held(&session) = Held::Serving(Some(tx));
        let give = options(true, Some(&session)).give_back.expect("remote");
        give(conn());
        assert!(
            matches!(*held(&session), Held::Gone),
            "the preview session's"
        );
        assert!(rx.try_recv().is_ok(), "sent to it");
        let (tx, rx) = mpsc::channel();
        *held(&session) = Held::Serving(Some(tx));
        drop(options(true, Some(&session)));
        assert!(matches!(*held(&session), Held::Gone));
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Disconnected)),
            "the preview session's next preparation connects again"
        );
    }
}
