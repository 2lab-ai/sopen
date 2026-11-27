//! Linux's engine (09 §2.3, WV-B): WebKitGTK 4.1, loaded at run time ([`api`]), offscreen on a
//! GTK thread of its own; each view's pixels are copied into an egui texture painted at its rect,
//! and egui's input reaches the page as synthesized GDK events. One path for X11 and Wayland:
//! nothing is embedded in sopen's window.
//!
//! - **The library** is opened at the first page view ([`engine`]); without it, or with one
//!   missing an entry point, or without the tools WebKit's sandbox runs its web processes under
//!   (`bwrap`, `xdg-dbus-proxy`), or where `bwrap` cannot make a sandbox ([`sandbox_runs`]:
//!   WebKit would abort the process there), the engine's reason is §4's (`no web engine here
//!   (…)`).
//! - **The GTK thread** is spawned at most once per process, by the first view ([`Life`]), and the
//!   windows' thread never waits for it: views opened while it starts are queued, and told
//!   should it not start — `gtk_init_check` false, no seat, the default main context another
//!   thread's, or nothing within [`START_WAIT`] — after which every view gets that reason (§4).
//!   Every check comes before WebKit's first use, which binds WebKit to the thread. It blocks in
//!   `g_main_context_iteration` (the default context its own for good), woken by
//!   `g_main_context_wakeup` when a view sends it something; every GTK, GLib and WebKit call is
//!   made there. It is not joined when the last view closes: a view made on a second thread
//!   aborts the process (measured: SIGABRT in `webkit_web_context_new_ephemeral` on a restarted
//!   thread), so it stays, parked in that wait, for the process. GTK is not let set the
//!   process's locale (`gtk_disable_setlocale`).
//! - **A view** ([`GtkView`]): a never-mapped `GtkOffscreenWindow` holding the `WebKitWebView`, on
//!   an ephemeral `WebKitWebContext` of its own — its own web and network processes, sandboxed
//!   (WebKit's bubblewrap sandbox, off by default in 4.1: turned on and read back — a context
//!   that says it is off makes no view and loads nothing, its view told why), nothing kept, no
//!   cookie,
//!   cache or storage shared with another view (§2.5); hardware acceleration
//!   off (the default aborts offscreen), no devtools, no file URL access, no WebRTC, no DNS
//!   prefetching, scripts as the view says ([`settings`]); no input method (keys are typed as
//!   their keyvals say: §2.3's IME limit); and nothing that would map a window of GTK's own — the
//!   context menu, a `<select>`'s popup, the file and colour choosers, script dialogs, printing,
//!   HTTP authentication, tooltips and full screen are all refused.
//! - **Drawing**: on `damage-event` the window's surface is mapped (`cairo_surface_map_to_image`,
//!   BGRA premultiplied) and copied, at most once per pass of the GTK loop, into the view's
//!   picture, which [`View::paint`] uploads and paints at its rect, one device pixel a pixel. The
//!   view is its rect × the window's scale in device pixels and its page is zoomed by that scale
//!   (an offscreen window ignores `GDK_SCALE`), so a page lays out as at 1× and draws at device
//!   resolution. A view says it finished once the picture after the load's end is copied (or
//!   [`FINISH_WAIT`] after it), so a screenshot taken then holds the page.
//! - **A file view** registers `sopen-page` on its context (§3.1) and is served through the scheme
//!   [`Table`] in pull mode: the head goes to WebKit as a `WebKitURISchemeResponse` over a page
//!   stream (`SopenPageStream`, a `GInputStream` whose class layout is checked against GLib's own
//!   before it is registered) whose reads stay pending until the server's bytes are in. A read's
//!   cancel is completed from an idle, never inside the emission; a request stopped before its
//!   head has no signal of its own, so the view's resource failing names it
//!   ([`Table::stop_headless`]) and its request is let go unanswered. No state is held across a
//!   call into GLib or WebKit (a completion can re-enter the stream's read).
//! - **Its content filter** (§3.3): the same JSON as macOS — everything blocked but `sopen-page:`,
//!   `data:` and `blob:` — compiled once per process into a new folder of its own in the temp
//!   folder (mode 0700, a name never taken before), removed at exit; one left by a sopen that was
//!   killed is swept at the next start. A file view loads nothing before the filter is on its
//!   content manager, and a filter that does not compile fails the view. WebKitGTK says nothing
//!   of a load its filter blocks, so the row's count of them is never said here. Beside the
//!   filter, every connection of a file view's context goes to a dead proxy ([`dead_proxy`]):
//!   a click or Enter on a link out makes WebKit connect to its host before the navigation
//!   policy refuses it, and the filter does not see that connection. Each holds a page's
//!   outside requests alone (measured).
//! - **Navigation** (§3.4, §7): WebKitGTK's policy decision does not say which frame a navigation
//!   is for. A file view's every frame keeps to its scheme host — a frame's `data:` and `blob:`
//!   documents and `about:srcdoc` allowed unless clicked, any other `about:` page refused in
//!   every frame (fail closed: it loads no resource, so a main frame going there cannot be told
//!   apart) — and anything else is refused and said ([`Event::Refused`]). The main frame is held
//!   to the view's scope as its main resource starts or is redirected ([`hold_main`]): a file
//!   view's `data:` or `blob:` document, an address window's other origin. An address window's
//!   frames load as in a browser; a click out and a new window out are refused at the policy. A load sopen asks for outside the scope is
//!   refused before WebKit sees it. A response the engine cannot show is refused, never
//!   downloaded, and a download that starts anyway is cancelled.

mod api;
#[cfg(test)]
mod tests;

use self::api::{
    text, Api, EventButton, EventFocus, EventKey, EventMotion, EventScroll, GBoolean, GError,
    GInputStreamClass, GType, GTypeQuery, PageStream, P,
};
use super::{Engine, Event, Events, Opened, Opening, Placement, View};
use crate::scheme::{Call, Head, Host, Mode, Read, RequestId, Table};
use eframe::egui;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The content filter of every file view (§3.3), the same JSON as macOS's rule list: every URL
/// blocked, then the page's own scheme and the in-process `data:` and `blob:` let through.
const RULES: &str = r#"[
{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
{"trigger":{"url-filter":"^sopen-page:"},"action":{"type":"ignore-previous-rules"}},
{"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
{"trigger":{"url-filter":"^blob:"},"action":{"type":"ignore-previous-rules"}}
]"#;

/// The filter's name in its store.
const RULES_ID: &std::ffi::CStr = c"sopen-file-view";

/// The temp folder of a process's filter is this, its pid and a random suffix
/// ([`make_rules_dir`]).
const RULES_PREFIX: &str = "sopen-web-rules-";

/// A test build starts no GTK unless the engine tests were asked for: run outside the private
/// compositor, it would reach the session's own display and bus.
#[cfg(test)]
const UNGATED: &str = "a test build starts no GTK without SOPEN_REQUIRE_WEBKIT=1";

/// A file view's proxy: port 0, which no socket can listen on, so a connection to it is refused
/// at once and nothing leaves the machine ([`dead_proxy`]).
const DEAD_PROXY: &str = "http://127.0.0.1:0";

/// WebKit's sandbox runs each web process under these (its own compiled-in paths): without them
/// the engine is not started (§3.2: the sandbox is on), rather than started unsandboxed.
const SANDBOX_TOOLS: [&str; 2] = ["/usr/bin/bwrap", "/usr/bin/xdg-dbus-proxy"];

/// How long after its load's end a view says it finished when no picture came.
const FINISH_WAIT: Duration = Duration::from_millis(150);

/// A new view's size in device pixels until it is placed.
const FIRST_SIZE: (i32, i32) = (64, 64);

/// The longest the sandbox probe may take ([`sandbox_runs`]).
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// The largest side of a view in device pixels, whatever the renderer allows.
const MAX_SIDE: usize = 16384;

/// The most a GTK thread waits for GTK to start.
const START_WAIT: Duration = Duration::from_secs(20);

/// The library, once per process: loaded at the first page view.
static API: OnceLock<Result<&'static Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(|| api::load(api::SONAME).map(|api| &*Box::leak(Box::new(api))))
        .clone()
}

/// The library in a GTK callback (loaded: a callback exists only after a view was made).
fn loaded() -> Option<&'static Api> {
    API.get().and_then(|api| api.as_ref().ok().copied())
}

/// The process's engine: the library loaded and its version read; GTK starts with the first view.
pub(super) fn engine() -> Result<Rc<dyn Engine>, String> {
    #[cfg(test)]
    if std::env::var("SOPEN_REQUIRE_WEBKIT").as_deref() != Ok("1") {
        return Err(UNGATED.to_string());
    }
    Ok(Rc::new(WebKitGtk::new(checked()?)))
}

/// The library, and a sandbox that starts here: what an engine needs before its first view.
fn checked() -> Result<&'static Api, String> {
    let api = api()?;
    static SANDBOX: OnceLock<Result<(), String>> = OnceLock::new();
    SANDBOX
        .get_or_init(|| {
            sandbox_tools(&SANDBOX_TOOLS)?;
            sandbox_runs(SANDBOX_TOOLS[0], PROBE_WAIT)
        })
        .clone()?;
    Ok(api)
}

/// `bwrap` makes a sandbox of the namespaces WebKit's uses — a user, PID, IPC, UTS and network
/// namespace — here; why not. Where it cannot (unprivileged user namespaces denied), WebKit
/// would abort the process at its first web process (measured: `g_error` "Failed to fully launch
/// dbus-proxy", SIGABRT, with nested user namespaces disabled), so there is no engine instead.
pub(super) fn sandbox_runs(bwrap: &str, wait: Duration) -> Result<(), String> {
    let at = Instant::now();
    let mut child = std::process::Command::new(bwrap)
        .args(["--ro-bind", "/", "/", "--unshare-all", "--die-with-parent"])
        .args(["--", "/bin/sh", "-c", ":"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("WebKitGTK's web process sandbox: {bwrap}: {e}"))?;
    // On the windows' thread: never longer than `wait`.
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if at.elapsed() < wait => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "WebKitGTK's web process sandbox: {bwrap} did not end within {wait:?}"
                ));
            }
            Err(e) => return Err(format!("WebKitGTK's web process sandbox: {bwrap}: {e}")),
        }
    }
    let ran = child
        .wait_with_output()
        .map_err(|e| format!("WebKitGTK's web process sandbox: {bwrap}: {e}"))?;
    crate::debug!(
        "page view: the sandbox probe took {:.1} ms",
        at.elapsed().as_secs_f64() * 1e3
    );
    if ran.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&ran.stderr);
    let said = said.lines().next().unwrap_or("").trim();
    Err(format!(
        "WebKitGTK's web process sandbox cannot start here ({bwrap}: {})",
        if said.is_empty() { "failed" } else { said }
    ))
}

/// Every one of `tools` is an executable file; the first that is not is the reason.
pub(super) fn sandbox_tools(tools: &[&str]) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for tool in tools {
        let runs = std::fs::metadata(tool)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
        if !runs {
            return Err(format!("WebKitGTK's web process sandbox needs {tool}"));
        }
    }
    Ok(())
}

/// WebKitGTK: the engine of every view of the process.
pub(super) struct WebKitGtk {
    api: &'static Api,
    version: String,
}

impl WebKitGtk {
    fn new(api: &'static Api) -> Self {
        // SAFETY: plain getters of the library's version.
        let version = unsafe {
            format!(
                "{}.{}.{}",
                (api.webkit_get_major_version)(),
                (api.webkit_get_minor_version)(),
                (api.webkit_get_micro_version)()
            )
        };
        WebKitGtk { api, version }
    }

    /// A new view; a file view contained as `contain` says (both, but in the tests' controls).
    pub(super) fn open_view(&self, opening: Opening, contain: Contain) -> Result<GtkView, String> {
        let api = self.api;
        let tx = sender(&LIFE, &opening.events, START_WAIT, move |rx, tx| {
            std::thread::Builder::new()
                .name("sopen-gtk".into())
                .spawn(move || gtk_main(api, rx, tx))
                .map(|_| ())
        })?;
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed) + 1;
        let shared = Arc::new(Shared::default());
        let scope = match &opening.content {
            Opened::Page { host, .. } => Scope::Page(host.clone()),
            Opened::Address { origin: address } => Scope::Address(origin(address)),
        };
        let open = Open {
            key,
            content: opening.content,
            scripts: opening.scripts,
            events: opening.events.clone(),
            shared: shared.clone(),
            contain,
        };
        post(api, &tx, Cmd::Open(Box::new(open)));
        Ok(GtkView {
            key,
            api,
            tx,
            shared,
            scope,
            events: opening.events,
            texture: None,
            seq: 0,
            forward: Forward::default(),
            told: None,
            max_side: MAX_SIDE,
            closed: false,
        })
    }
}

impl Engine for WebKitGtk {
    fn placement(&self) -> Placement {
        Placement::Texture
    }

    fn version(&self) -> String {
        self.version.clone()
    }

    /// A file view with scripts off is contained by its filter and its dead proxy: an outside
    /// image, stylesheet, frame, preconnect, prefetch, meta refresh, form, clicked link and Enter
    /// on a link made no connection (measured; a lookup of a name is WV-S2's, as on macOS). With
    /// scripts on, WebRTC is off by setting and the rest is WV-S2's — whose versions list
    /// ([`super::PASSED`]) keeps F2 off until then.
    fn uncontained(&self, _scripts: bool) -> Option<String> {
        None
    }

    fn open(&self, opening: Opening) -> Result<Box<dyn View>, String> {
        self.open_view(opening, Contain::ALL)
            .map(|view| Box::new(view) as Box<dyn View>)
    }

    /// Linux has no key monitor: sopen never gives the keyboard away (§2.4).
    fn monitor(&self, _on: bool) {}
}

// ---------------------------------------------------------------------------------------------
// The GTK thread.

/// What a view tells the GTK thread.
enum Cmd {
    Open(Box<Open>),
    Close(u64),
    Load(u64, String),
    Reload(u64),
    Scripts(u64, bool),
    /// Its size in device pixels and the window's scale.
    Place(u64, i32, i32, f32),
    Shown(u64, bool),
    Input(u64, Input),
    /// The view's table has calls to make.
    Drain(u64),
    /// The tests' look inside: run on the GTK thread with the view's web view.
    #[cfg(test)]
    Probe(u64, Probe),
    /// The tests' count of what the GTK thread holds.
    #[cfg(test)]
    Count(mpsc::Sender<Counts>),
}

/// What the GTK thread holds (the tests'): its views, the reads WebKit waits on, and every
/// view's requests by number.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub(super) views: usize,
    pub(super) reads: usize,
    pub(super) ids: usize,
}

/// The tests' hook: a new context is not asked for the sandbox, so its read-back says off.
#[cfg(test)]
pub(super) static SKIP_SANDBOX: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A look at a view on the GTK thread (the tests').
#[cfg(test)]
type Probe = Box<dyn FnOnce(&Api, P) + Send>;

struct Open {
    key: u64,
    content: Opened,
    scripts: bool,
    events: Events,
    shared: Arc<Shared>,
    contain: Contain,
}

/// What contains a file view (§3.3, §2.3): its content filter and its dead proxy — both, always,
/// but in the tests' controls, which take one away to show what the other does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Contain {
    pub(super) filter: bool,
    pub(super) proxy: bool,
}

impl Contain {
    pub(super) const ALL: Contain = Contain {
        filter: true,
        proxy: true,
    };
}

/// The GTK thread's life (the module's "GTK thread"): spawned at most once per process, from the
/// first view; never joined.
enum Life {
    Unstarted,
    /// Spawned, GTK not up yet: the views opened meanwhile, each told should it not start.
    Starting {
        tx: mpsc::Sender<Cmd>,
        waiting: Vec<Events>,
    },
    Running {
        tx: mpsc::Sender<Cmd>,
    },
    /// It did not start: why, for every later view (§4).
    Failed(String),
}

static LIFE: Mutex<Life> = Mutex::new(Life::Unstarted);

/// The GTK thread's sender for a new view whose events are `events`, at once: the first time the
/// thread is spawned (`spawn`), with a watchdog that fails its start after `deadline`; never
/// again — a view opened while it starts is queued, and told by `events` should the start fail;
/// once it failed, every view gets why.
fn sender(
    life: &'static Mutex<Life>,
    events: &Events,
    deadline: Duration,
    spawn: impl FnOnce(mpsc::Receiver<Cmd>, mpsc::Sender<Cmd>) -> std::io::Result<()>,
) -> Result<mpsc::Sender<Cmd>, String> {
    let mut state = life.lock().unwrap_or_else(PoisonError::into_inner);
    match &mut *state {
        Life::Running { tx } => Ok(tx.clone()),
        Life::Starting { tx, waiting } => {
            waiting.push(events.clone());
            Ok(tx.clone())
        }
        Life::Failed(why) => Err(why.clone()),
        Life::Unstarted => {
            let (tx, rx) = mpsc::channel();
            if let Err(e) = spawn(rx, tx.clone()) {
                let why = format!("no GTK thread: {e}");
                *state = Life::Failed(why.clone());
                return Err(why);
            }
            *state = Life::Starting {
                tx: tx.clone(),
                waiting: vec![events.clone()],
            };
            drop(state);
            let watchdog = std::thread::Builder::new()
                .name("sopen-gtk-start".into())
                .spawn(move || {
                    std::thread::sleep(deadline);
                    fail(life, &format!("GTK did not start within {deadline:?}"));
                });
            if let Err(e) = watchdog {
                fail(life, &format!("no watch on GTK's start: {e}"));
            }
            Ok(tx)
        }
    }
}

/// GTK is up: the thread runs, unless its start was given up already (then it serves nothing).
fn started(life: &Mutex<Life>) -> bool {
    let mut state = life.lock().unwrap_or_else(PoisonError::into_inner);
    let Life::Starting { tx, .. } = &*state else {
        return false;
    };
    *state = Life::Running { tx: tx.clone() };
    true
}

/// The start failed (`why`): the views waiting for it are told (§4's line), and every later one
/// gets it. Nothing once the thread runs.
fn fail(life: &Mutex<Life>, why: &str) {
    let mut state = life.lock().unwrap_or_else(PoisonError::into_inner);
    let Life::Starting { waiting, .. } = &mut *state else {
        return;
    };
    let line = super::Unavailable::NoEngine(why.to_string()).line();
    for events in waiting.drain(..) {
        events.send(Event::Failed(line.clone()));
    }
    crate::debug!("page view: {line}");
    *state = Life::Failed(why.to_string());
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

/// `cmd` to the GTK thread, which wakes for it.
fn post(api: &Api, tx: &mpsc::Sender<Cmd>, cmd: Cmd) {
    if tx.send(cmd).is_ok() {
        // SAFETY: documented as callable from any thread; NULL is the default context.
        unsafe { (api.g_main_context_wakeup)(std::ptr::null_mut()) };
    }
}

/// The GTK thread's state.
struct Gtk {
    tx: mpsc::Sender<Cmd>,
    views: HashMap<u64, Gview>,
    /// Reads WebKit is waiting on, by view and request; kept past a view's close until they are
    /// completed.
    reads: HashMap<(u64, u64), Pending>,
    pointer: P,
    keyboard: P,
}

/// One view on the GTK thread.
struct Gview {
    key: u64,
    win: P,
    web: P,
    /// Sopen's own references, let go at the close.
    context: P,
    manager: P,
    scope: Scope,
    events: Events,
    shared: Arc<Shared>,
    table: Option<Table>,
    /// The requests whose head has not gone yet: sopen's reference on each.
    requests: HashMap<u64, P>,
    /// Each live request's id by its number (the page stream carries the number); forgotten at
    /// its end, failure or stop.
    ids: HashMap<u64, RequestId>,
    needs_rules: bool,
    ruled: bool,
    /// The load waiting for the filter.
    pending: Option<String>,
    scale: f32,
    shown: bool,
    damaged: bool,
    /// The load ended: the view says it finished at the next copy, or [`FINISH_WAIT`] later.
    finishing: bool,
    finish_on_copy: bool,
    /// The main frame failed since its load started: no `Finished`.
    failed: bool,
}

/// A read WebKit waits on.
struct Pending {
    task: P,
    buffer: P,
    count: usize,
    cancellable: P,
    handler: std::ffi::c_ulong,
}

thread_local! {
    static GTK: RefCell<Option<Gtk>> = const { RefCell::new(None) };
}

/// `f` on the GTK thread's state; `None` off it or while it is borrowed (a callback never
/// panics: a panic cannot unwind through GTK).
fn with<R>(f: impl FnOnce(&mut Gtk) -> R) -> Option<R> {
    GTK.with(|gtk| {
        let mut gtk = gtk.try_borrow_mut().ok()?;
        gtk.as_mut().map(f)
    })
}

/// `f` on view `key`, if it is open.
fn with_view<R>(key: u64, f: impl FnOnce(&mut Gview) -> R) -> Option<R> {
    with(|gtk| gtk.views.get_mut(&key).map(f)).flatten()
}

fn gtk_main(api: &'static Api, rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<Cmd>) {
    // SAFETY: GTK starts on this thread, which makes every later GTK call.
    let devices = match unsafe { init(api) } {
        Ok(devices) => devices,
        Err(why) => {
            fail(&LIFE, &why);
            return;
        }
    };
    if !started(&LIFE) {
        // Given up on by the watchdog: no view is ever made here.
        return;
    }
    GTK.with(|gtk| {
        *gtk.borrow_mut() = Some(Gtk {
            tx,
            views: HashMap::new(),
            reads: HashMap::new(),
            pointer: devices.0,
            keyboard: devices.1,
        })
    });
    loop {
        // SAFETY: this thread owns the default context.
        unsafe { (api.g_main_context_iteration)(std::ptr::null_mut(), 1) };
        while let Ok(cmd) = rx.try_recv() {
            // SAFETY: on the GTK thread.
            unsafe { handle(api, cmd) };
        }
        // SAFETY: on the GTK thread.
        unsafe { copy_damaged(api) };
    }
}

/// GTK started, its display's seat found, the default main context this thread's for good, the
/// page stream's class registered and then — WebKit's first use, which binds it to this thread —
/// the filter compiling; the seat's pointer and keyboard. Every check comes before WebKit is
/// touched, so a start that fails leaves WebKit unbound.
///
/// SAFETY: on the thread that becomes the GTK thread.
unsafe fn init(api: &'static Api) -> Result<(P, P), String> {
    // SAFETY (this function's calls): GTK's start and getters on its own thread.
    unsafe {
        // GTK would set the whole process's locale from its thread; sopen's is left as it is.
        (api.gtk_disable_setlocale)();
        if (api.gtk_init_check)(std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
            return Err("GTK did not start (gtk_init_check: no display)".to_string());
        }
        let seat = (api.gdk_display_get_default_seat)((api.gdk_display_get_default)());
        if seat.is_null() {
            return Err("GTK's display has no seat".to_string());
        }
        // Owned here for the process: `g_main_context_invoke` from any other thread then never
        // runs its function there, inline.
        if (api.g_main_context_acquire)(std::ptr::null_mut()) == 0 {
            return Err("GLib's default main context is another thread's".to_string());
        }
        stream_type(api)?;
        compile(api);
        Ok((
            (api.gdk_seat_get_pointer)(seat),
            (api.gdk_seat_get_keyboard)(seat),
        ))
    }
}

/// One command, on the GTK thread.
///
/// SAFETY: on the GTK thread.
unsafe fn handle(api: &'static Api, cmd: Cmd) {
    // SAFETY (each arm): on the GTK thread, as the caller says.
    unsafe {
        match cmd {
            Cmd::Open(open) => self::open(api, *open),
            Cmd::Close(key) => close(api, key),
            Cmd::Load(key, url) => load(api, key, url),
            Cmd::Reload(key) => {
                let web = with_view(key, |v| v.pending.is_none().then_some(v.web)).flatten();
                if let Some(web) = web {
                    (api.webkit_web_view_reload)(web);
                }
            }
            Cmd::Scripts(key, on) => {
                if let Some(web) = with_view(key, |v| v.web) {
                    let settings = (api.webkit_web_view_get_settings)(web);
                    apply(api, settings, &scripts_settings(on));
                }
            }
            Cmd::Place(key, w, h, scale) => place(api, key, w, h, scale),
            Cmd::Shown(key, shown) => {
                with_view(key, |v| {
                    v.shown = shown;
                    v.damaged |= shown;
                });
            }
            Cmd::Input(key, input) => synthesize(api, key, input),
            Cmd::Drain(key) => drain_table(api, key),
            #[cfg(test)]
            Cmd::Probe(key, probe) => {
                let web = with_view(key, |v| v.web).unwrap_or(std::ptr::null_mut());
                probe(api, web);
            }
            #[cfg(test)]
            Cmd::Count(tx) => {
                let counts = with(|gtk| Counts {
                    views: gtk.views.len(),
                    reads: gtk.reads.len(),
                    ids: gtk.views.values().map(|v| v.ids.len()).sum(),
                });
                let _ = tx.send(counts.unwrap_or_default());
            }
        }
    }
}

/// The GTK loop's pending work run, a bounded number of passes (after a teardown).
///
/// SAFETY: on the GTK thread.
unsafe fn drain(api: &Api) {
    for _ in 0..200 {
        // SAFETY: on the GTK thread, without blocking.
        if unsafe { (api.g_main_context_iteration)(std::ptr::null_mut(), 0) } == 0 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A view on the GTK thread.

/// `instance`'s `signal` to `handler` with view `key` as its data.
///
/// SAFETY: on the GTK thread; `handler` has the signal's prototype.
unsafe fn connect(api: &Api, instance: P, signal: &std::ffi::CStr, handler: P, key: u64) {
    // SAFETY: as the caller says; the data is the key, not a pointer.
    unsafe {
        (api.g_signal_connect_data)(
            instance,
            signal.as_ptr(),
            handler,
            key as usize as P,
            std::ptr::null_mut(),
            0,
        );
    }
}

/// The key a callback's data carries.
fn key_of(data: P) -> u64 {
    data as usize as u64
}

/// A C string of `s`, its NULs cut off at the first.
fn c_string(s: &str) -> CString {
    let end = s.find('\0').unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}

/// The view's widgets and context, on the GTK thread.
///
/// SAFETY: on the GTK thread.
unsafe fn open(api: &'static Api, open: Open) {
    let Open {
        key,
        content,
        scripts,
        events,
        shared,
        contain,
    } = open;
    let Some(tx) = with(|gtk| gtk.tx.clone()) else {
        return;
    };
    // SAFETY (this function's calls): WebKit and GTK objects made and configured on the GTK
    // thread; the signal handlers have their signals' prototypes.
    unsafe {
        let context = (api.webkit_web_context_new_ephemeral)();
        // Before any process of the context is spawned (WebKit refuses it later).
        #[cfg(test)]
        let ask = !SKIP_SANDBOX.load(Ordering::SeqCst);
        #[cfg(not(test))]
        let ask = true;
        if ask {
            (api.webkit_web_context_set_sandbox_enabled)(context, 1);
        }
        if (api.webkit_web_context_get_sandbox_enabled)(context) == 0 {
            // Asked for, not on: no view, nothing loaded (§3.2).
            (api.g_object_unref)(context);
            let why = "WebKit's web process sandbox did not turn on".to_string();
            events.send(Event::Failed(super::Unavailable::NoEngine(why).line()));
            return;
        }
        (api.webkit_web_context_set_cache_model)(context, api::WEBKIT_CACHE_MODEL_DOCUMENT_VIEWER);
        connect(api, context, c"download-started", on_download as P, key);
        let (scope, table) = match content {
            Opened::Page { host, server } => {
                let wake = move || post(api, &tx, Cmd::Drain(key));
                let table = Table::new(host.clone(), server, Mode::Pull, wake);
                (Scope::Page(host), Some(table))
            }
            Opened::Address { origin: address } => (Scope::Address(origin(&address)), None),
        };
        if table.is_some() && contain.proxy {
            dead_proxy(api, context);
        }
        if table.is_some() {
            let scheme = c_string(crate::scheme::SCHEME);
            (api.webkit_web_context_register_uri_scheme)(
                context,
                scheme.as_ptr(),
                on_scheme as P,
                key as usize as P,
                std::ptr::null_mut(),
            );
        }
        let manager = (api.webkit_user_content_manager_new)();
        let settings = (api.webkit_settings_new)();
        apply(api, settings, &settings_for(scripts));
        let web = (api.g_object_new)(
            (api.webkit_web_view_get_type)(),
            c"web-context".as_ptr(),
            context,
            c"user-content-manager".as_ptr(),
            manager,
            c"settings".as_ptr(),
            settings,
            std::ptr::null::<c_char>(),
        );
        (api.g_object_unref)(settings);
        // No input method: a key is typed as its keyval says (§2.3); none reaches an IME
        // daemon, none of whose windows could show.
        (api.webkit_web_view_set_input_method_context)(web, std::ptr::null_mut());
        for (signal, handler) in view_signals() {
            connect(api, web, signal, handler, key);
        }
        let win = (api.gtk_offscreen_window_new)();
        (api.gtk_widget_set_size_request)(web, FIRST_SIZE.0, FIRST_SIZE.1);
        (api.gtk_container_add)(win, web);
        connect(api, win, c"damage-event", on_damage as P, key);
        (api.gtk_widget_show_all)(win);
        (api.gtk_widget_grab_focus)(web);
        let needs_rules = table.is_some() && contain.filter;
        let view = Gview {
            key,
            win,
            web,
            context,
            manager,
            scope,
            events,
            shared,
            table,
            requests: HashMap::new(),
            ids: HashMap::new(),
            needs_rules,
            ruled: false,
            pending: None,
            scale: 1.0,
            shown: true,
            damaged: false,
            finishing: false,
            finish_on_copy: false,
            failed: false,
        };
        with(|gtk| gtk.views.insert(key, view));
        if needs_rules {
            let filter = FILTER
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            match filter {
                Filter::Ready(filter) => rules_ready(api, key, filter as P),
                Filter::Failed(why) => rules_failed(key, &why),
                Filter::Unstarted | Filter::Compiling => {}
            }
        }
    }
}

/// The signals of every view's web view and their handlers.
fn view_signals() -> [(&'static std::ffi::CStr, P); 15] {
    [
        (c"decide-policy", on_decide as P),
        (c"load-changed", on_load_changed as P),
        (c"load-failed", on_load_failed as P),
        (c"web-process-terminated", on_process_ended as P),
        (c"resource-load-started", on_resource as P),
        (c"authenticate", on_authenticate as P),
        (c"context-menu", refuse4 as P),
        (c"show-option-menu", on_option_menu as P),
        (c"run-file-chooser", on_file_chooser as P),
        (c"run-color-chooser", on_color_chooser as P),
        (c"script-dialog", refuse2 as P),
        (c"print", refuse2 as P),
        (c"enter-fullscreen", refuse1 as P),
        (c"query-tooltip", no_tooltip as P),
        (c"create", no_window as P),
    ]
}

/// View `key` is gone: its requests stopped, its widgets destroyed, sopen's references let go,
/// the reads WebKit still waited on completed as cancelled.
///
/// SAFETY: on the GTK thread.
unsafe fn close(api: &Api, key: u64) {
    let Some(view) = with(|gtk| gtk.views.remove(&key)).flatten() else {
        return;
    };
    let at = Instant::now();
    let Gview {
        win,
        web,
        context,
        manager,
        table,
        requests,
        ..
    } = view;
    // The server is told every request stopped.
    drop(table);
    // SAFETY (this block's calls): the view's own objects, on the GTK thread; a request
    // never answered is let go unanswered (X-W1: WebKit accepts that).
    unsafe {
        (api.webkit_web_view_stop_loading)(web);
        (api.gtk_widget_destroy)(win);
        for request in requests.into_values() {
            (api.g_object_unref)(request);
        }
        (api.g_object_unref)(manager);
        (api.g_object_unref)(context);
    }
    let pending: Vec<Pending> = with(|gtk| {
        let keys: Vec<(u64, u64)> = gtk.reads.keys().filter(|k| k.0 == key).copied().collect();
        keys.into_iter()
            .filter_map(|k| gtk.reads.remove(&k))
            .collect()
    })
    .unwrap_or_default();
    for read in pending {
        // SAFETY: a read WebKit waits on, completed once.
        unsafe {
            complete(
                api,
                read,
                Err((api::G_IO_ERROR_CANCELLED, "the view closed".into())),
            )
        };
    }
    // SAFETY: on the GTK thread.
    unsafe { drain(api) };
    crate::debug!(
        "page view: view {key} torn down in {:.2} ms",
        at.elapsed().as_secs_f64() * 1e3
    );
}

/// A load asked of view `key`: waits for the filter in a file view that has none yet.
///
/// SAFETY: on the GTK thread.
unsafe fn load(api: &Api, key: u64, url: String) {
    let web = with_view(key, |v| {
        if v.needs_rules && !v.ruled {
            v.pending = Some(url.clone());
            None
        } else {
            Some(v.web)
        }
    })
    .flatten();
    let Some(web) = web else {
        return;
    };
    match CString::new(url.clone()) {
        // SAFETY: the view's web view, on the GTK thread.
        Ok(c) => unsafe { (api.webkit_web_view_load_uri)(web, c.as_ptr()) },
        Err(_) => {
            with_view(key, |v| {
                v.events
                    .send(Event::Failed(format!("not an address: {url}")))
            });
        }
    }
}

/// View `key` at `w` × `h` device pixels, its page zoomed by the window's scale (§2.3): an
/// offscreen window draws at GTK's own factor, so the widget is the device size over it and the
/// zoom the scale over it.
///
/// SAFETY: on the GTK thread.
unsafe fn place(api: &Api, key: u64, w: i32, h: i32, scale: f32) {
    let Some((web, win)) = with_view(key, |v| (v.web, v.win)) else {
        return;
    };
    // SAFETY (this block's calls): the view's widgets, on the GTK thread.
    unsafe {
        let factor = (api.gtk_widget_get_scale_factor)(web).max(1);
        let (lw, lh) = widget_size(w, h, factor);
        (api.gtk_widget_set_size_request)(web, lw, lh);
        (api.gtk_window_resize)(win, lw, lh);
        (api.webkit_web_view_set_zoom_level)(web, zoom(scale, factor));
    }
    with_view(key, |v| {
        v.scale = scale;
        v.damaged = true;
    });
}

/// The widget size for a view of `w` × `h` device pixels at GTK's `factor`.
pub(super) fn widget_size(w: i32, h: i32, factor: i32) -> (i32, i32) {
    let factor = factor.max(1);
    let up = |v: i32| ((v.max(1) + factor - 1) / factor).max(1);
    (up(w), up(h))
}

/// The page's zoom at the window's `scale` (pixels per point) and GTK's `factor`.
pub(super) fn zoom(scale: f32, factor: i32) -> f64 {
    let zoom = f64::from(scale) / f64::from(factor.max(1));
    if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    }
}

/// A rect of `rect` points at `scale` pixels a point, in device pixels: at least one, at most
/// `max_side` (the renderer's largest texture side; egui refuses a larger picture), at most
/// [`MAX_SIDE`].
pub(super) fn device_size(rect: egui::Rect, scale: f32, max_side: usize) -> (i32, i32) {
    let most = max_side.clamp(1, MAX_SIDE) as f32;
    let px = |points: f32| {
        let n = (points * scale).round();
        if n.is_finite() && n >= 1.0 {
            n.min(most) as i32
        } else {
            1
        }
    };
    (px(rect.width()), px(rect.height()))
}

// ---------------------------------------------------------------------------------------------
// Settings.

/// A setting of every view's `WebKitSettings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Setting {
    /// `enable-javascript` and `enable-javascript-markup`: the view's scripts.
    Javascript,
    JavascriptMarkup,
    /// `javascript-can-open-windows-automatically`.
    OpenWindows,
    /// `enable-developer-extras`: devtools.
    DeveloperExtras,
    /// `hardware-acceleration-policy`: `NEVER` (2), or the web process aborts offscreen.
    HardwareAcceleration,
    /// `allow-file-access-from-file-urls`, `allow-universal-access-from-file-urls`.
    FileAccess,
    UniversalAccess,
    /// `enable-webrtc`.
    Webrtc,
    /// `enable-dns-prefetching` (§3.2: it may be a no-op in 2.52).
    DnsPrefetching,
    /// `enable-page-cache`: nothing of a page kept after it is left.
    PageCache,
}

/// Every setting a new view gets, with its value (§3.2): scripts as asked, everything else off,
/// acceleration `NEVER`.
pub(super) fn settings_for(scripts: bool) -> Vec<(Setting, c_int)> {
    let mut all = scripts_settings(scripts);
    all.extend([
        (Setting::OpenWindows, 0),
        (Setting::DeveloperExtras, 0),
        (
            Setting::HardwareAcceleration,
            api::WEBKIT_HARDWARE_ACCELERATION_POLICY_NEVER,
        ),
        (Setting::FileAccess, 0),
        (Setting::UniversalAccess, 0),
        (Setting::Webrtc, 0),
        (Setting::DnsPrefetching, 0),
        (Setting::PageCache, 0),
    ]);
    all
}

/// The settings F2 changes (§3.3).
pub(super) fn scripts_settings(on: bool) -> Vec<(Setting, c_int)> {
    let on = c_int::from(on);
    vec![(Setting::Javascript, on), (Setting::JavascriptMarkup, on)]
}

/// `settings` given each value.
///
/// SAFETY: on the GTK thread; `settings` is a `WebKitSettings`.
unsafe fn apply(api: &Api, settings: P, values: &[(Setting, c_int)]) {
    for &(setting, value) in values {
        let set: unsafe extern "C" fn(P, GBoolean) = match setting {
            Setting::Javascript => api.webkit_settings_set_enable_javascript,
            Setting::JavascriptMarkup => api.webkit_settings_set_enable_javascript_markup,
            Setting::OpenWindows => {
                api.webkit_settings_set_javascript_can_open_windows_automatically
            }
            Setting::DeveloperExtras => api.webkit_settings_set_enable_developer_extras,
            Setting::HardwareAcceleration => api.webkit_settings_set_hardware_acceleration_policy,
            Setting::FileAccess => api.webkit_settings_set_allow_file_access_from_file_urls,
            Setting::UniversalAccess => {
                api.webkit_settings_set_allow_universal_access_from_file_urls
            }
            Setting::Webrtc => api.webkit_settings_set_enable_webrtc,
            Setting::DnsPrefetching => api.webkit_settings_set_enable_dns_prefetching,
            Setting::PageCache => api.webkit_settings_set_enable_page_cache,
        };
        // SAFETY: each setter takes the settings and an int (gboolean or the policy enum).
        unsafe { set(settings, value) };
    }
}

// ---------------------------------------------------------------------------------------------
// The content filter (§3.3).

/// Where the filter stands: compiled once per process.
#[derive(Clone)]
enum Filter {
    Unstarted,
    Compiling,
    /// The compiled `WebKitUserContentFilter`, kept for the process.
    Ready(usize),
    Failed(String),
}

static FILTER: Mutex<Filter> = Mutex::new(Filter::Unstarted);

/// The filter's folder, once made: removed at exit.
static RULES_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The filter compiles into this process's folder, once; the views waiting get it when it is
/// ready, on the GTK thread.
///
/// SAFETY: on the GTK thread.
unsafe fn compile(api: &Api) {
    {
        let mut filter = FILTER.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(*filter, Filter::Unstarted) {
            return;
        }
        *filter = Filter::Compiling;
    }
    let temp = std::env::temp_dir();
    sweep(&temp);
    let dir = match make_rules_dir(&temp) {
        Ok(dir) => dir,
        Err(e) => {
            // SAFETY: on the GTK thread.
            unsafe { compiled(api, Err(format!("the filter's folder: {e}"))) };
            return;
        }
    };
    if RULES_DIR.set(dir.clone()).is_ok() {
        // SAFETY: a plain function, run once at exit.
        unsafe { libc::atexit(remove_rules_dir) };
    }
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else {
        // SAFETY: on the GTK thread.
        unsafe { compiled(api, Err("the filter's folder has a NUL in its name".into())) };
        return;
    };
    // SAFETY: a store in sopen's own folder, the JSON as bytes, the callback's prototype is
    // `GAsyncReadyCallback`; the store's reference is let go in it.
    unsafe {
        let store = (api.webkit_user_content_filter_store_new)(path.as_ptr());
        let bytes = (api.g_bytes_new)(RULES.as_ptr().cast(), RULES.len());
        (api.webkit_user_content_filter_store_save)(
            store,
            RULES_ID.as_ptr(),
            bytes,
            std::ptr::null_mut(),
            on_compiled as P,
            std::ptr::null_mut(),
        );
        (api.g_bytes_unref)(bytes);
    }
}

unsafe extern "C" fn on_compiled(store: P, result: P, _data: P) {
    let Some(api) = loaded() else {
        return;
    };
    let mut error: *mut GError = std::ptr::null_mut();
    // SAFETY: the store's own result, on the GTK thread; the error is ours to free.
    let outcome = unsafe {
        let filter = (api.webkit_user_content_filter_store_save_finish)(store, result, &mut error);
        let outcome = if filter.is_null() {
            Err(error_text(api, error))
        } else {
            Ok(filter as usize)
        };
        if !error.is_null() {
            (api.g_error_free)(error);
        }
        (api.g_object_unref)(store);
        outcome
    };
    // SAFETY: on the GTK thread.
    unsafe { compiled(api, outcome) };
}

/// The filter's outcome to every view waiting for it.
///
/// SAFETY: on the GTK thread.
unsafe fn compiled(api: &Api, outcome: Result<usize, String>) {
    *FILTER.lock().unwrap_or_else(PoisonError::into_inner) = match &outcome {
        Ok(filter) => Filter::Ready(*filter),
        Err(why) => Filter::Failed(why.clone()),
    };
    let waiting: Vec<u64> = with(|gtk| {
        gtk.views
            .values()
            .filter(|v| v.needs_rules && !v.ruled)
            .map(|v| v.key)
            .collect()
    })
    .unwrap_or_default();
    for key in waiting {
        match &outcome {
            // SAFETY: on the GTK thread; the filter is kept for the process.
            Ok(filter) => unsafe { rules_ready(api, key, *filter as P) },
            Err(why) => rules_failed(key, why),
        }
    }
}

/// The filter goes on view `key`'s content manager; its waiting load goes.
///
/// SAFETY: on the GTK thread; `filter` is the compiled filter.
unsafe fn rules_ready(api: &Api, key: u64, filter: P) {
    let Some(manager) = with_view(key, |v| (!v.ruled).then_some(v.manager)).flatten() else {
        return;
    };
    // SAFETY: the view's content manager and the process's filter.
    unsafe { (api.webkit_user_content_manager_add_filter)(manager, filter) };
    let pending = with_view(key, |v| {
        v.ruled = true;
        v.pending.take()
    })
    .flatten();
    if let Some(url) = pending {
        // SAFETY: on the GTK thread.
        unsafe { load(api, key, url) };
    }
}

fn rules_failed(key: u64, why: &str) {
    with_view(key, |v| {
        v.pending = None;
        let line = format!("the content filter did not compile: {why}");
        v.events.send(Event::Failed(line));
    });
}

/// Every connection of a file view's context goes to [`DEAD_PROXY`], where nothing can listen:
/// WebKit connects to a link's host when a click or Enter on it starts a navigation, before its
/// policy refuses it (measured: a bare TCP connection to an outside server, no request, for a
/// clicked link and for Enter on a focused one), which no content filter sees. With the proxy
/// that connection — and any request the filter would not stop — is refused on this machine.
///
/// SAFETY: on the GTK thread; `context` is a new context of sopen's.
unsafe fn dead_proxy(api: &Api, context: P) {
    let uri = c_string(DEAD_PROXY);
    // SAFETY: the context's own data manager; the settings are copied by it and freed here.
    unsafe {
        let manager = (api.webkit_web_context_get_website_data_manager)(context);
        let settings = (api.webkit_network_proxy_settings_new)(uri.as_ptr(), std::ptr::null());
        (api.webkit_website_data_manager_set_network_proxy_settings)(
            manager,
            api::WEBKIT_NETWORK_PROXY_MODE_CUSTOM,
            settings,
        );
        (api.webkit_network_proxy_settings_free)(settings);
    }
}

/// A new filter folder of this process in `temp`: `sopen-web-rules-<pid>-<64 random bits>`.
pub(super) fn make_rules_dir(temp: &Path) -> std::io::Result<PathBuf> {
    let name = format!(
        "{RULES_PREFIX}{}-{}",
        std::process::id(),
        crate::remote::random_hex()
    );
    let dir = temp.join(name);
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    if !own_folder(&dir) {
        return Err(std::io::Error::other(format!(
            "{} is not a folder of this user's",
            dir.display()
        )));
    }
    Ok(dir)
}

/// Whether `path` itself (a link not followed) is a folder owned by this user.
fn own_folder(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        // SAFETY: reads this process's uid.
        meta.file_type().is_dir() && meta.uid() == unsafe { libc::getuid() }
    })
}

/// The filter folders in `temp` of processes that are gone are removed: only a real folder of
/// this user's (a link is never followed nor removed) named for a pid that no process has.
pub(super) fn sweep(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(RULES_PREFIX))
            .and_then(|rest| rest.split('-').next())
            .and_then(|p| p.parse::<i32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        if pid > 0 && !alive(pid) && own_folder(&path) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Whether process `pid` exists (another user's counts).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 checks the process without sending anything.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

extern "C" fn remove_rules_dir() {
    if let Some(dir) = RULES_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

// ---------------------------------------------------------------------------------------------
// Scope and navigation (§3.4, §7).

/// What a view keeps to: its scheme host (a file view) or its origin (an address window).
#[derive(Clone, Debug)]
pub(super) enum Scope {
    Page(Host),
    Address(Option<Origin>),
}

/// `scheme://host:port`, lower-cased, the port explicit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

/// `url`'s origin; `None` for one without an authority.
pub(super) fn origin(url: &str) -> Option<Origin> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match hostport.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            (format!("[{host}]"), after.strip_prefix(':'))
        }
        None => match hostport.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (hostport.to_string(), None),
        },
    };
    let port = match port.filter(|p| !p.is_empty()) {
        Some(p) => Some(p.parse().ok()?),
        None => match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        },
    };
    Some(Origin {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
    })
}

impl Scope {
    /// Whether `url` is the view's own: under its scheme host, or at its origin.
    pub(super) fn keeps(&self, url: &str) -> bool {
        match self {
            Scope::Page(host) => host.path_of(url).is_some(),
            Scope::Address(own) => own.is_some() && origin(url) == *own,
        }
    }

    /// The document on screen: a file view's path under its root, an address as it is.
    pub(super) fn document(&self, url: &str) -> String {
        match self {
            Scope::Page(host) => host
                .path_of(url)
                .map_or_else(|| url.to_string(), |p| decoded(p.trim_start_matches('/'))),
            Scope::Address(_) => url.to_string(),
        }
    }
}

/// What a navigation gets ([`navigation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Nav {
    /// It goes.
    Allow,
    /// Refused and said; `clicked` for a link the owner clicked (§3.4: F5 copies it).
    Refuse { clicked: bool },
    /// A new window inside the scope: opened in the view itself.
    InView,
}

/// The policy of a navigation to `url` in a view keeping to `scope` (§3.4, §7). WebKitGTK does
/// not say which frame a navigation is for (measured: not even its request's `Sec-Fetch-Dest`).
/// A file view's every frame keeps to its scheme host — containment comes first — but for a
/// frame's `data:` or `blob:` document, whose main-frame load is held as its main resource
/// starts ([`hold_main`]), and `about:srcdoc`. Any other `about:` page is refused here, every
/// frame alike: it loads no resource, so a main frame going there cannot be told apart (measured:
/// a meta refresh to `about:blank` reached the screen) — fail closed. An address window's frames
/// load as in a browser, and its main frame is held to its origin as its main resource starts:
/// there only a click out and a new window are refused here.
pub(super) fn navigation(scope: &Scope, url: &str, clicked: bool, new_window: bool) -> Nav {
    if scope.keeps(url) {
        return if new_window { Nav::InView } else { Nav::Allow };
    }
    let other = match scope {
        Scope::Page(_) => in_process(url) && (!scheme_is(url, "about") || srcdoc(url)),
        Scope::Address(_) => true,
    };
    if other && !clicked && !new_window {
        return Nav::Allow;
    }
    Nav::Refuse { clicked }
}

/// A main frame started loading `url` (or was redirected there) outside its view's scope — a
/// file view's `data:` or `blob:` document, an address window's other origin: it is stopped
/// there and said ([`hold_main`]); a frame's load is not held.
pub(super) fn main_out(scope: &Scope, url: &str) -> bool {
    !url.is_empty() && !scope.keeps(url)
}

/// `data:`, `blob:` and `about:`.
pub(super) fn in_process(url: &str) -> bool {
    ["data", "blob", "about"]
        .iter()
        .any(|scheme| scheme_is(url, scheme))
}

/// Whether `url`'s scheme is `scheme` (any case).
fn scheme_is(url: &str, scheme: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(s, _)| s.eq_ignore_ascii_case(scheme))
}

/// `about:srcdoc`: an `<iframe srcdoc>`'s document, a frame's only.
fn srcdoc(url: &str) -> bool {
    url.get(6..).is_some_and(|rest| {
        let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
        rest.eq_ignore_ascii_case("srcdoc")
    })
}

/// `path` percent-decoded (bytes that are not UTF-8 shown as U+FFFD).
pub(super) fn decoded(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `url` without its fragment: the scheme request and the view's resource name a request alike.
pub(super) fn without_fragment(url: &str) -> &str {
    &url[..url.find('#').unwrap_or(url.len())]
}

/// The row's line for a main-frame response the engine cannot show (never downloaded).
pub(super) fn cannot_show(status: u32, mime: &str) -> String {
    if status >= 400 {
        format!("the page answered {status}")
    } else if mime.is_empty() {
        "cannot show a response without a type".to_string()
    } else {
        format!("cannot show {mime}")
    }
}

/// A load failure that is a cancel — a new load, the policy's own refusal — and says nothing.
pub(super) fn cancelled(domain: &str, code: c_int) -> bool {
    (domain == "WebKitNetworkError" && code == 302)
        || (domain == "WebKitPolicyError" && code == 102)
}

// ---------------------------------------------------------------------------------------------
// The signal handlers (GTK thread).

/// A `GError`'s domain, code and message.
///
/// SAFETY: `error` is NULL or a `GError`.
unsafe fn error_parts(api: &Api, error: *const GError) -> (String, c_int, String) {
    if error.is_null() {
        return (String::new(), 0, String::new());
    }
    // SAFETY: as the caller says; the quark's string is GLib's.
    unsafe {
        let error = &*error;
        (
            text((api.g_quark_to_string)(error.domain)),
            error.code,
            text(error.message),
        )
    }
}

/// A `GError`'s message.
///
/// SAFETY: `error` is NULL or a `GError`.
unsafe fn error_text(api: &Api, error: *const GError) -> String {
    // SAFETY: as the caller says.
    let (domain, code, message) = unsafe { error_parts(api, error) };
    if domain.is_empty() {
        "unknown error".to_string()
    } else {
        format!("{message} ({domain} {code})")
    }
}

unsafe extern "C" fn on_decide(_web: P, decision: P, kind: c_int, data: P) -> GBoolean {
    let key = key_of(data);
    let Some(api) = loaded() else {
        return 0;
    };
    // SAFETY (this function's calls): the decision WebKit gave, decided once, on the GTK thread.
    unsafe {
        if kind == api::WEBKIT_POLICY_DECISION_TYPE_RESPONSE {
            decide_response(api, key, decision);
            return 1;
        }
        let action = (api.webkit_navigation_policy_decision_get_navigation_action)(decision);
        let url = text((api.webkit_uri_request_get_uri)((api
            .webkit_navigation_action_get_request)(
            action
        )));
        let clicked = (api.webkit_navigation_action_get_navigation_type)(action)
            == api::WEBKIT_NAVIGATION_TYPE_LINK_CLICKED;
        let new_window = kind == api::WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION;
        let verdict = with_view(key, |v| navigation(&v.scope, &url, clicked, new_window));
        match verdict {
            Some(Nav::Allow) => (api.webkit_policy_decision_use)(decision),
            Some(Nav::InView) => {
                (api.webkit_policy_decision_ignore)(decision);
                load(api, key, url);
            }
            Some(Nav::Refuse { clicked }) => {
                (api.webkit_policy_decision_ignore)(decision);
                with_view(key, |v| {
                    v.events.send(Event::Refused {
                        target: url,
                        clicked,
                    })
                });
            }
            // A closed view loads nothing more.
            None => (api.webkit_policy_decision_ignore)(decision),
        }
    }
    1
}

/// A response the engine cannot show is refused — never downloaded — and, for the main frame,
/// said.
///
/// SAFETY: on the GTK thread; `decision` is a response decision.
unsafe fn decide_response(api: &Api, key: u64, decision: P) {
    // SAFETY: the decision's fields, decided once.
    unsafe {
        if (api.webkit_response_policy_decision_is_mime_type_supported)(decision) != 0 {
            (api.webkit_policy_decision_use)(decision);
            return;
        }
        let main = (api.webkit_response_policy_decision_is_main_frame_main_resource)(decision) != 0;
        let response = (api.webkit_response_policy_decision_get_response)(decision);
        let status = (api.webkit_uri_response_get_status_code)(response);
        let mime = text((api.webkit_uri_response_get_mime_type)(response));
        (api.webkit_policy_decision_ignore)(decision);
        if main {
            with_view(key, |v| {
                v.failed = true;
                v.events.send(Event::Failed(cannot_show(status, &mime)));
            });
        }
    }
}

unsafe extern "C" fn on_load_changed(web: P, event: c_int, data: P) {
    let key = key_of(data);
    let Some(api) = loaded() else {
        return;
    };
    match event {
        api::WEBKIT_LOAD_STARTED => {
            with_view(key, |v| {
                v.failed = false;
                v.finishing = false;
                v.events.send(Event::Loading);
            });
        }
        api::WEBKIT_LOAD_COMMITTED => {
            // SAFETY: the view's URL, on the GTK thread.
            let url = unsafe { text((api.webkit_web_view_get_uri)(web)) };
            with_view(key, |v| {
                v.events.send(Event::Document(v.scope.document(&url)))
            });
        }
        api::WEBKIT_LOAD_FINISHED => {
            let wait = with_view(key, |v| {
                if v.failed {
                    return false;
                }
                if !v.shown {
                    v.events.send(Event::Finished);
                    return false;
                }
                v.finishing = true;
                true
            })
            .unwrap_or(false);
            if wait {
                // SAFETY: a one-shot timeout with the key as its data.
                unsafe {
                    (api.g_timeout_add_full)(
                        api::G_PRIORITY_DEFAULT,
                        FINISH_WAIT.as_millis() as c_uint,
                        finish_late as P,
                        key as usize as P,
                        std::ptr::null_mut(),
                    );
                }
            }
        }
        _ => {}
    }
}

/// [`FINISH_WAIT`] after a load's end with no picture: the view is copied now and says it
/// finished after the copy.
unsafe extern "C" fn finish_late(data: P) -> GBoolean {
    with_view(key_of(data), |v| {
        if v.finishing {
            v.finish_on_copy = true;
            v.damaged = true;
        }
    });
    0
}

unsafe extern "C" fn on_load_failed(
    _web: P,
    _event: c_int,
    _url: *const c_char,
    error: *const GError,
    data: P,
) -> GBoolean {
    let Some(api) = loaded() else {
        return 1;
    };
    // SAFETY: the error WebKit gave, for the call.
    let (domain, code, message) = unsafe { error_parts(api, error) };
    if !cancelled(&domain, code) {
        with_view(key_of(data), |v| {
            v.failed = true;
            v.finishing = false;
            v.events
                .send(Event::Failed(format!("{message} ({domain} {code})")));
        });
    }
    // WebKit's own error page is not shown: the row says why.
    1
}

unsafe extern "C" fn on_process_ended(_web: P, _reason: c_int, data: P) {
    with_view(key_of(data), |v| v.events.send(Event::ProcessEnded));
}

/// A resource the view watches: its request's address and `Range`.
struct Watched {
    key: u64,
    url: String,
    range: Option<String>,
}

unsafe extern "C" fn on_resource(web: P, resource: P, request: P, data: P) {
    let Some(api) = loaded() else {
        return;
    };
    // SAFETY: the request WebKit gave, for the call; the resource's `failed` gets a box of
    // sopen's, freed with the handler.
    unsafe {
        let url = text((api.webkit_uri_request_get_uri)(request));
        crate::debug!("page view: resource started {}", crate::url::redacted(&url));
        let range = header((api.webkit_uri_request_get_http_headers)(request), "Range");
        let key = key_of(data);
        let main = (api.webkit_web_view_get_main_resource)(web) == resource;
        if main {
            connect(api, resource, c"sent-request", on_main_request as P, key);
            hold_main(api, key, web, &url);
        }
        let watched = Box::new(Watched { key, url, range });
        (api.g_signal_connect_data)(
            resource,
            c"failed".as_ptr(),
            on_resource_failed as P,
            Box::into_raw(watched).cast(),
            drop_watched as P,
            0,
        );
    }
}

/// An address window's main resource asked again — a redirect: held to its origin too.
unsafe extern "C" fn on_main_request(resource: P, request: P, redirected: P, data: P) {
    let Some(api) = loaded() else {
        return;
    };
    if redirected.is_null() {
        return;
    }
    // SAFETY: the request WebKit gave, for the call; the view is the resource's.
    unsafe {
        let url = text((api.webkit_uri_request_get_uri)(request));
        let web = with_view(key_of(data), |v| v.web);
        if let Some(web) = web.filter(|w| (api.webkit_web_view_get_main_resource)(*w) == resource) {
            hold_main(api, key_of(data), web, &url);
        }
    }
}

/// A main frame loading `url` out of its view's scope ([`main_out`]: a file view's `data:` or
/// `blob:` document, an address window's other origin) is stopped at once and said (§3.4, §7):
/// its document never reaches the screen. WebKitGTK's policy decision does not say which frame
/// it is for, its view's address changes only at the commit, so this is where the main frame is
/// told apart: the view's main resource as it starts (measured: a frame's is not).
///
/// SAFETY: on the GTK thread; `web` is view `key`'s.
unsafe fn hold_main(api: &Api, key: u64, web: P, url: &str) {
    let out = with_view(key, |v| {
        let out = main_out(&v.scope, url);
        if out {
            v.events.send(Event::Refused {
                target: url.to_string(),
                clicked: false,
            });
        }
        out
    })
    .unwrap_or(false);
    if out {
        // SAFETY: the view's own load.
        unsafe { (api.webkit_web_view_stop_loading)(web) };
    }
}

unsafe extern "C" fn drop_watched(data: P, _closure: P) {
    // SAFETY: the box `on_resource` gave the handler, freed once, with it.
    drop(unsafe { Box::from_raw(data.cast::<Watched>()) });
}

/// A resource failed: a scheme request with no head yet is stopped and let go unanswered (X-W1:
/// WebKitGTK has no cancel signal before the head). A load the content filter blocks never comes
/// here: WebKitGTK says nothing of it (measured: no `resource-load-started`), so the row's count
/// of outside requests blocked (§3.3) is never said on Linux.
unsafe extern "C" fn on_resource_failed(_resource: P, error: *const GError, data: P) {
    let Some(api) = loaded() else {
        return;
    };
    // SAFETY: the box `on_resource` gave; the error WebKit gave, for the call.
    let (watched, (domain, code, message)) =
        unsafe { (&*data.cast::<Watched>(), error_parts(api, error)) };
    crate::debug!(
        "page view: resource failed {} ({domain} {code}: {message})",
        crate::url::redacted(&watched.url)
    );
    let key = watched.key;
    if is_scheme(&watched.url) {
        let url = without_fragment(&watched.url).to_string();
        let range = watched.range.clone();
        let request = with_view(key, |v| {
            let id = v.table.as_mut()?.stop_headless(&url, range.as_deref())?;
            v.ids.remove(&id.get());
            v.requests.remove(&id.get())
        })
        .flatten();
        if let Some(request) = request {
            // SAFETY: sopen's reference on a request never answered: let go unanswered.
            unsafe { (api.g_object_unref)(request) };
        }
    }
}

/// Whether `url` is a `sopen-page` address.
fn is_scheme(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(s, _)| s.eq_ignore_ascii_case(crate::scheme::SCHEME))
}

/// The header `name` of `headers` (a `SoupMessageHeaders`, or NULL).
///
/// SAFETY: on the GTK thread; `headers` is NULL or valid for the call.
unsafe fn header(headers: P, name: &str) -> Option<String> {
    let api = loaded()?;
    if headers.is_null() {
        return None;
    }
    let name = c_string(name);
    // SAFETY: as the caller says.
    let value = unsafe { (api.soup_message_headers_get_one)(headers, name.as_ptr()) };
    // SAFETY: a C string of libsoup's, or NULL.
    (!value.is_null()).then(|| unsafe { text(value) })
}

unsafe extern "C" fn on_damage(_widget: P, _event: P, data: P) -> GBoolean {
    with_view(key_of(data), |v| {
        v.damaged = true;
        if v.finishing {
            v.finish_on_copy = true;
        }
    });
    0
}

unsafe extern "C" fn on_download(_context: P, download: P, _data: P) {
    if let Some(api) = loaded() {
        crate::debug!("page view: a download started: cancelled");
        // SAFETY: the download WebKit gave, cancelled once.
        unsafe { (api.webkit_download_cancel)(download) };
    }
}

unsafe extern "C" fn on_authenticate(_web: P, request: P, data: P) -> GBoolean {
    if let Some(api) = loaded() {
        // SAFETY: the request WebKit gave, answered once.
        unsafe { (api.webkit_authentication_request_cancel)(request) };
    }
    with_view(key_of(data), |v| {
        let line = "the page asks for a login: refused".to_string();
        v.events.send(Event::Failed(line));
    });
    1
}

/// A `<select>`'s popup: none is shown (it would be a window of GTK's own); the menu is closed
/// from an idle, outside the emission.
unsafe extern "C" fn on_option_menu(_web: P, menu: P, _event: P, _rect: P, _data: P) -> GBoolean {
    if let Some(api) = loaded() {
        // SAFETY: a reference on the menu for the idle, which closes it and lets it go.
        unsafe {
            (api.g_object_ref)(menu);
            (api.g_idle_add_full)(
                api::G_PRIORITY_DEFAULT,
                close_option_menu as P,
                menu,
                std::ptr::null_mut(),
            );
        }
    }
    1
}

unsafe extern "C" fn close_option_menu(menu: P) -> GBoolean {
    if let Some(api) = loaded() {
        // SAFETY: the menu `on_option_menu` kept a reference on.
        unsafe {
            (api.webkit_option_menu_close)(menu);
            (api.g_object_unref)(menu);
        }
    }
    0
}

unsafe extern "C" fn on_file_chooser(_web: P, request: P, _data: P) -> GBoolean {
    if let Some(api) = loaded() {
        // SAFETY: the request WebKit gave, answered once.
        unsafe { (api.webkit_file_chooser_request_cancel)(request) };
    }
    1
}

unsafe extern "C" fn on_color_chooser(_web: P, request: P, _rect: P, _data: P) -> GBoolean {
    if let Some(api) = loaded() {
        // SAFETY: the request WebKit gave, answered once.
        unsafe { (api.webkit_color_chooser_request_cancel)(request) };
    }
    1
}

/// Handled, nothing shown: a signal with one argument.
unsafe extern "C" fn refuse1(_web: P, _data: P) -> GBoolean {
    1
}

/// Handled, nothing shown: a signal with two arguments (script dialog, print).
unsafe extern "C" fn refuse2(_web: P, _what: P, _data: P) -> GBoolean {
    1
}

/// Handled, nothing shown: the context menu.
unsafe extern "C" fn refuse4(_web: P, _menu: P, _event: P, _hit: P, _data: P) -> GBoolean {
    1
}

unsafe extern "C" fn no_tooltip(
    _widget: P,
    _x: c_int,
    _y: c_int,
    _keyboard: GBoolean,
    _tooltip: P,
    _data: P,
) -> GBoolean {
    0
}

/// `create`: no new web view, ever (a link to a new window inside the scope is the policy's).
unsafe extern "C" fn no_window(_web: P, _action: P, _data: P) -> P {
    std::ptr::null_mut()
}

// ---------------------------------------------------------------------------------------------
// The scheme glue (§3.1, X-W1): requests in, heads and pulled bodies out.

unsafe extern "C" fn on_scheme(request: P, data: P) {
    let key = key_of(data);
    let Some(api) = loaded() else {
        return;
    };
    // SAFETY (this function's calls): the request WebKit gave, referenced while the table has it.
    unsafe {
        let url = text((api.webkit_uri_scheme_request_get_uri)(request));
        let url = without_fragment(&url).to_string();
        let range = header(
            (api.webkit_uri_scheme_request_get_http_headers)(request),
            "Range",
        );
        (api.g_object_ref)(request);
        let started = with_view(key, |v| {
            let id = v.table.as_mut()?.start(&url, range);
            v.ids.insert(id.get(), id);
            v.requests.insert(id.get(), request);
            Some(id)
        })
        .flatten();
        if started.is_none() {
            // The view closed: the request fails, and sopen's reference goes.
            let why = c"the page view closed";
            let error = (api.g_error_new_literal)(
                (api.g_io_error_quark)(),
                api::G_IO_ERROR_FAILED,
                why.as_ptr(),
            );
            (api.webkit_uri_scheme_request_finish_error)(request, error);
            (api.g_error_free)(error);
            (api.g_object_unref)(request);
        }
    }
}

/// View `key`'s table's calls, made in its order, no state held while WebKit runs.
///
/// SAFETY: on the GTK thread.
unsafe fn drain_table(api: &Api, key: u64) {
    loop {
        let next = with_view(key, |v| v.table.as_mut().and_then(Table::next_call)).flatten();
        let Some((id, call)) = next else {
            return;
        };
        // SAFETY (each arm): on the GTK thread.
        unsafe {
            match call {
                Call::Head(head) => answer(api, key, id, &head),
                Call::Fail(why) => {
                    let request = with_view(key, |v| {
                        v.ids.remove(&id.get());
                        v.requests.remove(&id.get())
                    })
                    .flatten();
                    if let Some(request) = request {
                        let why = c_string(&why);
                        let error = (api.g_error_new_literal)(
                            (api.g_io_error_quark)(),
                            api::G_IO_ERROR_FAILED,
                            why.as_ptr(),
                        );
                        (api.webkit_uri_scheme_request_finish_error)(request, error);
                        (api.g_error_free)(error);
                        (api.g_object_unref)(request);
                    }
                }
                Call::Readable => fulfil(api, key, id.get()),
                // A push engine's (macOS).
                Call::Data(_) | Call::Done => {}
            }
        }
    }
}

/// Request `id`'s head to WebKit: a response over a page stream of its own.
///
/// SAFETY: on the GTK thread.
unsafe fn answer(api: &Api, key: u64, id: RequestId, head: &Head) {
    let Some(request) = with_view(key, |v| v.requests.remove(&id.get())).flatten() else {
        return;
    };
    let Ok(stream_type) = STREAM_TYPE.get().cloned().unwrap_or(Err(String::new())) else {
        return;
    };
    // SAFETY (this block's calls): a new stream of sopen's class, its fields written before
    // WebKit sees it; the headers' ownership goes to the response; every reference of sopen's
    // let go after the request is finished.
    unsafe {
        let stream = (api.g_object_new)(stream_type, std::ptr::null::<c_char>());
        let fields = stream.cast::<PageStream>();
        (*fields).view = key;
        (*fields).request = id.get();
        let length = i64::try_from(head.length()).unwrap_or(i64::MAX);
        let response = (api.webkit_uri_scheme_response_new)(stream, length);
        let reason = c_string(head.reason());
        (api.webkit_uri_scheme_response_set_status)(
            response,
            c_uint::from(head.code()),
            reason.as_ptr(),
        );
        if !head.mime().is_empty() {
            let mime = c_string(head.mime());
            (api.webkit_uri_scheme_response_set_content_type)(response, mime.as_ptr());
        }
        let headers = (api.soup_message_headers_new)(api::SOUP_MESSAGE_HEADERS_RESPONSE);
        for (name, value) in head.headers() {
            let (name, value) = (c_string(name), c_string(&value));
            (api.soup_message_headers_append)(headers, name.as_ptr(), value.as_ptr());
        }
        (api.webkit_uri_scheme_response_set_http_headers)(response, headers);
        (api.webkit_uri_scheme_request_finish_with_response)(request, response);
        (api.g_object_unref)(response);
        (api.g_object_unref)(stream);
        (api.g_object_unref)(request);
    }
}

/// The page stream's class: GLib's `GInputStream` and sopen's fields, registered once per
/// process on the layout checked against GLib's own.
static STREAM_TYPE: OnceLock<Result<GType, String>> = OnceLock::new();

/// SAFETY: GLib loaded.
unsafe fn stream_type(api: &Api) -> Result<GType, String> {
    STREAM_TYPE
        .get_or_init(|| {
            // SAFETY: GLib's type system, callable from any thread.
            unsafe {
                let parent = (api.g_input_stream_get_type)();
                let mut query = GTypeQuery {
                    ty: 0,
                    name: std::ptr::null(),
                    class_size: 0,
                    instance_size: 0,
                };
                (api.g_type_query)(parent, &mut query);
                let known = api::stream_layout();
                if (query.class_size, query.instance_size) != known {
                    return Err(format!(
                        "GLib's GInputStream is {}/{} bytes, not the {}/{} sopen knows",
                        query.class_size, query.instance_size, known.0, known.1
                    ));
                }
                let ty = (api.g_type_register_static_simple)(
                    parent,
                    c"SopenPageStream".as_ptr(),
                    query.class_size,
                    class_init as P,
                    api::page_stream_size(),
                    std::ptr::null_mut(),
                    0,
                );
                if ty == 0 {
                    return Err("the page stream's class was not registered".to_string());
                }
                Ok(ty)
            }
        })
        .clone()
}

unsafe extern "C" fn class_init(class: P, _data: P) {
    let class = class.cast::<GInputStreamClass>();
    // SAFETY: GLib hands the class struct of sopen's type, laid out as checked.
    unsafe {
        (*class).read_fn = read_fn as P;
        (*class).close_fn = close_fn as P;
        (*class).read_async = read_async as P;
        (*class).read_finish = read_finish as P;
        (*class).close_async = close_async as P;
        (*class).close_finish = close_finish as P;
    }
}

/// The view and request a page stream reads.
///
/// SAFETY: `stream` is a page stream.
unsafe fn stream_ids(stream: P) -> (u64, u64) {
    // SAFETY: as the caller says; written before WebKit saw the stream.
    let fields = unsafe { &*stream.cast::<PageStream>() };
    (fields.view, fields.request)
}

unsafe extern "C" fn read_fn(
    _stream: P,
    _buffer: P,
    _count: usize,
    _cancellable: P,
    error: *mut *mut GError,
) -> isize {
    if let Some(api) = loaded() {
        if !error.is_null() {
            let why = c"a page stream is read asynchronously";
            // SAFETY: the error slot GIO gave.
            unsafe {
                *error = (api.g_error_new_literal)(
                    (api.g_io_error_quark)(),
                    api::G_IO_ERROR_FAILED,
                    why.as_ptr(),
                );
            }
        }
    }
    -1
}

/// WebKit closes the stream: a request it closes before its end is stopped.
fn stream_closed(stream: P) {
    // SAFETY: GIO's page stream.
    let (key, raw) = unsafe { stream_ids(stream) };
    with_view(key, |v| stop_request(v, raw));
}

/// Request `raw` of view `v` stopped (a no-op at the table once it ended) and forgotten.
fn stop_request(v: &mut Gview, raw: u64) {
    if let (Some(id), Some(table)) = (v.ids.remove(&raw), v.table.as_mut()) {
        table.stop(id);
    }
}

unsafe extern "C" fn close_fn(stream: P, _cancellable: P, _error: *mut *mut GError) -> GBoolean {
    stream_closed(stream);
    1
}

unsafe extern "C" fn close_async(
    stream: P,
    _priority: c_int,
    cancellable: P,
    callback: P,
    data: P,
) {
    stream_closed(stream);
    if let Some(api) = loaded() {
        // SAFETY: a task of the stream, answered at once.
        unsafe {
            let task = (api.g_task_new)(stream, cancellable, callback, data);
            (api.g_task_return_boolean)(task, 1);
            (api.g_object_unref)(task);
        }
    }
}

unsafe extern "C" fn close_finish(_stream: P, result: P, error: *mut *mut GError) -> GBoolean {
    match loaded() {
        // SAFETY: the task `close_async` made.
        Some(api) => unsafe { (api.g_task_propagate_boolean)(result, error) },
        None => 0,
    }
}

unsafe extern "C" fn read_async(
    stream: P,
    buffer: P,
    count: usize,
    _priority: c_int,
    cancellable: P,
    callback: P,
    data: P,
) {
    let Some(api) = loaded() else {
        return;
    };
    // SAFETY: GIO's page stream.
    let (key, raw) = unsafe { stream_ids(stream) };
    // SAFETY (this block's calls): a task of the stream; the cancel handler's data is a box of
    // sopen's, freed when it is disconnected (called at once, inside the connect, when the
    // cancellable is cancelled already: it only schedules an idle).
    let read = unsafe {
        let task = (api.g_task_new)(stream, cancellable, callback, data);
        let handler = if cancellable.is_null() {
            0
        } else {
            (api.g_cancellable_connect)(
                cancellable,
                on_cancelled as P,
                Box::into_raw(Box::new((key, raw))).cast(),
                drop_ids as P,
            )
        };
        Pending {
            task,
            buffer,
            count,
            cancellable,
            handler,
        }
    };
    let kept = with(|gtk| gtk.reads.insert((key, raw), read)).is_some();
    if kept {
        // SAFETY: on the GTK thread.
        unsafe { fulfil(api, key, raw) };
    }
}

unsafe extern "C" fn read_finish(_stream: P, result: P, error: *mut *mut GError) -> isize {
    match loaded() {
        // SAFETY: the task `read_async` made.
        Some(api) => unsafe { (api.g_task_propagate_int)(result, error) },
        None => -1,
    }
}

unsafe extern "C" fn drop_ids(data: P) {
    // SAFETY: a box of sopen's, freed once.
    drop(unsafe { Box::from_raw(data.cast::<(u64, u64)>()) });
}

/// The read's cancellable fired (WebKit stopped the load): the read is completed from an idle,
/// never inside the emission.
unsafe extern "C" fn on_cancelled(_cancellable: P, data: P) {
    let Some(api) = loaded() else {
        return;
    };
    // SAFETY: the box `read_async` gave the handler.
    let ids = unsafe { *data.cast::<(u64, u64)>() };
    // SAFETY: an idle with a box of sopen's, freed with it.
    unsafe {
        (api.g_idle_add_full)(
            api::G_PRIORITY_DEFAULT,
            cancel_read as P,
            Box::into_raw(Box::new(ids)).cast(),
            drop_ids as P,
        );
    }
}

unsafe extern "C" fn cancel_read(data: P) -> GBoolean {
    let Some(api) = loaded() else {
        return 0;
    };
    // SAFETY: the idle's box.
    let (key, raw) = unsafe { *data.cast::<(u64, u64)>() };
    with_view(key, |v| stop_request(v, raw));
    let read = with(|gtk| gtk.reads.remove(&(key, raw))).flatten();
    if let Some(read) = read {
        // SAFETY: a read WebKit waits on, completed once.
        unsafe {
            complete(
                api,
                read,
                Err((api::G_IO_ERROR_CANCELLED, "stopped".into())),
            )
        };
    }
    0
}

/// The read WebKit waits on for request `raw` of view `key`, completed from what the table has
/// — or kept for its next `Readable`.
///
/// SAFETY: on the GTK thread.
unsafe fn fulfil(api: &Api, key: u64, raw: u64) {
    let Some(read) = with(|gtk| gtk.reads.remove(&(key, raw))).flatten() else {
        return;
    };
    let got = with_view(key, |v| {
        let got = match (v.ids.get(&raw).copied(), v.table.as_mut()) {
            (Some(id), Some(table)) => table.read(id, read.count),
            _ => Read::Gone,
        };
        // Its end, failure or stop is the request's last word: forgotten.
        if !matches!(got, Read::Pending | Read::Bytes(_)) {
            v.ids.remove(&raw);
        }
        got
    })
    .unwrap_or(Read::Gone);
    let result = match got {
        Read::Pending => {
            with(|gtk| gtk.reads.insert((key, raw), read));
            return;
        }
        Read::Bytes(bytes) => {
            let n = bytes.len().min(read.count);
            // SAFETY: WebKit's buffer of `count` bytes, written within it.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), read.buffer.cast::<u8>(), n) };
            Ok(n)
        }
        Read::End => Ok(0),
        Read::Failed(why) => Err((api::G_IO_ERROR_FAILED, why)),
        Read::Gone => Err((
            api::G_IO_ERROR_CANCELLED,
            "the request was stopped".to_string(),
        )),
    };
    // SAFETY: a read WebKit waits on, completed once.
    unsafe { complete(api, read, result) };
}

/// A read completed: its cancel handler disconnected (outside the emission), its task answered.
///
/// SAFETY: on the GTK thread; `read` is completed once.
unsafe fn complete(api: &Api, read: Pending, result: Result<usize, (c_int, String)>) {
    // SAFETY: the read's own task and handler.
    unsafe {
        if read.handler != 0 {
            (api.g_cancellable_disconnect)(read.cancellable, read.handler);
        }
        match result {
            Ok(n) => (api.g_task_return_int)(read.task, isize::try_from(n).unwrap_or(isize::MAX)),
            Err((code, why)) => {
                let why = c_string(&why);
                let error = (api.g_error_new_literal)((api.g_io_error_quark)(), code, why.as_ptr());
                (api.g_task_return_error)(read.task, error);
            }
        }
        (api.g_object_unref)(read.task);
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing.

/// What the GTK thread hands the window's side: the newest picture.
#[derive(Default)]
struct Shared {
    state: Mutex<SharedState>,
}

#[derive(Default)]
struct SharedState {
    picture: Option<Picture>,
    /// Bumped at every new picture.
    seq: u64,
    /// The window the view is painted in: woken at every new picture.
    ctx: Option<egui::Context>,
}

#[derive(Clone)]
struct Picture {
    image: Arc<egui::ColorImage>,
    /// The window's scale it was drawn at (device pixels a point).
    scale: f32,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn post(&self, image: egui::ColorImage, scale: f32) {
        let mut state = self.lock();
        state.picture = Some(Picture {
            image: Arc::new(image),
            scale,
        });
        state.seq += 1;
        if let Some(ctx) = &state.ctx {
            ctx.request_repaint();
        }
    }
}

/// Every damaged view copied, once per pass of the GTK loop; a view that waited for its picture
/// says it finished.
///
/// SAFETY: on the GTK thread.
unsafe fn copy_damaged(api: &Api) {
    let todo: Vec<(u64, P, Arc<Shared>, f32, bool)> = with(|gtk| {
        gtk.views
            .values_mut()
            .filter(|v| v.damaged && v.shown)
            .map(|v| {
                v.damaged = false;
                let finish = std::mem::take(&mut v.finish_on_copy);
                (v.key, v.win, v.shared.clone(), v.scale, finish)
            })
            .collect()
    })
    .unwrap_or_default();
    for (key, win, shared, scale, finish) in todo {
        let at = Instant::now();
        // SAFETY: the view's window, on the GTK thread.
        let image = unsafe { copy(api, win) };
        if let Some(image) = image {
            let [w, h] = image.size;
            crate::debug!(
                "page view: copy {w}x{h} in {:.2} ms",
                at.elapsed().as_secs_f64() * 1e3
            );
            shared.post(image, scale);
        }
        if finish {
            with_view(key, |v| {
                if v.finishing {
                    v.finishing = false;
                    v.events.send(Event::Finished);
                }
            });
        }
    }
}

/// The offscreen window's pixels: its surface mapped (BGRA premultiplied, cairo's ARGB32 on a
/// little-endian machine) and copied as egui's premultiplied RGBA.
///
/// SAFETY: on the GTK thread; `win` is an offscreen window.
unsafe fn copy(api: &Api, win: P) -> Option<egui::ColorImage> {
    // SAFETY (this function's calls): the window's own surface, mapped and unmapped once, read
    // within its stride and height.
    unsafe {
        let surface = (api.gtk_offscreen_window_get_surface)(win);
        if surface.is_null() {
            return None;
        }
        let image = (api.cairo_surface_map_to_image)(surface, std::ptr::null_mut());
        if image.is_null() {
            return None;
        }
        (api.cairo_surface_flush)(image);
        let w = usize::try_from((api.cairo_image_surface_get_width)(image)).unwrap_or(0);
        let h = usize::try_from((api.cairo_image_surface_get_height)(image)).unwrap_or(0);
        let stride = usize::try_from((api.cairo_image_surface_get_stride)(image)).unwrap_or(0);
        let data = (api.cairo_image_surface_get_data)(image);
        let picture = (w > 0 && h > 0 && stride >= w * 4 && !data.is_null()).then(|| {
            let mut pixels = Vec::with_capacity(w * h);
            for y in 0..h {
                let row = std::slice::from_raw_parts(data.add(y * stride), w * 4);
                pixels.extend(
                    row.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|&[b, g, r, a]| egui::Color32::from_rgba_premultiplied(r, g, b, a)),
                );
            }
            egui::ColorImage::new([w, h], pixels)
        });
        (api.cairo_surface_unmap_image)(surface, image);
        picture
    }
}

// ---------------------------------------------------------------------------------------------
// Input (§2.3, §2.4): egui's events as GDK's, in device pixels.

/// One input to the page.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Input {
    Motion {
        x: f64,
        y: f64,
        state: c_uint,
    },
    Button {
        x: f64,
        y: f64,
        button: c_uint,
        pressed: bool,
        state: c_uint,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        state: c_uint,
    },
    Key {
        keyval: c_uint,
        pressed: bool,
        state: c_uint,
    },
    Focus(bool),
}

impl Input {
    /// The input with its position divided by `factor` (GTK's own scale).
    pub(super) fn over(self, factor: f64) -> Input {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        match self {
            Input::Motion { x, y, state } => Input::Motion {
                x: x / factor,
                y: y / factor,
                state,
            },
            Input::Button {
                x,
                y,
                button,
                pressed,
                state,
            } => Input::Button {
                x: x / factor,
                y: y / factor,
                button,
                pressed,
                state,
            },
            Input::Scroll {
                x,
                y,
                dx,
                dy,
                state,
            } => Input::Scroll {
                x: x / factor,
                y: y / factor,
                dx,
                dy,
                state,
            },
            other => other,
        }
    }
}

/// The window side's input state of a view: its scale, the pointer, the buttons held, whether
/// the page was given focus.
#[derive(Clone, Debug)]
pub(super) struct Forward {
    pub(super) scale: f32,
    /// The page's size in points (a page-sized scroll).
    pub(super) height: f32,
    pointer: (f64, f64),
    buttons: c_uint,
    focused: bool,
}

impl Default for Forward {
    fn default() -> Self {
        Forward {
            scale: 1.0,
            height: 0.0,
            pointer: (0.0, 0.0),
            buttons: 0,
            focused: false,
        }
    }
}

impl Forward {
    /// `event` (positions in the page's points) as the page's input: a press gives the page
    /// focus first; typed text, an IME's committed text and a paste are typed key by key; Copy
    /// and Cut are Ctrl+C and Ctrl+X; a key that types text is left to its text event.
    pub(super) fn translate(&mut self, event: &egui::Event) -> Vec<Input> {
        let at = |pos: egui::Pos2, scale: f32| {
            (
                f64::from(pos.x) * f64::from(scale),
                f64::from(pos.y) * f64::from(scale),
            )
        };
        match event {
            egui::Event::PointerMoved(pos) => {
                self.pointer = at(*pos, self.scale);
                let (x, y) = self.pointer;
                vec![Input::Motion {
                    x,
                    y,
                    state: self.buttons,
                }]
            }
            egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                self.pointer = at(*pos, self.scale);
                let (x, y) = self.pointer;
                let mut out = vec![];
                if *pressed && !self.focused {
                    self.focused = true;
                    out.push(Input::Focus(true));
                }
                let number = button_number(*button);
                out.push(Input::Button {
                    x,
                    y,
                    button: number,
                    pressed: *pressed,
                    state: state_of(*modifiers) | self.buttons,
                });
                let mask = button_mask(number);
                if *pressed {
                    self.buttons |= mask;
                } else {
                    self.buttons &= !mask;
                }
                out
            }
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => {
                let (x, y) = self.pointer;
                let (dx, dy) = smooth_delta(*unit, *delta, self.scale, self.height);
                vec![Input::Scroll {
                    x,
                    y,
                    dx,
                    dy,
                    state: state_of(*modifiers) | self.buttons,
                }]
            }
            egui::Event::Key {
                key,
                pressed,
                modifiers,
                ..
            } => match key_keyval(*key, *modifiers) {
                Some(keyval) => vec![Input::Key {
                    keyval,
                    pressed: *pressed,
                    state: state_of(*modifiers),
                }],
                None => vec![],
            },
            egui::Event::Text(text)
            | egui::Event::Paste(text)
            | egui::Event::Ime(egui::ImeEvent::Commit(text)) => typed(text),
            egui::Event::Copy => shortcut('c'),
            egui::Event::Cut => shortcut('x'),
            _ => vec![],
        }
    }

    /// The page loses focus (hidden, a preview): said once.
    pub(super) fn blur(&mut self) -> Option<Input> {
        std::mem::take(&mut self.focused).then_some(Input::Focus(false))
    }
}

/// GDK's number of egui's button.
pub(super) fn button_number(button: egui::PointerButton) -> c_uint {
    match button {
        egui::PointerButton::Primary => 1,
        egui::PointerButton::Middle => 2,
        egui::PointerButton::Secondary => 3,
        egui::PointerButton::Extra1 => 8,
        egui::PointerButton::Extra2 => 9,
    }
}

/// The state mask of button `number` held (buttons 1–5).
fn button_mask(number: c_uint) -> c_uint {
    match number {
        1..=5 => api::GDK_BUTTON1_MASK << (number - 1),
        _ => 0,
    }
}

/// GDK's modifier state of egui's modifiers (Ctrl is Linux's command key).
pub(super) fn state_of(modifiers: egui::Modifiers) -> c_uint {
    let mut state = 0;
    if modifiers.shift {
        state |= api::GDK_SHIFT_MASK;
    }
    if modifiers.ctrl || modifiers.command {
        state |= api::GDK_CONTROL_MASK;
    }
    if modifiers.alt {
        state |= api::GDK_MOD1_MASK;
    }
    state
}

/// A wheel's delta as GDK's smooth scroll (WebKit moves 40 pixels a unit, content pixels — the
/// page zoomed by `scale`): a line a unit, points and pages by the 40; egui's positive delta
/// moves the content down, GDK's scrolls down.
pub(super) fn smooth_delta(
    unit: egui::MouseWheelUnit,
    delta: egui::Vec2,
    scale: f32,
    page: f32,
) -> (f64, f64) {
    let factor = match unit {
        egui::MouseWheelUnit::Line => f64::from(scale),
        egui::MouseWheelUnit::Point => f64::from(scale) / 40.0,
        egui::MouseWheelUnit::Page => f64::from(scale) * f64::from(page.max(40.0)) / 40.0,
    };
    (-f64::from(delta.x) * factor, -f64::from(delta.y) * factor)
}

/// The keyval of a key that types no text of its own, or of any key with Ctrl or Alt (a
/// shortcut, which egui sends no text for); `None` for a key its text event types. Esc and the
/// F-keys never reach the page (§2.4) and are not here.
pub(super) fn key_keyval(key: egui::Key, modifiers: egui::Modifiers) -> Option<c_uint> {
    use egui::Key::*;
    let named = match key {
        Enter => Some(0xff0d),
        Tab if modifiers.shift => Some(0xfe20),
        Tab => Some(0xff09),
        Backspace => Some(0xff08),
        Delete => Some(0xffff),
        Insert => Some(0xff63),
        Home => Some(0xff50),
        End => Some(0xff57),
        PageUp => Some(0xff55),
        PageDown => Some(0xff56),
        ArrowLeft => Some(0xff51),
        ArrowUp => Some(0xff52),
        ArrowRight => Some(0xff53),
        ArrowDown => Some(0xff54),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if !(modifiers.ctrl || modifiers.command || modifiers.alt) {
        return None;
    }
    let mut name = key.name().chars();
    match (name.next(), name.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => {
            Some(c_uint::from(c.to_ascii_lowercase() as u8))
        }
        _ if key == Space => Some(0x20),
        _ => None,
    }
}

/// GDK's keyval of character `c`: Latin-1's printable characters are their own, every other
/// character `0x01000000 + its code point` (GDK's Unicode keyvals).
pub(super) fn char_keyval(c: char) -> Option<c_uint> {
    let point = u32::from(c);
    match c {
        '\n' | '\r' => Some(0xff0d),
        '\t' => Some(0xff09),
        _ if c.is_control() => None,
        _ if (0x20..=0x7e).contains(&point) || (0xa0..=0xff).contains(&point) => Some(point),
        _ => Some(0x0100_0000 | point),
    }
}

/// `text` typed, a press and a release a character.
fn typed(text: &str) -> Vec<Input> {
    text.chars()
        .filter_map(char_keyval)
        .flat_map(|keyval| {
            [true, false].map(|pressed| Input::Key {
                keyval,
                pressed,
                state: 0,
            })
        })
        .collect()
}

/// Ctrl and `letter`, pressed and released.
fn shortcut(letter: char) -> Vec<Input> {
    let keyval = c_uint::from(letter as u8);
    [true, false]
        .map(|pressed| Input::Key {
            keyval,
            pressed,
            state: api::GDK_CONTROL_MASK,
        })
        .to_vec()
}

/// Milliseconds of the process, for GDK events' times (WebKit counts clicks by them).
fn event_time() -> u32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    (start.elapsed().as_millis() as u64 & u64::from(u32::MAX)) as u32
}

/// `input` to view `key`'s web view, as GDK events sent to it.
///
/// SAFETY: on the GTK thread.
unsafe fn synthesize(api: &Api, key: u64, input: Input) {
    let Some((web, pointer, keyboard)) = with(|gtk| {
        gtk.views
            .get(&key)
            .map(|v| (v.web, gtk.pointer, gtk.keyboard))
    })
    .flatten() else {
        return;
    };
    // SAFETY (this block's calls): GDK events made for the view's own window, its reference
    // taken for the event (freed with it), sent to the view on the GTK thread.
    unsafe {
        let window = (api.gtk_widget_get_window)(web);
        if window.is_null() {
            return;
        }
        let time = event_time();
        // Positions come in device pixels; the widget's own are those over GTK's factor, as its
        // size is (`place`).
        let factor = f64::from((api.gtk_widget_get_scale_factor)(web).max(1));
        let input = input.over(factor);
        let (event, device) = match input {
            Input::Motion { x, y, state } => {
                let event = (api.gdk_event_new)(api::GDK_MOTION_NOTIFY);
                let e = &mut *event.cast::<EventMotion>();
                e.window = (api.g_object_ref)(window);
                e.send_event = 1;
                e.time = time;
                (e.x, e.y, e.x_root, e.y_root) = (x, y, x, y);
                e.state = state;
                (event, pointer)
            }
            Input::Button {
                x,
                y,
                button,
                pressed,
                state,
            } => {
                let ty = if pressed {
                    api::GDK_BUTTON_PRESS
                } else {
                    api::GDK_BUTTON_RELEASE
                };
                let event = (api.gdk_event_new)(ty);
                let e = &mut *event.cast::<EventButton>();
                e.window = (api.g_object_ref)(window);
                e.send_event = 1;
                e.time = time;
                (e.x, e.y, e.x_root, e.y_root) = (x, y, x, y);
                e.state = state;
                e.button = button;
                (event, pointer)
            }
            Input::Scroll {
                x,
                y,
                dx,
                dy,
                state,
            } => {
                let event = (api.gdk_event_new)(api::GDK_SCROLL);
                let e = &mut *event.cast::<EventScroll>();
                e.window = (api.g_object_ref)(window);
                e.send_event = 1;
                e.time = time;
                (e.x, e.y, e.x_root, e.y_root) = (x, y, x, y);
                e.state = state;
                e.direction = api::GDK_SCROLL_SMOOTH;
                (e.delta_x, e.delta_y) = (dx, dy);
                (event, pointer)
            }
            Input::Key {
                keyval,
                pressed,
                state,
            } => {
                let ty = if pressed {
                    api::GDK_KEY_PRESS
                } else {
                    api::GDK_KEY_RELEASE
                };
                let event = (api.gdk_event_new)(ty);
                let e = &mut *event.cast::<EventKey>();
                e.window = (api.g_object_ref)(window);
                e.send_event = 1;
                e.time = time;
                e.state = state;
                e.keyval = keyval;
                (event, keyboard)
            }
            Input::Focus(focus_in) => {
                if focus_in {
                    (api.gtk_widget_grab_focus)(web);
                }
                let event = (api.gdk_event_new)(api::GDK_FOCUS_CHANGE);
                let e = &mut *event.cast::<EventFocus>();
                e.window = (api.g_object_ref)(window);
                e.send_event = 1;
                e.focus_in = i16::from(focus_in);
                (event, keyboard)
            }
        };
        if !device.is_null() {
            (api.gdk_event_set_device)(event, device);
        }
        (api.gtk_widget_event)(web, event);
        (api.gdk_event_free)(event);
    }
}

// ---------------------------------------------------------------------------------------------
// The view the component holds (the windows' thread).

/// A WebKitGTK view, as the windows' thread holds it: what it tells the GTK thread, and the
/// texture of its newest picture.
pub(super) struct GtkView {
    key: u64,
    api: &'static Api,
    tx: mpsc::Sender<Cmd>,
    shared: Arc<Shared>,
    scope: Scope,
    events: Events,
    texture: Option<(egui::TextureHandle, f32)>,
    /// The picture the texture holds.
    seq: u64,
    forward: Forward,
    /// The size and scale last told.
    told: Option<(i32, i32, f32)>,
    /// The renderer's largest texture side, as the last pass said.
    max_side: usize,
    closed: bool,
}

impl GtkView {
    fn send(&self, cmd: Cmd) {
        if !self.closed {
            post(self.api, &self.tx, cmd);
        }
    }
}

#[cfg(test)]
impl GtkView {
    /// The newest picture copied, and its scale.
    fn picture(&self) -> Option<(Arc<egui::ColorImage>, f32)> {
        let state = self.shared.lock();
        state.picture.as_ref().map(|p| (p.image.clone(), p.scale))
    }

    /// The pictures copied so far.
    fn pictures(&self) -> u64 {
        self.shared.lock().seq
    }

    /// `probe` on the GTK thread with the view's web view; its answer.
    fn probe<R: Send + 'static>(&self, probe: impl FnOnce(&Api, P) -> R + Send + 'static) -> R {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Probe(
            self.key,
            Box::new(move |api, web| {
                let _ = tx.send(probe(api, web));
            }),
        ));
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the GTK thread answers")
    }

    /// What the GTK thread holds, once it has run every command sent before.
    fn counts(&self) -> Counts {
        let (tx, rx) = mpsc::channel();
        post(self.api, &self.tx, Cmd::Count(tx));
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the GTK thread answers")
    }

    /// The page's title (the tests' look).
    fn title(&self) -> String {
        // SAFETY: on the GTK thread, the view's web view.
        self.probe(|api, web| unsafe { text((api.webkit_web_view_get_title)(web)) })
    }
}

impl View for GtkView {
    fn attach(&mut self, _parent: super::Parent) {}

    fn place(&mut self, rect: egui::Rect, scale: f32) {
        self.forward.scale = scale;
        self.forward.height = rect.height();
        let (w, h) = device_size(rect, scale, self.max_side);
        if self.told != Some((w, h, scale)) {
            self.told = Some((w, h, scale));
            self.send(Cmd::Place(self.key, w, h, scale));
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if hidden {
            if let Some(blur) = self.forward.blur() {
                self.send(Cmd::Input(self.key, blur));
            }
        }
    }

    fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let ctx = painter.ctx();
        // Before the frame's placement (the shell's after-frame step) uses it.
        self.max_side = ctx.input(|i| i.max_texture_side);
        let fresh = {
            let mut state = self.shared.lock();
            if state.ctx.is_none() {
                state.ctx = Some(ctx.clone());
            }
            (state.seq != self.seq).then(|| (state.picture.clone(), state.seq))
        };
        if let Some((Some(picture), seq)) = fresh {
            self.seq = seq;
            let at = Instant::now();
            let options = if (picture.scale - ctx.pixels_per_point()).abs() < f32::EPSILON {
                egui::TextureOptions::NEAREST
            } else {
                egui::TextureOptions::LINEAR
            };
            let image = egui::ImageData::Color(picture.image);
            match &mut self.texture {
                Some((texture, scale)) => {
                    texture.set(image, options);
                    *scale = picture.scale;
                }
                None => {
                    let name = format!("sopen-page-{}", self.key);
                    self.texture = Some((ctx.load_texture(name, image, options), picture.scale));
                }
            }
            crate::debug!(
                "page view: upload in {:.2} ms",
                at.elapsed().as_secs_f64() * 1e3
            );
        }
        if let Some((texture, scale)) = &self.texture {
            // Its own size, device pixels over its scale: one device pixel a pixel, a picture of
            // an earlier size drawn as it was until the next comes.
            let size = texture.size_vec2() / scale.max(f32::EPSILON);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(
                texture.id(),
                egui::Rect::from_min_size(rect.min, size),
                uv,
                egui::Color32::WHITE,
            );
        }
    }

    fn input(&mut self, event: &egui::Event) {
        for input in self.forward.translate(event) {
            self.send(Cmd::Input(self.key, input));
        }
    }

    /// A load asked of the view: refused outside its scope before WebKit sees it.
    fn load(&mut self, url: &str) {
        if !self.scope.keeps(url) {
            self.events.send(Event::Refused {
                target: url.to_string(),
                clicked: false,
            });
            return;
        }
        self.send(Cmd::Load(self.key, url.to_string()));
    }

    fn reload(&mut self) {
        self.send(Cmd::Reload(self.key));
    }

    fn set_scripts(&mut self, on: bool) {
        self.send(Cmd::Scripts(self.key, on));
    }

    fn set_preview(&mut self, on: bool) {
        if on {
            if let Some(blur) = self.forward.blur() {
                self.send(Cmd::Input(self.key, blur));
            }
        }
    }

    fn set_shown(&mut self, shown: bool) {
        if !shown {
            if let Some(blur) = self.forward.blur() {
                self.send(Cmd::Input(self.key, blur));
            }
        }
        self.send(Cmd::Shown(self.key, shown));
    }

    /// The newest picture: egui paints it, so the screenshot hook's read-back has it already;
    /// asked anyway, it is given.
    fn snapshot(&mut self, done: Box<dyn FnOnce(Option<egui::ColorImage>)>) {
        let picture = self.shared.lock().picture.clone();
        done(picture.map(|p| Arc::unwrap_or_clone(p.image)));
    }

    fn close(&mut self) {
        if !self.closed {
            self.send(Cmd::Close(self.key));
            self.closed = true;
            self.texture = None;
        }
    }
}

impl Drop for GtkView {
    fn drop(&mut self) {
        self.close();
    }
}
