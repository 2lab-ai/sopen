//! The page view (09 §2): [`Web`], the one thing a screen hosts to show a page — a file's, served
//! through the view's own `sopen-page` scheme ([`crate::scheme`]), or an address's — and the
//! [`Engine`] trait its two backends implement: WKWebView on macOS, a native view over the window
//! (WV-A), and WebKitGTK on Linux, offscreen and painted as a texture (WV-B). Everything here is
//! engine-independent and runs on the test kit with a recording fake engine (`fake`). The file
//! browser hosts one for a page file's preview and viewer ([`page`], WV-D); the address window is
//! WV-E2's.
//!
//! - **The engine** is picked at the first [`Web::new`] of the windows' thread — the process's
//!   main thread, so its pick is the process's; without one `Web::new` returns why
//!   ([`Unavailable`]) and the host shows §4's fallback. `SOPEN_WEB=off` forces that reason.
//! - **Placement**: [`Web::ui`] says each frame where the page is. After the frame — the window's
//!   pass and the shell's after-pass painters, F1's help among them — the shell's step
//!   ([`after_pass`]) tells the engine only what changed: a view drawn for the first time is put
//!   into its window ([`View::attach`], with the window's handle the shell gave,
//!   [`set_parent`]), its frame and scale on change ([`View::place`]), and whether it shows
//!   ([`View::set_hidden`]): a view the frame did not place is hidden, and a native one also
//!   while anything egui painted above the page's own layer meets its rect (§2.2) — decided
//!   from egui's paint order and the painted rects, never from a list of dialogs. A texture
//!   (Linux) is egui content: egui draws over it, nothing hides it.
//! - **Keys** (§2.4): sopen has the keyboard or the page has it ([`Keyboard`]); [`route`] says
//!   where a key goes in each, for the macOS key monitor and for Linux's forwarding alike. On
//!   macOS the state is the view's own word each frame (the first responder); on Linux it is
//!   this component's: a click in the page gives it the keyboard; a click elsewhere, Esc (with
//!   its repeats) and the other keys that are always sopen's, F12 (the switch), a dialog over
//!   it, a frame that does not draw it and being held unshown take it back.
//! - **Scripts** (§3.3): a file view starts with them off and the preview keeps them off; F2
//!   turns them on only where the backend contains every channel and only on a WebKit version
//!   that passed WV-S2 ([`scripts_available`]); an address window starts with them on.
//! - **Teardown** ([`teardown`]): every view of a window closes before the window goes. The
//!   macOS key monitor is installed with the first view put into a window and removed with the
//!   last ([`Engine::monitor`]).
//! - **Receipts**: the screenshot hook asks every shown native view for its picture
//!   ([`snapshots`]) before the read-back, which has an empty rect where such a page is, and
//!   draws each at its frame ([`compose`]).

#[cfg(test)]
pub(crate) mod fake;
#[cfg(target_os = "linux")]
mod gtk;
#[cfg(target_os = "macos")]
mod mac;
pub(crate) mod page;
#[cfg(test)]
mod tests;

use crate::gui::browser::keys::Mods;
use crate::scheme::{self, Host};
use eframe::egui;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};

/// The reason `SOPEN_WEB=off` gives (§4).
pub const OFF: &str = "turned off (SOPEN_WEB=off)";

/// Why there is no page view here (§4): the host shows the file's source with
/// [`Unavailable::line`] first (`…: showing the source`), or an address opens in the system
/// browser after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// No engine: the library is missing or did not start, or [`OFF`].
    NoEngine(String),
    /// The engine is here but does not contain a file view with scripts off (§3.3, R10): the
    /// channel. An address window is not affected.
    NotContained(String),
}

impl Unavailable {
    /// `no web engine here (<reason>)`, `the page view is not contained here (<channel>)`.
    pub fn line(&self) -> String {
        match self {
            Unavailable::NoEngine(why) => format!("no web engine here ({why})"),
            Unavailable::NotContained(channel) => {
                format!("the page view is not contained here ({channel})")
            }
        }
    }
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line())
    }
}

impl std::error::Error for Unavailable {}

/// What a view shows.
pub enum Content {
    /// A file's page, served by `server` through the view's own scheme (§3).
    Page { server: Box<dyn scheme::Server> },
    /// An address window's page (§7): `origin` is `scheme://host[:port]`; no `sopen-page` scheme.
    Address { origin: String },
}

/// How a view starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The browser's preview pane (§6): scripts off whatever is asked, nothing reaches the page.
    pub preview: bool,
}

/// What the engine gets to open a view.
pub enum Opened {
    /// A file's page: register `host`'s scheme for this view only, served by `server`.
    Page {
        host: Host,
        server: Box<dyn scheme::Server>,
    },
    /// An address window's page.
    Address { origin: String },
}

/// [`Engine::open`]'s arguments.
pub struct Opening {
    pub content: Opened,
    pub preview: bool,
    /// Scripts on at the start (an address window's; a file view starts off).
    pub scripts: bool,
    /// Where the view's events go.
    pub events: Events,
}

/// What a view says back (§2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The main frame started loading.
    Loading,
    /// The main frame finished.
    Finished,
    /// The main frame failed: why.
    Failed(String),
    /// The document on screen: its path under the root (a file view) or its address.
    Document(String),
    /// A navigation or request out was refused: its target; `clicked` for a link the owner
    /// clicked (a file view's row shows it and F5 copies it, §3.4).
    Refused { target: String, clicked: bool },
    /// Outside requests blocked so far (the row's count, §3.3).
    Blocked(u32),
    /// The web process ended.
    ProcessEnded,
}

/// The sender an engine's view says its [`Event`]s through, from any thread: each one wakes the
/// window it is drawn in.
#[derive(Clone)]
pub struct Events {
    tx: mpsc::Sender<Event>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl Events {
    fn new() -> (Self, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        (
            Events {
                tx,
                ctx: Arc::default(),
            },
            rx,
        )
    }

    pub fn send(&self, event: Event) {
        // A view whose `Web` is gone says nothing to anyone.
        if self.tx.send(event).is_ok() {
            if let Some(ctx) = self
                .ctx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                ctx.request_repaint();
            }
        }
    }

    /// From now on an event wakes `ctx`'s window (the one the view is drawn in).
    fn wake(&self, ctx: &egui::Context) {
        let mut woken = self.ctx.lock().unwrap_or_else(PoisonError::into_inner);
        if woken.is_none() {
            *woken = Some(ctx.clone());
        }
    }
}

/// How an engine's view reaches the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A view of the platform's over the window (macOS): egui cannot draw over it, so it is
    /// hidden while something egui painted is above it; it says itself who has the keyboard.
    Native,
    /// A picture egui paints (Linux): drawn over like any other; sopen forwards its input.
    Texture,
}

/// The window a view goes into, as the shell got it from eframe (`Shell`'s handle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    /// macOS: winit's NSView (`RawWindowHandle::AppKit`).
    AppKit(NonNull<c_void>),
    /// A window without a native view to give (a texture engine's, the test kit's).
    Window,
}

/// A backend: WKWebView (WV-A), WebKitGTK (WV-B), or the test kit's fake. Called on the windows'
/// thread.
pub trait Engine {
    fn placement(&self) -> Placement;
    /// The engine's version as read when it loaded (WebKitGTK's major.minor.micro, the macOS
    /// WebKit.framework bundle version), compared with [`PASSED`] (§3.3, R11).
    fn version(&self) -> String;
    /// A channel this backend does not contain in a file view (WV-S2): with scripts off
    /// (`false`; R10: file views show the source) or on (`true`; R1: F2 is off); `None` when
    /// every channel is contained.
    fn uncontained(&self, scripts: bool) -> Option<String>;
    /// A new view, not in a window yet and hidden.
    fn open(&self, opening: Opening) -> Result<Box<dyn View>, String>;
    /// The macOS key monitor on (with the first view in a window) or off (with the last).
    fn monitor(&self, on: bool);
}

/// One view of an [`Engine`]. Called on the windows' thread, never after [`View::close`].
pub trait View {
    /// Into its window (macOS: a subview of winit's NSView), the first frame it is drawn.
    fn attach(&mut self, parent: Parent);
    /// Its rect in points and the window's scale (pixels per point), when either changed.
    fn place(&mut self, rect: egui::Rect, scale: f32);
    /// Hidden for this frame or shown again (macOS `setHidden`).
    fn set_hidden(&mut self, hidden: bool);
    /// A texture engine paints its page at `rect` (clipped to it) in the frame's pass.
    fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let _ = (painter, rect);
    }
    /// A texture engine's input, forwarded while the pointer is over the page or the page has
    /// the keyboard: egui's event, positions relative to the page's rect, in points.
    fn input(&mut self, event: &egui::Event) {
        let _ = event;
    }
    /// A native engine: whether the page has the keyboard (macOS: WebKit is first responder).
    fn page_has_keyboard(&self) -> bool {
        false
    }
    /// A native engine: sopen takes the keyboard back (macOS: winit's view becomes first
    /// responder) — before the view hides, so a dialog over it gets its keys, and when it
    /// becomes a preview.
    fn take_keyboard(&mut self) {}
    fn load(&mut self, url: &str);
    fn reload(&mut self);
    /// Scripts on or off from the next load (the component reloads).
    fn set_scripts(&mut self, on: bool);
    /// The preview (`true`: the shield over the page, never first responder) or the viewer.
    fn set_preview(&mut self, on: bool);
    /// Shown, or held without being drawn: paused (media), no input.
    fn set_shown(&mut self, shown: bool);
    /// The page's picture as it is on screen, for the screenshot hook; `None` when there is
    /// none to take. `done` may be called later, on the windows' thread.
    fn snapshot(&mut self, done: Box<dyn FnOnce(Option<egui::ColorImage>)>);
    /// Gone: out of its window, released, its processes ended (§2.5).
    fn close(&mut self);
}

/// The WebKit versions whose file-view scripts WV-S2 found contained (§3.3, R11): none yet, so
/// F2 is off in file views on every engine.
pub const PASSED: &[&str] = &[];

/// Whether F2 may turn a file view's scripts on with `engine` (§3.3): `Err` with the line it
/// says when not.
pub fn scripts_available(engine: &dyn Engine) -> Result<(), String> {
    scripts_check(
        &engine.version(),
        PASSED,
        engine.uncontained(true).as_deref(),
    )
}

/// [`scripts_available`]'s rule: the backend's own word first (R1), then the version (R11).
pub(crate) fn scripts_check(
    version: &str,
    passed: &[&str],
    uncontained: Option<&str>,
) -> Result<(), String> {
    if let Some(channel) = uncontained {
        return Err(format!(
            "scripts are not available here: {channel} is not contained"
        ));
    }
    if !passed.contains(&version) {
        return Err(format!(
            "scripts are not available with this WebKit ({version}): not yet checked"
        ));
    }
    Ok(())
}

/// Who has the keyboard (§2.4): the page's frame is single-lined while sopen has it,
/// double-lined while the page has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyboard {
    Sopen,
    Page,
}

/// Where a key goes ([`route`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// The page's own (typing, Tab, Cmd/Ctrl+C/V/A).
    Page,
    /// sopen's, which has the keyboard already.
    Sopen,
    /// Esc while the page has the keyboard: the keyboard comes back, and that is all (the key
    /// is consumed).
    TakeBack,
    /// Another key that is always sopen's, while the page has the keyboard: it comes back and
    /// sopen acts on the key.
    TakeBackAndAct,
}

/// Where `key` with `modifiers` goes while `keyboard` has it (§2.4). Always sopen's: Esc, F1–F12,
/// Cmd/Ctrl+W, Cmd/Ctrl+Q and Cmd/Ctrl+1…0 (04 §6.1's F-key aliases).
pub fn route(keyboard: Keyboard, key: egui::Key, modifiers: egui::Modifiers) -> Route {
    match keyboard {
        Keyboard::Sopen => Route::Sopen,
        Keyboard::Page if key == egui::Key::Escape => Route::TakeBack,
        Keyboard::Page if always_sopens(key, modifiers) => Route::TakeBackAndAct,
        Keyboard::Page => Route::Page,
    }
}

/// F1–F12 with any modifiers (Shift+F-keys are sopen's file keys too), and Cmd/Ctrl alone with W,
/// Q or a digit.
fn always_sopens(key: egui::Key, modifiers: egui::Modifiers) -> bool {
    use egui::Key::*;
    let fkey = matches!(
        key,
        F1 | F2 | F3 | F4 | F5 | F6 | F7 | F8 | F9 | F10 | F11 | F12
    );
    let command = matches!(
        key,
        W | Q | Num1 | Num2 | Num3 | Num4 | Num5 | Num6 | Num7 | Num8 | Num9 | Num0
    ) && Mods::Command.matches(modifiers);
    fkey || command
}

/// The window a view is drawn in: a key the window's context carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowKey(u64);

/// A view as the window's side holds it: the engine's view and what it was told.
struct Slot {
    view: Box<dyn View>,
    engine: Rc<dyn Engine>,
    window: Option<WindowKey>,
    attached: bool,
    /// The rect and scale last told ([`View::place`]).
    told: Option<(egui::Rect, f32)>,
    /// What [`View::set_hidden`] last said; a new view is hidden.
    hidden: bool,
    /// Where this pass drew it.
    placed: Option<Placed>,
    shown: bool,
    /// A texture engine's keyboard state (a native engine says its own): here, so that the
    /// after-frame step takes it back from a view not drawn.
    keyboard: Keyboard,
    closed: bool,
}

#[derive(Clone, Copy, Debug)]
struct Placed {
    pass: u64,
    layer: egui::LayerId,
    rect: egui::Rect,
    scale: f32,
}

thread_local! {
    /// The engine this thread picked at its first `Web::new` ([`engine`]).
    static ENGINE: RefCell<Option<Result<Rc<dyn Engine>, Unavailable>>> =
        const { RefCell::new(None) };
    /// Every view not dropped yet.
    static VIEWS: RefCell<Vec<Weak<RefCell<Slot>>>> = const { RefCell::new(Vec::new()) };
    /// Each window's handle, as its shell gave it.
    static PARENTS: RefCell<HashMap<WindowKey, Parent>> = RefCell::new(HashMap::new());
    /// Views put into a window and not closed: the key monitor is on while there is one.
    static ATTACHED: Cell<usize> = const { Cell::new(0) };
}

/// A view goes into a window: the first one turns the key monitor on.
fn attached(engine: &dyn Engine) {
    let before = ATTACHED.with(|n| n.replace(n.get() + 1));
    if before == 0 {
        engine.monitor(true);
    }
}

/// A view in a window closed: the last one turns the key monitor off.
fn detached(engine: &dyn Engine) {
    let after = ATTACHED.with(|n| {
        n.set(n.get().saturating_sub(1));
        n.get()
    });
    if after == 0 {
        engine.monitor(false);
    }
}

/// Every view not dropped yet, of the window `key` (`None`: every one).
fn slots_of(key: Option<WindowKey>) -> Vec<Rc<RefCell<Slot>>> {
    VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        views.retain(|v| v.strong_count() > 0);
        views
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|slot| {
                key.is_none()
                    || slot
                        .try_borrow()
                        .is_ok_and(|s| !s.closed && s.window == key)
            })
            .collect()
    })
}

/// The view closes, once: the engine is told and the key monitor counts it out.
fn close_slot(slot: &RefCell<Slot>) {
    let (engine, was_attached) = {
        let mut s = slot.borrow_mut();
        if s.closed {
            return;
        }
        s.closed = true;
        // macOS: winit's view is first responder again before the page's view goes.
        if s.view.page_has_keyboard() {
            s.view.take_keyboard();
        }
        s.view.close();
        (s.engine.clone(), std::mem::take(&mut s.attached))
    };
    if was_attached {
        detached(&*engine);
    }
}

/// This thread's engine, picked once ([`choose`]).
pub fn engine() -> Result<Rc<dyn Engine>, Unavailable> {
    ENGINE.with(|engine| {
        engine
            .borrow_mut()
            .get_or_insert_with(|| choose(std::env::var("SOPEN_WEB").ok().as_deref(), platform))
            .clone()
    })
}

/// The pick: [`OFF`] when `sopen_web` is `off`; else what `load` gives, its error as the reason.
/// Any other value of `SOPEN_WEB` is ignored.
pub(crate) fn choose(
    sopen_web: Option<&str>,
    load: impl FnOnce() -> Result<Rc<dyn Engine>, String>,
) -> Result<Rc<dyn Engine>, Unavailable> {
    match sopen_web {
        Some("off") => Err(Unavailable::NoEngine(OFF.to_string())),
        _ => load().map_err(Unavailable::NoEngine),
    }
}

/// The platform's engine: WKWebView on macOS (WV-A).
#[cfg(target_os = "macos")]
fn platform() -> Result<Rc<dyn Engine>, String> {
    mac::engine()
}

/// The platform's engine: WebKitGTK on Linux, loaded at run time (WV-B).
#[cfg(target_os = "linux")]
fn platform() -> Result<Rc<dyn Engine>, String> {
    gtk::engine()
}

/// The platform's engine: none in this build.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform() -> Result<Rc<dyn Engine>, String> {
    Err("not in this build".to_string())
}

/// A test thread's pick, before its first `Web::new` (the fake engine, or a reason).
#[cfg(test)]
pub(crate) fn use_engine(engine: Result<Rc<dyn Engine>, Unavailable>) {
    ENGINE.with(|e| *e.borrow_mut() = Some(engine));
}

/// A page view.
pub struct Web {
    slot: Rc<RefCell<Slot>>,
    engine: Rc<dyn Engine>,
    /// A file page's scheme host; `None` for an address.
    host: Option<Host>,
    origin: Option<String>,
    events: mpsc::Receiver<Event>,
    wake: Events,
    scripts: bool,
    preview: bool,
    /// The Esc that took the keyboard back is held: its repeats and its release are its own.
    esc_held: bool,
    id: egui::Id,
}

impl Web {
    /// A new view of `content`, hidden until drawn; why not, when this process has no engine,
    /// its file views are not contained, or the engine could not open it.
    pub fn new(content: Content, settings: Settings) -> Result<Self, Unavailable> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let engine = engine()?;
        let (events, received) = Events::new();
        let (opened, host, origin, scripts) = match content {
            Content::Page { server } => {
                if let Some(channel) = engine.uncontained(false) {
                    return Err(Unavailable::NotContained(channel));
                }
                let host = Host::new();
                let opened = Opened::Page {
                    host: host.clone(),
                    server,
                };
                (opened, Some(host), None, false)
            }
            Content::Address { origin } => {
                let opened = Opened::Address {
                    origin: origin.clone(),
                };
                (opened, None, Some(origin), !settings.preview)
            }
        };
        let view = engine
            .open(Opening {
                content: opened,
                preview: settings.preview,
                scripts,
                events: events.clone(),
            })
            .map_err(Unavailable::NoEngine)?;
        let slot = Rc::new(RefCell::new(Slot {
            view,
            engine: engine.clone(),
            window: None,
            attached: false,
            told: None,
            hidden: true,
            placed: None,
            shown: true,
            keyboard: Keyboard::Sopen,
            closed: false,
        }));
        VIEWS.with(|views| {
            let mut views = views.borrow_mut();
            views.retain(|v| v.strong_count() > 0);
            views.push(Rc::downgrade(&slot));
        });
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Ok(Web {
            slot,
            engine,
            host,
            origin,
            events: received,
            wake: events,
            scripts,
            preview: settings.preview,
            esc_held: false,
            id: egui::Id::unique(("sopen-web", n)),
        })
    }

    /// The page this frame at `rect` (points): call it every frame the page is on screen, before
    /// reading the frame's keys — while the page has the keyboard its keys are taken off them.
    pub fn ui(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let ctx = ui.ctx().clone();
        let key = window(&ctx);
        let shown = {
            let mut slot = self.slot.borrow_mut();
            if slot.closed {
                return;
            }
            slot.window = Some(key);
            slot.placed = Some(Placed {
                pass: ctx.cumulative_pass_nr(),
                layer: ui.layer_id(),
                rect,
                scale: ctx.pixels_per_point(),
            });
            // F12 is taken before the pass (the shell's input hook): no key of it is routed.
            if !slot.shown || crate::gui::switch_asked(&ctx) {
                slot.keyboard = Keyboard::Sopen;
            }
            slot.shown
        };
        self.wake.wake(&ctx);
        // Held unshown: no input, nothing painted.
        if shown && self.engine.placement() == Placement::Texture {
            self.forward(ui, rect);
            if self.keyboard() == Keyboard::Page {
                ime_for_page(ui, rect);
            }
            let painter = ui.painter_at(rect);
            self.slot.borrow_mut().view.paint(&painter, rect);
        }
    }

    /// A texture's input (§2.3, §2.4): pointer events over the page go to it, positions in its
    /// own points; a press in it gives it the keyboard, a press elsewhere takes it back; while it
    /// has the keyboard its keys, typing and clipboard events go to it and are taken off the
    /// window's input, Esc only takes the keyboard back, the other always-sopen keys take it back
    /// and stay the window's. Nothing in a preview.
    fn forward(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if self.preview {
            return;
        }
        // Under a dialog the keys are the dialog's: the keyboard comes back, nothing goes on.
        if !ui.ctx().memory(|m| m.allows_interaction(ui.layer_id())) {
            self.slot.borrow_mut().keyboard = Keyboard::Sopen;
            return;
        }
        let over = ui
            .interact(rect, self.id, egui::Sense::hover())
            .contains_pointer();
        let origin = rect.min.to_vec2();
        let mut keyboard = self.slot.borrow().keyboard;
        let mut esc_held = self.esc_held;
        let mut out: Vec<egui::Event> = vec![];
        ui.input_mut(|i| {
            i.events.retain(|event| match event {
                egui::Event::PointerButton { pressed, .. } => {
                    if *pressed {
                        keyboard = if over {
                            Keyboard::Page
                        } else {
                            Keyboard::Sopen
                        };
                    }
                    if over {
                        out.push(in_page(event, origin));
                    }
                    true
                }
                egui::Event::PointerMoved(_) | egui::Event::MouseWheel { .. } => {
                    if over {
                        out.push(in_page(event, origin));
                    }
                    true
                }
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } => {
                    if *key == egui::Key::Escape && esc_held {
                        // The take-back's own repeats and release; a new press means its
                        // release was missed (the page was not drawn then).
                        if *repeat || !*pressed {
                            esc_held = *pressed;
                            return false;
                        }
                        esc_held = false;
                    }
                    match route(keyboard, *key, *modifiers) {
                        Route::Page => {
                            out.push(event.clone());
                            false
                        }
                        Route::TakeBack => {
                            if *pressed {
                                keyboard = Keyboard::Sopen;
                                esc_held = true;
                            }
                            false
                        }
                        Route::TakeBackAndAct => {
                            keyboard = Keyboard::Sopen;
                            true
                        }
                        Route::Sopen => true,
                    }
                }
                egui::Event::Text(_)
                | egui::Event::Copy
                | egui::Event::Cut
                | egui::Event::Paste(_)
                | egui::Event::Ime(_)
                    if keyboard == Keyboard::Page =>
                {
                    out.push(event.clone());
                    false
                }
                _ => true,
            });
        });
        self.esc_held = esc_held;
        let mut slot = self.slot.borrow_mut();
        slot.keyboard = keyboard;
        for event in &out {
            slot.view.input(event);
        }
    }

    /// Opens `path` — a file view: a path under its root; an address window: a path (and query)
    /// on its origin.
    pub fn load(&mut self, path: &str) {
        let url = match (&self.host, &self.origin) {
            (Some(host), _) => host.url(path),
            (None, Some(origin)) => {
                let origin = origin.trim_end_matches('/');
                format!("{origin}/{}", path.trim_start_matches('/'))
            }
            (None, None) => return,
        };
        self.with_view(|view| view.load(&url));
    }

    pub fn reload(&mut self) {
        self.with_view(|view| view.reload());
    }

    /// `f` on the engine's view, unless it closed.
    fn with_view(&self, f: impl FnOnce(&mut dyn View)) {
        let mut slot = self.slot.borrow_mut();
        if !slot.closed {
            f(&mut *slot.view);
        }
    }

    fn closed(&self) -> bool {
        self.slot.borrow().closed
    }

    /// Whether scripts are on.
    pub fn scripts(&self) -> bool {
        self.scripts
    }

    /// F2 (§3.3): scripts on or off, reloading the document on screen. `Ok(true)` when it
    /// changed, `Ok(false)` when there was nothing to do (the preview; already so), `Err` with
    /// the line F2 says when a file view's scripts are not available.
    pub fn set_scripts(&mut self, on: bool) -> Result<bool, String> {
        if self.closed() || on == self.scripts || self.preview {
            return Ok(false);
        }
        if on && self.host.is_some() {
            scripts_available(&*self.engine)?;
        }
        self.scripts = on;
        self.with_view(|view| {
            view.set_scripts(on);
            view.reload();
        });
        Ok(true)
    }

    /// The preview (`true`) or the viewer (`false`: the hand-over, §6).
    pub fn set_preview(&mut self, on: bool) {
        if self.closed() {
            return;
        }
        if on {
            self.slot.borrow_mut().keyboard = Keyboard::Sopen;
            self.with_view(|view| {
                if view.page_has_keyboard() {
                    view.take_keyboard();
                }
            });
            if self.scripts {
                self.scripts = false;
                self.with_view(|view| {
                    view.set_scripts(false);
                    view.reload();
                });
            }
        }
        self.preview = on;
        self.with_view(|view| view.set_preview(on));
    }

    /// On screen, or held without being drawn (a hidden page view shows nothing, takes no
    /// input and plays nothing).
    pub fn set_shown(&mut self, shown: bool) {
        let mut slot = self.slot.borrow_mut();
        if !slot.closed {
            if !shown {
                slot.keyboard = Keyboard::Sopen;
                if slot.view.page_has_keyboard() {
                    slot.view.take_keyboard();
                }
            }
            slot.shown = shown;
            slot.view.set_shown(shown);
        }
    }

    /// Who has the keyboard.
    pub fn keyboard(&self) -> Keyboard {
        let slot = self.slot.borrow();
        if slot.closed || self.preview {
            return Keyboard::Sopen;
        }
        match self.engine.placement() {
            Placement::Native if slot.view.page_has_keyboard() => Keyboard::Page,
            Placement::Native => Keyboard::Sopen,
            Placement::Texture => slot.keyboard,
        }
    }

    /// What the view said since the last call, in order.
    pub fn events(&mut self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// The page's picture (see [`View::snapshot`]); `None` once it closed.
    pub fn snapshot(&mut self, done: Box<dyn FnOnce(Option<egui::ColorImage>)>) {
        let mut slot = self.slot.borrow_mut();
        if slot.closed {
            drop(slot);
            done(None);
        } else {
            slot.view.snapshot(done);
        }
    }

    /// Gone (also when dropped); every later call does nothing.
    pub fn close(&mut self) {
        close_slot(&self.slot);
    }
}

impl Drop for Web {
    fn drop(&mut self) {
        close_slot(&self.slot);
    }
}

/// A texture page has the keyboard (§2.3): the window's input method is asked for, as a text
/// field asks for it, so it composes for the page and its committed text reaches it — winit
/// allows an IME only while egui says something wants it. The page's caret is not known here: the
/// composition is placed where the pointer last was in the page, else at its corner.
fn ime_for_page(ui: &egui::Ui, rect: egui::Rect) {
    let at = ui
        .input(|i| i.pointer.latest_pos())
        .filter(|p| rect.contains(*p))
        .unwrap_or(rect.min);
    let cursor = egui::Rect::from_min_size(at, egui::vec2(1.0, 16.0));
    let to_global = ui
        .ctx()
        .layer_transform_to_global(ui.layer_id())
        .unwrap_or_default();
    ui.output_mut(|o| {
        o.ime = Some(egui::output::IMEOutput {
            purpose: egui::IMEPurpose::Normal,
            rect: to_global * rect,
            cursor_rect: to_global * cursor,
            should_interrupt_composition: false,
        });
    });
}

/// A pointer event with its position in the page's own points (`origin`: the page's corner).
fn in_page(event: &egui::Event, origin: egui::Vec2) -> egui::Event {
    let mut event = event.clone();
    match &mut event {
        egui::Event::PointerMoved(pos) | egui::Event::PointerButton { pos, .. } => *pos -= origin,
        _ => {}
    }
    event
}

fn window_id() -> egui::Id {
    egui::Id::unique("sopen-web-window")
}

/// The window `ctx` draws: its key, given the first time it is asked for.
pub(crate) fn window(ctx: &egui::Context) -> WindowKey {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    ctx.data_mut(|d| {
        *d.get_temp_mut_or_insert_with(window_id(), || {
            WindowKey(NEXT.fetch_add(1, Ordering::Relaxed) + 1)
        })
    })
}

/// The window's key, if a view or its shell ever asked for one.
fn window_of(ctx: &egui::Context) -> Option<WindowKey> {
    ctx.data(|d| d.get_temp::<WindowKey>(window_id()))
}

/// The shell's handle (§2.1): the window `ctx` draws is `parent`; its key.
pub(crate) fn set_parent(ctx: &egui::Context, parent: Parent) -> WindowKey {
    let key = window(ctx);
    PARENTS.with(|p| p.borrow_mut().insert(key, parent));
    key
}

/// The shell's after-frame step (§2.2), after the pass's last painter: see the module's
/// "Placement". Nothing in a pass egui will discard (its next pass decides).
pub(crate) fn after_pass(ctx: &egui::Context) {
    // Every pass of every window comes here: with no page view, at once.
    if VIEWS.with(|v| v.borrow().iter().all(|w| w.strong_count() == 0)) {
        return;
    }
    if ctx.will_discard() {
        return;
    }
    let Some(key) = window_of(ctx) else {
        return;
    };
    let slots = slots_of(Some(key));
    if slots.is_empty() {
        return;
    }
    let pass = ctx.cumulative_pass_nr();
    // No handle from a shell (the test kit): the plain window.
    let parent = PARENTS.with(|p| p.borrow().get(&key).copied());
    let parent = parent.unwrap_or(Parent::Window);
    let mut above: HashMap<egui::LayerId, Vec<egui::Rect>> = HashMap::new();
    for slot in slots {
        let newly = {
            let mut s = slot.borrow_mut();
            let s = &mut *s;
            let placed = s.placed.filter(|p| p.pass == pass);
            let mut newly = None;
            if let Some(p) = placed {
                if !s.attached {
                    s.view.attach(parent);
                    s.attached = true;
                    newly = Some(s.engine.clone());
                }
                if s.told != Some((p.rect, p.scale)) {
                    s.view.place(p.rect, p.scale);
                    s.told = Some((p.rect, p.scale));
                }
            }
            let native = s.engine.placement() == Placement::Native;
            let hidden = match placed {
                None => true,
                Some(p) => {
                    !s.shown
                        || (native
                            && above
                                .entry(p.layer)
                                .or_insert_with(|| painted_above(ctx, p.layer))
                                .iter()
                                .any(|r| r.intersect(p.rect).is_positive()))
                }
            };
            if hidden {
                s.keyboard = Keyboard::Sopen;
            }
            if s.attached && hidden != s.hidden {
                if hidden && s.view.page_has_keyboard() {
                    s.view.take_keyboard();
                }
                s.view.set_hidden(hidden);
                s.hidden = hidden;
            }
            newly
        };
        if let Some(engine) = newly {
            attached(&*engine);
        }
    }
}

/// The rects (points) of everything this pass painted above `layer`: each shape later in paint
/// order than the layer's own — a later layer of its order, a painter-only one of its order
/// (egui paints those after the listed ones), any of a higher order — clipped as it is painted.
/// A rect counts with its fill transparent too: that is how a dialog (`egui::Modal`) paints its
/// backdrop over the whole window, blocking the input a native view would otherwise take. A
/// shape painted invisibly (egui's sizing pass of a new area) does not count: nothing shows yet.
///
/// egui keeps no list of painter-only layers, so the pass's shapes are drained, on a copy, in
/// the order egui will paint them, with a marker at the head of `layer`'s order to find where
/// the shapes above it start.
pub(crate) fn painted_above(ctx: &egui::Context, layer: egui::LayerId) -> Vec<egui::Rect> {
    use egui::Order;
    const ORDERS: [Order; 5] = [
        Order::Background,
        Order::Middle,
        Order::Foreground,
        Order::Tooltip,
        Order::Debug,
    ];
    let areas: Vec<egui::LayerId> = ctx.memory(|m| m.layer_ids().collect());
    let to_global = ctx.memory(|m| m.to_global.clone());
    let mut graphics = ctx.graphics(|g| g.clone());
    let marker = egui::LayerId::new(layer.order, egui::Id::unique("sopen-web-above"));
    let token: Arc<dyn std::any::Any + Send + Sync> = Arc::new(());
    graphics.entry(marker).add(
        egui::Rect::NOTHING,
        egui::Shape::Callback(egui::epaint::PaintCallback {
            rect: egui::Rect::NOTHING,
            callback: token.clone(),
        }),
    );
    // The paint order: per order the listed layers in egui's order (the marker first in the
    // page's); a page layer egui does not list goes after its order's listed ones — the first
    // place egui may paint it, so every other unlisted layer of its order counts as above it.
    let listed = areas.contains(&layer);
    let mut order = Vec::with_capacity(areas.len() + 2);
    for o in ORDERS {
        if o == layer.order {
            order.push(marker);
        }
        order.extend(areas.iter().filter(|l| l.order == o));
        if o == layer.order && !listed {
            order.push(layer);
        }
    }
    let len = |l: &egui::LayerId| graphics.get(*l).map_or(0, |list| list.all_entries().len());
    let page = order
        .iter()
        .position(|l| *l == layer)
        .unwrap_or(order.len());
    let first = order.iter().position(|l| *l == marker).unwrap_or(page);
    // The marker's shape, then those of its order's layers up to and including the page's.
    let through_page: usize = order[first..=page.min(order.len() - 1)]
        .iter()
        .map(len)
        .sum();
    let shapes = graphics.drain(&order, &to_global);
    let is_marker = |s: &egui::epaint::ClippedShape| match &s.shape {
        egui::Shape::Callback(c) => Arc::ptr_eq(&c.callback, &token),
        _ => false,
    };
    let Some(at) = shapes.iter().position(is_marker) else {
        return vec![];
    };
    shapes[at + through_page..]
        .iter()
        .map(|s| {
            let painted = match &s.shape {
                egui::Shape::Rect(r) => r.rect,
                shape => shape.visual_bounding_rect(),
            };
            s.clip_rect.intersect(painted)
        })
        .filter(|r| r.is_positive())
        .collect()
}

/// Every view of `ctx`'s window closes (the window is about to go).
pub(crate) fn teardown(ctx: &egui::Context) {
    if let Some(key) = window_of(ctx) {
        teardown_window(key);
    }
}

/// Every view of the window `key` closes.
pub(crate) fn teardown_window(key: WindowKey) {
    for slot in slots_of(Some(key)) {
        close_slot(&slot);
    }
}

/// The window `key` is gone (its shell dropped): its views close if anything left one, and its
/// handle is forgotten.
pub(crate) fn window_gone(key: WindowKey) {
    teardown_window(key);
    PARENTS.with(|p| p.borrow_mut().remove(&key));
}

/// A native view's picture at its frame (points).
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub rect: egui::Rect,
    pub image: egui::ColorImage,
}

/// The pictures [`snapshots`] asked for, as they come.
#[derive(Clone, Default)]
pub struct Shots {
    want: usize,
    got: Arc<Mutex<(usize, Vec<Snapshot>)>>,
}

impl Shots {
    /// Every view asked has answered.
    pub fn ready(&self) -> bool {
        self.got.lock().unwrap_or_else(PoisonError::into_inner).0 >= self.want
    }

    /// The pictures (those views that had one).
    pub fn take(&self) -> Vec<Snapshot> {
        let mut got = self.got.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut got.1)
    }
}

/// The screenshot hook's first step (§2.2): every native view `ctx`'s window shows is asked for
/// its picture.
pub(crate) fn snapshots(ctx: &egui::Context) -> Shots {
    let mut shots = Shots::default();
    let Some(key) = window_of(ctx) else {
        return shots;
    };
    for slot in slots_of(Some(key)) {
        let mut s = slot.borrow_mut();
        let shown = s.attached && !s.hidden && s.engine.placement() == Placement::Native;
        let Some((rect, _)) = s.told.filter(|_| shown) else {
            continue;
        };
        shots.want += 1;
        let (got, ctx) = (shots.got.clone(), ctx.clone());
        s.view.snapshot(Box::new(move |image| {
            let mut got = got.lock().unwrap_or_else(PoisonError::into_inner);
            got.0 += 1;
            if let Some(image) = image {
                got.1.push(Snapshot { rect, image });
            }
            ctx.request_repaint();
        }));
    }
    shots
}

/// The compose call (§2.2): each snapshot drawn over `image` — the window's read-back — at its
/// frame, `pixels_per_point` pixels a point, scaled to fit it, clipped to the image.
pub fn compose(image: &mut egui::ColorImage, pixels_per_point: f32, snapshots: &[Snapshot]) {
    let [w, h] = image.size;
    let px = |v: f32| (v * pixels_per_point).round() as i64;
    for shot in snapshots {
        let [sw, sh] = shot.image.size;
        let (x0, y0) = (px(shot.rect.min.x), px(shot.rect.min.y));
        let (tw, th) = (px(shot.rect.max.x) - x0, px(shot.rect.max.y) - y0);
        if sw == 0 || sh == 0 || tw <= 0 || th <= 0 {
            continue;
        }
        for y in y0.max(0)..(y0 + th).min(h as i64) {
            let sy = ((y - y0) * sh as i64 / th) as usize;
            for x in x0.max(0)..(x0 + tw).min(w as i64) {
                let sx = ((x - x0) * sw as i64 / tw) as usize;
                image[(x as usize, y as usize)] = shot.image[(sx, sy)];
            }
        }
    }
}
